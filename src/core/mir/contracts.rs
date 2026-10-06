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
    TypedCallExecutionDisposition, TypedLoopEdgeKind, TypedLoopProofReceipt,
};
use crate::core::{NodeId, ResolvedLocalId};
use std::collections::{BTreeMap, BTreeSet};

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
        hasher.update(b"mimi-mir-proof-worklist-source-v2\0");
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
                            "    proof.call owner={} body_owner={} statement={} node={} order={} scope={:?} disposition={:?} callee={:?} type_arguments={:?} result_type={} declared_effects={:?} authority={:?} instruction={} result={:?} havoc={:?}",
                            call.owner.0,
                            call.body_owner.0,
                            call.statement.0,
                            call.call_node.0,
                            call.order,
                            call.scope,
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
        let mut output = format!("mir.proof_worklist owner={} v6\n", self.owner.0);
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
                boundary.root.parameter.0 .0,
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
                use_id.parameter.0 .0,
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
            MirContractExpr::Value(value) if !arguments.contains(value) => {
                Err(format!("extern requires value '{value}' is not a call argument"))
            }
            MirContractExpr::Value(_) | MirContractExpr::Int(_) | MirContractExpr::Bool(_) => Ok(()),
            // A float literal is a literal leaf; its kind is rejected by the
            // contains_float_leaf guard that runs after this walk.
            MirContractExpr::Float(_) => Ok(()),
            MirContractExpr::Unary { operand, .. } => validate_leaves(operand, arguments),
            MirContractExpr::Binary { left, right, .. } => {
                validate_leaves(left, arguments)?;
                validate_leaves(right, arguments)
            }
            MirContractExpr::Result | MirContractExpr::Old(_) | MirContractExpr::Project { .. } => {
                Err("extern requires must use pre-call scalar arguments, without result/old/projection".into())
            }
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
            | MirContractExpr::Project { .. } => Err(
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
    for (index, contract) in function.contracts.iter().enumerate() {
        let subject = format!("contract[{index}] {}", contract.id.0.as_str());
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

fn contains_result(expression: &MirContractExpr) -> bool {
    match expression {
        MirContractExpr::Result => true,
        MirContractExpr::Unary { operand, .. } => contains_result(operand),
        MirContractExpr::Binary { left, right, .. } => {
            contains_result(left) || contains_result(right)
        }
        MirContractExpr::Project { base, .. } => contains_result(base),
        MirContractExpr::Value(_)
        | MirContractExpr::Old(_)
        | MirContractExpr::Float(_)
        | MirContractExpr::Int(_)
        | MirContractExpr::Bool(_) => false,
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
        MirContractExpr::Value(_)
        | MirContractExpr::Result
        | MirContractExpr::Int(_)
        | MirContractExpr::Bool(_)
        | MirContractExpr::Float(_) => false,
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
        ResolvedExprKind::Load(place) => lower_contract_place(place, function, body, false),
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
        _ => Err(format!(
            "contract expression shape {:?} is outside the canonical MIR verifier contract",
            expression.kind
        )),
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
        _ => {}
    }
    Ok(())
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

pub(crate) fn lower_contracts(
    callable: &crate::core::ir::ResolvedCallable,
    function: &MirFunction,
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
        match lower_contract_expr(&contract.condition, function, &callable.body) {
            Ok(condition) => {
                let mut expression_types = BTreeMap::new();
                if let Err(message) = collect_expression_types(
                    &contract.condition,
                    &callable.body,
                    &mut Vec::new(),
                    &mut expression_types,
                ) {
                    errors.push(super::lower::MirLoweringError {
                        node_id: contract.node_id.clone(),
                        message,
                    });
                    continue;
                }
                contracts.push(MirContract {
                    id: contract.node_id.clone(),
                    kind,
                    condition,
                    expression_types,
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

pub(crate) fn lower_proof_worklist(
    callable: &crate::core::ir::ResolvedCallable,
    function: &MirFunction,
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
    let body_proof_requested = !typed.points.is_empty()
        || !typed.loop_invariant_nodes().is_empty()
        || !typed.boundaries.is_empty();
    if !body_proof_requested && !active_default {
        // Call-site Requires are checked by the MIR call consumer directly.
        // An unrelated runtime call by itself is not a reason to materialize
        // an otherwise empty proof worklist.
        return Ok(None);
    }

    let deferred_body_calls = typed.calls.iter().any(|point| {
        matches!(
            &point.disposition,
            crate::core::ir::TypedCallExecutionDisposition::Deferred(_)
        )
    });
    if !typed.boundaries.is_empty()
        || !typed.deferred_calls.is_empty()
        || deferred_body_calls
        || active_default
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
            .chain(active_default.then_some(
                "declaration-default proof roots stay isolated from caller Requires/Ensures facts".into(),
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
        if matches!(
            &point.disposition,
            TypedCallExecutionDisposition::Deferred(_)
        ) {
            return Err(format!(
                "call proof point '{}' is deferred and cannot contribute facts",
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
        let instruction_ids = instruction_roles
            .iter()
            .map(|role| {
                super::MirInstructionId::new(format!("inst:{role}:{}", point.call_node.0))
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
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
        let (result, mir_arguments) = match (&source_call.callee, &instruction.kind) {
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
        };
        let expected_result = MirValueId::new(format!("expr:{}", point.call_node.0))
            .map_err(|error| error.to_string())?;
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
            match argument.conversion.kind {
                CheckedConversionKind::Identity => {
                    if argument.conversion.from != argument.conversion.to || actual != &source_value
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
            if function
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
        if body.3 != header_values || exit_record.3 != header_values {
            return Err(format!(
                "loop '{}' condition edges do not pass the exact header state locals",
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
            edges,
        })
    }

    fn lower_block(
        block: &crate::core::ir::TypedProofBlock,
        callable: &crate::core::ir::ResolvedCallable,
        function: &MirFunction,
        scope: &mut Vec<TypedProofScopeFrame>,
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
                    MirProofOperation::Call(lower_call_point(point, callable, function)?)
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
                    let then_block = lower_block(then_block, callable, function, scope)?;
                    scope.pop();
                    scope.push(TypedProofScopeFrame::Branch {
                        statement: point.clone(),
                        expression: expression.clone(),
                        then_arm: false,
                    });
                    let else_block = lower_block(else_block, callable, function, scope)?;
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
                    MirProofOperation::Scope(lower_block(inner, callable, function, scope)?)
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
                    if !typed_receipt.call_nodes.is_empty()
                        || !typed_receipt.effect_nodes.is_empty()
                        || !typed_receipt.loans.is_empty()
                    {
                        return Err(format!(
                            "proof loop '{}' has call/effect/loan events outside the scalar induction profile",
                            point.0
                        ));
                    }
                    let loop_receipt =
                        lower_mir_loop_receipt(typed_receipt, callable, function)?;
                    let lowered_condition_calls = condition_calls
                        .iter()
                        .map(|call| lower_call_point(call, callable, function))
                        .collect::<Result<Vec<_>, _>>()?;
                    scope.push(TypedProofScopeFrame::Loop(point.clone()));
                    let body = lower_block(body, callable, function, scope)?;
                    scope.pop();
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
    let root = match lower_block(&typed.root, callable, function, &mut Vec::new()) {
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
    if lowered_calls != expected_calls {
        return Ok(Some(boundary_worklist(
            &typed,
            callable,
            "lowered MIR call events disagree with the complete checker call census".into(),
        )));
    }
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
        expected_scoped_boundaries: Vec::new(),
        expected_default_roots: Vec::new(),
        expected_default_math_points: Vec::new(),
        expected_default_calls: Vec::new(),
        expected_default_boundaries: Vec::new(),
        expected_default_uses: Vec::new(),
        expected_place_inputs: callable
            .body
            .place_inputs
            .iter()
            .map(|(point, expression)| MirProofPlaceInputId {
                point: point.clone(),
                expression: expression.node_id.clone(),
            })
            .collect(),
        boundaries: Vec::new(),
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
        for argument in &call.arguments {
            let source_ty = argument.expression.expression_types.get(&Vec::new());
            if source_ty != Some(&argument.conversion.from)
                || function
                    .values
                    .get(&argument.value)
                    .is_none_or(|value| value.ty != argument.conversion.to)
            {
                errors.push(MirValidationError {
                    subject: call.call_node.0.clone(),
                    message: "call proof argument TypeDesc/conversion receipt disagrees with its MIR actual".into(),
                });
            }
            validate_expression(&argument.expression, function, catalog, false, errors);
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
                _ => false,
            })
            .count();
        if call.result.as_ref().is_some_and(|result| {
            function
                .values
                .get(result)
                .is_none_or(|value| value.ty != call.result_type)
        }) || (call.result.is_none()
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
        if typed.schema_version != 2
            || typed.owner != function.owner
            || typed.loop_node != *point
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
        let Some(exit_block) = function.blocks.get(exit) else {
            return false;
        };
        if header_block.parameters.len() != receipt.bindings.len()
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
            || condition_true.arguments != header_values
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
                    if !exact_loop_cfg
                        || !receipt_valid
                        || condition_value.as_str() != format!("expr:{}", condition.origin.0)
                        || value_type
                            .and_then(|ty| catalog.get(ty))
                            .is_none_or(|descriptor| descriptor.abi != MirAbiClass::Bool)
                    {
                        errors.push(MirValidationError {
                            subject: point.0.clone(),
                            message: "proof loop lacks a well-formed checker/MIR induction receipt or exact header CFG".into(),
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
            MirContractExpr::Int(_) | MirContractExpr::Bool(_) | MirContractExpr::Float(_) => true,
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
