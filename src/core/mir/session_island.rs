//! Whole-program contract for the typed session-channel island.
//!
//! Session programs already execute on all three MIR consumers (the R6-1046
//! differential pins prove reference/bytecode/native equivalence for the
//! integer-payload roundtrip face), and that face carries checker-owned MIR
//! receipts (`MirSessionCallContract` / `MirSessionPairBindContract`). This
//! module is the program-level admission that lets the default route
//! recognize exactly that proven face instead of handing it to the
//! compatibility route.
//!
//! Admission is deliberately narrow.  Only the differential-proven shape is
//! `CompleteCoverage`: typed `session_pair::<P>()` introductions and
//! top-level `session_send`/`session_recv`/`session_close` statements over
//! local endpoints.  Beyond the session operations themselves, the only
//! other constructs the face admits are the ones the differential matrix
//! exercised: non-session builtin calls (e.g. `println`) whose arguments are
//! all admitted signed integers, integer arithmetic over locals, and plain
//! binds.  Every other shape is a face hazard and forces `MixedCoverage`,
//! which keeps the explicit compatibility route the program had before this
//! island existed: the untyped plain-pair compat form, residual branch
//! merges, actor or Option-mediation faces, generic transfer, string
//! printing, assign statements (outside MIR Phase 0), and session calls
//! nested inside other expressions or structured statements.  A `Complete`
//! admission whose graph cannot materialize stays a hard route error and is
//! never converted into a legacy fallback.

use std::collections::BTreeSet;

use crate::core::ir::{
    ResolvedBlock, ResolvedCall, ResolvedCallee, ResolvedExpr, ResolvedExprKind, ResolvedPattern,
    ResolvedStmtKind, ResolvedType, ResolvedTypeTable,
};
use crate::core::mir::reference::MirProgram;
use crate::core::mir::types::{MirSessionOperation, MirTypeCatalog, MirTypeKind};
use crate::core::{CheckedProgram, PrimitiveType, ResolvedTypeId};

use super::{MirFunction, MirInstructionKind};

/// Versioned default-route island for the typed session-channel face.
pub const SESSION_CHANNEL_ISLAND: &str = "session-channel-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionChannelAdmission {
    OutsideProfile,
    /// Session declarations or calls exist, but the program is not the exact
    /// differential-proven roundtrip face.  This is an explicit compatibility
    /// state: the program keeps the legacy route exactly as before the island
    /// existed.
    MixedCoverage,
    CompleteCoverage,
}

/// Classify the checker shape before MIR construction; see the module docs
/// for the admitted face and the compatibility boundary.
pub fn classify_session_channel_admission(program: &CheckedProgram) -> SessionChannelAdmission {
    if program.sessions().is_empty() {
        return SessionChannelAdmission::OutsideProfile;
    }
    let mut has_typed_ops = false;
    for callable in program.callables().values() {
        if super::islands::is_prelude_origin(program, &callable.body.root.origin) {
            continue;
        }
        match classify_body_session_face(program.resolved_types(), &callable.body.root) {
            BodyFace::Violates => return SessionChannelAdmission::MixedCoverage,
            BodyFace::HasTypedOps => has_typed_ops = true,
            BodyFace::SessionFree => {}
        }
    }
    if has_typed_ops {
        SessionChannelAdmission::CompleteCoverage
    } else {
        SessionChannelAdmission::MixedCoverage
    }
}

enum BodyFace {
    /// No session usage in this body.
    SessionFree,
    /// At least one admitted top-level session operation.
    HasTypedOps,
    /// Session usage outside the admitted top-level face, a construct the
    /// MIR lowering cannot admit, or an assign statement (outside MIR
    /// Phase 0) anywhere in the body.
    Violates,
}

const SESSION_BUILTIN_NAMES: [&str; 4] = [
    "session_pair",
    "session_send",
    "session_recv",
    "session_close",
];

fn call_is_session_builtin(callee: &ResolvedCallee) -> bool {
    matches!(callee, ResolvedCallee::Builtin(name)
        if SESSION_BUILTIN_NAMES.contains(&name.as_str()))
}

fn expr_is_session_call(expression: &ResolvedExpr) -> bool {
    matches!(&expression.kind, ResolvedExprKind::Call(call)
        if call_is_session_builtin(&call.callee))
}

