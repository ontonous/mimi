//! Identity receipts for Canonical MIR consumer routes.
//!
//! A route receipt is an audit witness, not another semantic IR.  It is
//! computed from an already validated `MirProgram` and deliberately includes
//! the TypeDesc catalog, concrete instances, transition contracts, function
//! CFG/instructions, and ownership event streams.  Consumers may report or
//! compare the receipt, but they never use it to reconstruct frontend facts.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::core::mir::reference::MirProgram;
use crate::core::NodeId;

/// Schema version for the cross-consumer route receipt.
pub const MIR_ROUTE_RECEIPT_SCHEMA: &str = "mimi-mir-route-receipt-v1";

/// Stable profile used by the public AST-free bytecode adapter when the
/// caller supplies only an immutable `MirProgram` and no route manifest.
/// Keeping the profile beside the receipt schema prevents the adapter and its
/// evidence consumers from drifting into different provenance labels.
pub const MIR_BYTECODE_DIRECT_ROUTE_PROFILE: &str = "bytecode-direct-v1";

/// Stable profile used by the direct native `CodeGenerator::compile_checked`
/// entry.  The native preflight and final LLVM emission must carry the same
/// immutable MIR witness instead of silently manufacturing separate route
/// identities for bytecode, verifier, and native consumers.
pub const MIR_NATIVE_DIRECT_ROUTE_PROFILE: &str = "native-direct-v1";

/// Stable diagnostic identifiers for route receipt admission failures. Keep
/// these available through the MIR namespace while the diagnostic registry
/// remains the single source of truth for CLI/LSP code descriptions.
pub use crate::diagnostic::codes::{
    MIR_FFI_DECLARATION_BOUNDARY_ERROR_CODE, MIR_FFI_ROUTE_MANIFEST_ERROR_CODE,
    MIR_FFI_ROUTE_RECEIPT_ERROR_CODE, MIR_ROUTE_COVERAGE_ERROR_CODE, MIR_ROUTE_MANIFEST_ERROR_CODE,
    MIR_ROUTE_MATERIALIZATION_ERROR_CODE, MIR_ROUTE_RECEIPT_ERROR_CODE,
};

/// Stable header for the line-oriented CLI evidence manifest.
pub const MIR_ROUTE_RECEIPT_MANIFEST_HEADER: &str = "mimi-mir-route-manifest-v1";

/// Ordered field names emitted by the CLI evidence manifest. Keeping the
/// field set beside the receipt lets consumers detect schema drift without
/// reverse-engineering the renderer.
pub const MIR_ROUTE_RECEIPT_MANIFEST_FIELDS: [&str; 9] = [
    "schema",
    "profile",
    "mir_digest",
    "type_desc_digest",
    "abi_digest",
    "ffi_digest",
    "ownership_digest",
    "flow_transition_digest",
    "root_owners",
];

/// Schema prefix for the semantic MIR identity digest.
pub const MIR_IDENTITY_SCHEMA: &str = "mimi-canonical-mir-identity-v1";

/// Versioned contract used by every current MIR consumer before execution or
/// backend lowering. A route receipt records this separately from the MIR
/// digest so validator evolution cannot masquerade as a program identity
/// change.
pub const MIR_ROUTE_VALIDATOR_CONTRACT_ID: &str = "mimi-mir-route-validator-v1";

/// Schema for the Core-owned descriptor selection that authorizes payload
/// cloning in the ordinary Map/Any owner-v2 slice. This receipt is kept
/// separate from the public line-oriented route manifest so adding V2
/// evidence cannot change the manifest contract for unrelated routes.
pub const ORDINARY_MAP_OWNED_ANY_V2_PAYLOAD_SELECTION_SCHEMA: &str =
    "mimi-ordinary-map-owned-any-v2-payload-selection-v1";

/// Schema for the exact MIR binding/drop witness of one V2 `map_get` result.
pub const ORDINARY_MAP_OWNED_ANY_V2_GET_ANY_RESULT_SCHEMA: &str =
    "mimi-ordinary-map-owned-any-v2-get-any-result-v1";

/// Schema for the exact Checker/GetAny/TypeDesc proof of one V2 DynamicAny
/// unpack operation.
pub const ORDINARY_MAP_OWNED_ANY_V2_UNPACK_RESULT_SCHEMA: &str =
    "mimi-ordinary-map-owned-any-v2-unpack-result-v1";

/// Schema for the exact MIR binding/drop witness of one V2 `values` snapshot.
pub const ORDINARY_MAP_OWNED_ANY_V2_VALUES_RESULT_SCHEMA: &str =
    "mimi-ordinary-map-owned-any-v2-values-result-v1";

/// Schema for the exact Checker/index/Values/TypeDesc proof of one Values
/// element DynamicAny unpack operation.
pub const ORDINARY_MAP_OWNED_ANY_V2_VALUES_UNPACK_RESULT_SCHEMA: &str =
    "mimi-ordinary-map-owned-any-v2-values-unpack-result-v1";

const ORDINARY_MAP_OWNED_ANY_V2_PROFILE: &str = "ordinary-map-owned-any-v2";

/// One Checker-authorized Set payload, bound to its exact MIR instruction,
/// value identity, and both Checker/MIR structural type identities.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OrdinaryMapOwnedAnyV2PayloadSelectionEntry {
    pub owner: NodeId,
    pub instruction: super::MirInstructionId,
    pub value: super::MirValueId,
    pub checker_type: crate::core::ir::ResolvedTypeId,
    pub mir_type: crate::core::ir::ResolvedTypeId,
    pub type_desc_digest: String,
}

/// A unique TypeDesc admitted as an ordinary Map/Any owner-v2 payload.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OrdinaryMapOwnedAnyV2SelectedType {
    pub type_id: crate::core::ir::ResolvedTypeId,
    pub type_desc_digest: String,
}

/// Why one exact TypeDesc is accepted as the target of an AnyOwner-v2
/// unpack. A Set-selected payload is scoped to the GetAny root lineage;
/// otherwise only the canonical Copy i32 miss token may authorize an unpack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OrdinaryMapOwnedAnyV2UnpackPayloadSource {
    SelectedSetPayload,
    CanonicalI32Miss,
}

/// Core-owned, deterministic proof of the payload TypeDesc set selected by
/// Checker receipts for ordinary Map/Any owner-v2 Set actions.
///
/// Consumers may use `selected_types` as the closed selection table, but it
/// grants no other Map operation or route readiness by itself. `entries`
/// bind each selected type back to its Checker action and exact MIR Set value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrdinaryMapOwnedAnyV2PayloadSelectionReceipt {
    pub schema: &'static str,
    pub profile: &'static str,
    pub mir_digest: String,
    pub type_desc_digest: String,
    pub entries: Vec<OrdinaryMapOwnedAnyV2PayloadSelectionEntry>,
    pub selected_types: Vec<OrdinaryMapOwnedAnyV2SelectedType>,
    pub selection_digest: String,
}

/// Derived Core proof for one ordinary Map/Any V2 GetAny result. It joins
/// the private Checker action/result receipts to the dedicated MIR GetAny,
/// direct binding Move, explicit Drop, and the canonical miss payload TypeDesc.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OrdinaryMapOwnedAnyV2GetAnyResultReceipt {
    pub schema: &'static str,
    pub profile: &'static str,
    pub owner: NodeId,
    pub operation: NodeId,
    pub instruction: super::MirInstructionId,
    pub source_node: NodeId,
    pub source_local: crate::core::ir::ResolvedLocalId,
    pub root: super::MirValueId,
    pub checker_source_type: crate::core::ir::ResolvedTypeId,
    pub key_node: NodeId,
    pub key: super::MirValueId,
    pub checker_key_type: crate::core::ir::ResolvedTypeId,
    pub checker_key_local: Option<crate::core::ir::ResolvedLocalId>,
    pub checker_result_type: crate::core::ir::ResolvedTypeId,
    pub result: super::MirValueId,
    pub mir_result_type: crate::core::ir::ResolvedTypeId,
    pub binding_local: crate::core::ir::ResolvedLocalId,
    pub binding: super::MirValueId,
    pub binding_point: NodeId,
    pub(crate) settlement: crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind,
    pub settlement_point: NodeId,
    pub settlement_instruction: super::MirInstructionId,
    /// Present only for the directly nested `map_get(map_new(), key)` shape.
    /// This is the Checker-owned root identity plus exact MIR construction,
    /// GetAny, and immediate root-Drop sites.
    pub temporary_root: Option<OrdinaryMapOwnedAnyV2TemporaryRootMirReceipt>,
    pub miss_payload_type: crate::core::ir::ResolvedTypeId,
    pub miss_payload_type_desc_digest: String,
}

/// Derived Core proof for one checked AnyOwner-v2 unpack. It binds the
/// source projection and conversion to its GetAny pair owner, selected
/// payload descriptor (or canonical i32 miss), and later explicit pair Drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrdinaryMapOwnedAnyV2UnpackResultReceipt {
    pub schema: &'static str,
    pub profile: &'static str,
    pub owner: NodeId,
    pub operation: NodeId,
    pub instruction: super::MirInstructionId,
    pub pair_binding_local: crate::core::ir::ResolvedLocalId,
    pub pair: super::MirValueId,
    pub pair_binding_point: NodeId,
    pub projection: NodeId,
    pub cast: NodeId,
    pub conversion_kind: crate::core::ir::CheckedConversionKind,
    pub conversion_from: crate::core::ir::ResolvedTypeId,
    pub conversion_to: crate::core::ir::ResolvedTypeId,
    pub result: super::MirValueId,
    pub result_type: crate::core::ir::ResolvedTypeId,
    pub settlement: crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind,
    pub settlement_point: NodeId,
    pub settlement_instruction: super::MirInstructionId,
    pub payload_source: OrdinaryMapOwnedAnyV2UnpackPayloadSource,
    pub payload_selection_digest: String,
    pub payload_type_desc_digest: String,
}

/// MIR binding for the narrow compiler-owned temporary Map root receipt.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OrdinaryMapOwnedAnyV2TemporaryRootMirReceipt {
    pub local: crate::core::ir::ResolvedLocalId,
    pub root: super::MirValueId,
    pub construction: NodeId,
    pub root_type: crate::core::ir::ResolvedTypeId,
    pub get_any: NodeId,
    pub source_node: NodeId,
    pub source_type: crate::core::ir::ResolvedTypeId,
    pub settlement: crate::core::OrdinaryMapOwnedAnyV2RootSettlementKind,
    pub settlement_point: NodeId,
    pub new_instruction: super::MirInstructionId,
    pub get_any_instruction: super::MirInstructionId,
    pub drop_instruction: super::MirInstructionId,
}

/// Derived Core proof for one ordinary Map/Any V2 Values snapshot. The
/// Checker source type remains exactly `List<DynamicAny>`; MIR represents the
/// result with List storage and its dedicated V2 element owner, accepted only
/// with one direct local binding and one explicit Drop.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OrdinaryMapOwnedAnyV2ValuesResultReceipt {
    pub schema: &'static str,
    pub profile: &'static str,
    pub owner: NodeId,
    pub operation: NodeId,
    pub instruction: super::MirInstructionId,
    pub source_node: NodeId,
    pub source_local: crate::core::ir::ResolvedLocalId,
    pub root: super::MirValueId,
    pub checker_source_type: crate::core::ir::ResolvedTypeId,
    pub checker_result_type: crate::core::ir::ResolvedTypeId,
    pub result: super::MirValueId,
    pub mir_result_type: crate::core::ir::ResolvedTypeId,
    pub binding_local: crate::core::ir::ResolvedLocalId,
    pub binding: super::MirValueId,
    pub binding_point: NodeId,
    pub(crate) settlement: crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind,
    pub settlement_point: NodeId,
    pub settlement_instruction: super::MirInstructionId,
    pub payload_selection_digest: String,
}

/// Derived Core proof for one checked indexed unpack from a V2 Values
/// snapshot. It joins the exact snapshot result/drop receipt, source-level
/// projection and index identity, target conversion, and same-root Set which
/// selected the payload TypeDesc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrdinaryMapOwnedAnyV2ValuesUnpackResultReceipt {
    pub schema: &'static str,
    pub profile: &'static str,
    pub owner: NodeId,
    pub operation: NodeId,
    pub instruction: super::MirInstructionId,
    pub values_binding_local: crate::core::ir::ResolvedLocalId,
    pub values: super::MirValueId,
    pub values_binding_point: NodeId,
    pub projection: NodeId,
    pub index_expression: NodeId,
    pub index_type: crate::core::ir::ResolvedTypeId,
    pub index: super::MirValueId,
    pub cast: NodeId,
    pub conversion_kind: crate::core::ir::CheckedConversionKind,
    pub conversion_from: crate::core::ir::ResolvedTypeId,
    pub conversion_to: crate::core::ir::ResolvedTypeId,
    pub result: super::MirValueId,
    pub result_type: crate::core::ir::ResolvedTypeId,
    pub result_binding_local: crate::core::ir::ResolvedLocalId,
    pub result_binding_point: NodeId,
    pub result_binding: super::MirValueId,
    /// Checker disposition of the owner obligation created by the unpacked
    /// value. This is distinct from `settlement`, which belongs to the
    /// Values snapshot List.
    pub(crate) result_owner_settlement:
        crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind,
    /// Source Drop point for an explicit whole-record Drop, or the root
    /// tail-expression anchor for compiler-owned String cleanup.
    pub result_owner_settlement_point: Option<NodeId>,
    pub result_owner_settlement_index: Option<usize>,
    /// Exact MIR Drop that settles an owned String or whole-record result.
    /// Copy results have no owner Drop.
    pub result_owner_settlement_instruction: Option<super::MirInstructionId>,
    pub settlement: crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind,
    pub settlement_point: NodeId,
    pub settlement_instruction: super::MirInstructionId,
    pub payload_selection_digest: String,
    pub payload_type_desc_digest: String,
}

/// Immutable audit witness shared by reference, bytecode, native, and
/// verifier route tests.  The digest fields are independent so a report can
/// distinguish a TypeDesc/ownership drift from a whole-program MIR drift.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalMirRouteReceipt {
    pub schema: &'static str,
    pub profile: String,
    pub mir_digest: String,
    pub type_desc_digest: String,
    pub abi_digest: String,
    /// Digest of the checker-owned FFI receipt table, independent of the
    /// enclosing function/ownership graph.  Matrix and CLI evidence can
    /// compare the FFI boundary directly without parsing the whole MIR
    /// identity text.
    pub ffi_digest: String,
    pub ownership_digest: String,
    pub flow_transition_digest: String,
    pub root_owners: Vec<NodeId>,
}

impl MirProgram {
    /// Return the deterministic semantic digest of this complete MIR graph.
    ///
    /// This is the single identity algorithm used by proof artifacts and
    /// route receipts.  It is intentionally independent of source spans,
    /// public display names, backend ABI spellings, allocator addresses, and
    /// consumer execution order.
    pub fn canonical_digest(&self) -> String {
        digest(canonical_mir_text(self))
    }

    /// Produce a route receipt after the caller has passed the relevant MIR
    /// validity and island capability gates.
    pub fn route_receipt(&self, profile: impl Into<String>) -> CanonicalMirRouteReceipt {
        let type_desc_text = self.type_catalog().canonical_text();
        let mut abi_text = self.type_catalog().abi_canonical_text();
        abi_text.push_str(&self.export_contract_text());
        let ffi_text = canonical_ffi_text(self);
        let ownership_text = canonical_ownership_text(self);
        CanonicalMirRouteReceipt {
            schema: MIR_ROUTE_RECEIPT_SCHEMA,
            profile: profile.into(),
            mir_digest: self.canonical_digest(),
            type_desc_digest: digest(type_desc_text),
            abi_digest: digest(abi_text),
            ffi_digest: digest(ffi_text),
            ownership_digest: digest(ownership_text),
            flow_transition_digest: digest(canonical_transition_text(self)),
            root_owners: canonical_root_owners(self),
        }
    }

    /// Derive the Core-owned AnyOwner-v2 payload selection from the private
    /// Checker action ledger. The MIR Set operands and TypeDesc catalog are
    /// checked against that authority, but never used to widen the selected
    /// type set. `None` means this program has no V2 action or instruction;
    /// malformed or incomplete V2 evidence is an error.
    pub fn ordinary_map_owned_any_v2_payload_selection_receipt(
        &self,
    ) -> Result<Option<OrdinaryMapOwnedAnyV2PayloadSelectionReceipt>, String> {
        derive_ordinary_map_owned_any_v2_payload_selection_receipt(self)
    }

    /// Derive a validated GetAny result witness from private Checker receipts
    /// and exact MIR instruction/value sites. Values has its own disjoint
    /// result witness and TypeDesc.
    pub fn ordinary_map_owned_any_v2_get_any_result_receipts(
        &self,
    ) -> Result<Vec<OrdinaryMapOwnedAnyV2GetAnyResultReceipt>, String> {
        derive_ordinary_map_owned_any_v2_get_any_result_receipts(self)
    }

    /// Derive a validated checked-unpack witness from the private Checker
    /// CFG receipt, exact GetAny result owner, MIR instruction and TypeDesc
    /// selection. A cast without this complete chain is not executable.
    pub fn ordinary_map_owned_any_v2_unpack_result_receipts(
        &self,
    ) -> Result<Vec<OrdinaryMapOwnedAnyV2UnpackResultReceipt>, String> {
        derive_ordinary_map_owned_any_v2_unpack_result_receipts(self)
    }

    /// Derive a validated Values snapshot witness from the private Checker
    /// action/result receipts and the exact MIR instruction, binding and Drop.
    pub fn ordinary_map_owned_any_v2_values_result_receipts(
        &self,
    ) -> Result<Vec<OrdinaryMapOwnedAnyV2ValuesResultReceipt>, String> {
        derive_ordinary_map_owned_any_v2_values_result_receipts(self)
    }

    /// Derive a validated checked-unpack witness for one Values element from
    /// the private Checker CFG receipt, exact Values result/drop owner, MIR
    /// projection operands, and same-root selected payload descriptor.
    pub fn ordinary_map_owned_any_v2_values_unpack_result_receipts(
        &self,
    ) -> Result<Vec<OrdinaryMapOwnedAnyV2ValuesUnpackResultReceipt>, String> {
        derive_ordinary_map_owned_any_v2_values_unpack_result_receipts(self)
    }
}

impl OrdinaryMapOwnedAnyV2GetAnyResultReceipt {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != ORDINARY_MAP_OWNED_ANY_V2_GET_ANY_RESULT_SCHEMA
            || self.profile != ORDINARY_MAP_OWNED_ANY_V2_PROFILE
        {
            return Err("unexpected AnyOwner-v2 GetAny result schema/profile".into());
        }
        if self.operation.0.trim().is_empty() {
            return Err("AnyOwner-v2 GetAny operation/source identity is empty".into());
        }
        if self.settlement != crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop {
            return Err("AnyOwner-v2 GetAny result is not settled by explicit Drop".into());
        }
        for (name, digest) in [(
            "miss_payload_type_desc_digest",
            self.miss_payload_type_desc_digest.as_str(),
        )] {
            if !is_lower_hex_digest(digest) {
                return Err(format!(
                    "AnyOwner-v2 GetAny {name} must be a 64-character lowercase hex digest"
                ));
            }
        }
        if self.source_node.0.trim().is_empty()
            || self.key_node.0.trim().is_empty()
            || self.binding_point.0.trim().is_empty()
            || self.settlement_point.0.trim().is_empty()
            || self.source_local.0 .0.trim().is_empty()
            || self.binding_local.0 .0.trim().is_empty()
            || self.instruction.as_str().trim().is_empty()
            || self.settlement_instruction.as_str().trim().is_empty()
            || self.root.as_str().trim().is_empty()
            || self.key.as_str().trim().is_empty()
            || self.result.as_str().trim().is_empty()
            || self.binding.as_str().trim().is_empty()
        {
            return Err("AnyOwner-v2 GetAny result receipt contains an empty identity".into());
        }
        if let Some(root) = &self.temporary_root {
            if root.local.0 .0.trim().is_empty()
                || root.root.as_str().trim().is_empty()
                || root.construction.0.trim().is_empty()
                || root.get_any.0.trim().is_empty()
                || root.source_node.0.trim().is_empty()
                || root.settlement_point.0.trim().is_empty()
                || root.new_instruction.as_str().trim().is_empty()
                || root.get_any_instruction.as_str().trim().is_empty()
                || root.drop_instruction.as_str().trim().is_empty()
                || root.local != self.source_local
                || root.root != self.root
                || root.get_any != self.operation
                || root.get_any_instruction != self.instruction
                || root.source_node != self.source_node
                || root.source_type != self.checker_source_type
                || root.construction != root.source_node
                || root.root_type != root.source_type
                || root.settlement_point != root.get_any
                || root.settlement
                    != crate::core::OrdinaryMapOwnedAnyV2RootSettlementKind::ImmediateGetAnyDrop
            {
                return Err("AnyOwner-v2 temporary root receipt does not bind this GetAny source and immediate settlement".into());
            }
        }
        if self.mir_result_type != super::types::ordinary_map_owned_any_v2_get_result_type_id() {
            return Err("AnyOwner-v2 GetAny result uses another MIR TypeDesc identity".into());
        }
        if self.miss_payload_type
            != crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32)
        {
            return Err("AnyOwner-v2 GetAny miss payload is not canonical i32".into());
        }
        let expected_key = self
            .checker_key_local
            .as_ref()
            .and_then(|local| super::MirValueId::new(format!("local:{}", local.0 .0)).ok())
            .or_else(|| super::MirValueId::new(format!("expr:{}", self.key_node.0)).ok());
        if self.checker_key_type
            != crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String)
            || expected_key.as_ref() != Some(&self.key)
        {
            return Err(
                "AnyOwner-v2 GetAny key identity/type disagrees with its Checker local/source receipt"
                    .into(),
            );
        }
        Ok(())
    }

    /// Recompute this witness from the program's private Checker receipts,
    /// instruction graph and TypeDesc catalog.
    pub fn validate_against_program(&self, program: &MirProgram) -> Result<(), String> {
        self.validate()?;
        let actual = program
            .ordinary_map_owned_any_v2_get_any_result_receipts()?
            .into_iter()
            .find(|receipt| receipt.owner == self.owner && receipt.operation == self.operation)
            .ok_or_else(|| "MIR program has no matching AnyOwner-v2 GetAny result".to_string())?;
        if actual != *self {
            return Err(
                "AnyOwner-v2 GetAny result receipt does not match Checker actions, MIR, or TypeDescs"
                    .into(),
            );
        }
        Ok(())
    }
}

impl OrdinaryMapOwnedAnyV2UnpackResultReceipt {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != ORDINARY_MAP_OWNED_ANY_V2_UNPACK_RESULT_SCHEMA
            || self.profile != ORDINARY_MAP_OWNED_ANY_V2_PROFILE
        {
            return Err("unexpected AnyOwner-v2 unpack result schema/profile".into());
        }
        if self.settlement != crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop {
            return Err("AnyOwner-v2 unpack pair is not settled by explicit Drop".into());
        }
        for (name, digest) in [
            (
                "payload_selection_digest",
                self.payload_selection_digest.as_str(),
            ),
            (
                "payload_type_desc_digest",
                self.payload_type_desc_digest.as_str(),
            ),
        ] {
            if !is_lower_hex_digest(digest) {
                return Err(format!(
                    "AnyOwner-v2 unpack {name} must be a 64-character lowercase hex digest"
                ));
            }
        }
        if self.operation.0.trim().is_empty()
            || self.pair_binding_local.0 .0.trim().is_empty()
            || self.pair.as_str().trim().is_empty()
            || self.pair_binding_point.0.trim().is_empty()
            || self.projection.0.trim().is_empty()
            || self.cast.0.trim().is_empty()
            || self.result.as_str().trim().is_empty()
            || self.settlement_point.0.trim().is_empty()
            || self.instruction.as_str().trim().is_empty()
            || self.settlement_instruction.as_str().trim().is_empty()
        {
            return Err("AnyOwner-v2 unpack receipt contains an empty identity".into());
        }
        if self.conversion_kind != crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
            || self.conversion_from == self.conversion_to
            || self.conversion_to != self.result_type
        {
            return Err("AnyOwner-v2 unpack receipt has a noncanonical checked conversion".into());
        }
        if self.payload_source == OrdinaryMapOwnedAnyV2UnpackPayloadSource::CanonicalI32Miss
            && self.conversion_to
                != crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32)
        {
            return Err(
                "AnyOwner-v2 canonical miss source can authorize only the i32 target".into(),
            );
        }
        if self.pair
            != super::MirValueId::new(format!("local:{}", self.pair_binding_local.0 .0))
                .map_err(|error| error.to_string())?
        {
            return Err("AnyOwner-v2 unpack pair is not its direct Checker binding".into());
        }
        Ok(())
    }

    pub fn validate_against_program(&self, program: &MirProgram) -> Result<(), String> {
        self.validate()?;
        let actual = program
            .ordinary_map_owned_any_v2_unpack_result_receipts()?
            .into_iter()
            .find(|receipt| receipt.owner == self.owner && receipt.cast == self.cast)
            .ok_or_else(|| "MIR program has no matching AnyOwner-v2 unpack result".to_string())?;
        if actual != *self {
            return Err(
                "AnyOwner-v2 unpack receipt does not match Checker, MIR, or TypeDesc evidence"
                    .into(),
            );
        }
        Ok(())
    }
}

impl OrdinaryMapOwnedAnyV2ValuesResultReceipt {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != ORDINARY_MAP_OWNED_ANY_V2_VALUES_RESULT_SCHEMA
            || self.profile != ORDINARY_MAP_OWNED_ANY_V2_PROFILE
        {
            return Err("unexpected AnyOwner-v2 Values result schema/profile".into());
        }
        if self.settlement != crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop {
            return Err("AnyOwner-v2 Values result is not settled by explicit Drop".into());
        }
        if !is_lower_hex_digest(&self.payload_selection_digest) {
            return Err("AnyOwner-v2 Values payload selection digest is malformed".into());
        }
        if self.operation.0.trim().is_empty()
            || self.source_node.0.trim().is_empty()
            || self.binding_point.0.trim().is_empty()
            || self.settlement_point.0.trim().is_empty()
            || self.source_local.0 .0.trim().is_empty()
            || self.binding_local.0 .0.trim().is_empty()
            || self.instruction.as_str().trim().is_empty()
            || self.settlement_instruction.as_str().trim().is_empty()
            || self.root.as_str().trim().is_empty()
            || self.result.as_str().trim().is_empty()
            || self.binding.as_str().trim().is_empty()
        {
            return Err("AnyOwner-v2 Values result receipt contains an empty identity".into());
        }
        if self.mir_result_type != super::types::ordinary_map_owned_any_v2_values_type_id() {
            return Err("AnyOwner-v2 Values result uses another MIR TypeDesc identity".into());
        }
        Ok(())
    }

    /// Recompute this witness from the program's private Checker receipts,
    /// instruction graph and TypeDesc catalog.
    pub fn validate_against_program(&self, program: &MirProgram) -> Result<(), String> {
        self.validate()?;
        let actual = program
            .ordinary_map_owned_any_v2_values_result_receipts()?
            .into_iter()
            .find(|receipt| receipt.owner == self.owner && receipt.operation == self.operation)
            .ok_or_else(|| "MIR program has no matching AnyOwner-v2 Values result".to_string())?;
        if actual != *self {
            return Err(
                "AnyOwner-v2 Values result receipt does not match Checker actions, MIR, or TypeDescs"
                    .into(),
            );
        }
        Ok(())
    }
}

