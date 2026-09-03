//! Canonical Mimi middle-level IR (MIR).
//!
//! This module is deliberately backend-free. It is the first slice of the
//! 0.41 architecture migration: the data model and structural verifier exist
//! before any lowering from ResolvedBody or emission to bytecode/LLVM.
//!
//! A MIR consumer may lower an instruction, but it may not re-run name
//! resolution, type inference, or ownership classification.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};

use crate::core::ir::{
    NominalTypeId, ResolvedBinaryOp, ResolvedCallee, ResolvedLiteral, ResolvedTypeId,
    ResolvedUnaryOp,
};
use crate::core::{NodeId, ResolvedPlace};

mod contracts;
mod eligibility;
mod islands;
pub mod lower;
mod option_island;
mod receipt;
pub mod reference;
mod route;
pub mod types;

pub use contracts::{
    MirContract, MirContractBinaryOp, MirContractExpr, MirContractKind, MirContractUnaryOp,
};
pub use eligibility::{is_exact_s8_flow_transition, is_s8_flow_transition_candidate};
pub use islands::{
    classify_flat_copy_record_admission, classify_scalar_collection_admission,
    contains_flat_copy_record_candidate, contains_s8_flow_transition_candidate,
    contains_scalar_collection_candidate, contains_scalar_collection_operation_candidate,
    has_unsupported_list_concat_candidate, has_unsupported_list_reverse_candidate,
    validate_scalar_collection_island, FlatCopyRecordAdmission, ScalarCollectionAdmission,
    SCALAR_COLLECTION_ISLAND,
};
pub use option_island::{
    classify_option_string_variant_admission, contains_option_string_variant_candidate,
    validate_option_string_variant_island, OptionStringVariantAdmission,
    NON_COPY_OPTION_STRING_VARIANT_ISLAND,
};
pub use receipt::{
    CanonicalMirRouteReceipt, MIR_IDENTITY_SCHEMA, MIR_ROUTE_RECEIPT_SCHEMA,
    MIR_ROUTE_VALIDATOR_CONTRACT_ID,
};
pub use route::{
    classify_canonical_mir_route_admission, materialize_canonical_mir_route,
    CanonicalMirRouteAdmission, CanonicalMirRouteFailureStage, CanonicalMirRouteMaterialization,
    CanonicalMirRouteMaterializationError, CanonicalMirRouteProfile, S8FlowAdmission,
};
#[cfg(test)]
pub(crate) use route::{reset_test_route_materialization_count, test_route_materialization_count};

/// Stable owner identity shared by resolved transition bodies, transition
/// contracts, and all backend adapters.
pub fn transition_owner_from_id(transition: &crate::core::TransitionId) -> NodeId {
    NodeId(format!(
        "transition:{}::{}::{}",
        transition.flow.0, transition.event, transition.source.name
    ))
}

macro_rules! mir_id {
    ($name:ident, $label:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, MirIdError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(MirIdError {
                        kind: $label,
                        value,
                    });
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

mir_id!(MirBlockId, "block");
mir_id!(MirEdgeId, "edge");
mir_id!(MirValueId, "value");
mir_id!(MirInstructionId, "instruction");
mir_id!(MirInstanceId, "instance");

impl MirInstanceId {
    /// Stable identity for one checker-selected concrete generic instance.
    /// The type arguments are canonical `ResolvedTypeId`s, never source names
    /// or backend ABI spellings.
    pub fn for_template(
        template: &NodeId,
        arguments: &[ResolvedTypeId],
    ) -> Result<Self, MirIdError> {
        let mut identity = format!("instance:{}<", template.0);
        for (index, argument) in arguments.iter().enumerate() {
            if index != 0 {
                identity.push(',');
            }
            identity.push_str(argument.as_str());
        }
        identity.push('>');
        Self::new(identity)
    }
}

/// Checker-selected concrete generic instantiation recorded in canonical MIR.
/// The executable function referenced by `function` is already specialized;
/// the table keeps the template identity and argument proof attached so no
/// backend has to rediscover monomorphization from a callee name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirInstance {
    pub id: MirInstanceId,
    pub template: NodeId,
    pub arguments: Vec<ResolvedTypeId>,
    pub function: NodeId,
    pub contract: MirGenericInstanceContract,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirIdError {
    pub kind: &'static str,
    pub value: String,
}

impl fmt::Display for MirIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} identity must not be empty", self.kind)
    }
}

impl std::error::Error for MirIdError {}

/// A value available to a MIR instruction or block parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirValue {
    pub id: MirValueId,
    pub ty: ResolvedTypeId,
}

/// Block parameters are the canonical join-value form; consumers must not
/// invent a second phi representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirBlockParameter {
    pub value: MirValueId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirProjection {
    /// Checker-owned stable field identity.  A display name is deliberately
    /// not part of MIR; consumers resolve it through the TypeDesc layout.
    Field(NodeId),
    Tuple(usize),
    Index(MirValueId),
    Dereference,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirAggregateKind {
    Tuple,
    Record {
        nominal: NominalTypeId,
        /// Stable checker field identities in the same order as `fields` in
        /// [`MirInstructionKind::Construct`].
        fields: Vec<NodeId>,
    },
}

/// Closed semantic operations for the first Set production island. The
/// receiver and result ownership rules live in TypeDesc; consumers only map
/// this enum to their physical runtime operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirSetOperation {
    Size,
    IsEmpty,
    Contains,
    Insert,
    Remove,
    /// Kept explicit so the adapter cannot accidentally treat `to_list` as a
    /// generic builtin before its List/result ownership contract is ready.
    ToList,
}

/// Closed operations over the canonical scalar List production island.
/// `Len` borrows the source and returns a Copy scalar; `Reverse` borrows the
/// source while materializing a fresh List through the List Clone glue;
/// `Concat` consumes both List inputs and materializes a fresh result. The
/// operation identity is part of the MIR contract so a consumer cannot
/// silently change a borrow into a move or drop one side of a double-input
/// ownership transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirListOperation {
    Len,
    Reverse,
    Concat,
}

/// Read-only predicates over a checker-owned Option/Result discriminant.
/// The source is borrowed; the TypeDesc receipt fixes the family, variant
/// identity, and discriminant before a backend reads a physical tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirVariantPredicate {
    IsSome,
    IsNone,
    IsOk,
    IsErr,
}

/// Effect boundary carried by a materialized Flow transition contract.  The
/// first production island admits only a local silent self-loop; boundary
/// effects stay explicit so no consumer can accidentally erase epoch,
/// mailbox, FFI, or failure semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirTransitionEffect {
    SilentLocal,
    Boundary,
}

/// Checker-owned ABI/ownership/effect contract for a Flow transition.
/// `parameters` includes the consumed source state as its first entry.  The
/// transition instruction refers to `owner` rather than re-encoding a surface
/// Flow name, so all consumers use the same materialized identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirTransitionContract {
    pub owner: NodeId,
    pub source: ResolvedTypeId,
    pub parameters: Vec<ResolvedTypeId>,
    pub result: ResolvedTypeId,
    pub targets: Vec<ResolvedTypeId>,
    pub failure: Option<ResolvedTypeId>,
    pub effect: MirTransitionEffect,
    pub is_fallback: bool,
    pub is_ffi_pinned: bool,
}

impl MirTransitionContract {
    pub fn canonical_text(&self) -> String {
        format!(
            "mir.transition {} {:?} [{}] -> {} targets [{}] failure {} fallback={} pinned={}\n",
            self.owner.0,
            self.effect,
            self.parameters
                .iter()
                .map(|ty| ty.as_str())
                .collect::<Vec<_>>()
                .join(","),
            self.result.as_str(),
            self.targets
                .iter()
                .map(|ty| ty.as_str())
                .collect::<Vec<_>>()
                .join(","),
            self.failure
                .as_ref()
                .map(ResolvedTypeId::as_str)
                .unwrap_or("-"),
            self.is_fallback,
            self.is_ffi_pinned,
        )
    }
}