/// The admitted signed-integer primitives: the only payload/print types the
/// differential-proven face exercises.
fn ty_is_admitted_integer(types: &ResolvedTypeTable, ty: &ResolvedTypeId) -> bool {
    matches!(
        types.get(ty),
        Some(ResolvedType::Primitive(
            PrimitiveType::I32 | PrimitiveType::I64
        ))
    )
}

/// The admitted bind forms: the typed pair introduction
/// (`let (lo, hi) = session_pair::<P>()`) and a direct receive
/// (`let value = session_recv(endpoint)`).
fn admitted_session_bind(initializer: &ResolvedExpr, _pattern: &ResolvedPattern) -> bool {
    let ResolvedExprKind::Call(call) = &initializer.kind else {
        return false;
    };
    let ResolvedCallee::Builtin(name) = &call.callee else {
        return false;
    };
    match name.as_str() {
        "session_pair" => {
            // The untyped plain-pair compat form has no residual receipt and
            // stays on the compatibility route.
            !call.type_arguments.is_empty()
        }
        "session_recv" => session_endpoint_is_local_load(call),
        _ => false,
    }
}

/// The admitted statement forms: `session_send(endpoint, payload)` and
/// `session_close(endpoint)` over a local endpoint.
fn admitted_session_statement(expression: &ResolvedExpr) -> bool {
    let ResolvedExprKind::Call(call) = &expression.kind else {
        return false;
    };
    let ResolvedCallee::Builtin(name) = &call.callee else {
        return false;
    };
    match name.as_str() {
        "session_send" | "session_close" | "session_recv" => session_endpoint_is_local_load(call),
        _ => false,
    }
}

fn session_endpoint_is_local_load(call: &ResolvedCall) -> bool {
    call.arguments
        .first()
        .is_some_and(|argument| matches!(argument.value.kind, ResolvedExprKind::Load(_)))
}

/// A session call's arguments must not hide further session calls, hazards,
/// or assigns; the endpoint check above already ran.
fn session_call_arguments_are_clean(types: &ResolvedTypeTable, expression: &ResolvedExpr) -> bool {
    let ResolvedExprKind::Call(call) = &expression.kind else {
        return false;
    };
    call.arguments
        .iter()
        .all(|argument| expr_is_clean(types, &argument.value))
}

/// Walk one callable body.  Session operations are admitted only as top-level
/// statement forms; any session call in a nested position, any session call
/// whose endpoint is not a local load, the untyped pair form, any non-builtin
/// call, any non-session builtin call with a non-integer argument, and any
/// assign statement anywhere in the body all violate the face.
fn classify_body_session_face(types: &ResolvedTypeTable, block: &ResolvedBlock) -> BodyFace {
    let mut face = BodyFace::SessionFree;
    for statement in &block.statements {
        match &statement.kind {
            ResolvedStmtKind::Bind {
                pattern,
                initializer: Some(initializer),
            } if expr_is_session_call(initializer) => {
                if !admitted_session_bind(initializer, pattern)
                    || !session_call_arguments_are_clean(types, initializer)
                {
                    return BodyFace::Violates;
                }
                face = BodyFace::HasTypedOps;
            }
            ResolvedStmtKind::Expr(value) if expr_is_session_call(value) => {
                if !admitted_session_statement(value)
                    || !session_call_arguments_are_clean(types, value)
                {
                    return BodyFace::Violates;
                }
                face = BodyFace::HasTypedOps;
            }
            kind => {
                if !stmt_is_clean(types, kind) {
                    return BodyFace::Violates;
                }
            }
        }
    }
    if let Some(result) = block.result.as_deref() {
        if !expr_is_clean(types, result) {
            return BodyFace::Violates;
        }
    }
    face
}