impl OrdinaryMapOwnedAnyV2ValuesUnpackResultReceipt {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != ORDINARY_MAP_OWNED_ANY_V2_VALUES_UNPACK_RESULT_SCHEMA
            || self.profile != ORDINARY_MAP_OWNED_ANY_V2_PROFILE
        {
            return Err("unexpected AnyOwner-v2 Values unpack schema/profile".into());
        }
        if self.settlement != crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop {
            return Err("AnyOwner-v2 Values snapshot is not settled by explicit Drop".into());
        }
        for (name, digest) in [
            (
                "payload_selection_digest",
                self.payload_selection_digest.as_str(),
            ),
            (
                "payload_type_desc_digest",
                self.payload_type_desc_digest.as_str(),
            ),
        ] {
            if !is_lower_hex_digest(digest) {
                return Err(format!(
                    "AnyOwner-v2 Values unpack {name} must be a 64-character lowercase hex digest"
                ));
            }
        }
        if self.operation.0.trim().is_empty()
            || self.instruction.as_str().trim().is_empty()
            || self.values_binding_local.0 .0.trim().is_empty()
            || self.values.as_str().trim().is_empty()
            || self.values_binding_point.0.trim().is_empty()
            || self.projection.0.trim().is_empty()
            || self.index_expression.0.trim().is_empty()
            || self.index.as_str().trim().is_empty()
            || self.cast.0.trim().is_empty()
            || self.result.as_str().trim().is_empty()
            || self.result_binding_local.0 .0.trim().is_empty()
            || self.result_binding_point.0.trim().is_empty()
            || self.result_binding.as_str().trim().is_empty()
            || self.settlement_point.0.trim().is_empty()
            || self.settlement_instruction.as_str().trim().is_empty()
        {
            return Err("AnyOwner-v2 Values unpack receipt contains an empty identity".into());
        }
        if self.values
            != super::MirValueId::new(format!("local:{}", self.values_binding_local.0 .0))
                .map_err(|error| error.to_string())?
        {
            return Err(
                "AnyOwner-v2 Values unpack source is not its direct snapshot binding".into(),
            );
        }
        if self.conversion_kind != crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
            || self.conversion_from == self.conversion_to
            || self.conversion_to != self.result_type
        {
            return Err("AnyOwner-v2 Values unpack has a noncanonical checked conversion".into());
        }
        if self.result
            != super::MirValueId::new(format!("expr:{}", self.cast.0))
                .map_err(|error| error.to_string())?
            || self.result_binding
                != super::MirValueId::new(format!("local:{}", self.result_binding_local.0 .0))
                    .map_err(|error| error.to_string())?
        {
            return Err(
                "AnyOwner-v2 Values unpack result/binding is not its direct Checker cast binding"
                    .into(),
            );
        }
        let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        match self.result_owner_settlement {
            crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind::NoOwnedPayload => {
                if self.result_type != i32_type
                    || self.result_owner_settlement_point.is_some()
                    || self.result_owner_settlement_index.is_some()
                    || self.result_owner_settlement_instruction.is_some()
                {
                    return Err(
                        "AnyOwner-v2 Values Copy result has an owner Drop receipt".into(),
                    );
                }
            }
            crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind::CompilerOwnedStringDrop => {
                if self.result_type != string_type
                    || self.result_owner_settlement_point.is_none()
                    || self.result_owner_settlement_index.is_some()
                    || self
                        .result_owner_settlement_instruction
                        .as_ref()
                        .is_none_or(|instruction| instruction.as_str().trim().is_empty())
                {
                    return Err(
                        "AnyOwner-v2 Values String result lacks its exact compiler Drop receipt"
                            .into(),
                    );
                }
            }
            crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind::ExplicitWholeDrop => {
                if self.result_owner_settlement_point.is_none()
                    || self.result_owner_settlement_index.is_none()
                    || self
                        .result_owner_settlement_instruction
                        .as_ref()
                        .is_none_or(|instruction| instruction.as_str().trim().is_empty())
                {
                    return Err(
                        "AnyOwner-v2 Values record result lacks its exact whole-Drop receipt"
                            .into(),
                    );
                }
            }
        }
        Ok(())
    }

    pub fn validate_against_program(&self, program: &MirProgram) -> Result<(), String> {
        self.validate()?;
        let actual = program
            .ordinary_map_owned_any_v2_values_unpack_result_receipts()?
            .into_iter()
            .find(|receipt| receipt.owner == self.owner && receipt.cast == self.cast)
            .ok_or_else(|| {
                "MIR program has no matching AnyOwner-v2 Values unpack result".to_string()
            })?;
        if actual != *self {
            return Err(
                "AnyOwner-v2 Values unpack receipt does not match Checker, MIR, or TypeDesc evidence"
                    .into(),
            );
        }
        Ok(())
    }
}

impl OrdinaryMapOwnedAnyV2PayloadSelectionReceipt {
    /// Deterministic, length-framed text used to compute `selection_digest`.
    /// This is intentionally separate from the general route manifest.
    pub fn canonical_text(&self) -> String {
        let mut text = String::from("ordinary-map-owned-any-v2-payload-selection\n");
        for value in [self.schema, self.profile] {
            push_frame(&mut text, value);
        }
        push_frame(&mut text, &self.mir_digest);
        push_frame(&mut text, &self.type_desc_digest);
        push_frame(&mut text, &self.entries.len().to_string());
        for entry in &self.entries {
            push_frame(&mut text, &entry.owner.0);
            push_frame(&mut text, entry.instruction.as_str());
            push_frame(&mut text, entry.value.as_str());
            push_frame(&mut text, entry.checker_type.as_str());
            push_frame(&mut text, entry.mir_type.as_str());
            push_frame(&mut text, &entry.type_desc_digest);
        }
        push_frame(&mut text, &self.selected_types.len().to_string());
        for selected in &self.selected_types {
            push_frame(&mut text, selected.type_id.as_str());
            push_frame(&mut text, &selected.type_desc_digest);
        }
        text
    }

    /// Check schema, sorted-set closure, and the content digest without
    /// consulting a MIR program.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != ORDINARY_MAP_OWNED_ANY_V2_PAYLOAD_SELECTION_SCHEMA {
            return Err(format!(
                "unexpected AnyOwner-v2 payload selection schema '{}'",
                self.schema
            ));
        }
        if self.profile != ORDINARY_MAP_OWNED_ANY_V2_PROFILE {
            return Err(format!(
                "unexpected AnyOwner-v2 payload selection profile '{}'",
                self.profile
            ));
        }
        for (field, value) in [
            ("mir_digest", self.mir_digest.as_str()),
            ("type_desc_digest", self.type_desc_digest.as_str()),
            ("selection_digest", self.selection_digest.as_str()),
        ] {
            if !is_lower_hex_digest(value) {
                return Err(format!(
                    "AnyOwner-v2 payload selection {field} must be a 64-character lowercase hex digest"
                ));
            }
        }
        if self.entries.is_empty() != self.selected_types.is_empty() {
            return Err(
                "AnyOwner-v2 payload selection must contain both Set entries and selected TypeDescs, or neither".into(),
            );
        }
        if self.entries.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err("AnyOwner-v2 payload selection entries must be strictly sorted".into());
        }
        if self
            .selected_types
            .windows(2)
            .any(|pair| pair[0].type_id >= pair[1].type_id)
        {
            return Err(
                "AnyOwner-v2 selected TypeDescs must be strictly sorted by identity".into(),
            );
        }
        let selected_by_id = self
            .selected_types
            .iter()
            .map(|selected| (selected.type_id.clone(), selected.type_desc_digest.as_str()))
            .collect::<BTreeMap<_, _>>();
        if selected_by_id.len() != self.selected_types.len() {
            return Err("AnyOwner-v2 selected TypeDesc identity is duplicated".into());
        }
        let mut entry_types = std::collections::BTreeSet::new();
        for entry in &self.entries {
            if entry.checker_type != entry.mir_type {
                return Err(format!(
                    "AnyOwner-v2 Set '{}' Checker type '{}' disagrees with MIR value type '{}'",
                    entry.instruction,
                    entry.checker_type.as_str(),
                    entry.mir_type.as_str()
                ));
            }
            if !is_lower_hex_digest(&entry.type_desc_digest) {
                return Err(format!(
                    "AnyOwner-v2 Set '{}' TypeDesc digest is malformed",
                    entry.instruction
                ));
            }
            let Some(selected_digest) = selected_by_id.get(&entry.checker_type) else {
                return Err(format!(
                    "AnyOwner-v2 Set '{}' type '{}' is absent from the selected TypeDesc set",
                    entry.instruction,
                    entry.checker_type.as_str()
                ));
            };
            if *selected_digest != entry.type_desc_digest {
                return Err(format!(
                    "AnyOwner-v2 Set '{}' TypeDesc digest disagrees with selected type '{}'",
                    entry.instruction,
                    entry.checker_type.as_str()
                ));
            }
            entry_types.insert(entry.checker_type.clone());
        }
        if entry_types.len() != self.selected_types.len()
            || self
                .selected_types
                .iter()
                .any(|selected| !entry_types.contains(&selected.type_id))
        {
            return Err(
                "AnyOwner-v2 selected TypeDescs do not exactly match Checker Set types".into(),
            );
        }
        if digest(self.canonical_text()) != self.selection_digest {
            return Err(
                "AnyOwner-v2 payload selection digest does not match its canonical text".into(),
            );
        }
        Ok(())
    }

    /// Recompute this receipt solely from the program's Checker-private V2
    /// action ledger, then compare all MIR and TypeDesc identities.
    pub fn validate_against_program(&self, program: &MirProgram) -> Result<(), String> {
        self.validate()?;
        let actual = program
            .ordinary_map_owned_any_v2_payload_selection_receipt()?
            .ok_or_else(|| "MIR program has no AnyOwner-v2 payload selection".to_string())?;
        if actual != *self {
            return Err(
                "AnyOwner-v2 payload selection receipt does not match Checker actions, MIR, or TypeDescs"
                    .into(),
            );
        }
        Ok(())
    }
}

fn derive_ordinary_map_owned_any_v2_payload_selection_receipt(
    program: &MirProgram,
) -> Result<Option<OrdinaryMapOwnedAnyV2PayloadSelectionReceipt>, String> {
    use crate::core::mir::MirInstructionKind as Instruction;
    use crate::core::OrdinaryMapOwnedAnyV2ActionKind as ActionKind;
    use crate::core::OrdinaryMapOwnedAnyV2ProfileVersion as ProfileVersion;

    let mut entries = Vec::new();
    let mut selected_types = BTreeMap::new();
    let mut observed_sets = BTreeMap::new();
    let mut has_v2_signal = false;

    for function in program.functions().values() {
        for block in function.blocks.values() {
            for instruction in &block.instructions {
                if let Instruction::OrdinaryMapOwnedAnyV2Set {
                    result,
                    source,
                    key,
                    value,
                } = &instruction.kind
                {
                    has_v2_signal = true;
                    if observed_sets
                        .insert(
                            (function.owner.clone(), instruction.id.clone()),
                            (result.clone(), source.clone(), key.clone(), value.clone()),
                        )
                        .is_some()
                    {
                        return Err(format!(
                            "duplicate AnyOwner-v2 Set MIR identity '{}' in '{}'",
                            instruction.id, function.owner.0
                        ));
                    }
                } else if matches!(
                    instruction.kind,
                    Instruction::OrdinaryMapOwnedAnyV2New { .. }
                        | Instruction::OrdinaryMapOwnedAnyV2Remove { .. }
                        | Instruction::OrdinaryMapOwnedAnyV2Size { .. }
                        | Instruction::OrdinaryMapOwnedAnyV2Contains { .. }
                        | Instruction::OrdinaryMapOwnedAnyV2IsEmpty { .. }
                        | Instruction::OrdinaryMapOwnedAnyV2Drop { .. }
                ) {
                    has_v2_signal = true;
                }
            }
        }

        for action in &function.ownership.checker_ordinary_map_owned_any_v2 {
            has_v2_signal = true;
            if action.profile_version != ProfileVersion::V2 {
                return Err(format!(
                    "AnyOwner-v2 action '{}' has unsupported profile version",
                    action.instruction
                ));
            }
            if action.kind != ActionKind::Set {
                continue;
            }
            let value = action.value.as_ref().ok_or_else(|| {
                format!(
                    "AnyOwner-v2 Set action '{}' has no Checker payload value",
                    action.instruction
                )
            })?;
            let checker_type = action.value_type.as_ref().ok_or_else(|| {
                format!(
                    "AnyOwner-v2 Set action '{}' has no Checker payload TypeDesc identity",
                    action.instruction
                )
            })?;
            let key = action.key.as_ref().ok_or_else(|| {
                format!(
                    "AnyOwner-v2 Set action '{}' has no Checker key",
                    action.instruction
                )
            })?;
            let key_type = action.key_type.as_ref().ok_or_else(|| {
                format!(
                    "AnyOwner-v2 Set action '{}' has no Checker key TypeDesc identity",
                    action.instruction
                )
            })?;
            let source_local = action.source_local.as_ref().ok_or_else(|| {
                format!(
                    "AnyOwner-v2 Set action '{}' has no Checker source root",
                    action.instruction
                )
            })?;
            if !action.payload_clone || action.source.is_none() {
                return Err(format!(
                    "AnyOwner-v2 Set action '{}' has incomplete persistent-clone/source evidence",
                    action.instruction
                ));
            }
            let expected_id = format!(
                "inst:ordinary_map_owned_any_v2_set:{}:{}",
                action.local, action.point.0
            );
            if action.instruction.as_str() != expected_id {
                return Err(format!(
                    "AnyOwner-v2 Checker Set action '{}' has a noncanonical identity",
                    action.instruction
                ));
            }
            let (observed_result, observed_source, observed_key, observed_value) = observed_sets
                .get(&(function.owner.clone(), action.instruction.clone()))
                .ok_or_else(|| {
                    format!(
                        "AnyOwner-v2 Checker Set action '{}' has no matching MIR Set",
                        action.instruction
                    )
                })?;
            let expected_source = source_local.clone();
            if action.result.as_ref() != Some(&action.local)
                || observed_result != &action.local
                || observed_source != &expected_source
                || observed_key != key
                || observed_value != value
            {
                return Err(format!(
                    "AnyOwner-v2 Checker Set action '{}' operands disagree with its MIR Set",
                    action.instruction
                ));
            }
            let mir_key_type = function
                .values
                .get(key)
                .map(|mir_value| mir_value.ty.clone())
                .ok_or_else(|| {
                    format!(
                        "AnyOwner-v2 Checker Set action '{}' key '{}' is absent from the MIR value catalog",
                        action.instruction, key
                    )
                })?;
            if &mir_key_type != key_type {
                return Err(format!(
                    "AnyOwner-v2 Checker Set action '{}' key TypeDesc '{}' disagrees with MIR key TypeDesc '{}'",
                    action.instruction,
                    key_type.as_str(),
                    mir_key_type.as_str()
                ));
            }
            if program.type_catalog().get(key_type).is_none() {
                return Err(format!(
                    "AnyOwner-v2 Checker Set action '{}' key TypeDesc '{}' is absent from MIR catalog",
                    action.instruction,
                    key_type.as_str()
                ));
            }
            let mir_type = function
                .values
                .get(value)
                .map(|mir_value| mir_value.ty.clone())
                .ok_or_else(|| {
                    format!(
                        "AnyOwner-v2 Checker Set action '{}' payload '{}' is absent from the MIR value catalog",
                        action.instruction, value
                    )
                })?;
            if &mir_type != checker_type {
                return Err(format!(
                    "AnyOwner-v2 Checker Set action '{}' TypeDesc '{}' disagrees with MIR value TypeDesc '{}'",
                    action.instruction,
                    checker_type.as_str(),
                    mir_type.as_str()
                ));
            }
            let descriptor = program.type_catalog().get(checker_type).ok_or_else(|| {
                format!(
                    "AnyOwner-v2 Checker Set action '{}' TypeDesc '{}' is absent from MIR catalog",
                    action.instruction,
                    checker_type.as_str()
                )
            })?;
            if !super::reference::is_canonical_v2_payload_type(program.type_catalog(), checker_type)
            {
                return Err(format!(
                    "AnyOwner-v2 Checker Set action '{}' selects a payload outside the canonical i32/String/Label profile",
                    action.instruction
                ));
            }
            let descriptor_digest = digest(descriptor.canonical_text());
            selected_types.insert(
                checker_type.clone(),
                OrdinaryMapOwnedAnyV2SelectedType {
                    type_id: checker_type.clone(),
                    type_desc_digest: descriptor_digest.clone(),
                },
            );
            entries.push(OrdinaryMapOwnedAnyV2PayloadSelectionEntry {
                owner: function.owner.clone(),
                instruction: action.instruction.clone(),
                value: value.clone(),
                checker_type: checker_type.clone(),
                mir_type,
                type_desc_digest: descriptor_digest,
            });
        }
    }

    if !has_v2_signal {
        return Ok(None);
    }
    if observed_sets.len() != entries.len() {
        return Err(format!(
            "AnyOwner-v2 MIR Set count {} disagrees with Checker payload selection count {}",
            observed_sets.len(),
            entries.len()
        ));
    }
    entries.sort();
    let receipt = OrdinaryMapOwnedAnyV2PayloadSelectionReceipt {
        schema: ORDINARY_MAP_OWNED_ANY_V2_PAYLOAD_SELECTION_SCHEMA,
        profile: ORDINARY_MAP_OWNED_ANY_V2_PROFILE,
        mir_digest: program.canonical_digest(),
        type_desc_digest: digest(program.type_catalog().canonical_text()),
        entries,
        selected_types: selected_types.into_values().collect(),
        selection_digest: String::new(),
    };
    let mut receipt = receipt;
    receipt.selection_digest = digest(receipt.canonical_text());
    receipt.validate()?;
    Ok(Some(receipt))
}

fn derive_ordinary_map_owned_any_v2_temporary_root_mir_receipt(
    function: &super::MirFunction,
    cfg: &crate::core::OrdinaryMapOwnedAnyV2CfgReceipt,
    local: &crate::core::ir::ResolvedLocalId,
    operation: &NodeId,
    source_node: &NodeId,
    source_type: &crate::core::ir::ResolvedTypeId,
    root: &super::MirValueId,
    get_any_instruction: &super::MirInstructionId,
) -> Result<Option<OrdinaryMapOwnedAnyV2TemporaryRootMirReceipt>, String> {
    use super::MirInstructionKind as Instruction;
    use crate::core::OrdinaryMapOwnedAnyV2ActionKind as ActionKind;
    use crate::core::OrdinaryMapOwnedAnyV2RootSettlementKind as RootSettlement;
    use crate::core::OrdinaryMapOwnedAnyV2RootSettlementKind::ImmediateGetAnyDrop;

    let synthetic_identity = local
        .0
         .0
        .starts_with("ordinary-map-owned-any-v2:temporary-root:");
    let matching_receipts = cfg
        .temporary_root_receipts
        .iter()
        .filter(|receipt| receipt.local == *local && receipt.get_any == *operation)
        .collect::<Vec<_>>();
    if matching_receipts.is_empty() && !synthetic_identity {
        return Ok(None);
    }
    let [checker_receipt] = matching_receipts.as_slice() else {
        return Err(format!(
            "AnyOwner-v2 GetAny '{}' has a missing or duplicate temporary-root Checker receipt",
            operation.0
        ));
    };
    let expected_root = super::MirValueId::new(format!("local:{}", local.0 .0))
        .map_err(|error| error.to_string())?;
    if cfg.owner != function.owner
        || cfg.profile_version != crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
        || checker_receipt.profile_version != crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
        || checker_receipt.local != *local
        || checker_receipt.root
            != crate::core::ResourceId(crate::core::NodeId(format!(
                "ordinary-map-owned-any-v2:{}",
                local.0 .0
            )))
        || checker_receipt.construction != *source_node
        || checker_receipt.source_node != *source_node
        || checker_receipt.root_type != *source_type
        || checker_receipt.source_type != *source_type
        || checker_receipt.get_any != *operation
        || checker_receipt.settlement_point != *operation
        || checker_receipt.settlement != ImmediateGetAnyDrop
        || checker_receipt.root_type != *source_type
        || checker_receipt.local.0 .0
            != format!(
                "ordinary-map-owned-any-v2:temporary-root:{}",
                checker_receipt.construction.0
            )
        || *root != expected_root
    {
        return Err(format!(
            "AnyOwner-v2 GetAny '{}' temporary-root Checker identity/type/source/settlement disagrees",
            operation.0
        ));
    }

    let new_actions = cfg
        .actions
        .iter()
        .filter(|action| {
            action.kind == ActionKind::New
                && action.local == *local
                && action.point == checker_receipt.construction
                && action.source_local.is_none()
                && action.source.is_none()
        })
        .collect::<Vec<_>>();
    let get_any_actions = cfg
        .actions
        .iter()
        .filter(|action| {
            action.kind == ActionKind::GetAny
                && action.local == *local
                && action.point == *operation
                && action.source_local.as_ref() == Some(local)
                && action.source_node.as_ref() == Some(source_node)
                && action.source_type.as_ref() == Some(source_type)
        })
        .count();
    let drop_actions = cfg
        .actions
        .iter()
        .filter(|action| {
            action.kind == ActionKind::Drop
                && action.local == *local
                && action.point == checker_receipt.settlement_point
        })
        .collect::<Vec<_>>();
    if new_actions.len() != 1 || get_any_actions != 1 || drop_actions.len() != 1 {
        return Err(format!(
            "AnyOwner-v2 GetAny '{}' temporary root lacks unique Checker New/GetAny/Drop actions",
            operation.0
        ));
    }
    let new_attempts = cfg
        .attempts
        .iter()
        .filter(|attempt| {
            attempt.kind == crate::core::OrdinaryMapOwnedAnyV2AttemptKind::New
                && attempt.point == checker_receipt.construction
                && attempt.arguments.is_empty()
                && attempt.receiver_local.is_none()
                && attempt.result_local.as_ref() == Some(local)
        })
        .count();
    let drop_attempts = cfg
        .attempts
        .iter()
        .filter(|attempt| {
            attempt.kind == crate::core::OrdinaryMapOwnedAnyV2AttemptKind::Drop
                && attempt.point == checker_receipt.settlement_point
                && attempt.arguments == vec![local.0.clone()]
                && attempt.receiver_local.as_ref() == Some(local)
                && attempt.result_local.is_none()
        })
        .count();
    if new_attempts != 1 || drop_attempts != 1 {
        return Err(format!(
            "AnyOwner-v2 GetAny '{}' temporary root New/Drop attempt census disagrees",
            operation.0
        ));
    }

    let new_instruction = super::MirInstructionId::new(format!(
        "inst:ordinary_map_owned_any_v2_new:{expected_root}:{}",
        checker_receipt.construction.0
    ))
    .map_err(|error| error.to_string())?;
    let drop_instruction = super::MirInstructionId::new(format!(
        "inst:ordinary_map_owned_any_v2_drop:{expected_root}:{}",
        checker_receipt.settlement_point.0
    ))
    .map_err(|error| error.to_string())?;
    let checker_new_mir = function
        .ownership
        .checker_ordinary_map_owned_any_v2
        .iter()
        .filter(|action| {
            action.kind == ActionKind::New
                && action.point == checker_receipt.construction
                && action.local == expected_root
                && action.instruction == new_instruction
        })
        .count();
    let checker_drop_mir = function
        .ownership
        .checker_ordinary_map_owned_any_v2
        .iter()
        .filter(|action| {
            action.kind == ActionKind::Drop
                && action.point == checker_receipt.settlement_point
                && action.local == expected_root
                && action.instruction == drop_instruction
        })
        .count();
    if checker_new_mir != 1 || checker_drop_mir != 1 {
        return Err(format!(
            "AnyOwner-v2 GetAny '{}' temporary root lacks its exact MIR Checker New/Drop projection",
            operation.0
        ));
    }

    let mut new_sites = Vec::new();
    let mut get_any_sites = Vec::new();
    let mut drop_sites = Vec::new();
    for (block_id, block) in &function.blocks {
        for (index, instruction) in block.instructions.iter().enumerate() {
            match &instruction.kind {
                Instruction::OrdinaryMapOwnedAnyV2New { result }
                    if instruction.id == new_instruction && result == root =>
                {
                    new_sites.push((block_id, index));
                }
                Instruction::OrdinaryMapOwnedAnyV2GetAny { root: actual, .. }
                    if instruction.id == *get_any_instruction && actual == root =>
                {
                    get_any_sites.push((block_id, index));
                }
                Instruction::OrdinaryMapOwnedAnyV2Drop { root: actual }
                    if instruction.id == drop_instruction && actual == root =>
                {
                    drop_sites.push((block_id, index));
                }
                _ => {}
            }
        }
    }
    let [(new_block, new_index)] = new_sites.as_slice() else {
        return Err(format!(
            "AnyOwner-v2 GetAny '{}' temporary root has no unique exact MIR New",
            operation.0
        ));
    };
    let [(get_any_block, get_any_index)] = get_any_sites.as_slice() else {
        return Err(format!(
            "AnyOwner-v2 GetAny '{}' temporary root does not bind its exact MIR GetAny",
            operation.0
        ));
    };
    let [(drop_block, drop_index)] = drop_sites.as_slice() else {
        return Err(format!(
            "AnyOwner-v2 GetAny '{}' temporary root has no unique exact MIR Drop",
            operation.0
        ));
    };
    if new_block != get_any_block
        || get_any_block != drop_block
        || new_index >= get_any_index
        || *drop_index != *get_any_index + 1
        || function.values.get(root).map(|value| &value.ty)
            != Some(&super::types::ordinary_map_owned_any_v2_type_id())
    {
        return Err(format!(
            "AnyOwner-v2 GetAny '{}' temporary root MIR instructions are not New, GetAny, immediate Drop in one block",
            operation.0
        ));
    }
    let root_uses = function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| super::mir_instruction_value_uses(&instruction.kind).contains(root))
        .collect::<Vec<_>>();
    if root_uses.len() != 2
        || !root_uses
            .iter()
            .any(|instruction| instruction.id == *get_any_instruction)
        || !root_uses
            .iter()
            .any(|instruction| instruction.id == drop_instruction)
        || function
            .blocks
            .values()
            .any(|block| super::mir_terminator_value_uses(&block.terminator).contains(root))
    {
        return Err(format!(
            "AnyOwner-v2 GetAny '{}' temporary root has an alias, escape, or extra use",
            operation.0
        ));
    }

    Ok(Some(OrdinaryMapOwnedAnyV2TemporaryRootMirReceipt {
        local: local.clone(),
        root: root.clone(),
        construction: checker_receipt.construction.clone(),
        root_type: checker_receipt.root_type.clone(),
        get_any: checker_receipt.get_any.clone(),
        source_node: checker_receipt.source_node.clone(),
        source_type: checker_receipt.source_type.clone(),
        settlement: RootSettlement::ImmediateGetAnyDrop,
        settlement_point: checker_receipt.settlement_point.clone(),
        new_instruction,
        get_any_instruction: get_any_instruction.clone(),
        drop_instruction,
    }))
}

