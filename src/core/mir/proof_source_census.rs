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
    TypedDefaultProofUse, TypedLoopInvariantPoint, TypedMatchPathReceipt, TypedMathProofPoint,
    TypedProofBlock, TypedProofOperation, TypedProofPointId, TypedProofPointWorklist,
    TypedProofScopeFrame,
};
use crate::core::{CheckedProgram, NodeId, Origin, ResolvedCallSite, ResolvedItemKind};

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
    caller_qualified_name: String,
    owner: NodeId,
    statement: NodeId,
    order: Option<u64>,
    scope: Vec<MirProofSourceScopeFrame>,
    guards: Vec<(NodeId, bool)>,
    match_path: Vec<TypedMatchPathReceipt>,
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

    pub fn caller_qualified_name(&self) -> &str {
        &self.caller_qualified_name
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

    pub fn match_path(&self) -> &[TypedMatchPathReceipt] {
        &self.match_path
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
    eager_source_nodes: Vec<NodeId>,
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

    pub fn eager_source_nodes(&self) -> &[NodeId] {
        &self.eager_source_nodes
    }
}

/// Exact source call identity kept even when its callable is not materialized
/// into this MIR graph (for example, a dormant generic template or an
/// unreachable Actor helper). The resolved-body validator has already checked
/// that `call_nodes` matches every Call node in the body/default/place trees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofSourceCallIdentity {
    call_node: NodeId,
    caller_qualified_name: String,
    origin: Option<Origin>,
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
    pub fn caller_qualified_name(&self) -> &str {
        &self.caller_qualified_name
    }
    pub fn origin(&self) -> Option<&Origin> {
        self.origin.as_ref()
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

/// Execution meaning of a Checker-owned FFI call that appears in a
/// declaration expression root. Keeping this separate from the source call
/// identity prevents an unactivated parameter default from being represented
/// as an executable call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirProofSourceFfiRootExecution {
    /// A top-level constant initializer is an eager declaration root. Its
    /// call identity is censused, but this slice has no executable MIR owner.
    ConstantInitializer,
    /// Actor field initialization is conditional on Actor construction; the
    /// construction/reachability join remains outside this slice.
    ActorConstructionPending,
    /// Trait defaults remain declaration roots until an exact activation
    /// receipt is available.
    TraitDefaultDeclarationOnly,
    /// A callable default is not a call unless a Checker-issued omitted-
    /// argument use activates it.
    CallableDefaultDeclarationOnly,
    /// At least one exact Checker-issued omitted-argument use activates this
    /// root. The FFI default expansion remains explicitly unsupported.
    CallableDefaultActivatedUnsupported,
}

/// Exact Checker-issued omitted-argument link for an FFI call in a callable
/// default. It identifies the invocation that activates the default without
/// pretending that the declaration expression itself is an ordinary body
/// call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofSourceFfiDefaultActivation {
    caller_owner: NodeId,
    call_node: NodeId,
    callee_owner: NodeId,
    parameter: ResolvedParameterId,
    argument_node: NodeId,
    parameter_index: usize,
    order: u64,
    scope: Vec<MirProofSourceScopeFrame>,
    guards: Vec<(NodeId, bool)>,
    disposition: TypedCallExecutionDisposition,
    origin: Origin,
}

impl MirProofSourceFfiDefaultActivation {
    pub fn caller_owner(&self) -> &NodeId {
        &self.caller_owner
    }

    pub fn call_node(&self) -> &NodeId {
        &self.call_node
    }

    pub fn callee_owner(&self) -> &NodeId {
        &self.callee_owner
    }

    pub fn parameter(&self) -> &ResolvedParameterId {
        &self.parameter
    }

    pub fn argument_node(&self) -> &NodeId {
        &self.argument_node
    }

    pub fn parameter_index(&self) -> usize {
        self.parameter_index
    }

    pub fn order(&self) -> u64 {
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

    pub fn origin(&self) -> &Origin {
        &self.origin
    }
}

/// Declaration-owned FFI call identity materialized from Checker-issued
/// source-root and call handles. It is not an executable MIR call by itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofSourceFfiRootCall {
    root_node: NodeId,
    call_node: NodeId,
    declaration: NodeId,
    symbol: String,
    call_origin: Origin,
    call_fingerprint: String,
}

impl MirProofSourceFfiRootCall {
    pub fn root_node(&self) -> &NodeId {
        &self.root_node
    }

    pub fn call_node(&self) -> &NodeId {
        &self.call_node
    }

    pub fn declaration(&self) -> &NodeId {
        &self.declaration
    }

    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    pub fn call_origin(&self) -> &Origin {
        &self.call_origin
    }

    pub fn call_fingerprint(&self) -> &str {
        &self.call_fingerprint
    }
}

/// One checked declaration expression root and the direct typed extern calls
/// inside it. Constant, Actor-field, trait-default, and callable-default roots
/// keep distinct execution classifications and owner identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirProofSourceFfiRoot {
    root_node: NodeId,
    owner: NodeId,
    kind: crate::core::ir::lower::ResolvedDeclarationExpressionKind,
    identity: Option<crate::core::ir::lower::ResolvedDeclarationRootIdentity>,
    origin: Option<Origin>,
    body_fingerprint: String,
    call_census_valid: bool,
    execution: MirProofSourceFfiRootExecution,
    activations: Vec<MirProofSourceFfiDefaultActivation>,
    calls: Vec<MirProofSourceFfiRootCall>,
}

impl MirProofSourceFfiRoot {
    pub fn root_node(&self) -> &NodeId {
        &self.root_node
    }

    pub fn owner(&self) -> &NodeId {
        &self.owner
    }

    pub fn kind(&self) -> crate::core::ir::lower::ResolvedDeclarationExpressionKind {
        self.kind
    }

    pub fn identity(&self) -> Option<&crate::core::ir::lower::ResolvedDeclarationRootIdentity> {
        self.identity.as_ref()
    }

    pub fn origin(&self) -> Option<&Origin> {
        self.origin.as_ref()
    }

    pub fn body_fingerprint(&self) -> &str {
        &self.body_fingerprint
    }

    pub fn call_census_valid(&self) -> bool {
        self.call_census_valid
    }

    pub fn execution(&self) -> MirProofSourceFfiRootExecution {
        self.execution
    }

    pub fn activations(&self) -> &[MirProofSourceFfiDefaultActivation] {
        &self.activations
    }

    pub fn calls(&self) -> &[MirProofSourceFfiRootCall] {
        &self.calls
    }
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
    requires_types: BTreeMap<Vec<u8>, ResolvedTypeId>,
    ensures_declared: bool,
    ensures_root: Option<NodeId>,
    ensures_deferred: bool,
    ensures_types: BTreeMap<Vec<u8>, ResolvedTypeId>,
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