/// A statement outside the admitted session forms must be entirely free of
/// face hazards and Phase-0 violations: session operations are only admitted
/// at the top level of a callable body, and assigns are outside MIR Phase 0.
fn stmt_is_clean(types: &ResolvedTypeTable, kind: &ResolvedStmtKind) -> bool {
    match kind {
        ResolvedStmtKind::Assign { .. } => false,
        ResolvedStmtKind::Bind {
            initializer: Some(initializer),
            ..
        } => expr_is_clean(types, initializer),
        ResolvedStmtKind::Bind {
            initializer: None, ..
        } => true,
        ResolvedStmtKind::Expr(value) => expr_is_clean(types, value),
        ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => value
            .as_ref()
            .map_or(true, |value| expr_is_clean(types, value)),
        ResolvedStmtKind::While { condition, body }
        | ResolvedStmtKind::For {
            iterable: condition,
            body,
            ..
        } => expr_is_clean(types, condition) && block_is_clean(types, body),
        ResolvedStmtKind::WhileLet {
            initializer, body, ..
        } => expr_is_clean(types, initializer) && block_is_clean(types, body),
        ResolvedStmtKind::IfLet {
            initializer,
            then_block,
            else_block,
            ..
        } => {
            expr_is_clean(types, initializer)
                && block_is_clean(types, then_block)
                && else_block
                    .as_ref()
                    .map_or(true, |block| block_is_clean(types, block))
        }
        ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
            block_is_clean(types, body)
        }
        ResolvedStmtKind::Pinned { value, body, .. } => {
            expr_is_clean(types, value) && block_is_clean(types, body)
        }
        ResolvedStmtKind::Contract { condition, .. } => expr_is_clean(types, condition),
        ResolvedStmtKind::Math(expressions) => expressions
            .iter()
            .all(|expression| expr_is_clean(types, expression)),
        ResolvedStmtKind::Drop(_) | ResolvedStmtKind::Continue => true,
        ResolvedStmtKind::NestedCallable(_) => true,
    }
}

fn block_is_clean(types: &ResolvedTypeTable, block: &ResolvedBlock) -> bool {
    block
        .statements
        .iter()
        .all(|statement| stmt_is_clean(types, &statement.kind))
        && block
            .result
            .as_deref()
            .map_or(true, |expression| expr_is_clean(types, expression))
}

/// Whether the expression hides a face hazard anywhere: a session builtin
/// call in a nested position, or a call whose callee is not a builtin at
/// all (user functions, externs, methods — including generic instances and
/// excluded-prelude targets the MIR lowering cannot admit).  A non-session
/// builtin call (e.g. `println`) is only clean when every argument carries
/// an admitted signed-integer type: the differential matrix never exercised
/// any other builtin shape.
fn expr_has_face_hazard(types: &ResolvedTypeTable, expression: &ResolvedExpr) -> bool {
    match &expression.kind {
        ResolvedExprKind::Call(call) => {
            if !matches!(call.callee, ResolvedCallee::Builtin(_)) {
                return true;
            }
            call_is_session_builtin(&call.callee)
                || call.arguments.iter().any(|argument| {
                    !ty_is_admitted_integer(types, &argument.value.ty)
                        || expr_has_face_hazard(types, &argument.value)
                })
        }
        ResolvedExprKind::Block(block)
        | ResolvedExprKind::Scope { body: block, .. }
        | ResolvedExprKind::Comptime(block)
        | ResolvedExprKind::Quote(block) => block_has_face_hazard(types, block),
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            expr_has_face_hazard(types, condition)
                || block_has_face_hazard(types, then_block)
                || block_has_face_hazard(types, else_block)
        }
        ResolvedExprKind::Match { scrutinee, arms } => {
            expr_has_face_hazard(types, scrutinee)
                || arms.iter().any(|arm| {
                    arm.guard
                        .as_ref()
                        .is_some_and(|guard| expr_has_face_hazard(types, guard))
                        || expr_has_face_hazard(types, &arm.body)
                })
        }
        ResolvedExprKind::Lambda(lambda) => block_has_face_hazard(types, &lambda.body),
        ResolvedExprKind::Record { fields, rest, .. } => {
            fields
                .iter()
                .any(|field| expr_has_face_hazard(types, &field.value))
                || rest
                    .as_deref()
                    .is_some_and(|rest| expr_has_face_hazard(types, rest))
        }
        ResolvedExprKind::Binary { left, right, .. }
        | ResolvedExprKind::Range {
            start: left,
            end: right,
        } => expr_has_face_hazard(types, left) || expr_has_face_hazard(types, right),
        ResolvedExprKind::Unary { operand, .. }
        | ResolvedExprKind::Project { value: operand, .. }
        | ResolvedExprKind::TypeOf(operand)
        | ResolvedExprKind::Old(operand)
        | ResolvedExprKind::Try { value: operand, .. }
        | ResolvedExprKind::Cast { value: operand, .. }
        | ResolvedExprKind::Spawn(operand)
        | ResolvedExprKind::Await(operand)
        | ResolvedExprKind::OptionalChain {
            receiver: operand, ..
        } => expr_has_face_hazard(types, operand),
        ResolvedExprKind::Slice { target, start, end } => {
            expr_has_face_hazard(types, target)
                || start
                    .as_deref()
                    .is_some_and(|start| expr_has_face_hazard(types, start))
                || end
                    .as_deref()
                    .is_some_and(|end| expr_has_face_hazard(types, end))
        }
        ResolvedExprKind::Comprehension {
            value,
            iterable,
            guard,
            ..
        } => {
            expr_has_face_hazard(types, value)
                || expr_has_face_hazard(types, iterable)
                || guard
                    .as_deref()
                    .is_some_and(|guard| expr_has_face_hazard(types, guard))
        }
        ResolvedExprKind::Tuple(elements)
        | ResolvedExprKind::List(elements)
        | ResolvedExprKind::Set(elements) => elements
            .iter()
            .any(|element| expr_has_face_hazard(types, element)),
        ResolvedExprKind::Map(entries) => entries.iter().any(|(key, value)| {
            expr_has_face_hazard(types, key) || expr_has_face_hazard(types, value)
        }),
        ResolvedExprKind::Literal(_)
        | ResolvedExprKind::FString(_)
        | ResolvedExprKind::Load(_)
        | ResolvedExprKind::Constant(_)
        | ResolvedExprKind::Callable(_)
        | ResolvedExprKind::DefaultArgument { .. }
        | ResolvedExprKind::ComptimeValue(_)
        | ResolvedExprKind::TypeValue(_) => false,
    }
}