fn derive_ordinary_map_owned_any_v2_get_any_result_receipts(
    program: &MirProgram,
) -> Result<Vec<OrdinaryMapOwnedAnyV2GetAnyResultReceipt>, String> {
    use crate::core::mir::MirInstructionKind as Instruction;
    use crate::core::OrdinaryMapOwnedAnyV2ActionKind as ActionKind;
    use crate::core::OrdinaryMapOwnedAnyV2ProfileVersion as ProfileVersion;
    use crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind as Settlement;

    let has_get_any_signal = program.functions().values().any(|function| {
        function
            .ownership
            .checker_ordinary_map_owned_any_v2
            .iter()
            .any(|action| action.kind == ActionKind::GetAny)
            || function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        Instruction::OrdinaryMapOwnedAnyV2GetAny { .. }
                    )
                })
            })
    });
    if !has_get_any_signal {
        return Ok(Vec::new());
    }

    let payload_selection = program
        .ordinary_map_owned_any_v2_payload_selection_receipt()?
        .ok_or_else(|| {
            "AnyOwner-v2 GetAny has no Checker-owned payload-selection receipt".to_string()
        })?;
    payload_selection.validate_against_program(program)?;

    let catalog = program.type_catalog();
    let miss_payload_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    let miss_descriptor = catalog
        .get(&miss_payload_type)
        .ok_or_else(|| "AnyOwner-v2 GetAny canonical i32 miss TypeDesc is absent".to_string())?;
    if miss_descriptor.kind != super::types::MirTypeKind::Primitive(crate::core::PrimitiveType::I32)
        || miss_descriptor.layout != super::types::MirLayout::Scalar
        || miss_descriptor.ownership != super::types::MirOwnership::Copy
        || catalog.validate_copy_scalar(&miss_payload_type).is_err()
    {
        return Err(
            "AnyOwner-v2 GetAny miss payload is not the canonical Copy i32 scalar TypeDesc/glue"
                .into(),
        );
    }
    let miss_payload_type_desc_digest = digest(miss_descriptor.canonical_text());
    let expected_root_type = super::types::ordinary_map_owned_any_v2_type_id();
    let expected_payload_type = super::types::ordinary_map_owned_any_v2_payload_type_id();
    let expected_result_type = super::types::ordinary_map_owned_any_v2_get_result_type_id();
    for operation in [
        super::types::MirGlueOperation::MoveOut,
        super::types::MirGlueOperation::Clone,
        super::types::MirGlueOperation::Drop,
    ] {
        catalog.validate_glue(&expected_payload_type, operation)?;
        catalog.validate_glue(&expected_result_type, operation)?;
    }

    let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
    let bool_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Bool);
    let mut receipts = Vec::new();
    let mut observed_get_any = BTreeSet::new();
    let mut observed_read_receipts = BTreeSet::new();
    let mut observed_temporary_roots = BTreeSet::new();

    for function in program.functions().values() {
        let function_get_any = function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| match &instruction.kind {
                Instruction::OrdinaryMapOwnedAnyV2GetAny { result, root, key } => Some((
                    instruction.id.clone(),
                    result.clone(),
                    root.clone(),
                    key.clone(),
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        let checker_get_any = function
            .ownership
            .checker_ordinary_map_owned_any_v2
            .iter()
            .filter(|action| action.kind == ActionKind::GetAny)
            .collect::<Vec<_>>();
        if checker_get_any.is_empty() && function_get_any.is_empty() {
            continue;
        }
        if checker_get_any.len() != function_get_any.len() {
            return Err(format!(
                "AnyOwner-v2 GetAny action/instruction count differs in '{}'",
                function.owner.0
            ));
        }
        let cfg = function
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .ok_or_else(|| {
                format!(
                    "AnyOwner-v2 GetAny in '{}' has no private Checker CFG receipt",
                    function.owner.0
                )
            })?;
        if cfg.owner != function.owner || cfg.profile_version != ProfileVersion::V2 {
            return Err(format!(
                "AnyOwner-v2 GetAny CFG owner/profile disagrees in '{}'",
                function.owner.0
            ));
        }
        let cfg_get_any_actions = cfg
            .actions
            .iter()
            .filter(|action| action.kind == ActionKind::GetAny)
            .collect::<Vec<_>>();
        let cfg_get_any_results = cfg
            .result_receipts
            .iter()
            .filter(|result| result.kind == ActionKind::GetAny)
            .count();
        if cfg_get_any_actions.len() != checker_get_any.len()
            || cfg_get_any_results != checker_get_any.len()
        {
            return Err(format!(
                "AnyOwner-v2 GetAny action/result receipt census disagrees in '{}'",
                function.owner.0
            ));
        }
        for action in checker_get_any {
            if action.profile_version != ProfileVersion::V2 {
                return Err(format!(
                    "AnyOwner-v2 GetAny action '{}' has an unsupported profile",
                    action.instruction
                ));
            }
            let matching_cfg_actions = cfg_get_any_actions
                .iter()
                .filter(|checker_action| {
                    checker_action.point == action.point
                        && checker_action.root.0 .0 == action.root
                        && checker_action.source_local.as_ref() == Some(&checker_action.local)
                        && checker_action
                            .source
                            .as_ref()
                            .map(|resource| &resource.0 .0)
                            == Some(&action.root)
                        && checker_action.source_node == action.source_node
                        && checker_action.source_type == action.source_type
                        && checker_action.key.as_ref() == action.key_node.as_ref()
                        && checker_action.key_local == action.key_local
                        && action.key.as_ref()
                            == checker_action
                                .key_local
                                .as_ref()
                                .map(|local| {
                                    super::MirValueId::new(format!("local:{}", local.0 .0)).ok()
                                })
                                .flatten()
                                .or_else(|| {
                                    checker_action.key.as_ref().and_then(|node| {
                                        super::MirValueId::new(format!("expr:{}", node.0)).ok()
                                    })
                                })
                                .as_ref()
                        && checker_action.key_type == action.key_type
                        && checker_action.result_type == action.result_source_type
                        && checker_action.result_local == action.result_local
                        && checker_action.payload_clone == action.payload_clone
                })
                .collect::<Vec<_>>();
            if matching_cfg_actions.len() != 1 {
                return Err(format!(
                    "AnyOwner-v2 GetAny MIR action '{}' does not map to exactly one Checker action",
                    action.instruction
                ));
            }
            let checker_action = matching_cfg_actions[0];
            // Walk the exact persistent lineage to New. A lineage with no
            // Set is still useful evidence: New starts empty and Remove
            // cannot add entries, so GetAny is a certified miss-only query
            // whose payload is the separately receipted canonical i32 zero.
            // A Set in another callable or on an unrelated root never lends
            // payload authority to this root.
            // MIR ownership actions use MIR value identities, while the
            // private CFG receipt uses resolved-local identities. Join them
            // through the exact source node/root/type fields above, then keep
            // the lineage walk entirely in the MIR action identity domain.
            let mut root_local = action.local.clone();
            let mut visited_root_locals = BTreeSet::new();
            loop {
                if !visited_root_locals.insert(root_local.clone()) {
                    return Err(format!(
                        "AnyOwner-v2 GetAny '{}' source root lineage contains a cycle",
                        action.instruction
                    ));
                }
                let root_actions = function
                    .ownership
                    .checker_ordinary_map_owned_any_v2
                    .iter()
                    .filter(|candidate| {
                        candidate.local == root_local
                            && matches!(
                                candidate.kind,
                                ActionKind::New
                                    | ActionKind::Clone
                                    | ActionKind::Set
                                    | ActionKind::Remove
                            )
                    })
                    .collect::<Vec<_>>();
                let [root_action] = root_actions.as_slice() else {
                    return Err(format!(
                        "AnyOwner-v2 GetAny '{}' source root has no unique Checker construction lineage",
                        action.instruction
                    ));
                };
                match root_action.kind {
                    ActionKind::New => {
                        if root_action.source_local.is_some() || root_action.source.is_some() {
                            return Err(format!(
                                "AnyOwner-v2 GetAny '{}' source root New has unexpected source evidence",
                                action.instruction
                            ));
                        }
                        break;
                    }
                    ActionKind::Set | ActionKind::Remove => {
                        if root_action.kind == ActionKind::Set {
                            let selected_set_entries = payload_selection
                                .entries
                                .iter()
                                .filter(|entry| {
                                    entry.owner == function.owner
                                        && entry.instruction == root_action.instruction
                                })
                                .count();
                            if selected_set_entries != 1 {
                                return Err(format!(
                                    "AnyOwner-v2 GetAny '{}' source root Set is not independently payload-selected",
                                    action.instruction
                                ));
                            }
                        }
                        root_local = root_action.source_local.clone().ok_or_else(|| {
                            format!(
                                "AnyOwner-v2 GetAny '{}' source root update has no predecessor local",
                                action.instruction
                            )
                        })?;
                    }
                    ActionKind::Clone => {
                        root_local = root_action.source_local.clone().ok_or_else(|| {
                            format!(
                                "AnyOwner-v2 GetAny '{}' Clone root has no source local",
                                action.instruction
                            )
                        })?;
                    }
                    _ => unreachable!("the source-root action filter is exhaustive"),
                }
            }
            let result_receipts = cfg
                .result_receipts
                .iter()
                .filter(|result| result.operation == action.point)
                .collect::<Vec<_>>();
            let [checker_result] = result_receipts.as_slice() else {
                return Err(format!(
                    "AnyOwner-v2 GetAny action '{}' has no unique result settlement receipt",
                    action.instruction
                ));
            };
            let source_node = checker_action.source_node.as_ref().ok_or_else(|| {
                format!(
                    "AnyOwner-v2 GetAny '{}' has no Checker source node",
                    action.instruction
                )
            })?;
            let source_type = checker_action.source_type.as_ref().ok_or_else(|| {
                format!(
                    "AnyOwner-v2 GetAny '{}' has no Checker source type",
                    action.instruction
                )
            })?;
            let key_node = checker_action.key.as_ref().ok_or_else(|| {
                format!(
                    "AnyOwner-v2 GetAny '{}' has no Checker key node",
                    action.instruction
                )
            })?;
            let checker_key_value = checker_action
                .key_local
                .as_ref()
                .map(|local| super::MirValueId::new(format!("local:{}", local.0 .0)))
                .transpose()
                .map_err(|error| error.to_string())?
                .or_else(|| super::MirValueId::new(format!("expr:{}", key_node.0)).ok())
                .ok_or_else(|| "AnyOwner-v2 GetAny key has no MIR identity".to_string())?;
            let key_type = checker_action.key_type.as_ref().ok_or_else(|| {
                format!(
                    "AnyOwner-v2 GetAny '{}' has no Checker key type",
                    action.instruction
                )
            })?;
            let checker_result_type = checker_action.result_type.as_ref().ok_or_else(|| {
                format!(
                    "AnyOwner-v2 GetAny '{}' has no Checker result type",
                    action.instruction
                )
            })?;
            let binding_local = checker_action.result_local.as_ref().ok_or_else(|| {
                format!(
                    "AnyOwner-v2 GetAny '{}' has no direct binding",
                    action.instruction
                )
            })?;
            if checker_result.profile_version != ProfileVersion::V2
                || checker_result.kind != ActionKind::GetAny
                || checker_result.operation != action.point
                || checker_result.source_local != checker_action.local
                || checker_result.source != checker_action.root
                || checker_result.source_node != *source_node
                || checker_result.source_type != *source_type
                || checker_result.key.as_ref() != Some(key_node)
                || checker_result.key_type.as_ref() != Some(key_type)
                || checker_result.key_local != checker_action.key_local
                || checker_result.result != action.point
                || checker_result.result_type != *checker_result_type
                || checker_result.binding != *binding_local
                || checker_result.binding_point.0.trim().is_empty()
                || checker_result.settlement != Settlement::ExplicitDrop
                || checker_result.settlement_point.is_none()
            {
                return Err(format!(
                    "AnyOwner-v2 GetAny action '{}' disagrees with its independent result receipt",
                    action.instruction
                ));
            }
            if checker_action.source_local.as_ref() != Some(&checker_action.local)
                || checker_action
                    .source
                    .as_ref()
                    .map(|resource| &resource.0 .0)
                    != Some(&checker_action.root.0 .0)
                || checker_action.key_type.as_ref() != Some(&string_type)
                || checker_action.key.as_ref() != Some(key_node)
                || action.source_node.as_ref() != Some(source_node)
                || action.source_type.as_ref() != Some(source_type)
                || action.key.as_ref() != Some(&checker_key_value)
                || action.key_local != checker_action.key_local
                || action.result_source_type.as_ref() != Some(checker_result_type)
                || action.result_local.as_ref() != Some(binding_local)
            {
                return Err(format!(
                    "AnyOwner-v2 GetAny action '{}' has incomplete source/key/result bindings",
                    action.instruction
                ));
            }
            let matching_attempts = cfg
                .attempts
                .iter()
                .filter(|attempt| {
                    attempt.kind == crate::core::OrdinaryMapOwnedAnyV2AttemptKind::GetAny
                        && attempt.point == action.point
                        && attempt.receiver_local.as_ref() == Some(&checker_action.local)
                        && attempt.result_local.as_ref() == Some(binding_local)
                        && attempt.arguments == vec![source_node.clone(), key_node.clone()]
                })
                .count();
            if matching_attempts != 1 {
                return Err(format!(
                    "AnyOwner-v2 GetAny action '{}' has no unique matching source attempt",
                    action.instruction
                ));
            }

            let expected_instruction = format!(
                "inst:ordinary_map_owned_any_v2_get_any:{}:{}",
                action.local, action.point.0
            );
            if action.instruction.as_str() != expected_instruction {
                return Err(format!(
                    "AnyOwner-v2 GetAny action '{}' has a noncanonical instruction identity",
                    action.instruction
                ));
            }
            let root = super::MirValueId::new(format!("local:{}", checker_action.local.0 .0))
                .map_err(|error| error.to_string())?;
            let temporary_root = derive_ordinary_map_owned_any_v2_temporary_root_mir_receipt(
                function,
                cfg,
                &checker_action.local,
                &action.point,
                source_node,
                source_type,
                &root,
                &action.instruction,
            )?;
            if let Some(temporary_root) = &temporary_root {
                observed_temporary_roots.insert((
                    function.owner.clone(),
                    temporary_root.local.clone(),
                    temporary_root.get_any.clone(),
                ));
            }
            let result = super::MirValueId::new(format!("expr:{}", action.point.0))
                .map_err(|error| error.to_string())?;
            let binding = super::MirValueId::new(format!("local:{}", binding_local.0 .0))
                .map_err(|error| error.to_string())?;
            let matching_instructions = function_get_any
                .iter()
                .filter(|(id, mir_result, mir_root, _)| {
                    id == &action.instruction && mir_result == &result && mir_root == &root
                })
                .collect::<Vec<_>>();
            let [(instruction, _, _, key)] = matching_instructions.as_slice() else {
                return Err(format!(
                    "AnyOwner-v2 GetAny Checker action '{}' has no exact MIR result/root instruction",
                    action.instruction
                ));
            };
            if !observed_get_any.insert((function.owner.clone(), (*instruction).clone())) {
                return Err(format!(
                    "AnyOwner-v2 GetAny instruction '{}' is duplicated",
                    instruction
                ));
            }
            let source_value_type = function
                .values
                .get(&root)
                .map(|value| &value.ty)
                .ok_or_else(|| format!("AnyOwner-v2 GetAny root '{}' is absent", root))?;
            let key_value_type = function
                .values
                .get(key)
                .map(|value| &value.ty)
                .ok_or_else(|| format!("AnyOwner-v2 GetAny key '{}' is absent", key))?;
            let result_value_type = function
                .values
                .get(&result)
                .map(|value| &value.ty)
                .ok_or_else(|| format!("AnyOwner-v2 GetAny result '{}' is absent", result))?;
            let checker_result_descriptor = catalog.get(checker_result_type).ok_or_else(|| {
                format!(
                    "AnyOwner-v2 GetAny Checker result TypeDesc '{}' is absent",
                    checker_result_type.as_str()
                )
            })?;
            let source_result_children = match &checker_result_descriptor.layout {
                super::types::MirLayout::Tuple(children) if children.len() == 2 => children,
                _ => {
                    return Err(format!(
                        "AnyOwner-v2 GetAny Checker result '{}' is not a tuple TypeDesc",
                        checker_result_type.as_str()
                    ));
                }
            };
            if source_result_children[0] != bool_type
                || catalog
                    .get(&source_result_children[1])
                    .is_none_or(|descriptor| {
                        descriptor.kind != super::types::MirTypeKind::DynamicAny
                    })
            {
                return Err(format!(
                    "AnyOwner-v2 GetAny Checker result '{}' is not exact (bool, DynamicAny)",
                    checker_result_type.as_str()
                ));
            }
            if *key != checker_key_value
                || action.result.as_ref() != Some(&result)
                || catalog.get(source_type).is_none()
                || source_value_type != &expected_root_type
                || key_type != &string_type
                || key_value_type != key_type
                || result_value_type != &expected_result_type
                || action.result_source_type.as_ref() != Some(checker_result_type)
            {
                return Err(format!(
                    "AnyOwner-v2 GetAny instruction '{}' source/key/result TypeDescs disagree",
                    action.instruction
                ));
            }
            for operation in [
                super::types::MirGlueOperation::MoveOut,
                super::types::MirGlueOperation::Clone,
                super::types::MirGlueOperation::Drop,
            ] {
                catalog.validate_glue(source_value_type, operation)?;
            }

            let moves = function
                .blocks
                .values()
                .flat_map(|block| &block.instructions)
                .filter(|instruction| {
                    matches!(
                        &instruction.kind,
                        Instruction::Move { result: destination, source }
                            if destination == &binding && source == &result
                    )
                })
                .collect::<Vec<_>>();
            if moves.len() != 1
                || function.values.get(&binding).map(|value| &value.ty)
                    != Some(&expected_result_type)
            {
                return Err(format!(
                    "AnyOwner-v2 GetAny result '{}' is not transferred by one direct binding Move",
                    result
                ));
            }

            let settlement_point = checker_result.settlement_point.as_ref().ok_or_else(|| {
                format!(
                    "AnyOwner-v2 GetAny '{}' has no settlement point",
                    action.point.0
                )
            })?;
            let settlement_instruction =
                super::MirInstructionId::new(format!("inst:drop.0:{}", settlement_point.0))
                    .map_err(|error| error.to_string())?;
            let drops = function
                .blocks
                .values()
                .flat_map(|block| &block.instructions)
                .filter(|instruction| {
                    instruction.id == settlement_instruction
                        && matches!(&instruction.kind, Instruction::Drop { value } if value == &binding)
                })
                .collect::<Vec<_>>();
            if drops.len() != 1 {
                return Err(format!(
                    "AnyOwner-v2 GetAny binding '{}' lacks its exact explicit Drop instruction",
                    binding
                ));
            }

            let uses_of_result = function
                .blocks
                .values()
                .flat_map(|block| &block.instructions)
                .filter(|instruction| {
                    super::mir_instruction_value_uses(&instruction.kind).contains(&result)
                })
                .collect::<Vec<_>>();
            if uses_of_result.len() != 1 || uses_of_result[0].id != moves[0].id {
                return Err(format!(
                    "AnyOwner-v2 GetAny result '{}' escapes its direct binding Move",
                    result
                ));
            }
            for instruction in function
                .blocks
                .values()
                .flat_map(|block| &block.instructions)
                .filter(|instruction| {
                    super::mir_instruction_value_uses(&instruction.kind).contains(&binding)
                })
            {
                let allowed_observation = matches!(
                    &instruction.kind,
                    Instruction::Project {
                        base,
                        projection: super::MirProjection::Tuple(0),
                        ..
                    } if base == &binding
                );
                let allowed_checked_unpack = match &instruction.kind {
                    Instruction::OrdinaryMapOwnedAnyV2Unpack {
                        pair: observed_pair,
                        projection,
                        ..
                    } if observed_pair == &binding => {
                        let matches = cfg
                            .unpack_receipts
                            .iter()
                            .filter(|unpack| {
                                unpack.profile_version == ProfileVersion::V2
                                    && unpack.operation == action.point
                                    && unpack.pair_binding == checker_result.binding
                                    && unpack.pair_binding_point == checker_result.binding_point
                                    && unpack.projection == *projection
                                    && unpack.settlement == Settlement::ExplicitDrop
                                    && &unpack.settlement_point == settlement_point
                                    && instruction.id.as_str()
                                        == format!(
                                            "inst:ordinary_map_owned_any_v2_unpack:{}",
                                            unpack.cast.0
                                        )
                            })
                            .count();
                        matches == 1
                    }
                    _ => false,
                };
                let exact_drop = instruction.id == settlement_instruction
                    && matches!(&instruction.kind, Instruction::Drop { value } if value == &binding);
                if !allowed_observation && !allowed_checked_unpack && !exact_drop {
                    return Err(format!(
                        "AnyOwner-v2 GetAny binding '{}' has a non-observation/non-drop use at '{}'",
                        binding, instruction.id
                    ));
                }
            }
            if function
                .blocks
                .values()
                .any(|block| super::mir_terminator_value_uses(&block.terminator).contains(&binding))
            {
                return Err(format!(
                    "AnyOwner-v2 GetAny binding '{}' escapes through a terminator",
                    binding
                ));
            }

            let receipt = OrdinaryMapOwnedAnyV2GetAnyResultReceipt {
                schema: ORDINARY_MAP_OWNED_ANY_V2_GET_ANY_RESULT_SCHEMA,
                profile: ORDINARY_MAP_OWNED_ANY_V2_PROFILE,
                owner: function.owner.clone(),
                operation: action.point.clone(),
                instruction: action.instruction.clone(),
                source_node: source_node.clone(),
                source_local: checker_action.local.clone(),
                root,
                checker_source_type: source_type.clone(),
                key_node: key_node.clone(),
                key: (*key).clone(),
                checker_key_type: key_type.clone(),
                checker_key_local: checker_action.key_local.clone(),
                checker_result_type: checker_result_type.clone(),
                result,
                mir_result_type: expected_result_type.clone(),
                binding_local: binding_local.clone(),
                binding,
                binding_point: checker_result.binding_point.clone(),
                settlement: Settlement::ExplicitDrop,
                settlement_point: settlement_point.clone(),
                settlement_instruction,
                temporary_root,
                miss_payload_type: miss_payload_type.clone(),
                miss_payload_type_desc_digest: miss_payload_type_desc_digest.clone(),
            };
            receipt.validate()?;
            observed_read_receipts.insert((function.owner.clone(), action.point.clone()));
            receipts.push(receipt);
        }
    }

    let expected_temporary_roots = program
        .functions()
        .values()
        .flat_map(|function| {
            function
                .ownership
                .checker_ordinary_map_owned_any_v2_cfg_receipt
                .iter()
                .flat_map(move |cfg| {
                    cfg.temporary_root_receipts.iter().map(move |receipt| {
                        (
                            function.owner.clone(),
                            receipt.local.clone(),
                            receipt.get_any.clone(),
                        )
                    })
                })
        })
        .collect::<BTreeSet<_>>();
    if expected_temporary_roots != observed_temporary_roots {
        return Err("AnyOwner-v2 temporary-root Checker receipt census has missing, extra, or duplicate MIR bindings".into());
    }

    let observed_instruction_count = program
        .functions()
        .values()
        .flat_map(|function| {
            function.blocks.values().flat_map(move |block| {
                block
                    .instructions
                    .iter()
                    .map(move |instruction| (function, instruction))
            })
        })
        .filter(|(_, instruction)| {
            matches!(
                instruction.kind,
                Instruction::OrdinaryMapOwnedAnyV2GetAny { .. }
            )
        })
        .count();
    if observed_instruction_count != observed_get_any.len() {
        return Err(
            "AnyOwner-v2 GetAny instruction census contains unreceipted or duplicate MIR".into(),
        );
    }
    let observed_checker_results = program
        .functions()
        .values()
        .flat_map(|function| {
            function
                .ownership
                .checker_ordinary_map_owned_any_v2_cfg_receipt
                .iter()
                .flat_map(move |cfg| {
                    cfg.result_receipts
                        .iter()
                        .filter(move |result| result.kind == ActionKind::GetAny)
                        .map(move |result| (function.owner.clone(), result.operation.clone()))
                })
        })
        .collect::<BTreeSet<_>>();
    if observed_checker_results != observed_read_receipts {
        return Err(
            "AnyOwner-v2 GetAny result receipt census has missing or extra Checker receipts".into(),
        );
    }
    receipts.sort_by(|left, right| {
        (&left.owner, &left.operation).cmp(&(&right.owner, &right.operation))
    });
    Ok(receipts)
}

/// Revalidate that a payload TypeDesc was selected by a Set on the exact
/// persistent root lineage consumed by this GetAny. Program-wide selection
/// alone must not let an unrelated Map authorize a forged cast.
fn ordinary_map_owned_any_v2_root_selects_payload_type(
    function: &super::MirFunction,
    root: &crate::core::ir::ResolvedLocalId,
    ty: &crate::core::ir::ResolvedTypeId,
    selection: &OrdinaryMapOwnedAnyV2PayloadSelectionReceipt,
) -> bool {
    use crate::core::OrdinaryMapOwnedAnyV2ActionKind as ActionKind;

    let Ok(mut current) = super::MirValueId::new(format!("local:{}", root.0 .0)) else {
        return false;
    };
    let mut seen = BTreeSet::new();
    loop {
        if !seen.insert(current.clone()) {
            return false;
        }
        let root_actions = function
            .ownership
            .checker_ordinary_map_owned_any_v2
            .iter()
            .filter(|action| {
                action.local == current
                    && matches!(
                        action.kind,
                        ActionKind::New | ActionKind::Clone | ActionKind::Set | ActionKind::Remove
                    )
            })
            .collect::<Vec<_>>();
        let [root_action] = root_actions.as_slice() else {
            return false;
        };
        match root_action.kind {
            ActionKind::New => return false,
            ActionKind::Set => {
                let exact_selected_set = root_action.payload_clone
                    && root_action.value_type.as_ref() == Some(ty)
                    && selection.entries.iter().any(|entry| {
                        entry.owner == function.owner
                            && entry.instruction == root_action.instruction
                            && &entry.checker_type == ty
                            && &entry.mir_type == ty
                    });
                if exact_selected_set {
                    return true;
                }
                let Some(source) = root_action.source_local.as_ref() else {
                    return false;
                };
                current = source.clone();
            }
            ActionKind::Remove => {
                let Some(source) = root_action.source_local.as_ref() else {
                    return false;
                };
                current = source.clone();
            }
            ActionKind::Clone => {
                let Some(source) = root_action.source_local.as_ref() else {
                    return false;
                };
                current = source.clone();
            }
            _ => unreachable!("root action filter is exhaustive"),
        }
    }
}

fn derive_ordinary_map_owned_any_v2_unpack_result_receipts(
    program: &MirProgram,
) -> Result<Vec<OrdinaryMapOwnedAnyV2UnpackResultReceipt>, String> {
    use crate::core::mir::MirInstructionKind as Instruction;
    use crate::core::OrdinaryMapOwnedAnyV2ProfileVersion as ProfileVersion;
    use crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind as Settlement;

    let checker_unpack_count = program
        .functions()
        .values()
        .filter_map(|function| {
            function
                .ownership
                .checker_ordinary_map_owned_any_v2_cfg_receipt
                .as_ref()
        })
        .map(|cfg| cfg.unpack_receipts.len())
        .sum::<usize>();
    let mir_unpack_count = program
        .functions()
        .values()
        .flat_map(|function| function.blocks.values())
        .flat_map(|block| block.instructions.iter())
        .filter(|instruction| {
            matches!(
                instruction.kind,
                Instruction::OrdinaryMapOwnedAnyV2Unpack { .. }
            )
        })
        .count();
    if checker_unpack_count == 0 && mir_unpack_count == 0 {
        return Ok(Vec::new());
    }
    if checker_unpack_count == 0 || mir_unpack_count == 0 {
        return Err(
            "AnyOwner-v2 unpack has Checker/MIR evidence on only one side of the receipt boundary"
                .into(),
        );
    }

    let selection = program
        .ordinary_map_owned_any_v2_payload_selection_receipt()?
        .ok_or_else(|| "AnyOwner-v2 unpack has no Checker-owned payload selection".to_string())?;
    selection.validate_against_program(program)?;
    let get_any_receipts = program.ordinary_map_owned_any_v2_get_any_result_receipts()?;
    let catalog = program.type_catalog();
    let mut receipts = Vec::new();
    let mut observed_casts = BTreeSet::new();
    let mut expected_casts = BTreeSet::new();

    for function in program.functions().values() {
        let Some(cfg) = function
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
        else {
            let has_instruction = function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        Instruction::OrdinaryMapOwnedAnyV2Unpack { .. }
                    )
                })
            });
            if has_instruction {
                return Err(format!(
                    "AnyOwner-v2 unpack in '{}' has no private Checker CFG receipt",
                    function.owner.0
                ));
            }
            continue;
        };
        if cfg.owner != function.owner || cfg.profile_version != ProfileVersion::V2 {
            return Err(format!(
                "AnyOwner-v2 unpack CFG owner/profile disagrees in '{}'",
                function.owner.0
            ));
        }

        let function_instructions = function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .collect::<Vec<_>>();
        let function_unpack_instructions = function_instructions
            .iter()
            .filter(|instruction| {
                matches!(
                    instruction.kind,
                    Instruction::OrdinaryMapOwnedAnyV2Unpack { .. }
                )
            })
            .collect::<Vec<_>>();
        if function_unpack_instructions.len() != cfg.unpack_receipts.len() {
            return Err(format!(
                "AnyOwner-v2 unpack source/MIR receipt census differs in '{}'",
                function.owner.0
            ));
        }

        for checker in &cfg.unpack_receipts {
            if checker.profile_version != ProfileVersion::V2
                || checker.settlement != Settlement::ExplicitDrop
                || !expected_casts.insert((function.owner.clone(), checker.cast.clone()))
            {
                return Err(format!(
                    "AnyOwner-v2 unpack Checker receipt for '{}' has invalid profile, settlement, or identity",
                    checker.cast.0
                ));
            }
            let matching_get_any = get_any_receipts
                .iter()
                .filter(|receipt| {
                    receipt.owner == function.owner
                        && receipt.operation == checker.operation
                        && receipt.binding_local == checker.pair_binding
                        && receipt.binding_point == checker.pair_binding_point
                        && receipt.settlement == checker.settlement
                        && receipt.settlement_point == checker.settlement_point
                })
                .collect::<Vec<_>>();
            let [get_any] = matching_get_any.as_slice() else {
                return Err(format!(
                    "AnyOwner-v2 unpack '{}' does not join one exact GetAny pair/drop receipt",
                    checker.cast.0
                ));
            };
            get_any.validate_against_program(program)?;

            if checker.conversion_kind != crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
                || checker.conversion_from.as_str().trim().is_empty()
                || checker.conversion_to.as_str().trim().is_empty()
                || checker.conversion_from == checker.conversion_to
                || !matches!(
                    catalog.get(&checker.conversion_from),
                    Some(descriptor) if descriptor.kind == super::types::MirTypeKind::DynamicAny
                )
            {
                return Err(format!(
                    "AnyOwner-v2 unpack '{}' lacks an exact DynamicAny checked conversion",
                    checker.cast.0
                ));
            }
            let target = catalog.get(&checker.conversion_to).ok_or_else(|| {
                format!(
                    "AnyOwner-v2 unpack '{}' target TypeDesc is absent",
                    checker.cast.0
                )
            })?;
            let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
            let string_type =
                crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
            let target_supported = if checker.conversion_to == i32_type {
                target.kind == super::types::MirTypeKind::Primitive(crate::core::PrimitiveType::I32)
                    && target.layout == super::types::MirLayout::Scalar
                    && catalog.validate_copy_scalar(&checker.conversion_to).is_ok()
            } else if checker.conversion_to == string_type {
                catalog
                    .validate_owned_string(&checker.conversion_to)
                    .is_ok()
            } else if super::reference::is_canonical_v2_label_type(catalog, &checker.conversion_to)
            {
                true
            } else {
                false
            };
            if !target_supported {
                return Err(format!(
                    "AnyOwner-v2 unpack '{}' target is outside the selected i32/String/Label payload ABI",
                    checker.cast.0
                ));
            }
            for operation in [
                super::types::MirGlueOperation::Clone,
                super::types::MirGlueOperation::Drop,
            ] {
                catalog.validate_glue(&checker.conversion_to, operation)?;
            }

            let target_selected_on_source_root =
                ordinary_map_owned_any_v2_root_selects_payload_type(
                    function,
                    &get_any.source_local,
                    &checker.conversion_to,
                    &selection,
                );
            let selected_target = target_selected_on_source_root
                .then(|| {
                    selection
                        .selected_types
                        .iter()
                        .find(|selected| selected.type_id == checker.conversion_to)
                })
                .flatten();
            let is_canonical_miss = checker.conversion_to == get_any.miss_payload_type;
            // A selected payload is authorized only by a Set on this GetAny
            // root's own persistent lineage. Otherwise only canonical i32
            // zero may describe a miss; String and nominal Record targets do
            // not turn misses into synthetic payload values.
            let (payload_source, payload_type_desc_digest) = if let Some(selected) = selected_target
            {
                (
                    OrdinaryMapOwnedAnyV2UnpackPayloadSource::SelectedSetPayload,
                    selected.type_desc_digest.clone(),
                )
            } else if is_canonical_miss {
                (
                    OrdinaryMapOwnedAnyV2UnpackPayloadSource::CanonicalI32Miss,
                    get_any.miss_payload_type_desc_digest.clone(),
                )
            } else {
                return Err(format!(
                    "AnyOwner-v2 unpack '{}' target is absent from this GetAny root's selected payloads and is not the canonical miss payload",
                    checker.cast.0
                ));
            };
            if payload_type_desc_digest != digest(target.canonical_text()) {
                return Err(format!(
                    "AnyOwner-v2 unpack '{}' TypeDesc digest disagrees with its exact target",
                    checker.cast.0
                ));
            }

            let expected_instruction = super::MirInstructionId::new(format!(
                "inst:ordinary_map_owned_any_v2_unpack:{}",
                checker.cast.0
            ))
            .map_err(|error| error.to_string())?;
            let expected_pair =
                super::MirValueId::new(format!("local:{}", checker.pair_binding.0 .0))
                    .map_err(|error| error.to_string())?;
            let expected_result = super::MirValueId::new(format!("expr:{}", checker.cast.0))
                .map_err(|error| error.to_string())?;
            let candidates = function_instructions
                .iter()
                .filter_map(|instruction| match &instruction.kind {
                    Instruction::OrdinaryMapOwnedAnyV2Unpack {
                        result,
                        pair,
                        projection,
                    } if instruction.id == expected_instruction
                        && result == &expected_result
                        && pair == &expected_pair
                        && projection == &checker.projection =>
                    {
                        Some(*instruction)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            let [instruction] = candidates.as_slice() else {
                return Err(format!(
                    "AnyOwner-v2 unpack '{}' lacks its unique exact MIR instruction",
                    checker.cast.0
                ));
            };
            if !observed_casts.insert((function.owner.clone(), checker.cast.clone())) {
                return Err(format!(
                    "AnyOwner-v2 unpack '{}' has duplicate MIR instructions",
                    checker.cast.0
                ));
            }
            if function.values.get(&expected_pair).is_none_or(|value| {
                value.ty != super::types::ordinary_map_owned_any_v2_get_result_type_id()
            }) || function
                .values
                .get(&expected_result)
                .is_none_or(|value| value.ty != checker.conversion_to)
            {
                return Err(format!(
                    "AnyOwner-v2 unpack '{}' pair/result TypeDesc disagrees with the Checker receipt",
                    checker.cast.0
                ));
            }

            let receipt = OrdinaryMapOwnedAnyV2UnpackResultReceipt {
                schema: ORDINARY_MAP_OWNED_ANY_V2_UNPACK_RESULT_SCHEMA,
                profile: ORDINARY_MAP_OWNED_ANY_V2_PROFILE,
                owner: function.owner.clone(),
                operation: checker.operation.clone(),
                instruction: instruction.id.clone(),
                pair_binding_local: checker.pair_binding.clone(),
                pair: expected_pair,
                pair_binding_point: checker.pair_binding_point.clone(),
                projection: checker.projection.clone(),
                cast: checker.cast.clone(),
                conversion_kind: checker.conversion_kind,
                conversion_from: checker.conversion_from.clone(),
                conversion_to: checker.conversion_to.clone(),
                result: expected_result,
                result_type: checker.conversion_to.clone(),
                settlement: checker.settlement,
                settlement_point: checker.settlement_point.clone(),
                settlement_instruction: get_any.settlement_instruction.clone(),
                payload_source,
                payload_selection_digest: selection.selection_digest.clone(),
                payload_type_desc_digest,
            };
            receipt.validate()?;
            receipts.push(receipt);
        }

        for instruction in &function_unpack_instructions {
            let matched = cfg.unpack_receipts.iter().any(|checker| {
                instruction.id.as_str()
                    == format!("inst:ordinary_map_owned_any_v2_unpack:{}", checker.cast.0)
            });
            if !matched {
                return Err(format!(
                    "unreceipted AnyOwner-v2 unpack instruction '{}'",
                    instruction.id
                ));
            }
        }
    }

    if observed_casts != expected_casts {
        return Err(
            "AnyOwner-v2 unpack receipt census has missing or extra source identities".into(),
        );
    }
    receipts.sort_by(|left, right| (&left.owner, &left.cast).cmp(&(&right.owner, &right.cast)));
    Ok(receipts)
}

fn derive_ordinary_map_owned_any_v2_values_result_receipts(
    program: &MirProgram,
) -> Result<Vec<OrdinaryMapOwnedAnyV2ValuesResultReceipt>, String> {
    use crate::core::mir::MirInstructionKind as Instruction;
    use crate::core::OrdinaryMapOwnedAnyV2ActionKind as ActionKind;
    use crate::core::OrdinaryMapOwnedAnyV2ProfileVersion as ProfileVersion;
    use crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind as Settlement;

    let has_values_signal = program.functions().values().any(|function| {
        function
            .ownership
            .checker_ordinary_map_owned_any_v2
            .iter()
            .any(|action| action.kind == ActionKind::Values)
            || function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        Instruction::OrdinaryMapOwnedAnyV2Values { .. }
                    )
                })
            })
    });
    if !has_values_signal {
        return Ok(Vec::new());
    }

    let payload_selection = program
        .ordinary_map_owned_any_v2_payload_selection_receipt()?
        .ok_or_else(|| {
            "AnyOwner-v2 Values has no Checker-owned payload-selection receipt".to_string()
        })?;
    payload_selection.validate_against_program(program)?;
    let catalog = program.type_catalog();
    let expected_root_type = super::types::ordinary_map_owned_any_v2_type_id();
    let expected_result_type = super::types::ordinary_map_owned_any_v2_values_type_id();
    for operation in [
        super::types::MirGlueOperation::MoveOut,
        super::types::MirGlueOperation::Clone,
        super::types::MirGlueOperation::Drop,
    ] {
        catalog.validate_list_glue(&expected_result_type, operation)?;
    }

    let mut receipts = Vec::new();
    let mut observed_values = BTreeSet::new();
    let mut observed_checker_results = BTreeSet::new();
    for function in program.functions().values() {
        let function_values = function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| match &instruction.kind {
                Instruction::OrdinaryMapOwnedAnyV2Values { result, root } => {
                    Some((instruction.id.clone(), result.clone(), root.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let checker_values = function
            .ownership
            .checker_ordinary_map_owned_any_v2
            .iter()
            .filter(|action| action.kind == ActionKind::Values)
            .collect::<Vec<_>>();
        if checker_values.is_empty() && function_values.is_empty() {
            continue;
        }
        if checker_values.len() != function_values.len() {
            return Err(format!(
                "AnyOwner-v2 Values action/instruction count differs in '{}'",
                function.owner.0
            ));
        }
        let cfg = function
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .ok_or_else(|| {
                format!(
                    "AnyOwner-v2 Values in '{}' has no private Checker CFG receipt",
                    function.owner.0
                )
            })?;
        if cfg.owner != function.owner || cfg.profile_version != ProfileVersion::V2 {
            return Err(format!(
                "AnyOwner-v2 Values CFG owner/profile disagrees in '{}'",
                function.owner.0
            ));
        }
        let cfg_values_actions = cfg
            .actions
            .iter()
            .filter(|action| action.kind == ActionKind::Values)
            .collect::<Vec<_>>();
        let cfg_values_results = cfg
            .result_receipts
            .iter()
            .filter(|result| result.kind == ActionKind::Values)
            .collect::<Vec<_>>();
        if cfg_values_actions.len() != checker_values.len()
            || cfg_values_results.len() != checker_values.len()
        {
            return Err(format!(
                "AnyOwner-v2 Values action/result receipt census disagrees in '{}'",
                function.owner.0
            ));
        }

        for action in checker_values {
            if action.profile_version != ProfileVersion::V2 {
                return Err(format!(
                    "AnyOwner-v2 Values action '{}' has an unsupported profile",
                    action.instruction
                ));
            }
            let matching_cfg_actions = cfg_values_actions
                .iter()
                .filter(|checker_action| {
                    checker_action.point == action.point
                        && checker_action.root.0 .0 == action.root
                        && checker_action.source_local.as_ref() == Some(&checker_action.local)
                        && checker_action
                            .source
                            .as_ref()
                            .map(|resource| &resource.0 .0)
                            == Some(&action.root)
                        && checker_action.source_node == action.source_node
                        && checker_action.source_type == action.source_type
                        && checker_action.key.is_none()
                        && checker_action.key_type.is_none()
                        && checker_action.value.is_none()
                        && checker_action.value_type.is_none()
                        && !checker_action.payload_clone
                        && checker_action.result_type == action.result_source_type
                        && checker_action.result_local == action.result_local
                })
                .collect::<Vec<_>>();
            let [checker_action] = matching_cfg_actions.as_slice() else {
                return Err(format!(
                    "AnyOwner-v2 Values MIR action '{}' does not map to exactly one Checker action",
                    action.instruction
                ));
            };
            let source_node = checker_action.source_node.as_ref().ok_or_else(|| {
                format!(
                    "AnyOwner-v2 Values '{}' has no Checker source node",
                    action.instruction
                )
            })?;
            let source_type = checker_action.source_type.as_ref().ok_or_else(|| {
                format!(
                    "AnyOwner-v2 Values '{}' has no Checker source type",
                    action.instruction
                )
            })?;
            let checker_result_type = checker_action.result_type.as_ref().ok_or_else(|| {
                format!(
                    "AnyOwner-v2 Values '{}' has no Checker result type",
                    action.instruction
                )
            })?;
            let binding_local = checker_action.result_local.as_ref().ok_or_else(|| {
                format!(
                    "AnyOwner-v2 Values '{}' has no direct binding",
                    action.instruction
                )
            })?;
            let result_descriptor = catalog.get(checker_result_type).ok_or_else(|| {
                format!(
                    "AnyOwner-v2 Values Checker result TypeDesc '{}' is absent",
                    checker_result_type.as_str()
                )
            })?;
            let element_type = match &result_descriptor.layout {
                super::types::MirLayout::List { element }
                    if result_descriptor.kind == super::types::MirTypeKind::List =>
                {
                    element
                }
                _ => {
                    return Err(format!(
                        "AnyOwner-v2 Values Checker result '{}' is not a List TypeDesc",
                        checker_result_type.as_str()
                    ));
                }
            };
            if catalog
                .get(element_type)
                .is_none_or(|descriptor| descriptor.kind != super::types::MirTypeKind::DynamicAny)
            {
                return Err(format!(
                    "AnyOwner-v2 Values Checker result '{}' is not exact List<DynamicAny>",
                    checker_result_type.as_str()
                ));
            }
            let checker_results = cfg_values_results
                .iter()
                .filter(|result| result.operation == action.point)
                .collect::<Vec<_>>();
            let [checker_result] = checker_results.as_slice() else {
                return Err(format!(
                    "AnyOwner-v2 Values action '{}' has no unique result settlement receipt",
                    action.instruction
                ));
            };
            let settlement_point = checker_result.settlement_point.as_ref().ok_or_else(|| {
                format!(
                    "AnyOwner-v2 Values '{}' has no settlement point",
                    action.point.0
                )
            })?;
            if checker_result.profile_version != ProfileVersion::V2
                || checker_result.kind != ActionKind::Values
                || checker_result.operation != action.point
                || checker_result.source_local != checker_action.local
                || checker_result.source != checker_action.root
                || checker_result.source_node != *source_node
                || checker_result.source_type != *source_type
                || checker_result.key.is_some()
                || checker_result.key_type.is_some()
                || checker_result.result != action.point
                || checker_result.result_type != *checker_result_type
                || checker_result.binding != *binding_local
                || checker_result.binding_point.0.trim().is_empty()
                || checker_result.settlement != Settlement::ExplicitDrop
            {
                return Err(format!(
                    "AnyOwner-v2 Values action '{}' disagrees with its independent result receipt",
                    action.instruction
                ));
            }
            if checker_action.source_local.as_ref() != Some(&checker_action.local)
                || checker_action
                    .source
                    .as_ref()
                    .map(|resource| &resource.0 .0)
                    != Some(&checker_action.root.0 .0)
                || action.source_node.as_ref() != Some(source_node)
                || action.source_type.as_ref() != Some(source_type)
                || action.result_source_type.as_ref() != Some(checker_result_type)
                || action.result_local.as_ref() != Some(binding_local)
                || action.key.is_some()
                || action.key_type.is_some()
                || action.value.is_some()
                || action.value_type.is_some()
                || action.payload_clone
            {
                return Err(format!(
                    "AnyOwner-v2 Values action '{}' has incomplete source/result bindings",
                    action.instruction
                ));
            }
            let matching_attempts = cfg
                .attempts
                .iter()
                .filter(|attempt| {
                    attempt.kind == crate::core::OrdinaryMapOwnedAnyV2AttemptKind::Values
                        && attempt.point == action.point
                        && attempt.receiver_local.as_ref() == Some(&checker_action.local)
                        && attempt.result_local.as_ref() == Some(binding_local)
                        && attempt.arguments == vec![source_node.clone()]
                })
                .count();
            if matching_attempts != 1 {
                return Err(format!(
                    "AnyOwner-v2 Values action '{}' has no unique matching source attempt",
                    action.instruction
                ));
            }

            let expected_instruction = format!(
                "inst:ordinary_map_owned_any_v2_values:{}:{}",
                action.local, action.point.0
            );
            if action.instruction.as_str() != expected_instruction {
                return Err(format!(
                    "AnyOwner-v2 Values action '{}' has a noncanonical instruction identity",
                    action.instruction
                ));
            }
            let root = super::MirValueId::new(format!("local:{}", checker_action.local.0 .0))
                .map_err(|error| error.to_string())?;
            let result = super::MirValueId::new(format!("expr:{}", action.point.0))
                .map_err(|error| error.to_string())?;
            let binding = super::MirValueId::new(format!("local:{}", binding_local.0 .0))
                .map_err(|error| error.to_string())?;
            let matching_instructions = function_values
                .iter()
                .filter(|(id, mir_result, mir_root)| {
                    id == &action.instruction && mir_result == &result && mir_root == &root
                })
                .collect::<Vec<_>>();
            let [(instruction, _, _)] = matching_instructions.as_slice() else {
                return Err(format!(
                    "AnyOwner-v2 Values Checker action '{}' has no exact MIR result/root instruction",
                    action.instruction
                ));
            };
            if !observed_values.insert((function.owner.clone(), (*instruction).clone())) {
                return Err(format!(
                    "AnyOwner-v2 Values instruction '{}' is duplicated",
                    instruction
                ));
            }
            if catalog.get(source_type).is_none()
                || function.values.get(&root).map(|value| &value.ty) != Some(&expected_root_type)
                || function.values.get(&result).map(|value| &value.ty)
                    != Some(&expected_result_type)
                || action.result.as_ref() != Some(&result)
            {
                return Err(format!(
                    "AnyOwner-v2 Values instruction '{}' source/result TypeDescs disagree",
                    action.instruction
                ));
            }

            // Require a same-function lineage from New. A Set on this exact
            // lineage needs its Checker-selected TypeDesc; if there is no
            // Set, the root is provably empty because New starts empty and
            // Remove cannot add entries. Unrelated Sets cannot lend payload
            // authority to this snapshot.
            let mut root_local = action.local.clone();
            let mut visited = BTreeSet::new();
            loop {
                if !visited.insert(root_local.clone()) {
                    return Err(format!(
                        "AnyOwner-v2 Values '{}' source root lineage contains a cycle",
                        action.instruction
                    ));
                }
                let candidates = function
                    .ownership
                    .checker_ordinary_map_owned_any_v2
                    .iter()
                    .filter(|candidate| {
                        candidate.local == root_local
                            && matches!(
                                candidate.kind,
                                ActionKind::New
                                    | ActionKind::Clone
                                    | ActionKind::Set
                                    | ActionKind::Remove
                            )
                    })
                    .collect::<Vec<_>>();
                let [root_action] = candidates.as_slice() else {
                    return Err(format!(
                        "AnyOwner-v2 Values '{}' source root has no unique Checker construction lineage",
                        action.instruction
                    ));
                };
                match root_action.kind {
                    ActionKind::New => {
                        if root_action.source_local.is_some() || root_action.source.is_some() {
                            return Err(format!(
                                "AnyOwner-v2 Values '{}' source root New has unexpected source evidence",
                                action.instruction
                            ));
                        }
                        break;
                    }
                    ActionKind::Set | ActionKind::Remove => {
                        if root_action.kind == ActionKind::Set {
                            if payload_selection
                                .entries
                                .iter()
                                .filter(|entry| {
                                    entry.owner == function.owner
                                        && entry.instruction == root_action.instruction
                                })
                                .count()
                                != 1
                            {
                                return Err(format!(
                                    "AnyOwner-v2 Values '{}' source root Set is not independently payload-selected",
                                    action.instruction
                                ));
                            }
                        }
                        root_local = root_action.source_local.clone().ok_or_else(|| {
                            format!(
                                "AnyOwner-v2 Values '{}' source root update has no predecessor local",
                                action.instruction
                            )
                        })?;
                    }
                    ActionKind::Clone => {
                        root_local = root_action.source_local.clone().ok_or_else(|| {
                            format!(
                                "AnyOwner-v2 Values '{}' Clone root has no source local",
                                action.instruction
                            )
                        })?;
                    }
                    _ => unreachable!("the source-root action filter is exhaustive"),
                }
            }
            let moves = function
                .blocks
                .values()
                .flat_map(|block| &block.instructions)
                .filter(|instruction| {
                    matches!(
                        &instruction.kind,
                        Instruction::Move { result: destination, source }
                            if destination == &binding && source == &result
                    )
                })
                .collect::<Vec<_>>();
            if moves.len() != 1
                || function.values.get(&binding).map(|value| &value.ty)
                    != Some(&expected_result_type)
            {
                return Err(format!(
                    "AnyOwner-v2 Values result '{}' is not transferred by one direct binding Move",
                    result
                ));
            }
            let settlement_instruction =
                super::MirInstructionId::new(format!("inst:drop.0:{}", settlement_point.0))
                    .map_err(|error| error.to_string())?;
            let drops = function
                .blocks
                .iter()
                .flat_map(|(block_id, block)| {
                    block
                        .instructions
                        .iter()
                        .enumerate()
                        .map(move |(index, instruction)| (block_id, index, instruction))
                })
                .filter(|(_, _, instruction)| {
                    instruction.id == settlement_instruction
                        && matches!(&instruction.kind, Instruction::Drop { value } if value == &binding)
                })
                .collect::<Vec<_>>();
            if drops.len() != 1 {
                return Err(format!(
                    "AnyOwner-v2 Values binding '{}' lacks its exact explicit Drop instruction",
                    binding
                ));
            }
            let (binding_drop_block, binding_drop_index, _) = drops[0];
            if mir_values_binding_has_invalid_post_drop_path(
                function,
                &binding,
                &result,
                &moves[0].id,
                &action.instruction,
                &settlement_instruction,
                binding_drop_block,
                binding_drop_index,
            ) {
                return Err(format!(
                    "AnyOwner-v2 Values binding '{}' has a use or unsettled ownership state reachable after its explicit Drop",
                    binding
                ));
            }
            let uses_of_result = function
                .blocks
                .values()
                .flat_map(|block| &block.instructions)
                .filter(|instruction| {
                    super::mir_instruction_value_uses(&instruction.kind).contains(&result)
                })
                .collect::<Vec<_>>();
            if uses_of_result.len() != 1 || uses_of_result[0].id != moves[0].id {
                return Err(format!(
                    "AnyOwner-v2 Values result '{}' escapes its direct binding Move",
                    result
                ));
            }
            if function
                .blocks
                .values()
                .any(|block| super::mir_terminator_value_uses(&block.terminator).contains(&result))
            {
                return Err(format!(
                    "AnyOwner-v2 Values result '{}' escapes through a terminator",
                    result
                ));
            }
            let mut borrowed_values_clones = BTreeSet::new();
            for (block_id, block) in &function.blocks {
                for (index, instruction) in block.instructions.iter().enumerate() {
                    if !super::mir_instruction_value_uses(&instruction.kind).contains(&binding) {
                        continue;
                    }
                    let exact_drop = instruction.id == settlement_instruction
                        && matches!(&instruction.kind, Instruction::Drop { value } if value == &binding);
                    let list_len_observation = matches!(
                        &instruction.kind,
                        Instruction::ListOp {
                            operation: super::MirListOperation::Len,
                            list,
                            argument: None,
                            list_operation_contract: Some(_),
                            ..
                        } if list == &binding
                    );
                    let values_unpack_observation = match &instruction.kind {
                        Instruction::OrdinaryMapOwnedAnyV2ValuesUnpack {
                            result: unpack_result,
                            values,
                            index,
                            projection,
                        } if values == &binding => {
                            cfg.values_unpack_receipts.iter().any(|unpack| {
                                unpack.profile_version == ProfileVersion::V2
                                    && unpack.operation == action.point
                                    && unpack.snapshot_binding == *binding_local
                                    && unpack.snapshot_binding_point == checker_result.binding_point
                                    && unpack.index_projection == *projection
                                    && function
                                        .values
                                        .get(unpack_result)
                                        .is_some_and(|value| value.ty == unpack.conversion_to)
                                    && unpack.cast.0
                                        == unpack_result
                                            .as_str()
                                            .strip_prefix("expr:")
                                            .unwrap_or_default()
                                    && (unpack.index_local.as_ref().is_some_and(|local| {
                                        index.as_str() == format!("local:{}", local.0 .0)
                                    }) || (unpack.index_local.is_none()
                                        && index.as_str()
                                            == format!("expr:{}", unpack.index_expression.0)))
                                    && instruction.id.as_str()
                                        == format!(
                                            "inst:ordinary_map_owned_any_v2_values_unpack:{}",
                                            unpack.cast.0
                                        )
                            })
                        }
                        _ => false,
                    };
                    let borrowed_clone = matches!(
                        &instruction.kind,
                        Instruction::Clone { result, source }
                            if source == &binding
                                && result != &binding
                                && function.values.get(result).map(|value| &value.ty)
                                    == Some(&expected_result_type)
                                && borrowed_values_clones.insert(result.clone())
                    );
                    if !exact_drop
                        && !list_len_observation
                        && !values_unpack_observation
                        && !borrowed_clone
                    {
                        return Err(format!(
                            "AnyOwner-v2 Values binding '{}' has an unsupported use at '{}'",
                            binding, instruction.id
                        ));
                    }
                    if !exact_drop
                        && !mir_instruction_use_is_settled_on_all_finite_paths(
                            function,
                            block_id,
                            index,
                            binding_drop_block,
                            binding_drop_index,
                        )
                    {
                        return Err(format!(
                            "AnyOwner-v2 Values binding '{}' is used at '{}' without its explicit Drop postdominating that use",
                            binding, instruction.id
                        ));
                    }
                }
            }
            if function
                .blocks
                .values()
                .any(|block| super::mir_terminator_value_uses(&block.terminator).contains(&binding))
            {
                return Err(format!(
                    "AnyOwner-v2 Values binding '{}' escapes through a terminator",
                    binding
                ));
            }
            for clone in borrowed_values_clones {
                let mut clone_definitions = Vec::new();
                for (block_id, block) in &function.blocks {
                    for (index, instruction) in block.instructions.iter().enumerate() {
                        if matches!(
                            &instruction.kind,
                            Instruction::Clone { result, source }
                                if result == &clone && source == &binding
                        ) {
                            clone_definitions.push((block_id, index));
                        }
                    }
                }
                let [(clone_block, clone_index)] = clone_definitions.as_slice() else {
                    return Err(format!(
                        "AnyOwner-v2 Values borrow clone '{}' has no unique MIR definition",
                        clone
                    ));
                };
                let mut len_observations = Vec::new();
                let mut clone_drops = Vec::new();
                for (block_id, block) in &function.blocks {
                    for (index, instruction) in block.instructions.iter().enumerate() {
                        if !super::mir_instruction_value_uses(&instruction.kind).contains(&clone) {
                            continue;
                        }
                        match &instruction.kind {
                            Instruction::ListOp {
                                operation: super::MirListOperation::Len,
                                list,
                                argument: None,
                                list_operation_contract: Some(_),
                                ..
                            } if list == &clone => len_observations.push((block_id, index)),
                            Instruction::Drop { value } if value == &clone => {
                                clone_drops.push((block_id, index));
                            }
                            _ => {
                                return Err(format!(
                                    "AnyOwner-v2 Values borrow clone '{}' has an unsupported use at '{}'",
                                    clone, instruction.id
                                ));
                            }
                        }
                    }
                }
                if len_observations.len() != 1 || clone_drops.len() != 1 {
                    return Err(format!(
                        "AnyOwner-v2 Values borrow clone '{}' must have exactly one Len observation and one explicit Drop",
                        clone
                    ));
                }
                let (len_block, len_index) = len_observations[0];
                let (drop_block, drop_index) = clone_drops[0];
                if *clone_block != len_block
                    || len_block != drop_block
                    || *clone_index >= len_index
                    || len_index >= drop_index
                {
                    return Err(format!(
                        "AnyOwner-v2 Values borrow clone '{}' must be defined, observed by Len, and explicitly dropped in that order in one MIR block",
                        clone
                    ));
                }
                if function.blocks.values().any(|block| {
                    super::mir_terminator_value_uses(&block.terminator).contains(&clone)
                }) {
                    return Err(format!(
                        "AnyOwner-v2 Values borrow clone '{}' escapes through a terminator",
                        clone
                    ));
                }
            }

            let receipt = OrdinaryMapOwnedAnyV2ValuesResultReceipt {
                schema: ORDINARY_MAP_OWNED_ANY_V2_VALUES_RESULT_SCHEMA,
                profile: ORDINARY_MAP_OWNED_ANY_V2_PROFILE,
                owner: function.owner.clone(),
                operation: action.point.clone(),
                instruction: action.instruction.clone(),
                source_node: source_node.clone(),
                source_local: checker_action.local.clone(),
                root,
                checker_source_type: source_type.clone(),
                checker_result_type: checker_result_type.clone(),
                result,
                mir_result_type: expected_result_type.clone(),
                binding_local: binding_local.clone(),
                binding,
                binding_point: checker_result.binding_point.clone(),
                settlement: Settlement::ExplicitDrop,
                settlement_point: settlement_point.clone(),
                settlement_instruction,
                payload_selection_digest: payload_selection.selection_digest.clone(),
            };
            receipt.validate()?;
            observed_checker_results.insert((function.owner.clone(), action.point.clone()));
            receipts.push(receipt);
        }
    }

    let observed_instruction_count = program
        .functions()
        .values()
        .flat_map(|function| {
            function
                .blocks
                .values()
                .flat_map(|block| &block.instructions)
        })
        .filter(|instruction| {
            matches!(
                instruction.kind,
                Instruction::OrdinaryMapOwnedAnyV2Values { .. }
            )
        })
        .count();
    if observed_instruction_count != observed_values.len() {
        return Err(
            "AnyOwner-v2 Values instruction census contains unreceipted or duplicate MIR".into(),
        );
    }
    let checker_results = program
        .functions()
        .values()
        .flat_map(|function| {
            function
                .ownership
                .checker_ordinary_map_owned_any_v2_cfg_receipt
                .iter()
                .flat_map(move |cfg| {
                    cfg.result_receipts
                        .iter()
                        .filter(move |result| result.kind == ActionKind::Values)
                        .map(move |result| (function.owner.clone(), result.operation.clone()))
                })
        })
        .collect::<BTreeSet<_>>();
    if checker_results != observed_checker_results {
        return Err(
            "AnyOwner-v2 Values result receipt census has missing or extra Checker receipts".into(),
        );
    }
    receipts.sort();
    Ok(receipts)
}

/// Recompute the source-selected Set lineage for a Values snapshot target.
/// The returned order follows persistent versions from the Values root back
/// through newer Set/Remove nodes, with the selecting Set as the last entry.
fn ordinary_map_owned_any_v2_values_unpack_selection_lineage(
    function: &super::MirFunction,
    root: &crate::core::ir::ResolvedLocalId,
    ty: &crate::core::ir::ResolvedTypeId,
    selection: &OrdinaryMapOwnedAnyV2PayloadSelectionReceipt,
) -> Option<Vec<NodeId>> {
    use crate::core::OrdinaryMapOwnedAnyV2ActionKind as ActionKind;

    let mut current = super::MirValueId::new(format!("local:{}", root.0 .0)).ok()?;
    let mut seen = BTreeSet::new();
    let mut lineage = Vec::new();
    loop {
        if !seen.insert(current.clone()) {
            return None;
        }
        let actions = function
            .ownership
            .checker_ordinary_map_owned_any_v2
            .iter()
            .filter(|action| {
                action.local == current
                    && matches!(
                        action.kind,
                        ActionKind::New | ActionKind::Clone | ActionKind::Set | ActionKind::Remove
                    )
            })
            .collect::<Vec<_>>();
        let [action] = actions.as_slice() else {
            return None;
        };
        match action.kind {
            ActionKind::New => return None,
            ActionKind::Set => {
                lineage.push(action.point.clone());
                let exact_selected_set = action.payload_clone
                    && action.value_type.as_ref() == Some(ty)
                    && selection.entries.iter().any(|entry| {
                        entry.owner == function.owner
                            && entry.instruction == action.instruction
                            && &entry.checker_type == ty
                            && &entry.mir_type == ty
                    });
                if exact_selected_set {
                    return Some(lineage);
                }
                current = action.source_local.as_ref()?.clone();
            }
            ActionKind::Remove => {
                lineage.push(action.point.clone());
                current = action.source_local.as_ref()?.clone();
            }
            ActionKind::Clone => {
                current = action.source_local.as_ref()?.clone();
            }
            _ => unreachable!("root action filter is exhaustive"),
        }
    }
}

/// Derive the exact MIR Drop which discharges the unpacked result's own
/// owner. This proof is intentionally independent from the Values snapshot
/// Drop receipt: the List Drop cannot discharge an element clone that the
/// indexed-unpack operation created.
fn ordinary_map_owned_any_v2_values_unpack_owner_drop(
    function: &super::MirFunction,
    checker: &crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackReceipt,
    result_binding: &super::MirValueId,
    result_binding_move: &super::MirInstructionId,
    target_type: &crate::core::ResolvedTypeId,
    catalog: &super::types::MirTypeCatalog,
) -> Result<Option<super::MirInstructionId>, String> {
    use super::MirTerminator;
    use crate::core::mir::MirInstructionKind as Instruction;
    use crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind as Settlement;

    fn consumes_binding(kind: &Instruction, binding: &super::MirValueId) -> bool {
        match kind {
            Instruction::Move { source, .. }
            | Instruction::Drop { value: source }
            | Instruction::MoveProject { base: source, .. }
            | Instruction::MoveProjectDrop { base: source, .. }
            | Instruction::Convert { source, .. } => source == binding,
            Instruction::UpdateRecord { base, fields, .. } => {
                base == binding || fields.iter().any(|field| field == binding)
            }
            Instruction::ConstructVariantMove { fields, .. } => {
                fields.iter().any(|(_, field)| field == binding)
            }
            Instruction::Call { arguments, .. } | Instruction::FlowTransition { arguments, .. } => {
                arguments.iter().any(|argument| argument == binding)
            }
            Instruction::BuiltinCall { arguments, .. } => {
                arguments.iter().any(|argument| argument == binding)
            }
            Instruction::SessionCall {
                endpoint, payload, ..
            } => endpoint == binding || payload.as_ref() == Some(binding),
            _ => false,
        }
    }

    let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
    let drops = function
        .blocks
        .values()
        .flat_map(|block| {
            block
                .instructions
                .iter()
                .enumerate()
                .filter_map(move |(index, instruction)| {
                    matches!(
                        &instruction.kind,
                        Instruction::Drop { value } if value == result_binding
                    )
                    .then_some((block.id.clone(), index, instruction))
                })
        })
        .collect::<Vec<_>>();

    match checker.result_owner_settlement {
        Settlement::NoOwnedPayload => {
            if target_type != &i32_type
                || checker.result_owner_settlement_point.is_some()
                || checker.result_owner_settlement_index.is_some()
            {
                return Err(format!(
                    "AnyOwner-v2 Values unpack '{}' Copy result has an owner settlement obligation",
                    checker.cast.0
                ));
            }
            Ok(None)
        }
        Settlement::CompilerOwnedStringDrop => {
            if target_type != &string_type
                || checker.result_owner_settlement_point.is_none()
                || checker.result_owner_settlement_index.is_some()
                || function.blocks.len() != 1
            {
                return Err(format!(
                    "AnyOwner-v2 Values unpack '{}' String owner is outside the one-block compiler-cleanup profile",
                    checker.cast.0
                ));
            }
            let block = function.blocks.get(&function.entry).ok_or_else(|| {
                format!(
                    "AnyOwner-v2 Values unpack '{}' String owner has no entry block",
                    checker.cast.0
                )
            })?;
            if !matches!(&block.terminator, MirTerminator::Return { value: Some(_) }) {
                return Err(format!(
                    "AnyOwner-v2 Values unpack '{}' String owner has no root tail return",
                    checker.cast.0
                ));
            }
            let [(drop_block, drop_index, drop_instruction)] = drops.as_slice() else {
                return Err(format!(
                    "AnyOwner-v2 Values unpack '{}' String result must have one exact compiler-owned binding Drop",
                    checker.cast.0
                ));
            };
            let Some((role, anchor)) = drop_instruction
                .id
                .as_str()
                .strip_prefix("inst:drop.")
                .and_then(|suffix| suffix.split_once(':'))
            else {
                return Err(format!(
                    "AnyOwner-v2 Values unpack '{}' String result Drop is not an end-of-body cleanup instruction",
                    checker.cast.0
                ));
            };
            if !role
                .parse::<usize>()
                .is_ok_and(|index| role == index.to_string())
                || Some(anchor)
                    != checker
                        .result_owner_settlement_point
                        .as_ref()
                        .map(|point| point.0.as_str())
                || drop_block != &block.id
            {
                return Err(format!(
                    "AnyOwner-v2 Values unpack '{}' String result Drop has a noncanonical cleanup identity",
                    checker.cast.0
                ));
            }
            let binding_move = block
                .instructions
                .iter()
                .position(|instruction| &instruction.id == result_binding_move)
                .ok_or_else(|| {
                    format!(
                        "AnyOwner-v2 Values unpack '{}' String result binding Move is absent",
                        checker.cast.0
                    )
                })?;
            if binding_move >= *drop_index
                || block
                    .instructions
                    .iter()
                    .enumerate()
                    .any(|(index, instruction)| {
                        if !super::mir_instruction_value_uses(&instruction.kind)
                            .contains(result_binding)
                        {
                            return false;
                        }
                        if index > *drop_index {
                            return true;
                        }
                        index != *drop_index && consumes_binding(&instruction.kind, result_binding)
                    })
                || super::mir_terminator_value_uses(&block.terminator).contains(result_binding)
            {
                return Err(format!(
                    "AnyOwner-v2 Values unpack '{}' String binding is consumed before or used after its compiler Drop",
                    checker.cast.0
                ));
            }
            Ok(Some(drop_instruction.id.clone()))
        }
        Settlement::ExplicitWholeDrop => {
            let (Some(point), Some(place_index)) = (
                checker.result_owner_settlement_point.as_ref(),
                checker.result_owner_settlement_index,
            ) else {
                return Err(format!(
                    "AnyOwner-v2 Values unpack '{}' record result has no Checker whole-Drop point/index",
                    checker.cast.0
                ));
            };
            if !super::reference::is_canonical_v2_label_type(catalog, target_type)
                || function.blocks.len() != 1
            {
                return Err(format!(
                    "AnyOwner-v2 Values unpack '{}' record owner is outside the one-block Label whole-Drop profile",
                    checker.cast.0
                ));
            }
            let block = function.blocks.get(&function.entry).ok_or_else(|| {
                format!(
                    "AnyOwner-v2 Values unpack '{}' record owner has no entry block",
                    checker.cast.0
                )
            })?;
            if !matches!(&block.terminator, MirTerminator::Return { value: Some(_) }) {
                return Err(format!(
                    "AnyOwner-v2 Values unpack '{}' record owner has no root tail return",
                    checker.cast.0
                ));
            }
            let expected =
                super::MirInstructionId::new(format!("inst:drop.{place_index}:{}", point.0))
                    .map_err(|error| error.to_string())?;
            let [(drop_block, drop_index, drop_instruction)] = drops.as_slice() else {
                return Err(format!(
                    "AnyOwner-v2 Values unpack '{}' record result must have one exact whole-value Drop",
                    checker.cast.0
                ));
            };
            let binding_move = block
                .instructions
                .iter()
                .position(|instruction| &instruction.id == result_binding_move)
                .ok_or_else(|| {
                    format!(
                        "AnyOwner-v2 Values unpack '{}' record result binding Move is absent",
                        checker.cast.0
                    )
                })?;
            if drop_block != &block.id
                || drop_instruction.id != expected
                || binding_move >= *drop_index
                || block
                    .instructions
                    .iter()
                    .enumerate()
                    .any(|(index, instruction)| {
                        index != *drop_index
                            && super::mir_instruction_value_uses(&instruction.kind)
                                .contains(result_binding)
                    })
                || super::mir_terminator_value_uses(&block.terminator).contains(result_binding)
            {
                return Err(format!(
                    "AnyOwner-v2 Values unpack '{}' record result does not have one isolated whole-value Drop at its exact Checker point/index",
                    checker.cast.0
                ));
            }
            Ok(Some(drop_instruction.id.clone()))
        }
    }
}

fn derive_ordinary_map_owned_any_v2_values_unpack_result_receipts(
    program: &MirProgram,
) -> Result<Vec<OrdinaryMapOwnedAnyV2ValuesUnpackResultReceipt>, String> {
    use crate::core::mir::MirInstructionKind as Instruction;
    use crate::core::OrdinaryMapOwnedAnyV2ProfileVersion as ProfileVersion;
    use crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind as Settlement;
    use crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind as OwnerSettlement;

    let checker_count = program
        .functions()
        .values()
        .filter_map(|function| {
            function
                .ownership
                .checker_ordinary_map_owned_any_v2_cfg_receipt
                .as_ref()
        })
        .map(|cfg| cfg.values_unpack_receipts.len())
        .sum::<usize>();
    let mir_count = program
        .functions()
        .values()
        .flat_map(|function| function.blocks.values())
        .flat_map(|block| block.instructions.iter())
        .filter(|instruction| {
            matches!(
                instruction.kind,
                Instruction::OrdinaryMapOwnedAnyV2ValuesUnpack { .. }
            )
        })
        .count();
    if checker_count == 0 && mir_count == 0 {
        return Ok(Vec::new());
    }
    if checker_count != mir_count {
        return Err(format!(
            "AnyOwner-v2 Values unpack Checker/MIR receipt census differs ({checker_count} Checker, {mir_count} MIR)"
        ));
    }

    let selection = program
        .ordinary_map_owned_any_v2_payload_selection_receipt()?
        .ok_or_else(|| "AnyOwner-v2 Values unpack has no payload-selection receipt".to_string())?;
    selection.validate_against_program(program)?;
    let values_receipts = program.ordinary_map_owned_any_v2_values_result_receipts()?;
    let catalog = program.type_catalog();
    let values_type = super::types::ordinary_map_owned_any_v2_values_type_id();
    let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    let i64_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I64);
    let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
    let mut receipts = Vec::new();
    let mut observed = BTreeSet::new();

    for function in program.functions().values() {
        let Some(cfg) = function
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
        else {
            let has_instruction = function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        Instruction::OrdinaryMapOwnedAnyV2ValuesUnpack { .. }
                    )
                })
            });
            if has_instruction {
                return Err(format!(
                    "AnyOwner-v2 Values unpack in '{}' has no private Checker CFG receipt",
                    function.owner.0
                ));
            }
            continue;
        };
        if cfg.owner != function.owner || cfg.profile_version != ProfileVersion::V2 {
            return Err(format!(
                "AnyOwner-v2 Values unpack CFG owner/profile disagrees in '{}'",
                function.owner.0
            ));
        }

        let instructions = function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .collect::<Vec<_>>();
        let unpack_instructions = instructions
            .iter()
            .filter(|instruction| {
                matches!(
                    instruction.kind,
                    Instruction::OrdinaryMapOwnedAnyV2ValuesUnpack { .. }
                )
            })
            .collect::<Vec<_>>();
        if unpack_instructions.len() != cfg.values_unpack_receipts.len() {
            return Err(format!(
                "AnyOwner-v2 Values source/MIR unpack census differs in '{}'",
                function.owner.0
            ));
        }

        for checker in &cfg.values_unpack_receipts {
            if checker.profile_version != ProfileVersion::V2
                || checker.settlement != Settlement::ExplicitDrop
                || checker.conversion_kind
                    != crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
                || checker.conversion_from == checker.conversion_to
                || !observed.insert((function.owner.clone(), checker.cast.clone()))
            {
                return Err(format!(
                    "AnyOwner-v2 Values unpack Checker receipt '{}' has invalid profile, conversion, settlement, or identity",
                    checker.cast.0
                ));
            }
            if !matches!(
                catalog.get(&checker.conversion_from),
                Some(desc) if desc.kind == super::types::MirTypeKind::DynamicAny
            ) {
                return Err(format!(
                    "AnyOwner-v2 Values unpack '{}' does not consume the exact DynamicAny element type",
                    checker.cast.0
                ));
            }

            let values_matches = values_receipts
                .iter()
                .filter(|values| {
                    values.owner == function.owner
                        && values.operation == checker.operation
                        && values.source_local == checker.source_local
                        && values.source_node == checker.source_node
                        && values.checker_source_type == checker.source_type
                        && values.binding_local == checker.snapshot_binding
                        && values.binding_point == checker.snapshot_binding_point
                        && values.checker_result_type == checker.snapshot_type
                        && values.settlement == checker.settlement
                        && values.settlement_point == checker.settlement_point
                        && values.mir_result_type == values_type
                })
                .collect::<Vec<_>>();
            let [values_receipt] = values_matches.as_slice() else {
                return Err(format!(
                    "AnyOwner-v2 Values unpack '{}' is not tied to one exact snapshot and Drop receipt",
                    checker.cast.0
                ));
            };
            values_receipt.validate_against_program(program)?;

            let values_checker_type = catalog
                .get(&values_receipt.checker_result_type)
                .ok_or_else(|| {
                    format!(
                        "AnyOwner-v2 Values unpack '{}' Checker snapshot TypeDesc is absent",
                        checker.cast.0
                    )
                })?;
            let values_element_type = match &values_checker_type.layout {
                super::types::MirLayout::List { element }
                    if values_checker_type.kind == super::types::MirTypeKind::List =>
                {
                    element
                }
                _ => {
                    return Err(format!(
                        "AnyOwner-v2 Values unpack '{}' snapshot is not a canonical List TypeDesc",
                        checker.cast.0
                    ));
                }
            };
            if values_checker_type.kind != super::types::MirTypeKind::List
                || checker.conversion_from != *values_element_type
                || checker.snapshot_type != values_receipt.checker_result_type
                || catalog.get(values_element_type).is_none_or(|descriptor| {
                    descriptor.kind != super::types::MirTypeKind::DynamicAny
                })
            {
                return Err(format!(
                    "AnyOwner-v2 Values unpack '{}' conversion source is not the exact DynamicAny element of its Checker snapshot",
                    checker.cast.0
                ));
            }

            let lineage = ordinary_map_owned_any_v2_values_unpack_selection_lineage(
                function,
                &checker.source_local,
                &checker.conversion_to,
                &selection,
            )
            .ok_or_else(|| {
                format!(
                    "AnyOwner-v2 Values unpack '{}' target was not selected by a Set on the same root lineage",
                    checker.cast.0
                )
            })?;
            if lineage != checker.payload_selection {
                return Err(format!(
                    "AnyOwner-v2 Values unpack '{}' payload-selection lineage differs from its Checker receipt",
                    checker.cast.0
                ));
            }

            let selected = selection
                .selected_types
                .iter()
                .filter(|selected| selected.type_id == checker.conversion_to)
                .collect::<Vec<_>>();
            let [selected] = selected.as_slice() else {
                return Err(format!(
                    "AnyOwner-v2 Values unpack '{}' target is absent or duplicated in the selected TypeDesc table",
                    checker.cast.0
                ));
            };
            let target = catalog.get(&checker.conversion_to).ok_or_else(|| {
                format!(
                    "AnyOwner-v2 Values unpack '{}' target TypeDesc is absent",
                    checker.cast.0
                )
            })?;
            let target_supported = if checker.conversion_to == i32_type {
                target.kind == super::types::MirTypeKind::Primitive(crate::core::PrimitiveType::I32)
                    && target.layout == super::types::MirLayout::Scalar
                    && catalog.validate_copy_scalar(&checker.conversion_to).is_ok()
            } else if checker.conversion_to == string_type {
                catalog
                    .validate_owned_string(&checker.conversion_to)
                    .is_ok()
            } else {
                super::reference::is_canonical_v2_label_type(catalog, &checker.conversion_to)
            };
            if !target_supported || selected.type_desc_digest != digest(target.canonical_text()) {
                return Err(format!(
                    "AnyOwner-v2 Values unpack '{}' target is outside the exact selected i32/String/Label TypeDesc set",
                    checker.cast.0
                ));
            }
            for operation in [
                super::types::MirGlueOperation::Clone,
                super::types::MirGlueOperation::Drop,
            ] {
                catalog.validate_glue(&checker.conversion_to, operation)?;
            }

            let index_desc = catalog.get(&checker.index_type).ok_or_else(|| {
                format!(
                    "AnyOwner-v2 Values unpack '{}' index TypeDesc is absent",
                    checker.cast.0
                )
            })?;
            if checker.index_type != i32_type && checker.index_type != i64_type {
                return Err(format!(
                    "AnyOwner-v2 Values unpack '{}' index is not canonical signed i32/i64",
                    checker.cast.0
                ));
            }
            match index_desc.abi {
                super::types::MirAbiClass::Integer {
                    bits: 32 | 64,
                    signed: true,
                } => {}
                _ => {
                    return Err(format!(
                        "AnyOwner-v2 Values unpack '{}' index ABI is not signed integer",
                        checker.cast.0
                    ));
                }
            }

            let values = values_receipt.binding.clone();
            let index = if let Some(local) = &checker.index_local {
                super::MirValueId::new(format!("local:{}", local.0 .0))
                    .map_err(|error| error.to_string())?
            } else {
                super::MirValueId::new(format!("expr:{}", checker.index_expression.0))
                    .map_err(|error| error.to_string())?
            };
            let result = super::MirValueId::new(format!("expr:{}", checker.cast.0))
                .map_err(|error| error.to_string())?;
            let result_binding =
                super::MirValueId::new(format!("local:{}", checker.unpack_binding.0 .0))
                    .map_err(|error| error.to_string())?;
            let expected_instruction = super::MirInstructionId::new(format!(
                "inst:ordinary_map_owned_any_v2_values_unpack:{}",
                checker.cast.0
            ))
            .map_err(|error| error.to_string())?;
            let candidates = instructions
                .iter()
                .filter_map(|instruction| match &instruction.kind {
                    Instruction::OrdinaryMapOwnedAnyV2ValuesUnpack {
                        result: mir_result,
                        values: mir_values,
                        index: mir_index,
                        projection,
                    } if instruction.id == expected_instruction
                        && mir_result == &result
                        && mir_values == &values
                        && mir_index == &index
                        && projection == &checker.index_projection =>
                    {
                        Some(*instruction)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            let [instruction] = candidates.as_slice() else {
                return Err(format!(
                    "AnyOwner-v2 Values unpack '{}' lacks its unique exact MIR instruction/operands",
                    checker.cast.0
                ));
            };
            let direct_binding_moves = instructions
                .iter()
                .filter(|instruction| {
                    matches!(
                        &instruction.kind,
                        Instruction::Move {
                            result: destination,
                            source,
                        } if destination == &result_binding && source == &result
                    )
                })
                .collect::<Vec<_>>();
            let expected_binding_instruction = super::MirInstructionId::new(format!(
                "inst:bind:{}",
                checker.unpack_binding_point.0
            ))
            .map_err(|error| error.to_string())?;
            if direct_binding_moves.len() != 1
                || direct_binding_moves[0].id != expected_binding_instruction
                || function.values.get(&result_binding).map(|value| &value.ty)
                    != Some(&checker.conversion_to)
                || instructions
                    .iter()
                    .filter(|candidate| {
                        super::mir_instruction_value_uses(&candidate.kind).contains(&result)
                    })
                    .any(|candidate| candidate.id != direct_binding_moves[0].id)
                || function.blocks.values().any(|block| {
                    super::mir_terminator_value_uses(&block.terminator).contains(&result)
                })
            {
                return Err(format!(
                    "AnyOwner-v2 Values unpack '{}' does not have one direct, nonescaping Move at its exact Checker result binding point",
                    checker.cast.0
                ));
            }
            let owner_settlement_instruction = ordinary_map_owned_any_v2_values_unpack_owner_drop(
                function,
                checker,
                &result_binding,
                &expected_binding_instruction,
                &checker.conversion_to,
                catalog,
            )?;
            match checker.result_owner_settlement {
                OwnerSettlement::NoOwnedPayload
                    if checker.result_owner_settlement_point.is_some()
                        || checker.result_owner_settlement_index.is_some() =>
                {
                    return Err(format!(
                        "AnyOwner-v2 Values unpack '{}' Copy result Checker receipt has Drop metadata",
                        checker.cast.0
                    ));
                }
                OwnerSettlement::CompilerOwnedStringDrop
                    if checker.result_owner_settlement_point.is_none()
                        || checker.result_owner_settlement_index.is_some() =>
                {
                    return Err(format!(
                        "AnyOwner-v2 Values unpack '{}' String Checker receipt has source Drop metadata",
                        checker.cast.0
                    ));
                }
                OwnerSettlement::ExplicitWholeDrop
                    if checker.result_owner_settlement_point.is_none()
                        || checker.result_owner_settlement_index.is_none() =>
                {
                    return Err(format!(
                        "AnyOwner-v2 Values unpack '{}' record Checker receipt lacks source Drop metadata",
                        checker.cast.0
                    ));
                }
                _ => {}
            }
            if function.values.get(&values).map(|value| &value.ty) != Some(&values_type)
                || function.values.get(&index).map(|value| &value.ty) != Some(&checker.index_type)
                || function.values.get(&result).map(|value| &value.ty)
                    != Some(&checker.conversion_to)
                || checker.snapshot_type != values_receipt.checker_result_type
            {
                return Err(format!(
                    "AnyOwner-v2 Values unpack '{}' value TypeDescs disagree with Checker receipts",
                    checker.cast.0
                ));
            }
            if !ordinary_map_owned_any_v2_root_selects_payload_type(
                function,
                &checker.source_local,
                &checker.conversion_to,
                &selection,
            ) {
                return Err(format!(
                    "AnyOwner-v2 Values unpack '{}' selected payload is outside the snapshot root lineage",
                    checker.cast.0
                ));
            }

            let receipt = OrdinaryMapOwnedAnyV2ValuesUnpackResultReceipt {
                schema: ORDINARY_MAP_OWNED_ANY_V2_VALUES_UNPACK_RESULT_SCHEMA,
                profile: ORDINARY_MAP_OWNED_ANY_V2_PROFILE,
                owner: function.owner.clone(),
                operation: checker.operation.clone(),
                instruction: instruction.id.clone(),
                values_binding_local: checker.snapshot_binding.clone(),
                values,
                values_binding_point: checker.snapshot_binding_point.clone(),
                projection: checker.index_projection.clone(),
                index_expression: checker.index_expression.clone(),
                index_type: checker.index_type.clone(),
                index,
                cast: checker.cast.clone(),
                conversion_kind: checker.conversion_kind,
                conversion_from: checker.conversion_from.clone(),
                conversion_to: checker.conversion_to.clone(),
                result,
                result_type: checker.conversion_to.clone(),
                result_binding_local: checker.unpack_binding.clone(),
                result_binding_point: checker.unpack_binding_point.clone(),
                result_binding,
                result_owner_settlement: checker.result_owner_settlement,
                result_owner_settlement_point: checker.result_owner_settlement_point.clone(),
                result_owner_settlement_index: checker.result_owner_settlement_index,
                result_owner_settlement_instruction: owner_settlement_instruction,
                settlement: checker.settlement,
                settlement_point: checker.settlement_point.clone(),
                settlement_instruction: values_receipt.settlement_instruction.clone(),
                payload_selection_digest: selection.selection_digest.clone(),
                payload_type_desc_digest: selected.type_desc_digest.clone(),
            };
            receipt.validate()?;
            receipts.push(receipt);
        }

        for instruction in &unpack_instructions {
            if !cfg.values_unpack_receipts.iter().any(|checker| {
                instruction.id.as_str()
                    == format!(
                        "inst:ordinary_map_owned_any_v2_values_unpack:{}",
                        checker.cast.0
                    )
            }) {
                return Err(format!(
                    "unreceipted AnyOwner-v2 Values unpack instruction '{}'",
                    instruction.id
                ));
            }
        }
    }
    if receipts.len() != checker_count || receipts.len() != mir_count {
        return Err(
            "AnyOwner-v2 Values unpack result receipt census has missing or extra source sites"
                .into(),
        );
    }
    receipts.sort_by(|left, right| (&left.owner, &left.cast).cmp(&(&right.owner, &right.cast)));
    Ok(receipts)
}

fn mir_instruction_use_is_settled_on_all_finite_paths(
    function: &super::MirFunction,
    use_block: &super::MirBlockId,
    use_index: usize,
    settlement_block: &super::MirBlockId,
    settlement_index: usize,
) -> bool {
    if use_block == settlement_block {
        return use_index < settlement_index;
    }

    fn reaches_settlement_on_all_finite_paths(
        function: &super::MirFunction,
        block_id: &super::MirBlockId,
        settlement_block: &super::MirBlockId,
        visiting: &mut BTreeSet<super::MirBlockId>,
        memo: &mut BTreeMap<super::MirBlockId, bool>,
    ) -> bool {
        if block_id == settlement_block {
            return true;
        }
        if let Some(result) = memo.get(block_id) {
            return *result;
        }
        // An infinite cycle has no finite escape path that can bypass the
        // settlement. Any exit edge from that cycle is still checked by the
        // normal successor fold below.
        if !visiting.insert(block_id.clone()) {
            return true;
        }
        let result = function.blocks.get(block_id).is_some_and(|block| {
            let successors = match &block.terminator {
                super::MirTerminator::Goto { target, .. } => vec![target.clone()],
                super::MirTerminator::Branch {
                    then_target,
                    else_target,
                    ..
                } => vec![then_target.clone(), else_target.clone()],
                super::MirTerminator::Switch { arms, .. }
                | super::MirTerminator::SwitchMove { arms, .. } => {
                    arms.iter().map(|arm| arm.target.clone()).collect()
                }
                super::MirTerminator::Return { .. }
                | super::MirTerminator::Trap { .. }
                | super::MirTerminator::Fault { .. }
                | super::MirTerminator::Unreachable => Vec::new(),
            };
            !successors.is_empty()
                && successors.iter().all(|successor| {
                    reaches_settlement_on_all_finite_paths(
                        function,
                        successor,
                        settlement_block,
                        visiting,
                        memo,
                    )
                })
        });
        visiting.remove(block_id);
        memo.insert(block_id.clone(), result);
        result
    }

    let Some(block) = function.blocks.get(use_block) else {
        return false;
    };
    let successors = match &block.terminator {
        super::MirTerminator::Goto { target, .. } => vec![target.clone()],
        super::MirTerminator::Branch {
            then_target,
            else_target,
            ..
        } => vec![then_target.clone(), else_target.clone()],
        super::MirTerminator::Switch { arms, .. }
        | super::MirTerminator::SwitchMove { arms, .. } => {
            arms.iter().map(|arm| arm.target.clone()).collect()
        }
        super::MirTerminator::Return { .. }
        | super::MirTerminator::Trap { .. }
        | super::MirTerminator::Fault { .. }
        | super::MirTerminator::Unreachable => Vec::new(),
    };
    if successors.is_empty() {
        return false;
    }
    let mut visiting = BTreeSet::new();
    let mut memo = BTreeMap::new();
    successors.iter().all(|successor| {
        reaches_settlement_on_all_finite_paths(
            function,
            successor,
            settlement_block,
            &mut visiting,
            &mut memo,
        )
    })
}

fn mir_values_binding_has_invalid_post_drop_path(
    function: &super::MirFunction,
    binding: &super::MirValueId,
    binding_source: &super::MirValueId,
    binding_definition: &super::MirInstructionId,
    result_definition: &super::MirInstructionId,
    settlement_instruction: &super::MirInstructionId,
    drop_block_id: &super::MirBlockId,
    drop_index: usize,
) -> bool {
    let Some(drop_block) = function.blocks.get(drop_block_id) else {
        return true;
    };
    let Some(after_drop) = drop_block.instructions.get(drop_index + 1..) else {
        return true;
    };
    let mut state = (false, false);
    for instruction in after_drop {
        if !advance_ordinary_values_binding_state(
            instruction,
            binding,
            binding_source,
            binding_definition,
            result_definition,
            settlement_instruction,
            &mut state,
        ) {
            return true;
        }
    }

    fn successors(terminator: &super::MirTerminator) -> Vec<super::MirBlockId> {
        match terminator {
            super::MirTerminator::Goto { target, .. } => vec![target.clone()],
            super::MirTerminator::Branch {
                then_target,
                else_target,
                ..
            } => vec![then_target.clone(), else_target.clone()],
            super::MirTerminator::Switch { arms, .. }
            | super::MirTerminator::SwitchMove { arms, .. } => {
                arms.iter().map(|arm| arm.target.clone()).collect()
            }
            super::MirTerminator::Return { .. }
            | super::MirTerminator::Trap { .. }
            | super::MirTerminator::Fault { .. }
            | super::MirTerminator::Unreachable => Vec::new(),
        }
    }

    fn path_has_invalid_post_drop_state(
        function: &super::MirFunction,
        block_id: &super::MirBlockId,
        mut state: (bool, bool),
        binding: &super::MirValueId,
        binding_source: &super::MirValueId,
        binding_definition: &super::MirInstructionId,
        result_definition: &super::MirInstructionId,
        settlement_instruction: &super::MirInstructionId,
        active: &mut BTreeSet<(super::MirBlockId, (bool, bool))>,
        memo: &mut BTreeMap<(super::MirBlockId, (bool, bool)), bool>,
    ) -> bool {
        let key = (block_id.clone(), state);
        if let Some(invalid) = memo.get(&key) {
            return *invalid;
        }
        if !active.insert(key.clone()) {
            // A genuine backedge with a live binding or untransferred Values
            // result leaves the owner unsettled indefinitely. Reaching an
            // active key with an empty state is a safe ownership-neutral loop.
            return state.0 || state.1;
        }
        let Some(block) = function.blocks.get(block_id) else {
            active.remove(&key);
            memo.insert(key, true);
            return true;
        };
        for instruction in &block.instructions {
            if !advance_ordinary_values_binding_state(
                instruction,
                binding,
                binding_source,
                binding_definition,
                result_definition,
                settlement_instruction,
                &mut state,
            ) {
                active.remove(&key);
                memo.insert(key, true);
                return true;
            }
        }
        if super::mir_terminator_value_uses(&block.terminator).contains(binding)
            || super::mir_terminator_value_uses(&block.terminator).contains(binding_source)
        {
            active.remove(&key);
            memo.insert(key, true);
            return true;
        }
        let next = successors(&block.terminator);
        let invalid = if next.is_empty() {
            state.0 || state.1
        } else {
            next.iter().any(|successor| {
                path_has_invalid_post_drop_state(
                    function,
                    successor,
                    state,
                    binding,
                    binding_source,
                    binding_definition,
                    result_definition,
                    settlement_instruction,
                    active,
                    memo,
                )
            })
        };
        active.remove(&key);
        memo.insert(key, invalid);
        invalid
    }

    let next = successors(&drop_block.terminator);
    if next.is_empty() {
        return state.0 || state.1;
    }
    let mut active = BTreeSet::new();
    let mut memo = BTreeMap::new();
    next.iter().any(|successor| {
        path_has_invalid_post_drop_state(
            function,
            successor,
            state,
            binding,
            binding_source,
            binding_definition,
            result_definition,
            settlement_instruction,
            &mut active,
            &mut memo,
        )
    })
}

fn advance_ordinary_values_binding_state(
    instruction: &super::MirInstruction,
    binding: &super::MirValueId,
    binding_source: &super::MirValueId,
    binding_definition: &super::MirInstructionId,
    result_definition: &super::MirInstructionId,
    settlement_instruction: &super::MirInstructionId,
    state: &mut (bool, bool),
) -> bool {
    use super::MirInstructionKind as Instruction;

    if matches!(&instruction.kind, Instruction::Move { result, .. } if result == binding) {
        let exact_binding_move = instruction.id == *binding_definition
            && matches!(
                &instruction.kind,
                Instruction::Move { result, source }
                    if result == binding && source == binding_source
            );
        if !exact_binding_move || !state.1 || state.0 {
            return false;
        }
        state.0 = true;
        state.1 = false;
        return true;
    }

    let is_values_result = instruction.id == *result_definition
        && matches!(
            &instruction.kind,
            Instruction::OrdinaryMapOwnedAnyV2Values { result, .. }
                if result == binding_source
        );
    if is_values_result {
        if state.1 {
            return false;
        }
        state.1 = true;
        return true;
    }

    if super::mir_instruction_value_uses(&instruction.kind).contains(binding_source) {
        return false;
    }
    if matches!(&instruction.kind, Instruction::Drop { value } if value == binding) {
        if instruction.id != *settlement_instruction || !state.0 {
            return false;
        }
        state.0 = false;
        return true;
    }
    if super::mir_instruction_value_uses(&instruction.kind).contains(binding) {
        if !state.0 {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod ordinary_values_post_drop_tests {
    use super::*;

    #[test]
    fn accepts_live_binding_branch_join_when_every_path_reaches_drop() {
        use crate::core::mir::{
            MirBlock, MirEdgeId, MirInstruction, MirInstructionId, MirInstructionKind,
            MirTerminator, MirValueId,
        };

        let block_id = |id: &str| super::super::MirBlockId::new(id).expect("canonical block id");
        let edge_id = |id: &str| MirEdgeId::new(id).expect("canonical edge id");
        let instruction_id =
            |id: &str| MirInstructionId::new(id).expect("canonical instruction id");
        let value_id = |id: &str| MirValueId::new(id).expect("canonical value id");

        let join = block_id("block:values_drop_join");
        let create = block_id("block:values_create_owner");
        let left = block_id("block:values_drop_left");
        let right = block_id("block:values_drop_right");
        let root = value_id("local:map");
        let values_result = value_id("expr:values");
        let binding = value_id("local:snapshot");
        let condition = value_id("value:condition");
        let values_instruction = instruction_id("inst:values");
        let binding_move = instruction_id("inst:binding_move");
        let settlement = instruction_id("inst:binding_drop");

        let mut blocks = BTreeMap::new();
        blocks.insert(
            join.clone(),
            MirBlock {
                id: join.clone(),
                parameters: Vec::new(),
                instructions: vec![MirInstruction {
                    id: settlement.clone(),
                    kind: MirInstructionKind::Drop {
                        value: binding.clone(),
                    },
                }],
                terminator: MirTerminator::Goto {
                    edge: edge_id("edge:drop_to_create"),
                    target: create.clone(),
                    arguments: Vec::new(),
                },
            },
        );
        blocks.insert(
            create.clone(),
            MirBlock {
                id: create.clone(),
                parameters: Vec::new(),
                instructions: vec![
                    MirInstruction {
                        id: values_instruction.clone(),
                        kind: MirInstructionKind::OrdinaryMapOwnedAnyV2Values {
                            result: values_result.clone(),
                            root,
                        },
                    },
                    MirInstruction {
                        id: binding_move.clone(),
                        kind: MirInstructionKind::Move {
                            result: binding.clone(),
                            source: values_result.clone(),
                        },
                    },
                ],
                terminator: MirTerminator::Branch {
                    condition,
                    then_edge: edge_id("edge:create_left"),
                    then_target: left.clone(),
                    then_arguments: Vec::new(),
                    else_edge: edge_id("edge:create_right"),
                    else_target: right.clone(),
                    else_arguments: Vec::new(),
                },
            },
        );
        for (id, edge) in [(&left, "edge:left_to_join"), (&right, "edge:right_to_join")] {
            blocks.insert(
                id.clone(),
                MirBlock {
                    id: id.clone(),
                    parameters: Vec::new(),
                    instructions: Vec::new(),
                    terminator: MirTerminator::Goto {
                        edge: edge_id(edge),
                        target: join.clone(),
                        arguments: Vec::new(),
                    },
                },
            );
        }

        let function = crate::core::mir::MirFunction {
            owner: NodeId("function:values_path_test".into()),
            parameters: Vec::new(),
            parameter_permissions: Some(Vec::new()),
            result: crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Unit),
            entry: join.clone(),
            values: BTreeMap::new(),
            blocks,
            contracts: Vec::new(),
            proof_worklist: None,
            ownership: crate::core::mir::MirOwnershipSummary::default(),
        };

        assert!(!mir_values_binding_has_invalid_post_drop_path(
            &function,
            &binding,
            &values_result,
            &binding_move,
            &values_instruction,
            &settlement,
            &join,
            0,
        ));
    }
}

fn push_frame(output: &mut String, value: &str) {
    use std::fmt::Write as _;
    write!(output, "{}:", value.len()).expect("writing a String cannot fail");
    output.push_str(value);
    output.push('\n');
}

fn is_lower_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

impl CanonicalMirRouteReceipt {
    /// Compare the immutable semantic identity carried by two route receipts.
    ///
    /// The consumer profile is invocation provenance (for example, native,
    /// bytecode, or verifier) rather than program identity, so it is
    /// intentionally excluded.  All schema, digest, and root-owner fields
    /// remain part of the comparison; a receipt from another canonical MIR
    /// graph must never be accepted as an equivalent replay.
    pub fn same_semantic_identity(&self, other: &CanonicalMirRouteReceipt) -> bool {
        self.schema == other.schema
            && self.mir_digest == other.mir_digest
            && self.type_desc_digest == other.type_desc_digest
            && self.abi_digest == other.abi_digest
            && self.ffi_digest == other.ffi_digest
            && self.ownership_digest == other.ownership_digest
            && self.flow_transition_digest == other.flow_transition_digest
            && self.root_owners == other.root_owners
    }

    /// Return the deterministic digest used when a consumer needs a cache
    /// identity for this receipt.  The profile remains excluded for the same
    /// reason as [`Self::same_semantic_identity`]: it records which consumer
    /// admitted the graph, while the framed schema/digest/owner tuple is the
    /// reusable semantic witness.  Length framing keeps owner identities from
    /// creating delimiter collisions.
    pub fn semantic_identity_digest(&self) -> String {
        let mut framed = String::from("mimi-proof-mir-route-cache-v1");
        let mut frame = |value: &str| {
            use std::fmt::Write as _;
            write!(framed, "\n{}:", value.len()).expect("writing a String cannot fail");
            framed.push_str(value);
        };
        frame(self.schema);
        frame(&self.mir_digest);
        frame(&self.type_desc_digest);
        frame(&self.abi_digest);
        frame(&self.ffi_digest);
        frame(&self.ownership_digest);
        frame(&self.flow_transition_digest);
        for owner in &self.root_owners {
            frame(&owner.0);
        }
        blake3::hash(framed.as_bytes()).to_hex().to_string()
    }

    /// Validate the invariants required before a receipt is rendered as an
    /// evidence manifest. This remains a pure receipt check: it does not
    /// inspect source AST, infer types, or consult a backend.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != MIR_ROUTE_RECEIPT_SCHEMA {
            return Err(format!(
                "unexpected route receipt schema '{}', expected '{}'",
                self.schema, MIR_ROUTE_RECEIPT_SCHEMA
            ));
        }
        if !manifest_atom_is_safe(&self.profile, false) {
            return Err("route receipt profile is empty or not manifest-safe".into());
        }
        for (name, value) in [
            ("mir_digest", self.mir_digest.as_str()),
            ("type_desc_digest", self.type_desc_digest.as_str()),
            ("abi_digest", self.abi_digest.as_str()),
            ("ffi_digest", self.ffi_digest.as_str()),
            ("ownership_digest", self.ownership_digest.as_str()),
            (
                "flow_transition_digest",
                self.flow_transition_digest.as_str(),
            ),
        ] {
            if value.len() != 64
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return Err(format!(
                    "route receipt {name} must be a 64-character lowercase hex digest"
                ));
            }
        }
        if self
            .root_owners
            .iter()
            .any(|owner| !manifest_atom_is_safe(&owner.0, true))
        {
            return Err("route receipt root owner is empty or not manifest-safe".into());
        }
        if self
            .root_owners
            .windows(2)
            .any(|owners| matches!(owners, [left, right] if left >= right))
        {
            return Err("route receipt root owners must be strictly sorted".into());
        }
        Ok(())
    }

    /// Validate this receipt against the immutable MIR graph it claims to
    /// describe. This compares the whole identity tuple, including the FFI
    /// and ABI sub-digests, so consumers cannot accept a locally edited
    /// receipt merely because its top-level digest was left unchanged.
    pub fn validate_against_program(&self, program: &MirProgram) -> Result<(), String> {
        self.validate()?;
        let actual = program.route_receipt(self.profile.clone());
        for (field, expected, observed) in [
            (
                "mir_digest",
                self.mir_digest.as_str(),
                actual.mir_digest.as_str(),
            ),
            (
                "type_desc_digest",
                self.type_desc_digest.as_str(),
                actual.type_desc_digest.as_str(),
            ),
            (
                "abi_digest",
                self.abi_digest.as_str(),
                actual.abi_digest.as_str(),
            ),
            (
                "ffi_digest",
                self.ffi_digest.as_str(),
                actual.ffi_digest.as_str(),
            ),
            (
                "ownership_digest",
                self.ownership_digest.as_str(),
                actual.ownership_digest.as_str(),
            ),
            (
                "flow_transition_digest",
                self.flow_transition_digest.as_str(),
                actual.flow_transition_digest.as_str(),
            ),
        ] {
            if expected != observed {
                return Err(format!(
                    "route receipt {field} {expected} does not match MIR input {observed}"
                ));
            }
        }
        if self.root_owners != actual.root_owners {
            return Err(format!(
                "route receipt root_owners {:?} do not match MIR input {:?}",
                self.root_owners, actual.root_owners
            ));
        }
        Ok(())
    }

    /// Compare this evidence manifest with a checker-owned receipt.
    ///
    /// Both values are validated before comparison so a structurally malformed
    /// manifest cannot be accepted merely because it happens to differ from
    /// the expected route.  Returning the changed field names gives CLI and
    /// evidence consumers one fail-closed comparison path without requiring
    /// them to duplicate receipt identity knowledge.
    pub fn verify_against_receipt(
        &self,
        expected: &CanonicalMirRouteReceipt,
    ) -> Result<(), String> {
        self.validate()
            .map_err(|error| format!("invalid route receipt: {error}"))?;
        expected
            .validate()
            .map_err(|error| format!("invalid expected route receipt: {error}"))?;
        let mut mismatches = Vec::new();
        if self.schema != expected.schema {
            mismatches.push("schema");
        }
        if self.profile != expected.profile {
            mismatches.push("profile");
        }
        if self.mir_digest != expected.mir_digest {
            mismatches.push("mir_digest");
        }
        if self.type_desc_digest != expected.type_desc_digest {
            mismatches.push("type_desc_digest");
        }
        if self.abi_digest != expected.abi_digest {
            mismatches.push("abi_digest");
        }
        if self.ffi_digest != expected.ffi_digest {
            mismatches.push("ffi_digest");
        }
        if self.ownership_digest != expected.ownership_digest {
            mismatches.push("ownership_digest");
        }
        if self.flow_transition_digest != expected.flow_transition_digest {
            mismatches.push("flow_transition_digest");
        }
        if self.root_owners != expected.root_owners {
            mismatches.push("root_owners");
        }
        if mismatches.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "route receipt does not match expected checker receipt: {}",
                mismatches.join(", ")
            ))
        }
    }

    /// Render the validated receipt as the stable, line-oriented manifest
    /// shared by the CLI and evidence consumers.  Keeping field lookup beside
    /// the public field-order constant prevents a frontend or CLI edit from
    /// silently reordering or omitting a receipt value.
    pub fn manifest_text(&self) -> Result<String, String> {
        validate_manifest_field_schema()
            .map_err(|error| format!("invalid MIR route receipt: {error}"))?;
        self.validate()
            .map_err(|error| format!("invalid MIR route receipt: {error}"))?;
        let mut text = String::from(MIR_ROUTE_RECEIPT_MANIFEST_HEADER);
        text.push('\n');
        for field in MIR_ROUTE_RECEIPT_MANIFEST_FIELDS {
            let value = self.manifest_value(field).ok_or_else(|| {
                format!("invalid MIR route receipt: unknown manifest field '{field}'")
            })?;
            writeln!(text, "{field}={value}")
                .map_err(|_| "failed to render MIR route receipt manifest".to_string())?;
        }
        Ok(text)
    }

    /// Render and parse this receipt through the canonical evidence-manifest
    /// boundary, returning the reconstructed value only when the public
    /// fields survive the round trip exactly.  Every backend that admits a
    /// manifest can use this helper instead of maintaining its own pair of
    /// serializer/parser calls.
    pub fn manifest_round_trip(&self) -> Result<Self, String> {
        let manifest = self
            .manifest_text()
            .map_err(|error| format!("render: {error}"))?;
        let replayed = Self::from_manifest(&manifest).map_err(|error| format!("parse: {error}"))?;
        if replayed != *self {
            return Err("route receipt changed during manifest replay".into());
        }
        Ok(replayed)
    }

    /// Parse a line-oriented manifest and replay it through the canonical
    /// serializer/parser boundary before admitting the receipt.  This is the
    /// fallible counterpart used by AST-free consumers that receive a manifest
    /// directly instead of an already typed receipt.
    pub fn from_manifest_round_trip(text: &str) -> Result<Self, String> {
        let receipt = Self::from_manifest(text)?;
        receipt.manifest_round_trip()
    }

    /// Parse the line-oriented manifest emitted by [`Self::manifest_text`].
    ///
    /// This is intentionally a strict parser for evidence consumers: the
    /// versioned header, exact field set, field order, duplicate/unknown rows,
    /// and public value semantics are all checked before a map is returned.
    /// This keeps a parser from blessing a partially trusted receipt.
    pub fn parse_manifest(text: &str) -> Result<BTreeMap<String, String>, String> {
        validate_manifest_field_schema()
            .map_err(|error| format!("invalid MIR route manifest: {error}"))?;
        let mut lines = text.lines();
        if lines.next() != Some(MIR_ROUTE_RECEIPT_MANIFEST_HEADER) {
            return Err(format!(
                "invalid MIR route manifest: expected header '{MIR_ROUTE_RECEIPT_MANIFEST_HEADER}'"
            ));
        }

        let mut entries = BTreeMap::new();
        let mut seen = std::collections::BTreeSet::new();
        for (row, expected_field) in MIR_ROUTE_RECEIPT_MANIFEST_FIELDS.iter().enumerate() {
            let line = lines.next().ok_or_else(|| {
                format!("invalid MIR route manifest: missing field '{expected_field}'")
            })?;
            let (field, value) = line
                .split_once('=')
                .ok_or_else(|| format!("invalid MIR route manifest: row {row} is missing '='"))?;
            if !MIR_ROUTE_RECEIPT_MANIFEST_FIELDS.contains(&field) {
                return Err(format!(
                    "invalid MIR route manifest: unknown field '{field}' at row {row}"
                ));
            }
            if !seen.insert(field) {
                return Err(format!(
                    "invalid MIR route manifest: duplicate field '{field}' at row {row}"
                ));
            }
            if field != *expected_field {
                return Err(format!(
                    "invalid MIR route manifest: field '{field}' at row {row}, expected '{expected_field}'"
                ));
            }
            entries.insert(field.to_owned(), value.to_owned());
        }
        if let Some(line) = lines.next() {
            let field = line.split_once('=').map_or(line, |(field, _)| field);
            if MIR_ROUTE_RECEIPT_MANIFEST_FIELDS.contains(&field) {
                return Err(format!(
                    "invalid MIR route manifest: duplicate field '{field}' at row {}",
                    MIR_ROUTE_RECEIPT_MANIFEST_FIELDS.len()
                ));
            }
            return Err(format!(
                "invalid MIR route manifest: unknown field '{field}' at row {}",
                MIR_ROUTE_RECEIPT_MANIFEST_FIELDS.len()
            ));
        }
        validate_manifest_values(&entries)?;
        Ok(entries)
    }

    /// Parse and reconstitute a validated route receipt from its manifest.
    ///
    /// The conversion is deliberately versioned and lossless for the public
    /// receipt fields. A caller that needs to compare a CLI snapshot with a
    /// checked API receipt can therefore round-trip through one canonical
    /// value validator instead of duplicating digest or owner parsing.
    pub fn from_manifest(text: &str) -> Result<Self, String> {
        let entries = Self::parse_manifest(text)?;
        let profile = entries
            .get("profile")
            .cloned()
            .ok_or_else(|| "invalid MIR route manifest: missing field 'profile'".to_string())?;
        let digest = |field: &str| {
            entries
                .get(field)
                .cloned()
                .ok_or_else(|| format!("invalid MIR route manifest: missing field '{field}'"))
        };
        let root_owners = entries
            .get("root_owners")
            .ok_or_else(|| "invalid MIR route manifest: missing field 'root_owners'".to_string())?
            .split(',')
            .filter(|owner| !owner.is_empty())
            .map(|owner| NodeId(owner.to_owned()))
            .collect();
        Ok(Self {
            schema: MIR_ROUTE_RECEIPT_SCHEMA,
            profile,
            mir_digest: digest("mir_digest")?,
            type_desc_digest: digest("type_desc_digest")?,
            abi_digest: digest("abi_digest")?,
            ffi_digest: digest("ffi_digest")?,
            ownership_digest: digest("ownership_digest")?,
            flow_transition_digest: digest("flow_transition_digest")?,
            root_owners,
        })
    }

    fn manifest_value(&self, field: &str) -> Option<String> {
        Some(match field {
            "schema" => self.schema.to_owned(),
            "profile" => self.profile.clone(),
            "mir_digest" => self.mir_digest.clone(),
            "type_desc_digest" => self.type_desc_digest.clone(),
            "abi_digest" => self.abi_digest.clone(),
            "ffi_digest" => self.ffi_digest.clone(),
            "ownership_digest" => self.ownership_digest.clone(),
            "flow_transition_digest" => self.flow_transition_digest.clone(),
            "root_owners" => self
                .root_owners
                .iter()
                .map(|owner| owner.0.as_str())
                .collect::<Vec<_>>()
                .join(","),
            _ => return None,
        })
    }
}