    pub fn requires_types(&self) -> &BTreeMap<Vec<u8>, ResolvedTypeId> {
        &self.requires_types
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

    pub fn ensures_types(&self) -> &BTreeMap<Vec<u8>, ResolvedTypeId> {
        &self.ensures_types
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
    default_proof_roots: Vec<super::MirProofDefaultRoot>,
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

    /// Isolated typed MIR proof roots for declaration defaults. These roots
    /// are sealed with the checked-source census and carry exact omitted-
    /// argument use receipts; they do not inherit caller contracts.
    pub fn default_proof_roots(&self) -> &[super::MirProofDefaultRoot] {
        &self.default_proof_roots
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
    match_path: Vec<TypedMatchPathReceipt>,
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

fn complete_call_metadata_eager_nodes<'a>(
    metadata: impl IntoIterator<Item = &'a mut MirProofSourceCallMetadata>,
    place_inputs: &BTreeMap<NodeId, ResolvedExpr>,
) {
    for call in metadata {
        for argument in &mut call.arguments {
            argument.eager_source_nodes = ResolvedExpr::expand_eager_source_nodes(
                std::mem::take(&mut argument.eager_source_nodes),
                place_inputs,
            );
        }
    }
}

#[derive(Debug, Clone)]
struct MirProofSourceCallContext {
    owner: NodeId,
    statement: NodeId,
    order: u64,
    scope: Vec<MirProofSourceScopeFrame>,
    guards: Vec<(NodeId, bool)>,
    match_path: Vec<TypedMatchPathReceipt>,
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

/// Source declarations that a MIR-only consumer cannot recover from the
/// executable graph. This inventory is sealed with the proof census so a
/// narrow source profile can reject dormant top-level declarations.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MirProofSourceItem {
    node_id: NodeId,
    qualified_name: String,
    kind: ResolvedItemKind,
    origin: Origin,
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
    ffi_source_roots: Vec<MirProofSourceFfiRoot>,
    ffi_source_root_errors: Vec<String>,
    source_items: Vec<MirProofSourceItem>,
    has_imports: bool,
    main_function_safe: bool,
    checked_source_fingerprint: String,
    witness: String,
}

impl Default for MirProofSourceCensus {
    fn default() -> Self {
        Self::unavailable(&BTreeMap::new())
    }
}

impl MirProofSourceCensus {
    /// Build a sealed source-only FFI declaration-root census. This does not
    /// materialize executable functions or MIR instructions, so verifier
    /// boundary reporting cannot accidentally admit an unsupported default
    /// expansion to a backend.
    pub(crate) fn checked_ffi_declaration_roots(program: &CheckedProgram) -> Self {
        Self::checked(program, &BTreeMap::new(), &BTreeMap::new(), None)
    }

    /// Validate a source-only census against its empty executable-owner map.
    /// Full MIR consumers must use `MirProgram::validate_proof_source_census`.
    pub(crate) fn validate_source_only(&self) -> Result<(), MirProofCensusIntegrityError> {
        self.validate_integrity(&BTreeMap::new())
    }

    /// Validate only the declaration-root side of the FFI census. This is for
    /// verifier diagnostics that report an explicit unsupported boundary; it
    /// cannot produce an executable call contract or backend admission.
    pub(crate) fn validate_ffi_source_roots(&self) -> Result<(), Vec<String>> {
        self.validate_source_only()
            .map_err(|error| vec![format!("checked-source census integrity failed: {error:?}")])?;
        let errors = super::validate_ffi_source_root_census(self, &BTreeSet::new());
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

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
                        default_proof_roots: Vec::new(),
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
            Vec::new(),
            Vec::new(),
            Vec::new(),
            true,
            false,
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
                    requires_types: ffi_root_types(
                        program,
                        &declaration.requires,
                        crate::core::ir::lower::ResolvedDeclarationExpressionKind::ExternRequires,
                    ),
                    ensures_declared: declaration.ensures.is_declared(),
                    ensures_root: ffi_root(&declaration.ensures),
                    ensures_deferred: matches!(
                        &declaration.ensures,
                        ResolvedFfiContractRoot::Deferred { .. }
                    ),
                    ensures_types: ffi_root_types(
                        program,
                        &declaration.ensures,
                        crate::core::ir::lower::ResolvedDeclarationExpressionKind::ExternEnsures,
                    ),
                }
            })
            .collect::<Vec<_>>();
        declarations.sort_by(|left, right| left.declaration.cmp(&right.declaration));

        let (ffi_source_roots, ffi_source_root_errors) =
            checked_ffi_source_roots(program, excluded_sources);

        // Keep declaration inventory independent from MIR lowering: constants,
        // traits, types, and other source-only items may not become functions,
        // transitions, or FFI calls. Match the closed Map profile's standard
        // prelude exclusion by stable source key, then honor any additional
        // explicit source exclusion used by the MIR constructor.
        let mut source_items = program
            .items()
            .values()
            .filter(|item| {
                let source_id = item.origin.user_span().source_id;
                !excluded_sources.is_some_and(|excluded| excluded.contains(&source_id))
                    && !program
                        .source_registry()
                        .key(source_id)
                        .is_some_and(|key| key.as_str() == "stdlib:prelude.mimi")
            })
            .map(|item| MirProofSourceItem {
                node_id: item.node_id.clone(),
                qualified_name: item.qualified_name.clone(),
                kind: item.kind,
                origin: item.origin.clone(),
            })
            .collect::<Vec<_>>();
        source_items.sort_by(|left, right| left.node_id.cmp(&right.node_id));
        let has_imports = program.has_imports();
        let main_function_safe = program
            .functions()
            .get(&NodeId("function:main".into()))
            .is_some_and(|main| {
                !main.pub_
                    && !main.is_comptime
                    && !main.is_async
                    && main.extern_abi.is_none()
                    && main.generics.is_empty()
                    && main.effects.is_empty()
            });

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
                let mut unavailable =
                    Self::unavailable(&BTreeMap::from([(owner.clone(), function.clone())]));
                if let Some(entry) = unavailable.owners.remove(owner) {
                    owners.insert(owner.clone(), entry);
                }
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
                    identity: contract.condition.node_id.0.to_string(),
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
            complete_call_metadata_eager_nodes(
                call_metadata_occurrences.values_mut(),
                &source_callable.body.place_inputs,
            );
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
                        caller_qualified_name: checked_callable_qualified_name(
                            program,
                            &source_callable.owner,
                        ),
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
                        match_path: context
                            .as_ref()
                            .map_or_else(Vec::new, |context| context.match_path.clone()),
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
            let default_proof_roots = lower_default_proof_roots(source_callable, program);
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
                    default_proof_roots,
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
            "callables={source_fingerprints:#?}\ndeclarations={declarations:#?}\nffi_source_roots={ffi_source_roots:#?}\nffi_source_root_errors={ffi_source_root_errors:#?}\ninventory={checked_callables:#?}\nsource_items={source_items:#?}\nhas_imports={has_imports}\nmain_function_safe={main_function_safe}"
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
            ffi_source_roots,
            ffi_source_root_errors,
            source_items,
            has_imports,
            main_function_safe,
            checked_source_fingerprint,
        )
    }

    /// Rebuild the independent source census from a live CheckedProgram and
    /// the supplied MIR owner tables, then compare its private seal. Narrow
    /// Checker-aware verifier routes use this when their MIR graph is a
    /// deliberately isolated projection and cannot be compared with the
    /// general ResolvedBody lowering digest.
    pub(crate) fn matches_checked_program(
        &self,
        program: &CheckedProgram,
        functions: &BTreeMap<NodeId, super::MirFunction>,
        instances: &BTreeMap<super::MirInstanceId, MirInstance>,
    ) -> bool {
        let expected = Self::checked(program, functions, instances, None);
        self.witness == expected.witness
            && self.checked_source_fingerprint == expected.checked_source_fingerprint
    }

    fn seal(
        authority: CensusAuthority,
        function_owners: Vec<NodeId>,
        owners: BTreeMap<NodeId, MirProofOwnerCensus>,
        checked_callables: BTreeMap<NodeId, MirProofSourceCallableCensus>,
        declarations: Vec<MirProofSourceDeclaration>,
        ffi_source_roots: Vec<MirProofSourceFfiRoot>,
        ffi_source_root_errors: Vec<String>,
        source_items: Vec<MirProofSourceItem>,
        has_imports: bool,
        main_function_safe: bool,
        checked_source_fingerprint: String,
    ) -> Self {
        let mut census = Self {
            authority,
            function_owners,
            owners,
            checked_callables,
            declarations,
            ffi_source_roots,
            ffi_source_root_errors,
            source_items,
            has_imports,
            main_function_safe,
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

    /// Declaration-owned extern roots resolved through Checker-issued root
    /// and call handles. A default root with no activation remains
    /// declaration-only and is not treated as a call.
    pub fn ffi_source_roots(&self) -> &[MirProofSourceFfiRoot] {
        &self.ffi_source_roots
    }

    pub(crate) fn ffi_source_root_errors(&self) -> &[String] {
        &self.ffi_source_root_errors
    }

    /// Whether the sealed CheckedProgram source inventory is exactly the
    /// import-free MapEmptyRootRootQueriesV1 declaration profile. The raw MIR
    /// graph cannot reveal constants or other dormant source declarations, so
    /// this decision uses the independent item census attached at construction.
    pub fn admits_map_empty_root_root_queries_v1_source(&self) -> bool {
        if self.authority != CensusAuthority::CheckedSource
            || self.has_imports
            || !self.declarations.is_empty()
            || !self.main_function_safe
            || self.source_items.len() != 4
        {
            return false;
        }

        let main_count = self
            .source_items
            .iter()
            .filter(|item| {
                item.node_id.0 == "function:main"
                    && item.qualified_name == "main"
                    && item.kind == ResolvedItemKind::Function
                    && matches!(&item.origin, Origin::User(_))
            })
            .count();
        let generated_main_flow = self.source_items.iter().find(|item| {
            item.kind == ResolvedItemKind::Flow
                && item.node_id.0 == "flow:Main"
                && item.qualified_name == "Main"
                && matches!(
                    &item.origin,
                    Origin::RuntimeSystem { parent, rule, .. }
                        if parent.0 == "function:main" && rule == "progressive.main"
                )
        });
        let Some(generated_main_flow) = generated_main_flow else {
            return false;
        };
        let generated_flow_span = generated_main_flow.origin.user_span();
        let generated_fault_nominal_count = |name: &str| {
            self.source_items
                .iter()
                .filter(|item| {
                    item.kind == ResolvedItemKind::Type
                        && item.node_id.0 == format!("type:flow::Main::{name}")
                        && item.qualified_name == format!("flow::Main::{name}")
                        && matches!(
                            &item.origin,
                            Origin::Desugared { parent, rule, span }
                                if parent.0 == "compilation:root"
                                    && rule == "flow_matrix.fault_nominal"
                                    && *span == generated_flow_span
                        )
                })
                .count()
        };
        main_count == 1
            && generated_fault_nominal_count("EventId") == 1
            && generated_fault_nominal_count("StateId") == 1
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
                    .or({
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
        let mut output = String::from("mir.proof-source-census.v8\n");
        atom(
            &mut output,
            match self.authority {
                CensusAuthority::CheckedSource => "checked",
                CensusAuthority::Unavailable => "unavailable",
            },
        );
        output.push('\n');
        output.push_str("  source-imports=");
        atom(
            &mut output,
            if self.has_imports {
                "present"
            } else {
                "absent"
            },
        );
        output.push_str(" main-safe=");
        atom(
            &mut output,
            if self.main_function_safe {
                "true"
            } else {
                "false"
            },
        );
        output.push('\n');
        for item in &self.source_items {
            output.push_str("  source-item=");
            atom(&mut output, &item.node_id.0);
            atom(&mut output, &item.qualified_name);
            atom(&mut output, resolved_item_kind_text(item.kind));
            atom(&mut output, &format!("{:?}", item.origin));
            output.push('\n');
        }
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
                atom(&mut output, &format!("{:#?}", call.match_path));
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
                    atom(&mut output, &format!("{:?}", argument.eager_source_nodes));
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
            for root in &entry.default_proof_roots {
                output.push_str("    default-proof-root=");
                atom(&mut output, &format!("{:#?}", root));
                output.push('\n');
            }
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
                atom(&mut output, &call.caller_qualified_name);
                atom(&mut output, &format!("{:?}", call.origin));
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
                    atom(&mut output, &format!("{:?}", argument.eager_source_nodes));
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
        for root in &self.ffi_source_roots {
            output.push_str("  ffi-source-root=");
            atom(&mut output, &root.root_node.0);
            atom(&mut output, &root.owner.0);
            atom(&mut output, &format!("{:?}", root.kind));
            atom(&mut output, &format!("{:?}", root.identity));
            atom(&mut output, &format!("{:?}", root.origin));
            atom(&mut output, &root.body_fingerprint);
            atom(
                &mut output,
                if root.call_census_valid {
                    "calls-valid"
                } else {
                    "calls-invalid"
                },
            );
            atom(&mut output, &format!("{:?}", root.execution));
            output.push('\n');
            for activation in &root.activations {
                output.push_str("    ffi-default-activation=");
                atom(&mut output, &activation.caller_owner.0);
                atom(&mut output, &activation.call_node.0);
                atom(&mut output, &activation.callee_owner.0);
                atom(&mut output, &activation.parameter.0 .0);
                atom(&mut output, &activation.argument_node.0);
                atom(&mut output, &activation.parameter_index.to_string());
                atom(&mut output, &activation.order.to_string());
                atom(&mut output, &format!("{:?}", activation.origin));
                output.push('\n');
            }
            for call in &root.calls {
                output.push_str("    ffi-source-call=");
                atom(&mut output, &call.root_node.0);
                atom(&mut output, &call.call_node.0);
                atom(&mut output, &call.declaration.0);
                atom(&mut output, &call.symbol);
                atom(&mut output, &format!("{:?}", call.call_origin));
                atom(&mut output, &call.call_fingerprint);
                output.push('\n');
            }
        }
        for error in &self.ffi_source_root_errors {
            output.push_str("  ffi-source-root-error=");
            atom(&mut output, error);
            output.push('\n');
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
            append_type_path_map(&mut output, &declaration.requires_types);
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
            append_type_path_map(&mut output, &declaration.ensures_types);
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
        if self.authority == CensusAuthority::CheckedSource {
            let source_fingerprints = self
                .checked_callables
                .iter()
                .map(|(owner, entry)| (owner.clone(), entry.source_fingerprint.clone()))
                .collect::<BTreeMap<_, _>>();
            let checked_source_material = format!(
                "callables={source_fingerprints:#?}\ndeclarations={:#?}\nffi_source_roots={:#?}\nffi_source_root_errors={:#?}\ninventory={:#?}\nsource_items={:#?}\nhas_imports={}\nmain_function_safe={}",
                self.declarations,
                self.ffi_source_roots,
                self.ffi_source_root_errors,
                self.checked_callables,
                self.source_items,
                self.has_imports,
                self.main_function_safe,
            );
            let checked_source_fingerprint = blake3::hash(checked_source_material.as_bytes())
                .to_hex()
                .to_string();
            if self.checked_source_fingerprint != checked_source_fingerprint {
                return Err(MirProofCensusIntegrityError {
                    message: "checked source fingerprint does not bind the complete source-root and declaration inventory".into(),
                });
            }
        }
        let mut ffi_root_nodes = BTreeSet::new();
        let mut ffi_call_nodes = BTreeMap::<NodeId, NodeId>::new();
        for root in &self.ffi_source_roots {
            if !ffi_root_nodes.insert(root.root_node.clone()) {
                return Err(MirProofCensusIntegrityError {
                    message: format!(
                        "declaration FFI root '{}' is duplicated in the proof census",
                        root.root_node.0
                    ),
                });
            }
            if root
                .calls
                .iter()
                .any(|call| call.root_node != root.root_node)
            {
                return Err(MirProofCensusIntegrityError {
                    message: format!(
                        "declaration FFI root '{}' contains a cross-root call receipt",
                        root.root_node.0
                    ),
                });
            }
            for call in &root.calls {
                if let Some(previous) =
                    ffi_call_nodes.insert(call.call_node.clone(), root.root_node.clone())
                {
                    return Err(MirProofCensusIntegrityError {
                        message: format!(
                            "declaration FFI call '{}' is duplicated or cross-bound between roots '{}' and '{}'",
                            call.call_node.0, previous.0, root.root_node.0
                        ),
                    });
                }
            }
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
            "authority={:?}\nfunctions={:#?}\nowners={:#?}\nchecked_callables={:#?}\ndeclarations={:#?}\nffi_source_roots={:#?}\nffi_source_root_errors={:#?}\nsource_items={:#?}\nhas_imports={}\nmain_function_safe={}\nsource={}",
            self.authority,
            self.function_owners,
            self.owners,
            self.checked_callables,
            self.declarations,
            self.ffi_source_roots,
            self.ffi_source_root_errors,
            self.source_items,
            self.has_imports,
            self.main_function_safe,
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

pub(crate) type CallableDefaultKey = (NodeId, ResolvedParameterId);

#[derive(Debug)]
struct CheckerCallableDefaultUseCandidate {
    receipt: TypedDefaultProofUse,
    enclosing_defaults: Vec<CallableDefaultKey>,
}

#[derive(Debug)]
struct CheckedCallableDefaultUse {
    target: CallableDefaultKey,
    enclosing_defaults: Vec<CallableDefaultKey>,
    disposition: TypedCallExecutionDisposition,
    activation: MirProofSourceFfiDefaultActivation,
}

#[derive(Debug, Default)]
struct CheckedCallableDefaultActivationClosure {
    activated: BTreeSet<CallableDefaultKey>,
    activations_by_default: BTreeMap<CallableDefaultKey, Vec<MirProofSourceFfiDefaultActivation>>,
    errors: Vec<String>,
}

const DEFAULT_EXPRESSION_CALL_DISPOSITION: &str =
    "default expression call requires exact omitted-argument expansion";

fn is_known_non_runtime_deferred_default_use(reason: &str) -> bool {
    matches!(
        reason,
        "lambda body is latent until a checked invocation path exists"
            | "comptime/quote body is not a runtime call point"
            | "type-of operand is not an execution point"
            | "old(...) is a snapshot term, not a runtime call point"
            | "contract expression is an obligation, not a runtime call point"
            | "call inside callable contract is a proof term, not a runtime call point"
            | "call inside loop invariant is a proof term, not a runtime call point"
            | "call inside nested contract is a proof term, not a runtime call point"
            | "call inside Math is a proof term, not a runtime call point"
            | "math term is a proof obligation, not a runtime call point"
    )
}

fn classify_callable_default_use(
    usage: &CheckedCallableDefaultUse,
    activated_defaults: &BTreeSet<CallableDefaultKey>,
) -> Result<bool, String> {
    let has_enclosing_default = !usage.enclosing_defaults.is_empty();
    if has_enclosing_default
        && !usage
            .enclosing_defaults
            .iter()
            .all(|default| activated_defaults.contains(default))
    {
        // Even an unsupported or possibly-executing call stays dormant while
        // its exact enclosing declaration default is dormant.
        return Ok(false);
    }

    match &usage.disposition {
        TypedCallExecutionDisposition::Unconditional | TypedCallExecutionDisposition::Guarded => {
            Ok(true)
        }
        TypedCallExecutionDisposition::Deferred(reason)
            if has_enclosing_default && reason == DEFAULT_EXPRESSION_CALL_DISPOSITION =>
        {
            // The Checker defers ordinary calls in a default only because the
            // default has not yet been expanded. Once every exact enclosing
            // default is active, this receipt propagates activation.
            Ok(true)
        }
        TypedCallExecutionDisposition::Deferred(reason)
            if is_known_non_runtime_deferred_default_use(reason) =>
        {
            // These exact Checker reasons identify latent lambda bodies or
            // terms that have no runtime execution point. CheckedProgram has
            // no closure invocation receipt that could make the lambda case
            // executable; it remains fail-closed until that authority exists.
            Ok(false)
        }
        TypedCallExecutionDisposition::Deferred(reason) => Err(reason.clone()),
    }
}

/// Shared authenticated activation closure for callable parameter defaults.
/// The verifier and the MIR source census must use the same result so a
/// declaration default cannot be declaration-only in one consumer and active
/// in another.
pub(crate) fn checked_ffi_callable_default_activation_closure(
    program: &CheckedProgram,
) -> Result<BTreeSet<CallableDefaultKey>, String> {
    let closure = compute_checked_callable_default_activation_closure(program, None);
    if closure.errors.is_empty() {
        Ok(closure.activated)
    } else {
        Err(closure.errors.join("; "))
    }
}

fn compute_checked_callable_default_activation_closure(
    program: &CheckedProgram,
    excluded_sources: Option<&std::collections::HashSet<crate::span::SourceId>>,
) -> CheckedCallableDefaultActivationClosure {
    let mut closure = CheckedCallableDefaultActivationClosure::default();
    let mut all_default_expressions = BTreeMap::<CallableDefaultKey, NodeId>::new();
    let mut eligible_defaults = BTreeSet::<CallableDefaultKey>::new();
    for callable in program.callables().values() {
        for (parameter, expression) in &callable.body.default_values {
            let has_default = callable
                .signature
                .parameters
                .iter()
                .any(|candidate| &candidate.id == parameter && candidate.has_default);
            if !has_default {
                continue;
            }
            let key = (callable.owner.clone(), parameter.clone());
            if all_default_expressions
                .insert(key.clone(), expression.node_id.clone())
                .is_some()
            {
                closure.errors.push(format!(
                    "callable default '{}:{}' has a duplicate Checker default-expression identity",
                    key.0 .0, key.1 .0 .0
                ));
            }
            if !excluded_sources
                .is_some_and(|excluded| excluded.contains(&expression.origin.user_span().source_id))
            {
                eligible_defaults.insert(key);
            }
        }
    }

    let mut relevant_defaults = BTreeSet::<CallableDefaultKey>::new();
    for root in program.checked_ffi_source_roots() {
        if root.kind()
            != crate::core::ir::lower::ResolvedDeclarationExpressionKind::CallableParameterDefault
            || root.calls().is_empty()
            || excluded_sources.is_some_and(|excluded| {
                root.origin()
                    .is_some_and(|origin| excluded.contains(&origin.user_span().source_id))
            })
        {
            continue;
        }
        if let Some(
            crate::core::ir::lower::ResolvedDeclarationRootIdentity::CallableParameterDefault {
                callable,
                parameter,
            },
        ) = root.identity()
        {
            let key = (callable.clone(), parameter.clone());
            if eligible_defaults.contains(&key) {
                relevant_defaults.insert(key);
            }
        }
    }

    let mut candidates = Vec::<CheckerCallableDefaultUseCandidate>::new();
    let mut call_points_by_node = BTreeMap::<NodeId, Vec<TypedCallProofPoint>>::new();
    for caller in program.callables().values() {
        if excluded_sources.is_some_and(|excluded| {
            excluded.contains(&caller.body.root.origin.user_span().source_id)
        }) {
            continue;
        }
        let worklist = caller.proof_point_worklist();
        for point in worklist
            .calls
            .into_iter()
            .chain(worklist.deferred_calls)
            .chain(
                worklist
                    .default_roots
                    .into_iter()
                    .flat_map(|root| root.calls),
            )
        {
            call_points_by_node
                .entry(point.call_node.clone())
                .or_default()
                .push(point);
        }
        for use_receipt in worklist.default_uses {
            let target_key = (
                use_receipt.callee_owner.clone(),
                use_receipt.parameter.clone(),
            );
            // Trait defaults and other non-callable default surfaces do not
            // participate in this closure. They remain explicit declaration
            // boundaries until they have their own activation receipt.
            if !all_default_expressions.contains_key(&target_key) {
                continue;
            }
            let enclosing_defaults = use_receipt
                .scope
                .iter()
                .filter_map(|frame| match frame {
                    TypedProofScopeFrame::Default { parameter, .. } => {
                        Some((use_receipt.caller_owner.clone(), parameter.clone()))
                    }
                    _ => None,
                })
                .collect();
            candidates.push(CheckerCallableDefaultUseCandidate {
                receipt: use_receipt,
                enclosing_defaults,
            });
        }
    }

    // Work backward from actual FFI-bearing callable defaults. An enclosing
    // default matters only if it can lead to one of those roots; unrelated
    // method/default receipts stay outside this classifier.
    loop {
        let mut changed = false;
        for candidate in &candidates {
            let target = (
                candidate.receipt.callee_owner.clone(),
                candidate.receipt.parameter.clone(),
            );
            if !relevant_defaults.contains(&target) {
                continue;
            }
            for enclosing in &candidate.enclosing_defaults {
                if all_default_expressions.contains_key(enclosing)
                    && eligible_defaults.contains(enclosing)
                    && relevant_defaults.insert(enclosing.clone())
                {
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    let mut checked_uses = Vec::<CheckedCallableDefaultUse>::new();
    for candidate in candidates {
        let use_receipt = candidate.receipt;
        let target_key = (
            use_receipt.callee_owner.clone(),
            use_receipt.parameter.clone(),
        );
        if !eligible_defaults.contains(&target_key) || !relevant_defaults.contains(&target_key) {
            continue;
        }
        let Some(target) = program.callable(&use_receipt.callee_owner) else {
            closure.errors.push(format!(
                "callable default activation '{}:{}' has no checked target callable",
                target_key.0 .0, target_key.1 .0 .0
            ));
            continue;
        };
        if !use_receipt.validate_in_checked_program(program) {
            closure.errors.push(format!(
                "callable default activation '{}:{}' has a malformed Checker default-use receipt at '{}'",
                target_key.0 .0, target_key.1 .0 .0, use_receipt.call_node.0
            ));
            continue;
        }
        let Some(caller) = program.callable(&use_receipt.caller_owner) else {
            closure.errors.push(format!(
                "callable default activation '{}:{}' has no checked caller owner at '{}'",
                target_key.0 .0, target_key.1 .0 .0, use_receipt.call_node.0
            ));
            continue;
        };
        let enclosing_defaults = match checked_default_scope_chain(
            &use_receipt,
            caller,
            &all_default_expressions,
        ) {
            Ok(chain) => chain,
            Err(message) => {
                closure.errors.push(format!(
                    "callable default activation '{}:{}' at '{}' has an invalid enclosing default chain: {message}",
                    target_key.0.0, target_key.1.0.0, use_receipt.call_node.0
                ));
                continue;
            }
        };
        let call_points = call_points_by_node
            .get(&use_receipt.call_node)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let [point] = call_points else {
            closure.errors.push(format!(
                "callable default activation '{}:{}' at '{}' has no unique typed caller Call",
                target_key.0 .0, target_key.1 .0 .0, use_receipt.call_node.0
            ));
            continue;
        };
        let Some(site) = program.call_sites().get(&use_receipt.call_node) else {
            closure.errors.push(format!(
                "callable default activation '{}:{}' at '{}' is absent from the checked call-site directory",
                target_key.0.0, target_key.1.0.0, use_receipt.call_node.0
            ));
            continue;
        };
        let matching_site_count = program
            .call_sites()
            .values()
            .filter(|candidate| candidate.node_id == use_receipt.call_node)
            .count();
        if !checked_default_activation_site_matches(
            &use_receipt,
            caller,
            target,
            point,
            site,
            matching_site_count,
        ) {
            closure.errors.push(format!(
                "callable default activation '{}:{}' at '{}' disagrees with the exact checked caller Call and syntax call-site identity",
                target_key.0.0, target_key.1.0.0, use_receipt.call_node.0
            ));
            continue;
        }
        checked_uses.push(CheckedCallableDefaultUse {
            target: target_key,
            enclosing_defaults,
            disposition: use_receipt.disposition.clone(),
            activation: MirProofSourceFfiDefaultActivation {
                caller_owner: use_receipt.caller_owner.clone(),
                call_node: use_receipt.call_node.clone(),
                callee_owner: use_receipt.callee_owner.clone(),
                parameter: use_receipt.parameter.clone(),
                argument_node: use_receipt.argument_node.clone(),
                parameter_index: use_receipt.parameter_index,
                order: use_receipt.order,
                scope: source_proof_scope(&use_receipt.scope),
                guards: use_receipt
                    .guards
                    .iter()
                    .map(|guard| (guard.condition.node_id.clone(), guard.when))
                    .collect(),
                disposition: use_receipt.disposition.clone(),
                origin: site.origin.clone(),
            },
        });
    }

    // A direct body use is a root only when the Checker placed it in an
    // executable body path. Calls inside a declaration default remain latent
    // until every exact enclosing default expression is itself activated.
    loop {
        let mut changed = false;
        for usage in &checked_uses {
            match classify_callable_default_use(usage, &closure.activated) {
                Ok(true) if closure.activated.insert(usage.target.clone()) => changed = true,
                Ok(_) => {}
                Err(reason) => closure.errors.push(format!(
                    "callable default activation '{}:{}' at '{}' crosses a possibly executable Deferred boundary: '{reason}'",
                    usage.target.0 .0, usage.target.1 .0 .0, usage.activation.call_node.0
                )),
            }
        }
        if !changed {
            break;
        }
    }

    for usage in checked_uses {
        match classify_callable_default_use(&usage, &closure.activated) {
            Ok(true) => closure
                .activations_by_default
                .entry(usage.target)
                .or_default()
                .push(usage.activation),
            Ok(false) => {}
            Err(reason) => closure.errors.push(format!(
                "callable default activation '{}:{}' at '{}' crosses a possibly executable Deferred boundary: '{reason}'",
                usage.target.0 .0, usage.target.1 .0 .0, usage.activation.call_node.0
            )),
        }
    }
    for activations in closure.activations_by_default.values_mut() {
        activations.sort_by(|left, right| {
            left.caller_owner
                .cmp(&right.caller_owner)
                .then_with(|| left.call_node.cmp(&right.call_node))
                .then_with(|| left.argument_node.cmp(&right.argument_node))
                .then_with(|| left.parameter_index.cmp(&right.parameter_index))
                .then_with(|| left.order.cmp(&right.order))
        });
        let original_len = activations.len();
        activations.dedup();
        if activations.len() != original_len {
            closure.errors.push(
                "duplicate Checker default-use activation receipt in FFI source census".into(),
            );
        }
    }
    closure.errors.sort();
    closure.errors.dedup();
    closure
}

fn checked_default_scope_chain(
    use_receipt: &TypedDefaultProofUse,
    caller: &crate::core::ir::ResolvedCallable,
    default_expressions: &BTreeMap<CallableDefaultKey, NodeId>,
) -> Result<Vec<CallableDefaultKey>, String> {
    let mut chain = Vec::new();
    let mut seen = BTreeSet::new();
    for frame in &use_receipt.scope {
        let TypedProofScopeFrame::Default {
            parameter,
            expression,
        } = frame
        else {
            continue;
        };
        let key = (caller.owner.clone(), parameter.clone());
        if default_expressions.get(&key) != Some(expression) {
            return Err(format!(
                "default frame '{}:{}' does not bind its exact declaration expression",
                key.0 .0, key.1 .0 .0
            ));
        }
        if !seen.insert(key.clone()) {
            return Err(format!(
                "default frame '{}:{}' appears more than once in the activation scope",
                key.0 .0, key.1 .0 .0
            ));
        }
        chain.push(key);
    }
    Ok(chain)
}

fn checked_default_activation_site_matches(
    use_receipt: &TypedDefaultProofUse,
    caller: &crate::core::ir::ResolvedCallable,
    target: &crate::core::ir::ResolvedCallable,
    point: &TypedCallProofPoint,
    site: &ResolvedCallSite,
    matching_site_count: usize,
) -> bool {
    let ResolvedExprKind::Call(typed_call) = &point.expression.kind else {
        return false;
    };
    let explicit_argc = typed_call
        .arguments
        .iter()
        .filter(|argument| {
            !matches!(
                &argument.value.kind,
                ResolvedExprKind::DefaultArgument { .. }
            )
        })
        .count();
    let slots_match_signature = typed_call.arguments.len() == target.signature.parameters.len()
        && typed_call
            .arguments
            .iter()
            .zip(&target.signature.parameters)
            .all(|(argument, parameter)| {
                argument.parameter == parameter.id
                    && match &argument.value.kind {
                        ResolvedExprKind::DefaultArgument {
                            callable,
                            parameter: marker,
                        } => callable == &target.owner && marker == &parameter.id,
                        _ => true,
                    }
            });
    let expected_arity_matches = site
        .expected_argc
        .is_none_or(|expected| expected == target.signature.parameters.len());
    matching_site_count == 1
        && use_receipt.call_node == point.call_node
        && point.body_owner == caller.owner
        && point.validate_against(caller)
        && typed_call.callee == ResolvedCallee::Function(target.owner.clone())
        && site.node_id == use_receipt.call_node
        && site.owner == caller.owner.0
        && site.kind == crate::core::ResolvedCallKind::Function
        && site.target_function.as_ref() == Some(&target.owner)
        && site.resolved_callee.as_ref() == Some(&typed_call.callee)
        && site.origin == point.expression.origin
        && site.argc == explicit_argc
        && expected_arity_matches
        && slots_match_signature
}

fn checked_ffi_source_roots(
    program: &CheckedProgram,
    excluded_sources: Option<&std::collections::HashSet<crate::span::SourceId>>,
) -> (Vec<MirProofSourceFfiRoot>, Vec<String>) {
    use crate::core::ir::lower::{
        ResolvedDeclarationExpressionKind as Kind, ResolvedDeclarationRootIdentity as Identity,
    };

    let handles = program
        .checked_ffi_source_roots()
        .iter()
        .filter(|root| {
            !excluded_sources.is_some_and(|excluded| {
                root.origin()
                    .is_some_and(|origin| excluded.contains(&origin.user_span().source_id))
            })
        })
        .collect::<Vec<_>>();
    let activation_closure =
        compute_checked_callable_default_activation_closure(program, excluded_sources);
    let mut errors = activation_closure.errors;
    let activations_by_default = activation_closure.activations_by_default;

    let mut roots = Vec::with_capacity(handles.len());
    for root in handles {
        let execution = match root.kind() {
            Kind::ConstantInitializer => MirProofSourceFfiRootExecution::ConstantInitializer,
            Kind::ActorFieldInitializer => MirProofSourceFfiRootExecution::ActorConstructionPending,
            Kind::TraitParameterDefault => {
                MirProofSourceFfiRootExecution::TraitDefaultDeclarationOnly
            }
            Kind::CallableParameterDefault => {
                let has_activation = match root.identity() {
                    Some(Identity::CallableParameterDefault {
                        callable,
                        parameter,
                    }) => activations_by_default
                        .get(&(callable.clone(), parameter.clone()))
                        .is_some_and(|activations| !activations.is_empty()),
                    _ => false,
                };
                if has_activation {
                    MirProofSourceFfiRootExecution::CallableDefaultActivatedUnsupported
                } else {
                    MirProofSourceFfiRootExecution::CallableDefaultDeclarationOnly
                }
            }
            Kind::ExternRequires | Kind::ExternEnsures => {
                errors.push(format!(
                    "unexpected FFI source-call root kind {:?} at '{}'",
                    root.kind(),
                    root.root_node().0
                ));
                continue;
            }
        };
        let identity = root.identity().cloned();
        if identity.is_none() {
            errors.push(format!(
                "Checker FFI source root '{}' has no declaration owner identity",
                root.root_node().0
            ));
        }
        if !root.has_valid_witness() || !root.has_valid_call_census() {
            errors.push(format!(
                "Checker FFI source root '{}' has an invalid root or direct-call witness",
                root.root_node().0
            ));
        }
        if let Err(failures) = program.checked_ffi_source_root_body(root) {
            errors.push(format!(
                "Checker FFI source root '{}' failed body-handle resolution: {}",
                root.root_node().0,
                failures
                    .iter()
                    .map(|failure| failure.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        let mut calls = Vec::with_capacity(root.calls().len());
        for call in root.calls() {
            match program.checked_ffi_source_call_context(root, call) {
                Ok(context) => {
                    calls.push(MirProofSourceFfiRootCall {
                        root_node: root.root_node().clone(),
                        call_node: call.call_node().clone(),
                        declaration: call.external_declaration().clone(),
                        symbol: context.declaration.symbol.clone(),
                        call_origin: context.call.origin.clone(),
                        call_fingerprint: blake3::hash(
                            format!("{:#?}", context.call).as_bytes(),
                        )
                        .to_hex()
                        .to_string(),
                    });
                }
                Err(failures) => errors.push(format!(
                    "Checker FFI source call '{}' in root '{}' failed exact call-handle resolution: {}",
                    call.call_node().0,
                    root.root_node().0,
                    failures
                        .iter()
                        .map(|failure| failure.message.as_str())
                        .collect::<Vec<_>>()
                        .join("; ")
                )),
            }
        }
        calls.sort_by(|left, right| left.call_node.cmp(&right.call_node));
        let activations = match root.identity() {
            Some(Identity::CallableParameterDefault {
                callable,
                parameter,
            }) => activations_by_default
                .get(&(callable.clone(), parameter.clone()))
                .cloned()
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        roots.push(MirProofSourceFfiRoot {
            root_node: root.root_node().clone(),
            owner: root.owner().clone(),
            kind: root.kind(),
            identity,
            origin: root.origin().cloned(),
            body_fingerprint: root.body_fingerprint().to_hex().to_string(),
            call_census_valid: root.has_valid_call_census(),
            execution,
            activations,
            calls,
        });
    }
    roots.sort_by(|left, right| {
        left.root_node
            .cmp(&right.root_node)
            .then_with(|| left.owner.cmp(&right.owner))
    });
    errors.sort();
    errors.dedup();
    (roots, errors)
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
    complete_call_metadata_eager_nodes(call_metadata.values_mut(), &callable.body.place_inputs);
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
                caller_qualified_name: checked_callable_qualified_name(program, &callable.owner),
                origin: metadata.map(|metadata| metadata.source_origin.clone()),
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

#[cfg(test)]
mod ffi_source_root_census_tests {
    use super::*;

    fn canonical_with_two_constant_ffi_roots(
    ) -> (CheckedProgram, crate::core::mir::reference::MirProgram) {
        let source = r#"
            extern "C" {
                func positive(value: i32) -> i32 requires: value > 0;
            }
            const FIRST: i32 = positive(1);
            const SECOND: i32 = positive(2);
            func main() -> i32 { 0 }
        "#;
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("lex FFI declaration-root source");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse FFI declaration-root source");
        let checked = crate::core::check_program(&file).expect("check FFI declaration roots");
        let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
            .expect("materialize Checker-rooted MIR census");
        (checked, program)
    }

    fn checked_from_source(source: &str) -> CheckedProgram {
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("lex callable-default activation source");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse callable-default activation source");
        crate::core::check_program(&file).expect("check callable-default activation source")
    }

    fn reseal_outer_witness(census: &mut MirProofSourceCensus) {
        census.witness = census.compute_witness();
    }

    #[test]
    fn constant_ffi_root_census_binds_checker_root_call_and_span() {
        let (checked, program) = canonical_with_two_constant_ffi_roots();
        program
            .validate_ffi_source_census()
            .expect("canonical MIR consumes and validates declaration-root calls");
        let roots = program
            .proof_source_census()
            .ffi_source_roots()
            .iter()
            .filter(|root| root.kind == crate::core::ir::lower::ResolvedDeclarationExpressionKind::ConstantInitializer)
            .collect::<Vec<_>>();
        assert_eq!(roots.len(), 2);
        for root in roots {
            let checker_root = checked
                .checked_ffi_source_roots()
                .iter()
                .find(|checker_root| checker_root.root_node() == &root.root_node)
                .expect("MIR root retains the exact Checker-issued root identity");
            assert!(root.call_census_valid);
            assert_eq!(
                root.execution,
                MirProofSourceFfiRootExecution::ConstantInitializer
            );
            assert_eq!(
                root.identity,
                Some(
                    crate::core::ir::lower::ResolvedDeclarationRootIdentity::Constant {
                        declaration: root.owner.clone()
                    }
                )
            );
            let [call] = root.calls.as_slice() else {
                panic!("each constant root has exactly one typed FFI call");
            };
            let [checker_call] = checker_root.calls() else {
                panic!("each Checker constant root has exactly one FFI call handle");
            };
            let checker_context = checked
                .checked_ffi_source_call_context(checker_root, checker_call)
                .expect("Checker root/call handles resolve to the typed source Call");
            assert_eq!(call.root_node, root.root_node);
            assert_eq!(call.root_node, *checker_root.root_node());
            assert_eq!(call.call_node, *checker_call.call_node());
            assert_eq!(call.declaration, *checker_call.external_declaration());
            assert_eq!(call.symbol, "positive");
            assert_eq!(call.call_origin, checker_context.call.origin);
            let checker_declaration = checked
                .ffi_declarations()
                .get(checker_call.external_declaration())
                .expect("Checker-issued extern declaration identity exists");
            assert_eq!(checker_declaration.declaration, call.declaration);
            let declaration = program
                .proof_source_census()
                .declaration(&call.declaration)
                .expect("Checker-issued call declaration is in the census directory");
            assert_eq!(declaration.declaration, call.declaration);
            assert_eq!(declaration.symbol, call.symbol);
            assert_eq!(declaration.abi, "C");
            assert!(declaration.requires_declared);
            assert_ne!(call.call_origin.user_span(), crate::span::Span::UNKNOWN);
            assert_eq!(call.call_fingerprint.len(), 64);
        }
    }

    #[test]
    fn ffi_root_census_fails_closed_for_tamper_omission_duplicate_and_cross_root_calls() {
        let (_, program) = canonical_with_two_constant_ffi_roots();
        let original = program.proof_source_census().clone();

        let mut tampered = original.clone();
        tampered.ffi_source_roots[0].calls[0].symbol = "forged".into();
        reseal_outer_witness(&mut tampered);
        let error = tampered
            .validate_integrity(program.functions())
            .expect_err("changed source-call symbol must fail the Checker source seal");
        assert!(error.message.contains("checked source fingerprint"));

        let mut omitted = original.clone();
        omitted.ffi_source_roots.remove(0);
        reseal_outer_witness(&mut omitted);
        let error = omitted
            .validate_integrity(program.functions())
            .expect_err("omitted declaration root must fail the Checker source seal");
        assert!(error.message.contains("checked source fingerprint"));

        let mut repeated = original.clone();
        let repeated_call = repeated.ffi_source_roots[0].calls[0].clone();
        repeated.ffi_source_roots[0].calls.push(repeated_call);
        reseal_outer_witness(&mut repeated);
        let error = repeated
            .validate_integrity(program.functions())
            .expect_err("repeated source call must fail closed");
        assert!(
            error.message.contains("checked source fingerprint")
                || error.message.contains("duplicated or cross-bound"),
            "{error:?}"
        );

        let mut cross_root = original;
        let foreign_call = cross_root.ffi_source_roots[0].calls[0].clone();
        cross_root.ffi_source_roots[1].calls.push(foreign_call);
        reseal_outer_witness(&mut cross_root);
        let error = cross_root
            .validate_integrity(program.functions())
            .expect_err("call receipt moved across declaration roots must fail closed");
        assert!(
            error.message.contains("checked source fingerprint")
                || error.message.contains("cross-root call receipt"),
            "{error:?}"
        );
    }

    #[test]
    fn ffi_callable_default_activation_closure_tracks_direct_nested_and_middle_omission_uses() {
        let source = r#"
            extern "C" {
                func positive(value: i32) -> i32 requires: value > 0;
            }
            func direct_default(value: i32 = positive(10)) -> i32 { value }
            func supplied_default(value: i32 = positive(20)) -> i32 { value }
            func dormant_inner(value: i32 = positive(30)) -> i32 { value }
            func dormant_outer(value: i32 = dormant_inner()) -> i32 { value }
            func active_inner(value: i32 = positive(40)) -> i32 { value }
            func active_outer(value: i32 = active_inner()) -> i32 { value }
            func middle_default(a: i32 = 1, middle: i32 = positive(a), c: i32 = 3) -> i32 { middle }
            func main() -> i32 {
                let direct = direct_default()
                let supplied = supplied_default(21)
                let active = active_outer()
                let middle = middle_default(a = 2, c = 4)
                direct + supplied + active + middle
            }
        "#;
        let checked = checked_from_source(source);
        let census = MirProofSourceCensus::checked_ffi_declaration_roots(&checked);
        census
            .validate_ffi_source_roots()
            .expect("shared activation closure validates exact declaration roots");

        let root_for_owner = |owner: &str| {
            census
                .ffi_source_roots()
                .iter()
                .find(|root| {
                    root.owner().0 == format!("function:{owner}")
                        && root.kind()
                            == crate::core::ir::lower::ResolvedDeclarationExpressionKind::CallableParameterDefault
                })
                .unwrap_or_else(|| panic!("callable default FFI root for {owner}"))
        };
        let direct = root_for_owner("direct_default");
        assert_eq!(
            direct.execution(),
            MirProofSourceFfiRootExecution::CallableDefaultActivatedUnsupported
        );
        assert_eq!(direct.activations().len(), 1);

        let supplied = root_for_owner("supplied_default");
        assert_eq!(
            supplied.execution(),
            MirProofSourceFfiRootExecution::CallableDefaultDeclarationOnly
        );
        assert!(supplied.activations().is_empty());

        let dormant = root_for_owner("dormant_inner");
        assert_eq!(
            dormant.execution(),
            MirProofSourceFfiRootExecution::CallableDefaultDeclarationOnly
        );
        assert!(dormant.activations().is_empty());

        let active = root_for_owner("active_inner");
        assert_eq!(
            active.execution(),
            MirProofSourceFfiRootExecution::CallableDefaultActivatedUnsupported
        );
        let active_ffi_instruction = crate::core::mir::MirInstructionId::new(format!(
            "inst:call:{}",
            active.calls()[0].call_node().0
        ))
        .expect("canonical activated-default FFI instruction identity");
        let executable_identity_errors = crate::core::mir::validate_ffi_source_root_census(
            &census,
            &BTreeSet::from([active_ffi_instruction]),
        );
        assert!(executable_identity_errors.iter().any(|error| {
            error.contains("duplicated as an executable MIR Call")
                && error.contains(&active.calls()[0].call_node().0)
        }));
        let [nested_activation] = active.activations() else {
            panic!("activated nested default has exactly one authenticated use");
        };
        assert_eq!(nested_activation.caller_owner().0, "function:active_outer");
        assert!(nested_activation
            .scope()
            .iter()
            .any(|frame| matches!(frame, MirProofSourceScopeFrame::Default { .. })));

        let middle = root_for_owner("middle_default");
        assert_eq!(
            middle.execution(),
            MirProofSourceFfiRootExecution::CallableDefaultActivatedUnsupported
        );
        let [middle_activation] = middle.activations() else {
            panic!("omitted middle parameter has exactly one activation receipt");
        };
        assert_eq!(middle_activation.parameter_index(), 1);
        let site = checked
            .call_sites()
            .get(middle_activation.call_node())
            .expect("activation is bound to its exact syntax call site");
        assert_eq!(
            site.argc, 2,
            "only the supplied named slots count as syntax argc"
        );
        assert_eq!(
            site.expected_argc,
            Some(3),
            "expected argc is full signature arity"
        );
        assert_ne!(
            site.argc,
            site.expected_argc.expect("known full signature arity"),
            "an omitted middle default makes syntax argc differ from full arity"
        );

        let caller = checked
            .callable(&NodeId("function:main".into()))
            .expect("main callable");
        let target = checked
            .callable(&NodeId("function:middle_default".into()))
            .expect("middle-default target callable");
        let worklist = caller.proof_point_worklist();
        let use_receipts = worklist
            .default_uses
            .iter()
            .filter(|usage| {
                usage.callee_owner == target.owner
                    && usage.parameter_index == 1
                    && usage.call_node == *middle_activation.call_node()
            })
            .collect::<Vec<_>>();
        let [use_receipt] = use_receipts.as_slice() else {
            panic!("one Checker default-use receipt binds the omitted middle slot");
        };
        let matching_points = worklist
            .calls
            .iter()
            .chain(&worklist.deferred_calls)
            .chain(
                worklist
                    .default_roots
                    .iter()
                    .flat_map(|root| root.calls.iter()),
            )
            .filter(|point| point.call_node == *middle_activation.call_node())
            .collect::<Vec<_>>();
        let [point] = matching_points.as_slice() else {
            panic!("one typed caller Call owns the activation");
        };
        assert!(checked_default_activation_site_matches(
            use_receipt,
            caller,
            target,
            point,
            site,
            1,
        ));
        assert!(!checked_default_activation_site_matches(
            use_receipt,
            caller,
            target,
            point,
            site,
            2,
        ));
        let mut tampered_argc = site.clone();
        tampered_argc.argc += 1;
        assert!(!checked_default_activation_site_matches(
            use_receipt,
            caller,
            target,
            point,
            &tampered_argc,
            1,
        ));
        let mut tampered_expected = site.clone();
        tampered_expected.expected_argc = Some(2);
        assert!(!checked_default_activation_site_matches(
            use_receipt,
            caller,
            target,
            point,
            &tampered_expected,
            1,
        ));
        let mut tampered_target = site.clone();
        tampered_target.target_function = Some(NodeId("function:direct_default".into()));
        assert!(!checked_default_activation_site_matches(
            use_receipt,
            caller,
            target,
            point,
            &tampered_target,
            1,
        ));
        let mut tampered_owner = site.clone();
        tampered_owner.owner = "function:wrong_owner".into();
        assert!(!checked_default_activation_site_matches(
            use_receipt,
            caller,
            target,
            point,
            &tampered_owner,
            1,
        ));
        let mut tampered_node = site.clone();
        tampered_node.node_id = NodeId("call:wrong_node".into());
        assert!(!checked_default_activation_site_matches(
            use_receipt,
            caller,
            target,
            point,
            &tampered_node,
            1,
        ));
        let mut tampered_kind = site.clone();
        tampered_kind.kind = crate::core::ResolvedCallKind::Method;
        assert!(!checked_default_activation_site_matches(
            use_receipt,
            caller,
            target,
            point,
            &tampered_kind,
            1,
        ));
        let mut tampered_origin = site.clone();
        tampered_origin.origin = crate::core::Origin::User(crate::span::Span::UNKNOWN);
        assert!(!checked_default_activation_site_matches(
            use_receipt,
            caller,
            target,
            point,
            &tampered_origin,
            1,
        ));
        let mut tampered_resolved_callee = site.clone();
        tampered_resolved_callee.resolved_callee = Some(ResolvedCallee::Function(NodeId(
            "function:direct_default".into(),
        )));
        assert!(!checked_default_activation_site_matches(
            use_receipt,
            caller,
            target,
            point,
            &tampered_resolved_callee,
            1,
        ));
    }

    #[test]
    fn ffi_default_activation_rejects_direct_unsupported_loop_use() {
        let source = r#"
            extern "C" {
                func positive(value: i32) -> i32 requires: value > 0;
            }
            func loop_default(value: i32 = positive(10)) -> i32 { value }
            func main() -> i32 {
                let mut index = 0
                while index < 1 {
                    let value = loop_default()
                    index += 1
                }
                0
            }
        "#;
        let checked = checked_from_source(source);
        let error = checked_ffi_callable_default_activation_closure(&checked)
            .expect_err("an unsupported loop invocation must not become declaration-only");
        assert!(
            error.contains("possibly executable Deferred boundary")
                && error.contains(
                    "loop call point lacks an exact immutable at-most-once FFI path receipt"
                ),
            "loop activation fails closed with its Checker boundary reason: {error}"
        );
    }

    #[test]
    fn ffi_default_activation_rejects_active_outer_with_nested_block_use() {
        let source = r#"
            extern "C" {
                func positive(value: i32) -> i32 requires: value > 0;
            }
            func block_inner(value: i32 = positive(20)) -> i32 { value }
            func block_outer(value: i32 = {
                let nested = block_inner()
                nested
            }) -> i32 { value }
            func main() -> i32 { block_outer() }
        "#;
        let checked = checked_from_source(source);
        let error = checked_ffi_callable_default_activation_closure(&checked)
            .expect_err("the active outer default exposes the nested executable block call");
        assert!(
            error.contains("possibly executable Deferred boundary")
                && error.contains("nested expression block has no transfer timeline"),
            "active nested block fails closed with its Checker boundary reason: {error}"
        );
    }

    #[test]
    fn ffi_default_activation_keeps_unsupported_use_dormant_with_outer_default() {
        let source = r#"
            extern "C" {
                func positive(value: i32) -> i32 requires: value > 0;
            }
            func dormant_inner(value: i32 = positive(30)) -> i32 { value }
            func dormant_outer(value: i32 = {
                let nested = dormant_inner()
                nested
            }) -> i32 { value }
            func main() -> i32 { 0 }
        "#;
        let checked = checked_from_source(source);
        assert!(checked_ffi_callable_default_activation_closure(&checked)
            .expect("an unsupported use under a dormant default stays dormant")
            .is_empty());
        let census = MirProofSourceCensus::checked_ffi_declaration_roots(&checked);
        census
            .validate_ffi_source_roots()
            .expect("dormant nested default remains a valid declaration-only root");
        let inner = census
            .ffi_source_roots()
            .iter()
            .find(|root| {
                root.owner().0 == "function:dormant_inner"
                    && root.kind()
                        == crate::core::ir::lower::ResolvedDeclarationExpressionKind::CallableParameterDefault
            })
            .expect("dormant inner FFI default root");
        assert_eq!(
            inner.execution(),
            MirProofSourceFfiRootExecution::CallableDefaultDeclarationOnly
        );
        assert!(inner.activations().is_empty());
    }

    #[test]
    fn ffi_default_activation_keeps_exact_latent_and_proof_only_uses_inactive() {
        let source = r#"
            extern "C" {
                func positive(value: i32) -> i32 requires: value > 0;
            }
            func latent_inner(value: i32 = positive(40)) -> i32 { value }
            func latent_outer(callback: fn() -> i32 = fn() -> i32 { latent_inner() }) -> i32 { 0 }
            func proof_default(value: i32 = positive(50)) -> i32 { value }
            func main() -> i32 {
                math: { proof_default() > 0; }
                latent_outer()
            }
        "#;
        let checked = checked_from_source(source);
        let activated = checked_ffi_callable_default_activation_closure(&checked)
            .expect("latent lambda bodies and proof-only calls are not runtime roots");
        assert!(activated
            .iter()
            .any(|(owner, _)| owner.0 == "function:latent_outer"));
        assert!(!activated.iter().any(|(owner, _)| {
            owner.0 == "function:latent_inner" || owner.0 == "function:proof_default"
        }));

        let census = MirProofSourceCensus::checked_ffi_declaration_roots(&checked);
        census
            .validate_ffi_source_roots()
            .expect("whitelisted non-runtime uses remain declaration-only");
        for owner in ["latent_inner", "proof_default"] {
            let root = census
                .ffi_source_roots()
                .iter()
                .find(|root| {
                    root.owner().0 == format!("function:{owner}")
                        && root.kind()
                            == crate::core::ir::lower::ResolvedDeclarationExpressionKind::CallableParameterDefault
                })
                .unwrap_or_else(|| panic!("FFI default root for {owner}"));
            assert_eq!(
                root.execution(),
                MirProofSourceFfiRootExecution::CallableDefaultDeclarationOnly
            );
            assert!(root.activations().is_empty());
        }
    }
}

fn lower_default_proof_roots(
    callable: &crate::core::ResolvedCallable,
    program: &CheckedProgram,
) -> Vec<super::MirProofDefaultRoot> {
    let typed = callable.proof_point_worklist();
    let typed_worklist_valid = typed.validate_against(callable);
    let mut uses_by_root =
        BTreeMap::<(NodeId, ResolvedParameterId), (Vec<super::MirProofDefaultUseId>, bool)>::new();

    // Default uses are owned by the caller, while their proof program is
    // owned by the target declaration. Re-derive the complete use set across
    // checked callables and keep invalid/deferred entries in the census so
    // they cannot disappear from source accounting.
    for caller in program.callables().values() {
        let worklist = caller.proof_point_worklist();
        let worklist_valid = worklist.validate_against(caller);
        for usage in &worklist.default_uses {
            let key = (usage.callee_owner.clone(), usage.parameter.clone());
            let (uses, valid) = uses_by_root
                .entry(key)
                .or_insert_with(|| (Vec::new(), true));
            let executable_disposition = matches!(
                &usage.disposition,
                TypedCallExecutionDisposition::Unconditional
                    | TypedCallExecutionDisposition::Guarded
            );
            *valid &= worklist_valid
                && executable_disposition
                && usage.validate_in_checked_program(program);
            uses.push(super::MirProofDefaultUseId {
                caller_owner: usage.caller_owner.clone(),
                call_node: usage.call_node.clone(),
                callee_owner: usage.callee_owner.clone(),
                parameter: usage.parameter.clone(),
                argument_node: usage.argument_node.clone(),
                parameter_index: usage.parameter_index,
                order: usage.order,
                scope: usage.scope.clone(),
                guards: usage
                    .guards
                    .iter()
                    .map(|guard| (guard.condition.node_id.clone(), guard.when))
                    .collect(),
                disposition: format!("{:?}", usage.disposition),
            });
        }
    }

    let mut roots = typed
        .default_roots
        .iter()
        .filter(|root| {
            !root.points.is_empty() || !root.calls.is_empty() || !root.boundaries.is_empty()
        })
        .map(|root| {
            let key = (root.owner.clone(), root.parameter.clone());
            let (mut uses, uses_valid) = uses_by_root.remove(&key).unwrap_or_default();
            let use_count = uses.len();
            uses.sort();
            uses.dedup();
            let uses_are_unique = uses.len() == use_count;
            let target_valid = root.validate_in_checked_program(program);
            super::contracts::lower_default_proof_root(
                root,
                callable,
                uses,
                typed_worklist_valid && target_valid && uses_valid && uses_are_unique,
            )
        })
        .collect::<Vec<_>>();
    roots.sort_by(|left, right| left.id.cmp(&right.id));
    roots
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
        match_path: point.match_path.clone(),
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
        match_path: point.match_path.clone(),
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
                ..
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
                match_path: call.match_path.clone(),
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

fn checked_callable_qualified_name(program: &CheckedProgram, owner: &NodeId) -> String {
    if let Some(function) = program.functions().get(owner) {
        return function.qualified_name.clone();
    }
    if let Some(transition) = program
        .transitions()
        .values()
        .find(|transition| &transition.node_id == owner)
    {
        return format!(
            "{}::{} from {}",
            transition.id.flow.0, transition.id.event, transition.id.source.name
        );
    }
    owner.0.clone()
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

fn ffi_root_types(
    program: &CheckedProgram,
    root: &ResolvedFfiContractRoot,
    expected_kind: crate::core::ir::lower::ResolvedDeclarationExpressionKind,
) -> BTreeMap<Vec<u8>, ResolvedTypeId> {
    let ResolvedFfiContractRoot::Ready { node_id } = root else {
        return BTreeMap::new();
    };
    let Some(root) = program.declaration_expression_bodies().get(node_id) else {
        return BTreeMap::new();
    };
    if &root.node_id != node_id || root.kind != expected_kind {
        return BTreeMap::new();
    }
    let Ok(body) = &root.body else {
        return BTreeMap::new();
    };
    let Some(expression) = body.root.result.as_deref() else {
        return BTreeMap::new();
    };
    super::contracts::collect_contract_expression_types(expression, body)
        .unwrap_or_else(|_| BTreeMap::from([(Vec::new(), expression.ty.clone())]))
}

fn append_type_path_map(output: &mut String, types: &BTreeMap<Vec<u8>, ResolvedTypeId>) {
    use std::fmt::Write as _;
    let _ = write!(output, "type-paths:{}[", types.len());
    for (path, ty) in types {
        let _ = write!(output, "{}:", path.len());
        for byte in path {
            let _ = write!(output, "{byte:02x}");
        }
        let ty = ty.as_str();
        let _ = write!(output, "{}:{ty}", ty.len());
    }
    output.push(']');
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
                            eager_source_nodes: argument.value.eager_source_nodes(),
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

fn resolved_item_kind_text(kind: ResolvedItemKind) -> &'static str {
    match kind {
        ResolvedItemKind::Function => "function",
        ResolvedItemKind::Type => "type",
        ResolvedItemKind::Constant => "constant",
        ResolvedItemKind::Capability => "capability",
        ResolvedItemKind::Trait => "trait",
        ResolvedItemKind::Impl => "impl",
        ResolvedItemKind::ExternBlock => "extern-block",
        ResolvedItemKind::Actor => "actor",
        ResolvedItemKind::Flow => "flow",
        ResolvedItemKind::Session => "session",
    }
}