fn block_has_face_hazard(types: &ResolvedTypeTable, block: &ResolvedBlock) -> bool {
    block
        .statements
        .iter()
        .any(|statement| match &statement.kind {
            ResolvedStmtKind::Assign { value, .. } => expr_has_face_hazard(types, value),
            ResolvedStmtKind::Bind {
                initializer: None, ..
            } => true,
            ResolvedStmtKind::Bind {
                initializer: Some(initializer),
                ..
            }
            | ResolvedStmtKind::Expr(initializer)
            | ResolvedStmtKind::Contract {
                condition: initializer,
                ..
            }
            | ResolvedStmtKind::Pinned {
                value: initializer, ..
            }
            | ResolvedStmtKind::WhileLet { initializer, .. }
            | ResolvedStmtKind::IfLet { initializer, .. } => {
                expr_has_face_hazard(types, initializer)
                    || stmt_blocks_have_face_hazard(types, &statement.kind)
            }
            ResolvedStmtKind::While { condition, .. }
            | ResolvedStmtKind::For {
                iterable: condition,
                ..
            } => {
                expr_has_face_hazard(types, condition)
                    || stmt_blocks_have_face_hazard(types, &statement.kind)
            }
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => value
                .as_ref()
                .is_some_and(|value| expr_has_face_hazard(types, value)),
            ResolvedStmtKind::Math(expressions) => expressions
                .iter()
                .any(|expression| expr_has_face_hazard(types, expression)),
            ResolvedStmtKind::Drop(_)
            | ResolvedStmtKind::Continue
            | ResolvedStmtKind::NestedCallable(_)
            | ResolvedStmtKind::Loop(_)
            | ResolvedStmtKind::Scope { .. } => {
                stmt_blocks_have_face_hazard(types, &statement.kind)
            }
        })
        || block
            .result
            .as_deref()
            .is_some_and(|expression| expr_has_face_hazard(types, expression))
}

fn stmt_blocks_have_face_hazard(types: &ResolvedTypeTable, kind: &ResolvedStmtKind) -> bool {
    match kind {
        ResolvedStmtKind::While { body, .. }
        | ResolvedStmtKind::WhileLet { body, .. }
        | ResolvedStmtKind::Loop(body)
        | ResolvedStmtKind::Scope { body, .. }
        | ResolvedStmtKind::For { body, .. }
        | ResolvedStmtKind::Pinned { body, .. } => block_has_face_hazard(types, body),
        ResolvedStmtKind::IfLet {
            then_block,
            else_block,
            ..
        } => {
            block_has_face_hazard(types, then_block)
                || else_block
                    .as_ref()
                    .is_some_and(|block| block_has_face_hazard(types, block))
        }
        _ => false,
    }
}