fn validate_manifest_field_schema() -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for field in MIR_ROUTE_RECEIPT_MANIFEST_FIELDS {
        if field.trim().is_empty()
            || field
                .chars()
                .any(|character| character.is_control() || character == '=')
        {
            return Err(format!(
                "manifest field '{field}' is empty or not manifest-safe"
            ));
        }
        if !seen.insert(field) {
            return Err(format!("manifest field '{field}' is duplicated"));
        }
    }
    Ok(())
}

/// Validate one scalar value before it crosses the line-oriented manifest or
/// canonical receipt boundary.  Profile values are standalone fields and may
/// contain commas; root-owner values are joined with commas and therefore
/// reject both delimiters.  Whitespace is rejected for both so a value cannot
/// acquire a different tokenization in a consumer that splits canonical text.
fn manifest_atom_is_safe(value: &str, reject_comma: bool) -> bool {
    !value.trim().is_empty()
        && !value.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || character == '='
                || (reject_comma && character == ',')
        })
}

fn validate_manifest_values(entries: &BTreeMap<String, String>) -> Result<(), String> {
    let schema = entries
        .get("schema")
        .ok_or_else(|| "invalid MIR route manifest: missing field 'schema'".to_string())?;
    if schema != MIR_ROUTE_RECEIPT_SCHEMA {
        return Err(format!(
            "invalid MIR route manifest: field 'schema' has unexpected value '{schema}'"
        ));
    }
    let profile = entries
        .get("profile")
        .ok_or_else(|| "invalid MIR route manifest: missing field 'profile'".to_string())?;
    if !manifest_atom_is_safe(profile, false) {
        return Err(
            "invalid MIR route manifest: field 'profile' is empty or not manifest-safe".into(),
        );
    }
    for field in [
        "mir_digest",
        "type_desc_digest",
        "abi_digest",
        "ffi_digest",
        "ownership_digest",
        "flow_transition_digest",
    ] {
        let value = entries
            .get(field)
            .ok_or_else(|| format!("invalid MIR route manifest: missing field '{field}'"))?;
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(format!(
                "invalid MIR route manifest: field '{field}' must be a 64-character lowercase hex digest"
            ));
        }
    }
    let owners = entries
        .get("root_owners")
        .ok_or_else(|| "invalid MIR route manifest: missing field 'root_owners'".to_string())?;
    let owners = if owners.is_empty() {
        Vec::new()
    } else {
        owners
            .split(',')
            .map(|owner| {
                if !manifest_atom_is_safe(owner, true) {
                    return Err(
                        "invalid MIR route manifest: root owner is empty or not manifest-safe"
                            .to_string(),
                    );
                }
                Ok(NodeId(owner.to_owned()))
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    if owners
        .windows(2)
        .any(|window| matches!(window, [left, right] if left >= right))
    {
        return Err("invalid MIR route manifest: root owners must be strictly sorted".into());
    }
    Ok(())
}

fn canonical_mir_text(program: &MirProgram) -> String {
    let mut text = String::new();
    text.push_str(MIR_IDENTITY_SCHEMA);
    text.push('\n');
    text.push_str(&program.type_catalog().canonical_text());
    text.push_str(&program.actor_contract_text());
    text.push_str(&program.export_contract_text());
    for instance in program.instances().values() {
        text.push_str("mir.instance ");
        text.push_str(instance.id.as_str());
        text.push(' ');
        text.push_str(instance.template.0.as_str());
        text.push_str(" -> ");
        text.push_str(instance.function.0.as_str());
        text.push('<');
        for (index, argument) in instance.arguments.iter().enumerate() {
            if index != 0 {
                text.push(',');
            }
            text.push_str(argument.as_str());
        }
        text.push_str("> contract=");
        text.push_str(&crate::core::mir::canonical_instance_contract_text(
            &instance.contract,
        ));
        text.push('\n');
    }
    for transition in program.transitions().values() {
        text.push_str(&transition.canonical_text());
    }
    text.push_str(&canonical_ffi_text(program));
    text.push_str(&super::nested_callable::canonical_nested_callable_scope_text(program));
    for receipt in program.closure_callable_receipts().values() {
        text.push_str(&receipt.canonical_text());
    }
    for function in program.functions().values() {
        text.push_str(&function.canonical_text());
    }
    text
}

fn canonical_ffi_text(program: &MirProgram) -> String {
    let mut text = String::new();
    // Receipt identity must not depend on diagnostic provenance.  Consumers
    // still use `ffi_call_entries_in_source_order` when they need source-order
    // output, but the semantic digest uses only stable call-site identities so
    // a span remap cannot turn into a MIR identity change.
    let mut entries = program.ffi_calls().iter().collect::<Vec<_>>();
    entries.sort_by(|(left_key, left), (right_key, right)| {
        left.caller
            .cmp(&right.caller)
            .then_with(|| left.instruction.cmp(&right.instruction))
            .then_with(|| left_key.cmp(right_key))
    });
    for (instruction, contract) in entries {
        text.push_str("mir.ffi ");
        // Valid canonical programs keep the table key and checker-owned
        // receipt identity equal, so this marker is absent and preserves the
        // established digest.  A test-only forged table may change just the
        // key while leaving the receipt body untouched; retain that mismatch
        // in the digest instead of silently collapsing two table identities.
        if instruction != &contract.instruction {
            text.push_str("key_mismatch=");
            text.push_str(instruction.as_str());
            text.push(' ');
        }
        text.push_str(&contract.canonical_text());
        text.push('\n');
    }
    text
}

fn canonical_ownership_text(program: &MirProgram) -> String {
    let mut text = String::new();
    for function in program.functions().values() {
        text.push_str(&function.owner.0);
        text.push('\n');
        text.push_str(&function.ownership.canonical_text());
    }
    text
}

fn canonical_transition_text(program: &MirProgram) -> String {
    let mut text = String::from("mimi-flow-transition-contract-v1\n");
    for transition in program.transitions().values() {
        text.push_str(&transition.canonical_text());
    }
    text
}

fn canonical_root_owners(program: &MirProgram) -> Vec<NodeId> {
    let mut owners = program
        .functions()
        .keys()
        .chain(program.transitions().keys())
        .cloned()
        .collect::<Vec<_>>();
    owners.sort();
    // Flow transition bodies are executable MIR functions and are also
    // represented by transition contracts.  The receipt exposes root owners
    // as a set, so the shared identity must not repeat that owner merely
    // because it has two canonical tables.
    owners.dedup();
    owners
}

fn digest(text: String) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ordinary_map_v2_program() -> MirProgram {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mir_ordinary_map_any_owner_v2_record.mimi");
        let source =
            include_str!("../../../tests/fixtures/mir_ordinary_map_any_owner_v2_record.mimi");
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("V2 fixture lexes");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("V2 fixture parses");
        let mut loader = crate::loader::ModuleLoader::new(
            path.parent()
                .expect("V2 fixture has a parent directory")
                .to_path_buf(),
        );
        loader
            .load_main_with_file(&path, file)
            .expect("V2 fixture imports load");
        let mut file = loader.merge_all().expect("V2 fixture imports merge");
        crate::loader::merge_prelude_into(&mut file);
        let checked = crate::core::check_program(&file).expect("V2 fixture checks");
        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect("V2 route materializes");
        let materialized_receipt = route
            .ordinary_map_owned_any_v2_payload_selection
            .as_ref()
            .expect("materialized route carries its Core-owned V2 selection");
        assert!(materialized_receipt
            .validate_against_program(&route.program)
            .is_ok());
        route.program
    }

    fn ordinary_map_v2_temporary_root_program() -> MirProgram {
        let source = r#"
func main() -> i32 {
    let pair = map_get(map_new(), "missing")
    drop(pair)
    0
}
"#;
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("temporary-root fixture lexes");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("temporary-root fixture parses");
        let checked = crate::core::check_program(&file).expect("temporary-root fixture checks");
        crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect("temporary-root MIR route materializes")
            .program
    }

    fn ordinary_map_v2_if_diamond_program() -> MirProgram {
        let source = r#"
func main() -> i32 {
    let root = map_new()
    let pair = map_get(root, "missing")
    let snapshot = values(root)
    let flag = true
    let observed = if flag {
        map_size(root);
        1
    } else {
        map_size(root);
        2
    }
    drop(pair)
    drop(snapshot)
    drop(root)
    observed
}
"#;
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("V2 if diamond fixture lexes");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("V2 if diamond fixture parses");
        let checked = crate::core::check_program(&file).expect("V2 if diamond fixture checks");
        let catalog = super::super::types::MirTypeCatalog::from_checked_program(&checked)
            .expect("V2 if diamond TypeDesc catalog");
        let functions =
            crate::core::mir::lower::lower_program_with_type_catalog(&checked, &catalog)
                .expect("Checker-receipted V2 diamond lowering");
        MirProgram::with_type_catalog(functions, catalog).expect("V2 if diamond MIR validates")
    }

    fn ordinary_map_v2_unpack_program(source: &str) -> MirProgram {
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("V2 unpack fixture lexes");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("V2 unpack fixture parses");
        let checked = crate::core::check_program(&file).expect("V2 unpack fixture checks");
        let catalog = super::super::types::MirTypeCatalog::from_checked_program(&checked)
            .expect("V2 unpack fixture TypeDesc catalog");
        let functions =
            crate::core::mir::lower::lower_program_with_type_catalog(&checked, &catalog)
                .expect("Checker-receipted V2 unpack lowering");
        MirProgram::with_type_catalog(functions, catalog).expect("V2 unpack MIR validates")
    }

    fn valid_receipt() -> CanonicalMirRouteReceipt {
        let digest = "a".repeat(64);
        CanonicalMirRouteReceipt {
            schema: MIR_ROUTE_RECEIPT_SCHEMA,
            profile: "test-v1".into(),
            mir_digest: digest.clone(),
            type_desc_digest: digest.clone(),
            abi_digest: digest.clone(),
            ffi_digest: digest.clone(),
            ownership_digest: digest.clone(),
            flow_transition_digest: digest,
            root_owners: vec![NodeId("function:main".into()), NodeId("function:z".into())],
        }
    }

    #[test]
    fn ordinary_map_v2_payload_selection_is_checker_derived_and_stable() {
        let program = ordinary_map_v2_program();
        let receipt = program
            .ordinary_map_owned_any_v2_payload_selection_receipt()
            .expect("Checker-derived selection validates")
            .expect("V2 Set actions produce a selection");

        assert_eq!(
            receipt.schema,
            ORDINARY_MAP_OWNED_ANY_V2_PAYLOAD_SELECTION_SCHEMA
        );
        assert_eq!(receipt.profile, "ordinary-map-owned-any-v2");
        assert_eq!(receipt.selected_types.len(), 2);
        assert!(receipt
            .selected_types
            .windows(2)
            .all(|pair| pair[0].type_id < pair[1].type_id));
        assert!(receipt.validate().is_ok());
        assert!(receipt.validate_against_program(&program).is_ok());
        assert_eq!(
            receipt,
            program
                .ordinary_map_owned_any_v2_payload_selection_receipt()
                .expect("second derivation validates")
                .expect("V2 Set actions still produce a selection")
        );
        assert_eq!(
            digest(receipt.canonical_text()),
            receipt.selection_digest,
            "the selection digest covers the deterministic canonical text"
        );
    }

    #[test]
    fn ordinary_map_v2_mir_replays_diamond_owner_sets_and_rejects_forged_flow_facts() {
        let program = ordinary_map_v2_if_diamond_program();
        let function = program
            .functions()
            .values()
            .find(|function| {
                function
                    .ownership
                    .checker_ordinary_map_owned_any_v2_cfg_receipt
                    .as_ref()
                    .and_then(|receipt| receipt.flow.as_ref())
                    .is_some()
            })
            .expect("MIR carries a dedicated V2 diamond CFG proof");
        function
            .validate()
            .expect("source and MIR owner facts replay equally");

        let mut forged = function.clone();
        let flow = forged
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_mut()
            .expect("Checker V2 receipt")
            .flow
            .as_mut()
            .expect("Checker V2 flow receipt");
        let edge = flow.arm_edges.first().expect("one split edge").clone();
        flow.live_owners_on_edge.insert(edge, BTreeSet::new());
        let errors = forged
            .validate()
            .expect_err("a forged edge owner set fails independent MIR replay");
        assert!(errors.iter().any(|error| error
            .message
            .contains("ordinary Map/Any v2 CFG flow receipt")));
    }

    #[test]
    fn ordinary_map_v2_mir_rejects_action_moved_to_sibling_arm_with_resealed_body() {
        let program = ordinary_map_v2_if_diamond_program();
        let function = program
            .functions()
            .values()
            .find(|function| {
                function
                    .ownership
                    .checker_ordinary_map_owned_any_v2_cfg_receipt
                    .as_ref()
                    .and_then(|receipt| receipt.flow.as_ref())
                    .is_some()
            })
            .expect("MIR carries a dedicated V2 diamond CFG proof");
        let action = function
            .ownership
            .checker_ordinary_map_owned_any_v2
            .iter()
            .find(|action| action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::Size)
            .expect("read-only arm action has an instruction receipt")
            .clone();
        let arm_blocks = function
            .blocks
            .keys()
            .filter(|block| {
                block.as_str().contains(":if.then:") || block.as_str().contains(":if.else:")
            })
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(arm_blocks.len(), 2, "MIR carries both source arms");
        let source_block = function
            .blocks
            .values()
            .find(|block| {
                block
                    .instructions
                    .iter()
                    .any(|instruction| instruction.id == action.instruction)
            })
            .expect("receipted size instruction has a MIR block")
            .id
            .clone();
        let target_block = arm_blocks
            .iter()
            .find(|block| **block != source_block)
            .expect("the sibling arm differs from the source arm")
            .clone();
        let mut displaced = function.clone();
        let instruction_origins = displaced
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("ManagedAny full-body seal")
            .instructions
            .iter()
            .map(|(_, origin, instruction)| (instruction.id.clone(), origin.clone()))
            .collect::<BTreeMap<_, _>>();
        let instruction = {
            let block = displaced
                .blocks
                .get_mut(&source_block)
                .expect("source arm block exists");
            let index = block
                .instructions
                .iter()
                .position(|instruction| instruction.id == action.instruction)
                .expect("size instruction remains in source arm");
            block.instructions.remove(index)
        };
        displaced
            .blocks
            .get_mut(&target_block)
            .expect("sibling arm block exists")
            .instructions
            .push(instruction);
        let resealed_instructions = displaced
            .blocks
            .values()
            .flat_map(|block| {
                block.instructions.iter().map(|instruction| {
                    (
                        block.id.clone(),
                        instruction_origins[&instruction.id].clone(),
                        instruction.clone(),
                    )
                })
            })
            .collect();
        displaced
            .ownership
            .checker_managed_any
            .as_mut()
            .expect("ManagedAny full-body seal")
            .instructions = resealed_instructions;

        let errors = displaced
            .validate()
            .expect_err("action placement no longer agrees with its Checker source point");
        assert!(
            errors.iter().any(|error| error.message.contains(
                "action instruction is not in the MIR block mapped from its Checker source point"
            )),
            "unexpected validation errors: {errors:#?}"
        );
    }

    #[test]
    fn ordinary_map_v2_record_unpack_receipt_exposes_exact_selected_type_and_provenance() {
        let program = ordinary_map_v2_unpack_program(
            r#"
type Label { text: string }
func main() -> i32 {
    let label = Label { text: "selected label" }
    let empty = map_new()
    let root = map_set(empty, "label", label)
    let pair = map_get(root, "label")
    let value = pair.1 as Label
    drop(pair)
    drop(empty)
    drop(root)
    drop(label)
    drop(value)
    0
}
"#,
        );
        let selection = program
            .ordinary_map_owned_any_v2_payload_selection_receipt()
            .expect("Set selection validates")
            .expect("Label Set selects its exact TypeDesc");
        let label_types = selection
            .selected_types
            .iter()
            .filter(|selected| {
                super::super::reference::is_canonical_v2_label_type(
                    program.type_catalog(),
                    &selected.type_id,
                )
            })
            .collect::<Vec<_>>();
        let [label] = label_types.as_slice() else {
            panic!("one exact single-field Label TypeDesc is selected");
        };
        let receipts = program
            .ordinary_map_owned_any_v2_unpack_result_receipts()
            .expect("Core derives exact Record unpack receipt");
        let [unpack] = receipts.as_slice() else {
            panic!("one Record unpack receipt");
        };
        assert_eq!(
            unpack.payload_source,
            OrdinaryMapOwnedAnyV2UnpackPayloadSource::SelectedSetPayload
        );
        assert_eq!(unpack.conversion_to, label.type_id);
        assert_eq!(unpack.payload_type_desc_digest, label.type_desc_digest);
        assert_eq!(unpack.payload_selection_digest, selection.selection_digest);
        assert!(unpack.validate_against_program(&program).is_ok());

        let mut forged_miss = unpack.clone();
        forged_miss.payload_source = OrdinaryMapOwnedAnyV2UnpackPayloadSource::CanonicalI32Miss;
        assert!(forged_miss
            .validate()
            .expect_err("a Record target cannot claim canonical i32 miss authority")
            .contains("only the i32 target"));
    }

    #[test]
    fn ordinary_map_v2_values_unpack_receipt_binds_result_move_to_checker_point() {
        let source = include_str!(
            "../../../tests/fixtures/mir_ordinary_map_any_owner_v2_values_unpack.mimi"
        );
        let mut program = ordinary_map_v2_unpack_program(source);
        let receipt = program
            .ordinary_map_owned_any_v2_values_unpack_result_receipts()
            .expect("Values unpack receipt validates")
            .into_iter()
            .next()
            .expect("fixture has a direct Values unpack");
        let mut forged = program
            .functions()
            .get(&receipt.owner)
            .expect("Values unpack owner")
            .clone();
        let move_instruction = forged
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    super::super::MirInstructionKind::Move { result, source }
                        if result == &receipt.result_binding && source == &receipt.result
                )
            })
            .expect("direct result binding Move");
        move_instruction.id =
            super::super::MirInstructionId::new(format!("inst:bind:forged:{}", receipt.cast.0))
                .expect("distinct forged instruction identity");
        program.replace_function_for_test_only(forged);

        let error = program
            .ordinary_map_owned_any_v2_values_unpack_result_receipts()
            .expect_err("result Move must use the Checker binding point identity");
        assert!(
            error.contains("exact Checker result binding point"),
            "{error}"
        );
    }

    #[test]
    fn ordinary_map_v2_values_unpack_receipt_separates_result_owner_drops_from_snapshot_drop() {
        use crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind as OwnerSettlement;

        let source = r#"
type Label { text: string }
func main() -> i32 {
    let label = Label { text: "owner receipt Label" }
    let empty = map_new()
    let with_label = map_set(empty, "z", label)
    let with_number = map_set(with_label, "a", 42)
    let both = map_set(with_number, "m", "owner receipt String")
    let snapshot = values(both)
    let number = snapshot[0] as i32
    let text = snapshot[1] as string
    let label_copy = snapshot[2] as Label
    drop(snapshot)
    drop(number)
    drop(label_copy)
    drop(empty)
    drop(with_label)
    drop(with_number)
    drop(both)
    drop(label)
    0
}
"#;
        let mut program = ordinary_map_v2_unpack_program(source);
        let receipts = program
            .ordinary_map_owned_any_v2_values_unpack_result_receipts()
            .expect("i32, String, and whole-Label result owners have exact receipts");
        assert_eq!(receipts.len(), 3);
        let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        let number = receipts
            .iter()
            .find(|receipt| receipt.result_type == i32_type)
            .expect("Copy result receipt");
        assert_eq!(
            number.result_owner_settlement,
            OwnerSettlement::NoOwnedPayload
        );
        assert!(number.result_owner_settlement_instruction.is_none());

        let text = receipts
            .iter()
            .find(|receipt| receipt.result_type == string_type)
            .expect("String result receipt");
        assert_eq!(
            text.result_owner_settlement,
            OwnerSettlement::CompilerOwnedStringDrop
        );
        let text_anchor = text
            .result_owner_settlement_point
            .as_ref()
            .expect("root tail-expression cleanup anchor");
        let text_drop = text
            .result_owner_settlement_instruction
            .as_ref()
            .expect("exact String binding cleanup instruction");
        assert!(text_drop.as_str().ends_with(&format!(":{}", text_anchor.0)));
        assert!(program
            .functions()
            .get(&text.owner)
            .expect("String owner function")
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .any(|instruction| {
                &instruction.id == text_drop
                    && matches!(
                        &instruction.kind,
                        super::super::MirInstructionKind::Drop { value }
                            if value == &text.result_binding
                    )
            }));

        let label = receipts
            .iter()
            .find(|receipt| receipt.result_type != i32_type && receipt.result_type != string_type)
            .expect("Label result receipt");
        assert_eq!(
            label.result_owner_settlement,
            OwnerSettlement::ExplicitWholeDrop
        );
        let label_drop = label
            .result_owner_settlement_instruction
            .as_ref()
            .expect("exact Label whole-Drop instruction");
        let function = program
            .functions()
            .get(&label.owner)
            .expect("Label owner function");
        assert!(function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .any(|instruction| {
                &instruction.id == label_drop
                    && matches!(
                        &instruction.kind,
                        super::super::MirInstructionKind::Drop { value }
                            if value == &label.result_binding
                    )
            }));
        assert!(label.result_owner_settlement_point.is_some());
        assert!(label.result_owner_settlement_index.is_some());
        assert_eq!(
            label.settlement_instruction, receipts[0].settlement_instruction,
            "the Values snapshot Drop remains a separate shared receipt"
        );

        let mut forged = program
            .functions()
            .get(&label.owner)
            .expect("Label owner function")
            .clone();
        let forged_drop = forged
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| &instruction.id == label_drop)
            .expect("Label owner Drop instruction");
        forged_drop.id = super::super::MirInstructionId::new("inst:drop:forged-label-owner")
            .expect("forged instruction id");
        program.replace_function_for_test_only(forged);
        let error = program
            .ordinary_map_owned_any_v2_values_unpack_result_receipts()
            .expect_err("the Label owner Drop must retain its exact Checker statement/place ID");
        assert!(error.contains("exact Checker point/index"), "{error}");
    }

    #[test]
    fn ordinary_map_v2_temporary_root_receipt_binds_new_get_any_and_immediate_drop() {
        let program = ordinary_map_v2_temporary_root_program();
        let receipts = program
            .ordinary_map_owned_any_v2_get_any_result_receipts()
            .expect("temporary-root GetAny receipt validates");
        let [receipt] = receipts.as_slice() else {
            panic!("one temporary-root GetAny result receipt is expected");
        };
        let root = receipt
            .temporary_root
            .as_ref()
            .expect("immediate map_new source is bound into the shared receipt");
        assert_eq!(root.local, receipt.source_local);
        assert_eq!(root.root, receipt.root);
        assert_eq!(root.source_node, receipt.source_node);
        assert_eq!(root.get_any, receipt.operation);
        assert_eq!(root.get_any_instruction, receipt.instruction);
        assert_eq!(root.settlement_point, receipt.operation);

        let mut missing = program.clone();
        let mut function = program
            .functions()
            .values()
            .find(|function| {
                function
                    .ownership
                    .checker_ordinary_map_owned_any_v2_cfg_receipt
                    .as_ref()
                    .is_some_and(|cfg| !cfg.temporary_root_receipts.is_empty())
            })
            .expect("temporary-root GetAny function")
            .clone();
        function
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_mut()
            .expect("temporary-root Checker CFG receipt")
            .temporary_root_receipts
            .clear();
        missing.replace_function_for_test_only(function);
        assert!(
            missing
                .ordinary_map_owned_any_v2_get_any_result_receipts()
                .is_err(),
            "a missing temporary-root Checker receipt cannot be reconstructed from MIR"
        );

        let mut tampered = program.clone();
        let mut function = program
            .functions()
            .values()
            .find(|function| {
                function
                    .ownership
                    .checker_ordinary_map_owned_any_v2_cfg_receipt
                    .as_ref()
                    .is_some_and(|cfg| !cfg.temporary_root_receipts.is_empty())
            })
            .expect("temporary-root GetAny function")
            .clone();
        function
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_mut()
            .expect("temporary-root Checker CFG receipt")
            .temporary_root_receipts[0]
            .source_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        tampered.replace_function_for_test_only(function);
        assert!(
            tampered
                .ordinary_map_owned_any_v2_get_any_result_receipts()
                .is_err(),
            "a temporary-root source TypeId mutation invalidates the shared MIR receipt"
        );
    }

    #[test]
    fn ordinary_map_v2_payload_selection_rejects_missing_extra_and_forged_rows() {
        let program = ordinary_map_v2_program();
        let receipt = program
            .ordinary_map_owned_any_v2_payload_selection_receipt()
            .expect("selection derives")
            .expect("V2 Set actions produce a selection");

        let mut missing = receipt.clone();
        missing.entries.pop();
        assert!(
            missing.validate().is_err(),
            "missing Checker Set row is rejected"
        );
        assert!(missing.validate_against_program(&program).is_err());

        let mut extra = receipt.clone();
        extra.entries.push(receipt.entries[0].clone());
        assert!(
            extra.validate().is_err(),
            "duplicate/extra Set row is rejected"
        );

        let mut forged = receipt.clone();
        forged.entries[0].value =
            super::super::MirValueId::new("forged:payload").expect("test value identity is valid");
        assert!(
            forged.validate().is_err(),
            "forged MIR value invalidates the digest"
        );
        assert!(forged.validate_against_program(&program).is_err());

        let mut forged_type = receipt.clone();
        forged_type.selected_types[0].type_id = forged_type.entries[0].checker_type.clone();
        forged_type.selected_types[0].type_desc_digest = "0".repeat(64);
        assert!(
            forged_type.validate().is_err(),
            "forged TypeDesc digest is rejected"
        );
    }

    #[test]
    fn ordinary_map_v2_payload_selection_rejects_unreceipted_or_changed_mir_sets() {
        use crate::core::mir::MirInstructionKind as Instruction;

        let program = ordinary_map_v2_program();
        let mut missing_action = program.clone();
        let mut function = program
            .functions()
            .values()
            .find(|function| {
                function
                    .blocks
                    .values()
                    .flat_map(|block| &block.instructions)
                    .any(|instruction| {
                        matches!(
                            instruction.kind,
                            Instruction::OrdinaryMapOwnedAnyV2Set { .. }
                        )
                    })
            })
            .expect("V2 Set function")
            .clone();
        function
            .ownership
            .checker_ordinary_map_owned_any_v2
            .retain(|action| action.kind != crate::core::OrdinaryMapOwnedAnyV2ActionKind::Set);
        missing_action.replace_function_for_test_only(function);
        assert!(
            missing_action
                .ordinary_map_owned_any_v2_payload_selection_receipt()
                .is_err(),
            "an MIR Set cannot authorize its own payload descriptor"
        );

        let mut changed_mir = program.clone();
        let mut function = program
            .functions()
            .values()
            .find(|function| {
                function
                    .blocks
                    .values()
                    .flat_map(|block| &block.instructions)
                    .any(|instruction| {
                        matches!(
                            instruction.kind,
                            Instruction::OrdinaryMapOwnedAnyV2Set { .. }
                        )
                    })
            })
            .expect("V2 Set function")
            .clone();
        let set = function
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    instruction.kind,
                    Instruction::OrdinaryMapOwnedAnyV2Set { .. }
                )
            })
            .expect("V2 Set instruction");
        let replacement = function
            .values
            .keys()
            .find(|value| match &set.kind {
                Instruction::OrdinaryMapOwnedAnyV2Set {
                    value: original, ..
                } => *value != original,
                _ => false,
            })
            .expect("another MIR value")
            .clone();
        let Instruction::OrdinaryMapOwnedAnyV2Set { value, .. } = &mut set.kind else {
            unreachable!()
        };
        *value = replacement;
        changed_mir.replace_function_for_test_only(function);
        assert!(
            changed_mir
                .ordinary_map_owned_any_v2_payload_selection_receipt()
                .is_err(),
            "a Set operand changed after Checker lowering cannot mint a receipt"
        );
    }

    #[test]
    fn ordinary_map_v2_payload_selection_binds_the_type_catalog_digest() {
        let program = ordinary_map_v2_program();
        let receipt = program
            .ordinary_map_owned_any_v2_payload_selection_receipt()
            .expect("selection derives")
            .expect("V2 Set actions produce a selection");

        let mut catalog = program.type_catalog().clone();
        let (primitive_id, primitive) = catalog
            .iter()
            .find(|(id, descriptor)| {
                !receipt
                    .selected_types
                    .iter()
                    .any(|selected| &selected.type_id == *id)
                    && matches!(
                        descriptor.kind,
                        crate::core::mir::types::MirTypeKind::Primitive(_)
                    )
            })
            .map(|(id, descriptor)| (id.clone(), descriptor.clone()))
            .expect("fixture has an unselected primitive TypeDesc");
        let extra_id = crate::core::ir::ResolvedTypeId::synthetic(
            "mimi-test:ordinary-map-owned-any-v2-extra-type",
        );
        let mut extra_descriptor = primitive;
        extra_descriptor.id = extra_id.clone();
        catalog.replace_for_test_only(extra_id, extra_descriptor);
        let changed_catalog_program =
            MirProgram::with_type_catalog(program.functions().clone(), catalog)
                .expect("an unused additional TypeDesc keeps function MIR valid");

        assert!(
            receipt
                .validate_against_program(&changed_catalog_program)
                .is_err(),
            "the selection is bound to the complete TypeDesc catalog digest"
        );
        assert!(program.type_catalog().get(&primitive_id).is_some());
    }

    #[test]
    fn route_receipt_validation_accepts_canonical_shape() {
        assert!(valid_receipt().validate().is_ok());
        assert_eq!(MIR_ROUTE_RECEIPT_MANIFEST_FIELDS.len(), 9);
    }

    #[test]
    fn route_receipt_validation_rejects_schema_digest_profile_and_owner_drift() {
        let mut receipt = valid_receipt();
        receipt.schema = "future-schema";
        assert!(receipt.validate().is_err());

        let mut receipt = valid_receipt();
        receipt.mir_digest = "A".repeat(64);
        assert!(receipt.validate().is_err());

        let mut receipt = valid_receipt();
        receipt.profile = "bad=profile".into();
        assert!(receipt.validate().is_err());

        let mut receipt = valid_receipt();
        receipt.root_owners.reverse();
        assert!(receipt.validate().is_err());

        let mut receipt = valid_receipt();
        receipt.root_owners[0] = NodeId("function:owner=drift".into());
        assert!(receipt.validate().is_err());

        let mut receipt = valid_receipt();
        receipt.root_owners[0] = NodeId("function:owner,drift".into());
        assert!(receipt.validate().is_err());

        let mut receipt = valid_receipt();
        receipt.root_owners[1] = receipt.root_owners[0].clone();
        assert!(receipt.validate().is_err());
    }

    #[test]
    fn route_receipt_comparison_reports_identity_drift() {
        let receipt = valid_receipt();
        receipt
            .verify_against_receipt(&receipt)
            .expect("identical receipts must compare successfully");

        let mut profile = receipt.clone();
        profile.profile = "other-v1".into();
        assert!(
            receipt.same_semantic_identity(&profile),
            "consumer profile is provenance and must not change semantic identity"
        );
        assert_eq!(
            receipt.semantic_identity_digest(),
            profile.semantic_identity_digest(),
            "consumer profile is provenance and must not split semantic cache identity"
        );
        let mut mir_digest = receipt.clone();
        mir_digest.mir_digest = "b".repeat(64);
        assert!(!receipt.same_semantic_identity(&mir_digest));
        assert_ne!(
            receipt.semantic_identity_digest(),
            mir_digest.semantic_identity_digest()
        );
        let mut type_desc_digest = receipt.clone();
        type_desc_digest.type_desc_digest = "b".repeat(64);
        let mut abi_digest = receipt.clone();
        abi_digest.abi_digest = "b".repeat(64);
        let mut ffi_digest = receipt.clone();
        ffi_digest.ffi_digest = "b".repeat(64);
        let mut ownership_digest = receipt.clone();
        ownership_digest.ownership_digest = "b".repeat(64);
        let mut flow_transition_digest = receipt.clone();
        flow_transition_digest.flow_transition_digest = "b".repeat(64);
        let mut root_owners = receipt.clone();
        root_owners.root_owners = vec![NodeId("function:a".into()), NodeId("function:z".into())];
        for (field, drifted) in [
            ("profile", profile),
            ("mir_digest", mir_digest),
            ("type_desc_digest", type_desc_digest),
            ("abi_digest", abi_digest),
            ("ffi_digest", ffi_digest),
            ("ownership_digest", ownership_digest),
            ("flow_transition_digest", flow_transition_digest),
            ("root_owners", root_owners),
        ] {
            let error = receipt
                .verify_against_receipt(&drifted)
                .expect_err("receipt identity drift must fail closed");
            assert_eq!(
                error,
                format!("route receipt does not match expected checker receipt: {field}")
            );
        }

        let mut invalid = receipt.clone();
        invalid.profile = "bad=profile".into();
        let error = receipt
            .verify_against_receipt(&invalid)
            .expect_err("invalid expected receipt must fail before comparison");
        assert_eq!(
            error,
            "invalid expected route receipt: route receipt profile is empty or not manifest-safe"
        );
    }

    #[test]
    fn route_receipt_comparison_reports_joint_identity_drift_in_declared_order() {
        let receipt = valid_receipt();
        let mut drifted = receipt.clone();
        drifted.profile = "other-v1".into();
        drifted.mir_digest = "b".repeat(64);
        drifted.abi_digest = "b".repeat(64);
        drifted.ffi_digest = "b".repeat(64);
        drifted.ownership_digest = "b".repeat(64);
        drifted.root_owners = vec![NodeId("function:a".into()), NodeId("function:z".into())];

        let error = receipt
            .verify_against_receipt(&drifted)
            .expect_err("joint receipt identity drift must fail closed");
        assert_eq!(
            error,
            "route receipt does not match expected checker receipt: profile, mir_digest, abi_digest, ffi_digest, ownership_digest, root_owners"
        );
    }

    #[test]
    fn route_receipt_manifest_uses_declared_field_order_and_values() {
        let receipt = valid_receipt();
        let text = receipt.manifest_text().expect("valid receipt manifest");
        let lines = text.lines().collect::<Vec<_>>();
        assert_eq!(
            lines.first().copied(),
            Some(MIR_ROUTE_RECEIPT_MANIFEST_HEADER)
        );
        let fields = lines[1..]
            .iter()
            .map(|line| line.split_once('=').expect("manifest key/value").0)
            .collect::<Vec<_>>();
        assert_eq!(fields, MIR_ROUTE_RECEIPT_MANIFEST_FIELDS.to_vec());
        assert_eq!(lines[1], "schema=mimi-mir-route-receipt-v1");
        assert_eq!(lines[2], "profile=test-v1");
        for line in &lines[3..9] {
            let (_, value) = line.split_once('=').expect("digest key/value");
            assert_eq!(value.len(), 64);
            assert!(value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
        }
        assert_eq!(lines[9], "root_owners=function:main,function:z");
    }

    #[test]
    fn route_receipt_manifest_rejects_unsafe_profile_and_owner_without_partial_output() {
        let mut receipt = valid_receipt();
        receipt.profile = "bad=profile".into();
        let error = receipt
            .manifest_text()
            .expect_err("unsafe profile rejection");
        assert_eq!(
            error,
            "invalid MIR route receipt: route receipt profile is empty or not manifest-safe"
        );

        let mut receipt = valid_receipt();
        receipt.profile = "bad profile".into();
        let error = receipt
            .manifest_text()
            .expect_err("whitespace in profile must be rejected");
        assert_eq!(
            error,
            "invalid MIR route receipt: route receipt profile is empty or not manifest-safe"
        );

        let mut receipt = valid_receipt();
        receipt.root_owners[0] = NodeId("function:bad,owner".into());
        let error = receipt.manifest_text().expect_err("unsafe owner rejection");
        assert_eq!(
            error,
            "invalid MIR route receipt: route receipt root owner is empty or not manifest-safe"
        );

        let mut receipt = valid_receipt();
        receipt.root_owners[0] = NodeId("function:bad owner".into());
        let error = receipt
            .manifest_text()
            .expect_err("whitespace in root owner must be rejected");
        assert_eq!(
            error,
            "invalid MIR route receipt: route receipt root owner is empty or not manifest-safe"
        );
    }

    #[test]
    fn route_receipt_manifest_schema_is_unique_safe_and_unknown_lookup_is_fail_closed() {
        assert!(validate_manifest_field_schema().is_ok());
        let receipt = valid_receipt();
        assert_eq!(receipt.manifest_value("future_field"), None);
    }

    #[test]
    fn route_receipt_manifest_parser_rejects_unknown_and_duplicate_rows() {
        let receipt = valid_receipt();
        let manifest = receipt.manifest_text().expect("valid receipt manifest");
        let mut rows = manifest.lines().map(str::to_owned).collect::<Vec<_>>();
        rows.insert(1, "future_field=unexpected".into());
        let unknown = CanonicalMirRouteReceipt::parse_manifest(&rows.join("\n"))
            .expect_err("unknown manifest field must fail closed");
        assert_eq!(
            unknown,
            "invalid MIR route manifest: unknown field 'future_field' at row 0"
        );

        let mut rows = manifest.lines().map(str::to_owned).collect::<Vec<_>>();
        rows.insert(1, rows[1].clone());
        let duplicate = CanonicalMirRouteReceipt::parse_manifest(&rows.join("\n"))
            .expect_err("duplicate manifest field must fail closed");
        assert_eq!(
            duplicate,
            "invalid MIR route manifest: duplicate field 'schema' at row 1"
        );
    }

    #[test]
    fn route_receipt_manifest_parser_rejects_reordered_rows_before_value_comparison() {
        let receipt = valid_receipt();
        let manifest = receipt.manifest_text().expect("valid receipt manifest");
        let mut rows = manifest.lines().map(str::to_owned).collect::<Vec<_>>();
        rows.swap(3, 4);
        let error = CanonicalMirRouteReceipt::parse_manifest(&rows.join("\n"))
            .expect_err("reordered manifest rows must fail closed");
        assert_eq!(
            error,
            "invalid MIR route manifest: field 'type_desc_digest' at row 2, expected 'mir_digest'"
        );
    }

    #[test]
    fn route_receipt_manifest_parser_rejects_value_semantic_drift() {
        let receipt = valid_receipt();
        let manifest = receipt.manifest_text().expect("valid receipt manifest");
        for (field, value, expected) in [
            (
                "schema",
                "mimi-mir-route-receipt-v2",
                "invalid MIR route manifest: field 'schema' has unexpected value 'mimi-mir-route-receipt-v2'",
            ),
            (
                "profile",
                "bad=profile",
                "invalid MIR route manifest: field 'profile' is empty or not manifest-safe",
            ),
            (
                "profile",
                "bad profile",
                "invalid MIR route manifest: field 'profile' is empty or not manifest-safe",
            ),
            (
                "profile",
                "bad\u{00a0}profile",
                "invalid MIR route manifest: field 'profile' is empty or not manifest-safe",
            ),
            (
                "mir_digest",
                "not-a-digest",
                "invalid MIR route manifest: field 'mir_digest' must be a 64-character lowercase hex digest",
            ),
            (
                "root_owners",
                "z-owner,a-owner",
                "invalid MIR route manifest: root owners must be strictly sorted",
            ),
            (
                "root_owners",
                "bad owner,function:z",
                "invalid MIR route manifest: root owner is empty or not manifest-safe",
            ),
            (
                "root_owners",
                "bad\u{2003}owner,function:z",
                "invalid MIR route manifest: root owner is empty or not manifest-safe",
            ),
        ] {
            let mutated = manifest
                .lines()
                .map(|line| {
                    line.strip_prefix(&format!("{field}="))
                        .map_or_else(|| line.to_owned(), |_| format!("{field}={value}"))
                })
                .collect::<Vec<_>>()
                .join("\n");
            let error = CanonicalMirRouteReceipt::parse_manifest(&mutated)
                .expect_err("manifest value drift must fail closed");
            assert_eq!(error, expected, "unexpected diagnostic for {field}");
        }
    }

    #[test]
    fn route_receipt_manifest_parser_normalizes_crlf_to_canonical_lf() {
        let receipt = valid_receipt();
        let manifest = receipt.manifest_text().expect("valid receipt manifest");
        let crlf = manifest.replace('\n', "\r\n");
        let parsed = CanonicalMirRouteReceipt::from_manifest(&crlf)
            .expect("CRLF manifest must preserve receipt semantics");
        assert_eq!(parsed, receipt);
        assert_eq!(
            parsed.manifest_text().expect("render parsed receipt"),
            manifest,
            "manifest rendering must use the canonical LF byte form"
        );
    }

    #[test]
    fn route_receipt_manifest_round_trips_to_the_same_receipt() {
        let receipt = valid_receipt();
        let manifest = receipt.manifest_text().expect("valid receipt manifest");
        assert_eq!(
            CanonicalMirRouteReceipt::from_manifest(&manifest),
            Ok(receipt.clone())
        );
        assert_eq!(
            CanonicalMirRouteReceipt::from_manifest_round_trip(&manifest),
            Ok(receipt.clone())
        );
        assert_eq!(receipt.manifest_round_trip(), Ok(receipt));
    }

    #[test]
    fn route_receipt_manifest_rejects_an_old_header_with_a_stable_hint() {
        let receipt = valid_receipt();
        let manifest = receipt
            .manifest_text()
            .expect("valid receipt manifest")
            .replacen(
                MIR_ROUTE_RECEIPT_MANIFEST_HEADER,
                "mimi-mir-route-manifest-v0",
                1,
            );
        assert_eq!(
            CanonicalMirRouteReceipt::from_manifest(&manifest),
            Err("invalid MIR route manifest: expected header 'mimi-mir-route-manifest-v1'".into())
        );
    }

    #[test]
    fn route_receipt_manifest_rejects_future_version_and_field_extensions() {
        let receipt = valid_receipt();
        let manifest = receipt.manifest_text().expect("valid receipt manifest");

        let future_header = manifest.replacen(
            MIR_ROUTE_RECEIPT_MANIFEST_HEADER,
            "mimi-mir-route-manifest-v2",
            1,
        );
        assert_eq!(
            CanonicalMirRouteReceipt::from_manifest(&future_header),
            Err("invalid MIR route manifest: expected header 'mimi-mir-route-manifest-v1'".into())
        );

        for insertion in [1, 5, 10] {
            let mut rows = manifest.lines().map(str::to_owned).collect::<Vec<_>>();
            rows.insert(insertion, "future_field=reserved".into());
            let error = CanonicalMirRouteReceipt::parse_manifest(&rows.join("\n"))
                .expect_err("future fields require an explicit schema version");
            let row = insertion.saturating_sub(1);
            assert_eq!(
                error,
                format!("invalid MIR route manifest: unknown field 'future_field' at row {row}")
            );
        }
    }

    #[test]
    fn route_receipt_manifest_enforces_field_completeness_and_empty_owner_round_trip() {
        let receipt = valid_receipt();
        let manifest = receipt.manifest_text().expect("valid receipt manifest");

        let missing_owner = manifest
            .lines()
            .filter(|line| !line.starts_with("root_owners="))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            CanonicalMirRouteReceipt::parse_manifest(&missing_owner),
            Err("invalid MIR route manifest: missing field 'root_owners'".into())
        );

        let trailing_future = format!("{manifest}future_field=reserved\n");
        assert_eq!(
            CanonicalMirRouteReceipt::parse_manifest(&trailing_future),
            Err("invalid MIR route manifest: unknown field 'future_field' at row 9".into())
        );

        let empty_owner_manifest =
            manifest.replace("root_owners=function:main,function:z", "root_owners=");
        let empty_owner_receipt = CanonicalMirRouteReceipt::from_manifest(&empty_owner_manifest)
            .expect("empty owner set is a valid manifest value");
        assert!(empty_owner_receipt.root_owners.is_empty());
        assert_eq!(
            empty_owner_receipt
                .manifest_text()
                .expect("render empty owner set"),
            empty_owner_manifest
        );
    }

    #[test]
    fn canonical_mir_instance_contract_uses_explicit_stable_spelling() {
        let source = include_str!("../../../tests/fixtures/mir_native_generic_list_len.mimi");
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("lex generic instance fixture");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse generic instance fixture");
        let checked = crate::core::check_program(&file).expect("check generic instance fixture");
        let program = MirProgram::from_checked_program(&checked)
            .expect("materialize generic instance fixture");
        let text = canonical_mir_text(&program);

        assert!(
            text.contains("contract=scalar_list_facade operation=len\n"),
            "canonical MIR must use the explicit instance contract spelling"
        );
        assert!(
            !text.contains("ScalarListFacade"),
            "canonical MIR must not inherit Rust Debug enum names"
        );
    }
}
