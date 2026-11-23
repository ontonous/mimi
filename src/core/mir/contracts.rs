//! Canonical contract predicates attached to MIR functions.
//!
//! Contract conditions cross the frontend/backend boundary as a deliberately
//! small, typed predicate language.  They do not retain surface names or
//! `ResolvedExpr` nodes: local references use canonical MIR value identities,
//! and the return value has an explicit `Result` marker.  This lets the MIR
//! verifier consume the same contract facts as the execution backends without
//! reparsing or re-encoding the source AST.

use crate::core::ir::{
    CheckedConversion, EffectId, ResolvedBinaryOp, ResolvedCallee, ResolvedParameterId,
    ResolvedPlace, ResolvedProjection, ResolvedTypeId, TypedCallEffectAuthority,
    TypedCallExecutionDisposition, TypedLoopEdgeKind, TypedLoopFfiCallPath, TypedLoopProofReceipt,
    TypedProofScopeFrame, TYPED_LOOP_PROOF_SCHEMA_VERSION,
};
use crate::core::{NodeId, ResolvedLocalId};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use super::types::{
    verifier_float_boundary_message, MirAbiClass, MirGlueKind, MirLayout, MirOwnership,
    MirTypeCatalog,
};
use super::{MirFunction, MirProjection, MirValidationError, MirValueId};
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirContractKind {
    Requires,
    Ensures,
    Invariant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirContractUnaryOp {
    Negate,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirContractBinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    /// Checked integer power. Its trap semantics are discharged separately
    /// by the typed verifier; this is not a total mathematical exponent.
    Power,
    Equal,
    NotEqual,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    LogicalAnd,
    LogicalOr,
}

impl MirContractBinaryOp {
    pub(crate) fn from_resolved(op: ResolvedBinaryOp) -> Option<Self> {
        Some(match op {
            ResolvedBinaryOp::Add => Self::Add,
            ResolvedBinaryOp::Subtract => Self::Subtract,
            ResolvedBinaryOp::Multiply => Self::Multiply,
            ResolvedBinaryOp::Divide => Self::Divide,
            ResolvedBinaryOp::Remainder => Self::Remainder,
            ResolvedBinaryOp::Power => Self::Power,
            ResolvedBinaryOp::Equal => Self::Equal,
            ResolvedBinaryOp::NotEqual => Self::NotEqual,
            ResolvedBinaryOp::Less => Self::Less,
            ResolvedBinaryOp::Greater => Self::Greater,
            ResolvedBinaryOp::LessEqual => Self::LessEqual,
            ResolvedBinaryOp::GreaterEqual => Self::GreaterEqual,
            ResolvedBinaryOp::LogicalAnd => Self::LogicalAnd,
            ResolvedBinaryOp::LogicalOr => Self::LogicalOr,
            ResolvedBinaryOp::BitAnd
            | ResolvedBinaryOp::BitOr
            | ResolvedBinaryOp::BitXor
            | ResolvedBinaryOp::ShiftLeft
            | ResolvedBinaryOp::ShiftRight => return None,
        })
    }
}

/// A scalar contract expression whose leaves are canonical MIR identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirContractExpr {
    Value(MirValueId),
    Result,
    Old(MirValueId),
    Project {
        base: Box<Self>,
        projection: MirProjection,
    },
    Int(i64),
    Bool(bool),
    /// R6-1065: an f64 literal carried as raw IEEE bits — the exact form the
    /// resolved literal holds, so the canonical text (and digest) is stable.
    /// The Z3 verifier evaluates it as a known constant in the finite-only
    /// Float domain; the runtime FFI predicate evaluator has no float slot
    /// and rejects it at construction (`contains_float_leaf`).
    Float(u64),
    /// Exact UTF-8 string literal used by typed FFI predicates. The runtime
    /// FFI adapters remain on the closed scalar language and reject it.
    String(String),
    /// Exact Mimi `len(String)` operation used by typed FFI predicates.
    StringLength(Box<Self>),
    /// Mimi `char_at(s, 0)` over a checked String value. The index is fixed
    /// to zero here so the verifier can model the language's Unicode-scalar
    /// semantics without admitting the still-unsupported dynamic-index face.
    StringCharAt0(Box<Self>),
    /// A checked FFI root that the MIR predicate language does not yet model.
    /// Keeping this node in the receipt preserves the source obligation and
    /// lets the MIR verifier return NotInTrustedSubset instead of dropping it.
    Unsupported(String),
    Unary {
        op: MirContractUnaryOp,
        operand: Box<Self>,
    },
    Binary {
        op: MirContractBinaryOp,
        left: Box<Self>,
        right: Box<Self>,
    },
}

impl MirContractExpr {
    pub(crate) fn canonical_text(&self) -> String {
        match self {
            Self::Value(value) => value.to_string(),
            Self::Result => "result".into(),
            Self::Old(value) => format!("old({value})"),
            Self::Project { base, projection } => {
                format!(
                    "project({}, {})",
                    base.canonical_text(),
                    projection.canonical_text()
                )
            }
            Self::Int(value) => value.to_string(),
            Self::Bool(value) => value.to_string(),
            Self::Float(bits) => format!("float({})", f64::from_bits(*bits)),
            Self::String(value) => {
                let mut text = format!("string({}:", value.len());
                for byte in value.as_bytes() {
                    let _ = write!(text, "{byte:02x}");
                }
                text.push(')');
                text
            }
            Self::StringLength(value) => format!("string_len({})", value.canonical_text()),
            Self::StringCharAt0(value) => {
                format!("string_char_at0({})", value.canonical_text())
            }
            Self::Unsupported(reason) => {
                let mut text = format!("unsupported({}:", reason.len());
                for byte in reason.as_bytes() {
                    let _ = write!(text, "{byte:02x}");
                }
                text.push(')');
                text
            }
            Self::Unary { op, operand } => {
                let name = match op {
                    MirContractUnaryOp::Negate => "neg",
                    MirContractUnaryOp::Not => "not",
                };
                format!("{name}({})", operand.canonical_text())
            }
            Self::Binary { op, left, right } => {
                let name = match op {
                    MirContractBinaryOp::Add => "add",
                    MirContractBinaryOp::Subtract => "sub",
                    MirContractBinaryOp::Multiply => "mul",
                    MirContractBinaryOp::Divide => "div",
                    MirContractBinaryOp::Remainder => "rem",
                    MirContractBinaryOp::Power => "pow.checked.i64_then_i32_wrap.v0111",
                    MirContractBinaryOp::Equal => "eq",
                    MirContractBinaryOp::NotEqual => "ne",
                    MirContractBinaryOp::Less => "lt",
                    MirContractBinaryOp::Greater => "gt",
                    MirContractBinaryOp::LessEqual => "le",
                    MirContractBinaryOp::GreaterEqual => "ge",
                    MirContractBinaryOp::LogicalAnd => "and",
                    MirContractBinaryOp::LogicalOr => "or",
                };
                format!(
                    "{name}({}, {})",
                    left.canonical_text(),
                    right.canonical_text()
                )
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirContract {
    pub id: NodeId,
    pub kind: MirContractKind,
    pub condition: MirContractExpr,
    /// Actual checker-owned TypeDesc for every canonical predicate expression.
    /// Paths follow the canonical predicate tree: child 0 is a unary/base/
    /// left operand, child 1 is a right operand. Literal and return types must
    /// never be guessed when materializing checked arithmetic obligations.
    pub expression_types: BTreeMap<Vec<u8>, crate::core::ir::ResolvedTypeId>,
    /// Exact checked callsite that supplied an inlined pure helper predicate.
    /// This is emitted only by the verifier-only FFI owner route.
    pub ffi_helper_call: Option<MirContractHelperCall>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirContractHelperCall {
    pub call_node: NodeId,
    pub helper: NodeId,
}

/// A checked scalar expression used at a lexical proof point.  It is kept in
/// the canonical MIR request (and therefore in its digest), without retaining
/// a ResolvedExpr or surface-AST node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofExpression {
    pub origin: NodeId,
    pub expression: MirContractExpr,
    pub expression_types: BTreeMap<Vec<u8>, crate::core::ir::ResolvedTypeId>,
    /// Checker TypeDesc and effect census for the exact source root.
    pub ty: crate::core::ir::ResolvedTypeId,
    pub effects: Vec<EffectId>,
    pub source_fingerprint: [u8; 32],
    pub type_path_fingerprint: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MirProofPointId {
    pub owner: NodeId,
    pub statement: NodeId,
    pub ordinal: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MirProofCallPointId {
    pub owner: NodeId,
    pub statement: NodeId,
    pub call_node: NodeId,
    pub order: u64,
}

/// Checked source identity for one place-input expression. It is retained in
/// the optional proof request census even when no proof consumer executes it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MirProofPlaceInputId {
    pub point: NodeId,
    pub expression: NodeId,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MirProofDefaultRootId {
    pub owner: NodeId,
    pub parameter: ResolvedParameterId,
    pub expression: NodeId,
}

/// Canonical MIR proof program for one declaration-default root. Default
/// expressions are not part of the function's executable body CFG, so this
/// isolated program carries its own typed terms and exact omitted-argument
/// use receipts. A boundary entry keeps unsupported roots visible without
/// granting them proof facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofDefaultRoot {
    pub id: MirProofDefaultRootId,
    pub span: crate::span::Span,
    pub source_spans: BTreeMap<NodeId, crate::span::Span>,
    pub uses: Vec<MirProofDefaultUseId>,
    pub operations: Vec<MirProofDefaultOperation>,
    pub boundaries: Vec<MirProofDefaultBoundary>,
    pub uses_valid: bool,
    pub source_fingerprint: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirProofDefaultOperation {
    Bind {
        point: NodeId,
        span: crate::span::Span,
        local: MirValueId,
        local_type: ResolvedTypeId,
        initializer: MirProofExpression,
    },
    Eval {
        point: NodeId,
        span: crate::span::Span,
        expression: MirProofExpression,
    },
    Math {
        id: MirProofPointId,
        order: u64,
        span: crate::span::Span,
        scope: Vec<TypedProofScopeFrame>,
        expression: MirProofExpression,
    },
    Scope {
        id: NodeId,
        operations: Vec<MirProofDefaultOperation>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MirProofDefaultUseId {
    pub caller_owner: NodeId,
    pub call_node: NodeId,
    pub callee_owner: NodeId,
    pub parameter: ResolvedParameterId,
    pub argument_node: NodeId,
    pub parameter_index: usize,
    pub order: u64,
    pub scope: Vec<crate::core::ir::TypedProofScopeFrame>,
    pub guards: Vec<(NodeId, bool)>,
    pub disposition: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MirProofSourceBoundary {
    pub point: NodeId,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MirProofDefaultBoundary {
    pub root: MirProofDefaultRootId,
    pub point: NodeId,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MirProofBoundaryMathId {
    pub point: MirProofPointId,
    pub order: u64,
    pub expression: NodeId,
    pub scope: Vec<crate::core::ir::TypedProofScopeFrame>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofBoundaryArgument {
    pub ordinal: usize,
    pub parameter: ResolvedParameterId,
    pub expression: NodeId,
    pub value_type: ResolvedTypeId,
    pub conversion: CheckedConversion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofBoundaryGuard {
    pub condition: NodeId,
    pub when: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofCallBoundary {
    pub id: MirProofCallPointId,
    pub body_owner: NodeId,
    pub expression: NodeId,
    pub scope: Vec<crate::core::ir::TypedProofScopeFrame>,
    pub guards: Vec<MirProofBoundaryGuard>,
    pub disposition: TypedCallExecutionDisposition,
    pub callee: Option<ResolvedCallee>,
    pub expression_type: ResolvedTypeId,
    pub type_arguments: Vec<ResolvedTypeId>,
    pub result_type: Option<ResolvedTypeId>,
    pub declared_effects: Vec<EffectId>,
    pub effect: crate::core::ir::TypedCallEffectReceipt,
    pub argument_count: usize,
    pub arguments: Vec<MirProofBoundaryArgument>,
}

/// Explicit proof-only boundary. It preserves the full checker census while
/// keeping unsupported/deferred proof shapes out of runtime MIR lowering.
/// A consumer must treat these identities as unproved, never as facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofBoundary {
    pub point: NodeId,
    pub reason: String,
    pub scoped_boundaries: Vec<MirProofSourceBoundary>,
    pub math_points: Vec<MirProofBoundaryMathId>,
    pub loop_invariants: Vec<MirProofBoundaryMathId>,
    pub calls: Vec<MirProofCallBoundary>,
    pub deferred_calls: Vec<MirProofCallBoundary>,
    pub default_roots: Vec<MirProofDefaultRootId>,
    pub default_math_points: Vec<MirProofBoundaryMathId>,
    pub default_calls: Vec<MirProofCallBoundary>,
    pub default_boundaries: Vec<MirProofDefaultBoundary>,
    pub default_uses: Vec<MirProofDefaultUseId>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirProofScopeFrame {
    Block(NodeId),
    Branch {
        statement: NodeId,
        expression: NodeId,
        then_arm: bool,
    },
    Loop(NodeId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofAssertion {
    pub id: MirProofPointId,
    pub order: u64,
    pub scope: Vec<MirProofScopeFrame>,
    pub expression: MirProofExpression,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofCallGuard {
    pub condition: MirProofExpression,
    pub when: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofCallArgument {
    pub parameter: ResolvedParameterId,
    pub expression: MirProofExpression,
    pub conversion: CheckedConversion,
    pub value: MirValueId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofCallPoint {
    pub body_owner: NodeId,
    pub owner: NodeId,
    pub statement: NodeId,
    pub call_node: NodeId,
    pub order: u64,
    pub scope: Vec<MirProofScopeFrame>,
    pub guards: Vec<MirProofCallGuard>,
    pub match_path: Vec<crate::core::ir::TypedMatchPathReceipt>,
    pub disposition: TypedCallExecutionDisposition,
    pub callee: ResolvedCallee,
    pub type_arguments: Vec<ResolvedTypeId>,
    pub result_type: ResolvedTypeId,
    pub declared_effects: Vec<EffectId>,
    pub effect_authority: TypedCallEffectAuthority,
    pub instruction: super::MirInstructionId,
    pub arguments: Vec<MirProofCallArgument>,
    pub result: Option<MirValueId>,
    /// Conservative havoc set after the call. No callee Ensures predicate is
    /// imported as an assumption by this receipt.
    pub havoc_locals: Vec<ResolvedLocalId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofLoopBinding {
    pub local: crate::core::ir::ResolvedLocalId,
    pub ty: ResolvedTypeId,
    pub preheader_value: MirValueId,
    pub header_parameter: MirValueId,
    pub exit_parameter: MirValueId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirProofLoopEdgeKind {
    Entry,
    ConditionTrue,
    ConditionFalse,
    BodyFallthrough,
    Continue,
    Break,
    Return,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofLoopEdge {
    pub source_site: NodeId,
    pub kind: MirProofLoopEdgeKind,
    pub source_cfg_edge: Option<crate::core::cfg::EdgeId>,
    pub source_reachable: bool,
    pub mir_edge: Option<super::MirEdgeId>,
    pub from_block: super::MirBlockId,
    pub to_block: Option<super::MirBlockId>,
    pub arguments: Vec<MirValueId>,
    pub terminator_value: Option<MirValueId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofLoopReceipt {
    pub typed: TypedLoopProofReceipt,
    pub bindings: Vec<MirProofLoopBinding>,
    /// Exact MIR block containing the checker-profiled FFI call and its
    /// terminal Break edge. This is present only for the narrow immutable
    /// at-most-once call path.
    pub ffi_call_block: Option<super::MirBlockId>,
    /// Exact MIR edge/terminal inventory corresponding to the typed CFG
    /// receipt. The consumer must compare this with the actual MirFunction;
    /// this vector is not execution authority by itself.
    pub edges: Vec<MirProofLoopEdge>,
}

impl MirProofLoopEdgeKind {
    pub(crate) fn from_typed(kind: TypedLoopEdgeKind) -> Self {
        match kind {
            TypedLoopEdgeKind::BodyFallthrough => Self::BodyFallthrough,
            TypedLoopEdgeKind::Continue => Self::Continue,
            TypedLoopEdgeKind::Break => Self::Break,
            TypedLoopEdgeKind::Return => Self::Return,
        }
    }
}

fn loop_arm_is_terminal_or_empty(block: &MirProofBlock, loop_node: &NodeId) -> Option<bool> {
    match block.operations.as_slice() {
        [] => Some(true),
        [MirProofOperation::Break {
            target_loop: Some(target),
            value: None,
            ..
        }] if target == loop_node => Some(false),
        [MirProofOperation::Continue {
            target_loop: Some(target),
            ..
        }] if target == loop_node => Some(false),
        _ => None,
    }
}

/// Independently bind the checker loop-call profile to its materialized MIR
/// worklist shape. This admits only pure guarded prefix branches, one exact
/// call, and a direct break immediately after it; nested/repeated/effectful
/// loop calls do not get this receipt.
fn mir_loop_body_matches_ffi_call_path(
    body: &MirProofBlock,
    profile: &TypedLoopFfiCallPath,
) -> bool {
    let mut index = 0usize;
    for guard in &profile.call_guards {
        let Some(MirProofOperation::Branch {
            point,
            expression,
            condition,
            then_block,
            else_block,
            ..
        }) = body.operations.get(index)
        else {
            return false;
        };
        let Some(then_falls_through) =
            loop_arm_is_terminal_or_empty(then_block, &profile.loop_node)
        else {
            return false;
        };
        let Some(else_falls_through) =
            loop_arm_is_terminal_or_empty(else_block, &profile.loop_node)
        else {
            return false;
        };
        if point != &guard.statement
            || expression != &guard.expression
            || condition.origin != guard.condition
            || then_falls_through == else_falls_through
            || then_falls_through != guard.then_arm
        {
            return false;
        }
        index += 1;
    }
    let Some(MirProofOperation::Call(call)) = body.operations.get(index) else {
        return false;
    };
    if call.statement != profile.call_statement
        || call.call_node != profile.call_node
        || !matches!(&call.callee, ResolvedCallee::Extern(_))
        || !matches!(&call.disposition, TypedCallExecutionDisposition::Guarded)
        || !call.match_path.is_empty()
        || !call.type_arguments.is_empty()
        || !call.declared_effects.is_empty()
    {
        return false;
    }
    index += 1;
    matches!(
        body.operations.get(index),
        Some(MirProofOperation::Break {
            point,
            target_loop: Some(target),
            value: None,
        }) if point == &profile.terminal_break && target == &profile.loop_node
    ) && index + 1 == body.operations.len()
}

fn mir_proof_block_contains_call(block: &MirProofBlock) -> bool {
    block.operations.iter().any(|operation| match operation {
        MirProofOperation::Call(_) => true,
        MirProofOperation::Branch {
            then_block,
            else_block,
            ..
        } => mir_proof_block_contains_call(then_block) || mir_proof_block_contains_call(else_block),
        MirProofOperation::Scope(inner) => mir_proof_block_contains_call(inner),
        MirProofOperation::Loop { .. } => true,
        MirProofOperation::Bind { .. }
        | MirProofOperation::Eval { .. }
        | MirProofOperation::Assign { .. }
        | MirProofOperation::Math(_)
        | MirProofOperation::Break { .. }
        | MirProofOperation::Continue { .. }
        | MirProofOperation::Return { .. }
        | MirProofOperation::Exit => false,
    })
}

/// Validate the source side of the one admitted loop-call shape without
/// trusting public receipt fields. The checker witness binds the typed AST;
/// these explicit cross-checks keep the profile's call/guard/break identities
/// joined to its exact source CFG slice at the MIR boundary.
fn typed_loop_ffi_profile_matches_source(
    typed: &TypedLoopProofReceipt,
    loop_node: &NodeId,
) -> bool {
    use crate::core::cfg::{EdgeId, EdgeKind, Terminator};
    use crate::core::ir::{TypedLoopEdgeKind as K, TypedLoopExpressionRole as R};

    if typed.schema_version != TYPED_LOOP_PROOF_SCHEMA_VERSION
        || typed.loop_node != *loop_node
        || !typed.has_valid_source_witness()
    {
        return false;
    }
    let Some(profile) = typed.ffi_call_path.as_ref() else {
        return typed.call_nodes.is_empty();
    };
    if profile.loop_node != *loop_node
        || profile.condition_node != typed.condition_node
        || typed.call_nodes.as_slice() != [profile.call_node.clone()]
        || profile.call_guards.len() > 1
        || typed.invariant_points.len() != 0
        || !typed.writes.is_empty()
        || !typed.effect_nodes.is_empty()
        || !typed.loans.is_empty()
        || typed
            .resource_actions
            .iter()
            .any(|action| action.kind != crate::core::ownership::CanonicalActionKind::Read)
        || typed.body_edges.len() != 1 + usize::from(!profile.call_guards.is_empty())
        || typed.body_edges.iter().any(|edge| {
            !edge.reachable
                || edge.target_loop.as_ref() != Some(loop_node)
                || !matches!(edge.kind, K::Break | K::Continue)
        })
    {
        return false;
    }

    let condition_sites = typed
        .expression_sites
        .iter()
        .filter(|site| {
            site.owner == typed.owner
                && site.loop_node == *loop_node
                && site.role == R::Condition
                && site.point == *loop_node
                && site.expression == profile.condition_node
                && site.branch_expression.is_none()
                && site.effects.is_empty()
        })
        .count();
    let call_sites = typed
        .expression_sites
        .iter()
        .filter(|site| {
            site.owner == typed.owner
                && site.loop_node == *loop_node
                && site.role == R::Eval
                && site.point == profile.call_statement
                && site.expression == profile.call_node
                && site.branch_expression.is_none()
                && site.effects.is_empty()
        })
        .count();
    if condition_sites != 1 || call_sites != 1 {
        return false;
    }

    let call_points = typed
        .cfg_blocks
        .iter()
        .flat_map(|block| {
            block
                .points
                .iter()
                .filter(|point| point.source.node == profile.call_node)
                .map(move |_| block.id.clone())
        })
        .collect::<Vec<_>>();
    let break_points = typed
        .cfg_blocks
        .iter()
        .flat_map(|block| {
            block
                .points
                .iter()
                .filter(|point| point.source.node == profile.terminal_break)
                .map(move |_| block.id.clone())
        })
        .collect::<Vec<_>>();
    if call_points.as_slice() != [profile.terminal_block.clone()]
        || break_points.as_slice() != [profile.terminal_block.clone()]
    {
        return false;
    }

    let expected_break_edges = typed
        .body_edges
        .iter()
        .filter(|edge| {
            edge.site == profile.terminal_break
                && edge.kind == K::Break
                && edge.target_loop.as_ref() == Some(loop_node)
                && edge.cfg_terminal_block == profile.terminal_block
                && edge.guards == profile.call_guards
                && edge.terminal_value.is_none()
        })
        .collect::<Vec<_>>();
    if expected_break_edges.len() != 1 {
        return false;
    }
    let exit_edge_id = EdgeId(NodeId(format!("{}/cfg:edge:while-exit", loop_node.0)));
    let Some(exit_edge) = typed
        .cfg_edges
        .iter()
        .find(|edge| edge.id == exit_edge_id && edge.kind == EdgeKind::LoopExit)
    else {
        return false;
    };
    let break_edge_id = EdgeId(NodeId(format!(
        "{}/cfg:edge:break",
        profile.terminal_break.0
    )));
    if expected_break_edges[0].cfg_terminal_edge.as_ref() != Some(&break_edge_id)
        || !typed.cfg_edges.iter().any(|edge| {
            edge.id == break_edge_id
                && edge.kind == EdgeKind::Break
                && edge.from == profile.terminal_block
                && edge.to == exit_edge.to
                && edge.source.node == profile.terminal_break
        })
    {
        return false;
    }

    if let Some(call_guard) = profile.call_guards.first() {
        let mut complementary = call_guard.clone();
        complementary.then_arm = !complementary.then_arm;
        let early_edges = typed
            .body_edges
            .iter()
            .filter(|edge| edge.site != profile.terminal_break)
            .collect::<Vec<_>>();
        let [early] = early_edges.as_slice() else {
            return false;
        };
        if early.guards.as_slice() != [complementary]
            || !matches!(early.kind, K::Break | K::Continue)
        {
            return false;
        }
    }

    // Every recorded guard is an exact ordered source CFG branch. The
    // corresponding call-reaching break edge above carries the same guard
    // vector, so a prefix branch that only continues/breaks cannot create a
    // call obligation by itself.
    for guard in &profile.call_guards {
        let then_id = EdgeId(NodeId(format!("{}/cfg:edge:if-then", guard.expression.0)));
        let else_id = EdgeId(NodeId(format!("{}/cfg:edge:if-else", guard.expression.0)));
        let then_edges = typed
            .cfg_edges
            .iter()
            .filter(|edge| edge.id == then_id && edge.kind == EdgeKind::Then)
            .collect::<Vec<_>>();
        let else_edges = typed
            .cfg_edges
            .iter()
            .filter(|edge| edge.id == else_id && edge.kind == EdgeKind::Else)
            .collect::<Vec<_>>();
        let ([then_edge], [else_edge]) = (then_edges.as_slice(), else_edges.as_slice()) else {
            return false;
        };
        if then_edge.from != else_edge.from
            || then_edge.source.node != guard.expression
            || else_edge.source.node != guard.expression
            || !typed.cfg_blocks.iter().any(|block| {
                block.id == then_edge.from
                    && matches!(
                        &block.terminator,
                        Terminator::Branch {
                            condition,
                            then_edge: actual_then,
                            else_edge: actual_else,
                        } if condition == &guard.condition
                            && actual_then == &then_id
                            && actual_else == &else_id
                    )
            })
        {
            return false;
        }
    }
    true
}

fn validate_loop_mir_edge(edge: &MirProofLoopEdge, function: &MirFunction) -> bool {
    let matching = function
        .blocks
        .values()
        .filter_map(|block| {
            let record_matches = match &block.terminator {
                super::MirTerminator::Goto {
                    edge: id,
                    target,
                    arguments,
                } => {
                    edge.mir_edge.as_ref() == Some(id)
                        && edge.to_block.as_ref() == Some(target)
                        && edge.arguments == *arguments
                }
                super::MirTerminator::Branch {
                    then_edge,
                    then_target,
                    then_arguments,
                    else_edge,
                    else_target,
                    else_arguments,
                    ..
                } => {
                    (edge.mir_edge.as_ref() == Some(then_edge)
                        && edge.to_block.as_ref() == Some(then_target)
                        && edge.arguments == *then_arguments)
                        || (edge.mir_edge.as_ref() == Some(else_edge)
                            && edge.to_block.as_ref() == Some(else_target)
                            && edge.arguments == *else_arguments)
                }
                super::MirTerminator::Switch { arms, .. }
                | super::MirTerminator::SwitchMove { arms, .. } => arms.iter().any(|arm| {
                    edge.mir_edge.as_ref() == Some(&arm.edge)
                        && edge.to_block.as_ref() == Some(&arm.target)
                        && edge.arguments == arm.arguments
                }),
                _ => false,
            };
            record_matches.then(|| block.id.clone())
        })
        .filter(|from| from == &edge.from_block)
        .count();
    matching == 1
}

fn mir_loop_ffi_call_block_matches(
    function: &MirFunction,
    receipt: &MirProofLoopReceipt,
    profile: &TypedLoopFfiCallPath,
    loop_exit: &super::MirBlockId,
) -> bool {
    let Ok(call_id) = super::MirInstructionId::new(format!("inst:call:{}", profile.call_node.0))
    else {
        return false;
    };
    let call_locations = function
        .blocks
        .values()
        .flat_map(|block| {
            block
                .instructions
                .iter()
                .filter(|instruction| instruction.id == call_id)
                .map(move |instruction| (block.id.clone(), instruction))
        })
        .collect::<Vec<_>>();
    let [call_location] = call_locations.as_slice() else {
        return false;
    };
    if !matches!(
        &call_location.1.kind,
        super::MirInstructionKind::Call {
            callee: ResolvedCallee::Extern(_),
            ..
        }
    ) || receipt.ffi_call_block.as_ref() != Some(&call_location.0)
    {
        return false;
    }
    let break_edges = receipt
        .edges
        .iter()
        .filter(|edge| {
            edge.source_site == profile.terminal_break && edge.kind == MirProofLoopEdgeKind::Break
        })
        .collect::<Vec<_>>();
    let [break_edge] = break_edges.as_slice() else {
        return false;
    };
    let expected_id =
        super::MirEdgeId::new(format!("edge:while.break:{}", profile.terminal_break.0));
    expected_id.is_ok_and(|expected_id| {
        break_edge.mir_edge.as_ref() == Some(&expected_id)
            && break_edge.from_block == call_location.0
            && break_edge.to_block.as_ref() == Some(loop_exit)
            && receipt.ffi_call_block.as_ref() == Some(&break_edge.from_block)
            && validate_loop_mir_edge(break_edge, function)
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofBlock {
    pub id: NodeId,
    pub operations: Vec<MirProofOperation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirProofOperation {
    Bind {
        point: NodeId,
        local_identity: NodeId,
        local: MirValueId,
        initializer: MirProofExpression,
    },
    Call(MirProofCallPoint),
    Eval {
        point: NodeId,
        expression: MirProofExpression,
    },
    Assign {
        point: NodeId,
        target: ResolvedPlace,
        value: MirProofExpression,
        conversion: CheckedConversion,
        result: MirValueId,
        conversion_result: Option<MirValueId>,
    },
    Math(MirProofAssertion),
    Branch {
        point: NodeId,
        expression: NodeId,
        branch_block: super::MirBlockId,
        then_target: super::MirBlockId,
        else_target: super::MirBlockId,
        condition_value: MirValueId,
        condition: MirProofExpression,
        then_block: MirProofBlock,
        else_block: MirProofBlock,
    },
    Scope(MirProofBlock),
    Loop {
        point: NodeId,
        header: super::MirBlockId,
        body_entry: super::MirBlockId,
        exit: super::MirBlockId,
        condition_value: MirValueId,
        condition: MirProofExpression,
        condition_calls: Vec<MirProofCallPoint>,
        invariants: Vec<MirProofAssertion>,
        body: MirProofBlock,
        stationary: bool,
        /// Required for induction over a mutable loop. `None` is retained
        /// only for old stationary compatibility requests and never
        /// authorizes a mutable-loop proof.
        receipt: Option<MirProofLoopReceipt>,
    },
    Break {
        point: NodeId,
        target_loop: Option<NodeId>,
        value: Option<MirProofExpression>,
    },
    Continue {
        point: NodeId,
        target_loop: Option<NodeId>,
    },
    Return {
        point: NodeId,
        value: Option<MirProofExpression>,
        conversion: Option<CheckedConversion>,
    },
    Exit,
}

/// Complete, ordered proof-point census for one callable.  This is an MIR
/// artifact rather than a verifier-side side table, so route receipts bind
/// each obligation to its owner, lexical point, expression TypeDesc, and
/// supported control-flow shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofWorklist {
    pub owner: NodeId,
    pub expected_math_points: Vec<MirProofPointId>,
    pub expected_loop_invariants: Vec<MirProofPointId>,
    pub expected_calls: Vec<MirProofCallPointId>,
    pub expected_deferred_calls: Vec<MirProofCallBoundary>,
    pub expected_scoped_boundaries: Vec<NodeId>,
    pub expected_default_roots: Vec<MirProofDefaultRootId>,
    pub expected_default_math_points: Vec<MirProofBoundaryMathId>,
    pub expected_default_calls: Vec<MirProofCallBoundary>,
    pub expected_default_boundaries: Vec<MirProofDefaultBoundary>,
    pub expected_default_uses: Vec<MirProofDefaultUseId>,
    pub expected_place_inputs: Vec<MirProofPlaceInputId>,
    pub boundaries: Vec<MirProofBoundary>,
    pub root: MirProofBlock,
    // Worklists are built from checked Resolved expressions. A private seal
    // makes post-lowering edits to the public term or any nested TypeDesc
    // path detectable before MIR verification can use the worklist.
    private_source_witness: [u8; 32],
}

impl MirProofWorklist {
    fn compute_private_source_witness(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"mimi-mir-proof-worklist-source-v3\0");
        hasher.update(
            format!(
                "{:#?}",
                (
                    (
                        &self.owner,
                        &self.expected_math_points,
                        &self.expected_loop_invariants,
                        &self.expected_calls,
                        &self.expected_deferred_calls,
                        &self.expected_scoped_boundaries,
                        &self.expected_default_roots,
                    ),
                    (
                        &self.expected_default_math_points,
                        &self.expected_default_calls,
                        &self.expected_default_boundaries,
                        &self.expected_default_uses,
                        &self.expected_place_inputs,
                        &self.boundaries,
                        &self.root,
                    ),
                )
            )
            .as_bytes(),
        );
        *hasher.finalize().as_bytes()
    }

    fn seal_source_witness(mut self) -> Self {
        self.private_source_witness = self.compute_private_source_witness();
        self
    }

    /// Detect public-field mutation before any proof path consumes terms.
    pub fn has_valid_source_witness(&self) -> bool {
        self.private_source_witness == self.compute_private_source_witness()
    }

    pub(crate) fn canonical_text(&self) -> String {
        fn expression_text(expression: &MirProofExpression) -> String {
            let mut text = format!(
                "origin={} ty={} effects={:?} source_fingerprint={:02x?} type_path_fingerprint={:02x?} expression={}",
                expression.origin.0,
                expression.ty.as_str(),
                expression.effects,
                expression.source_fingerprint,
                expression.type_path_fingerprint,
                expression.expression.canonical_text()
            );
            for (path, ty) in &expression.expression_types {
                text.push_str(" type[");
                text.push_str(&contract_path_text(path));
                text.push_str("]=");
                text.push_str(ty.as_str());
            }
            text
        }
        fn assertion_text(assertion: &MirProofAssertion) -> String {
            format!(
                "owner={} statement={} ordinal={} order={} scope={:?} {}",
                assertion.id.owner.0,
                assertion.id.statement.0,
                assertion.id.ordinal,
                assertion.order,
                assertion.scope,
                expression_text(&assertion.expression)
            )
        }
        fn block_text(block: &MirProofBlock, output: &mut String) {
            use std::fmt::Write as _;
            let _ = writeln!(output, "  proof.block {}", block.id.0);
            for operation in &block.operations {
                match operation {
                    MirProofOperation::Bind {
                        point,
                        local_identity,
                        local,
                        initializer,
                    } => {
                        let _ = writeln!(
                            output,
                            "    proof.bind point={} local_identity={} local={} {}",
                            point.0,
                            local_identity.0,
                            local,
                            expression_text(initializer)
                        );
                    }
                    MirProofOperation::Call(call) => {
                        let _ = writeln!(
                            output,
                            "    proof.call owner={} body_owner={} statement={} node={} order={} scope={:?} guards={:?} match_path={:?} disposition={:?} callee={:?} type_arguments={:?} result_type={} declared_effects={:?} authority={:?} instruction={} result={:?} havoc={:?}",
                            call.owner.0,
                            call.body_owner.0,
                            call.statement.0,
                            call.call_node.0,
                            call.order,
                            call.scope,
                            call.guards,
                            call.match_path,
                            call.disposition,
                            call.callee,
                            call.type_arguments,
                            call.result_type.as_str(),
                            call.declared_effects,
                            call.effect_authority,
                            call.instruction,
                            call.result,
                            call.havoc_locals
                        );
                        for guard in &call.guards {
                            let _ = writeln!(
                                output,
                                "      proof.call_guard when={} {}",
                                guard.when,
                                expression_text(&guard.condition)
                            );
                        }
                        for argument in &call.arguments {
                            let _ = writeln!(
                                output,
                                "      proof.call_argument parameter={} value={} conversion={:?} {}",
                                argument.parameter.0.0,
                                argument.value,
                                argument.conversion,
                                expression_text(&argument.expression)
                            );
                        }
                    }
                    MirProofOperation::Eval { point, expression } => {
                        let _ = writeln!(
                            output,
                            "    proof.eval point={} {}",
                            point.0,
                            expression_text(expression)
                        );
                    }
                    MirProofOperation::Assign {
                        point,
                        target,
                        value,
                        conversion,
                        result,
                        conversion_result,
                    } => {
                        let _ = writeln!(
                            output,
                            "    proof.assign point={} target={:?} conversion={:?} result={} conversion_result={:?} {}",
                            point.0,
                            target,
                            conversion,
                            result,
                            conversion_result,
                            expression_text(value)
                        );
                    }
                    MirProofOperation::Math(assertion) => {
                        let _ = writeln!(output, "    proof.math {}", assertion_text(assertion));
                    }
                    MirProofOperation::Branch {
                        point,
                        expression,
                        branch_block,
                        then_target,
                        else_target,
                        condition_value,
                        condition,
                        then_block,
                        else_block,
                    } => {
                        let _ = writeln!(
                            output,
                            "    proof.branch point={} expression={} block={} then={} else={} condition_value={} {}",
                            point.0,
                            expression.0,
                            branch_block,
                            then_target,
                            else_target,
                            condition_value,
                            expression_text(condition)
                        );
                        block_text(then_block, output);
                        block_text(else_block, output);
                    }
                    MirProofOperation::Scope(inner) => block_text(inner, output),
                    MirProofOperation::Loop {
                        point,
                        header,
                        body_entry,
                        exit,
                        condition_value,
                        condition,
                        condition_calls,
                        invariants,
                        body,
                        stationary,
                        receipt,
                    } => {
                        let _ = writeln!(
                            output,
                            "    proof.loop point={} header={} body={} exit={} condition_value={} stationary={} receipt={:?} condition_calls={:?} {}",
                            point.0,
                            header,
                            body_entry,
                            exit,
                            condition_value,
                            stationary,
                            receipt,
                            condition_calls,
                            expression_text(condition)
                        );
                        for invariant in invariants {
                            let _ = writeln!(
                                output,
                                "      proof.loop_invariant {}",
                                assertion_text(invariant)
                            );
                        }
                        block_text(body, output);
                    }
                    MirProofOperation::Break {
                        point,
                        target_loop,
                        value,
                    } => {
                        let _ = writeln!(
                            output,
                            "    proof.break point={} target_loop={:?} value={:?}",
                            point.0,
                            target_loop,
                            value.as_ref().map(expression_text)
                        );
                    }
                    MirProofOperation::Continue { point, target_loop } => {
                        let _ = writeln!(
                            output,
                            "    proof.continue point={} target_loop={:?}",
                            point.0, target_loop
                        );
                    }
                    MirProofOperation::Return {
                        point,
                        value,
                        conversion,
                    } => {
                        let _ = writeln!(
                            output,
                            "    proof.return point={} conversion={:?} value={:?}",
                            point.0,
                            conversion,
                            value.as_ref().map(expression_text)
                        );
                    }
                    MirProofOperation::Exit => {
                        let _ = writeln!(output, "    proof.exit");
                    }
                }
            }
        }

        use std::fmt::Write as _;
        let mut output = format!("mir.proof_worklist owner={} v7\n", self.owner.0);
        for point in &self.expected_math_points {
            let _ = writeln!(
                output,
                "  proof.expected_math owner={} statement={} ordinal={}",
                point.owner.0, point.statement.0, point.ordinal
            );
        }
        for point in &self.expected_loop_invariants {
            let _ = writeln!(
                output,
                "  proof.expected_loop_invariant owner={} statement={} ordinal={}",
                point.owner.0, point.statement.0, point.ordinal
            );
        }
        for call in &self.expected_calls {
            let _ = writeln!(
                output,
                "  proof.expected_call owner={} statement={} call={} order={}",
                call.owner.0, call.statement.0, call.call_node.0, call.order
            );
        }
        for call in &self.expected_deferred_calls {
            let _ = writeln!(
                output,
                "  proof.expected_deferred_call owner={} body_owner={} statement={} call={} order={} disposition={:?} scope={:?} guards={:?} callee={:?} expression_type={:?} type_arguments={:?} result_type={:?} effects={:?} effect={:?} argument_count={} arguments={:?}",
                call.id.owner.0,
                call.body_owner.0,
                call.id.statement.0,
                call.id.call_node.0,
                call.id.order,
                call.disposition,
                call.scope,
                call.guards,
                call.callee,
                call.expression_type,
                call.type_arguments,
                call.result_type,
                call.declared_effects,
                call.effect,
                call.argument_count,
                call.arguments
            );
        }
        for root in &self.expected_default_roots {
            let _ = writeln!(
                output,
                "  proof.expected_default_root owner={} parameter={} expression={}",
                root.owner.0, root.parameter.0 .0, root.expression.0
            );
        }
        for point in &self.expected_default_math_points {
            let _ = writeln!(
                output,
                "  proof.expected_default_math owner={} statement={} ordinal={} order={}",
                point.point.owner.0, point.point.statement.0, point.point.ordinal, point.order
            );
        }
        for call in &self.expected_default_calls {
            let _ = writeln!(
                output,
                "  proof.expected_default_call owner={} body_owner={} statement={} call={} order={} disposition={:?} scope={:?} guards={:?} callee={:?} expression_type={:?} type_arguments={:?} result_type={:?} effects={:?} effect={:?} argument_count={} arguments={:?}",
                call.id.owner.0,
                call.body_owner.0,
                call.id.statement.0,
                call.id.call_node.0,
                call.id.order,
                call.disposition,
                call.scope,
                call.guards,
                call.callee,
                call.expression_type,
                call.type_arguments,
                call.result_type,
                call.declared_effects,
                call.effect,
                call.argument_count,
                call.arguments
            );
        }
        for boundary in &self.expected_default_boundaries {
            let _ = writeln!(
                output,
                "  proof.expected_default_boundary owner={} parameter={} expression={} point={} reason={:?}",
                boundary.root.owner.0,
                boundary.root.parameter.0.0,
                boundary.root.expression.0,
                boundary.point.0,
                boundary.reason
            );
        }
        for use_id in &self.expected_default_uses {
            let _ = writeln!(
                output,
                "  proof.expected_default_use caller={} call={} callee={} parameter={} argument={} index={} order={}",
                use_id.caller_owner.0,
                use_id.call_node.0,
                use_id.callee_owner.0,
                use_id.parameter.0.0,
                use_id.argument_node.0,
                use_id.parameter_index,
                use_id.order,
            );
            let _ = writeln!(
                output,
                "    scope={:?} guards={:?} disposition={}",
                use_id.scope, use_id.guards, use_id.disposition
            );
        }
        for place in &self.expected_place_inputs {
            let _ = writeln!(
                output,
                "  proof.expected_place_input point={} expression={}",
                place.point.0, place.expression.0
            );
        }
        for boundary in &self.boundaries {
            let _ = writeln!(
                output,
                "  proof.boundary point={} reason={:?} scoped={:?} math={:?} invariants={:?} calls={:?} deferred_calls={:?} default_roots={:?} default_math={:?} default_calls={:?} default_boundaries={:?} default_uses={:?}",
                boundary.point.0,
                boundary.reason,
                boundary.scoped_boundaries,
                boundary.math_points,
                boundary.loop_invariants,
                boundary.calls,
                boundary.deferred_calls,
                boundary.default_roots,
                boundary.default_math_points,
                boundary.default_calls,
                boundary.default_boundaries,
                boundary.default_uses
            );
        }
        block_text(&self.root, &mut output);
        output
    }
}

impl MirContract {
    pub(crate) fn canonical_text(&self) -> String {
        let kind = match self.kind {
            MirContractKind::Requires => "requires",
            MirContractKind::Ensures => "ensures",
            MirContractKind::Invariant => "invariant",
        };
        let mut text = format!(
            "  contract {} {} = {}",
            self.id.0.as_str(),
            kind,
            self.condition.canonical_text()
        );
        if let Some(helper_call) = &self.ffi_helper_call {
            text.push_str(" ffi_helper_call=");
            text.push_str(&helper_call.call_node.0);
            text.push_str("->");
            text.push_str(&helper_call.helper.0);
        }
        for (path, ty) in &self.expression_types {
            text.push_str(" expression_type[");
            text.push_str(&contract_path_text(path));
            text.push_str("]=");
            text.push_str(ty.as_str());
        }
        text
    }
}

fn contract_path_text(path: &[u8]) -> String {
    if path.is_empty() {
        "root".into()
    } else {
        path.iter().map(u8::to_string).collect::<Vec<_>>().join(".")
    }
}

fn is_arithmetic_expression(expression: &MirContractExpr) -> bool {
    matches!(
        expression,
        MirContractExpr::Unary {
            op: MirContractUnaryOp::Negate,
            ..
        } | MirContractExpr::Binary {
            op: MirContractBinaryOp::Add
                | MirContractBinaryOp::Subtract
                | MirContractBinaryOp::Multiply
                | MirContractBinaryOp::Divide
                | MirContractBinaryOp::Remainder
                | MirContractBinaryOp::Power,
            ..
        }
    )
}

fn validate_expression_types(
    contract: &MirContract,
    function: &MirFunction,
    catalog: &MirTypeCatalog,
) -> Result<(), String> {
    fn visit(
        expression: &MirContractExpr,
        path: &mut Vec<u8>,
        contract: &MirContract,
        function: &MirFunction,
        catalog: &MirTypeCatalog,
        visited: &mut BTreeSet<Vec<u8>>,
    ) -> Result<crate::core::ir::ResolvedTypeId, String> {
        let ty = contract.expression_types.get(path).ok_or_else(|| {
            format!(
                "contract expression at '{}' has no TypeDesc receipt",
                contract_path_text(path)
            )
        })?;
        visited.insert(path.clone());
        let kind = type_kind(catalog, ty)?;
        if !matches!(
            kind,
            ContractValueKind::Aggregate(_) | ContractValueKind::ProjectionBase(_)
        ) && !catalog
            .get(ty)
            .is_some_and(|descriptor| descriptor.is_canonical_copy_scalar(true))
            && catalog.validate_owned_string(ty).is_err()
        {
            return Err("contract scalar TypeDesc has invalid ownership/glue metadata".into());
        }
        if kind != expr_kind(expression, function, catalog)? {
            return Err(
                "contract expression TypeDesc disagrees with its scalar/aggregate kind".into(),
            );
        }
        match expression {
            MirContractExpr::Value(_) | MirContractExpr::Old(_) | MirContractExpr::Result => {
                if expression_type(expression, function, catalog)? != *ty {
                    return Err(
                        "contract value TypeDesc disagrees with its canonical MIR value".into(),
                    );
                }
            }
            MirContractExpr::Int(value) => {
                let descriptor = catalog
                    .get(ty)
                    .ok_or_else(|| "contract literal TypeDesc is absent".to_string())?;
                match descriptor.abi {
                    MirAbiClass::Integer {
                        bits: 32,
                        signed: true,
                    } if i32::try_from(*value).is_err() => {
                        return Err(
                            "contract integer literal is outside its recorded i32 TypeDesc".into(),
                        );
                    }
                    MirAbiClass::Integer {
                        bits: 32 | 64,
                        signed: true,
                    } => {}
                    _ => return Err("contract integer literal has non-integer TypeDesc".into()),
                }
            }
            MirContractExpr::Bool(_) | MirContractExpr::Float(_) => {}
            MirContractExpr::String(_) => {
                catalog
                    .validate_owned_string(ty)
                    .map_err(|_| "String literal has a non-String TypeDesc".to_string())?;
            }
            MirContractExpr::StringLength(operand) => {
                path.push(0);
                let operand_ty = visit(operand, path, contract, function, catalog, visited)?;
                path.pop();
                catalog
                    .validate_owned_string(&operand_ty)
                    .map_err(|_| "len() operand is not an owned String".to_string())?;
                if type_kind(catalog, ty)? != ContractValueKind::Int {
                    return Err("len() result TypeDesc is not an integer".into());
                }
            }
            MirContractExpr::Unsupported(_) => {
                return Err("unsupported nodes are not ordinary contract expressions".into());
            }
            MirContractExpr::StringCharAt0(operand) => {
                path.push(0);
                let operand_ty = visit(operand, path, contract, function, catalog, visited)?;
                path.pop();
                catalog
                    .validate_owned_string(&operand_ty)
                    .map_err(|_| "char_at(0) operand is not an owned String".to_string())?;
                catalog
                    .validate_owned_string(ty)
                    .map_err(|_| "char_at(0) result is not an owned String".to_string())?;
            }
            MirContractExpr::Project { base, projection } => {
                path.push(0);
                let base_ty = visit(base, path, contract, function, catalog, visited)?;
                path.pop();
                if catalog.projection_result_type(&base_ty, projection)? != *ty {
                    return Err(
                        "contract projection TypeDesc disagrees with its canonical field".into(),
                    );
                }
            }
            MirContractExpr::Unary { operand, .. } => {
                path.push(0);
                let operand_ty = visit(operand, path, contract, function, catalog, visited)?;
                path.pop();
                if operand_ty != *ty {
                    return Err("contract unary operand TypeDesc disagrees with its result".into());
                }
            }
            MirContractExpr::Binary { op, left, right } => {
                let mut operands = Vec::new();
                for (index, operand) in [left, right].into_iter().enumerate() {
                    path.push(index as u8);
                    operands.push(visit(operand, path, contract, function, catalog, visited)?);
                    path.pop();
                }
                if is_arithmetic_expression(expression) {
                    let operand_abis = operands
                        .iter()
                        .map(|operand| catalog.get(operand).map(|descriptor| descriptor.abi))
                        .collect::<Vec<_>>();
                    if matches!(op, MirContractBinaryOp::Power) {
                        let exact_checked_power_abi = operands[0] == operands[1]
                            && operands[1] == *ty
                            && matches!(
                                operand_abis[0],
                                Some(MirAbiClass::Integer {
                                    bits: 32 | 64,
                                    signed: true,
                                })
                            );
                        if !exact_checked_power_abi {
                            return Err(
                                "checked power requires matching signed i32/i64 operand and result TypeDescs".into(),
                            );
                        }
                        return Ok(ty.clone());
                    }
                    // Mirror the checker's numeric promotion using recorded
                    // operand types. A literal keeps its actual type, and
                    // widening an i32/i32 result by editing one receipt is
                    // rejected; mixed i32/i64 operands legitimately use i64.
                    let promoted = match (&operand_abis[0], &operand_abis[1]) {
                        (
                            Some(MirAbiClass::Integer {
                                bits: left,
                                signed: true,
                            }),
                            Some(MirAbiClass::Integer {
                                bits: right,
                                signed: true,
                            }),
                        ) => MirAbiClass::Integer {
                            bits: (*left).max(*right),
                            signed: true,
                        },
                        (
                            Some(MirAbiClass::Float { bits: 64 }),
                            Some(MirAbiClass::Float { bits: 64 }),
                        ) => MirAbiClass::Float { bits: 64 },
                        _ => {
                            return Err(
                                "contract arithmetic operands have invalid numeric TypeDescs"
                                    .into(),
                            );
                        }
                    };
                    if catalog.get(ty).map(|descriptor| descriptor.abi) != Some(promoted) {
                        return Err("contract arithmetic TypeDesc disagrees with recorded operand promotion".into());
                    }
                } else if matches!(
                    op,
                    MirContractBinaryOp::LogicalAnd | MirContractBinaryOp::LogicalOr
                ) && (operands[0] != *ty || operands[1] != *ty)
                {
                    return Err(
                        "contract logical operand TypeDesc disagrees with its result".into(),
                    );
                }
            }
        }
        Ok(ty.clone())
    }

    // Hand-authored legacy predicates retain their former scalar-kind gate.
    // Checked arithmetic evaluation still requires explicit type receipts;
    // in particular an untyped division cannot acquire a proof verdict.
    if contract.expression_types.is_empty() {
        return Ok(());
    }
    let mut visited = BTreeSet::new();
    visit(
        &contract.condition,
        &mut Vec::new(),
        contract,
        function,
        catalog,
        &mut visited,
    )?;
    if contract
        .expression_types
        .keys()
        .any(|path| !visited.contains(path))
    {
        return Err("contract expression TypeDesc receipt has an absent path".into());
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ContractValueKind {
    Int,
    Bool,
    String,
    /// R6-1063 Face B: a finite-only IEEE f64 value. Ordering/equality
    /// comparisons are admitted; arithmetic stays integer-only below.
    Float,
    Aggregate(crate::core::ir::ResolvedTypeId),
    /// Move-owned products may serve as read-only projection bases. They do
    /// not acquire aggregate equality or arithmetic semantics.
    ProjectionBase(crate::core::ir::ResolvedTypeId),
}

fn is_canonical_projection_base(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ir::ResolvedTypeId,
    descriptor: &super::types::MirTypeDesc,
) -> bool {
    if !matches!(
        descriptor.ownership,
        MirOwnership::Move | MirOwnership::Linear
    ) {
        return false;
    }
    match &descriptor.layout {
        MirLayout::Tuple(_) => catalog.validate_recursive_tuple_abi(ty).is_ok(),
        MirLayout::Record { .. } => {
            descriptor.abi == MirAbiClass::Aggregate
                && descriptor.glue.move_out == MirGlueKind::Aggregate
                && descriptor.glue.clone == MirGlueKind::Aggregate
                && descriptor.glue.drop == MirGlueKind::Aggregate
                && descriptor.drop_plan.is_some()
        }
        _ => false,
    }
}

fn type_kind(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ir::ResolvedTypeId,
) -> Result<ContractValueKind, String> {
    let descriptor = catalog.get(ty).ok_or_else(|| {
        format!(
            "contract type '{}' is absent from MIR TypeDesc",
            ty.as_str()
        )
    })?;
    match descriptor.abi {
        MirAbiClass::Integer {
            bits: 32 | 64,
            signed: true,
        } => Ok(ContractValueKind::Int),
        MirAbiClass::Bool => Ok(ContractValueKind::Bool),
        MirAbiClass::Float { bits: 64 } => Ok(ContractValueKind::Float),
        MirAbiClass::StringHandle if catalog.validate_owned_string(ty).is_ok() => {
            Ok(ContractValueKind::String)
        }
        _ if matches!(
            descriptor.layout,
            MirLayout::Tuple(_) | MirLayout::Record { .. }
        ) && descriptor.has_canonical_copy_noop_metadata() =>
        {
            Ok(ContractValueKind::Aggregate(ty.clone()))
        }
        _ if is_canonical_projection_base(catalog, ty, descriptor) => {
            Ok(ContractValueKind::ProjectionBase(ty.clone()))
        }
        _ if matches!(
            descriptor.layout,
            MirLayout::Tuple(_) | MirLayout::Record { .. }
        ) =>
        {
            let boundary = if descriptor.ownership == MirOwnership::Copy {
                "canonical Copy aggregate contract"
            } else {
                "canonical aggregate projection contract"
            };
            Err(format!(
                "contract type '{}' is outside the {boundary}",
                ty.as_str()
            ))
        }
        MirAbiClass::Float { .. } => Err(verifier_float_boundary_message(ty, descriptor.abi)),
        abi => Err(format!(
            "contract type '{}' ABI {:?} is outside the canonical scalar verifier contract",
            ty.as_str(),
            abi
        )),
    }
}

fn value_kind(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    value: &MirValueId,
) -> Result<ContractValueKind, String> {
    let value = function
        .values
        .get(value)
        .ok_or_else(|| format!("contract value '{}' is absent from MIR values", value))?;
    type_kind(catalog, &value.ty).map_err(|message| {
        format!(
            "contract value '{}' {}",
            value.id.as_str(),
            message.trim_start_matches("contract ")
        )
    })
}

fn result_kind(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
) -> Result<ContractValueKind, String> {
    type_kind(catalog, &function.result).map_err(|message| {
        format!(
            "contract result {}",
            message.trim_start_matches("contract ")
        )
    })
}

fn expression_type(
    expression: &MirContractExpr,
    function: &MirFunction,
    catalog: &MirTypeCatalog,
) -> Result<crate::core::ir::ResolvedTypeId, String> {
    match expression {
        MirContractExpr::Value(value) | MirContractExpr::Old(value) => function
            .values
            .get(value)
            .map(|value| value.ty.clone())
            .ok_or_else(|| format!("contract value '{}' is absent from MIR values", value)),
        MirContractExpr::Result => Ok(function.result.clone()),
        MirContractExpr::Project { base, projection } => {
            let base_ty = expression_type(base, function, catalog)?;
            catalog.projection_result_type(&base_ty, projection)
        }
        MirContractExpr::Int(_)
        | MirContractExpr::Bool(_)
        | MirContractExpr::Float(_)
        | MirContractExpr::String(_)
        | MirContractExpr::StringLength(_)
        | MirContractExpr::StringCharAt0(_)
        | MirContractExpr::Unsupported(_)
        | MirContractExpr::Unary { .. }
        | MirContractExpr::Binary { .. } => {
            Err("contract expression has no aggregate projection type".into())
        }
    }
}

fn expr_kind(
    expression: &MirContractExpr,
    function: &MirFunction,
    catalog: &MirTypeCatalog,
) -> Result<ContractValueKind, String> {
    match expression {
        MirContractExpr::Value(value) | MirContractExpr::Old(value) => {
            value_kind(function, catalog, value)
        }
        MirContractExpr::Result => result_kind(function, catalog),
        MirContractExpr::Project { base, projection } => {
            let base_ty = expression_type(base, function, catalog)?;
            if !matches!(
                type_kind(catalog, &base_ty)?,
                ContractValueKind::Aggregate(_) | ContractValueKind::ProjectionBase(_)
            ) {
                return Err("contract projection base must be a canonical tuple or record".into());
            }
            let result_ty = catalog.projection_result_type(&base_ty, projection)?;
            type_kind(catalog, &result_ty)
        }
        MirContractExpr::Int(_) => Ok(ContractValueKind::Int),
        MirContractExpr::Bool(_) => Ok(ContractValueKind::Bool),
        MirContractExpr::Float(_) => Ok(ContractValueKind::Float),
        MirContractExpr::String(_) => Ok(ContractValueKind::String),
        MirContractExpr::StringCharAt0(operand) => {
            if expr_kind(operand, function, catalog)? != ContractValueKind::String {
                return Err("char_at(0) operand is not a String".into());
            }
            Ok(ContractValueKind::String)
        }
        MirContractExpr::StringLength(operand) => {
            if expr_kind(operand, function, catalog)? != ContractValueKind::String {
                return Err("len() operand is not a String".into());
            }
            Ok(ContractValueKind::Int)
        }
        MirContractExpr::Unsupported(_) => {
            Err("unsupported expression is outside ordinary callable contracts".into())
        }
        MirContractExpr::Unary { op, operand } => {
            let kind = expr_kind(operand, function, catalog)?;
            match (op, kind.clone()) {
                // R6-1066: IEEE negation over a finite-only Float value —
                // the same exact `Z3_mk_fpa_neg` semantics the verifier
                // applies; the runtime FFI evaluator stays integer-only
                // (its contains_float_leaf guard rejects the operand
                // before this walk could admit it).
                (MirContractUnaryOp::Negate, ContractValueKind::Int)
                | (MirContractUnaryOp::Negate, ContractValueKind::Float)
                | (MirContractUnaryOp::Not, ContractValueKind::Bool) => Ok(kind),
                _ => Err("contract unary operator has an incompatible scalar operand".into()),
            }
        }
        MirContractExpr::Binary { op, left, right } => {
            let left_kind = expr_kind(left, function, catalog)?;
            let right_kind = expr_kind(right, function, catalog)?;
            match op {
                MirContractBinaryOp::Add
                | MirContractBinaryOp::Subtract
                | MirContractBinaryOp::Multiply
                | MirContractBinaryOp::Divide
                | MirContractBinaryOp::Remainder => {
                    if left_kind == ContractValueKind::Int && right_kind == ContractValueKind::Int {
                        Ok(ContractValueKind::Int)
                    // R6-1067: float arithmetic joins the R6-1063 comparison
                    // face — RNE fpa ops are the exact fmul/fdiv runtime
                    // semantics, and the runtime owns the traps (overflow /
                    // non-finite results are the E0813 finiteness trap, the
                    // ±0.0 divisor the E0801 division-definedness
                    // violation).  Remainder stays integer-only.
                    } else if left_kind == ContractValueKind::Float
                        && right_kind == ContractValueKind::Float
                        && !matches!(op, MirContractBinaryOp::Remainder)
                    {
                        Ok(ContractValueKind::Float)
                    } else {
                        Err("contract arithmetic requires integer operands".into())
                    }
                }
                MirContractBinaryOp::Power => {
                    if left_kind == ContractValueKind::Int && right_kind == ContractValueKind::Int {
                        Ok(ContractValueKind::Int)
                    } else {
                        Err("checked integer power requires integer operands".into())
                    }
                }
                MirContractBinaryOp::LogicalAnd | MirContractBinaryOp::LogicalOr => {
                    if left_kind == ContractValueKind::Bool && right_kind == ContractValueKind::Bool
                    {
                        Ok(ContractValueKind::Bool)
                    } else {
                        Err("contract logical operator requires boolean operands".into())
                    }
                }
                MirContractBinaryOp::Equal | MirContractBinaryOp::NotEqual => {
                    if matches!(
                        (left_kind, right_kind),
                        (ContractValueKind::Int, ContractValueKind::Int)
                            | (ContractValueKind::Bool, ContractValueKind::Bool)
                            | (ContractValueKind::String, ContractValueKind::String)
                            | (ContractValueKind::Float, ContractValueKind::Float)
                    ) {
                        Ok(ContractValueKind::Bool)
                    } else {
                        Err("contract equality operands have incompatible scalar types".into())
                    }
                }
                MirContractBinaryOp::Less
                | MirContractBinaryOp::Greater
                | MirContractBinaryOp::LessEqual
                | MirContractBinaryOp::GreaterEqual => {
                    if matches!(
                        (left_kind, right_kind),
                        (ContractValueKind::Int, ContractValueKind::Int)
                            | (ContractValueKind::Float, ContractValueKind::Float)
                    ) {
                        Ok(ContractValueKind::Bool)
                    } else {
                        Err("contract ordering requires integer or finite-float operands".into())
                    }
                }
            }
        }
    }
}

/// Face B keeps the runtime FFI predicate evaluator integer/boolean: float
/// leaves have no checked runtime scalar slot (`MirContractScalar`), so an
/// extern contract must not admit them even though function contracts now
/// compare finite f64 values symbolically.
fn contains_float_leaf(
    expression: &MirContractExpr,
    function: &MirFunction,
    catalog: &MirTypeCatalog,
) -> bool {
    match expression {
        MirContractExpr::Value(value) => {
            matches!(
                value_kind(function, catalog, value),
                Ok(ContractValueKind::Float)
            )
        }
        MirContractExpr::Int(_) | MirContractExpr::Bool(_) => false,
        // R6-1065: a float literal is a float leaf by construction — the
        // runtime FFI evaluator could not evaluate it.
        MirContractExpr::Float(_) => true,
        MirContractExpr::String(_)
        | MirContractExpr::StringLength(_)
        | MirContractExpr::StringCharAt0(_)
        | MirContractExpr::Unsupported(_) => false,
        MirContractExpr::Unary { operand, .. } => contains_float_leaf(operand, function, catalog),
        MirContractExpr::Binary { left, right, .. } => {
            contains_float_leaf(left, function, catalog)
                || contains_float_leaf(right, function, catalog)
        }
        MirContractExpr::Result | MirContractExpr::Old(_) | MirContractExpr::Project { .. } => {
            false
        }
    }
}

/// Validate the typed MIR form of an FFI predicate receipt. This is the
/// verifier boundary: unlike execution backends it may carry owned Strings,
/// record/tuple projections, and finite f64 predicates. Every expression
/// node must have an exact root-local Checker TypeDesc path. Unsupported
/// typed shapes remain present as `Unsupported` and are classified by the
/// verifier as NotInTrustedSubset.
pub(crate) fn validate_ffi_mir_contracts(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    receipt: &super::MirFfiCallContract,
) -> Result<(), String> {
    fn validate_root(
        function: &MirFunction,
        catalog: &MirTypeCatalog,
        arguments: &[MirValueId],
        parameter_types: &[crate::core::ir::ResolvedTypeId],
        result: Option<&MirValueId>,
        result_type: &crate::core::ir::ResolvedTypeId,
        phase: &str,
        expression: Option<&MirContractExpr>,
        types: &BTreeMap<Vec<u8>, crate::core::ir::ResolvedTypeId>,
    ) -> Result<(), String> {
        let Some(expression) = expression else {
            return if types.is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "extern {phase} has TypeDesc paths without a predicate"
                ))
            };
        };
        if contains_string_char_at0(expression)
            && (phase != "requires"
                || !ffi_declaration_char_at0_requires_is_supported(
                    expression,
                    arguments,
                    parameter_types,
                    catalog,
                ))
        {
            return Err(format!(
                "extern {phase} char_at(0) requires the exact guarded direct-String form len(s) > 0 && char_at(s, 0) == one non-NUL Unicode scalar"
            ));
        }
        fn kind(
            catalog: &MirTypeCatalog,
            ty: &crate::core::ir::ResolvedTypeId,
            phase: &str,
        ) -> Result<ContractValueKind, String> {
            let descriptor = catalog
                .get(ty)
                .ok_or_else(|| format!("extern predicate TypeDesc '{}' is absent", ty.as_str()))?;
            match descriptor.abi {
                MirAbiClass::Integer {
                    bits: 32 | 64,
                    signed: true,
                } => Ok(ContractValueKind::Int),
                MirAbiClass::Bool => Ok(ContractValueKind::Bool),
                MirAbiClass::Float { bits: 64 } => Ok(ContractValueKind::Float),
                MirAbiClass::StringHandle if catalog.validate_owned_string(ty).is_ok() => {
                    Ok(ContractValueKind::String)
                }
                _ if matches!(
                    descriptor.layout,
                    MirLayout::Tuple(_) | MirLayout::Record { .. }
                ) =>
                {
                    Ok(ContractValueKind::ProjectionBase(ty.clone()))
                }
                abi => Err(format!(
                    "extern {phase} predicate TypeDesc '{}' ABI {abi:?} is outside the typed FFI proof language",
                    ty.as_str()
                )),
            }
        }
        fn ffi_integer_width(
            catalog: &MirTypeCatalog,
            ty: &crate::core::ir::ResolvedTypeId,
        ) -> Option<u16> {
            let descriptor = catalog.get(ty)?;
            match descriptor.abi {
                MirAbiClass::Integer {
                    bits: bits @ (32 | 64),
                    signed: true,
                } => Some(bits),
                _ => None,
            }
        }
        fn ffi_primitive_integer_width(
            catalog: &MirTypeCatalog,
            ty: &crate::core::ir::ResolvedTypeId,
        ) -> Option<u16> {
            use crate::core::ir::PrimitiveType;
            let descriptor = catalog.get(ty)?;
            match (&descriptor.kind, descriptor.abi) {
                (
                    super::types::MirTypeKind::Primitive(PrimitiveType::I32),
                    MirAbiClass::Integer {
                        bits: 32,
                        signed: true,
                    },
                ) => Some(32),
                (
                    super::types::MirTypeKind::Primitive(PrimitiveType::I64),
                    MirAbiClass::Integer {
                        bits: 64,
                        signed: true,
                    },
                ) => Some(64),
                _ => None,
            }
        }
        fn ffi_integer_common_width(
            catalog: &MirTypeCatalog,
            left: &crate::core::ir::ResolvedTypeId,
            right: &crate::core::ir::ResolvedTypeId,
        ) -> Option<(u16, bool)> {
            if left == right {
                return ffi_integer_width(catalog, left).map(|width| (width, true));
            }
            let left_width = ffi_primitive_integer_width(catalog, left)?;
            let right_width = ffi_primitive_integer_width(catalog, right)?;
            Some((left_width.max(right_width), false))
        }
        fn ffi_integer_result_matches(
            catalog: &MirTypeCatalog,
            result: &crate::core::ir::ResolvedTypeId,
            common: (u16, bool),
            left: &crate::core::ir::ResolvedTypeId,
        ) -> bool {
            if common.1 {
                return result == left;
            }
            use crate::core::ir::PrimitiveType;
            let expected_primitive = match common.0 {
                32 => PrimitiveType::I32,
                64 => PrimitiveType::I64,
                _ => return false,
            };
            let expected = crate::core::ir::primitive_type_id(expected_primitive);
            catalog.get(result).is_some_and(|descriptor| {
                result == &expected
                    && descriptor.kind == super::types::MirTypeKind::Primitive(expected_primitive)
            })
        }
        fn visit(
            expression: &MirContractExpr,
            path: &mut Vec<u8>,
            function: &MirFunction,
            catalog: &MirTypeCatalog,
            arguments: &[MirValueId],
            parameter_types: &[crate::core::ir::ResolvedTypeId],
            result: Option<&MirValueId>,
            result_type: &crate::core::ir::ResolvedTypeId,
            phase: &str,
            types: &BTreeMap<Vec<u8>, crate::core::ir::ResolvedTypeId>,
            visited: &mut BTreeSet<Vec<u8>>,
        ) -> Result<crate::core::ir::ResolvedTypeId, String> {
            let ty = types.get(path).cloned().ok_or_else(|| {
                format!(
                    "extern {phase} expression at '{}' has no root-local TypeDesc witness",
                    contract_path_text(path)
                )
            })?;
            visited.insert(path.clone());
            if let MirContractExpr::Unsupported(_) = expression {
                visited.extend(
                    types
                        .keys()
                        .filter(|candidate| candidate.starts_with(path))
                        .cloned(),
                );
                return Ok(ty);
            }
            match expression {
                MirContractExpr::Value(value) => {
                    if !arguments.contains(value) && !(phase == "ensures" && result == Some(value))
                    {
                        return Err(format!(
                            "extern {phase} value '{value}' is not an allowed call argument/result"
                        ));
                    }
                    if !function.values.contains_key(value) {
                        return Err(format!("extern {phase} MIR value '{value}' is absent"));
                    }
                    // FFI contract roots are typed against the declaration's
                    // parameter/result ABI. The MIR value is the caller's
                    // source-side value, whose type can differ when its exact
                    // ABI conversion widens/narrows the endpoint. The caller
                    // TypeDesc ↔ source receipt ↔ ABI conversion chain is
                    // checked independently by the source census and FFI
                    // receipt validator; here the root-local Checker type
                    // must join the physical declaration endpoint.
                    let endpoint_type = arguments
                        .iter()
                        .position(|argument| argument == value)
                        .and_then(|index| parameter_types.get(index))
                        .or_else(|| {
                            (phase == "ensures" && result == Some(value)).then_some(result_type)
                        });
                    if endpoint_type != Some(&ty) {
                        return Err(format!(
                            "extern {phase} value TypeDesc disagrees with its declaration endpoint"
                        ));
                    }
                }
                MirContractExpr::Int(value) => {
                    let descriptor = catalog
                        .get(&ty)
                        .ok_or_else(|| "extern integer literal TypeDesc is absent".to_string())?;
                    match descriptor.abi {
                        MirAbiClass::Integer {
                            bits: 32,
                            signed: true,
                        } if i32::try_from(*value).is_err() => {
                            return Err("extern integer literal is outside its i32 TypeDesc".into());
                        }
                        MirAbiClass::Integer {
                            bits: 32 | 64,
                            signed: true,
                        } => {}
                        _ => return Err("extern integer literal has non-integer TypeDesc".into()),
                    }
                }
                MirContractExpr::Bool(_) => {
                    if catalog
                        .get(&ty)
                        .is_none_or(|desc| desc.abi != MirAbiClass::Bool)
                    {
                        return Err("extern boolean literal has non-boolean TypeDesc".into());
                    }
                }
                MirContractExpr::Float(bits) => {
                    if catalog
                        .get(&ty)
                        .is_none_or(|desc| !matches!(desc.abi, MirAbiClass::Float { bits: 64 }))
                        || !f64::from_bits(*bits).is_finite()
                    {
                        return Err(
                            "extern float literal is not a finite f64 TypeDesc value".into()
                        );
                    }
                }
                MirContractExpr::String(_) => {
                    catalog
                        .validate_owned_string(&ty)
                        .map_err(|_| "extern string literal has non-String TypeDesc".to_string())?;
                }
                MirContractExpr::StringLength(operand) => {
                    path.push(0);
                    let operand_ty = visit(
                        operand,
                        path,
                        function,
                        catalog,
                        arguments,
                        parameter_types,
                        result,
                        result_type,
                        phase,
                        types,
                        visited,
                    )?;
                    path.pop();
                    catalog
                        .validate_owned_string(&operand_ty)
                        .map_err(|_| "extern len() operand is not an owned String".to_string())?;
                    if !matches!(kind(catalog, &ty, phase)?, ContractValueKind::Int) {
                        return Err("extern len() result is not an integer TypeDesc".into());
                    }
                }
                MirContractExpr::StringCharAt0(operand) => {
                    if phase != "requires" {
                        return Err(format!(
                            "extern {phase} char_at(0) is outside the typed declaration-contract subset"
                        ));
                    }
                    path.push(0);
                    let operand_ty = visit(
                        operand,
                        path,
                        function,
                        catalog,
                        arguments,
                        parameter_types,
                        result,
                        result_type,
                        phase,
                        types,
                        visited,
                    )?;
                    path.pop();
                    catalog.validate_owned_string(&operand_ty).map_err(|_| {
                        "extern char_at(0) operand is not an owned String".to_string()
                    })?;
                    catalog.validate_owned_string(&ty).map_err(|_| {
                        "extern char_at(0) result is not an owned String".to_string()
                    })?;
                }
                MirContractExpr::Project { base, projection } => {
                    path.push(0);
                    let base_ty = visit(
                        base,
                        path,
                        function,
                        catalog,
                        arguments,
                        parameter_types,
                        result,
                        result_type,
                        phase,
                        types,
                        visited,
                    )?;
                    path.pop();
                    if catalog.projection_result_type(&base_ty, projection)? != ty {
                        return Err("extern record/tuple projection TypeDesc disagrees".into());
                    }
                }
                MirContractExpr::Unary { op, operand } => {
                    path.push(0);
                    let operand_ty = visit(
                        operand,
                        path,
                        function,
                        catalog,
                        arguments,
                        parameter_types,
                        result,
                        result_type,
                        phase,
                        types,
                        visited,
                    )?;
                    path.pop();
                    let operand_kind = kind(catalog, &operand_ty, phase)?;
                    if operand_ty != ty
                        || !matches!(
                            (op, operand_kind),
                            (MirContractUnaryOp::Negate, ContractValueKind::Int)
                                | (MirContractUnaryOp::Negate, ContractValueKind::Float)
                                | (MirContractUnaryOp::Not, ContractValueKind::Bool)
                        )
                    {
                        return Err("extern unary predicate TypeDesc/operator mismatch".into());
                    }
                }
                MirContractExpr::Binary { op, left, right } => {
                    let mut operand_types = Vec::with_capacity(2);
                    for (index, operand) in [left, right].into_iter().enumerate() {
                        path.push(index as u8);
                        operand_types.push(visit(
                            operand,
                            path,
                            function,
                            catalog,
                            arguments,
                            parameter_types,
                            result,
                            result_type,
                            phase,
                            types,
                            visited,
                        )?);
                        path.pop();
                    }
                    let left_kind = kind(catalog, &operand_types[0], phase)?;
                    let right_kind = kind(catalog, &operand_types[1], phase)?;
                    let result_kind = kind(catalog, &ty, phase)?;
                    let integer_common =
                        ffi_integer_common_width(catalog, &operand_types[0], &operand_types[1]);
                    let valid = match op {
                        MirContractBinaryOp::Add
                        | MirContractBinaryOp::Subtract
                        | MirContractBinaryOp::Multiply
                        | MirContractBinaryOp::Divide
                        | MirContractBinaryOp::Remainder
                        | MirContractBinaryOp::Power => {
                            let integer_result_matches = integer_common.is_some_and(|common| {
                                ffi_integer_result_matches(catalog, &ty, common, &operand_types[0])
                            });
                            let same_float_type = operand_types[0] == operand_types[1]
                                && ty == operand_types[0]
                                && left_kind == ContractValueKind::Float
                                && right_kind == ContractValueKind::Float;
                            (integer_result_matches || same_float_type)
                                && !(left_kind == ContractValueKind::Float
                                    && matches!(
                                        op,
                                        MirContractBinaryOp::Remainder | MirContractBinaryOp::Power
                                    ))
                        }
                        MirContractBinaryOp::LogicalAnd | MirContractBinaryOp::LogicalOr => {
                            left_kind == ContractValueKind::Bool
                                && right_kind == ContractValueKind::Bool
                                && result_kind == ContractValueKind::Bool
                        }
                        MirContractBinaryOp::Equal | MirContractBinaryOp::NotEqual => {
                            let same_non_int_exact = operand_types[0] == operand_types[1]
                                && matches!(
                                    left_kind,
                                    ContractValueKind::Bool | ContractValueKind::String
                                );
                            let both_f64 = left_kind == ContractValueKind::Float
                                && right_kind == ContractValueKind::Float;
                            let mixed_float_int = matches!(
                                (left_kind, right_kind),
                                (ContractValueKind::Float, ContractValueKind::Int)
                                    | (ContractValueKind::Int, ContractValueKind::Float)
                            );
                            (integer_common.is_some()
                                || same_non_int_exact
                                || both_f64
                                || mixed_float_int)
                                && result_kind == ContractValueKind::Bool
                        }
                        MirContractBinaryOp::Less
                        | MirContractBinaryOp::Greater
                        | MirContractBinaryOp::LessEqual
                        | MirContractBinaryOp::GreaterEqual => {
                            let integer_comparison = integer_common.is_some();
                            let both_f64 = left_kind == ContractValueKind::Float
                                && right_kind == ContractValueKind::Float;
                            let mixed_float_int = matches!(
                                (left_kind, right_kind),
                                (ContractValueKind::Float, ContractValueKind::Int)
                                    | (ContractValueKind::Int, ContractValueKind::Float)
                            );
                            (integer_comparison || both_f64 || mixed_float_int)
                                && result_kind == ContractValueKind::Bool
                        }
                    };
                    if !valid {
                        return Err(format!(
                            "extern binary predicate TypeDesc/operator mismatch for {op:?}: left='{}', right='{}', result='{}'",
                            operand_types[0].as_str(),
                            operand_types[1].as_str(),
                            ty.as_str(),
                        ));
                    }
                }
                MirContractExpr::Result | MirContractExpr::Old(_) => {
                    return Err("extern predicate contains an invalid result/old marker".into());
                }
                MirContractExpr::Unsupported(_) => unreachable!("handled above"),
            }
            Ok(ty)
        }

        let mut visited = BTreeSet::new();
        let root_ty = visit(
            expression,
            &mut Vec::new(),
            function,
            catalog,
            arguments,
            parameter_types,
            result,
            result_type,
            phase,
            types,
            &mut visited,
        )?;
        if types.keys().any(|path| !visited.contains(path)) {
            return Err(format!(
                "extern {phase} TypeDesc receipt has an unused path"
            ));
        }
        if kind(catalog, &root_ty, phase)? != ContractValueKind::Bool {
            return Err(format!(
                "extern {phase} predicate must have boolean TypeDesc"
            ));
        }
        Ok(())
    }

    validate_root(
        function,
        catalog,
        &receipt.arguments,
        &receipt.parameter_types,
        receipt.result.as_ref(),
        &receipt.result_type,
        "requires",
        receipt.requires.as_ref(),
        &receipt.requires_types,
    )?;
    validate_root(
        function,
        catalog,
        &receipt.arguments,
        &receipt.parameter_types,
        receipt.result.as_ref(),
        &receipt.result_type,
        "ensures",
        receipt.ensures.as_ref(),
        &receipt.ensures_types,
    )
}

fn contains_string_char_at0(expression: &MirContractExpr) -> bool {
    match expression {
        MirContractExpr::StringCharAt0(_) => true,
        MirContractExpr::StringLength(value) => contains_string_char_at0(value),
        MirContractExpr::Project { base, .. } => contains_string_char_at0(base),
        MirContractExpr::Unary { operand, .. } => contains_string_char_at0(operand),
        MirContractExpr::Binary { left, right, .. } => {
            contains_string_char_at0(left) || contains_string_char_at0(right)
        }
        MirContractExpr::Value(_)
        | MirContractExpr::Result
        | MirContractExpr::Old(_)
        | MirContractExpr::Int(_)
        | MirContractExpr::Bool(_)
        | MirContractExpr::Float(_)
        | MirContractExpr::String(_)
        | MirContractExpr::Unsupported(_) => false,
    }
}

fn exact_guarded_char_at0_requires(expression: &MirContractExpr) -> Option<(&MirValueId, &str)> {
    let MirContractExpr::Binary {
        op: MirContractBinaryOp::LogicalAnd,
        left,
        right,
    } = expression
    else {
        return None;
    };
    let MirContractExpr::Binary {
        op: MirContractBinaryOp::Greater,
        left: length,
        right: zero,
    } = left.as_ref()
    else {
        return None;
    };
    if !matches!(zero.as_ref(), MirContractExpr::Int(0)) {
        return None;
    }
    let MirContractExpr::StringLength(length) = length.as_ref() else {
        return None;
    };
    let MirContractExpr::Value(length_parameter) = length.as_ref() else {
        return None;
    };
    let MirContractExpr::Binary {
        op: MirContractBinaryOp::Equal,
        left: char_side,
        right: literal_side,
    } = right.as_ref()
    else {
        return None;
    };
    let (character, literal) = match (char_side.as_ref(), literal_side.as_ref()) {
        (MirContractExpr::StringCharAt0(value), MirContractExpr::String(literal)) => {
            (value.as_ref(), literal.as_str())
        }
        (MirContractExpr::String(literal), MirContractExpr::StringCharAt0(value)) => {
            (value.as_ref(), literal.as_str())
        }
        _ => return None,
    };
    let MirContractExpr::Value(character_parameter) = character else {
        return None;
    };
    if character_parameter != length_parameter || !is_non_nul_scalar_literal(literal) {
        return None;
    }
    Some((length_parameter, literal))
}

fn is_non_nul_scalar_literal(literal: &str) -> bool {
    let mut characters = literal.chars();
    let Some(character) = characters.next() else {
        return false;
    };
    character != '\0' && (character as u32) <= 0x2ffff && characters.next().is_none()
}

fn ffi_declaration_char_at0_requires_is_supported(
    expression: &MirContractExpr,
    arguments: &[MirValueId],
    parameter_types: &[crate::core::ir::ResolvedTypeId],
    catalog: &MirTypeCatalog,
) -> bool {
    let Some((parameter, literal)) = exact_guarded_char_at0_requires(expression) else {
        return false;
    };
    let Some(index) = arguments.iter().position(|argument| argument == parameter) else {
        return false;
    };
    parameter_types.get(index).is_some_and(|ty| {
        is_non_nul_scalar_literal(literal) && catalog.is_canonical_ffi_verifier_string_leaf(ty)
    })
}

/// Restrict one verifier-only record FFI contract tree to scalar and owned
/// String predicates, scalar arithmetic/logic, and exact field projections
/// from checker-owned `repr(C)` records. The general MIR FFI validator runs
/// first and validates every path/type/projection; this second profile gate
/// keeps tuples, indexed projections, character extraction, and other shapes
/// out of the proof language. Checker-complete but unsupported expression
/// nodes remain receipt-bound so the FFI consumer can emit a per-phase NITS
/// result without discarding an independently provable sibling phase.
pub(crate) fn validate_ffi_record_verifier_contracts(
    catalog: &MirTypeCatalog,
    receipt: &super::MirFfiCallContract,
) -> Result<(), String> {
    fn validate_expression(
        expression: &MirContractExpr,
        phase: &str,
        allow_result: bool,
    ) -> Result<(), String> {
        match expression {
            MirContractExpr::Value(_)
            | MirContractExpr::Int(_)
            | MirContractExpr::Bool(_)
            | MirContractExpr::Float(_) => Ok(()),
            MirContractExpr::Project { base, .. } => validate_expression(base, phase, allow_result),
            MirContractExpr::String(_) => Ok(()),
            MirContractExpr::StringLength(operand) => {
                validate_expression(operand, phase, allow_result)
            }
            MirContractExpr::Unary { operand, .. } => {
                validate_expression(operand, phase, allow_result)
            }
            MirContractExpr::Binary { left, right, .. } => {
                validate_expression(left, phase, allow_result)?;
                validate_expression(right, phase, allow_result)
            }
            // The consumer's `ffi_contract_term` returns these as an
            // explicit NotInTrustedSubset result. Keep the declaration root
            // attached to its exact receipt so an unsupported Ensures does
            // not turn an otherwise provable Requires into an owner-level
            // materialization failure.
            MirContractExpr::StringCharAt0(_) | MirContractExpr::Unsupported(_) => Ok(()),
            MirContractExpr::Result if allow_result => Ok(()),
            MirContractExpr::Result | MirContractExpr::Old(_) => Err(format!(
                "verifier repr(C) record FFI {phase} contains a deferred or non-call-local expression"
            )),
        }
    }

    for (phase, expression, types) in [
        (
            "Requires",
            receipt.requires.as_ref(),
            &receipt.requires_types,
        ),
        ("Ensures", receipt.ensures.as_ref(), &receipt.ensures_types),
    ] {
        let Some(expression) = expression else {
            if !types.is_empty() {
                return Err(format!(
                    "verifier repr(C) record FFI {phase} has TypeDesc paths without a predicate"
                ));
            }
            continue;
        };
        // Ensures has one extra exact call-local value: this call's fresh MIR
        // result. Requires must remain a predicate over pre-call arguments,
        // and neither phase admits Old or arbitrary deferred roots.
        validate_expression(expression, phase, phase == "Ensures")?;
        if types.values().any(|ty| {
            !catalog
                .get(ty)
                .is_some_and(super::types::MirTypeDesc::is_canonical_ffi_verifier_scalar_leaf)
                && !catalog.is_canonical_ffi_verifier_record(ty)
                && !catalog.is_canonical_ffi_verifier_string_record(ty)
                && !catalog.is_canonical_ffi_verifier_string_leaf(ty)
        }) {
            return Err(format!(
                "verifier repr(C) record FFI {phase} TypeDesc path is outside the scalar-leaf record profile"
            ));
        }
    }
    Ok(())
}

/// Validate an extern precondition before any consumer sees the program.
/// Its value leaves must refer to this call's evaluated scalar arguments;
/// ordinary function-contract result/old/aggregate forms are not FFI facts.
pub(crate) fn validate_ffi_requires(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    receipt: &super::MirFfiCallContract,
) -> Result<(), String> {
    fn validate_leaves(
        expression: &MirContractExpr,
        arguments: &[MirValueId],
    ) -> Result<(), String> {
        match expression {
            MirContractExpr::Value(value) if !arguments.contains(value) => Err(format!(
                "extern requires value '{value}' is not a call argument"
            )),
            MirContractExpr::Value(_) | MirContractExpr::Int(_) | MirContractExpr::Bool(_) => {
                Ok(())
            }
            // A float literal is a literal leaf; its kind is rejected by the
            // contains_float_leaf guard that runs after this walk.
            MirContractExpr::Float(_) => Ok(()),
            MirContractExpr::Unary { operand, .. } => validate_leaves(operand, arguments),
            MirContractExpr::Binary { left, right, .. } => {
                validate_leaves(left, arguments)?;
                validate_leaves(right, arguments)
            }
            MirContractExpr::Result
            | MirContractExpr::Old(_)
            | MirContractExpr::Project { .. }
            | MirContractExpr::String(_)
            | MirContractExpr::StringLength(_)
            | MirContractExpr::StringCharAt0(_)
            | MirContractExpr::Unsupported(_) => Err(
                "extern requires must use pre-call scalar arguments, without result/old/projection"
                    .into(),
            ),
        }
    }
    let Some(condition) = &receipt.requires else {
        return Ok(());
    };
    validate_leaves(condition, &receipt.arguments)?;
    if contains_float_leaf(condition, function, catalog) {
        return Err(
            "extern requires float leaves are outside the runtime FFI predicate contract".into(),
        );
    }
    if expr_kind(condition, function, catalog)? != ContractValueKind::Bool {
        return Err("extern requires condition must be boolean".into());
    }
    Ok(())
}

/// Validate an extern postcondition before any consumer sees the program.
/// Its value leaves may refer to this call's evaluated scalar arguments and
/// to the call's scalar result value.  The latter is represented as a normal
/// MIR `Value` identity in the receipt, rather than the enclosing function's
/// `Result` marker, because the foreign result is local to this call-site.
pub(crate) fn validate_ffi_ensures(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    receipt: &super::MirFfiCallContract,
) -> Result<(), String> {
    fn validate_leaves(
        expression: &MirContractExpr,
        arguments: &[MirValueId],
        result: Option<&MirValueId>,
    ) -> Result<(), String> {
        match expression {
            MirContractExpr::Value(value)
                if !arguments.contains(value) && result != Some(value) => Err(format!(
                    "extern ensures value '{value}' is neither a call argument nor the call result"
                )),
            MirContractExpr::Value(_)
            | MirContractExpr::Int(_)
            | MirContractExpr::Bool(_) => Ok(()),
            // A float literal is a literal leaf; its kind is rejected by the
            // contains_float_leaf guard that runs after this walk.
            MirContractExpr::Float(_) => Ok(()),
            MirContractExpr::Unary { operand, .. } => {
                validate_leaves(operand, arguments, result)
            }
            MirContractExpr::Binary { left, right, .. } => {
                validate_leaves(left, arguments, result)?;
                validate_leaves(right, arguments, result)
            }
            MirContractExpr::Result
            | MirContractExpr::Old(_)
            | MirContractExpr::Project { .. }
            | MirContractExpr::String(_)
            | MirContractExpr::StringLength(_)
            | MirContractExpr::StringCharAt0(_)
            | MirContractExpr::Unsupported(_) => Err(
                "extern ensures must use scalar call arguments/result, without old/result marker/projection"
                    .into(),
            ),
        }
    }
    let Some(condition) = &receipt.ensures else {
        return Ok(());
    };
    validate_leaves(condition, &receipt.arguments, receipt.result.as_ref())?;
    if contains_float_leaf(condition, function, catalog) {
        return Err(
            "extern ensures float leaves are outside the runtime FFI predicate contract".into(),
        );
    }
    if expr_kind(condition, function, catalog)? != ContractValueKind::Bool {
        return Err("extern ensures condition must be boolean".into());
    }
    Ok(())
}

/// Validate the shape of a materialized bytecode FFI predicate without a
/// `MirFunction` or type catalog.  The bytecode adapter still performs the
/// richer checker-owned receipt validation, but a hand-built
/// `CanonicalFfiDescriptor` must not be able to run a foreign call before an
/// unknown identity or an ill-typed predicate is rejected.
pub(crate) fn validate_ffi_runtime_contracts(
    requires: Option<&MirContractExpr>,
    ensures: Option<&MirContractExpr>,
    arguments: &[MirValueId],
    argument_abis: &[MirAbiClass],
    result: Option<&MirValueId>,
    result_abi: MirAbiClass,
) -> Result<(), String> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum RuntimeContractKind {
        Int,
        Bool,
    }

    fn scalar_kind(abi: MirAbiClass, phase: &str) -> Result<RuntimeContractKind, String> {
        match abi {
            MirAbiClass::Integer {
                bits: 32 | 64,
                signed: true,
            } => Ok(RuntimeContractKind::Int),
            MirAbiClass::Bool => Ok(RuntimeContractKind::Bool),
            _ => Err(format!(
                "extern {phase} value ABI {abi:?} is outside the scalar predicate contract"
            )),
        }
    }

    fn expression_kind(
        expression: &MirContractExpr,
        phase: &str,
        arguments: &[MirValueId],
        argument_abis: &[MirAbiClass],
        result: Option<&MirValueId>,
        result_abi: MirAbiClass,
    ) -> Result<RuntimeContractKind, String> {
        match expression {
            MirContractExpr::Value(value) => {
                if let Some(index) = arguments.iter().position(|argument| argument == value) {
                    let abi = argument_abis.get(index).copied().ok_or_else(|| {
                        format!("extern {phase} argument ABI metadata is absent at index {index}")
                    })?;
                    return scalar_kind(abi, phase);
                }
                if phase == "postcondition" && result == Some(value) {
                    return scalar_kind(result_abi, phase);
                }
                if phase == "precondition" {
                    Err(format!(
                        "extern requires value '{value}' is not a call argument"
                    ))
                } else {
                    Err(format!(
                        "extern ensures value '{value}' is neither a call argument nor the call result"
                    ))
                }
            }
            MirContractExpr::Int(_) => Ok(RuntimeContractKind::Int),
            MirContractExpr::Bool(_) => Ok(RuntimeContractKind::Bool),
            MirContractExpr::Float(_) => Err(format!(
                "extern {phase} predicate cannot evaluate float literals"
            )),
            MirContractExpr::String(_)
            | MirContractExpr::StringLength(_)
            | MirContractExpr::StringCharAt0(_)
            | MirContractExpr::Unsupported(_) => Err(format!(
                "extern {phase} expression is outside the runtime scalar predicate contract"
            )),
            MirContractExpr::Unary { op, operand } => {
                let kind =
                    expression_kind(operand, phase, arguments, argument_abis, result, result_abi)?;
                match (op, kind) {
                    (MirContractUnaryOp::Negate, RuntimeContractKind::Int)
                    | (MirContractUnaryOp::Not, RuntimeContractKind::Bool) => Ok(kind),
                    _ => Err(format!(
                        "extern {phase} unary operator has an incompatible scalar operand"
                    )),
                }
            }
            MirContractExpr::Binary { op, left, right } => {
                let left_kind =
                    expression_kind(left, phase, arguments, argument_abis, result, result_abi)?;
                let right_kind =
                    expression_kind(right, phase, arguments, argument_abis, result, result_abi)?;
                match op {
                    MirContractBinaryOp::Add
                    | MirContractBinaryOp::Subtract
                    | MirContractBinaryOp::Multiply
                    | MirContractBinaryOp::Divide
                    | MirContractBinaryOp::Remainder
                        if left_kind == RuntimeContractKind::Int
                            && right_kind == RuntimeContractKind::Int =>
                    {
                        Ok(RuntimeContractKind::Int)
                    }
                    MirContractBinaryOp::LogicalAnd | MirContractBinaryOp::LogicalOr
                        if left_kind == RuntimeContractKind::Bool
                            && right_kind == RuntimeContractKind::Bool =>
                    {
                        Ok(RuntimeContractKind::Bool)
                    }
                    MirContractBinaryOp::Equal | MirContractBinaryOp::NotEqual
                        if left_kind == right_kind =>
                    {
                        Ok(RuntimeContractKind::Bool)
                    }
                    MirContractBinaryOp::Less
                    | MirContractBinaryOp::Greater
                    | MirContractBinaryOp::LessEqual
                    | MirContractBinaryOp::GreaterEqual
                        if left_kind == RuntimeContractKind::Int
                            && right_kind == RuntimeContractKind::Int =>
                    {
                        Ok(RuntimeContractKind::Bool)
                    }
                    MirContractBinaryOp::Add
                    | MirContractBinaryOp::Subtract
                    | MirContractBinaryOp::Multiply
                    | MirContractBinaryOp::Divide
                    | MirContractBinaryOp::Remainder => Err(format!(
                        "extern {phase} arithmetic requires integer operands"
                    )),
                    MirContractBinaryOp::Power => Err(format!(
                        "extern {phase} predicate cannot evaluate checked integer power"
                    )),
                    MirContractBinaryOp::LogicalAnd | MirContractBinaryOp::LogicalOr => Err(
                        format!("extern {phase} logical operator requires boolean operands"),
                    ),
                    MirContractBinaryOp::Equal | MirContractBinaryOp::NotEqual => Err(format!(
                        "extern {phase} equality operands have incompatible scalar types"
                    )),
                    MirContractBinaryOp::Less
                    | MirContractBinaryOp::Greater
                    | MirContractBinaryOp::LessEqual
                    | MirContractBinaryOp::GreaterEqual => {
                        Err(format!("extern {phase} ordering requires integer operands"))
                    }
                }
            }
            MirContractExpr::Result | MirContractExpr::Old(_) | MirContractExpr::Project { .. } => {
                Err(format!(
                    "extern {phase} must use scalar call arguments/result, without old/result marker/projection"
                ))
            }
        }
    }

    if arguments.len() != argument_abis.len() {
        return Err("extern FFI runtime argument ABI metadata arity mismatch".into());
    }
    for (phase, condition) in [("precondition", requires), ("postcondition", ensures)] {
        let Some(condition) = condition else {
            continue;
        };
        if expression_kind(
            condition,
            phase,
            arguments,
            argument_abis,
            result,
            result_abi,
        )? != RuntimeContractKind::Bool
        {
            return Err(format!("extern {phase} condition must be boolean"));
        }
    }
    Ok(())
}

/// Validate function predicates independently of Z3 and backend ABI.
pub(crate) fn validate_contracts(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
) -> Vec<MirValidationError> {
    let mut errors = Vec::new();
    let has_extern_call = function.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                &instruction.kind,
                super::MirInstructionKind::Call {
                    callee: crate::core::ir::ResolvedCallee::Extern(_),
                    ..
                }
            )
        })
    });
    for (index, contract) in function.contracts.iter().enumerate() {
        let subject = format!("contract[{index}] {}", contract.id.0.as_str());
        let supported_ffi_string_guard = contract.kind == MirContractKind::Requires
            && has_extern_call
            && (is_direct_nonempty_string_parameter_requires(contract, function, catalog)
                || direct_string_length_parameter_requires(contract, function, catalog).is_some()
                || direct_string_char_at0_parameter_requires(contract, function, catalog)
                    .is_some()
                || aggregate_string_length_parameter_requires(contract, function, catalog)
                    .is_some()
                || aggregate_string_literal_equality_requires(contract, function, catalog));
        let supported_ordinary_string_length_guard = contract.kind == MirContractKind::Requires
            && !has_extern_call
            && direct_string_length_scalar_requires(&contract.condition, function, catalog);
        if contract_contains_string_expression(&contract.condition, function, catalog)
            && !supported_ffi_string_guard
            && !supported_ordinary_string_length_guard
        {
            let message = if contract.kind == MirContractKind::Requires && has_extern_call {
                "String-valued FFI caller predicates remain outside the trusted subset except for direct parameter != empty-literal guards, direct len(parameter) > 0 / == 0 guards, guarded direct char_at(parameter, 0) equality to one non-NUL Unicode scalar, one direct repr(C) String-field length guard, and one exact String-literal equality over a checked repr(C) field path in a scalar conjunction; foreign String ingress validates UTF-8"
            } else {
                "String predicates are outside this canonical MIR contract profile"
            };
            errors.push(MirValidationError {
                subject,
                message: message.into(),
            });
            continue;
        }
        if let Err(message) = validate_expression_types(contract, function, catalog) {
            errors.push(MirValidationError {
                subject: subject.clone(),
                message,
            });
        }
        let kind = match expr_kind(&contract.condition, function, catalog) {
            Ok(kind) => kind,
            Err(message) => {
                errors.push(MirValidationError { subject, message });
                continue;
            }
        };
        if kind != ContractValueKind::Bool {
            errors.push(MirValidationError {
                subject: format!("contract[{index}] {}", contract.id.0.as_str()),
                message: "contract condition must be boolean".into(),
            });
        }
        if matches!(
            contract.kind,
            MirContractKind::Requires | MirContractKind::Invariant
        ) && contains_result(&contract.condition)
        {
            errors.push(MirValidationError {
                subject: format!("contract[{index}] {}", contract.id.0.as_str()),
                message: "requires/invariant contract cannot reference the function result".into(),
            });
        }
        if contains_invalid_old(function, &contract.condition) {
            errors.push(MirValidationError {
                subject: format!("contract[{index}] {}", contract.id.0.as_str()),
                message: "old() must reference a callable parameter value".into(),
            });
        }
    }
    errors
}

/// Recognize ordinary non-FFI `requires` predicates whose only String operation
/// is `len(parameter)` over a direct checked String parameter combined cleanly
/// with scalar arithmetic/comparisons. String literals, `char_at`, locals, and
/// FFI caller guards keep their dedicated stricter gates.
fn direct_string_length_scalar_requires(
    expression: &MirContractExpr,
    function: &MirFunction,
    catalog: &MirTypeCatalog,
) -> bool {
    match expression {
        MirContractExpr::StringLength(operand) => matches!(
            operand.as_ref(),
            MirContractExpr::Value(parameter)
                if function.parameters.contains(parameter)
                    && function
                        .values
                        .get(parameter)
                        .is_some_and(|value| catalog.validate_owned_string(&value.ty).is_ok())
        ),
        MirContractExpr::Value(value) | MirContractExpr::Old(value) => !function
            .values
            .get(value)
            .is_some_and(|value| catalog.validate_owned_string(&value.ty).is_ok()),
        MirContractExpr::Result => catalog.validate_owned_string(&function.result).is_err(),
        MirContractExpr::Project { base, .. } => {
            expression_type(expression, function, catalog)
                .is_ok_and(|ty| catalog.validate_owned_string(&ty).is_err())
                && direct_string_length_scalar_requires(base, function, catalog)
        }
        MirContractExpr::Unary { operand, .. } => {
            direct_string_length_scalar_requires(operand, function, catalog)
        }
        MirContractExpr::Binary { left, right, .. } => {
            direct_string_length_scalar_requires(left, function, catalog)
                && direct_string_length_scalar_requires(right, function, catalog)
        }
        MirContractExpr::Int(_) | MirContractExpr::Bool(_) | MirContractExpr::Float(_) => true,
        MirContractExpr::String(_)
        | MirContractExpr::StringCharAt0(_)
        | MirContractExpr::Unsupported(_) => false,
    }
}

/// Recognize the closed FFI caller profile `len(parameter) > 0` or
/// `len(parameter) == 0` (including the symmetric comparison order).
/// The parameter must be a direct callable String parameter; locals,
/// projections, arithmetic, conjunctions, and other String predicates stay
/// outside this proof slice. The bool is true for the nonempty predicate and
/// false for the exact empty predicate.
pub(crate) fn direct_string_length_parameter_requires<'a>(
    contract: &'a MirContract,
    function: &MirFunction,
    catalog: &MirTypeCatalog,
) -> Option<(&'a MirValueId, bool)> {
    if contract.kind != MirContractKind::Requires {
        return None;
    }
    let MirContractExpr::Binary { op, left, right } = &contract.condition else {
        return None;
    };
    let (parameter, nonempty) = match (op, left.as_ref(), right.as_ref()) {
        (
            MirContractBinaryOp::Greater,
            MirContractExpr::StringLength(value),
            MirContractExpr::Int(0),
        )
        | (
            MirContractBinaryOp::Less,
            MirContractExpr::Int(0),
            MirContractExpr::StringLength(value),
        ) => (value.as_ref(), true),
        (
            MirContractBinaryOp::Equal,
            MirContractExpr::StringLength(value),
            MirContractExpr::Int(0),
        )
        | (
            MirContractBinaryOp::Equal,
            MirContractExpr::Int(0),
            MirContractExpr::StringLength(value),
        ) => (value.as_ref(), false),
        _ => return None,
    };
    let MirContractExpr::Value(parameter) = parameter else {
        return None;
    };
    if !function.parameters.contains(parameter)
        || !function
            .values
            .get(parameter)
            .is_some_and(|value| catalog.validate_owned_string(&value.ty).is_ok())
    {
        return None;
    }
    Some((parameter, nonempty))
}

/// Recognize only the exact caller context
/// len(parameter) > 0 && char_at(parameter, 0) == scalar, with both leaves
/// bound to the same direct checked String parameter.  The non-NUL scalar
/// restriction preserves native C-string result semantics for char_at at
/// index zero; all other orderings, operators, and projections stay closed.
pub(crate) fn direct_string_char_at0_parameter_requires<'a>(
    contract: &'a MirContract,
    function: &MirFunction,
    catalog: &MirTypeCatalog,
) -> Option<(&'a MirValueId, &'a str)> {
    if contract.kind != MirContractKind::Requires {
        return None;
    }
    let (parameter, literal) = exact_guarded_char_at0_requires(&contract.condition)?;
    if !function.parameters.contains(parameter)
        || !function
            .values
            .get(parameter)
            .is_some_and(|value| catalog.is_canonical_ffi_verifier_string_leaf(&value.ty))
    {
        return None;
    }
    Some((parameter, literal))
}

/// Recognize the aggregate FFI caller shape `scalar && len(record.field) > 0`
/// (or the exact `== 0` counterpart).  The record must be a direct callable
/// parameter with one canonical String field and only scalar siblings.  The
/// complete conjunction tree is checked so no other String predicate can
/// hitchhike on the one admitted length guard.
fn aggregate_string_length_parameter_requires<'a>(
    contract: &'a MirContract,
    function: &MirFunction,
    catalog: &MirTypeCatalog,
) -> Option<(&'a MirValueId, &'a MirProjection, bool)> {
    if contract.kind != MirContractKind::Requires
        || !matches!(
            &contract.condition,
            MirContractExpr::Binary {
                op: MirContractBinaryOp::LogicalAnd,
                ..
            }
        )
    {
        return None;
    }

    fn exact_length_guard(
        expression: &MirContractExpr,
    ) -> Option<(&MirValueId, &MirProjection, bool)> {
        let MirContractExpr::Binary { op, left, right } = expression else {
            return None;
        };
        let (length, nonempty) = match (op, left.as_ref(), right.as_ref()) {
            (
                MirContractBinaryOp::Greater,
                MirContractExpr::StringLength(value),
                MirContractExpr::Int(0),
            )
            | (
                MirContractBinaryOp::Less,
                MirContractExpr::Int(0),
                MirContractExpr::StringLength(value),
            ) => (value.as_ref(), true),
            (
                MirContractBinaryOp::Equal,
                MirContractExpr::StringLength(value),
                MirContractExpr::Int(0),
            )
            | (
                MirContractBinaryOp::Equal,
                MirContractExpr::Int(0),
                MirContractExpr::StringLength(value),
            ) => (value.as_ref(), false),
            _ => return None,
        };
        let MirContractExpr::Project { base, projection } = length else {
            return None;
        };
        let MirContractExpr::Value(parameter) = base.as_ref() else {
            return None;
        };
        Some((parameter, projection, nonempty))
    }

    fn is_one_string_field_record_parameter(
        parameter: &MirValueId,
        projection: &MirProjection,
        function: &MirFunction,
        catalog: &MirTypeCatalog,
    ) -> bool {
        let MirProjection::Field(field_id) = projection else {
            return false;
        };
        if !function.parameters.contains(parameter) {
            return false;
        }
        let Some(parameter_value) = function.values.get(parameter) else {
            return false;
        };
        if !catalog.is_canonical_ffi_verifier_string_record(&parameter_value.ty) {
            return false;
        }
        let Some(descriptor) = catalog.get(&parameter_value.ty) else {
            return false;
        };
        let MirLayout::Record { fields, .. } = &descriptor.layout else {
            return false;
        };
        let string_fields = fields
            .iter()
            .filter(|field| catalog.is_canonical_ffi_verifier_string_leaf(&field.ty))
            .collect::<Vec<_>>();
        if string_fields.len() != 1
            || string_fields[0].id != *field_id
            || fields.iter().any(|field| {
                !catalog
                    .get(&field.ty)
                    .is_some_and(super::types::MirTypeDesc::is_canonical_ffi_verifier_scalar_leaf)
                    && !catalog.is_canonical_ffi_verifier_string_leaf(&field.ty)
            })
        {
            return false;
        }
        catalog
            .projection_result_type(&parameter_value.ty, projection)
            .is_ok_and(|field_ty| catalog.is_canonical_ffi_verifier_string_leaf(&field_ty))
    }

    fn scan_conjunction<'a>(
        expression: &'a MirContractExpr,
        function: &MirFunction,
        catalog: &MirTypeCatalog,
        found_guard: &mut Option<(&'a MirValueId, &'a MirProjection, bool)>,
    ) -> bool {
        if let MirContractExpr::Binary {
            op: MirContractBinaryOp::LogicalAnd,
            left,
            right,
        } = expression
        {
            return scan_conjunction(left, function, catalog, found_guard)
                && scan_conjunction(right, function, catalog, found_guard);
        }
        if let Some((parameter, projection, is_nonempty)) = exact_length_guard(expression) {
            if found_guard.is_some()
                || !is_one_string_field_record_parameter(parameter, projection, function, catalog)
            {
                return false;
            }
            *found_guard = Some((parameter, projection, is_nonempty));
            return true;
        }
        !contract_contains_string_expression(expression, function, catalog)
    }

    let mut found_guard = None;
    if scan_conjunction(&contract.condition, function, catalog, &mut found_guard) {
        found_guard
    } else {
        None
    }
}

/// Recognize one exact String-literal equality over a checked `repr(C)` field
/// path inside a scalar conjunction. This supports equality facts such as a
/// nested UTF-8/NUL-bearing record field while keeping general String
/// comparisons, multiple String leaves, and computed String values closed.
fn aggregate_string_literal_equality_requires(
    contract: &MirContract,
    function: &MirFunction,
    catalog: &MirTypeCatalog,
) -> bool {
    if contract.kind != MirContractKind::Requires
        || !matches!(
            &contract.condition,
            MirContractExpr::Binary {
                op: MirContractBinaryOp::LogicalAnd,
                ..
            }
        )
    {
        return false;
    }

    fn string_field_parameter(expression: &MirContractExpr) -> Option<(&MirValueId, Vec<NodeId>)> {
        match expression {
            MirContractExpr::Value(parameter) => Some((parameter, Vec::new())),
            MirContractExpr::Project { base, projection } => {
                let MirProjection::Field(field) = projection else {
                    return None;
                };
                let (parameter, mut fields) = string_field_parameter(base)?;
                fields.push(field.clone());
                Some((parameter, fields))
            }
            _ => None,
        }
    }

    fn is_canonical_string_field_path(
        parameter: &MirValueId,
        fields: &[NodeId],
        function: &MirFunction,
        catalog: &MirTypeCatalog,
    ) -> bool {
        if fields.is_empty() || !function.parameters.contains(parameter) {
            return false;
        }
        let Some(parameter_value) = function.values.get(parameter) else {
            return false;
        };
        if !catalog.is_canonical_ffi_verifier_string_record(&parameter_value.ty) {
            return false;
        }
        let mut current_ty = parameter_value.ty.clone();
        for (index, field) in fields.iter().enumerate() {
            let projection = MirProjection::Field(field.clone());
            let Ok(projected_ty) = catalog.projection_result_type(&current_ty, &projection) else {
                return false;
            };
            if index + 1 == fields.len() {
                return catalog.is_canonical_ffi_verifier_string_leaf(&projected_ty);
            }
            if !catalog.is_canonical_ffi_verifier_string_record(&projected_ty) {
                return false;
            }
            current_ty = projected_ty;
        }
        false
    }

    fn exact_string_equality<'a>(
        expression: &'a MirContractExpr,
        function: &MirFunction,
        catalog: &MirTypeCatalog,
    ) -> Option<&'a MirContractExpr> {
        let MirContractExpr::Binary {
            op: MirContractBinaryOp::Equal,
            left,
            right,
        } = expression
        else {
            return None;
        };
        let field = match (left.as_ref(), right.as_ref()) {
            (field @ MirContractExpr::Project { .. }, MirContractExpr::String(_))
            | (MirContractExpr::String(_), field @ MirContractExpr::Project { .. }) => field,
            _ => return None,
        };
        let (parameter, fields) = string_field_parameter(field)?;
        is_canonical_string_field_path(parameter, &fields, function, catalog).then_some(expression)
    }

    fn scan_conjunction(
        expression: &MirContractExpr,
        function: &MirFunction,
        catalog: &MirTypeCatalog,
        found_guard: &mut bool,
    ) -> bool {
        if let MirContractExpr::Binary {
            op: MirContractBinaryOp::LogicalAnd,
            left,
            right,
        } = expression
        {
            return scan_conjunction(left, function, catalog, found_guard)
                && scan_conjunction(right, function, catalog, found_guard);
        }
        if exact_string_equality(expression, function, catalog).is_some() {
            if *found_guard {
                return false;
            }
            *found_guard = true;
            return true;
        }
        !contract_contains_string_expression(expression, function, catalog)
    }

    let mut found_guard = false;
    scan_conjunction(&contract.condition, function, catalog, &mut found_guard) && found_guard
}

/// Recognize only `parameter != ""` (or the symmetric operand order) on a
/// caller Requires that is being used by verifier-only FFI reasoning.  The
/// canonical value identity must be one of the callable's direct parameters,
/// and its TypeDesc must be a valid Mimi-owned String.  Locals, projections,
/// old/result leaves, conjunctions, nonempty literals, Ensures, and every
/// other String-valued predicate remain outside this trusted subset.
fn is_direct_nonempty_string_parameter_requires(
    contract: &MirContract,
    function: &MirFunction,
    catalog: &MirTypeCatalog,
) -> bool {
    if contract.kind != MirContractKind::Requires {
        return false;
    }
    let MirContractExpr::Binary {
        op: MirContractBinaryOp::NotEqual,
        left,
        right,
    } = &contract.condition
    else {
        return false;
    };

    let parameter = match (left.as_ref(), right.as_ref()) {
        (MirContractExpr::Value(value), MirContractExpr::String(literal)) if literal.is_empty() => {
            value
        }
        (MirContractExpr::String(literal), MirContractExpr::Value(value)) if literal.is_empty() => {
            value
        }
        _ => return false,
    };
    if !function.parameters.contains(parameter) {
        return false;
    }
    function
        .values
        .get(parameter)
        .is_some_and(|value| catalog.validate_owned_string(&value.ty).is_ok())
}

fn contract_contains_string_expression(
    expression: &MirContractExpr,
    function: &MirFunction,
    catalog: &MirTypeCatalog,
) -> bool {
    match expression {
        MirContractExpr::Value(value) | MirContractExpr::Old(value) => function
            .values
            .get(value)
            .is_some_and(|value| catalog.validate_owned_string(&value.ty).is_ok()),
        MirContractExpr::Result => catalog.validate_owned_string(&function.result).is_ok(),
        MirContractExpr::Project { base, .. } => {
            expression_type(expression, function, catalog)
                .is_ok_and(|ty| catalog.validate_owned_string(&ty).is_ok())
                || contract_contains_string_expression(base, function, catalog)
        }
        MirContractExpr::String(_)
        | MirContractExpr::StringLength(_)
        | MirContractExpr::StringCharAt0(_) => true,
        MirContractExpr::Unary { operand, .. } => {
            contract_contains_string_expression(operand, function, catalog)
        }
        MirContractExpr::Binary { left, right, .. } => {
            contract_contains_string_expression(left, function, catalog)
                || contract_contains_string_expression(right, function, catalog)
        }
        MirContractExpr::Int(_)
        | MirContractExpr::Bool(_)
        | MirContractExpr::Float(_)
        | MirContractExpr::Unsupported(_) => false,
    }
}

fn contains_result(expression: &MirContractExpr) -> bool {
    match expression {
        MirContractExpr::Result => true,
        MirContractExpr::StringCharAt0(operand) => contains_result(operand),
        MirContractExpr::Unary { operand, .. } => contains_result(operand),
        MirContractExpr::Binary { left, right, .. } => {
            contains_result(left) || contains_result(right)
        }
        MirContractExpr::Project { base, .. } => contains_result(base),
        MirContractExpr::Value(_)
        | MirContractExpr::Old(_)
        | MirContractExpr::Float(_)
        | MirContractExpr::Int(_)
        | MirContractExpr::Bool(_)
        | MirContractExpr::String(_)
        | MirContractExpr::StringLength(_)
        | MirContractExpr::Unsupported(_) => false,
    }
}

/// Runtime values of the scalar FFI predicate language. Integer arguments are
/// sign-extended to the canonical checked i64 slot before predicate arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MirContractScalar {
    Int(i64),
    Bool(bool),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MirFfiContractError {
    Invalid(String),
    Violation,
    Overflow,
    DivisionByZero,
}

impl From<&str> for MirFfiContractError {
    fn from(message: &str) -> Self {
        Self::Invalid(message.to_string())
    }
}

impl From<String> for MirFfiContractError {
    fn from(message: String) -> Self {
        Self::Invalid(message)
    }
}

impl std::fmt::Display for MirFfiContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => f.write_str(message),
            Self::Violation => f.write_str("[E0808] FFI precondition failed"),
            Self::Overflow => f.write_str("[E0802] integer overflow in FFI precondition"),
            Self::DivisionByZero => f.write_str("[E0801] division by zero in FFI precondition"),
        }
    }
}

/// Render a checked FFI predicate error for either the pre-call or post-call
/// phase.  `Display` remains pinned to the historical precondition wording so
/// existing callers/tests retain their diagnostics; new consumers use this
/// phase-aware helper for postconditions.
pub(crate) fn ffi_contract_error_message(error: &MirFfiContractError, phase: &str) -> String {
    match error {
        MirFfiContractError::Invalid(message) => message.clone(),
        MirFfiContractError::Violation => format!("[E0808] FFI {phase} failed"),
        MirFfiContractError::Overflow => {
            format!("[E0802] integer overflow in FFI {phase}")
        }
        MirFfiContractError::DivisionByZero => {
            format!("[E0801] division by zero in FFI {phase}")
        }
    }
}

pub(crate) fn evaluate_ffi_requires(
    condition: &MirContractExpr,
    argument: impl FnMut(&MirValueId) -> Result<MirContractScalar, String>,
) -> Result<(), MirFfiContractError> {
    evaluate_ffi_contract(condition, argument, "precondition")
}

pub(crate) fn evaluate_ffi_ensures(
    condition: &MirContractExpr,
    argument: impl FnMut(&MirValueId) -> Result<MirContractScalar, String>,
) -> Result<(), MirFfiContractError> {
    evaluate_ffi_contract(condition, argument, "postcondition")
}

fn evaluate_ffi_contract(
    condition: &MirContractExpr,
    mut argument: impl FnMut(&MirValueId) -> Result<MirContractScalar, String>,
    phase: &str,
) -> Result<(), MirFfiContractError> {
    fn evaluate(
        expression: &MirContractExpr,
        argument: &mut impl FnMut(&MirValueId) -> Result<MirContractScalar, String>,
        phase: &str,
    ) -> Result<MirContractScalar, MirFfiContractError> {
        use MirContractBinaryOp as Op;
        use MirContractScalar::{Bool, Int};
        match expression {
            MirContractExpr::Value(id) => argument(id).map_err(MirFfiContractError::Invalid),
            MirContractExpr::Int(value) => Ok(Int(*value)),
            MirContractExpr::Bool(value) => Ok(Bool(*value)),
            // R6-1065: rejected at construction by `contains_float_leaf`;
            // this arm keeps the evaluator total for programs that bypass
            // the constructor validation.
            MirContractExpr::Float(_) => {
                Err(format!("FFI {phase} predicate cannot evaluate float leaves").into())
            }
            MirContractExpr::String(_)
            | MirContractExpr::StringLength(_)
            | MirContractExpr::StringCharAt0(_)
            | MirContractExpr::Unsupported(_) => Err(format!(
                "FFI {phase} expression is outside the runtime scalar predicate evaluator"
            )
            .into()),
            MirContractExpr::Unary { op, operand } => {
                match (op, evaluate(operand, argument, phase)?) {
                    (MirContractUnaryOp::Not, Bool(value)) => Ok(Bool(!value)),
                    (MirContractUnaryOp::Negate, Int(value)) => value
                        .checked_neg()
                        .map(Int)
                        .ok_or(MirFfiContractError::Overflow),
                    _ => Err(format!("FFI {phase} unary operand type mismatch").into()),
                }
            }
            MirContractExpr::Binary { op, left, right } => {
                let left = evaluate(left, argument, phase)?;
                // Contract conjunction/disjunction retain source short-circuit
                // semantics, including suppression of unreachable arithmetic traps.
                match (op, left) {
                    (Op::LogicalAnd, Bool(false)) => return Ok(Bool(false)),
                    (Op::LogicalOr, Bool(true)) => return Ok(Bool(true)),
                    _ => {}
                }
                let right = evaluate(right, argument, phase)?;
                match (op, left, right) {
                    (Op::Add, Int(a), Int(b)) => a
                        .checked_add(b)
                        .map(Int)
                        .ok_or(MirFfiContractError::Overflow),
                    (Op::Subtract, Int(a), Int(b)) => a
                        .checked_sub(b)
                        .map(Int)
                        .ok_or(MirFfiContractError::Overflow),
                    (Op::Multiply, Int(a), Int(b)) => a
                        .checked_mul(b)
                        .map(Int)
                        .ok_or(MirFfiContractError::Overflow),
                    (Op::Divide | Op::Remainder, Int(_), Int(0)) => {
                        Err(MirFfiContractError::DivisionByZero)
                    }
                    (Op::Divide, Int(a), Int(b)) => a
                        .checked_div(b)
                        .map(Int)
                        .ok_or(MirFfiContractError::Overflow),
                    (Op::Remainder, Int(a), Int(b)) => a
                        .checked_rem(b)
                        .map(Int)
                        .ok_or(MirFfiContractError::Overflow),
                    (Op::Equal, Int(a), Int(b)) => Ok(Bool(a == b)),
                    (Op::NotEqual, Int(a), Int(b)) => Ok(Bool(a != b)),
                    (Op::Equal, Bool(a), Bool(b)) => Ok(Bool(a == b)),
                    (Op::NotEqual, Bool(a), Bool(b)) => Ok(Bool(a != b)),
                    (Op::Less, Int(a), Int(b)) => Ok(Bool(a < b)),
                    (Op::LessEqual, Int(a), Int(b)) => Ok(Bool(a <= b)),
                    (Op::Greater, Int(a), Int(b)) => Ok(Bool(a > b)),
                    (Op::GreaterEqual, Int(a), Int(b)) => Ok(Bool(a >= b)),
                    (Op::LogicalAnd, Bool(a), Bool(b)) => Ok(Bool(a && b)),
                    (Op::LogicalOr, Bool(a), Bool(b)) => Ok(Bool(a || b)),
                    _ => Err(format!("FFI {phase} binary operand type mismatch").into()),
                }
            }
            MirContractExpr::Result | MirContractExpr::Old(_) | MirContractExpr::Project { .. } => {
                Err(format!("unsupported FFI {phase} expression").into())
            }
        }
    }
    match evaluate(condition, &mut argument, phase)? {
        MirContractScalar::Bool(true) => Ok(()),
        MirContractScalar::Bool(false) => Err(MirFfiContractError::Violation),
        _ => Err(format!("FFI {phase} must return bool").into()),
    }
}

fn contains_invalid_old(function: &MirFunction, expression: &MirContractExpr) -> bool {
    match expression {
        MirContractExpr::Old(value) => !function.parameters.contains(value),
        MirContractExpr::Unary { operand, .. } => contains_invalid_old(function, operand),
        MirContractExpr::Binary { left, right, .. } => {
            contains_invalid_old(function, left) || contains_invalid_old(function, right)
        }
        MirContractExpr::Project { base, .. } => contains_invalid_old(function, base),
        MirContractExpr::StringCharAt0(operand) => contains_invalid_old(function, operand),
        MirContractExpr::Value(_)
        | MirContractExpr::Result
        | MirContractExpr::Int(_)
        | MirContractExpr::Bool(_)
        | MirContractExpr::Float(_)
        | MirContractExpr::String(_)
        | MirContractExpr::StringLength(_)
        | MirContractExpr::Unsupported(_) => false,
    }
}

fn lower_projection(projection: &ResolvedProjection) -> Result<MirProjection, String> {
    match projection {
        ResolvedProjection::Field { field, .. } => Ok(MirProjection::Field(field.clone())),
        ResolvedProjection::Tuple { index, .. } => Ok(MirProjection::Tuple(*index)),
        ResolvedProjection::Index { .. } => {
            Err("contract indexed projection is outside the canonical aggregate contract".into())
        }
        ResolvedProjection::Deref { .. } => Err(
            "contract dereference projection is outside the canonical aggregate contract".into(),
        ),
    }
}

fn lower_contract_place(
    place: &crate::core::ResolvedPlace,
    function: &MirFunction,
    body: &crate::core::ir::ResolvedBody,
    old: bool,
) -> Result<MirContractExpr, String> {
    let value =
        MirValueId::new(format!("local:{}", place.base.0 .0)).map_err(|error| error.to_string())?;
    let mut base = if old {
        if place.base.0 .0.ends_with("/contract-result/local")
            || !body.parameters.contains(&place.base)
        {
            return Err("old() requires a direct callable parameter load".into());
        }
        if !function.values.contains_key(&value) {
            return Err(format!(
                "old() parameter '{}' is absent from canonical MIR values",
                place.base.0 .0
            ));
        }
        MirContractExpr::Old(value)
    } else if place.base.0 .0.ends_with("/contract-result/local") {
        MirContractExpr::Result
    } else {
        if !function.values.contains_key(&value) {
            return Err(format!(
                "contract local '{}' is absent from canonical MIR values",
                place.base.0 .0
            ));
        }
        MirContractExpr::Value(value)
    };
    for projection in &place.projections {
        base = MirContractExpr::Project {
            base: Box::new(base),
            projection: lower_projection(projection)?,
        };
    }
    Ok(base)
}

/// Resolve a ResolvedExpr condition into canonical MIR identities.  This is
/// the only frontend-facing part of the contract path; all consumers receive
/// the resulting `MirContractExpr` and never see the source expression.
pub(crate) fn lower_contract_expr(
    expression: &crate::core::ir::ResolvedExpr,
    function: &MirFunction,
    body: &crate::core::ir::ResolvedBody,
) -> Result<MirContractExpr, String> {
    use crate::core::ir::{ResolvedExprKind, ResolvedLiteral, ResolvedUnaryOp};

    match &expression.kind {
        ResolvedExprKind::Literal(ResolvedLiteral::Int(value)) => Ok(MirContractExpr::Int(*value)),
        ResolvedExprKind::Literal(ResolvedLiteral::Bool(value)) => {
            Ok(MirContractExpr::Bool(*value))
        }
        // R6-1065: an f64 contract literal lowers to its raw IEEE bits —
        // exactly what the resolved literal holds — so the canonical text
        // and digest are deterministic and the verifier widens it as a
        // known finite constant.
        ResolvedExprKind::Literal(ResolvedLiteral::FloatBits(bits)) => {
            Ok(MirContractExpr::Float(*bits))
        }
        ResolvedExprKind::Literal(ResolvedLiteral::String(value)) => {
            Ok(MirContractExpr::String(value.clone()))
        }
        ResolvedExprKind::Load(place) => lower_contract_place(place, function, body, false),
        ResolvedExprKind::Cast { value, conversion }
            if conversion.kind == crate::core::ir::CheckedConversionKind::Identity
                && conversion.from == conversion.to
                && expression.ty == value.ty =>
        {
            // A checker-sealed identity cast has no runtime or proof effect.
            // Keep the canonical contract language surface-independent while
            // accepting source expressions that spell an exact-width value.
            lower_contract_expr(value, function, body)
        }
        ResolvedExprKind::Cast { value, conversion }
            if conversion.kind == crate::core::ir::CheckedConversionKind::NumericWiden
                && conversion.from == value.ty
                && conversion.to == expression.ty
                && matches!(
                    value.kind,
                    ResolvedExprKind::Literal(ResolvedLiteral::Int(_))
                ) =>
        {
            // Widening an integer literal preserves its exact mathematical
            // value. Restrict this erasure to literals; non-constant casts
            // retain width-sensitive semantics and stay fail-closed.
            lower_contract_expr(value, function, body)
        }
        ResolvedExprKind::Old(inner) => {
            let ResolvedExprKind::Load(place) = &inner.kind else {
                return Err("old() requires a direct callable parameter load".into());
            };
            lower_contract_place(place, function, body, true)
        }
        ResolvedExprKind::Project { value, projection } => Ok(MirContractExpr::Project {
            base: Box::new(lower_contract_expr(value, function, body)?),
            projection: lower_value_projection(projection)?,
        }),
        ResolvedExprKind::Unary { op, operand } => {
            if *op == ResolvedUnaryOp::Negate && expression.ty != operand.ty {
                if let ResolvedExprKind::Literal(ResolvedLiteral::Int(value)) = &operand.kind {
                    if let Some(value) = value.checked_neg() {
                        // Match executable MIR's contextual MIN literal
                        // materialization, retaining the checked result type.
                        return Ok(MirContractExpr::Int(value));
                    }
                }
            }
            let op = match op {
                ResolvedUnaryOp::Negate => MirContractUnaryOp::Negate,
                ResolvedUnaryOp::Not => MirContractUnaryOp::Not,
                _ => {
                    return Err(
                        "contract unary operator is outside the canonical MIR contract".into(),
                    );
                }
            };
            Ok(MirContractExpr::Unary {
                op,
                operand: Box::new(lower_contract_expr(operand, function, body)?),
            })
        }
        ResolvedExprKind::Binary { op, left, right } => {
            let op = MirContractBinaryOp::from_resolved(*op).ok_or_else(|| {
                "contract binary operator is outside the canonical MIR contract".to_string()
            })?;
            Ok(MirContractExpr::Binary {
                op,
                left: Box::new(lower_contract_expr(left, function, body)?),
                right: Box::new(lower_contract_expr(right, function, body)?),
            })
        }
        ResolvedExprKind::Call(call)
            if matches!(&call.callee, crate::core::ir::ResolvedCallee::Builtin(name) if name.as_str() == "len")
                && call.arguments.len() == 1
                && call.arguments[0].parameter.0 == NodeId("builtin:len/parameter:0".into())
                && call.arguments[0].conversion.kind
                    == crate::core::ir::CheckedConversionKind::Identity
                && call.arguments[0].conversion.from == call.arguments[0].value.ty
                && call.arguments[0].conversion.to == call.arguments[0].value.ty
                && call.arguments[0].value.ty
                    == crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String)
                && supported_string_length_argument(&call.arguments[0].value, body)
                && call.type_arguments.is_empty()
                && call.permission.is_none()
                && call.effects.is_empty()
                && call.session.is_empty()
                && call.result
                    == crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32)
                && expression.ty == call.result =>
        {
            // Keep this call in checker-owned contract IR only for a direct
            // String parameter or one direct String field of a callable
            // parameter. The FFI verifier admits only its closed length
            // guards; locals, nested projections, and computed strings stay
            // outside that trusted subset.
            Ok(MirContractExpr::StringLength(Box::new(
                lower_contract_expr(&call.arguments[0].value, function, body)?,
            )))
        }
        ResolvedExprKind::Call(call)
            if matches!(&call.callee, crate::core::ir::ResolvedCallee::Builtin(name) if name.as_str() == "char_at")
                && call.arguments.len() == 2
                && call.arguments[0].parameter.0
                    == NodeId("builtin:char_at/parameter:0".into())
                && call.arguments[1].parameter.0
                    == NodeId("builtin:char_at/parameter:1".into())
                && call.arguments.iter().all(|argument| {
                    argument.conversion.kind == crate::core::ir::CheckedConversionKind::Identity
                        && argument.conversion.from == argument.value.ty
                        && argument.conversion.to == argument.value.ty
                        && argument.value.effects.is_empty()
                        && argument.value.backend_requirements.is_empty()
                })
                && call.type_arguments.is_empty()
                && call.permission.is_none()
                && call.effects.is_empty()
                && call.session.is_empty()
                && expression.effects.is_empty()
                && expression.backend_requirements.is_empty()
                && call.arguments[0].value.ty
                    == crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String)
                && [
                    crate::core::PrimitiveType::I8,
                    crate::core::PrimitiveType::I16,
                    crate::core::PrimitiveType::I32,
                    crate::core::PrimitiveType::I64,
                    crate::core::PrimitiveType::I128,
                    crate::core::PrimitiveType::U8,
                    crate::core::PrimitiveType::U16,
                    crate::core::PrimitiveType::U32,
                    crate::core::PrimitiveType::U64,
                    crate::core::PrimitiveType::U128,
                    crate::core::PrimitiveType::Isize,
                    crate::core::PrimitiveType::Usize,
                ]
                .into_iter()
                .any(|primitive| {
                    call.arguments[1].value.ty == crate::core::ir::primitive_type_id(primitive)
                })
                && matches!(
                    &call.arguments[1].value.kind,
                    ResolvedExprKind::Literal(ResolvedLiteral::Int(0))
                )
                && matches!(
                    &call.arguments[0].value.kind,
                    ResolvedExprKind::Load(place)
                        if place.projections.is_empty()
                            && body.parameters.contains(&place.base)
                            && body.locals.get(&place.base).is_some_and(|local| {
                                local.ty
                                    == crate::core::ir::primitive_type_id(
                                        crate::core::PrimitiveType::String,
                                    )
                            })
                )
                && call.result
                    == crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String)
                && expression.ty == call.result =>
        {
            Ok(MirContractExpr::StringCharAt0(Box::new(
                lower_contract_expr(&call.arguments[0].value, function, body)?,
            )))
        }
        _ => Err(format!(
            "contract expression shape {:?} is outside the canonical MIR verifier contract",
            expression.kind
        )),
    }
}

fn supported_string_length_argument(
    expression: &crate::core::ir::ResolvedExpr,
    body: &crate::core::ir::ResolvedBody,
) -> bool {
    let crate::core::ir::ResolvedExprKind::Load(place) = &expression.kind else {
        return false;
    };
    body.parameters.contains(&place.base)
        && match place.projections.as_slice() {
            [] => true,
            [crate::core::ir::ResolvedProjection::Field { ty, .. }] => ty == &expression.ty,
            _ => false,
        }
}

fn lower_value_projection(
    projection: &crate::core::ir::ResolvedValueProjection,
) -> Result<MirProjection, String> {
    match projection {
        crate::core::ir::ResolvedValueProjection::Field(field) => {
            Ok(MirProjection::Field(field.clone()))
        }
        crate::core::ir::ResolvedValueProjection::Tuple(index) => Ok(MirProjection::Tuple(*index)),
        crate::core::ir::ResolvedValueProjection::Index(_) => {
            Err("contract indexed projection is outside the canonical aggregate contract".into())
        }
        crate::core::ir::ResolvedValueProjection::Dereference => Err(
            "contract dereference projection is outside the canonical aggregate contract".into(),
        ),
    }
}

fn collect_expression_types(
    expression: &crate::core::ir::ResolvedExpr,
    body: &crate::core::ir::ResolvedBody,
    path: &mut Vec<u8>,
    types: &mut BTreeMap<Vec<u8>, crate::core::ir::ResolvedTypeId>,
) -> Result<(), String> {
    use crate::core::ir::{ResolvedExprKind, ResolvedLiteral, ResolvedUnaryOp};
    types.insert(path.clone(), expression.ty.clone());
    if let ResolvedExprKind::Unary {
        op: ResolvedUnaryOp::Negate,
        operand,
    } = &expression.kind
    {
        if expression.ty != operand.ty
            && matches!(&operand.kind, ResolvedExprKind::Literal(ResolvedLiteral::Int(value)) if value.checked_neg().is_some())
        {
            return Ok(());
        }
    }
    match &expression.kind {
        ResolvedExprKind::Unary { operand, .. }
        | ResolvedExprKind::Project { value: operand, .. } => {
            path.push(0);
            collect_expression_types(operand, body, path, types)?;
            path.pop();
        }
        ResolvedExprKind::Binary { left, right, .. } => {
            for (index, operand) in [left, right].into_iter().enumerate() {
                path.push(index as u8);
                collect_expression_types(operand, body, path, types)?;
                path.pop();
            }
        }
        ResolvedExprKind::Load(place) => collect_place_expression_types(place, body, path, types)?,
        ResolvedExprKind::Old(inner) => {
            let ResolvedExprKind::Load(place) = &inner.kind else {
                return Err("old() requires a direct callable parameter load".into());
            };
            // Old replaces the Value leaf; it does not add a canonical child.
            collect_place_expression_types(place, body, path, types)?;
        }
        ResolvedExprKind::Call(call)
            if matches!(&call.callee, crate::core::ir::ResolvedCallee::Builtin(name) if name.as_str() == "len")
                && call.arguments.len() == 1 =>
        {
            path.push(0);
            collect_expression_types(&call.arguments[0].value, body, path, types)?;
            path.pop();
        }
        ResolvedExprKind::Call(call)
            if matches!(&call.callee, crate::core::ir::ResolvedCallee::Builtin(name) if name.as_str() == "char_at")
                && call.arguments.len() == 2
                && matches!(
                    &call.arguments[1].value.kind,
                    ResolvedExprKind::Literal(ResolvedLiteral::Int(0))
                ) =>
        {
            // The specialized MIR node commits to this literal index in its
            // canonical operation name, so only the String operand needs a
            // child TypeDesc path.
            path.push(0);
            collect_expression_types(&call.arguments[0].value, body, path, types)?;
            path.pop();
        }
        _ => {}
    }
    Ok(())
}

/// Collect the Checker-authenticated TypeDesc tree for a standalone
/// declaration expression.  FFI roots use the same path convention as
/// callable contracts, but keep their maps on the individual Requires/Ensures
/// receipt so each root has an independent typed identity.
pub(crate) fn collect_contract_expression_types(
    expression: &crate::core::ir::ResolvedExpr,
    body: &crate::core::ir::ResolvedBody,
) -> Result<BTreeMap<Vec<u8>, crate::core::ir::ResolvedTypeId>, String> {
    let mut expression_types = BTreeMap::new();
    collect_expression_types(expression, body, &mut Vec::new(), &mut expression_types)?;
    Ok(expression_types)
}

fn collect_place_expression_types(
    place: &crate::core::ir::ResolvedPlace,
    body: &crate::core::ir::ResolvedBody,
    path: &mut Vec<u8>,
    types: &mut BTreeMap<Vec<u8>, crate::core::ir::ResolvedTypeId>,
) -> Result<(), String> {
    let base = body.locals.get(&place.base).ok_or_else(|| {
        "contract place type is absent from the checked local catalog".to_string()
    })?;
    let original_length = path.len();
    for count in (0..place.projections.len()).rev() {
        path.push(0);
        let ty = count
            .checked_sub(1)
            .map(|index| place.projections[index].ty())
            .unwrap_or(&base.ty);
        types.insert(path.clone(), ty.clone());
    }
    path.truncate(original_length);
    Ok(())
}

/// Lower the checker-owned local-only declaration-default Math profile into
/// an isolated, typed MIR proof program. The synthetic values name only
/// immutable locals introduced inside this exact default root; caller state
/// and callable Requires facts are never imported.
pub(crate) fn lower_default_proof_root(
    root: &crate::core::ir::TypedDefaultProofRoot,
    callable: &crate::core::ir::ResolvedCallable,
    uses: Vec<MirProofDefaultUseId>,
    uses_valid: bool,
) -> MirProofDefaultRoot {
    fn default_local_value(
        owner: &NodeId,
        parameter: &ResolvedParameterId,
        local: &ResolvedLocalId,
    ) -> Result<MirValueId, String> {
        MirValueId::new(format!(
            "default-local:{}:{}:{}",
            owner.0, parameter.0 .0, local.0 .0
        ))
        .map_err(|error| error.to_string())
    }

    fn lower_expression(
        expression: &crate::core::ir::ResolvedExpr,
        root: &crate::core::ir::TypedDefaultProofRoot,
        body: &crate::core::ir::ResolvedBody,
    ) -> Result<MirProofExpression, String> {
        fn term(
            expression: &crate::core::ir::ResolvedExpr,
            root: &crate::core::ir::TypedDefaultProofRoot,
        ) -> Result<MirContractExpr, String> {
            use crate::core::ir::{ResolvedExprKind, ResolvedLiteral, ResolvedUnaryOp};
            Ok(match &expression.kind {
                ResolvedExprKind::Literal(ResolvedLiteral::Int(value)) => {
                    MirContractExpr::Int(*value)
                }
                ResolvedExprKind::Literal(ResolvedLiteral::Bool(value)) => {
                    MirContractExpr::Bool(*value)
                }
                ResolvedExprKind::Literal(ResolvedLiteral::FloatBits(bits)) => {
                    MirContractExpr::Float(*bits)
                }
                ResolvedExprKind::Literal(ResolvedLiteral::String(value)) => {
                    MirContractExpr::String(value.clone())
                }
                ResolvedExprKind::Load(place) if place.projections.is_empty() => {
                    MirContractExpr::Value(default_local_value(
                        &root.owner,
                        &root.parameter,
                        &place.base,
                    )?)
                }
                ResolvedExprKind::Unary { op, operand } => {
                    let op = match op {
                        ResolvedUnaryOp::Negate => MirContractUnaryOp::Negate,
                        ResolvedUnaryOp::Not => MirContractUnaryOp::Not,
                        _ => {
                            return Err(
                                "default unary operator is outside the MIR proof language".into()
                            );
                        }
                    };
                    MirContractExpr::Unary {
                        op,
                        operand: Box::new(term(operand, root)?),
                    }
                }
                ResolvedExprKind::Binary { op, left, right } => MirContractExpr::Binary {
                    op: MirContractBinaryOp::from_resolved(*op).ok_or_else(|| {
                        "default binary operator is outside the MIR proof language".to_string()
                    })?,
                    left: Box::new(term(left, root)?),
                    right: Box::new(term(right, root)?),
                },
                ResolvedExprKind::Project { value, projection } => MirContractExpr::Project {
                    base: Box::new(term(value, root)?),
                    projection: lower_value_projection(projection)?,
                },
                _ => {
                    return Err(
                        "default expression is outside the local-only MIR proof language".into(),
                    );
                }
            })
        }

        let canonical = term(expression, root)?;
        let mut expression_types = BTreeMap::new();
        collect_expression_types(expression, body, &mut Vec::new(), &mut expression_types)?;
        let type_path_fingerprint = crate::core::ir::type_path_fingerprint(&expression_types);
        Ok(MirProofExpression {
            origin: expression.node_id.clone(),
            expression: canonical,
            expression_types,
            ty: expression.ty.clone(),
            effects: expression.effects.clone(),
            source_fingerprint: crate::core::ir::typed_loop_expression_fingerprint(expression),
            type_path_fingerprint,
        })
    }

    fn lower_block(
        block: &crate::core::ir::TypedProofBlock,
        root: &crate::core::ir::TypedDefaultProofRoot,
        callable: &crate::core::ir::ResolvedCallable,
    ) -> Result<Vec<MirProofDefaultOperation>, String> {
        let mut operations = Vec::with_capacity(block.operations.len());
        for operation in &block.operations {
            use crate::core::ir::TypedProofOperation as T;
            operations.push(match operation {
                T::Bind {
                    point,
                    local,
                    initializer,
                } => {
                    let local_info = callable.body.locals.get(local).ok_or_else(|| {
                        "default proof local is absent from the checked local catalog".to_string()
                    })?;
                    MirProofDefaultOperation::Bind {
                        point: point.clone(),
                        span: initializer.origin.user_span(),
                        local: default_local_value(&root.owner, &root.parameter, local)?,
                        local_type: local_info.ty.clone(),
                        initializer: lower_expression(initializer, root, &callable.body)?,
                    }
                }
                T::Eval { point, expression } => MirProofDefaultOperation::Eval {
                    point: point.clone(),
                    span: expression.origin.user_span(),
                    expression: lower_expression(expression, root, &callable.body)?,
                },
                T::Math(point) => MirProofDefaultOperation::Math {
                    id: MirProofPointId {
                        owner: point.id.owner.clone(),
                        statement: point.id.statement.clone(),
                        ordinal: point.id.ordinal,
                    },
                    order: point.order,
                    span: point.statement_span,
                    scope: point.scope.clone(),
                    expression: lower_expression(&point.condition, root, &callable.body)?,
                },
                T::Scope(inner) => MirProofDefaultOperation::Scope {
                    id: inner.id.clone(),
                    operations: lower_block(inner, root, callable)?,
                },
                _ => {
                    return Err(
                        "default proof operation is outside the local-only MIR profile".into(),
                    );
                }
            });
        }
        Ok(operations)
    }

    let id = MirProofDefaultRootId {
        owner: root.owner.clone(),
        parameter: root.parameter.clone(),
        expression: root.expression.clone(),
    };
    let mut boundaries = root
        .boundaries
        .iter()
        .map(|boundary| MirProofDefaultBoundary {
            root: id.clone(),
            point: boundary.point.clone(),
            reason: boundary.reason.clone(),
        })
        .collect::<Vec<_>>();
    let fallback_boundary_point = root
        .points
        .first()
        .map(|point| point.id.statement.clone())
        .unwrap_or_else(|| root.expression.clone());
    let profile_supported = root.validate_against(callable)
        && root.supports_local_only_profile(callable)
        && !root.points.is_empty()
        && uses_valid
        && !uses.is_empty();
    let operations = if profile_supported {
        lower_block(&root.root, root, callable).unwrap_or_else(|message| {
            boundaries.push(MirProofDefaultBoundary {
                root: id.clone(),
                point: fallback_boundary_point.clone(),
                reason: message,
            });
            Vec::new()
        })
    } else {
        if boundaries.is_empty() {
            let reason = if root.points.is_empty() {
                "default proof root has no supported Math obligation"
            } else if !uses_valid || uses.is_empty() {
                "default Math root has no validated exact omitted-argument use receipt"
            } else {
                "default Math root is outside the local-only proof profile"
            };
            boundaries.push(MirProofDefaultBoundary {
                root: id.clone(),
                point: fallback_boundary_point,
                reason: reason.into(),
            });
        }
        Vec::new()
    };
    let span = callable
        .body
        .default_values
        .get(&root.parameter)
        .map(|expression| expression.origin.user_span())
        .unwrap_or(crate::span::Span::UNKNOWN);
    let mut source_spans = BTreeMap::from([(root.expression.clone(), span)]);
    fn collect_source_spans(
        operations: &[MirProofDefaultOperation],
        output: &mut BTreeMap<NodeId, crate::span::Span>,
    ) {
        for operation in operations {
            match operation {
                MirProofDefaultOperation::Bind { point, span, .. }
                | MirProofDefaultOperation::Eval { point, span, .. } => {
                    output.insert(point.clone(), *span);
                }
                MirProofDefaultOperation::Math { id, span, .. } => {
                    output.insert(id.statement.clone(), *span);
                }
                MirProofDefaultOperation::Scope { id, operations } => {
                    collect_source_spans(operations, output);
                    output
                        .entry(id.clone())
                        .or_insert(crate::span::Span::UNKNOWN);
                }
            }
        }
    }
    collect_source_spans(&operations, &mut source_spans);
    for point in &root.points {
        source_spans.insert(
            point.condition.node_id.clone(),
            point.condition.origin.user_span(),
        );
        source_spans.insert(point.id.statement.clone(), point.statement_span);
    }
    MirProofDefaultRoot {
        id,
        span,
        source_spans,
        uses,
        operations,
        boundaries,
        uses_valid,
        source_fingerprint: *blake3::hash(format!("{root:#?}").as_bytes()).as_bytes(),
    }
}

pub(crate) fn lower_contracts(
    callable: &crate::core::ir::ResolvedCallable,
    function: &MirFunction,
) -> Result<Vec<MirContract>, Vec<super::lower::MirLoweringError>> {
    lower_contracts_inner(callable, function, None)
}

pub(crate) fn lower_contracts_with_ffi_helper(
    callable: &crate::core::ir::ResolvedCallable,
    function: &MirFunction,
    helper: &crate::core::ir::ResolvedCallable,
    call_node: &NodeId,
) -> Result<Vec<MirContract>, Vec<super::lower::MirLoweringError>> {
    lower_contracts_inner(callable, function, Some((helper, call_node)))
}

fn lower_ffi_helper_contract_expr(
    condition: &crate::core::ir::ResolvedExpr,
    caller_function: &MirFunction,
    caller_body: &crate::core::ir::ResolvedBody,
    helper: &crate::core::ir::ResolvedCallable,
    call_node: &NodeId,
) -> Result<(MirContractExpr, BTreeMap<Vec<u8>, ResolvedTypeId>), String> {
    use crate::core::ir::{CheckedConversionKind, ResolvedCallee, ResolvedExprKind};

    let ResolvedExprKind::Call(call) = &condition.kind else {
        return Err("verifier-only FFI helper contract must be a direct call expression".into());
    };
    if condition.node_id != *call_node
        || call.callee != ResolvedCallee::Function(helper.owner.clone())
        || !call.type_arguments.is_empty()
        || call.permission.is_some()
        || !call.effects.is_empty()
        || !call.session.is_empty()
        || call.result != helper.signature.result
        || condition.ty != helper.signature.result
        || helper.body.root.ty != helper.signature.result
        || call.arguments.len() != helper.signature.parameters.len()
        || helper.body.parameters.len() != helper.signature.parameters.len()
        || !helper.body.root.statements.is_empty()
        || !helper.body.captures.is_empty()
    {
        return Err(
            "verifier-only FFI helper call does not match its checked concrete signature".into(),
        );
    }
    let helper_result = helper.body.root.result.as_deref().ok_or_else(|| {
        "verifier-only FFI helper body has no scalar result expression".to_string()
    })?;

    let mut substitutions = BTreeMap::new();
    for (index, ((parameter, local), argument)) in helper
        .signature
        .parameters
        .iter()
        .zip(&helper.body.parameters)
        .zip(&call.arguments)
        .enumerate()
    {
        if argument.parameter != parameter.id
            || argument.conversion.kind != CheckedConversionKind::Identity
            || argument.conversion.from != argument.value.ty
            || argument.conversion.to != parameter.ty
            || argument.value.ty != parameter.ty
            || !argument.value.effects.is_empty()
            || !argument.value.backend_requirements.is_empty()
            || !matches!(
                &argument.value.kind,
                ResolvedExprKind::Load(place)
                    if place.projections.is_empty()
                        && caller_body.parameters.contains(&place.base)
            )
        {
            return Err(format!(
                "verifier-only FFI helper argument {index} is not an exact direct parameter load"
            ));
        }
        let local_type = helper
            .body
            .locals
            .get(local)
            .map(|local| &local.ty)
            .ok_or_else(|| {
                format!(
                    "verifier-only FFI helper parameter local '{:?}' is absent",
                    local.0
                )
            })?;
        if local_type != &parameter.ty {
            return Err(format!(
                "verifier-only FFI helper parameter {:?} has inconsistent local type",
                parameter.id.0
            ));
        }
        let lowered = lower_contract_expr(&argument.value, caller_function, caller_body)?;
        substitutions.insert(local.clone(), lowered);
    }

    let condition = lower_ffi_helper_expression(helper_result, &substitutions)?;
    let mut expression_types = BTreeMap::new();
    collect_expression_types(
        helper_result,
        &helper.body,
        &mut Vec::new(),
        &mut expression_types,
    )?;
    Ok((condition, expression_types))
}

fn lower_ffi_helper_expression(
    expression: &crate::core::ir::ResolvedExpr,
    substitutions: &BTreeMap<crate::core::ResolvedLocalId, MirContractExpr>,
) -> Result<MirContractExpr, String> {
    use crate::core::ir::{
        CheckedConversionKind, ResolvedExprKind, ResolvedLiteral, ResolvedUnaryOp,
    };

    if !expression.effects.is_empty() || !expression.backend_requirements.is_empty() {
        return Err(
            "verifier-only FFI helper body contains effects or backend requirements".into(),
        );
    }
    match &expression.kind {
        ResolvedExprKind::Literal(ResolvedLiteral::Bool(value)) => {
            Ok(MirContractExpr::Bool(*value))
        }
        ResolvedExprKind::Literal(ResolvedLiteral::Int(value)) => Ok(MirContractExpr::Int(*value)),
        ResolvedExprKind::Load(place) if place.projections.is_empty() => substitutions
            .get(&place.base)
            .cloned()
            .ok_or_else(|| "verifier-only FFI helper loads a non-parameter local".into()),
        ResolvedExprKind::Cast { value, conversion }
            if conversion.kind == CheckedConversionKind::Identity
                && conversion.from == conversion.to
                && expression.ty == value.ty =>
        {
            lower_ffi_helper_expression(value, substitutions)
        }
        ResolvedExprKind::Unary { op, operand } => {
            let op = match op {
                ResolvedUnaryOp::Negate => MirContractUnaryOp::Negate,
                ResolvedUnaryOp::Not => MirContractUnaryOp::Not,
                _ => {
                    return Err(
                        "verifier-only FFI helper unary operator is outside the scalar predicate subset"
                            .into(),
                    )
                }
            };
            Ok(MirContractExpr::Unary {
                op,
                operand: Box::new(lower_ffi_helper_expression(operand, substitutions)?),
            })
        }
        ResolvedExprKind::Binary { op, left, right } => {
            let op = MirContractBinaryOp::from_resolved(*op).ok_or_else(|| {
                "verifier-only FFI helper binary operator is outside the scalar predicate subset"
                    .to_string()
            })?;
            Ok(MirContractExpr::Binary {
                op,
                left: Box::new(lower_ffi_helper_expression(left, substitutions)?),
                right: Box::new(lower_ffi_helper_expression(right, substitutions)?),
            })
        }
        ResolvedExprKind::Literal(ResolvedLiteral::FloatBits(_))
        | ResolvedExprKind::Literal(ResolvedLiteral::String(_))
        | ResolvedExprKind::Literal(ResolvedLiteral::Unit)
        | ResolvedExprKind::FString(_)
        | ResolvedExprKind::Constant(_)
        | ResolvedExprKind::Callable(_)
        | ResolvedExprKind::Project { .. }
        | ResolvedExprKind::DefaultArgument { .. }
        | ResolvedExprKind::Call(_)
        | ResolvedExprKind::Tuple(_)
        | ResolvedExprKind::List(_)
        | ResolvedExprKind::Map(_)
        | ResolvedExprKind::Set(_)
        | ResolvedExprKind::Comprehension { .. }
        | ResolvedExprKind::OptionalChain { .. }
        | ResolvedExprKind::TypeOf(_)
        | ResolvedExprKind::Old(_)
        | ResolvedExprKind::Record { .. }
        | ResolvedExprKind::Block(_)
        | ResolvedExprKind::Scope { .. }
        | ResolvedExprKind::Comptime(_)
        | ResolvedExprKind::If { .. }
        | ResolvedExprKind::Match { .. }
        | ResolvedExprKind::Try { .. }
        | ResolvedExprKind::Range { .. }
        | ResolvedExprKind::Slice { .. }
        | ResolvedExprKind::Cast { .. }
        | ResolvedExprKind::Spawn(_)
        | ResolvedExprKind::Await(_)
        | ResolvedExprKind::Lambda(_)
        | ResolvedExprKind::ComptimeValue(_)
        | ResolvedExprKind::Quote(_)
        | ResolvedExprKind::TypeValue(_)
        | ResolvedExprKind::Load(_) => Err(
            "verifier-only FFI helper expression is outside the scalar pure predicate subset"
                .into(),
        ),
    }
}

fn lower_contracts_inner(
    callable: &crate::core::ir::ResolvedCallable,
    function: &MirFunction,
    ffi_helper: Option<(&crate::core::ir::ResolvedCallable, &NodeId)>,
) -> Result<Vec<MirContract>, Vec<super::lower::MirLoweringError>> {
    let root_contracts = callable
        .body
        .root
        .statements
        .iter()
        .filter_map(|statement| {
            matches!(
                &statement.kind,
                crate::core::ir::ResolvedStmtKind::Contract { .. }
            )
            .then_some(statement.node_id.clone())
        })
        .collect::<BTreeSet<_>>();
    let mut contracts = Vec::with_capacity(callable.contracts.len());
    let mut errors = Vec::new();
    for contract in &callable.contracts {
        // `callable.contracts` is the complete checked census, including
        // nested and dormant roots. Only direct body-entry roots may become
        // MIR function assumptions/postconditions; scoped points are carried
        // by `MirProofWorklist` or rejected by its typed boundary.
        if !root_contracts.contains(&contract.node_id) {
            continue;
        }
        let kind = match contract.kind {
            crate::core::ir::ContractKind::Requires => MirContractKind::Requires,
            crate::core::ir::ContractKind::Ensures => MirContractKind::Ensures,
            crate::core::ir::ContractKind::Invariant => MirContractKind::Invariant,
        };
        let helper_call = ffi_helper.filter(|(_, call_node)| {
            kind == MirContractKind::Requires && contract.condition.node_id == **call_node
        });
        let lowered = if let Some((helper, call_node)) = helper_call {
            lower_ffi_helper_contract_expr(
                &contract.condition,
                function,
                &callable.body,
                helper,
                call_node,
            )
        } else {
            lower_contract_expr(&contract.condition, function, &callable.body).and_then(
                |condition| {
                    let mut expression_types = BTreeMap::new();
                    collect_expression_types(
                        &contract.condition,
                        &callable.body,
                        &mut Vec::new(),
                        &mut expression_types,
                    )?;
                    Ok((condition, expression_types))
                },
            )
        };
        match lowered {
            Ok((condition, expression_types)) => {
                contracts.push(MirContract {
                    id: contract.node_id.clone(),
                    kind,
                    condition,
                    expression_types,
                    ffi_helper_call: helper_call.map(|(helper, call_node)| MirContractHelperCall {
                        call_node: call_node.clone(),
                        helper: helper.owner.clone(),
                    }),
                });
            }
            Err(message) => errors.push(super::lower::MirLoweringError {
                node_id: contract.node_id.clone(),
                message,
            }),
        }
    }
    if errors.is_empty() {
        Ok(contracts)
    } else {
        Err(errors)
    }
}

fn mir_successors(
    block: &super::MirBlock,
) -> Vec<(super::MirEdgeId, super::MirBlockId, Vec<MirValueId>)> {
    match &block.terminator {
        super::MirTerminator::Goto {
            edge,
            target,
            arguments,
        } => vec![(edge.clone(), target.clone(), arguments.clone())],
        super::MirTerminator::Branch {
            then_edge,
            then_target,
            then_arguments,
            else_edge,
            else_target,
            else_arguments,
            ..
        } => vec![
            (
                then_edge.clone(),
                then_target.clone(),
                then_arguments.clone(),
            ),
            (
                else_edge.clone(),
                else_target.clone(),
                else_arguments.clone(),
            ),
        ],
        super::MirTerminator::Switch { arms, .. }
        | super::MirTerminator::SwitchMove { arms, .. } => arms
            .iter()
            .map(|arm| (arm.edge.clone(), arm.target.clone(), arm.arguments.clone()))
            .collect(),
        super::MirTerminator::Return { .. }
        | super::MirTerminator::Trap { .. }
        | super::MirTerminator::Fault { .. }
        | super::MirTerminator::Unreachable => Vec::new(),
    }
}

fn mir_edge_records(
    function: &MirFunction,
) -> Vec<(
    super::MirEdgeId,
    super::MirBlockId,
    super::MirBlockId,
    Vec<MirValueId>,
)> {
    function
        .blocks
        .values()
        .flat_map(|block| {
            mir_successors(block)
                .into_iter()
                .map(|(edge, target, arguments)| (edge, block.id.clone(), target, arguments))
        })
        .collect()
}

#[derive(Debug, Clone)]
struct OrdinaryMapNewResultCallBridge {
    instruction: super::MirInstructionId,
    result: MirValueId,
    arguments: Vec<MirValueId>,
}

fn ordinary_map_values_local_index_i32_result_branch_call_bridge_for_source(
    point: &crate::core::ir::TypedCallProofPoint,
    callable: &crate::core::ir::ResolvedCallable,
    function: &MirFunction,
    receipt: &crate::core::ownership::OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultBranchReceipt,
) -> Result<Option<OrdinaryMapNewResultCallBridge>, String> {
    use crate::core::ir::{ResolvedCallee, ResolvedExprKind};
    use crate::core::ownership::OrdinaryMapOwnedAnyV2ActionKind as ActionKind;

    if point.call_node != receipt.map_new_operation {
        return Ok(None);
    }
    let checked_cfg = callable
        .resources
        .ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
        .ok_or_else(|| {
            "ValuesLocalIndexI32 New proof bridge has no CheckedProgram CFG".to_string()
        })?;
    let mir_cfg = function
        .ownership
        .ordinary_map_owned_any_v2_cfg_receipt()
        .ok_or_else(|| "ValuesLocalIndexI32 New proof bridge has no MIR CFG".to_string())?;
    let checked_receipt = checked_cfg
        .values_local_index_i32_result_branch_receipt
        .as_ref();
    let mir_receipt = mir_cfg
        .values_local_index_i32_result_branch_receipt
        .as_ref();
    let actions = checked_cfg
        .actions
        .iter()
        .filter(|action| {
            action.kind == ActionKind::New && action.point == receipt.map_new_operation
        })
        .collect::<Vec<_>>();
    let [action] = actions.as_slice() else {
        return Err("ValuesLocalIndexI32 New proof bridge has no unique Checker New action".into());
    };
    if callable.owner != receipt.owner
        || function.owner != receipt.owner
        || checked_receipt != Some(receipt)
        || mir_receipt != Some(receipt)
        || action.local != receipt.initial_root
        || point.owner != receipt.owner
        || point.body_owner != receipt.owner
        || point.expression.node_id != point.call_node
    {
        return Err(
            "ValuesLocalIndexI32 New proof bridge is detached from its exact Checker/MIR receipt"
                .into(),
        );
    }
    let ResolvedExprKind::Call(call) = &point.expression.kind else {
        return Err("ValuesLocalIndexI32 New proof point is not a resolved Call".into());
    };
    if !matches!(&call.callee, ResolvedCallee::Builtin(name) if name.as_str() == "map_new")
        || !call.arguments.is_empty()
        || !call.type_arguments.is_empty()
        || call.permission.is_some()
        || !call.effects.is_empty()
        || !call.session.is_empty()
        || point.expression.ty != call.result
    {
        return Err(
            "ValuesLocalIndexI32 New proof point is not the exact zero-argument map_new call"
                .into(),
        );
    }
    let result = MirValueId::new(format!("expr:{}", receipt.map_new_operation.0))
        .map_err(|error| error.to_string())?;
    let instruction = super::MirInstructionId::new(format!(
        "inst:ordinary_map_owned_any_v2_new:local:{}:{}",
        receipt.initial_root.0 .0, receipt.map_new_operation.0
    ))
    .map_err(|error| error.to_string())?;
    if function.values.get(&result).map(|value| &value.ty)
        != Some(&super::types::ordinary_map_owned_any_v2_root_result_type_id())
        || !function.blocks.values().any(|block| {
            block.instructions.iter().any(|candidate| {
                candidate.id == instruction
                    && matches!(
                        &candidate.kind,
                        super::MirInstructionKind::OrdinaryMapOwnedAnyV2New { result: actual }
                            if actual == &result
                    )
            })
        })
    {
        return Err("ValuesLocalIndexI32 New proof bridge is detached from the canonical Result-valued New instruction".into());
    }
    Ok(Some(OrdinaryMapNewResultCallBridge {
        instruction,
        result,
        arguments: Vec::new(),
    }))
}

/// Bind only the two source calls covered by the exact NewResult Checker
/// receipt to their dedicated canonical MIR operations. The bridge preserves
/// the original TypedCallProofPoint metadata; it changes only the selected
/// MIR instruction/result/actual identities that represent these builtins.
fn ordinary_map_new_result_call_bridge_for_source(
    point: &crate::core::ir::TypedCallProofPoint,
    callable: &crate::core::ir::ResolvedCallable,
    function: &MirFunction,
    receipt: &crate::core::ownership::OrdinaryMapOwnedAnyV2NewResultReceipt,
) -> Result<Option<OrdinaryMapNewResultCallBridge>, String> {
    use crate::core::ir::{CheckedConversionKind, ResolvedCallee, ResolvedExprKind};

    if point.call_node != receipt.operation && point.call_node != receipt.size_operation {
        return Ok(None);
    }
    if callable.owner != receipt.owner
        || point.owner != receipt.owner
        || point.body_owner != receipt.owner
        || point.expression.node_id != point.call_node
    {
        return Err(
            "NewResult proof-call bridge has a mismatched Checker owner or call identity".into(),
        );
    }
    let ResolvedExprKind::Call(call) = &point.expression.kind else {
        return Err("NewResult proof-call bridge source point is not a resolved Call".into());
    };
    if point.call_node == receipt.operation {
        let (ResolvedCallee::Builtin(builtin), []) = (&call.callee, call.arguments.as_slice())
        else {
            return Err(
                "NewResult proof-call bridge does not name the exact zero-argument map_new builtin"
                    .into(),
            );
        };
        let result = MirValueId::new(format!("local:{}", receipt.result_local.0 .0))
            .map_err(|error| error.to_string())?;
        let instruction = super::MirInstructionId::new(format!(
            "inst:ordinary_map_new_result:new:{}",
            receipt.operation.0
        ))
        .map_err(|error| error.to_string())?;
        if builtin.as_str() != "map_new"
            || call.result != receipt.source_result_type
            || point.expression.ty != receipt.source_result_type
            || function.values.get(&result).map(|value| &value.ty)
                != Some(&super::types::ordinary_map_owned_any_v2_root_result_type_id())
            || !function.blocks.values().any(|block| {
                block.instructions.iter().any(|candidate| {
                    candidate.id == instruction
                        && matches!(
                            &candidate.kind,
                            super::MirInstructionKind::OrdinaryMapOwnedAnyV2New { result: actual }
                                if actual == &result
                        )
                })
            })
        {
            return Err(
                "NewResult proof-call bridge is detached from the exact canonical map_new result"
                    .into(),
            );
        }
        return Ok(Some(OrdinaryMapNewResultCallBridge {
            instruction,
            result,
            arguments: Vec::new(),
        }));
    }

    let [argument] = call.arguments.as_slice() else {
        return Err(
            "NewResult Size proof-call bridge does not have exactly one source argument".into(),
        );
    };
    let root = MirValueId::new(format!("local:{}", receipt.root_local.0 .0))
        .map_err(|error| error.to_string())?;
    let result = MirValueId::new(format!("local:{}", receipt.size_local.0 .0))
        .map_err(|error| error.to_string())?;
    let instruction = super::MirInstructionId::new(format!(
        "inst:ordinary_map_new_result:size:{}",
        receipt.size_operation.0
    ))
    .map_err(|error| error.to_string())?;
    if !matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == "map_size")
        || call.result != receipt.status_type
        || point.expression.ty != receipt.status_type
        || argument.value.node_id != receipt.size_receiver
        || argument.conversion.kind != CheckedConversionKind::Identity
        || argument.conversion.from != receipt.root_type
        || argument.conversion.to != receipt.root_type
        || !matches!(&argument.value.kind, ResolvedExprKind::Load(place)
            if place.base == receipt.root_local && place.projections.is_empty())
        || function.values.get(&root).map(|value| &value.ty)
            != Some(&super::types::ordinary_map_owned_any_v2_type_id())
        || function.values.get(&result).map(|value| &value.ty) != Some(&receipt.status_type)
        || !function.blocks.values().any(|block| {
            block.instructions.iter().any(|candidate| {
                candidate.id == instruction
                    && matches!(
                        &candidate.kind,
                        super::MirInstructionKind::OrdinaryMapOwnedAnyV2Size {
                            result: actual_result,
                            root: actual_root,
                        } if actual_result == &result && actual_root == &root
                    )
            })
        })
    {
        return Err("NewResult proof-call bridge is detached from the exact canonical map_size/root operation".into());
    }
    Ok(Some(OrdinaryMapNewResultCallBridge {
        instruction,
        result,
        arguments: vec![root],
    }))
}

fn ordinary_map_set_result_call_bridge_for_source(
    point: &crate::core::ir::TypedCallProofPoint,
    callable: &crate::core::ir::ResolvedCallable,
    function: &MirFunction,
    receipt: &crate::core::ownership::OrdinaryMapOwnedAnyV2SetLineageReceipt,
) -> Result<Option<OrdinaryMapNewResultCallBridge>, String> {
    use crate::core::ir::{
        CheckedConversionKind, ResolvedCallee, ResolvedExprKind, ResolvedLiteral,
    };

    if ![
        &receipt.new_operation,
        &receipt.set_operation,
        &receipt.size_operation,
    ]
    .contains(&&point.call_node)
    {
        return Ok(None);
    }
    if callable.owner != receipt.owner
        || function.owner != receipt.owner
        || point.owner != receipt.owner
        || point.body_owner != receipt.owner
        || point.call_node != point.expression.node_id
        || function.ownership.ordinary_map_set_lineage_receipt() != Some(receipt)
        || callable
            .resources
            .ordinary_map_owned_any_v2_set_lineage_receipt
            .as_ref()
            != Some(receipt)
    {
        return Err(
            "SetResult proof-call bridge is detached from the exact CheckedProgram/MIR lineage receipt"
                .into(),
        );
    }
    let ResolvedExprKind::Call(call) = &point.expression.kind else {
        return Err("SetResult proof-call source point is not a resolved Call".into());
    };
    if !call.type_arguments.is_empty() {
        return Err("SetResult proof-call bridge does not admit generic builtin calls".into());
    }

    let (builtin_name, result_local, result_type, instruction_role, arguments) = if point.call_node
        == receipt.new_operation
    {
        if !call.arguments.is_empty()
            || !matches!(&call.callee, ResolvedCallee::Builtin(name) if name.as_str() == "map_new")
            || call.result != receipt.new_result_type
            || point.expression.ty != receipt.new_result_type
        {
            return Err(
                "SetResult New proof call is not the exact checked zero-argument map_new".into(),
            );
        }
        (
            "map_new",
            receipt.new_result_local.clone(),
            receipt.new_result_type.clone(),
            "new_result",
            Vec::new(),
        )
    } else if point.call_node == receipt.set_operation {
        let [root_arg, key_arg, value_arg] = call.arguments.as_slice() else {
            return Err(
                "SetResult map_set proof call requires exactly root/key/i32 arguments".into(),
            );
        };
        let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        if !matches!(&call.callee, ResolvedCallee::Builtin(name) if name.as_str() == "map_set")
            || call.result != receipt.set_result_type
            || point.expression.ty != receipt.set_result_type
            || root_arg.value.node_id != receipt.set_receiver
            || !matches!(&root_arg.value.kind, ResolvedExprKind::Load(place)
                    if place.base == receipt.initial_root_local && place.projections.is_empty())
            || root_arg.value.ty != receipt.initial_root_type
            || root_arg.conversion.kind != CheckedConversionKind::Identity
            || root_arg.conversion.from != receipt.initial_root_type
            || root_arg.conversion.to != receipt.initial_root_type
            || !matches!(&key_arg.value.kind, ResolvedExprKind::Literal(ResolvedLiteral::String(value)) if value == "answer")
            || key_arg.value.ty != string_type
            || key_arg.conversion.kind != CheckedConversionKind::Identity
            || key_arg.conversion.from != string_type
            || key_arg.conversion.to != string_type
            || !matches!(&value_arg.value.kind, ResolvedExprKind::Literal(ResolvedLiteral::Int(value)) if *value == 42)
            || value_arg.value.ty != i32_type
            || value_arg.conversion.kind != CheckedConversionKind::Identity
            || value_arg.conversion.from != i32_type
            || value_arg.conversion.to != i32_type
        {
            return Err(
                "SetResult map_set proof call lost its exact Checker root/\"answer\"/42 operands"
                    .into(),
            );
        }
        (
            "map_set",
            receipt.set_result_local.clone(),
            receipt.set_result_type.clone(),
            "set_result",
            vec![
                MirValueId::new(format!("local:{}", receipt.initial_root_local.0 .0))
                    .map_err(|error| error.to_string())?,
                MirValueId::new("literal:ordinary_map_set_lineage:answer")
                    .map_err(|error| error.to_string())?,
                MirValueId::new("literal:ordinary_map_set_lineage:42")
                    .map_err(|error| error.to_string())?,
            ],
        )
    } else {
        let [root_arg] = call.arguments.as_slice() else {
            return Err("SetResult map_size proof call requires exactly one root".into());
        };
        if !matches!(&call.callee, ResolvedCallee::Builtin(name) if name.as_str() == "map_size")
            || call.result != crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32)
            || point.expression.ty
                != crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32)
            || root_arg.value.node_id != receipt.size_receiver
            || !matches!(&root_arg.value.kind, ResolvedExprKind::Load(place)
                    if place.base == receipt.updated_root_local && place.projections.is_empty())
            || root_arg.value.ty != receipt.updated_root_type
            || root_arg.conversion.kind != CheckedConversionKind::Identity
            || root_arg.conversion.from != receipt.updated_root_type
            || root_arg.conversion.to != receipt.updated_root_type
        {
            return Err(
                "SetResult map_size proof call lost its exact checked updated-root operand".into(),
            );
        }
        (
            "map_size",
            receipt.size_local.clone(),
            crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32),
            "size",
            vec![
                MirValueId::new(format!("local:{}", receipt.updated_root_local.0 .0))
                    .map_err(|error| error.to_string())?,
            ],
        )
    };

    if !matches!(&call.callee, ResolvedCallee::Builtin(name) if name.as_str() == builtin_name)
        || call.result != result_type
        || point.expression.ty != result_type
        || call.arguments.len() != arguments.len()
    {
        return Err(format!(
            "SetResult proof-call bridge is detached from exact {builtin_name} call/result identity"
        ));
    }
    let result = MirValueId::new(format!("local:{}", result_local.0 .0))
        .map_err(|error| error.to_string())?;
    let instruction = super::MirInstructionId::new(format!(
        "inst:ordinary_map_set_lineage:{instruction_role}:{}",
        point.call_node.0
    ))
    .map_err(|error| error.to_string())?;
    let operation_matches = function.blocks.values().any(|block| {
        block.instructions.iter().any(|candidate| {
            candidate.id == instruction
                && match &candidate.kind {
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2New { result: actual } => {
                        actual == &result && builtin_name == "map_new"
                    }
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult {
                        result: actual,
                        source,
                        key,
                        value,
                    } => {
                        actual == &result
                            && arguments.as_slice() == [source.clone(), key.clone(), value.clone()]
                            && builtin_name == "map_set"
                    }
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2Size {
                        result: actual_result,
                        root,
                    } => {
                        actual_result == &result
                            && arguments.as_slice() == [root.clone()]
                            && builtin_name == "map_size"
                    }
                    _ => false,
                }
        })
    });
    if !operation_matches {
        return Err(format!(
            "SetResult proof-call bridge is detached from canonical {builtin_name} MIR operation"
        ));
    }
    Ok(Some(OrdinaryMapNewResultCallBridge {
        instruction,
        result,
        arguments,
    }))
}

fn boundaries_are_set_result_exact(
    worklist: &crate::core::ir::TypedProofPointWorklist,
    receipt: &crate::core::ownership::OrdinaryMapOwnedAnyV2SetLineageReceipt,
) -> bool {
    let allowed_reasons = [
        "proof root or effect is nested in an unsupported expression form",
        "block result call/effect needs a typed return-value receipt",
        "block result embeds a proof root without an evaluation-order receipt",
    ];
    let actual = worklist
        .boundaries
        .iter()
        .map(|boundary| (boundary.point.clone(), boundary.reason.clone()))
        .collect::<BTreeSet<_>>();
    actual.len() == worklist.boundaries.len()
        && actual.len() <= 6
        && actual.iter().all(|(point, reason)| {
            (point == &receipt.outer_match || point == &receipt.inner_match)
                && allowed_reasons.contains(&reason.as_str())
        })
}

fn ordinary_map_set_get_i32_call_bridge_for_source(
    point: &crate::core::ir::TypedCallProofPoint,
    callable: &crate::core::ir::ResolvedCallable,
    function: &MirFunction,
    receipt: &crate::core::ownership::OrdinaryMapOwnedAnyV2SetGetI32LineageReceipt,
) -> Result<Option<OrdinaryMapNewResultCallBridge>, String> {
    use crate::core::ir::{
        CheckedConversionKind, ResolvedCallee, ResolvedExprKind, ResolvedLiteral,
    };
    if ![
        &receipt.new_operation,
        &receipt.set_operation,
        &receipt.get_operation,
    ]
    .contains(&&point.call_node)
    {
        return Ok(None);
    }
    if callable.owner != receipt.owner
        || point.owner != receipt.owner
        || point.body_owner != receipt.owner
        || point.expression.node_id != point.call_node
    {
        return Err(
            "Set/Get-i32 proof-call bridge has a mismatched Checker owner or call identity".into(),
        );
    }
    let ResolvedExprKind::Call(call) = &point.expression.kind else {
        return Err("Set/Get-i32 proof-call bridge source point is not a resolved Call".into());
    };
    let (builtin_name, result_local, result_ty, instruction_name, arguments) = if point.call_node
        == receipt.new_operation
    {
        (
            "map_new",
            receipt.new_result_local.clone(),
            receipt.set_result_type.clone(),
            "new_result",
            Vec::new(),
        )
    } else if point.call_node == receipt.set_operation {
        let [root_arg, key_arg, value_arg] = call.arguments.as_slice() else {
            return Err(
                "Set/Get-i32 map_set proof call requires exactly root/key/i32 arguments".into(),
            );
        };
        if root_arg.value.node_id != receipt.set_receiver
            || !matches!(&root_arg.value.kind, ResolvedExprKind::Load(place) if place.base == receipt.initial_root_local && place.projections.is_empty())
            || root_arg.conversion.kind != CheckedConversionKind::Identity
            || root_arg.conversion.from != receipt.initial_root_type
            || root_arg.conversion.to != receipt.initial_root_type
            || !matches!(&key_arg.value.kind, ResolvedExprKind::Literal(ResolvedLiteral::String(value)) if value == "answer")
            || !matches!(&value_arg.value.kind, ResolvedExprKind::Literal(ResolvedLiteral::Int(value)) if *value == 42)
        {
            return Err("Set/Get-i32 map_set proof call no longer has the exact root/\"answer\"/42 source operands".into());
        }
        (
            "map_set",
            receipt.set_result_local.clone(),
            receipt.set_result_type.clone(),
            "set_result",
            vec![
                MirValueId::new(format!("local:{}", receipt.initial_root_local.0 .0))
                    .map_err(|e| e.to_string())?,
                MirValueId::new("literal:ordinary_map_set_get_i32_lineage:set-answer")
                    .map_err(|e| e.to_string())?,
                MirValueId::new("literal:ordinary_map_set_get_i32_lineage:42")
                    .map_err(|e| e.to_string())?,
            ],
        )
    } else {
        let [root_arg, key_arg] = call.arguments.as_slice() else {
            return Err(
                "Set/Get-i32 map_get_i32 proof call requires exactly root/key arguments".into(),
            );
        };
        if root_arg.value.node_id != receipt.get_receiver
            || !matches!(&root_arg.value.kind, ResolvedExprKind::Load(place) if place.base == receipt.updated_root_local && place.projections.is_empty())
            || root_arg.conversion.kind != CheckedConversionKind::Identity
            || root_arg.conversion.from != receipt.updated_root_type
            || root_arg.conversion.to != receipt.updated_root_type
            || !matches!(&key_arg.value.kind, ResolvedExprKind::Literal(ResolvedLiteral::String(value)) if value == "answer")
        {
            return Err("Set/Get-i32 map_get_i32 proof call no longer has the exact updated-root/\"answer\" source operands".into());
        }
        (
            "map_get_i32",
            receipt.get_result_local.clone(),
            receipt.get_result_type.clone(),
            "get_i32_result",
            vec![
                MirValueId::new(format!("local:{}", receipt.updated_root_local.0 .0))
                    .map_err(|e| e.to_string())?,
                MirValueId::new("literal:ordinary_map_set_get_i32_lineage:get-answer")
                    .map_err(|e| e.to_string())?,
            ],
        )
    };
    if !matches!(&call.callee, ResolvedCallee::Builtin(name) if name.as_str() == builtin_name)
        || call.result != result_ty
        || point.expression.ty != result_ty
        || call.arguments.len() != arguments.len()
    {
        return Err(format!("Set/Get-i32 proof-call bridge is detached from exact {builtin_name} call/result identity"));
    }
    let result =
        MirValueId::new(format!("local:{}", result_local.0 .0)).map_err(|e| e.to_string())?;
    let instruction = super::MirInstructionId::new(format!(
        "inst:ordinary_map_set_get_i32_lineage:{instruction_name}:{}",
        point.call_node.0
    ))
    .map_err(|e| e.to_string())?;
    if !function.blocks.values().any(|block| {
        block.instructions.iter().any(|candidate| {
            candidate.id == instruction
                && match &candidate.kind {
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2New { result: actual } => {
                        actual == &result && builtin_name == "map_new"
                    }
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult {
                        result: actual,
                        source,
                        key,
                        value,
                    } => {
                        actual == &result
                            && arguments.as_slice() == [source.clone(), key.clone(), value.clone()]
                            && builtin_name == "map_set"
                    }
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2GetI32Result {
                        result: actual,
                        root,
                        key,
                    } => {
                        actual == &result
                            && arguments.as_slice() == [root.clone(), key.clone()]
                            && builtin_name == "map_get_i32"
                    }
                    _ => false,
                }
        })
    }) {
        return Err(format!(
            "Set/Get-i32 proof-call bridge is detached from canonical {builtin_name} MIR operation"
        ));
    }
    Ok(Some(OrdinaryMapNewResultCallBridge {
        instruction,
        result,
        arguments,
    }))
}

fn ordinary_map_set_get_any_i32_call_bridge_for_source(
    point: &crate::core::ir::TypedCallProofPoint,
    callable: &crate::core::ir::ResolvedCallable,
    function: &MirFunction,
    receipt: &crate::core::ownership::OrdinaryMapOwnedAnyV2SetGetAnyI32LineageReceipt,
) -> Result<Option<OrdinaryMapNewResultCallBridge>, String> {
    use crate::core::ir::{
        CheckedConversionKind, ResolvedCallee, ResolvedExprKind, ResolvedLiteral,
    };
    if ![
        &receipt.new_operation,
        &receipt.set_operation,
        &receipt.get_operation,
    ]
    .contains(&&point.call_node)
    {
        return Ok(None);
    }
    if callable.owner != receipt.owner
        || point.owner != receipt.owner
        || point.body_owner != receipt.owner
        || point.expression.node_id != point.call_node
    {
        return Err(
            "Set/GetAny-i32 proof-call bridge has a mismatched Checker owner or call identity"
                .into(),
        );
    }
    let ResolvedExprKind::Call(call) = &point.expression.kind else {
        return Err("Set/GetAny-i32 proof-call bridge source is not a resolved Call".into());
    };
    let (builtin_name, result_local, result_ty, instruction_name, arguments) = if point.call_node
        == receipt.new_operation
    {
        (
            "map_new",
            receipt.new_result_local.clone(),
            receipt.new_result_type.clone(),
            "new_result",
            Vec::new(),
        )
    } else if point.call_node == receipt.set_operation {
        let [root_arg, key_arg, value_arg] = call.arguments.as_slice() else {
            return Err("Set/GetAny-i32 map_set proof call requires root/key/i32 arguments".into());
        };
        let string_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        let i32_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        if root_arg.value.node_id != receipt.set_receiver
            || !matches!(&root_arg.value.kind, ResolvedExprKind::Load(place) if place.base == receipt.initial_root_local && place.projections.is_empty())
            || root_arg.conversion.kind != CheckedConversionKind::Identity
            || root_arg.conversion.from != receipt.initial_root_type
            || root_arg.conversion.to != receipt.initial_root_type
            || !matches!(&key_arg.value.kind, ResolvedExprKind::Literal(ResolvedLiteral::String(value)) if value == &receipt.set_key_value)
            || key_arg.value.ty != string_ty
            || key_arg.conversion.kind != CheckedConversionKind::Identity
            || key_arg.conversion.from != string_ty
            || key_arg.conversion.to != string_ty
            || !matches!(&value_arg.value.kind, ResolvedExprKind::Literal(ResolvedLiteral::Int(value)) if *value == receipt.set_i32_value)
            || value_arg.value.ty != i32_ty
            || value_arg.conversion.kind != CheckedConversionKind::Identity
            || value_arg.conversion.from != i32_ty
            || value_arg.conversion.to != i32_ty
        {
            return Err(
                "Set/GetAny-i32 map_set proof call lost its exact Checker root/key/value operands"
                    .into(),
            );
        }
        (
            "map_set",
            receipt.set_result_local.clone(),
            receipt.set_result_type.clone(),
            "set_result",
            vec![
                MirValueId::new(format!("local:{}", receipt.initial_root_local.0 .0))
                    .map_err(|error| error.to_string())?,
                MirValueId::new(format!("literal:{}", receipt.set_key.0))
                    .map_err(|error| error.to_string())?,
                MirValueId::new(format!("literal:{}", receipt.set_value.0))
                    .map_err(|error| error.to_string())?,
            ],
        )
    } else {
        let [root_arg, key_arg] = call.arguments.as_slice() else {
            return Err("Set/GetAny-i32 map_get proof call requires root/key arguments".into());
        };
        let string_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        if root_arg.value.node_id != receipt.get_receiver
            || !matches!(&root_arg.value.kind, ResolvedExprKind::Load(place) if place.base == receipt.updated_root_local && place.projections.is_empty())
            || root_arg.conversion.kind != CheckedConversionKind::Identity
            || root_arg.conversion.from != receipt.updated_root_type
            || root_arg.conversion.to != receipt.updated_root_type
            || !matches!(&key_arg.value.kind, ResolvedExprKind::Literal(ResolvedLiteral::String(value)) if value == &receipt.get_key_value)
            || key_arg.value.ty != string_ty
            || key_arg.conversion.kind != CheckedConversionKind::Identity
            || key_arg.conversion.from != string_ty
            || key_arg.conversion.to != string_ty
        {
            return Err(
                    "Set/GetAny-i32 map_get proof call lost its exact Checker updated-root/query operands"
                        .into(),
                );
        }
        (
            "map_get",
            receipt.get_result_local.clone(),
            receipt.get_result_type.clone(),
            "get_any",
            vec![
                MirValueId::new(format!("local:{}", receipt.updated_root_local.0 .0))
                    .map_err(|error| error.to_string())?,
                MirValueId::new(format!("literal:{}", receipt.get_key.0))
                    .map_err(|error| error.to_string())?,
            ],
        )
    };
    if !matches!(&call.callee, ResolvedCallee::Builtin(name) if name.as_str() == builtin_name)
        || call.result != result_ty
        || point.expression.ty != result_ty
        || call.arguments.len() != arguments.len()
    {
        return Err(format!(
            "Set/GetAny-i32 proof-call bridge is detached from exact {builtin_name} call/result identity"
        ));
    }
    let result = MirValueId::new(format!("local:{}", result_local.0 .0))
        .map_err(|error| error.to_string())?;
    let instruction = super::MirInstructionId::new(format!(
        "inst:ordinary_map_set_get_any_i32_lineage:{instruction_name}:{}",
        point.call_node.0
    ))
    .map_err(|error| error.to_string())?;
    let operation_matches = function.blocks.values().any(|block| {
        block.instructions.iter().any(|candidate| {
            candidate.id == instruction
                && match &candidate.kind {
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2New { result: actual } => {
                        actual == &result && builtin_name == "map_new"
                    }
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult {
                        result: actual,
                        source,
                        key,
                        value,
                    } => {
                        actual == &result
                            && arguments.as_slice() == [source.clone(), key.clone(), value.clone()]
                            && builtin_name == "map_set"
                    }
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny {
                        result: actual,
                        root,
                        key,
                    } => {
                        actual == &result
                            && arguments.as_slice() == [root.clone(), key.clone()]
                            && builtin_name == "map_get"
                    }
                    _ => false,
                }
        })
    });
    if !operation_matches {
        return Err(format!(
            "Set/GetAny-i32 proof-call bridge is detached from canonical {builtin_name} MIR operation"
        ));
    }
    Ok(Some(OrdinaryMapNewResultCallBridge {
        instruction,
        result,
        arguments,
    }))
}

fn ordinary_map_set_get_any_result_i32_call_bridge_for_source(
    point: &crate::core::ir::TypedCallProofPoint,
    callable: &crate::core::ir::ResolvedCallable,
    function: &MirFunction,
    receipt: &crate::core::ownership::OrdinaryMapOwnedAnyV2SetGetAnyResultI32LineageReceipt,
) -> Result<Option<OrdinaryMapNewResultCallBridge>, String> {
    use crate::core::ir::{
        CheckedConversionKind, ResolvedCallee, ResolvedExprKind, ResolvedLiteral,
    };
    if ![
        &receipt.new_operation,
        &receipt.set_operation,
        &receipt.get_operation,
    ]
    .contains(&&point.call_node)
    {
        return Ok(None);
    }
    if callable.owner != receipt.owner
        || function.owner != receipt.owner
        || point.owner != receipt.owner
        || point.body_owner != receipt.owner
        || point.call_node != point.expression.node_id
    {
        return Err(
            "Set/GetAny-Result-i32 proof-call bridge has a mismatched Checker owner or call identity"
                .into(),
        );
    }
    let ResolvedExprKind::Call(call) = &point.expression.kind else {
        return Err("Set/GetAny-Result-i32 proof-call source point is not a resolved Call".into());
    };
    let (builtin_name, result_local, result_type, instruction_role, arguments) = if point.call_node
        == receipt.new_operation
    {
        (
            "map_new",
            receipt.new_result_local.clone(),
            receipt.new_result_type.clone(),
            "new_result",
            Vec::new(),
        )
    } else if point.call_node == receipt.set_operation {
        let [root_arg, key_arg, value_arg] = call.arguments.as_slice() else {
            return Err("Set/GetAny-Result-i32 map_set requires root/key/i32 arguments".into());
        };
        let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        if root_arg.value.node_id != receipt.set_receiver
            || !matches!(&root_arg.value.kind, ResolvedExprKind::Load(place) if place.base == receipt.initial_root_local && place.projections.is_empty())
            || root_arg.conversion.kind != CheckedConversionKind::Identity
            || root_arg.conversion.from != receipt.initial_root_type
            || root_arg.conversion.to != receipt.initial_root_type
            || key_arg.value.node_id != receipt.set_key
            || !matches!(&key_arg.value.kind, ResolvedExprKind::Literal(ResolvedLiteral::String(value)) if value == &receipt.set_key_value)
            || key_arg.value.ty != string_type
            || key_arg.conversion.kind != CheckedConversionKind::Identity
            || key_arg.conversion.from != string_type
            || key_arg.conversion.to != string_type
            || value_arg.value.node_id != receipt.set_value
            || !matches!(&value_arg.value.kind, ResolvedExprKind::Literal(ResolvedLiteral::Int(value)) if *value == receipt.set_i32_value)
            || value_arg.value.ty != i32_type
            || value_arg.conversion.kind != CheckedConversionKind::Identity
            || value_arg.conversion.from != i32_type
            || value_arg.conversion.to != i32_type
        {
            return Err(
                "Set/GetAny-Result-i32 map_set call lost its exact Checker root/key/value operands"
                    .into(),
            );
        }
        (
            "map_set",
            receipt.set_result_local.clone(),
            receipt.set_result_type.clone(),
            "set_result",
            vec![
                MirValueId::new(format!("local:{}", receipt.initial_root_local.0 .0))
                    .map_err(|error| error.to_string())?,
                MirValueId::new(format!("literal:{}", receipt.set_key.0))
                    .map_err(|error| error.to_string())?,
                MirValueId::new(format!("literal:{}", receipt.set_value.0))
                    .map_err(|error| error.to_string())?,
            ],
        )
    } else {
        let [root_arg, key_arg] = call.arguments.as_slice() else {
            return Err("Set/GetAny-Result-i32 map_get_any requires root/key arguments".into());
        };
        let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        if root_arg.value.node_id != receipt.get_receiver
            || !matches!(&root_arg.value.kind, ResolvedExprKind::Load(place) if place.base == receipt.updated_root_local && place.projections.is_empty())
            || root_arg.conversion.kind != CheckedConversionKind::Identity
            || root_arg.conversion.from != receipt.updated_root_type
            || root_arg.conversion.to != receipt.updated_root_type
            || key_arg.value.node_id != receipt.get_key
            || !matches!(&key_arg.value.kind, ResolvedExprKind::Literal(ResolvedLiteral::String(value)) if value == &receipt.get_key_value)
            || key_arg.value.ty != string_type
            || key_arg.conversion.kind != CheckedConversionKind::Identity
            || key_arg.conversion.from != string_type
            || key_arg.conversion.to != string_type
        {
            return Err(
                "Set/GetAny-Result-i32 map_get_any call lost its exact updated-root/query operands"
                    .into(),
            );
        }
        (
            "map_get_any",
            receipt.get_result_local.clone(),
            receipt.get_result_type.clone(),
            "get_any_result",
            vec![
                MirValueId::new(format!("local:{}", receipt.updated_root_local.0 .0))
                    .map_err(|error| error.to_string())?,
                MirValueId::new(format!("literal:{}", receipt.get_key.0))
                    .map_err(|error| error.to_string())?,
            ],
        )
    };

    if !matches!(&call.callee, ResolvedCallee::Builtin(name) if name.as_str() == builtin_name)
        || call.result != result_type
        || point.expression.ty != result_type
        || call.type_arguments.len() != 0
        || call.arguments.len() != arguments.len()
    {
        return Err(format!(
            "Set/GetAny-Result-i32 proof-call bridge is detached from exact {builtin_name} call/result identity"
        ));
    }
    let result = MirValueId::new(format!("local:{}", result_local.0 .0))
        .map_err(|error| error.to_string())?;
    let instruction = super::MirInstructionId::new(format!(
        "inst:ordinary_map_set_get_any_result_i32_lineage:{instruction_role}:{}",
        point.call_node.0
    ))
    .map_err(|error| error.to_string())?;
    let operation_matches = function.blocks.values().any(|block| {
        block.instructions.iter().any(|candidate| {
            candidate.id == instruction
                && match &candidate.kind {
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2New { result: actual } => {
                        actual == &result && builtin_name == "map_new"
                    }
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult {
                        result: actual,
                        source,
                        key,
                        value,
                    } => {
                        actual == &result
                            && arguments.as_slice() == [source.clone(), key.clone(), value.clone()]
                            && builtin_name == "map_set"
                    }
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2GetAnyResult {
                        result: actual,
                        root,
                        key,
                    } => {
                        actual == &result
                            && arguments.as_slice() == [root.clone(), key.clone()]
                            && builtin_name == "map_get_any"
                    }
                    _ => false,
                }
        })
    });
    if !operation_matches {
        return Err(format!(
            "Set/GetAny-Result-i32 proof-call bridge is detached from canonical {builtin_name} MIR operation"
        ));
    }
    Ok(Some(OrdinaryMapNewResultCallBridge {
        instruction,
        result,
        arguments,
    }))
}

fn ordinary_map_set_get_string_call_bridge_for_source(
    point: &crate::core::ir::TypedCallProofPoint,
    callable: &crate::core::ir::ResolvedCallable,
    function: &MirFunction,
    receipt: &crate::core::ownership::OrdinaryMapOwnedAnyV2SetGetStringLineageReceipt,
) -> Result<Option<OrdinaryMapNewResultCallBridge>, String> {
    use crate::core::ir::{
        CheckedConversionKind, ResolvedCallee, ResolvedExprKind, ResolvedLiteral,
    };
    if ![
        &receipt.new_operation,
        &receipt.set_operation,
        &receipt.get_operation,
    ]
    .contains(&&point.call_node)
    {
        return Ok(None);
    }
    if callable.owner != receipt.owner
        || point.owner != receipt.owner
        || point.body_owner != receipt.owner
        || point.expression.node_id != point.call_node
    {
        return Err(
            "Set/Get-String proof-call bridge has a mismatched Checker owner or call identity"
                .into(),
        );
    }
    let ResolvedExprKind::Call(call) = &point.expression.kind else {
        return Err("Set/Get-String proof-call bridge source point is not a resolved Call".into());
    };
    let (builtin_name, result_local, result_ty, instruction_name, arguments) = if point.call_node
        == receipt.new_operation
    {
        (
            "map_new",
            receipt.new_result_local.clone(),
            receipt.set_result_type.clone(),
            "new_result",
            Vec::new(),
        )
    } else if point.call_node == receipt.set_operation {
        let [root_arg, key_arg, value_arg] = call.arguments.as_slice() else {
            return Err(
                "Set/Get-String map_set proof call requires exactly root/key/String arguments"
                    .into(),
            );
        };
        let root_ty = &receipt.initial_root_type;
        let string_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        if root_arg.value.node_id != receipt.set_receiver
            || !matches!(&root_arg.value.kind, ResolvedExprKind::Load(place) if place.base == receipt.initial_root_local && place.projections.is_empty())
            || root_arg.conversion.kind != CheckedConversionKind::Identity
            || root_arg.conversion.from != *root_ty
            || root_arg.conversion.to != *root_ty
            || !matches!(&key_arg.value.kind, ResolvedExprKind::Literal(ResolvedLiteral::String(value)) if value == "answer")
            || key_arg.value.ty != string_ty
            || key_arg.conversion.kind != CheckedConversionKind::Identity
            || key_arg.conversion.from != string_ty
            || key_arg.conversion.to != string_ty
            || !matches!(&value_arg.value.kind, ResolvedExprKind::Load(place) if place.base == receipt.set_payload_local && place.projections.is_empty())
            || value_arg.conversion.kind != CheckedConversionKind::Identity
            || value_arg.conversion.from != string_ty
            || value_arg.conversion.to != string_ty
        {
            return Err("Set/Get-String map_set proof call lost its exact root/\"answer\"/String source operands".into());
        }
        (
            "map_set",
            receipt.set_result_local.clone(),
            receipt.set_result_type.clone(),
            "set_string_result",
            vec![
                MirValueId::new(format!("local:{}", receipt.initial_root_local.0 .0))
                    .map_err(|error| error.to_string())?,
                MirValueId::new("literal:ordinary_map_set_get_string_lineage:set-answer")
                    .map_err(|error| error.to_string())?,
                MirValueId::new(format!("local:{}", receipt.set_payload_local.0 .0))
                    .map_err(|error| error.to_string())?,
            ],
        )
    } else {
        let [root_arg, key_arg] = call.arguments.as_slice() else {
            return Err(
                "Set/Get-String map_get_string proof call requires exactly root/key arguments"
                    .into(),
            );
        };
        let string_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        if root_arg.value.node_id != receipt.get_receiver
            || !matches!(&root_arg.value.kind, ResolvedExprKind::Load(place) if place.base == receipt.updated_root_local && place.projections.is_empty())
            || root_arg.conversion.kind != CheckedConversionKind::Identity
            || root_arg.conversion.from != receipt.updated_root_type
            || root_arg.conversion.to != receipt.updated_root_type
            || !matches!(&key_arg.value.kind, ResolvedExprKind::Literal(ResolvedLiteral::String(value)) if value == "answer")
            || key_arg.value.ty != string_ty
            || key_arg.conversion.kind != CheckedConversionKind::Identity
            || key_arg.conversion.from != string_ty
            || key_arg.conversion.to != string_ty
        {
            return Err("Set/Get-String map_get_string proof call lost its exact updated-root/\"answer\" source operands".into());
        }
        (
            "map_get_string",
            receipt.get_result_local.clone(),
            receipt.get_result_type.clone(),
            "get_string_result",
            vec![
                MirValueId::new(format!("local:{}", receipt.updated_root_local.0 .0))
                    .map_err(|error| error.to_string())?,
                MirValueId::new("literal:ordinary_map_set_get_string_lineage:get-answer")
                    .map_err(|error| error.to_string())?,
            ],
        )
    };
    if !matches!(&call.callee, ResolvedCallee::Builtin(name) if name.as_str() == builtin_name)
        || call.result != result_ty
        || point.expression.ty != result_ty
        || call.arguments.len() != arguments.len()
    {
        return Err(format!(
            "Set/Get-String proof-call bridge is detached from exact {builtin_name} call/result identity"
        ));
    }
    let result = MirValueId::new(format!("local:{}", result_local.0 .0))
        .map_err(|error| error.to_string())?;
    let instruction = super::MirInstructionId::new(format!(
        "inst:ordinary_map_set_get_string_lineage:{instruction_name}:{}",
        point.call_node.0
    ))
    .map_err(|error| error.to_string())?;
    let operation_matches = function.blocks.values().any(|block| {
        block.instructions.iter().any(|candidate| {
            candidate.id == instruction
                && match &candidate.kind {
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2New { result: actual } => {
                        actual == &result && builtin_name == "map_new"
                    }
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2SetStringResult {
                        result: actual,
                        source,
                        key,
                        value,
                    } => {
                        actual == &result
                            && arguments.as_slice() == [source.clone(), key.clone(), value.clone()]
                            && builtin_name == "map_set"
                    }
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2GetStringResult {
                        result: actual,
                        root,
                        key,
                    } => {
                        actual == &result
                            && arguments.as_slice() == [root.clone(), key.clone()]
                            && builtin_name == "map_get_string"
                    }
                    _ => false,
                }
        })
    });
    if !operation_matches {
        return Err(format!(
            "Set/Get-String proof-call bridge is detached from canonical {builtin_name} MIR operation"
        ));
    }
    Ok(Some(OrdinaryMapNewResultCallBridge {
        instruction,
        result,
        arguments,
    }))
}

pub(crate) fn lower_proof_worklist(
    callable: &crate::core::ir::ResolvedCallable,
    function: &MirFunction,
) -> Result<Option<MirProofWorklist>, Vec<super::lower::MirLoweringError>> {
    lower_proof_worklist_inner(callable, function, None, None, None, None, false)
}

pub(crate) fn lower_proof_worklist_with_new_result_receipt(
    callable: &crate::core::ir::ResolvedCallable,
    function: &MirFunction,
    receipt: &crate::core::ownership::OrdinaryMapOwnedAnyV2NewResultReceipt,
) -> Result<Option<MirProofWorklist>, Vec<super::lower::MirLoweringError>> {
    lower_proof_worklist_inner(callable, function, Some(receipt), None, None, None, false)
}

pub(crate) fn lower_proof_worklist_with_set_result_receipt(
    callable: &crate::core::ir::ResolvedCallable,
    function: &MirFunction,
    receipt: &crate::core::ownership::OrdinaryMapOwnedAnyV2SetLineageReceipt,
) -> Result<Option<MirProofWorklist>, Vec<super::lower::MirLoweringError>> {
    let typed = callable.proof_point_worklist();
    let expected_calls = BTreeSet::from([
        receipt.new_operation.clone(),
        receipt.set_operation.clone(),
        receipt.size_operation.clone(),
    ]);
    let actual_calls = typed
        .calls
        .iter()
        .map(|call| call.call_node.clone())
        .collect::<BTreeSet<_>>();
    let boundaries_are_exact = boundaries_are_set_result_exact(&typed, receipt);
    let has_direct_contract = callable.body.root.statements.iter().any(|statement| {
        matches!(
            &statement.kind,
            crate::core::ir::ResolvedStmtKind::Contract { .. }
        )
    });
    if callable.owner != receipt.owner
        || function.owner != receipt.owner
        || function.ownership.ordinary_map_set_lineage_receipt() != Some(receipt)
        || callable
            .resources
            .ordinary_map_owned_any_v2_set_lineage_receipt
            .as_ref()
            != Some(receipt)
        || receipt.profile_version
            != crate::core::ownership::OrdinaryMapOwnedAnyV2SetLineageProfileVersion::V1
        || !typed.validate_against(callable)
        || typed.owner != receipt.owner
        || !matches!(
            callable.body.root.result.as_deref(),
            Some(crate::core::ir::ResolvedExpr {
                node_id,
                kind: crate::core::ir::ResolvedExprKind::Match { .. },
                ..
            }) if node_id == &receipt.outer_match
        )
        || typed.calls.len() != expected_calls.len()
        || actual_calls != expected_calls
        || !typed.points.is_empty()
        || !typed.loop_invariant_points().is_empty()
        || !typed.deferred_calls.is_empty()
        || typed.default_roots.iter().any(|root| {
            !root.points.is_empty() || !root.calls.is_empty() || !root.boundaries.is_empty()
        })
        || !typed.default_uses.is_empty()
        || !boundaries_are_exact
        || !has_direct_contract
    {
        return Err(vec![super::lower::MirLoweringError {
            node_id: callable.owner.clone(),
            message: format!(
                "SetResult proof worklist is not the exact checked New/Set/Size Result-match profile (owner_match={}, function_owner_match={}, MIR_receipt_match={}, CheckedProgram_receipt_match={}, profile_v1={}, worklist_valid={}, root_match={}, exact_calls={}, no_unmodeled_roots={}, exact_boundaries={}, direct_contract={})",
                callable.owner == receipt.owner,
                function.owner == receipt.owner,
                function.ownership.ordinary_map_set_lineage_receipt() == Some(receipt),
                callable
                    .resources
                    .ordinary_map_owned_any_v2_set_lineage_receipt
                    .as_ref()
                    == Some(receipt),
                receipt.profile_version
                    == crate::core::ownership::OrdinaryMapOwnedAnyV2SetLineageProfileVersion::V1,
                typed.validate_against(callable),
                matches!(
                    callable.body.root.result.as_deref(),
                    Some(crate::core::ir::ResolvedExpr {
                        node_id,
                        kind: crate::core::ir::ResolvedExprKind::Match { .. },
                        ..
                    }) if node_id == &receipt.outer_match
                ),
                typed.calls.len() == expected_calls.len() && actual_calls == expected_calls,
                typed.points.is_empty()
                    && typed.loop_invariant_points().is_empty()
                    && typed.deferred_calls.is_empty()
                    && typed.default_roots.iter().all(|root| {
                        root.points.is_empty() && root.calls.is_empty() && root.boundaries.is_empty()
                    })
                    && typed.default_uses.is_empty(),
                boundaries_are_exact,
                has_direct_contract,
            ),
        }]);
    }
    let worklist = lower_proof_worklist_inner(callable, function, None, None, None, None, true)?;
    if worklist.is_none() {
        return Err(vec![super::lower::MirLoweringError {
            node_id: callable.owner.clone(),
            message: "SetResult L2 direct contract did not produce a proof worklist".into(),
        }]);
    }
    Ok(worklist)
}

pub(crate) fn lower_proof_worklist_with_set_get_i32_receipt(
    callable: &crate::core::ir::ResolvedCallable,
    function: &MirFunction,
    receipt: &crate::core::ownership::OrdinaryMapOwnedAnyV2SetGetI32LineageReceipt,
) -> Result<Option<MirProofWorklist>, Vec<super::lower::MirLoweringError>> {
    lower_proof_worklist_inner(callable, function, None, Some(receipt), None, None, false)
}

pub(crate) fn lower_proof_worklist_with_set_get_any_i32_receipt(
    callable: &crate::core::ir::ResolvedCallable,
    function: &MirFunction,
    receipt: &crate::core::ownership::OrdinaryMapOwnedAnyV2SetGetAnyI32LineageReceipt,
) -> Result<Option<MirProofWorklist>, Vec<super::lower::MirLoweringError>> {
    let typed_worklist = callable.proof_point_worklist();
    let exact_worklist = callable.has_exact_set_get_any_i32_match_worklist(&typed_worklist);
    if callable.owner != receipt.owner
        || function.owner != receipt.owner
        || function
            .ownership
            .ordinary_map_set_get_any_i32_lineage_receipt()
            != Some(receipt)
        || !exact_worklist
    {
        return Err(vec![super::lower::MirLoweringError {
            node_id: callable.owner.clone(),
            message: format!(
                "Set/GetAny-i32 proof worklist is detached from the exact Checker lineage receipt (owner_match={}, function_owner_match={}, receipt_match={}, worklist_valid={}, exact_worklist={}, root_result={:?}, calls={:?}, points={}, deferred_calls={}, boundaries={:?})",
                callable.owner == receipt.owner,
                function.owner == receipt.owner,
                function
                    .ownership
                    .ordinary_map_set_get_any_i32_lineage_receipt()
                    == Some(receipt),
                typed_worklist.validate_against(callable),
                exact_worklist,
                callable.body.root.result.as_ref().map(|result| &result.node_id),
                typed_worklist
                    .calls
                    .iter()
                    .map(|call| &call.call_node)
                    .collect::<Vec<_>>(),
                typed_worklist.points.len(),
                typed_worklist.deferred_calls.len(),
                typed_worklist.boundaries,
            ),
        }]);
    }
    lower_proof_worklist_inner(callable, function, None, None, Some(receipt), None, true)
}

pub(crate) fn lower_proof_worklist_with_set_get_any_result_i32_receipt(
    callable: &crate::core::ir::ResolvedCallable,
    function: &MirFunction,
    receipt: &crate::core::ownership::OrdinaryMapOwnedAnyV2SetGetAnyResultI32LineageReceipt,
) -> Result<Option<MirProofWorklist>, Vec<super::lower::MirLoweringError>> {
    let typed_worklist = callable.proof_point_worklist();
    let exact_worklist = callable.has_exact_set_get_any_result_i32_match_worklist(&typed_worklist);
    if callable.owner != receipt.owner
        || function.owner != receipt.owner
        || function
            .ownership
            .ordinary_map_owned_any_v2_set_get_any_result_i32_lineage_receipt()
            != Some(receipt)
        || callable
            .resources
            .ordinary_map_owned_any_v2_set_get_any_result_i32_lineage_receipt
            .as_ref()
            != Some(receipt)
        || !exact_worklist
    {
        return Err(vec![super::lower::MirLoweringError {
            node_id: callable.owner.clone(),
            message: format!(
                "Set/GetAny-Result-i32 proof worklist is detached from its exact Checker receipt (owner_match={}, function_owner_match={}, MIR_receipt_match={}, CheckedProgram_receipt_match={}, worklist_valid={}, exact_worklist={}, root_result={:?}, calls={:?}, points={}, deferred_calls={}, boundaries={:?})",
                callable.owner == receipt.owner,
                function.owner == receipt.owner,
                function
                    .ownership
                    .ordinary_map_owned_any_v2_set_get_any_result_i32_lineage_receipt()
                    == Some(receipt),
                callable
                    .resources
                    .ordinary_map_owned_any_v2_set_get_any_result_i32_lineage_receipt
                    .as_ref()
                    == Some(receipt),
                typed_worklist.validate_against(callable),
                exact_worklist,
                callable.body.root.result.as_ref().map(|result| &result.node_id),
                typed_worklist
                    .calls
                    .iter()
                    .map(|call| &call.call_node)
                    .collect::<Vec<_>>(),
                typed_worklist.points.len(),
                typed_worklist.deferred_calls.len(),
                typed_worklist.boundaries,
            ),
        }]);
    }
    lower_proof_worklist_inner(callable, function, None, None, None, Some(receipt), true)
}

pub(crate) fn lower_proof_worklist_with_set_get_string_receipt(
    callable: &crate::core::ir::ResolvedCallable,
    function: &MirFunction,
    receipt: &crate::core::ownership::OrdinaryMapOwnedAnyV2SetGetStringLineageReceipt,
) -> Result<Option<MirProofWorklist>, Vec<super::lower::MirLoweringError>> {
    if callable.owner != receipt.owner
        || function.owner != receipt.owner
        || function
            .ownership
            .ordinary_map_set_get_string_lineage_receipt()
            != Some(receipt)
    {
        return Err(vec![super::lower::MirLoweringError {
            node_id: callable.owner.clone(),
            message:
                "Set/Get-String proof worklist is detached from the exact Checker lineage receipt"
                    .into(),
        }]);
    }
    // This exact profile retains all three checked Map operation call points
    // beside its direct Ensures root, even though ordinary call-only bodies
    // normally do not need a standalone MIR worklist.
    lower_proof_worklist_inner(callable, function, None, None, None, None, true)
}

pub(crate) fn lower_proof_worklist_with_values_local_index_i32_result_branch_receipt(
    callable: &crate::core::ir::ResolvedCallable,
    function: &MirFunction,
    receipt: &crate::core::ownership::OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultBranchReceipt,
) -> Result<Option<MirProofWorklist>, Vec<super::lower::MirLoweringError>> {
    let checked_receipt = callable
        .resources
        .ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
        .and_then(|cfg| cfg.values_local_index_i32_result_branch_receipt.as_ref());
    let mir_receipt = function
        .ownership
        .ordinary_map_owned_any_v2_cfg_receipt()
        .and_then(|cfg| cfg.values_local_index_i32_result_branch_receipt.as_ref());
    let direct_ensures = callable
        .body
        .root
        .statements
        .iter()
        .filter_map(|statement| match &statement.kind {
            crate::core::ir::ResolvedStmtKind::Contract {
                kind: crate::core::ir::ContractKind::Ensures,
                ..
            } => Some(statement.node_id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if callable.owner != receipt.owner
        || function.owner != receipt.owner
        || checked_receipt != Some(receipt)
        || mir_receipt != Some(receipt)
        || direct_ensures.len() != 1
        || function.contracts.len() != 1
        || function.contracts[0].kind != MirContractKind::Ensures
        || function.contracts[0].id != direct_ensures[0]
    {
        return Err(vec![super::lower::MirLoweringError {
            node_id: callable.owner.clone(),
            message: "ValuesLocalIndexI32 Result proof worklist is detached from its exact Checker receipt or sole direct Ensures".into(),
        }]);
    }
    // A direct Ensures root is a proof obligation even when the checked body
    // has no standalone Math/boundary point. Preserve the exact typed call
    // census in the worklist so the verifier cannot treat it as a no-op.
    let lowered = lower_proof_worklist_inner(callable, function, None, None, None, None, true)?;
    Ok(lowered)
}

fn lower_proof_worklist_inner(
    callable: &crate::core::ir::ResolvedCallable,
    function: &MirFunction,
    new_result_receipt: Option<&crate::core::ownership::OrdinaryMapOwnedAnyV2NewResultReceipt>,
    set_get_i32_receipt: Option<
        &crate::core::ownership::OrdinaryMapOwnedAnyV2SetGetI32LineageReceipt,
    >,
    set_get_any_i32_receipt: Option<
        &crate::core::ownership::OrdinaryMapOwnedAnyV2SetGetAnyI32LineageReceipt,
    >,
    set_get_any_result_i32_receipt: Option<
        &crate::core::ownership::OrdinaryMapOwnedAnyV2SetGetAnyResultI32LineageReceipt,
    >,
    force_owner_worklist: bool,
) -> Result<Option<MirProofWorklist>, Vec<super::lower::MirLoweringError>> {
    use crate::core::ir::{TypedProofOperation as T, TypedProofScopeFrame};

    let typed = callable.proof_point_worklist();
    if !typed.validate_against(callable) {
        return Err(vec![super::lower::MirLoweringError {
            node_id: callable.owner.clone(),
            message: "typed proof worklist failed checker-body revalidation".into(),
        }]);
    }

    fn point_id(point: &crate::core::ir::TypedProofPointId) -> MirProofPointId {
        MirProofPointId {
            owner: point.owner.clone(),
            statement: point.statement.clone(),
            ordinal: point.ordinal,
        }
    }
    fn call_id(point: &crate::core::ir::TypedCallProofPoint) -> MirProofCallPointId {
        MirProofCallPointId {
            owner: point.owner.clone(),
            statement: point.statement.clone(),
            call_node: point.call_node.clone(),
            order: point.order,
        }
    }
    fn boundary_call(point: &crate::core::ir::TypedCallProofPoint) -> MirProofCallBoundary {
        let (callee, type_arguments, result_type, declared_effects, argument_count, arguments) =
            match &point.expression.kind {
                crate::core::ir::ResolvedExprKind::Call(call) => (
                    Some(call.callee.clone()),
                    call.type_arguments.clone(),
                    Some(call.result.clone()),
                    call.effects.clone(),
                    call.arguments.len(),
                    call.arguments
                        .iter()
                        .enumerate()
                        .map(|(ordinal, argument)| MirProofBoundaryArgument {
                            ordinal,
                            parameter: argument.parameter.clone(),
                            expression: argument.value.node_id.clone(),
                            value_type: argument.value.ty.clone(),
                            conversion: argument.conversion.clone(),
                        })
                        .collect(),
                ),
                _ => (None, Vec::new(), None, Vec::new(), 0, Vec::new()),
            };
        MirProofCallBoundary {
            id: call_id(point),
            body_owner: point.body_owner.clone(),
            expression: point.expression.node_id.clone(),
            scope: point.scope.clone(),
            guards: point
                .guards
                .iter()
                .map(|guard| MirProofBoundaryGuard {
                    condition: guard.condition.node_id.clone(),
                    when: guard.when,
                })
                .collect(),
            disposition: point.disposition.clone(),
            callee,
            expression_type: point.expression.ty.clone(),
            type_arguments,
            result_type,
            declared_effects,
            effect: point.effect.clone(),
            argument_count,
            arguments,
        }
    }
    fn boundary_worklist(
        typed: &crate::core::ir::TypedProofPointWorklist,
        callable: &crate::core::ir::ResolvedCallable,
        reason: String,
    ) -> MirProofWorklist {
        let mut expected_math_points = typed
            .points
            .iter()
            .map(|point| point_id(&point.id))
            .collect::<Vec<_>>();
        expected_math_points.sort();
        let mut expected_loop_invariants = typed
            .loop_invariant_points()
            .iter()
            .map(|point| point_id(&point.id))
            .collect::<Vec<_>>();
        expected_loop_invariants.sort();
        let mut expected_calls = typed.calls.iter().map(call_id).collect::<Vec<_>>();
        expected_calls.sort();
        let mut expected_deferred_calls = typed
            .deferred_calls
            .iter()
            .map(boundary_call)
            .collect::<Vec<_>>();
        expected_deferred_calls.sort_by_key(|call| call.id.clone());
        let mut expected_scoped_boundaries = typed
            .boundaries
            .iter()
            .map(|boundary| boundary.point.clone())
            .collect::<Vec<_>>();
        expected_scoped_boundaries.sort();
        // A single checker program point may carry more than one boundary
        // reason. The identity-only worklist census records that point once;
        // the reason-bearing entries remain in `scoped_boundaries`.
        expected_scoped_boundaries.dedup();

        // Only defaults which carry a proof root or a real call/boundary need
        // a verifier boundary. Ordinary default literals remain part of the
        // checked declaration ABI, but cannot affect a scoped proof term.
        let active_defaults = typed
            .default_roots
            .iter()
            .filter(|root| {
                !root.points.is_empty() || !root.calls.is_empty() || !root.boundaries.is_empty()
            })
            .collect::<Vec<_>>();
        let active_default_keys = active_defaults
            .iter()
            .map(|root| (root.owner.clone(), root.parameter.clone()))
            .collect::<BTreeSet<_>>();
        let mut expected_default_roots = active_defaults
            .iter()
            .map(|root| MirProofDefaultRootId {
                owner: root.owner.clone(),
                parameter: root.parameter.clone(),
                expression: root.expression.clone(),
            })
            .collect::<Vec<_>>();
        expected_default_roots.sort();
        let mut expected_default_math_points = active_defaults
            .iter()
            .flat_map(|root| root.points.iter())
            .map(|point| MirProofBoundaryMathId {
                point: point_id(&point.id),
                order: point.order,
                expression: point.condition.node_id.clone(),
                scope: point.scope.clone(),
            })
            .collect::<Vec<_>>();
        expected_default_math_points.sort();
        let mut expected_default_calls = active_defaults
            .iter()
            .flat_map(|root| root.calls.iter())
            .map(boundary_call)
            .collect::<Vec<_>>();
        expected_default_calls.sort_by_key(|call| call.id.clone());
        let mut expected_default_boundaries = active_defaults
            .iter()
            .flat_map(|root| {
                root.boundaries
                    .iter()
                    .map(move |boundary| MirProofDefaultBoundary {
                        root: MirProofDefaultRootId {
                            owner: root.owner.clone(),
                            parameter: root.parameter.clone(),
                            expression: root.expression.clone(),
                        },
                        point: boundary.point.clone(),
                        reason: boundary.reason.clone(),
                    })
            })
            .collect::<Vec<_>>();
        expected_default_boundaries.sort();
        let mut expected_default_uses = typed
            .default_uses
            .iter()
            .filter(|use_id| {
                active_default_keys
                    .contains(&(use_id.callee_owner.clone(), use_id.parameter.clone()))
            })
            .map(|use_id| MirProofDefaultUseId {
                caller_owner: use_id.caller_owner.clone(),
                call_node: use_id.call_node.clone(),
                callee_owner: use_id.callee_owner.clone(),
                parameter: use_id.parameter.clone(),
                argument_node: use_id.argument_node.clone(),
                parameter_index: use_id.parameter_index,
                order: use_id.order,
                scope: use_id.scope.clone(),
                guards: use_id
                    .guards
                    .iter()
                    .map(|guard| (guard.condition.node_id.clone(), guard.when))
                    .collect(),
                disposition: format!("{:?}", use_id.disposition),
            })
            .collect::<Vec<_>>();
        expected_default_uses.sort();
        let mut expected_place_inputs = callable
            .body
            .place_inputs
            .iter()
            .map(|(point, expression)| MirProofPlaceInputId {
                point: point.clone(),
                expression: expression.node_id.clone(),
            })
            .collect::<Vec<_>>();
        expected_place_inputs.sort();
        let mut boundary_math_points = typed
            .points
            .iter()
            .map(|point| MirProofBoundaryMathId {
                point: point_id(&point.id),
                order: point.order,
                expression: point.condition.node_id.clone(),
                scope: point.scope.clone(),
            })
            .collect::<Vec<_>>();
        boundary_math_points.sort();
        let mut boundary_loop_invariants = typed
            .loop_invariant_points()
            .iter()
            .map(|point| MirProofBoundaryMathId {
                point: point_id(&point.id),
                // Invariants are header obligations rather than timeline
                // events; their stable ordinal is their local order.
                order: u64::from(point.id.ordinal),
                expression: point.condition.node_id.clone(),
                scope: point.scope.clone(),
            })
            .collect::<Vec<_>>();
        boundary_loop_invariants.sort();
        let mut boundary_calls = typed.calls.iter().map(boundary_call).collect::<Vec<_>>();
        boundary_calls.sort_by_key(|call| call.id.clone());
        let mut boundary_deferred_calls = typed
            .deferred_calls
            .iter()
            .map(boundary_call)
            .collect::<Vec<_>>();
        boundary_deferred_calls.sort_by_key(|call| call.id.clone());
        let mut boundary_scoped = typed
            .boundaries
            .iter()
            .map(|boundary| MirProofSourceBoundary {
                point: boundary.point.clone(),
                reason: boundary.reason.clone(),
            })
            .collect::<Vec<_>>();
        boundary_scoped.sort();

        let boundary = MirProofBoundary {
            point: callable.owner.clone(),
            reason,
            scoped_boundaries: boundary_scoped,
            math_points: boundary_math_points,
            loop_invariants: boundary_loop_invariants,
            calls: boundary_calls,
            deferred_calls: boundary_deferred_calls,
            default_roots: expected_default_roots.clone(),
            default_math_points: expected_default_math_points.clone(),
            default_calls: expected_default_calls.clone(),
            default_boundaries: expected_default_boundaries.clone(),
            default_uses: expected_default_uses.clone(),
        };
        MirProofWorklist {
            owner: typed.owner.clone(),
            expected_math_points,
            expected_loop_invariants,
            expected_calls,
            expected_deferred_calls,
            expected_scoped_boundaries,
            expected_default_roots,
            expected_default_math_points,
            expected_default_calls,
            expected_default_boundaries,
            expected_default_uses,
            expected_place_inputs,
            boundaries: vec![boundary],
            root: MirProofBlock {
                id: callable.body.root.node_id.clone(),
                operations: Vec::new(),
            },
            private_source_witness: [0; 32],
        }
        .seal_source_witness()
    }

    let active_defaults = typed
        .default_roots
        .iter()
        .filter(|root| {
            !root.points.is_empty() || !root.calls.is_empty() || !root.boundaries.is_empty()
        })
        .collect::<Vec<_>>();
    let active_default = typed.default_roots.iter().any(|root| {
        !root.points.is_empty() || !root.calls.is_empty() || !root.boundaries.is_empty()
    });
    let set_result_receipt = function.ownership.ordinary_map_set_lineage_receipt();
    let bridged_body_calls = typed
        .calls
        .iter()
        .map(|point| {
            let new_bridge = new_result_receipt
                .map(|receipt| {
                    ordinary_map_new_result_call_bridge_for_source(
                        point, callable, function, receipt,
                    )
                })
                .unwrap_or(Ok(None))?;
            if new_bridge.is_some() {
                return Ok(new_bridge);
            }
            let set_result_bridge = set_result_receipt
                .map(|receipt| {
                    ordinary_map_set_result_call_bridge_for_source(
                        point, callable, function, receipt,
                    )
                })
                .unwrap_or(Ok(None))?;
            if set_result_bridge.is_some() {
                return Ok(set_result_bridge);
            }
            let set_get_any_i32_bridge = set_get_any_i32_receipt
                .map(|receipt| {
                    ordinary_map_set_get_any_i32_call_bridge_for_source(
                        point, callable, function, receipt,
                    )
                })
                .unwrap_or(Ok(None))?;
            if set_get_any_i32_bridge.is_some() {
                return Ok(set_get_any_i32_bridge);
            }
            let set_get_any_result_i32_bridge = set_get_any_result_i32_receipt
                .map(|receipt| {
                    ordinary_map_set_get_any_result_i32_call_bridge_for_source(
                        point, callable, function, receipt,
                    )
                })
                .unwrap_or(Ok(None))?;
            if set_get_any_result_i32_bridge.is_some() {
                return Ok(set_get_any_result_i32_bridge);
            }
            set_get_i32_receipt
                .map(|receipt| {
                    ordinary_map_set_get_i32_call_bridge_for_source(
                        point, callable, function, receipt,
                    )
                })
                .unwrap_or(Ok(None))
                .and_then(|bridge| {
                    if bridge.is_some() {
                        return Ok(bridge);
                    }
                    function
                        .ownership
                        .ordinary_map_set_get_string_lineage_receipt()
                        .map(|receipt| {
                            ordinary_map_set_get_string_call_bridge_for_source(
                                point, callable, function, receipt,
                            )
                        })
                        .unwrap_or(Ok(None))
                })
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|message| {
            vec![super::lower::MirLoweringError {
                node_id: callable.owner.clone(),
                message,
            }]
        })?;
    let retained_new_result_match_boundary = new_result_receipt.and_then(|receipt| {
        let [boundary] = typed.boundaries.as_slice() else {
            return None;
        };
        (boundary.point == receipt.match_node
            && boundary.reason
                == "proof root or effect is nested in an unsupported expression form")
            .then_some(boundary)
    });
    let retained_set_result_match_boundaries = set_result_receipt
        .filter(|receipt| {
            force_owner_worklist
                && callable
                    .resources
                    .ordinary_map_owned_any_v2_set_lineage_receipt
                    .as_ref()
                    == Some(*receipt)
                && function.owner == receipt.owner
                && callable.owner == receipt.owner
                && typed.validate_against(callable)
                && callable
                    .body
                    .root
                    .result
                    .as_deref()
                    .is_some_and(|result| result.node_id == receipt.outer_match)
                && function.contracts.iter().any(|contract| {
                    contract.kind == MirContractKind::Ensures
                        || contract.kind == MirContractKind::Requires
                })
                && function.contracts.len()
                    == callable
                        .body
                        .root
                        .statements
                        .iter()
                        .filter(|statement| {
                            matches!(
                                &statement.kind,
                                crate::core::ir::ResolvedStmtKind::Contract { .. }
                            )
                        })
                        .count()
        })
        .map(|_| typed.boundaries.as_slice());
    let retained_string_match_boundaries = function
        .ownership
        .ordinary_map_set_get_string_lineage_receipt()
        .filter(|receipt| {
            force_owner_worklist
                && callable
                    .resources
                    .ordinary_map_owned_any_v2_set_get_string_lineage_receipt
                    .as_ref()
                    == Some(*receipt)
                && callable.has_exact_string_set_get_match_worklist(&typed)
                && function.owner == receipt.owner
                && receipt.proof_ensures.as_ref().is_some_and(|ensures| {
                    function.contracts.iter().any(|contract| {
                        contract.id == *ensures && contract.kind == MirContractKind::Ensures
                    })
                })
                && function.contracts.len() == 1
        })
        .map(|_| typed.boundaries.as_slice());
    let retained_set_get_any_i32_match_boundaries = function
        .ownership
        .ordinary_map_set_get_any_i32_lineage_receipt()
        .filter(|receipt| {
            force_owner_worklist
                && callable
                    .resources
                    .ordinary_map_owned_any_v2_set_get_any_i32_lineage_receipt
                    .as_ref()
                    == Some(*receipt)
                && callable.has_exact_set_get_any_i32_match_worklist(&typed)
                && function.owner == receipt.owner
                && function.contracts.len() == 1
                && function.contracts[0].kind == MirContractKind::Ensures
        })
        .map(|_| typed.boundaries.as_slice());
    let retained_set_get_any_result_i32_match_boundaries = function
        .ownership
        .ordinary_map_owned_any_v2_set_get_any_result_i32_lineage_receipt()
        .filter(|receipt| {
            force_owner_worklist
                && set_get_any_result_i32_receipt == Some(*receipt)
                && callable
                    .resources
                    .ordinary_map_owned_any_v2_set_get_any_result_i32_lineage_receipt
                    .as_ref()
                    == Some(*receipt)
                && callable.has_exact_set_get_any_result_i32_match_worklist(&typed)
                && function.owner == receipt.owner
                && function.contracts.len() == 1
                && function.contracts[0].kind == MirContractKind::Ensures
                && callable
                    .body
                    .root
                    .statements
                    .iter()
                    .find(|statement| {
                        matches!(
                            &statement.kind,
                            crate::core::ir::ResolvedStmtKind::Contract {
                                kind: crate::core::ir::ContractKind::Ensures,
                                ..
                            }
                        )
                    })
                    .is_some_and(|statement| function.contracts[0].id == statement.node_id)
        })
        .map(|_| typed.boundaries.as_slice());
    let values_local_index_i32_result_branch_receipt = function
        .ownership
        .ordinary_map_owned_any_v2_cfg_receipt()
        .and_then(|cfg| cfg.values_local_index_i32_result_branch_receipt.as_ref());
    let retained_values_local_index_i32_result_branch_calls =
        values_local_index_i32_result_branch_receipt.filter(|receipt| {
            let checked_receipt = callable
                .resources
                .ordinary_map_owned_any_v2_cfg_receipt
                .as_ref()
                .and_then(|cfg| cfg.values_local_index_i32_result_branch_receipt.as_ref());
            let direct_ensures = callable
                .body
                .root
                .statements
                .iter()
                .filter_map(|statement| match &statement.kind {
                    crate::core::ir::ResolvedStmtKind::Contract {
                        kind: crate::core::ir::ContractKind::Ensures,
                        ..
                    } => Some(statement.node_id.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let expected_calls = BTreeSet::from([
                receipt.map_new_operation.clone(),
                receipt.map_set_operation.clone(),
                receipt.values_operation.clone(),
            ]);
            let actual_calls = typed
                .calls
                .iter()
                .map(|call| call.call_node.clone())
                .collect::<BTreeSet<_>>();
            let call_has_disposition = |node: &NodeId, deferred: bool| {
                typed.calls.iter().any(|call| {
                    &call.call_node == node
                        && if deferred {
                            matches!(
                                &call.disposition,
                                crate::core::ir::TypedCallExecutionDisposition::Deferred(_)
                            )
                        } else {
                            matches!(
                                &call.disposition,
                                crate::core::ir::TypedCallExecutionDisposition::Unconditional
                            )
                        }
                })
            };
            force_owner_worklist
                && callable.owner == receipt.owner
                && function.owner == receipt.owner
                && checked_receipt == Some(*receipt)
                && typed.validate_against(callable)
                && typed.root.operations.as_slice().iter().any(|operation| {
                    matches!(operation, T::Call(call) if call.call_node == receipt.map_new_operation)
                })
                && typed.root.operations.len() == 1
                && typed.boundaries.is_empty()
                && typed.deferred_calls.is_empty()
                && expected_calls.len() == 3
                && actual_calls == expected_calls
                && typed.calls.len() == 3
                && call_has_disposition(&receipt.map_new_operation, false)
                && call_has_disposition(&receipt.map_set_operation, true)
                && call_has_disposition(&receipt.values_operation, true)
                && direct_ensures.len() == 1
                && function.contracts.len() == 1
                && function.contracts[0].kind == MirContractKind::Ensures
                && function.contracts[0].id == direct_ensures[0]
                && !active_default
        });
    fn block_has_exact_ffi_loop(block: &crate::core::ir::TypedProofBlock) -> bool {
        block.operations.iter().any(|operation| match operation {
            crate::core::ir::TypedProofOperation::Loop {
                body,
                stationary,
                receipt,
                ..
            } => {
                receipt
                    .as_ref()
                    .is_some_and(|receipt| *stationary || receipt.ffi_call_path.is_some())
                    || block_has_exact_ffi_loop(body)
            }
            crate::core::ir::TypedProofOperation::Branch {
                then_block,
                else_block,
                ..
            } => block_has_exact_ffi_loop(then_block) || block_has_exact_ffi_loop(else_block),
            crate::core::ir::TypedProofOperation::Scope(inner) => block_has_exact_ffi_loop(inner),
            crate::core::ir::TypedProofOperation::Bind { .. }
            | crate::core::ir::TypedProofOperation::Call(_)
            | crate::core::ir::TypedProofOperation::Eval { .. }
            | crate::core::ir::TypedProofOperation::Assign { .. }
            | crate::core::ir::TypedProofOperation::Math(_)
            | crate::core::ir::TypedProofOperation::Break { .. }
            | crate::core::ir::TypedProofOperation::Continue { .. }
            | crate::core::ir::TypedProofOperation::Return { .. }
            | crate::core::ir::TypedProofOperation::Exit => false,
        })
    }
    let body_proof_requested = !typed.points.is_empty()
        || !typed.loop_invariant_nodes().is_empty()
        || !typed.boundaries.is_empty()
        // An exact immutable FFI loop profile carries the only source/MIR
        // receipt that can authorize its otherwise rejected CFG backedge.
        // Keep that receipt even when the caller has no separate Math or
        // Requires root; the foreign Requires is itself the proof objective.
        || block_has_exact_ffi_loop(&typed.root);
    if !body_proof_requested && !active_default && !force_owner_worklist {
        // Call-site Requires are checked by the MIR call consumer directly.
        // An unrelated runtime call by itself is not a reason to materialize
        // an otherwise empty proof worklist.
        return Ok(None);
    }

    let deferred_body_calls = typed
        .calls
        .iter()
        .zip(&bridged_body_calls)
        .any(|(point, bridge)| {
            bridge.is_none()
                && matches!(
                    &point.disposition,
                    crate::core::ir::TypedCallExecutionDisposition::Deferred(_)
                )
        })
        && retained_values_local_index_i32_result_branch_calls.is_none();
    let unsupported_default = active_defaults
        .iter()
        .any(|root| !root.calls.is_empty() || !root.boundaries.is_empty());
    if (!typed.boundaries.is_empty()
        && retained_new_result_match_boundary.is_none()
        && retained_set_result_match_boundaries.is_none()
        && retained_string_match_boundaries.is_none()
        && retained_set_get_any_i32_match_boundaries.is_none()
        && retained_set_get_any_result_i32_match_boundaries.is_none()
        && retained_values_local_index_i32_result_branch_calls.is_none())
        || (!typed.deferred_calls.is_empty()
            && retained_values_local_index_i32_result_branch_calls.is_none())
        || deferred_body_calls
        || unsupported_default
    {
        let reasons = typed
            .boundaries
            .iter()
            .map(|boundary| format!("{}: {}", boundary.point.0, boundary.reason))
            .chain(typed.deferred_calls.iter().map(|call| {
                format!(
                    "deferred call '{}': place-input execution order is not sealed",
                    call.call_node.0
                )
            }))
            .chain(
                typed
                    .calls
                    .iter()
                    .filter(|call| {
                        matches!(
                            &call.disposition,
                            crate::core::ir::TypedCallExecutionDisposition::Deferred(_)
                        )
                    })
                    .map(|call| {
                        format!(
                            "deferred call '{}': checker did not seal an executable body point",
                            call.call_node.0
                        )
                    }),
            )
            .chain(active_defaults.iter().flat_map(|root| {
                let identity = format!(
                    "default root owner={} parameter={} expression={}",
                    root.owner.0, root.parameter.0.0, root.expression.0
                );
                let mut root_reasons = root
                    .boundaries
                    .iter()
                    .map(|boundary| {
                        format!(
                            "{identity} boundary point={}: {}",
                            boundary.point.0, boundary.reason
                        )
                    })
                    .collect::<Vec<_>>();
                if !root.points.is_empty() {
                    root_reasons.push(format!(
                        "{identity} retains {} isolated Math event(s); they cannot become body facts",
                        root.points.len()
                    ));
                }
                if !root.calls.is_empty() {
                    root_reasons.push(format!(
                        "{identity} retains {} call event(s) with no default-expression effect proof",
                        root.calls.len()
                    ));
                }
                if root_reasons.is_empty() {
                    root_reasons.push(format!(
                        "{identity} is retained because a checked default-argument use is in the callable census"
                    ));
                }
                root_reasons
            }))
            .chain(unsupported_default.then_some(
                "unsupported declaration-default proof roots stay isolated from caller Requires/Ensures facts".into(),
            ))
            .collect::<Vec<_>>();
        return Ok(Some(boundary_worklist(
            &typed,
            callable,
            reasons.join("; "),
        )));
    }

    fn lower_expression(
        expression: &crate::core::ir::ResolvedExpr,
        callable: &crate::core::ir::ResolvedCallable,
        function: &MirFunction,
    ) -> Result<MirProofExpression, String> {
        let canonical = lower_contract_expr(expression, function, &callable.body)?;
        let mut expression_types = BTreeMap::new();
        collect_expression_types(
            expression,
            &callable.body,
            &mut Vec::new(),
            &mut expression_types,
        )?;
        let type_path_fingerprint = crate::core::ir::type_path_fingerprint(&expression_types);
        Ok(MirProofExpression {
            origin: expression.node_id.clone(),
            expression: canonical,
            expression_types,
            ty: expression.ty.clone(),
            effects: expression.effects.clone(),
            source_fingerprint: crate::core::ir::typed_loop_expression_fingerprint(expression),
            type_path_fingerprint,
        })
    }

    /// Aggregate return values are executed and ownership-checked by the
    /// canonical body MIR, but they are not scalar proof expressions.  Keep
    /// the Return proof-point event in the worklist while omitting only this
    /// non-scalar payload; dropping the whole worklist would hide preceding
    /// Math/contract obligations behind a lowering boundary.
    fn return_value_is_body_aggregate(expression: &crate::core::ir::ResolvedExpr) -> bool {
        matches!(
            expression.kind,
            crate::core::ir::ResolvedExprKind::Record { .. }
                | crate::core::ir::ResolvedExprKind::Tuple(_)
                | crate::core::ir::ResolvedExprKind::List(_)
                | crate::core::ir::ResolvedExprKind::Set(_)
        )
    }

    fn lower_assertion(
        point: &crate::core::ir::TypedProofPointId,
        order: u64,
        scope: &[TypedProofScopeFrame],
        condition: &crate::core::ir::ResolvedExpr,
        callable: &crate::core::ir::ResolvedCallable,
        function: &MirFunction,
    ) -> Result<MirProofAssertion, String> {
        Ok(MirProofAssertion {
            id: MirProofPointId {
                owner: point.owner.clone(),
                statement: point.statement.clone(),
                ordinal: point.ordinal,
            },
            order,
            scope: scope_frames(scope)?,
            expression: lower_expression(condition, callable, function)?,
        })
    }

    fn scope_frames(scope: &[TypedProofScopeFrame]) -> Result<Vec<MirProofScopeFrame>, String> {
        scope
            .iter()
            .map(|frame| match frame {
                TypedProofScopeFrame::Block(node) => Ok(MirProofScopeFrame::Block(node.clone())),
                TypedProofScopeFrame::Branch {
                    statement,
                    expression,
                    then_arm,
                } => Ok(MirProofScopeFrame::Branch {
                    statement: statement.clone(),
                    expression: expression.clone(),
                    then_arm: *then_arm,
                }),
                TypedProofScopeFrame::Loop(node) => Ok(MirProofScopeFrame::Loop(node.clone())),
                TypedProofScopeFrame::Default { .. } => {
                    Err("default-expression proof has no body execution receipt".into())
                }
                TypedProofScopeFrame::PlaceInput(_) => {
                    Err("place-input proof has no ordered body execution receipt".into())
                }
            })
            .collect()
    }

    fn lower_call_point(
        point: &crate::core::ir::TypedCallProofPoint,
        callable: &crate::core::ir::ResolvedCallable,
        function: &MirFunction,
        new_result_receipt: Option<&crate::core::ownership::OrdinaryMapOwnedAnyV2NewResultReceipt>,
        set_get_any_result_i32_receipt: Option<
            &crate::core::ownership::OrdinaryMapOwnedAnyV2SetGetAnyResultI32LineageReceipt,
        >,
    ) -> Result<MirProofCallPoint, String> {
        use crate::core::ir::{
            CheckedConversionKind, ResolvedExprKind, TypedCallEffectAuthority,
            TypedCallExecutionDisposition,
        };

        if !point.validate_against(callable)
            || point.body_owner != callable.owner
            || point.owner != callable.owner
            || point.call_node != point.expression.node_id
        {
            return Err(format!(
                "call proof point '{}' is not sealed by this callable body",
                point.call_node.0
            ));
        }
        let ResolvedExprKind::Call(source_call) = &point.expression.kind else {
            return Err(format!(
                "call proof point '{}' no longer names a resolved Call",
                point.call_node.0
            ));
        };
        if point.effect.call_node != point.call_node
            || point.effect.callee != source_call.callee
            || point.effect.declared_effects != source_call.effects
            || point.effect.authority != TypedCallEffectAuthority::UnknownMayReadWrite
        {
            return Err(format!(
                "call proof point '{}' has a mismatched or nonconservative effect receipt",
                point.call_node.0
            ));
        }

        let new_result_bridge = new_result_receipt
            .map(|receipt| {
                ordinary_map_new_result_call_bridge_for_source(point, callable, function, receipt)
            })
            .transpose()?
            .flatten();
        let values_local_index_i32_result_branch_bridge = function
            .ownership
            .ordinary_map_owned_any_v2_cfg_receipt()
            .and_then(|cfg| cfg.values_local_index_i32_result_branch_receipt.as_ref())
            .map(|receipt| {
                ordinary_map_values_local_index_i32_result_branch_call_bridge_for_source(
                    point, callable, function, receipt,
                )
            })
            .transpose()?
            .flatten();
        let set_get_i32_bridge = function
            .ownership
            .ordinary_map_set_get_i32_lineage_receipt()
            .map(|receipt| {
                ordinary_map_set_get_i32_call_bridge_for_source(point, callable, function, receipt)
            })
            .transpose()?
            .flatten();
        let set_get_any_i32_bridge = function
            .ownership
            .ordinary_map_set_get_any_i32_lineage_receipt()
            .map(|receipt| {
                ordinary_map_set_get_any_i32_call_bridge_for_source(
                    point, callable, function, receipt,
                )
            })
            .transpose()?
            .flatten();
        let set_get_any_result_i32_bridge = set_get_any_result_i32_receipt
            .map(|receipt| {
                if function
                    .ownership
                    .ordinary_map_owned_any_v2_set_get_any_result_i32_lineage_receipt()
                    != Some(receipt)
                    || callable
                        .resources
                        .ordinary_map_owned_any_v2_set_get_any_result_i32_lineage_receipt
                        .as_ref()
                        != Some(receipt)
                {
                    return Err(
                        "Set/GetAny-Result-i32 proof-call bridge is detached from CheckedProgram/MIR ownership receipt"
                            .to_owned(),
                    );
                }
                ordinary_map_set_get_any_result_i32_call_bridge_for_source(
                    point, callable, function, receipt,
                )
            })
            .transpose()?
            .flatten();
        let set_result_bridge = function
            .ownership
            .ordinary_map_set_lineage_receipt()
            .map(|receipt| {
                ordinary_map_set_result_call_bridge_for_source(point, callable, function, receipt)
            })
            .transpose()?
            .flatten();
        let set_get_string_bridge = function
            .ownership
            .ordinary_map_set_get_string_lineage_receipt()
            .map(|receipt| {
                ordinary_map_set_get_string_call_bridge_for_source(
                    point, callable, function, receipt,
                )
            })
            .transpose()?
            .flatten();
        let call_bridge = values_local_index_i32_result_branch_bridge
            .or(new_result_bridge)
            .or(set_result_bridge)
            .or(set_get_any_result_i32_bridge)
            .or(set_get_any_i32_bridge)
            .or(set_get_i32_bridge)
            .or(set_get_string_bridge);
        if matches!(
            &point.disposition,
            TypedCallExecutionDisposition::Deferred(_)
        ) && call_bridge.is_none()
        {
            return Err(format!(
                "call proof point '{}' is deferred and cannot contribute facts",
                point.call_node.0
            ));
        }

        let builtin_variant = match &source_call.callee {
            crate::core::ir::ResolvedCallee::Builtin(builtin) => match builtin.as_str() {
                "Some" => Some(NodeId("builtin:variant:Option::Some".into())),
                "None" => Some(NodeId("builtin:variant:Option::None".into())),
                "Ok" => Some(NodeId("builtin:variant:Result::Ok".into())),
                "Err" => Some(NodeId("builtin:variant:Result::Err".into())),
                _ => None,
            },
            _ => None,
        };
        let instruction_roles: &[&str] = match &source_call.callee {
            crate::core::ir::ResolvedCallee::Builtin(_) if builtin_variant.is_some() => {
                &["construct_variant", "construct_variant_move"]
            }
            crate::core::ir::ResolvedCallee::Builtin(_) => &["builtin_call"],
            crate::core::ir::ResolvedCallee::Transition(_) => &["flow_transition"],
            crate::core::ir::ResolvedCallee::Constructor(_) => {
                &["construct_variant", "construct_variant_move"]
            }
            _ => &["call"],
        };
        let instruction_ids = if let Some(bridge) = &call_bridge {
            BTreeSet::from([bridge.instruction.clone()])
        } else {
            instruction_roles
                .iter()
                .map(|role| {
                    super::MirInstructionId::new(format!("inst:{role}:{}", point.call_node.0))
                        .map_err(|error| error.to_string())
                })
                .collect::<Result<BTreeSet<_>, _>>()?
        };
        let matching = function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| instruction_ids.contains(&instruction.id))
            .collect::<Vec<_>>();
        let [instruction] = matching.as_slice() else {
            return Err(format!(
                "call proof point '{}' has {} matching MIR call/construction instructions",
                point.call_node.0,
                matching.len()
            ));
        };
        let instruction_id = instruction.id.clone();
        let (result, mir_arguments) = if let Some(bridge) = &call_bridge {
            (Some(bridge.result.clone()), bridge.arguments.clone())
        } else {
            match (&source_call.callee, &instruction.kind) {
                (
                    _,
                    super::MirInstructionKind::Call {
                        result,
                        callee: mir_callee,
                        arguments,
                        ..
                    },
                ) if mir_callee == &source_call.callee => (result.clone(), arguments.clone()),
                (
                    crate::core::ir::ResolvedCallee::Builtin(_),
                    super::MirInstructionKind::BuiltinCall {
                        result, arguments, ..
                    },
                ) => (Some(result.clone()), arguments.clone()),
                (
                    crate::core::ir::ResolvedCallee::Transition(transition),
                    super::MirInstructionKind::FlowTransition {
                        result,
                        transition: mir_transition,
                        arguments,
                        ..
                    },
                ) if mir_transition == &super::transition_owner_from_id(transition) => {
                    (Some(result.clone()), arguments.clone())
                }
                (
                    crate::core::ir::ResolvedCallee::Constructor(variant),
                    super::MirInstructionKind::ConstructVariant {
                        result,
                        variant: mir_variant,
                        fields,
                        ..
                    },
                )
                | (
                    crate::core::ir::ResolvedCallee::Constructor(variant),
                    super::MirInstructionKind::ConstructVariantMove {
                        result,
                        variant: mir_variant,
                        fields,
                        ..
                    },
                ) if mir_variant == variant => (
                    Some(result.clone()),
                    fields.iter().map(|(_, value)| value.clone()).collect(),
                ),
                (
                    crate::core::ir::ResolvedCallee::Builtin(_),
                    super::MirInstructionKind::ConstructVariant {
                        result,
                        variant: mir_variant,
                        fields,
                        ..
                    },
                )
                | (
                    crate::core::ir::ResolvedCallee::Builtin(_),
                    super::MirInstructionKind::ConstructVariantMove {
                        result,
                        variant: mir_variant,
                        fields,
                        ..
                    },
                ) if builtin_variant.as_ref() == Some(mir_variant) => (
                    Some(result.clone()),
                    fields.iter().map(|(_, value)| value.clone()).collect(),
                ),
                _ => {
                    return Err(format!(
                        "call proof point '{}' does not match its exact direct-call, builtin, transition, or constructor MIR operation",
                        point.call_node.0
                    ));
                }
            }
        };
        let expected_result = if let Some(bridge) = &call_bridge {
            bridge.result.clone()
        } else {
            MirValueId::new(format!("expr:{}", point.call_node.0))
                .map_err(|error| error.to_string())?
        };
        if source_call.arguments.len() != mir_arguments.len()
            || result
                .as_ref()
                .is_some_and(|value| value != &expected_result)
        {
            return Err(format!(
                "call proof point '{}' disagrees with its canonical MIR result or arity",
                point.call_node.0
            ));
        }

        let mut arguments = Vec::with_capacity(source_call.arguments.len());
        for (index, (argument, actual)) in source_call
            .arguments
            .iter()
            .zip(mir_arguments.iter())
            .enumerate()
        {
            let expression = lower_expression(&argument.value, callable, function)?;
            let source_value = MirValueId::new(format!("expr:{}", argument.value.node_id.0))
                .map_err(|error| error.to_string())?;
            let bridge_argument = call_bridge
                .as_ref()
                .is_some_and(|bridge| bridge.arguments.get(index) == Some(actual));
            match argument.conversion.kind {
                CheckedConversionKind::Identity => {
                    if argument.conversion.from != argument.conversion.to
                        || (!bridge_argument && actual != &source_value)
                    {
                        return Err(format!(
                            "call argument {} at '{}' is not an exact identity-converted MIR value",
                            index, point.call_node.0
                        ));
                    }
                }
                CheckedConversionKind::NumericWiden => {
                    let conversion_node = NodeId(format!(
                        "{}/numeric-argument-conversion:{index}",
                        argument.value.node_id.0
                    ));
                    let converted = MirValueId::new(format!("convert:{}", conversion_node.0))
                        .map_err(|error| error.to_string())?;
                    let conversion_instruction = super::MirInstructionId::new(format!(
                        "inst:numeric_argument_convert:{}",
                        conversion_node.0
                    ))
                    .map_err(|error| error.to_string())?;
                    let conversions = function
                        .blocks
                        .values()
                        .flat_map(|block| block.instructions.iter())
                        .filter(|candidate| candidate.id == conversion_instruction)
                        .filter(|candidate| {
                            matches!(
                                &candidate.kind,
                                super::MirInstructionKind::Convert { result, source }
                                    if result == &converted && source == &source_value
                            )
                        })
                        .count();
                    if actual != &converted || conversions != 1 {
                        return Err(format!(
                            "call argument {} at '{}' does not bind its exact NumericWiden instruction",
                            index, point.call_node.0
                        ));
                    }
                }
                _ => {
                    return Err(format!(
                        "call argument {} at '{}' uses an unmodeled conversion",
                        index, point.call_node.0
                    ));
                }
            }
            if !bridge_argument
                && function
                    .values
                    .get(actual)
                    .is_none_or(|value| value.ty != argument.conversion.to)
            {
                return Err(format!(
                    "call argument {} at '{}' has no matching post-conversion MIR TypeDesc",
                    index, point.call_node.0
                ));
            }
            arguments.push(MirProofCallArgument {
                parameter: argument.parameter.clone(),
                expression,
                conversion: argument.conversion.clone(),
                value: actual.clone(),
            });
        }

        let guards = point
            .guards
            .iter()
            .map(|guard| {
                Ok(MirProofCallGuard {
                    condition: lower_expression(&guard.condition, callable, function)?,
                    when: guard.when,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let mut havoc_locals = callable.body.locals.keys().cloned().collect::<Vec<_>>();
        havoc_locals.sort();
        Ok(MirProofCallPoint {
            body_owner: point.body_owner.clone(),
            owner: point.owner.clone(),
            statement: point.statement.clone(),
            call_node: point.call_node.clone(),
            order: point.order,
            scope: scope_frames(&point.scope)?,
            guards,
            match_path: point.match_path.clone(),
            disposition: point.disposition.clone(),
            callee: source_call.callee.clone(),
            type_arguments: source_call.type_arguments.clone(),
            result_type: source_call.result.clone(),
            declared_effects: source_call.effects.clone(),
            effect_authority: point.effect.authority,
            instruction: instruction_id,
            arguments,
            result: result.clone(),
            havoc_locals,
        })
    }

    fn mir_reachable(
        function: &MirFunction,
        start: &super::MirBlockId,
        target: &super::MirBlockId,
    ) -> bool {
        let mut seen = BTreeSet::new();
        let mut pending = VecDeque::from([start.clone()]);
        while let Some(block_id) = pending.pop_front() {
            if &block_id == target {
                return true;
            }
            if !seen.insert(block_id.clone()) {
                continue;
            }
            if let Some(block) = function.blocks.get(&block_id) {
                pending.extend(mir_successors(block).into_iter().map(|(_, to, _)| to));
            }
        }
        false
    }

    fn guarded_mir_path_start(
        function: &MirFunction,
        body_entry: &super::MirBlockId,
        guards: &[crate::core::ir::TypedLoopBranchGuard],
    ) -> Result<super::MirBlockId, String> {
        let mut start = body_entry.clone();
        for guard in guards {
            let condition = MirValueId::new(format!("expr:{}", guard.condition.0))
                .map_err(|error| error.to_string())?;
            let branches = function
                .blocks
                .values()
                .filter_map(|block| match &block.terminator {
                    super::MirTerminator::Branch {
                        condition: actual,
                        then_target,
                        else_target,
                        ..
                    } if actual == &condition => Some((
                        block.id.clone(),
                        if guard.then_arm {
                            then_target.clone()
                        } else {
                            else_target.clone()
                        },
                    )),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let [(branch_block, next)] = branches.as_slice() else {
                return Err(format!(
                    "loop guard expression '{}' has {} matching MIR branch terminators",
                    guard.expression.0,
                    branches.len()
                ));
            };
            if !mir_reachable(function, &start, branch_block) {
                return Err(format!(
                    "loop guard '{}' is not reachable from its preceding guarded body path",
                    guard.expression.0
                ));
            }
            start = next.clone();
        }
        Ok(start)
    }

    fn lower_mir_loop_receipt(
        typed: &TypedLoopProofReceipt,
        callable: &crate::core::ir::ResolvedCallable,
        function: &MirFunction,
    ) -> Result<MirProofLoopReceipt, String> {
        use crate::core::cfg::{EdgeId, EdgeKind};
        use crate::core::ir::TypedLoopEdgeKind as TKind;

        let expected_cfg_id =
            |role: &str| EdgeId(NodeId(format!("{}/cfg:edge:{role}", typed.loop_node.0)));
        let expected_mir_id = |role: &str| {
            super::MirEdgeId::new(format!("edge:{role}:{}", typed.loop_node.0))
                .map_err(|error| error.to_string())
        };
        let header = super::MirBlockId::new(format!("bb:while.header:{}", typed.loop_node.0))
            .map_err(|error| error.to_string())?;
        let body_entry = super::MirBlockId::new(format!("bb:while.body:{}", typed.loop_node.0))
            .map_err(|error| error.to_string())?;
        let exit = super::MirBlockId::new(format!("bb:while.exit:{}", typed.loop_node.0))
            .map_err(|error| error.to_string())?;
        let records = mir_edge_records(function);

        let ffi_call_block = if let Some(profile) = typed.ffi_call_path.as_ref() {
            let call_id =
                super::MirInstructionId::new(format!("inst:call:{}", profile.call_node.0))
                    .map_err(|error| error.to_string())?;
            let call_locations = function
                .blocks
                .values()
                .flat_map(|block| {
                    block
                        .instructions
                        .iter()
                        .filter(|instruction| instruction.id == call_id)
                        .map(move |instruction| (block.id.clone(), instruction))
                })
                .collect::<Vec<_>>();
            let call_break_id =
                super::MirEdgeId::new(format!("edge:while.break:{}", profile.terminal_break.0))
                    .map_err(|error| error.to_string())?;
            let break_locations = records
                .iter()
                .filter(|record| record.0 == call_break_id)
                .collect::<Vec<_>>();
            let [call_location] = call_locations.as_slice() else {
                return Err(format!(
                    "loop '{}' exact FFI call has no unique MIR instruction location",
                    typed.loop_node.0
                ));
            };
            let [break_record] = break_locations.as_slice() else {
                return Err(format!(
                    "loop '{}' exact call-path break has no unique MIR CFG edge",
                    typed.loop_node.0
                ));
            };
            if !matches!(
                &call_location.1.kind,
                super::MirInstructionKind::Call {
                    callee: ResolvedCallee::Extern(_),
                    ..
                }
            ) || call_location.0 != break_record.1
            {
                return Err(format!(
                    "loop '{}' FFI call instruction and terminal break do not share the exact source CFG block",
                    typed.loop_node.0
                ));
            }
            Some(call_location.0.clone())
        } else {
            None
        };

        let cfg_edge = |role: &str, kind: EdgeKind| -> Result<EdgeId, String> {
            let id = expected_cfg_id(role);
            let matches = typed
                .cfg_edges
                .iter()
                .filter(|edge| edge.id == id && edge.kind == kind)
                .count();
            if matches != 1 {
                return Err(format!(
                    "loop '{}' source receipt has {} exact '{}' CFG edges",
                    typed.loop_node.0, matches, role
                ));
            }
            Ok(id)
        };
        let edge_record = |edge: &super::MirEdgeId| {
            let matches = records
                .iter()
                .filter(|record| &record.0 == edge)
                .collect::<Vec<_>>();
            if matches.len() != 1 {
                return Err(format!(
                    "loop '{}' MIR edge '{}' has {} source/target records",
                    typed.loop_node.0,
                    edge,
                    matches.len()
                ));
            }
            Ok(matches[0])
        };
        let mut edges = Vec::new();
        let mut add_edge = |source_site: NodeId,
                            kind: MirProofLoopEdgeKind,
                            source_cfg_edge: Option<EdgeId>,
                            source_reachable: bool,
                            mir_edge: Option<super::MirEdgeId>,
                            from_block: super::MirBlockId,
                            to_block: Option<super::MirBlockId>,
                            arguments: Vec<MirValueId>,
                            terminator_value: Option<MirValueId>| {
            edges.push(MirProofLoopEdge {
                source_site,
                kind,
                source_cfg_edge,
                source_reachable,
                mir_edge,
                from_block,
                to_block,
                arguments,
                terminator_value,
            });
        };

        let entry_cfg = cfg_edge("while-enter", EdgeKind::Fallthrough)?;
        let entry_id = expected_mir_id("while.entry")?;
        let entry = edge_record(&entry_id)?;
        let cfg_entry = typed
            .cfg_edges
            .iter()
            .find(|edge| edge.id == entry_cfg)
            .ok_or_else(|| format!("loop '{}' source entry edge is absent", typed.loop_node.0))?;
        if entry.2 != header {
            return Err(format!(
                "loop '{}' MIR entry edge does not target its exact header",
                typed.loop_node.0
            ));
        }
        add_edge(
            typed.loop_node.clone(),
            MirProofLoopEdgeKind::Entry,
            Some(entry_cfg),
            typed.cfg_reachable_blocks.contains(&cfg_entry.to),
            Some(entry_id.clone()),
            entry.1.clone(),
            Some(entry.2.clone()),
            entry.3.clone(),
            None,
        );

        let body_cfg = cfg_edge("while-body", EdgeKind::LoopBody)?;
        let body_id = expected_mir_id("while.body")?;
        let body = edge_record(&body_id)?;
        let cfg_body = typed
            .cfg_edges
            .iter()
            .find(|edge| edge.id == body_cfg)
            .ok_or_else(|| format!("loop '{}' source body edge is absent", typed.loop_node.0))?;
        if body.1 != header || body.2 != body_entry {
            return Err(format!(
                "loop '{}' MIR condition-true edge does not bind header to body",
                typed.loop_node.0
            ));
        }
        add_edge(
            typed.loop_node.clone(),
            MirProofLoopEdgeKind::ConditionTrue,
            Some(body_cfg),
            typed.cfg_reachable_blocks.contains(&cfg_body.from),
            Some(body_id.clone()),
            body.1.clone(),
            Some(body.2.clone()),
            body.3.clone(),
            None,
        );

        let exit_cfg = cfg_edge("while-exit", EdgeKind::LoopExit)?;
        let exit_id = expected_mir_id("while.exit")?;
        let exit_record = edge_record(&exit_id)?;
        let cfg_exit = typed
            .cfg_edges
            .iter()
            .find(|edge| edge.id == exit_cfg)
            .ok_or_else(|| format!("loop '{}' source exit edge is absent", typed.loop_node.0))?;
        if exit_record.1 != header || exit_record.2 != exit {
            return Err(format!(
                "loop '{}' MIR condition-false edge does not bind header to exit",
                typed.loop_node.0
            ));
        }
        add_edge(
            typed.loop_node.clone(),
            MirProofLoopEdgeKind::ConditionFalse,
            Some(exit_cfg),
            typed.cfg_reachable_blocks.contains(&cfg_exit.from),
            Some(exit_id.clone()),
            exit_record.1.clone(),
            Some(exit_record.2.clone()),
            exit_record.3.clone(),
            None,
        );

        let entry_arguments = &entry.3;
        let header_block = function
            .blocks
            .get(&header)
            .ok_or_else(|| format!("loop '{}' MIR header block is absent", typed.loop_node.0))?;
        let exit_block = function
            .blocks
            .get(&exit)
            .ok_or_else(|| format!("loop '{}' MIR exit block is absent", typed.loop_node.0))?;
        if entry_arguments.len() != typed.state_locals.len()
            || header_block.parameters.len() != typed.state_locals.len()
            || exit_block.parameters.len() != typed.state_locals.len()
        {
            return Err(format!(
                "loop '{}' MIR entry/header/exit carry arities disagree with the typed state-local census",
                typed.loop_node.0
            ));
        }
        let mut bindings = Vec::with_capacity(typed.state_locals.len());
        for (index, local) in typed.state_locals.iter().enumerate() {
            if index > 0 && typed.state_locals[index - 1].local >= local.local {
                return Err(format!(
                    "loop '{}' typed state locals are not unique and sorted",
                    typed.loop_node.0
                ));
            }
            let expected_header = MirValueId::new(format!(
                "loop.header.local:{}:{}",
                typed.loop_node.0, local.local.0 .0
            ))
            .map_err(|error| error.to_string())?;
            let expected_exit = MirValueId::new(format!(
                "loop.exit.local:{}:{}",
                typed.loop_node.0, local.local.0 .0
            ))
            .map_err(|error| error.to_string())?;
            let preheader_value = entry_arguments[index].clone();
            let header_parameter = header_block.parameters[index].value.clone();
            let exit_parameter = exit_block.parameters[index].value.clone();
            if header_parameter != expected_header || exit_parameter != expected_exit {
                return Err(format!(
                    "loop '{}' carried local '{}' has noncanonical header/exit parameter identities",
                    typed.loop_node.0, local.local.0.0
                ));
            }
            for value in [&preheader_value, &header_parameter, &exit_parameter] {
                if function
                    .values
                    .get(value)
                    .is_none_or(|actual| actual.ty != local.ty)
                {
                    return Err(format!(
                        "loop '{}' carried local '{}' value '{}' has a mismatched TypeDesc",
                        typed.loop_node.0, local.local.0 .0, value
                    ));
                }
            }
            bindings.push(MirProofLoopBinding {
                local: local.local.clone(),
                ty: local.ty.clone(),
                preheader_value,
                header_parameter,
                exit_parameter,
            });
        }
        let header_values = bindings
            .iter()
            .map(|binding| binding.header_parameter.clone())
            .collect::<Vec<_>>();
        // The body block has no block parameters: the exact header values
        // dominate it directly. The condition-false edge does enter the
        // parameterized exit block and must carry the complete state.
        if !body.3.is_empty() || exit_record.3 != header_values {
            return Err(format!(
                "loop '{}' condition edges do not preserve the exact header state locals",
                typed.loop_node.0
            ));
        }

        let mut seen_source_edges = BTreeSet::new();
        for source in &typed.body_edges {
            let source_cfg_edge = source.cfg_terminal_edge.clone();
            let kind = MirProofLoopEdgeKind::from_typed(source.kind);
            let path_start = guarded_mir_path_start(function, &body_entry, &source.guards)?;
            if !typed
                .cfg_blocks
                .iter()
                .any(|block| block.id == source.cfg_terminal_block)
                || source.reachable != callable.cfg.reachable.contains(&source.cfg_terminal_block)
            {
                return Err(format!(
                    "loop edge site '{}' has an absent CFG terminal or a stale reachability seal",
                    source.site.0
                ));
            }
            if !seen_source_edges.insert((
                source.site.clone(),
                source.kind,
                source_cfg_edge.clone(),
                source.cfg_terminal_block.clone(),
                source.guards.clone(),
            )) {
                return Err(format!(
                    "loop '{}' repeats source edge site '{}'",
                    typed.loop_node.0, source.site.0
                ));
            }
            match source.kind {
                TKind::BodyFallthrough | TKind::Continue | TKind::Break => {
                    let (cfg_id, expected_kind, role, target) = match source.kind {
                        TKind::BodyFallthrough => (
                            cfg_edge("while-backedge", EdgeKind::Backedge)?,
                            EdgeKind::Backedge,
                            "while.back",
                            &header,
                        ),
                        TKind::Continue => (
                            EdgeId(NodeId(format!("{}/cfg:edge:continue", source.site.0))),
                            EdgeKind::Continue,
                            "while.continue",
                            &header,
                        ),
                        TKind::Break => (
                            EdgeId(NodeId(format!("{}/cfg:edge:break", source.site.0))),
                            EdgeKind::Break,
                            "while.break",
                            &exit,
                        ),
                        TKind::Return => {
                            return Err(format!(
                                "loop return site '{}' was routed through a successor-edge arm",
                                source.site.0
                            ));
                        }
                    };
                    let recorded_cfg_edge = source_cfg_edge.ok_or_else(|| {
                        format!(
                            "loop edge site '{}' has no terminal CFG edge",
                            source.site.0
                        )
                    })?;
                    if recorded_cfg_edge != cfg_id
                        || !typed.cfg_edges.iter().any(|edge| {
                            edge.id == recorded_cfg_edge
                                && edge.kind == expected_kind
                                && edge.from == source.cfg_terminal_block
                                && &edge.to
                                    == if source.kind == TKind::Break {
                                        &cfg_exit.to
                                    } else {
                                        &cfg_entry.to
                                    }
                        })
                    {
                        return Err(format!(
                            "loop edge site '{}' does not bind its exact checker CFG edge",
                            source.site.0
                        ));
                    }
                    let mir_id = if source.kind == TKind::BodyFallthrough {
                        expected_mir_id(role)?
                    } else {
                        super::MirEdgeId::new(format!("edge:{role}:{}", source.site.0))
                            .map_err(|error| error.to_string())?
                    };
                    let record = edge_record(&mir_id)?;
                    if &record.2 != target || !mir_reachable(function, &path_start, &record.1) {
                        return Err(format!(
                            "loop edge site '{}' does not reach its exact MIR terminal on the guarded path",
                            source.site.0
                        ));
                    }
                    if record.3.len() != bindings.len() {
                        return Err(format!(
                            "loop source edge '{}' carries the wrong number of state locals",
                            source.site.0
                        ));
                    }
                    add_edge(
                        source.site.clone(),
                        kind,
                        Some(recorded_cfg_edge),
                        source.reachable,
                        Some(mir_id),
                        record.1.clone(),
                        Some(record.2.clone()),
                        record.3.clone(),
                        None,
                    );
                }
                TKind::Return => {
                    if source_cfg_edge.is_some() {
                        return Err(format!(
                            "loop return site '{}' unexpectedly carries a successor edge",
                            source.site.0
                        ));
                    }
                    let terminal_value = source
                        .terminal_value
                        .as_ref()
                        .map(|node| MirValueId::new(format!("expr:{}", node.0)))
                        .transpose()
                        .map_err(|error| error.to_string())?;
                    let cfg_terminal = typed
                        .cfg_blocks
                        .iter()
                        .find(|block| block.id == source.cfg_terminal_block)
                        .ok_or_else(|| {
                            format!(
                                "loop return site '{}' CFG terminal block is absent",
                                source.site.0
                            )
                        })?;
                    if !cfg_terminal
                        .points
                        .iter()
                        .any(|point| point.source.node == source.site)
                        || !matches!(
                            &cfg_terminal.terminator,
                            crate::core::cfg::Terminator::Return { value, implicit: false }
                                if value == &source.terminal_value
                        )
                    {
                        return Err(format!(
                            "loop return site '{}' does not bind its exact source CFG terminal",
                            source.site.0
                        ));
                    }
                    let candidates = function
                        .blocks
                        .values()
                        .filter_map(|block| match &block.terminator {
                            super::MirTerminator::Return { value }
                                if value == &terminal_value
                                    && mir_reachable(function, &path_start, &block.id) =>
                            {
                                Some((block.id.clone(), value.clone()))
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    let [(from_block, terminator_value)] = candidates.as_slice() else {
                        return Err(format!(
                            "loop return site '{}' has {} matching guarded MIR Return terminators",
                            source.site.0,
                            candidates.len()
                        ));
                    };
                    add_edge(
                        source.site.clone(),
                        kind,
                        None,
                        source.reachable,
                        None,
                        from_block.clone(),
                        None,
                        Vec::new(),
                        terminator_value.clone(),
                    );
                }
            }
        }

        let actual_incoming_header = records
            .iter()
            .filter(|record| record.2 == header)
            .map(|record| record.0.clone())
            .collect::<BTreeSet<_>>();
        let receipted_incoming_header = edges
            .iter()
            .filter(|edge| edge.to_block.as_ref() == Some(&header))
            .filter_map(|edge| edge.mir_edge.clone())
            .collect::<BTreeSet<_>>();
        let actual_incoming_exit = records
            .iter()
            .filter(|record| record.2 == exit)
            .map(|record| record.0.clone())
            .collect::<BTreeSet<_>>();
        let actual_incoming_body = records
            .iter()
            .filter(|record| record.2 == body_entry)
            .map(|record| record.0.clone())
            .collect::<BTreeSet<_>>();
        let receipted_incoming_exit = edges
            .iter()
            .filter(|edge| edge.to_block.as_ref() == Some(&exit))
            .filter_map(|edge| edge.mir_edge.clone())
            .collect::<BTreeSet<_>>();
        let receipted_incoming_body = edges
            .iter()
            .filter(|edge| edge.to_block.as_ref() == Some(&body_entry))
            .filter_map(|edge| edge.mir_edge.clone())
            .collect::<BTreeSet<_>>();
        if actual_incoming_header != receipted_incoming_header
            || actual_incoming_exit != receipted_incoming_exit
            || actual_incoming_body != receipted_incoming_body
        {
            return Err(format!(
                "loop '{}' MIR header/exit has unreceipted or missing incoming edges",
                typed.loop_node.0
            ));
        }

        if typed
            .resource_actions
            .iter()
            .any(|action| action.kind != crate::core::ownership::CanonicalActionKind::Read)
        {
            return Err(format!(
                "loop '{}' contains a non-read ownership action outside the scalar induction profile",
                typed.loop_node.0
            ));
        }
        let expected_resource_edges = typed
            .cfg_edges
            .iter()
            .map(|edge| edge.id.clone())
            .collect::<BTreeSet<_>>();
        let actual_resource_edges = typed
            .resource_edges
            .iter()
            .map(|edge| edge.edge.clone())
            .collect::<BTreeSet<_>>();
        let expected_resource_blocks = typed
            .cfg_blocks
            .iter()
            .filter(|block| block.id != cfg_exit.to)
            .map(|block| block.id.clone())
            .collect::<BTreeSet<_>>();
        let actual_resource_blocks = typed
            .resource_blocks
            .iter()
            .map(|block| block.block.clone())
            .collect::<BTreeSet<_>>();
        let resource_edges_valid = typed.resource_edges.iter().all(|state| {
            let Some(cfg_edge) = typed.cfg_edges.iter().find(|edge| edge.id == state.edge) else {
                return false;
            };
            state.from_output == state.to_input
                && (!callable.cfg.reachable.contains(&cfg_edge.from)
                    || (state.from_input.is_some() && state.from_output.is_some()))
                && (!callable.cfg.reachable.contains(&cfg_edge.to) || state.to_input.is_some())
        });
        if expected_resource_edges != actual_resource_edges
            || expected_resource_blocks != actual_resource_blocks
            || !resource_edges_valid
            || typed.resource_blocks.iter().any(|block| {
                if callable.cfg.reachable.contains(&block.block) {
                    block.input.is_none() || block.output.is_none() || block.input != block.output
                } else {
                    false
                }
            })
        {
            return Err(format!(
                "loop '{}' ownership snapshots do not form a complete unchanged scalar-state profile",
                typed.loop_node.0
            ));
        }

        Ok(MirProofLoopReceipt {
            typed: typed.clone(),
            bindings,
            ffi_call_block,
            edges,
        })
    }

    fn lower_block(
        block: &crate::core::ir::TypedProofBlock,
        callable: &crate::core::ir::ResolvedCallable,
        function: &MirFunction,
        scope: &mut Vec<TypedProofScopeFrame>,
        new_result_receipt: Option<&crate::core::ownership::OrdinaryMapOwnedAnyV2NewResultReceipt>,
        set_get_any_result_i32_receipt: Option<
            &crate::core::ownership::OrdinaryMapOwnedAnyV2SetGetAnyResultI32LineageReceipt,
        >,
    ) -> Result<MirProofBlock, String> {
        let mut operations = Vec::with_capacity(block.operations.len());
        scope.push(TypedProofScopeFrame::Block(block.id.clone()));
        for operation in &block.operations {
            operations.push(match operation {
                T::Bind {
                    point,
                    local,
                    initializer,
                } => MirProofOperation::Bind {
                    point: point.clone(),
                    local_identity: local.0.clone(),
                    local: MirValueId::new(format!("local:{}", local.0 .0))
                        .map_err(|error| error.to_string())?,
                    initializer: lower_expression(initializer, callable, function)?,
                },
                T::Call(point) => {
                    MirProofOperation::Call(lower_call_point(
                        point,
                        callable,
                        function,
                        new_result_receipt,
                        set_get_any_result_i32_receipt,
                    )?)
                }
                T::Eval { point, expression } => MirProofOperation::Eval {
                    point: point.clone(),
                    expression: lower_expression(expression, callable, function)?,
                },
                T::Assign {
                    point,
                    target,
                    value,
                    conversion,
                } => {
                    let result = MirValueId::new(format!("assign:{}", point.0))
                        .map_err(|error| error.to_string())?;
                    let conversion_result = match conversion.kind {
                        crate::core::ir::CheckedConversionKind::Identity => None,
                        crate::core::ir::CheckedConversionKind::NumericWiden => Some(
                            MirValueId::new(format!(
                                "assign.convert:{}/assign-conversion",
                                point.0
                            ))
                            .map_err(|error| error.to_string())?,
                        ),
                        _ => {
                            return Err(format!(
                                "assignment '{}' has an unmodeled checked conversion",
                                point.0
                            ));
                        }
                    };
                    if !target.projections.is_empty() {
                        return Err(format!(
                            "assignment '{}' targets a projected place outside the scalar proof profile",
                            point.0
                        ));
                    }
                    let local = callable.body.locals.get(&target.base).ok_or_else(|| {
                        format!(
                            "assignment '{}' target is absent from the ResolvedBody local catalog",
                            point.0
                        )
                    })?;
                    if !local.mutable || conversion.from != value.ty || conversion.to != local.ty {
                        return Err(format!(
                            "assignment '{}' target mutability or checked conversion types do not match",
                            point.0
                        ));
                    }
                    let move_id = super::MirInstructionId::new(format!("inst:assign:{}", point.0))
                        .map_err(|error| error.to_string())?;
                    let move_source = conversion_result.clone().unwrap_or(
                        MirValueId::new(format!("expr:{}", value.node_id.0))
                            .map_err(|error| error.to_string())?,
                    );
                    let moves = function
                        .blocks
                        .values()
                        .flat_map(|candidate| candidate.instructions.iter())
                        .filter(|candidate| candidate.id == move_id)
                        .filter(|candidate| {
                            matches!(
                                &candidate.kind,
                                super::MirInstructionKind::Move { result: actual_result, source }
                                    if actual_result == &result && source == &move_source
                            )
                        })
                        .count();
                    if moves != 1
                        || function.values.get(&result).is_none_or(|actual| {
                            actual.ty != conversion.to
                        })
                    {
                        return Err(format!(
                            "assignment '{}' does not bind its exact typed MIR Move result",
                            point.0
                        ));
                    }
                    if let Some(converted) = &conversion_result {
                        let conversion_node = NodeId(format!("{}/assign-conversion", point.0));
                        let conversion_instruction = super::MirInstructionId::new(format!(
                            "inst:assign_numeric_convert:{}",
                            conversion_node.0
                        ))
                        .map_err(|error| error.to_string())?;
                        let conversions = function
                            .blocks
                            .values()
                            .flat_map(|candidate| candidate.instructions.iter())
                            .filter(|candidate| candidate.id == conversion_instruction)
                            .filter(|candidate| {
                                matches!(
                                    &candidate.kind,
                                    super::MirInstructionKind::Convert { result, source }
                                        if result == converted
                                            && source.as_str() == format!("expr:{}", value.node_id.0)
                                )
                            })
                            .count();
                        if conversions != 1
                            || function.values.get(converted).is_none_or(|actual| {
                                actual.ty != conversion.to
                            })
                        {
                            return Err(format!(
                                "assignment '{}' does not bind its exact checked NumericWiden conversion",
                                point.0
                            ));
                        }
                    }
                    MirProofOperation::Assign {
                        point: point.clone(),
                        target: target.clone(),
                        value: lower_expression(value, callable, function)?,
                        conversion: conversion.clone(),
                        result,
                        conversion_result,
                    }
                }
                T::Math(point) => MirProofOperation::Math(lower_assertion(
                    &point.id,
                    point.order,
                    &point.scope,
                    &point.condition,
                    callable,
                    function,
                )?),
                T::Branch {
                    point,
                    expression,
                    condition,
                    then_block,
                    else_block,
                } => {
                    let condition_value = MirValueId::new(format!("expr:{}", condition.node_id.0))
                        .map_err(|error| error.to_string())?;
                    let branches = function
                        .blocks
                        .values()
                        .filter_map(|block| match &block.terminator {
                            super::MirTerminator::Branch {
                                condition: actual,
                                then_target,
                                else_target,
                                ..
                            } if actual == &condition_value => {
                                Some((block.id.clone(), then_target.clone(), else_target.clone()))
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    if branches.len() != 1 {
                        return Err(format!(
                            "typed branch point '{}' has {} matching canonical MIR branch terminators",
                            point.0,
                            branches.len()
                        ));
                    }
                    let Some((branch_block, then_target, else_target)) =
                        branches.into_iter().next()
                    else {
                        return Err(format!(
                            "typed branch point '{}' lost its unique MIR terminator",
                            point.0
                        ));
                    };
                    scope.push(TypedProofScopeFrame::Branch {
                        statement: point.clone(),
                        expression: expression.clone(),
                        then_arm: true,
                    });
                    let then_block = lower_block(
                        then_block,
                        callable,
                        function,
                        scope,
                        new_result_receipt,
                        set_get_any_result_i32_receipt,
                    )?;
                    scope.pop();
                    scope.push(TypedProofScopeFrame::Branch {
                        statement: point.clone(),
                        expression: expression.clone(),
                        then_arm: false,
                    });
                    let else_block = lower_block(
                        else_block,
                        callable,
                        function,
                        scope,
                        new_result_receipt,
                        set_get_any_result_i32_receipt,
                    )?;
                    scope.pop();
                    MirProofOperation::Branch {
                        point: point.clone(),
                        expression: expression.clone(),
                        branch_block,
                        then_target,
                        else_target,
                        condition_value,
                        condition: lower_expression(condition, callable, function)?,
                        then_block,
                        else_block,
                    }
                }
                T::Scope(inner) => {
                    MirProofOperation::Scope(lower_block(
                        inner,
                        callable,
                        function,
                        scope,
                        new_result_receipt,
                        set_get_any_result_i32_receipt,
                    )?)
                }
                T::Loop {
                    point,
                    condition,
                        invariants,
                        body,
                        stationary,
                        receipt,
                        condition_calls,
                    } => {
                    let header = super::MirBlockId::new(format!(
                        "bb:while.header:{}",
                        point.0
                    ))
                    .map_err(|error| error.to_string())?;
                    let body_entry = super::MirBlockId::new(format!("bb:while.body:{}", point.0))
                        .map_err(|error| error.to_string())?;
                    let exit = super::MirBlockId::new(format!("bb:while.exit:{}", point.0))
                        .map_err(|error| error.to_string())?;
                    let condition_value = MirValueId::new(format!("expr:{}", condition.node_id.0))
                        .map_err(|error| error.to_string())?;
                    let invariant_points = invariants
                        .iter()
                        .map(|invariant| {
                            lower_assertion(
                                &invariant.id,
                                0,
                                &invariant.scope,
                                &invariant.condition,
                                callable,
                                function,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    if !function.values.contains_key(&condition_value) {
                        return Err(format!(
                            "stationary loop condition MIR value '{}' was not materialized",
                            condition_value
                        ));
                    }
                    let typed_receipt = receipt.as_ref().ok_or_else(|| {
                        format!(
                            "proof loop '{}' has no independently validated typed induction receipt",
                            point.0
                        )
                    })?;
                    crate::core::ir::validate_typed_loop_proof_receipt(
                        typed_receipt,
                        callable,
                        point,
                        scope,
                        invariants,
                    )?;
                    let source_call_profile = typed_receipt.ffi_call_path.as_ref();
                    let loop_call_inventory_matches = match source_call_profile {
                        Some(profile) => {
                            profile.loop_node == *point
                                && profile.condition_node == condition.node_id
                                && typed_receipt.call_nodes.as_slice() == [profile.call_node.clone()]
                                && typed_receipt.effect_nodes.is_empty()
                                && typed_receipt.writes.is_empty()
                                && typed_receipt.loans.is_empty()
                        }
                        None => typed_receipt.call_nodes.is_empty(),
                    };
                    if !loop_call_inventory_matches
                        || !typed_receipt.effect_nodes.is_empty()
                        || !typed_receipt.loans.is_empty()
                    {
                        return Err(format!(
                            "proof loop '{}' has call/effect/loan events outside the scalar induction or immutable at-most-once FFI profile",
                            point.0
                        ));
                    }
                    let loop_receipt =
                        lower_mir_loop_receipt(typed_receipt, callable, function)?;
                    let lowered_condition_calls = condition_calls
                        .iter()
                        .map(|call| {
                            lower_call_point(
                                call,
                                callable,
                                function,
                                new_result_receipt,
                                set_get_any_result_i32_receipt,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    scope.push(TypedProofScopeFrame::Loop(point.clone()));
                    let body = lower_block(
                        body,
                        callable,
                        function,
                        scope,
                        new_result_receipt,
                        set_get_any_result_i32_receipt,
                    )?;
                    scope.pop();
                    if let Some(profile) = source_call_profile {
                        if !mir_loop_body_matches_ffi_call_path(&body, profile)
                            || !condition_calls.is_empty()
                        {
                            return Err(format!(
                                "proof loop '{}' MIR body does not match its exact immutable at-most-once FFI path receipt",
                                point.0
                            ));
                        }
                    } else if mir_proof_block_contains_call(&body) {
                        return Err(format!(
                            "proof loop '{}' contains a call without an exact FFI path receipt",
                            point.0
                        ));
                    }
                    MirProofOperation::Loop {
                        point: point.clone(),
                        header,
                        body_entry,
                        exit,
                        condition_value,
                        condition: lower_expression(condition, callable, function)?,
                        condition_calls: lowered_condition_calls,
                        invariants: invariant_points,
                        body,
                        stationary: *stationary,
                        receipt: Some(loop_receipt),
                    }
                }
                T::Break {
                    point,
                    target_loop,
                    value,
                } => MirProofOperation::Break {
                    point: point.clone(),
                    target_loop: target_loop.clone(),
                    value: value
                        .as_ref()
                        .map(|value| lower_expression(value, callable, function))
                        .transpose()?,
                },
                T::Continue { point, target_loop } => MirProofOperation::Continue {
                    point: point.clone(),
                    target_loop: target_loop.clone(),
                },
                T::Return {
                    point,
                    value,
                    conversion,
                } => MirProofOperation::Return {
                        point: point.clone(),
                        value: value
                            .as_ref()
                            .filter(|value| !return_value_is_body_aggregate(value))
                            .map(|value| lower_expression(value, callable, function))
                        .transpose()?,
                        conversion: conversion.clone(),
                    },
                T::Exit => MirProofOperation::Exit,
            });
        }
        scope.pop();
        Ok(MirProofBlock {
            id: block.id.clone(),
            operations,
        })
    }

    fn collect_calls(block: &MirProofBlock, output: &mut Vec<MirProofCallPointId>) {
        for operation in &block.operations {
            match operation {
                MirProofOperation::Call(call) => output.push(MirProofCallPointId {
                    owner: call.owner.clone(),
                    statement: call.statement.clone(),
                    call_node: call.call_node.clone(),
                    order: call.order,
                }),
                MirProofOperation::Branch {
                    then_block,
                    else_block,
                    ..
                } => {
                    collect_calls(then_block, output);
                    collect_calls(else_block, output);
                }
                MirProofOperation::Scope(inner) => collect_calls(inner, output),
                MirProofOperation::Loop {
                    condition_calls,
                    body,
                    ..
                } => {
                    output.extend(condition_calls.iter().map(|call| MirProofCallPointId {
                        owner: call.owner.clone(),
                        statement: call.statement.clone(),
                        call_node: call.call_node.clone(),
                        order: call.order,
                    }));
                    collect_calls(body, output);
                }
                _ => {}
            }
        }
    }

    let expected_loop_invariants = typed
        .loop_invariant_points()
        .into_iter()
        .map(|point| MirProofPointId {
            owner: point.id.owner,
            statement: point.id.statement,
            ordinal: point.id.ordinal,
        })
        .collect();
    let mut expected_default_roots = active_defaults
        .iter()
        .map(|root| MirProofDefaultRootId {
            owner: root.owner.clone(),
            parameter: root.parameter.clone(),
            expression: root.expression.clone(),
        })
        .collect::<Vec<_>>();
    expected_default_roots.sort();
    let mut expected_default_math_points = active_defaults
        .iter()
        .flat_map(|root| root.points.iter())
        .map(|point| MirProofBoundaryMathId {
            point: point_id(&point.id),
            order: point.order,
            expression: point.condition.node_id.clone(),
            scope: point.scope.clone(),
        })
        .collect::<Vec<_>>();
    expected_default_math_points.sort();
    let mut expected_default_calls = active_defaults
        .iter()
        .flat_map(|root| root.calls.iter())
        .map(boundary_call)
        .collect::<Vec<_>>();
    expected_default_calls.sort_by_key(|call| call.id.clone());
    let mut expected_default_boundaries = active_defaults
        .iter()
        .flat_map(|root| {
            root.boundaries
                .iter()
                .map(move |boundary| MirProofDefaultBoundary {
                    root: MirProofDefaultRootId {
                        owner: root.owner.clone(),
                        parameter: root.parameter.clone(),
                        expression: root.expression.clone(),
                    },
                    point: boundary.point.clone(),
                    reason: boundary.reason.clone(),
                })
        })
        .collect::<Vec<_>>();
    expected_default_boundaries.sort();
    let mut expected_default_uses = typed
        .default_uses
        .iter()
        .filter(|use_id| {
            active_defaults
                .iter()
                .any(|root| root.owner == use_id.callee_owner && root.parameter == use_id.parameter)
        })
        .map(|use_id| MirProofDefaultUseId {
            caller_owner: use_id.caller_owner.clone(),
            call_node: use_id.call_node.clone(),
            callee_owner: use_id.callee_owner.clone(),
            parameter: use_id.parameter.clone(),
            argument_node: use_id.argument_node.clone(),
            parameter_index: use_id.parameter_index,
            order: use_id.order,
            scope: use_id.scope.clone(),
            guards: use_id
                .guards
                .iter()
                .map(|guard| (guard.condition.node_id.clone(), guard.when))
                .collect(),
            disposition: format!("{:?}", use_id.disposition),
        })
        .collect::<Vec<_>>();
    expected_default_uses.sort();
    let root = match lower_block(
        &typed.root,
        callable,
        function,
        &mut Vec::new(),
        new_result_receipt,
        set_get_any_result_i32_receipt,
    ) {
        Ok(root) => root,
        Err(message) => {
            return Ok(Some(boundary_worklist(
                &typed,
                callable,
                format!("proof-point lowering boundary: {message}"),
            )));
        }
    };
    let mut lowered_calls = Vec::new();
    collect_calls(&root, &mut lowered_calls);
    let mut expected_calls = typed
        .calls
        .iter()
        .map(|point| MirProofCallPointId {
            owner: point.owner.clone(),
            statement: point.statement.clone(),
            call_node: point.call_node.clone(),
            order: point.order,
        })
        .collect::<Vec<_>>();
    lowered_calls.sort();
    expected_calls.sort();
    let mut string_boundary_calls = Vec::new();
    let mut set_result_boundary_calls = Vec::new();
    let mut set_get_any_i32_boundary_calls = Vec::new();
    let mut set_get_any_result_i32_boundary_calls = Vec::new();
    let mut values_local_index_i32_result_branch_boundary_calls = Vec::new();
    if lowered_calls != expected_calls {
        let lowered = lowered_calls.iter().cloned().collect::<BTreeSet<_>>();
        let expected = expected_calls.iter().cloned().collect::<BTreeSet<_>>();
        let string_receipt = function
            .ownership
            .ordinary_map_set_get_string_lineage_receipt();
        let set_result_receipt = function.ownership.ordinary_map_set_lineage_receipt();
        let any_i32_receipt = function
            .ownership
            .ordinary_map_set_get_any_i32_lineage_receipt();
        let any_result_i32_receipt = function
            .ownership
            .ordinary_map_owned_any_v2_set_get_any_result_i32_lineage_receipt();
        let receipted_calls =
            if let Some(receipt) = retained_values_local_index_i32_result_branch_calls {
                BTreeSet::from([
                    receipt.map_new_operation.clone(),
                    receipt.map_set_operation.clone(),
                    receipt.values_operation.clone(),
                ])
            } else if let Some(receipt) = set_result_receipt {
                BTreeSet::from([
                    receipt.new_operation.clone(),
                    receipt.set_operation.clone(),
                    receipt.size_operation.clone(),
                ])
            } else if let Some(receipt) = string_receipt {
                BTreeSet::from([
                    receipt.new_operation.clone(),
                    receipt.set_operation.clone(),
                    receipt.get_operation.clone(),
                ])
            } else if let Some(receipt) = any_i32_receipt {
                BTreeSet::from([
                    receipt.new_operation.clone(),
                    receipt.set_operation.clone(),
                    receipt.get_operation.clone(),
                ])
            } else if let Some(receipt) = any_result_i32_receipt {
                BTreeSet::from([
                    receipt.new_operation.clone(),
                    receipt.set_operation.clone(),
                    receipt.get_operation.clone(),
                ])
            } else {
                return Ok(Some(boundary_worklist(
                    &typed,
                    callable,
                    "lowered MIR call events disagree with the complete checker call census".into(),
                )));
            };
        let typed_receipted_calls = typed
            .calls
            .iter()
            .filter(|point| receipted_calls.contains(&point.call_node))
            .collect::<Vec<_>>();
        let exact_receipted_census = typed_receipted_calls.len() == receipted_calls.len()
            && receipted_calls.iter().all(|node| {
                typed_receipted_calls
                    .iter()
                    .filter(|point| &point.call_node == node)
                    .count()
                    == 1
            });
        let missing_receipted_calls = typed
            .calls
            .iter()
            .zip(&bridged_body_calls)
            .filter(|(point, _)| !lowered.contains(&call_id(point)))
            .collect::<Vec<_>>();
        let missing_ids = missing_receipted_calls
            .iter()
            .map(|(point, _)| call_id(point).call_node)
            .collect::<BTreeSet<_>>();
        let exact_values_branch_deferred_calls = retained_values_local_index_i32_result_branch_calls
            .is_some()
            && missing_receipted_calls.len() == 2
            && missing_ids
                == BTreeSet::from([
                    retained_values_local_index_i32_result_branch_calls
                        .expect("checked above")
                        .map_set_operation
                        .clone(),
                    retained_values_local_index_i32_result_branch_calls
                        .expect("checked above")
                        .values_operation
                        .clone(),
                ])
            && missing_receipted_calls.iter().all(|(point, bridge)| {
                bridge.is_none()
                    && matches!(
                        &point.disposition,
                        crate::core::ir::TypedCallExecutionDisposition::Deferred(_)
                    )
            });
        if !exact_receipted_census
            || !lowered.is_subset(&expected)
            || missing_receipted_calls.is_empty()
            || missing_ids.len() != missing_receipted_calls.len()
            || missing_ids
                .iter()
                .any(|node| !receipted_calls.contains(node))
            || missing_receipted_calls
                .iter()
                .any(|(_, bridge)| bridge.is_none())
                && !exact_values_branch_deferred_calls
        {
            return Ok(Some(boundary_worklist(
                &typed,
                callable,
                "lowered MIR call events disagree with the complete checker call census".into(),
            )));
        }
        let mut calls = missing_receipted_calls
            .iter()
            .map(|(point, _)| boundary_call(point))
            .collect::<Vec<_>>();
        calls.sort_by_key(|call| call.id.clone());
        if retained_values_local_index_i32_result_branch_calls.is_some() {
            values_local_index_i32_result_branch_boundary_calls = calls;
        } else if set_result_receipt.is_some() {
            if callable
                .resources
                .ordinary_map_owned_any_v2_set_lineage_receipt
                .as_ref()
                != set_result_receipt
                || !boundaries_are_set_result_exact(
                    &typed,
                    set_result_receipt.expect("checked above"),
                )
            {
                return Ok(Some(boundary_worklist(
                    &typed,
                    callable,
                    "SetResult retained calls have no matching exact Checker worklist/receipt"
                        .into(),
                )));
            }
            set_result_boundary_calls = calls;
        } else if string_receipt.is_some() {
            string_boundary_calls = calls;
        } else if any_result_i32_receipt.is_some() {
            if set_get_any_result_i32_receipt != any_result_i32_receipt {
                return Ok(Some(boundary_worklist(
                    &typed,
                    callable,
                    "Set/GetAny-Result-i32 retained calls have no matching explicit receipt input"
                        .into(),
                )));
            }
            set_get_any_result_i32_boundary_calls = calls;
        } else {
            set_get_any_i32_boundary_calls = calls;
        }
    }
    let mut string_scoped_boundaries = retained_string_match_boundaries
        .map(|boundaries| {
            boundaries
                .iter()
                .map(|boundary| MirProofSourceBoundary {
                    point: boundary.point.clone(),
                    reason: boundary.reason.clone(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    string_scoped_boundaries.sort();
    let mut set_result_scoped_boundaries = retained_set_result_match_boundaries
        .map(|boundaries| {
            boundaries
                .iter()
                .map(|boundary| MirProofSourceBoundary {
                    point: boundary.point.clone(),
                    reason: boundary.reason.clone(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    set_result_scoped_boundaries.sort();
    let set_result_call_boundary = (!set_result_boundary_calls.is_empty()
        || !set_result_scoped_boundaries.is_empty())
    .then(|| MirProofBoundary {
        point: callable.owner.clone(),
        reason: "exact Checker SetResult receipt and MIR bridges cover the three fallible New/Set/Size call points".into(),
        scoped_boundaries: set_result_scoped_boundaries,
        math_points: Vec::new(),
        loop_invariants: Vec::new(),
        calls: set_result_boundary_calls,
        deferred_calls: Vec::new(),
        default_roots: Vec::new(),
        default_math_points: Vec::new(),
        default_calls: Vec::new(),
        default_boundaries: Vec::new(),
        default_uses: Vec::new(),
    });
    let string_call_boundary = (!string_boundary_calls.is_empty()
        || !string_scoped_boundaries.is_empty())
    .then(|| MirProofBoundary {
        point: callable.owner.clone(),
        reason: "exact Checker String Set/Get receipt and MIR operation bridges cover Result-match call points".into(),
        scoped_boundaries: string_scoped_boundaries,
        math_points: Vec::new(),
        loop_invariants: Vec::new(),
        calls: string_boundary_calls,
        deferred_calls: Vec::new(),
        default_roots: Vec::new(),
        default_math_points: Vec::new(),
        default_calls: Vec::new(),
        default_boundaries: Vec::new(),
        default_uses: Vec::new(),
    });
    let mut set_get_any_i32_scoped_boundaries = retained_set_get_any_i32_match_boundaries
        .map(|boundaries| {
            boundaries
                .iter()
                .map(|boundary| MirProofSourceBoundary {
                    point: boundary.point.clone(),
                    reason: boundary.reason.clone(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    set_get_any_i32_scoped_boundaries.sort();
    let set_get_any_i32_call_boundary = (!set_get_any_i32_boundary_calls.is_empty()
        || !set_get_any_i32_scoped_boundaries.is_empty())
    .then(|| MirProofBoundary {
        point: callable.owner.clone(),
        reason: "exact Checker Set/GetAny-i32 receipt and MIR bridges cover fallible Result and GetAny call points".into(),
        scoped_boundaries: set_get_any_i32_scoped_boundaries,
        math_points: Vec::new(),
        loop_invariants: Vec::new(),
        calls: set_get_any_i32_boundary_calls,
        deferred_calls: Vec::new(),
        default_roots: Vec::new(),
        default_math_points: Vec::new(),
        default_calls: Vec::new(),
        default_boundaries: Vec::new(),
        default_uses: Vec::new(),
    });
    let mut set_get_any_result_i32_scoped_boundaries =
        retained_set_get_any_result_i32_match_boundaries
            .map(|boundaries| {
                boundaries
                    .iter()
                    .map(|boundary| MirProofSourceBoundary {
                        point: boundary.point.clone(),
                        reason: boundary.reason.clone(),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
    set_get_any_result_i32_scoped_boundaries.sort();
    let set_get_any_result_i32_call_boundary =
        (!set_get_any_result_i32_boundary_calls.is_empty()
            || !set_get_any_result_i32_scoped_boundaries.is_empty())
        .then(|| MirProofBoundary {
            point: callable.owner.clone(),
            reason: "exact Checker Set/GetAny-Result-i32 receipt and MIR bridges cover the three fallible Map call points".into(),
            scoped_boundaries: set_get_any_result_i32_scoped_boundaries,
            math_points: Vec::new(),
            loop_invariants: Vec::new(),
            calls: set_get_any_result_i32_boundary_calls,
            deferred_calls: Vec::new(),
            default_roots: Vec::new(),
            default_math_points: Vec::new(),
            default_calls: Vec::new(),
            default_boundaries: Vec::new(),
            default_uses: Vec::new(),
        });
    let values_local_index_i32_result_branch_call_boundary =
        (!values_local_index_i32_result_branch_boundary_calls.is_empty()).then(|| {
            MirProofBoundary {
                point: callable.owner.clone(),
                reason: "exact Checker ValuesLocalIndexI32 Result-branch receipt retains the deferred SetAnyResult and Values call events".into(),
                scoped_boundaries: Vec::new(),
                math_points: Vec::new(),
                loop_invariants: Vec::new(),
                calls: values_local_index_i32_result_branch_boundary_calls,
                deferred_calls: Vec::new(),
                default_roots: Vec::new(),
                default_math_points: Vec::new(),
                default_calls: Vec::new(),
                default_boundaries: Vec::new(),
                default_uses: Vec::new(),
            }
        });
    let mut expected_scoped_boundaries = typed
        .boundaries
        .iter()
        .map(|boundary| boundary.point.clone())
        .collect::<Vec<_>>();
    expected_scoped_boundaries.sort();
    expected_scoped_boundaries.dedup();
    let mut retained_boundaries = retained_new_result_match_boundary
        .map(|boundary| {
            vec![MirProofBoundary {
                point: callable.owner.clone(),
                reason: "exact NewResult Checker CFG/MIR receipt covers this source match boundary"
                    .into(),
                scoped_boundaries: vec![MirProofSourceBoundary {
                    point: boundary.point.clone(),
                    reason: boundary.reason.clone(),
                }],
                math_points: Vec::new(),
                loop_invariants: Vec::new(),
                calls: Vec::new(),
                deferred_calls: Vec::new(),
                default_roots: Vec::new(),
                default_math_points: Vec::new(),
                default_calls: Vec::new(),
                default_boundaries: Vec::new(),
                default_uses: Vec::new(),
            }]
        })
        .unwrap_or_default();
    retained_boundaries.extend(set_result_call_boundary);
    retained_boundaries.extend(string_call_boundary);
    retained_boundaries.extend(set_get_any_i32_call_boundary);
    retained_boundaries.extend(set_get_any_result_i32_call_boundary);
    retained_boundaries.extend(values_local_index_i32_result_branch_call_boundary);
    let worklist = MirProofWorklist {
        owner: typed.owner,
        expected_math_points: typed
            .points
            .iter()
            .map(|point| MirProofPointId {
                owner: point.id.owner.clone(),
                statement: point.id.statement.clone(),
                ordinal: point.id.ordinal,
            })
            .collect(),
        expected_loop_invariants,
        expected_calls,
        expected_deferred_calls: Vec::new(),
        expected_scoped_boundaries,
        expected_default_roots,
        expected_default_math_points,
        expected_default_calls,
        expected_default_boundaries,
        expected_default_uses,
        expected_place_inputs: callable
            .body
            .place_inputs
            .iter()
            .map(|(point, expression)| MirProofPlaceInputId {
                point: point.clone(),
                expression: expression.node_id.clone(),
            })
            .collect(),
        boundaries: retained_boundaries,
        root,
        private_source_witness: [0; 32],
    }
    .seal_source_witness();
    Ok(Some(worklist))
}

pub(crate) fn validate_proof_worklist(
    worklist: &MirProofWorklist,
    function: &MirFunction,
    catalog: &MirTypeCatalog,
) -> Result<(), Vec<MirValidationError>> {
    let mut errors = Vec::new();
    if !worklist.has_valid_source_witness() {
        errors.push(MirValidationError {
            subject: function.owner.0.clone(),
            message: "proof worklist has an invalid private checked-source witness".into(),
        });
    }
    if worklist.owner != function.owner {
        errors.push(MirValidationError {
            subject: function.owner.0.clone(),
            message: "proof worklist owner disagrees with its MIR function".into(),
        });
    }

    fn validate_expression(
        expression: &MirProofExpression,
        function: &MirFunction,
        catalog: &MirTypeCatalog,
        require_bool: bool,
        errors: &mut Vec<MirValidationError>,
    ) {
        if !expression.expression_types.contains_key(&Vec::new()) {
            errors.push(MirValidationError {
                subject: expression.origin.0.clone(),
                message: "proof-point expression has no checker-owned root TypeDesc receipt".into(),
            });
        }
        if expression.expression_types.get(&Vec::new()) != Some(&expression.ty) {
            errors.push(MirValidationError {
                subject: expression.origin.0.clone(),
                message: "proof-point expression root TypeDesc disagrees with its source seal"
                    .into(),
            });
        }
        if crate::core::ir::type_path_fingerprint(&expression.expression_types)
            != expression.type_path_fingerprint
        {
            errors.push(MirValidationError {
                subject: expression.origin.0.clone(),
                message: "proof-point expression TypeDesc path map disagrees with its source seal"
                    .into(),
            });
        }
        let contract = MirContract {
            id: expression.origin.clone(),
            kind: MirContractKind::Requires,
            condition: expression.expression.clone(),
            expression_types: expression.expression_types.clone(),
            ffi_helper_call: None,
        };
        if let Err(message) = validate_expression_types(&contract, function, catalog) {
            errors.push(MirValidationError {
                subject: expression.origin.0.clone(),
                message: format!("proof-point expression TypeDesc receipt rejected: {message}"),
            });
        }
        if contains_invalid_old(function, &expression.expression) {
            errors.push(MirValidationError {
                subject: expression.origin.0.clone(),
                message: "proof-point old() is not a direct callable-entry parameter snapshot"
                    .into(),
            });
        }
        if require_bool
            && expression
                .expression_types
                .get(&Vec::new())
                .and_then(|ty| catalog.get(ty))
                .is_none_or(|descriptor| descriptor.abi != MirAbiClass::Bool)
        {
            errors.push(MirValidationError {
                subject: expression.origin.0.clone(),
                message: "proof-point predicate TypeDesc is not canonical bool".into(),
            });
        }
    }

    fn validate_boundary_call(
        call: &MirProofCallBoundary,
        expected_owner: &NodeId,
        errors: &mut Vec<MirValidationError>,
    ) {
        use crate::core::ir::TypedCallEffectAuthority;

        let arguments_are_ordered = call.arguments.len() == call.argument_count
            && call
                .arguments
                .iter()
                .enumerate()
                .all(|(ordinal, argument)| argument.ordinal == ordinal);
        let effect_matches = call.callee.as_ref().is_some_and(|callee| {
            call.effect.call_node == call.id.call_node
                && call.effect.callee == *callee
                && call.effect.declared_effects == call.declared_effects
                && call.effect.authority == TypedCallEffectAuthority::UnknownMayReadWrite
        });
        if call.id.owner != *expected_owner
            || call.expression != call.id.call_node
            || call.callee.is_none()
            || call.result_type.as_ref() != Some(&call.expression_type)
            || !arguments_are_ordered
            || !effect_matches
        {
            errors.push(MirValidationError {
                subject: call.id.call_node.0.clone(),
                message: "deferred proof-call boundary lost its exact selected callee, ordered actuals, result type, or conservative effect identity".into(),
            });
        }
    }

    fn validate_call_point(
        call: &MirProofCallPoint,
        expected_scope: &[MirProofScopeFrame],
        function: &MirFunction,
        catalog: &MirTypeCatalog,
        errors: &mut Vec<MirValidationError>,
    ) {
        use crate::core::ir::TypedCallEffectAuthority;

        let new_result_receipt = function.ownership.ordinary_map_new_result_receipt();
        let set_result_receipt = function.ownership.ordinary_map_set_lineage_receipt();
        let set_get_i32_receipt = function
            .ownership
            .ordinary_map_set_get_i32_lineage_receipt();
        let set_get_string_receipt = function
            .ownership
            .ordinary_map_set_get_string_lineage_receipt();
        let set_get_any_i32_receipt = function
            .ownership
            .ordinary_map_set_get_any_i32_lineage_receipt();
        let set_get_any_result_i32_receipt = function
            .ownership
            .ordinary_map_owned_any_v2_set_get_any_result_i32_lineage_receipt();
        let values_local_index_i32_result_branch_receipt = function
            .ownership
            .ordinary_map_owned_any_v2_cfg_receipt()
            .and_then(|cfg| cfg.values_local_index_i32_result_branch_receipt.as_ref());
        let new_result_new_bridge = new_result_receipt.is_some_and(|receipt| {
            let expected_result = MirValueId::new(format!("local:{}", receipt.result_local.0 .0));
            let expected_instruction = super::MirInstructionId::new(format!(
                "inst:ordinary_map_new_result:new:{}",
                receipt.operation.0
            ));
            matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == "map_new")
                && call.call_node == receipt.operation
                && call.result_type == receipt.source_result_type
                && expected_instruction.is_ok_and(|instruction| instruction == call.instruction)
                && expected_result.as_ref().is_ok_and(|result| call.result.as_ref() == Some(result))
                && call.arguments.is_empty()
                && expected_result.as_ref().is_ok_and(|result| {
                    function.values.get(result).is_some_and(|value| {
                        value.ty == super::types::ordinary_map_owned_any_v2_root_result_type_id()
                    })
                })
        });
        let new_result_size_bridge = new_result_receipt.is_some_and(|receipt| {
            let expected_result = MirValueId::new(format!("local:{}", receipt.size_local.0 .0));
            let expected_root = MirValueId::new(format!("local:{}", receipt.root_local.0 .0));
            let expected_instruction = super::MirInstructionId::new(format!(
                "inst:ordinary_map_new_result:size:{}",
                receipt.size_operation.0
            ));
            matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == "map_size")
                && call.call_node == receipt.size_operation
                && call.result_type == receipt.status_type
                && expected_instruction.is_ok_and(|instruction| instruction == call.instruction)
                && expected_result.as_ref().is_ok_and(|result| call.result.as_ref() == Some(result))
                && call.arguments.len() == 1
                && expected_root
                    .as_ref()
                    .is_ok_and(|root| call.arguments[0].value == *root)
                && call.arguments[0].expression.origin == receipt.size_receiver
                && call.arguments[0].expression.ty == receipt.root_type
                && call.arguments[0].conversion.kind
                    == crate::core::ir::CheckedConversionKind::Identity
                && call.arguments[0].conversion.from == receipt.root_type
                && call.arguments[0].conversion.to == receipt.root_type
                && function.values.get(&call.arguments[0].value)
                    .is_some_and(|value| value.ty == super::types::ordinary_map_owned_any_v2_type_id())
                && expected_result.as_ref().is_ok_and(|result| {
                    function.values.get(result).is_some_and(|value| value.ty == receipt.status_type)
                })
        });
        let set_result_new_bridge = set_result_receipt.is_some_and(|receipt| {
            let expected_result =
                MirValueId::new(format!("local:{}", receipt.new_result_local.0 .0));
            let expected_instruction = super::MirInstructionId::new(format!(
                "inst:ordinary_map_set_lineage:new_result:{}",
                receipt.new_operation.0
            ));
            matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == "map_new")
                && call.call_node == receipt.new_operation
                && call.result_type == receipt.new_result_type
                && expected_instruction.is_ok_and(|instruction| instruction == call.instruction)
                && expected_result
                    .as_ref()
                    .is_ok_and(|result| call.result.as_ref() == Some(result))
                && call.arguments.is_empty()
                && expected_result.as_ref().is_ok_and(|result| {
                    function.values.get(result).is_some_and(|value| {
                        value.ty == super::types::ordinary_map_owned_any_v2_root_result_type_id()
                    })
                })
        });
        let set_result_set_bridge = set_result_receipt.is_some_and(|receipt| {
            let expected_result =
                MirValueId::new(format!("local:{}", receipt.set_result_local.0 .0));
            let root = MirValueId::new(format!("local:{}", receipt.initial_root_local.0 .0));
            let key = MirValueId::new("literal:ordinary_map_set_lineage:answer");
            let value = MirValueId::new("literal:ordinary_map_set_lineage:42");
            let expected_instruction = super::MirInstructionId::new(format!(
                "inst:ordinary_map_set_lineage:set_result:{}",
                receipt.set_operation.0
            ));
            matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == "map_set")
                && call.call_node == receipt.set_operation
                && call.result_type == receipt.set_result_type
                && expected_instruction.is_ok_and(|instruction| instruction == call.instruction)
                && expected_result
                    .as_ref()
                    .is_ok_and(|result| call.result.as_ref() == Some(result))
                && call.arguments.len() == 3
                && root
                    .as_ref()
                    .is_ok_and(|root| call.arguments[0].value == *root)
                && call.arguments[0].expression.origin == receipt.set_receiver
                && call.arguments[0].expression.ty == receipt.initial_root_type
                && key.as_ref().is_ok_and(|key| call.arguments[1].value == *key)
                && value
                    .as_ref()
                    .is_ok_and(|value| call.arguments[2].value == *value)
        });
        let set_result_size_bridge = set_result_receipt.is_some_and(|receipt| {
            let expected_result = MirValueId::new(format!("local:{}", receipt.size_local.0 .0));
            let expected_root =
                MirValueId::new(format!("local:{}", receipt.updated_root_local.0 .0));
            let expected_instruction = super::MirInstructionId::new(format!(
                "inst:ordinary_map_set_lineage:size:{}",
                receipt.size_operation.0
            ));
            let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
            matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == "map_size")
                && call.call_node == receipt.size_operation
                && call.result_type == i32_type
                && expected_instruction.is_ok_and(|instruction| instruction == call.instruction)
                && expected_result
                    .as_ref()
                    .is_ok_and(|result| call.result.as_ref() == Some(result))
                && call.arguments.len() == 1
                && expected_root
                    .as_ref()
                    .is_ok_and(|root| call.arguments[0].value == *root)
                && call.arguments[0].expression.origin == receipt.size_receiver
                && call.arguments[0].expression.ty == receipt.updated_root_type
                && function.values.get(&call.arguments[0].value).is_some_and(|value| {
                    value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                })
                && expected_result.as_ref().is_ok_and(|result| {
                    function
                        .values
                        .get(result)
                        .is_some_and(|value| value.ty == i32_type)
                })
        });
        let set_get_i32_new_bridge = set_get_i32_receipt.is_some_and(|receipt| {
            let expected_result = MirValueId::new(format!("local:{}", receipt.new_result_local.0 .0));
            let expected_instruction = super::MirInstructionId::new(format!("inst:ordinary_map_set_get_i32_lineage:new_result:{}", receipt.new_operation.0));
            matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == "map_new")
                && call.call_node == receipt.new_operation && call.result_type == receipt.set_result_type
                && expected_instruction.is_ok_and(|instruction| instruction == call.instruction)
                && expected_result.as_ref().is_ok_and(|result| call.result.as_ref() == Some(result))
                && call.arguments.is_empty()
        });
        let set_get_i32_set_bridge = set_get_i32_receipt.is_some_and(|receipt| {
            let expected_result = MirValueId::new(format!("local:{}", receipt.set_result_local.0 .0));
            let root = MirValueId::new(format!("local:{}", receipt.initial_root_local.0 .0));
            let key = MirValueId::new("literal:ordinary_map_set_get_i32_lineage:set-answer");
            let value = MirValueId::new("literal:ordinary_map_set_get_i32_lineage:42");
            let expected_instruction = super::MirInstructionId::new(format!("inst:ordinary_map_set_get_i32_lineage:set_result:{}", receipt.set_operation.0));
            matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == "map_set")
                && call.call_node == receipt.set_operation && call.result_type == receipt.set_result_type
                && expected_instruction.is_ok_and(|instruction| instruction == call.instruction)
                && expected_result.as_ref().is_ok_and(|result| call.result.as_ref() == Some(result))
                && call.arguments.len() == 3
                && root.as_ref().is_ok_and(|root| call.arguments[0].value == *root)
                && call.arguments[0].expression.origin == receipt.set_receiver
                && call.arguments[0].expression.ty == receipt.initial_root_type
                && key.as_ref().is_ok_and(|key| call.arguments[1].value == *key)
                && value.as_ref().is_ok_and(|value| call.arguments[2].value == *value)
        });
        let set_get_i32_get_bridge = set_get_i32_receipt.is_some_and(|receipt| {
            let expected_result = MirValueId::new(format!("local:{}", receipt.get_result_local.0 .0));
            let root = MirValueId::new(format!("local:{}", receipt.updated_root_local.0 .0));
            let key = MirValueId::new("literal:ordinary_map_set_get_i32_lineage:get-answer");
            let expected_instruction = super::MirInstructionId::new(format!("inst:ordinary_map_set_get_i32_lineage:get_i32_result:{}", receipt.get_operation.0));
            matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == "map_get_i32")
                && call.call_node == receipt.get_operation && call.result_type == receipt.get_result_type
                && expected_instruction.is_ok_and(|instruction| instruction == call.instruction)
                && expected_result.as_ref().is_ok_and(|result| call.result.as_ref() == Some(result))
                && call.arguments.len() == 2
                && root.as_ref().is_ok_and(|root| call.arguments[0].value == *root)
                && call.arguments[0].expression.origin == receipt.get_receiver
                && call.arguments[0].expression.ty == receipt.updated_root_type
                && key.as_ref().is_ok_and(|key| call.arguments[1].value == *key)
        });
        let set_get_string_new_bridge = set_get_string_receipt.is_some_and(|receipt| {
            let expected_result = MirValueId::new(format!("local:{}", receipt.new_result_local.0 .0));
            let expected_instruction = super::MirInstructionId::new(format!("inst:ordinary_map_set_get_string_lineage:new_result:{}", receipt.new_operation.0));
            matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == "map_new")
                && call.call_node == receipt.new_operation
                && call.result_type == receipt.set_result_type
                && expected_instruction.is_ok_and(|instruction| instruction == call.instruction)
                && expected_result.as_ref().is_ok_and(|result| call.result.as_ref() == Some(result))
                && call.arguments.is_empty()
        });
        let set_get_string_set_bridge = set_get_string_receipt.is_some_and(|receipt| {
            let expected_result = MirValueId::new(format!("local:{}", receipt.set_result_local.0 .0));
            let root = MirValueId::new(format!("local:{}", receipt.initial_root_local.0 .0));
            let key = MirValueId::new("literal:ordinary_map_set_get_string_lineage:set-answer");
            let value = MirValueId::new(format!("local:{}", receipt.set_payload_local.0 .0));
            let expected_instruction = super::MirInstructionId::new(format!("inst:ordinary_map_set_get_string_lineage:set_string_result:{}", receipt.set_operation.0));
            matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == "map_set")
                && call.call_node == receipt.set_operation
                && call.result_type == receipt.set_result_type
                && expected_instruction.is_ok_and(|instruction| instruction == call.instruction)
                && expected_result.as_ref().is_ok_and(|result| call.result.as_ref() == Some(result))
                && call.arguments.len() == 3
                && root.as_ref().is_ok_and(|root| call.arguments[0].value == *root)
                && call.arguments[0].expression.origin == receipt.set_receiver
                && call.arguments[0].expression.ty == receipt.initial_root_type
                && key.as_ref().is_ok_and(|key| call.arguments[1].value == *key)
                && value.as_ref().is_ok_and(|value| call.arguments[2].value == *value)
        });
        let set_get_string_get_bridge = set_get_string_receipt.is_some_and(|receipt| {
            let expected_result = MirValueId::new(format!("local:{}", receipt.get_result_local.0 .0));
            let root = MirValueId::new(format!("local:{}", receipt.updated_root_local.0 .0));
            let key = MirValueId::new("literal:ordinary_map_set_get_string_lineage:get-answer");
            let expected_instruction = super::MirInstructionId::new(format!("inst:ordinary_map_set_get_string_lineage:get_string_result:{}", receipt.get_operation.0));
            matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == "map_get_string")
                && call.call_node == receipt.get_operation
                && call.result_type == receipt.get_result_type
                && expected_instruction.is_ok_and(|instruction| instruction == call.instruction)
                && expected_result.as_ref().is_ok_and(|result| call.result.as_ref() == Some(result))
                && call.arguments.len() == 2
                && root.as_ref().is_ok_and(|root| call.arguments[0].value == *root)
                && call.arguments[0].expression.origin == receipt.get_receiver
                && call.arguments[0].expression.ty == receipt.updated_root_type
                && key.as_ref().is_ok_and(|key| call.arguments[1].value == *key)
        });
        let set_get_any_i32_new_bridge = set_get_any_i32_receipt.is_some_and(|receipt| {
            let expected_result =
                MirValueId::new(format!("local:{}", receipt.new_result_local.0 .0));
            let expected_instruction = super::MirInstructionId::new(format!(
                "inst:ordinary_map_set_get_any_i32_lineage:new_result:{}",
                receipt.new_operation.0
            ));
            matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == "map_new")
                && call.call_node == receipt.new_operation
                && call.result_type == receipt.new_result_type
                && expected_instruction.is_ok_and(|instruction| instruction == call.instruction)
                && expected_result.as_ref().is_ok_and(|result| call.result.as_ref() == Some(result))
                && call.arguments.is_empty()
        });
        let set_get_any_i32_set_bridge = set_get_any_i32_receipt.is_some_and(|receipt| {
            let expected_result =
                MirValueId::new(format!("local:{}", receipt.set_result_local.0 .0));
            let root = MirValueId::new(format!("local:{}", receipt.initial_root_local.0 .0));
            let key = MirValueId::new(format!("literal:{}", receipt.set_key.0));
            let value = MirValueId::new(format!("literal:{}", receipt.set_value.0));
            let expected_instruction = super::MirInstructionId::new(format!(
                "inst:ordinary_map_set_get_any_i32_lineage:set_result:{}",
                receipt.set_operation.0
            ));
            matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == "map_set")
                && call.call_node == receipt.set_operation
                && call.result_type == receipt.set_result_type
                && expected_instruction.is_ok_and(|instruction| instruction == call.instruction)
                && expected_result.as_ref().is_ok_and(|result| call.result.as_ref() == Some(result))
                && call.arguments.len() == 3
                && root.as_ref().is_ok_and(|root| call.arguments[0].value == *root)
                && call.arguments[0].expression.origin == receipt.set_receiver
                && call.arguments[0].expression.ty == receipt.initial_root_type
                && key.as_ref().is_ok_and(|key| call.arguments[1].value == *key)
                && value.as_ref().is_ok_and(|value| call.arguments[2].value == *value)
        });
        let set_get_any_i32_get_bridge = set_get_any_i32_receipt.is_some_and(|receipt| {
            let expected_result =
                MirValueId::new(format!("local:{}", receipt.get_result_local.0 .0));
            let root = MirValueId::new(format!("local:{}", receipt.updated_root_local.0 .0));
            let key = MirValueId::new(format!("literal:{}", receipt.get_key.0));
            let expected_instruction = super::MirInstructionId::new(format!(
                "inst:ordinary_map_set_get_any_i32_lineage:get_any_result:{}",
                receipt.get_operation.0
            ));
            matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == "map_get")
                && call.call_node == receipt.get_operation
                && call.result_type == receipt.get_result_type
                && expected_instruction.is_ok_and(|instruction| instruction == call.instruction)
                && expected_result.as_ref().is_ok_and(|result| call.result.as_ref() == Some(result))
                && call.arguments.len() == 2
                && root.as_ref().is_ok_and(|root| call.arguments[0].value == *root)
                && call.arguments[0].expression.origin == receipt.get_receiver
                && call.arguments[0].expression.ty == receipt.updated_root_type
                && key.as_ref().is_ok_and(|key| call.arguments[1].value == *key)
        });
        let set_get_any_result_i32_new_bridge =
            set_get_any_result_i32_receipt.is_some_and(|receipt| {
                let expected_result =
                    MirValueId::new(format!("local:{}", receipt.new_result_local.0 .0));
                let expected_instruction = super::MirInstructionId::new(format!(
                    "inst:ordinary_map_set_get_any_result_i32_lineage:new_result:{}",
                    receipt.new_operation.0
                ));
                matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == "map_new")
                    && call.call_node == receipt.new_operation
                    && call.result_type == receipt.new_result_type
                    && expected_instruction.is_ok_and(|instruction| instruction == call.instruction)
                    && expected_result.as_ref().is_ok_and(|result| call.result.as_ref() == Some(result))
                    && call.arguments.is_empty()
            });
        let values_local_index_i32_result_branch_new_bridge =
            values_local_index_i32_result_branch_receipt.is_some_and(|receipt| {
                let expected_result =
                    MirValueId::new(format!("expr:{}", receipt.map_new_operation.0));
                let expected_instruction = super::MirInstructionId::new(format!(
                    "inst:ordinary_map_owned_any_v2_new:local:{}:{}",
                    receipt.initial_root.0 .0, receipt.map_new_operation.0
                ));
                matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == "map_new")
                    && receipt.owner == function.owner
                    && call.call_node == receipt.map_new_operation
                    && expected_instruction.is_ok_and(|instruction| instruction == call.instruction)
                    && expected_result.as_ref().is_ok_and(|result| call.result.as_ref() == Some(result))
                    && call.arguments.is_empty()
                    && expected_result.as_ref().is_ok_and(|result| {
                        function.values.get(result).is_some_and(|value| {
                            value.ty
                                == super::types::ordinary_map_owned_any_v2_root_result_type_id()
                        })
                    })
            });
        let set_get_any_result_i32_set_bridge =
            set_get_any_result_i32_receipt.is_some_and(|receipt| {
                let expected_result =
                    MirValueId::new(format!("local:{}", receipt.set_result_local.0 .0));
                let root = MirValueId::new(format!("local:{}", receipt.initial_root_local.0 .0));
                let key = MirValueId::new(format!("literal:{}", receipt.set_key.0));
                let value = MirValueId::new(format!("literal:{}", receipt.set_value.0));
                let expected_instruction = super::MirInstructionId::new(format!(
                    "inst:ordinary_map_set_get_any_result_i32_lineage:set_result:{}",
                    receipt.set_operation.0
                ));
                matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == "map_set")
                    && call.call_node == receipt.set_operation
                    && call.result_type == receipt.set_result_type
                    && expected_instruction.is_ok_and(|instruction| instruction == call.instruction)
                    && expected_result.as_ref().is_ok_and(|result| call.result.as_ref() == Some(result))
                    && call.arguments.len() == 3
                    && root.as_ref().is_ok_and(|root| call.arguments[0].value == *root)
                    && call.arguments[0].expression.origin == receipt.set_receiver
                    && call.arguments[0].expression.ty == receipt.initial_root_type
                    && key.as_ref().is_ok_and(|key| call.arguments[1].value == *key)
                    && value.as_ref().is_ok_and(|value| call.arguments[2].value == *value)
            });
        let set_get_any_result_i32_get_bridge =
            set_get_any_result_i32_receipt.is_some_and(|receipt| {
                let expected_result =
                    MirValueId::new(format!("local:{}", receipt.get_result_local.0 .0));
                let root = MirValueId::new(format!("local:{}", receipt.updated_root_local.0 .0));
                let key = MirValueId::new(format!("literal:{}", receipt.get_key.0));
                let expected_instruction = super::MirInstructionId::new(format!(
                    "inst:ordinary_map_set_get_any_result_i32_lineage:get_any_result:{}",
                    receipt.get_operation.0
                ));
                matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == "map_get_any")
                    && call.call_node == receipt.get_operation
                    && call.result_type == receipt.get_result_type
                    && expected_instruction.is_ok_and(|instruction| instruction == call.instruction)
                    && expected_result.as_ref().is_ok_and(|result| call.result.as_ref() == Some(result))
                    && call.arguments.len() == 2
                    && root.as_ref().is_ok_and(|root| call.arguments[0].value == *root)
                    && call.arguments[0].expression.origin == receipt.get_receiver
                    && call.arguments[0].expression.ty == receipt.updated_root_type
                    && key.as_ref().is_ok_and(|key| call.arguments[1].value == *key)
            });
        let expected_instruction = match &call.callee {
            ResolvedCallee::Builtin(builtin)
                if matches!(builtin.as_str(), "Some" | "None" | "Ok" | "Err") =>
            {
                ["construct_variant", "construct_variant_move"]
                    .iter()
                    .filter_map(|role| {
                        super::MirInstructionId::new(format!("inst:{role}:{}", call.call_node.0))
                            .ok()
                    })
                    .any(|id| id == call.instruction)
            }
            ResolvedCallee::Builtin(builtin)
                if builtin.as_str() == "map_new" && new_result_new_bridge =>
            {
                true
            }
            ResolvedCallee::Builtin(builtin)
                if builtin.as_str() == "map_new"
                    && values_local_index_i32_result_branch_new_bridge =>
            {
                true
            }
            ResolvedCallee::Builtin(builtin)
                if builtin.as_str() == "map_size" && new_result_size_bridge =>
            {
                true
            }
            ResolvedCallee::Builtin(builtin)
                if (builtin.as_str() == "map_new" && set_result_new_bridge)
                    || (builtin.as_str() == "map_set" && set_result_set_bridge)
                    || (builtin.as_str() == "map_size" && set_result_size_bridge) =>
            {
                true
            }
            ResolvedCallee::Builtin(builtin)
                if (builtin.as_str() == "map_new" && set_get_i32_new_bridge)
                    || (builtin.as_str() == "map_set" && set_get_i32_set_bridge)
                    || (builtin.as_str() == "map_get_i32" && set_get_i32_get_bridge) =>
            {
                true
            }
            ResolvedCallee::Builtin(builtin)
                if (builtin.as_str() == "map_new" && set_get_string_new_bridge)
                    || (builtin.as_str() == "map_set" && set_get_string_set_bridge)
                    || (builtin.as_str() == "map_get_string" && set_get_string_get_bridge) =>
            {
                true
            }
            ResolvedCallee::Builtin(builtin)
                if (builtin.as_str() == "map_new" && set_get_any_i32_new_bridge)
                    || (builtin.as_str() == "map_set" && set_get_any_i32_set_bridge)
                    || (builtin.as_str() == "map_get" && set_get_any_i32_get_bridge) =>
            {
                true
            }
            ResolvedCallee::Builtin(builtin)
                if (builtin.as_str() == "map_new" && set_get_any_result_i32_new_bridge)
                    || (builtin.as_str() == "map_set" && set_get_any_result_i32_set_bridge)
                    || (builtin.as_str() == "map_get_any" && set_get_any_result_i32_get_bridge) =>
            {
                true
            }
            ResolvedCallee::Builtin(_) => {
                super::MirInstructionId::new(format!("inst:builtin_call:{}", call.call_node.0))
                    .is_ok_and(|id| id == call.instruction)
            }
            ResolvedCallee::Transition(_) => {
                super::MirInstructionId::new(format!("inst:flow_transition:{}", call.call_node.0))
                    .is_ok_and(|id| id == call.instruction)
            }
            ResolvedCallee::Constructor(_) => ["construct_variant", "construct_variant_move"]
                .iter()
                .filter_map(|role| {
                    super::MirInstructionId::new(format!("inst:{role}:{}", call.call_node.0)).ok()
                })
                .any(|id| id == call.instruction),
            _ => super::MirInstructionId::new(format!("inst:call:{}", call.call_node.0))
                .is_ok_and(|id| id == call.instruction),
        };
        if call.body_owner != function.owner
            || call.owner != function.owner
            || call.scope.as_slice() != expected_scope
            || !expected_instruction
            || call.effect_authority != TypedCallEffectAuthority::UnknownMayReadWrite
            || call.havoc_locals.windows(2).any(|pair| pair[0] >= pair[1])
        {
            errors.push(MirValidationError {
                subject: call.call_node.0.clone(),
                message: "call proof point has an invalid owner, scope, selected operation identity, or conservative effect/havoc seal".into(),
            });
        }
        for guard in &call.guards {
            validate_expression(&guard.condition, function, catalog, true, errors);
        }
        for (index, argument) in call.arguments.iter().enumerate() {
            let new_result_size_argument =
                (new_result_size_bridge || set_result_set_bridge || set_result_size_bridge)
                    && index == 0;
            let source_ty = argument.expression.expression_types.get(&Vec::new());
            if source_ty != Some(&argument.conversion.from)
                || (!new_result_size_argument
                    && function
                        .values
                        .get(&argument.value)
                        .is_none_or(|value| value.ty != argument.conversion.to))
            {
                errors.push(MirValidationError {
                    subject: call.call_node.0.clone(),
                    message: "call proof argument TypeDesc/conversion receipt disagrees with its MIR actual".into(),
                });
            }
            if !new_result_size_argument {
                validate_expression(&argument.expression, function, catalog, false, errors);
            }
        }
        let expected_arguments = call
            .arguments
            .iter()
            .map(|argument| argument.value.clone())
            .collect::<Vec<_>>();
        let matches = function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| instruction.id == call.instruction)
            .filter(|instruction| match (&call.callee, &instruction.kind) {
                (
                    callee,
                    super::MirInstructionKind::Call {
                        result,
                        callee: actual_callee,
                        type_arguments,
                        arguments,
                        ..
                    },
                ) => {
                    actual_callee == callee
                        && type_arguments == &call.type_arguments
                        && result == &call.result
                        && arguments == &expected_arguments
                }
                (
                    ResolvedCallee::Builtin(builtin),
                    super::MirInstructionKind::BuiltinCall {
                        result,
                        kind,
                        arguments,
                        ..
                    },
                ) => {
                    let expected_kind = if builtin.as_str() == "println" {
                        expected_arguments.first().and_then(|argument| {
                            function
                                .values
                                .get(argument)
                                .and_then(|value| catalog.get(&value.ty))
                                .and_then(|descriptor| {
                                    super::types::MirBuiltinContract::from_builtin_with_abi(
                                        builtin,
                                        descriptor.abi,
                                    )
                                })
                                .map(|contract| contract.kind)
                        })
                    } else {
                        super::types::MirBuiltinContract::from_builtin(builtin)
                            .map(|contract| contract.kind)
                    };
                    call.result.as_ref() == Some(result)
                        && arguments == &expected_arguments
                        && expected_kind == Some(*kind)
                }
                (
                    ResolvedCallee::Transition(transition),
                    super::MirInstructionKind::FlowTransition {
                        result,
                        transition: actual_transition,
                        arguments,
                        ..
                    },
                ) => {
                    call.result.as_ref() == Some(result)
                        && actual_transition == &super::transition_owner_from_id(transition)
                        && arguments == &expected_arguments
                }
                (
                    ResolvedCallee::Builtin(builtin),
                    super::MirInstructionKind::ConstructVariant {
                        result,
                        variant,
                        fields,
                        ..
                    }
                    | super::MirInstructionKind::ConstructVariantMove {
                        result,
                        variant,
                        fields,
                        ..
                    },
                ) => {
                    let expected_variant = match builtin.as_str() {
                        "Some" => Some("builtin:variant:Option::Some"),
                        "None" => Some("builtin:variant:Option::None"),
                        "Ok" => Some("builtin:variant:Result::Ok"),
                        "Err" => Some("builtin:variant:Result::Err"),
                        _ => None,
                    };
                    expected_variant.is_some_and(|expected| variant.0 == expected)
                        && call.result.as_ref() == Some(result)
                        && fields
                            .iter()
                            .map(|(_, value)| value)
                            .eq(expected_arguments.iter())
                }
                (
                    ResolvedCallee::Constructor(expected_variant),
                    super::MirInstructionKind::ConstructVariant {
                        result,
                        variant,
                        fields,
                        ..
                    }
                    | super::MirInstructionKind::ConstructVariantMove {
                        result,
                        variant,
                        fields,
                        ..
                    },
                ) => {
                    variant == expected_variant
                        && call.result.as_ref() == Some(result)
                        && fields
                            .iter()
                            .map(|(_, value)| value)
                            .eq(expected_arguments.iter())
                }
                (
                    ResolvedCallee::Builtin(builtin),
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2New { result },
                ) if builtin.as_str() == "map_new" && new_result_new_bridge => {
                    call.result.as_ref() == Some(result) && expected_arguments.is_empty()
                }
                (
                    ResolvedCallee::Builtin(builtin),
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2Size { result, root },
                ) if builtin.as_str() == "map_size" && new_result_size_bridge => {
                    call.result.as_ref() == Some(result)
                        && expected_arguments.as_slice() == std::slice::from_ref(root)
                }
                (
                    ResolvedCallee::Builtin(builtin),
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2New { result },
                ) if builtin.as_str() == "map_new" && set_result_new_bridge => {
                    call.result.as_ref() == Some(result) && expected_arguments.is_empty()
                }
                (
                    ResolvedCallee::Builtin(builtin),
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult {
                        result,
                        source,
                        key,
                        value,
                    },
                ) if builtin.as_str() == "map_set" && set_result_set_bridge => {
                    call.result.as_ref() == Some(result)
                        && expected_arguments.as_slice()
                            == [source.clone(), key.clone(), value.clone()]
                }
                (
                    ResolvedCallee::Builtin(builtin),
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2Size { result, root },
                ) if builtin.as_str() == "map_size" && set_result_size_bridge => {
                    call.result.as_ref() == Some(result)
                        && expected_arguments.as_slice() == std::slice::from_ref(root)
                }
                (
                    ResolvedCallee::Builtin(builtin),
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2New { result },
                ) if builtin.as_str() == "map_new" && set_get_i32_new_bridge => {
                    call.result.as_ref() == Some(result) && expected_arguments.is_empty()
                }
                (
                    ResolvedCallee::Builtin(builtin),
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult {
                        result,
                        source,
                        key,
                        value,
                    },
                ) if builtin.as_str() == "map_set" && set_get_i32_set_bridge => {
                    call.result.as_ref() == Some(result)
                        && expected_arguments.as_slice()
                            == [source.clone(), key.clone(), value.clone()]
                }
                (
                    ResolvedCallee::Builtin(builtin),
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2GetI32Result {
                        result,
                        root,
                        key,
                    },
                ) if builtin.as_str() == "map_get_i32" && set_get_i32_get_bridge => {
                    call.result.as_ref() == Some(result)
                        && expected_arguments.as_slice() == [root.clone(), key.clone()]
                }
                (
                    ResolvedCallee::Builtin(builtin),
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2New { result },
                ) if builtin.as_str() == "map_new" && set_get_string_new_bridge => {
                    call.result.as_ref() == Some(result) && expected_arguments.is_empty()
                }
                (
                    ResolvedCallee::Builtin(builtin),
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2SetStringResult {
                        result,
                        source,
                        key,
                        value,
                    },
                ) if builtin.as_str() == "map_set" && set_get_string_set_bridge => {
                    call.result.as_ref() == Some(result)
                        && expected_arguments.as_slice()
                            == [source.clone(), key.clone(), value.clone()]
                }
                (
                    ResolvedCallee::Builtin(builtin),
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2GetStringResult {
                        result,
                        root,
                        key,
                    },
                ) if builtin.as_str() == "map_get_string" && set_get_string_get_bridge => {
                    call.result.as_ref() == Some(result)
                        && expected_arguments.as_slice() == [root.clone(), key.clone()]
                }
                (
                    ResolvedCallee::Builtin(builtin),
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2New { result },
                ) if builtin.as_str() == "map_new" && set_get_any_i32_new_bridge => {
                    call.result.as_ref() == Some(result) && expected_arguments.is_empty()
                }
                (
                    ResolvedCallee::Builtin(builtin),
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult {
                        result,
                        source,
                        key,
                        value,
                    },
                ) if builtin.as_str() == "map_set" && set_get_any_i32_set_bridge => {
                    call.result.as_ref() == Some(result)
                        && expected_arguments.as_slice()
                            == [source.clone(), key.clone(), value.clone()]
                }
                (
                    ResolvedCallee::Builtin(builtin),
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { result, root, key },
                ) if builtin.as_str() == "map_get" && set_get_any_i32_get_bridge => {
                    call.result.as_ref() == Some(result)
                        && expected_arguments.as_slice() == [root.clone(), key.clone()]
                }
                (
                    ResolvedCallee::Builtin(builtin),
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2New { result },
                ) if builtin.as_str() == "map_new" && set_get_any_result_i32_new_bridge => {
                    call.result.as_ref() == Some(result) && expected_arguments.is_empty()
                }
                (
                    ResolvedCallee::Builtin(builtin),
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2New { result },
                ) if builtin.as_str() == "map_new"
                    && values_local_index_i32_result_branch_new_bridge =>
                {
                    call.result.as_ref() == Some(result) && expected_arguments.is_empty()
                }
                (
                    ResolvedCallee::Builtin(builtin),
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult {
                        result,
                        source,
                        key,
                        value,
                    },
                ) if builtin.as_str() == "map_set" && set_get_any_result_i32_set_bridge => {
                    call.result.as_ref() == Some(result)
                        && expected_arguments.as_slice()
                            == [source.clone(), key.clone(), value.clone()]
                }
                (
                    ResolvedCallee::Builtin(builtin),
                    super::MirInstructionKind::OrdinaryMapOwnedAnyV2GetAnyResult {
                        result,
                        root,
                        key,
                    },
                ) if builtin.as_str() == "map_get_any" && set_get_any_result_i32_get_bridge => {
                    call.result.as_ref() == Some(result)
                        && expected_arguments.as_slice() == [root.clone(), key.clone()]
                }
                _ => false,
            })
            .count();
        if !new_result_new_bridge
            && !set_result_new_bridge
            && !set_result_set_bridge
            && !set_get_i32_new_bridge
            && !set_get_string_new_bridge
            && !set_get_any_i32_new_bridge
            && !set_get_any_i32_set_bridge
            && !set_get_any_i32_get_bridge
            && !set_get_any_result_i32_new_bridge
            && !set_get_any_result_i32_set_bridge
            && !set_get_any_result_i32_get_bridge
            && !values_local_index_i32_result_branch_new_bridge
            && call.result.as_ref().is_some_and(|result| {
                function
                    .values
                    .get(result)
                    .is_none_or(|value| value.ty != call.result_type)
            })
            || (call.result.is_none()
                && catalog
                    .get(&call.result_type)
                    .is_none_or(|descriptor| descriptor.abi != MirAbiClass::Unit))
        {
            errors.push(MirValidationError {
                subject: call.call_node.0.clone(),
                message: "call proof result TypeDesc disagrees with its selected MIR operation"
                    .into(),
            });
        }
        if matches != 1 {
            errors.push(MirValidationError {
                subject: call.call_node.0.clone(),
                message: "call proof event is not bound to one exact canonical MIR call, builtin, Flow-transition, or constructor instruction".into(),
            });
        }
    }

    fn validate_loop_receipt(
        receipt: &MirProofLoopReceipt,
        point: &NodeId,
        header: &super::MirBlockId,
        body_entry: &super::MirBlockId,
        exit: &super::MirBlockId,
        function: &MirFunction,
    ) -> bool {
        use crate::core::cfg::{EdgeId, EdgeKind};
        use crate::core::ir::TypedLoopEdgeKind;

        let typed = &receipt.typed;
        let ffi_call_binding_valid = typed
            .ffi_call_path
            .as_ref()
            .map(|profile| mir_loop_ffi_call_block_matches(function, receipt, profile, exit))
            .unwrap_or(receipt.ffi_call_block.is_none());
        if !typed_loop_ffi_profile_matches_source(typed, point)
            || !ffi_call_binding_valid
            || typed.owner != function.owner
            || receipt.edges.len() != typed.body_edges.len() + 3
            || receipt.bindings.len() != typed.state_locals.len()
        {
            return false;
        }

        let cfg_edge_id = |role: &str| EdgeId(NodeId(format!("{}/cfg:edge:{role}", point.0)));
        let cfg_entry_id = cfg_edge_id("while-enter");
        let cfg_body_id = cfg_edge_id("while-body");
        let cfg_exit_id = cfg_edge_id("while-exit");
        let Some(cfg_entry) = typed
            .cfg_edges
            .iter()
            .find(|edge| edge.id == cfg_entry_id && edge.kind == EdgeKind::Fallthrough)
        else {
            return false;
        };
        let Some(cfg_body) = typed
            .cfg_edges
            .iter()
            .find(|edge| edge.id == cfg_body_id && edge.kind == EdgeKind::LoopBody)
        else {
            return false;
        };
        let Some(cfg_exit) = typed
            .cfg_edges
            .iter()
            .find(|edge| edge.id == cfg_exit_id && edge.kind == EdgeKind::LoopExit)
        else {
            return false;
        };
        if cfg_entry.to != cfg_body.from || cfg_body.from != cfg_exit.from {
            return false;
        }

        let edge_records = mir_edge_records(function);
        let exact_mir_edge =
            |role: &str| super::MirEdgeId::new(format!("edge:{role}:{}", point.0)).ok();
        let Some(entry_id) = exact_mir_edge("while.entry") else {
            return false;
        };
        let Some(body_id) = exact_mir_edge("while.body") else {
            return false;
        };
        let Some(exit_id) = exact_mir_edge("while.exit") else {
            return false;
        };
        let find_core = |kind: MirProofLoopEdgeKind| {
            let matching = receipt
                .edges
                .iter()
                .filter(|edge| edge.kind == kind)
                .collect::<Vec<_>>();
            if matching.len() == 1 {
                Some(matching[0])
            } else {
                None
            }
        };
        let (Some(entry), Some(condition_true), Some(condition_false)) = (
            find_core(MirProofLoopEdgeKind::Entry),
            find_core(MirProofLoopEdgeKind::ConditionTrue),
            find_core(MirProofLoopEdgeKind::ConditionFalse),
        ) else {
            return false;
        };
        if receipt
            .edges
            .iter()
            .filter(|edge| {
                matches!(
                    edge.kind,
                    MirProofLoopEdgeKind::Entry
                        | MirProofLoopEdgeKind::ConditionTrue
                        | MirProofLoopEdgeKind::ConditionFalse
                )
            })
            .count()
            != 3
            || entry.source_site != *point
            || entry.kind != MirProofLoopEdgeKind::Entry
            || entry.source_cfg_edge.as_ref() != Some(&cfg_entry_id)
            || entry.source_reachable != typed.cfg_reachable_blocks.contains(&cfg_entry.to)
            || entry.mir_edge.as_ref() != Some(&entry_id)
            || entry.to_block.as_ref() != Some(header)
            || condition_true.source_site != *point
            || condition_true.source_cfg_edge.as_ref() != Some(&cfg_body_id)
            || condition_true.source_reachable
                != typed.cfg_reachable_blocks.contains(&cfg_body.from)
            || condition_true.mir_edge.as_ref() != Some(&body_id)
            || &condition_true.from_block != header
            || condition_true.to_block.as_ref() != Some(body_entry)
            || condition_false.source_site != *point
            || condition_false.source_cfg_edge.as_ref() != Some(&cfg_exit_id)
            || condition_false.source_reachable
                != typed.cfg_reachable_blocks.contains(&cfg_exit.from)
            || condition_false.mir_edge.as_ref() != Some(&exit_id)
            || &condition_false.from_block != header
            || condition_false.to_block.as_ref() != Some(exit)
        {
            return false;
        }

        let Some(header_block) = function.blocks.get(header) else {
            return false;
        };
        let Some(body_block) = function.blocks.get(body_entry) else {
            return false;
        };
        let Some(exit_block) = function.blocks.get(exit) else {
            return false;
        };
        if header_block.parameters.len() != receipt.bindings.len()
            || !body_block.parameters.is_empty()
            || exit_block.parameters.len() != receipt.bindings.len()
        {
            return false;
        }
        let mut header_values = Vec::with_capacity(receipt.bindings.len());
        let mut preheader_values = Vec::with_capacity(receipt.bindings.len());
        for (index, (binding, local)) in
            receipt.bindings.iter().zip(&typed.state_locals).enumerate()
        {
            let expected_header = format!("loop.header.local:{}:{}", point.0, local.local.0 .0);
            let expected_exit = format!("loop.exit.local:{}:{}", point.0, local.local.0 .0);
            if binding.local != local.local
                || binding.ty != local.ty
                || binding.header_parameter.as_str() != expected_header
                || binding.exit_parameter.as_str() != expected_exit
                || (index > 0 && typed.state_locals[index - 1].local >= local.local)
                || header_block.parameters[index].value != binding.header_parameter
                || exit_block.parameters[index].value != binding.exit_parameter
                || function
                    .values
                    .get(&binding.preheader_value)
                    .is_none_or(|value| value.ty != binding.ty)
                || function
                    .values
                    .get(&binding.header_parameter)
                    .is_none_or(|value| value.ty != binding.ty)
                || function
                    .values
                    .get(&binding.exit_parameter)
                    .is_none_or(|value| value.ty != binding.ty)
            {
                return false;
            }
            header_values.push(binding.header_parameter.clone());
            preheader_values.push(binding.preheader_value.clone());
        }
        if entry.arguments != preheader_values
            || !condition_true.arguments.is_empty()
            || condition_false.arguments != header_values
            || ![entry, condition_true, condition_false]
                .iter()
                .all(|edge| validate_loop_mir_edge(edge, function))
        {
            return false;
        }

        let mut typed_paths = BTreeMap::new();
        let mir_reachable_from = |start: &super::MirBlockId, target: &super::MirBlockId| {
            let mut pending = VecDeque::from([start.clone()]);
            let mut seen = BTreeSet::new();
            while let Some(block) = pending.pop_front() {
                if &block == target {
                    return true;
                }
                if !seen.insert(block.clone()) {
                    continue;
                }
                pending.extend(
                    edge_records
                        .iter()
                        .filter(|edge| edge.1 == block)
                        .map(|edge| edge.2.clone()),
                );
            }
            false
        };
        let guarded_path_start = |guards: &[crate::core::ir::TypedLoopBranchGuard]| {
            let mut start = body_entry.clone();
            for guard in guards {
                let Ok(condition) = MirValueId::new(format!("expr:{}", guard.condition.0)) else {
                    return None;
                };
                let branches = function
                    .blocks
                    .values()
                    .filter_map(|block| match &block.terminator {
                        super::MirTerminator::Branch {
                            condition: actual,
                            then_target,
                            else_target,
                            ..
                        } if actual == &condition => Some((
                            block.id.clone(),
                            if guard.then_arm {
                                then_target.clone()
                            } else {
                                else_target.clone()
                            },
                        )),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let [(branch, next)] = branches.as_slice() else {
                    return None;
                };
                if !mir_reachable_from(&start, branch) {
                    return None;
                }
                start = next.clone();
            }
            Some(start)
        };
        for source in &typed.body_edges {
            let expected_target_loop = matches!(
                source.kind,
                TypedLoopEdgeKind::Break | TypedLoopEdgeKind::Continue
            )
            .then(|| point.clone());
            if source.target_loop != expected_target_loop {
                return false;
            }
            if !typed
                .cfg_blocks
                .iter()
                .any(|block| block.id == source.cfg_terminal_block)
                || source.reachable
                    != typed
                        .cfg_reachable_blocks
                        .contains(&source.cfg_terminal_block)
            {
                return false;
            }
            *typed_paths
                .entry((
                    source.site.clone(),
                    MirProofLoopEdgeKind::from_typed(source.kind),
                    source.cfg_terminal_edge.clone(),
                    source.reachable,
                ))
                .or_insert(0usize) += 1;
            let Some(path_start) = guarded_path_start(&source.guards) else {
                return false;
            };
            let path_edges = receipt
                .edges
                .iter()
                .filter(|edge| {
                    edge.source_site == source.site
                        && edge.kind == MirProofLoopEdgeKind::from_typed(source.kind)
                        && edge.source_cfg_edge == source.cfg_terminal_edge
                        && edge.source_reachable == source.reachable
                })
                .collect::<Vec<_>>();
            if path_edges.is_empty()
                || path_edges
                    .iter()
                    .any(|edge| !mir_reachable_from(&path_start, &edge.from_block))
            {
                return false;
            }
            match source.kind {
                TypedLoopEdgeKind::BodyFallthrough
                | TypedLoopEdgeKind::Continue
                | TypedLoopEdgeKind::Break => {
                    let (expected_role, expected_kind, expected_target) = match source.kind {
                        TypedLoopEdgeKind::BodyFallthrough => (
                            format!("{}/cfg:edge:while-backedge", point.0),
                            EdgeKind::Backedge,
                            &cfg_entry.to,
                        ),
                        TypedLoopEdgeKind::Continue => (
                            format!("{}/cfg:edge:continue", source.site.0),
                            EdgeKind::Continue,
                            &cfg_entry.to,
                        ),
                        TypedLoopEdgeKind::Break => (
                            format!("{}/cfg:edge:break", source.site.0),
                            EdgeKind::Break,
                            &cfg_exit.to,
                        ),
                        TypedLoopEdgeKind::Return => return false,
                    };
                    let Some(cfg_terminal_edge) = typed.cfg_edges.iter().find(|edge| {
                        edge.id.0 .0 == expected_role
                            && edge.kind == expected_kind
                            && edge.from == source.cfg_terminal_block
                            && &edge.to == expected_target
                    }) else {
                        return false;
                    };
                    if source.cfg_terminal_edge.as_ref() != Some(&cfg_terminal_edge.id) {
                        return false;
                    }
                }
                TypedLoopEdgeKind::Return => {
                    let Some(block) = typed
                        .cfg_blocks
                        .iter()
                        .find(|block| block.id == source.cfg_terminal_block)
                    else {
                        return false;
                    };
                    if source.cfg_terminal_edge.is_some()
                        || !block
                            .points
                            .iter()
                            .any(|point| point.source.node == source.site)
                        || !matches!(
                            &block.terminator,
                            crate::core::cfg::Terminator::Return { value, implicit: false }
                                if value == &source.terminal_value
                        )
                    {
                        return false;
                    }
                }
            }
        }
        let mut mir_paths = BTreeMap::new();
        for edge in &receipt.edges {
            if matches!(
                edge.kind,
                MirProofLoopEdgeKind::Entry
                    | MirProofLoopEdgeKind::ConditionTrue
                    | MirProofLoopEdgeKind::ConditionFalse
            ) {
                continue;
            }
            *mir_paths
                .entry((
                    edge.source_site.clone(),
                    edge.kind,
                    edge.source_cfg_edge.clone(),
                    edge.source_reachable,
                ))
                .or_insert(0usize) += 1;
            match edge.kind {
                MirProofLoopEdgeKind::BodyFallthrough
                | MirProofLoopEdgeKind::Continue
                | MirProofLoopEdgeKind::Break => {
                    let (role, target) = match edge.kind {
                        MirProofLoopEdgeKind::BodyFallthrough => ("while.back".to_string(), header),
                        MirProofLoopEdgeKind::Continue => ("while.continue".to_string(), header),
                        MirProofLoopEdgeKind::Break => ("while.break".to_string(), exit),
                        _ => return false,
                    };
                    let expected_id = if edge.kind == MirProofLoopEdgeKind::BodyFallthrough {
                        exact_mir_edge(&role)
                    } else {
                        super::MirEdgeId::new(format!("edge:{role}:{}", edge.source_site.0)).ok()
                    };
                    if expected_id.as_ref() != edge.mir_edge.as_ref()
                        || edge.to_block.as_ref() != Some(target)
                        || edge.arguments.len() != receipt.bindings.len()
                        || !validate_loop_mir_edge(edge, function)
                    {
                        return false;
                    }
                }
                MirProofLoopEdgeKind::Return => {
                    let expected_value = typed
                        .body_edges
                        .iter()
                        .find(|source| {
                            source.site == edge.source_site
                                && source.kind == TypedLoopEdgeKind::Return
                                && source.reachable == edge.source_reachable
                        })
                        .and_then(|source| source.terminal_value.as_ref())
                        .and_then(|node| MirValueId::new(format!("expr:{}", node.0)).ok());
                    if edge.mir_edge.is_some()
                        || edge.to_block.is_some()
                        || !edge.arguments.is_empty()
                        || edge.terminator_value != expected_value
                        || !function.blocks.get(&edge.from_block).is_some_and(|block| {
                            matches!(
                                &block.terminator,
                                super::MirTerminator::Return { value }
                                    if value == &edge.terminator_value
                            )
                        })
                    {
                        return false;
                    }
                }
                MirProofLoopEdgeKind::Entry
                | MirProofLoopEdgeKind::ConditionTrue
                | MirProofLoopEdgeKind::ConditionFalse => return false,
            }
        }
        if typed_paths != mir_paths {
            return false;
        }

        let actual_incoming = |target: &super::MirBlockId| {
            edge_records
                .iter()
                .filter(|record| &record.2 == target)
                .map(|record| record.0.clone())
                .collect::<BTreeSet<_>>()
        };
        let receipted_incoming = |target: &super::MirBlockId| {
            receipt
                .edges
                .iter()
                .filter(|edge| edge.to_block.as_ref() == Some(target))
                .filter_map(|edge| edge.mir_edge.clone())
                .collect::<BTreeSet<_>>()
        };
        if actual_incoming(header) != receipted_incoming(header)
            || actual_incoming(body_entry) != receipted_incoming(body_entry)
            || actual_incoming(exit) != receipted_incoming(exit)
        {
            return false;
        }

        if typed
            .resource_actions
            .iter()
            .any(|action| action.kind != crate::core::ownership::CanonicalActionKind::Read)
        {
            return false;
        }
        let expected_resource_edges = typed
            .cfg_edges
            .iter()
            .map(|edge| edge.id.clone())
            .collect::<BTreeSet<_>>();
        let actual_resource_edges = typed
            .resource_edges
            .iter()
            .map(|edge| edge.edge.clone())
            .collect::<BTreeSet<_>>();
        let expected_resource_blocks = typed
            .cfg_blocks
            .iter()
            .filter(|block| block.id != cfg_exit.to)
            .map(|block| block.id.clone())
            .collect::<BTreeSet<_>>();
        let actual_resource_blocks = typed
            .resource_blocks
            .iter()
            .map(|block| block.block.clone())
            .collect::<BTreeSet<_>>();
        let resource_edges_valid = typed.resource_edges.iter().all(|state| {
            let Some(edge) = typed.cfg_edges.iter().find(|edge| edge.id == state.edge) else {
                return false;
            };
            state.from_output == state.to_input
                && (!typed.cfg_reachable_blocks.contains(&edge.from)
                    || (state.from_input.is_some() && state.from_output.is_some()))
                && (!typed.cfg_reachable_blocks.contains(&edge.to) || state.to_input.is_some())
        });
        expected_resource_edges == actual_resource_edges
            && expected_resource_blocks == actual_resource_blocks
            && resource_edges_valid
            && typed.resource_blocks.iter().all(|block| {
                !typed.cfg_reachable_blocks.contains(&block.block)
                    || (block.input.is_some()
                        && block.output.is_some()
                        && block.input == block.output)
            })
    }

    fn validate_block(
        block: &MirProofBlock,
        function: &MirFunction,
        catalog: &MirTypeCatalog,
        math: &mut Vec<MirProofPointId>,
        invariants: &mut Vec<MirProofPointId>,
        calls: &mut Vec<MirProofCallPointId>,
        event_orders: &mut Vec<u64>,
        seen: &mut BTreeSet<MirProofPointId>,
        scope: &mut Vec<MirProofScopeFrame>,
        errors: &mut Vec<MirValidationError>,
    ) {
        scope.push(MirProofScopeFrame::Block(block.id.clone()));
        for operation in &block.operations {
            match operation {
                MirProofOperation::Bind {
                    point,
                    local_identity,
                    local,
                    initializer,
                } => {
                    let expected = format!("local:{}", local_identity.0);
                    if local.as_str() != expected
                        || !function.values.contains_key(local)
                        || function.values.get(local).is_none_or(|value| {
                            initializer
                                .expression_types
                                .get(&Vec::new())
                                .is_none_or(|ty| value.ty != *ty)
                        })
                    {
                        errors.push(MirValidationError {
                            subject: point.0.clone(),
                            message:
                                "proof binding target is not a type-matched canonical local value"
                                    .into(),
                        });
                    }
                    validate_expression(initializer, function, catalog, false, errors);
                }
                MirProofOperation::Call(call) => {
                    calls.push(MirProofCallPointId {
                        owner: call.owner.clone(),
                        statement: call.statement.clone(),
                        call_node: call.call_node.clone(),
                        order: call.order,
                    });
                    event_orders.push(call.order);
                    validate_call_point(call, scope, function, catalog, errors);
                }
                MirProofOperation::Eval { point, expression } => {
                    validate_expression(expression, function, catalog, false, errors);
                    let expression_value =
                        MirValueId::new(format!("expr:{}", expression.origin.0)).ok();
                    if expression_value
                        .as_ref()
                        .is_none_or(|value| !function.values.contains_key(value))
                    {
                        errors.push(MirValidationError {
                            subject: point.0.clone(),
                            message: "proof Eval has no corresponding canonical MIR value".into(),
                        });
                    }
                }
                MirProofOperation::Assign {
                    point,
                    target,
                    value,
                    conversion,
                    result,
                    conversion_result,
                } => {
                    validate_expression(value, function, catalog, false, errors);
                    let move_id = super::MirInstructionId::new(format!("inst:assign:{}", point.0));
                    let source = conversion_result
                        .clone()
                        .or_else(|| MirValueId::new(format!("expr:{}", value.origin.0)).ok());
                    let move_matches = move_id.ok().is_some_and(|move_id| {
                        source.as_ref().is_some_and(|source| {
                            function
                                .blocks
                                .values()
                                .flat_map(|block| block.instructions.iter())
                                .filter(|instruction| instruction.id == move_id)
                                .filter(|instruction| {
                                    matches!(
                                        &instruction.kind,
                                        super::MirInstructionKind::Move {
                                            result: actual_result,
                                            source: actual_source,
                                        } if actual_result == result && actual_source == source
                                    )
                                })
                                .count()
                                == 1
                        })
                    });
                    let types_match = value
                        .expression_types
                        .get(&Vec::new())
                        .is_some_and(|ty| ty == &conversion.from)
                        && function
                            .values
                            .get(result)
                            .is_some_and(|actual| actual.ty == conversion.to);
                    if !target.projections.is_empty() || !move_matches || !types_match {
                        errors.push(MirValidationError {
                            subject: point.0.clone(),
                            message: "proof Assign does not bind an exact direct-local Move and checked TypeDesc conversion".into(),
                        });
                    }
                    match conversion.kind {
                        crate::core::ir::CheckedConversionKind::Identity => {
                            if conversion_result.is_some() || conversion.from != conversion.to {
                                errors.push(MirValidationError {
                                    subject: point.0.clone(),
                                    message: "identity Assign carries an inconsistent conversion result/type".into(),
                                });
                            }
                        }
                        crate::core::ir::CheckedConversionKind::NumericWiden => {
                            let conversion_node = NodeId(format!("{}/assign-conversion", point.0));
                            let expected_result =
                                MirValueId::new(format!("assign.convert:{}", conversion_node.0));
                            let expected_instruction = super::MirInstructionId::new(format!(
                                "inst:assign_numeric_convert:{}",
                                conversion_node.0
                            ));
                            let valid = conversion_result.as_ref() == expected_result.as_ref().ok()
                                && expected_instruction.ok().is_some_and(|expected_instruction| {
                                    function.blocks.values().flat_map(|block| block.instructions.iter())
                                        .filter(|instruction| instruction.id == expected_instruction)
                                        .filter(|instruction| {
                                            matches!(
                                                &instruction.kind,
                                                super::MirInstructionKind::Convert { result, source }
                                                    if Some(result) == conversion_result.as_ref()
                                                        && source.as_str() == format!("expr:{}", value.origin.0)
                                            )
                                        })
                                        .count() == 1
                                });
                            if !valid {
                                errors.push(MirValidationError {
                                    subject: point.0.clone(),
                                    message: "NumericWiden Assign has no exact source/result Convert receipt".into(),
                                });
                            }
                        }
                        _ => errors.push(MirValidationError {
                            subject: point.0.clone(),
                            message: "proof Assign uses an unsupported conversion kind".into(),
                        }),
                    }
                }
                MirProofOperation::Math(assertion) => {
                    if assertion.scope.as_slice() != scope.as_slice() {
                        errors.push(MirValidationError {
                            subject: assertion.id.statement.0.clone(),
                            message: "Math proof-point lexical scope disagrees with its typed worklist path".into(),
                        });
                    }
                    if !seen.insert(assertion.id.clone()) {
                        errors.push(MirValidationError {
                            subject: assertion.id.statement.0.clone(),
                            message: "proof worklist repeats a Math point identity".into(),
                        });
                    }
                    math.push(assertion.id.clone());
                    event_orders.push(assertion.order);
                    validate_expression(&assertion.expression, function, catalog, true, errors);
                }
                MirProofOperation::Branch {
                    point,
                    expression,
                    branch_block,
                    then_target,
                    else_target,
                    condition_value,
                    condition,
                    then_block,
                    else_block,
                } => {
                    let value_type = function.values.get(condition_value).map(|value| &value.ty);
                    if condition_value.as_str() != format!("expr:{}", condition.origin.0)
                        || value_type
                            .and_then(|ty| catalog.get(ty))
                            .is_none_or(|descriptor| descriptor.abi != MirAbiClass::Bool)
                        || !function.blocks.get(branch_block).is_some_and(|candidate| {
                            matches!(
                                &candidate.terminator,
                                super::MirTerminator::Branch {
                                    condition,
                                    then_target: actual_then,
                                    else_target: actual_else,
                                    ..
                                } if condition == condition_value
                                    && actual_then == then_target
                                    && actual_else == else_target
                            )
                        })
                    {
                        errors.push(MirValidationError {
                            subject: point.0.clone(),
                            message: "proof branch condition is not tied to its canonical MIR branch value".into(),
                        });
                    }
                    validate_expression(condition, function, catalog, true, errors);
                    scope.push(MirProofScopeFrame::Branch {
                        statement: point.clone(),
                        expression: expression.clone(),
                        then_arm: true,
                    });
                    validate_block(
                        then_block,
                        function,
                        catalog,
                        math,
                        invariants,
                        calls,
                        event_orders,
                        seen,
                        scope,
                        errors,
                    );
                    scope.pop();
                    scope.push(MirProofScopeFrame::Branch {
                        statement: point.clone(),
                        expression: expression.clone(),
                        then_arm: false,
                    });
                    validate_block(
                        else_block,
                        function,
                        catalog,
                        math,
                        invariants,
                        calls,
                        event_orders,
                        seen,
                        scope,
                        errors,
                    );
                    scope.pop();
                }
                MirProofOperation::Scope(inner) => {
                    validate_block(
                        inner,
                        function,
                        catalog,
                        math,
                        invariants,
                        calls,
                        event_orders,
                        seen,
                        scope,
                        errors,
                    );
                }
                MirProofOperation::Loop {
                    point,
                    header,
                    body_entry,
                    exit,
                    condition_value,
                    condition,
                    condition_calls,
                    invariants: loop_invariants,
                    body,
                    stationary: _,
                    receipt,
                } => {
                    let header_block = function.blocks.get(header);
                    let exact_loop_cfg = header.as_str() == format!("bb:while.header:{}", point.0)
                        && body_entry.as_str() == format!("bb:while.body:{}", point.0)
                        && exit.as_str() == format!("bb:while.exit:{}", point.0)
                        && header_block.is_some_and(|header| {
                            matches!(
                                &header.terminator,
                                super::MirTerminator::Branch {
                                    condition: actual,
                                    then_target,
                                    else_target,
                                    ..
                                } if actual == condition_value
                                    && then_target == body_entry
                                    && else_target == exit
                            )
                        });
                    let value_type = function.values.get(condition_value).map(|value| &value.ty);
                    let receipt_valid = receipt.as_ref().is_some_and(|receipt| {
                        validate_loop_receipt(receipt, point, header, body_entry, exit, function)
                    });
                    let loop_call_profile_valid = receipt.as_ref().is_some_and(|receipt| {
                        match receipt.typed.ffi_call_path.as_ref() {
                            Some(profile) => {
                                condition_calls.is_empty()
                                    && mir_loop_body_matches_ffi_call_path(body, profile)
                            }
                            None => {
                                receipt.typed.call_nodes.is_empty()
                                    && condition_calls.is_empty()
                                    && !mir_proof_block_contains_call(body)
                            }
                        }
                    });
                    if !exact_loop_cfg
                        || !receipt_valid
                        || !loop_call_profile_valid
                        || condition_value.as_str() != format!("expr:{}", condition.origin.0)
                        || value_type
                            .and_then(|ty| catalog.get(ty))
                            .is_none_or(|descriptor| descriptor.abi != MirAbiClass::Bool)
                    {
                        errors.push(MirValidationError {
                            subject: point.0.clone(),
                            message: "proof loop lacks a well-formed checker/MIR induction receipt, exact header CFG, or call-path profile binding".into(),
                        });
                    }
                    validate_expression(condition, function, catalog, true, errors);
                    let mut condition_scope = scope.clone();
                    condition_scope.push(MirProofScopeFrame::Loop(point.clone()));
                    for call in condition_calls {
                        calls.push(MirProofCallPointId {
                            owner: call.owner.clone(),
                            statement: call.statement.clone(),
                            call_node: call.call_node.clone(),
                            order: call.order,
                        });
                        event_orders.push(call.order);
                        validate_call_point(call, &condition_scope, function, catalog, errors);
                    }
                    scope.push(MirProofScopeFrame::Loop(point.clone()));
                    scope.push(MirProofScopeFrame::Block(body.id.clone()));
                    for invariant in loop_invariants {
                        if invariant.scope.as_slice() != scope.as_slice() {
                            errors.push(MirValidationError {
                                subject: invariant.id.statement.0.clone(),
                                message: "loop invariant lexical scope disagrees with its typed header/body path".into(),
                            });
                        }
                        if !seen.insert(invariant.id.clone()) {
                            errors.push(MirValidationError {
                                subject: invariant.id.statement.0.clone(),
                                message: "proof worklist repeats a loop invariant identity".into(),
                            });
                        }
                        invariants.push(invariant.id.clone());
                        validate_expression(&invariant.expression, function, catalog, true, errors);
                    }
                    scope.pop();
                    validate_block(
                        body,
                        function,
                        catalog,
                        math,
                        invariants,
                        calls,
                        event_orders,
                        seen,
                        scope,
                        errors,
                    );
                    scope.pop();
                }
                MirProofOperation::Break {
                    point,
                    target_loop,
                    value,
                } => {
                    if let Some(value) = value {
                        validate_expression(value, function, catalog, false, errors);
                    }
                    let active_loop = scope.iter().rev().find_map(|frame| match frame {
                        MirProofScopeFrame::Loop(loop_node) => Some(loop_node),
                        _ => None,
                    });
                    if active_loop.is_none() || target_loop.as_ref() != active_loop {
                        errors.push(MirValidationError {
                            subject: point.0.clone(),
                            message: "proof Break target is not the exact innermost checked loop"
                                .into(),
                        });
                    }
                    if !scope
                        .iter()
                        .any(|frame| matches!(frame, MirProofScopeFrame::Loop(_)))
                    {
                        errors.push(MirValidationError {
                            subject: point.0.clone(),
                            message: "proof Break is outside a checked loop scope".into(),
                        });
                    }
                }
                MirProofOperation::Continue { point, target_loop } => {
                    let active_loop = scope.iter().rev().find_map(|frame| match frame {
                        MirProofScopeFrame::Loop(loop_node) => Some(loop_node),
                        _ => None,
                    });
                    if active_loop.is_none() || target_loop.as_ref() != active_loop {
                        errors.push(MirValidationError {
                            subject: point.0.clone(),
                            message:
                                "proof Continue target is not the exact innermost checked loop"
                                    .into(),
                        });
                    }
                    if !scope
                        .iter()
                        .any(|frame| matches!(frame, MirProofScopeFrame::Loop(_)))
                    {
                        errors.push(MirValidationError {
                            subject: point.0.clone(),
                            message: "proof Continue is outside a checked loop scope".into(),
                        });
                    }
                }
                MirProofOperation::Return { value, .. } => {
                    if let Some(value) = value {
                        validate_expression(value, function, catalog, false, errors);
                    }
                }
                MirProofOperation::Exit => {}
            }
        }
        scope.pop();
    }

    let mut math = Vec::new();
    let mut invariants = Vec::new();
    let mut calls = Vec::new();
    let mut event_orders = Vec::new();
    let mut seen = BTreeSet::new();
    validate_block(
        &worklist.root,
        function,
        catalog,
        &mut math,
        &mut invariants,
        &mut calls,
        &mut event_orders,
        &mut seen,
        &mut Vec::new(),
        &mut errors,
    );
    math.sort();
    invariants.sort();
    let mut expected_math = worklist.expected_math_points.clone();
    let mut expected_invariants = worklist.expected_loop_invariants.clone();
    let mut expected_calls = worklist.expected_calls.clone();
    let mut expected_deferred_calls = worklist.expected_deferred_calls.clone();
    let mut expected_scoped_boundaries = worklist.expected_scoped_boundaries.clone();
    let mut expected_default_roots = worklist.expected_default_roots.clone();
    let mut expected_default_math = worklist.expected_default_math_points.clone();
    let mut expected_default_calls = worklist.expected_default_calls.clone();
    let mut expected_default_boundaries = worklist.expected_default_boundaries.clone();
    let mut expected_default_uses = worklist.expected_default_uses.clone();
    expected_math.sort();
    expected_invariants.sort();
    calls.sort();
    expected_calls.sort();

    let mut covered_math = math.clone();
    let mut covered_invariants = invariants.clone();
    let mut covered_calls = calls.clone();
    let mut covered_deferred_calls = Vec::new();
    let mut covered_scoped_boundaries = Vec::new();
    let mut covered_default_roots = Vec::new();
    let mut covered_default_math = Vec::new();
    let mut covered_default_calls = Vec::new();
    let mut covered_default_boundaries = Vec::new();
    let mut covered_default_uses = Vec::new();
    for boundary in &worklist.boundaries {
        if boundary.point != worklist.owner || boundary.reason.trim().is_empty() {
            errors.push(MirValidationError {
                subject: boundary.point.0.clone(),
                message: "proof boundary has no owner-matched point or reason".into(),
            });
        }
        covered_math.extend(boundary.math_points.iter().map(|event| event.point.clone()));
        covered_invariants.extend(
            boundary
                .loop_invariants
                .iter()
                .map(|event| event.point.clone()),
        );
        covered_calls.extend(boundary.calls.iter().map(|call| call.id.clone()));
        covered_deferred_calls.extend(boundary.deferred_calls.iter().cloned());
        covered_scoped_boundaries.extend(
            boundary
                .scoped_boundaries
                .iter()
                .map(|source| source.point.clone()),
        );
        covered_default_roots.extend(boundary.default_roots.iter().cloned());
        covered_default_math.extend(boundary.default_math_points.iter().cloned());
        covered_default_calls.extend(boundary.default_calls.iter().cloned());
        covered_default_boundaries.extend(boundary.default_boundaries.iter().cloned());
        covered_default_uses.extend(boundary.default_uses.iter().cloned());

        for events in [
            &boundary.math_points,
            &boundary.loop_invariants,
            &boundary.default_math_points,
        ] {
            if events.windows(2).any(|pair| pair[0] >= pair[1]) {
                errors.push(MirValidationError {
                    subject: boundary.point.0.clone(),
                    message: "proof boundary math census is not strictly ordered and unique".into(),
                });
            }
        }
        for calls in [
            &boundary.calls,
            &boundary.deferred_calls,
            &boundary.default_calls,
        ] {
            if calls.windows(2).any(|pair| pair[0].id >= pair[1].id) {
                errors.push(MirValidationError {
                    subject: boundary.point.0.clone(),
                    message: "proof boundary call census is not strictly ordered and unique".into(),
                });
            }
        }
        if boundary
            .scoped_boundaries
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
            || boundary
                .scoped_boundaries
                .iter()
                .any(|source| source.reason.trim().is_empty())
        {
            errors.push(MirValidationError {
                subject: boundary.point.0.clone(),
                message: "proof boundary source-boundary reasons are not an exact ordered census"
                    .into(),
            });
        }
        for call in boundary
            .calls
            .iter()
            .chain(&boundary.deferred_calls)
            .chain(&boundary.default_calls)
        {
            validate_boundary_call(call, &worklist.owner, &mut errors);
        }
        event_orders.extend(boundary.math_points.iter().map(|event| event.order));
        event_orders.extend(boundary.calls.iter().map(|call| call.id.order));
    }
    if worklist.boundaries.is_empty() {
        // Supported declaration-default roots are executable records in the
        // independently sealed proof-source census, not deferred worklist
        // boundaries. Their exact ids and use records are cross-checked by
        // that census and consumed by the verifier; keep them in this
        // function-local coverage check without manufacturing a boundary
        // that would suppress the executable body Math worklist.
        covered_default_roots.extend(worklist.expected_default_roots.iter().cloned());
        covered_default_math.extend(worklist.expected_default_math_points.iter().cloned());
        covered_default_calls.extend(worklist.expected_default_calls.iter().cloned());
        covered_default_boundaries.extend(worklist.expected_default_boundaries.iter().cloned());
        covered_default_uses.extend(worklist.expected_default_uses.iter().cloned());
    }
    for call in expected_deferred_calls
        .iter()
        .chain(&expected_default_calls)
    {
        validate_boundary_call(call, &worklist.owner, &mut errors);
    }
    covered_math.sort();
    covered_invariants.sort();
    covered_calls.sort();
    covered_deferred_calls.sort_by_key(|call| call.id.clone());
    covered_scoped_boundaries.sort();
    covered_scoped_boundaries.dedup();
    covered_default_roots.sort();
    covered_default_math.sort();
    covered_default_calls.sort_by_key(|call| call.id.clone());
    covered_default_boundaries.sort();
    covered_default_uses.sort();
    expected_deferred_calls.sort_by_key(|call| call.id.clone());
    expected_scoped_boundaries.sort();
    expected_scoped_boundaries.dedup();
    expected_default_roots.sort();
    expected_default_math.sort();
    expected_default_calls.sort_by_key(|call| call.id.clone());
    expected_default_boundaries.sort();
    expected_default_uses.sort();
    if expected_deferred_calls
        .windows(2)
        .any(|pair| pair[0].id >= pair[1].id)
        || expected_default_calls
            .windows(2)
            .any(|pair| pair[0].id >= pair[1].id)
    {
        errors.push(MirValidationError {
            subject: worklist.owner.0.clone(),
            message: "proof boundary expected call census is not strictly ordered and unique"
                .into(),
        });
    }
    for (name, strictly_ordered) in [
        (
            "Math",
            expected_math.windows(2).all(|pair| pair[0] < pair[1]),
        ),
        (
            "loop invariant",
            expected_invariants.windows(2).all(|pair| pair[0] < pair[1]),
        ),
        (
            "call",
            expected_calls.windows(2).all(|pair| pair[0] < pair[1]),
        ),
        (
            "scoped boundary",
            expected_scoped_boundaries
                .windows(2)
                .all(|pair| pair[0] < pair[1]),
        ),
        (
            "default root",
            expected_default_roots
                .windows(2)
                .all(|pair| pair[0] < pair[1]),
        ),
        (
            "default boundary",
            expected_default_boundaries
                .windows(2)
                .all(|pair| pair[0] < pair[1]),
        ),
        (
            "default use",
            expected_default_uses
                .windows(2)
                .all(|pair| pair[0] < pair[1]),
        ),
    ] {
        if !strictly_ordered {
            errors.push(MirValidationError {
                subject: worklist.owner.0.clone(),
                message: format!("proof expected {name} census is not strictly ordered and unique"),
            });
        }
    }
    if expected_default_math
        .windows(2)
        .any(|pair| pair[0] >= pair[1])
    {
        errors.push(MirValidationError {
            subject: worklist.owner.0.clone(),
            message: "proof expected default Math census is not strictly ordered and unique".into(),
        });
    }
    event_orders.sort();
    let unique_event_orders = event_orders.windows(2).all(|pair| pair[0] < pair[1]);
    if covered_math != expected_math {
        errors.push(MirValidationError {
            subject: worklist.owner.0.clone(),
            message:
                "proof worklist operations/boundaries disagree with the complete checker Math census"
                    .into(),
        });
    }
    if covered_invariants != expected_invariants {
        errors.push(MirValidationError {
            subject: worklist.owner.0.clone(),
            message:
                "proof worklist operations/boundaries disagree with the complete checker loop-invariant census"
                    .into(),
        });
    }
    if covered_calls != expected_calls {
        errors.push(MirValidationError {
            subject: worklist.owner.0.clone(),
            message:
                "proof worklist operations/boundaries disagree with the complete checker call census"
                    .into(),
        });
    }
    if covered_deferred_calls != expected_deferred_calls
        || covered_scoped_boundaries != expected_scoped_boundaries
        || covered_default_roots != expected_default_roots
        || covered_default_math != expected_default_math
        || covered_default_calls != expected_default_calls
        || covered_default_boundaries != expected_default_boundaries
        || covered_default_uses != expected_default_uses
    {
        errors.push(MirValidationError {
            subject: worklist.owner.0.clone(),
            message:
                "proof boundaries disagree with the complete checker declaration/place-root census"
                    .into(),
        });
    }
    if !unique_event_orders {
        errors.push(MirValidationError {
            subject: worklist.owner.0.clone(),
            message: "proof worklist repeats a Math/Call evaluation order".into(),
        });
    }
    if math
        .iter()
        .chain(invariants.iter())
        .any(|point| point.owner != worklist.owner)
        || calls.iter().any(|call| call.owner != worklist.owner)
        || expected_deferred_calls
            .iter()
            .any(|call| call.id.owner != worklist.owner)
        || expected_default_roots
            .iter()
            .any(|root| root.owner != worklist.owner)
        || expected_default_math
            .iter()
            .any(|event| event.point.owner != worklist.owner)
        || expected_default_calls
            .iter()
            .any(|call| call.id.owner != worklist.owner)
        || expected_default_uses
            .iter()
            .any(|use_id| use_id.caller_owner != worklist.owner)
    {
        errors.push(MirValidationError {
            subject: worklist.owner.0.clone(),
            message: "proof-point identity belongs to another callable owner".into(),
        });
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::{type_kind, ContractValueKind};
    use crate::core::ir::{PrimitiveType, ResolvedType, ResolvedTypeTable};
    use crate::core::mir::types::{MirDropGluePlan, MirTypeCatalog};

    fn copy_tuple_catalog() -> (MirTypeCatalog, crate::core::ir::ResolvedTypeId) {
        let mut table = ResolvedTypeTable::new();
        let i32_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::I32))
            .expect("i32");
        let bool_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::Bool))
            .expect("bool");
        let tuple = table
            .intern_resolved(ResolvedType::Tuple(vec![i32_id, bool_id]))
            .expect("tuple");
        let catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");
        (catalog, tuple)
    }

    #[test]
    fn contract_copy_aggregate_requires_complete_noop_metadata() {
        let (catalog, tuple) = copy_tuple_catalog();
        assert_eq!(
            type_kind(&catalog, &tuple).expect("canonical tuple"),
            ContractValueKind::Aggregate(tuple.clone())
        );

        for mutation in 0..4 {
            let mut forged = catalog.clone();
            let mut descriptor = forged.get(&tuple).expect("tuple descriptor").clone();
            match mutation {
                0 => descriptor.session_protocol = Some(tuple.clone()),
                1 => descriptor.needs_drop_glue = true,
                2 => descriptor.drop_plan = Some(MirDropGluePlan { fields: Vec::new() }),
                3 => descriptor.variant_drop_plan = Some(Vec::new()),
                _ => unreachable!(),
            }
            forged.replace_for_test_only(tuple.clone(), descriptor);
            let error = type_kind(&forged, &tuple)
                .expect_err("forged Copy aggregate metadata must fail closed");
            assert!(
                error.contains("outside the canonical Copy aggregate contract"),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn contract_expression_types_reject_changed_width_and_incomplete_paths() {
        let source = r#"
func predicate(x: i32, y: i32) -> bool {
    ensures: x / y == x / y
    true
}
func wide(value: i64) -> i64 { value }
"#;
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
            .expect("canonical contracts");
        let function = program
            .functions()
            .values()
            .find(|function| function.owner.0.ends_with("predicate"))
            .expect("predicate");
        let contract = &function.contracts[0];
        let narrow = contract
            .expression_types
            .get(&vec![0])
            .expect("division type");
        let wide = program
            .type_catalog()
            .iter()
            .find_map(|(ty, descriptor)| {
                (descriptor.abi
                    == super::MirAbiClass::Integer {
                        bits: 64,
                        signed: true,
                    })
                .then_some(ty.clone())
            })
            .expect("i64 TypeDesc");
        assert_ne!(narrow, &wide);
        assert!(super::validate_contracts(function, program.type_catalog()).is_empty());
        let original_text = function.canonical_text();
        for mutation in 0..5 {
            let mut forged = function.clone();
            let types = &mut forged.contracts[0].expression_types;
            match mutation {
                0 => {
                    types.insert(vec![0], wide.clone());
                }
                1 => {
                    types.remove(&vec![0]);
                }
                2 => {
                    types.insert(vec![42], wide.clone());
                }
                3 => {
                    types.insert(vec![0, 0], wide.clone());
                }
                4 => {
                    types.insert(vec![0], contract.expression_types[&Vec::new()].clone());
                }
                _ => unreachable!(),
            }
            assert_ne!(
                forged.canonical_text(),
                original_text,
                "metadata belongs to canonical digest"
            );
            let errors = super::validate_contracts(&forged, program.type_catalog());
            assert!(!errors.is_empty(), "mutation {mutation} was admitted");
            assert!(
                errors
                    .iter()
                    .any(|error| error.message.contains("TypeDesc")),
                "{errors:?}"
            );
        }
    }

    #[test]
    fn ffi_mir_integer_promotion_proves_and_disproves_mixed_width_and_exact_width_calls() {
        let source = r#"
extern "C" {
    func wide_positive(value: i64) -> i64 requires: value > 0;
    func exact_order(left: i64, right: i64) -> i64 requires: left > right;
}
func mixed_positive() -> i64 { wide_positive(1 as i64) }
func mixed_negative() -> i64 { wide_positive(0 as i64) }
func exact_positive() -> i64 { exact_order(2 as i64, 1 as i64) }
func main() -> i32 { 0 }
"#;
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("lex FFI mixed-width fixture");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse FFI mixed-width fixture");
        let checked = crate::core::check_program(&file).expect("check FFI fixture");
        let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
            .expect("Checker-approved mixed-width integer predicate materializes to MIR");
        let mixed_types = program
            .ffi_calls()
            .values()
            .find(|receipt| receipt.symbol == "wide_positive")
            .expect("mixed-width FFI receipt")
            .requires_types
            .clone();
        let mixed_left = mixed_types.get(&vec![0]).expect("left TypeDesc witness");
        let mixed_right = mixed_types.get(&vec![1]).expect("right TypeDesc witness");
        assert_ne!(
            mixed_left, mixed_right,
            "Checker's parameter and literal TypeDesc witnesses must remain distinct"
        );
        assert_eq!(
            program.type_catalog().get(mixed_left).unwrap().abi,
            super::MirAbiClass::Integer {
                bits: 64,
                signed: true
            }
        );
        assert_eq!(
            program.type_catalog().get(mixed_right).unwrap().abi,
            super::MirAbiClass::Integer {
                bits: 32,
                signed: true
            }
        );
        let exact_types = program
            .ffi_calls()
            .values()
            .find(|receipt| receipt.symbol == "exact_order")
            .expect("exact-width FFI receipt")
            .requires_types
            .clone();
        assert_eq!(
            exact_types.get(&vec![0]),
            exact_types.get(&vec![1]),
            "existing identical-TypeDesc validation path remains covered"
        );
        let results = crate::verifier::verify_mir(&program, "ffi-mixed-width-integer".into())
            .expect("production MIR verifier accepts typed FFI predicates");

        for (owner, expected) in [
            ("mixed_positive", crate::verifier::VerifStatus::Proven),
            ("mixed_negative", crate::verifier::VerifStatus::Disproven),
            ("exact_positive", crate::verifier::VerifStatus::Proven),
        ] {
            let result = results
                .iter()
                .find(|result| result.func_name.contains(owner))
                .unwrap_or_else(|| panic!("missing {owner} FFI proof result: {results:#?}"));
            assert_eq!(result.status, expected, "{owner}: {result:#?}");
            let artifact = result
                .artifact
                .as_ref()
                .expect("accepted FFI proof has canonical MIR artifact");
            assert_eq!(artifact.engine, crate::verifier::ProofArtifact::ENGINE_MIR);
            assert_eq!(artifact.mir_hash, program.canonical_digest());
        }
    }

    #[test]
    fn ffi_mir_contract_types_join_source_and_physical_abi_receipts() {
        use crate::core::ir::{primitive_type_id, PrimitiveType};
        use crate::core::mir::reference::MirProgram;
        use crate::core::mir::types::MirAbiClass;

        let source = r#"
extern "C" {
    func foreign_endpoint(value: i64) -> i64
        requires: value > 0
        ensures: result > value;
}
func main() -> i32 {
    foreign_endpoint(7 as i32);
    0
}
"#;
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("lex FFI endpoint-type fixture");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse FFI endpoint-type fixture");
        let checked = crate::core::check_program(&file).expect("check FFI endpoint-type fixture");
        let program = MirProgram::from_checked_program(&checked)
            .expect("materialize FFI source-to-ABI conversion fixture");
        program
            .validate_ffi_source_census()
            .expect("source argument types join the checked source census");

        let owner = crate::core::NodeId("function:main".into());
        let (instruction, original_receipt) = program
            .ffi_calls()
            .iter()
            .next()
            .map(|(instruction, receipt)| (instruction.clone(), receipt.clone()))
            .expect("one foreign call receipt");
        let function = program.functions().get(&owner).expect("MIR caller");
        let i32_type = primitive_type_id(PrimitiveType::I32);
        let i64_type = primitive_type_id(PrimitiveType::I64);
        assert_eq!(
            function.values[&original_receipt.arguments[0]].ty, i32_type,
            "the MIR operand retains the checked source TypeDesc"
        );
        assert_eq!(original_receipt.parameter_types, vec![i64_type.clone()]);
        assert_eq!(
            original_receipt.parameter_conversions,
            vec![super::super::MirFfiAbiConversion {
                from: MirAbiClass::Integer {
                    bits: 32,
                    signed: true,
                },
                to: MirAbiClass::Integer {
                    bits: 64,
                    signed: true,
                },
            }],
            "the physical conversion remains a distinct i32-to-i64 receipt"
        );
        super::validate_ffi_mir_contracts(function, program.type_catalog(), &original_receipt)
            .expect("Requires and Ensures TypeDescs join the declaration ABI endpoints");
        assert!(
            crate::core::mir::validate_ffi_call_contract_receipt(
                program.type_catalog(),
                function,
                &instruction,
                &original_receipt.callee,
                original_receipt.result.as_ref(),
                &original_receipt.arguments,
                &original_receipt,
            )
            .is_empty(),
            "the independent physical conversion receipt remains valid"
        );

        // The Checker currently gives the call result its declaration type.
        // A test-only caller slot exercises the existing narrowing receipt
        // without pretending that the surface type checker inserted it.
        let mut converted_result = program.clone();
        let result = original_receipt
            .result
            .as_ref()
            .expect("foreign result identity")
            .clone();
        let i32_type = primitive_type_id(PrimitiveType::I32);
        converted_result
            .replace_function_result_and_value_type_for_test_only(&owner, &result, i32_type);
        let mut receipts = converted_result.ffi_calls().clone();
        let receipt = receipts
            .get_mut(&instruction)
            .expect("converted result receipt");
        receipt.result_conversion = Some(super::super::MirFfiAbiConversion {
            from: MirAbiClass::Integer {
                bits: 64,
                signed: true,
            },
            to: MirAbiClass::Integer {
                bits: 32,
                signed: true,
            },
        });
        let source_fingerprint = receipt
            .source_witness
            .as_ref()
            .expect("Checker source witness")
            .checked_source_fingerprint;
        receipt.seal_checked_source(
            converted_result
                .functions()
                .get(&owner)
                .expect("converted MIR caller"),
            source_fingerprint,
        );
        converted_result.replace_ffi_calls_for_test_only(receipts);
        converted_result
            .validate_ffi_source_census()
            .expect("converted result retains its exact checked source call");

        let receipt = &converted_result.ffi_calls()[&instruction];
        let function = converted_result
            .functions()
            .get(&owner)
            .expect("converted MIR caller");
        super::validate_ffi_mir_contracts(function, converted_result.type_catalog(), receipt)
            .expect("Ensures result TypeDesc joins the declaration result endpoint");
        assert!(
            crate::core::mir::validate_ffi_call_contract_receipt(
                converted_result.type_catalog(),
                function,
                &instruction,
                &receipt.callee,
                receipt.result.as_ref(),
                &receipt.arguments,
                receipt,
            )
            .is_empty(),
            "the independent i64-to-i32 result conversion receipt remains valid"
        );
    }

    #[test]
    fn ffi_mir_integer_promotion_still_rejects_incompatible_operand_types() {
        use super::{MirContractBinaryOp, MirContractExpr};

        let source = r#"
extern "C" { func foreign(value: i64) -> i64 requires: value > 0; }
func caller() -> i64 { foreign(1 as i64) }
func main() -> i32 { 0 }
"#;
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("lex FFI incompatible-type fixture");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse FFI incompatible-type fixture");
        let checked = crate::core::check_program(&file).expect("check FFI fixture");
        let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
            .expect("canonical FFI MIR");
        let mut receipt = program
            .ffi_calls()
            .values()
            .next()
            .expect("one FFI call receipt")
            .clone();
        let bool_type = program
            .type_catalog()
            .iter()
            .find_map(|(ty, descriptor)| {
                (descriptor.abi == super::MirAbiClass::Bool).then_some(ty.clone())
            })
            .expect("canonical bool TypeDesc");
        receipt.requires = Some(MirContractExpr::Binary {
            op: MirContractBinaryOp::Greater,
            left: Box::new(MirContractExpr::Value(receipt.arguments[0].clone())),
            right: Box::new(MirContractExpr::Bool(true)),
        });
        receipt.requires_types.insert(vec![1], bool_type);

        let function = program
            .functions()
            .get(&receipt.caller)
            .expect("receipt caller function");
        let error = super::validate_ffi_mir_contracts(function, program.type_catalog(), &receipt)
            .expect_err("mixed integer promotion must not admit int/bool comparison");
        assert!(
            error.contains("TypeDesc/operator mismatch"),
            "unexpected validation failure: {error}"
        );
    }

    #[test]
    fn ffi_mir_mixed_width_arithmetic_requires_canonical_i64_result_in_both_orders() {
        let source = r#"
extern "C" {
    func mixed_arithmetic(small: i32, wide: i64) -> i64
        requires: small + wide == wide + small;
}
func caller(small: i32, wide: i64) -> i64 {
    mixed_arithmetic(small, wide)
}
func main() -> i32 { 0 }
"#;
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("lex mixed-width arithmetic fixture");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse mixed-width arithmetic fixture");
        let checked = crate::core::check_program(&file).expect("check mixed-width FFI fixture");
        let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
            .expect("canonical mixed-width arithmetic FFI MIR");
        let receipt = program
            .ffi_calls()
            .values()
            .find(|receipt| receipt.symbol == "mixed_arithmetic")
            .expect("mixed-width arithmetic FFI receipt");
        let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        let i64_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I64);

        // Both operand orders widen to the same exact canonical i64 result.
        assert_eq!(receipt.requires_types.get(&vec![0]), Some(&i64_type));
        assert_eq!(receipt.requires_types.get(&vec![0, 0]), Some(&i32_type));
        assert_eq!(receipt.requires_types.get(&vec![0, 1]), Some(&i64_type));
        assert_eq!(receipt.requires_types.get(&vec![1]), Some(&i64_type));
        assert_eq!(receipt.requires_types.get(&vec![1, 0]), Some(&i64_type));
        assert_eq!(receipt.requires_types.get(&vec![1, 1]), Some(&i32_type));

        let function = program
            .functions()
            .get(&receipt.caller)
            .expect("canonical FFI caller function");
        super::validate_ffi_mir_contracts(function, program.type_catalog(), receipt)
            .expect("both mixed-width operand orders have canonical i64 results");

        for wrong_result_path in [vec![0], vec![1]] {
            let mut forged = receipt.clone();
            forged
                .requires_types
                .insert(wrong_result_path.clone(), i32_type.clone());
            let error =
                super::validate_ffi_mir_contracts(function, program.type_catalog(), &forged)
                    .expect_err("a mixed i32/i64 operation cannot claim an i32 result");
            assert!(
                error.contains("TypeDesc/operator mismatch"),
                "wrong result at {wrong_result_path:?} failed for an unexpected reason: {error}"
            );
        }
    }

    #[test]
    fn loop_ffi_receipt_binds_call_to_the_guarded_terminal_mir_block() {
        use super::{mir_loop_ffi_call_block_matches, validate_proof_worklist, MirProofOperation};
        use crate::core::mir::MirBlockId;

        fn loop_operation(block: &super::MirProofBlock) -> Option<&MirProofOperation> {
            for operation in &block.operations {
                match operation {
                    MirProofOperation::Loop { .. } => return Some(operation),
                    MirProofOperation::Branch {
                        then_block,
                        else_block,
                        ..
                    } => {
                        if let Some(found) = loop_operation(then_block) {
                            return Some(found);
                        }
                        if let Some(found) = loop_operation(else_block) {
                            return Some(found);
                        }
                    }
                    MirProofOperation::Scope(inner) => {
                        if let Some(found) = loop_operation(inner) {
                            return Some(found);
                        }
                    }
                    MirProofOperation::Bind { .. }
                    | MirProofOperation::Call(_)
                    | MirProofOperation::Eval { .. }
                    | MirProofOperation::Assign { .. }
                    | MirProofOperation::Math(_)
                    | MirProofOperation::Break { .. }
                    | MirProofOperation::Continue { .. }
                    | MirProofOperation::Return { .. }
                    | MirProofOperation::Exit => {}
                }
            }
            None
        }

        fn loop_operation_mut(block: &mut super::MirProofBlock) -> Option<&mut MirProofOperation> {
            for operation in &mut block.operations {
                match operation {
                    MirProofOperation::Loop { .. } => return Some(operation),
                    MirProofOperation::Branch {
                        then_block,
                        else_block,
                        ..
                    } => {
                        if let Some(found) = loop_operation_mut(then_block) {
                            return Some(found);
                        }
                        if let Some(found) = loop_operation_mut(else_block) {
                            return Some(found);
                        }
                    }
                    MirProofOperation::Scope(inner) => {
                        if let Some(found) = loop_operation_mut(inner) {
                            return Some(found);
                        }
                    }
                    MirProofOperation::Bind { .. }
                    | MirProofOperation::Call(_)
                    | MirProofOperation::Eval { .. }
                    | MirProofOperation::Assign { .. }
                    | MirProofOperation::Math(_)
                    | MirProofOperation::Break { .. }
                    | MirProofOperation::Continue { .. }
                    | MirProofOperation::Return { .. }
                    | MirProofOperation::Exit => {}
                }
            }
            None
        }

        let source = r#"
extern "C" { func positive(value: i32) -> i32 requires: value > 0; }
func while_continue_positive(value: i32) -> i32 {
    while value >= 0 {
        if value == 0 { continue; }
        positive(value);
        break;
    }
    0
}
func main() -> i32 { 0 }
"#;
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
            .expect("lower exact loop FFI source and MIR receipts");
        let function = program
            .functions()
            .values()
            .find(|function| function.owner.0.ends_with("while_continue_positive"))
            .expect("loop caller function");
        let worklist = function
            .proof_worklist
            .as_ref()
            .expect("loop proof worklist");
        assert!(worklist.has_valid_source_witness());
        validate_proof_worklist(worklist, function, program.type_catalog())
            .expect("source, worklist, actual call, and terminal edge agree");

        let (header, body, receipt) = match loop_operation(&worklist.root) {
            Some(MirProofOperation::Loop {
                header,
                body,
                receipt: Some(receipt),
                ..
            }) => (header.clone(), body.clone(), receipt.clone()),
            operation => panic!("expected loop operation, got {operation:?}"),
        };
        let profile = receipt
            .typed
            .ffi_call_path
            .as_ref()
            .expect("exact FFI loop call profile")
            .clone();
        let actual_call_block = receipt
            .ffi_call_block
            .as_ref()
            .expect("exact MIR call block");
        let break_edge = receipt
            .edges
            .iter()
            .find(|edge| {
                edge.source_site == profile.terminal_break
                    && edge.kind == super::MirProofLoopEdgeKind::Break
            })
            .expect("profiled terminal break edge");
        let continue_edge = receipt
            .edges
            .iter()
            .find(|edge| edge.kind == super::MirProofLoopEdgeKind::Continue)
            .expect("call-free early continue edge");
        let branch_block = body
            .operations
            .iter()
            .find_map(|operation| match operation {
                MirProofOperation::Branch { branch_block, .. } => Some(branch_block.clone()),
                _ => None,
            })
            .expect("the early-continue guard block");
        assert_eq!(actual_call_block, &break_edge.from_block);
        assert_eq!(
            break_edge.to_block,
            Some(
                MirBlockId::new(format!("bb:while.exit:{}", profile.loop_node.0))
                    .expect("canonical loop exit")
            )
        );
        assert_ne!(actual_call_block, &continue_edge.from_block);
        assert_ne!(actual_call_block, &branch_block);
        assert_ne!(actual_call_block, &header);
        assert_eq!(body.operations.len(), 3, "one guard, one call, one break");

        for wrong_block in [header, continue_edge.from_block.clone(), branch_block] {
            let mut forged = worklist.clone();
            let operation = loop_operation_mut(&mut forged.root).expect("loop operation");
            let MirProofOperation::Loop {
                receipt: Some(receipt),
                ..
            } = operation
            else {
                panic!("expected loop receipt");
            };
            receipt.ffi_call_block = Some(wrong_block);
            let forged = forged.seal_source_witness();
            let errors = validate_proof_worklist(&forged, function, program.type_catalog())
                .expect_err("a call block cannot be moved to header, continue, or guard block");
            assert!(errors
                .iter()
                .any(|error| { error.message.contains("call-path profile binding") }));
        }

        let mut extra_call = worklist.clone();
        let operation = loop_operation_mut(&mut extra_call.root).expect("loop operation");
        let MirProofOperation::Loop { body, .. } = operation else {
            panic!("expected loop operation");
        };
        let call = body
            .operations
            .iter()
            .find_map(|operation| match operation {
                MirProofOperation::Call(call) => Some(call.clone()),
                _ => None,
            })
            .expect("the one profiled call");
        body.operations.push(MirProofOperation::Call(call));
        let extra_call = extra_call.seal_source_witness();
        let errors = validate_proof_worklist(&extra_call, function, program.type_catalog())
            .expect_err("a second loop call cannot reuse the first call's receipt");
        assert!(errors
            .iter()
            .any(|error| { error.message.contains("call-path profile binding") }));

        let mut wrong_source_terminal = receipt.clone();
        wrong_source_terminal.ffi_call_block = Some(continue_edge.from_block.clone());
        assert!(!mir_loop_ffi_call_block_matches(
            function,
            &wrong_source_terminal,
            &profile,
            &MirBlockId::new(format!("bb:while.exit:{}", profile.loop_node.0))
                .expect("canonical loop exit")
        ));
    }

    #[test]
    fn proof_worklist_private_source_seal_rejects_term_and_type_map_forgery() {
        use super::{MirContractExpr, MirProofOperation};

        let source = r#"
func prove(value: i32) -> i32 {
    math: { value + 1 > value; }
    value
}
func main() -> i32 { 0 }
"#;
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
            .expect("canonical MIR");
        let function = program
            .functions()
            .values()
            .find(|function| function.owner.0.ends_with("prove"))
            .expect("prove function");
        let worklist = function
            .proof_worklist
            .as_ref()
            .expect("typed math worklist");
        assert!(worklist.has_valid_source_witness());

        let mut forged_term = worklist.clone();
        let MirProofOperation::Math(assertion) = &mut forged_term.root.operations[0] else {
            panic!("expected source Math event");
        };
        let MirContractExpr::Binary { right, .. } = &mut assertion.expression.expression else {
            panic!("expected binary source predicate");
        };
        **right = MirContractExpr::Int(0);
        assert!(!forged_term.has_valid_source_witness());

        let mut forged_types = worklist.clone();
        let MirProofOperation::Math(assertion) = &mut forged_types.root.operations[0] else {
            panic!("expected source Math event");
        };
        let root_ty = assertion.expression.expression_types[&Vec::new()].clone();
        assertion
            .expression
            .expression_types
            .insert(vec![0], root_ty);
        assert!(!forged_types.has_valid_source_witness());
        let errors =
            super::validate_proof_worklist(&forged_types, function, program.type_catalog())
                .expect_err("forged private source witness must reject the worklist");
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("invalid private checked-source witness")
        }));
    }
}

/// Entry invariants can only refer to canonical immutable parameter snapshots.
/// This shared predicate is deliberately structural, independent of a solver.
/// Mutation/loop invariants need separate program-point and induction receipts.
pub(crate) fn validate_entry_invariant_shapes(function: &MirFunction) -> Result<(), String> {
    fn parameters_only(expression: &MirContractExpr, parameters: &[MirValueId]) -> bool {
        match expression {
            MirContractExpr::Value(value) | MirContractExpr::Old(value) => {
                parameters.contains(value)
            }
            MirContractExpr::Result => false,
            MirContractExpr::Project { base, .. } => parameters_only(base, parameters),
            MirContractExpr::Unary { operand, .. } => parameters_only(operand, parameters),
            MirContractExpr::Binary { left, right, .. } => {
                parameters_only(left, parameters) && parameters_only(right, parameters)
            }
            MirContractExpr::Int(_)
            | MirContractExpr::Bool(_)
            | MirContractExpr::Float(_)
            | MirContractExpr::String(_) => true,
            MirContractExpr::StringLength(value) | MirContractExpr::StringCharAt0(value) => {
                parameters_only(value, parameters)
            }
            MirContractExpr::Unsupported(_) => false,
        }
    }
    if function.contracts.iter().any(|contract| {
        contract.kind == MirContractKind::Invariant
            && !parameters_only(&contract.condition, &function.parameters)
    }) {
        return Err("entry invariant requires immutable parameter snapshots; result/local/iteration invariant has no program-point receipt".into());
    }
    if function
        .contracts
        .iter()
        .any(|contract| contract.kind == MirContractKind::Invariant)
        && function
            .parameter_permissions
            .as_ref()
            .is_some_and(|permissions| {
                permissions.iter().any(|permission| {
                    matches!(
                        permission,
                        Some(
                            crate::core::ir::Permission::View | crate::core::ir::Permission::Mutate
                        )
                    )
                })
            })
    {
        return Err("borrowed parameter invariant requires an effect-preservation proof".into());
    }
    Ok(())
}