/// Assigns live at statement level, so an expression can only hide one
/// behind a nested block-like expression.
fn expr_has_phase_zero_violation(expression: &ResolvedExpr) -> bool {
    match &expression.kind {
        ResolvedExprKind::Block(block)
        | ResolvedExprKind::Scope { body: block, .. }
        | ResolvedExprKind::Comptime(block)
        | ResolvedExprKind::Quote(block) => block_has_phase_zero_violation(block),
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            expr_has_phase_zero_violation(condition)
                || block_has_phase_zero_violation(then_block)
                || block_has_phase_zero_violation(else_block)
        }
        ResolvedExprKind::Match { scrutinee, arms } => {
            expr_has_phase_zero_violation(scrutinee)
                || arms.iter().any(|arm| {
                    arm.guard
                        .as_ref()
                        .is_some_and(expr_has_phase_zero_violation)
                        || expr_has_phase_zero_violation(&arm.body)
                })
        }
        ResolvedExprKind::Lambda(lambda) => block_has_phase_zero_violation(&lambda.body),
        ResolvedExprKind::Binary { left, right, .. }
        | ResolvedExprKind::Range {
            start: left,
            end: right,
        } => expr_has_phase_zero_violation(left) || expr_has_phase_zero_violation(right),
        ResolvedExprKind::Unary { operand, .. }
        | ResolvedExprKind::Project { value: operand, .. }
        | ResolvedExprKind::TypeOf(operand)
        | ResolvedExprKind::Old(operand)
        | ResolvedExprKind::Try { value: operand, .. }
        | ResolvedExprKind::Cast { value: operand, .. }
        | ResolvedExprKind::Spawn(operand)
        | ResolvedExprKind::Await(operand)
        | ResolvedExprKind::OptionalChain {
            receiver: operand, ..
        } => expr_has_phase_zero_violation(operand),
        ResolvedExprKind::Call(call) => call
            .arguments
            .iter()
            .any(|argument| expr_has_phase_zero_violation(&argument.value)),
        ResolvedExprKind::Record { fields, rest, .. } => {
            fields
                .iter()
                .any(|field| expr_has_phase_zero_violation(&field.value))
                || rest.as_deref().is_some_and(expr_has_phase_zero_violation)
        }
        ResolvedExprKind::Slice { target, start, end } => {
            expr_has_phase_zero_violation(target)
                || start.as_deref().is_some_and(expr_has_phase_zero_violation)
                || end.as_deref().is_some_and(expr_has_phase_zero_violation)
        }
        ResolvedExprKind::Comprehension {
            value,
            iterable,
            guard,
            ..
        } => {
            expr_has_phase_zero_violation(value)
                || expr_has_phase_zero_violation(iterable)
                || guard.as_deref().is_some_and(expr_has_phase_zero_violation)
        }
        ResolvedExprKind::Tuple(elements)
        | ResolvedExprKind::List(elements)
        | ResolvedExprKind::Set(elements) => elements.iter().any(expr_has_phase_zero_violation),
        ResolvedExprKind::Map(entries) => entries.iter().any(|(key, value)| {
            expr_has_phase_zero_violation(key) || expr_has_phase_zero_violation(value)
        }),
        _ => false,
    }
}

fn block_has_phase_zero_violation(block: &ResolvedBlock) -> bool {
    block
        .statements
        .iter()
        .any(|statement| match &statement.kind {
            ResolvedStmtKind::Assign { value, .. } => expr_has_phase_zero_violation(value),
            ResolvedStmtKind::Bind {
                initializer: None, ..
            } => true,
            ResolvedStmtKind::Bind {
                initializer: Some(initializer),
                ..
            }
            | ResolvedStmtKind::Expr(initializer)
            | ResolvedStmtKind::Contract {
                condition: initializer,
                ..
            }
            | ResolvedStmtKind::Pinned {
                value: initializer, ..
            }
            | ResolvedStmtKind::WhileLet { initializer, .. }
            | ResolvedStmtKind::IfLet { initializer, .. } => {
                expr_has_phase_zero_violation(initializer)
                    || stmt_blocks_have_phase_zero_violation(&statement.kind)
            }
            ResolvedStmtKind::While { condition, .. }
            | ResolvedStmtKind::For {
                iterable: condition,
                ..
            } => {
                expr_has_phase_zero_violation(condition)
                    || stmt_blocks_have_phase_zero_violation(&statement.kind)
            }
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                value.as_ref().is_some_and(expr_has_phase_zero_violation)
            }
            ResolvedStmtKind::Math(expressions) => {
                expressions.iter().any(expr_has_phase_zero_violation)
            }
            ResolvedStmtKind::Drop(_)
            | ResolvedStmtKind::Continue
            | ResolvedStmtKind::NestedCallable(_)
            | ResolvedStmtKind::Loop(_)
            | ResolvedStmtKind::Scope { .. } => {
                stmt_blocks_have_phase_zero_violation(&statement.kind)
            }
        })
        || block
            .result
            .as_deref()
            .is_some_and(expr_has_phase_zero_violation)
}