/// Closed proof carried by a materialized generic MIR instance. The instance
/// table is part of the canonical program, so consumers must not guess which
/// generic template family a specialized body belongs to from its symbol name
/// or physical ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirGenericInstanceContract {
    ScalarIdentity,
    /// `identity<T>(T) -> T` specialized to the canonical move-owned String
    /// ABI.  This is separate from the Copy/flat-variant identity island so
    /// consumers cannot accidentally treat an owned handle as a no-op value.
    OwnedStringIdentity,
    ScalarSetFacade {
        operation: MirSetOperation,
    },
}

/// Operations with explicit value and ownership boundaries. The MIR validator
/// checks their TypeDesc/glue contract before any backend; effect-bearing
/// operations remain fail-closed until their own effect summary is materialized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirInstructionKind {
    Const {
        result: MirValueId,
        literal: ResolvedLiteral,
    },
    Load {
        result: MirValueId,
        place: ResolvedPlace,
    },
    Copy {
        result: MirValueId,
        source: MirValueId,
    },
    Move {
        result: MirValueId,
        source: MirValueId,
    },
    Clone {
        result: MirValueId,
        source: MirValueId,
    },
    Drop {
        value: MirValueId,
    },
    Borrow {
        result: MirValueId,
        source: MirValueId,
        mutable: bool,
    },
    EndBorrow {
        borrow: MirValueId,
    },
    Project {
        result: MirValueId,
        base: MirValueId,
        projection: MirProjection,
        /// TypeDesc receipt required for canonical List index projections.
        /// Other projection kinds must leave this absent until their own
        /// canonical MIR receipt is materialized.
        list_index_contract: Option<types::MirListIndexProjectionContract>,
    },
    /// Consume a non-Copy record product and move one non-Copy field out.
    /// The TypeDesc contract requires every sibling field to be Copy, so the
    /// consumed record has no residual ownership obligation. General
    /// field-level partial move remains a separate MIR shape.
    MoveProject {
        result: MirValueId,
        base: MirValueId,
        projection: MirProjection,
    },
    Construct {
        result: MirValueId,
        kind: MirAggregateKind,
        fields: Vec<MirValueId>,
    },
    /// Construct a move-owned List from element values. The TypeDesc List
    /// contract supplies the element layout and glue; this node carries no
    /// backend-specific capacity or handle representation.
    ConstructList {
        result: MirValueId,
        elements: Vec<MirValueId>,
    },
    /// Execute a canonical operation over a Copy-scalar List. `Len` returns a
    /// Copy i32 without transferring the source; `Reverse` returns a fresh
    /// move-owned List and leaves the source available for its own Drop;
    /// `Concat` consumes `list` and `argument` and transfers both obligations
    /// into a fresh move-owned result. TypeDesc fixes both List ABI/glue and
    /// result ABI before any consumer is invoked.
    ListOp {
        result: MirValueId,
        operation: MirListOperation,
        list: MirValueId,
        /// The second List input for `Concat`; absent for borrow-only `Len`
        /// and `Reverse`. The receipt repeats its TypeDesc identity.
        argument: Option<MirValueId>,
        /// TypeDesc receipt required for canonical List operations.
        /// Legacy/non-canonical constructors must not be presented as MIR.
        list_operation_contract: Option<types::MirListOperationContract>,
    },
    /// Read the active tag of a flat Copy Option/Result without consuming it.
    /// The receipt is mandatory for canonical consumers.
    VariantPredicate {
        result: MirValueId,
        predicate: MirVariantPredicate,
        variant: MirValueId,
        contract: Option<types::MirVariantPredicateContract>,
    },
    /// Construct a move-owned Set from concrete Copy-scalar elements. The
    /// Set<T> layout, equality representation, and handle glue are supplied
    /// by the TypeDesc catalog.
    ConstructSet {
        result: MirValueId,
        elements: Vec<MirValueId>,
    },
    /// Execute one closed Set operation. `Insert` and `Remove` consume the
    /// receiver and return a new move-owned Set; read operations preserve it.
    SetOp {
        result: MirValueId,
        operation: MirSetOperation,
        set: MirValueId,
        argument: Option<MirValueId>,
    },
    /// Construct a canonical Option/Result variant. Payload identities are
    /// checker-owned; discriminant and physical payload encoding come from
    /// the TypeDesc variant layout.
    ConstructVariant {
        result: MirValueId,
        nominal: NominalTypeId,
        variant: NodeId,
        fields: Vec<(NodeId, MirValueId)>,
    },
    /// Construct a non-Copy Option/Result variant by consuming its payloads.
    /// The TypeDesc variant glue plan proves the payload ownership boundary;
    /// a backend must not lower this as a shallow Copy construction.
    ConstructVariantMove {
        result: MirValueId,
        nominal: NominalTypeId,
        variant: NodeId,
        fields: Vec<(NodeId, MirValueId)>,
    },
    /// Consume a record base and produce the same record with the explicit
    /// field values overlaid.  The field identities in `kind` remain
    /// checker-owned; backend field names are recovered from TypeDesc only.
    UpdateRecord {
        result: MirValueId,
        base: MirValueId,
        kind: MirAggregateKind,
        fields: Vec<MirValueId>,
    },
    Binary {
        result: MirValueId,
        op: ResolvedBinaryOp,
        left: MirValueId,
        right: MirValueId,
    },
    Unary {
        result: MirValueId,
        op: ResolvedUnaryOp,
        operand: MirValueId,
    },
    Call {
        result: Option<MirValueId>,
        callee: ResolvedCallee,
        /// Checker-finalized generic arguments in binder order.  An empty
        /// list is the canonical marker for a non-generic call.
        type_arguments: Vec<ResolvedTypeId>,
        arguments: Vec<MirValueId>,
        /// TypeDesc/ABI receipt for a direct call returning a flat Copy
        /// Option/Result. Other call result shapes remain on their existing
        /// explicit compatibility boundary until their own receipt exists.
        variant_call_contract: Option<types::MirVariantCallAbiContract>,
    },
    /// Invoke a checker-resolved Flow transition through its materialized
    /// contract.  This is intentionally distinct from ordinary calls: the
    /// source state is consumed and effect/failure/target facts are carried by
    /// `MirProgram::transitions`, never reconstructed by a backend.
    FlowTransition {
        result: MirValueId,
        transition: NodeId,
        arguments: Vec<MirValueId>,
    },
    /// A builtin whose ABI, trap behavior, and ownership boundary have been
    /// fully materialized in the canonical MIR contract. Surface builtin
    /// names must not cross this boundary as backend-local string dispatch.
    BuiltinCall {
        result: MirValueId,
        kind: types::MirBuiltinKind,
        arguments: Vec<MirValueId>,
    },
    /// A checked conversion. Source/target facts live in the value catalog
    /// and the eventual lowering contract.
    Convert {
        result: MirValueId,
        source: MirValueId,
    },
    Nop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirInstruction {
    pub id: MirInstructionId,
    pub kind: MirInstructionKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirTerminator {
    Goto {
        edge: MirEdgeId,
        target: MirBlockId,
        arguments: Vec<MirValueId>,
    },
    Branch {
        condition: MirValueId,
        then_edge: MirEdgeId,
        then_target: MirBlockId,
        then_arguments: Vec<MirValueId>,
        else_edge: MirEdgeId,
        else_target: MirBlockId,
        else_arguments: Vec<MirValueId>,
    },
    Switch {
        scrutinee: MirValueId,
        arms: Vec<MirSwitchArm>,
    },
    /// Consume a non-Copy Option/Result scrutinee and route its active
    /// payload into the selected arm.  Unbound payload fields are released
    /// by the variant glue contract; bound fields become target block
    /// parameters.  This is deliberately distinct from Copy-only `Switch`.
    SwitchMove {
        scrutinee: MirValueId,
        arms: Vec<MirSwitchArm>,
    },
    Return {
        value: Option<MirValueId>,
    },
    Trap {
        code: String,
    },
    Fault {
        value: Option<MirValueId>,
    },
    Unreachable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirSwitchCase {
    Literal(ResolvedLiteral),
    Variant(NodeId),
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirSwitchArm {
    pub edge: MirEdgeId,
    pub target: MirBlockId,
    pub arguments: Vec<MirValueId>,
    /// Payloads projected from the selected variant into target block
    /// parameters. A default arm cannot contain bindings.
    pub bindings: Vec<MirSwitchBinding>,
    pub case: MirSwitchCase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirSwitchBinding {
    pub parameter: MirValueId,
    /// TypeDesc-owned proof of the payload projection. Canonical consumers
    /// must use this receipt; they may not reconstruct the variant field
    /// index, arity, or field type from a runtime tag or payload vector.
    pub projection: types::MirVariantProjectionContract,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirBlock {
    pub id: MirBlockId,
    pub parameters: Vec<MirBlockParameter>,
    pub instructions: Vec<MirInstruction>,
    pub terminator: MirTerminator,
}

/// A concrete callable after frontend resolution but before a backend is
/// selected. values is a catalog, not an implicit vector index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirFunction {
    pub owner: NodeId,
    pub parameters: Vec<MirValueId>,
    pub result: ResolvedTypeId,
    pub entry: MirBlockId,
    pub values: BTreeMap<MirValueId, MirValue>,
    pub blocks: BTreeMap<MirBlockId, MirBlock>,
    /// Canonical scalar contract predicates.  Conditions use MIR value
    /// identities rather than source expressions or display names.
    pub contracts: Vec<MirContract>,
    /// Checker-owned resource facts projected into a backend-neutral event
    /// stream.  Consumers must use this stream together with TypeDesc rather
    /// than infer ownership from a physical register or pointer shape.
    pub ownership: MirOwnershipSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirOwnershipEventKind {
    Read,
    Write,
    Introduce,
    Move,
    Drop,
    Return,
    TransferSession,
    TransferChild,
    BorrowShared,
    BorrowMut,
    BorrowEnd,
}

impl MirOwnershipEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Introduce => "introduce",
            Self::Move => "move",
            Self::Drop => "drop",
            Self::Return => "return",
            Self::TransferSession => "transfer_session",
            Self::TransferChild => "transfer_child",
            Self::BorrowShared => "borrow_shared",
            Self::BorrowMut => "borrow_mut",
            Self::BorrowEnd => "borrow_end",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirOwnershipEvent {
    pub kind: MirOwnershipEventKind,
    pub resource: String,
    /// Stable value identity when the resource is backed by a MIR local.
    /// Synthetic discarded/session resources intentionally leave this empty.
    pub value: Option<MirValueId>,
    pub source: Option<String>,
    pub target: Option<String>,
    pub point: NodeId,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MirOwnershipSummary {
    pub events: Vec<MirOwnershipEvent>,
}

impl MirOwnershipSummary {
    pub fn validate(&self) -> Result<(), Vec<MirValidationError>> {
        let mut errors = Vec::new();
        for (index, event) in self.events.iter().enumerate() {
            if event.resource.trim().is_empty() {
                errors.push(MirValidationError {
                    subject: format!("ownership[{index}]"),
                    message: "resource identity is empty".into(),
                });
            }
            if event.point.0.trim().is_empty() {
                errors.push(MirValidationError {
                    subject: format!("ownership[{index}]"),
                    message: "event point identity is empty".into(),
                });
            }
            if matches!(
                event.kind,
                MirOwnershipEventKind::Move
                    | MirOwnershipEventKind::Drop
                    | MirOwnershipEventKind::Return
                    | MirOwnershipEventKind::TransferSession
                    | MirOwnershipEventKind::TransferChild
            ) && event.source.is_none()
            {
                errors.push(MirValidationError {
                    subject: format!("ownership[{index}]"),
                    message: format!("{} event has no source place", event.kind.as_str()),
                });
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    pub fn canonical_text(&self) -> String {
        let mut output = String::new();
        for (index, event) in self.events.iter().enumerate() {
            let source = event.source.as_deref().unwrap_or("_");
            let target = event.target.as_deref().unwrap_or("_");
            let value = event
                .value
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "_".into());
            let _ = writeln!(
                output,
                "    ownership[{index}] {} resource={} value={} source={} target={} point={}",
                event.kind.as_str(),
                event.resource,
                value,
                source,
                target,
                event.point.0
            );
        }
        output
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirValidationError {
    pub subject: String,
    pub message: String,
}

impl fmt::Display for MirValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "MIR {}: {}", self.subject, self.message)
    }
}

impl std::error::Error for MirValidationError {}

impl MirFunction {
    /// Validate identities, graph shape, and SSA-like value dominance without
    /// depending on a backend. Kind/effect/ownership checks belong to later
    /// MIR passes, but a value may never be read from a non-dominating path.
    pub fn validate(&self) -> Result<(), Vec<MirValidationError>> {
        let mut validator = MirValidator::new(self);
        validator.check_function_header();
        validator.check_blocks();
        validator.check_ownership();
        validator.finish()
    }

    /// Deterministic, human-readable form for golden tests and differential
    /// debugging. BTreeMap ordering makes catalog insertion order irrelevant.
    pub fn canonical_text(&self) -> String {
        let mut output = String::new();
        let _ = writeln!(
            output,
            "mir.function {} -> {}",
            self.owner.0,
            self.result.as_str()
        );
        let _ = writeln!(
            output,
            "  params [{}] entry {}",
            self.parameters
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            self.entry
        );
        for (id, value) in &self.values {
            let _ = writeln!(output, "  value {}: {}", id, value.ty.as_str());
        }
        for block in self.blocks.values() {
            let _ = writeln!(
                output,
                "  block {}({})",
                block.id,
                format_params(&block.parameters)
            );
            for instruction in &block.instructions {
                let _ = writeln!(
                    output,
                    "    {} {}",
                    instruction.id,
                    format_instruction(&instruction.kind)
                );
            }
            let _ = writeln!(output, "    -> {}", format_terminator(&block.terminator));
        }
        for contract in &self.contracts {
            let _ = writeln!(output, "{}", contract.canonical_text());
        }
        output.push_str(&self.ownership.canonical_text());
        output
    }
}

/// Validate the closed generic identity body contract shared by lowering,
/// canonical-program admission, and the MIR verifier. The body may be the
/// original one-block `Clone; Return` form or an acyclic total `Goto`/`Branch`
/// CFG in which every path clones the one parameter exactly once and returns
/// that clone. No frontend or backend fact is consulted here.
pub(crate) fn validate_generic_identity_shape(
    function: &MirFunction,
    expected_type: &ResolvedTypeId,
) -> Result<(), String> {
    let [parameter] = function.parameters.as_slice() else {
        return Err("generic MIR identity body must have exactly one parameter".into());
    };
    if function.result != *expected_type
        || !function
            .values
            .get(parameter)
            .is_some_and(|value| value.ty == *expected_type)
    {
        return Err(
            "generic MIR identity body must preserve one canonical parameter and result".into(),
        );
    }
    if function.blocks.len() == 1 {
        return validate_single_block_generic_identity(function, parameter, expected_type);
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Origin {
        Parameter,
        Clone,
        Other,
    }

    #[derive(Clone)]
    struct PathState {
        origins: BTreeMap<MirValueId, Origin>,
        clone_count: usize,
    }

    fn edge_state(
        function: &MirFunction,
        state: &PathState,
        target: &MirBlockId,
        arguments: &[MirValueId],
    ) -> Result<PathState, String> {
        let block = function
            .blocks
            .get(target)
            .ok_or_else(|| format!("generic MIR identity target block '{}' is absent", target))?;
        if block.parameters.len() != arguments.len() {
            return Err(
                "generic MIR identity branch edge arguments disagree with block parameters".into(),
            );
        }
        let mut next = state.clone();
        for (parameter, argument) in block.parameters.iter().zip(arguments) {
            let origin = state.origins.get(argument).copied().ok_or_else(|| {
                format!(
                    "generic MIR identity branch edge argument '{}' is not defined",
                    argument
                )
            })?;
            next.origins.insert(parameter.value.clone(), origin);
        }
        Ok(next)
    }

    fn visit(
        function: &MirFunction,
        block_id: &MirBlockId,
        mut state: PathState,
        parameter: &MirValueId,
        expected_type: &ResolvedTypeId,
        active: &mut BTreeSet<MirBlockId>,
        reachable: &mut BTreeSet<MirBlockId>,
    ) -> Result<(), String> {
        if !active.insert(block_id.clone()) {
            return Err("generic MIR identity body does not admit cyclic CFG".into());
        }
        reachable.insert(block_id.clone());
        let block = function
            .blocks
            .get(block_id)
            .ok_or_else(|| format!("generic MIR identity block '{}' is absent", block_id))?;
        for instruction in &block.instructions {
            match &instruction.kind {
                MirInstructionKind::Const { result, literal } => {
                    if !matches!(literal, ResolvedLiteral::Bool(_)) {
                        return Err(
                            "generic MIR identity branch body may only use boolean constants"
                                .into(),
                        );
                    }
                    state.origins.insert(result.clone(), Origin::Other);
                }
                MirInstructionKind::Clone { result, source } => {
                    if source != parameter
                        || !function
                            .values
                            .get(result)
                            .is_some_and(|value| value.ty == *expected_type)
                        || state.clone_count != 0
                    {
                        return Err(
                            "generic MIR identity branch must Clone its parameter exactly once"
                                .into(),
                        );
                    }
                    state.origins.insert(result.clone(), Origin::Clone);
                    state.clone_count += 1;
                }
                MirInstructionKind::Copy { result, source }
                | MirInstructionKind::Move { result, source } => {
                    let origin = state.origins.get(source).copied().ok_or_else(|| {
                        format!(
                            "generic MIR identity branch value '{}' is not defined",
                            source
                        )
                    })?;
                    state.origins.insert(result.clone(), origin);
                }
                _ => {
                    return Err(
                        "generic MIR identity branch body may only contain Const/Clone/Copy/Move"
                            .into(),
                    )
                }
            }
        }
        match &block.terminator {
            MirTerminator::Goto {
                target, arguments, ..
            } => {
                let next = edge_state(function, &state, target, arguments)?;
                visit(
                    function,
                    target,
                    next,
                    parameter,
                    expected_type,
                    active,
                    reachable,
                )?;
            }
            MirTerminator::Branch {
                condition,
                then_target,
                then_arguments,
                else_target,
                else_arguments,
                ..
            } => {
                if !state.origins.contains_key(condition) {
                    return Err(format!(
                        "generic MIR identity branch condition '{}' is not defined",
                        condition
                    ));
                }
                let then_state = edge_state(function, &state, then_target, then_arguments)?;
                visit(
                    function,
                    then_target,
                    then_state,
                    parameter,
                    expected_type,
                    &mut active.clone(),
                    reachable,
                )?;
                let else_state = edge_state(function, &state, else_target, else_arguments)?;
                visit(
                    function,
                    else_target,
                    else_state,
                    parameter,
                    expected_type,
                    &mut active.clone(),
                    reachable,
                )?;
            }
            MirTerminator::Return { value: Some(value) } => {
                if state.clone_count != 1
                    || state.origins.get(value).copied() != Some(Origin::Clone)
                {
                    return Err(
                        "generic MIR identity branch must return its exactly-once Clone result"
                            .into(),
                    );
                }
            }
            MirTerminator::Return { value: None } => {
                return Err("generic MIR identity branch must return a value".into())
            }
            MirTerminator::Switch { .. } | MirTerminator::SwitchMove { .. } => {
                return Err("generic MIR identity branch body only admits Goto/Branch CFG".into())
            }
            MirTerminator::Trap { .. }
            | MirTerminator::Fault { .. }
            | MirTerminator::Unreachable => {
                return Err(
                    "generic MIR identity branch body requires total non-trapping returns".into(),
                )
            }
        }
        active.remove(block_id);
        Ok(())
    }

    let mut origins = BTreeMap::new();
    origins.insert(parameter.clone(), Origin::Parameter);
    let mut reachable = BTreeSet::new();
    visit(
        function,
        &function.entry,
        PathState {
            origins,
            clone_count: 0,
        },
        parameter,
        expected_type,
        &mut BTreeSet::new(),
        &mut reachable,
    )?;
    if reachable.len() != function.blocks.len() {
        return Err("generic MIR identity body contains unreachable blocks".into());
    }
    Ok(())
}

fn validate_single_block_generic_identity(
    function: &MirFunction,
    parameter: &MirValueId,
    expected_type: &ResolvedTypeId,
) -> Result<(), String> {
    let block = function
        .blocks
        .get(&function.entry)
        .ok_or_else(|| "generic MIR identity entry block is absent".to_string())?;
    let [instruction] = block.instructions.as_slice() else {
        return Err("generic MIR identity body must contain exactly one Clone instruction".into());
    };
    let MirInstructionKind::Clone { result, source } = &instruction.kind else {
        return Err("generic MIR identity body must use canonical Clone from its parameter".into());
    };
    if source != parameter
        || !function
            .values
            .get(result)
            .is_some_and(|value| value.ty == *expected_type)
    {
        return Err("generic MIR identity Clone must copy the canonical parameter".into());
    }
    if !matches!(
        &block.terminator,
        MirTerminator::Return { value: Some(value) } if value == result
    ) {
        return Err("generic MIR identity body must return the Clone result".into());
    }
    Ok(())
}

/// Validate the ownership-complete specialization of generic identity for
/// the move-owned String ABI.  A Copy identity may leave its parameter
/// untouched because the value has no destruction obligation; a String
/// identity must explicitly clone the returned value and drop the consumed
/// parameter before returning.  Keeping this as a distinct MIR shape prevents
/// a backend from silently treating an owned handle as a shallow Copy.
pub(crate) fn validate_owned_string_identity_shape(
    function: &MirFunction,
    expected_type: &ResolvedTypeId,
) -> Result<(), String> {
    let [parameter] = function.parameters.as_slice() else {
        return Err("owned String generic identity body must have exactly one parameter".into());
    };
    if function.result != *expected_type
        || !function
            .values
            .get(parameter)
            .is_some_and(|value| value.ty == *expected_type)
    {
        return Err(
            "owned String generic identity body must preserve one canonical parameter and result"
                .into(),
        );
    }
    if function.blocks.len() != 1 {
        return Err(
            "owned String generic identity body currently requires one canonical MIR block".into(),
        );
    }
    let block = function
        .blocks
        .get(&function.entry)
        .ok_or_else(|| "owned String generic identity entry block is absent".to_string())?;
    let [clone, drop] = block.instructions.as_slice() else {
        return Err(
            "owned String generic identity body must contain Clone followed by Drop".into(),
        );
    };
    let MirInstructionKind::Clone {
        result: clone_result,
        source: clone_source,
    } = &clone.kind
    else {
        return Err("owned String generic identity body must clone its canonical parameter".into());
    };
    if clone_source != parameter
        || !function
            .values
            .get(clone_result)
            .is_some_and(|value| value.ty == *expected_type)
    {
        return Err("owned String generic identity Clone must copy the canonical parameter".into());
    }
    if !matches!(&drop.kind, MirInstructionKind::Drop { value } if value == parameter) {
        return Err("owned String generic identity body must drop its consumed parameter".into());
    }
    if !matches!(
        &block.terminator,
        MirTerminator::Return { value: Some(value) } if value == clone_result
    ) {
        return Err("owned String generic identity body must return the cloned value".into());
    }
    Ok(())
}

fn format_params(parameters: &[MirBlockParameter]) -> String {
    parameters
        .iter()
        .map(|parameter| parameter.value.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_instruction(kind: &MirInstructionKind) -> String {
    match kind {
        MirInstructionKind::Const { result, literal } => format!("const {result} = {literal:?}"),
        MirInstructionKind::Load { result, .. } => format!("load {result}"),
        MirInstructionKind::Copy { result, source } => format!("copy {result} <- {source}"),
        MirInstructionKind::Move { result, source } => format!("move {result} <- {source}"),
        MirInstructionKind::Clone { result, source } => format!("clone {result} <- {source}"),
        MirInstructionKind::Drop { value } => format!("drop {value}"),
        MirInstructionKind::Borrow {
            result,
            source,
            mutable,
        } => format!(
            "borrow{} {result} <- {source}",
            if *mutable { "_mut" } else { "" }
        ),
        MirInstructionKind::EndBorrow { borrow } => format!("end_borrow {borrow}"),
        MirInstructionKind::Project {
            result,
            base,
            projection,
            list_index_contract,
        } => format!(
            "project {result} <- {base}.{projection:?}{}",
            list_index_contract
                .as_ref()
                .map(|contract| format!(" [list_index={contract:?}]"))
                .unwrap_or_default()
        ),
        MirInstructionKind::MoveProject {
            result,
            base,
            projection,
        } => format!("move_project {result} <- {base}.{projection:?}"),
        MirInstructionKind::Construct {
            result,
            kind,
            fields,
        } => format!("construct {result} = {kind:?}({})", format_values(fields)),
        MirInstructionKind::ConstructList { result, elements } => {
            format!("construct_list {result} = [{}]", format_values(elements))
        }
        MirInstructionKind::ListOp {
            result,
            operation,
            list,
            argument,
            list_operation_contract,
        } => format!(
            "list_op {result} = {operation:?} {list}{}{}",
            argument
                .as_ref()
                .map(|value| format!(", {value}"))
                .unwrap_or_default(),
            list_operation_contract
                .as_ref()
                .map(|contract| format!(" [list_contract={contract:?}]"))
                .unwrap_or_default()
        ),
        MirInstructionKind::VariantPredicate {
            result,
            predicate,
            variant,
            contract,
        } => format!(
            "variant_predicate {result} = {predicate:?} {variant}{}",
            contract
                .as_ref()
                .map(|contract| format!(" [variant_contract={contract:?}]"))
                .unwrap_or_default()
        ),
        MirInstructionKind::ConstructSet { result, elements } => {
            format!("construct_set {result} = {{{}}}", format_values(elements))
        }
        MirInstructionKind::SetOp {
            result,
            operation,
            set,
            argument,
        } => format!(
            "set_op {result} = {operation:?} {set}{}",
            argument
                .as_ref()
                .map(|value| format!(", {value}"))
                .unwrap_or_default()
        ),
        MirInstructionKind::ConstructVariant {
            result,
            nominal,
            variant,
            fields,
        } => format!(
            "construct_variant {result} = {nominal:?}::{variant:?}({})",
            fields
                .iter()
                .map(|(field, value)| format!("{field:?}:{value}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        MirInstructionKind::ConstructVariantMove {
            result,
            nominal,
            variant,
            fields,
        } => format!(
            "construct_variant_move {result} = {nominal:?}::{variant:?}({})",
            fields
                .iter()
                .map(|(field, value)| format!("{field:?}:{value}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        MirInstructionKind::UpdateRecord {
            result,
            base,
            kind,
            fields,
        } => format!(
            "update_record {result} = {base} {kind:?}({})",
            format_values(fields)
        ),
        MirInstructionKind::Binary {
            result,
            op,
            left,
            right,
        } => format!("binary {result} = {op:?} {left}, {right}"),
        MirInstructionKind::Unary {
            result,
            op,
            operand,
        } => format!("unary {result} = {op:?} {operand}"),
        MirInstructionKind::Call {
            result,
            callee,
            type_arguments,
            arguments,
            variant_call_contract,
        } => format!(
            "call {} {:?}{}({}){}",
            result
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "_".into()),
            callee,
            if type_arguments.is_empty() {
                String::new()
            } else {
                format!(
                    "<{}>",
                    type_arguments
                        .iter()
                        .map(|argument| argument.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                )
            },
            arguments
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            variant_call_contract
                .as_ref()
                .map(|contract| format!(" [variant_call_contract={contract:?}]"))
                .unwrap_or_default()
        ),
        MirInstructionKind::FlowTransition {
            result,
            transition,
            arguments,
        } => format!(
            "flow_transition {result} {}({})",
            transition.0,
            arguments
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        MirInstructionKind::BuiltinCall {
            result,
            kind,
            arguments,
        } => format!(
            "builtin_call {result} {kind:?}({})",
            arguments
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        MirInstructionKind::Convert { result, source } => {
            format!("convert {result} <- {source}")
        }
        MirInstructionKind::Nop => "nop".into(),
    }
}

fn format_terminator(terminator: &MirTerminator) -> String {
    match terminator {
        MirTerminator::Goto {
            edge,
            target,
            arguments,
        } => format!("goto {edge} {target}({})", format_values(arguments)),
        MirTerminator::Branch {
            condition,
            then_edge,
            then_target,
            then_arguments,
            else_edge,
            else_target,
            else_arguments,
        } => format!(
            "branch {condition} ? {then_edge}:{then_target}({}) : {else_edge}:{else_target}({})",
            format_values(then_arguments),
            format_values(else_arguments)
        ),
        MirTerminator::Switch { scrutinee, arms } => {
            format_switch_terminator("switch", scrutinee, arms)
        }
        MirTerminator::SwitchMove { scrutinee, arms } => {
            format_switch_terminator("switch_move", scrutinee, arms)
        }
        MirTerminator::Return { value } => format!(
            "return {}",
            value
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "()".into())
        ),
        MirTerminator::Trap { code } => format!("trap {code}"),
        MirTerminator::Fault { value } => format!(
            "fault {}",
            value
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "()".into())
        ),
        MirTerminator::Unreachable => "unreachable".into(),
    }
}

fn format_switch_terminator(name: &str, scrutinee: &MirValueId, arms: &[MirSwitchArm]) -> String {
    format!(
        "{name} {scrutinee} [{}]",
        arms.iter()
            .map(|arm| {
                format!(
                    "{:?}:{:?}:{}({}; bind={:?})",
                    arm.case,
                    arm.edge,
                    arm.target,
                    format_values(&arm.arguments),
                    arm.bindings
                        .iter()
                        .map(|binding| {
                            format!(
                                "{}<-{:?}[index={},arity={},ty={}]",
                                binding.parameter,
                                binding.projection.field,
                                binding.projection.field_index,
                                binding.projection.arity,
                                binding.projection.field_ty.as_str()
                            )
                        })
                        .collect::<Vec<_>>()
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn format_values(values: &[MirValueId]) -> String {
    values
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

struct MirValidator<'a> {
    function: &'a MirFunction,
    errors: Vec<MirValidationError>,
    definitions: BTreeMap<MirValueId, String>,
    definition_sites: BTreeMap<MirValueId, MirDefinitionSite>,
    instruction_ids: BTreeSet<MirInstructionId>,
    edge_ids: BTreeSet<MirEdgeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MirDefinitionSite {
    FunctionParameter,
    BlockParameter(MirBlockId),
    Instruction { block: MirBlockId, index: usize },
}

impl<'a> MirValidator<'a> {
    fn new(function: &'a MirFunction) -> Self {
        Self {
            function,
            errors: Vec::new(),
            definitions: BTreeMap::new(),
            definition_sites: BTreeMap::new(),
            instruction_ids: BTreeSet::new(),
            edge_ids: BTreeSet::new(),
        }
    }

    fn error(&mut self, subject: impl Into<String>, message: impl Into<String>) {
        self.errors.push(MirValidationError {
            subject: subject.into(),
            message: message.into(),
        });
    }

    fn check_function_header(&mut self) {
        if self.function.owner.0.trim().is_empty() {
            self.error("function", "owner identity is empty");
        }
        if self.function.result.as_str().trim().is_empty() {
            self.error("function", "result type identity is empty");
        }
        if !self.function.blocks.contains_key(&self.function.entry) {
            self.error(self.function.entry.to_string(), "entry block is missing");
        }
        let mut parameters = BTreeSet::new();
        let function_parameters = self.function.parameters.clone();
        for parameter in &function_parameters {
            if !parameters.insert(parameter) {
                self.error(parameter.to_string(), "function parameter is duplicated");
            }
            if !self.function.values.contains_key(parameter) {
                self.error(
                    parameter.to_string(),
                    "function parameter is absent from value catalog",
                );
            }
            self.define_at(
                parameter,
                "function parameter".into(),
                MirDefinitionSite::FunctionParameter,
            );
        }
    }

    fn check_blocks(&mut self) {
        for (id, block) in &self.function.blocks {
            if id != &block.id {
                self.error(
                    id.to_string(),
                    "block map key disagrees with block identity",
                );
            }
            let mut parameters = BTreeSet::new();
            for parameter in &block.parameters {
                if !parameters.insert(&parameter.value) {
                    self.error(parameter.value.to_string(), "block parameter is duplicated");
                }
                self.define_at(
                    &parameter.value,
                    format!("block {} parameter", id),
                    MirDefinitionSite::BlockParameter(id.clone()),
                );
            }
            for (index, instruction) in block.instructions.iter().enumerate() {
                if !self.instruction_ids.insert(instruction.id.clone()) {
                    self.error(
                        instruction.id.to_string(),
                        "instruction identity is duplicated",
                    );
                }
                self.check_instruction(instruction, id, index);
            }
            self.check_terminator(&block.terminator);
        }
        for (id, value) in &self.function.values {
            if id != &value.id {
                self.error(
                    id.to_string(),
                    "value catalog key disagrees with value identity",
                );
            }
            if value.ty.as_str().trim().is_empty() {
                self.error(id.to_string(), "value type identity is empty");
            }
            if !self.definitions.contains_key(id) {
                self.error(id.to_string(), "value is declared but never defined");
            }
        }
        self.check_dominance();
    }

    fn check_ownership(&mut self) {
        if let Err(errors) = self.function.ownership.validate() {
            self.errors.extend(errors);
        }
        for (index, event) in self.function.ownership.events.iter().enumerate() {
            if let Some(value) = &event.value {
                if !self.function.values.contains_key(value) {
                    self.error(
                        format!("ownership[{index}]"),
                        format!(
                            "event value '{}' is absent from the function value catalog",
                            value
                        ),
                    );
                }
            }
        }
    }

    fn define_at(&mut self, value: &MirValueId, subject: String, site: MirDefinitionSite) {
        if !self.function.values.contains_key(value) {
            self.error(value.to_string(), "definition is absent from value catalog");
        }
        if let Some(previous) = self.definitions.get(value).cloned() {
            self.error(
                value.to_string(),
                format!("value is defined more than once (also {previous})"),
            );
        } else {
            self.definitions.insert(value.clone(), subject);
            self.definition_sites.insert(value.clone(), site);
        }
    }

    fn use_value(&mut self, value: &MirValueId) {
        if !self.function.values.contains_key(value) {
            self.error(value.to_string(), "use is absent from value catalog");
        }
    }

    fn result_at(
        &mut self,
        value: &MirValueId,
        instruction: &MirInstructionId,
        block: &MirBlockId,
        index: usize,
    ) {
        self.define_at(
            value,
            format!("instruction {instruction}"),
            MirDefinitionSite::Instruction {
                block: block.clone(),
                index,
            },
        );
    }

    fn check_instruction(
        &mut self,
        instruction: &MirInstruction,
        block: &MirBlockId,
        index: usize,
    ) {
        use MirInstructionKind::*;
        match &instruction.kind {
            Const { result, .. } | Load { result, .. } => {
                self.result_at(result, &instruction.id, block, index)
            }
            Copy { result, source }
            | Move { result, source }
            | Clone { result, source }
            | Convert { result, source } => {
                self.use_value(source);
                self.result_at(result, &instruction.id, block, index);
            }
            Drop { value } | EndBorrow { borrow: value } => self.use_value(value),
            Borrow { result, source, .. } => {
                self.use_value(source);
                self.result_at(result, &instruction.id, block, index);
            }
            Project {
                result,
                base,
                projection,
                list_index_contract,
            } => {
                self.use_value(base);
                if let MirProjection::Index(index) = projection {
                    self.use_value(index);
                }
                if matches!(projection, MirProjection::Index(_)) && list_index_contract.is_none() {
                    self.error(
                        result.to_string(),
                        "List index projection has no canonical receipt",
                    );
                }
                if !matches!(projection, MirProjection::Index(_)) && list_index_contract.is_some() {
                    self.error(
                        result.to_string(),
                        "List index receipt is attached to a non-index projection",
                    );
                }
                if let MirProjection::Field(field) = projection {
                    if field.0.trim().is_empty() {
                        self.error(
                            result.to_string(),
                            "record projection field identity is empty",
                        );
                    }
                }
                self.result_at(result, &instruction.id, block, index);
            }
            MoveProject {
                result,
                base,
                projection,
            } => {
                self.use_value(base);
                if let MirProjection::Index(index) = projection {
                    self.use_value(index);
                }
                if let MirProjection::Field(field) = projection {
                    if field.0.trim().is_empty() {
                        self.error(
                            result.to_string(),
                            "record move projection field identity is empty",
                        );
                    }
                }
                self.result_at(result, &instruction.id, block, index);
            }
            Construct { result, fields, .. } => {
                self.values(fields);
                self.result_at(result, &instruction.id, block, index);
            }
            ConstructList { result, elements } => {
                self.values(elements);
                self.result_at(result, &instruction.id, block, index);
            }
            ListOp {
                result,
                operation,
                list,
                argument,
                list_operation_contract,
            } => {
                self.use_value(list);
                if let Some(argument) = argument {
                    self.use_value(argument);
                }
                if list_operation_contract.is_none() {
                    self.error(
                        result.to_string(),
                        "List operation has no canonical receipt",
                    );
                }
                if list_operation_contract
                    .as_ref()
                    .is_some_and(|contract| contract.operation != *operation)
                {
                    self.error(
                        result.to_string(),
                        "List operation receipt disagrees with MIR operation",
                    );
                }
                self.result_at(result, &instruction.id, block, index);
            }
            VariantPredicate {
                result,
                variant,
                contract,
                ..
            } => {
                self.use_value(variant);
                if contract.is_none() {
                    self.error(
                        result.to_string(),
                        "Variant predicate has no canonical receipt",
                    );
                }
                self.result_at(result, &instruction.id, block, index);
            }
            ConstructSet { result, elements } => {
                self.values(elements);
                self.result_at(result, &instruction.id, block, index);
            }
            SetOp {
                result,
                set,
                argument,
                ..
            } => {
                self.use_value(set);
                if let Some(argument) = argument {
                    self.use_value(argument);
                }
                self.result_at(result, &instruction.id, block, index);
            }
            ConstructVariant { result, fields, .. }
            | ConstructVariantMove { result, fields, .. } => {
                for (field, value) in fields {
                    self.use_value(value);
                    if field.0.trim().is_empty() {
                        self.error(
                            result.to_string(),
                            "variant payload field identity is empty",
                        );
                    }
                }
                self.result_at(result, &instruction.id, block, index);
            }
            UpdateRecord {
                result,
                base,
                fields,
                kind,
            } => {
                self.use_value(base);
                self.values(fields);
                if let MirAggregateKind::Record { fields, .. } = kind {
                    for field in fields {
                        if field.0.trim().is_empty() {
                            self.error(result.to_string(), "record update field identity is empty");
                        }
                    }
                } else {
                    self.error(
                        result.to_string(),
                        "record update instruction requires a record aggregate kind",
                    );
                }
                self.result_at(result, &instruction.id, block, index);
            }
            Binary {
                result,
                left,
                right,
                ..
            } => {
                self.use_value(left);
                self.use_value(right);
                self.result_at(result, &instruction.id, block, index);
            }
            Unary {
                result, operand, ..
            } => {
                self.use_value(operand);
                self.result_at(result, &instruction.id, block, index);
            }
            Call {
                result, arguments, ..
            } => {
                for argument in arguments {
                    self.use_value(argument);
                }
                if let Some(result) = result {
                    self.result_at(result, &instruction.id, block, index);
                }
            }
            FlowTransition {
                result, arguments, ..
            } => {
                self.values(arguments);
                self.result_at(result, &instruction.id, block, index);
            }
            BuiltinCall {
                result, arguments, ..
            } => {
                self.values(arguments);
                self.result_at(result, &instruction.id, block, index);
            }
            Nop => {}
        }
    }

    fn check_terminator(&mut self, terminator: &MirTerminator) {
        match terminator {
            MirTerminator::Goto {
                edge,
                target,
                arguments,
            } => {
                self.edge(edge);
                self.target(target);
                self.values(arguments);
                self.check_arity(target, arguments, &[]);
            }
            MirTerminator::Branch {
                condition,
                then_edge,
                then_target,
                then_arguments,
                else_edge,
                else_target,
                else_arguments,
            } => {
                self.use_value(condition);
                self.edge(then_edge);
                self.edge(else_edge);
                self.target(then_target);
                self.target(else_target);
                self.values(then_arguments);
                self.values(else_arguments);
                self.check_arity(then_target, then_arguments, &[]);
                self.check_arity(else_target, else_arguments, &[]);
            }
            MirTerminator::Switch { scrutinee, arms }
            | MirTerminator::SwitchMove { scrutinee, arms } => {
                self.use_value(scrutinee);
                let mut has_default = false;
                for arm in arms {
                    self.edge(&arm.edge);
                    self.target(&arm.target);
                    self.values(&arm.arguments);
                    self.check_arity(&arm.target, &arm.arguments, &arm.bindings);
                    let mut binding_fields = BTreeSet::new();
                    for binding in &arm.bindings {
                        if binding.parameter.as_str().trim().is_empty()
                            || binding.projection.nominal.as_str().trim().is_empty()
                            || binding.projection.variant.0.trim().is_empty()
                            || binding.projection.field.0.trim().is_empty()
                            || binding.projection.field_ty.as_str().trim().is_empty()
                        {
                            self.error(arm.edge.to_string(), "switch binding identity is empty");
                        }
                        if binding.projection.field_index >= binding.projection.arity {
                            self.error(
                                arm.edge.to_string(),
                                "switch binding projection index is outside its payload arity",
                            );
                        }
                        if !binding_fields.insert(&binding.projection.field) {
                            self.error(
                                arm.edge.to_string(),
                                "switch binding field identity is duplicated",
                            );
                        }
                    }
                    if matches!(arm.case, MirSwitchCase::Default) {
                        if has_default {
                            self.error(
                                arm.edge.to_string(),
                                "switch has more than one default arm",
                            );
                        }
                        has_default = true;
                    }
                }
            }
            MirTerminator::Return { value } => {
                if let Some(value) = value {
                    self.use_value(value);
                    if let Some(value_ty) = self.function.values.get(value).map(|value| &value.ty) {
                        if value_ty != &self.function.result {
                            self.error(
                                value.to_string(),
                                "return value type disagrees with function result type",
                            );
                        }
                    }
                }
            }
            MirTerminator::Fault { value } => {
                if let Some(value) = value {
                    self.use_value(value);
                }
            }
            MirTerminator::Trap { code } => {
                if let Err(message) = types::validate_trap_code(code) {
                    self.error("terminator", message);
                }
            }
            MirTerminator::Unreachable => {}
        }
    }

    fn edge(&mut self, edge: &MirEdgeId) {
        if !self.edge_ids.insert(edge.clone()) {
            self.error(edge.to_string(), "edge identity is duplicated");
        }
    }

    fn target(&mut self, target: &MirBlockId) {
        if !self.function.blocks.contains_key(target) {
            self.error(target.to_string(), "edge targets a missing block");
        }
    }

    fn values(&mut self, values: &[MirValueId]) {
        for value in values {
            self.use_value(value);
        }
    }

    fn check_arity(
        &mut self,
        target: &MirBlockId,
        arguments: &[MirValueId],
        bindings: &[MirSwitchBinding],
    ) {
        if let Some(block) = self.function.blocks.get(target) {
            if block.parameters.len() != arguments.len() + bindings.len() {
                self.error(
                    target.to_string(),
                    format!(
                        "edge passes {} values/bindings but target expects {} parameters",
                        arguments.len() + bindings.len(),
                        block.parameters.len()
                    ),
                );
            }
            let parameters = block.parameters.clone();
            for (index, (argument, parameter)) in
                arguments.iter().zip(parameters.iter()).enumerate()
            {
                let argument_ty = self
                    .function
                    .values
                    .get(argument)
                    .map(|value| value.ty.clone());
                let parameter_ty = self
                    .function
                    .values
                    .get(&parameter.value)
                    .map(|value| value.ty.clone());
                if argument_ty.is_some() && parameter_ty.is_some() && argument_ty != parameter_ty {
                    self.error(
                        target.to_string(),
                        format!("edge argument {index} type disagrees with target parameter"),
                    );
                }
            }
            for (index, (binding, parameter)) in bindings
                .iter()
                .zip(parameters.iter().skip(arguments.len()))
                .enumerate()
            {
                if binding.parameter != parameter.value {
                    self.error(
                        target.to_string(),
                        format!(
                            "switch binding {index} parameter '{}' disagrees with target parameter '{}'",
                            binding.parameter, parameter.value
                        ),
                    );
                }
            }
        }
    }

    /// Reject values used outside the block that defines them unless that
    /// defining block dominates the use. This turns the value catalog from a
    /// mere name table into a real SSA-like contract while retaining explicit
    /// block parameters for control-flow joins.
    fn check_dominance(&mut self) {
        let reachable = self.reachable_blocks();
        if reachable.is_empty() {
            return;
        }
        let mut dominators: BTreeMap<MirBlockId, BTreeSet<MirBlockId>> = BTreeMap::new();
        for block in &reachable {
            if block == &self.function.entry {
                dominators.insert(block.clone(), BTreeSet::from([block.clone()]));
            } else {
                dominators.insert(block.clone(), reachable.clone());
            }
        }
        let predecessors = self.predecessors(&reachable);
        let mut changed = true;
        while changed {
            changed = false;
            for block in reachable
                .iter()
                .filter(|block| *block != &self.function.entry)
            {
                let Some(preds) = predecessors.get(block) else {
                    continue;
                };
                if preds.is_empty() {
                    continue;
                }
                let mut next = reachable.clone();
                for predecessor in preds {
                    if let Some(pred_dominators) = dominators.get(predecessor) {
                        next.retain(|candidate| pred_dominators.contains(candidate));
                    }
                }
                next.insert(block.clone());
                if dominators.get(block) != Some(&next) {
                    dominators.insert(block.clone(), next);
                    changed = true;
                }
            }
        }

        for (block_id, block) in &self.function.blocks {
            if !reachable.contains(block_id) {
                self.error(
                    block_id.to_string(),
                    "block is unreachable from function entry",
                );
                continue;
            }
            for (index, instruction) in block.instructions.iter().enumerate() {
                self.check_instruction_uses(block_id, index, instruction, &dominators, &reachable);
            }
            self.check_terminator_uses(
                block_id,
                block.instructions.len(),
                &block.terminator,
                &dominators,
                &reachable,
            );
        }
    }

    fn reachable_blocks(&self) -> BTreeSet<MirBlockId> {
        let mut reachable = BTreeSet::new();
        let mut pending = vec![self.function.entry.clone()];
        while let Some(block_id) = pending.pop() {
            if !reachable.insert(block_id.clone()) {
                continue;
            }
            let Some(block) = self.function.blocks.get(&block_id) else {
                continue;
            };
            for successor in Self::successors(&block.terminator) {
                if self.function.blocks.contains_key(&successor) {
                    pending.push(successor);
                }
            }
        }
        reachable
    }

    fn predecessors(
        &self,
        reachable: &BTreeSet<MirBlockId>,
    ) -> BTreeMap<MirBlockId, BTreeSet<MirBlockId>> {
        let mut predecessors = reachable
            .iter()
            .cloned()
            .map(|block| (block, BTreeSet::new()))
            .collect::<BTreeMap<_, _>>();
        for (block_id, block) in &self.function.blocks {
            if !reachable.contains(block_id) {
                continue;
            }
            for successor in Self::successors(&block.terminator) {
                if let Some(preds) = predecessors.get_mut(&successor) {
                    preds.insert(block_id.clone());
                }
            }
        }
        predecessors
    }

    fn successors(terminator: &MirTerminator) -> Vec<MirBlockId> {
        match terminator {
            MirTerminator::Goto { target, .. } => vec![target.clone()],
            MirTerminator::Branch {
                then_target,
                else_target,
                ..
            } => vec![then_target.clone(), else_target.clone()],
            MirTerminator::Switch { arms, .. } | MirTerminator::SwitchMove { arms, .. } => {
                arms.iter().map(|arm| arm.target.clone()).collect()
            }
            MirTerminator::Return { .. }
            | MirTerminator::Trap { .. }
            | MirTerminator::Fault { .. }
            | MirTerminator::Unreachable => Vec::new(),
        }
    }

    fn check_instruction_uses(
        &mut self,
        block: &MirBlockId,
        index: usize,
        instruction: &MirInstruction,
        dominators: &BTreeMap<MirBlockId, BTreeSet<MirBlockId>>,
        reachable: &BTreeSet<MirBlockId>,
    ) {
        let mut uses: Vec<MirValueId> = Vec::new();
        match &instruction.kind {
            MirInstructionKind::Const { .. } | MirInstructionKind::Nop => {}
            MirInstructionKind::Load { place, .. } => {
                if let Ok(local) = MirValueId::new(format!("local:{}", place.base.0 .0)) {
                    uses.push(local);
                }
            }
            MirInstructionKind::Copy { source, .. }
            | MirInstructionKind::Move { source, .. }
            | MirInstructionKind::Clone { source, .. }
            | MirInstructionKind::Convert { source, .. } => uses.push(source.clone()),
            MirInstructionKind::Drop { value }
            | MirInstructionKind::EndBorrow { borrow: value } => uses.push(value.clone()),
            MirInstructionKind::Borrow { source, .. } => uses.push(source.clone()),
            MirInstructionKind::Project {
                base, projection, ..
            } => {
                uses.push(base.clone());
                if let MirProjection::Index(index) = projection {
                    uses.push(index.clone());
                }
            }
            MirInstructionKind::MoveProject {
                base, projection, ..
            } => {
                uses.push(base.clone());
                if let MirProjection::Index(index) = projection {
                    uses.push(index.clone());
                }
            }
            MirInstructionKind::Construct { fields, .. } => {
                uses.extend(fields.iter().cloned());
            }
            MirInstructionKind::ConstructList { elements, .. } => {
                uses.extend(elements.iter().cloned());
            }
            MirInstructionKind::ListOp { list, argument, .. } => {
                uses.push(list.clone());
                if let Some(argument) = argument {
                    uses.push(argument.clone());
                }
            }
            MirInstructionKind::VariantPredicate { variant, .. } => uses.push(variant.clone()),
            MirInstructionKind::ConstructSet { elements, .. } => {
                uses.extend(elements.iter().cloned());
            }
            MirInstructionKind::SetOp { set, argument, .. } => {
                uses.push(set.clone());
                if let Some(argument) = argument {
                    uses.push(argument.clone());
                }
            }
            MirInstructionKind::ConstructVariant { fields, .. }
            | MirInstructionKind::ConstructVariantMove { fields, .. } => {
                uses.extend(fields.iter().map(|(_, value)| value.clone()));
            }
            MirInstructionKind::UpdateRecord { base, fields, .. } => {
                uses.push(base.clone());
                uses.extend(fields.iter().cloned());
            }
            MirInstructionKind::Binary { left, right, .. } => {
                uses.push(left.clone());
                uses.push(right.clone());
            }
            MirInstructionKind::Unary { operand, .. } => uses.push(operand.clone()),
            MirInstructionKind::Call { arguments, .. }
            | MirInstructionKind::FlowTransition { arguments, .. }
            | MirInstructionKind::BuiltinCall { arguments, .. } => {
                uses.extend(arguments.iter().cloned())
            }
        }
        for value in uses {
            self.check_use_site(&value, block, index, dominators, reachable);
        }
    }

    fn check_terminator_uses(
        &mut self,
        block: &MirBlockId,
        index: usize,
        terminator: &MirTerminator,
        dominators: &BTreeMap<MirBlockId, BTreeSet<MirBlockId>>,
        reachable: &BTreeSet<MirBlockId>,
    ) {
        let mut uses = Vec::new();
        match terminator {
            MirTerminator::Goto { arguments, .. } => uses.extend(arguments.iter().cloned()),
            MirTerminator::Branch {
                condition,
                then_arguments,
                else_arguments,
                ..
            } => {
                uses.push(condition.clone());
                uses.extend(then_arguments.iter().cloned());
                uses.extend(else_arguments.iter().cloned());
            }
            MirTerminator::Switch { scrutinee, arms }
            | MirTerminator::SwitchMove { scrutinee, arms } => {
                uses.push(scrutinee.clone());
                for arm in arms {
                    uses.extend(arm.arguments.iter().cloned());
                }
            }
            MirTerminator::Return { value } | MirTerminator::Fault { value } => {
                uses.extend(value.iter().cloned());
            }
            MirTerminator::Trap { .. } | MirTerminator::Unreachable => {}
        }
        for value in uses {
            self.check_use_site(&value, block, index, dominators, reachable);
        }
    }

    fn check_use_site(
        &mut self,
        value: &MirValueId,
        use_block: &MirBlockId,
        use_index: usize,
        dominators: &BTreeMap<MirBlockId, BTreeSet<MirBlockId>>,
        reachable: &BTreeSet<MirBlockId>,
    ) {
        let Some(definition) = self.definition_sites.get(value) else {
            return;
        };
        let valid = match definition {
            MirDefinitionSite::FunctionParameter => true,
            MirDefinitionSite::BlockParameter(def_block) => {
                self.definition_dominates(def_block, use_block, dominators, reachable)
            }
            MirDefinitionSite::Instruction { block, index } => {
                if block == use_block {
                    *index < use_index
                } else {
                    self.definition_dominates(block, use_block, dominators, reachable)
                }
            }
        };
        if !valid {
            self.error(
                value.to_string(),
                format!("value is used before its definition at block {use_block}"),
            );
        }
    }

    fn definition_dominates(
        &self,
        definition_block: &MirBlockId,
        use_block: &MirBlockId,
        dominators: &BTreeMap<MirBlockId, BTreeSet<MirBlockId>>,
        reachable: &BTreeSet<MirBlockId>,
    ) -> bool {
        reachable.contains(definition_block)
            && dominators
                .get(use_block)
                .is_some_and(|blocks| blocks.contains(definition_block))
    }

    fn finish(self) -> Result<(), Vec<MirValidationError>> {
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(self.errors)
        }
    }
}

#[cfg(test)]
mod tests;
