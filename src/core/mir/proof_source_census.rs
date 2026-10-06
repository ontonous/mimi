//! Checker-derived, nonoptional census of proof-bearing MIR source.
//!
//! `MirFunction::proof_worklist` is an optional execution representation.  It
//! cannot also be the only evidence that proof obligations exist: removing
//! that option would erase the evidence along with the consumer input.  This
//! module stores a separately sealed owner census on every `MirProgram`.

use std::collections::{BTreeMap, BTreeSet};

use crate::core::ir::{
    CheckedConversion, CheckedConversionKind, ContractKind, EffectId, ResolvedBlock, ResolvedBody,
    ResolvedCallee, ResolvedExpr, ResolvedExprKind, ResolvedFfiContractRoot,
    ResolvedFfiParameterMode, ResolvedParameterId, ResolvedStmtKind, ResolvedTypeId,
    TypedCallEffectAuthority, TypedCallExecutionDisposition, TypedCallProofPoint,
    TypedLoopInvariantPoint, TypedMathProofPoint, TypedProofBlock, TypedProofOperation,
    TypedProofPointId, TypedProofPointWorklist, TypedProofScopeFrame,
};
use crate::core::{CheckedProgram, NodeId, Origin};

use super::{MirInstance, MirProofPointId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirProofSourceRootKind {
    Requires,
    Ensures,
    Invariant,
    Math,
    Loop,
    Call,
    DefaultValue,
    PlaceInput,
    Boundary,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MirProofSourceRoot {
    kind: MirProofSourceRootKind,
    node: NodeId,
    identity: String,
}

impl MirProofSourceRoot {
    pub fn kind(&self) -> MirProofSourceRootKind {
        self.kind
    }

    pub fn node(&self) -> &NodeId {
        &self.node
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofSourceCall {
    origin: MirProofSourceCallOrigin,
    body_owner: NodeId,
    owner: NodeId,
    statement: NodeId,
    order: Option<u64>,
    scope: Vec<MirProofSourceScopeFrame>,
    guards: Vec<(NodeId, bool)>,
    disposition: TypedCallExecutionDisposition,
    call_node: NodeId,
    callee: ResolvedCallee,
    callee_identity: String,
    selected_signature: String,
    expression_type: Option<ResolvedTypeId>,
    type_arguments: Option<Vec<ResolvedTypeId>>,
    result_type: Option<ResolvedTypeId>,
    effects: Option<Vec<EffectId>>,
    effect_authority: TypedCallEffectAuthority,
    requires_declared: bool,
    ensures_declared: bool,
    source_origin: Option<Origin>,
    arguments: Vec<MirProofSourceCallArgument>,
}

impl MirProofSourceCall {
    pub fn origin(&self) -> &MirProofSourceCallOrigin {
        &self.origin
    }

    pub fn call_node(&self) -> &NodeId {
        &self.call_node
    }

    /// Checker body whose Resolved expression tree owns this call.
    pub fn body_owner(&self) -> &NodeId {
        &self.body_owner
    }

    /// Selected callable owner at the call point (different from `body_owner`
    /// for a latent lambda body).
    pub fn owner(&self) -> &NodeId {
        &self.owner
    }

    pub fn statement(&self) -> &NodeId {
        &self.statement
    }

    /// Ordered execution events have a typed ordinal. `None` means the
    /// source call was found but the typed event census did not bind it; such
    /// a call is not eligible for proof consumption.
    pub fn order(&self) -> Option<u64> {
        self.order
    }

    pub fn scope(&self) -> &[MirProofSourceScopeFrame] {
        &self.scope
    }

    pub fn guards(&self) -> &[(NodeId, bool)] {
        &self.guards
    }

    pub fn disposition(&self) -> &TypedCallExecutionDisposition {
        &self.disposition
    }

    pub fn callee(&self) -> &ResolvedCallee {
        &self.callee
    }

    pub fn callee_identity(&self) -> &str {
        &self.callee_identity
    }

    /// Exact selected declaration/signature identity, with the instantiated
    /// call shape included for generic, builtin, and externally declared
    /// callees.
    pub fn selected_signature(&self) -> &str {
        &self.selected_signature
    }

    pub fn expression_type(&self) -> Option<&ResolvedTypeId> {
        self.expression_type.as_ref()
    }

    pub fn type_arguments(&self) -> Option<&[ResolvedTypeId]> {
        self.type_arguments.as_deref()
    }

    pub fn result_type(&self) -> Option<&ResolvedTypeId> {
        self.result_type.as_ref()
    }

    pub fn effects(&self) -> Option<&[EffectId]> {
        self.effects.as_deref()
    }

    pub fn effect_authority(&self) -> TypedCallEffectAuthority {
        self.effect_authority
    }

    /// Exact checker source origin for this call expression. A missing origin
    /// means the source identity directory found a call without a matching
    /// expression tree, so diagnostics must keep the unknown span.
    pub fn source_origin(&self) -> Option<&Origin> {
        self.source_origin.as_ref()
    }

    pub fn requires_declared(&self) -> bool {
        self.requires_declared
    }

    /// Extern Ensures are post-call obligations. Their presence must be
    /// attached to this exact selected declaration/call and must never be
    /// promoted into a caller-entry fact.
    pub fn ensures_declared(&self) -> bool {
        self.ensures_declared
    }

    /// Exact checker argument ordering and source identities. This includes
    /// variadic tail arguments, whose indices and original types must remain
    /// available to the independent ABI consumer.
    pub fn arguments(&self) -> &[MirProofSourceCallArgument] {
        &self.arguments
    }

    /// A source call can be matched to an executed MIR FFI call only when the
    /// checker-produced typed timeline bound its exact body event. Deferred
    /// calls remain in the census but cannot be consumed as executable facts.
    pub fn has_typed_execution_event(&self) -> bool {
        self.order.is_some()
            && !matches!(
                &self.disposition,
                TypedCallExecutionDisposition::Deferred(_)
            )
    }

    /// Whether the source expression directory supplied the complete typed
    /// call shape, including its result/effect facts and every argument
    /// conversion. A directory-only call identity is retained for census
    /// completeness but is never enough to authorize a physical call match.
    pub fn has_complete_source_shape(&self) -> bool {
        self.expression_type.is_some()
            && self.result_type.is_some()
            && self.effects.is_some()
            && self
                .arguments
                .iter()
                .enumerate()
                .all(|(index, argument)| argument.index == index)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirProofSourceCallOrigin {
    Body,
    /// Call syntactically inside a contract or Math root. It is present in
    /// the source census but is not an executable body call.
    ProofRoot(NodeId),
    /// A call lexically inside a closure body; it belongs to that closure's
    /// owner and is latent until a checked invocation receipt exists.
    Lambda(NodeId),
    Default(ResolvedParameterId),
    PlaceInput(NodeId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofSourceCallArgument {
    index: usize,
    parameter: ResolvedParameterId,
    source_node: NodeId,
    source_type: ResolvedTypeId,
    conversion: CheckedConversionKind,
    conversion_from: ResolvedTypeId,
    target_type: ResolvedTypeId,
}

impl MirProofSourceCallArgument {
    pub fn index(&self) -> usize {
        self.index
    }

    pub fn parameter(&self) -> &ResolvedParameterId {
        &self.parameter
    }

    pub fn source_node(&self) -> &NodeId {
        &self.source_node
    }

    pub fn source_type(&self) -> &ResolvedTypeId {
        &self.source_type
    }

    pub fn conversion(&self) -> CheckedConversionKind {
        self.conversion
    }

    pub fn conversion_from(&self) -> &ResolvedTypeId {
        &self.conversion_from
    }

    pub fn target_type(&self) -> &ResolvedTypeId {
        &self.target_type
    }
}

/// Exact source call identity kept even when its callable is not materialized
/// into this MIR graph (for example, a dormant generic template or an
/// unreachable Actor helper). The resolved-body validator has already checked
/// that `call_nodes` matches every Call node in the body/default/place trees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofSourceCallIdentity {
    call_node: NodeId,
    callee: ResolvedCallee,
    callee_identity: String,
    selected_signature: String,
    expression_type: Option<ResolvedTypeId>,
    type_arguments: Option<Vec<ResolvedTypeId>>,
    result_type: Option<ResolvedTypeId>,
    effects: Option<Vec<EffectId>>,
    requires_declared: bool,
    ensures_declared: bool,
    arguments: Vec<MirProofSourceCallArgument>,
}

impl MirProofSourceCallIdentity {
    pub fn call_node(&self) -> &NodeId {
        &self.call_node
    }
    pub fn callee(&self) -> &ResolvedCallee {
        &self.callee
    }
    pub fn callee_identity(&self) -> &str {
        &self.callee_identity
    }
    pub fn selected_signature(&self) -> &str {
        &self.selected_signature
    }
    pub fn expression_type(&self) -> Option<&ResolvedTypeId> {
        self.expression_type.as_ref()
    }
    pub fn type_arguments(&self) -> Option<&[ResolvedTypeId]> {
        self.type_arguments.as_deref()
    }
    pub fn result_type(&self) -> Option<&ResolvedTypeId> {
        self.result_type.as_ref()
    }
    pub fn effects(&self) -> Option<&[EffectId]> {
        self.effects.as_deref()
    }
    pub fn requires_declared(&self) -> bool {
        self.requires_declared
    }
    pub fn ensures_declared(&self) -> bool {
        self.ensures_declared
    }
    pub fn arguments(&self) -> &[MirProofSourceCallArgument] {
        &self.arguments
    }

    /// True only when the Resolved expression tree supplied all typed actuals
    /// and selected-call result/effect facts. This complements
    /// `has_typed_execution_event`: neither source directory presence nor an
    /// event ordinal alone authenticates the full call shape.
    pub fn has_complete_source_shape(&self) -> bool {
        self.expression_type.is_some()
            && self.result_type.is_some()
            && self.effects.is_some()
            && self
                .arguments
                .iter()
                .enumerate()
                .all(|(index, argument)| argument.index == index)
    }
}

/// CheckedProgram-wide callable census, including source owners not emitted as
/// a `MirFunction` by a reachability-based materializer. Such an owner is not
/// silently dropped: consumers can report a typed owner coverage gap when it
/// carries proof obligations but has no MIR owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofSourceCallableCensus {
    owner: NodeId,
    source_fingerprint: String,
    roots: Vec<MirProofSourceRoot>,
    calls: Vec<MirProofSourceCallIdentity>,
    defaults: Vec<MirProofSourceDefault>,
    place_inputs: Vec<MirProofSourcePlaceInput>,
    materialized_owners: Vec<NodeId>,
    verification_required: bool,
    worklist_required: bool,
}

impl MirProofSourceCallableCensus {
    pub fn owner(&self) -> &NodeId {
        &self.owner
    }
    pub fn source_fingerprint(&self) -> &str {
        &self.source_fingerprint
    }
    pub fn roots(&self) -> &[MirProofSourceRoot] {
        &self.roots
    }
    pub fn calls(&self) -> &[MirProofSourceCallIdentity] {
        &self.calls
    }
    pub fn defaults(&self) -> &[MirProofSourceDefault] {
        &self.defaults
    }
    pub fn place_inputs(&self) -> &[MirProofSourcePlaceInput] {
        &self.place_inputs
    }
    pub fn materialized_owners(&self) -> &[NodeId] {
        &self.materialized_owners
    }
    pub fn verification_required(&self) -> bool {
        self.verification_required
    }
    pub fn worklist_required(&self) -> bool {
        self.worklist_required
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MirProofSourceDefault {
    parameter: ResolvedParameterId,
    expression: NodeId,
    proof_relevant: bool,
}

impl MirProofSourceDefault {
    pub fn parameter(&self) -> &ResolvedParameterId {
        &self.parameter
    }

    pub fn expression(&self) -> &NodeId {
        &self.expression
    }

    pub fn proof_relevant(&self) -> bool {
        self.proof_relevant
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MirProofSourcePlaceInput {
    point: NodeId,
    expression: NodeId,
}

impl MirProofSourcePlaceInput {
    pub fn point(&self) -> &NodeId {
        &self.point
    }

    pub fn expression(&self) -> &NodeId {
        &self.expression
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirProofSourceFfiParameterMode {
    Value,
    Borrow,
    Move,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MirProofSourceDeclaration {
    declaration: NodeId,
    block: NodeId,
    symbol: String,
    abi: String,
    fixed_parameter_ids: Vec<ResolvedParameterId>,
    fixed_parameter_modes: Vec<MirProofSourceFfiParameterMode>,
    fixed_parameter_types: Vec<ResolvedTypeId>,
    signature_ready: bool,
    result_type: Option<ResolvedTypeId>,
    variadic: bool,
    block_no_panic: bool,
    declaration_no_panic: bool,
    block_returns_errno: bool,
    declaration_returns_errno: bool,
    unsafe_block: bool,
    requires_declared: bool,
    requires_root: Option<NodeId>,
    requires_deferred: bool,
    ensures_declared: bool,
    ensures_root: Option<NodeId>,
    ensures_deferred: bool,
}

impl MirProofSourceDeclaration {
    pub fn declaration(&self) -> &NodeId {
        &self.declaration
    }

    pub fn block(&self) -> &NodeId {
        &self.block
    }

    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    pub fn abi(&self) -> &str {
        &self.abi
    }

    pub fn fixed_parameter_count(&self) -> usize {
        self.fixed_parameter_ids.len()
    }

    pub fn fixed_parameter_ids(&self) -> &[ResolvedParameterId] {
        &self.fixed_parameter_ids
    }

    pub fn fixed_parameter_modes(&self) -> &[MirProofSourceFfiParameterMode] {
        &self.fixed_parameter_modes
    }

    pub fn fixed_parameter_types(&self) -> &[ResolvedTypeId] {
        &self.fixed_parameter_types
    }

    pub fn signature_ready(&self) -> bool {
        self.signature_ready
    }

    pub fn result_type(&self) -> Option<&ResolvedTypeId> {
        self.result_type.as_ref()
    }

    pub fn variadic(&self) -> bool {
        self.variadic
    }

    pub fn block_no_panic(&self) -> bool {
        self.block_no_panic
    }

    pub fn declaration_no_panic(&self) -> bool {
        self.declaration_no_panic
    }

    pub fn block_returns_errno(&self) -> bool {
        self.block_returns_errno
    }

    pub fn declaration_returns_errno(&self) -> bool {
        self.declaration_returns_errno
    }

    pub fn unsafe_block(&self) -> bool {
        self.unsafe_block
    }

    pub fn requires_declared(&self) -> bool {
        self.requires_declared
    }

    pub fn requires_root(&self) -> Option<&NodeId> {
        self.requires_root.as_ref()
    }

    pub fn requires_deferred(&self) -> bool {
        self.requires_deferred
    }

    pub fn ensures_declared(&self) -> bool {
        self.ensures_declared
    }

    pub fn ensures_root(&self) -> Option<&NodeId> {
        self.ensures_root.as_ref()
    }

    pub fn ensures_deferred(&self) -> bool {
        self.ensures_deferred
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofOwnerCensus {
    owner: NodeId,
    source_owner: Option<NodeId>,
    source_fingerprint: String,
    roots: Vec<MirProofSourceRoot>,
    calls: Vec<MirProofSourceCall>,
    defaults: Vec<MirProofSourceDefault>,
    place_inputs: Vec<MirProofSourcePlaceInput>,
    math_points: Vec<MirProofPointId>,
    loop_invariants: Vec<MirProofPointId>,
    expected_worklist: MirProofExpectedWorklist,
    typed_operations: MirProofSourceBlock,
    loop_nodes: Vec<NodeId>,
    direct_contracts: Vec<(NodeId, MirProofSourceRootKind)>,
    worklist_required: bool,
}

impl MirProofOwnerCensus {
    pub fn owner(&self) -> &NodeId {
        &self.owner
    }

    pub fn source_owner(&self) -> Option<&NodeId> {
        self.source_owner.as_ref()
    }

    pub fn roots(&self) -> &[MirProofSourceRoot] {
        &self.roots
    }

    pub fn calls(&self) -> &[MirProofSourceCall] {
        &self.calls
    }

    pub fn body_calls(&self) -> impl Iterator<Item = &MirProofSourceCall> {
        self.calls
            .iter()
            .filter(|call| call.origin == MirProofSourceCallOrigin::Body)
    }

    pub fn defaults(&self) -> &[MirProofSourceDefault] {
        &self.defaults
    }

    pub fn place_inputs(&self) -> &[MirProofSourcePlaceInput] {
        &self.place_inputs
    }

    pub fn math_points(&self) -> &[MirProofPointId] {
        &self.math_points
    }

    pub fn loop_invariants(&self) -> &[MirProofPointId] {
        &self.loop_invariants
    }

    pub fn loop_nodes(&self) -> &[NodeId] {
        &self.loop_nodes
    }

    pub fn direct_contracts(&self) -> &[(NodeId, MirProofSourceRootKind)] {
        &self.direct_contracts
    }

    pub fn requires_worklist(&self) -> bool {
        self.worklist_required
    }

    pub fn required_roots(&self) -> &[MirProofSourceRoot] {
        &self.roots
    }

    pub fn source_is_available(&self) -> bool {
        self.source_owner.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirProofOwnerCoverageGapKind {
    SourceUnavailable,
    MissingMirOwner,
    MissingWorklist {
        required_roots: Vec<MirProofSourceRoot>,
    },
    OwnerMismatch,
    MathCensusMismatch,
    LoopInvariantCensusMismatch,
    LoopCensusMismatch,
    TypedOperationCensusMismatch,
    CallCensusMismatch,
    DefaultCensusMismatch,
    PlaceInputCensusMismatch,
    DirectContractCensusMismatch,
    DeferredBoundary {
        points: Vec<NodeId>,
    },
    InvalidWorklist {
        messages: Vec<String>,
    },
}

/// A normalized projection of the checked typed proof plan. The executable
/// MIR worklist carries physical values and block identities as well, but its
/// source-level operation tree must have exactly this shape and ordering.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MirProofSourceBlock {
    id: NodeId,
    operations: Vec<MirProofSourceOperation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MirProofSourceOperation {
    Bind {
        point: NodeId,
        local_identity: NodeId,
        initializer: NodeId,
    },
    Call(MirProofSourceCallEvent),
    Eval {
        point: NodeId,
        expression: NodeId,
    },
    Assign {
        point: NodeId,
        target: String,
        value: NodeId,
        conversion: String,
    },
    Math(MirProofSourceAssertion),
    Branch {
        point: NodeId,
        expression: NodeId,
        condition: NodeId,
        then_block: MirProofSourceBlock,
        else_block: MirProofSourceBlock,
    },
    Scope(MirProofSourceBlock),
    Loop {
        point: NodeId,
        condition: NodeId,
        condition_calls: Vec<MirProofSourceCallEvent>,
        invariants: Vec<MirProofSourceAssertion>,
        body: MirProofSourceBlock,
        stationary: bool,
        receipt: Option<String>,
    },
    Break {
        point: NodeId,
        target_loop: Option<NodeId>,
        value: Option<NodeId>,
    },
    Continue {
        point: NodeId,
        target_loop: Option<NodeId>,
    },
    Return {
        point: NodeId,
        value: Option<NodeId>,
        conversion: Option<String>,
    },
    Exit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MirProofSourceAssertion {
    id: MirProofPointId,
    order: u64,
    scope: Vec<MirProofSourceScopeFrame>,
    expression: NodeId,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirProofSourceScopeFrame {
    Block(NodeId),
    Branch {
        statement: NodeId,
        expression: NodeId,
        then_arm: bool,
    },
    Loop(NodeId),
    Default {
        parameter: ResolvedParameterId,
        expression: NodeId,
    },
    PlaceInput(NodeId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MirProofSourceCallArgumentEvent {
    ordinal: usize,
    parameter: ResolvedParameterId,
    expression: NodeId,
    source_type: ResolvedTypeId,
    conversion: CheckedConversion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MirProofSourceCallEvent {
    body_owner: NodeId,
    owner: NodeId,
    statement: NodeId,
    call_node: NodeId,
    order: u64,
    scope: Vec<MirProofSourceScopeFrame>,
    guards: Vec<(NodeId, bool)>,
    disposition: TypedCallExecutionDisposition,
    expression: NodeId,
    expression_type: ResolvedTypeId,
    callee: ResolvedCallee,
    type_arguments: Vec<ResolvedTypeId>,
    result_type: ResolvedTypeId,
    effects: Vec<EffectId>,
    effect_authority: TypedCallEffectAuthority,
    arguments: Vec<MirProofSourceCallArgumentEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MirProofSourceCallMetadata {
    source_origin: Origin,
    expression_type: ResolvedTypeId,
    type_arguments: Vec<ResolvedTypeId>,
    result_type: ResolvedTypeId,
    effects: Vec<EffectId>,
    /// Canonical debug representation of the resolved call shape. The typed
    /// FFI context includes full `ResolvedArgument` expressions, while the
    /// compact public receipt stores only source identities.
    call_shape_debug: String,
    arguments: Vec<MirProofSourceCallArgument>,
}

#[derive(Debug, Clone)]
struct MirProofSourceCallContext {
    owner: NodeId,
    statement: NodeId,
    order: u64,
    scope: Vec<MirProofSourceScopeFrame>,
    guards: Vec<(NodeId, bool)>,
    disposition: TypedCallExecutionDisposition,
    effect_authority: TypedCallEffectAuthority,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MirProofExpectedWorklist {
    calls: Vec<super::MirProofCallPointId>,
    deferred_calls: Vec<super::MirProofCallBoundary>,
    scoped_boundaries: Vec<NodeId>,
    default_roots: Vec<super::MirProofDefaultRootId>,
    default_math_points: Vec<super::MirProofBoundaryMathId>,
    default_calls: Vec<super::MirProofCallBoundary>,
    default_boundaries: Vec<super::MirProofDefaultBoundary>,
    default_uses: Vec<super::MirProofDefaultUseId>,
    place_inputs: Vec<super::MirProofPlaceInputId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofOwnerCoverageGap {
    pub owner: NodeId,
    pub kind: MirProofOwnerCoverageGapKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofCensusIntegrityError {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirProofCensusError {
    GlobalIntegrity(MirProofCensusIntegrityError),
    OwnerCoverageGap(MirProofOwnerCoverageGap),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirProofWorklistCoverage {
    NotRequired,
    Present,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CensusAuthority {
    CheckedSource,
    Unavailable,
}

/// Private and nonoptional on `MirProgram`. The fields, including the seal,
/// are intentionally not public so consumer code cannot erase or rewrite the
/// independently derived source census.
#[derive(Debug, Clone)]
pub struct MirProofSourceCensus {
    authority: CensusAuthority,
    function_owners: Vec<NodeId>,
    owners: BTreeMap<NodeId, MirProofOwnerCensus>,
    checked_callables: BTreeMap<NodeId, MirProofSourceCallableCensus>,
    declarations: Vec<MirProofSourceDeclaration>,
    checked_source_fingerprint: String,
    witness: String,
}

impl Default for MirProofSourceCensus {
    fn default() -> Self {
        Self::unavailable(&BTreeMap::new())
    }
}

impl MirProofSourceCensus {
    pub(crate) fn unavailable(functions: &BTreeMap<NodeId, super::MirFunction>) -> Self {
        let owners = functions
            .iter()
            .map(|(owner, function)| {
                let roots = function
                    .contracts
                    .iter()
                    .map(|contract| MirProofSourceRoot {
                        kind: match contract.kind {
                            super::MirContractKind::Requires => MirProofSourceRootKind::Requires,
                            super::MirContractKind::Ensures => MirProofSourceRootKind::Ensures,
                            super::MirContractKind::Invariant => MirProofSourceRootKind::Invariant,
                        },
                        node: contract.id.clone(),
                        identity: "unsealed-mir-contract".into(),
                    })
                    .collect::<Vec<_>>();
                (
                    owner.clone(),
                    MirProofOwnerCensus {
                        owner: owner.clone(),
                        source_owner: None,
                        source_fingerprint: String::new(),
                        roots,
                        calls: Vec::new(),
                        defaults: Vec::new(),
                        place_inputs: Vec::new(),
                        math_points: Vec::new(),
                        loop_invariants: Vec::new(),
                        expected_worklist: MirProofExpectedWorklist::empty(),
                        typed_operations: MirProofSourceBlock {
                            id: function
                                .proof_worklist
                                .as_ref()
                                .map(|worklist| worklist.root.id.clone())
                                .unwrap_or_else(|| NodeId("unavailable-proof-root".into())),
                            operations: Vec::new(),
                        },
                        loop_nodes: Vec::new(),
                        direct_contracts: Vec::new(),
                        worklist_required: !function.contracts.is_empty()
                            || function.proof_worklist.is_some(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        Self::seal(
            CensusAuthority::Unavailable,
            functions.keys().cloned().collect(),
            owners,
            BTreeMap::new(),
            Vec::new(),
            String::new(),
        )
    }

    pub(crate) fn checked(
        program: &CheckedProgram,
        functions: &BTreeMap<NodeId, super::MirFunction>,
        instances: &BTreeMap<super::MirInstanceId, MirInstance>,
        excluded_sources: Option<&std::collections::HashSet<crate::span::SourceId>>,
    ) -> Self {
        let mut declarations = program
            .ffi_declarations()
            .iter()
            .map(|(identity, declaration)| {
                let (signature_ready, fixed_parameter_types, result_type): (
                    bool,
                    Vec<ResolvedTypeId>,
                    Option<ResolvedTypeId>,
                ) = match &declaration.signature {
                    Ok(signature) => (
                        true,
                        signature
                            .parameters
                            .iter()
                            .map(|parameter| parameter.ty.clone())
                            .collect(),
                        Some(signature.result.clone()),
                    ),
                    Err(_) => (false, Vec::new(), None),
                };
                MirProofSourceDeclaration {
                    declaration: identity.clone(),
                    block: declaration.block.clone(),
                    symbol: declaration.symbol.clone(),
                    abi: declaration.abi.clone(),
                    fixed_parameter_ids: declaration.fixed_parameter_ids.clone(),
                    fixed_parameter_modes: declaration
                        .fixed_parameter_modes
                        .iter()
                        .map(|mode| match mode {
                            ResolvedFfiParameterMode::Value => {
                                MirProofSourceFfiParameterMode::Value
                            }
                            ResolvedFfiParameterMode::Borrow => {
                                MirProofSourceFfiParameterMode::Borrow
                            }
                            ResolvedFfiParameterMode::Move => MirProofSourceFfiParameterMode::Move,
                        })
                        .collect(),
                    fixed_parameter_types,
                    signature_ready,
                    result_type,
                    variadic: declaration.invocation.variadic,
                    block_no_panic: declaration.invocation.block_no_panic,
                    declaration_no_panic: declaration.invocation.declaration_no_panic,
                    block_returns_errno: declaration.invocation.block_returns_errno,
                    declaration_returns_errno: declaration.invocation.declaration_returns_errno,
                    unsafe_block: declaration.invocation.unsafe_block,
                    requires_declared: declaration.requires.is_declared(),
                    requires_root: ffi_root(&declaration.requires),
                    requires_deferred: matches!(
                        &declaration.requires,
                        ResolvedFfiContractRoot::Deferred { .. }
                    ),
                    ensures_declared: declaration.ensures.is_declared(),
                    ensures_root: ffi_root(&declaration.ensures),
                    ensures_deferred: matches!(
                        &declaration.ensures,
                        ResolvedFfiContractRoot::Deferred { .. }
                    ),
                }
            })
            .collect::<Vec<_>>();
        declarations.sort_by(|left, right| left.declaration.cmp(&right.declaration));

        let mut source_for_owner = BTreeMap::<NodeId, NodeId>::new();
        for owner in functions.keys() {
            if program.callable(owner).is_some() {
                source_for_owner.insert(owner.clone(), owner.clone());
            }
        }
        for instance in instances.values() {
            if functions.contains_key(&instance.function)
                && program.callable(&instance.template).is_some()
            {
                source_for_owner.insert(instance.function.clone(), instance.template.clone());
            }
        }
        let mut materialized_by_source = BTreeMap::<NodeId, Vec<NodeId>>::new();
        for (mir_owner, source_owner) in &source_for_owner {
            materialized_by_source
                .entry(source_owner.clone())
                .or_default()
                .push(mir_owner.clone());
        }
        for materialized in materialized_by_source.values_mut() {
            materialized.sort();
            materialized.dedup();
        }
        // This inventory deliberately iterates the entire Checker callable
        // directory, not merely the MIR lowering's reachability-selected
        // function map. It therefore retains dormant/unreachable and generic
        // callables even when this particular MIR graph has no function body.
        let checked_callables = program
            .callables()
            .iter()
            .filter(|(_, callable)| {
                !excluded_sources.is_some_and(|excluded| {
                    excluded.contains(&callable.body.root.origin.user_span().source_id)
                })
            })
            .map(|(owner, callable)| {
                let materialized = materialized_by_source
                    .get(owner)
                    .cloned()
                    .unwrap_or_default();
                (
                    owner.clone(),
                    source_callable_inventory(callable, program, materialized),
                )
            })
            .collect::<BTreeMap<_, _>>();

        let mut owners = BTreeMap::new();
        for (owner, function) in functions {
            let source_owner = source_for_owner.get(owner).cloned();
            let Some(source_callable) = source_owner
                .as_ref()
                .and_then(|source_owner| program.callable(source_owner))
            else {
                let mut unavailable =
                    Self::unavailable(&BTreeMap::from([(owner.clone(), function.clone())]));
                let mut owner_census = unavailable
                    .owners
                    .remove(owner)
                    .expect("one unavailable owner was just inserted");
                owner_census.owner = owner.clone();
                owners.insert(owner.clone(), owner_census);
                continue;
            };
            if excluded_sources.is_some_and(|excluded| {
                excluded.contains(&source_callable.body.root.origin.user_span().source_id)
            }) {
                // A function that survived source exclusion without a source
                // owner is a structural integrity issue, represented as an
                // unavailable owner and handled as a per-owner coverage gap.
                let unavailable =
                    Self::unavailable(&BTreeMap::from([(owner.clone(), function.clone())]));
                owners.insert(
                    owner.clone(),
                    unavailable.owners.get(owner).unwrap().clone(),
                );
                continue;
            }

            let mut roots = BTreeSet::new();
            let mut direct_contracts = Vec::new();
            for contract in source_callable.body.contract_proof_roots() {
                let kind = match contract.kind {
                    ContractKind::Requires => MirProofSourceRootKind::Requires,
                    ContractKind::Ensures => MirProofSourceRootKind::Ensures,
                    ContractKind::Invariant => MirProofSourceRootKind::Invariant,
                };
                roots.insert(MirProofSourceRoot {
                    kind,
                    node: contract.node_id.clone(),
                    identity: format!("{}", contract.condition.node_id.0),
                });
                if source_callable
                    .body
                    .root
                    .statements
                    .iter()
                    .any(|statement| statement.node_id == contract.node_id)
                {
                    direct_contracts.push((contract.node_id, kind));
                }
            }
            let math_roots = source_callable.body.math_proof_roots();
            for (node, _) in &math_roots {
                roots.insert(MirProofSourceRoot {
                    kind: MirProofSourceRootKind::Math,
                    node: node.clone(),
                    identity: "math-statement".into(),
                });
            }

            // CFG loop edge sources are checker-owned stable statement ids.
            // This census is independent of whether the optional proof
            // worklist can encode that loop.
            let loop_nodes = source_callable
                .cfg
                .edges
                .values()
                .filter(|edge| {
                    matches!(
                        edge.kind,
                        crate::core::cfg::EdgeKind::LoopBody
                            | crate::core::cfg::EdgeKind::LoopExit
                            | crate::core::cfg::EdgeKind::Backedge
                    )
                })
                .map(|edge| edge.source.node.clone())
                .collect::<BTreeSet<_>>();
            for node in &loop_nodes {
                roots.insert(MirProofSourceRoot {
                    kind: MirProofSourceRootKind::Loop,
                    node: node.clone(),
                    identity: "cfg-loop-edge-source".into(),
                });
            }

            let typed = source_callable.proof_point_worklist();
            let mut proof_root_statements = source_callable
                .body
                .contract_proof_roots()
                .into_iter()
                .map(|contract| contract.node_id)
                .collect::<BTreeSet<_>>();
            proof_root_statements.extend(math_roots.iter().map(|(node, _)| node.clone()));
            let mut typed_call_contexts = BTreeMap::<NodeId, Vec<MirProofSourceCallContext>>::new();
            let typed_call_points = typed
                .calls
                .iter()
                .chain(&typed.deferred_calls)
                .chain(
                    typed
                        .default_roots
                        .iter()
                        .flat_map(|root| root.calls.iter()),
                )
                .collect::<Vec<_>>();
            let mut typed_call_keys = BTreeSet::new();
            let mut call_directory_matches = true;
            for point in &typed_call_points {
                let origin =
                    source_call_origin(point, &source_callable.owner, &proof_root_statements);
                if !typed_call_keys.insert((origin, point.call_node.clone())) {
                    call_directory_matches = false;
                }
                typed_call_contexts
                    .entry(point.call_node.clone())
                    .or_default()
                    .push(source_call_context(point));
            }

            let mut body_call_nodes = BTreeMap::new();
            let mut nested_lambda_call_nodes = BTreeMap::new();
            let mut body_call_arguments = BTreeMap::new();
            collect_block_calls(
                &source_callable.body.root,
                &mut body_call_nodes,
                &mut nested_lambda_call_nodes,
                &mut body_call_arguments,
                false,
            );
            let mut call_occurrences =
                BTreeMap::<(MirProofSourceCallOrigin, NodeId), ResolvedCallee>::new();
            let mut call_metadata_occurrences =
                BTreeMap::<(MirProofSourceCallOrigin, NodeId), MirProofSourceCallMetadata>::new();
            let mut call_context_occurrences =
                BTreeMap::<(MirProofSourceCallOrigin, NodeId), MirProofSourceCallContext>::new();
            for (node, callee) in &body_call_nodes {
                let candidates = typed_call_contexts.get(node).cloned().unwrap_or_default();
                let matching_context = candidates.first();
                if candidates.len() > 1 {
                    call_directory_matches = false;
                }
                let origin = if let Some(context) = matching_context {
                    if context.owner != source_callable.owner {
                        MirProofSourceCallOrigin::Lambda(context.owner.clone())
                    } else if proof_root_statements.contains(&context.statement) {
                        MirProofSourceCallOrigin::ProofRoot(context.statement.clone())
                    } else {
                        MirProofSourceCallOrigin::Body
                    }
                } else {
                    MirProofSourceCallOrigin::Body
                };
                let key = (origin, node.clone());
                call_occurrences.insert(key.clone(), callee.clone());
                if let Some(metadata) = body_call_arguments.remove(node) {
                    call_metadata_occurrences.insert(key.clone(), metadata);
                } else {
                    call_directory_matches = false;
                }
                if let Some(context) = matching_context {
                    call_context_occurrences.insert(key, context.clone());
                } else {
                    call_directory_matches = false;
                }
            }
            for (node, callee) in &nested_lambda_call_nodes {
                let context = typed_call_contexts.get(node).and_then(|contexts| {
                    contexts
                        .iter()
                        .find(|context| context.owner != source_callable.owner)
                });
                let origin = MirProofSourceCallOrigin::Lambda(
                    context
                        .map(|context| context.owner.clone())
                        .unwrap_or_else(|| source_callable.owner.clone()),
                );
                let key = (origin, node.clone());
                call_occurrences.insert(key.clone(), callee.clone());
                if let Some(metadata) = body_call_arguments.remove(node) {
                    call_metadata_occurrences.insert(key.clone(), metadata);
                } else {
                    call_directory_matches = false;
                }
                if let Some(context) = context {
                    call_context_occurrences.insert(key, context.clone());
                } else {
                    call_directory_matches = false;
                }
            }
            for (parameter, expression) in &source_callable.body.default_values {
                let mut calls = BTreeMap::new();
                let mut nested = BTreeMap::new();
                let mut arguments = BTreeMap::new();
                collect_expr_calls(expression, &mut calls, &mut nested, &mut arguments, false);
                for (node, callee) in calls {
                    let key = (
                        MirProofSourceCallOrigin::Default(parameter.clone()),
                        node.clone(),
                    );
                    call_occurrences.insert(key.clone(), callee);
                    if let Some(metadata) = arguments.remove(&node) {
                        call_metadata_occurrences.insert(key.clone(), metadata);
                    }
                    let context = typed_call_contexts.get(&node).and_then(|contexts| {
                        contexts.iter().find(|context| {
                            context.scope.iter().any(|frame| {
                                matches!(frame, MirProofSourceScopeFrame::Default { parameter: candidate, .. } if candidate == parameter)
                            })
                        })
                    });
                    if let Some(context) = context {
                        call_context_occurrences.insert(key, context.clone());
                    } else {
                        call_directory_matches = false;
                    }
                }
                for (node, callee) in nested {
                    nested_lambda_call_nodes.insert(node.clone(), callee.clone());
                    let key = (
                        MirProofSourceCallOrigin::Default(parameter.clone()),
                        node.clone(),
                    );
                    call_occurrences.insert(key.clone(), callee);
                    if let Some(metadata) = arguments.remove(&node) {
                        call_metadata_occurrences.insert(key.clone(), metadata);
                    }
                    let context = typed_call_contexts.get(&node).and_then(|contexts| {
                        contexts.iter().find(|context| {
                            context.scope.iter().any(|frame| {
                                matches!(frame, MirProofSourceScopeFrame::Default { parameter: candidate, .. } if candidate == parameter)
                            })
                        })
                    });
                    if let Some(context) = context {
                        call_context_occurrences.insert(key, context.clone());
                    } else {
                        call_directory_matches = false;
                    }
                }
            }
            for (point, expression) in &source_callable.body.place_inputs {
                let mut calls = BTreeMap::new();
                let mut nested = BTreeMap::new();
                let mut arguments = BTreeMap::new();
                collect_expr_calls(expression, &mut calls, &mut nested, &mut arguments, false);
                for (node, callee) in calls {
                    let key = (
                        MirProofSourceCallOrigin::PlaceInput(point.clone()),
                        node.clone(),
                    );
                    call_occurrences.insert(key.clone(), callee);
                    if let Some(metadata) = arguments.remove(&node) {
                        call_metadata_occurrences.insert(key.clone(), metadata);
                    }
                    let context = typed_call_contexts.get(&node).and_then(|contexts| {
                        contexts.iter().find(|context| {
                            context.scope.iter().any(|frame| {
                                matches!(frame, MirProofSourceScopeFrame::PlaceInput(candidate) if candidate == point)
                            })
                        })
                    });
                    if let Some(context) = context {
                        call_context_occurrences.insert(key, context.clone());
                    } else {
                        call_directory_matches = false;
                    }
                }
                for (node, callee) in nested {
                    nested_lambda_call_nodes.insert(node.clone(), callee.clone());
                    let key = (
                        MirProofSourceCallOrigin::PlaceInput(point.clone()),
                        node.clone(),
                    );
                    call_occurrences.insert(key.clone(), callee);
                    if let Some(metadata) = arguments.remove(&node) {
                        call_metadata_occurrences.insert(key.clone(), metadata);
                    }
                    let context = typed_call_contexts.get(&node).and_then(|contexts| {
                        contexts.iter().find(|context| {
                            context.scope.iter().any(|frame| {
                                matches!(frame, MirProofSourceScopeFrame::PlaceInput(candidate) if candidate == point)
                            })
                        })
                    });
                    if let Some(context) = context {
                        call_context_occurrences.insert(key, context.clone());
                    } else {
                        call_directory_matches = false;
                    }
                }
            }
            // The independently materialized identity directory also carries
            // dormant declaration-owned calls that a consumer may not
            // execute. Preserve the union and flag any mismatch with the
            // actual typed expression tree as a scoped proof boundary.
            for (node, callee) in &source_callable.body.resolved_calls {
                if let Some(typed_callee) = body_call_nodes.get(node) {
                    if typed_callee != callee {
                        call_directory_matches = false;
                    }
                } else if nested_lambda_call_nodes.get(node) == Some(callee) {
                    // Lambda expressions have their own checked callable
                    // owner. The enclosing body validator also walks them,
                    // but their calls do not execute in this owner's body.
                } else if call_occurrences
                    .iter()
                    .any(|((_, source_node), source_callee)| {
                        source_node == node && source_callee == callee
                    })
                {
                    // Declaration defaults and dynamic place inputs are
                    // separately censused origins, not executable body
                    // calls. Their source entries still discharge this
                    // ResolvedBody-wide identity directory member.
                } else {
                    // Preserve an identity-directory call that could not be
                    // rebound to a known expression path, and force the
                    // owner to fail closed instead of trimming it.
                    call_directory_matches = false;
                    let key = (MirProofSourceCallOrigin::Body, node.clone());
                    call_occurrences.insert(key, callee.clone());
                }
            }
            for (node, callee) in &body_call_nodes {
                if source_callable.body.resolved_calls.get(node) != Some(callee) {
                    call_directory_matches = false;
                }
            }
            let source_call_keys = call_occurrences.keys().cloned().collect::<BTreeSet<_>>();
            if source_call_keys != typed_call_keys {
                call_directory_matches = false;
            }
            let mut calls = call_occurrences
                .iter()
                .map(|((origin, call_node), callee)| {
                    let key = (origin.clone(), call_node.clone());
                    let metadata = call_metadata_occurrences.remove(&key);
                    let context = call_context_occurrences.remove(&key);
                    MirProofSourceCall {
                        origin: origin.clone(),
                        body_owner: source_callable.owner.clone(),
                        owner: context.as_ref().map_or_else(
                            || source_callable.owner.clone(),
                            |context| context.owner.clone(),
                        ),
                        statement: context
                            .as_ref()
                            .map_or_else(|| call_node.clone(), |context| context.statement.clone()),
                        order: context.as_ref().map(|context| context.order),
                        scope: context
                            .as_ref()
                            .map_or_else(Vec::new, |context| context.scope.clone()),
                        guards: context
                            .as_ref()
                            .map_or_else(Vec::new, |context| context.guards.clone()),
                        disposition: context.as_ref().map_or_else(
                            || {
                                TypedCallExecutionDisposition::Deferred(
                                    "source call has no matching typed call event".into(),
                                )
                            },
                            |context| context.disposition.clone(),
                        ),
                        call_node: call_node.clone(),
                        callee: callee.clone(),
                        callee_identity: format!("{callee:?}"),
                        selected_signature: selected_signature_identity(
                            callee,
                            metadata.as_ref(),
                            program,
                        ),
                        expression_type: metadata
                            .as_ref()
                            .map(|metadata| metadata.expression_type.clone()),
                        type_arguments: metadata
                            .as_ref()
                            .map(|metadata| metadata.type_arguments.clone()),
                        result_type: metadata
                            .as_ref()
                            .map(|metadata| metadata.result_type.clone()),
                        effects: metadata.as_ref().map(|metadata| metadata.effects.clone()),
                        effect_authority: context
                            .as_ref()
                            .map_or(TypedCallEffectAuthority::UnknownMayReadWrite, |context| {
                                context.effect_authority
                            }),
                        requires_declared: callee_requires_declared(callee, program),
                        ensures_declared: callee_ensures_declared(callee, program),
                        source_origin: metadata
                            .as_ref()
                            .map(|metadata| metadata.source_origin.clone()),
                        arguments: metadata.map_or_else(Vec::new, |metadata| metadata.arguments),
                    }
                })
                .collect::<Vec<_>>();
            calls.sort_by(|left, right| {
                left.origin
                    .cmp(&right.origin)
                    .then_with(|| left.call_node.cmp(&right.call_node))
            });
            for call in &calls {
                roots.insert(MirProofSourceRoot {
                    kind: MirProofSourceRootKind::Call,
                    node: call.call_node.clone(),
                    identity: call.callee_identity.clone(),
                });
            }
            if !call_directory_matches {
                roots.insert(MirProofSourceRoot {
                    kind: MirProofSourceRootKind::Boundary,
                    node: source_callable.body.root.node_id.clone(),
                    identity:
                        "ResolvedBody call tree disagrees with the checker call-node directory"
                            .into(),
                });
            }

            let mut defaults = source_callable
                .body
                .default_values
                .iter()
                .map(|(parameter, expression)| MirProofSourceDefault {
                    parameter: parameter.clone(),
                    expression: expression.node_id.clone(),
                    proof_relevant: expression_has_proof_roots(expression)
                        || expression_has_required_call(expression, program),
                })
                .collect::<Vec<_>>();
            defaults.sort();
            for default in &defaults {
                roots.insert(MirProofSourceRoot {
                    kind: MirProofSourceRootKind::DefaultValue,
                    node: default.expression.clone(),
                    identity: default.parameter.0 .0.clone(),
                });
                let default_has_exact_consumer = typed.default_roots.iter().any(|root| {
                    root.owner == source_callable.owner
                        && root.parameter == default.parameter
                        && root.expression == default.expression
                        && root.supports_local_only_profile(source_callable)
                });
                if default.proof_relevant() && !default_has_exact_consumer {
                    roots.insert(MirProofSourceRoot {
                        kind: MirProofSourceRootKind::Boundary,
                        node: default.expression.clone(),
                        identity: "declaration default has proof obligations without an exact MIR default-use receipt".into(),
                    });
                }
            }
            let mut place_inputs = source_callable
                .body
                .place_inputs
                .iter()
                .map(|(point, expression)| MirProofSourcePlaceInput {
                    point: point.clone(),
                    expression: expression.node_id.clone(),
                })
                .collect::<Vec<_>>();
            place_inputs.sort();
            for place_input in &place_inputs {
                roots.insert(MirProofSourceRoot {
                    kind: MirProofSourceRootKind::PlaceInput,
                    node: place_input.expression.clone(),
                    identity: place_input.point.0.clone(),
                });
                if source_callable
                    .body
                    .place_inputs
                    .get(&place_input.point)
                    .is_some_and(|expression| {
                        expression_has_proof_roots(expression)
                            || expression_has_required_call(expression, program)
                    })
                {
                    roots.insert(MirProofSourceRoot {
                        kind: MirProofSourceRootKind::Boundary,
                        node: place_input.expression.clone(),
                        identity: "place input has proof obligations without an exact MIR place-input receipt".into(),
                    });
                }
            }
            let proof_relevant_place_input =
                source_callable
                    .body
                    .place_inputs
                    .values()
                    .any(|expression| {
                        expression_has_proof_roots(expression)
                            || expression_has_required_call(expression, program)
                    });

            let typed_operations = source_proof_block(&typed.root);
            let typed_loop_nodes = typed.loop_nodes().into_iter().collect::<BTreeSet<_>>();
            if typed_loop_nodes != loop_nodes
                && (has_direct_ensures_contract(source_callable)
                    || !math_roots.is_empty()
                    || !typed.points.is_empty()
                    || !typed.loop_invariant_points().is_empty())
            {
                for missing in loop_nodes.difference(&typed_loop_nodes) {
                    roots.insert(MirProofSourceRoot {
                        kind: MirProofSourceRootKind::Boundary,
                        node: missing.clone(),
                        identity: "proof-relevant loop is absent from the typed loop worklist"
                            .into(),
                    });
                }
                for extra in typed_loop_nodes.difference(&loop_nodes) {
                    roots.insert(MirProofSourceRoot {
                        kind: MirProofSourceRootKind::Boundary,
                        node: extra.clone(),
                        identity: "typed loop worklist node has no checker CFG loop".into(),
                    });
                }
            }
            for boundary in &typed.boundaries {
                roots.insert(MirProofSourceRoot {
                    kind: MirProofSourceRootKind::Boundary,
                    node: boundary.point.clone(),
                    identity: boundary.reason.clone(),
                });
            }
            if source_callable.has_unmodeled_proof_roots() {
                roots.insert(MirProofSourceRoot {
                    kind: MirProofSourceRootKind::Boundary,
                    node: source_callable.body.root.node_id.clone(),
                    identity:
                        "checked source census contains proof roots absent from the typed worklist"
                            .into(),
                });
            }
            let math_points = typed
                .points
                .iter()
                .map(|point| mir_point_id(&point.id))
                .collect::<Vec<_>>();
            let loop_invariants = typed
                .loop_invariant_points()
                .iter()
                .map(|point| mir_point_id(&point.id))
                .collect::<Vec<_>>();

            let direct_contract_ids = direct_contracts
                .iter()
                .map(|(node, _)| node.clone())
                .collect::<BTreeSet<_>>();
            let has_scoped_contract = roots.iter().any(|root| {
                matches!(
                    root.kind,
                    MirProofSourceRootKind::Requires
                        | MirProofSourceRootKind::Ensures
                        | MirProofSourceRootKind::Invariant
                ) && !direct_contract_ids.contains(&root.node)
            });
            let has_math = !math_roots.is_empty();
            let has_loop_invariants = !typed.loop_invariant_points().is_empty();
            let has_direct_ensures = direct_contracts
                .iter()
                .any(|(_, kind)| *kind == MirProofSourceRootKind::Ensures);
            let proof_relevant_default = defaults.iter().any(MirProofSourceDefault::proof_relevant);
            // Derive worklist presence from checked source roots rather than
            // from the optional worklist. Direct call preconditions and
            // post-call Ensures remain in the independent source call census
            // and are discharged by the call consumer.
            let body_proof_requested = has_math
                || has_loop_invariants
                || has_scoped_contract
                || (has_direct_ensures && !loop_nodes.is_empty())
                || proof_relevant_default
                || proof_relevant_place_input
                || source_callable.has_unmodeled_proof_roots();
            let worklist_required = body_proof_requested;

            let source_fingerprint = blake3::hash(format!("{source_callable:#?}").as_bytes())
                .to_hex()
                .to_string();
            owners.insert(
                owner.clone(),
                MirProofOwnerCensus {
                    owner: owner.clone(),
                    source_owner,
                    source_fingerprint,
                    roots: roots.into_iter().collect(),
                    calls,
                    defaults,
                    place_inputs,
                    math_points,
                    loop_invariants,
                    expected_worklist: expected_worklist_from_typed(
                        &typed,
                        &source_callable.body.place_inputs,
                    ),
                    typed_operations,
                    loop_nodes: loop_nodes.into_iter().collect(),
                    direct_contracts,
                    worklist_required,
                },
            );
        }
        let source_fingerprints: BTreeMap<NodeId, String> = checked_callables
            .iter()
            .map(|(owner, entry)| (owner.clone(), entry.source_fingerprint.clone()))
            .collect();
        let checked_source_material = format!(
            "callables={source_fingerprints:#?}\ndeclarations={declarations:#?}\ninventory={checked_callables:#?}"
        );
        let checked_source_fingerprint = blake3::hash(checked_source_material.as_bytes())
            .to_hex()
            .to_string();
        Self::seal(
            CensusAuthority::CheckedSource,
            functions.keys().cloned().collect(),
            owners,
            checked_callables,
            declarations,
            checked_source_fingerprint,
        )
    }

    fn seal(
        authority: CensusAuthority,
        function_owners: Vec<NodeId>,
        owners: BTreeMap<NodeId, MirProofOwnerCensus>,
        checked_callables: BTreeMap<NodeId, MirProofSourceCallableCensus>,
        declarations: Vec<MirProofSourceDeclaration>,
        checked_source_fingerprint: String,
    ) -> Self {
        let mut census = Self {
            authority,
            function_owners,
            owners,
            checked_callables,
            declarations,
            checked_source_fingerprint,
            witness: String::new(),
        };
        census.witness = census.compute_witness();
        census
    }

    pub fn owner(&self, owner: &NodeId) -> Option<&MirProofOwnerCensus> {
        self.owners.get(owner)
    }

    pub fn owners(&self) -> impl Iterator<Item = &MirProofOwnerCensus> {
        self.owners.values()
    }

    pub fn declarations(&self) -> &[MirProofSourceDeclaration] {
        &self.declarations
    }

    pub fn declaration(&self, declaration: &NodeId) -> Option<&MirProofSourceDeclaration> {
        self.declarations
            .binary_search_by(|candidate| candidate.declaration.cmp(declaration))
            .ok()
            .map(|index| &self.declarations[index])
    }

    /// Every in-scope Checker callable, including callable bodies omitted from
    /// this MIR graph by reachability or specialization selection.
    pub fn checked_callables(&self) -> impl Iterator<Item = &MirProofSourceCallableCensus> {
        self.checked_callables.values()
    }

    pub fn checked_callable(&self, owner: &NodeId) -> Option<&MirProofSourceCallableCensus> {
        self.checked_callables.get(owner)
    }

    /// Source owners with proof work that have no materialized MIR function.
    /// Verifiers should turn each into an owner-scoped NITS with no artifact.
    pub fn missing_mir_verification_owners(&self) -> Vec<NodeId> {
        self.checked_callables
            .values()
            .filter(|entry| entry.materialized_owners.is_empty() && entry.verification_required)
            .map(|entry| entry.owner.clone())
            .collect()
    }

    pub fn requires_worklist(&self, owner: &NodeId) -> bool {
        self.owners
            .get(owner)
            .map(MirProofOwnerCensus::requires_worklist)
            .or_else(|| {
                self.checked_callables
                    .get(owner)
                    .map(MirProofSourceCallableCensus::worklist_required)
            })
            .unwrap_or(true)
    }

    pub fn requires_verification(&self, owner: &NodeId) -> bool {
        self.owners
            .get(owner)
            .and_then(|entry| {
                entry
                    .source_owner
                    .as_ref()
                    .and_then(|source_owner| self.checked_callables.get(source_owner))
                    .map(MirProofSourceCallableCensus::verification_required)
                    .or_else(|| {
                        Some(
                            entry.worklist_required
                                || !entry.direct_contracts.is_empty()
                                || !entry.math_points.is_empty()
                                || !entry.loop_invariants.is_empty(),
                        )
                    })
            })
            .or_else(|| {
                self.checked_callables
                    .get(owner)
                    .map(MirProofSourceCallableCensus::verification_required)
            })
            .unwrap_or(false)
    }

    pub fn canonical_text(&self) -> String {
        fn atom(output: &mut String, value: &str) {
            use std::fmt::Write as _;
            let _ = write!(output, "{}:", value.len());
            output.push_str(value);
        }
        let mut output = String::from("mir.proof-source-census.v3\n");
        atom(
            &mut output,
            match self.authority {
                CensusAuthority::CheckedSource => "checked",
                CensusAuthority::Unavailable => "unavailable",
            },
        );
        output.push('\n');
        for (owner, entry) in &self.owners {
            output.push_str("  owner=");
            atom(&mut output, &owner.0);
            output.push_str(" source-owner=");
            atom(
                &mut output,
                entry
                    .source_owner
                    .as_ref()
                    .map_or("unavailable", |id| id.0.as_str()),
            );
            output.push_str(" required=");
            atom(
                &mut output,
                if entry.worklist_required {
                    "true"
                } else {
                    "false"
                },
            );
            output.push('\n');
            for root in &entry.roots {
                output.push_str("    root=");
                atom(&mut output, root_kind_text(root.kind));
                atom(&mut output, &root.node.0);
                atom(&mut output, &root.identity);
                output.push('\n');
            }
            for call in &entry.calls {
                output.push_str("    call=");
                atom(&mut output, &format!("{:?}", call.origin));
                atom(&mut output, &call.body_owner.0);
                atom(&mut output, &call.owner.0);
                atom(&mut output, &call.statement.0);
                atom(
                    &mut output,
                    &call
                        .order
                        .map_or_else(|| "unbound".into(), |order| order.to_string()),
                );
                atom(&mut output, &call.call_node.0);
                atom(&mut output, &call.callee_identity);
                atom(&mut output, &call.selected_signature);
                atom(&mut output, &format!("{:?}", call.disposition));
                atom(&mut output, &format!("{:?}", call.scope));
                atom(&mut output, &format!("{:?}", call.guards));
                atom(&mut output, &format!("{:?}", call.effect_authority));
                atom(
                    &mut output,
                    if call.requires_declared {
                        "requires"
                    } else {
                        "no-requires"
                    },
                );
                atom(
                    &mut output,
                    if call.ensures_declared {
                        "ensures"
                    } else {
                        "no-ensures"
                    },
                );
                atom(&mut output, &format!("{:?}", call.expression_type));
                atom(&mut output, &format!("{:?}", call.type_arguments));
                atom(&mut output, &format!("{:?}", call.result_type));
                atom(&mut output, &format!("{:?}", call.effects));
                output.push('\n');
                for argument in &call.arguments {
                    output.push_str("      call-argument=");
                    atom(&mut output, &argument.index.to_string());
                    atom(&mut output, &argument.parameter.0 .0);
                    atom(&mut output, &argument.source_node.0);
                    atom(&mut output, argument.source_type.as_str());
                    atom(&mut output, &format!("{:?}", argument.conversion));
                    atom(&mut output, argument.conversion_from.as_str());
                    atom(&mut output, argument.target_type.as_str());
                    output.push('\n');
                }
            }
            for default in &entry.defaults {
                output.push_str("    default=");
                atom(&mut output, &default.parameter.0 .0);
                atom(&mut output, &default.expression.0);
                atom(
                    &mut output,
                    if default.proof_relevant {
                        "proof-relevant"
                    } else {
                        "ordinary"
                    },
                );
                output.push('\n');
            }
            for place in &entry.place_inputs {
                output.push_str("    place-input=");
                atom(&mut output, &place.point.0);
                atom(&mut output, &place.expression.0);
                output.push('\n');
            }
            for point in &entry.math_points {
                output.push_str("    expected-math=");
                atom(&mut output, &point.owner.0);
                atom(&mut output, &point.statement.0);
                atom(&mut output, &point.ordinal.to_string());
                output.push('\n');
            }
            for point in &entry.loop_invariants {
                output.push_str("    expected-invariant=");
                atom(&mut output, &point.owner.0);
                atom(&mut output, &point.statement.0);
                atom(&mut output, &point.ordinal.to_string());
                output.push('\n');
            }
            for node in &entry.loop_nodes {
                output.push_str("    expected-loop=");
                atom(&mut output, &node.0);
                output.push('\n');
            }
            output.push_str("    expected-worklist=");
            atom(&mut output, &format!("{:#?}", entry.expected_worklist));
            output.push('\n');
            output.push_str("    typed-operations=");
            atom(&mut output, &format!("{:#?}", entry.typed_operations));
            output.push('\n');
        }
        for (owner, entry) in &self.checked_callables {
            output.push_str("  checked-callable=");
            atom(&mut output, &owner.0);
            atom(&mut output, &entry.source_fingerprint);
            atom(
                &mut output,
                if entry.verification_required {
                    "verify"
                } else {
                    "no-verify"
                },
            );
            atom(
                &mut output,
                if entry.worklist_required {
                    "worklist"
                } else {
                    "no-worklist"
                },
            );
            atom(&mut output, &format!("{:?}", entry.materialized_owners));
            output.push('\n');
            for root in &entry.roots {
                output.push_str("    checked-root=");
                atom(&mut output, root_kind_text(root.kind));
                atom(&mut output, &root.node.0);
                atom(&mut output, &root.identity);
                output.push('\n');
            }
            for call in &entry.calls {
                output.push_str("    checked-call=");
                atom(&mut output, &call.call_node.0);
                atom(&mut output, &call.callee_identity);
                atom(&mut output, &call.selected_signature);
                atom(&mut output, &format!("{:?}", call.expression_type));
                atom(&mut output, &format!("{:?}", call.type_arguments));
                atom(&mut output, &format!("{:?}", call.result_type));
                atom(&mut output, &format!("{:?}", call.effects));
                atom(
                    &mut output,
                    if call.requires_declared {
                        "requires"
                    } else {
                        "no-requires"
                    },
                );
                atom(
                    &mut output,
                    if call.ensures_declared {
                        "ensures"
                    } else {
                        "no-ensures"
                    },
                );
                output.push('\n');
                for argument in &call.arguments {
                    atom(&mut output, &argument.index.to_string());
                    atom(&mut output, &argument.parameter.0 .0);
                    atom(&mut output, &argument.source_node.0);
                    atom(&mut output, argument.source_type.as_str());
                    atom(&mut output, &format!("{:?}", argument.conversion));
                    atom(&mut output, argument.conversion_from.as_str());
                    atom(&mut output, argument.target_type.as_str());
                    output.push('\n');
                }
            }
            for default in &entry.defaults {
                output.push_str("    checked-default=");
                atom(&mut output, &default.parameter.0 .0);
                atom(&mut output, &default.expression.0);
                atom(
                    &mut output,
                    if default.proof_relevant {
                        "proof-relevant"
                    } else {
                        "ordinary"
                    },
                );
                output.push('\n');
            }
            for place in &entry.place_inputs {
                output.push_str("    checked-place-input=");
                atom(&mut output, &place.point.0);
                atom(&mut output, &place.expression.0);
                output.push('\n');
            }
        }
        for declaration in &self.declarations {
            output.push_str("  ffi-declaration=");
            atom(&mut output, &declaration.declaration.0);
            atom(&mut output, &declaration.block.0);
            atom(&mut output, &declaration.symbol);
            atom(&mut output, &declaration.abi);
            atom(
                &mut output,
                &format!(
                    "fixed={:?};modes={:?};types={:?};signature_ready={};result={:?};variadic={};block_no_panic={};declaration_no_panic={};block_errno={};declaration_errno={};unsafe={}",
                    declaration.fixed_parameter_ids,
                    declaration.fixed_parameter_modes,
                    declaration.fixed_parameter_types,
                    declaration.signature_ready,
                    declaration.result_type,
                    declaration.variadic,
                    declaration.block_no_panic,
                    declaration.declaration_no_panic,
                    declaration.block_returns_errno,
                    declaration.declaration_returns_errno,
                    declaration.unsafe_block,
                ),
            );
            atom(
                &mut output,
                if declaration.requires_declared {
                    "declared"
                } else {
                    "absent"
                },
            );
            atom(
                &mut output,
                declaration
                    .requires_root
                    .as_ref()
                    .map_or("", |n| n.0.as_str()),
            );
            atom(
                &mut output,
                if declaration.requires_deferred {
                    "requires-deferred"
                } else {
                    "requires-not-deferred"
                },
            );
            atom(
                &mut output,
                if declaration.ensures_declared {
                    "declared"
                } else {
                    "absent"
                },
            );
            atom(
                &mut output,
                declaration
                    .ensures_root
                    .as_ref()
                    .map_or("", |n| n.0.as_str()),
            );
            atom(
                &mut output,
                if declaration.ensures_deferred {
                    "ensures-deferred"
                } else {
                    "ensures-not-deferred"
                },
            );
            output.push('\n');
        }
        output
    }

    pub(crate) fn validate_integrity(
        &self,
        functions: &BTreeMap<NodeId, super::MirFunction>,
    ) -> Result<(), MirProofCensusIntegrityError> {
        if self.witness != self.compute_witness() {
            return Err(MirProofCensusIntegrityError {
                message: "private checked-source proof census witness mismatch".into(),
            });
        }
        let function_owners = functions.keys().cloned().collect::<Vec<_>>();
        if self.function_owners != function_owners {
            return Err(MirProofCensusIntegrityError {
                message: "proof census owner table does not match the complete MIR function map"
                    .into(),
            });
        }
        if self.owners.keys().cloned().collect::<Vec<_>>() != function_owners {
            return Err(MirProofCensusIntegrityError {
                message: "proof census is missing or adds a MIR owner entry".into(),
            });
        }
        if self.owners.iter().any(|(key, entry)| key != &entry.owner) {
            return Err(MirProofCensusIntegrityError {
                message: "proof census owner key disagrees with its sealed entry".into(),
            });
        }
        if self
            .checked_callables
            .iter()
            .any(|(key, entry)| key != &entry.owner)
        {
            return Err(MirProofCensusIntegrityError {
                message: "checked-callable census key disagrees with its sealed entry".into(),
            });
        }
        for entry in self.checked_callables.values() {
            if entry.materialized_owners.iter().any(|owner| {
                !functions.contains_key(owner)
                    || self.owners.get(owner).is_none_or(|mir_owner| {
                        mir_owner.source_owner.as_ref() != Some(&entry.owner)
                    })
            }) {
                return Err(MirProofCensusIntegrityError {
                    message: format!(
                        "checked-callable '{}' has a stale or mismatched MIR owner binding",
                        entry.owner.0
                    ),
                });
            }
        }
        for entry in self.owners.values() {
            if entry.source_owner.as_ref().is_some_and(|source_owner| {
                self.checked_callables
                    .get(source_owner)
                    .is_none_or(|source| !source.materialized_owners.contains(&entry.owner))
            }) {
                return Err(MirProofCensusIntegrityError {
                    message: format!(
                        "MIR owner '{}' is not bound by its checked-callable census",
                        entry.owner.0
                    ),
                });
            }
        }
        Ok(())
    }

    fn compute_witness(&self) -> String {
        let material = format!(
            "authority={:?}\nfunctions={:#?}\nowners={:#?}\nchecked_callables={:#?}\ndeclarations={:#?}\nsource={}",
            self.authority,
            self.function_owners,
            self.owners,
            self.checked_callables,
            self.declarations,
            self.checked_source_fingerprint
        );
        blake3::hash(material.as_bytes()).to_hex().to_string()
    }
}

fn mir_point_id(point: &TypedProofPointId) -> MirProofPointId {
    MirProofPointId {
        owner: point.owner.clone(),
        statement: point.statement.clone(),
        ordinal: point.ordinal,
    }
}

fn source_callable_inventory(
    callable: &crate::core::ir::ResolvedCallable,
    program: &CheckedProgram,
    materialized_owners: Vec<NodeId>,
) -> MirProofSourceCallableCensus {
    let mut roots = BTreeSet::new();
    let contract_roots = callable.body.contract_proof_roots();
    let direct_contracts = callable
        .body
        .root
        .statements
        .iter()
        .filter_map(|statement| {
            matches!(statement.kind, ResolvedStmtKind::Contract { .. })
                .then_some(statement.node_id.clone())
        })
        .collect::<BTreeSet<_>>();
    for contract in &contract_roots {
        let kind = match contract.kind {
            ContractKind::Requires => MirProofSourceRootKind::Requires,
            ContractKind::Ensures => MirProofSourceRootKind::Ensures,
            ContractKind::Invariant => MirProofSourceRootKind::Invariant,
        };
        roots.insert(MirProofSourceRoot {
            kind,
            node: contract.node_id.clone(),
            identity: contract.condition.node_id.0.clone(),
        });
    }
    let math_roots = callable.body.math_proof_roots();
    for (node, _) in &math_roots {
        roots.insert(MirProofSourceRoot {
            kind: MirProofSourceRootKind::Math,
            node: node.clone(),
            identity: "math-statement".into(),
        });
    }
    let loop_nodes = callable
        .cfg
        .edges
        .values()
        .filter(|edge| {
            matches!(
                edge.kind,
                crate::core::cfg::EdgeKind::LoopBody
                    | crate::core::cfg::EdgeKind::LoopExit
                    | crate::core::cfg::EdgeKind::Backedge
            )
        })
        .map(|edge| edge.source.node.clone())
        .collect::<BTreeSet<_>>();
    for node in &loop_nodes {
        roots.insert(MirProofSourceRoot {
            kind: MirProofSourceRootKind::Loop,
            node: node.clone(),
            identity: "cfg-loop-edge-source".into(),
        });
    }

    let mut body_calls = BTreeMap::new();
    let mut nested_lambda_calls = BTreeMap::new();
    let mut call_metadata = BTreeMap::new();
    collect_block_calls(
        &callable.body.root,
        &mut body_calls,
        &mut nested_lambda_calls,
        &mut call_metadata,
        false,
    );
    for expression in callable
        .body
        .default_values
        .values()
        .chain(callable.body.place_inputs.values())
    {
        let mut calls = BTreeMap::new();
        let mut nested = BTreeMap::new();
        collect_expr_calls(
            expression,
            &mut calls,
            &mut nested,
            &mut call_metadata,
            false,
        );
        body_calls.extend(calls);
        nested_lambda_calls.extend(nested);
    }
    let mut call_directory_matches = body_calls
        .iter()
        .all(|(node, callee)| callable.body.resolved_calls.get(node) == Some(callee));
    call_directory_matches &= nested_lambda_calls
        .iter()
        .all(|(node, callee)| callable.body.resolved_calls.get(node) == Some(callee));
    let mut calls = callable
        .body
        .resolved_calls
        .iter()
        .map(|(node, callee)| {
            let metadata = call_metadata.get(node);
            if metadata.is_none() {
                call_directory_matches = false;
            }
            let call = MirProofSourceCallIdentity {
                call_node: node.clone(),
                callee: callee.clone(),
                callee_identity: format!("{callee:?}"),
                selected_signature: selected_signature_identity(callee, metadata, program),
                expression_type: metadata.map(|metadata| metadata.expression_type.clone()),
                type_arguments: metadata.map(|metadata| metadata.type_arguments.clone()),
                result_type: metadata.map(|metadata| metadata.result_type.clone()),
                effects: metadata.map(|metadata| metadata.effects.clone()),
                requires_declared: callee_requires_declared(callee, program),
                ensures_declared: callee_ensures_declared(callee, program),
                arguments: metadata.map_or_else(Vec::new, |metadata| metadata.arguments.clone()),
            };
            call
        })
        .collect::<Vec<_>>();
    calls.sort_by(|left, right| left.call_node.cmp(&right.call_node));
    for call in &calls {
        roots.insert(MirProofSourceRoot {
            kind: MirProofSourceRootKind::Call,
            node: call.call_node.clone(),
            identity: call.callee_identity.clone(),
        });
    }
    if !call_directory_matches || call_metadata.len() != callable.body.resolved_calls.len() {
        roots.insert(MirProofSourceRoot {
            kind: MirProofSourceRootKind::Boundary,
            node: callable.body.root.node_id.clone(),
            identity: "checked callable call identity directory does not match its complete body/default/place trees".into(),
        });
    }

    let typed = callable.proof_point_worklist();
    let mut defaults = callable
        .body
        .default_values
        .iter()
        .map(|(parameter, expression)| MirProofSourceDefault {
            parameter: parameter.clone(),
            expression: expression.node_id.clone(),
            proof_relevant: expression_has_proof_roots(expression)
                || expression_has_required_call(expression, program),
        })
        .collect::<Vec<_>>();
    defaults.sort();
    for default in &defaults {
        roots.insert(MirProofSourceRoot {
            kind: MirProofSourceRootKind::DefaultValue,
            node: default.expression.clone(),
            identity: default.parameter.0 .0.clone(),
        });
        let default_has_exact_consumer = typed.default_roots.iter().any(|root| {
            root.owner == callable.owner
                && root.parameter == default.parameter
                && root.expression == default.expression
                && root.supports_local_only_profile(callable)
        });
        if default.proof_relevant && !default_has_exact_consumer {
            roots.insert(MirProofSourceRoot {
                kind: MirProofSourceRootKind::Boundary,
                node: default.expression.clone(),
                identity: "declaration default requires an exact scoped MIR use receipt".into(),
            });
        }
    }
    let mut place_inputs = callable
        .body
        .place_inputs
        .iter()
        .map(|(point, expression)| MirProofSourcePlaceInput {
            point: point.clone(),
            expression: expression.node_id.clone(),
        })
        .collect::<Vec<_>>();
    place_inputs.sort();
    for place in &place_inputs {
        roots.insert(MirProofSourceRoot {
            kind: MirProofSourceRootKind::PlaceInput,
            node: place.expression.clone(),
            identity: place.point.0.clone(),
        });
        let expression = &callable.body.place_inputs[&place.point];
        if expression_has_proof_roots(expression)
            || expression_has_required_call(expression, program)
        {
            roots.insert(MirProofSourceRoot {
                kind: MirProofSourceRootKind::Boundary,
                node: place.expression.clone(),
                identity: "place input requires a scoped MIR receipt".into(),
            });
        }
    }

    let direct_ensures = contract_roots.iter().any(|contract| {
        direct_contracts.contains(&contract.node_id) && contract.kind == ContractKind::Ensures
    });
    let has_scoped_contract = contract_roots
        .iter()
        .any(|contract| !direct_contracts.contains(&contract.node_id));
    let proof_relevant_place_input = callable.body.place_inputs.values().any(|expression| {
        expression_has_proof_roots(expression) || expression_has_required_call(expression, program)
    });
    let worklist_required = !math_roots.is_empty()
        || !typed.loop_invariant_points().is_empty()
        || has_scoped_contract
        || (direct_ensures && !loop_nodes.is_empty())
        || defaults.iter().any(MirProofSourceDefault::proof_relevant)
        || proof_relevant_place_input
        || callable.has_unmodeled_proof_roots();
    let call_requires_proof = callable.body.resolved_calls.values().any(|callee| {
        callee_requires_declared(callee, program) || callee_ensures_declared(callee, program)
    });
    let verification_required = callable.body.has_proof_obligations() || call_requires_proof;
    MirProofSourceCallableCensus {
        owner: callable.owner.clone(),
        source_fingerprint: blake3::hash(format!("{callable:#?}").as_bytes())
            .to_hex()
            .to_string(),
        roots: roots.into_iter().collect(),
        calls,
        defaults,
        place_inputs,
        materialized_owners,
        verification_required,
        worklist_required,
    }
}

impl MirProofExpectedWorklist {
    fn empty() -> Self {
        Self {
            calls: Vec::new(),
            deferred_calls: Vec::new(),
            scoped_boundaries: Vec::new(),
            default_roots: Vec::new(),
            default_math_points: Vec::new(),
            default_calls: Vec::new(),
            default_boundaries: Vec::new(),
            default_uses: Vec::new(),
            place_inputs: Vec::new(),
        }
    }
}

fn source_proof_scope(scope: &[TypedProofScopeFrame]) -> Vec<MirProofSourceScopeFrame> {
    scope
        .iter()
        .map(|frame| match frame {
            TypedProofScopeFrame::Block(block) => MirProofSourceScopeFrame::Block(block.clone()),
            TypedProofScopeFrame::Branch {
                statement,
                expression,
                then_arm,
            } => MirProofSourceScopeFrame::Branch {
                statement: statement.clone(),
                expression: expression.clone(),
                then_arm: *then_arm,
            },
            TypedProofScopeFrame::Loop(node) => MirProofSourceScopeFrame::Loop(node.clone()),
            TypedProofScopeFrame::Default {
                parameter,
                expression,
            } => MirProofSourceScopeFrame::Default {
                parameter: parameter.clone(),
                expression: expression.clone(),
            },
            TypedProofScopeFrame::PlaceInput(node) => {
                MirProofSourceScopeFrame::PlaceInput(node.clone())
            }
        })
        .collect()
}

fn source_call_context(point: &TypedCallProofPoint) -> MirProofSourceCallContext {
    MirProofSourceCallContext {
        owner: point.owner.clone(),
        statement: point.statement.clone(),
        order: point.order,
        scope: source_proof_scope(&point.scope),
        guards: point
            .guards
            .iter()
            .map(|guard| (guard.condition.node_id.clone(), guard.when))
            .collect(),
        disposition: point.disposition.clone(),
        effect_authority: point.effect.authority,
    }
}

fn source_call_origin(
    point: &TypedCallProofPoint,
    body_owner: &NodeId,
    proof_root_statements: &BTreeSet<NodeId>,
) -> MirProofSourceCallOrigin {
    if let Some(parameter) = point.scope.iter().find_map(|frame| match frame {
        TypedProofScopeFrame::Default { parameter, .. } => Some(parameter.clone()),
        _ => None,
    }) {
        return MirProofSourceCallOrigin::Default(parameter);
    }
    if let Some(point) = point.scope.iter().find_map(|frame| match frame {
        TypedProofScopeFrame::PlaceInput(point) => Some(point.clone()),
        _ => None,
    }) {
        return MirProofSourceCallOrigin::PlaceInput(point);
    }
    if point.owner != *body_owner {
        return MirProofSourceCallOrigin::Lambda(point.owner.clone());
    }
    if proof_root_statements.contains(&point.statement) {
        return MirProofSourceCallOrigin::ProofRoot(point.statement.clone());
    }
    MirProofSourceCallOrigin::Body
}

fn selected_signature_identity(
    callee: &ResolvedCallee,
    call: Option<&MirProofSourceCallMetadata>,
    program: &CheckedProgram,
) -> String {
    let declaration = match callee {
        ResolvedCallee::Function(owner) | ResolvedCallee::Constructor(owner) => program
            .callable(owner)
            .map(|callable| format!("{:#?}", callable.signature)),
        ResolvedCallee::ActorMethod { method, .. }
        | ResolvedCallee::ProtocolMethod { method, .. } => program
            .callable(&NodeId(method.as_str().to_owned()))
            .map(|callable| format!("{:#?}", callable.signature)),
        ResolvedCallee::Extern(owner) => program.ffi_declarations().get(owner).map(|descriptor| {
            format!(
                "symbol={:?}; abi={:?}; signature={:#?}; invocation={:?}",
                descriptor.symbol, descriptor.abi, descriptor.signature, descriptor.invocation
            )
        }),
        _ => None,
    };
    format!(
        "callee={callee:?}; declaration={}; call_shape={}",
        declaration.unwrap_or_else(|| "unavailable-or-builtin".into()),
        call.map_or_else(|| "None".to_string(), |call| call.call_shape_debug.clone(),),
    )
}

fn source_proof_assertion(
    point: &TypedProofPointId,
    order: u64,
    scope: &[TypedProofScopeFrame],
    expr: &ResolvedExpr,
) -> MirProofSourceAssertion {
    MirProofSourceAssertion {
        id: mir_point_id(point),
        order,
        scope: source_proof_scope(scope),
        expression: expr.node_id.clone(),
    }
}

fn source_call_event(point: &TypedCallProofPoint) -> MirProofSourceCallEvent {
    let (expression_type, callee, type_arguments, result_type, effects, arguments) =
        match &point.expression.kind {
            ResolvedExprKind::Call(call) => (
                point.expression.ty.clone(),
                call.callee.clone(),
                call.type_arguments.clone(),
                call.result.clone(),
                call.effects.clone(),
                call.arguments
                    .iter()
                    .enumerate()
                    .map(|(ordinal, argument)| MirProofSourceCallArgumentEvent {
                        ordinal,
                        parameter: argument.parameter.clone(),
                        expression: argument.value.node_id.clone(),
                        source_type: argument.value.ty.clone(),
                        conversion: argument.conversion.clone(),
                    })
                    .collect(),
            ),
            _ => (
                point.expression.ty.clone(),
                point.effect.callee.clone(),
                Vec::new(),
                point.expression.ty.clone(),
                point.effect.declared_effects.clone(),
                Vec::new(),
            ),
        };
    MirProofSourceCallEvent {
        body_owner: point.body_owner.clone(),
        owner: point.owner.clone(),
        statement: point.statement.clone(),
        call_node: point.call_node.clone(),
        order: point.order,
        scope: source_proof_scope(&point.scope),
        guards: point
            .guards
            .iter()
            .map(|guard| (guard.condition.node_id.clone(), guard.when))
            .collect(),
        disposition: point.disposition.clone(),
        expression: point.expression.node_id.clone(),
        expression_type,
        callee,
        type_arguments,
        result_type,
        effects,
        effect_authority: point.effect.authority,
        arguments,
    }
}

fn source_call_boundary(point: &TypedCallProofPoint) -> super::MirProofCallBoundary {
    let (callee, type_arguments, result_type, declared_effects, argument_count, arguments) =
        match &point.expression.kind {
            ResolvedExprKind::Call(call) => (
                Some(call.callee.clone()),
                call.type_arguments.clone(),
                Some(call.result.clone()),
                call.effects.clone(),
                call.arguments.len(),
                call.arguments
                    .iter()
                    .enumerate()
                    .map(|(ordinal, argument)| super::MirProofBoundaryArgument {
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
    super::MirProofCallBoundary {
        id: super::MirProofCallPointId {
            owner: point.owner.clone(),
            statement: point.statement.clone(),
            call_node: point.call_node.clone(),
            order: point.order,
        },
        body_owner: point.body_owner.clone(),
        expression: point.expression.node_id.clone(),
        scope: point.scope.clone(),
        guards: point
            .guards
            .iter()
            .map(|guard| super::MirProofBoundaryGuard {
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

fn expected_worklist_from_typed(
    typed: &TypedProofPointWorklist,
    place_inputs: &BTreeMap<NodeId, ResolvedExpr>,
) -> MirProofExpectedWorklist {
    let mut expected = MirProofExpectedWorklist::empty();
    expected.calls = typed
        .calls
        .iter()
        .map(|call| super::MirProofCallPointId {
            owner: call.owner.clone(),
            statement: call.statement.clone(),
            call_node: call.call_node.clone(),
            order: call.order,
        })
        .collect();
    expected.calls.sort();
    expected.deferred_calls = typed
        .deferred_calls
        .iter()
        .map(source_call_boundary)
        .collect();
    expected.deferred_calls.sort_by_key(|call| call.id.clone());
    expected.scoped_boundaries = typed
        .boundaries
        .iter()
        .map(|boundary| boundary.point.clone())
        .collect();
    expected.scoped_boundaries.sort();
    // Multiple checker reasons can intentionally point at one source node;
    // this identity-only census records the node once while the reason-bearing
    // MIR boundary retains every reason.
    expected.scoped_boundaries.dedup();

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
    expected.default_roots = active_defaults
        .iter()
        .map(|root| super::MirProofDefaultRootId {
            owner: root.owner.clone(),
            parameter: root.parameter.clone(),
            expression: root.expression.clone(),
        })
        .collect();
    expected.default_roots.sort();
    expected.default_math_points = active_defaults
        .iter()
        .flat_map(|root| root.points.iter())
        .map(|point| super::MirProofBoundaryMathId {
            point: mir_point_id(&point.id),
            order: point.order,
            expression: point.condition.node_id.clone(),
            scope: point.scope.clone(),
        })
        .collect();
    expected.default_math_points.sort();
    expected.default_calls = active_defaults
        .iter()
        .flat_map(|root| root.calls.iter())
        .map(source_call_boundary)
        .collect();
    expected.default_calls.sort_by_key(|call| call.id.clone());
    expected.default_boundaries = active_defaults
        .iter()
        .flat_map(|root| {
            root.boundaries
                .iter()
                .map(move |boundary| super::MirProofDefaultBoundary {
                    root: super::MirProofDefaultRootId {
                        owner: root.owner.clone(),
                        parameter: root.parameter.clone(),
                        expression: root.expression.clone(),
                    },
                    point: boundary.point.clone(),
                    reason: boundary.reason.clone(),
                })
        })
        .collect();
    expected.default_boundaries.sort();
    expected.default_uses = typed
        .default_uses
        .iter()
        .filter(|use_id| {
            active_default_keys.contains(&(use_id.callee_owner.clone(), use_id.parameter.clone()))
        })
        .map(|use_id| super::MirProofDefaultUseId {
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
        .collect();
    expected.default_uses.sort();
    expected.place_inputs = place_inputs
        .iter()
        .map(|(point, expression)| super::MirProofPlaceInputId {
            point: point.clone(),
            expression: expression.node_id.clone(),
        })
        .collect();
    expected.place_inputs.sort();
    expected
}

fn source_proof_block(block: &TypedProofBlock) -> MirProofSourceBlock {
    fn return_value_node(value: &ResolvedExpr) -> Option<NodeId> {
        // Aggregate return payloads are owned/executed by body MIR.  The
        // proof worklist retains the Return event itself, but its scalar
        // expression slot is intentionally empty so a record/tuple payload
        // cannot make an otherwise valid Math census look incomplete.
        if matches!(
            value.kind,
            ResolvedExprKind::Record { .. }
                | ResolvedExprKind::Tuple(_)
                | ResolvedExprKind::List(_)
                | ResolvedExprKind::Set(_)
        ) {
            None
        } else {
            Some(value.node_id.clone())
        }
    }
    let operations = block
        .operations
        .iter()
        .map(|operation| match operation {
            TypedProofOperation::Bind {
                point,
                local,
                initializer,
            } => MirProofSourceOperation::Bind {
                point: point.clone(),
                local_identity: local.0.clone(),
                initializer: initializer.node_id.clone(),
            },
            TypedProofOperation::Call(point) => {
                MirProofSourceOperation::Call(source_call_event(point))
            }
            TypedProofOperation::Eval { point, expression } => MirProofSourceOperation::Eval {
                point: point.clone(),
                expression: expression.node_id.clone(),
            },
            TypedProofOperation::Assign {
                point,
                target,
                value,
                conversion,
            } => MirProofSourceOperation::Assign {
                point: point.clone(),
                target: format!("{target:?}"),
                value: value.node_id.clone(),
                conversion: format!("{conversion:?}"),
            },
            TypedProofOperation::Math(TypedMathProofPoint {
                id,
                order,
                scope,
                condition,
            }) => {
                MirProofSourceOperation::Math(source_proof_assertion(id, *order, scope, condition))
            }
            TypedProofOperation::Branch {
                point,
                expression,
                condition,
                then_block,
                else_block,
            } => MirProofSourceOperation::Branch {
                point: point.clone(),
                expression: expression.clone(),
                condition: condition.node_id.clone(),
                then_block: source_proof_block(then_block),
                else_block: source_proof_block(else_block),
            },
            TypedProofOperation::Scope(inner) => {
                MirProofSourceOperation::Scope(source_proof_block(inner))
            }
            TypedProofOperation::Loop {
                point,
                condition,
                condition_calls,
                invariants,
                body,
                stationary,
                receipt,
            } => MirProofSourceOperation::Loop {
                point: point.clone(),
                condition: condition.node_id.clone(),
                condition_calls: condition_calls.iter().map(source_call_event).collect(),
                invariants: invariants
                    .iter()
                    .map(
                        |TypedLoopInvariantPoint {
                             id,
                             scope,
                             condition,
                         }| {
                            source_proof_assertion(id, u64::from(id.ordinal), scope, condition)
                        },
                    )
                    .collect(),
                body: source_proof_block(body),
                stationary: *stationary,
                receipt: receipt.as_ref().map(|receipt| format!("{receipt:#?}")),
            },
            TypedProofOperation::Break {
                point,
                target_loop,
                value,
            } => MirProofSourceOperation::Break {
                point: point.clone(),
                target_loop: target_loop.clone(),
                value: value.as_ref().map(|value| value.node_id.clone()),
            },
            TypedProofOperation::Continue { point, target_loop } => {
                MirProofSourceOperation::Continue {
                    point: point.clone(),
                    target_loop: target_loop.clone(),
                }
            }
            TypedProofOperation::Return {
                point,
                value,
                conversion,
            } => MirProofSourceOperation::Return {
                point: point.clone(),
                value: value.as_ref().and_then(return_value_node),
                conversion: conversion
                    .as_ref()
                    .map(|conversion| format!("{conversion:?}")),
            },
            TypedProofOperation::Exit => MirProofSourceOperation::Exit,
        })
        .collect();
    MirProofSourceBlock {
        id: block.id.clone(),
        operations,
    }
}

impl MirProofOwnerCensus {
    pub(crate) fn worklist_matches_checked_operation_census(
        &self,
        worklist: &super::MirProofWorklist,
    ) -> bool {
        fn actual_assertion(assertion: &super::MirProofAssertion) -> MirProofSourceAssertion {
            MirProofSourceAssertion {
                id: assertion.id.clone(),
                order: assertion.order,
                scope: assertion
                    .scope
                    .iter()
                    .map(|frame| match frame {
                        super::MirProofScopeFrame::Block(block) => {
                            MirProofSourceScopeFrame::Block(block.clone())
                        }
                        super::MirProofScopeFrame::Branch {
                            statement,
                            expression,
                            then_arm,
                        } => MirProofSourceScopeFrame::Branch {
                            statement: statement.clone(),
                            expression: expression.clone(),
                            then_arm: *then_arm,
                        },
                        super::MirProofScopeFrame::Loop(node) => {
                            MirProofSourceScopeFrame::Loop(node.clone())
                        }
                    })
                    .collect(),
                expression: assertion.expression.origin.clone(),
            }
        }
        fn actual_call(call: &super::MirProofCallPoint) -> MirProofSourceCallEvent {
            MirProofSourceCallEvent {
                body_owner: call.body_owner.clone(),
                owner: call.owner.clone(),
                statement: call.statement.clone(),
                call_node: call.call_node.clone(),
                order: call.order,
                scope: call
                    .scope
                    .iter()
                    .map(|frame| match frame {
                        super::MirProofScopeFrame::Block(block) => {
                            MirProofSourceScopeFrame::Block(block.clone())
                        }
                        super::MirProofScopeFrame::Branch {
                            statement,
                            expression,
                            then_arm,
                        } => MirProofSourceScopeFrame::Branch {
                            statement: statement.clone(),
                            expression: expression.clone(),
                            then_arm: *then_arm,
                        },
                        super::MirProofScopeFrame::Loop(node) => {
                            MirProofSourceScopeFrame::Loop(node.clone())
                        }
                    })
                    .collect(),
                guards: call
                    .guards
                    .iter()
                    .map(|guard| (guard.condition.origin.clone(), guard.when))
                    .collect(),
                disposition: call.disposition.clone(),
                expression: call.call_node.clone(),
                expression_type: call.result_type.clone(),
                callee: call.callee.clone(),
                type_arguments: call.type_arguments.clone(),
                result_type: call.result_type.clone(),
                effects: call.declared_effects.clone(),
                effect_authority: call.effect_authority,
                arguments: call
                    .arguments
                    .iter()
                    .enumerate()
                    .map(|(ordinal, argument)| MirProofSourceCallArgumentEvent {
                        ordinal,
                        parameter: argument.parameter.clone(),
                        expression: argument.expression.origin.clone(),
                        source_type: argument.expression.ty.clone(),
                        conversion: argument.conversion.clone(),
                    })
                    .collect(),
            }
        }
        fn actual_block(block: &super::MirProofBlock) -> MirProofSourceBlock {
            MirProofSourceBlock {
                id: block.id.clone(),
                operations: block
                    .operations
                    .iter()
                    .map(|operation| match operation {
                        super::MirProofOperation::Bind {
                            point,
                            local_identity,
                            initializer,
                            ..
                        } => MirProofSourceOperation::Bind {
                            point: point.clone(),
                            local_identity: local_identity.clone(),
                            initializer: initializer.origin.clone(),
                        },
                        super::MirProofOperation::Call(call) => {
                            MirProofSourceOperation::Call(actual_call(call))
                        }
                        super::MirProofOperation::Eval { point, expression } => {
                            MirProofSourceOperation::Eval {
                                point: point.clone(),
                                expression: expression.origin.clone(),
                            }
                        }
                        super::MirProofOperation::Assign {
                            point,
                            target,
                            value,
                            conversion,
                            ..
                        } => MirProofSourceOperation::Assign {
                            point: point.clone(),
                            target: format!("{target:?}"),
                            value: value.origin.clone(),
                            conversion: format!("{conversion:?}"),
                        },
                        super::MirProofOperation::Math(assertion) => {
                            MirProofSourceOperation::Math(actual_assertion(assertion))
                        }
                        super::MirProofOperation::Branch {
                            point,
                            expression,
                            condition,
                            then_block,
                            else_block,
                            ..
                        } => MirProofSourceOperation::Branch {
                            point: point.clone(),
                            expression: expression.clone(),
                            condition: condition.origin.clone(),
                            then_block: actual_block(then_block),
                            else_block: actual_block(else_block),
                        },
                        super::MirProofOperation::Scope(inner) => {
                            MirProofSourceOperation::Scope(actual_block(inner))
                        }
                        super::MirProofOperation::Loop {
                            point,
                            condition,
                            condition_calls,
                            invariants,
                            body,
                            stationary,
                            receipt,
                            ..
                        } => MirProofSourceOperation::Loop {
                            point: point.clone(),
                            condition: condition.origin.clone(),
                            condition_calls: condition_calls.iter().map(actual_call).collect(),
                            invariants: invariants.iter().map(actual_assertion).collect(),
                            body: actual_block(body),
                            stationary: *stationary,
                            receipt: receipt
                                .as_ref()
                                .map(|receipt| format!("{:#?}", receipt.typed)),
                        },
                        super::MirProofOperation::Break {
                            point,
                            target_loop,
                            value,
                        } => MirProofSourceOperation::Break {
                            point: point.clone(),
                            target_loop: target_loop.clone(),
                            value: value.as_ref().map(|value| value.origin.clone()),
                        },
                        super::MirProofOperation::Continue { point, target_loop } => {
                            MirProofSourceOperation::Continue {
                                point: point.clone(),
                                target_loop: target_loop.clone(),
                            }
                        }
                        super::MirProofOperation::Return {
                            point,
                            value,
                            conversion,
                        } => MirProofSourceOperation::Return {
                            point: point.clone(),
                            value: value.as_ref().map(|value| value.origin.clone()),
                            conversion: conversion
                                .as_ref()
                                .map(|conversion| format!("{conversion:?}")),
                        },
                        super::MirProofOperation::Exit => MirProofSourceOperation::Exit,
                    })
                    .collect(),
            }
        }

        worklist.owner == self.owner
            && worklist.expected_math_points == self.math_points
            && worklist.expected_loop_invariants == self.loop_invariants
            && worklist.expected_calls == self.expected_worklist.calls
            && worklist.expected_deferred_calls == self.expected_worklist.deferred_calls
            && worklist.expected_scoped_boundaries == self.expected_worklist.scoped_boundaries
            && worklist.expected_default_roots == self.expected_worklist.default_roots
            && worklist.expected_default_math_points == self.expected_worklist.default_math_points
            && worklist.expected_default_calls == self.expected_worklist.default_calls
            && worklist.expected_default_boundaries == self.expected_worklist.default_boundaries
            && worklist.expected_default_uses == self.expected_worklist.default_uses
            && worklist.expected_place_inputs == self.expected_worklist.place_inputs
            && actual_block(&worklist.root) == self.typed_operations
    }
}

fn has_direct_ensures_contract(callable: &crate::core::ir::ResolvedCallable) -> bool {
    callable.body.root.statements.iter().any(|statement| {
        matches!(
            statement.kind,
            ResolvedStmtKind::Contract {
                kind: ContractKind::Ensures,
                ..
            }
        )
    })
}

fn expression_has_required_call(expression: &ResolvedExpr, program: &CheckedProgram) -> bool {
    let mut calls = BTreeMap::new();
    let mut nested_lambda_calls = BTreeMap::new();
    let mut call_arguments = BTreeMap::new();
    collect_expr_calls(
        expression,
        &mut calls,
        &mut nested_lambda_calls,
        &mut call_arguments,
        false,
    );
    calls.values().any(|callee| {
        callee_requires_declared(callee, program) || callee_ensures_declared(callee, program)
    })
}

fn ffi_root(root: &ResolvedFfiContractRoot) -> Option<NodeId> {
    match root {
        ResolvedFfiContractRoot::Absent => None,
        ResolvedFfiContractRoot::Ready { node_id } => Some(node_id.clone()),
        ResolvedFfiContractRoot::Deferred { node_id, .. } => node_id.clone(),
    }
}

fn callee_requires_declared(callee: &ResolvedCallee, program: &CheckedProgram) -> bool {
    match callee {
        ResolvedCallee::Function(owner) | ResolvedCallee::Constructor(owner) => {
            program.callable(owner).is_some_and(|callable| {
                callable
                    .contracts
                    .iter()
                    .any(|contract| contract.kind == ContractKind::Requires)
            })
        }
        ResolvedCallee::ActorMethod { method, .. }
        | ResolvedCallee::ProtocolMethod { method, .. } => program
            .callable(&NodeId(method.as_str().to_owned()))
            .is_some_and(|callable| {
                callable
                    .contracts
                    .iter()
                    .any(|contract| contract.kind == ContractKind::Requires)
            }),
        ResolvedCallee::Extern(owner) => program
            .ffi_declarations()
            .get(owner)
            .is_some_and(|declaration| declaration.requires.is_declared()),
        _ => false,
    }
}

fn callee_ensures_declared(callee: &ResolvedCallee, program: &CheckedProgram) -> bool {
    match callee {
        ResolvedCallee::Function(owner) | ResolvedCallee::Constructor(owner) => {
            program.callable(owner).is_some_and(|callable| {
                callable
                    .contracts
                    .iter()
                    .any(|contract| contract.kind == ContractKind::Ensures)
            })
        }
        ResolvedCallee::ActorMethod { method, .. }
        | ResolvedCallee::ProtocolMethod { method, .. } => program
            .callable(&NodeId(method.as_str().to_owned()))
            .is_some_and(|callable| {
                callable
                    .contracts
                    .iter()
                    .any(|contract| contract.kind == ContractKind::Ensures)
            }),
        ResolvedCallee::Extern(owner) => program
            .ffi_declarations()
            .get(owner)
            .is_some_and(|declaration| declaration.ensures.is_declared()),
        _ => false,
    }
}

fn expression_has_proof_roots(expression: &crate::core::ir::ResolvedExpr) -> bool {
    let root = ResolvedBlock {
        node_id: expression.node_id.clone(),
        origin: expression.origin.clone(),
        ty: expression.ty.clone(),
        statements: Vec::new(),
        result: Some(Box::new(expression.clone())),
    };
    let body = ResolvedBody {
        owner: NodeId("proof-census-expression-fragment".into()),
        locals: BTreeMap::new(),
        parameters: Vec::new(),
        captures: Vec::new(),
        place_inputs: BTreeMap::new(),
        default_values: BTreeMap::new(),
        resolved_calls: BTreeMap::new(),
        root,
    };
    body.has_proof_obligations()
}

fn collect_block_calls(
    block: &ResolvedBlock,
    calls: &mut BTreeMap<NodeId, ResolvedCallee>,
    nested_lambda_calls: &mut BTreeMap<NodeId, ResolvedCallee>,
    call_arguments: &mut BTreeMap<NodeId, MirProofSourceCallMetadata>,
    in_lambda: bool,
) {
    for statement in &block.statements {
        match &statement.kind {
            ResolvedStmtKind::Bind { initializer, .. } => {
                if let Some(initializer) = initializer {
                    collect_expr_calls(
                        initializer,
                        calls,
                        nested_lambda_calls,
                        call_arguments,
                        in_lambda,
                    );
                }
            }
            ResolvedStmtKind::Assign { value, .. } | ResolvedStmtKind::Expr(value) => {
                collect_expr_calls(value, calls, nested_lambda_calls, call_arguments, in_lambda)
            }
            ResolvedStmtKind::Math(expressions) => {
                for expression in expressions {
                    collect_expr_calls(
                        expression,
                        calls,
                        nested_lambda_calls,
                        call_arguments,
                        in_lambda,
                    );
                }
            }
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                if let Some(value) = value {
                    collect_expr_calls(
                        value,
                        calls,
                        nested_lambda_calls,
                        call_arguments,
                        in_lambda,
                    );
                }
            }
            ResolvedStmtKind::While { condition, body } => {
                collect_expr_calls(
                    condition,
                    calls,
                    nested_lambda_calls,
                    call_arguments,
                    in_lambda,
                );
                collect_block_calls(body, calls, nested_lambda_calls, call_arguments, in_lambda);
            }
            ResolvedStmtKind::WhileLet {
                initializer, body, ..
            }
            | ResolvedStmtKind::For {
                iterable: initializer,
                body,
                ..
            } => {
                collect_expr_calls(
                    initializer,
                    calls,
                    nested_lambda_calls,
                    call_arguments,
                    in_lambda,
                );
                collect_block_calls(body, calls, nested_lambda_calls, call_arguments, in_lambda);
            }
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                collect_expr_calls(
                    initializer,
                    calls,
                    nested_lambda_calls,
                    call_arguments,
                    in_lambda,
                );
                collect_block_calls(
                    then_block,
                    calls,
                    nested_lambda_calls,
                    call_arguments,
                    in_lambda,
                );
                if let Some(else_block) = else_block {
                    collect_block_calls(
                        else_block,
                        calls,
                        nested_lambda_calls,
                        call_arguments,
                        in_lambda,
                    );
                }
            }
            ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                collect_block_calls(body, calls, nested_lambda_calls, call_arguments, in_lambda)
            }
            ResolvedStmtKind::Pinned { value, body, .. } => {
                collect_expr_calls(value, calls, nested_lambda_calls, call_arguments, in_lambda);
                collect_block_calls(body, calls, nested_lambda_calls, call_arguments, in_lambda);
            }
            ResolvedStmtKind::Contract { condition, .. } => collect_expr_calls(
                condition,
                calls,
                nested_lambda_calls,
                call_arguments,
                in_lambda,
            ),
            ResolvedStmtKind::Drop(_)
            | ResolvedStmtKind::Continue
            | ResolvedStmtKind::NestedCallable(_) => {}
        }
    }
    if let Some(result) = &block.result {
        collect_expr_calls(
            result,
            calls,
            nested_lambda_calls,
            call_arguments,
            in_lambda,
        );
    }
}

fn collect_expr_calls(
    expression: &ResolvedExpr,
    calls: &mut BTreeMap<NodeId, ResolvedCallee>,
    nested_lambda_calls: &mut BTreeMap<NodeId, ResolvedCallee>,
    call_arguments: &mut BTreeMap<NodeId, MirProofSourceCallMetadata>,
    in_lambda: bool,
) {
    match &expression.kind {
        ResolvedExprKind::Call(call) => {
            if in_lambda {
                nested_lambda_calls.insert(expression.node_id.clone(), call.callee.clone());
            } else {
                calls.insert(expression.node_id.clone(), call.callee.clone());
            }
            call_arguments.insert(
                expression.node_id.clone(),
                MirProofSourceCallMetadata {
                    source_origin: expression.origin.clone(),
                    expression_type: expression.ty.clone(),
                    type_arguments: call.type_arguments.clone(),
                    result_type: call.result.clone(),
                    effects: call.effects.clone(),
                    call_shape_debug: format!(
                        "{:?}",
                        Some((
                            &expression.ty,
                            &call.type_arguments,
                            &call.result,
                            &call.effects,
                            &call.arguments,
                        ))
                    ),
                    arguments: call
                        .arguments
                        .iter()
                        .enumerate()
                        .map(|(index, argument)| MirProofSourceCallArgument {
                            index,
                            parameter: argument.parameter.clone(),
                            source_node: argument.value.node_id.clone(),
                            source_type: argument.value.ty.clone(),
                            conversion: argument.conversion.kind,
                            conversion_from: argument.conversion.from.clone(),
                            target_type: argument.conversion.to.clone(),
                        })
                        .collect(),
                },
            );
            for argument in &call.arguments {
                collect_expr_calls(
                    &argument.value,
                    calls,
                    nested_lambda_calls,
                    call_arguments,
                    in_lambda,
                );
            }
        }
        ResolvedExprKind::FString(parts) => {
            for part in parts {
                if let crate::core::ir::ResolvedFStringPart::Interpolation(value) = part {
                    collect_expr_calls(
                        value,
                        calls,
                        nested_lambda_calls,
                        call_arguments,
                        in_lambda,
                    );
                }
            }
        }
        ResolvedExprKind::Project { value, projection } => {
            collect_expr_calls(value, calls, nested_lambda_calls, call_arguments, in_lambda);
            if let crate::core::ir::ResolvedValueProjection::Index(index) = projection {
                collect_expr_calls(index, calls, nested_lambda_calls, call_arguments, in_lambda);
            }
        }
        ResolvedExprKind::Binary { left, right, .. }
        | ResolvedExprKind::Range {
            start: left,
            end: right,
        } => {
            collect_expr_calls(left, calls, nested_lambda_calls, call_arguments, in_lambda);
            collect_expr_calls(right, calls, nested_lambda_calls, call_arguments, in_lambda);
        }
        ResolvedExprKind::Unary { operand, .. }
        | ResolvedExprKind::OptionalChain {
            receiver: operand, ..
        }
        | ResolvedExprKind::TypeOf(operand)
        | ResolvedExprKind::Old(operand)
        | ResolvedExprKind::Try { value: operand, .. }
        | ResolvedExprKind::Cast { value: operand, .. }
        | ResolvedExprKind::Spawn(operand)
        | ResolvedExprKind::Await(operand) => collect_expr_calls(
            operand,
            calls,
            nested_lambda_calls,
            call_arguments,
            in_lambda,
        ),
        ResolvedExprKind::Tuple(values)
        | ResolvedExprKind::List(values)
        | ResolvedExprKind::Set(values) => {
            for value in values {
                collect_expr_calls(value, calls, nested_lambda_calls, call_arguments, in_lambda);
            }
        }
        ResolvedExprKind::Map(entries) => {
            for (key, value) in entries {
                collect_expr_calls(key, calls, nested_lambda_calls, call_arguments, in_lambda);
                collect_expr_calls(value, calls, nested_lambda_calls, call_arguments, in_lambda);
            }
        }
        ResolvedExprKind::Comprehension {
            value,
            iterable,
            guard,
            ..
        } => {
            collect_expr_calls(value, calls, nested_lambda_calls, call_arguments, in_lambda);
            collect_expr_calls(
                iterable,
                calls,
                nested_lambda_calls,
                call_arguments,
                in_lambda,
            );
            if let Some(guard) = guard {
                collect_expr_calls(guard, calls, nested_lambda_calls, call_arguments, in_lambda);
            }
        }
        ResolvedExprKind::Record { fields, rest, .. } => {
            for field in fields {
                collect_expr_calls(
                    &field.value,
                    calls,
                    nested_lambda_calls,
                    call_arguments,
                    in_lambda,
                );
            }
            if let Some(rest) = rest {
                collect_expr_calls(rest, calls, nested_lambda_calls, call_arguments, in_lambda);
            }
        }
        ResolvedExprKind::Block(block)
        | ResolvedExprKind::Scope { body: block, .. }
        | ResolvedExprKind::Comptime(block)
        | ResolvedExprKind::Quote(block) => {
            collect_block_calls(block, calls, nested_lambda_calls, call_arguments, in_lambda)
        }
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            collect_expr_calls(
                condition,
                calls,
                nested_lambda_calls,
                call_arguments,
                in_lambda,
            );
            collect_block_calls(
                then_block,
                calls,
                nested_lambda_calls,
                call_arguments,
                in_lambda,
            );
            collect_block_calls(
                else_block,
                calls,
                nested_lambda_calls,
                call_arguments,
                in_lambda,
            );
        }
        ResolvedExprKind::Match { scrutinee, arms } => {
            collect_expr_calls(
                scrutinee,
                calls,
                nested_lambda_calls,
                call_arguments,
                in_lambda,
            );
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_expr_calls(
                        guard,
                        calls,
                        nested_lambda_calls,
                        call_arguments,
                        in_lambda,
                    );
                }
                collect_expr_calls(
                    &arm.body,
                    calls,
                    nested_lambda_calls,
                    call_arguments,
                    in_lambda,
                );
            }
        }
        ResolvedExprKind::Slice { target, start, end } => {
            collect_expr_calls(
                target,
                calls,
                nested_lambda_calls,
                call_arguments,
                in_lambda,
            );
            if let Some(start) = start {
                collect_expr_calls(start, calls, nested_lambda_calls, call_arguments, in_lambda);
            }
            if let Some(end) = end {
                collect_expr_calls(end, calls, nested_lambda_calls, call_arguments, in_lambda);
            }
        }
        // A lambda has its own callable owner and proof census.
        ResolvedExprKind::Lambda(lambda) => collect_block_calls(
            &lambda.body,
            calls,
            nested_lambda_calls,
            call_arguments,
            true,
        ),
        ResolvedExprKind::Literal(_)
        | ResolvedExprKind::Load(_)
        | ResolvedExprKind::Constant(_)
        | ResolvedExprKind::Callable(_)
        | ResolvedExprKind::DefaultArgument { .. }
        | ResolvedExprKind::ComptimeValue(_)
        | ResolvedExprKind::TypeValue(_) => {}
    }
}

fn root_kind_text(kind: MirProofSourceRootKind) -> &'static str {
    match kind {
        MirProofSourceRootKind::Requires => "requires",
        MirProofSourceRootKind::Ensures => "ensures",
        MirProofSourceRootKind::Invariant => "invariant",
        MirProofSourceRootKind::Math => "math",
        MirProofSourceRootKind::Loop => "loop",
        MirProofSourceRootKind::Call => "call",
        MirProofSourceRootKind::DefaultValue => "default",
        MirProofSourceRootKind::PlaceInput => "place-input",
        MirProofSourceRootKind::Boundary => "boundary",
    }
}