fn stmt_blocks_have_phase_zero_violation(kind: &ResolvedStmtKind) -> bool {
    match kind {
        ResolvedStmtKind::While { body, .. }
        | ResolvedStmtKind::WhileLet { body, .. }
        | ResolvedStmtKind::Loop(body)
        | ResolvedStmtKind::Scope { body, .. }
        | ResolvedStmtKind::For { body, .. }
        | ResolvedStmtKind::Pinned { body, .. } => block_has_phase_zero_violation(body),
        ResolvedStmtKind::IfLet {
            then_block,
            else_block,
            ..
        } => {
            block_has_phase_zero_violation(then_block)
                || else_block
                    .as_ref()
                    .is_some_and(block_has_phase_zero_violation)
        }
        _ => false,
    }
}

/// An expression is clean when it hides neither a face hazard nor an assign
/// statement at any depth.
fn expr_is_clean(types: &ResolvedTypeTable, expression: &ResolvedExpr) -> bool {
    !expr_has_face_hazard(types, expression) && !expr_has_phase_zero_violation(expression)
}

/// Whether the materialized program carries any session instruction: the
/// route-level candidate hint that distinguishes a materialized island from
/// a checker shape whose graph never reached the session receipts.
pub fn contains_session_channel_candidate(program: &MirProgram) -> bool {
    program.functions().values().any(function_has_session_call)
}

fn function_has_session_call(function: &MirFunction) -> bool {
    function.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction.kind,
                MirInstructionKind::SessionCall { .. } | MirInstructionKind::SessionPairBind { .. }
            )
        })
    })
}

/// Structural gate for the session-channel island: every session instruction
/// must carry its checker receipt, closes must be terminal and payload-free,
/// sends must carry an admitted signed-integer payload, receives must be
/// payload-free, and every payload type must be a materialized TypeDesc.
/// Anything else fails closed with named errors.
pub fn validate_session_channel_island(program: &MirProgram) -> Result<(), BTreeSet<String>> {
    let mut errors = BTreeSet::new();
    let mut session_instances = 0usize;
    for (function_id, function) in program.functions() {
        for block in function.blocks.values() {
            for instruction in &block.instructions {
                match &instruction.kind {
                    MirInstructionKind::SessionCall {
                        operation,
                        contract,
                        ..
                    } => {
                        session_instances += 1;
                        let Some(contract) = contract else {
                            errors.insert(format!(
                                "{}: session operation has no checker receipt",
                                function_id.0
                            ));
                            continue;
                        };
                        match operation {
                            MirSessionOperation::Close => {
                                if !contract.terminal {
                                    errors.insert(format!(
                                        "{}: session close must terminate the endpoint",
                                        function_id.0
                                    ));
                                }
                                if contract.payload_ty.is_some() {
                                    errors.insert(format!(
                                        "{}: session close must not carry a payload",
                                        function_id.0
                                    ));
                                }
                            }
                            MirSessionOperation::Send => {
                                if !payload_ty_is_admitted(
                                    program.type_catalog(),
                                    contract.payload_ty.as_ref(),
                                ) {
                                    errors.insert(format!(
                                        "{}: session send payload is not an admitted signed integer",
                                        function_id.0
                                    ));
                                }
                            }
                            MirSessionOperation::Recv => {
                                if contract.payload_ty.is_some() {
                                    errors.insert(format!(
                                        "{}: session receive must not carry a payload",
                                        function_id.0
                                    ));
                                }
                            }
                        }
                    }
                    MirInstructionKind::SessionPairBind { contract: None, .. } => {
                        session_instances += 1;
                        errors.insert(format!(
                            "{}: session pair bind has no checker receipt",
                            function_id.0
                        ));
                    }
                    MirInstructionKind::SessionPairBind {
                        contract: Some(_), ..
                    } => {
                        session_instances += 1;
                    }
                    _ => {}
                }
            }
        }
    }
    if session_instances == 0 {
        errors.insert("session-channel island materialized no session operations".to_string());
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn payload_ty_is_admitted(catalog: &MirTypeCatalog, payload_ty: Option<&ResolvedTypeId>) -> bool {
    payload_ty.is_some_and(|payload_ty| {
        catalog.get(payload_ty).is_some_and(|type_desc| {
            matches!(
                type_desc.kind,
                MirTypeKind::Primitive(PrimitiveType::I32 | PrimitiveType::I64)
            )
        })
    })
}
