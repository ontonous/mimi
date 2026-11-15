//! SD-4/Body migration: Z3 encoding for ResolvedExpr (Typed Resolved IR).
//!
//! Parallel to `expr.rs` (raw AST encoding). Consumes `ResolvedExprKind`
//! instead of `Expr`, enabling the verifier to work from CheckedProgram
//! without reopening the surface AST.
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

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{BinOp, Lit, UnOp};
use crate::core::ir::{
    CheckedConversionKind, ContractKind, PrimitiveType, ResolvedBinaryOp, ResolvedBlock,
    ResolvedBody, ResolvedCallee, ResolvedExpr, ResolvedExprKind, ResolvedIndex, ResolvedLiteral,
    ResolvedLocalId, ResolvedPatternKind, ResolvedPlace, ResolvedProjection, ResolvedStmtKind,
    ResolvedType, ResolvedTypeTable, ResolvedUnaryOp, ResolvedValueProjection, TypedProofBlock,
    TypedProofOperation, TypedProofPointWorklist,
};
use crate::core::{NodeId, ResolvedVariantSchema};
use crate::verifier::checked_power::{
    encode_checked_integer_power, CheckedPowerWidth, POWER_PROFILE_0_1_11_COMPAT,
};
use crate::verifier::ctx::Z3VarMap;
use z3::ast::{Bool as Z3Bool, Int as Z3Int, Real as Z3Real, Set as Z3Set};

pub(crate) type ResolvedFlowStateFieldCatalog =
    BTreeMap<NodeId, BTreeMap<NodeId, crate::core::ir::ResolvedTypeId>>;

/// Build the exact Flow-state field directory needed by the verifier's
/// structural state-pattern model. A state is included only when all of its
/// checker-owned field identities have canonical types.
pub(crate) fn resolved_flow_state_field_catalog(
    program: &crate::core::CheckedProgram,
) -> Option<ResolvedFlowStateFieldCatalog> {
    let mut catalog = BTreeMap::new();
    for flow in program.flows().values() {
        for (name, state) in &flow.states {
            if state.id.flow != flow.id
                || state.id.name != *name
                || state.payload.len() != state.field_ids.len()
            {
                return None;
            }
            let mut fields = BTreeMap::new();
            for (field_name, _) in &state.payload {
                let field = state.field_ids.get(field_name)?;
                let ty = program.resolved_field_type(field)?.clone();
                if program.resolved_types().get(&ty).is_none()
                    || fields.insert(field.clone(), ty).is_some()
                {
                    return None;
                }
            }
            if catalog.insert(state.node_id.clone(), fields).is_some() {
                return None;
            }
        }
    }
    Some(catalog)
}

/// Resolve a place to the unique variable key of its base local.
///
/// C-7 (audit 2026-08-05): keys are derived from `ResolvedLocalId`, not
/// `display_name`. Shadowed locals share the bare display name of the local
/// they shadow; keying by display name resolved shadow loads back onto the
/// shadowed parameter/local variable, fabricating Proven for contracts such
/// as `if c { let x = x + 1; x } else { x }` + `ensures: result == x`.
/// Checker-typed field/tuple projections are encoded by their stable field
/// identity or tuple index. Index and dereference projections remain outside
/// the trusted subset.
fn local_key(
    place: &ResolvedPlace,
    body: &ResolvedBody,
    expected_type: &crate::core::ir::ResolvedTypeId,
) -> Option<String> {
    let local = body.locals.get(&place.base)?;
    if place.projected_type(local) != expected_type {
        return None;
    }
    let mut key = local_var_key(&local.id, &local.display_name);
    for projection in &place.projections {
        match projection {
            ResolvedProjection::Field { field, .. } => {
                // Match the key used for an explicit Project(Field(..)) so
                // both typed IR encodings denote the same scalar field.
                key.push('.');
                key.push_str(&field.0);
            }
            ResolvedProjection::Tuple { index, .. } => {
                key.push_str(&format!(".t{index}"));
            }
            ResolvedProjection::Index { .. } | ResolvedProjection::Deref { .. } => return None,
        }
    }
    Some(key)
}

/// Z3 variable key for a local: `{display_name}#{local_id}`. The display-name
/// prefix keeps solver output readable; the id suffix makes the key unique
/// under shadowing.
fn local_var_key(id: &ResolvedLocalId, display_name: &str) -> String {
    format!("{}#{}", display_name, id.0 .0)
}

/// Exact checked-machine bounds for the scalar integer types modeled by this
/// verifier.  These are static type premises, not arithmetic-definedness
/// obligations: a typed i32/i64 value cannot denote an out-of-range input.
fn machine_integer_bounds(
    ty: &crate::core::ir::ResolvedTypeId,
    types: &ResolvedTypeTable,
) -> Option<(i64, i64)> {
    match types.get(ty)? {
        ResolvedType::Primitive(PrimitiveType::I32) => Some((i32::MIN as i64, i32::MAX as i64)),
        ResolvedType::Primitive(PrimitiveType::I64) => Some((i64::MIN, i64::MAX)),
        _ => None,
    }
}

fn projected_integer_expr_key(expr: &ResolvedExpr, body: &ResolvedBody) -> Option<String> {
    match &expr.kind {
        ResolvedExprKind::Load(place) if !place.projections.is_empty() => {
            local_key(place, body, &expr.ty)
        }
        ResolvedExprKind::Project { projection, .. }
            if matches!(
                projection,
                ResolvedValueProjection::Field(_) | ResolvedValueProjection::Tuple(_)
            ) =>
        {
            resolved_field_var_name(expr, body)
        }
        _ => None,
    }
}

fn record_projected_integer_range_fact(
    key: String,
    bounds: (i64, i64),
    facts: &mut BTreeMap<String, (i64, i64)>,
) -> Result<(), String> {
    if let Some(previous) = facts.insert(key.clone(), bounds) {
        if previous != bounds {
            return Err(format!(
                "typed projected integer '{key}' has conflicting machine ranges"
            ));
        }
    }
    Ok(())
}

/// Gather checker-typed Field/Tuple integer projections throughout the body
/// before any root contract is assumed.  The supported symbolic projections
/// are deliberately the same closed set as `resolved_to_z3_int`: Index and
/// Dereference never acquire a free integer variable here.
fn collect_projected_integer_range_facts(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    facts: &mut BTreeMap<String, (i64, i64)>,
) -> Result<(), String> {
    for statement in &block.statements {
        match &statement.kind {
            ResolvedStmtKind::Bind { initializer, .. } => {
                if let Some(expr) = initializer {
                    collect_projected_expr_range_facts(expr, body, types, facts)?;
                }
            }
            ResolvedStmtKind::Assign { value, .. } | ResolvedStmtKind::Expr(value) => {
                collect_projected_expr_range_facts(value, body, types, facts)?;
            }
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                if let Some(expr) = value {
                    collect_projected_expr_range_facts(expr, body, types, facts)?;
                }
            }
            ResolvedStmtKind::While {
                condition,
                body: inner,
            } => {
                collect_projected_expr_range_facts(condition, body, types, facts)?;
                collect_projected_integer_range_facts(inner, body, types, facts)?;
            }
            ResolvedStmtKind::WhileLet {
                initializer,
                body: inner,
                ..
            } => {
                collect_projected_expr_range_facts(initializer, body, types, facts)?;
                collect_projected_integer_range_facts(inner, body, types, facts)?;
            }
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                collect_projected_expr_range_facts(initializer, body, types, facts)?;
                collect_projected_integer_range_facts(then_block, body, types, facts)?;
                if let Some(else_block) = else_block {
                    collect_projected_integer_range_facts(else_block, body, types, facts)?;
                }
            }
            ResolvedStmtKind::Loop(inner) | ResolvedStmtKind::Scope { body: inner, .. } => {
                collect_projected_integer_range_facts(inner, body, types, facts)?;
            }
            ResolvedStmtKind::Pinned {
                value, body: inner, ..
            } => {
                collect_projected_expr_range_facts(value, body, types, facts)?;
                collect_projected_integer_range_facts(inner, body, types, facts)?;
            }
            ResolvedStmtKind::For {
                iterable,
                body: inner,
                ..
            } => {
                collect_projected_expr_range_facts(iterable, body, types, facts)?;
                collect_projected_integer_range_facts(inner, body, types, facts)?;
            }
            ResolvedStmtKind::Math(expressions) => {
                for expr in expressions {
                    collect_projected_expr_range_facts(expr, body, types, facts)?;
                }
            }
            ResolvedStmtKind::Contract { condition, .. } => {
                collect_projected_expr_range_facts(condition, body, types, facts)?;
            }
            ResolvedStmtKind::Continue
            | ResolvedStmtKind::Drop(_)
            | ResolvedStmtKind::NestedCallable(_) => {}
        }
    }
    if let Some(result) = &block.result {
        collect_projected_expr_range_facts(result, body, types, facts)?;
    }
    Ok(())
}

fn collect_projected_expr_range_facts(
    expr: &ResolvedExpr,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    facts: &mut BTreeMap<String, (i64, i64)>,
) -> Result<(), String> {
    if let (Some(bounds), Some(key)) = (
        machine_integer_bounds(&expr.ty, types),
        projected_integer_expr_key(expr, body),
    ) {
        record_projected_integer_range_fact(key, bounds, facts)?;
    }

    match &expr.kind {
        ResolvedExprKind::FString(parts) => {
            for part in parts {
                if let crate::core::ir::ResolvedFStringPart::Interpolation(value) = part {
                    collect_projected_expr_range_facts(value, body, types, facts)?;
                }
            }
        }
        ResolvedExprKind::Project { value, projection } => {
            collect_projected_expr_range_facts(value, body, types, facts)?;
            if let ResolvedValueProjection::Index(index) = projection {
                collect_projected_expr_range_facts(index, body, types, facts)?;
            }
        }
        ResolvedExprKind::Binary { left, right, .. } => {
            collect_projected_expr_range_facts(left, body, types, facts)?;
            collect_projected_expr_range_facts(right, body, types, facts)?;
        }
        ResolvedExprKind::Unary { operand, .. } => {
            collect_projected_expr_range_facts(operand, body, types, facts)?;
        }
        ResolvedExprKind::Call(call) => {
            for argument in &call.arguments {
                collect_projected_expr_range_facts(&argument.value, body, types, facts)?;
            }
        }
        ResolvedExprKind::Tuple(items)
        | ResolvedExprKind::List(items)
        | ResolvedExprKind::Set(items) => {
            for item in items {
                collect_projected_expr_range_facts(item, body, types, facts)?;
            }
        }
        ResolvedExprKind::Map(entries) => {
            for (key, value) in entries {
                collect_projected_expr_range_facts(key, body, types, facts)?;
                collect_projected_expr_range_facts(value, body, types, facts)?;
            }
        }
        ResolvedExprKind::Comprehension {
            value,
            iterable,
            guard,
            ..
        } => {
            collect_projected_expr_range_facts(value, body, types, facts)?;
            collect_projected_expr_range_facts(iterable, body, types, facts)?;
            if let Some(guard) = guard {
                collect_projected_expr_range_facts(guard, body, types, facts)?;
            }
        }
        ResolvedExprKind::OptionalChain { receiver, .. } => {
            collect_projected_expr_range_facts(receiver, body, types, facts)?;
        }
        ResolvedExprKind::TypeOf(inner) => {
            collect_projected_expr_range_facts(inner, body, types, facts)?;
        }
        ResolvedExprKind::Old(inner) => {
            // An Old projection denotes a separate function-entry snapshot.
            // Pin only that snapshot symbol; never accidentally constrain the
            // current value merely because its syntax appears under Old.
            if let (Some(bounds), Some(key)) = (
                machine_integer_bounds(&inner.ty, types),
                projected_integer_expr_key(inner, body),
            ) {
                record_projected_integer_range_fact(format!("old.{key}"), bounds, facts)?;
            }
        }
        ResolvedExprKind::Record { fields, rest, .. } => {
            for field in fields {
                collect_projected_expr_range_facts(&field.value, body, types, facts)?;
            }
            if let Some(rest) = rest {
                collect_projected_expr_range_facts(rest, body, types, facts)?;
            }
        }
        ResolvedExprKind::Block(inner) | ResolvedExprKind::Comptime(inner) => {
            collect_projected_integer_range_facts(inner, body, types, facts)?;
        }
        ResolvedExprKind::Scope { body: inner, .. } => {
            collect_projected_integer_range_facts(inner, body, types, facts)?;
        }
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            collect_projected_expr_range_facts(condition, body, types, facts)?;
            collect_projected_integer_range_facts(then_block, body, types, facts)?;
            collect_projected_integer_range_facts(else_block, body, types, facts)?;
        }
        ResolvedExprKind::Match { scrutinee, arms } => {
            collect_projected_expr_range_facts(scrutinee, body, types, facts)?;
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_projected_expr_range_facts(guard, body, types, facts)?;
                }
                collect_projected_expr_range_facts(&arm.body, body, types, facts)?;
            }
        }
        ResolvedExprKind::Try { value, .. } | ResolvedExprKind::Cast { value, .. } => {
            collect_projected_expr_range_facts(value, body, types, facts)?;
        }
        ResolvedExprKind::Range { start, end } => {
            collect_projected_expr_range_facts(start, body, types, facts)?;
            collect_projected_expr_range_facts(end, body, types, facts)?;
        }
        ResolvedExprKind::Slice { target, start, end } => {
            collect_projected_expr_range_facts(target, body, types, facts)?;
            if let Some(start) = start {
                collect_projected_expr_range_facts(start, body, types, facts)?;
            }
            if let Some(end) = end {
                collect_projected_expr_range_facts(end, body, types, facts)?;
            }
        }
        ResolvedExprKind::Spawn(inner) | ResolvedExprKind::Await(inner) => {
            collect_projected_expr_range_facts(inner, body, types, facts)?;
        }
        ResolvedExprKind::Lambda(lambda) => {
            collect_projected_integer_range_facts(&lambda.body, body, types, facts)?;
        }
        // Quote is inert data and leaves have no child expression.
        ResolvedExprKind::Quote(_)
        | ResolvedExprKind::Literal(_)
        | ResolvedExprKind::Load(_)
        | ResolvedExprKind::Constant(_)
        | ResolvedExprKind::Callable(_)
        | ResolvedExprKind::DefaultArgument { .. }
        | ResolvedExprKind::ComptimeValue(_)
        | ResolvedExprKind::TypeValue(_) => {}
    }
    Ok(())
}

fn assert_projected_integer_range_facts(
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
    session: &mut crate::verifier::ctx::SolverSession,
) -> Result<(), String> {
    let mut facts = BTreeMap::new();
    collect_projected_integer_range_facts(&body.root, body, types, &mut facts)?;
    for (key, (minimum, maximum)) in facts {
        let value = vars.get_or_create_int(&key);
        let lower = Z3Int::from_i64(minimum);
        let upper = Z3Int::from_i64(maximum);
        session.solver.assert(value.ge(&lower));
        session.solver.assert(value.le(&upper));
    }
    Ok(())
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
            let key = local_key(place, body, &expr.ty)?;
            if z3_type_category(&expr.ty, types) != Z3TypeCategory::Int {
                return None;
            }
            if place.projections.is_empty() {
                vars.get_int(&key).cloned()
            } else {
                Some(vars.get_or_create_int(&key))
            }
        }
        ResolvedExprKind::Old(inner) => {
            if let ResolvedExprKind::Load(place) = &inner.kind {
                let key = local_key(place, body, &inner.ty)?;
                // §11-#37: dot separator for namespace consistency.
                let old_key = format!("old.{}", key);
                return vars.get_int(&old_key).cloned();
            }
            None
        }
        ResolvedExprKind::Project { value, projection } => {
            if z3_type_category(&expr.ty, types) != Z3TypeCategory::Int {
                return None;
            }
            let base = resolved_field_var_name(value, body)?;
            let proj_name = match projection {
                ResolvedValueProjection::Field(id) => id.0.clone(),
                ResolvedValueProjection::Tuple(idx) => format!("t{}", idx),
                // Indexing may trap, and the current verifier has no bounds or
                // effect receipt for it. Treating every `xs[i]` as one
                // uninterpreted `xs.idx` variable made `xs[0] == xs[0]` a
                // tautology even when evaluating either side traps.
                ResolvedValueProjection::Index(_) | ResolvedValueProjection::Dereference => {
                    return None;
                }
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
                ResolvedBinaryOp::Power => {
                    let width = checked_power_source_width(expr, left, right, types)?;
                    Some(
                        encode_checked_integer_power(&l, &r, width, POWER_PROFILE_0_1_11_COMPAT)
                            .value,
                    )
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

/// Power uses the exact checker-selected width of both operands and the
/// result.  The phase compatibility profile internally checks every i64
/// multiplication and only narrows the successful i32 result afterward.
fn checked_power_source_width(
    expression: &ResolvedExpr,
    left: &ResolvedExpr,
    right: &ResolvedExpr,
    types: &ResolvedTypeTable,
) -> Option<CheckedPowerWidth> {
    let result_bounds = int_bounds(expression, types)?;
    if int_bounds(left, types)? != result_bounds || int_bounds(right, types)? != result_bounds {
        return None;
    }
    if result_bounds == (i32::MIN as i64, i32::MAX as i64) {
        Some(CheckedPowerWidth::I32)
    } else if result_bounds == (i64::MIN, i64::MAX) {
        Some(CheckedPowerWidth::I64)
    } else {
        None
    }
}

/// Encode a ResolvedExpr as a Z3 Real term.
///
/// H-21 (audit 2026-08-05): this encoder is only reachable for i32-exact
/// values now — the Resolved verifier fail-closes any callable
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
            let key = local_key(place, body, &expr.ty)?;
            if z3_type_category(&expr.ty, types) != Z3TypeCategory::Real {
                return None;
            }
            if place.projections.is_empty() {
                vars.get_real(&key).cloned()
            } else {
                Some(vars.get_or_create_real(&key))
            }
        }
        ResolvedExprKind::Old(inner) => {
            if let ResolvedExprKind::Load(place) = &inner.kind {
                let key = local_key(place, body, &inner.ty)?;
                let old_key = format!("old.{}", key);
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
            let key = local_key(place, body, &expr.ty)?;
            if z3_type_category(&expr.ty, types) != Z3TypeCategory::Bool {
                return None;
            }
            if place.projections.is_empty() {
                vars.get_bool(&key).cloned()
            } else {
                Some(vars.get_or_create_bool(&key))
            }
        }
        ResolvedExprKind::Binary { op, left, right } => {
            // Bool logic FIRST: LogicalAnd/LogicalOr operate on boolean
            // operands. Previously these arms were nested under the int
            // comparison branch (reachable only when both sides encode as
            // Int) — so `requires: x >= 0 && x <= 1000` failed to encode and
            // the precondition was silently dropped, leaving the solver free
            // to find an overflow counterexample (E0439 divergence, Phase E
            // 0.39.80).
            if matches!(
                op,
                ResolvedBinaryOp::LogicalAnd | ResolvedBinaryOp::LogicalOr
            ) {
                let lb = resolved_to_z3_bool(left, body, types, vars)?;
                let rb = resolved_to_z3_bool(right, body, types, vars)?;
                return match op {
                    ResolvedBinaryOp::LogicalAnd => Some(Z3Bool::and(&[&lb, &rb])),
                    ResolvedBinaryOp::LogicalOr => Some(Z3Bool::or(&[&lb, &rb])),
                    _ => unreachable!("bool logic arm"),
                };
            }
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

/// Build a variable name for a supported field/tuple projection.
///
/// Returning `None` for Index/Dereference (including when nested under a
/// field projection) prevents the Z3 encoder from silently replacing an
/// operation with a free variable.
fn resolved_field_var_name(expr: &ResolvedExpr, body: &ResolvedBody) -> Option<String> {
    match &expr.kind {
        ResolvedExprKind::Load(place) => Some(local_key(place, body, &expr.ty)?),
        ResolvedExprKind::Project { value, projection } => {
            let proj_name = match projection {
                ResolvedValueProjection::Field(id) => id.0.clone(),
                ResolvedValueProjection::Tuple(idx) => format!("t{}", idx),
                ResolvedValueProjection::Index(_) | ResolvedValueProjection::Dereference => {
                    return None;
                }
            };
            Some(format!(
                "{}.{}",
                resolved_field_var_name(value, body)?,
                proj_name
            ))
        }
        _ => None,
    }
}

/// A checker-typed path through a symbolic aggregate.  Field identities are
/// stable NodeIds; display names never participate in the verifier's value
/// identity. Variant payloads are namespaced by both the variant and member
/// identity so mutually-exclusive payloads cannot alias one another.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ResolvedValuePathPart {
    Field(NodeId),
    Tuple(usize),
    VariantPayload { variant: NodeId, member: NodeId },
}

#[derive(Debug, Clone)]
enum ResolvedScalarValue {
    Int(Z3Int),
    Real(Z3Real),
    Bool(Z3Bool),
}

fn append_value_path(base: &str, path: &[ResolvedValuePathPart]) -> String {
    let mut key = base.to_string();
    for part in path {
        match part {
            ResolvedValuePathPart::Field(field) => {
                key.push('.');
                key.push_str(&field.0);
            }
            ResolvedValuePathPart::Tuple(index) => key.push_str(&format!(".t{index}")),
            ResolvedValuePathPart::VariantPayload { variant, member } => {
                key.push_str(".variant[");
                key.push_str(&variant.0);
                key.push_str("].payload[");
                key.push_str(&member.0);
                key.push(']');
            }
        }
    }
    key
}

fn value_base_key(expr: &ResolvedExpr, body: &ResolvedBody) -> Option<String> {
    match &expr.kind {
        ResolvedExprKind::Load(place) => local_key(place, body, &expr.ty),
        ResolvedExprKind::Project { value, projection } => {
            let mut key = value_base_key(value, body)?;
            match projection {
                ResolvedValueProjection::Field(field) => {
                    key.push('.');
                    key.push_str(&field.0);
                }
                ResolvedValueProjection::Tuple(index) => {
                    key.push_str(&format!(".t{index}"));
                }
                ResolvedValueProjection::Index(_) | ResolvedValueProjection::Dereference => {
                    return None;
                }
            }
            Some(key)
        }
        _ => None,
    }
}

fn resolved_variant_tag_number(
    ty: &crate::core::ir::ResolvedTypeId,
    variant: &NodeId,
    types: &ResolvedTypeTable,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
) -> Option<i64> {
    let identity = variant.0.as_str();
    let (option_type, result_type) = match types.get(ty)? {
        ResolvedType::Option(_) => (true, false),
        ResolvedType::Result { .. } => (false, true),
        ResolvedType::Nominal {
            item, arguments, ..
        } => (
            item.as_str() == "builtin:type:Option" && arguments.len() == 1,
            item.as_str() == "builtin:type:Result" && arguments.len() == 2,
        ),
        _ => (false, false),
    };
    if option_type {
        return match identity {
            "builtin:variant:Option::None" => Some(0),
            "builtin:variant:Option::Some" => Some(1),
            _ => None,
        };
    }
    if result_type {
        return match identity {
            "builtin:variant:Result::Ok" => Some(0),
            "builtin:variant:Result::Err" => Some(1),
            _ => None,
        };
    }
    let owner = match types.get(ty)? {
        ResolvedType::Nominal { item, .. } => item.as_str(),
        _ => return None,
    };
    let mut owner_variants = variants
        .values()
        .filter(|schema| schema.owner.0 == owner)
        .collect::<Vec<_>>();
    owner_variants.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    owner_variants
        .iter()
        .position(|schema| &schema.node_id == variant)
        .and_then(|index| i64::try_from(index).ok())
}

fn builtin_variant_member_type(
    ty: &crate::core::ir::ResolvedTypeId,
    variant: &NodeId,
    member: &NodeId,
    types: &ResolvedTypeTable,
) -> Option<crate::core::ir::ResolvedTypeId> {
    let expected_member = match variant.0.as_str() {
        "builtin:variant:Option::Some" => "builtin:variant:Option::Some/payload:0",
        "builtin:variant:Result::Ok" => "builtin:variant:Result::Ok/payload:0",
        "builtin:variant:Result::Err" => "builtin:variant:Result::Err/payload:0",
        _ => return None,
    };
    if member.0 != expected_member {
        return None;
    }
    match (variant.0.as_str(), types.get(ty)?) {
        ("builtin:variant:Option::Some", ResolvedType::Option(inner)) => Some(inner.clone()),
        (
            "builtin:variant:Option::Some",
            ResolvedType::Nominal {
                item, arguments, ..
            },
        ) if item.as_str() == "builtin:type:Option" && arguments.len() == 1 => {
            Some(arguments[0].clone())
        }
        ("builtin:variant:Result::Ok", ResolvedType::Result { ok, .. }) => Some(ok.clone()),
        ("builtin:variant:Result::Err", ResolvedType::Result { error, .. }) => Some(error.clone()),
        (
            "builtin:variant:Result::Ok",
            ResolvedType::Nominal {
                item, arguments, ..
            },
        ) if item.as_str() == "builtin:type:Result" && arguments.len() == 2 => {
            Some(arguments[0].clone())
        }
        (
            "builtin:variant:Result::Err",
            ResolvedType::Nominal {
                item, arguments, ..
            },
        ) if item.as_str() == "builtin:type:Result" && arguments.len() == 2 => {
            Some(arguments[1].clone())
        }
        _ => None,
    }
}

fn resolved_scalar_leaf_supported(
    ty: &crate::core::ir::ResolvedTypeId,
    types: &ResolvedTypeTable,
) -> bool {
    matches!(
        types.get(ty),
        Some(ResolvedType::Primitive(
            PrimitiveType::I32 | PrimitiveType::I64 | PrimitiveType::Bool
        ))
    )
}

fn scalar_value_at_key(
    key: &str,
    ty: &crate::core::ir::ResolvedTypeId,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
) -> Option<ResolvedScalarValue> {
    if !resolved_scalar_leaf_supported(ty, types) {
        return None;
    }
    match z3_type_category(ty, types) {
        Z3TypeCategory::Int => Some(ResolvedScalarValue::Int(vars.get_or_create_int(key))),
        Z3TypeCategory::Real => Some(ResolvedScalarValue::Real(vars.get_or_create_real(key))),
        Z3TypeCategory::Bool => Some(ResolvedScalarValue::Bool(vars.get_or_create_bool(key))),
    }
}

fn scalar_value_of_expr_with_catalog(
    expr: &ResolvedExpr,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<ResolvedScalarValue> {
    if !resolved_scalar_leaf_supported(&expr.ty, types) {
        return None;
    }
    match z3_type_category(&expr.ty, types) {
        Z3TypeCategory::Int => Some(ResolvedScalarValue::Int(resolved_to_z3_int_with_catalog(
            expr, body, types, callables, variants, vars,
        )?)),
        Z3TypeCategory::Real => Some(ResolvedScalarValue::Real(resolved_to_z3_real_with_catalog(
            expr, body, types, callables, variants, vars,
        )?)),
        Z3TypeCategory::Bool => Some(ResolvedScalarValue::Bool(resolved_to_z3_bool_with_catalog(
            expr, body, types, callables, variants, vars,
        )?)),
    }
}

fn resolved_variant_tag_int(
    value: &ResolvedExpr,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Int> {
    match &value.kind {
        ResolvedExprKind::Call(call) => {
            if let ResolvedCallee::Constructor(variant) = &call.callee {
                if call.result != value.ty
                    || call.permission.is_some()
                    || !call.effects.is_empty()
                    || !call.session.is_empty()
                {
                    return None;
                }
                let tag = resolved_variant_tag_number(&value.ty, variant, types, variants)?;
                return Some(Z3Int::from_i64(tag));
            }
            if call.result == value.ty {
                if let Some(variant) =
                    resolved_builtin_sum_constructor_variant(call, types, variants)
                {
                    let tag = resolved_variant_tag_number(&value.ty, &variant, types, variants)?;
                    return Some(Z3Int::from_i64(tag));
                }
            }
            if matches!(&call.callee, ResolvedCallee::Transition(_)) {
                if call.result != value.ty {
                    return None;
                }
                let (callee, _frame, _try_value, _return_value, success) =
                    resolved_transition_evaluation(call, body, types, callables, variants, vars)?;
                let (ok_variant, err_variant) =
                    resolved_transition_result_variants(call, &callee, types, variants)?;
                let ok_tag =
                    resolved_variant_tag_number(&call.result, &ok_variant, types, variants)?;
                let err_tag =
                    resolved_variant_tag_number(&call.result, &err_variant, types, variants)?;
                // `?` propagates the inner Err into the transition's outer
                // Err. Its Result tag domain is a checker-owned type premise,
                // so no invalid-tag behavior is silently discarded here.
                return Some(success.ite(&Z3Int::from_i64(ok_tag), &Z3Int::from_i64(err_tag)));
            }
            if let ResolvedCallee::Function(_) = &call.callee {
                let (tail, callee_body, mut callee_vars) = inline_pure_call_tail(
                    call,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                    &mut BTreeSet::new(),
                )?;
                return resolved_variant_tag_int(
                    &tail,
                    &callee_body,
                    types,
                    callables,
                    variants,
                    &mut callee_vars,
                );
            }
            None
        }
        ResolvedExprKind::Load(_) | ResolvedExprKind::Project { .. } => {
            let key = value_base_key(value, body)?;
            Some(vars.get_or_create_int(&format!("{key}.tag")))
        }
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            let cond = resolved_to_z3_bool_with_catalog(
                condition, body, types, callables, variants, vars,
            )?;
            let then_tag =
                resolved_block_tail_tag_int(then_block, body, types, callables, variants, vars)?;
            let else_tag =
                resolved_block_tail_tag_int(else_block, body, types, callables, variants, vars)?;
            Some(cond.ite(&then_tag, &else_tag))
        }
        ResolvedExprKind::Block(block) => {
            resolved_block_stmts_with_catalog(block, body, types, callables, variants, vars)?;
            resolved_variant_tag_int(
                block.result.as_deref()?,
                body,
                types,
                callables,
                variants,
                vars,
            )
        }
        _ => None,
    }
}

fn resolved_block_tail_tag_int(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Int> {
    resolved_block_stmts_with_catalog(block, body, types, callables, variants, vars)?;
    resolved_variant_tag_int(
        block.result.as_deref()?,
        body,
        types,
        callables,
        variants,
        vars,
    )
}

/// The tag guard used by both match-arm encoding and call-definedness VCs.
/// The helper never equates different constructors and refuses to invent a
/// tag for an unsupported sum type.
pub(crate) fn resolved_variant_tag_condition(
    value: &ResolvedExpr,
    expected_variant: &NodeId,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Bool> {
    let expected = resolved_variant_tag_number(&value.ty, expected_variant, types, variants)?;
    let actual = resolved_variant_tag_int(value, body, types, callables, variants, vars)?;
    Some(actual.eq(&Z3Int::from_i64(expected)))
}

pub(crate) fn resolved_variant_tag_domain_condition(
    value: &ResolvedExpr,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Bool> {
    let allowed = match types.get(&value.ty)? {
        ResolvedType::Option(_) | ResolvedType::Result { .. } => vec![0, 1],
        ResolvedType::Nominal {
            item, arguments, ..
        } if item.as_str() == "builtin:type:Option" && arguments.len() == 1 => vec![0, 1],
        ResolvedType::Nominal {
            item, arguments, ..
        } if item.as_str() == "builtin:type:Result" && arguments.len() == 2 => vec![0, 1],
        ResolvedType::Nominal { item, .. } => {
            let mut owner_variants = variants
                .values()
                .filter(|schema| schema.owner == NodeId(item.as_str().to_string()))
                .collect::<Vec<_>>();
            owner_variants.sort_by(|left, right| left.node_id.cmp(&right.node_id));
            if owner_variants.is_empty() {
                // The checker-owned variant catalog is complete. A nominal
                // with no variants is a record/newtype, so it carries no sum
                // tag that could be outside a finite domain.
                return Some(Z3Bool::from_bool(true));
            }
            let _ = resolved_variant_tag_int(value, body, types, callables, variants, vars)?;
            (0..owner_variants.len())
                .map(i64::try_from)
                .collect::<Result<Vec<_>, _>>()
                .ok()?
        }
        ResolvedType::Tuple(_)
        | ResolvedType::Primitive(_)
        | ResolvedType::GenericParameter(_)
        | ResolvedType::Reference { .. }
        | ResolvedType::Capability(_)
        | ResolvedType::FlowStateSet { .. }
        | ResolvedType::Function { .. }
        | ResolvedType::CBuffer(_)
        | ResolvedType::Ownership { .. }
        | ResolvedType::Newtype { .. }
        | ResolvedType::Array { .. }
        | ResolvedType::Slice(_)
        | ResolvedType::Trait { .. }
        | ResolvedType::RawPointer { .. }
        | ResolvedType::DynamicAny { .. } => return Some(Z3Bool::from_bool(true)),
    };
    let tag = resolved_variant_tag_int(value, body, types, callables, variants, vars)?;
    let alternatives = allowed
        .iter()
        .map(|tag_value| tag.eq(&Z3Int::from_i64(*tag_value)))
        .collect::<Vec<_>>();
    let refs = alternatives.iter().collect::<Vec<_>>();
    Some(Z3Bool::or(&refs))
}

fn resolved_variant_payload_scalar(
    value: &ResolvedExpr,
    expected_variant: &NodeId,
    member: &NodeId,
    expected_type: &crate::core::ir::ResolvedTypeId,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<ResolvedScalarValue> {
    match &value.kind {
        ResolvedExprKind::Call(call) => {
            if let ResolvedCallee::Constructor(variant) = &call.callee {
                if variant != expected_variant
                    || call.result != value.ty
                    || call.permission.is_some()
                    || !call.effects.is_empty()
                    || !call.session.is_empty()
                {
                    return None;
                }
                let argument = call
                    .arguments
                    .iter()
                    .find(|argument| argument.parameter.0 == *member)
                    .or_else(|| (call.arguments.len() == 1).then(|| &call.arguments[0]))?;
                if argument.value.ty != *expected_type {
                    return None;
                }
                return scalar_value_of_expr_with_catalog(
                    &argument.value,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                );
            }
            if call.result == value.ty {
                if let Some(argument) = resolved_builtin_sum_constructor_payload(
                    call,
                    expected_variant,
                    member,
                    types,
                    variants,
                ) {
                    if argument.ty != *expected_type {
                        return None;
                    }
                    return scalar_value_of_expr_with_catalog(
                        argument, body, types, callables, variants, vars,
                    );
                }
            }
            if let ResolvedCallee::Function(_) = &call.callee {
                let (tail, callee_body, mut callee_vars) = inline_pure_call_tail(
                    call,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                    &mut BTreeSet::new(),
                )?;
                return resolved_variant_payload_scalar(
                    &tail,
                    expected_variant,
                    member,
                    expected_type,
                    &callee_body,
                    types,
                    callables,
                    variants,
                    &mut callee_vars,
                );
            }
            None
        }
        ResolvedExprKind::Load(_) | ResolvedExprKind::Project { .. } => {
            let base = value_base_key(value, body)?;
            let path = [ResolvedValuePathPart::VariantPayload {
                variant: expected_variant.clone(),
                member: member.clone(),
            }];
            scalar_value_at_key(&append_value_path(&base, &path), expected_type, types, vars)
        }
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            let cond = resolved_to_z3_bool_with_catalog(
                condition, body, types, callables, variants, vars,
            )?;
            let then_value = resolved_block_payload_scalar_or_inactive(
                then_block,
                expected_variant,
                member,
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            )?;
            let else_value = resolved_block_payload_scalar_or_inactive(
                else_block,
                expected_variant,
                member,
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            )?;
            scalar_ite(cond, then_value, else_value)
        }
        ResolvedExprKind::Block(block) => {
            resolved_block_stmts_with_catalog(block, body, types, callables, variants, vars)?;
            resolved_variant_payload_scalar(
                block.result.as_deref()?,
                expected_variant,
                member,
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            )
        }
        _ => None,
    }
}

fn scalar_ite(
    condition: Z3Bool,
    then_value: ResolvedScalarValue,
    else_value: ResolvedScalarValue,
) -> Option<ResolvedScalarValue> {
    match (then_value, else_value) {
        (ResolvedScalarValue::Int(then_value), ResolvedScalarValue::Int(else_value)) => Some(
            ResolvedScalarValue::Int(condition.ite(&then_value, &else_value)),
        ),
        (ResolvedScalarValue::Real(then_value), ResolvedScalarValue::Real(else_value)) => Some(
            ResolvedScalarValue::Real(condition.ite(&then_value, &else_value)),
        ),
        (ResolvedScalarValue::Bool(then_value), ResolvedScalarValue::Bool(else_value)) => Some(
            ResolvedScalarValue::Bool(condition.ite(&then_value, &else_value)),
        ),
        _ => None,
    }
}

pub(crate) fn resolved_variant_payload_int(
    value: &ResolvedExpr,
    expected_variant: &NodeId,
    member: &NodeId,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Int> {
    let payload_type =
        resolved_variant_member_type(&value.ty, expected_variant, member, types, variants)?;
    resolved_variant_payload_int_as(
        value,
        expected_variant,
        member,
        &payload_type,
        body,
        types,
        callables,
        variants,
        vars,
    )
}

/// Encode an integer payload using a checker-substituted concrete payload
/// type. Generic wrappers may use this only after their call-site type
/// arguments have been substituted and the caller's actual Option/Result
/// value is passed here.
pub(crate) fn resolved_variant_payload_int_as(
    value: &ResolvedExpr,
    expected_variant: &NodeId,
    member: &NodeId,
    payload_type: &crate::core::ir::ResolvedTypeId,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Int> {
    if resolved_variant_member_type(&value.ty, expected_variant, member, types, variants)?
        != *payload_type
    {
        return None;
    }
    match resolved_variant_payload_scalar(
        value,
        expected_variant,
        member,
        payload_type,
        body,
        types,
        callables,
        variants,
        vars,
    )? {
        ResolvedScalarValue::Int(value) => Some(value),
        _ => None,
    }
}

pub(crate) fn resolved_variant_payload_bool(
    value: &ResolvedExpr,
    expected_variant: &NodeId,
    member: &NodeId,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Bool> {
    let payload_type =
        resolved_variant_member_type(&value.ty, expected_variant, member, types, variants)?;
    resolved_variant_payload_bool_as(
        value,
        expected_variant,
        member,
        &payload_type,
        body,
        types,
        callables,
        variants,
        vars,
    )
}

pub(crate) fn resolved_variant_payload_bool_as(
    value: &ResolvedExpr,
    expected_variant: &NodeId,
    member: &NodeId,
    payload_type: &crate::core::ir::ResolvedTypeId,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Bool> {
    if resolved_variant_member_type(&value.ty, expected_variant, member, types, variants)?
        != *payload_type
    {
        return None;
    }
    match resolved_variant_payload_scalar(
        value,
        expected_variant,
        member,
        payload_type,
        body,
        types,
        callables,
        variants,
        vars,
    )? {
        ResolvedScalarValue::Bool(value) => Some(value),
        _ => None,
    }
}

pub(crate) fn resolved_variant_payload_real(
    value: &ResolvedExpr,
    expected_variant: &NodeId,
    member: &NodeId,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Real> {
    let payload_type =
        resolved_variant_member_type(&value.ty, expected_variant, member, types, variants)?;
    resolved_variant_payload_real_as(
        value,
        expected_variant,
        member,
        &payload_type,
        body,
        types,
        callables,
        variants,
        vars,
    )
}

pub(crate) fn resolved_variant_payload_real_as(
    value: &ResolvedExpr,
    expected_variant: &NodeId,
    member: &NodeId,
    payload_type: &crate::core::ir::ResolvedTypeId,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Real> {
    if resolved_variant_member_type(&value.ty, expected_variant, member, types, variants)?
        != *payload_type
    {
        return None;
    }
    match resolved_variant_payload_scalar(
        value,
        expected_variant,
        member,
        payload_type,
        body,
        types,
        callables,
        variants,
        vars,
    )? {
        ResolvedScalarValue::Real(value) => Some(value),
        _ => None,
    }
}

fn inline_pure_call_tail(
    call: &crate::core::ir::ResolvedCall,
    caller_body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &Z3VarMap,
    active: &mut BTreeSet<NodeId>,
) -> Option<(ResolvedExpr, ResolvedBody, Z3VarMap)> {
    let ResolvedCallee::Function(owner) = &call.callee else {
        return None;
    };
    if active.contains(owner)
        || !call.effects.is_empty()
        || !call.session.is_empty()
        || call.permission.is_some()
        || !call.type_arguments.is_empty()
    {
        return None;
    }
    active.insert(owner.clone());
    let result = (|| {
        let callable = callables.get(owner)?;
        if !callable.signature.effects.is_empty()
            || !callable.signature.generic_parameters.is_empty()
            || !callable.contracts.is_empty()
            || !callable.body.captures.is_empty()
            || call.result != callable.signature.result
            || callable.body.parameters.len() != callable.signature.parameters.len()
            || call.arguments.len() != callable.signature.parameters.len()
            || callable
                .body
                .root
                .result
                .as_deref()
                .is_none_or(|result| result.ty != callable.signature.result)
            || callable
                .body
                .resolved_calls
                .values()
                .any(|callee| matches!(callee, ResolvedCallee::Function(_)))
            || callable.body.root.statements.iter().any(|statement| {
                !matches!(
                    &statement.kind,
                    ResolvedStmtKind::Bind {
                        pattern,
                        initializer: Some(_),
                    } if matches!(
                        &pattern.kind,
                        ResolvedPatternKind::Binding {
                            by_reference: None,
                            ..
                        }
                    )
                )
            })
        {
            return None;
        }
        let mut callee_vars = vars.clone();
        for (index, parameter) in callable.signature.parameters.iter().enumerate() {
            if parameter.has_default {
                return None;
            }
            let local_id = callable.body.parameters.get(index)?;
            let local = callable.body.locals.get(local_id)?;
            let argument = call
                .arguments
                .iter()
                .find(|argument| argument.parameter == parameter.id)?;
            if conversion_may_trap(argument.conversion.kind) {
                return None;
            }
            bind_value_to_local(
                &argument.value,
                caller_body,
                local,
                types,
                callables,
                variants,
                &mut callee_vars,
                active,
            )?;
        }
        resolved_block_stmts_with_catalog(
            &callable.body.root,
            &callable.body,
            types,
            callables,
            variants,
            &mut callee_vars,
        )?;
        Some((
            callable.body.root.result.as_deref()?.clone(),
            callable.body.clone(),
            callee_vars,
        ))
    })();
    active.remove(owner);
    result
}

fn resolved_variant_member_type(
    ty: &crate::core::ir::ResolvedTypeId,
    variant: &NodeId,
    member: &NodeId,
    types: &ResolvedTypeTable,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
) -> Option<crate::core::ir::ResolvedTypeId> {
    if let Some(builtin) = builtin_variant_member_type(ty, variant, member, types) {
        return Some(builtin);
    }
    let Some(ResolvedType::Nominal { item, .. }) = types.get(ty) else {
        return None;
    };
    let schema = variants.get(variant)?;
    if schema.owner.0.as_str() != item.as_str() {
        return None;
    }
    schema
        .members
        .iter()
        .find(|candidate| &candidate.node_id == member)
        .map(|candidate| candidate.ty.clone())
}

/// Drops do not affect scalar verification state only for inert strings and
/// a List whose exact literal length was installed by the value encoder.
/// Projected, linear, or otherwise opaque resources remain outside the slice.
pub(crate) fn resolved_drop_is_value_neutral(
    places: &[ResolvedPlace],
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &Z3VarMap,
) -> bool {
    places.iter().all(|place| {
        if !place.projections.is_empty() {
            return false;
        }
        let Some(local) = body.locals.get(&place.base) else {
            return false;
        };
        match types.get(&local.ty) {
            Some(ResolvedType::Primitive(PrimitiveType::String)) => true,
            Some(ResolvedType::Nominal {
                item,
                arguments,
                is_linear,
            }) if item.as_str() == "builtin:type:List"
                && !*is_linear
                && arguments.len() == 1
                && value_neutral_collection_element(&arguments[0], types) =>
            {
                vars.get_list_len(&local_var_key(&local.id, &local.display_name))
                    .is_some()
            }
            Some(ResolvedType::Nominal {
                item,
                arguments,
                is_linear,
            }) if item.as_str() == "builtin:type:Set"
                && !*is_linear
                && arguments.len() == 1
                && resolved_set_element_sort(&arguments[0], types).is_some() =>
            {
                vars.get_set_var(&local_var_key(&local.id, &local.display_name))
                    .is_some()
            }
            _ => false,
        }
    })
}

fn value_neutral_collection_element(
    ty: &crate::core::ir::ResolvedTypeId,
    types: &ResolvedTypeTable,
) -> bool {
    match types.get(ty) {
        Some(ResolvedType::Primitive(
            PrimitiveType::Unit
            | PrimitiveType::String
            | PrimitiveType::Bool
            | PrimitiveType::I8
            | PrimitiveType::I16
            | PrimitiveType::I32
            | PrimitiveType::I64
            | PrimitiveType::I128
            | PrimitiveType::U8
            | PrimitiveType::U16
            | PrimitiveType::U32
            | PrimitiveType::U64
            | PrimitiveType::U128
            | PrimitiveType::Isize
            | PrimitiveType::Usize
            | PrimitiveType::F32
            | PrimitiveType::F64
            | PrimitiveType::Char,
        )) => true,
        Some(ResolvedType::Option(inner)) => value_neutral_collection_element(inner, types),
        Some(ResolvedType::Result { ok, error }) => {
            value_neutral_collection_element(ok, types)
                && value_neutral_collection_element(error, types)
        }
        Some(ResolvedType::Tuple(items)) => items
            .iter()
            .all(|item| value_neutral_collection_element(item, types)),
        Some(ResolvedType::Nominal {
            item,
            arguments,
            is_linear,
        }) if (item.as_str() == "builtin:type:List" || item.as_str() == "builtin:type:Set")
            && !*is_linear
            && arguments.len() == 1 =>
        {
            value_neutral_collection_element(&arguments[0], types)
        }
        _ => false,
    }
}

#[derive(Clone)]
enum ResolvedSumShape {
    Option,
    Result,
    User(NodeId),
}

fn resolved_sum_shape(
    ty: &crate::core::ir::ResolvedTypeId,
    types: &ResolvedTypeTable,
) -> Option<ResolvedSumShape> {
    match types.get(ty)? {
        ResolvedType::Option(_) => Some(ResolvedSumShape::Option),
        ResolvedType::Result { .. } => Some(ResolvedSumShape::Result),
        ResolvedType::Nominal {
            item, arguments, ..
        } if item.as_str() == "builtin:type:Option" && arguments.len() == 1 => {
            Some(ResolvedSumShape::Option)
        }
        ResolvedType::Nominal {
            item, arguments, ..
        } if item.as_str() == "builtin:type:Result" && arguments.len() == 2 => {
            Some(ResolvedSumShape::Result)
        }
        ResolvedType::Nominal { item, .. } => {
            Some(ResolvedSumShape::User(NodeId(item.as_str().into())))
        }
        _ => None,
    }
}

fn resolved_sum_shapes_match(
    left: &crate::core::ir::ResolvedTypeId,
    right: &crate::core::ir::ResolvedTypeId,
    types: &ResolvedTypeTable,
) -> bool {
    matches!(
        (
            resolved_sum_shape(left, types),
            resolved_sum_shape(right, types)
        ),
        (
            Some(ResolvedSumShape::Option),
            Some(ResolvedSumShape::Option)
        ) | (
            Some(ResolvedSumShape::Result),
            Some(ResolvedSumShape::Result)
        )
    ) || matches!(
        (resolved_sum_shape(left, types), resolved_sum_shape(right, types)),
        (Some(ResolvedSumShape::User(left)), Some(ResolvedSumShape::User(right))) if left == right
    )
}

fn resolved_sum_variants(
    ty: &crate::core::ir::ResolvedTypeId,
    types: &ResolvedTypeTable,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
) -> Option<Vec<(NodeId, Vec<(NodeId, crate::core::ir::ResolvedTypeId)>)>> {
    match (resolved_sum_shape(ty, types)?, types.get(ty)?) {
        (ResolvedSumShape::Option, ResolvedType::Option(inner)) => Some(vec![
            (NodeId("builtin:variant:Option::None".into()), Vec::new()),
            (
                NodeId("builtin:variant:Option::Some".into()),
                vec![(
                    NodeId("builtin:variant:Option::Some/payload:0".into()),
                    inner.clone(),
                )],
            ),
        ]),
        (ResolvedSumShape::Option, ResolvedType::Nominal { arguments, .. })
            if arguments.len() == 1 =>
        {
            Some(vec![
                (NodeId("builtin:variant:Option::None".into()), Vec::new()),
                (
                    NodeId("builtin:variant:Option::Some".into()),
                    vec![(
                        NodeId("builtin:variant:Option::Some/payload:0".into()),
                        arguments[0].clone(),
                    )],
                ),
            ])
        }
        (ResolvedSumShape::Result, ResolvedType::Result { ok, error }) => Some(vec![
            (
                NodeId("builtin:variant:Result::Ok".into()),
                vec![(
                    NodeId("builtin:variant:Result::Ok/payload:0".into()),
                    ok.clone(),
                )],
            ),
            (
                NodeId("builtin:variant:Result::Err".into()),
                vec![(
                    NodeId("builtin:variant:Result::Err/payload:0".into()),
                    error.clone(),
                )],
            ),
        ]),
        (ResolvedSumShape::Result, ResolvedType::Nominal { arguments, .. })
            if arguments.len() == 2 =>
        {
            Some(vec![
                (
                    NodeId("builtin:variant:Result::Ok".into()),
                    vec![(
                        NodeId("builtin:variant:Result::Ok/payload:0".into()),
                        arguments[0].clone(),
                    )],
                ),
                (
                    NodeId("builtin:variant:Result::Err".into()),
                    vec![(
                        NodeId("builtin:variant:Result::Err/payload:0".into()),
                        arguments[1].clone(),
                    )],
                ),
            ])
        }
        (ResolvedSumShape::User(owner), ResolvedType::Nominal { .. }) => {
            let mut output = variants
                .values()
                .filter(|schema| schema.owner == owner)
                .map(|schema| {
                    (
                        schema.node_id.clone(),
                        schema
                            .members
                            .iter()
                            .map(|member| (member.node_id.clone(), member.ty.clone()))
                            .collect(),
                    )
                })
                .collect::<Vec<_>>();
            output.sort_by(|left, right| left.0.cmp(&right.0));
            (!output.is_empty()).then_some(output)
        }
        _ => None,
    }
}

/// Resolve and validate the typed shape of the builtin Option/Result
/// constructors.  These calls are represented as Builtin callees in Resolved
/// IR, but they are value constructors rather than opaque runtime builtins.
fn resolved_builtin_sum_constructor_variant(
    call: &crate::core::ir::ResolvedCall,
    types: &ResolvedTypeTable,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
) -> Option<NodeId> {
    use crate::core::ir::ResolvedCallee;

    let ResolvedCallee::Builtin(builtin) = &call.callee else {
        return None;
    };
    let variant = match builtin.as_str() {
        "Some" => NodeId("builtin:variant:Option::Some".into()),
        "None" => NodeId("builtin:variant:Option::None".into()),
        "Ok" => NodeId("builtin:variant:Result::Ok".into()),
        "Err" => NodeId("builtin:variant:Result::Err".into()),
        _ => return None,
    };
    if call.permission.is_some()
        || !call.effects.is_empty()
        || !call.session.is_empty()
        || !call.type_arguments.is_empty()
    {
        return None;
    }
    resolved_variant_tag_number(&call.result, &variant, types, variants)?;
    let members = resolved_sum_variants(&call.result, types, variants)?
        .into_iter()
        .find_map(|(candidate, members)| (candidate == variant).then_some(members))?;
    let member_count = members.len();
    if member_count != call.arguments.len() {
        return None;
    }
    let mut seen = BTreeSet::new();
    for (member, member_type) in members {
        let (index, argument) = call
            .arguments
            .iter()
            .enumerate()
            .find(|(_, argument)| argument.parameter.0 == member)
            .or_else(|| {
                (call.arguments.len() == 1 && member_count == 1).then(|| (0, &call.arguments[0]))
            })?;
        if !seen.insert(index)
            || argument.value.ty != member_type
            || argument.conversion.kind != CheckedConversionKind::Identity
            || argument.conversion.from != argument.value.ty
            || argument.conversion.to != member_type
            || !argument.value.effects.is_empty()
        {
            return None;
        }
    }
    if seen.len() != call.arguments.len() {
        return None;
    }
    Some(variant)
}

fn resolved_builtin_sum_constructor_payload<'a>(
    call: &'a crate::core::ir::ResolvedCall,
    expected_variant: &NodeId,
    member: &NodeId,
    types: &ResolvedTypeTable,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
) -> Option<&'a ResolvedExpr> {
    let actual_variant = resolved_builtin_sum_constructor_variant(call, types, variants)?;
    if &actual_variant != expected_variant {
        return None;
    }
    let payload_type = resolved_sum_variants(&call.result, types, variants)?
        .into_iter()
        .find_map(|(variant, members)| {
            (variant == actual_variant)
                .then(|| {
                    members
                        .into_iter()
                        .find(|(candidate, _)| candidate == member)
                })
                .flatten()
                .map(|(_, ty)| ty)
        })?;
    let argument = call
        .arguments
        .iter()
        .find(|argument| argument.parameter.0 == *member)
        .or_else(|| (call.arguments.len() == 1).then(|| &call.arguments[0]))?;
    (argument.value.ty == payload_type).then_some(&argument.value)
}

/// Preserve already-encoded typed leaf facts when a checker-owned aggregate
/// local is forwarded into a newly-bound local.  This copies only facts that
/// already exist under the exact source projection prefix; it never creates
/// unconstrained aliases or derives facts from a constructor name.
fn copy_known_value_path_facts(source_key: &str, target_key: &str, vars: &mut Z3VarMap) {
    let source_vars = vars.clone();
    copy_known_value_path_facts_between(&source_vars, source_key, target_key, vars);
}

/// Copy only already encoded facts under one exact typed value-path namespace
/// between verifier frames. Transition bodies use a temporary frame, so
/// aggregate result bindings must explicitly transfer the selected payload
/// terms back to their caller without leaking unrelated callee locals.
fn copy_known_value_path_facts_between(
    source_vars: &Z3VarMap,
    source_key: &str,
    target_key: &str,
    target_vars: &mut Z3VarMap,
) {
    let source_prefix = format!("{source_key}.");
    let remap = |key: &str| {
        if key == source_key {
            Some(target_key.to_string())
        } else {
            key.strip_prefix(&source_prefix)
                .map(|suffix| format!("{target_key}.{suffix}"))
        }
    };

    let ints = source_vars
        .int_vars
        .iter()
        .filter_map(|(key, value)| remap(key).map(|target| (target, value.clone())))
        .collect::<Vec<_>>();
    let reals = source_vars
        .real_vars
        .iter()
        .filter_map(|(key, value)| remap(key).map(|target| (target, value.clone())))
        .collect::<Vec<_>>();
    let bools = source_vars
        .bool_vars
        .iter()
        .filter_map(|(key, value)| remap(key).map(|target| (target, value.clone())))
        .collect::<Vec<_>>();
    let strings = source_vars
        .string_vars
        .iter()
        .filter_map(|(key, value)| remap(key).map(|target| (target, value.clone())))
        .collect::<Vec<_>>();
    let string_lengths = source_vars
        .string_len
        .iter()
        .filter_map(|(key, value)| remap(key).map(|target| (target, value.clone())))
        .collect::<Vec<_>>();
    let string_nonempty = source_vars
        .string_nonempty
        .iter()
        .filter_map(|(key, value)| remap(key).map(|target| (target, value.clone())))
        .collect::<Vec<_>>();
    let list_lengths = source_vars
        .list_len
        .iter()
        .filter_map(|(key, value)| remap(key).map(|target| (target, value.clone())))
        .collect::<Vec<_>>();
    let set_vars = source_vars
        .set_vars
        .iter()
        .filter_map(|(key, value)| remap(key).map(|target| (target, value.clone())))
        .collect::<Vec<_>>();

    for (key, value) in ints {
        target_vars.insert_int(key, value);
    }
    for (key, value) in reals {
        target_vars.insert_real(key, value);
    }
    for (key, value) in bools {
        target_vars.insert_bool(key, value);
    }
    for (key, value) in strings {
        target_vars.insert_string_var(key, value);
    }
    for (key, value) in string_lengths {
        target_vars.insert_string_len(key, value);
    }
    for (key, value) in string_nonempty {
        target_vars.insert_string_nonempty(key, value);
    }
    for (key, value) in list_lengths {
        target_vars.insert_list_len(key, value);
    }
    for (key, value) in set_vars {
        target_vars.insert_set_var(key, value);
    }
}

fn resolved_type_at_path(
    ty: &crate::core::ir::ResolvedTypeId,
    path: &[ResolvedValuePathPart],
    types: &ResolvedTypeTable,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
) -> Option<crate::core::ir::ResolvedTypeId> {
    let Some((first, rest)) = path.split_first() else {
        return Some(ty.clone());
    };
    let next = match (first, types.get(ty)?) {
        (ResolvedValuePathPart::Tuple(index), ResolvedType::Tuple(items)) => {
            items.get(*index)?.clone()
        }
        (ResolvedValuePathPart::VariantPayload { variant, member }, _) => {
            resolved_variant_member_type(ty, variant, member, types, variants)?
        }
        // ResolvedTypeTable intentionally carries no record field metadata.
        // Record-literal and typed Project cases are resolved from their
        // checker-owned expression nodes below.
        _ => return None,
    };
    resolved_type_at_path(&next, rest, types, variants)
}

fn resolved_flow_state_type_at_path(
    root_type: &crate::core::ir::ResolvedTypeId,
    path: &[ResolvedValuePathPart],
    types: &ResolvedTypeTable,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    flow_state_fields: Option<&ResolvedFlowStateFieldCatalog>,
) -> Option<crate::core::ir::ResolvedTypeId> {
    let mut current = root_type.clone();
    for part in path {
        current = match (part, types.get(&current)?) {
            (ResolvedValuePathPart::Tuple(index), ResolvedType::Tuple(items)) => {
                items.get(*index)?.clone()
            }
            (ResolvedValuePathPart::VariantPayload { variant, member }, _) => {
                resolved_variant_member_type(&current, variant, member, types, variants)?
            }
            (
                ResolvedValuePathPart::Field(field),
                ResolvedType::Nominal {
                    item,
                    is_linear: true,
                    ..
                },
            ) => flow_state_fields?
                .get(&NodeId(item.as_str().to_string()))?
                .get(field)?
                .clone(),
            _ => return None,
        };
    }
    Some(current)
}

fn flow_state_pattern_fields<'a>(
    pattern: &crate::core::ir::ResolvedPattern,
    variant: &NodeId,
    fields: &[(NodeId, crate::core::ir::ResolvedPattern)],
    types: &ResolvedTypeTable,
    flow_state_fields: Option<&'a ResolvedFlowStateFieldCatalog>,
) -> Option<&'a BTreeMap<NodeId, crate::core::ir::ResolvedTypeId>> {
    let ResolvedType::Nominal {
        item,
        is_linear: true,
        ..
    } = types.get(&pattern.ty)?
    else {
        return None;
    };
    let owner = NodeId(item.as_str().to_string());
    if &owner != variant {
        return None;
    }
    let schema = flow_state_fields?.get(&owner)?;
    if fields.len() != schema.len() {
        return None;
    }
    let mut seen = BTreeSet::new();
    for (field, child) in fields {
        if !seen.insert(field)
            || schema.get(field) != Some(&child.ty)
            || types.get(&child.ty).is_none()
        {
            return None;
        }
    }
    (seen.len() == schema.len()).then_some(schema)
}

fn flow_state_pattern_candidate(
    pattern: &crate::core::ir::ResolvedPattern,
    variant: &NodeId,
    types: &ResolvedTypeTable,
) -> bool {
    let is_flow_state_type = matches!(
        types.get(&pattern.ty),
        Some(ResolvedType::Nominal { item, .. }) if item.as_str().starts_with("state:")
    );
    is_flow_state_type || variant.0.starts_with("state:")
}

fn resolved_pattern_variant_tag_at_path(
    value: &ResolvedExpr,
    path: &[ResolvedValuePathPart],
    expected_type: &crate::core::ir::ResolvedTypeId,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    flow_state_fields: Option<&ResolvedFlowStateFieldCatalog>,
    vars: &mut Z3VarMap,
) -> Option<Z3Int> {
    if let Some(source_type) =
        resolved_flow_state_type_at_path(&value.ty, path, types, variants, flow_state_fields)
    {
        if &source_type != expected_type {
            return None;
        }
        if path.is_empty() {
            return resolved_variant_tag_int(value, body, types, callables, variants, vars);
        }
        if matches!(value.kind, ResolvedExprKind::Load(_)) {
            let key = value_base_key(value, body)?;
            return Some(vars.get_or_create_int(&format!("{}.tag", append_value_path(&key, path))));
        }
    }
    resolved_variant_tag_at_path(
        value,
        path,
        expected_type,
        body,
        types,
        callables,
        variants,
        vars,
    )
}

fn bind_flow_state_scalar_at_path(
    value: &ResolvedExpr,
    path: &[ResolvedValuePathPart],
    target_type: &crate::core::ir::ResolvedTypeId,
    target_key: &str,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
) -> Option<()> {
    let key = append_value_path(&value_base_key(value, body)?, path);
    match types.get(target_type)? {
        ResolvedType::Primitive(PrimitiveType::String | PrimitiveType::Unit) => Some(()),
        ResolvedType::Primitive(_) if resolved_scalar_leaf_supported(target_type, types) => {
            match scalar_value_at_key(&key, target_type, types, vars)? {
                ResolvedScalarValue::Int(value) => vars.insert_int(target_key, value),
                ResolvedScalarValue::Real(value) => vars.insert_real(target_key, value),
                ResolvedScalarValue::Bool(value) => vars.insert_bool(target_key, value),
            }
            Some(())
        }
        _ => None,
    }
}

fn resolved_expression_type_at_path(
    value: &ResolvedExpr,
    path: &[ResolvedValuePathPart],
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
) -> Option<crate::core::ir::ResolvedTypeId> {
    let Some((first, rest)) = path.split_first() else {
        return Some(value.ty.clone());
    };
    match &value.kind {
        ResolvedExprKind::Tuple(items) => {
            let ResolvedValuePathPart::Tuple(index) = first else {
                return None;
            };
            resolved_expression_type_at_path(
                items.get(*index)?,
                rest,
                body,
                types,
                callables,
                variants,
            )
        }
        ResolvedExprKind::Record {
            fields, rest: base, ..
        } => {
            let ResolvedValuePathPart::Field(field_id) = first else {
                return None;
            };
            if let Some(field) = fields.iter().find(|field| &field.field == field_id) {
                return resolved_expression_type_at_path(
                    &field.value,
                    rest,
                    body,
                    types,
                    callables,
                    variants,
                );
            }
            resolved_expression_type_at_path(
                base.as_deref()?,
                path,
                body,
                types,
                callables,
                variants,
            )
        }
        ResolvedExprKind::Call(call) => {
            if matches!(&call.callee, ResolvedCallee::Transition(_)) {
                return resolved_transition_expression_type_at_path(
                    call, path, body, types, callables, variants,
                );
            }
            if call.result == value.ty {
                if let Some(ResolvedValuePathPart::VariantPayload { variant, member }) =
                    path.first()
                {
                    if let Some(argument) = resolved_builtin_sum_constructor_payload(
                        call, variant, member, types, variants,
                    ) {
                        return resolved_expression_type_at_path(
                            argument, rest, body, types, callables, variants,
                        );
                    }
                }
            }
            if let ResolvedCallee::Constructor(actual_variant) = &call.callee {
                if call.result != value.ty
                    || call.permission.is_some()
                    || !call.effects.is_empty()
                    || !call.session.is_empty()
                {
                    return None;
                }
                let ResolvedValuePathPart::VariantPayload { variant, member } = first else {
                    return resolved_type_at_path(&value.ty, path, types, variants);
                };
                if actual_variant != variant {
                    return None;
                }
                let argument = call
                    .arguments
                    .iter()
                    .find(|argument| argument.parameter.0 == *member)
                    .or_else(|| (call.arguments.len() == 1).then(|| &call.arguments[0]))?;
                if resolved_variant_member_type(&value.ty, actual_variant, member, types, variants)?
                    != argument.value.ty
                {
                    return None;
                }
                return resolved_expression_type_at_path(
                    &argument.value,
                    rest,
                    body,
                    types,
                    callables,
                    variants,
                );
            }
            if matches!(&call.callee, ResolvedCallee::Function(_)) {
                if call.result != value.ty {
                    return None;
                }
                return resolved_type_at_path(&value.ty, path, types, variants);
            }
            None
        }
        ResolvedExprKind::If {
            then_block,
            else_block,
            ..
        } => {
            let left = resolved_block_expression_type_at_path(
                then_block, path, body, types, callables, variants,
            )?;
            let right = resolved_block_expression_type_at_path(
                else_block, path, body, types, callables, variants,
            )?;
            (left == right).then_some(left)
        }
        ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
            resolved_block_expression_type_at_path(block, path, body, types, callables, variants)
        }
        // Loads and projections are already checker-typed values. Tuple and
        // sum payload paths can be followed from their canonical type; record
        // field paths remain unavailable unless a literal supplies the field.
        ResolvedExprKind::Load(_) | ResolvedExprKind::Project { .. } => {
            resolved_type_at_path(&value.ty, path, types, variants)
        }
        _ => None,
    }
}

fn resolved_block_expression_type_at_path(
    block: &ResolvedBlock,
    path: &[ResolvedValuePathPart],
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
) -> Option<crate::core::ir::ResolvedTypeId> {
    resolved_expression_type_at_path(
        block.result.as_deref()?,
        path,
        body,
        types,
        callables,
        variants,
    )
}

fn resolved_constructor_at_path(
    value: &ResolvedExpr,
    path: &[ResolvedValuePathPart],
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
) -> Option<NodeId> {
    if path.is_empty() {
        return match &value.kind {
            ResolvedExprKind::Call(call) => match &call.callee {
                ResolvedCallee::Constructor(variant)
                    if call.result == value.ty
                        && call.permission.is_none()
                        && call.effects.is_empty()
                        && call.session.is_empty() =>
                {
                    Some(variant.clone())
                }
                ResolvedCallee::Builtin(_) if call.result == value.ty => {
                    resolved_builtin_sum_constructor_variant(call, types, variants)
                }
                ResolvedCallee::Function(_) => {
                    let (tail, callee_body, _) = inline_pure_call_tail(
                        call,
                        body,
                        types,
                        callables,
                        variants,
                        &Z3VarMap::new(),
                        &mut BTreeSet::new(),
                    )?;
                    resolved_constructor_at_path(
                        &tail,
                        &[],
                        &callee_body,
                        types,
                        callables,
                        variants,
                    )
                }
                _ => None,
            },
            ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
                resolved_constructor_at_path(
                    block.result.as_deref()?,
                    &[],
                    body,
                    types,
                    callables,
                    variants,
                )
            }
            _ => None,
        };
    }
    let Some((first, rest)) = path.split_first() else {
        return None;
    };
    match &value.kind {
        ResolvedExprKind::Tuple(items) => {
            let ResolvedValuePathPart::Tuple(index) = first else {
                return None;
            };
            resolved_constructor_at_path(items.get(*index)?, rest, body, types, callables, variants)
        }
        ResolvedExprKind::Record {
            fields, rest: base, ..
        } => {
            let ResolvedValuePathPart::Field(field_id) = first else {
                return None;
            };
            if let Some(field) = fields.iter().find(|field| &field.field == field_id) {
                return resolved_constructor_at_path(
                    &field.value,
                    rest,
                    body,
                    types,
                    callables,
                    variants,
                );
            }
            resolved_constructor_at_path(base.as_deref()?, path, body, types, callables, variants)
        }
        ResolvedExprKind::Call(call) => {
            if call.result == value.ty {
                if let Some(actual_variant) =
                    resolved_builtin_sum_constructor_variant(call, types, variants)
                {
                    let ResolvedValuePathPart::VariantPayload { variant, member } = first else {
                        return None;
                    };
                    if &actual_variant != variant {
                        return None;
                    }
                    let argument = resolved_builtin_sum_constructor_payload(
                        call, variant, member, types, variants,
                    )?;
                    return resolved_constructor_at_path(
                        argument, rest, body, types, callables, variants,
                    );
                }
            }
            let ResolvedCallee::Constructor(actual_variant) = &call.callee else {
                return None;
            };
            if call.result != value.ty
                || call.permission.is_some()
                || !call.effects.is_empty()
                || !call.session.is_empty()
            {
                return None;
            }
            let ResolvedValuePathPart::VariantPayload { variant, member } = first else {
                return None;
            };
            if actual_variant != variant {
                return None;
            }
            let argument = call
                .arguments
                .iter()
                .find(|argument| argument.parameter.0 == *member)
                .or_else(|| (call.arguments.len() == 1).then(|| &call.arguments[0]))?;
            if resolved_variant_member_type(&value.ty, actual_variant, member, types, variants)?
                != argument.value.ty
            {
                return None;
            }
            resolved_constructor_at_path(&argument.value, rest, body, types, callables, variants)
        }
        ResolvedExprKind::Load(_) | ResolvedExprKind::Project { .. } => None,
        ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
            resolved_constructor_at_path(
                block.result.as_deref()?,
                path,
                body,
                types,
                callables,
                variants,
            )
        }
        _ => None,
    }
}

fn bind_sum_value_at_path(
    value: &ResolvedExpr,
    source_path: &[ResolvedValuePathPart],
    target_type: &crate::core::ir::ResolvedTypeId,
    target_key: &str,
    source_body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
    active: &mut BTreeSet<NodeId>,
) -> Option<()> {
    let tag = resolved_variant_tag_at_path(
        value,
        source_path,
        target_type,
        source_body,
        types,
        callables,
        variants,
        vars,
    )?;
    vars.insert_int(format!("{target_key}.tag"), tag);
    let possible_variants = resolved_sum_variants(target_type, types, variants)?;
    let known =
        resolved_constructor_at_path(value, source_path, source_body, types, callables, variants);
    for (variant, members) in possible_variants {
        if known.as_ref().is_some_and(|known| known != &variant) {
            continue;
        }
        for (member, member_type) in members {
            if matches!(
                types.get(&member_type),
                Some(ResolvedType::GenericParameter(_))
            ) {
                // The tag remains useful for generic predicates. A generic
                // payload receives no scalar fact without a type substitution.
                continue;
            }
            let mut child_source_path = source_path.to_vec();
            child_source_path.push(ResolvedValuePathPart::VariantPayload {
                variant: variant.clone(),
                member: member.clone(),
            });
            let child_key = append_value_path(
                target_key,
                &[ResolvedValuePathPart::VariantPayload {
                    variant: variant.clone(),
                    member: member.clone(),
                }],
            );
            bind_value_at_path(
                value,
                &child_source_path,
                &member_type,
                &child_key,
                source_body,
                types,
                callables,
                variants,
                vars,
                active,
            )?;
        }
    }
    Some(())
}

fn bind_value_at_path(
    value: &ResolvedExpr,
    source_path: &[ResolvedValuePathPart],
    target_type: &crate::core::ir::ResolvedTypeId,
    target_key: &str,
    source_body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
    active: &mut BTreeSet<NodeId>,
) -> Option<()> {
    if let ResolvedExprKind::Try {
        value: inner,
        propagation_target,
    } = &value.kind
    {
        if !source_body.owner.0.starts_with("transition:")
            || propagation_target != &source_body.owner
        {
            return None;
        }
        let mut success_path = vec![ResolvedValuePathPart::VariantPayload {
            variant: NodeId("builtin:variant:Result::Ok".into()),
            member: NodeId("builtin:variant:Result::Ok/payload:0".into()),
        }];
        success_path.extend_from_slice(source_path);
        return bind_value_at_path(
            inner,
            &success_path,
            target_type,
            target_key,
            source_body,
            types,
            callables,
            variants,
            vars,
            active,
        );
    }
    if let ResolvedExprKind::Call(call) = &value.kind {
        if matches!(&call.callee, ResolvedCallee::Transition(_)) {
            if source_path.is_empty() && value.ty == call.result && target_type == &call.result {
                return bind_sum_value_at_path(
                    value,
                    source_path,
                    target_type,
                    target_key,
                    source_body,
                    types,
                    callables,
                    variants,
                    vars,
                    active,
                );
            }
            return bind_transition_value_at_path(
                call,
                source_path,
                target_type,
                target_key,
                source_body,
                types,
                callables,
                variants,
                vars,
                active,
            );
        }
    }
    if let Some(ResolvedType::Nominal { item, .. }) = types.get(target_type) {
        if item.as_str().starts_with("state:") {
            let source_type = resolved_expression_type_at_path(
                value,
                source_path,
                source_body,
                types,
                callables,
                variants,
            )?;
            if &source_type != target_type {
                return None;
            }
            return match (&value.kind, source_path) {
                (
                    ResolvedExprKind::Record {
                        nominal,
                        fields,
                        rest: None,
                    },
                    [],
                ) if nominal.as_str() == item.as_str() => {
                    for field in fields {
                        if field.conversion.kind != CheckedConversionKind::Identity
                            || field.conversion.from != field.value.ty
                            || field.conversion.to != field.value.ty
                            || !field.value.effects.is_empty()
                        {
                            return None;
                        }
                        match types.get(&field.value.ty) {
                            Some(ResolvedType::Primitive(
                                PrimitiveType::String | PrimitiveType::Unit,
                            )) => {}
                            Some(ResolvedType::Primitive(_))
                                if resolved_scalar_leaf_supported(&field.value.ty, types) =>
                            {
                                let field_key = append_value_path(
                                    target_key,
                                    &[ResolvedValuePathPart::Field(field.field.clone())],
                                );
                                match scalar_value_of_expr_with_catalog(
                                    &field.value,
                                    source_body,
                                    types,
                                    callables,
                                    variants,
                                    vars,
                                )? {
                                    ResolvedScalarValue::Int(value) => {
                                        vars.insert_int(field_key, value)
                                    }
                                    ResolvedScalarValue::Real(value) => {
                                        vars.insert_real(field_key, value)
                                    }
                                    ResolvedScalarValue::Bool(value) => {
                                        vars.insert_bool(field_key, value)
                                    }
                                }
                            }
                            _ => return None,
                        }
                    }
                    Some(())
                }
                (ResolvedExprKind::Load(_) | ResolvedExprKind::Project { .. }, _) => {
                    let source_key =
                        append_value_path(&value_base_key(value, source_body)?, source_path);
                    copy_known_value_path_facts(&source_key, target_key, vars);
                    Some(())
                }
                _ => None,
            };
        }
    }
    // Sum payload types belong to the checker-owned sum schema, even when a
    // value-level `if` constructs different variants in its two arms. The
    // expression-level path walker quite correctly rejects reading a payload
    // from the arm that constructs another variant; using the typed schema
    // here lets the payload term be encoded with an inactive-branch value and
    // the sum tag still selects the only reachable payload.
    let source_type =
        resolved_type_at_path(&value.ty, source_path, types, variants).or_else(|| {
            resolved_expression_type_at_path(
                value,
                source_path,
                source_body,
                types,
                callables,
                variants,
            )
        })?;
    match types.get(target_type)? {
        ResolvedType::Primitive(PrimitiveType::String | PrimitiveType::Unit)
            if &source_type == target_type =>
        {
            Some(())
        }
        ResolvedType::Primitive(_)
            if &source_type == target_type
                && resolved_scalar_leaf_supported(target_type, types) =>
        {
            let scalar = projected_scalar_value(
                value,
                source_path,
                target_type,
                source_body,
                types,
                callables,
                variants,
                vars,
            )?;
            match scalar {
                ResolvedScalarValue::Int(term) => vars.insert_int(target_key, term),
                ResolvedScalarValue::Real(term) => vars.insert_real(target_key, term),
                ResolvedScalarValue::Bool(term) => vars.insert_bool(target_key, term),
            }
            Some(())
        }
        ResolvedType::Tuple(items) if &source_type == target_type => {
            for (index, item_type) in items.iter().enumerate() {
                let mut child_source_path = source_path.to_vec();
                child_source_path.push(ResolvedValuePathPart::Tuple(index));
                let child_key =
                    append_value_path(target_key, &[ResolvedValuePathPart::Tuple(index)]);
                bind_value_at_path(
                    value,
                    &child_source_path,
                    item_type,
                    &child_key,
                    source_body,
                    types,
                    callables,
                    variants,
                    vars,
                    active,
                )?;
            }
            Some(())
        }
        _ if resolved_sum_variants(target_type, types, variants).is_some()
            && resolved_sum_shapes_match(&source_type, target_type, types) =>
        {
            bind_sum_value_at_path(
                value,
                source_path,
                target_type,
                target_key,
                source_body,
                types,
                callables,
                variants,
                vars,
                active,
            )
        }
        ResolvedType::Nominal { item, .. }
            if item.as_str() == "builtin:type:List" && &source_type == target_type =>
        {
            if !resolved_list_value_supported(target_type, types) {
                return None;
            }
            let length = resolved_list_length_at_path(
                value,
                source_path,
                target_type,
                source_body,
                types,
                callables,
                variants,
                vars,
            )?;
            vars.insert_list_len(target_key, length);
            Some(())
        }
        ResolvedType::Nominal { item, .. }
            if item.as_str() == "builtin:type:Set" && &source_type == target_type =>
        {
            if resolved_set_value_supported(target_type, types).is_none() {
                return None;
            }
            let state = resolved_set_state_at_path(
                value,
                source_path,
                target_type,
                source_body,
                types,
                callables,
                variants,
                vars,
            )?;
            vars.insert_set_var(target_key, state);
            Some(())
        }
        _ => None,
    }
}

fn bind_value_to_local(
    value: &ResolvedExpr,
    source_body: &ResolvedBody,
    local: &crate::core::ir::ResolvedLocal,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
    active: &mut BTreeSet<NodeId>,
) -> Option<()> {
    let target_key = local_var_key(&local.id, &local.display_name);
    bind_value_at_path(
        value,
        &[],
        &local.ty,
        &target_key,
        source_body,
        types,
        callables,
        variants,
        vars,
        active,
    )
}

pub(crate) fn resolved_bind_value_to_local_with_catalog(
    value: &ResolvedExpr,
    source_body: &ResolvedBody,
    local: &crate::core::ir::ResolvedLocal,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<()> {
    bind_value_to_local(
        value,
        source_body,
        local,
        types,
        callables,
        variants,
        vars,
        &mut BTreeSet::new(),
    )
}

fn resolved_builtin_call_is_closed(
    call: &crate::core::ir::ResolvedCall,
    expected_permission: Option<crate::core::ir::Permission>,
) -> bool {
    call.permission == expected_permission
        && call.effects.is_empty()
        && call.session.is_empty()
        && call.arguments.len() == 1
}

fn resolved_set_builtin_call_is_closed(
    call: &crate::core::ir::ResolvedCall,
    expected_arity: usize,
    expected_permission: Option<crate::core::ir::Permission>,
) -> bool {
    call.permission == expected_permission
        && call.effects.is_empty()
        && call.session.is_empty()
        && call.type_arguments.is_empty()
        && call.arguments.len() == expected_arity
        && call.arguments.iter().all(|argument| {
            argument.conversion.kind == CheckedConversionKind::Identity
                && argument.conversion.from == argument.value.ty
                && argument.conversion.to == argument.value.ty
                && argument.value.effects.is_empty()
        })
}

fn resolved_set_element_sort(
    ty: &crate::core::ir::ResolvedTypeId,
    types: &ResolvedTypeTable,
) -> Option<z3::Sort> {
    match types.get(ty) {
        Some(ResolvedType::Primitive(PrimitiveType::I32 | PrimitiveType::I64)) => {
            Some(z3::Sort::int())
        }
        Some(ResolvedType::Primitive(PrimitiveType::Bool)) => Some(z3::Sort::bool()),
        _ => None,
    }
}

fn resolved_set_value_supported(
    ty: &crate::core::ir::ResolvedTypeId,
    types: &ResolvedTypeTable,
) -> Option<crate::core::ir::ResolvedTypeId> {
    let Some(ResolvedType::Nominal {
        item,
        arguments,
        is_linear,
    }) = types.get(ty)
    else {
        return None;
    };
    (item.as_str() == "builtin:type:Set"
        && !*is_linear
        && arguments.len() == 1
        && resolved_set_element_sort(&arguments[0], types).is_some())
    .then(|| arguments[0].clone())
}

enum ResolvedSetElement {
    Int(Z3Int),
    Bool(Z3Bool),
}

impl ResolvedSetElement {
    fn from_expr(
        expr: &ResolvedExpr,
        element_ty: &crate::core::ir::ResolvedTypeId,
        body: &ResolvedBody,
        types: &ResolvedTypeTable,
        callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
        variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
        vars: &mut Z3VarMap,
    ) -> Option<Self> {
        if expr.ty != *element_ty {
            return None;
        }
        match types.get(element_ty) {
            Some(ResolvedType::Primitive(PrimitiveType::I32 | PrimitiveType::I64)) => {
                resolved_to_z3_int_with_catalog(expr, body, types, callables, variants, vars)
                    .map(Self::Int)
            }
            Some(ResolvedType::Primitive(PrimitiveType::Bool)) => {
                resolved_to_z3_bool_with_catalog(expr, body, types, callables, variants, vars)
                    .map(Self::Bool)
            }
            _ => None,
        }
    }

    fn member(&self, set: &Z3Set) -> Z3Bool {
        match self {
            Self::Int(value) => set.member(value),
            Self::Bool(value) => set.member(value),
        }
    }

    fn add(&self, set: &Z3Set) -> Z3Set {
        match self {
            Self::Int(value) => set.add(value),
            Self::Bool(value) => set.add(value),
        }
    }

    fn del(&self, set: &Z3Set) -> Z3Set {
        match self {
            Self::Int(value) => set.del(value),
            Self::Bool(value) => set.del(value),
        }
    }
}

fn resolved_set_state_at_path(
    value: &ResolvedExpr,
    path: &[ResolvedValuePathPart],
    expected_type: &crate::core::ir::ResolvedTypeId,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<(Z3Set, Z3Int)> {
    let element_ty = resolved_set_value_supported(expected_type, types)?;
    if path.is_empty() {
        if value.ty != *expected_type {
            return None;
        }
        return match &value.kind {
            ResolvedExprKind::Set(items) => {
                let sort = resolved_set_element_sort(&element_ty, types)?;
                let mut set = Z3Set::empty(&sort);
                let mut size = Z3Int::from_i64(0);
                for item in items {
                    let elem = ResolvedSetElement::from_expr(
                        item,
                        &element_ty,
                        body,
                        types,
                        callables,
                        variants,
                        vars,
                    )?;
                    let present = elem.member(&set);
                    let increment = present.ite(&Z3Int::from_i64(0), &Z3Int::from_i64(1));
                    size = Z3Int::add(&[&size, &increment]);
                    set = elem.add(&set);
                }
                Some((set, size))
            }
            ResolvedExprKind::Load(_) | ResolvedExprKind::Project { .. } => {
                let key = value_base_key(value, body)?;
                vars.get_set_var(&key).cloned()
            }
            ResolvedExprKind::Call(call) if matches!(&call.callee, ResolvedCallee::Function(_)) => {
                let (tail, callee_body, mut callee_vars) = inline_pure_call_tail(
                    call,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                    &mut BTreeSet::new(),
                )?;
                resolved_set_state_at_path(
                    &tail,
                    &[],
                    expected_type,
                    &callee_body,
                    types,
                    callables,
                    variants,
                    &mut callee_vars,
                )
            }
            ResolvedExprKind::Call(call)
                if matches!(
                    &call.callee,
                    ResolvedCallee::Builtin(b)
                        if matches!(
                            b.as_str(),
                            "builtin.method.set.insert" | "builtin.method.set.remove"
                        )
                ) =>
            {
                if !resolved_set_builtin_call_is_closed(
                    call,
                    2,
                    Some(crate::core::ir::Permission::View),
                ) {
                    return None;
                }
                let (set, size) = resolved_set_state_at_path(
                    &call.arguments[0].value,
                    &[],
                    expected_type,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                )?;
                let elem = ResolvedSetElement::from_expr(
                    &call.arguments[1].value,
                    &element_ty,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                )?;
                let present = elem.member(&set);
                let is_insert = matches!(
                    &call.callee,
                    ResolvedCallee::Builtin(b) if b.as_str() == "builtin.method.set.insert"
                );
                if is_insert {
                    let delta = present.ite(&Z3Int::from_i64(0), &Z3Int::from_i64(1));
                    Some((elem.add(&set), Z3Int::add(&[&size, &delta])))
                } else {
                    let delta = present.ite(&Z3Int::from_i64(1), &Z3Int::from_i64(0));
                    Some((elem.del(&set), Z3Int::sub(&[&size, &delta])))
                }
            }
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                let condition = resolved_to_z3_bool_with_catalog(
                    condition, body, types, callables, variants, vars,
                )?;
                let (then_set, then_size) = resolved_block_tail_set_state(
                    then_block,
                    &[],
                    expected_type,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                )?;
                let (else_set, else_size) = resolved_block_tail_set_state(
                    else_block,
                    &[],
                    expected_type,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                )?;
                Some((
                    condition.ite(&then_set, &else_set),
                    condition.ite(&then_size, &else_size),
                ))
            }
            ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
                resolved_block_tail_set_state(
                    block,
                    &[],
                    expected_type,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                )
            }
            _ => None,
        };
    }

    match &value.kind {
        ResolvedExprKind::Load(_) => {
            let base = value_base_key(value, body)?;
            vars.get_set_var(&append_value_path(&base, path)).cloned()
        }
        ResolvedExprKind::Project {
            value: base,
            projection,
        } => {
            let first = match projection {
                ResolvedValueProjection::Field(field) => {
                    ResolvedValuePathPart::Field(field.clone())
                }
                ResolvedValueProjection::Tuple(index) => ResolvedValuePathPart::Tuple(*index),
                ResolvedValueProjection::Index(_) | ResolvedValueProjection::Dereference => {
                    return None;
                }
            };
            let mut combined = vec![first];
            combined.extend_from_slice(path);
            resolved_set_state_at_path(
                base,
                &combined,
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            )
        }
        ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
            resolved_block_tail_set_state(
                block,
                path,
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            )
        }
        _ => None,
    }
}

fn resolved_block_tail_set_state(
    block: &ResolvedBlock,
    path: &[ResolvedValuePathPart],
    expected_type: &crate::core::ir::ResolvedTypeId,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<(Z3Set, Z3Int)> {
    let mut scoped_vars = vars.clone();
    resolved_block_stmts_with_catalog(block, body, types, callables, variants, &mut scoped_vars)?;
    resolved_set_state_at_path(
        block.result.as_deref()?,
        path,
        expected_type,
        body,
        types,
        callables,
        variants,
        &mut scoped_vars,
    )
}

fn resolved_list_value_supported(
    ty: &crate::core::ir::ResolvedTypeId,
    types: &ResolvedTypeTable,
) -> bool {
    let Some(ResolvedType::Nominal {
        item,
        arguments,
        is_linear,
    }) = types.get(ty)
    else {
        return false;
    };
    item.as_str() == "builtin:type:List"
        && !*is_linear
        && arguments.len() == 1
        && value_neutral_collection_element(&arguments[0], types)
}

fn resolved_list_length_at_path(
    value: &ResolvedExpr,
    path: &[ResolvedValuePathPart],
    expected_type: &crate::core::ir::ResolvedTypeId,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Int> {
    if !resolved_list_value_supported(expected_type, types) {
        return None;
    }
    if path.is_empty() {
        if value.ty != *expected_type {
            return None;
        }
        return match &value.kind {
            ResolvedExprKind::List(items) => Some(Z3Int::from_i64(items.len() as i64)),
            ResolvedExprKind::Load(_) | ResolvedExprKind::Project { .. } => {
                let key = value_base_key(value, body)?;
                vars.get_list_len(&key).cloned()
            }
            ResolvedExprKind::Call(call)
                if matches!(
                    &call.callee,
                    ResolvedCallee::Builtin(builtin) if builtin.as_str() == "builtin.method.set.to_list"
                ) =>
            {
                if !resolved_set_builtin_call_is_closed(
                    call,
                    1,
                    Some(crate::core::ir::Permission::View),
                ) {
                    return None;
                }
                resolved_set_state_at_path(
                    &call.arguments[0].value,
                    &[],
                    &call.arguments[0].value.ty,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                )
                .map(|(_, size)| size)
            }
            ResolvedExprKind::Call(call) if matches!(&call.callee, ResolvedCallee::Function(_)) => {
                let (tail, callee_body, mut callee_vars) = inline_pure_call_tail(
                    call,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                    &mut BTreeSet::new(),
                )?;
                resolved_list_length_at_path(
                    &tail,
                    &[],
                    expected_type,
                    &callee_body,
                    types,
                    callables,
                    variants,
                    &mut callee_vars,
                )
            }
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                let condition = resolved_to_z3_bool_with_catalog(
                    condition, body, types, callables, variants, vars,
                )?;
                let then_len = resolved_block_tail_list_length(
                    then_block,
                    &[],
                    expected_type,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                )?;
                let else_len = resolved_block_tail_list_length(
                    else_block,
                    &[],
                    expected_type,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                )?;
                Some(condition.ite(&then_len, &else_len))
            }
            ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
                resolved_block_tail_list_length(
                    block,
                    &[],
                    expected_type,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                )
            }
            _ => None,
        };
    }

    match &value.kind {
        ResolvedExprKind::Load(_) => {
            let base = value_base_key(value, body)?;
            vars.get_list_len(&append_value_path(&base, path)).cloned()
        }
        ResolvedExprKind::Project {
            value: base,
            projection,
        } => {
            let first = match projection {
                ResolvedValueProjection::Field(field) => {
                    ResolvedValuePathPart::Field(field.clone())
                }
                ResolvedValueProjection::Tuple(index) => ResolvedValuePathPart::Tuple(*index),
                ResolvedValueProjection::Index(_) | ResolvedValueProjection::Dereference => {
                    return None;
                }
            };
            let mut combined = vec![first];
            combined.extend_from_slice(path);
            resolved_list_length_at_path(
                base,
                &combined,
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            )
        }
        ResolvedExprKind::Record { fields, rest, .. } => {
            let ResolvedValuePathPart::Field(field_id) = path.first()? else {
                return None;
            };
            if let Some(field) = fields.iter().find(|field| &field.field == field_id) {
                resolved_list_length_at_path(
                    &field.value,
                    &path[1..],
                    expected_type,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                )
            } else {
                resolved_list_length_at_path(
                    rest.as_deref()?,
                    path,
                    expected_type,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                )
            }
        }
        ResolvedExprKind::Tuple(items) => {
            let ResolvedValuePathPart::Tuple(index) = path.first()? else {
                return None;
            };
            resolved_list_length_at_path(
                items.get(*index)?,
                &path[1..],
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            )
        }
        ResolvedExprKind::Call(call) => match &call.callee {
            ResolvedCallee::Constructor(actual_variant) => {
                if call.result != value.ty
                    || call.permission.is_some()
                    || !call.effects.is_empty()
                    || !call.session.is_empty()
                {
                    return None;
                }
                let ResolvedValuePathPart::VariantPayload { variant, member } = path.first()?
                else {
                    return None;
                };
                if actual_variant != variant {
                    return None;
                }
                let argument = call
                    .arguments
                    .iter()
                    .find(|argument| argument.parameter.0 == *member)
                    .or_else(|| (call.arguments.len() == 1).then(|| &call.arguments[0]))?;
                if resolved_variant_member_type(&value.ty, actual_variant, member, types, variants)?
                    != argument.value.ty
                {
                    return None;
                }
                resolved_list_length_at_path(
                    &argument.value,
                    &path[1..],
                    expected_type,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                )
            }
            ResolvedCallee::Function(_) => {
                let (tail, callee_body, mut callee_vars) = inline_pure_call_tail(
                    call,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                    &mut BTreeSet::new(),
                )?;
                resolved_list_length_at_path(
                    &tail,
                    path,
                    expected_type,
                    &callee_body,
                    types,
                    callables,
                    variants,
                    &mut callee_vars,
                )
            }
            ResolvedCallee::Builtin(_) if call.result == value.ty => {
                let ResolvedValuePathPart::VariantPayload { variant, member } = path.first()?
                else {
                    return None;
                };
                let payload = resolved_builtin_sum_constructor_payload(
                    call, variant, member, types, variants,
                )?;
                resolved_list_length_at_path(
                    payload,
                    &path[1..],
                    expected_type,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                )
            }
            _ => None,
        },
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            let condition = resolved_to_z3_bool_with_catalog(
                condition, body, types, callables, variants, vars,
            )?;
            let then_len = resolved_block_tail_list_length(
                then_block,
                path,
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            )?;
            let else_len = resolved_block_tail_list_length(
                else_block,
                path,
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            )?;
            Some(condition.ite(&then_len, &else_len))
        }
        ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
            resolved_block_tail_list_length(
                block,
                path,
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            )
        }
        _ => None,
    }
}

fn resolved_block_tail_list_length(
    block: &ResolvedBlock,
    path: &[ResolvedValuePathPart],
    expected_type: &crate::core::ir::ResolvedTypeId,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Int> {
    resolved_block_stmts_with_catalog(block, body, types, callables, variants, vars)?;
    resolved_list_length_at_path(
        block.result.as_deref()?,
        path,
        expected_type,
        body,
        types,
        callables,
        variants,
        vars,
    )
}

fn direct_result_projection_path(
    expression: &ResolvedExpr,
    result_local: &ResolvedLocalId,
    body: &ResolvedBody,
) -> Option<Option<Vec<ResolvedValuePathPart>>> {
    match &expression.kind {
        ResolvedExprKind::Load(place) if &place.base == result_local => {
            local_key(place, body, &expression.ty)?;
            let mut path = Vec::with_capacity(place.projections.len());
            for projection in &place.projections {
                match projection {
                    ResolvedProjection::Field { field, .. } => {
                        path.push(ResolvedValuePathPart::Field(field.clone()));
                    }
                    ResolvedProjection::Tuple { index, .. } => {
                        path.push(ResolvedValuePathPart::Tuple(*index));
                    }
                    ResolvedProjection::Index { .. } | ResolvedProjection::Deref { .. } => {
                        return None;
                    }
                }
            }
            Some(Some(path))
        }
        ResolvedExprKind::Project { value, projection } => {
            let Some(mut path) = direct_result_projection_path(value, result_local, body)? else {
                return Some(None);
            };
            match projection {
                ResolvedValueProjection::Field(field) => {
                    path.push(ResolvedValuePathPart::Field(field.clone()));
                }
                ResolvedValueProjection::Tuple(index) => {
                    path.push(ResolvedValuePathPart::Tuple(*index));
                }
                ResolvedValueProjection::Index(_) | ResolvedValueProjection::Dereference => {
                    return None;
                }
            }
            Some(Some(path))
        }
        _ => {
            let _ = body;
            Some(None)
        }
    }
}

fn collect_aggregate_result_projection_terms(
    expression: &ResolvedExpr,
    result_local: &ResolvedLocalId,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    output: &mut BTreeMap<String, (Vec<ResolvedValuePathPart>, crate::core::ir::ResolvedTypeId)>,
) -> Option<()> {
    if !resolved_expr_references_local(expression, result_local) {
        return Some(());
    }
    if let Some(path) = direct_result_projection_path(expression, result_local, body)? {
        if path.is_empty()
            || path.iter().any(|part| {
                !matches!(
                    part,
                    ResolvedValuePathPart::Field(_) | ResolvedValuePathPart::Tuple(_)
                )
            })
            || !resolved_scalar_leaf_supported(&expression.ty, types)
        {
            return None;
        }
        let local = body.locals.get(result_local)?;
        let key = append_value_path(&local_var_key(&local.id, &local.display_name), &path);
        let entry = (path, expression.ty.clone());
        if let Some(previous) = output.get(&key) {
            if previous != &entry {
                return None;
            }
        } else {
            output.insert(key, entry);
        }
        return Some(());
    }

    use ResolvedExprKind as E;
    match &expression.kind {
        E::Project { value, projection } => {
            collect_aggregate_result_projection_terms(value, result_local, body, types, output)?;
            if let ResolvedValueProjection::Index(index) = projection {
                collect_aggregate_result_projection_terms(
                    index,
                    result_local,
                    body,
                    types,
                    output,
                )?;
            }
        }
        E::Binary { left, right, .. }
        | E::Range {
            start: left,
            end: right,
        } => {
            collect_aggregate_result_projection_terms(left, result_local, body, types, output)?;
            collect_aggregate_result_projection_terms(right, result_local, body, types, output)?;
        }
        E::Unary { operand, .. } | E::Cast { value: operand, .. } => {
            collect_aggregate_result_projection_terms(operand, result_local, body, types, output)?;
        }
        E::Old(_) => return None,
        E::Call(call) => {
            for argument in &call.arguments {
                collect_aggregate_result_projection_terms(
                    &argument.value,
                    result_local,
                    body,
                    types,
                    output,
                )?;
            }
        }
        E::Tuple(values) | E::List(values) | E::Set(values) => {
            for value in values {
                collect_aggregate_result_projection_terms(
                    value,
                    result_local,
                    body,
                    types,
                    output,
                )?;
            }
        }
        E::Map(entries) => {
            for (key, value) in entries {
                collect_aggregate_result_projection_terms(key, result_local, body, types, output)?;
                collect_aggregate_result_projection_terms(
                    value,
                    result_local,
                    body,
                    types,
                    output,
                )?;
            }
        }
        E::Record { fields, rest, .. } => {
            for field in fields {
                collect_aggregate_result_projection_terms(
                    &field.value,
                    result_local,
                    body,
                    types,
                    output,
                )?;
            }
            if let Some(rest) = rest {
                collect_aggregate_result_projection_terms(rest, result_local, body, types, output)?;
            }
        }
        E::If {
            condition,
            then_block,
            else_block,
        } => {
            collect_aggregate_result_projection_terms(
                condition,
                result_local,
                body,
                types,
                output,
            )?;
            collect_aggregate_result_projection_block(
                then_block,
                result_local,
                body,
                types,
                output,
            )?;
            collect_aggregate_result_projection_block(
                else_block,
                result_local,
                body,
                types,
                output,
            )?;
        }
        E::Block(block) | E::Scope { body: block, .. } => {
            collect_aggregate_result_projection_block(block, result_local, body, types, output)?;
        }
        E::Match { scrutinee, arms } => {
            collect_aggregate_result_projection_terms(
                scrutinee,
                result_local,
                body,
                types,
                output,
            )?;
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_aggregate_result_projection_terms(
                        guard,
                        result_local,
                        body,
                        types,
                        output,
                    )?;
                }
                collect_aggregate_result_projection_terms(
                    &arm.body,
                    result_local,
                    body,
                    types,
                    output,
                )?;
            }
        }
        E::Try { value, .. }
        | E::TypeOf(value)
        | E::Spawn(value)
        | E::Await(value)
        | E::OptionalChain {
            receiver: value, ..
        } => {
            collect_aggregate_result_projection_terms(value, result_local, body, types, output)?;
        }
        E::Slice { target, start, end } => {
            collect_aggregate_result_projection_terms(target, result_local, body, types, output)?;
            if let Some(start) = start {
                collect_aggregate_result_projection_terms(
                    start,
                    result_local,
                    body,
                    types,
                    output,
                )?;
            }
            if let Some(end) = end {
                collect_aggregate_result_projection_terms(end, result_local, body, types, output)?;
            }
        }
        E::FString(parts) => {
            for part in parts {
                if let crate::core::ir::ResolvedFStringPart::Interpolation(value) = part {
                    collect_aggregate_result_projection_terms(
                        value,
                        result_local,
                        body,
                        types,
                        output,
                    )?;
                }
            }
        }
        E::Comprehension {
            value,
            iterable,
            guard,
            ..
        } => {
            collect_aggregate_result_projection_terms(value, result_local, body, types, output)?;
            collect_aggregate_result_projection_terms(iterable, result_local, body, types, output)?;
            if let Some(guard) = guard {
                collect_aggregate_result_projection_terms(
                    guard,
                    result_local,
                    body,
                    types,
                    output,
                )?;
            }
        }
        E::Lambda(_) | E::Quote(_) | E::Comptime(_) => return None,
        E::Load(place) if &place.base == result_local => return None,
        E::Load(_)
        | E::Literal(_)
        | E::Constant(_)
        | E::Callable(_)
        | E::DefaultArgument { .. }
        | E::ComptimeValue(_)
        | E::TypeValue(_) => return None,
    }
    Some(())
}

fn collect_aggregate_result_projection_block(
    block: &ResolvedBlock,
    result_local: &ResolvedLocalId,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    output: &mut BTreeMap<String, (Vec<ResolvedValuePathPart>, crate::core::ir::ResolvedTypeId)>,
) -> Option<()> {
    for statement in &block.statements {
        match &statement.kind {
            ResolvedStmtKind::Bind { initializer, .. } => {
                if let Some(initializer) = initializer {
                    collect_aggregate_result_projection_terms(
                        initializer,
                        result_local,
                        body,
                        types,
                        output,
                    )?;
                }
            }
            ResolvedStmtKind::Assign { target, value, .. } => {
                if &target.base == result_local {
                    return None;
                }
                collect_aggregate_result_projection_terms(
                    value,
                    result_local,
                    body,
                    types,
                    output,
                )?;
            }
            ResolvedStmtKind::Expr(value) => {
                collect_aggregate_result_projection_terms(
                    value,
                    result_local,
                    body,
                    types,
                    output,
                )?;
            }
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                if let Some(value) = value {
                    collect_aggregate_result_projection_terms(
                        value,
                        result_local,
                        body,
                        types,
                        output,
                    )?;
                }
            }
            ResolvedStmtKind::Math(values) => {
                for value in values {
                    collect_aggregate_result_projection_terms(
                        value,
                        result_local,
                        body,
                        types,
                        output,
                    )?;
                }
            }
            ResolvedStmtKind::Contract { condition, .. } => {
                collect_aggregate_result_projection_terms(
                    condition,
                    result_local,
                    body,
                    types,
                    output,
                )?;
            }
            ResolvedStmtKind::While {
                condition,
                body: inner,
            } => {
                collect_aggregate_result_projection_terms(
                    condition,
                    result_local,
                    body,
                    types,
                    output,
                )?;
                collect_aggregate_result_projection_block(
                    inner,
                    result_local,
                    body,
                    types,
                    output,
                )?;
            }
            ResolvedStmtKind::WhileLet {
                initializer,
                body: inner,
                ..
            }
            | ResolvedStmtKind::For {
                iterable: initializer,
                body: inner,
                ..
            } => {
                collect_aggregate_result_projection_terms(
                    initializer,
                    result_local,
                    body,
                    types,
                    output,
                )?;
                collect_aggregate_result_projection_block(
                    inner,
                    result_local,
                    body,
                    types,
                    output,
                )?;
            }
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                collect_aggregate_result_projection_terms(
                    initializer,
                    result_local,
                    body,
                    types,
                    output,
                )?;
                collect_aggregate_result_projection_block(
                    then_block,
                    result_local,
                    body,
                    types,
                    output,
                )?;
                if let Some(else_block) = else_block {
                    collect_aggregate_result_projection_block(
                        else_block,
                        result_local,
                        body,
                        types,
                        output,
                    )?;
                }
            }
            ResolvedStmtKind::Loop(inner) | ResolvedStmtKind::Scope { body: inner, .. } => {
                collect_aggregate_result_projection_block(
                    inner,
                    result_local,
                    body,
                    types,
                    output,
                )?;
            }
            ResolvedStmtKind::Pinned {
                value, body: inner, ..
            } => {
                collect_aggregate_result_projection_terms(
                    value,
                    result_local,
                    body,
                    types,
                    output,
                )?;
                collect_aggregate_result_projection_block(
                    inner,
                    result_local,
                    body,
                    types,
                    output,
                )?;
            }
            ResolvedStmtKind::Continue
            | ResolvedStmtKind::Drop(_)
            | ResolvedStmtKind::NestedCallable(_) => {}
        }
    }
    if let Some(result) = &block.result {
        collect_aggregate_result_projection_terms(result, result_local, body, types, output)?;
    }
    Some(())
}

/// Tie only contract-visible scalar leaves of an aggregate return to the
/// corresponding checker-identified Field/Tuple path in the body result.
/// Whole aggregate equality, variant tag equality, non-scalar leaves, and
/// indexed/dereferenced result paths remain fail-closed.
pub(crate) fn resolved_bind_aggregate_result_with_catalog(
    value: &ResolvedExpr,
    body: &ResolvedBody,
    ensures: &[&ResolvedExpr],
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
    session: &mut crate::verifier::ctx::SolverSession,
) -> Option<()> {
    let result_local = contract_result_local(body)?;
    if body.locals.get(&result_local)?.ty != value.ty {
        return None;
    }
    let mut projected = BTreeMap::new();
    for ensure in ensures {
        collect_aggregate_result_projection_terms(
            ensure,
            &result_local,
            body,
            types,
            &mut projected,
        )?;
    }
    if projected.is_empty() {
        return None;
    }
    let result_base = {
        let local = body.locals.get(&result_local)?;
        local_var_key(&local.id, &local.display_name)
    };
    for (key, (path, field_type)) in projected {
        let computed = projected_scalar_value(
            value,
            &path,
            &field_type,
            body,
            types,
            callables,
            variants,
            vars,
        )?;
        let expected_key = append_value_path(&result_base, &path);
        if key != expected_key {
            return None;
        }
        match computed {
            ResolvedScalarValue::Int(computed) => {
                let result = vars.get_or_create_int(&key);
                session.solver.assert(result.eq(&computed));
            }
            ResolvedScalarValue::Real(computed) => {
                let result = vars.get_or_create_real(&key);
                session.solver.assert(result.eq(&computed));
            }
            ResolvedScalarValue::Bool(computed) => {
                let result = vars.get_or_create_bool(&key);
                session.solver.assert(result.eq(&computed));
            }
        }
    }
    Some(())
}

struct ResolvedBuiltinSumProjection<'a> {
    receiver: &'a ResolvedExpr,
    fallback: Option<&'a ResolvedExpr>,
    variant: NodeId,
    member: NodeId,
    payload_type: crate::core::ir::ResolvedTypeId,
}

/// Validate the exact typed shape of the scalar Option/Result projection
/// builtins.  This is shared by the value encoder and the definedness
/// collector so a total `unwrap_or` cannot accidentally inherit the
/// precondition semantics of `unwrap` (or vice versa).
fn resolved_builtin_sum_projection<'a>(
    call: &'a crate::core::ir::ResolvedCall,
    result_type: &crate::core::ir::ResolvedTypeId,
    types: &ResolvedTypeTable,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
) -> Option<ResolvedBuiltinSumProjection<'a>> {
    let ResolvedCallee::Builtin(builtin) = &call.callee else {
        return None;
    };
    let (variant, wants_fallback) = match builtin.as_str() {
        "builtin.method.option.unwrap" => ("builtin:variant:Option::Some", false),
        "builtin.method.option.unwrap_or" => ("builtin:variant:Option::Some", true),
        "builtin.method.result.unwrap" => ("builtin:variant:Result::Ok", false),
        "builtin.method.result.unwrap_err" => ("builtin:variant:Result::Err", false),
        "builtin.method.result.unwrap_or" => ("builtin:variant:Result::Ok", true),
        _ => return None,
    };
    let expected_arity = if wants_fallback { 2 } else { 1 };
    if call.result != *result_type
        || call.permission != Some(crate::core::ir::Permission::Consume)
        || !call.effects.is_empty()
        || !call.session.is_empty()
        || !call.type_arguments.is_empty()
        || call.arguments.len() != expected_arity
    {
        return None;
    }
    let receiver_argument = call.arguments.first()?;
    if receiver_argument.conversion.kind != CheckedConversionKind::Identity
        || receiver_argument.conversion.from != receiver_argument.value.ty
        || receiver_argument.conversion.to != receiver_argument.value.ty
        || !receiver_argument.value.effects.is_empty()
    {
        return None;
    }
    let variant = NodeId(variant.to_string());
    let member = NodeId(format!("{}/payload:0", variant.0));
    let payload_type = resolved_variant_member_type(
        &receiver_argument.value.ty,
        &variant,
        &member,
        types,
        variants,
    )?;
    if &payload_type != result_type {
        return None;
    }
    let fallback = if wants_fallback {
        let argument = call.arguments.get(1)?;
        if argument.conversion.kind != CheckedConversionKind::Identity
            || argument.conversion.from != argument.value.ty
            || argument.conversion.to != *result_type
            || argument.value.ty != *result_type
            || !argument.value.effects.is_empty()
        {
            return None;
        }
        Some(&argument.value)
    } else {
        None
    };
    Some(ResolvedBuiltinSumProjection {
        receiver: &receiver_argument.value,
        fallback,
        variant,
        member,
        payload_type,
    })
}

/// A payload is irrelevant when a constructor proves the receiver has the
/// other tag.  Supply a type-correct neutral term only for that known,
/// unreachable branch; an unknown receiver or unsupported payload remains
/// unencodable.  Callers use this only under the matching tag `ite` for
/// total `unwrap_or`.
fn resolved_variant_payload_scalar_or_inactive(
    value: &ResolvedExpr,
    expected_variant: &NodeId,
    member: &NodeId,
    expected_type: &crate::core::ir::ResolvedTypeId,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<ResolvedScalarValue> {
    match &value.kind {
        ResolvedExprKind::Call(call) if matches!(&call.callee, ResolvedCallee::Function(_)) => {
            let (tail, callee_body, mut callee_vars) = inline_pure_call_tail(
                call,
                body,
                types,
                callables,
                variants,
                vars,
                &mut BTreeSet::new(),
            )?;
            return resolved_variant_payload_scalar_or_inactive(
                &tail,
                expected_variant,
                member,
                expected_type,
                &callee_body,
                types,
                callables,
                variants,
                &mut callee_vars,
            );
        }
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            let condition = resolved_to_z3_bool_with_catalog(
                condition, body, types, callables, variants, vars,
            )?;
            let then_value = resolved_block_payload_scalar_or_inactive(
                then_block,
                expected_variant,
                member,
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            )?;
            let else_value = resolved_block_payload_scalar_or_inactive(
                else_block,
                expected_variant,
                member,
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            )?;
            return scalar_ite(condition, then_value, else_value);
        }
        ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
            return resolved_block_payload_scalar_or_inactive(
                block,
                expected_variant,
                member,
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            );
        }
        _ => {}
    }
    if let Some(actual) = resolved_constructor_at_path(value, &[], body, types, callables, variants)
    {
        if actual != *expected_variant {
            let actual_tag = resolved_variant_tag_number(&value.ty, &actual, types, variants)?;
            let expected_tag =
                resolved_variant_tag_number(&value.ty, expected_variant, types, variants)?;
            if actual_tag == expected_tag || !resolved_scalar_leaf_supported(expected_type, types) {
                return None;
            }
            return match z3_type_category(expected_type, types) {
                Z3TypeCategory::Int => Some(ResolvedScalarValue::Int(Z3Int::from_i64(0))),
                Z3TypeCategory::Real => Some(ResolvedScalarValue::Real(Z3Real::from_int(
                    &Z3Int::from_i64(0),
                ))),
                Z3TypeCategory::Bool => Some(ResolvedScalarValue::Bool(Z3Bool::from_bool(false))),
            };
        }
    }
    resolved_variant_payload_scalar(
        value,
        expected_variant,
        member,
        expected_type,
        body,
        types,
        callables,
        variants,
        vars,
    )
}

fn resolved_block_payload_scalar_or_inactive(
    block: &ResolvedBlock,
    expected_variant: &NodeId,
    member: &NodeId,
    expected_type: &crate::core::ir::ResolvedTypeId,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<ResolvedScalarValue> {
    resolved_block_stmts_with_catalog(block, body, types, callables, variants, vars)?;
    resolved_variant_payload_scalar_or_inactive(
        block.result.as_deref()?,
        expected_variant,
        member,
        expected_type,
        body,
        types,
        callables,
        variants,
        vars,
    )
}

fn resolved_builtin_int_call(
    expr: &ResolvedExpr,
    call: &crate::core::ir::ResolvedCall,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Int> {
    let ResolvedCallee::Builtin(builtin) = &call.callee else {
        return None;
    };
    let expected_permission = match builtin.as_str() {
        "len" => None,
        "builtin.method.list.len" | "builtin.method.set.size" | "builtin.method.set.len" => {
            Some(crate::core::ir::Permission::View)
        }
        "builtin.method.option.unwrap"
        | "builtin.method.result.unwrap"
        | "builtin.method.result.unwrap_err"
        | "builtin.method.option.unwrap_or"
        | "builtin.method.result.unwrap_or" => Some(crate::core::ir::Permission::Consume),
        _ => return None,
    };
    if expr.ty != call.result
        || z3_type_category(&expr.ty, types) != Z3TypeCategory::Int
        || call.permission != expected_permission
        || !call.effects.is_empty()
        || !call.session.is_empty()
        || !call.type_arguments.is_empty()
    {
        return None;
    }
    match builtin.as_str() {
        "len" | "builtin.method.list.len" => {
            if !resolved_builtin_call_is_closed(call, expected_permission)
                || !resolved_list_value_supported(&call.arguments[0].value.ty, types)
                || z3_type_category(&expr.ty, types) != Z3TypeCategory::Int
            {
                return None;
            }
            resolved_list_length_at_path(
                &call.arguments[0].value,
                &[],
                &call.arguments[0].value.ty,
                body,
                types,
                callables,
                variants,
                vars,
            )
        }
        "builtin.method.set.size" | "builtin.method.set.len" => {
            if !resolved_set_builtin_call_is_closed(call, 1, expected_permission)
                || z3_type_category(&expr.ty, types) != Z3TypeCategory::Int
            {
                return None;
            }
            resolved_set_state_at_path(
                &call.arguments[0].value,
                &[],
                &call.arguments[0].value.ty,
                body,
                types,
                callables,
                variants,
                vars,
            )
            .map(|(_, size)| size)
        }
        "builtin.method.option.unwrap"
        | "builtin.method.result.unwrap"
        | "builtin.method.result.unwrap_err"
        | "builtin.method.option.unwrap_or"
        | "builtin.method.result.unwrap_or" => {
            let projection = resolved_builtin_sum_projection(call, &expr.ty, types, variants)?;
            if z3_type_category(&projection.payload_type, types) != Z3TypeCategory::Int {
                return None;
            }
            if let Some(fallback) = projection.fallback {
                let tag = resolved_variant_tag_condition(
                    projection.receiver,
                    &projection.variant,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                )?;
                let payload = match resolved_variant_payload_scalar_or_inactive(
                    projection.receiver,
                    &projection.variant,
                    &projection.member,
                    &projection.payload_type,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                )? {
                    ResolvedScalarValue::Int(value) => value,
                    _ => return None,
                };
                let fallback = resolved_to_z3_int_with_catalog(
                    fallback, body, types, callables, variants, vars,
                )?;
                Some(tag.ite(&payload, &fallback))
            } else {
                resolved_variant_payload_int(
                    projection.receiver,
                    &projection.variant,
                    &projection.member,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                )
            }
        }
        _ => None,
    }
}

fn resolved_builtin_real_call(
    expr: &ResolvedExpr,
    call: &crate::core::ir::ResolvedCall,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Real> {
    let ResolvedCallee::Builtin(_) = &call.callee else {
        return None;
    };
    if expr.ty != call.result
        || z3_type_category(&expr.ty, types) != Z3TypeCategory::Real
        || call.permission != Some(crate::core::ir::Permission::Consume)
        || !call.effects.is_empty()
        || !call.session.is_empty()
        || !call.type_arguments.is_empty()
    {
        return None;
    }
    let projection = resolved_builtin_sum_projection(call, &expr.ty, types, variants)?;
    if z3_type_category(&projection.payload_type, types) != Z3TypeCategory::Real {
        return None;
    }
    if let Some(fallback) = projection.fallback {
        let tag = resolved_variant_tag_condition(
            projection.receiver,
            &projection.variant,
            body,
            types,
            callables,
            variants,
            vars,
        )?;
        let payload = match resolved_variant_payload_scalar_or_inactive(
            projection.receiver,
            &projection.variant,
            &projection.member,
            &projection.payload_type,
            body,
            types,
            callables,
            variants,
            vars,
        )? {
            ResolvedScalarValue::Real(value) => value,
            _ => return None,
        };
        let fallback =
            resolved_to_z3_real_with_catalog(fallback, body, types, callables, variants, vars)?;
        Some(tag.ite(&payload, &fallback))
    } else {
        resolved_variant_payload_real(
            projection.receiver,
            &projection.variant,
            &projection.member,
            body,
            types,
            callables,
            variants,
            vars,
        )
    }
}

fn resolved_builtin_bool_call(
    expr: &ResolvedExpr,
    call: &crate::core::ir::ResolvedCall,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Bool> {
    let ResolvedCallee::Builtin(builtin) = &call.callee else {
        return None;
    };
    if expr.ty != call.result
        || z3_type_category(&expr.ty, types) != Z3TypeCategory::Bool
        || !call.effects.is_empty()
        || !call.session.is_empty()
        || !call.type_arguments.is_empty()
    {
        return None;
    }
    if matches!(&call.callee, ResolvedCallee::Builtin(builtin) if matches!(builtin.as_str(),
        "builtin.method.option.unwrap" | "builtin.method.option.unwrap_or"
            | "builtin.method.result.unwrap" | "builtin.method.result.unwrap_err"
            | "builtin.method.result.unwrap_or"))
    {
        if call.permission != Some(crate::core::ir::Permission::Consume) {
            return None;
        }
        let projection = resolved_builtin_sum_projection(call, &expr.ty, types, variants)?;
        if z3_type_category(&projection.payload_type, types) != Z3TypeCategory::Bool {
            return None;
        }
        if let Some(fallback) = projection.fallback {
            let tag = resolved_variant_tag_condition(
                projection.receiver,
                &projection.variant,
                body,
                types,
                callables,
                variants,
                vars,
            )?;
            let payload = match resolved_variant_payload_scalar_or_inactive(
                projection.receiver,
                &projection.variant,
                &projection.member,
                &projection.payload_type,
                body,
                types,
                callables,
                variants,
                vars,
            )? {
                ResolvedScalarValue::Bool(value) => value,
                _ => return None,
            };
            let fallback =
                resolved_to_z3_bool_with_catalog(fallback, body, types, callables, variants, vars)?;
            return Some(tag.ite(&payload, &fallback));
        }
        return resolved_variant_payload_bool(
            projection.receiver,
            &projection.variant,
            &projection.member,
            body,
            types,
            callables,
            variants,
            vars,
        );
    }
    if builtin.as_str() == "builtin.method.set.is_empty" {
        if !resolved_set_builtin_call_is_closed(call, 1, Some(crate::core::ir::Permission::View)) {
            return None;
        }
        let (_, size) = resolved_set_state_at_path(
            &call.arguments[0].value,
            &[],
            &call.arguments[0].value.ty,
            body,
            types,
            callables,
            variants,
            vars,
        )?;
        return Some(size.eq(Z3Int::from_i64(0)));
    }
    if matches!(builtin.as_str(), "builtin.method.set.contains" | "contains") {
        let expected_permission = if builtin.as_str() == "contains" {
            None
        } else {
            Some(crate::core::ir::Permission::View)
        };
        if !resolved_set_builtin_call_is_closed(call, 2, expected_permission) {
            return None;
        }
        let element_ty = resolved_set_value_supported(&call.arguments[0].value.ty, types)?;
        let (set, _) = resolved_set_state_at_path(
            &call.arguments[0].value,
            &[],
            &call.arguments[0].value.ty,
            body,
            types,
            callables,
            variants,
            vars,
        )?;
        let elem = ResolvedSetElement::from_expr(
            &call.arguments[1].value,
            &element_ty,
            body,
            types,
            callables,
            variants,
            vars,
        )?;
        return Some(elem.member(&set));
    }
    if call.permission != Some(crate::core::ir::Permission::View)
        || !resolved_builtin_call_is_closed(call, Some(crate::core::ir::Permission::View))
    {
        return None;
    }
    let (receiver, expected_variant, negate) = match builtin.as_str() {
        "builtin.method.option.is_some" => (
            &call.arguments[0].value,
            NodeId("builtin:variant:Option::Some".into()),
            false,
        ),
        "builtin.method.option.is_none" => (
            &call.arguments[0].value,
            NodeId("builtin:variant:Option::Some".into()),
            true,
        ),
        "builtin.method.result.is_ok" => (
            &call.arguments[0].value,
            NodeId("builtin:variant:Result::Ok".into()),
            false,
        ),
        "builtin.method.result.is_err" => (
            &call.arguments[0].value,
            NodeId("builtin:variant:Result::Err".into()),
            false,
        ),
        _ => return None,
    };
    let condition = resolved_variant_tag_condition(
        receiver,
        &expected_variant,
        body,
        types,
        callables,
        variants,
        vars,
    )?;
    Some(if negate { condition.not() } else { condition })
}

pub(crate) fn resolved_block_stmts_with_catalog(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<()> {
    for statement in &block.statements {
        match &statement.kind {
            ResolvedStmtKind::Bind {
                pattern,
                initializer: Some(initializer),
            } => match &pattern.kind {
                ResolvedPatternKind::Binding {
                    local,
                    by_reference: None,
                } => {
                    let local = body.locals.get(local)?;
                    bind_value_to_local(
                        initializer,
                        body,
                        local,
                        types,
                        callables,
                        variants,
                        vars,
                        &mut BTreeSet::new(),
                    )?;
                }
                _ => resolved_bind_match_pattern(
                    initializer,
                    pattern,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                )?,
            },
            ResolvedStmtKind::Drop(places)
                if resolved_drop_is_value_neutral(places, body, types, vars)
                    || resolved_drop_has_checked_actions(
                        places,
                        &statement.node_id,
                        body,
                        callables,
                    ) => {}
            ResolvedStmtKind::Expr(ResolvedExpr {
                kind: ResolvedExprKind::Call(call),
                ..
            }) if matches!(&call.callee, ResolvedCallee::Builtin(id) if id.as_str() == "println")
                && resolved_builtin_call_is_closed(call, None)
                && call.type_arguments.is_empty() =>
            {
                // `println` is an output-only leaf in the supported proof
                // profile. Its argument definedness is collected separately.
            }
            // Root contracts and checked math points are discharged by the
            // verifier before it binds the returned value.
            ResolvedStmtKind::Contract { .. } | ResolvedStmtKind::Math(_) => {}
            _ => return None,
        }
    }
    Some(())
}

fn resolved_drop_has_checked_actions(
    places: &[crate::core::ir::ResolvedPlace],
    statement: &NodeId,
    body: &ResolvedBody,
    callables: &BTreeMap<NodeId, crate::core::ResolvedCallable>,
) -> bool {
    use crate::core::{CanonicalActionKind, PlaceProjection};

    if places.is_empty() {
        return false;
    }
    let Some(callable) = callables.get(&body.owner) else {
        return false;
    };
    if callable.owner != body.owner || callable.body.owner != body.owner {
        return false;
    }
    let drop_actions = callable
        .resources
        .actions
        .iter()
        .filter(|action| {
            action.kind == CanonicalActionKind::Drop && action.location.point == *statement
        })
        .collect::<Vec<_>>();
    if drop_actions.is_empty() {
        return false;
    }
    let place_matches = |resolved: &crate::core::ir::ResolvedPlace, actual: &crate::core::Place| {
        if resolved.base.0 != actual.base.0 {
            return false;
        }
        if resolved.projections.len() != actual.projections.len() {
            return false;
        }
        resolved
            .projections
            .iter()
            .zip(&actual.projections)
            .all(|(resolved, actual)| match (resolved, actual) {
                (
                    crate::core::ir::ResolvedProjection::Field { field, .. },
                    PlaceProjection::Field { field: actual, .. },
                ) => field == actual,
                (
                    crate::core::ir::ResolvedProjection::Tuple { index, .. },
                    PlaceProjection::Tuple(actual),
                ) => index == actual,
                _ => false,
            })
    };
    let mut resources = BTreeSet::new();
    drop_actions.iter().all(|action| {
        action.target.is_none()
            && action.loan.is_none()
            && action
                .source
                .as_ref()
                .is_some_and(|source| places.iter().any(|place| place_matches(place, source)))
            && resources.insert(action.resource.clone())
    })
}

pub(crate) fn resolved_block_tail_int_with_catalog(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Int> {
    resolved_block_stmts_with_catalog(block, body, types, callables, variants, vars)?;
    resolved_to_z3_int_with_catalog(
        block.result.as_deref()?,
        body,
        types,
        callables,
        variants,
        vars,
    )
}

pub(crate) fn resolved_block_tail_real_with_catalog(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Real> {
    resolved_block_stmts_with_catalog(block, body, types, callables, variants, vars)?;
    resolved_to_z3_real_with_catalog(
        block.result.as_deref()?,
        body,
        types,
        callables,
        variants,
        vars,
    )
}

pub(crate) fn resolved_block_tail_bool_with_catalog(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Bool> {
    resolved_block_stmts_with_catalog(block, body, types, callables, variants, vars)?;
    resolved_to_z3_bool_with_catalog(
        block.result.as_deref()?,
        body,
        types,
        callables,
        variants,
        vars,
    )
}

fn resolved_match_is_irrefutable(
    pattern: &crate::core::ir::ResolvedPattern,
    types: &ResolvedTypeTable,
    flow_state_fields: Option<&ResolvedFlowStateFieldCatalog>,
) -> bool {
    match &pattern.kind {
        ResolvedPatternKind::Wildcard
        | ResolvedPatternKind::Binding {
            by_reference: None, ..
        } => true,
        ResolvedPatternKind::Tuple(items) => items
            .iter()
            .all(|item| resolved_match_is_irrefutable(item, types, flow_state_fields)),
        ResolvedPatternKind::Constructor { variant, fields }
            if flow_state_pattern_fields(pattern, variant, fields, types, flow_state_fields)
                .is_some() =>
        {
            fields
                .iter()
                .all(|(_, child)| resolved_match_is_irrefutable(child, types, flow_state_fields))
        }
        _ => false,
    }
}

fn resolved_match_is_exhaustive(
    scrutinee: &ResolvedExpr,
    arms: &[crate::core::ir::MatchArm],
    types: &ResolvedTypeTable,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    flow_state_fields: Option<&ResolvedFlowStateFieldCatalog>,
) -> bool {
    if arms.iter().any(|arm| {
        arm.guard.is_none() && resolved_match_is_irrefutable(&arm.pattern, types, flow_state_fields)
    }) {
        return true;
    }
    let Some(sum_variants) = resolved_sum_variants(&scrutinee.ty, types, variants) else {
        return matches!(types.get(&scrutinee.ty), Some(ResolvedType::Primitive(PrimitiveType::Bool))
        if {
            let values = arms.iter().filter(|arm| arm.guard.is_none()).filter_map(|arm| match &arm.pattern.kind {
                ResolvedPatternKind::Literal(ResolvedLiteral::Bool(value)) => Some(*value),
                _ => None,
            }).collect::<BTreeSet<_>>();
            values.contains(&false) && values.contains(&true)
        });
    };
    let covered = arms
        .iter()
        .filter(|arm| arm.guard.is_none())
        .filter_map(|arm| match &arm.pattern.kind {
            ResolvedPatternKind::Constructor { variant, fields }
                if fields.iter().all(|(_, pattern)| {
                    resolved_match_is_irrefutable(pattern, types, flow_state_fields)
                }) =>
            {
                Some(variant.clone())
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    sum_variants
        .iter()
        .all(|(variant, _)| covered.contains(variant))
}

fn resolved_match_int_with_catalog(
    scrutinee: &ResolvedExpr,
    arms: &[crate::core::ir::MatchArm],
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Int> {
    if !resolved_match_is_exhaustive(scrutinee, arms, types, variants, None) {
        return None;
    }
    let mut result = None;
    for arm in arms.iter().rev() {
        let mut arm_vars = vars.clone();
        let mut condition = resolved_match_pattern_condition(
            scrutinee,
            &arm.pattern,
            body,
            types,
            callables,
            variants,
            &mut arm_vars,
        )?;
        resolved_bind_match_pattern(
            scrutinee,
            &arm.pattern,
            body,
            types,
            callables,
            variants,
            &mut arm_vars,
        )?;
        if let Some(guard) = &arm.guard {
            let guard = resolved_to_z3_bool_with_catalog(
                guard,
                body,
                types,
                callables,
                variants,
                &mut arm_vars,
            )?;
            condition = Z3Bool::and(&[&condition, &guard]);
        }
        let value = resolved_to_z3_int_with_catalog(
            &arm.body,
            body,
            types,
            callables,
            variants,
            &mut arm_vars,
        )?;
        result = Some(match result {
            Some(other) => condition.ite(&value, &other),
            None => value,
        });
    }
    result
}

fn resolved_match_int_in_context(
    scrutinee: &ResolvedExpr,
    arms: &[crate::core::ir::MatchArm],
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    context: &ResolvedDefinednessContext<'_>,
    vars: &mut Z3VarMap,
) -> Option<Z3Int> {
    let callables = context.callables?;
    let variants = context.variants?;
    if !resolved_match_is_exhaustive(scrutinee, arms, types, variants, context.flow_state_fields) {
        return None;
    }
    let mut result = None;
    for arm in arms.iter().rev() {
        let mut arm_vars = vars.clone();
        let mut condition = resolved_match_pattern_condition_with_flow_state_fields(
            scrutinee,
            &arm.pattern,
            body,
            types,
            callables,
            variants,
            context.flow_state_fields,
            &mut arm_vars,
        )?;
        resolved_bind_match_pattern_with_flow_state_fields(
            scrutinee,
            &arm.pattern,
            body,
            types,
            callables,
            variants,
            context.flow_state_fields,
            &mut arm_vars,
        )?;
        if let Some(guard) = &arm.guard {
            let guard = resolved_to_z3_bool_in_context(guard, body, types, &mut arm_vars, context)?;
            condition = Z3Bool::and(&[&condition, &guard]);
        }
        let value = resolved_to_z3_int_in_context(&arm.body, body, types, &mut arm_vars, context)?;
        result = Some(match result {
            Some(other) => condition.ite(&value, &other),
            None => value,
        });
    }
    result
}

fn resolved_match_real_in_context(
    scrutinee: &ResolvedExpr,
    arms: &[crate::core::ir::MatchArm],
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    context: &ResolvedDefinednessContext<'_>,
    vars: &mut Z3VarMap,
) -> Option<Z3Real> {
    let callables = context.callables?;
    let variants = context.variants?;
    if !resolved_match_is_exhaustive(scrutinee, arms, types, variants, context.flow_state_fields) {
        return None;
    }
    let mut result = None;
    for arm in arms.iter().rev() {
        let mut arm_vars = vars.clone();
        let mut condition = resolved_match_pattern_condition_with_flow_state_fields(
            scrutinee,
            &arm.pattern,
            body,
            types,
            callables,
            variants,
            context.flow_state_fields,
            &mut arm_vars,
        )?;
        resolved_bind_match_pattern_with_flow_state_fields(
            scrutinee,
            &arm.pattern,
            body,
            types,
            callables,
            variants,
            context.flow_state_fields,
            &mut arm_vars,
        )?;
        if let Some(guard) = &arm.guard {
            let guard = resolved_to_z3_bool_in_context(guard, body, types, &mut arm_vars, context)?;
            condition = Z3Bool::and(&[&condition, &guard]);
        }
        let value = resolved_to_z3_real_in_context(&arm.body, body, types, &mut arm_vars, context)?;
        result = Some(match result {
            Some(other) => condition.ite(&value, &other),
            None => value,
        });
    }
    result
}

fn resolved_match_bool_in_context(
    scrutinee: &ResolvedExpr,
    arms: &[crate::core::ir::MatchArm],
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    context: &ResolvedDefinednessContext<'_>,
    vars: &mut Z3VarMap,
) -> Option<Z3Bool> {
    let callables = context.callables?;
    let variants = context.variants?;
    if !resolved_match_is_exhaustive(scrutinee, arms, types, variants, context.flow_state_fields) {
        return None;
    }
    let mut result = None;
    for arm in arms.iter().rev() {
        let mut arm_vars = vars.clone();
        let mut condition = resolved_match_pattern_condition_with_flow_state_fields(
            scrutinee,
            &arm.pattern,
            body,
            types,
            callables,
            variants,
            context.flow_state_fields,
            &mut arm_vars,
        )?;
        resolved_bind_match_pattern_with_flow_state_fields(
            scrutinee,
            &arm.pattern,
            body,
            types,
            callables,
            variants,
            context.flow_state_fields,
            &mut arm_vars,
        )?;
        if let Some(guard) = &arm.guard {
            let guard = resolved_to_z3_bool_in_context(guard, body, types, &mut arm_vars, context)?;
            condition = Z3Bool::and(&[&condition, &guard]);
        }
        let value = resolved_to_z3_bool_in_context(&arm.body, body, types, &mut arm_vars, context)?;
        result = Some(match result {
            Some(other) => condition.ite(&value, &other),
            None => value,
        });
    }
    result
}

fn resolved_match_real_with_catalog(
    scrutinee: &ResolvedExpr,
    arms: &[crate::core::ir::MatchArm],
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Real> {
    if !resolved_match_is_exhaustive(scrutinee, arms, types, variants, None) {
        return None;
    }
    let mut result = None;
    for arm in arms.iter().rev() {
        let mut arm_vars = vars.clone();
        let mut condition = resolved_match_pattern_condition(
            scrutinee,
            &arm.pattern,
            body,
            types,
            callables,
            variants,
            &mut arm_vars,
        )?;
        resolved_bind_match_pattern(
            scrutinee,
            &arm.pattern,
            body,
            types,
            callables,
            variants,
            &mut arm_vars,
        )?;
        if let Some(guard) = &arm.guard {
            let guard = resolved_to_z3_bool_with_catalog(
                guard,
                body,
                types,
                callables,
                variants,
                &mut arm_vars,
            )?;
            condition = Z3Bool::and(&[&condition, &guard]);
        }
        let value = resolved_to_z3_real_with_catalog(
            &arm.body,
            body,
            types,
            callables,
            variants,
            &mut arm_vars,
        )?;
        result = Some(match result {
            Some(other) => condition.ite(&value, &other),
            None => value,
        });
    }
    result
}

fn resolved_match_bool_with_catalog(
    scrutinee: &ResolvedExpr,
    arms: &[crate::core::ir::MatchArm],
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Bool> {
    if !resolved_match_is_exhaustive(scrutinee, arms, types, variants, None) {
        return None;
    }
    let mut result = None;
    for arm in arms.iter().rev() {
        let mut arm_vars = vars.clone();
        let mut condition = resolved_match_pattern_condition(
            scrutinee,
            &arm.pattern,
            body,
            types,
            callables,
            variants,
            &mut arm_vars,
        )?;
        resolved_bind_match_pattern(
            scrutinee,
            &arm.pattern,
            body,
            types,
            callables,
            variants,
            &mut arm_vars,
        )?;
        if let Some(guard) = &arm.guard {
            let guard = resolved_to_z3_bool_with_catalog(
                guard,
                body,
                types,
                callables,
                variants,
                &mut arm_vars,
            )?;
            condition = Z3Bool::and(&[&condition, &guard]);
        }
        let value = resolved_to_z3_bool_with_catalog(
            &arm.body,
            body,
            types,
            callables,
            variants,
            &mut arm_vars,
        )?;
        result = Some(match result {
            Some(other) => condition.ite(&value, &other),
            None => value,
        });
    }
    result
}

pub(crate) fn resolved_to_z3_int_with_catalog(
    expr: &ResolvedExpr,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Int> {
    match &expr.kind {
        ResolvedExprKind::Call(call) => match &call.callee {
            ResolvedCallee::Builtin(_) => {
                resolved_builtin_int_call(expr, call, body, types, callables, variants, vars)
            }
            ResolvedCallee::Function(_) => {
                resolved_inline_scalar_call_int(call, body, types, callables, variants, vars)
            }
            _ => None,
        },
        ResolvedExprKind::Match { scrutinee, arms } => {
            resolved_match_int_with_catalog(scrutinee, arms, body, types, callables, variants, vars)
        }
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            let condition = resolved_to_z3_bool_with_catalog(
                condition, body, types, callables, variants, vars,
            )?;
            let then_value = resolved_block_tail_int_with_catalog(
                then_block, body, types, callables, variants, vars,
            )?;
            let else_value = resolved_block_tail_int_with_catalog(
                else_block, body, types, callables, variants, vars,
            )?;
            Some(condition.ite(&then_value, &else_value))
        }
        ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
            resolved_block_tail_int_with_catalog(block, body, types, callables, variants, vars)
        }
        ResolvedExprKind::Try {
            value,
            propagation_target,
        } if body.owner.0.starts_with("transition:") && propagation_target == &body.owner => {
            resolved_variant_payload_int(
                value,
                &NodeId("builtin:variant:Result::Ok".into()),
                &NodeId("builtin:variant:Result::Ok/payload:0".into()),
                body,
                types,
                callables,
                variants,
                vars,
            )
        }
        ResolvedExprKind::Load(place) => {
            let key = local_key(place, body, &expr.ty)?;
            if z3_type_category(&expr.ty, types) != Z3TypeCategory::Int {
                return None;
            }
            if place.projections.is_empty() {
                vars.get_int(&key).cloned()
            } else {
                Some(vars.get_or_create_int(&key))
            }
        }
        ResolvedExprKind::Project { value, projection } => {
            if z3_type_category(&expr.ty, types) != Z3TypeCategory::Int {
                return None;
            }
            let base = resolved_field_var_name(value, body)?;
            let projection = match projection {
                ResolvedValueProjection::Field(field) => field.0.clone(),
                ResolvedValueProjection::Tuple(index) => format!("t{index}"),
                ResolvedValueProjection::Index(_) | ResolvedValueProjection::Dereference => {
                    return None;
                }
            };
            Some(vars.get_or_create_int(&format!("{base}.{projection}")))
        }
        ResolvedExprKind::Literal(ResolvedLiteral::Int(value)) => Some(Z3Int::from_i64(*value)),
        ResolvedExprKind::Literal(ResolvedLiteral::Bool(value)) => {
            Some(Z3Int::from_i64(i64::from(*value)))
        }
        ResolvedExprKind::Old(inner) => {
            if !immutable_parameter_projection(inner, body) {
                return None;
            }
            resolved_to_z3_int_with_catalog(inner, body, types, callables, variants, vars)
        }
        ResolvedExprKind::Binary { op, left, right } => {
            let left =
                resolved_to_z3_int_with_catalog(left, body, types, callables, variants, vars)?;
            let right =
                resolved_to_z3_int_with_catalog(right, body, types, callables, variants, vars)?;
            match op {
                ResolvedBinaryOp::Add => Some(Z3Int::add(&[&left, &right])),
                ResolvedBinaryOp::Subtract => Some(Z3Int::sub(&[&left, &right])),
                ResolvedBinaryOp::Multiply => Some(Z3Int::mul(&[&left, &right])),
                ResolvedBinaryOp::Divide => {
                    let zero = Z3Int::from_i64(0);
                    let abs_left = left.ge(&zero).ite(&left, &left.unary_minus());
                    let abs_right = right.ge(&zero).ite(&right, &right.unary_minus());
                    let quotient = abs_left.div(&abs_right);
                    let same_sign = left.ge(&zero).eq(&right.ge(&zero));
                    Some(same_sign.ite(&quotient, &quotient.unary_minus()))
                }
                ResolvedBinaryOp::Remainder => {
                    let zero = Z3Int::from_i64(0);
                    let abs_left = left.ge(&zero).ite(&left, &left.unary_minus());
                    let abs_right = right.ge(&zero).ite(&right, &right.unary_minus());
                    let remainder = abs_left.modulo(&abs_right);
                    Some(left.ge(&zero).ite(&remainder, &remainder.unary_minus()))
                }
                _ => None,
            }
        }
        ResolvedExprKind::Unary {
            op: ResolvedUnaryOp::Negate,
            operand,
        } => Some(
            resolved_to_z3_int_with_catalog(operand, body, types, callables, variants, vars)?
                .unary_minus(),
        ),
        _ => None,
    }
}

pub(crate) fn resolved_to_z3_real_with_catalog(
    expr: &ResolvedExpr,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Real> {
    match &expr.kind {
        ResolvedExprKind::Call(call) => match &call.callee {
            ResolvedCallee::Builtin(_) => {
                resolved_builtin_real_call(expr, call, body, types, callables, variants, vars)
            }
            ResolvedCallee::Function(_) => {
                resolved_inline_scalar_call_real(call, body, types, callables, variants, vars)
            }
            _ => None,
        },
        ResolvedExprKind::Match { scrutinee, arms } => resolved_match_real_with_catalog(
            scrutinee, arms, body, types, callables, variants, vars,
        ),
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            let condition = resolved_to_z3_bool_with_catalog(
                condition, body, types, callables, variants, vars,
            )?;
            let then_value = resolved_block_tail_real_with_catalog(
                then_block, body, types, callables, variants, vars,
            )?;
            let else_value = resolved_block_tail_real_with_catalog(
                else_block, body, types, callables, variants, vars,
            )?;
            Some(condition.ite(&then_value, &else_value))
        }
        ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
            resolved_block_tail_real_with_catalog(block, body, types, callables, variants, vars)
        }
        ResolvedExprKind::Try {
            value,
            propagation_target,
        } if body.owner.0.starts_with("transition:") && propagation_target == &body.owner => {
            resolved_variant_payload_real(
                value,
                &NodeId("builtin:variant:Result::Ok".into()),
                &NodeId("builtin:variant:Result::Ok/payload:0".into()),
                body,
                types,
                callables,
                variants,
                vars,
            )
        }
        ResolvedExprKind::Load(place) => {
            let key = local_key(place, body, &expr.ty)?;
            if z3_type_category(&expr.ty, types) != Z3TypeCategory::Real {
                return None;
            }
            if place.projections.is_empty() {
                vars.get_real(&key).cloned()
            } else {
                Some(vars.get_or_create_real(&key))
            }
        }
        ResolvedExprKind::Project { value, projection } => {
            if z3_type_category(&expr.ty, types) != Z3TypeCategory::Real {
                return None;
            }
            let base = resolved_field_var_name(value, body)?;
            let projection = match projection {
                ResolvedValueProjection::Field(field) => field.0.clone(),
                ResolvedValueProjection::Tuple(index) => format!("t{index}"),
                ResolvedValueProjection::Index(_) | ResolvedValueProjection::Dereference => {
                    return None;
                }
            };
            Some(vars.get_or_create_real(&format!("{base}.{projection}")))
        }
        ResolvedExprKind::Literal(ResolvedLiteral::FloatBits(bits)) => {
            (f64::from_bits(*bits) == 0.0).then(|| Z3Real::from_int(&Z3Int::from_i64(0)))
        }
        ResolvedExprKind::Literal(ResolvedLiteral::Int(value)) => {
            Some(Z3Real::from_int(&Z3Int::from_i64(*value)))
        }
        ResolvedExprKind::Old(inner) => {
            if !immutable_parameter_projection(inner, body) {
                return None;
            }
            resolved_to_z3_real_with_catalog(inner, body, types, callables, variants, vars)
        }
        ResolvedExprKind::Binary { op, left, right } => {
            let left =
                resolved_to_z3_real_with_catalog(left, body, types, callables, variants, vars)?;
            let right =
                resolved_to_z3_real_with_catalog(right, body, types, callables, variants, vars)?;
            match op {
                ResolvedBinaryOp::Add => Some(Z3Real::add(&[&left, &right])),
                ResolvedBinaryOp::Subtract => Some(Z3Real::sub(&[&left, &right])),
                ResolvedBinaryOp::Multiply => Some(Z3Real::mul(&[&left, &right])),
                ResolvedBinaryOp::Divide => Some(left.div(&right)),
                _ => None,
            }
        }
        ResolvedExprKind::Unary {
            op: ResolvedUnaryOp::Negate,
            operand,
        } => Some(
            resolved_to_z3_real_with_catalog(operand, body, types, callables, variants, vars)?
                .unary_minus(),
        ),
        _ => None,
    }
}

pub(crate) fn resolved_to_z3_bool_with_catalog(
    expr: &ResolvedExpr,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Bool> {
    match &expr.kind {
        ResolvedExprKind::Call(call) => match &call.callee {
            ResolvedCallee::Builtin(_) => {
                resolved_builtin_bool_call(expr, call, body, types, callables, variants, vars)
            }
            ResolvedCallee::Function(_) => {
                resolved_inline_scalar_call_bool(call, body, types, callables, variants, vars)
            }
            _ => None,
        },
        ResolvedExprKind::Match { scrutinee, arms } => resolved_match_bool_with_catalog(
            scrutinee, arms, body, types, callables, variants, vars,
        ),
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            let condition = resolved_to_z3_bool_with_catalog(
                condition, body, types, callables, variants, vars,
            )?;
            let then_value = resolved_block_tail_bool_with_catalog(
                then_block, body, types, callables, variants, vars,
            )?;
            let else_value = resolved_block_tail_bool_with_catalog(
                else_block, body, types, callables, variants, vars,
            )?;
            Some(condition.ite(&then_value, &else_value))
        }
        ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
            resolved_block_tail_bool_with_catalog(block, body, types, callables, variants, vars)
        }
        ResolvedExprKind::Try {
            value,
            propagation_target,
        } if body.owner.0.starts_with("transition:") && propagation_target == &body.owner => {
            resolved_variant_payload_bool(
                value,
                &NodeId("builtin:variant:Result::Ok".into()),
                &NodeId("builtin:variant:Result::Ok/payload:0".into()),
                body,
                types,
                callables,
                variants,
                vars,
            )
        }
        ResolvedExprKind::Literal(ResolvedLiteral::Bool(value)) => Some(Z3Bool::from_bool(*value)),
        ResolvedExprKind::Load(place) => {
            let key = local_key(place, body, &expr.ty)?;
            if z3_type_category(&expr.ty, types) != Z3TypeCategory::Bool {
                return None;
            }
            if place.projections.is_empty() {
                vars.get_bool(&key).cloned()
            } else {
                Some(vars.get_or_create_bool(&key))
            }
        }
        ResolvedExprKind::Project { value, projection } => {
            if z3_type_category(&expr.ty, types) != Z3TypeCategory::Bool {
                return None;
            }
            let base = resolved_field_var_name(value, body)?;
            let projection = match projection {
                ResolvedValueProjection::Field(field) => field.0.clone(),
                ResolvedValueProjection::Tuple(index) => format!("t{index}"),
                ResolvedValueProjection::Index(_) | ResolvedValueProjection::Dereference => {
                    return None;
                }
            };
            Some(vars.get_or_create_bool(&format!("{base}.{projection}")))
        }
        ResolvedExprKind::Binary { op, left, right } => {
            if matches!(
                op,
                ResolvedBinaryOp::LogicalAnd | ResolvedBinaryOp::LogicalOr
            ) {
                let left =
                    resolved_to_z3_bool_with_catalog(left, body, types, callables, variants, vars)?;
                let right = resolved_to_z3_bool_with_catalog(
                    right, body, types, callables, variants, vars,
                )?;
                return match op {
                    ResolvedBinaryOp::LogicalAnd => Some(Z3Bool::and(&[&left, &right])),
                    ResolvedBinaryOp::LogicalOr => Some(Z3Bool::or(&[&left, &right])),
                    _ => None,
                };
            }
            if let (Some(left), Some(right)) = (
                resolved_to_z3_int_with_catalog(left, body, types, callables, variants, vars),
                resolved_to_z3_int_with_catalog(right, body, types, callables, variants, vars),
            ) {
                return match op {
                    ResolvedBinaryOp::Equal => Some(left.eq(&right)),
                    ResolvedBinaryOp::NotEqual => Some(left.eq(&right).not()),
                    ResolvedBinaryOp::Less => Some(left.lt(&right)),
                    ResolvedBinaryOp::Greater => Some(left.gt(&right)),
                    ResolvedBinaryOp::LessEqual => Some(left.le(&right)),
                    ResolvedBinaryOp::GreaterEqual => Some(left.ge(&right)),
                    _ => None,
                };
            }
            if let (Some(left), Some(right)) = (
                resolved_to_z3_real_with_catalog(left, body, types, callables, variants, vars),
                resolved_to_z3_real_with_catalog(right, body, types, callables, variants, vars),
            ) {
                return match op {
                    ResolvedBinaryOp::Equal => Some(left.eq(&right)),
                    ResolvedBinaryOp::NotEqual => Some(left.eq(&right).not()),
                    ResolvedBinaryOp::Less => Some(left.lt(&right)),
                    ResolvedBinaryOp::Greater => Some(left.gt(&right)),
                    ResolvedBinaryOp::LessEqual => Some(left.le(&right)),
                    ResolvedBinaryOp::GreaterEqual => Some(left.ge(&right)),
                    _ => None,
                };
            }
            let left =
                resolved_to_z3_bool_with_catalog(left, body, types, callables, variants, vars)?;
            let right =
                resolved_to_z3_bool_with_catalog(right, body, types, callables, variants, vars)?;
            match op {
                ResolvedBinaryOp::Equal => Some(left.eq(&right)),
                ResolvedBinaryOp::NotEqual => Some(left.eq(&right).not()),
                _ => None,
            }
        }
        ResolvedExprKind::Unary {
            op: ResolvedUnaryOp::Not,
            operand,
        } => Some(
            resolved_to_z3_bool_with_catalog(operand, body, types, callables, variants, vars)?
                .not(),
        ),
        ResolvedExprKind::Old(inner) => {
            if !immutable_parameter_projection(inner, body) {
                return None;
            }
            resolved_to_z3_bool_with_catalog(inner, body, types, callables, variants, vars)
        }
        _ => None,
    }
}

fn immutable_parameter_projection(expression: &ResolvedExpr, body: &ResolvedBody) -> bool {
    match &expression.kind {
        ResolvedExprKind::Load(place) => body.locals.get(&place.base).is_some_and(|local| {
            !local.mutable
                && body.parameters.contains(&place.base)
                && place.projected_type(local) == &expression.ty
        }),
        ResolvedExprKind::Project { value, projection } => {
            !matches!(
                projection,
                ResolvedValueProjection::Index(_) | ResolvedValueProjection::Dereference
            ) && immutable_parameter_projection(value, body)
        }
        _ => false,
    }
}

fn projected_scalar_value(
    value: &ResolvedExpr,
    path: &[ResolvedValuePathPart],
    expected_type: &crate::core::ir::ResolvedTypeId,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<ResolvedScalarValue> {
    if path.is_empty() {
        // A scalar leaf is only substituted when its canonical type identity
        // matches exactly. Same-category i32/i64 coercions are not equality.
        if &value.ty != expected_type {
            return None;
        }
        return scalar_value_of_expr_with_catalog(value, body, types, callables, variants, vars);
    }
    match &value.kind {
        ResolvedExprKind::Load(_) => {
            let key = value_base_key(value, body)?;
            scalar_value_at_key(&append_value_path(&key, path), expected_type, types, vars)
        }
        ResolvedExprKind::Project {
            value: base,
            projection,
        } => {
            let part = match projection {
                ResolvedValueProjection::Field(field) => {
                    ResolvedValuePathPart::Field(field.clone())
                }
                ResolvedValueProjection::Tuple(index) => ResolvedValuePathPart::Tuple(*index),
                ResolvedValueProjection::Index(_) | ResolvedValueProjection::Dereference => {
                    return None;
                }
            };
            let mut combined = Vec::with_capacity(path.len() + 1);
            combined.push(part);
            combined.extend_from_slice(path);
            projected_scalar_value(
                base,
                &combined,
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            )
        }
        ResolvedExprKind::Record { fields, rest, .. } => {
            let ResolvedValuePathPart::Field(field_id) = &path[0] else {
                return None;
            };
            if let Some(field) = fields.iter().find(|field| &field.field == field_id) {
                return projected_scalar_value(
                    &field.value,
                    &path[1..],
                    expected_type,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                );
            }
            projected_scalar_value(
                rest.as_deref()?,
                path,
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            )
        }
        ResolvedExprKind::Tuple(items) => {
            let ResolvedValuePathPart::Tuple(index) = &path[0] else {
                return None;
            };
            projected_scalar_value(
                items.get(*index)?,
                &path[1..],
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            )
        }
        ResolvedExprKind::Call(call) => match &call.callee {
            ResolvedCallee::Constructor(variant) => {
                if call.result != value.ty
                    || call.permission.is_some()
                    || !call.effects.is_empty()
                    || !call.session.is_empty()
                {
                    return None;
                }
                let ResolvedValuePathPart::VariantPayload {
                    variant: path_variant,
                    member,
                } = &path[0]
                else {
                    return None;
                };
                if path_variant != variant {
                    return None;
                }
                let argument = call
                    .arguments
                    .iter()
                    .find(|argument| argument.parameter.0 == *member)
                    .or_else(|| (call.arguments.len() == 1).then(|| &call.arguments[0]))?;
                if resolved_variant_member_type(&value.ty, variant, member, types, variants)?
                    != argument.value.ty
                {
                    return None;
                }
                projected_scalar_value(
                    &argument.value,
                    &path[1..],
                    expected_type,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                )
            }
            ResolvedCallee::Builtin(_) => {
                if call.result != value.ty {
                    return None;
                }
                let ResolvedValuePathPart::VariantPayload { variant, member } = &path[0] else {
                    return None;
                };
                let argument = resolved_builtin_sum_constructor_payload(
                    call, variant, member, types, variants,
                )?;
                projected_scalar_value(
                    argument,
                    &path[1..],
                    expected_type,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                )
            }
            ResolvedCallee::Function(_) => {
                let (tail, callee_body, mut callee_vars) = inline_pure_call_tail(
                    call,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                    &mut BTreeSet::new(),
                )?;
                projected_scalar_value(
                    &tail,
                    path,
                    expected_type,
                    &callee_body,
                    types,
                    callables,
                    variants,
                    &mut callee_vars,
                )
            }
            _ => None,
        },
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            let condition = resolved_to_z3_bool_with_catalog(
                condition, body, types, callables, variants, vars,
            )?;
            if let [ResolvedValuePathPart::VariantPayload { variant, member }] = path {
                let then_value = resolved_block_payload_scalar_or_inactive(
                    then_block,
                    variant,
                    member,
                    expected_type,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                )?;
                let else_value = resolved_block_payload_scalar_or_inactive(
                    else_block,
                    variant,
                    member,
                    expected_type,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                )?;
                return scalar_ite(condition, then_value, else_value);
            }
            let then_value = projected_block_scalar_value(
                then_block,
                path,
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            )?;
            let else_value = projected_block_scalar_value(
                else_block,
                path,
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            )?;
            scalar_ite(condition, then_value, else_value)
        }
        ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
            projected_block_scalar_value(
                block,
                path,
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            )
        }
        _ => None,
    }
}

fn projected_block_scalar_value(
    block: &ResolvedBlock,
    path: &[ResolvedValuePathPart],
    expected_type: &crate::core::ir::ResolvedTypeId,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<ResolvedScalarValue> {
    resolved_block_stmts_with_catalog(block, body, types, callables, variants, vars)?;
    projected_scalar_value(
        block.result.as_deref()?,
        path,
        expected_type,
        body,
        types,
        callables,
        variants,
        vars,
    )
}

fn resolved_variant_tag_at_path(
    value: &ResolvedExpr,
    path: &[ResolvedValuePathPart],
    expected_type: &crate::core::ir::ResolvedTypeId,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Int> {
    let source_type =
        resolved_expression_type_at_path(value, path, body, types, callables, variants)?;
    if !resolved_sum_shapes_match(&source_type, expected_type, types) {
        return None;
    }
    if path.is_empty() {
        // Type arguments may be generic at the declaration site. The tag
        // representation depends on the outer Option/Result/enum identity,
        // not on its payload type, so compare canonical sum shape rather than
        // requiring identical payload type IDs.
        return resolved_variant_tag_int(value, body, types, callables, variants, vars);
    }
    match &value.kind {
        ResolvedExprKind::Load(_) => {
            let base = value_base_key(value, body)?;
            Some(vars.get_or_create_int(&format!("{}.tag", append_value_path(&base, path))))
        }
        ResolvedExprKind::Project {
            value: base,
            projection,
        } => {
            let part = match projection {
                ResolvedValueProjection::Field(field) => {
                    ResolvedValuePathPart::Field(field.clone())
                }
                ResolvedValueProjection::Tuple(index) => ResolvedValuePathPart::Tuple(*index),
                _ => return None,
            };
            let mut combined = vec![part];
            combined.extend_from_slice(path);
            resolved_variant_tag_at_path(
                base,
                &combined,
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            )
        }
        ResolvedExprKind::Tuple(items) => {
            let ResolvedValuePathPart::Tuple(index) = &path[0] else {
                return None;
            };
            resolved_variant_tag_at_path(
                items.get(*index)?,
                &path[1..],
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            )
        }
        ResolvedExprKind::Record { fields, rest, .. } => {
            let ResolvedValuePathPart::Field(field_id) = &path[0] else {
                return None;
            };
            if let Some(field) = fields.iter().find(|field| &field.field == field_id) {
                return resolved_variant_tag_at_path(
                    &field.value,
                    &path[1..],
                    expected_type,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                );
            }
            resolved_variant_tag_at_path(
                rest.as_deref()?,
                path,
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            )
        }
        ResolvedExprKind::Call(call) => match &call.callee {
            ResolvedCallee::Constructor(variant) => {
                if call.result != value.ty
                    || call.permission.is_some()
                    || !call.effects.is_empty()
                    || !call.session.is_empty()
                {
                    return None;
                }
                let ResolvedValuePathPart::VariantPayload {
                    variant: path_variant,
                    member,
                } = &path[0]
                else {
                    return None;
                };
                if path_variant != variant {
                    return None;
                }
                let argument = call
                    .arguments
                    .iter()
                    .find(|argument| argument.parameter.0 == *member)
                    .or_else(|| (call.arguments.len() == 1).then(|| &call.arguments[0]))?;
                if resolved_variant_member_type(&value.ty, variant, member, types, variants)?
                    != argument.value.ty
                {
                    return None;
                }
                resolved_variant_tag_at_path(
                    &argument.value,
                    &path[1..],
                    expected_type,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                )
            }
            ResolvedCallee::Builtin(_) => {
                if call.result != value.ty {
                    return None;
                }
                let ResolvedValuePathPart::VariantPayload { variant, member } = &path[0] else {
                    return None;
                };
                let argument = resolved_builtin_sum_constructor_payload(
                    call, variant, member, types, variants,
                )?;
                resolved_variant_tag_at_path(
                    argument,
                    &path[1..],
                    expected_type,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                )
            }
            ResolvedCallee::Function(_) => {
                let (tail, callee_body, mut callee_vars) = inline_pure_call_tail(
                    call,
                    body,
                    types,
                    callables,
                    variants,
                    vars,
                    &mut BTreeSet::new(),
                )?;
                resolved_variant_tag_at_path(
                    &tail,
                    path,
                    expected_type,
                    &callee_body,
                    types,
                    callables,
                    variants,
                    &mut callee_vars,
                )
            }
            _ => None,
        },
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            let condition = resolved_to_z3_bool_with_catalog(
                condition, body, types, callables, variants, vars,
            )?;
            let then_tag = resolved_block_tail_tag_at_path(
                then_block,
                path,
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            )?;
            let else_tag = resolved_block_tail_tag_at_path(
                else_block,
                path,
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            )?;
            Some(condition.ite(&then_tag, &else_tag))
        }
        ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
            resolved_block_stmts_with_catalog(block, body, types, callables, variants, vars)?;
            resolved_variant_tag_at_path(
                block.result.as_deref()?,
                path,
                expected_type,
                body,
                types,
                callables,
                variants,
                vars,
            )
        }
        _ => None,
    }
}

fn resolved_block_tail_tag_at_path(
    block: &ResolvedBlock,
    path: &[ResolvedValuePathPart],
    expected_type: &crate::core::ir::ResolvedTypeId,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Int> {
    resolved_block_stmts_with_catalog(block, body, types, callables, variants, vars)?;
    resolved_variant_tag_at_path(
        block.result.as_deref()?,
        path,
        expected_type,
        body,
        types,
        callables,
        variants,
        vars,
    )
}

fn pattern_condition_at_path(
    value: &ResolvedExpr,
    pattern: &crate::core::ir::ResolvedPattern,
    path: &[ResolvedValuePathPart],
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    flow_state_fields: Option<&ResolvedFlowStateFieldCatalog>,
    vars: &mut Z3VarMap,
) -> Option<Z3Bool> {
    match &pattern.kind {
        ResolvedPatternKind::Wildcard | ResolvedPatternKind::Binding { .. } => {
            Some(Z3Bool::from_bool(true))
        }
        ResolvedPatternKind::Literal(ResolvedLiteral::Int(expected)) => {
            if let Some(source_type) = resolved_flow_state_type_at_path(
                &value.ty,
                path,
                types,
                variants,
                flow_state_fields,
            ) {
                if source_type != pattern.ty {
                    return None;
                }
            }
            match projected_scalar_value(
                value,
                path,
                &pattern.ty,
                body,
                types,
                callables,
                variants,
                vars,
            )? {
                ResolvedScalarValue::Int(actual) => Some(actual.eq(&Z3Int::from_i64(*expected))),
                _ => None,
            }
        }
        ResolvedPatternKind::Literal(ResolvedLiteral::Bool(expected)) => {
            if let Some(source_type) = resolved_flow_state_type_at_path(
                &value.ty,
                path,
                types,
                variants,
                flow_state_fields,
            ) {
                if source_type != pattern.ty {
                    return None;
                }
            }
            match projected_scalar_value(
                value,
                path,
                &pattern.ty,
                body,
                types,
                callables,
                variants,
                vars,
            )? {
                ResolvedScalarValue::Bool(actual) => Some(actual.eq(&Z3Bool::from_bool(*expected))),
                _ => None,
            }
        }
        ResolvedPatternKind::Literal(_) => None,
        ResolvedPatternKind::Constructor { variant, fields } => {
            if let Some(schema) =
                flow_state_pattern_fields(pattern, variant, fields, types, flow_state_fields)
            {
                if resolved_flow_state_type_at_path(
                    &value.ty,
                    path,
                    types,
                    variants,
                    flow_state_fields,
                )? != pattern.ty
                {
                    return None;
                }
                let mut conditions = Vec::with_capacity(schema.len());
                for (field, child) in fields {
                    let mut child_path = path.to_vec();
                    child_path.push(ResolvedValuePathPart::Field(field.clone()));
                    conditions.push(pattern_condition_at_path(
                        value,
                        child,
                        &child_path,
                        body,
                        types,
                        callables,
                        variants,
                        flow_state_fields,
                        vars,
                    )?);
                }
                let refs = conditions.iter().collect::<Vec<_>>();
                return Some(Z3Bool::and(&refs));
            }
            if flow_state_pattern_candidate(pattern, variant, types) {
                return None;
            }
            if let Some(source_type) = resolved_flow_state_type_at_path(
                &value.ty,
                path,
                types,
                variants,
                flow_state_fields,
            ) {
                if source_type != pattern.ty {
                    return None;
                }
            }
            let tag = resolved_pattern_variant_tag_at_path(
                value,
                path,
                &pattern.ty,
                body,
                types,
                callables,
                variants,
                flow_state_fields,
                vars,
            )?;
            let expected = resolved_variant_tag_number(&pattern.ty, variant, types, variants)?;
            let mut conditions = vec![tag.eq(&Z3Int::from_i64(expected))];
            for (member, child) in fields {
                let mut child_path = path.to_vec();
                child_path.push(ResolvedValuePathPart::VariantPayload {
                    variant: variant.clone(),
                    member: member.clone(),
                });
                conditions.push(pattern_condition_at_path(
                    value,
                    child,
                    &child_path,
                    body,
                    types,
                    callables,
                    variants,
                    flow_state_fields,
                    vars,
                )?);
            }
            let refs = conditions.iter().collect::<Vec<_>>();
            Some(Z3Bool::and(&refs))
        }
        ResolvedPatternKind::Tuple(patterns) => {
            let mut conditions = Vec::with_capacity(patterns.len());
            for (index, child) in patterns.iter().enumerate() {
                let mut child_path = path.to_vec();
                child_path.push(ResolvedValuePathPart::Tuple(index));
                conditions.push(pattern_condition_at_path(
                    value,
                    child,
                    &child_path,
                    body,
                    types,
                    callables,
                    variants,
                    flow_state_fields,
                    vars,
                )?);
            }
            let refs = conditions.iter().collect::<Vec<_>>();
            Some(Z3Bool::and(&refs))
        }
        ResolvedPatternKind::Array(_) | ResolvedPatternKind::Slice { .. } => None,
    }
}

pub(crate) fn resolved_match_pattern_condition(
    scrutinee: &ResolvedExpr,
    pattern: &crate::core::ir::ResolvedPattern,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<Z3Bool> {
    resolved_match_pattern_condition_with_flow_state_fields(
        scrutinee, pattern, body, types, callables, variants, None, vars,
    )
}

pub(crate) fn resolved_match_pattern_condition_with_flow_state_fields(
    scrutinee: &ResolvedExpr,
    pattern: &crate::core::ir::ResolvedPattern,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    flow_state_fields: Option<&ResolvedFlowStateFieldCatalog>,
    vars: &mut Z3VarMap,
) -> Option<Z3Bool> {
    pattern_condition_at_path(
        scrutinee,
        pattern,
        &[],
        body,
        types,
        callables,
        variants,
        flow_state_fields,
        vars,
    )
}

fn bind_pattern_at_path(
    scrutinee: &ResolvedExpr,
    pattern: &crate::core::ir::ResolvedPattern,
    path: &[ResolvedValuePathPart],
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    flow_state_fields: Option<&ResolvedFlowStateFieldCatalog>,
    vars: &mut Z3VarMap,
    active: &mut BTreeSet<NodeId>,
) -> Option<()> {
    match &pattern.kind {
        ResolvedPatternKind::Binding {
            local,
            by_reference: None,
        } => {
            let local = body.locals.get(local)?;
            let key = local_var_key(&local.id, &local.display_name);
            if path
                .iter()
                .any(|part| matches!(part, ResolvedValuePathPart::Field(_)))
            {
                if let Some(source_type) = resolved_flow_state_type_at_path(
                    &scrutinee.ty,
                    path,
                    types,
                    variants,
                    flow_state_fields,
                ) {
                    if source_type != local.ty {
                        return None;
                    }
                    if matches!(&scrutinee.kind, ResolvedExprKind::Load(_)) {
                        return bind_flow_state_scalar_at_path(
                            scrutinee, path, &local.ty, &key, body, types, vars,
                        );
                    }
                }
            }
            bind_value_at_path(
                scrutinee, path, &local.ty, &key, body, types, callables, variants, vars, active,
            )
        }
        ResolvedPatternKind::Binding { .. } => None,
        ResolvedPatternKind::Constructor { variant, fields } => {
            let state_schema =
                flow_state_pattern_fields(pattern, variant, fields, types, flow_state_fields);
            if state_schema.is_none() && flow_state_pattern_candidate(pattern, variant, types) {
                return None;
            }
            if let Some(schema) = state_schema {
                if resolved_flow_state_type_at_path(
                    &scrutinee.ty,
                    path,
                    types,
                    variants,
                    flow_state_fields,
                )? != pattern.ty
                {
                    return None;
                }
                for (field, child) in fields {
                    if !schema.contains_key(field) {
                        return None;
                    }
                    let mut child_path = path.to_vec();
                    child_path.push(ResolvedValuePathPart::Field(field.clone()));
                    bind_pattern_at_path(
                        scrutinee,
                        child,
                        &child_path,
                        body,
                        types,
                        callables,
                        variants,
                        flow_state_fields,
                        vars,
                        active,
                    )?;
                }
                return Some(());
            }
            for (member, child) in fields {
                let mut child_path = path.to_vec();
                child_path.push(ResolvedValuePathPart::VariantPayload {
                    variant: variant.clone(),
                    member: member.clone(),
                });
                bind_pattern_at_path(
                    scrutinee,
                    child,
                    &child_path,
                    body,
                    types,
                    callables,
                    variants,
                    flow_state_fields,
                    vars,
                    active,
                )?;
            }
            Some(())
        }
        ResolvedPatternKind::Tuple(patterns) => {
            for (index, child) in patterns.iter().enumerate() {
                let mut child_path = path.to_vec();
                child_path.push(ResolvedValuePathPart::Tuple(index));
                bind_pattern_at_path(
                    scrutinee,
                    child,
                    &child_path,
                    body,
                    types,
                    callables,
                    variants,
                    flow_state_fields,
                    vars,
                    active,
                )?;
            }
            Some(())
        }
        ResolvedPatternKind::Wildcard | ResolvedPatternKind::Literal(_) => Some(()),
        ResolvedPatternKind::Array(_) | ResolvedPatternKind::Slice { .. } => None,
    }
}

pub(crate) fn resolved_bind_match_pattern(
    scrutinee: &ResolvedExpr,
    pattern: &crate::core::ir::ResolvedPattern,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ir::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    vars: &mut Z3VarMap,
) -> Option<()> {
    resolved_bind_match_pattern_with_flow_state_fields(
        scrutinee, pattern, body, types, callables, variants, None, vars,
    )
}

pub(crate) fn resolved_bind_match_pattern_with_flow_state_fields(
    scrutinee: &ResolvedExpr,
    pattern: &crate::core::ir::ResolvedPattern,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<NodeId, crate::core::ResolvedCallable>,
    variants: &BTreeMap<NodeId, ResolvedVariantSchema>,
    flow_state_fields: Option<&ResolvedFlowStateFieldCatalog>,
    vars: &mut Z3VarMap,
) -> Option<()> {
    bind_pattern_at_path(
        scrutinee,
        pattern,
        &[],
        body,
        types,
        callables,
        variants,
        flow_state_fields,
        vars,
        &mut BTreeSet::new(),
    )
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

/// Catalog available to the Resolved evaluator when checking expression
/// definedness and inlining a pure direct call. Compatibility-only wrappers
/// leave both indexes absent and fail closed on cross-call identities.
struct ResolvedDefinednessContext<'a> {
    callables: Option<&'a BTreeMap<crate::core::NodeId, crate::core::ResolvedCallable>>,
    variants: Option<&'a BTreeMap<crate::core::NodeId, crate::core::ResolvedVariantSchema>>,
    flow_state_fields: Option<&'a ResolvedFlowStateFieldCatalog>,
    active_calls: BTreeSet<crate::core::NodeId>,
    /// Checker-owned session actions whose exact endpoint transition and
    /// argument definedness were validated by the statement collector. The
    /// value encoder may elide only these already-checked protocol statements
    /// while retaining the actual scalar locals they bind.
    validated_session_statements: BTreeSet<(crate::core::NodeId, crate::core::NodeId)>,
    /// Finite discriminant domains follow from the checker-owned sum type.
    /// They are static typing premises, not user proof obligations: asking a
    /// caller to prove that an arbitrary Z3 integer is a valid tag would make
    /// every symbolic exhaustive match spuriously Disproven.
    type_premises: Vec<Z3Bool>,
    /// Current lexical continuation path. A `?` inside an admitted failing
    /// transition constrains only the normal continuation to its Ok tag.
    /// This guard scopes later arithmetic VCs without assuming that the
    /// Result succeeds.
    active_path: Z3Bool,
}

impl<'a> ResolvedDefinednessContext<'a> {
    fn without_catalog() -> Self {
        Self {
            callables: None,
            variants: None,
            flow_state_fields: None,
            active_calls: BTreeSet::new(),
            validated_session_statements: BTreeSet::new(),
            type_premises: Vec::new(),
            active_path: Z3Bool::from_bool(true),
        }
    }

    fn with_flow_state_catalog(
        callables: &'a BTreeMap<crate::core::NodeId, crate::core::ResolvedCallable>,
        variants: &'a BTreeMap<crate::core::NodeId, crate::core::ResolvedVariantSchema>,
        flow_state_fields: Option<&'a ResolvedFlowStateFieldCatalog>,
    ) -> Self {
        Self {
            callables: Some(callables),
            variants: Some(variants),
            flow_state_fields,
            active_calls: BTreeSet::new(),
            validated_session_statements: BTreeSet::new(),
            type_premises: Vec::new(),
            active_path: Z3Bool::from_bool(true),
        }
    }

    fn assert_type_premises(&mut self, session: &mut crate::verifier::ctx::SolverSession) {
        for premise in self.type_premises.drain(..) {
            session.solver.assert(premise);
        }
    }
}

fn resolved_to_z3_int_in_context(
    expr: &ResolvedExpr,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
    context: &ResolvedDefinednessContext<'_>,
) -> Option<Z3Int> {
    match (context.callables, context.variants) {
        (Some(callables), Some(variants)) => match &expr.kind {
            ResolvedExprKind::Match { scrutinee, arms } => {
                resolved_match_int_in_context(scrutinee, arms, body, types, context, vars)
            }
            _ => resolved_to_z3_int_with_catalog(expr, body, types, callables, variants, vars),
        },
        _ => resolved_to_z3_int(expr, body, types, vars),
    }
}

fn resolved_to_z3_real_in_context(
    expr: &ResolvedExpr,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
    context: &ResolvedDefinednessContext<'_>,
) -> Option<Z3Real> {
    match (context.callables, context.variants) {
        (Some(_), Some(_)) => match &expr.kind {
            ResolvedExprKind::Match { scrutinee, arms } => {
                resolved_match_real_in_context(scrutinee, arms, body, types, context, vars)
            }
            _ => resolved_to_z3_real_with_catalog(
                expr,
                body,
                types,
                context.callables?,
                context.variants?,
                vars,
            ),
        },
        _ => resolved_to_z3_real(expr, body, types, vars),
    }
}

fn resolved_to_z3_bool_in_context(
    expr: &ResolvedExpr,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
    context: &ResolvedDefinednessContext<'_>,
) -> Option<Z3Bool> {
    match (context.callables, context.variants) {
        (Some(_), Some(_)) => match &expr.kind {
            ResolvedExprKind::Match { scrutinee, arms } => {
                resolved_match_bool_in_context(scrutinee, arms, body, types, context, vars)
            }
            _ => resolved_to_z3_bool_with_catalog(
                expr,
                body,
                types,
                context.callables?,
                context.variants?,
                vars,
            ),
        },
        _ => resolved_to_z3_bool(expr, body, types, vars),
    }
}

/// Detect only the direct statement shapes for which the proof collector has
/// a typed SessionChan residual receipt. Nested or value-producing session
/// calls remain outside the scalar tail encoder and fail closed.
fn block_has_session_value_statement_candidate(block: &ResolvedBlock) -> bool {
    fn is_session_call(expression: &ResolvedExpr) -> bool {
        let ResolvedExprKind::Call(call) = &expression.kind else {
            return false;
        };
        [
            "session_pair",
            "session_send",
            "session_recv",
            "session_close",
        ]
        .iter()
        .any(|name| is_session_builtin(call, name))
    }

    block
        .statements
        .iter()
        .any(|statement| match &statement.kind {
            ResolvedStmtKind::Bind {
                initializer: Some(initializer),
                ..
            } => is_session_call(initializer),
            ResolvedStmtKind::Expr(expression) => is_session_call(expression),
            _ => false,
        })
}

/// Remove the exact protocol statements whose validation receipts were
/// produced by the strict definedness collector. The remaining pure block is
/// still encoded by the shared catalog encoder. In particular, receive
/// symbols are read from their checker-local key; this helper never creates,
/// widens, or substitutes a result value of its own.
fn block_without_validated_session_statements(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &Z3VarMap,
    context: &ResolvedDefinednessContext<'_>,
) -> Option<ResolvedBlock> {
    let mut filtered = block.clone();
    let mut kept = Vec::with_capacity(block.statements.len());
    let mut removed_session_statement = false;

    for statement in &block.statements {
        let mut remove = false;
        match &statement.kind {
            ResolvedStmtKind::Bind {
                pattern,
                initializer: Some(initializer),
            } => {
                let ResolvedExprKind::Call(call) = &initializer.kind else {
                    kept.push(statement.clone());
                    continue;
                };
                if is_session_builtin(call, "session_pair") {
                    if !context
                        .validated_session_statements
                        .contains(&(body.owner.clone(), statement.node_id.clone()))
                        || !initializer.effects.is_empty()
                        || initializer.ty != call.result
                    {
                        return None;
                    }
                    valid_session_pair_binding(initializer, pattern, body, types)?;
                    remove = true;
                } else if is_session_builtin(call, "session_recv") {
                    if !context
                        .validated_session_statements
                        .contains(&(body.owner.clone(), statement.node_id.clone()))
                        || !initializer.effects.is_empty()
                        || initializer.ty != call.result
                    {
                        return None;
                    }
                    valid_session_operation(call, "session_recv", types)?;
                    let ResolvedPatternKind::Binding {
                        local,
                        by_reference: None,
                    } = &pattern.kind
                    else {
                        return None;
                    };
                    let local_info = body.locals.get(local)?;
                    if pattern.ty != call.result
                        || local_info.ty != call.result
                        || !matches!(
                            types.get(&call.result),
                            Some(ResolvedType::Primitive(
                                PrimitiveType::I32 | PrimitiveType::I64
                            ))
                        )
                    {
                        return None;
                    }
                    let key = local_var_key(local, &local_info.display_name);
                    vars.get_int(&key)?;
                    remove = true;
                } else if ["session_send", "session_close"]
                    .iter()
                    .any(|name| is_session_builtin(call, name))
                {
                    // Binding unit-valued sends/closes is outside this narrow
                    // value profile even if the protocol action itself is
                    // otherwise well typed.
                    return None;
                }
            }
            ResolvedStmtKind::Expr(expression) => {
                let ResolvedExprKind::Call(call) = &expression.kind else {
                    kept.push(statement.clone());
                    continue;
                };
                let name = ["session_send", "session_recv", "session_close"]
                    .into_iter()
                    .find(|name| is_session_builtin(call, name));
                if let Some(name) = name {
                    if !context
                        .validated_session_statements
                        .contains(&(body.owner.clone(), statement.node_id.clone()))
                        || !expression.effects.is_empty()
                        || expression.ty != call.result
                    {
                        return None;
                    }
                    valid_session_operation(call, name, types)?;
                    remove = true;
                } else if is_session_builtin(call, "session_pair") {
                    return None;
                }
            }
            _ => {}
        }
        if remove {
            removed_session_statement = true;
        } else {
            kept.push(statement.clone());
        }
    }

    if !removed_session_statement {
        return None;
    }
    filtered.statements = kept;
    Some(filtered)
}

fn resolved_prepare_context_value_block_stmts(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
    context: &ResolvedDefinednessContext<'_>,
) -> Option<()> {
    match (context.callables, context.variants) {
        (Some(callables), Some(variants)) => {
            if block_has_session_value_statement_candidate(block) {
                let filtered =
                    block_without_validated_session_statements(block, body, types, vars, context)?;
                crate::verifier::resolved_expr::resolved_block_stmts_with_catalog(
                    &filtered, body, types, callables, variants, vars,
                )
            } else {
                crate::verifier::resolved_expr::resolved_block_stmts_with_catalog(
                    block, body, types, callables, variants, vars,
                )
            }
        }
        _ => block
            .statements
            .iter()
            .all(|statement| {
                matches!(
                    &statement.kind,
                    ResolvedStmtKind::Contract { .. } | ResolvedStmtKind::Math(_)
                )
            })
            .then_some(()),
    }
}

fn resolved_block_tail_int_with_context_and_session_receipts(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
    context: &ResolvedDefinednessContext<'_>,
) -> Option<Z3Int> {
    resolved_prepare_context_value_block_stmts(block, body, types, vars, context)?;
    resolved_to_z3_int_in_context(block.result.as_deref()?, body, types, vars, context)
}

fn resolved_block_tail_int_with_context(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
    context: &ResolvedDefinednessContext<'_>,
) -> Option<Z3Int> {
    resolved_prepare_context_value_block_stmts(block, body, types, vars, context)?;
    resolved_to_z3_int_in_context(block.result.as_deref()?, body, types, vars, context)
}

fn resolved_block_tail_real_with_context_and_session_receipts(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
    context: &ResolvedDefinednessContext<'_>,
) -> Option<Z3Real> {
    resolved_prepare_context_value_block_stmts(block, body, types, vars, context)?;
    resolved_to_z3_real_in_context(block.result.as_deref()?, body, types, vars, context)
}

fn resolved_block_tail_bool_with_context_and_session_receipts(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
    context: &ResolvedDefinednessContext<'_>,
) -> Option<Z3Bool> {
    resolved_prepare_context_value_block_stmts(block, body, types, vars, context)?;
    resolved_to_z3_bool_in_context(block.result.as_deref()?, body, types, vars, context)
}

fn resolved_block_tail_real_with_context(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
    context: &ResolvedDefinednessContext<'_>,
) -> Option<Z3Real> {
    resolved_prepare_context_value_block_stmts(block, body, types, vars, context)?;
    resolved_to_z3_real_in_context(block.result.as_deref()?, body, types, vars, context)
}

fn resolved_block_tail_bool_with_context(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
    context: &ResolvedDefinednessContext<'_>,
) -> Option<Z3Bool> {
    resolved_prepare_context_value_block_stmts(block, body, types, vars, context)?;
    resolved_to_z3_bool_in_context(block.result.as_deref()?, body, types, vars, context)
}

fn resolved_block_tail_int_in_context(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
    context: &ResolvedDefinednessContext<'_>,
) -> Option<Z3Int> {
    match (context.callables, context.variants) {
        (Some(_), Some(_)) => {
            if block_has_session_value_statement_candidate(block) {
                resolved_block_tail_int_with_context_and_session_receipts(
                    block, body, types, vars, context,
                )
            } else {
                resolved_block_tail_int_with_context(block, body, types, vars, context)
            }
        }
        _ => resolved_block_tail_int(block, body, types, vars),
    }
}

fn resolved_block_tail_real_in_context(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
    context: &ResolvedDefinednessContext<'_>,
) -> Option<Z3Real> {
    match (context.callables, context.variants) {
        (Some(_), Some(_)) => {
            if block_has_session_value_statement_candidate(block) {
                resolved_block_tail_real_with_context_and_session_receipts(
                    block, body, types, vars, context,
                )
            } else {
                resolved_block_tail_real_with_context(block, body, types, vars, context)
            }
        }
        _ => resolved_block_tail_real(block, body, types, vars),
    }
}

fn resolved_block_tail_bool_in_context(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
    context: &ResolvedDefinednessContext<'_>,
) -> Option<Z3Bool> {
    match (context.callables, context.variants) {
        (Some(_), Some(_)) => {
            if block_has_session_value_statement_candidate(block) {
                resolved_block_tail_bool_with_context_and_session_receipts(
                    block, body, types, vars, context,
                )
            } else {
                resolved_block_tail_bool_with_context(block, body, types, vars, context)
            }
        }
        _ => resolved_block_tail_bool(block, body, types, vars),
    }
}

/// Extract only a single terminal, identity-converted explicit scalar return.
/// Branching/multiple returns and non-identity ABI conversions stay outside
/// this encoding path.
fn terminal_explicit_scalar_return<'a>(
    block: &'a ResolvedBlock,
    expected_type: &crate::core::ir::ResolvedTypeId,
) -> Option<(&'a ResolvedExpr, ResolvedBlock)> {
    if block.result.is_some() {
        return None;
    }
    let mut found = None;
    for (index, statement) in block.statements.iter().enumerate() {
        if let ResolvedStmtKind::Return {
            value: Some(value),
            conversion: Some(conversion),
        } = &statement.kind
        {
            if found.is_some() {
                return None;
            }
            found = Some((index, value, conversion));
        } else if matches!(&statement.kind, ResolvedStmtKind::Return { .. }) {
            return None;
        }
    }
    let (index, value, conversion) = found?;
    if index + 1 != block.statements.len()
        || !value.effects.is_empty()
        || &value.ty != expected_type
        || conversion.kind != CheckedConversionKind::Identity
        || conversion.from != value.ty
        || &conversion.to != expected_type
    {
        return None;
    }
    let mut prefix = block.clone();
    prefix.statements.truncate(index);
    prefix.result = None;
    Some((value, prefix))
}

/// Whether a callable contains a checker-resolved direct call whose target
/// has a call-site precondition or checked-integer body that the Resolved
/// evaluator must discharge. This keeps ordinary contract-free leaf functions
/// out of the result stream while still covering helper-only libraries.
pub(crate) fn has_resolved_callsite_obligations(
    callable: &crate::core::ResolvedCallable,
    callables: &BTreeMap<crate::core::NodeId, crate::core::ResolvedCallable>,
    types: &ResolvedTypeTable,
) -> bool {
    fn target_needs_proof(
        target: &crate::core::NodeId,
        callables: &BTreeMap<crate::core::NodeId, crate::core::ResolvedCallable>,
        types: &ResolvedTypeTable,
        active: &mut BTreeSet<crate::core::NodeId>,
    ) -> bool {
        if !active.insert(target.clone()) {
            return false;
        }
        let Some(callee) = callables.get(target) else {
            active.remove(target);
            return false;
        };
        let needs = !root_contracts_for(callee, ContractKind::Requires).is_empty()
            || block_has_checked_integer_operation(&callee.body.root, &callee.body, types)
            || callee.body.resolved_calls.values().any(|nested| {
                matches!(nested, crate::core::ir::ResolvedCallee::Function(owner)
                    if target_needs_proof(owner, callables, types, active))
            });
        active.remove(target);
        needs
    }

    let mut active = BTreeSet::new();
    callable.body.resolved_calls.values().any(|callee| {
        matches!(callee, crate::core::ir::ResolvedCallee::Function(target)
            if target_needs_proof(target, callables, types, &mut active))
    })
}

fn block_has_checked_integer_operation(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
) -> bool {
    fn expr_has(expr: &ResolvedExpr, body: &ResolvedBody, types: &ResolvedTypeTable) -> bool {
        let own = match &expr.kind {
            ResolvedExprKind::Binary { op, .. } => {
                matches!(
                    op,
                    ResolvedBinaryOp::Add
                        | ResolvedBinaryOp::Subtract
                        | ResolvedBinaryOp::Multiply
                        | ResolvedBinaryOp::Divide
                        | ResolvedBinaryOp::Remainder
                        | ResolvedBinaryOp::Power
                ) && int_bounds(expr, types).is_some()
            }
            ResolvedExprKind::Unary {
                op: ResolvedUnaryOp::Negate,
                ..
            } => int_bounds(expr, types).is_some(),
            _ => false,
        };
        own || match &expr.kind {
            ResolvedExprKind::FString(parts) => parts.iter().any(|part| {
                matches!(part,
                crate::core::ir::ResolvedFStringPart::Interpolation(value)
                    if expr_has(value, body, types))
            }),
            ResolvedExprKind::Project { value, projection } => {
                expr_has(value, body, types)
                    || matches!(projection, ResolvedValueProjection::Index(index)
                    if expr_has(index, body, types))
            }
            ResolvedExprKind::Binary { left, right, .. } => {
                expr_has(left, body, types) || expr_has(right, body, types)
            }
            ResolvedExprKind::Unary { operand, .. } => expr_has(operand, body, types),
            ResolvedExprKind::Call(call) => {
                let trap_bearing_builtin = matches!(
                    &call.callee,
                    crate::core::ir::ResolvedCallee::Builtin(builtin)
                        if matches!(builtin.as_str(),
                            "builtin.method.option.unwrap"
                                | "builtin.method.option.expect"
                                | "builtin.method.result.unwrap"
                                | "builtin.method.result.unwrap_err"
                                | "builtin.method.result.expect"
                                | "builtin.method.session.send"
                                | "builtin.method.session.recv"
                                | "builtin.method.session.close"
                                | "session_send"
                                | "session_recv"
                                | "session_close")
                );
                trap_bearing_builtin
                    || call
                        .arguments
                        .iter()
                        .any(|argument| expr_has(&argument.value, body, types))
            }
            ResolvedExprKind::Cast { value, conversion } => {
                conversion_may_trap(conversion.kind) || expr_has(value, body, types)
            }
            ResolvedExprKind::Tuple(values)
            | ResolvedExprKind::List(values)
            | ResolvedExprKind::Set(values) => {
                values.iter().any(|value| expr_has(value, body, types))
            }
            ResolvedExprKind::Map(entries) => entries
                .iter()
                .any(|(key, value)| expr_has(key, body, types) || expr_has(value, body, types)),
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                expr_has(value, body, types)
                    || expr_has(iterable, body, types)
                    || guard
                        .as_ref()
                        .is_some_and(|guard| expr_has(guard, body, types))
            }
            ResolvedExprKind::OptionalChain { receiver, .. }
            | ResolvedExprKind::TypeOf(receiver)
            | ResolvedExprKind::Old(receiver)
            | ResolvedExprKind::Try {
                value: receiver, ..
            }
            | ResolvedExprKind::Spawn(receiver)
            | ResolvedExprKind::Await(receiver) => expr_has(receiver, body, types),
            ResolvedExprKind::Record { fields, rest, .. } => {
                fields
                    .iter()
                    .any(|field| expr_has(&field.value, body, types))
                    || rest
                        .as_ref()
                        .is_some_and(|rest| expr_has(rest, body, types))
            }
            ResolvedExprKind::Block(block) | ResolvedExprKind::Comptime(block) => {
                block_has_checked_integer_operation(block, body, types)
            }
            ResolvedExprKind::Scope { body: block, .. } => {
                block_has_checked_integer_operation(block, body, types)
            }
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                expr_has(condition, body, types)
                    || block_has_checked_integer_operation(then_block, body, types)
                    || block_has_checked_integer_operation(else_block, body, types)
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                expr_has(scrutinee, body, types)
                    || arms.iter().any(|arm| {
                        arm.guard
                            .as_ref()
                            .is_some_and(|guard| expr_has(guard, body, types))
                            || expr_has(&arm.body, body, types)
                    })
            }
            ResolvedExprKind::Range { start, end } => {
                expr_has(start, body, types) || expr_has(end, body, types)
            }
            ResolvedExprKind::Slice { target, start, end } => {
                expr_has(target, body, types)
                    || start
                        .as_ref()
                        .is_some_and(|value| expr_has(value, body, types))
                    || end
                        .as_ref()
                        .is_some_and(|value| expr_has(value, body, types))
            }
            ResolvedExprKind::Lambda(lambda) => {
                block_has_checked_integer_operation(&lambda.body, body, types)
            }
            _ => false,
        }
    }

    block
        .statements
        .iter()
        .any(|statement| match &statement.kind {
            ResolvedStmtKind::Bind { initializer, .. } => initializer
                .as_ref()
                .is_some_and(|expr| expr_has(expr, body, types)),
            ResolvedStmtKind::Assign { value, .. } | ResolvedStmtKind::Expr(value) => {
                expr_has(value, body, types)
            }
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => value
                .as_ref()
                .is_some_and(|expr| expr_has(expr, body, types)),
            ResolvedStmtKind::While {
                condition,
                body: inner,
            } => {
                expr_has(condition, body, types)
                    || block_has_checked_integer_operation(inner, body, types)
            }
            ResolvedStmtKind::WhileLet {
                initializer,
                body: inner,
                ..
            } => {
                expr_has(initializer, body, types)
                    || block_has_checked_integer_operation(inner, body, types)
            }
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                expr_has(initializer, body, types)
                    || block_has_checked_integer_operation(then_block, body, types)
                    || else_block.as_ref().is_some_and(|inner| {
                        block_has_checked_integer_operation(inner, body, types)
                    })
            }
            ResolvedStmtKind::Loop(inner) | ResolvedStmtKind::Scope { body: inner, .. } => {
                block_has_checked_integer_operation(inner, body, types)
            }
            ResolvedStmtKind::Pinned {
                value, body: inner, ..
            } => {
                expr_has(value, body, types)
                    || block_has_checked_integer_operation(inner, body, types)
            }
            ResolvedStmtKind::For {
                iterable,
                body: inner,
                ..
            } => {
                expr_has(iterable, body, types)
                    || block_has_checked_integer_operation(inner, body, types)
            }
            ResolvedStmtKind::Math(exprs) => exprs.iter().any(|expr| expr_has(expr, body, types)),
            ResolvedStmtKind::Contract { condition, .. } => expr_has(condition, body, types),
            ResolvedStmtKind::Continue
            | ResolvedStmtKind::Drop(_)
            | ResolvedStmtKind::NestedCallable(_) => false,
        })
        || block
            .result
            .as_ref()
            .is_some_and(|expr| expr_has(expr, body, types))
}

/// One definedness or call-precondition verification condition for checked scalar semantics.
pub(crate) struct ResolvedIntDefinedness {
    pub(crate) condition: Z3Bool,
    pub(crate) failure: &'static str,
    /// A failed source-width premise is a verifier model gap, not a runtime
    /// counterexample. Keep it fail-closed as NITS instead of E0800/E0802.
    pub(crate) modeling_gap: bool,
    /// True only for a callee `requires` obligation at a direct call site.
    /// Definedness obligations keep non-entry contract-free helpers hidden once
    /// discharged, whereas comparison-only call preconditions surface `Proven`.
    pub(crate) is_call_precondition: bool,
}

/// Bounds for a checked (trap-semantics) integer type. SD-7: integer
/// overflow traps at runtime (E0801); the verifier must model every checked
/// integer type with its real bounds — i32 previously got obligations while
/// i64 was modeled unbounded, so `i64::MAX + 1` verified Proven even though
/// the runtime traps (H2).
fn int_bounds(expr: &ResolvedExpr, types: &ResolvedTypeTable) -> Option<(i64, i64)> {
    match types.get(&expr.ty)? {
        ResolvedType::Primitive(PrimitiveType::I32) => Some((i32::MIN as i64, i32::MAX as i64)),
        ResolvedType::Primitive(PrimitiveType::I64) => Some((i64::MIN, i64::MAX)),
        _ => None,
    }
}

/// H-24 + H2: every checked (trap-semantics) integer sub-expression generates
/// a definedness obligation in its OWN bounds — i32 and i64 (the language's
/// checked integer types; SD-7 traps on overflow for both). The old code only
/// handled i32 and left i64 unbounded: `ensures: result > x; x + 1` verified
/// Proven at x == i64::MAX while the runtime traps. Add/Sub/Mul must stay in
/// range, Div/Rem needs a non-zero divisor and must not be MIN / -1, Negate
/// must not hit MIN.
fn collect_expr_i32_definedness(
    expr: &ResolvedExpr,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
    obligations: &mut Vec<ResolvedIntDefinedness>,
) -> Option<()> {
    let mut context = ResolvedDefinednessContext::without_catalog();
    collect_expr_i32_definedness_with_context(expr, body, types, vars, obligations, &mut context)
}

fn collect_expr_i32_definedness_with_context(
    expr: &ResolvedExpr,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
    obligations: &mut Vec<ResolvedIntDefinedness>,
    context: &mut ResolvedDefinednessContext<'_>,
) -> Option<()> {
    collect_expr_i32_definedness_scoped(
        expr,
        body,
        types,
        vars,
        obligations,
        &BTreeSet::new(),
        context,
    )
}

fn collect_expr_i32_definedness_scoped(
    expr: &ResolvedExpr,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
    obligations: &mut Vec<ResolvedIntDefinedness>,
    stationary_loops: &BTreeSet<crate::core::NodeId>,
    context: &mut ResolvedDefinednessContext<'_>,
) -> Option<()> {
    let bounds = int_bounds(expr, types);
    match &expr.kind {
        ResolvedExprKind::Binary { op, left, right } => {
            if matches!(
                op,
                ResolvedBinaryOp::LogicalAnd | ResolvedBinaryOp::LogicalOr
            ) {
                // Mimi evaluates logical operators left-to-right with
                // short-circuiting. The Z3 boolean encoder may use ordinary
                // `and`/`or` for the result only after definedness is checked
                // under the actual path on which each operand executes.
                let mut left_vars = vars.clone();
                let mut left_obligations = Vec::new();
                collect_expr_i32_definedness_scoped(
                    left,
                    body,
                    types,
                    &mut left_vars,
                    &mut left_obligations,
                    stationary_loops,
                    context,
                )?;
                obligations.extend(left_obligations);
                let left_value =
                    resolved_to_z3_bool_in_context(left, body, types, &mut left_vars, context)?;

                let mut right_vars = vars.clone();
                let mut right_obligations = Vec::new();
                collect_expr_i32_definedness_scoped(
                    right,
                    body,
                    types,
                    &mut right_vars,
                    &mut right_obligations,
                    stationary_loops,
                    context,
                )?;
                let right_guard = match op {
                    ResolvedBinaryOp::LogicalAnd => left_value,
                    ResolvedBinaryOp::LogicalOr => left_value.not(),
                    _ => unreachable!("short-circuit guard only applies to logical operators"),
                };
                for obligation in &mut right_obligations {
                    obligation.condition = right_guard.implies(&obligation.condition);
                }
                obligations.extend(right_obligations);
                return Some(());
            }
            collect_expr_i32_definedness_scoped(
                left,
                body,
                types,
                vars,
                obligations,
                stationary_loops,
                context,
            )?;
            collect_expr_i32_definedness_scoped(
                right,
                body,
                types,
                vars,
                obligations,
                stationary_loops,
                context,
            )?;
            // The phase profile matches VM/native i64::checked_pow followed
            // by a successful i32 wrap. Keep its exponent-domain and
            // intermediate-multiply traps separate from operand obligations.
            if matches!(op, ResolvedBinaryOp::Power) {
                let width = checked_power_source_width(expr, left, right, types)?;
                let l = resolved_to_z3_int_in_context(left, body, types, vars, context)?;
                let r = resolved_to_z3_int_in_context(right, body, types, vars, context)?;
                let encoding =
                    encode_checked_integer_power(&l, &r, width, POWER_PROFILE_0_1_11_COMPAT);
                // Typed source ranges are static premises, not runtime Power
                // traps. Prove them first so a missing projected/range pin is
                // reported as NITS; the runtime definedness VC then consists
                // only of negative exponent, u32 conversion, and checked-i64
                // intermediate multiplication failures.
                obligations.push(ResolvedIntDefinedness {
                    condition: encoding.faults.operands_outside_source_width.not(),
                    failure:
                        "integer power operand escaped its typed source range (verifier model gap)",
                    modeling_gap: true,
                    is_call_precondition: false,
                });
                obligations.push(ResolvedIntDefinedness {
                    condition: encoding.defined,
                    failure: "integer power may trap (E0800 negative or >u32 exponent; E0802 checked i64 overflow)",
                    modeling_gap: false,
                    is_call_precondition: false,
                });
                return Some(());
            }
            if let Some((lo, hi)) = bounds {
                let l = resolved_to_z3_int_in_context(left, body, types, vars, context)?;
                let r = resolved_to_z3_int_in_context(right, body, types, vars, context)?;
                match op {
                    ResolvedBinaryOp::Add
                    | ResolvedBinaryOp::Subtract
                    | ResolvedBinaryOp::Multiply => {
                        let value = match op {
                            ResolvedBinaryOp::Add => Z3Int::add(&[&l, &r]),
                            ResolvedBinaryOp::Subtract => Z3Int::sub(&[&l, &r]),
                            ResolvedBinaryOp::Multiply => Z3Int::mul(&[&l, &r]),
                            _ => unreachable!(),
                        };
                        let lo = Z3Int::from_i64(lo);
                        let hi = Z3Int::from_i64(hi);
                        obligations.push(ResolvedIntDefinedness {
                            condition: Z3Bool::and(&[&value.ge(&lo), &value.le(&hi)]),
                            failure: "integer overflow is not excluded by preconditions",
                            modeling_gap: false,
                            is_call_precondition: false,
                        });
                    }
                    ResolvedBinaryOp::Divide | ResolvedBinaryOp::Remainder => {
                        let zero = Z3Int::from_i64(0);
                        let min = Z3Int::from_i64(lo);
                        let neg_one = Z3Int::from_i64(-1);
                        let min_overflow = Z3Bool::and(&[&l.eq(&min), &r.eq(&neg_one)]);
                        obligations.push(ResolvedIntDefinedness {
                            condition: Z3Bool::and(&[&r.ne(&zero), &min_overflow.not()]),
                            failure: "integer operation is undefined (zero divisor or MIN / -1)",
                            modeling_gap: false,
                            is_call_precondition: false,
                        });
                    }
                    _ => {}
                }
            }
        }
        ResolvedExprKind::Unary { op, operand } => {
            collect_expr_i32_definedness_scoped(
                operand,
                body,
                types,
                vars,
                obligations,
                stationary_loops,
                context,
            )?;
            match op {
                ResolvedUnaryOp::Negate => {
                    if let Some((lo, _)) = bounds {
                        let value =
                            resolved_to_z3_int_in_context(operand, body, types, vars, context)?;
                        obligations.push(ResolvedIntDefinedness {
                            condition: value.ne(&Z3Int::from_i64(lo)),
                            failure: "integer overflow is not excluded by preconditions",
                            modeling_gap: false,
                            is_call_precondition: false,
                        });
                    }
                }
                ResolvedUnaryOp::Not => {}
                ResolvedUnaryOp::BorrowShared
                | ResolvedUnaryOp::BorrowMutable
                | ResolvedUnaryOp::Dereference => return None,
            }
        }
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            let mut condition_vars = vars.clone();
            let mut condition_obligations = Vec::new();
            collect_expr_i32_definedness_scoped(
                condition,
                body,
                types,
                &mut condition_vars,
                &mut condition_obligations,
                stationary_loops,
                context,
            )?;
            obligations.extend(condition_obligations);
            let cond = resolved_to_z3_bool_in_context(
                condition,
                body,
                types,
                &mut condition_vars,
                context,
            )?;
            let mut then_vars = vars.clone();
            let mut then_obligations = Vec::new();
            collect_block_i32_definedness_scoped(
                then_block,
                body,
                types,
                &mut then_vars,
                &mut then_obligations,
                stationary_loops,
                context,
            )?;
            for obligation in &mut then_obligations {
                obligation.condition = cond.implies(&obligation.condition);
            }
            obligations.extend(then_obligations);
            let mut else_vars = vars.clone();
            let mut else_obligations = Vec::new();
            collect_block_i32_definedness_scoped(
                else_block,
                body,
                types,
                &mut else_vars,
                &mut else_obligations,
                stationary_loops,
                context,
            )?;
            for obligation in &mut else_obligations {
                obligation.condition = cond.not().implies(&obligation.condition);
            }
            obligations.extend(else_obligations);
        }
        ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
            let mut scoped_vars = vars.clone();
            collect_block_i32_definedness_scoped(
                block,
                body,
                types,
                &mut scoped_vars,
                obligations,
                stationary_loops,
                context,
            )?;
        }
        ResolvedExprKind::Project { value, projection } => {
            let value_supported = collect_expr_i32_definedness_scoped(
                value,
                body,
                types,
                vars,
                obligations,
                stationary_loops,
                context,
            )
            .is_some();
            let projection_supported = match projection {
                ResolvedValueProjection::Field(_) | ResolvedValueProjection::Tuple(_) => true,
                ResolvedValueProjection::Index(index) => {
                    let _ = collect_expr_i32_definedness_scoped(
                        index,
                        body,
                        types,
                        vars,
                        obligations,
                        stationary_loops,
                        context,
                    );
                    false
                }
                ResolvedValueProjection::Dereference => false,
            };
            if !value_supported || !projection_supported {
                return None;
            }
        }
        ResolvedExprKind::Load(place) => {
            if place.projections.iter().any(|projection| {
                matches!(
                    projection,
                    ResolvedProjection::Index { .. } | ResolvedProjection::Deref { .. }
                )
            }) {
                for projection in &place.projections {
                    if let ResolvedProjection::Index {
                        index: ResolvedIndex::Dynamic(node),
                        ..
                    } = projection
                    {
                        if let Some(index) = body.place_inputs.get(node) {
                            let _ = collect_expr_i32_definedness_scoped(
                                index,
                                body,
                                types,
                                vars,
                                obligations,
                                stationary_loops,
                                context,
                            );
                        }
                    }
                }
                return None;
            }
            // A checker-resolved Field/Tuple path is a total typed load. Its
            // scalar value is represented by the same stable key used by the
            // arithmetic and boolean encoders. All other projections fail
            // closed above.
            local_key(place, body, &expr.ty)?;
        }
        ResolvedExprKind::Record { fields, rest, .. } => {
            for field in fields {
                collect_expr_i32_definedness_scoped(
                    &field.value,
                    body,
                    types,
                    vars,
                    obligations,
                    stationary_loops,
                    context,
                )?;
                if conversion_may_trap(field.conversion.kind) {
                    return None;
                }
            }
            if let Some(rest) = rest {
                collect_expr_i32_definedness_scoped(
                    rest,
                    body,
                    types,
                    vars,
                    obligations,
                    stationary_loops,
                    context,
                )?;
            }
        }
        ResolvedExprKind::Tuple(elements) => {
            for element in elements {
                collect_expr_i32_definedness_scoped(
                    element,
                    body,
                    types,
                    vars,
                    obligations,
                    stationary_loops,
                    context,
                )?;
            }
        }
        ResolvedExprKind::List(elements) | ResolvedExprKind::Set(elements) => {
            for element in elements {
                collect_expr_i32_definedness_scoped(
                    element,
                    body,
                    types,
                    vars,
                    obligations,
                    stationary_loops,
                    context,
                )?;
            }
        }
        ResolvedExprKind::Map(entries) => {
            for (key, value) in entries {
                collect_expr_i32_definedness_scoped(
                    key,
                    body,
                    types,
                    vars,
                    obligations,
                    stationary_loops,
                    context,
                )?;
                collect_expr_i32_definedness_scoped(
                    value,
                    body,
                    types,
                    vars,
                    obligations,
                    stationary_loops,
                    context,
                )?;
            }
        }
        ResolvedExprKind::Call(call)
            if matches!(&call.callee, ResolvedCallee::Constructor(_))
                && call.effects.is_empty()
                && call.session.is_empty() =>
        {
            for index in resolved_call_argument_evaluation_order(call)? {
                let argument = &call.arguments[index];
                collect_expr_i32_definedness_scoped(
                    &argument.value,
                    body,
                    types,
                    vars,
                    obligations,
                    stationary_loops,
                    context,
                )?;
                if conversion_may_trap(argument.conversion.kind) {
                    return None;
                }
            }
        }
        ResolvedExprKind::Match { scrutinee, arms } => {
            collect_expr_i32_definedness_scoped(
                scrutinee,
                body,
                types,
                vars,
                obligations,
                stationary_loops,
                context,
            )?;
            let callables = context.callables?;
            let variants = context.variants?;
            let tag_domain = crate::verifier::resolved_expr::resolved_variant_tag_domain_condition(
                scrutinee, body, types, callables, variants, vars,
            )?;
            context.type_premises.push(tag_domain);
            let mut remaining = Z3Bool::from_bool(true);
            for arm in arms {
                let mut arm_vars = vars.clone();
                let pattern_condition =
                    crate::verifier::resolved_expr::resolved_match_pattern_condition_with_flow_state_fields(
                        scrutinee,
                        &arm.pattern,
                        body,
                        types,
                        callables,
                        variants,
                        context.flow_state_fields,
                        &mut arm_vars,
                    )?;
                crate::verifier::resolved_expr::resolved_bind_match_pattern_with_flow_state_fields(
                    scrutinee,
                    &arm.pattern,
                    body,
                    types,
                    callables,
                    variants,
                    context.flow_state_fields,
                    &mut arm_vars,
                )?;

                let arm_entry = Z3Bool::and(&[&remaining, &pattern_condition]);
                let guard = if let Some(guard) = &arm.guard {
                    let mut guard_obligations = Vec::new();
                    collect_expr_i32_definedness_scoped(
                        guard,
                        body,
                        types,
                        &mut arm_vars,
                        &mut guard_obligations,
                        stationary_loops,
                        context,
                    )?;
                    for obligation in &mut guard_obligations {
                        obligation.condition = arm_entry.implies(&obligation.condition);
                    }
                    obligations.extend(guard_obligations);
                    resolved_to_z3_bool_in_context(guard, body, types, &mut arm_vars, context)?
                } else {
                    Z3Bool::from_bool(true)
                };
                let arm_path = Z3Bool::and(&[&arm_entry, &guard]);
                let mut arm_obligations = Vec::new();
                collect_expr_i32_definedness_scoped(
                    &arm.body,
                    body,
                    types,
                    &mut arm_vars,
                    &mut arm_obligations,
                    stationary_loops,
                    context,
                )?;
                for obligation in &mut arm_obligations {
                    obligation.condition = arm_path.implies(&obligation.condition);
                }
                obligations.extend(arm_obligations);

                let arm_taken = Z3Bool::and(&[&pattern_condition, &guard]);
                remaining = Z3Bool::and(&[&remaining, &arm_taken.not()]);
            }
            // A non-exhaustive match traps at runtime. Keep that fact as an
            // ordinary definedness VC; an unconstrained unmatched case must
            // never disappear merely because every encoded arm is safe.
            obligations.push(ResolvedIntDefinedness {
                condition: remaining.not(),
                failure: "match may be non-exhaustive",
                modeling_gap: false,
                is_call_precondition: false,
            });
        }
        ResolvedExprKind::Call(call) => {
            collect_resolved_call_definedness(
                call,
                body,
                types,
                vars,
                obligations,
                stationary_loops,
                context,
            )?;
        }
        ResolvedExprKind::Cast { value, conversion } => {
            collect_expr_i32_definedness_scoped(
                value,
                body,
                types,
                vars,
                obligations,
                stationary_loops,
                context,
            )?;
            if conversion_may_trap(conversion.kind) {
                return None;
            }
        }
        ResolvedExprKind::Try {
            value,
            propagation_target,
        } => {
            // Checker-lowered `?` propagates a failed Result out of a
            // `fails` Flow transition. That is a control transfer, not a
            // trap: inspect the operand's own evaluation obligations and
            // keep both tags available to the transition result encoder.
            // Outside this exact callable-local boundary, `?` remains
            // unsupported by the Resolved contract slice.
            if !body.owner.0.starts_with("transition:")
                || propagation_target != &body.owner
                || resolved_variant_member_type(
                    &value.ty,
                    &NodeId("builtin:variant:Result::Ok".into()),
                    &NodeId("builtin:variant:Result::Ok/payload:0".into()),
                    types,
                    context.variants?,
                )? != expr.ty
            {
                return None;
            }
            collect_expr_i32_definedness_scoped(
                value,
                body,
                types,
                vars,
                obligations,
                stationary_loops,
                context,
            )?;
            let tag_domain = crate::verifier::resolved_expr::resolved_variant_tag_domain_condition(
                value,
                body,
                types,
                context.callables?,
                context.variants?,
                vars,
            )?;
            context.type_premises.push(tag_domain);
        }
        ResolvedExprKind::Old(inner) | ResolvedExprKind::TypeOf(inner) => {
            collect_expr_i32_definedness_scoped(
                inner,
                body,
                types,
                vars,
                obligations,
                stationary_loops,
                context,
            )?;
        }
        ResolvedExprKind::Literal(_)
        | ResolvedExprKind::Constant(_)
        | ResolvedExprKind::ComptimeValue(_)
        | ResolvedExprKind::TypeValue(_) => {}
        _ => return None,
    }
    Some(())
}

fn root_contracts_for(
    callable: &crate::core::ResolvedCallable,
    kind: ContractKind,
) -> Vec<&ResolvedExpr> {
    let root_nodes = callable
        .body
        .root
        .statements
        .iter()
        .filter_map(|statement| {
            matches!(&statement.kind, ResolvedStmtKind::Contract { .. })
                .then_some(statement.node_id.clone())
        })
        .collect::<BTreeSet<_>>();
    callable
        .contracts
        .iter()
        .filter(|contract| contract.kind == kind && root_nodes.contains(&contract.node_id))
        .map(|contract| &contract.condition)
        .collect()
}

/// The typed call stores actuals in checker/formal-parameter order. That is
/// sufficient for parameter substitution, but not for definedness: source
/// arguments execute in lexical order. Recover that order from their checked
/// origins; if an actual lacks a unique source position, multi-argument calls
/// stay outside this proof subset.
fn resolved_call_argument_evaluation_order(
    call: &crate::core::ir::ResolvedCall,
) -> Option<Vec<usize>> {
    if call.arguments.len() < 2 {
        return Some((0..call.arguments.len()).collect());
    }
    let mut ordered = Vec::with_capacity(call.arguments.len());
    for (index, argument) in call.arguments.iter().enumerate() {
        let span = argument.value.origin.user_span();
        if !span.source_id.is_known() || span.start_line == 0 || span.start_col == 0 {
            return None;
        }
        ordered.push((
            span.source_id.raw(),
            span.start_line,
            span.start_col,
            span.end_line,
            span.end_col,
            index,
        ));
    }
    if ordered.iter().any(|left| {
        ordered.iter().any(|right| {
            left.5 != right.5 && left.0 == right.0 && left.1 == right.1 && left.2 == right.2
        })
    }) {
        return None;
    }
    ordered.sort_unstable();
    let source_id = ordered.first()?.0;
    if ordered.iter().any(|entry| entry.0 != source_id) {
        return None;
    }
    Some(ordered.into_iter().map(|entry| entry.5).collect())
}

/// Build a frame for a checker-identified failing Flow transition.  Admission
/// is intentionally narrow: the TransitionId must resolve to its canonical
/// callable; its full result must be `Result<Target, (Source, Error)>`; and every
/// actual must match its declared formal by exact typed identity.  No flow
/// precondition or transition effect is inferred from a name.
fn resolved_transition_call_frame(
    call: &crate::core::ir::ResolvedCall,
    caller_body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<crate::core::NodeId, crate::core::ResolvedCallable>,
    variants: &BTreeMap<crate::core::NodeId, ResolvedVariantSchema>,
    caller_vars: &mut Z3VarMap,
) -> Option<(crate::core::ResolvedCallable, Z3VarMap)> {
    use crate::core::ir::{Permission, ResolvedCallee, ResolvedType};

    let ResolvedCallee::Transition(id) = &call.callee else {
        return None;
    };
    let target = crate::core::mir::transition_owner_from_id(id);
    let Some(callee) = callables.get(&target).cloned() else {
        return None;
    };
    if callee.owner != target
        || callee.signature.owner != target
        || callee.body.owner != target
        || call.permission != Some(Permission::Consume)
        || call.effects != callee.signature.effects
        || !callee.signature.effects.is_empty()
        || !call.session.is_empty()
        || !call.type_arguments.is_empty()
        || !callee.signature.generic_parameters.is_empty()
        || callee.signature.parameters.is_empty()
        || call.arguments.len() != callee.signature.parameters.len()
        || callee.body.parameters.len() != callee.signature.parameters.len()
        || !callee.body.captures.is_empty()
        || !callee.body.default_values.is_empty()
        || callee.has_unmodeled_proof_roots()
    {
        return None;
    }
    let (ok_type, error_type) = resolved_result_type_payloads(&call.result, types)?;
    if call.result != callee.signature.result {
        return None;
    }
    let (_, _, return_value) = resolved_transition_body_tail_and_try(&callee, types)?;
    if ok_type != &return_value.ty {
        return None;
    }
    let error_items = match types.get(error_type)? {
        ResolvedType::Tuple(items) if items.len() == 2 => items,
        _ => return None,
    };
    let source_type = &callee.signature.parameters.first()?.ty;
    if &error_items[0] != source_type {
        return None;
    }

    let mut frame = Z3VarMap::new();
    let mut seen = BTreeSet::new();
    for (index, (formal, local_id)) in callee
        .signature
        .parameters
        .iter()
        .zip(&callee.body.parameters)
        .enumerate()
    {
        let expected_permission = (index == 0).then_some(Permission::Consume);
        if formal.has_default || formal.permission != expected_permission {
            return None;
        }
        let local = callee.body.locals.get(local_id)?;
        if local.ty != formal.ty {
            return None;
        }
        let argument = call
            .arguments
            .iter()
            .find(|argument| argument.parameter == formal.id)?;
        if !seen.insert(argument.parameter.clone())
            || argument.conversion.kind != CheckedConversionKind::Identity
            || argument.conversion.from != argument.value.ty
            || argument.conversion.to != formal.ty
            || argument.value.ty != formal.ty
            || !argument.value.effects.is_empty()
        {
            return None;
        }
        match resolved_scalar_category(&formal.ty, types) {
            Some(Z3TypeCategory::Int) => {
                let value = resolved_to_z3_int_with_catalog(
                    &argument.value,
                    caller_body,
                    types,
                    callables,
                    variants,
                    caller_vars,
                )?;
                let key = local_var_key(&local.id, &local.display_name);
                frame.insert_int(key.clone(), value.clone());
                frame.insert_int(format!("old.{key}"), value);
            }
            Some(Z3TypeCategory::Real) => {
                let value = resolved_to_z3_real_with_catalog(
                    &argument.value,
                    caller_body,
                    types,
                    callables,
                    variants,
                    caller_vars,
                )?;
                let key = local_var_key(&local.id, &local.display_name);
                frame.insert_real(key.clone(), value.clone());
                frame.insert_real(format!("old.{key}"), value);
            }
            Some(Z3TypeCategory::Bool) => {
                let value = resolved_to_z3_bool_with_catalog(
                    &argument.value,
                    caller_body,
                    types,
                    callables,
                    variants,
                    caller_vars,
                )?;
                let key = local_var_key(&local.id, &local.display_name);
                frame.insert_bool(key.clone(), value.clone());
                frame.insert_bool(format!("old.{key}"), value);
            }
            None => {
                bind_value_to_local(
                    &argument.value,
                    caller_body,
                    local,
                    types,
                    callables,
                    variants,
                    &mut frame,
                    &mut BTreeSet::new(),
                )?;
            }
        }
    }
    if seen.len() != call.arguments.len() {
        return None;
    }
    Some((callee, frame))
}

fn resolved_result_type_payloads<'a>(
    ty: &'a crate::core::ir::ResolvedTypeId,
    types: &'a ResolvedTypeTable,
) -> Option<(
    &'a crate::core::ir::ResolvedTypeId,
    &'a crate::core::ir::ResolvedTypeId,
)> {
    match types.get(ty)? {
        crate::core::ir::ResolvedType::Result { ok, error } => Some((ok, error)),
        crate::core::ir::ResolvedType::Nominal {
            item, arguments, ..
        } if item.as_str() == "builtin:type:Result" && arguments.len() == 2 => {
            Some((&arguments[0], &arguments[1]))
        }
        _ => None,
    }
}

fn resolved_transition_body_tail_and_try<'a>(
    callable: &'a crate::core::ResolvedCallable,
    types: &ResolvedTypeTable,
) -> Option<(&'a ResolvedExpr, &'a ResolvedExpr, &'a ResolvedExpr)> {
    fn block_contains_try(block: &ResolvedBlock) -> bool {
        for statement in &block.statements {
            let contains = match &statement.kind {
                ResolvedStmtKind::Bind { initializer, .. } => {
                    initializer.as_ref().is_some_and(expr_contains_try)
                }
                ResolvedStmtKind::Assign { value, .. } | ResolvedStmtKind::Expr(value) => {
                    expr_contains_try(value)
                }
                ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                    value.as_ref().is_some_and(expr_contains_try)
                }
                ResolvedStmtKind::While { condition, body } => {
                    expr_contains_try(condition) || block_contains_try(body)
                }
                ResolvedStmtKind::WhileLet {
                    initializer, body, ..
                } => expr_contains_try(initializer) || block_contains_try(body),
                ResolvedStmtKind::IfLet {
                    initializer,
                    then_block,
                    else_block,
                    ..
                } => {
                    expr_contains_try(initializer)
                        || block_contains_try(then_block)
                        || else_block.as_ref().is_some_and(block_contains_try)
                }
                ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                    block_contains_try(body)
                }
                ResolvedStmtKind::For { iterable, body, .. } => {
                    expr_contains_try(iterable) || block_contains_try(body)
                }
                ResolvedStmtKind::Contract { condition, .. } => expr_contains_try(condition),
                ResolvedStmtKind::Math(expressions) => expressions.iter().any(expr_contains_try),
                ResolvedStmtKind::Pinned { value, body, .. } => {
                    expr_contains_try(value) || block_contains_try(body)
                }
                ResolvedStmtKind::Continue | ResolvedStmtKind::Drop(_) => false,
                // A nested callable is not evaluated at this transition
                // point, but conservatively keep it outside this restricted
                // source-tail shape if it appears in an expression block.
                ResolvedStmtKind::NestedCallable(_) => true,
            };
            if contains {
                return true;
            }
        }
        block.result.as_deref().is_some_and(expr_contains_try)
    }

    fn expr_contains_try(expression: &ResolvedExpr) -> bool {
        match &expression.kind {
            ResolvedExprKind::Try { .. } => true,
            ResolvedExprKind::FString(parts) => parts.iter().any(|part| {
                matches!(part, crate::core::ir::ResolvedFStringPart::Interpolation(value)
                    if expr_contains_try(value))
            }),
            ResolvedExprKind::Project { value, projection } => {
                expr_contains_try(value)
                    || matches!(projection, ResolvedValueProjection::Index(index)
                        if expr_contains_try(index))
            }
            ResolvedExprKind::Binary { left, right, .. }
            | ResolvedExprKind::Range {
                start: left,
                end: right,
            } => expr_contains_try(left) || expr_contains_try(right),
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::TypeOf(operand)
            | ResolvedExprKind::Old(operand)
            | ResolvedExprKind::Spawn(operand)
            | ResolvedExprKind::Await(operand)
            | ResolvedExprKind::Cast { value: operand, .. } => expr_contains_try(operand),
            ResolvedExprKind::Call(call) => call
                .arguments
                .iter()
                .any(|argument| expr_contains_try(&argument.value)),
            ResolvedExprKind::Tuple(values)
            | ResolvedExprKind::List(values)
            | ResolvedExprKind::Set(values) => values.iter().any(expr_contains_try),
            ResolvedExprKind::Map(entries) => entries
                .iter()
                .any(|(key, value)| expr_contains_try(key) || expr_contains_try(value)),
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                expr_contains_try(value)
                    || expr_contains_try(iterable)
                    || guard.as_deref().is_some_and(expr_contains_try)
            }
            ResolvedExprKind::OptionalChain { receiver, .. } => expr_contains_try(receiver),
            ResolvedExprKind::Record { fields, rest, .. } => {
                fields.iter().any(|field| expr_contains_try(&field.value))
                    || rest.as_deref().is_some_and(expr_contains_try)
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Scope { body: block, .. }
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => block_contains_try(block),
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                expr_contains_try(condition)
                    || block_contains_try(then_block)
                    || block_contains_try(else_block)
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                expr_contains_try(scrutinee)
                    || arms.iter().any(|arm| {
                        arm.guard.as_ref().is_some_and(expr_contains_try)
                            || expr_contains_try(&arm.body)
                    })
            }
            ResolvedExprKind::Slice { target, start, end } => {
                expr_contains_try(target)
                    || start.as_deref().is_some_and(expr_contains_try)
                    || end.as_deref().is_some_and(expr_contains_try)
            }
            // Closures and first-class callable values can defer evaluation
            // past this transition point. Keep them outside this narrow
            // result-propagation model.
            ResolvedExprKind::Lambda(_)
            | ResolvedExprKind::Callable(_)
            | ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Load(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_) => false,
        }
    }

    let statements = &callable.body.root.statements;
    let (result_ok_type, _) = resolved_result_type_payloads(&callable.signature.result, types)?;
    if callable.body.root.result.is_some() || statements.is_empty() {
        return None;
    }
    let mut try_expression = None;
    let mut try_value = None;
    let mut return_value = None;
    for (index, statement) in statements.iter().enumerate() {
        match &statement.kind {
            ResolvedStmtKind::Bind {
                pattern,
                initializer: Some(initializer),
            } if matches!(
                &pattern.kind,
                ResolvedPatternKind::Binding {
                    by_reference: None,
                    ..
                }
            ) =>
            {
                if let ResolvedExprKind::Try {
                    value,
                    propagation_target,
                } = &initializer.kind
                {
                    if propagation_target != &callable.owner || try_value.is_some() {
                        return None;
                    }
                    let ResolvedPatternKind::Binding {
                        local,
                        by_reference: None,
                    } = &pattern.kind
                    else {
                        return None;
                    };
                    let local = callable.body.locals.get(local)?;
                    if initializer.ty != local.ty || pattern.ty != local.ty {
                        return None;
                    }
                    if expr_contains_try(value) {
                        return None;
                    }
                    try_expression = Some(initializer);
                    try_value = Some(value.as_ref());
                } else {
                    let ResolvedPatternKind::Binding {
                        local,
                        by_reference: None,
                    } = &pattern.kind
                    else {
                        return None;
                    };
                    let local = callable.body.locals.get(local)?;
                    if initializer.ty != local.ty
                        || pattern.ty != local.ty
                        || !initializer.effects.is_empty()
                        || expr_contains_try(initializer)
                    {
                        return None;
                    }
                }
            }
            ResolvedStmtKind::Return {
                value: Some(value),
                conversion: Some(conversion),
            } if index + 1 == statements.len() => {
                if conversion.kind != CheckedConversionKind::Identity
                    || conversion.from != value.ty
                    || conversion.to != value.ty
                    || &value.ty != result_ok_type
                    || !value.effects.is_empty()
                {
                    return None;
                }
                return_value = Some(value);
            }
            _ => return None,
        }
    }
    let try_expression = try_expression?;
    let try_value = try_value?;
    let return_value = return_value?;
    if expr_contains_try(return_value) {
        // The one top-level Try is the transition's propagation point. A
        // second or nested Try needs a separate path-residual model.
        return None;
    }
    let try_count = statements
        .iter()
        .filter(|statement| {
            matches!(
                &statement.kind,
                ResolvedStmtKind::Bind {
                    initializer: Some(ResolvedExpr {
                        kind: ResolvedExprKind::Try { .. },
                        ..
                    }),
                    ..
                }
            )
        })
        .count();
    if try_count != 1 {
        return None;
    }
    // The other accepted statement forms are only binding expressions. A
    // nested propagation point in any such binding needs its own residual
    // path and is not covered by this one-Try model.
    for statement in statements {
        if let ResolvedStmtKind::Bind {
            initializer: Some(initializer),
            ..
        } = &statement.kind
        {
            if !std::ptr::eq(initializer, try_expression) && expr_contains_try(initializer) {
                return None;
            }
        }
    }
    Some((try_expression, try_value, return_value))
}

fn resolved_transition_result_variants(
    call: &crate::core::ir::ResolvedCall,
    callable: &crate::core::ResolvedCallable,
    types: &ResolvedTypeTable,
    variants: &BTreeMap<crate::core::NodeId, ResolvedVariantSchema>,
) -> Option<(crate::core::NodeId, crate::core::NodeId)> {
    use crate::core::ir::ResolvedType;

    if !matches!(&call.callee, crate::core::ir::ResolvedCallee::Transition(_)) {
        return None;
    }
    // A failing transition's callable signature and typed call expression
    // both carry Result<Target, (Source, Error)>; the source-level Return
    // value supplies only the Result::Ok payload type.
    let (call_ok, call_error) = resolved_result_type_payloads(&call.result, types)?;
    let (inner_try, inner_error, return_value) =
        resolved_transition_body_tail_and_try(callable, types)?;
    let (inner_ok, actual_inner_error) = resolved_result_type_payloads(&inner_error.ty, types)?;
    let error_members = match types.get(call_error)? {
        ResolvedType::Tuple(members) if members.len() == 2 => members,
        _ => return None,
    };
    if call.result != callable.signature.result
        || call_ok != &return_value.ty
        || inner_ok != &inner_try.ty
        || actual_inner_error != &error_members[1]
        || error_members[0] != callable.signature.parameters.first()?.ty
    {
        return None;
    }
    let ok = crate::core::NodeId("builtin:variant:Result::Ok".into());
    let err = crate::core::NodeId("builtin:variant:Result::Err".into());
    resolved_variant_tag_number(&call.result, &ok, types, variants)?;
    resolved_variant_tag_number(&call.result, &err, types, variants)?;
    Some((ok, err))
}

fn resolved_transition_evaluation(
    call: &crate::core::ir::ResolvedCall,
    caller_body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<crate::core::NodeId, crate::core::ResolvedCallable>,
    variants: &BTreeMap<crate::core::NodeId, ResolvedVariantSchema>,
    caller_vars: &mut Z3VarMap,
) -> Option<(
    crate::core::ResolvedCallable,
    Z3VarMap,
    ResolvedExpr,
    ResolvedExpr,
    Z3Bool,
)> {
    use crate::core::ir::ResolvedCallee;

    let ResolvedCallee::Transition(_) = &call.callee else {
        return None;
    };
    let (callable, mut frame) =
        resolved_transition_call_frame(call, caller_body, types, callables, variants, caller_vars)?;
    if callable
        .body
        .resolved_calls
        .values()
        .any(|callee| matches!(callee, ResolvedCallee::Transition(_)))
    {
        return None;
    }
    let (try_expr, try_value, return_value) =
        resolved_transition_body_tail_and_try(&callable, types)?;
    let _ = resolved_transition_result_variants(call, &callable, types, variants)?;
    if try_expr.effects != try_value.effects || !try_expr.effects.is_empty() {
        return None;
    }
    // The terminal source-level Return is encoded separately as the Ok
    // payload of the transition result. Bind only the checked prefix here;
    // the ordinary block-tail binder correctly rejects explicit Return
    // statements because it is intended for fallthrough blocks.
    let mut prefix = callable.body.root.clone();
    let Some(last) = prefix.statements.pop() else {
        return None;
    };
    if !matches!(last.kind, ResolvedStmtKind::Return { .. }) {
        return None;
    }
    prefix.result = None;
    resolved_block_stmts_with_catalog(
        &prefix,
        &callable.body,
        types,
        callables,
        variants,
        &mut frame,
    )?;
    let success = resolved_variant_tag_condition(
        try_value,
        &crate::core::NodeId("builtin:variant:Result::Ok".into()),
        &callable.body,
        types,
        callables,
        variants,
        &mut frame,
    )?;
    let try_value = try_value.clone();
    let return_value = return_value.clone();
    Some((callable, frame, try_value, return_value, success))
}

fn resolved_transition_expression_type_at_path(
    call: &crate::core::ir::ResolvedCall,
    path: &[ResolvedValuePathPart],
    caller_body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<crate::core::NodeId, crate::core::ResolvedCallable>,
    variants: &BTreeMap<crate::core::NodeId, ResolvedVariantSchema>,
) -> Option<crate::core::ir::ResolvedTypeId> {
    use crate::core::ir::{ResolvedCallee, ResolvedType};

    let ResolvedCallee::Transition(id) = &call.callee else {
        return None;
    };
    let callee = callables.get(&crate::core::mir::transition_owner_from_id(id))?;
    let (_try_expr, try_value, return_value) =
        resolved_transition_body_tail_and_try(callee, types)?;
    let (ok_variant, err_variant) =
        resolved_transition_result_variants(call, callee, types, variants)?;
    let [ResolvedValuePathPart::VariantPayload { variant, member }, rest @ ..] = path else {
        return None;
    };
    let member_type = resolved_variant_member_type(&call.result, variant, member, types, variants)?;
    if variant == &ok_variant
        && member == &NodeId("builtin:variant:Result::Ok/payload:0".into())
        && member_type == return_value.ty
    {
        if return_value.ty != member_type {
            return None;
        }
        return resolved_expression_type_at_path(
            return_value,
            rest,
            &callee.body,
            types,
            callables,
            variants,
        );
    }
    if variant != &err_variant || member != &NodeId("builtin:variant:Result::Err/payload:0".into())
    {
        return None;
    }
    let error_members = match types.get(&member_type)? {
        ResolvedType::Tuple(members) if members.len() == 2 => members,
        _ => return None,
    };
    if rest.is_empty() {
        return Some(member_type);
    }
    let [ResolvedValuePathPart::Tuple(index), member_path @ ..] = rest else {
        return None;
    };
    match index {
        0 => {
            let source_formal = callee.signature.parameters.first()?;
            if error_members[0] != source_formal.ty {
                return None;
            }
            let actual = call
                .arguments
                .iter()
                .find(|argument| argument.parameter == source_formal.id)?;
            if actual.value.ty != source_formal.ty {
                return None;
            }
            resolved_expression_type_at_path(
                &actual.value,
                member_path,
                caller_body,
                types,
                callables,
                variants,
            )
        }
        1 => {
            let error_payload = NodeId("builtin:variant:Result::Err/payload:0".into());
            if try_expr_error_type(callee, types)? != error_members[1] {
                return None;
            }
            let mut inner_path = vec![ResolvedValuePathPart::VariantPayload {
                variant: err_variant,
                member: error_payload,
            }];
            inner_path.extend_from_slice(member_path);
            resolved_expression_type_at_path(
                try_value,
                &inner_path,
                &callee.body,
                types,
                callables,
                variants,
            )
        }
        _ => None,
    }
}

fn bind_transition_value_at_path(
    call: &crate::core::ir::ResolvedCall,
    source_path: &[ResolvedValuePathPart],
    target_type: &crate::core::ir::ResolvedTypeId,
    target_key: &str,
    caller_body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<crate::core::NodeId, crate::core::ResolvedCallable>,
    variants: &BTreeMap<crate::core::NodeId, ResolvedVariantSchema>,
    caller_vars: &mut Z3VarMap,
    active: &mut BTreeSet<NodeId>,
) -> Option<()> {
    use crate::core::ir::{ResolvedCallee, ResolvedType};

    let ResolvedCallee::Transition(id) = &call.callee else {
        return None;
    };
    let callee = callables.get(&crate::core::mir::transition_owner_from_id(id))?;
    let (ok_variant, err_variant) =
        resolved_transition_result_variants(call, callee, types, variants)?;
    let [ResolvedValuePathPart::VariantPayload { variant, member }, rest @ ..] = source_path else {
        return None;
    };
    let member_type = resolved_variant_member_type(&call.result, variant, member, types, variants)?;
    if variant == &ok_variant && member == &NodeId("builtin:variant:Result::Ok/payload:0".into()) {
        let (callee, mut frame, _, return_value, _) = resolved_transition_evaluation(
            call,
            caller_body,
            types,
            callables,
            variants,
            caller_vars,
        )?;
        if member_type != return_value.ty {
            return None;
        }
        bind_value_at_path(
            &return_value,
            rest,
            target_type,
            target_key,
            &callee.body,
            types,
            callables,
            variants,
            &mut frame,
            active,
        )?;
        copy_known_value_path_facts_between(&frame, target_key, target_key, caller_vars);
        return Some(());
    }
    if variant != &err_variant || member != &NodeId("builtin:variant:Result::Err/payload:0".into())
    {
        return None;
    }
    let error_members = match types.get(&member_type)? {
        ResolvedType::Tuple(members) if members.len() == 2 => members,
        _ => return None,
    };
    let (callee, mut frame, try_value, _, _) =
        resolved_transition_evaluation(call, caller_body, types, callables, variants, caller_vars)?;
    if rest.is_empty() {
        let ResolvedType::Tuple(target_members) = types.get(target_type)? else {
            return None;
        };
        if target_type != &member_type || target_members.len() != 2 {
            return None;
        }
        for (index, item_type) in target_members.iter().enumerate() {
            let child_key = append_value_path(target_key, &[ResolvedValuePathPart::Tuple(index)]);
            let (source, path, body, source_vars) = if index == 0 {
                let source_formal = callee.signature.parameters.first()?;
                let actual = call
                    .arguments
                    .iter()
                    .find(|argument| argument.parameter == source_formal.id)?;
                if item_type != &source_formal.ty || &actual.value.ty != item_type {
                    return None;
                }
                (&actual.value, Vec::new(), caller_body, &mut *caller_vars)
            } else {
                if item_type != &error_members[1] {
                    return None;
                }
                let path = vec![ResolvedValuePathPart::VariantPayload {
                    variant: err_variant.clone(),
                    member: NodeId("builtin:variant:Result::Err/payload:0".into()),
                }];
                (&try_value, path, &callee.body, &mut frame)
            };
            bind_value_at_path(
                source,
                &path,
                item_type,
                &child_key,
                body,
                types,
                callables,
                variants,
                source_vars,
                active,
            )?;
        }
        copy_known_value_path_facts_between(&frame, target_key, target_key, caller_vars);
        return Some(());
    }
    let [ResolvedValuePathPart::Tuple(index), member_path @ ..] = rest else {
        return None;
    };
    let (source, mut path, body, source_vars) = match index {
        0 => {
            let source_formal = callee.signature.parameters.first()?;
            let actual = call
                .arguments
                .iter()
                .find(|argument| argument.parameter == source_formal.id)?;
            if error_members[0] != source_formal.ty || actual.value.ty != source_formal.ty {
                return None;
            }
            (&actual.value, Vec::new(), caller_body, &mut *caller_vars)
        }
        1 => {
            if error_members[1] != try_expr_error_type(&callee, types)? {
                return None;
            }
            (
                &try_value,
                vec![ResolvedValuePathPart::VariantPayload {
                    variant: err_variant,
                    member: NodeId("builtin:variant:Result::Err/payload:0".into()),
                }],
                &callee.body,
                &mut frame,
            )
        }
        _ => return None,
    };
    path.extend_from_slice(member_path);
    if resolved_expression_type_at_path(source, &path, body, types, callables, variants)?
        != *target_type
    {
        return None;
    }
    bind_value_at_path(
        source,
        &path,
        target_type,
        target_key,
        body,
        types,
        callables,
        variants,
        source_vars,
        active,
    )?;
    if *index == 1 {
        copy_known_value_path_facts_between(&frame, target_key, target_key, caller_vars);
    }
    Some(())
}

fn try_expr_error_type(
    callable: &crate::core::ResolvedCallable,
    types: &ResolvedTypeTable,
) -> Option<crate::core::ir::ResolvedTypeId> {
    let (_, try_value, _) = resolved_transition_body_tail_and_try(callable, types)?;
    match types.get(&try_value.ty)? {
        ResolvedType::Result { error, .. } => Some(error.clone()),
        ResolvedType::Nominal {
            item, arguments, ..
        } if item.as_str() == "builtin:type:Result" && arguments.len() == 2 => {
            Some(arguments[1].clone())
        }
        _ => None,
    }
}

fn collect_resolved_call_definedness(
    call: &crate::core::ir::ResolvedCall,
    caller_body: &ResolvedBody,
    types: &ResolvedTypeTable,
    caller_vars: &mut Z3VarMap,
    obligations: &mut Vec<ResolvedIntDefinedness>,
    stationary_loops: &BTreeSet<crate::core::NodeId>,
    context: &mut ResolvedDefinednessContext<'_>,
) -> Option<()> {
    use crate::core::ir::ResolvedCallee;

    if (matches!(&call.callee, ResolvedCallee::Constructor(_))
        || matches!(&call.callee, ResolvedCallee::Builtin(builtin)
            if matches!(builtin.as_str(), "Some" | "None" | "Ok" | "Err")))
        && call.effects.is_empty()
        && call.session.is_empty()
    {
        if matches!(&call.callee, ResolvedCallee::Builtin(_)) {
            let _ = resolved_builtin_sum_constructor_variant(call, types, context.variants?)?;
        }
        for index in resolved_call_argument_evaluation_order(call)? {
            let argument = &call.arguments[index];
            collect_expr_i32_definedness_scoped(
                &argument.value,
                caller_body,
                types,
                caller_vars,
                obligations,
                stationary_loops,
                context,
            )?;
            if conversion_may_trap(argument.conversion.kind) {
                return None;
            }
        }
        return Some(());
    }

    if let ResolvedCallee::Builtin(builtin) = &call.callee {
        if !call.effects.is_empty() || !call.session.is_empty() {
            return None;
        }
        let name = builtin.as_str();
        let unwrap_variant = match name {
            "builtin.method.option.unwrap" => Some("builtin:variant:Option::Some"),
            "builtin.method.result.unwrap" => Some("builtin:variant:Result::Ok"),
            "builtin.method.result.unwrap_err" => Some("builtin:variant:Result::Err"),
            _ => None,
        };
        let is_total_predicate = matches!(
            name,
            "builtin.method.option.is_some"
                | "builtin.method.option.is_none"
                | "builtin.method.result.is_ok"
                | "builtin.method.result.is_err"
        );
        let is_fallback_unwrap = matches!(
            name,
            "builtin.method.option.unwrap_or" | "builtin.method.result.unwrap_or"
        );
        let is_list_len = matches!(name, "len" | "builtin.method.list.len");
        if is_list_len {
            let Some(argument) = call.arguments.first() else {
                return None;
            };
            let expected_permission = if name == "len" {
                None
            } else {
                Some(crate::core::ir::Permission::View)
            };
            if call.arguments.len() != 1
                || !call.type_arguments.is_empty()
                || call.permission != expected_permission
                || !matches!(types.get(&argument.value.ty), Some(ResolvedType::Nominal {
                    item, arguments, ..
                }) if item.as_str() == "builtin:type:List" && arguments.len() == 1)
                || argument.conversion.kind != CheckedConversionKind::Identity
                || argument.conversion.from != argument.value.ty
                || argument.conversion.to != argument.value.ty
                || !argument.value.effects.is_empty()
                || !matches!(
                    types.get(&call.result),
                    Some(ResolvedType::Primitive(
                        PrimitiveType::I32 | PrimitiveType::I64
                    ))
                )
            {
                return None;
            }
            collect_expr_i32_definedness_scoped(
                &argument.value,
                caller_body,
                types,
                caller_vars,
                obligations,
                stationary_loops,
                context,
            )?;
            return Some(());
        }
        let set_op_shape = match name {
            "builtin.method.set.size"
            | "builtin.method.set.len"
            | "builtin.method.set.is_empty"
            | "builtin.method.set.to_list" => Some((1, Some(crate::core::ir::Permission::View))),
            "builtin.method.set.contains"
            | "builtin.method.set.insert"
            | "builtin.method.set.remove" => Some((2, Some(crate::core::ir::Permission::View))),
            "contains"
                if call.arguments.first().is_some_and(|argument| {
                    resolved_set_value_supported(&argument.value.ty, types).is_some()
                }) =>
            {
                Some((2, None))
            }
            _ => None,
        };
        if let Some((expected_arity, expected_permission)) = set_op_shape {
            if !resolved_set_builtin_call_is_closed(call, expected_arity, expected_permission)
                || resolved_set_value_supported(&call.arguments[0].value.ty, types).is_none()
            {
                return None;
            }
            let result_ok = match name {
                "builtin.method.set.size" | "builtin.method.set.len" => matches!(
                    types.get(&call.result),
                    Some(ResolvedType::Primitive(
                        PrimitiveType::I32 | PrimitiveType::I64
                    ))
                ),
                "builtin.method.set.is_empty" | "builtin.method.set.contains" | "contains" => {
                    matches!(
                        types.get(&call.result),
                        Some(ResolvedType::Primitive(PrimitiveType::Bool))
                    )
                }
                "builtin.method.set.to_list" => resolved_list_value_supported(&call.result, types),
                "builtin.method.set.insert" | "builtin.method.set.remove" => {
                    call.result == call.arguments[0].value.ty
                }
                _ => false,
            };
            if !result_ok {
                return None;
            }
            for argument in &call.arguments {
                collect_expr_i32_definedness_scoped(
                    &argument.value,
                    caller_body,
                    types,
                    caller_vars,
                    obligations,
                    stationary_loops,
                    context,
                )?;
            }
            return Some(());
        }
        if unwrap_variant.is_some() || is_total_predicate || is_fallback_unwrap {
            let expected_permission = if is_total_predicate {
                crate::core::ir::Permission::View
            } else {
                crate::core::ir::Permission::Consume
            };
            if call.permission != Some(expected_permission)
                || !call.type_arguments.is_empty()
                || call.arguments.is_empty()
                || (!is_fallback_unwrap && call.arguments.len() != 1)
                || (is_fallback_unwrap && call.arguments.len() != 2)
            {
                return None;
            }
            if unwrap_variant.is_some() || is_fallback_unwrap {
                let projection =
                    resolved_builtin_sum_projection(call, &call.result, types, context.variants?)?;
                if unwrap_variant.is_some_and(|expected| projection.variant.0 != expected) {
                    return None;
                }
            }
            for index in resolved_call_argument_evaluation_order(call)? {
                let argument = &call.arguments[index];
                collect_expr_i32_definedness_scoped(
                    &argument.value,
                    caller_body,
                    types,
                    caller_vars,
                    obligations,
                    stationary_loops,
                    context,
                )?;
                if conversion_may_trap(argument.conversion.kind)
                    || !argument.value.effects.is_empty()
                {
                    return None;
                }
            }
            if let Some(variant_name) = unwrap_variant {
                let receiver = &call.arguments[0].value;
                let expected = crate::core::NodeId(variant_name.to_string());
                let tag = crate::verifier::resolved_expr::resolved_variant_tag_condition(
                    receiver,
                    &expected,
                    caller_body,
                    types,
                    context.callables?,
                    context.variants?,
                    caller_vars,
                )?;
                obligations.push(ResolvedIntDefinedness {
                    condition: tag,
                    failure: "variant unwrap may select a missing payload",
                    modeling_gap: false,
                    is_call_precondition: false,
                });
            }
            return Some(());
        }
        return None;
    }

    let (target, is_transition) = match &call.callee {
        ResolvedCallee::Function(target) => (target.clone(), false),
        ResolvedCallee::Transition(transition) => {
            (crate::core::mir::transition_owner_from_id(transition), true)
        }
        // Extern, closure, protocol/actor, and session calls need their own
        // proof contract and effect model.
        _ => return None,
    };
    if context.active_calls.contains(&target) {
        return None;
    }
    // Arguments are evaluated in the caller before the target begins. Keep
    // their VCs in source semantics and then build the target frame by stable
    // formal parameter identity; frame construction itself accepts only the
    // value-preserving scalar subset or nullary aggregate-producing helpers.
    for index in resolved_call_argument_evaluation_order(call)? {
        let argument = &call.arguments[index];
        if !argument.value.effects.is_empty() {
            return None;
        }
        collect_expr_i32_definedness_scoped(
            &argument.value,
            caller_body,
            types,
            caller_vars,
            obligations,
            stationary_loops,
            context,
        )?;
    }
    let (callee, mut callee_vars) = if is_transition {
        let transition_frame = resolved_transition_call_frame(
            call,
            caller_body,
            types,
            context.callables?,
            context.variants?,
            caller_vars,
        );
        let Some((callee, frame)) = transition_frame else {
            return None;
        };
        // This first Flow slice models only checker-lowered recoverable
        // transitions with one Result `?` and a terminal target-state
        // return. Other Flow effects/controls remain outside the subset.
        let Some((try_expression, try_value, _)) =
            resolved_transition_body_tail_and_try(&callee, types)
        else {
            return None;
        };
        if try_expression.effects != try_value.effects || !try_expression.effects.is_empty() {
            return None;
        }
        if resolved_transition_result_variants(call, &callee, types, context.variants?).is_none() {
            return None;
        }
        if callee
            .body
            .resolved_calls
            .values()
            .any(|callee| matches!(callee, ResolvedCallee::Transition(_)))
        {
            return None;
        }
        (callee, frame)
    } else {
        resolved_inline_scalar_call_frame(
            call,
            caller_body,
            types,
            context.callables?,
            context.variants?,
            caller_vars,
        )?
    };

    context.active_calls.insert(target.clone());
    let result = (|| {
        let mut prior_requires = Vec::new();
        for requires in root_contracts_for(&callee, ContractKind::Requires) {
            let mut local_obligations = Vec::new();
            collect_expr_i32_definedness_scoped(
                requires,
                &callee.body,
                types,
                &mut callee_vars,
                &mut local_obligations,
                stationary_loops,
                context,
            )?;
            if !prior_requires.is_empty() {
                let refs = prior_requires.iter().collect::<Vec<_>>();
                let prior = Z3Bool::and(&refs);
                for obligation in &mut local_obligations {
                    obligation.condition = prior.implies(&obligation.condition);
                }
            }
            obligations.extend(local_obligations);
            let predicate = resolved_to_z3_bool_with_catalog(
                requires,
                &callee.body,
                types,
                context.callables?,
                context.variants?,
                &mut callee_vars,
            )?;
            obligations.push(ResolvedIntDefinedness {
                condition: predicate.clone(),
                failure: "call may violate precondition",
                modeling_gap: false,
                is_call_precondition: true,
            });
            prior_requires.push(predicate);
        }

        let mut body_obligations = Vec::new();
        collect_block_i32_definedness_scoped(
            &callee.body.root,
            &callee.body,
            types,
            &mut callee_vars,
            &mut body_obligations,
            stationary_loops,
            context,
        )?;
        if !prior_requires.is_empty() {
            let refs = prior_requires.iter().collect::<Vec<_>>();
            let applicable = Z3Bool::and(&refs);
            for obligation in &mut body_obligations {
                obligation.condition = applicable.implies(&obligation.condition);
            }
        }
        obligations.extend(body_obligations);
        Some(())
    })();
    context.active_calls.remove(&target);
    result
}

fn resolved_scalar_category(
    ty: &crate::core::ir::ResolvedTypeId,
    types: &ResolvedTypeTable,
) -> Option<Z3TypeCategory> {
    match types.get(ty)? {
        ResolvedType::Primitive(PrimitiveType::I32 | PrimitiveType::I64) => {
            Some(Z3TypeCategory::Int)
        }
        ResolvedType::Primitive(PrimitiveType::F64) => Some(Z3TypeCategory::Real),
        ResolvedType::Primitive(PrimitiveType::Bool) => Some(Z3TypeCategory::Bool),
        _ => None,
    }
}

fn resolved_call_graph_reaches(
    owner: &crate::core::NodeId,
    target: &crate::core::NodeId,
    callables: &BTreeMap<crate::core::NodeId, crate::core::ResolvedCallable>,
    seen: &mut BTreeSet<crate::core::NodeId>,
) -> bool {
    if owner == target {
        return true;
    }
    if !seen.insert(owner.clone()) {
        return false;
    }
    callables.get(owner).is_some_and(|callable| {
        callable.body.resolved_calls.values().any(|callee| {
            matches!(callee, crate::core::ir::ResolvedCallee::Function(next)
                if resolved_call_graph_reaches(next, target, callables, seen))
        })
    })
}

/// Return the instantiated Option/Result type for the one generic shape the
/// scalar-call encoder admits: generic payload parameters directly stored in
/// the builtin sum. This is enough to bind `Option<T>` / `Result<T, E>` to a
/// helper frame without specializing arbitrary expression trees.
fn resolved_generic_sum_parameter_type(
    ty: &crate::core::ir::ResolvedTypeId,
    generic_parameters: &[crate::core::NodeId],
    type_arguments: &[crate::core::ir::ResolvedTypeId],
    types: &ResolvedTypeTable,
) -> Option<crate::core::ir::ResolvedTypeId> {
    use crate::core::ir::ResolvedType;

    if generic_parameters.len() != type_arguments.len()
        || type_arguments
            .iter()
            .any(|argument| !resolved_type_is_closed_nonlinear(argument, types))
    {
        return None;
    }
    let substitute_payload = |payload: &crate::core::ir::ResolvedTypeId| match types.get(payload)? {
        ResolvedType::GenericParameter(parameter) => {
            let index = generic_parameters
                .iter()
                .position(|candidate| candidate == parameter)?;
            Some(type_arguments[index].clone())
        }
        _ => Some(payload.clone()),
    };
    let instantiated = match types.get(ty)? {
        ResolvedType::Option(payload) => {
            let resolved_payload = substitute_payload(payload)?;
            if !matches!(types.get(payload), Some(ResolvedType::GenericParameter(parameter))
                if generic_parameters.contains(parameter))
            {
                return None;
            }
            ResolvedType::Option(resolved_payload)
        }
        ResolvedType::Result { ok, error } => {
            let resolved_ok = substitute_payload(ok)?;
            let resolved_error = substitute_payload(error)?;
            let uses_parameter = [ok, error].iter().any(|payload| {
                matches!(types.get(payload), Some(ResolvedType::GenericParameter(parameter))
                    if generic_parameters.contains(parameter))
            });
            if !uses_parameter {
                return None;
            }
            ResolvedType::Result {
                ok: resolved_ok,
                error: resolved_error,
            }
        }
        _ => return None,
    };
    types
        .iter()
        .find_map(|(candidate, shape)| (shape == &instantiated).then(|| candidate.clone()))
}

fn resolved_type_is_closed_nonlinear(
    ty: &crate::core::ir::ResolvedTypeId,
    types: &ResolvedTypeTable,
) -> bool {
    use crate::core::ir::ResolvedType;

    match types.get(ty) {
        Some(ResolvedType::Primitive(_)) => true,
        Some(ResolvedType::Nominal {
            arguments,
            is_linear: false,
            ..
        }) => arguments
            .iter()
            .all(|argument| resolved_type_is_closed_nonlinear(argument, types)),
        Some(ResolvedType::Option(inner)) => resolved_type_is_closed_nonlinear(inner, types),
        Some(ResolvedType::Result { ok, error }) => {
            resolved_type_is_closed_nonlinear(ok, types)
                && resolved_type_is_closed_nonlinear(error, types)
        }
        Some(ResolvedType::Tuple(elements)) => elements
            .iter()
            .all(|element| resolved_type_is_closed_nonlinear(element, types)),
        _ => false,
    }
}

fn resolved_generic_predicate_wrapper_parameter(
    callee: &crate::core::ResolvedCallable,
    call: &crate::core::ir::ResolvedCall,
    types: &ResolvedTypeTable,
) -> Option<ResolvedLocalId> {
    use crate::core::ir::{Permission, ResolvedCallee, ResolvedExprKind};

    if callee.signature.generic_parameters.is_empty()
        || callee.signature.generic_parameters.len() != call.type_arguments.len()
        || call.result != callee.signature.result
        || callee.signature.parameters.len() != 1
        || callee.body.parameters.len() != 1
        || !callee.contracts.is_empty()
        || !callee.signature.effects.is_empty()
        || !callee.body.captures.is_empty()
        || !callee.body.default_values.is_empty()
        || callee.has_unmodeled_proof_roots()
        || !callee.body.root.statements.is_empty()
        || callee.body.root.result.is_none()
        || call.permission.is_some()
        || call.arguments.len() != 1
        || call.arguments[0].parameter != callee.signature.parameters[0].id
    {
        return None;
    }
    let result_is_bool = matches!(
        types.get(&callee.signature.result),
        Some(crate::core::ir::ResolvedType::Primitive(
            PrimitiveType::Bool
        ))
    );
    if !result_is_bool {
        return None;
    }
    let formal = &callee.signature.parameters[0];
    if formal.permission.is_some()
        || formal.has_default
        || resolved_generic_sum_parameter_type(
            &formal.ty,
            &callee.signature.generic_parameters,
            &call.type_arguments,
            types,
        )
        .is_none()
    {
        return None;
    }
    let local = callee.body.parameters[0].clone();
    if callee.body.locals.get(&local)?.ty != formal.ty {
        return None;
    }
    let result = callee.body.root.result.as_ref()?;
    if !result.effects.is_empty() {
        return None;
    }
    let ResolvedExprKind::Call(predicate) = &result.kind else {
        return None;
    };
    let ResolvedCallee::Builtin(builtin) = &predicate.callee else {
        return None;
    };
    if !matches!(
        builtin.as_str(),
        "builtin.method.option.is_some"
            | "builtin.method.option.is_none"
            | "builtin.method.result.is_ok"
            | "builtin.method.result.is_err"
    ) || predicate.arguments.len() != 1
        || !predicate.effects.is_empty()
        || !predicate.session.is_empty()
        || predicate.permission != Some(Permission::View)
    {
        return None;
    }
    let receiver = &predicate.arguments[0].value;
    let ResolvedExprKind::Load(place) = &receiver.kind else {
        return None;
    };
    if place.base != local || !place.projections.is_empty() {
        return None;
    }
    Some(local)
}

#[derive(Clone)]
struct ResolvedGenericSumProjectionWrapper {
    receiver_parameter: crate::core::ir::ResolvedParameterId,
    receiver_local: ResolvedLocalId,
    receiver_type: crate::core::ir::ResolvedTypeId,
    fallback_parameter: Option<crate::core::ir::ResolvedParameterId>,
    variant: crate::core::NodeId,
    payload_member: crate::core::NodeId,
    requires_tag: bool,
}

fn direct_local_load(expr: &ResolvedExpr, expected_local: &ResolvedLocalId) -> bool {
    matches!(&expr.kind, ResolvedExprKind::Load(place)
        if &place.base == expected_local && place.projections.is_empty())
}

fn resolved_generic_sum_projection_wrapper(
    callee: &crate::core::ResolvedCallable,
    call: &crate::core::ir::ResolvedCall,
    types: &ResolvedTypeTable,
) -> Option<ResolvedGenericSumProjectionWrapper> {
    use crate::core::ir::{Permission, ResolvedType};

    let crate::core::ir::ResolvedCallee::Function(target) = &call.callee else {
        return None;
    };
    if callee.owner != *target
        || callee.signature.owner != *target
        || callee.signature.generic_parameters.is_empty()
        || callee.signature.generic_parameters.len() != call.type_arguments.len()
        || !call.effects.is_empty()
        || !call.session.is_empty()
        || call.permission.is_some()
        || !callee.signature.effects.is_empty()
        || !callee.body.captures.is_empty()
        || !callee.body.default_values.is_empty()
        || !callee.contracts.is_empty()
        || callee.has_unmodeled_proof_roots()
        || !callee.body.root.statements.is_empty()
        || callee.body.root.result.is_none()
        || callee.signature.parameters.len() != callee.body.parameters.len()
        || !call
            .arguments
            .iter()
            .all(|argument| argument.value.effects.is_empty())
        || call.arguments.len() != callee.signature.parameters.len()
        || call
            .type_arguments
            .iter()
            .any(|argument| !resolved_type_is_closed_nonlinear(argument, types))
    {
        return None;
    }

    let result_parameter = match types.get(&callee.signature.result)? {
        ResolvedType::GenericParameter(parameter) => parameter,
        _ => return None,
    };
    let result_argument_index = callee
        .signature
        .generic_parameters
        .iter()
        .position(|parameter| parameter == result_parameter)?;
    if call.type_arguments.get(result_argument_index)? != &call.result
        || resolved_scalar_category(&call.result, types).is_none()
    {
        return None;
    }

    let result_expr = callee.body.root.result.as_ref()?;
    if !result_expr.effects.is_empty() || result_expr.ty != callee.signature.result {
        return None;
    }
    let ResolvedExprKind::Call(inner_call) = &result_expr.kind else {
        return None;
    };
    let crate::core::ir::ResolvedCallee::Builtin(builtin) = &inner_call.callee else {
        return None;
    };
    let expected_inner_permission = match builtin.as_str() {
        "builtin.method.option.unwrap"
        | "builtin.method.option.unwrap_or"
        | "builtin.method.result.unwrap"
        | "builtin.method.result.unwrap_err"
        | "builtin.method.result.unwrap_or" => Permission::Consume,
        _ => return None,
    };
    if !inner_call.effects.is_empty()
        || !inner_call.session.is_empty()
        || inner_call.permission != Some(expected_inner_permission)
        || !inner_call.type_arguments.is_empty()
        || inner_call.result != callee.signature.result
        || inner_call.arguments.is_empty()
    {
        return None;
    }

    let (sum_payload_parameter, variant_name, payload_name, requires_tag, expects_fallback) = match (
        types.get(&callee.signature.parameters.first()?.ty)?,
        builtin.as_str(),
    ) {
        (ResolvedType::Option(payload), "builtin.method.option.unwrap") => {
            let parameter = match types.get(payload)? {
                ResolvedType::GenericParameter(parameter) => parameter,
                _ => return None,
            };
            (
                parameter,
                "builtin:variant:Option::Some",
                "builtin:variant:Option::Some/payload:0",
                true,
                false,
            )
        }
        (ResolvedType::Option(payload), "builtin.method.option.unwrap_or") => {
            let parameter = match types.get(payload)? {
                ResolvedType::GenericParameter(parameter) => parameter,
                _ => return None,
            };
            (
                parameter,
                "builtin:variant:Option::Some",
                "builtin:variant:Option::Some/payload:0",
                false,
                true,
            )
        }
        (ResolvedType::Result { ok, .. }, "builtin.method.result.unwrap") => {
            let parameter = match types.get(ok)? {
                ResolvedType::GenericParameter(parameter) => parameter,
                _ => return None,
            };
            (
                parameter,
                "builtin:variant:Result::Ok",
                "builtin:variant:Result::Ok/payload:0",
                true,
                false,
            )
        }
        (ResolvedType::Result { error, .. }, "builtin.method.result.unwrap_err") => {
            let parameter = match types.get(error)? {
                ResolvedType::GenericParameter(parameter) => parameter,
                _ => return None,
            };
            (
                parameter,
                "builtin:variant:Result::Err",
                "builtin:variant:Result::Err/payload:0",
                true,
                false,
            )
        }
        (ResolvedType::Result { ok, .. }, "builtin.method.result.unwrap_or") => {
            let parameter = match types.get(ok)? {
                ResolvedType::GenericParameter(parameter) => parameter,
                _ => return None,
            };
            (
                parameter,
                "builtin:variant:Result::Ok",
                "builtin:variant:Result::Ok/payload:0",
                false,
                true,
            )
        }
        _ => return None,
    };
    if sum_payload_parameter != result_parameter
        || callee.signature.parameters.len() != if expects_fallback { 2 } else { 1 }
        || inner_call.arguments.len() != if expects_fallback { 2 } else { 1 }
    {
        return None;
    }

    let receiver_formal = &callee.signature.parameters[0];
    if receiver_formal.permission.is_some() || receiver_formal.has_default {
        return None;
    }
    let instantiated_sum = resolved_generic_sum_parameter_type(
        &receiver_formal.ty,
        &callee.signature.generic_parameters,
        &call.type_arguments,
        types,
    )?;
    let receiver_local = callee.body.parameters[0].clone();
    if callee.body.locals.get(&receiver_local)?.ty != receiver_formal.ty
        || inner_call.arguments[0].conversion.kind != CheckedConversionKind::Identity
        || inner_call.arguments[0].conversion.from != receiver_formal.ty
        || inner_call.arguments[0].conversion.to != receiver_formal.ty
        || inner_call.arguments[0].value.ty != receiver_formal.ty
        || !direct_local_load(&inner_call.arguments[0].value, &receiver_local)
    {
        return None;
    }

    let fallback_parameter = if expects_fallback {
        let fallback_formal = &callee.signature.parameters[1];
        let fallback_local = callee.body.parameters[1].clone();
        if fallback_formal.permission.is_some()
            || fallback_formal.has_default
            || types.get(&fallback_formal.ty)
                != Some(&ResolvedType::GenericParameter(result_parameter.clone()))
            || callee.body.locals.get(&fallback_local)?.ty != fallback_formal.ty
            || inner_call.arguments[1].conversion.kind != CheckedConversionKind::Identity
            || inner_call.arguments[1].conversion.from != fallback_formal.ty
            || inner_call.arguments[1].conversion.to != fallback_formal.ty
            || inner_call.arguments[1].value.ty != fallback_formal.ty
            || !direct_local_load(&inner_call.arguments[1].value, &fallback_local)
        {
            return None;
        }
        Some(fallback_formal.id.clone())
    } else {
        None
    };

    let mut seen = BTreeSet::new();
    for formal in &callee.signature.parameters {
        let argument = call
            .arguments
            .iter()
            .find(|argument| argument.parameter == formal.id)?;
        if !seen.insert(argument.parameter.clone())
            || argument.conversion.from != argument.value.ty
            || !argument.value.effects.is_empty()
        {
            return None;
        }
        let expected_type = if formal.id == receiver_formal.id {
            instantiated_sum.clone()
        } else {
            let parameter = match types.get(&formal.ty)? {
                ResolvedType::GenericParameter(parameter) => parameter,
                _ => return None,
            };
            let index = callee
                .signature
                .generic_parameters
                .iter()
                .position(|candidate| candidate == parameter)?;
            call.type_arguments[index].clone()
        };
        if argument.conversion.to != expected_type
            || argument.conversion.from != expected_type
            || argument.conversion.kind != CheckedConversionKind::Identity
        {
            return None;
        }
    }

    Some(ResolvedGenericSumProjectionWrapper {
        receiver_parameter: receiver_formal.id.clone(),
        receiver_local,
        receiver_type: instantiated_sum,
        fallback_parameter,
        variant: crate::core::NodeId(variant_name.to_string()),
        payload_member: crate::core::NodeId(payload_name.to_string()),
        requires_tag,
    })
}

fn resolved_inline_scalar_call_frame(
    call: &crate::core::ir::ResolvedCall,
    caller_body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<crate::core::NodeId, crate::core::ResolvedCallable>,
    variants: &BTreeMap<crate::core::NodeId, crate::core::ResolvedVariantSchema>,
    caller_vars: &mut Z3VarMap,
) -> Option<(crate::core::ResolvedCallable, Z3VarMap)> {
    let crate::core::ir::ResolvedCallee::Function(target) = &call.callee else {
        return None;
    };
    if !call.effects.is_empty() || !call.session.is_empty() || call.permission.is_some() {
        return None;
    }
    let callee = callables.get(target)?.clone();
    let generic_sum_parameter = if callee.signature.generic_parameters.is_empty() {
        if !call.type_arguments.is_empty() {
            return None;
        }
        None
    } else {
        Some(
            resolved_generic_predicate_wrapper_parameter(&callee, call, types).or_else(|| {
                resolved_generic_sum_projection_wrapper(&callee, call, types)
                    .map(|wrapper| wrapper.receiver_local)
            })?,
        )
    };
    if callee.owner != *target
        || !callee.signature.effects.is_empty()
        || !callee.body.captures.is_empty()
        || !callee.body.default_values.is_empty()
        || callee.has_unmodeled_proof_roots()
        || (generic_sum_parameter.is_none() && call.result != callee.signature.result)
        || call.arguments.len() != callee.signature.parameters.len()
        || callee.body.parameters.len() != callee.signature.parameters.len()
        || callee
            .signature
            .parameters
            .iter()
            .any(|parameter| parameter.has_default || parameter.permission.is_some())
    {
        return None;
    }

    if callee.body.resolved_calls.values().any(|nested| {
        matches!(nested, crate::core::ir::ResolvedCallee::Function(owner)
            if resolved_call_graph_reaches(owner, target, callables, &mut BTreeSet::new()))
    }) {
        return None;
    }

    let mut frame = Z3VarMap::new();
    let mut seen = BTreeSet::new();
    for ((formal, formal_local), argument) in callee
        .signature
        .parameters
        .iter()
        .zip(&callee.body.parameters)
        .zip(&call.arguments)
    {
        if argument.parameter != formal.id
            || !seen.insert(argument.parameter.clone())
            || argument.conversion.from != argument.value.ty
            || !argument.value.effects.is_empty()
        {
            return None;
        }
        if generic_sum_parameter.as_ref() == Some(formal_local) {
            let instantiated_formal = resolved_generic_sum_parameter_type(
                &formal.ty,
                &callee.signature.generic_parameters,
                &call.type_arguments,
                types,
            )?;
            if argument.value.ty != instantiated_formal
                || argument.conversion.from != instantiated_formal
                || argument.conversion.to != instantiated_formal
                || argument.conversion.kind != CheckedConversionKind::Identity
            {
                return None;
            }
            let formal_info = callee.body.locals.get(formal_local)?;
            if formal_info.ty != formal.ty {
                return None;
            }
            // Keep the checker-owned local identity and display name, but give
            // the value binder the already-validated concrete sum type for
            // this call. Passing the raw `Option<T>`/`Result<T, E>` local
            // would either reject a supported actual or create an untyped
            // payload symbol in the caller frame.
            let mut instantiated_local = formal_info.clone();
            instantiated_local.ty = instantiated_formal;
            crate::verifier::resolved_expr::resolved_bind_value_to_local_with_catalog(
                &argument.value,
                caller_body,
                &instantiated_local,
                types,
                callables,
                variants,
                &mut frame,
            )?;
            continue;
        }
        let instantiated_formal = if callee.signature.generic_parameters.is_empty() {
            formal.ty.clone()
        } else {
            match types.get(&formal.ty)? {
                crate::core::ir::ResolvedType::GenericParameter(parameter) => {
                    let index = callee
                        .signature
                        .generic_parameters
                        .iter()
                        .position(|candidate| candidate == parameter)?;
                    call.type_arguments[index].clone()
                }
                crate::core::ir::ResolvedType::Primitive(_) => formal.ty.clone(),
                _ => return None,
            }
        };
        if argument.conversion.to != instantiated_formal
            || !matches!(argument.conversion.kind, CheckedConversionKind::Identity)
        {
            return None;
        }
        let formal_category = resolved_scalar_category(&instantiated_formal, types)?;
        let actual_category = resolved_scalar_category(&argument.value.ty, types)?;
        if formal_category != actual_category {
            return None;
        }
        let formal_info = callee.body.locals.get(formal_local)?;
        if formal_info.ty != formal.ty {
            return None;
        }
        let key = local_var_key(formal_local, &formal_info.display_name);
        let old_key = format!("old.{key}");
        match formal_category {
            Z3TypeCategory::Int => {
                let value = resolved_to_z3_int_with_catalog(
                    &argument.value,
                    caller_body,
                    types,
                    callables,
                    variants,
                    caller_vars,
                )?;
                frame.insert_int(key, value.clone());
                frame.insert_int(old_key, value);
            }
            Z3TypeCategory::Real => {
                let value = resolved_to_z3_real_with_catalog(
                    &argument.value,
                    caller_body,
                    types,
                    callables,
                    variants,
                    caller_vars,
                )?;
                frame.insert_real(key, value.clone());
                frame.insert_real(old_key, value);
            }
            Z3TypeCategory::Bool => {
                let value = resolved_to_z3_bool_with_catalog(
                    &argument.value,
                    caller_body,
                    types,
                    callables,
                    variants,
                    caller_vars,
                )?;
                frame.insert_bool(key, value.clone());
                frame.insert_bool(old_key, value);
            }
        }
    }
    Some((callee, frame))
}

fn resolved_generic_projection_actuals<'a>(
    call: &'a crate::core::ir::ResolvedCall,
    wrapper: &ResolvedGenericSumProjectionWrapper,
) -> Option<(&'a ResolvedExpr, Option<&'a ResolvedExpr>)> {
    let receiver = call
        .arguments
        .iter()
        .find(|argument| argument.parameter == wrapper.receiver_parameter)?;
    let fallback = match &wrapper.fallback_parameter {
        Some(parameter) => Some(
            call.arguments
                .iter()
                .find(|argument| argument.parameter == *parameter)?,
        ),
        None => None,
    };
    if receiver.conversion.kind != CheckedConversionKind::Identity
        || receiver.conversion.from != receiver.value.ty
        || receiver.conversion.to != wrapper.receiver_type
        || receiver.value.ty != wrapper.receiver_type
        || !receiver.value.effects.is_empty()
    {
        return None;
    }
    if let Some(fallback) = fallback {
        if fallback.conversion.kind != CheckedConversionKind::Identity
            || fallback.conversion.from != fallback.value.ty
            || fallback.conversion.to != call.result
            || fallback.value.ty != call.result
            || !fallback.value.effects.is_empty()
        {
            return None;
        }
    }
    Some((&receiver.value, fallback.map(|argument| &argument.value)))
}

fn resolved_inline_generic_projection_int(
    call: &crate::core::ir::ResolvedCall,
    caller_body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<crate::core::NodeId, crate::core::ResolvedCallable>,
    variants: &BTreeMap<crate::core::NodeId, crate::core::ResolvedVariantSchema>,
    caller_vars: &mut Z3VarMap,
) -> Option<Z3Int> {
    let callee = callables.get(match &call.callee {
        crate::core::ir::ResolvedCallee::Function(target) => target,
        _ => return None,
    })?;
    let wrapper = resolved_generic_sum_projection_wrapper(callee, call, types)?;
    if resolved_scalar_category(&call.result, types)? != Z3TypeCategory::Int {
        return None;
    }
    let (receiver, fallback) = resolved_generic_projection_actuals(call, &wrapper)?;
    if wrapper.requires_tag {
        return crate::verifier::resolved_expr::resolved_variant_payload_int(
            receiver,
            &wrapper.variant,
            &wrapper.payload_member,
            caller_body,
            types,
            callables,
            variants,
            caller_vars,
        );
    }
    let tag = crate::verifier::resolved_expr::resolved_variant_tag_condition(
        receiver,
        &wrapper.variant,
        caller_body,
        types,
        callables,
        variants,
        caller_vars,
    )?;
    let payload = match resolved_variant_payload_scalar_or_inactive(
        receiver,
        &wrapper.variant,
        &wrapper.payload_member,
        &call.result,
        caller_body,
        types,
        callables,
        variants,
        caller_vars,
    )? {
        ResolvedScalarValue::Int(value) => value,
        _ => return None,
    };
    let fallback = resolved_to_z3_int_with_catalog(
        fallback?,
        caller_body,
        types,
        callables,
        variants,
        caller_vars,
    )?;
    Some(tag.ite(&payload, &fallback))
}

fn resolved_inline_generic_projection_real(
    call: &crate::core::ir::ResolvedCall,
    caller_body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<crate::core::NodeId, crate::core::ResolvedCallable>,
    variants: &BTreeMap<crate::core::NodeId, crate::core::ResolvedVariantSchema>,
    caller_vars: &mut Z3VarMap,
) -> Option<Z3Real> {
    let callee = callables.get(match &call.callee {
        crate::core::ir::ResolvedCallee::Function(target) => target,
        _ => return None,
    })?;
    let wrapper = resolved_generic_sum_projection_wrapper(callee, call, types)?;
    if resolved_scalar_category(&call.result, types)? != Z3TypeCategory::Real {
        return None;
    }
    let (receiver, fallback) = resolved_generic_projection_actuals(call, &wrapper)?;
    if wrapper.requires_tag {
        return crate::verifier::resolved_expr::resolved_variant_payload_real(
            receiver,
            &wrapper.variant,
            &wrapper.payload_member,
            caller_body,
            types,
            callables,
            variants,
            caller_vars,
        );
    }
    let tag = crate::verifier::resolved_expr::resolved_variant_tag_condition(
        receiver,
        &wrapper.variant,
        caller_body,
        types,
        callables,
        variants,
        caller_vars,
    )?;
    let payload = match resolved_variant_payload_scalar_or_inactive(
        receiver,
        &wrapper.variant,
        &wrapper.payload_member,
        &call.result,
        caller_body,
        types,
        callables,
        variants,
        caller_vars,
    )? {
        ResolvedScalarValue::Real(value) => value,
        _ => return None,
    };
    let fallback = resolved_to_z3_real_with_catalog(
        fallback?,
        caller_body,
        types,
        callables,
        variants,
        caller_vars,
    )?;
    Some(tag.ite(&payload, &fallback))
}

fn resolved_inline_generic_projection_bool(
    call: &crate::core::ir::ResolvedCall,
    caller_body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<crate::core::NodeId, crate::core::ResolvedCallable>,
    variants: &BTreeMap<crate::core::NodeId, crate::core::ResolvedVariantSchema>,
    caller_vars: &mut Z3VarMap,
) -> Option<Z3Bool> {
    let callee = callables.get(match &call.callee {
        crate::core::ir::ResolvedCallee::Function(target) => target,
        _ => return None,
    })?;
    let wrapper = resolved_generic_sum_projection_wrapper(callee, call, types)?;
    if resolved_scalar_category(&call.result, types)? != Z3TypeCategory::Bool {
        return None;
    }
    let (receiver, fallback) = resolved_generic_projection_actuals(call, &wrapper)?;
    if wrapper.requires_tag {
        return crate::verifier::resolved_expr::resolved_variant_payload_bool(
            receiver,
            &wrapper.variant,
            &wrapper.payload_member,
            caller_body,
            types,
            callables,
            variants,
            caller_vars,
        );
    }
    let tag = crate::verifier::resolved_expr::resolved_variant_tag_condition(
        receiver,
        &wrapper.variant,
        caller_body,
        types,
        callables,
        variants,
        caller_vars,
    )?;
    let payload = match resolved_variant_payload_scalar_or_inactive(
        receiver,
        &wrapper.variant,
        &wrapper.payload_member,
        &call.result,
        caller_body,
        types,
        callables,
        variants,
        caller_vars,
    )? {
        ResolvedScalarValue::Bool(value) => value,
        _ => return None,
    };
    let fallback = resolved_to_z3_bool_with_catalog(
        fallback?,
        caller_body,
        types,
        callables,
        variants,
        caller_vars,
    )?;
    Some(tag.ite(&payload, &fallback))
}

fn resolved_inline_scalar_call_int(
    call: &crate::core::ir::ResolvedCall,
    caller_body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<crate::core::NodeId, crate::core::ResolvedCallable>,
    variants: &BTreeMap<crate::core::NodeId, crate::core::ResolvedVariantSchema>,
    caller_vars: &mut Z3VarMap,
) -> Option<Z3Int> {
    if let crate::core::ir::ResolvedCallee::Function(target) = &call.callee {
        if let Some(callee) = callables.get(target) {
            if resolved_generic_sum_projection_wrapper(callee, call, types).is_some() {
                return resolved_inline_generic_projection_int(
                    call,
                    caller_body,
                    types,
                    callables,
                    variants,
                    caller_vars,
                );
            }
        }
    }
    let (callee, mut frame) = resolved_inline_scalar_call_frame(
        call,
        caller_body,
        types,
        callables,
        variants,
        caller_vars,
    )?;
    if resolved_scalar_category(&callee.signature.result, types)? != Z3TypeCategory::Int {
        return None;
    }
    resolved_block_tail_int_with_catalog(
        &callee.body.root,
        &callee.body,
        types,
        callables,
        variants,
        &mut frame,
    )
}

fn resolved_inline_scalar_call_real(
    call: &crate::core::ir::ResolvedCall,
    caller_body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<crate::core::NodeId, crate::core::ResolvedCallable>,
    variants: &BTreeMap<crate::core::NodeId, crate::core::ResolvedVariantSchema>,
    caller_vars: &mut Z3VarMap,
) -> Option<Z3Real> {
    if let crate::core::ir::ResolvedCallee::Function(target) = &call.callee {
        if let Some(callee) = callables.get(target) {
            if resolved_generic_sum_projection_wrapper(callee, call, types).is_some() {
                return resolved_inline_generic_projection_real(
                    call,
                    caller_body,
                    types,
                    callables,
                    variants,
                    caller_vars,
                );
            }
        }
    }
    let (callee, mut frame) = resolved_inline_scalar_call_frame(
        call,
        caller_body,
        types,
        callables,
        variants,
        caller_vars,
    )?;
    if resolved_scalar_category(&callee.signature.result, types)? != Z3TypeCategory::Real {
        return None;
    }
    resolved_block_tail_real_with_catalog(
        &callee.body.root,
        &callee.body,
        types,
        callables,
        variants,
        &mut frame,
    )
}

fn resolved_inline_scalar_call_bool(
    call: &crate::core::ir::ResolvedCall,
    caller_body: &ResolvedBody,
    types: &ResolvedTypeTable,
    callables: &BTreeMap<crate::core::NodeId, crate::core::ResolvedCallable>,
    variants: &BTreeMap<crate::core::NodeId, crate::core::ResolvedVariantSchema>,
    caller_vars: &mut Z3VarMap,
) -> Option<Z3Bool> {
    if let crate::core::ir::ResolvedCallee::Function(target) = &call.callee {
        if let Some(callee) = callables.get(target) {
            if resolved_generic_sum_projection_wrapper(callee, call, types).is_some() {
                return resolved_inline_generic_projection_bool(
                    call,
                    caller_body,
                    types,
                    callables,
                    variants,
                    caller_vars,
                );
            }
        }
    }
    let (callee, mut frame) = resolved_inline_scalar_call_frame(
        call,
        caller_body,
        types,
        callables,
        variants,
        caller_vars,
    )?;
    if resolved_scalar_category(&callee.signature.result, types)? != Z3TypeCategory::Bool {
        return None;
    }
    resolved_block_tail_bool_with_catalog(
        &callee.body.root,
        &callee.body,
        types,
        callables,
        variants,
        &mut frame,
    )
}

fn conversion_may_trap(kind: CheckedConversionKind) -> bool {
    matches!(
        kind,
        CheckedConversionKind::NumericNarrowChecked
            | CheckedConversionKind::DynamicDowncastChecked
            | CheckedConversionKind::DynamicAnyUnpack
            | CheckedConversionKind::SliceView
    )
}

fn is_session_builtin(call: &crate::core::ir::ResolvedCall, expected: &str) -> bool {
    matches!(&call.callee, crate::core::ir::ResolvedCallee::Builtin(builtin)
        if builtin.as_str() == expected)
}

fn session_endpoint_argument<'a>(
    call: &'a crate::core::ir::ResolvedCall,
    types: &ResolvedTypeTable,
) -> Option<&'a ResolvedLocalId> {
    let argument = call.arguments.first()?;
    if !matches!(types.get(&argument.value.ty), Some(ResolvedType::Nominal {
        item, arguments, is_linear: true
    }) if item.as_str() == "builtin:type:SessionChan" && arguments.len() == 1)
        || argument.conversion.kind != CheckedConversionKind::Identity
        || argument.conversion.from != argument.value.ty
        || argument.conversion.to != argument.value.ty
        || !argument.value.effects.is_empty()
    {
        return None;
    }
    let ResolvedExprKind::Load(place) = &argument.value.kind else {
        return None;
    };
    if !place.projections.is_empty() {
        return None;
    }
    Some(&place.base)
}

fn session_transition_matches(
    call: &crate::core::ir::ResolvedCall,
    endpoint: &ResolvedLocalId,
    operation: &str,
) -> bool {
    let [transition] = call.session.as_slice() else {
        return false;
    };
    if &transition.endpoint != endpoint || transition.before == transition.after {
        return false;
    }
    match operation {
        "session_send" => {
            !transition.terminal
                && transition.after.as_str() != "closed"
                && (transition.before.as_str().strip_prefix("!i32 . ")
                    == Some(transition.after.as_str())
                    || transition.before.as_str().strip_prefix("!i64 . ")
                        == Some(transition.after.as_str()))
        }
        "session_recv" => {
            !transition.terminal
                && transition.after.as_str() != "closed"
                && (transition.before.as_str().strip_prefix("?i32 . ")
                    == Some(transition.after.as_str())
                    || transition.before.as_str().strip_prefix("?i64 . ")
                        == Some(transition.after.as_str()))
        }
        "session_close" => {
            transition.terminal
                && transition.before.as_str() == "end"
                && transition.after.as_str() == "closed"
        }
        _ => false,
    }
}

fn session_residual_integer_width(
    call: &crate::core::ir::ResolvedCall,
    marker: char,
) -> Option<PrimitiveType> {
    let transition = call.session.as_slice().first()?;
    let width = transition.before.as_str().strip_prefix(marker)?;
    if width.starts_with("i32 .") {
        Some(PrimitiveType::I32)
    } else if width.starts_with("i64 .") {
        Some(PrimitiveType::I64)
    } else {
        None
    }
}

fn valid_session_operation(
    call: &crate::core::ir::ResolvedCall,
    name: &str,
    types: &ResolvedTypeTable,
) -> Option<()> {
    if !call.effects.is_empty() || call.permission.is_some() || !call.type_arguments.is_empty() {
        return None;
    }
    let endpoint = session_endpoint_argument(call, types)?;
    match name {
        "session_send" => {
            let [_, payload] = call.arguments.as_slice() else {
                return None;
            };
            let residual_width = session_residual_integer_width(call, '!')?;
            let source_is_integer = matches!(
                types.get(&payload.conversion.from),
                Some(ResolvedType::Primitive(
                    PrimitiveType::I32 | PrimitiveType::I64
                ))
            );
            let conversion_is_value_preserving = match payload.conversion.kind {
                CheckedConversionKind::Identity => payload.conversion.from == payload.conversion.to,
                CheckedConversionKind::NumericWiden => matches!(
                    (
                        types.get(&payload.conversion.from),
                        types.get(&payload.conversion.to)
                    ),
                    (
                        Some(ResolvedType::Primitive(PrimitiveType::I32)),
                        Some(ResolvedType::Primitive(PrimitiveType::I64)),
                    )
                ),
                _ => false,
            };
            if !session_transition_matches(call, endpoint, "session_send")
                || !matches!(
                    types.get(&payload.conversion.to),
                    Some(ResolvedType::Primitive(
                        PrimitiveType::I32 | PrimitiveType::I64
                    ))
                )
                || payload.conversion.from != payload.value.ty
                || !source_is_integer
                || !conversion_is_value_preserving
                || types.get(&payload.conversion.to)
                    != Some(&ResolvedType::Primitive(residual_width))
                || !payload.value.effects.is_empty()
                || !matches!(
                    types.get(&call.result),
                    Some(ResolvedType::Primitive(PrimitiveType::Unit))
                )
            {
                return None;
            }
        }
        "session_recv" => {
            let residual_width = session_residual_integer_width(call, '?')?;
            if call.arguments.len() != 1
                || !session_transition_matches(call, endpoint, "session_recv")
                || types.get(&call.result) != Some(&ResolvedType::Primitive(residual_width))
            {
                return None;
            }
        }
        "session_close" => {
            if call.arguments.len() != 1
                || !session_transition_matches(call, endpoint, "session_close")
                || !matches!(
                    types.get(&call.result),
                    Some(ResolvedType::Primitive(PrimitiveType::Unit))
                )
            {
                return None;
            }
        }
        _ => return None,
    }
    Some(())
}

fn valid_session_pair_binding(
    initializer: &ResolvedExpr,
    pattern: &crate::core::ir::ResolvedPattern,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
) -> Option<()> {
    let ResolvedExprKind::Call(call) = &initializer.kind else {
        return None;
    };
    if !is_session_builtin(call, "session_pair")
        || call.type_arguments.len() != 1
        || !call.effects.is_empty()
        || !call.session.is_empty()
        || call.permission.is_some()
        || !call.arguments.is_empty()
        || call.result != initializer.ty
        || pattern.ty != initializer.ty
    {
        return None;
    }
    let Some(ResolvedType::Tuple(endpoint_types)) = types.get(&initializer.ty) else {
        return None;
    };
    let ResolvedPatternKind::Tuple(elements) = &pattern.kind else {
        return None;
    };
    if endpoint_types.len() != 2 || elements.len() != 2 {
        return None;
    }
    let protocol = call.type_arguments.first()?;
    let first_has_protocol = matches!(
        types.get(&endpoint_types[0]),
        Some(ResolvedType::Nominal {
            item,
            arguments,
            is_linear: true,
        }) if item.as_str() == "builtin:type:SessionChan"
            && arguments.len() == 1
            && &arguments[0] == protocol
    );
    let second_is_dual = match types.get(&endpoint_types[1]) {
        Some(ResolvedType::Nominal {
            item,
            arguments,
            is_linear: true,
        }) if item.as_str() == "builtin:type:SessionChan" && arguments.len() == 1 => {
            matches!(
                types.get(&arguments[0]),
                Some(ResolvedType::Nominal {
                    item: dual,
                    arguments,
                    ..
                }) if dual.as_str() == "builtin:type:dual"
                    && arguments.len() == 1
                    && &arguments[0] == protocol
            )
        }
        _ => false,
    };
    if !first_has_protocol || !second_is_dual {
        return None;
    }
    let mut locals = BTreeSet::new();
    for (element, expected_type) in elements.iter().zip(endpoint_types) {
        let ResolvedPatternKind::Binding {
            local,
            by_reference: None,
        } = &element.kind
        else {
            return None;
        };
        if element.ty != *expected_type
            || !locals.insert(local.clone())
            || body.locals.get(local)?.ty != *expected_type
            || !matches!(types.get(expected_type), Some(ResolvedType::Nominal {
                item, arguments, is_linear: true
            }) if item.as_str() == "builtin:type:SessionChan" && arguments.len() == 1)
        {
            return None;
        }
    }
    Some(())
}

fn collect_block_i32_definedness_scoped(
    block: &ResolvedBlock,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
    obligations: &mut Vec<ResolvedIntDefinedness>,
    stationary_loops: &BTreeSet<crate::core::NodeId>,
    context: &mut ResolvedDefinednessContext<'_>,
) -> Option<()> {
    let enclosing_path = context.active_path.clone();
    let mut active_path = enclosing_path.clone();
    let mut reachable = true;
    for stmt in &block.statements {
        if !reachable {
            break;
        }
        context.active_path = active_path.clone();
        let statement_obligation_start = obligations.len();
        match &stmt.kind {
            ResolvedStmtKind::Bind {
                pattern,
                initializer: Some(init),
            } => {
                let is_valid_session_pair = match &init.kind {
                    ResolvedExprKind::Call(call)
                        if init.effects.is_empty() && init.ty == call.result =>
                    {
                        valid_session_pair_binding(init, pattern, body, types).is_some()
                    }
                    _ => false,
                };
                if is_valid_session_pair {
                    context
                        .validated_session_statements
                        .insert((body.owner.clone(), stmt.node_id.clone()));
                    continue;
                }
                if let ResolvedExprKind::Call(call) = &init.kind {
                    if is_session_builtin(call, "session_recv") {
                        if !init.effects.is_empty() || init.ty != call.result {
                            return None;
                        }
                        valid_session_operation(call, "session_recv", types)?;
                        let ResolvedPatternKind::Binding {
                            local,
                            by_reference: None,
                        } = &pattern.kind
                        else {
                            return None;
                        };
                        let local_info = body.locals.get(local)?;
                        if local_info.ty != call.result || pattern.ty != call.result {
                            return None;
                        }
                        let key = local_var_key(local, &local_info.display_name);
                        // The typed session receipt determines the operation's
                        // success/residual transition. Its integer payload is
                        // deliberately left unconstrained here: this collector
                        // only proves checked-expression definedness, and an
                        // unconstrained payload is an over-approximation.
                        let value = Z3Int::new_const(key.as_str());
                        let (minimum, maximum) = machine_integer_bounds(&call.result, types)?;
                        context.type_premises.push(Z3Bool::and(&[
                            &value.ge(&Z3Int::from_i64(minimum)),
                            &value.le(&Z3Int::from_i64(maximum)),
                        ]));
                        vars.insert_int(&key, value);
                        context
                            .validated_session_statements
                            .insert((body.owner.clone(), stmt.node_id.clone()));
                        continue;
                    }
                }
                collect_expr_i32_definedness_scoped(
                    init,
                    body,
                    types,
                    vars,
                    obligations,
                    stationary_loops,
                    context,
                )?;
                let ResolvedPatternKind::Binding {
                    local,
                    by_reference: None,
                } = &pattern.kind
                else {
                    return None;
                };
                let local_info = body.locals.get(local)?;
                let key = local_var_key(local, &local_info.display_name);
                if !matches!(types.get(&init.ty), Some(ResolvedType::Primitive(_))) {
                    if init.ty != local_info.ty || pattern.ty != local_info.ty {
                        return None;
                    }
                    crate::verifier::resolved_expr::resolved_bind_value_to_local_with_catalog(
                        init,
                        body,
                        local_info,
                        types,
                        context.callables?,
                        context.variants?,
                        vars,
                    )?;
                    continue;
                }
                match z3_type_category(&init.ty, types) {
                    Z3TypeCategory::Int => {
                        if let Some(term) =
                            resolved_to_z3_int_in_context(init, body, types, vars, context)
                        {
                            vars.insert_int(key, term);
                        }
                    }
                    Z3TypeCategory::Real => {
                        if let Some(term) =
                            resolved_to_z3_real_in_context(init, body, types, vars, context)
                        {
                            vars.insert_real(key, term);
                        }
                    }
                    Z3TypeCategory::Bool => {
                        if let Some(term) =
                            resolved_to_z3_bool_in_context(init, body, types, vars, context)
                        {
                            vars.insert_bool(key, term);
                        }
                    }
                }
            }
            ResolvedStmtKind::Bind {
                initializer: None, ..
            } => return None,
            ResolvedStmtKind::Contract { .. } | ResolvedStmtKind::Math(_) => {}
            ResolvedStmtKind::Expr(
                expression @ ResolvedExpr {
                    kind: ResolvedExprKind::Call(call),
                    ..
                },
            ) if ["session_send", "session_recv", "session_close"]
                .iter()
                .any(|name| is_session_builtin(call, name)) =>
            {
                if !expression.effects.is_empty() || expression.ty != call.result {
                    return None;
                }
                let name = ["session_send", "session_recv", "session_close"]
                    .into_iter()
                    .find(|name| is_session_builtin(call, name))?;
                valid_session_operation(call, name, types)?;
                if name == "session_send" {
                    let payload = &call.arguments[1];
                    collect_expr_i32_definedness_scoped(
                        &payload.value,
                        body,
                        types,
                        vars,
                        obligations,
                        stationary_loops,
                        context,
                    )?;
                }
                context
                    .validated_session_statements
                    .insert((body.owner.clone(), stmt.node_id.clone()));
            }
            ResolvedStmtKind::Expr(
                expression @ ResolvedExpr {
                    kind: ResolvedExprKind::Call(call),
                    ..
                },
            ) if matches!(
                &call.callee,
                crate::core::ir::ResolvedCallee::Builtin(builtin)
                    if builtin.as_str() == "println"
            ) && call.arguments.len() == 1
                && call.type_arguments.is_empty()
                && call.effects.is_empty()
                && call.session.is_empty()
                && call.permission.is_none()
                && call.result == expression.ty
                && expression.effects.is_empty() =>
            {
                let argument = &call.arguments[0];
                if conversion_may_trap(argument.conversion.kind)
                    || argument.conversion.kind != CheckedConversionKind::Identity
                    || argument.conversion.from != argument.value.ty
                    || argument.conversion.to != argument.value.ty
                    || !argument.value.effects.is_empty()
                {
                    return None;
                }
                collect_expr_i32_definedness_scoped(
                    &argument.value,
                    body,
                    types,
                    vars,
                    obligations,
                    stationary_loops,
                    context,
                )?;
            }
            ResolvedStmtKind::Expr(expression) => collect_expr_i32_definedness_scoped(
                expression,
                body,
                types,
                vars,
                obligations,
                stationary_loops,
                context,
            )?,
            ResolvedStmtKind::Drop(places) => {
                if !crate::verifier::resolved_expr::resolved_drop_is_value_neutral(
                    places, body, types, vars,
                ) && !context.callables.is_some_and(|callables| {
                    resolved_drop_has_checked_actions(places, &stmt.node_id, body, callables)
                }) {
                    return None;
                }
            }
            ResolvedStmtKind::Return { value, .. } => {
                if let Some(value) = value {
                    collect_expr_i32_definedness_scoped(
                        value,
                        body,
                        types,
                        vars,
                        obligations,
                        stationary_loops,
                        context,
                    )?;
                }
                reachable = false;
            }
            ResolvedStmtKind::Break(value) => {
                if let Some(value) = value {
                    collect_expr_i32_definedness_scoped(
                        value,
                        body,
                        types,
                        vars,
                        obligations,
                        stationary_loops,
                        context,
                    )?;
                }
                reachable = false;
            }
            ResolvedStmtKind::Continue => reachable = false,
            ResolvedStmtKind::Scope { body: scoped, .. } => {
                let mut scoped_vars = vars.clone();
                collect_block_i32_definedness_scoped(
                    scoped,
                    body,
                    types,
                    &mut scoped_vars,
                    obligations,
                    stationary_loops,
                    context,
                )?;
            }
            ResolvedStmtKind::While {
                condition,
                body: loop_body,
            } => {
                if !stationary_loops.contains(&stmt.node_id) {
                    return None;
                }
                collect_expr_i32_definedness_scoped(
                    condition,
                    body,
                    types,
                    vars,
                    obligations,
                    stationary_loops,
                    context,
                )?;
                let mut loop_vars = vars.clone();
                collect_block_i32_definedness_scoped(
                    loop_body,
                    body,
                    types,
                    &mut loop_vars,
                    obligations,
                    stationary_loops,
                    context,
                )?;
            }
            ResolvedStmtKind::Assign { value, .. } => {
                let _ = collect_expr_i32_definedness_scoped(
                    value,
                    body,
                    types,
                    vars,
                    obligations,
                    stationary_loops,
                    context,
                );
                return None;
            }
            ResolvedStmtKind::WhileLet { .. }
            | ResolvedStmtKind::Loop(_)
            | ResolvedStmtKind::For { .. }
            | ResolvedStmtKind::IfLet { .. }
            | ResolvedStmtKind::Pinned { .. }
            | ResolvedStmtKind::NestedCallable(_) => return None,
        }
        for obligation in &mut obligations[statement_obligation_start..] {
            obligation.condition = active_path.implies(&obligation.condition);
        }
        if let ResolvedStmtKind::Bind {
            initializer:
                Some(ResolvedExpr {
                    kind:
                        ResolvedExprKind::Try {
                            value,
                            propagation_target,
                        },
                    ..
                }),
            ..
        } = &stmt.kind
        {
            if !body.owner.0.starts_with("transition:") || propagation_target != &body.owner {
                return None;
            }
            let success = crate::verifier::resolved_expr::resolved_variant_tag_condition(
                value,
                &crate::core::NodeId("builtin:variant:Result::Ok".into()),
                body,
                types,
                context.callables?,
                context.variants?,
                vars,
            )?;
            active_path = Z3Bool::and(&[&active_path, &success]);
        }
    }
    if reachable {
        if let Some(result) = &block.result {
            context.active_path = active_path.clone();
            let result_obligation_start = obligations.len();
            collect_expr_i32_definedness_scoped(
                result,
                body,
                types,
                vars,
                obligations,
                stationary_loops,
                context,
            )?;
            for obligation in &mut obligations[result_obligation_start..] {
                obligation.condition = active_path.implies(&obligation.condition);
            }
        }
    }
    context.active_path = enclosing_path;
    Some(())
}

/// Prove that evaluating a root contract expression cannot trap before the
/// expression is used as a precondition or postcondition.  The expression's
/// own boolean formula is deliberately not asserted here: in particular, a
/// Requires clause must not establish facts that hide a trap in that same
/// clause.
fn discharge_root_contract_definedness(
    expression: &ResolvedExpr,
    contract_kind: ContractKind,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
    context: &mut ResolvedDefinednessContext<'_>,
    session: &mut crate::verifier::ctx::SolverSession,
) -> Result<(), (crate::verifier::ctx::VerifStatus, String)> {
    use crate::verifier::ctx::VerifStatus;
    use z3::SatResult;

    let mut obligations = Vec::new();
    if collect_expr_i32_definedness_with_context(
        expression,
        body,
        types,
        vars,
        &mut obligations,
        context,
    )
    .is_none()
    {
        return Err((
            VerifStatus::NotInTrustedSubset,
            "root contract expression is outside the definedness subset".to_string(),
        ));
    }
    context.assert_type_premises(session);
    for obligation in obligations {
        match session.check_scope(obligation.condition.not()).0 {
            SatResult::Unsat => {
                // A proved definedness fact may constrain later evaluation,
                // but never derives from the contract expression itself.
                session.solver.assert(obligation.condition);
            }
            SatResult::Sat => {
                let status = if obligation.modeling_gap {
                    VerifStatus::NotInTrustedSubset
                } else {
                    VerifStatus::Disproven
                };
                let contract_label = match contract_kind {
                    ContractKind::Requires => "requires",
                    ContractKind::Ensures => "ensures",
                    ContractKind::Invariant => "invariant",
                };
                return Err((
                    status,
                    format!(
                        "{contract_label} contract expression is undefined: {}",
                        obligation.failure
                    ),
                ));
            }
            SatResult::Unknown => {
                return Err((
                    session.unknown_status(),
                    "solver could not prove root contract definedness".to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn stationary_loop_receipt_nodes(block: &TypedProofBlock) -> BTreeSet<crate::core::NodeId> {
    fn walk(block: &TypedProofBlock, out: &mut BTreeSet<crate::core::NodeId>) {
        for operation in &block.operations {
            match operation {
                TypedProofOperation::Branch {
                    then_block,
                    else_block,
                    ..
                } => {
                    walk(then_block, out);
                    walk(else_block, out);
                }
                TypedProofOperation::Scope(inner) => walk(inner, out),
                TypedProofOperation::Loop {
                    point,
                    body,
                    receipt,
                    ..
                } => {
                    if receipt.is_some() {
                        // The pathwise Resolved consumer discharges every
                        // checked expression on all reachable loop paths.
                        // Do not rescan the same loop with the legacy
                        // callable-entry-only definedness collector.
                        out.insert(point.clone());
                    }
                    walk(body, out);
                }
                TypedProofOperation::Bind { .. }
                | TypedProofOperation::Call(_)
                | TypedProofOperation::Eval { .. }
                | TypedProofOperation::Assign { .. }
                | TypedProofOperation::Math(_)
                | TypedProofOperation::Break { .. }
                | TypedProofOperation::Continue { .. }
                | TypedProofOperation::Return { .. }
                | TypedProofOperation::Exit => {}
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(block, &mut out);
    out
}

/// One reachable symbolic execution path through the checker-owned proof
/// worklist.  Solver facts and local substitutions stay path-local: a binding
/// introduced in one arm cannot change the expression seen by its sibling.
#[derive(Clone)]
struct TypedProofFact {
    condition: Z3Bool,
    /// Current-state locals mentioned by the source expression that yielded
    /// this fact. At a loop induction cut, facts about a carried local's
    /// preheader value cannot constrain an arbitrary later header value.
    /// Facts about immutable inputs (including `old(parameter)`) may survive.
    current_locals: BTreeSet<ResolvedLocalId>,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct TypedProofBranchGuard {
    statement: crate::core::NodeId,
    expression: crate::core::NodeId,
    condition: crate::core::NodeId,
    then_arm: bool,
}

#[derive(Clone)]
struct TypedProofLoopContext {
    loop_node: crate::core::NodeId,
    guards: Vec<TypedProofBranchGuard>,
    receipt: crate::core::ir::loop_proof::TypedLoopProofReceipt,
}

#[derive(Clone)]
enum TypedProofPathExit {
    Break {
        target_loop: crate::core::NodeId,
        point: crate::core::NodeId,
        value: Option<ResolvedExpr>,
    },
    Continue {
        target_loop: crate::core::NodeId,
        point: crate::core::NodeId,
    },
    Return {
        point: crate::core::NodeId,
        value: Option<ResolvedExpr>,
        conversion: Option<crate::core::ir::CheckedConversion>,
    },
}

#[derive(Clone, Default)]
struct TypedProofPath {
    facts: Vec<TypedProofFact>,
    vars: Z3VarMap,
    loop_contexts: Vec<TypedProofLoopContext>,
    exit: Option<TypedProofPathExit>,
}

fn typed_proof_fact(
    condition: Z3Bool,
    source: &ResolvedExpr,
    body: &ResolvedBody,
) -> TypedProofFact {
    let current_locals = body
        .locals
        .keys()
        .filter(|local| resolved_expr_references_current_local(source, local))
        .cloned()
        .collect();
    TypedProofFact {
        condition,
        current_locals,
    }
}

fn discard_loop_carried_facts(
    path: &mut TypedProofPath,
    carried_locals: &BTreeSet<ResolvedLocalId>,
) {
    path.facts
        .retain(|fact| fact.current_locals.is_disjoint(carried_locals));
}

fn fresh_loop_header_path(
    preheader: &TypedProofPath,
    state_locals: &[(ResolvedLocalId, crate::core::ir::ResolvedTypeId, bool)],
    loop_node: &crate::core::NodeId,
    preheader_ordinal: usize,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
) -> Result<TypedProofPath, (crate::verifier::ctx::VerifStatus, String)> {
    use crate::verifier::ctx::VerifStatus;

    let written_locals = state_locals
        .iter()
        .filter_map(|(local, _, written)| written.then_some(local.clone()))
        .collect::<BTreeSet<_>>();
    let state_local_ids = state_locals
        .iter()
        .map(|(local, _, _)| local.clone())
        .collect::<BTreeSet<_>>();
    if state_local_ids.len() != state_locals.len() {
        return Err(typed_proof_error(
            VerifStatus::NotInTrustedSubset,
            "loop receipt repeats a state-local identity",
        ));
    }

    let mut header = preheader.clone();
    discard_loop_carried_facts(&mut header, &written_locals);
    for (local, receipt_ty, written_in_loop) in state_locals {
        let Some(local_info) = body.locals.get(local) else {
            return Err(typed_proof_error(
                VerifStatus::NotInTrustedSubset,
                "loop receipt names a state local absent from the resolved body",
            ));
        };
        if &local_info.ty != receipt_ty || (*written_in_loop && !local_info.mutable) {
            return Err(typed_proof_error(
                VerifStatus::NotInTrustedSubset,
                "loop state-local type or mutability disagrees with the resolved body",
            ));
        }

        let key = local_var_key(local, &local_info.display_name);
        let symbol = format!("loop.current.{}.{}.{}", loop_node.0, preheader_ordinal, key);
        let local_dependency = BTreeSet::from([local.clone()]);
        match types.get(&local_info.ty) {
            Some(ResolvedType::Primitive(PrimitiveType::Bool)) => {
                let current = Z3Bool::new_const(symbol.as_str());
                if !written_in_loop {
                    let entry_value = preheader.vars.get_bool(&key).cloned().ok_or_else(|| {
                        typed_proof_error(
                            VerifStatus::NotInTrustedSubset,
                            "read-only loop bool state has no actual preheader value",
                        )
                    })?;
                    header.facts.push(TypedProofFact {
                        condition: current.eq(&entry_value),
                        current_locals: local_dependency.clone(),
                    });
                }
                header.vars.insert_bool(key, current);
            }
            Some(ResolvedType::Primitive(PrimitiveType::I32)) => {
                let current = Z3Int::new_const(symbol.as_str());
                let low = current.ge(&Z3Int::from_i64(i32::MIN as i64));
                let high = current.le(&Z3Int::from_i64(i32::MAX as i64));
                header.facts.push(TypedProofFact {
                    condition: Z3Bool::and(&[&low, &high]),
                    current_locals: local_dependency.clone(),
                });
                if !written_in_loop {
                    let entry_value = preheader.vars.get_int(&key).cloned().ok_or_else(|| {
                        typed_proof_error(
                            VerifStatus::NotInTrustedSubset,
                            "read-only loop i32 state has no actual preheader value",
                        )
                    })?;
                    header.facts.push(TypedProofFact {
                        condition: current.eq(&entry_value),
                        current_locals: local_dependency,
                    });
                }
                header.vars.insert_int(key, current);
            }
            Some(ResolvedType::Primitive(PrimitiveType::I64)) => {
                let current = Z3Int::new_const(symbol.as_str());
                let low = current.ge(&Z3Int::from_i64(i64::MIN));
                let high = current.le(&Z3Int::from_i64(i64::MAX));
                header.facts.push(TypedProofFact {
                    condition: Z3Bool::and(&[&low, &high]),
                    current_locals: local_dependency.clone(),
                });
                if !written_in_loop {
                    let entry_value = preheader.vars.get_int(&key).cloned().ok_or_else(|| {
                        typed_proof_error(
                            VerifStatus::NotInTrustedSubset,
                            "read-only loop i64 state has no actual preheader value",
                        )
                    })?;
                    header.facts.push(TypedProofFact {
                        condition: current.eq(&entry_value),
                        current_locals: local_dependency,
                    });
                }
                header.vars.insert_int(key, current);
            }
            _ => {
                return Err(typed_proof_error(
                    VerifStatus::NotInTrustedSubset,
                    "loop-carried local is outside the scalar bool/i32/i64 profile",
                ));
            }
        }
    }
    Ok(header)
}

fn typed_proof_path_condition(path: &TypedProofPath) -> Z3Bool {
    if path.facts.is_empty() {
        Z3Bool::from_bool(true)
    } else {
        let facts = path
            .facts
            .iter()
            .map(|fact| &fact.condition)
            .collect::<Vec<_>>();
        Z3Bool::and(&facts)
    }
}

fn typed_proof_error(
    status: crate::verifier::ctx::VerifStatus,
    detail: impl Into<String>,
) -> (crate::verifier::ctx::VerifStatus, String) {
    (status, detail.into())
}

fn loop_has_intersecting_loan(
    callable: &crate::core::ir::ResolvedCallable,
    receipt: &crate::core::ir::loop_proof::TypedLoopProofReceipt,
    post_loop_exit: &crate::core::cfg::BasicBlockId,
) -> bool {
    let loop_blocks = receipt
        .cfg_reachable_blocks
        .iter()
        .filter(|block| *block != post_loop_exit)
        .cloned()
        .collect::<BTreeSet<_>>();

    receipt.loans.iter().any(|loan| {
        if !callable.cfg.reachable.contains(&loan.start.block) {
            return false;
        }
        if loop_blocks.contains(&loan.start.block) {
            return true;
        }

        // A loan may start before the loop and end after it, so inspecting
        // only loan.start or end_edges inside the loop misses a live loan
        // spanning the induction cut. Check whether the start can reach an
        // executed loop block without crossing one of its checker-recorded
        // end edges. This over-approximates block-internal timing and thus
        // rejects ambiguous lifetimes rather than assuming them inactive.
        let end_edges = loan.end_edges.iter().cloned().collect::<BTreeSet<_>>();
        let mut seen = BTreeSet::new();
        let mut pending = vec![loan.start.block.clone()];
        while let Some(block) = pending.pop() {
            if !seen.insert(block.clone()) {
                continue;
            }
            if loop_blocks.contains(&block) {
                return true;
            }
            for edge in callable
                .cfg
                .edges
                .values()
                .filter(|edge| edge.from == block)
            {
                if end_edges.contains(&edge.id) || !callable.cfg.reachable.contains(&edge.to) {
                    continue;
                }
                pending.push(edge.to.clone());
            }
        }
        false
    })
}

/// Admit only loops whose checker-owned resource/loan evidence is completely
/// present and unchanged through every reachable loop block and edge. Scalar
/// Z3 substitution does not model ownership transitions, so even a balanced
/// move/drop inside one iteration remains outside this capability.
fn validate_resolved_loop_ownership_profile(
    callable: &crate::core::ir::ResolvedCallable,
    receipt: &crate::core::ir::loop_proof::TypedLoopProofReceipt,
) -> Result<(), (crate::verifier::ctx::VerifStatus, String)> {
    use crate::core::cfg::EdgeKind;
    use crate::verifier::ctx::VerifStatus;

    if !receipt.call_nodes.is_empty() || !receipt.effect_nodes.is_empty() {
        return Err(typed_proof_error(
            VerifStatus::NotInTrustedSubset,
            "loop calls/effects need a validated call/effect transfer receipt",
        ));
    }

    let entries = receipt
        .cfg_edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Fallthrough && edge.source.node == receipt.loop_node)
        .collect::<Vec<_>>();
    let [entry] = entries.as_slice() else {
        return Err(typed_proof_error(
            VerifStatus::NotInTrustedSubset,
            "loop receipt lacks one exact preheader edge",
        ));
    };
    let body_edges = receipt
        .cfg_edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::LoopBody && edge.source.node == receipt.loop_node)
        .collect::<Vec<_>>();
    let [body_edge] = body_edges.as_slice() else {
        return Err(typed_proof_error(
            VerifStatus::NotInTrustedSubset,
            "loop receipt lacks one exact condition-true edge",
        ));
    };
    let exit_edges = receipt
        .cfg_edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::LoopExit && edge.source.node == receipt.loop_node)
        .collect::<Vec<_>>();
    let [exit_edge] = exit_edges.as_slice() else {
        return Err(typed_proof_error(
            VerifStatus::NotInTrustedSubset,
            "loop receipt lacks one exact condition-false edge",
        ));
    };
    if entry.to != body_edge.from || body_edge.from != exit_edge.from {
        return Err(typed_proof_error(
            VerifStatus::NotInTrustedSubset,
            "loop preheader, condition and body edges do not share the receipted header",
        ));
    }
    let header = entry.to.clone();
    let post_loop_exit = exit_edge.to.clone();
    if loop_has_intersecting_loan(callable, receipt, &post_loop_exit) {
        return Err(typed_proof_error(
            VerifStatus::NotInTrustedSubset,
            "loop has a checker-owned loan live on an induction path",
        ));
    }

    let resource_by_edge = receipt
        .resource_edges
        .iter()
        .map(|state| (state.edge.clone(), state))
        .collect::<std::collections::BTreeMap<_, _>>();
    if resource_by_edge.len() != receipt.resource_edges.len()
        || resource_by_edge.len() != receipt.cfg_edges.len()
    {
        return Err(typed_proof_error(
            VerifStatus::NotInTrustedSubset,
            "loop receipt does not carry exactly one resource snapshot per CFG edge",
        ));
    }

    let resource_by_block = receipt
        .resource_blocks
        .iter()
        .map(|state| (state.block.clone(), state))
        .collect::<std::collections::BTreeMap<_, _>>();
    if resource_by_block.len() != receipt.resource_blocks.len()
        || resource_by_block.keys().cloned().collect::<BTreeSet<_>>()
            != receipt
                .cfg_blocks
                .iter()
                .map(|block| block.id.clone())
                .filter(|block| block != &post_loop_exit)
                .collect::<BTreeSet<_>>()
    {
        return Err(typed_proof_error(
            VerifStatus::NotInTrustedSubset,
            "loop receipt does not carry exactly one ownership snapshot per CFG block",
        ));
    }
    if receipt
        .resource_actions
        .iter()
        .any(|action| action.kind != crate::core::CanonicalActionKind::Read)
    {
        return Err(typed_proof_error(
            VerifStatus::NotInTrustedSubset,
            "loop has a checker-owned ownership action outside the scalar induction profile",
        ));
    }

    for edge in &receipt.cfg_edges {
        let edge_is_reachable = (edge.id == entry.id
            || receipt.cfg_reachable_blocks.contains(&edge.from))
            && receipt.cfg_reachable_blocks.contains(&edge.to);
        if !edge_is_reachable {
            continue;
        }
        let Some(snapshot) = resource_by_edge.get(&edge.id) else {
            return Err(typed_proof_error(
                VerifStatus::NotInTrustedSubset,
                "reachable loop CFG edge has no ownership snapshot",
            ));
        };
        let (Some(from_input), Some(from_output), Some(to_input)) = (
            &snapshot.from_input,
            &snapshot.from_output,
            &snapshot.to_input,
        ) else {
            return Err(typed_proof_error(
                VerifStatus::NotInTrustedSubset,
                "reachable loop CFG edge has incomplete ownership state",
            ));
        };
        if from_output != to_input {
            return Err(typed_proof_error(
                VerifStatus::NotInTrustedSubset,
                "loop ownership changes across a reachable CFG edge",
            ));
        }
        if callable.resources.in_states.get(&edge.from) != Some(from_input)
            || callable.resources.out_states.get(&edge.from) != Some(from_output)
            || callable.resources.in_states.get(&edge.to) != Some(to_input)
        {
            return Err(typed_proof_error(
                VerifStatus::NotInTrustedSubset,
                "loop edge ownership snapshot disagrees with the callable analysis",
            ));
        }
    }

    if !receipt.cfg_reachable_blocks.contains(&header) {
        return Err(typed_proof_error(
            VerifStatus::NotInTrustedSubset,
            "loop header is not reachable in the checker CFG receipt",
        ));
    }
    for block in &receipt.cfg_reachable_blocks {
        if block == &post_loop_exit {
            continue;
        }
        let Some(snapshot) = resource_by_block.get(block) else {
            return Err(typed_proof_error(
                VerifStatus::NotInTrustedSubset,
                "reachable loop block has no ownership snapshot",
            ));
        };
        let (Some(before), Some(after)) = (&snapshot.input, &snapshot.output) else {
            return Err(typed_proof_error(
                VerifStatus::NotInTrustedSubset,
                "reachable loop block has incomplete ownership in/out state",
            ));
        };
        if callable.resources.in_states.get(block) != Some(before)
            || callable.resources.out_states.get(block) != Some(after)
        {
            return Err(typed_proof_error(
                VerifStatus::NotInTrustedSubset,
                "loop block ownership snapshot disagrees with the callable analysis",
            ));
        }
        if before != after {
            return Err(typed_proof_error(
                VerifStatus::NotInTrustedSubset,
                "loop ownership changes within a reachable CFG block",
            ));
        }
    }
    Ok(())
}

fn validate_typed_loop_receipt(
    receipt: &crate::core::ir::loop_proof::TypedLoopProofReceipt,
    callable: &crate::core::ir::ResolvedCallable,
    loop_node: &crate::core::NodeId,
    scope: &[crate::core::ir::TypedProofScopeFrame],
    invariants: &[crate::core::ir::TypedLoopInvariantPoint],
) -> Result<(), (crate::verifier::ctx::VerifStatus, String)> {
    use crate::core::ir::loop_proof::{
        validate_typed_loop_proof_receipt, TYPED_LOOP_PROOF_SCHEMA_VERSION,
    };
    use crate::verifier::ctx::VerifStatus;

    if receipt.schema_version != TYPED_LOOP_PROOF_SCHEMA_VERSION
        || receipt.owner != callable.owner
        || receipt.loop_node != *loop_node
        || receipt.scope.as_slice() != scope
    {
        return Err(typed_proof_error(
            VerifStatus::NotInTrustedSubset,
            "loop receipt owner, scope, target or schema version does not match the proof point",
        ));
    }
    validate_typed_loop_proof_receipt(receipt, callable, loop_node, scope, invariants).map_err(
        |detail| {
            typed_proof_error(
                VerifStatus::NotInTrustedSubset,
                format!("checker loop receipt validation failed: {detail}"),
            )
        },
    )?;
    validate_resolved_loop_ownership_profile(callable, receipt)
}

/// Check expression-definedness independently under each path's bindings and
/// facts.  Definedness is then available to later statements on that same path.
fn discharge_typed_expr_definedness(
    expression: &ResolvedExpr,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    paths: &mut [TypedProofPath],
    session: &mut crate::verifier::ctx::SolverSession,
) -> Result<(), (crate::verifier::ctx::VerifStatus, String)> {
    use crate::verifier::ctx::VerifStatus;
    use z3::SatResult;

    for path in paths {
        let mut obligations = Vec::new();
        if collect_expr_i32_definedness(expression, body, types, &mut path.vars, &mut obligations)
            .is_none()
        {
            return Err(typed_proof_error(
                VerifStatus::NotInTrustedSubset,
                "could not encode scoped checked-integer definedness obligations",
            ));
        }
        for obligation in obligations {
            let path_condition = typed_proof_path_condition(path);
            let counterexample = Z3Bool::and(&[&path_condition, &obligation.condition.not()]);
            match session.check_scope(counterexample).0 {
                SatResult::Unsat => {
                    path.facts
                        .push(typed_proof_fact(obligation.condition, expression, body))
                }
                SatResult::Sat => {
                    let status = if obligation.modeling_gap {
                        VerifStatus::NotInTrustedSubset
                    } else {
                        VerifStatus::Disproven
                    };
                    return Err(typed_proof_error(status, obligation.failure));
                }
                SatResult::Unknown => {
                    return Err(typed_proof_error(
                        session.unknown_status(),
                        "solver could not prove scoped integer definedness",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn encode_typed_proof_bool(
    expression: &ResolvedExpr,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
) -> Result<Z3Bool, (crate::verifier::ctx::VerifStatus, String)> {
    resolved_to_z3_bool(expression, body, types, vars).ok_or_else(|| {
        typed_proof_error(
            crate::verifier::ctx::VerifStatus::NotInTrustedSubset,
            "typed proof point is outside the scalar boolean encoding",
        )
    })
}

fn check_typed_proof_condition(
    expression: &ResolvedExpr,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    paths: &mut [TypedProofPath],
    session: &mut crate::verifier::ctx::SolverSession,
    proof_label: &str,
) -> Result<(), (crate::verifier::ctx::VerifStatus, String)> {
    use crate::verifier::ctx::VerifStatus;
    use z3::SatResult;

    discharge_typed_expr_definedness(expression, body, types, paths, session)?;
    for path in paths {
        let condition = encode_typed_proof_bool(expression, body, types, &mut path.vars)?;
        let path_condition = typed_proof_path_condition(path);
        let counterexample = Z3Bool::and(&[&path_condition, &condition.not()]);
        match session.check_scope(counterexample).0 {
            SatResult::Unsat => {
                path.facts
                    .push(typed_proof_fact(condition, expression, body));
            }
            SatResult::Sat => {
                return Err(typed_proof_error(
                    VerifStatus::Disproven,
                    format!("{proof_label} is false on a reachable typed path"),
                ));
            }
            SatResult::Unknown => {
                return Err(typed_proof_error(
                    session.unknown_status(),
                    format!("solver could not prove {proof_label}"),
                ));
            }
        }
    }
    Ok(())
}

fn bind_typed_proof_local(
    local: &ResolvedLocalId,
    initializer: &ResolvedExpr,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
) -> Result<(), (crate::verifier::ctx::VerifStatus, String)> {
    use crate::verifier::ctx::VerifStatus;

    let local_info = body.locals.get(local).ok_or_else(|| {
        typed_proof_error(
            VerifStatus::NotInTrustedSubset,
            "typed proof binding has no checker-owned local identity",
        )
    })?;
    if initializer.ty != local_info.ty {
        return Err(typed_proof_error(
            VerifStatus::NotInTrustedSubset,
            "typed proof binding local type disagrees with its checked initializer",
        ));
    }
    let key = local_var_key(local, &local_info.display_name);
    match z3_type_category(&initializer.ty, types) {
        Z3TypeCategory::Int => {
            let term = resolved_to_z3_int(initializer, body, types, vars).ok_or_else(|| {
                typed_proof_error(
                    VerifStatus::NotInTrustedSubset,
                    "typed proof integer binding cannot be encoded",
                )
            })?;
            vars.insert_int(key, term);
        }
        Z3TypeCategory::Real => {
            let term = resolved_to_z3_real(initializer, body, types, vars).ok_or_else(|| {
                typed_proof_error(
                    VerifStatus::NotInTrustedSubset,
                    "typed proof real binding cannot be encoded",
                )
            })?;
            vars.insert_real(key, term);
        }
        Z3TypeCategory::Bool => {
            let term = resolved_to_z3_bool(initializer, body, types, vars).ok_or_else(|| {
                typed_proof_error(
                    VerifStatus::NotInTrustedSubset,
                    "typed proof boolean binding cannot be encoded",
                )
            })?;
            vars.insert_bool(key, term);
        }
    }
    Ok(())
}

fn assign_typed_proof_local(
    target: &ResolvedPlace,
    value: &ResolvedExpr,
    conversion: &crate::core::ir::CheckedConversion,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
) -> Result<(), (crate::verifier::ctx::VerifStatus, String)> {
    use crate::verifier::ctx::VerifStatus;

    if !target.projections.is_empty() {
        return Err(typed_proof_error(
            VerifStatus::NotInTrustedSubset,
            "loop assignment targets a projected place",
        ));
    }
    let Some(local_info) = body.locals.get(&target.base) else {
        return Err(typed_proof_error(
            VerifStatus::NotInTrustedSubset,
            "loop assignment target has no checker-owned local identity",
        ));
    };
    if !local_info.mutable
        || conversion.kind != CheckedConversionKind::Identity
        || conversion.from != local_info.ty
        || conversion.to != local_info.ty
        || value.ty != local_info.ty
    {
        return Err(typed_proof_error(
            VerifStatus::NotInTrustedSubset,
            "loop assignment is not an identity write to a typed mutable local",
        ));
    }

    let key = local_var_key(&local_info.id, &local_info.display_name);
    match types.get(&local_info.ty) {
        Some(ResolvedType::Primitive(PrimitiveType::Bool)) => {
            let term = resolved_to_z3_bool(value, body, types, vars).ok_or_else(|| {
                typed_proof_error(
                    VerifStatus::NotInTrustedSubset,
                    "loop assignment boolean value cannot be encoded",
                )
            })?;
            vars.insert_bool(key, term);
        }
        Some(ResolvedType::Primitive(PrimitiveType::I32 | PrimitiveType::I64)) => {
            let term = resolved_to_z3_int(value, body, types, vars).ok_or_else(|| {
                typed_proof_error(
                    VerifStatus::NotInTrustedSubset,
                    "loop assignment integer value cannot be encoded",
                )
            })?;
            vars.insert_int(key, term);
        }
        _ => {
            return Err(typed_proof_error(
                VerifStatus::NotInTrustedSubset,
                "loop assignment type is outside the scalar bool/i32/i64 profile",
            ));
        }
    }
    Ok(())
}

fn validate_loop_body_edge_coverage(
    receipt: &crate::core::ir::loop_proof::TypedLoopProofReceipt,
    outcomes: &[TypedProofPath],
    loop_node: &crate::core::NodeId,
    body_node: &crate::core::NodeId,
) -> Result<(), (crate::verifier::ctx::VerifStatus, String)> {
    use crate::core::ir::loop_proof::TypedLoopEdgeKind;
    use crate::verifier::ctx::VerifStatus;

    type EdgeKey = (
        crate::core::NodeId,
        TypedLoopEdgeKind,
        Vec<crate::core::ir::loop_proof::TypedLoopBranchGuard>,
        Option<crate::core::NodeId>,
    );

    let mut expected = std::collections::BTreeMap::<EdgeKey, usize>::new();
    for edge in receipt.body_edges.iter().filter(|edge| edge.reachable) {
        *expected
            .entry((
                edge.site.clone(),
                edge.kind,
                edge.guards.clone(),
                edge.terminal_value.clone(),
            ))
            .or_default() += 1;
    }

    let mut actual = std::collections::BTreeMap::<EdgeKey, usize>::new();
    for outcome in outcomes {
        let Some(context) = outcome
            .loop_contexts
            .last()
            .filter(|context| context.loop_node == *loop_node)
        else {
            return Err(typed_proof_error(
                VerifStatus::NotInTrustedSubset,
                "loop outcome is not bound to the exact checker loop context",
            ));
        };
        if context.receipt != *receipt {
            return Err(typed_proof_error(
                VerifStatus::NotInTrustedSubset,
                "loop outcome carries a different checker receipt",
            ));
        }
        let guards = context
            .guards
            .iter()
            .map(|guard| crate::core::ir::loop_proof::TypedLoopBranchGuard {
                statement: guard.statement.clone(),
                expression: guard.expression.clone(),
                condition: guard.condition.clone(),
                then_arm: guard.then_arm,
            })
            .collect::<Vec<_>>();
        let (site, kind, terminal_value) = match outcome.exit.as_ref() {
            None => (body_node.clone(), TypedLoopEdgeKind::BodyFallthrough, None),
            Some(TypedProofPathExit::Break {
                target_loop,
                point,
                value,
            }) if target_loop == loop_node => (
                point.clone(),
                TypedLoopEdgeKind::Break,
                value.as_ref().map(|value| value.node_id.clone()),
            ),
            Some(TypedProofPathExit::Continue { target_loop, point })
                if target_loop == loop_node =>
            {
                (point.clone(), TypedLoopEdgeKind::Continue, None)
            }
            Some(TypedProofPathExit::Return { point, value, .. }) => (
                point.clone(),
                TypedLoopEdgeKind::Return,
                value.as_ref().map(|value| value.node_id.clone()),
            ),
            Some(TypedProofPathExit::Break { .. } | TypedProofPathExit::Continue { .. }) => {
                return Err(typed_proof_error(
                    VerifStatus::NotInTrustedSubset,
                    "loop outcome targets a different enclosing loop",
                ));
            }
        };
        *actual
            .entry((site, kind, guards, terminal_value))
            .or_default() += 1;
    }

    if actual != expected {
        return Err(typed_proof_error(
            VerifStatus::NotInTrustedSubset,
            "Resolved loop paths do not cover the exact reachable checker body edges",
        ));
    }
    Ok(())
}

fn validate_typed_return_shape(
    value: Option<&ResolvedExpr>,
    conversion: Option<&crate::core::ir::CheckedConversion>,
    result_type: &crate::core::ir::ResolvedTypeId,
    types: &ResolvedTypeTable,
) -> Result<(), (crate::verifier::ctx::VerifStatus, String)> {
    use crate::core::ir::{CheckedConversionKind, PrimitiveType, ResolvedType};
    use crate::verifier::ctx::VerifStatus;

    match value {
        Some(value)
            if value.ty == *result_type
                && conversion.is_none_or(|conversion| {
                    conversion.kind == CheckedConversionKind::Identity
                        && conversion.from == value.ty
                        && conversion.to == *result_type
                }) =>
        {
            Ok(())
        }
        None if conversion.is_none()
            && matches!(
                types.get(result_type),
                Some(ResolvedType::Primitive(PrimitiveType::Unit))
            ) =>
        {
            Ok(())
        }
        _ => Err(typed_proof_error(
            VerifStatus::NotInTrustedSubset,
            "return value or checked conversion is outside the scalar identity profile",
        )),
    }
}

fn verify_typed_proof_block(
    block: &TypedProofBlock,
    mut paths: Vec<TypedProofPath>,
    callable: &crate::core::ir::ResolvedCallable,
    scope: &mut Vec<crate::core::ir::TypedProofScopeFrame>,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    session: &mut crate::verifier::ctx::SolverSession,
) -> Result<Vec<TypedProofPath>, (crate::verifier::ctx::VerifStatus, String)> {
    use crate::core::ir::TypedProofOperation;
    use crate::verifier::ctx::VerifStatus;
    let mut terminal_paths = Vec::new();

    for operation in &block.operations {
        let mut active_paths = Vec::with_capacity(paths.len());
        for path in paths.drain(..) {
            if path.exit.is_some() {
                terminal_paths.push(path);
            } else {
                active_paths.push(path);
            }
        }
        paths = active_paths;
        if paths.is_empty() {
            // Every proof point remains in the census. A point after an
            // unconditional typed exit is vacuously checked because its path
            // set is empty; it is never turned into a fact for another path.
            break;
        }
        match operation {
            TypedProofOperation::Bind {
                local, initializer, ..
            } => {
                if !initializer.effects.is_empty() {
                    return Err(typed_proof_error(
                        VerifStatus::NotInTrustedSubset,
                        "typed proof binding has an unmodeled effect",
                    ));
                }
                discharge_typed_expr_definedness(initializer, body, types, &mut paths, session)?;
                for path in &mut paths {
                    bind_typed_proof_local(local, initializer, body, types, &mut path.vars)?;
                }
            }
            TypedProofOperation::Call(_) => {
                return Err(typed_proof_error(
                    VerifStatus::NotInTrustedSubset,
                    "call effects and return-value ownership are outside the Resolved loop profile",
                ));
            }
            TypedProofOperation::Eval { expression, .. } => {
                if !expression.effects.is_empty() {
                    return Err(typed_proof_error(
                        VerifStatus::NotInTrustedSubset,
                        "typed expression evaluation has an unmodeled effect",
                    ));
                }
                discharge_typed_expr_definedness(expression, body, types, &mut paths, session)?;
            }
            TypedProofOperation::Assign {
                point,
                target,
                value,
                conversion,
            } => {
                if !value.effects.is_empty() {
                    return Err(typed_proof_error(
                        VerifStatus::NotInTrustedSubset,
                        "typed assignment value has an unmodeled effect",
                    ));
                }
                for path in &paths {
                    if let Some(context) = path.loop_contexts.last() {
                        let exact_writes = context
                            .receipt
                            .writes
                            .iter()
                            .filter(|write| write.statement == *point)
                            .collect::<Vec<_>>();
                        if let [write] = exact_writes.as_slice() {
                            if write.target != *target
                                || write.value != value.node_id
                                || write.conversion != *conversion
                            {
                                return Err(typed_proof_error(
                                    VerifStatus::NotInTrustedSubset,
                                    "typed assignment disagrees with its checker loop write receipt",
                                ));
                            }
                        } else {
                            return Err(typed_proof_error(
                                VerifStatus::NotInTrustedSubset,
                                "loop assignment has no unique checker-owned write receipt",
                            ));
                        }
                        if !context
                            .receipt
                            .state_locals
                            .iter()
                            .any(|state| state.local == target.base && state.written_in_loop)
                        {
                            return Err(typed_proof_error(
                                VerifStatus::NotInTrustedSubset,
                                "loop assignment target is absent from the state-local receipt",
                            ));
                        }
                    }
                }
                discharge_typed_expr_definedness(value, body, types, &mut paths, session)?;
                for path in &mut paths {
                    assign_typed_proof_local(
                        target,
                        value,
                        conversion,
                        body,
                        types,
                        &mut path.vars,
                    )?;
                }
            }
            TypedProofOperation::Math(point) => {
                if point.scope.as_slice() != scope.as_slice() {
                    return Err(typed_proof_error(
                        VerifStatus::NotInTrustedSubset,
                        "Math proof point lexical scope disagrees with the checked worklist",
                    ));
                }
                check_typed_proof_condition(
                    &point.condition,
                    body,
                    types,
                    &mut paths,
                    session,
                    "math assertion",
                )?;
            }
            TypedProofOperation::Branch {
                point,
                expression,
                condition,
                then_block,
                else_block,
            } => {
                if !condition.effects.is_empty() {
                    return Err(typed_proof_error(
                        VerifStatus::NotInTrustedSubset,
                        "branch condition has an unmodeled effect",
                    ));
                }
                discharge_typed_expr_definedness(condition, body, types, &mut paths, session)?;
                let mut then_paths = Vec::with_capacity(paths.len());
                let mut else_paths = Vec::with_capacity(paths.len());
                for mut path in paths.drain(..) {
                    let condition_value =
                        encode_typed_proof_bool(condition, body, types, &mut path.vars)?;
                    let mut then_path = path.clone();
                    then_path.facts.push(typed_proof_fact(
                        condition_value.clone(),
                        condition,
                        body,
                    ));
                    if let Some(context) = then_path.loop_contexts.last_mut() {
                        context.guards.push(TypedProofBranchGuard {
                            statement: point.clone(),
                            expression: expression.clone(),
                            condition: condition.node_id.clone(),
                            then_arm: true,
                        });
                    }
                    then_paths.push(then_path);
                    let mut else_path = path;
                    else_path
                        .facts
                        .push(typed_proof_fact(condition_value.not(), condition, body));
                    if let Some(context) = else_path.loop_contexts.last_mut() {
                        context.guards.push(TypedProofBranchGuard {
                            statement: point.clone(),
                            expression: expression.clone(),
                            condition: condition.node_id.clone(),
                            then_arm: false,
                        });
                    }
                    else_paths.push(else_path);
                }
                scope.push(crate::core::ir::TypedProofScopeFrame::Branch {
                    statement: point.clone(),
                    expression: expression.clone(),
                    then_arm: true,
                });
                scope.push(crate::core::ir::TypedProofScopeFrame::Block(
                    then_block.id.clone(),
                ));
                let then_paths = verify_typed_proof_block(
                    then_block, then_paths, callable, scope, body, types, session,
                )?;
                scope.pop();
                scope.pop();
                scope.push(crate::core::ir::TypedProofScopeFrame::Branch {
                    statement: point.clone(),
                    expression: expression.clone(),
                    then_arm: false,
                });
                scope.push(crate::core::ir::TypedProofScopeFrame::Block(
                    else_block.id.clone(),
                ));
                let else_paths = verify_typed_proof_block(
                    else_block, else_paths, callable, scope, body, types, session,
                )?;
                scope.pop();
                scope.pop();
                paths = then_paths;
                paths.extend(else_paths);
                if paths.len() > 256 {
                    return Err(typed_proof_error(
                        VerifStatus::NotInTrustedSubset,
                        "scoped proof worklist exceeds the bounded path count",
                    ));
                }
            }
            TypedProofOperation::Scope(inner) => {
                scope.push(crate::core::ir::TypedProofScopeFrame::Block(
                    inner.id.clone(),
                ));
                paths =
                    verify_typed_proof_block(inner, paths, callable, scope, body, types, session)?;
                scope.pop();
            }
            TypedProofOperation::Break {
                point,
                target_loop,
                value,
            } => {
                if let Some(value) = value {
                    if !value.effects.is_empty() {
                        return Err(typed_proof_error(
                            VerifStatus::NotInTrustedSubset,
                            "break value has an unmodeled effect",
                        ));
                    }
                    discharge_typed_expr_definedness(value, body, types, &mut paths, session)?;
                }
                let Some(current_loop) = paths
                    .first()
                    .and_then(|path| path.loop_contexts.last())
                    .map(|context| context.loop_node.clone())
                else {
                    return Err(typed_proof_error(
                        VerifStatus::NotInTrustedSubset,
                        "break is outside the trusted loop context",
                    ));
                };
                if target_loop.as_ref() != Some(&current_loop)
                    || paths.iter().any(|path| {
                        path.loop_contexts
                            .last()
                            .is_none_or(|context| context.loop_node != current_loop)
                    })
                {
                    return Err(typed_proof_error(
                        VerifStatus::NotInTrustedSubset,
                        "break target does not match the exact enclosing loop",
                    ));
                }
                for path in &mut paths {
                    path.exit = Some(TypedProofPathExit::Break {
                        target_loop: current_loop.clone(),
                        point: point.clone(),
                        value: value.clone(),
                    });
                }
            }
            TypedProofOperation::Continue { point, target_loop } => {
                let Some(current_loop) = paths
                    .first()
                    .and_then(|path| path.loop_contexts.last())
                    .map(|context| context.loop_node.clone())
                else {
                    return Err(typed_proof_error(
                        VerifStatus::NotInTrustedSubset,
                        "continue is outside the trusted loop context",
                    ));
                };
                if target_loop.as_ref() != Some(&current_loop)
                    || paths.iter().any(|path| {
                        path.loop_contexts
                            .last()
                            .is_none_or(|context| context.loop_node != current_loop)
                    })
                {
                    return Err(typed_proof_error(
                        VerifStatus::NotInTrustedSubset,
                        "continue target does not match the exact enclosing loop",
                    ));
                }
                for path in &mut paths {
                    path.exit = Some(TypedProofPathExit::Continue {
                        target_loop: current_loop.clone(),
                        point: point.clone(),
                    });
                }
            }
            TypedProofOperation::Return {
                point,
                value,
                conversion,
            } => {
                validate_typed_return_shape(
                    value.as_ref(),
                    conversion.as_ref(),
                    &callable.signature.result,
                    types,
                )?;
                if let Some(value) = value {
                    if !value.effects.is_empty() {
                        return Err(typed_proof_error(
                            VerifStatus::NotInTrustedSubset,
                            "return value has an unmodeled effect",
                        ));
                    }
                    discharge_typed_expr_definedness(value, body, types, &mut paths, session)?;
                }
                for path in &mut paths {
                    path.exit = Some(TypedProofPathExit::Return {
                        point: point.clone(),
                        value: value.clone(),
                        conversion: conversion.clone(),
                    });
                }
            }
            TypedProofOperation::Loop {
                point,
                condition,
                condition_calls,
                invariants,
                body: loop_body,
                receipt,
                ..
            } => {
                if !condition.effects.is_empty() || !condition_calls.is_empty() {
                    return Err(typed_proof_error(
                        VerifStatus::NotInTrustedSubset,
                        "loop condition calls/effects need a validated transfer receipt",
                    ));
                }
                if invariants.is_empty() {
                    return Err(typed_proof_error(
                        VerifStatus::NotInTrustedSubset,
                        "loop has no leading invariant to establish and preserve",
                    ));
                }
                let Some(receipt) = receipt.as_ref() else {
                    return Err(typed_proof_error(
                        VerifStatus::NotInTrustedSubset,
                        "loop worklist has no checker-owned induction receipt",
                    ));
                };
                validate_typed_loop_receipt(receipt, callable, point, scope, invariants)?;

                let mut loop_body_scope = scope.clone();
                loop_body_scope.push(crate::core::ir::TypedProofScopeFrame::Loop(point.clone()));
                loop_body_scope.push(crate::core::ir::TypedProofScopeFrame::Block(
                    loop_body.id.clone(),
                ));
                if invariants.iter().any(|invariant| {
                    invariant.id.owner != callable.owner
                        || invariant.scope.as_slice() != loop_body_scope.as_slice()
                        || !invariant.condition.effects.is_empty()
                }) {
                    return Err(typed_proof_error(
                        VerifStatus::NotInTrustedSubset,
                        "loop invariant owner, scope or effect census is invalid",
                    ));
                }

                let state_locals = receipt
                    .state_locals
                    .iter()
                    .map(|state| (state.local.clone(), state.ty.clone(), state.written_in_loop))
                    .collect::<Vec<_>>();
                let mut exit_paths = Vec::with_capacity(paths.len());
                for (preheader_index, mut preheader) in paths.drain(..).enumerate() {
                    // Establish each invariant on the actual preheader state.
                    // Only after these VCs pass may the formulas be used as
                    // induction hypotheses at a fresh arbitrary header.
                    for invariant in invariants {
                        check_typed_proof_condition(
                            &invariant.condition,
                            body,
                            types,
                            std::slice::from_mut(&mut preheader),
                            session,
                            "loop invariant establishment",
                        )?;
                    }

                    let mut header = fresh_loop_header_path(
                        &preheader,
                        &state_locals,
                        point,
                        preheader_index,
                        body,
                        types,
                    )?;
                    for invariant in invariants {
                        let formula = encode_typed_proof_bool(
                            &invariant.condition,
                            body,
                            types,
                            &mut header.vars,
                        )?;
                        header
                            .facts
                            .push(typed_proof_fact(formula, &invariant.condition, body));
                    }

                    discharge_typed_expr_definedness(
                        condition,
                        body,
                        types,
                        std::slice::from_mut(&mut header),
                        session,
                    )?;
                    let condition_value =
                        encode_typed_proof_bool(condition, body, types, &mut header.vars)?;

                    let mut body_path = header.clone();
                    body_path.facts.push(typed_proof_fact(
                        condition_value.clone(),
                        condition,
                        body,
                    ));
                    body_path.loop_contexts.push(TypedProofLoopContext {
                        loop_node: point.clone(),
                        guards: Vec::new(),
                        receipt: receipt.clone(),
                    });
                    let mut normal_exit = header;
                    normal_exit.facts.push(typed_proof_fact(
                        condition_value.not(),
                        condition,
                        body,
                    ));
                    exit_paths.push(normal_exit);

                    let mut body_scope = loop_body_scope.clone();
                    let body_outcomes = verify_typed_proof_block(
                        loop_body,
                        vec![body_path],
                        callable,
                        &mut body_scope,
                        body,
                        types,
                        session,
                    )?;
                    validate_loop_body_edge_coverage(
                        receipt,
                        &body_outcomes,
                        point,
                        &loop_body.id,
                    )?;

                    for mut outcome in body_outcomes {
                        let context_matches = outcome
                            .loop_contexts
                            .last()
                            .is_some_and(|context| context.loop_node == *point);
                        if !context_matches {
                            return Err(typed_proof_error(
                                VerifStatus::NotInTrustedSubset,
                                "loop body outcome lost its checker loop context",
                            ));
                        }
                        let backedge = match outcome.exit.as_ref() {
                            None => Some("fallthrough"),
                            Some(TypedProofPathExit::Continue { target_loop, .. })
                                if target_loop == point =>
                            {
                                Some("continue")
                            }
                            _ => None,
                        };
                        if let Some(edge_name) = backedge {
                            for invariant in invariants {
                                check_typed_proof_condition(
                                    &invariant.condition,
                                    body,
                                    types,
                                    std::slice::from_mut(&mut outcome),
                                    session,
                                    &format!("loop invariant preservation ({edge_name})"),
                                )?;
                            }
                            continue;
                        }

                        match outcome.exit.as_ref() {
                            Some(TypedProofPathExit::Break {
                                target_loop, value, ..
                            }) if target_loop == point => {
                                if value.is_some() {
                                    return Err(typed_proof_error(
                                        VerifStatus::NotInTrustedSubset,
                                        "value-carrying break needs a result-value receipt",
                                    ));
                                }
                                outcome.loop_contexts.pop();
                                outcome.exit = None;
                                exit_paths.push(outcome);
                            }
                            Some(TypedProofPathExit::Return { .. }) => {
                                outcome.loop_contexts.pop();
                                exit_paths.push(outcome);
                            }
                            _ => {
                                return Err(typed_proof_error(
                                    VerifStatus::NotInTrustedSubset,
                                    "loop body has an unmatched or outer-target control exit",
                                ));
                            }
                        }
                    }
                }
                paths = exit_paths;
            }
            TypedProofOperation::Exit => {
                return Err(typed_proof_error(
                    VerifStatus::NotInTrustedSubset,
                    "generic proof-worklist exit has no checker-owned return/break/continue edge identity",
                ));
            }
        }
        if paths.len() + terminal_paths.len() > 256 {
            return Err(typed_proof_error(
                VerifStatus::NotInTrustedSubset,
                "scoped proof worklist exceeds the bounded path count",
            ));
        }
    }
    paths.extend(terminal_paths);
    Ok(paths)
}

fn verify_typed_proof_worklist(
    worklist: &TypedProofPointWorklist,
    callable: &crate::core::ir::ResolvedCallable,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &Z3VarMap,
    session: &mut crate::verifier::ctx::SolverSession,
) -> Result<Vec<TypedProofPath>, (crate::verifier::ctx::VerifStatus, String)> {
    if worklist.owner != body.owner || worklist.owner != callable.owner {
        return Err(typed_proof_error(
            crate::verifier::ctx::VerifStatus::NotInTrustedSubset,
            "typed proof worklist owner disagrees with its checked body",
        ));
    }
    if worklist.root.id != body.root.node_id || !worklist.validate_against(callable) {
        return Err(typed_proof_error(
            crate::verifier::ctx::VerifStatus::NotInTrustedSubset,
            "typed proof worklist does not validate against the exact resolved callable",
        ));
    }
    if !worklist.boundaries.is_empty() {
        return Err(typed_proof_error(
            crate::verifier::ctx::VerifStatus::NotInTrustedSubset,
            "typed proof-point worklist contains an unresolved lexical/effect boundary",
        ));
    }
    let paths = vec![TypedProofPath {
        facts: Vec::new(),
        vars: vars.clone(),
        loop_contexts: Vec::new(),
        exit: None,
    }];
    let mut scope = vec![crate::core::ir::TypedProofScopeFrame::Block(
        body.root.node_id.clone(),
    )];
    verify_typed_proof_block(
        &worklist.root,
        paths,
        callable,
        &mut scope,
        body,
        types,
        session,
    )
}

fn bind_typed_proof_result(
    expression: &ResolvedExpr,
    conversion: Option<&crate::core::ir::CheckedConversion>,
    result_type: &crate::core::ir::ResolvedTypeId,
    result_key: &str,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    vars: &mut Z3VarMap,
) -> Result<(), (crate::verifier::ctx::VerifStatus, String)> {
    use crate::core::ir::{CheckedConversionKind, PrimitiveType, ResolvedType};
    use crate::verifier::ctx::VerifStatus;

    if expression.ty != *result_type
        || conversion.is_some_and(|conversion| {
            conversion.kind != CheckedConversionKind::Identity
                || conversion.from != expression.ty
                || conversion.to != *result_type
        })
    {
        return Err(typed_proof_error(
            VerifStatus::NotInTrustedSubset,
            "function result uses an unmodeled checked conversion",
        ));
    }

    match types.get(result_type) {
        Some(ResolvedType::Primitive(PrimitiveType::Bool)) => {
            let value = resolved_to_z3_bool(expression, body, types, vars).ok_or_else(|| {
                typed_proof_error(
                    VerifStatus::NotInTrustedSubset,
                    "boolean function result is outside the typed proof encoding",
                )
            })?;
            vars.insert_bool(result_key, value);
        }
        Some(ResolvedType::Primitive(PrimitiveType::I32 | PrimitiveType::I64)) => {
            let value = resolved_to_z3_int(expression, body, types, vars).ok_or_else(|| {
                typed_proof_error(
                    VerifStatus::NotInTrustedSubset,
                    "integer function result is outside the typed proof encoding",
                )
            })?;
            vars.insert_int(result_key, value);
        }
        _ => {
            return Err(typed_proof_error(
                VerifStatus::NotInTrustedSubset,
                "function result is outside the scalar bool/i32/i64 proof profile",
            ));
        }
    }
    Ok(())
}

/// Check Ensures against every actual normal return outcome.  A loop exit's
/// current-state locals and a return expression remain path-local; checking
/// against the callable-entry variable map would make mutable-loop
/// postconditions unsound.
fn verify_typed_postconditions(
    paths: &mut [TypedProofPath],
    ensures: &[&ResolvedExpr],
    body: &ResolvedBody,
    result_type: &crate::core::ir::ResolvedTypeId,
    types: &ResolvedTypeTable,
    session: &mut crate::verifier::ctx::SolverSession,
) -> Result<(), (crate::verifier::ctx::VerifStatus, String)> {
    use crate::verifier::ctx::VerifStatus;
    use z3::SatResult;

    if paths.is_empty() || ensures.is_empty() {
        return Ok(());
    }
    let result_key = contract_result_key(body);
    let needs_result = ensures_reference_result(ensures, body);

    for path in paths.iter_mut() {
        match path.exit.as_ref() {
            Some(TypedProofPathExit::Return {
                value: Some(value),
                conversion,
                ..
            }) if needs_result => bind_typed_proof_result(
                value,
                conversion.as_ref(),
                result_type,
                &result_key,
                body,
                types,
                &mut path.vars,
            )?,
            Some(TypedProofPathExit::Return { value: None, .. }) if needs_result => {
                return Err(typed_proof_error(
                    VerifStatus::NotInTrustedSubset,
                    "a result-dependent Ensures follows a valueless return",
                ));
            }
            Some(TypedProofPathExit::Return { .. }) => {}
            Some(TypedProofPathExit::Break { .. } | TypedProofPathExit::Continue { .. }) => {
                return Err(typed_proof_error(
                    VerifStatus::NotInTrustedSubset,
                    "a break/continue edge escaped its checked loop",
                ));
            }
            None if needs_result => {
                let Some(value) = body.root.result.as_ref() else {
                    return Err(typed_proof_error(
                        VerifStatus::NotInTrustedSubset,
                        "a result-dependent Ensures has no checked normal-return value",
                    ));
                };
                bind_typed_proof_result(
                    value,
                    None,
                    result_type,
                    &result_key,
                    body,
                    types,
                    &mut path.vars,
                )?;
            }
            None => {}
        }
    }

    for ensure in ensures {
        for path in paths.iter_mut() {
            let mut one_path = vec![path.clone()];
            discharge_typed_expr_definedness(ensure, body, types, &mut one_path, session)?;
            *path = one_path.pop().expect("one path remains after definedness");
            let condition = encode_typed_proof_bool(ensure, body, types, &mut path.vars)?;
            let path_condition = typed_proof_path_condition(path);
            match session
                .check_scope(Z3Bool::and(&[&path_condition, &condition.not()]))
                .0
            {
                SatResult::Unsat => {}
                SatResult::Sat => {
                    return Err(typed_proof_error(
                        VerifStatus::Disproven,
                        "Ensures is false on a reachable normal exit state",
                    ));
                }
                SatResult::Unknown => {
                    return Err(typed_proof_error(
                        session.unknown_status(),
                        "solver could not prove Ensures on a reachable normal exit state",
                    ));
                }
            }
        }
    }
    Ok(())
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
                // V-H4 + H2: constrain checked integer params/locals to
                // machine range so unbounded Z3 Int does not prove false
                // modular properties. H2 extends this from i32-only to i64 —
                // without the i64 range pin, `x == i64::MAX` is unreachable
                // to the solver and overflow obligations validate vacuously.
                if let Some((lo, hi)) = machine_integer_bounds(&local.ty, types) {
                    let lo = Z3Int::from_i64(lo);
                    let hi = Z3Int::from_i64(hi);
                    session.solver.assert(iv.ge(&lo));
                    session.solver.assert(iv.le(&hi));
                }
            }
        }
        // Create old snapshot variable
        // §11-#37: dot separator prevents namespace collision.
        let old_key = format!("old.{}", key);
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
fn contract_result_local(body: &ResolvedBody) -> Option<ResolvedLocalId> {
    body.locals
        .values()
        .find(|local| local.id.0 .0.ends_with("/contract-result/local"))
        .map(|local| local.id.clone())
}

fn contract_result_key(body: &ResolvedBody) -> String {
    contract_result_local(body)
        .and_then(|id| {
            body.locals
                .get(&id)
                .map(|local| local_var_key(&local.id, &local.display_name))
        })
        .unwrap_or_else(|| "result".to_string())
}

/// A nominal return value may be omitted only when every Ensures expression
/// is proven independent of the checker-created `result` local. Unknown or
/// nested forms conservatively count as a result use.
fn ensures_reference_result(ensures: &[&ResolvedExpr], body: &ResolvedBody) -> bool {
    let Some(result) = contract_result_local(body) else {
        return !ensures.is_empty();
    };
    ensures
        .iter()
        .any(|condition| resolved_expr_references_local(condition, &result))
}

fn resolved_expr_references_local(expression: &ResolvedExpr, local: &ResolvedLocalId) -> bool {
    use crate::core::ir::ResolvedExprKind as E;
    match &expression.kind {
        E::Load(place) => &place.base == local,
        E::Callable(crate::core::ir::ResolvedCallee::LocalClosure(id)) => id == local,
        E::Callable(_)
        | E::Literal(_)
        | E::Constant(_)
        | E::DefaultArgument { .. }
        | E::ComptimeValue(_)
        | E::TypeValue(_) => false,
        E::Project { value, projection } => {
            resolved_expr_references_local(value, local)
                || matches!(projection, crate::core::ir::ResolvedValueProjection::Index(index)
                    if resolved_expr_references_local(index, local))
        }
        E::Binary { left, right, .. }
        | E::Range {
            start: left,
            end: right,
        } => {
            resolved_expr_references_local(left, local)
                || resolved_expr_references_local(right, local)
        }
        E::Unary { operand, .. }
        | E::Old(operand)
        | E::TypeOf(operand)
        | E::Spawn(operand)
        | E::Await(operand) => resolved_expr_references_local(operand, local),
        E::Cast { value, .. } => resolved_expr_references_local(value, local),
        E::Call(call) => {
            matches!(&call.callee, crate::core::ir::ResolvedCallee::LocalClosure(id) if id == local)
                || call
                    .arguments
                    .iter()
                    .any(|arg| resolved_expr_references_local(&arg.value, local))
        }
        E::Tuple(values) | E::List(values) | E::Set(values) => values
            .iter()
            .any(|value| resolved_expr_references_local(value, local)),
        E::Map(entries) => entries.iter().any(|(key, value)| {
            resolved_expr_references_local(key, local)
                || resolved_expr_references_local(value, local)
        }),
        E::Record { fields, rest, .. } => {
            fields
                .iter()
                .any(|field| resolved_expr_references_local(&field.value, local))
                || rest
                    .as_deref()
                    .is_some_and(|value| resolved_expr_references_local(value, local))
        }
        E::If {
            condition,
            then_block,
            else_block,
        } => {
            resolved_expr_references_local(condition, local)
                || resolved_block_references_local(then_block, local)
                || resolved_block_references_local(else_block, local)
        }
        E::Block(block) | E::Scope { body: block, .. } | E::Comptime(block) | E::Quote(block) => {
            resolved_block_references_local(block, local)
        }
        E::Match { scrutinee, arms } => {
            resolved_expr_references_local(scrutinee, local)
                || arms.iter().any(|arm| {
                    arm.guard
                        .as_ref()
                        .is_some_and(|guard| resolved_expr_references_local(guard, local))
                        || resolved_expr_references_local(&arm.body, local)
                })
        }
        E::Try { value, .. } => resolved_expr_references_local(value, local),
        E::Slice { target, start, end } => {
            resolved_expr_references_local(target, local)
                || start
                    .as_deref()
                    .is_some_and(|value| resolved_expr_references_local(value, local))
                || end
                    .as_deref()
                    .is_some_and(|value| resolved_expr_references_local(value, local))
        }
        E::FString(parts) => parts.iter().any(|part| match part {
            crate::core::ir::ResolvedFStringPart::Text(_) => false,
            crate::core::ir::ResolvedFStringPart::Interpolation(value) => {
                resolved_expr_references_local(value, local)
            }
        }),
        E::Lambda(lambda) => {
            lambda.captures.contains(local) || resolved_block_references_local(&lambda.body, local)
        }
        E::Comprehension {
            value,
            iterable,
            guard,
            ..
        } => {
            resolved_expr_references_local(value, local)
                || resolved_expr_references_local(iterable, local)
                || guard
                    .as_deref()
                    .is_some_and(|guard| resolved_expr_references_local(guard, local))
        }
        E::OptionalChain { receiver, .. } => resolved_expr_references_local(receiver, local),
    }
}

/// Classify only dependencies on the current callable state. `old(x)` reads
/// the immutable entry snapshot and must survive an induction cut even when
/// the current value of `x` is loop-carried.
fn resolved_expr_references_current_local(
    expression: &ResolvedExpr,
    local: &ResolvedLocalId,
) -> bool {
    use crate::core::ir::ResolvedExprKind as E;
    match &expression.kind {
        E::Old(_) => false,
        E::Load(place) => &place.base == local,
        E::Callable(crate::core::ir::ResolvedCallee::LocalClosure(id)) => id == local,
        E::Callable(_)
        | E::Literal(_)
        | E::Constant(_)
        | E::DefaultArgument { .. }
        | E::ComptimeValue(_)
        | E::TypeValue(_) => false,
        E::Project { value, projection } => {
            resolved_expr_references_current_local(value, local)
                || matches!(projection, crate::core::ir::ResolvedValueProjection::Index(index)
                    if resolved_expr_references_current_local(index, local))
        }
        E::Binary { left, right, .. }
        | E::Range {
            start: left,
            end: right,
        } => {
            resolved_expr_references_current_local(left, local)
                || resolved_expr_references_current_local(right, local)
        }
        E::Unary { operand, .. }
        | E::TypeOf(operand)
        | E::Spawn(operand)
        | E::Await(operand)
        | E::Cast { value: operand, .. }
        | E::Try { value: operand, .. } => resolved_expr_references_current_local(operand, local),
        E::Call(call) => {
            matches!(&call.callee, crate::core::ir::ResolvedCallee::LocalClosure(id) if id == local)
                || call
                    .arguments
                    .iter()
                    .any(|arg| resolved_expr_references_current_local(&arg.value, local))
        }
        E::Tuple(values) | E::List(values) | E::Set(values) => values
            .iter()
            .any(|value| resolved_expr_references_current_local(value, local)),
        E::Map(entries) => entries.iter().any(|(key, value)| {
            resolved_expr_references_current_local(key, local)
                || resolved_expr_references_current_local(value, local)
        }),
        E::Record { fields, rest, .. } => {
            fields
                .iter()
                .any(|field| resolved_expr_references_current_local(&field.value, local))
                || rest
                    .as_deref()
                    .is_some_and(|value| resolved_expr_references_current_local(value, local))
        }
        E::If {
            condition,
            then_block,
            else_block,
        } => {
            resolved_expr_references_current_local(condition, local)
                || resolved_block_references_current_local(then_block, local)
                || resolved_block_references_current_local(else_block, local)
        }
        E::Block(block) | E::Scope { body: block, .. } | E::Comptime(block) | E::Quote(block) => {
            resolved_block_references_current_local(block, local)
        }
        E::Match { scrutinee, arms } => {
            resolved_expr_references_current_local(scrutinee, local)
                || arms.iter().any(|arm| {
                    arm.guard
                        .as_ref()
                        .is_some_and(|guard| resolved_expr_references_current_local(guard, local))
                        || resolved_expr_references_current_local(&arm.body, local)
                })
        }
        E::Slice { target, start, end } => {
            resolved_expr_references_current_local(target, local)
                || start
                    .as_deref()
                    .is_some_and(|value| resolved_expr_references_current_local(value, local))
                || end
                    .as_deref()
                    .is_some_and(|value| resolved_expr_references_current_local(value, local))
        }
        E::FString(parts) => parts.iter().any(|part| match part {
            crate::core::ir::ResolvedFStringPart::Text(_) => false,
            crate::core::ir::ResolvedFStringPart::Interpolation(value) => {
                resolved_expr_references_current_local(value, local)
            }
        }),
        E::Lambda(lambda) => {
            lambda.captures.contains(local)
                || resolved_block_references_current_local(&lambda.body, local)
        }
        E::Comprehension {
            value,
            iterable,
            guard,
            ..
        } => {
            resolved_expr_references_current_local(value, local)
                || resolved_expr_references_current_local(iterable, local)
                || guard
                    .as_deref()
                    .is_some_and(|guard| resolved_expr_references_current_local(guard, local))
        }
        E::OptionalChain { receiver, .. } => {
            resolved_expr_references_current_local(receiver, local)
        }
    }
}

fn resolved_block_references_current_local(block: &ResolvedBlock, local: &ResolvedLocalId) -> bool {
    use crate::core::ir::ResolvedStmtKind as S;
    block
        .statements
        .iter()
        .any(|statement| match &statement.kind {
            S::Bind { initializer, .. } => initializer
                .as_ref()
                .is_some_and(|value| resolved_expr_references_current_local(value, local)),
            S::Assign { target, value, .. } => {
                &target.base == local || resolved_expr_references_current_local(value, local)
            }
            S::Return { value, .. } | S::Break(value) => value
                .as_ref()
                .is_some_and(|value| resolved_expr_references_current_local(value, local)),
            S::Continue | S::NestedCallable(_) => false,
            S::Expr(value) => resolved_expr_references_current_local(value, local),
            S::Math(values) => values
                .iter()
                .any(|value| resolved_expr_references_current_local(value, local)),
            S::Contract { condition, .. } => {
                resolved_expr_references_current_local(condition, local)
            }
            S::While { condition, body } => {
                resolved_expr_references_current_local(condition, local)
                    || resolved_block_references_current_local(body, local)
            }
            S::WhileLet {
                initializer, body, ..
            }
            | S::For {
                iterable: initializer,
                body,
                ..
            } => {
                resolved_expr_references_current_local(initializer, local)
                    || resolved_block_references_current_local(body, local)
            }
            S::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                resolved_expr_references_current_local(initializer, local)
                    || resolved_block_references_current_local(then_block, local)
                    || else_block
                        .as_ref()
                        .is_some_and(|body| resolved_block_references_current_local(body, local))
            }
            S::Loop(body) | S::Scope { body, .. } => {
                resolved_block_references_current_local(body, local)
            }
            S::Drop(places) => places.iter().any(|place| &place.base == local),
            S::Pinned { value, body, .. } => {
                resolved_expr_references_current_local(value, local)
                    || resolved_block_references_current_local(body, local)
            }
        })
        || block
            .result
            .as_deref()
            .is_some_and(|value| resolved_expr_references_current_local(value, local))
}

fn resolved_block_references_local(block: &ResolvedBlock, local: &ResolvedLocalId) -> bool {
    use crate::core::ir::ResolvedStmtKind as S;
    block
        .statements
        .iter()
        .any(|statement| match &statement.kind {
            S::Bind { initializer, .. } => initializer
                .as_ref()
                .is_some_and(|value| resolved_expr_references_local(value, local)),
            S::Assign { target, value, .. } => {
                &target.base == local || resolved_expr_references_local(value, local)
            }
            S::Return { value, .. } | S::Break(value) => value
                .as_ref()
                .is_some_and(|value| resolved_expr_references_local(value, local)),
            S::Continue | S::NestedCallable(_) => false,
            S::Expr(value) => resolved_expr_references_local(value, local),
            S::Math(values) => values
                .iter()
                .any(|value| resolved_expr_references_local(value, local)),
            S::Contract { condition, .. } => resolved_expr_references_local(condition, local),
            S::While { condition, body } => {
                resolved_expr_references_local(condition, local)
                    || resolved_block_references_local(body, local)
            }
            S::WhileLet {
                initializer, body, ..
            }
            | S::For {
                iterable: initializer,
                body,
                ..
            } => {
                resolved_expr_references_local(initializer, local)
                    || resolved_block_references_local(body, local)
            }
            S::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                resolved_expr_references_local(initializer, local)
                    || resolved_block_references_local(then_block, local)
                    || else_block
                        .as_ref()
                        .is_some_and(|body| resolved_block_references_local(body, local))
            }
            S::Loop(body) | S::Scope { body, .. } => resolved_block_references_local(body, local),
            S::Drop(places) => places.iter().any(|place| &place.base == local),
            S::Pinned { value, body, .. } => {
                resolved_expr_references_local(value, local)
                    || resolved_block_references_local(body, local)
            }
        })
        || block
            .result
            .as_deref()
            .is_some_and(|value| resolved_expr_references_local(value, local))
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
/// - Direct pure call-site requires and body-definedness VCs: ✅ (catalog-aware,
///   non-generic scalar actuals, ordered Requires, bounded pure-call subset)
/// - Flow transition source invariants: ✅ (checker-owned entry assumptions;
///   definedness is discharged before the invariant is admitted)
/// - Ordinary callable invariant checking: ❌ (deferred)
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
#[cfg(test)]
pub(crate) fn verify_contracts_from_resolved(
    callable: &crate::core::ir::ResolvedCallable,
    types: &crate::core::ir::ResolvedTypeTable,
    session: &mut crate::verifier::ctx::SolverSession,
) -> Option<(crate::verifier::ctx::VerifStatus, String)> {
    let mut context = ResolvedDefinednessContext::without_catalog();
    context.active_calls.insert(callable.owner.clone());
    verify_contracts_from_resolved_in_context(callable, types, false, &mut context, session)
}

pub(crate) fn verify_contracts_from_resolved_with_flow_state_catalog(
    callable: &crate::core::ir::ResolvedCallable,
    types: &crate::core::ir::ResolvedTypeTable,
    callables: &BTreeMap<crate::core::NodeId, crate::core::ResolvedCallable>,
    variants: &BTreeMap<crate::core::NodeId, crate::core::ResolvedVariantSchema>,
    flow_state_fields: Option<&ResolvedFlowStateFieldCatalog>,
    is_flow_transition: bool,
    session: &mut crate::verifier::ctx::SolverSession,
) -> Option<(crate::verifier::ctx::VerifStatus, String)> {
    let mut context =
        ResolvedDefinednessContext::with_flow_state_catalog(callables, variants, flow_state_fields);
    context.active_calls.insert(callable.owner.clone());
    verify_contracts_from_resolved_in_context(
        callable,
        types,
        is_flow_transition,
        &mut context,
        session,
    )
}

fn verify_contracts_from_resolved_in_context(
    callable: &crate::core::ir::ResolvedCallable,
    types: &crate::core::ir::ResolvedTypeTable,
    is_flow_transition: bool,
    context: &mut ResolvedDefinednessContext<'_>,
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

    // Projected scalar machine ranges are type premises.  Assert them after
    // creating the callable-entry state and before any root Requires clause
    // can be admitted as a solver assumption.
    if let Err(detail) = assert_projected_integer_range_facts(body, types, &mut vars, session) {
        return Some(status_msg(
            VerifStatus::NotInTrustedSubset,
            &format!("inconsistent projected integer type premises: {detail}"),
        ));
    }

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
            // V-H4 + H2: constrain checked integer result to machine
            // range (same rationale as the local pin above — i64 was modeled
            // unbounded, so overflow-adjacent postconditions could verify
            // vacuously).
            if let Some((lo, hi)) = machine_integer_bounds(&callable.signature.result, types) {
                let lo = Z3Int::from_i64(lo);
                let hi = Z3Int::from_i64(hi);
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
        let old_key = format!("old.{}", key);
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
    let mut invariants = Vec::new();
    let mut requires = Vec::new();
    let mut ensures = Vec::new();
    let root_contracts = body
        .root
        .statements
        .iter()
        .filter_map(|statement| {
            matches!(&statement.kind, ResolvedStmtKind::Contract { .. })
                .then_some(statement.node_id.clone())
        })
        .collect::<std::collections::BTreeSet<_>>();
    for contract in &callable.contracts {
        if !root_contracts.contains(&contract.node_id) {
            continue;
        }
        match contract.kind {
            ContractKind::Requires => requires.push(&contract.condition),
            ContractKind::Ensures => ensures.push(&contract.condition),
            ContractKind::Invariant if is_flow_transition => invariants.push(&contract.condition),
            ContractKind::Invariant => {} // ordinary callable invariant proof is deferred
        }
    }

    let proof_worklist = callable.proof_point_worklist();
    if !proof_worklist.boundaries.is_empty() {
        return Some(status_msg(
            VerifStatus::NotInTrustedSubset,
            "typed proof-point worklist contains an unresolved lexical/effect boundary",
        ));
    }
    let has_proof_points =
        !proof_worklist.points.is_empty() || !proof_worklist.loop_invariant_points().is_empty();
    let has_direct_function_call = context.callables.is_some()
        && body
            .resolved_calls
            .values()
            .any(|callee| matches!(callee, crate::core::ir::ResolvedCallee::Function(_)));
    if invariants.is_empty()
        && requires.is_empty()
        && ensures.is_empty()
        && !has_proof_points
        && !has_direct_function_call
    {
        return None; // No contracts, proof points, or direct-call obligations to verify
    }

    // 4b. H-21 (audit 2026-08-05): f64 contracts FAIL CLOSED. The Resolved
    //     engine encoded f64 as exact Z3 Reals with no IEEE 754 rounding or
    //     NaN modeling, proving fake contracts such as float reassociation.
    //     The Resolved IR is fully typed, so detection is exact: any f64-typed
    //     sub-expression in the contracts or the body rejects the callable.
    //     (Full IEEE modeling is deliberately not attempted — conservative.)
    let f64_involved = invariants
        .iter()
        .chain(requires.iter())
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

    // 5. A transition's own invariant describes valid source states.  Prove
    // its expression is defined before assuming its truth, then admit that
    // checker-owned fact ahead of the transition Requires clauses. Ordinary
    // callable invariants remain deferred until their program-point model is
    // available.
    for invariant in &invariants {
        if let Err((status, detail)) = discharge_root_contract_definedness(
            invariant,
            ContractKind::Invariant,
            body,
            types,
            &mut vars,
            context,
            session,
        ) {
            return Some(status_msg(status, &detail));
        }
        if let Some(z3_bool) =
            resolved_to_z3_bool_in_context(invariant, body, types, &mut vars, context)
        {
            session.solver.assert(z3_bool);
        } else {
            return Some(status_msg(
                VerifStatus::NotInTrustedSubset,
                "Flow transition invariant is outside the scalar contract encoding",
            ));
        }
    }

    // 5. Check Requires evaluation before admitting that clause as an
    //    assumption.  Earlier requirements have already been checked and may
    //    constrain later ones; a requirement never gets to prove its own
    //    definedness by being asserted first.
    let mut encoding_failures = 0;
    for req in &requires {
        if let Err((status, detail)) = discharge_root_contract_definedness(
            req,
            ContractKind::Requires,
            body,
            types,
            &mut vars,
            context,
            session,
        ) {
            return Some(status_msg(status, &detail));
        }
        if let Some(z3_bool) = resolved_to_z3_bool_in_context(req, body, types, &mut vars, context)
        {
            session.solver.assert(z3_bool);
        } else {
            return Some(status_msg(
                VerifStatus::NotInTrustedSubset,
                "root Requires clause is outside the scalar contract encoding",
            ));
        }
    }

    // Preconditions describe admissible inputs; an inconsistent conjunction
    // cannot make the function's implementation contract vacuously Proven.
    // Check consistency before body definedness or Ensures can use them.
    match session.check_scope(Z3Bool::from_bool(true)).0 {
        SatResult::Sat => {}
        SatResult::Unsat => {
            return Some(status_msg(
                VerifStatus::Disproven,
                "preconditions are unsatisfiable",
            ));
        }
        SatResult::Unknown => {
            return Some(status_msg(
                VerifStatus::SolverUnknown,
                "solver could not establish precondition satisfiability",
            ));
        }
    }

    // 5b. Verify ordered Math and loop-header proof points from the typed
    // worklist. The worklist preserves lexical scope; branch/loop lemmas are
    // asserted only inside the corresponding solver scope and cannot leak to
    // sibling arms or after a possibly-zero-iteration loop.
    // Direct scalar call-site obligations are discharged by the catalog-aware
    // body/definedness encoder below.  The typed worklist is needed only when
    // this callable owns an actual Math/loop proof point; replaying a
    // census-only Call for a contract-free helper or entry branch would turn
    // an otherwise supported call into the old "return-value ownership"
    // boundary.
    let mut proof_paths = if has_proof_points {
        match verify_typed_proof_worklist(&proof_worklist, callable, body, types, &vars, session) {
            Ok(paths) => paths,
            Err((status, detail)) => return Some(status_msg(status, &detail)),
        }
    } else {
        Vec::new()
    };

    if has_proof_points {
        if encoding_failures > 0 {
            return Some(status_msg(VerifStatus::NotInTrustedSubset, ""));
        }
        if let Err((status, detail)) = verify_typed_postconditions(
            &mut proof_paths,
            &ensures,
            body,
            &callable.signature.result,
            types,
            session,
        ) {
            return Some(status_msg(status, &detail));
        }
        // The scoped executor has already discharged every reachable body
        // expression at its actual typed program point and checked each
        // callable Ensures against the corresponding return/loop-exit map.
        // The old callable-wide scan uses entry bindings and cannot model
        // mutable loop updates.
        return Some(status_msg(VerifStatus::Proven, ""));
    }

    // 5c. H-24 (audit 2026-08-05): i32 definedness VCs. Checked i32
    //     arithmetic traps on overflow / zero divisor / MIN ÷ -1 (SD-7/SD-8);
    //     a postcondition may not be proved under "assume no trap". Mirror
    //     the AST engine: collect obligations in evaluation order, discharge
    //     each under the preconditions, and only then bind the body result.
    let stationary_loops = stationary_loop_receipt_nodes(&proof_worklist.root);
    let mut obligations = Vec::new();
    if collect_block_i32_definedness_scoped(
        &body.root,
        body,
        types,
        &mut vars,
        &mut obligations,
        &stationary_loops,
        context,
    )
    .is_none()
    {
        return Some(status_msg(
            VerifStatus::NotInTrustedSubset,
            "could not encode i32 definedness obligations (fail-closed)",
        ));
    }
    let had_definedness_obligations = obligations
        .iter()
        .any(|obligation| !obligation.is_call_precondition);
    let had_call_precondition_obligations = obligations
        .iter()
        .any(|obligation| obligation.is_call_precondition);
    context.assert_type_premises(session);
    for obligation in obligations {
        let (result, _model) = session.check_scope(obligation.condition.not());
        match result {
            SatResult::Unsat => {
                // Defined under the preconditions — admit for the rest of the proof.
                session.solver.assert(obligation.condition);
            }
            SatResult::Sat => {
                let status = if obligation.modeling_gap {
                    VerifStatus::NotInTrustedSubset
                } else {
                    VerifStatus::Disproven
                };
                return Some(status_msg(status, obligation.failure));
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
    let mut body_encoded = !ensures_reference_result(&ensures, body);
    if !body_encoded && body.root.result.is_some() {
        let encoded = match result_category {
            Z3TypeCategory::Int => resolved_block_tail_int_in_context(
                &body.root, body, types, &mut vars, context,
            )
            .map(|body_z3| {
                if let Some(rv) = vars.get_int(&result_key).cloned() {
                    session.solver.assert(rv.eq(&body_z3));
                }
            }),
            Z3TypeCategory::Real => {
                resolved_block_tail_real_in_context(&body.root, body, types, &mut vars, context)
                    .map(|body_z3| {
                        if let Some(rv) = vars.get_real(&result_key).cloned() {
                            session.solver.assert(rv.eq(&body_z3));
                        }
                    })
            }
            Z3TypeCategory::Bool => {
                resolved_block_tail_bool_in_context(&body.root, body, types, &mut vars, context)
                    .map(|body_z3| {
                        if let Some(rv) = vars.get_bool(&result_key).cloned() {
                            session.solver.assert(rv.eq(&body_z3));
                        }
                    })
            }
        };
        body_encoded = encoded.is_some();
    }
    if !body_encoded
        && body.root.result.is_none()
        && matches!(
            types.get(&callable.signature.result),
            Some(ResolvedType::Primitive(
                PrimitiveType::I32 | PrimitiveType::I64 | PrimitiveType::Bool
            ))
        )
    {
        if let Some((return_value, prefix)) =
            terminal_explicit_scalar_return(&body.root, &callable.signature.result)
        {
            if resolved_prepare_context_value_block_stmts(&prefix, body, types, &mut vars, context)
                .is_some()
            {
                let returned_value = match result_category {
                    Z3TypeCategory::Int => {
                        resolved_to_z3_int_in_context(return_value, body, types, &mut vars, context)
                            .map(|value| {
                                if let Some(result) = vars.get_int(&result_key).cloned() {
                                    session.solver.assert(result.eq(&value));
                                    true
                                } else {
                                    false
                                }
                            })
                    }
                    Z3TypeCategory::Real => resolved_to_z3_real_in_context(
                        return_value,
                        body,
                        types,
                        &mut vars,
                        context,
                    )
                    .map(|value| {
                        if let Some(result) = vars.get_real(&result_key).cloned() {
                            session.solver.assert(result.eq(&value));
                            true
                        } else {
                            false
                        }
                    }),
                    Z3TypeCategory::Bool => resolved_to_z3_bool_in_context(
                        return_value,
                        body,
                        types,
                        &mut vars,
                        context,
                    )
                    .map(|value| {
                        if let Some(result) = vars.get_bool(&result_key).cloned() {
                            session.solver.assert(result.eq(&value));
                            true
                        } else {
                            false
                        }
                    }),
                };
                body_encoded = returned_value == Some(true);
            }
        }
    }
    if !body_encoded
        && !matches!(
            types.get(&callable.signature.result),
            Some(ResolvedType::Primitive(_))
        )
    {
        if let (Some(value), Some(callables), Some(variants)) = (
            body.root.result.as_ref(),
            context.callables,
            context.variants,
        ) {
            body_encoded =
                crate::verifier::resolved_expr::resolved_bind_aggregate_result_with_catalog(
                    value, body, &ensures, types, callables, variants, &mut vars, session,
                )
                .is_some();
        }
    }

    // 7. Check each ensures independently
    if ensures.is_empty() {
        // If we had math obligations that were all proven, this is a success,
        // not NoObligations — the math was a verification condition.
        if encoding_failures > 0 {
            return Some(status_msg(VerifStatus::NotInTrustedSubset, ""));
        }
        if has_proof_points {
            return Some(status_msg(VerifStatus::Proven, ""));
        }
        if !requires.is_empty() {
            return Some(status_msg(VerifStatus::Proven, ""));
        }
        let is_entry_owner = callable.owner.0 == "function:main";
        if had_definedness_obligations && (!is_entry_owner || !had_call_precondition_obligations) {
            // A source-contract-free callable with only definedness VCs (or a
            // non-entry helper whose callee definedness was checked internally)
            // matches canonical MIR by omitting a separate public obligation result.
            return None;
        }
        if had_call_precondition_obligations {
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
        // Ensures are evaluated only after a normal body return.  At this
        // point the body-result relation (when needed) and body definedness
        // facts are available, but no Ensures formula has been assumed.
        if let Err((status, detail)) = discharge_root_contract_definedness(
            ens,
            ContractKind::Ensures,
            body,
            types,
            &mut vars,
            context,
            session,
        ) {
            return Some(status_msg(status, &detail));
        }
        if let Some(z3_bool) = resolved_to_z3_bool_in_context(ens, body, types, &mut vars, context)
        {
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
            resolved_calls: BTreeMap::new(),
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
    fn projected_place_keys_match_explicit_field_and_tuple_projects() {
        let (_types, ty) = test_table_and_ty();
        let body = test_body_with_local("aggregate");
        let base = body.parameters[0].clone();
        let field = NodeId("type:Counter/node:decl.field@count".into());

        let projected_field = test_expr(ResolvedExprKind::Load(ResolvedPlace {
            base: base.clone(),
            projections: vec![ResolvedProjection::Field {
                field: field.clone(),
                name: "count".into(),
                ty: ty.clone(),
            }],
        }));
        let explicit_field = test_expr(ResolvedExprKind::Project {
            value: Box::new(test_expr(ResolvedExprKind::Load(ResolvedPlace::root(
                base.clone(),
            )))),
            projection: ResolvedValueProjection::Field(field),
        });
        assert_eq!(
            local_key(
                match &projected_field.kind {
                    ResolvedExprKind::Load(place) => place,
                    _ => unreachable!(),
                },
                &body,
                &projected_field.ty,
            ),
            resolved_field_var_name(&explicit_field, &body),
            "Field place and explicit Project encodings use the same semantic field key"
        );

        let projected_tuple = ResolvedPlace {
            base: base.clone(),
            projections: vec![ResolvedProjection::Tuple {
                index: 0,
                ty: ty.clone(),
            }],
        };
        let tuple_load = test_expr(ResolvedExprKind::Load(projected_tuple));
        let explicit_tuple = test_expr(ResolvedExprKind::Project {
            value: Box::new(test_expr(ResolvedExprKind::Load(ResolvedPlace::root(
                base.clone(),
            )))),
            projection: ResolvedValueProjection::Tuple(0),
        });
        assert_eq!(
            local_key(
                match &tuple_load.kind {
                    ResolvedExprKind::Load(place) => place,
                    _ => unreachable!(),
                },
                &body,
                &tuple_load.ty,
            ),
            resolved_field_var_name(&explicit_tuple, &body),
            "Tuple places use the same stable tuple-index notation as Project(Tuple)"
        );
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
            format!("old.{}", local_var_key(&local_id, "x")),
            Z3Int::new_const("old.x#local:x"),
        );
        let inner = test_expr(ResolvedExprKind::Load(ResolvedPlace::root(local_id)));
        let expr = test_expr(ResolvedExprKind::Old(Box::new(inner)));
        let result = resolved_to_z3_int(&expr, &body, &table, &mut vars);
        assert!(result.is_some(), "old(x) must resolve to old_x variable");
    }

    #[test]
    fn loop_induction_cut_discards_only_facts_about_carried_locals() {
        let carried = ResolvedLocalId(NodeId("local:carried".into()));
        let stable = ResolvedLocalId(NodeId("local:stable".into()));
        let mut path = TypedProofPath::default();
        path.facts.push(TypedProofFact {
            condition: Z3Bool::from_bool(true),
            current_locals: BTreeSet::from([carried.clone()]),
        });
        path.facts.push(TypedProofFact {
            condition: Z3Bool::from_bool(true),
            current_locals: BTreeSet::from([stable.clone()]),
        });

        discard_loop_carried_facts(&mut path, &BTreeSet::from([carried]));

        assert_eq!(path.facts.len(), 1);
        assert_eq!(path.facts[0].current_locals, BTreeSet::from([stable]));
    }

    #[test]
    fn loop_fact_dependencies_keep_old_separate_from_current_locals() {
        let body = test_body_with_local("x");
        let local = body.parameters[0].clone();
        let load = test_expr(ResolvedExprKind::Load(ResolvedPlace::root(local.clone())));
        let old = test_expr(ResolvedExprKind::Old(Box::new(load.clone())));
        assert!(!resolved_expr_references_current_local(&old, &local));

        let mixed = test_expr(ResolvedExprKind::Binary {
            op: ResolvedBinaryOp::Add,
            left: Box::new(load),
            right: Box::new(old),
        });
        assert!(resolved_expr_references_current_local(&mixed, &local));
    }

    #[test]
    fn typed_proof_bind_rejects_initializer_local_type_mismatch() {
        let (mut types, _) = test_table_and_ty();
        let bool_ty = types
            .intern_zonked(
                &crate::core::phase::ZonkedTy::from_resolved(crate::ast::Type::Name(
                    "bool".into(),
                    Vec::new(),
                ))
                .unwrap(),
                &Default::default(),
                crate::core::ir::ResolvedTypeName::primitive,
            )
            .unwrap();
        let mut body = test_body_with_local("x");
        let local = body.parameters[0].clone();
        body.locals.get_mut(&local).unwrap().ty = bool_ty;

        let initializer = test_expr(ResolvedExprKind::Literal(ResolvedLiteral::Int(1)));
        let error =
            bind_typed_proof_local(&local, &initializer, &body, &types, &mut Z3VarMap::new())
                .expect_err(
                    "a differently typed initializer must not populate this local's Z3 key",
                );
        assert_eq!(
            error.0,
            crate::verifier::ctx::VerifStatus::NotInTrustedSubset
        );
    }

    #[test]
    fn loop_header_refreshes_read_only_state_with_a_preheader_frame() {
        use z3::SatResult;

        let (types, ty) = test_table_and_ty();
        let body = test_body_with_local("stable");
        let local = body.parameters[0].clone();
        let local_key = local_var_key(&local, "stable");
        let entry_value = Z3Int::new_const("loop.test.entry.stable");
        let lower = entry_value.ge(&Z3Int::from_i64(i32::MIN as i64));
        let upper = entry_value.le(&Z3Int::from_i64(i32::MAX as i64));
        let positive = entry_value.gt(&Z3Int::from_i64(0));
        let mut preheader = TypedProofPath::default();
        preheader
            .vars
            .insert_int(local_key.clone(), entry_value.clone());
        preheader.facts.extend([
            TypedProofFact {
                condition: lower,
                current_locals: BTreeSet::from([local.clone()]),
            },
            TypedProofFact {
                condition: upper,
                current_locals: BTreeSet::from([local.clone()]),
            },
            TypedProofFact {
                condition: positive,
                current_locals: BTreeSet::from([local.clone()]),
            },
        ]);

        let header = fresh_loop_header_path(
            &preheader,
            &[(local.clone(), ty, false)],
            &NodeId("loop:test".into()),
            0,
            &body,
            &types,
        )
        .expect("read-only loop state has a fresh framed value");
        let current = header
            .vars
            .get_int(&local_key)
            .expect("fresh header current value");
        assert_ne!(
            current.to_string(),
            entry_value.to_string(),
            "the loop header must not reuse the preheader Z3 symbol"
        );
        let path_condition = typed_proof_path_condition(&header);
        let frame_violation = Z3Bool::and(&[&path_condition, &current.eq(&entry_value).not()]);
        let fact_violation = Z3Bool::and(&[&path_condition, &current.le(&Z3Int::from_i64(0))]);
        let mut solver = crate::verifier::ctx::SolverSession::new(1000).expect("solver");
        assert_eq!(solver.check_scope(frame_violation).0, SatResult::Unsat);
        assert_eq!(solver.check_scope(fact_violation).0, SatResult::Unsat);
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
    fn e2e_resolved_flow_state_record_match_preserves_bool_result_context() {
        let source = r#"
flow F {
    state A { n: i32 }
    state B { n: i32 }

    transition go(A) -> B fails string {
        let checked: Result<i32, string> = if self.n == 0 {
            Err("bad")
        } else {
            Ok(self.n + 1)
        }
        let next = checked?
        return B { n: next }
    }
}

func reports_failure() -> bool {
    requires: true
    ensures: result == false
    let outcome = F::go(A { n: 0 })
    match outcome {
        Ok(B { n }) => n > 0,
        Err((source, error)) => {
            drop(source)
            drop(error)
            false
        },
    }
}

func reports_the_wrong_result() -> bool {
    requires: true
    ensures: result == true
    let outcome = F::go(A { n: 0 })
    match outcome {
        Ok(B { n }) => n > 0,
        Err((source, error)) => {
            drop(source)
            drop(error)
            false
        },
    }
}

func main() -> i32 { 0 }
"#;
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let flow_state_fields =
            resolved_flow_state_field_catalog(&program).expect("canonical Flow state fields");

        for (name, expected) in [
            ("reports_failure", crate::verifier::ctx::VerifStatus::Proven),
            (
                "reports_the_wrong_result",
                crate::verifier::ctx::VerifStatus::Disproven,
            ),
        ] {
            let owner = NodeId(format!("function:{name}"));
            let callable = program.callables().get(&owner).expect("callable");
            let mut session = crate::verifier::ctx::SolverSession::new(5000).expect("solver");
            let (actual, detail) = verify_contracts_from_resolved_with_flow_state_catalog(
                callable,
                program.resolved_types(),
                program.callables(),
                program.resolved_variants(),
                Some(&flow_state_fields),
                false,
                &mut session,
            )
            .expect("verification result");
            assert_eq!(actual, expected, "{name}: {detail}");
        }

        let callable = program
            .callables()
            .get(&NodeId("function:reports_failure".into()))
            .expect("callable");
        let mut session = crate::verifier::ctx::SolverSession::new(5000).expect("solver");
        let (actual, detail) = verify_contracts_from_resolved_with_flow_state_catalog(
            callable,
            program.resolved_types(),
            program.callables(),
            program.resolved_variants(),
            None,
            false,
            &mut session,
        )
        .expect("fail-closed result");
        assert_eq!(
            actual,
            crate::verifier::ctx::VerifStatus::NotInTrustedSubset,
            "a state-shaped pattern without checker-owned field metadata must fail closed: {detail}"
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

    #[test]
    fn e2e_resolved_indexed_proof_expressions_fail_closed() {
        // `xs[0] == xs[0]` was encoded as a free `xs.idx` variable and proved
        // tautologically, without proving that the index operation is in
        // bounds. Both contract roots and ordered Math points must reject it.
        let source = r#"
func indexed_requirement(xs: List<i32>) -> i32 {
    requires: xs[0] == xs[0]
    0
}
func indexed_math(xs: List<i32>) -> i32 {
    requires: true
    math: { xs[0] == xs[0] }
    0
}
func main() -> i32 { 0 }
"#;
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");

        for name in ["indexed_requirement", "indexed_math"] {
            let owner = NodeId(format!("function:{name}"));
            let callable = program.callables().get(&owner).expect("callable");
            let mut session = crate::verifier::ctx::SolverSession::new(5000).expect("solver");
            let status =
                verify_contracts_from_resolved(callable, program.resolved_types(), &mut session)
                    .map(|(status, _)| status);
            assert_eq!(
                status,
                Some(crate::verifier::ctx::VerifStatus::NotInTrustedSubset),
                "indexed proof expression in {name} must not be Proven"
            );
        }
    }

    #[test]
    fn e2e_resolved_root_contract_definedness_obeys_short_circuit() {
        // Root contracts are evaluated Mimi expressions, not merely total
        // Z3 formulas.  In particular a Requires clause cannot assume itself
        // to make a conditionally executed division defined, and an Ensures
        // clause cannot let its boolean result hide a trapping operand.  The
        // final case is a defined, short-circuited positive control.  All
        // three functions return a nominal value so their Ensures are
        // independent of `result` and exercise the body-result elision path.
        let source = r#"
type Box { n: i32 }

func bad_requires(x: i32) -> Box {
    requires: x != 0 or 1 / x == 1
    ensures: true
    Box { n: 0 }
}

func bad_ensures_left(x: i32) -> Box {
    requires: x == 0
    ensures: 1 / x == 0 or x == 0
    Box { n: 0 }
}

func bad_ensures_right(x: i32) -> Box {
    requires: x == 0
    ensures: x != 0 or 1 / x == 0
    Box { n: 0 }
}

func bad_body_power(x: i32) -> Box {
    requires: true
    ensures: true
    let value = 2 ** x
    Box { n: value }
}

func unencodable_body_cast(x: i32) -> Box {
    requires: true
    ensures: true
    if (x as i64) > 0 {
        Box { n: 1 }
    } else {
        Box { n: 0 }
    }
}

func safe_short_circuit(x: i32) -> Box {
    requires: true
    ensures: x == 0 or (x != 0 and x / x == 1)
    Box { n: 0 }
}

func main() -> i32 { 0 }
"#;
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");

        for name in ["bad_requires", "bad_ensures_left", "bad_ensures_right"] {
            let owner = NodeId(format!("function:{name}"));
            let callable = program.callables().get(&owner).expect("callable");
            let mut session = crate::verifier::ctx::SolverSession::new(5000).expect("solver");
            let status =
                verify_contracts_from_resolved(callable, program.resolved_types(), &mut session)
                    .map(|(status, _)| status);
            assert_eq!(
                status,
                Some(crate::verifier::ctx::VerifStatus::Disproven),
                "a reachable zero-divisor trap in {name} must not be Proven"
            );
        }

        let owner = NodeId("function:safe_short_circuit".into());
        let callable = program.callables().get(&owner).expect("safe callable");
        let mut session = crate::verifier::ctx::SolverSession::new(5000).expect("solver");
        let status =
            verify_contracts_from_resolved(callable, program.resolved_types(), &mut session)
                .map(|(status, _)| status);
        assert_eq!(
            status,
            Some(crate::verifier::ctx::VerifStatus::Proven),
            "short-circuiting x / x behind x != 0 is defined and provable"
        );

        let owner = NodeId("function:bad_body_power".into());
        let callable = program.callables().get(&owner).expect("power callable");
        let mut session = crate::verifier::ctx::SolverSession::new(5000).expect("solver");
        let status =
            verify_contracts_from_resolved(callable, program.resolved_types(), &mut session)
                .map(|(status, _)| status);
        assert_eq!(
            status,
            Some(crate::verifier::ctx::VerifStatus::Disproven),
            "checked integer power trap must be Disproven even with nominal-result encoding"
        );

        let owner = NodeId("function:unencodable_body_cast".into());
        let callable = program
            .callables()
            .get(&owner)
            .expect("unencodable cast callable");
        let mut session = crate::verifier::ctx::SolverSession::new(5000).expect("solver");
        let status =
            verify_contracts_from_resolved(callable, program.resolved_types(), &mut session)
                .map(|(status, _)| status);
        assert_eq!(
            status,
            Some(crate::verifier::ctx::VerifStatus::NotInTrustedSubset),
            "unencodable cast in body must not be skipped with nominal-result encoding"
        );
    }

    #[test]
    fn e2e_resolved_typed_field_tuple_loads_and_requires_fail_closed() {
        let source = r#"
type Counter { count: i32 }

func add_positive(counter: Counter, amount: i32) -> i32 {
    requires: counter.count >= 0 and counter.count <= 10 and amount > 0 and amount <= 10
    math: { amount > 0 }
    ensures: result > 0
    counter.count + amount
}

func tuple_component(pair: (i32, i32)) -> i32 {
    requires: pair.0 > 0
    ensures: result == pair.0
    pair.0
}

func unencodable_requires(x: i32) -> i32 {
    requires: (x as i64) > 0
    math: { x > 0 }
    ensures: result == x
    x
}

func main() -> i32 { 0 }
"#;
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");

        for name in ["add_positive", "tuple_component"] {
            let owner = NodeId(format!("function:{name}"));
            let callable = program.callables().get(&owner).expect("callable");
            let mut session = crate::verifier::ctx::SolverSession::new(5000).expect("solver");
            let status =
                verify_contracts_from_resolved(callable, program.resolved_types(), &mut session)
                    .map(|(status, _)| status);
            assert_eq!(
                status,
                Some(crate::verifier::ctx::VerifStatus::Proven),
                "checker-typed Field/Tuple projected Loads must share a stable scalar key in {name}"
            );
        }

        let owner = NodeId("function:unencodable_requires".into());
        let callable = program
            .callables()
            .get(&owner)
            .expect("unencodable callable");
        let mut session = crate::verifier::ctx::SolverSession::new(5000).expect("solver");
        let status =
            verify_contracts_from_resolved(callable, program.resolved_types(), &mut session)
                .map(|(status, _)| status);
        assert_eq!(
            status,
            Some(crate::verifier::ctx::VerifStatus::NotInTrustedSubset),
            "an unencodable root Requires must stop before later Math can use missing assumptions"
        );
    }

    #[test]
    fn e2e_resolved_projected_integer_range_facts_are_type_premises() {
        // A projected i32/i64 is not an unconstrained mathematical Int.  The
        // entry type range must be available before root clauses and body
        // definedness, including when the postcondition does not mention the
        // result.  A real endpoint overflow remains a counterexample.
        let source = r#"
type Counter { count: i32 }
type WideCounter { count: i64 }

flow CounterFlow {
    state Open { count: i32 }
    state Closed { count: i32 }
    transition step(Open) -> Closed {
        requires: true
        ensures: true
        return Closed { count: self.count + 0 }
    }
}

func field_i32_zero(counter: Counter) -> i32 {
    requires: true
    ensures: true
    return counter.count + 0
}

func tuple_i32_zero(pair: (i32, i32)) -> i32 {
    requires: true
    ensures: true
    return pair.0 + 0
}

func field_i64_zero(counter: WideCounter) -> i64 {
    requires: true
    ensures: true
    return counter.count + 0
}

func field_i32_overflow(counter: Counter) -> i32 {
    requires: counter.count == 2147483647
    ensures: true
    return counter.count + 1
}

func main() -> i32 { 0 }
"#;
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");

        for name in ["field_i32_zero", "tuple_i32_zero", "field_i64_zero"] {
            let owner = NodeId(format!("function:{name}"));
            let callable = program.callables().get(&owner).expect("callable");
            let mut session = crate::verifier::ctx::SolverSession::new(5000).expect("solver");
            let status =
                verify_contracts_from_resolved(callable, program.resolved_types(), &mut session)
                    .map(|(status, _)| status);
            assert_eq!(
                status,
                Some(crate::verifier::ctx::VerifStatus::Proven),
                "typed projected machine values must be range-pinned at proof entry in {name}"
            );
        }

        let owner = NodeId("transition:CounterFlow::step::Open".into());
        let callable = program.callables().get(&owner).expect("Flow transition");
        let mut session = crate::verifier::ctx::SolverSession::new(5000).expect("solver");
        let status =
            verify_contracts_from_resolved(callable, program.resolved_types(), &mut session)
                .map(|(status, _)| status);
        assert_eq!(
            status,
            Some(crate::verifier::ctx::VerifStatus::Proven),
            "a Flow `self.count + 0` return needs only the static i32 field range"
        );

        let owner = NodeId("function:field_i32_overflow".into());
        let callable = program.callables().get(&owner).expect("overflow callable");
        let mut session = crate::verifier::ctx::SolverSession::new(5000).expect("solver");
        let status =
            verify_contracts_from_resolved(callable, program.resolved_types(), &mut session)
                .map(|(status, _)| status);
        assert_eq!(
            status,
            Some(crate::verifier::ctx::VerifStatus::Disproven),
            "an in-range i32 field at MAX plus one remains a real overflow counterexample"
        );
    }

    #[test]
    fn e2e_resolved_flow_projected_self_loads_check_requirements_and_return_traps() {
        // Flow's checker representation uses Load(ResolvedPlace { Field(..) })
        // for `self.count`. Bounds in Requires must use that same field key in
        // the body-overflow VC, or a safe transition is spuriously Disproven.
        const FLOW: &str = r#"
flow Account {
    state Open { count: i32 }
    state Closed { count: i32 }
    transition close(Open, amount: i32) -> Closed {
        requires: self.count >= 0 && self.count <= 100 && amount >= 1 && amount <= 100
        invariant: self.count >= 0
        math: { amount > 0 }
        ensures: amount > 0
        return Closed { count: self.count + 1 }
    }
}
func main() -> i32 { 0 }
"#;
        let owner = NodeId("transition:Account::close::Open".into());

        for (source, expected, detail) in [
            (
                FLOW.to_string(),
                crate::verifier::ctx::VerifStatus::Proven,
                "Requires field bounds discharge the return-field overflow check",
            ),
            (
                FLOW.replace(" && self.count <= 100", ""),
                crate::verifier::ctx::VerifStatus::Disproven,
                "removing the field upper bound exposes the i32 overflow counterexample",
            ),
        ] {
            let tokens = crate::lexer::Lexer::new(&source).tokenize().expect("lex");
            let file = crate::parser::Parser::new(tokens)
                .parse_file()
                .expect("parse");
            let program = crate::core::check_program(&file).expect("check");
            let callable = program.callables().get(&owner).expect("Flow transition");
            let mut session = crate::verifier::ctx::SolverSession::new(5000).expect("solver");
            let status =
                verify_contracts_from_resolved(callable, program.resolved_types(), &mut session)
                    .map(|(status, _)| status);
            assert_eq!(status, Some(expected), "{detail}");
        }
    }

    #[test]
    fn resolved_index_projection_recurses_into_hidden_effectful_place_input() {
        use crate::core::ir::{EffectId, ResolvedCall, ResolvedCallee};

        let (types, ty) = test_table_and_ty();
        let mut body = test_body_with_local("xs");
        let local = body.parameters[0].clone();
        let input_id = NodeId("place-input:index:effectful".into());
        let mut index_call = test_expr(ResolvedExprKind::Call(ResolvedCall {
            callee: ResolvedCallee::Function(NodeId("function:mutating_index".into())),
            result: ty.clone(),
            type_arguments: vec![],
            arguments: vec![],
            permission: Some(crate::core::ir::Permission::Mutate),
            effects: vec![EffectId::new("mutate-counter").unwrap()],
            session: vec![],
        }));
        index_call.effects = vec![EffectId::new("mutate-counter").unwrap()];
        body.place_inputs
            .insert(input_id.clone(), index_call.clone());

        let dereference = test_expr(ResolvedExprKind::Project {
            value: Box::new(test_expr(ResolvedExprKind::Load(ResolvedPlace::root(
                local.clone(),
            )))),
            projection: ResolvedValueProjection::Dereference,
        });
        let mut vars = Z3VarMap::new();
        let mut obligations = Vec::new();
        assert!(
            collect_expr_i32_definedness(&dereference, &body, &types, &mut vars, &mut obligations,)
                .is_none(),
            "unreceipted dereference must be rejected by definedness collection"
        );
        assert!(
            resolved_to_z3_int(&dereference, &body, &types, &mut vars).is_none(),
            "unreceipted dereference must not be replaced by a free Z3 term"
        );

        let explicit_project = test_expr(ResolvedExprKind::Project {
            value: Box::new(test_expr(ResolvedExprKind::Load(ResolvedPlace::root(
                local.clone(),
            )))),
            projection: ResolvedValueProjection::Index(Box::new(index_call)),
        });
        let mut vars = Z3VarMap::new();
        let mut obligations = Vec::new();
        assert!(
            collect_expr_i32_definedness(
                &explicit_project,
                &body,
                &types,
                &mut vars,
                &mut obligations,
            )
            .is_none(),
            "an effectful explicit Project index is visited and rejected"
        );
        assert!(
            resolved_to_z3_int(&explicit_project, &body, &types, &mut vars).is_none(),
            "an effectful Project index must not be replaced by a free Z3 term"
        );

        let projected_load = test_expr(ResolvedExprKind::Load(ResolvedPlace {
            base: local,
            projections: vec![ResolvedProjection::Index {
                index: ResolvedIndex::Dynamic(input_id),
                ty,
            }],
        }));
        let mut vars = Z3VarMap::new();
        let mut obligations = Vec::new();
        assert!(
            collect_expr_i32_definedness(
                &projected_load,
                &body,
                &types,
                &mut vars,
                &mut obligations,
            )
            .is_none(),
            "Index projected place is unsupported after visiting its hidden input"
        );
        assert!(
            resolved_to_z3_int(&projected_load, &body, &types, &mut vars).is_none(),
            "effectful index input must not be replaced by a free Z3 term"
        );
    }

    #[test]
    fn e2e_resolved_checked_power_uses_typed_term_and_body_definedness() {
        const SOURCE: &str = r#"
type PowerReceipt { completed: bool }

func power_i32_wrap(x: i32, exponent: i32) -> i32 {
    requires: x == 2 && exponent == 31
    ensures: result == -2147483648
    return x ** exponent
}

func hidden_safe_nominal(x: i32, exponent: i32) -> PowerReceipt {
    requires: x == 0 && exponent == 2147483647
    ensures: true
    let computed = x ** exponent
    return PowerReceipt { completed: true }
}

func hidden_negative_nominal(x: i32, exponent: i32) -> PowerReceipt {
    requires: true
    ensures: true
    let computed = x ** exponent
    return PowerReceipt { completed: true }
}

func hidden_u32_cap_nominal(x: i64, exponent: i64) -> PowerReceipt {
    requires: x == 1 && exponent == 4294967296
    ensures: true
    let computed = x ** exponent
    return PowerReceipt { completed: true }
}

func hidden_overflow_nominal(x: i64, exponent: i64) -> PowerReceipt {
    requires: x == 2 && exponent == 63
    ensures: true
    let computed = x ** exponent
    return PowerReceipt { completed: true }
}

func branch_guarded_power(x: i32, exponent: i32, run: bool) -> i32 {
    requires: x == 1 && (!run || exponent >= 0)
    ensures: true
    if run { x ** exponent } else { 0 }
}

func hidden_division_operand(divisor: i32, exponent: i32) -> PowerReceipt {
    requires: true
    ensures: true
    let computed = (1 / divisor) ** exponent
    return PowerReceipt { completed: true }
}

func exponent_source() -> i32 { 1 }

func call_operand_is_not_assumed_total(x: i32) -> PowerReceipt {
    requires: true
    ensures: true
    let computed = x ** exponent_source()
    return PowerReceipt { completed: true }
}

func main() -> i32 { 0 }
"#;
        let tokens = crate::lexer::Lexer::new(SOURCE).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("checked Power sources");

        for (name, expected) in [
            ("power_i32_wrap", crate::verifier::ctx::VerifStatus::Proven),
            (
                "hidden_safe_nominal",
                crate::verifier::ctx::VerifStatus::Proven,
            ),
            (
                "branch_guarded_power",
                crate::verifier::ctx::VerifStatus::Proven,
            ),
            (
                "hidden_negative_nominal",
                crate::verifier::ctx::VerifStatus::Disproven,
            ),
            (
                "hidden_u32_cap_nominal",
                crate::verifier::ctx::VerifStatus::Disproven,
            ),
            (
                "hidden_overflow_nominal",
                crate::verifier::ctx::VerifStatus::Disproven,
            ),
            (
                "hidden_division_operand",
                crate::verifier::ctx::VerifStatus::Disproven,
            ),
            (
                "call_operand_is_not_assumed_total",
                crate::verifier::ctx::VerifStatus::NotInTrustedSubset,
            ),
        ] {
            let owner = NodeId(format!("function:{name}"));
            let callable = program.callables().get(&owner).expect("Power callable");
            let mut session = crate::verifier::ctx::SolverSession::new(5000).expect("solver");
            let result =
                verify_contracts_from_resolved(callable, program.resolved_types(), &mut session);
            let (actual, detail) = result.expect("verification result");
            assert_eq!(actual, expected, "{name}: {detail}");
        }
    }
}
