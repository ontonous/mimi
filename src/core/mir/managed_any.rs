//! Receipted generation-bearing ordinary Any snapshot contract.
//!
//! Descriptor materialization describes values. The private seal binds every
//! owner-bearing instruction, value identity and escape boundary to lowering
//! of the checker-owned body; it is the operation authority.
use super::types::{
    managed_any_i32_string_v1_type_id, ordinary_map_get_any_tuple_i32_string_v1_type_id,
    ordinary_map_owned_any_v2_get_result_type_id, ordinary_map_owned_any_v2_payload_type_id,
    ordinary_map_owned_any_v2_type_id, ordinary_map_owned_any_v2_values_element_type_id,
    ordinary_map_owned_any_v2_values_type_id, MirAbiClass, MirGlueContract, MirGlueKind,
    MirGlueOperation, MirLayout, MirOwnership, MirTypeCatalog, MirTypeKind,
};
use super::{
    MirAggregateKind, MirBlockId, MirFunction, MirGenericInstanceContract, MirInstance,
    MirInstanceId, MirInstruction, MirInstructionId, MirInstructionKind, MirProjection,
    MirTerminator, MirValidationError, MirValueId,
};
use crate::core::ir::{
    CheckedConversion, CheckedConversionKind, PrimitiveType, ResolvedBinaryOp, ResolvedLiteral,
    ResolvedTypeId,
};
use crate::core::NodeId;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub const MIR_MANAGED_ANY_SCHEMA_VERSION: u32 = 1;
pub const MIR_MANAGED_ANY_DESCRIPTOR_VERSION: u32 = 1;

/// Validate the narrow executable Core MIR profile projected from the exact
/// direct-tuple Checker receipt. This path owns no generic ManagedAny seal;
/// authority comes from the source receipt, exact graph rematerialization,
/// and the ordinary operational TypeDesc contracts below.
pub(crate) fn validate_ordinary_map_direct_tuple_get_unpack_l1_types(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
) -> Result<(), String> {
    use crate::core::ownership::{
        OrdinaryMapOwnedAnyV2DirectTupleGetUnpackPayload as Payload,
        OrdinaryMapOwnedAnyV2DirectTupleGetUnpackProfileVersion as ProfileVersion,
    };

    let receipt = function
        .ownership
        .ordinary_map_direct_tuple_get_unpack_l1_receipt()
        .ok_or("direct tuple Get L1 has no exact Checker receipt")?;
    if function
        .ownership
        .ordinary_map_direct_tuple_get_unpack_l2_projection()
        .is_some()
    {
        return Err("direct tuple Get L1 cannot borrow the L2 verifier projection".into());
    }
    if function.ownership.checker_managed_any.is_some()
        || function.ownership.checker_managed_any_cfg_receipt.is_some()
    {
        return Err(
            "direct tuple Get L1 cannot mix its exact Checker receipt with generic ManagedAny seals"
                .into(),
        );
    }

    let i32_ty = crate::core::ir::primitive_type_id(PrimitiveType::I32);
    let Some(selected_set) = receipt.sets.first() else {
        return Err("direct tuple Get L1 has no exact selected Set receipt".into());
    };
    if receipt.profile_version != ProfileVersion::V1
        || receipt.owner != function.owner
        || receipt.sets.len() != 1
        || receipt.selected_winning_set.as_ref() != Some(&selected_set.operation)
        || selected_set.payload_type != i32_ty
        || !matches!(selected_set.payload, Payload::I32(_))
        || receipt.unpack_target_type != i32_ty
    {
        return Err(
            "direct tuple Get L1 is outside its exact one-selected-i32-Set source receipt profile"
                .into(),
        );
    }

    let expected = super::lower::ordinary_map_direct_tuple_get_unpack_l1_function(receipt)?;
    if expected != *function {
        return Err("direct tuple Get L1 MIR graph differs from its exact Checker receipt".into());
    }

    let root = super::types::ordinary_map_owned_any_v2_type_id();
    for operation in [
        MirGlueOperation::MoveOut,
        MirGlueOperation::Clone,
        MirGlueOperation::Drop,
    ] {
        catalog.validate_glue(&root, operation)?;
    }
    let root_result = super::types::ordinary_map_owned_any_v2_root_result_type_id();
    for source_result in std::iter::once(&receipt.new_result_type)
        .chain(receipt.sets.iter().map(|set| &set.result_type))
    {
        catalog.validate_ordinary_map_owned_any_v2_root_result(&root_result, source_result)?;
    }

    let payload = super::types::ordinary_map_owned_any_v2_payload_type_id();
    let pair = super::types::ordinary_map_owned_any_v2_get_result_type_id();
    for operation in [
        MirGlueOperation::MoveOut,
        MirGlueOperation::Clone,
        MirGlueOperation::Drop,
    ] {
        catalog.validate_ordinary_map_owned_any_v2_payload_glue(&payload, operation)?;
        catalog.validate_ordinary_map_owned_any_v2_get_result_glue(&pair, operation)?;
    }
    catalog.validate_copy_scalar(&i32_ty)?;
    catalog.validate_owned_string(&crate::core::ir::primitive_type_id(PrimitiveType::String))?;

    if super::types::ordinary_map_owned_any_v2_direct_tuple_get_unpack_l2_type_descs()
        .iter()
        .any(|descriptor| catalog.get(&descriptor.id).is_some())
    {
        return Err("direct tuple Get L1 catalog contains an L2 proof-only TypeDesc".into());
    }
    Ok(())
}

/// Validate the narrow executable Core MIR profile projected from the exact
/// nested New/Set Result-match direct-print receipt. This profile has no
/// generic ManagedAny body seal: its Checker receipt, exact graph
/// rematerialization, and operational TypeDesc contracts are the authority.
pub(crate) fn validate_ordinary_map_direct_print_result_l1_types(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
) -> Result<(), String> {
    use crate::core::ownership::OrdinaryMapOwnedAnyV2DirectPrintResultProfileVersion as Version;

    let receipt = function
        .ownership
        .checker_ordinary_map_owned_any_v2_direct_print_result_receipt
        .as_ref()
        .ok_or("DirectPrintResult L1 has no exact Checker receipt")?;
    if function.ownership.checker_managed_any.is_some()
        || function.ownership.checker_managed_any_cfg_receipt.is_some()
        || !function.contracts.is_empty()
        || function.proof_worklist.is_some()
    {
        return Err(
            "DirectPrintResult L1 cannot mix its exact Checker receipt with generic ManagedAny seals or proof worklists".into(),
        );
    }
    if receipt.profile_version != Version::V1 || receipt.owner != function.owner {
        return Err("DirectPrintResult L1 has the wrong Checker profile or owner".into());
    }

    let expected = super::lower::ordinary_map_direct_print_result_function(receipt)?;
    if expected != *function {
        return Err("DirectPrintResult L1 MIR graph differs from its exact Checker receipt".into());
    }

    let root = super::types::ordinary_map_owned_any_v2_type_id();
    for operation in [
        MirGlueOperation::MoveOut,
        MirGlueOperation::Clone,
        MirGlueOperation::Drop,
    ] {
        catalog.validate_glue(&root, operation)?;
    }
    let root_result = super::types::ordinary_map_owned_any_v2_root_result_type_id();
    catalog
        .validate_ordinary_map_owned_any_v2_root_result(&root_result, &receipt.new_result_type)?;
    catalog
        .validate_ordinary_map_owned_any_v2_root_result(&root_result, &receipt.set_result_type)?;

    let payload = super::types::ordinary_map_owned_any_v2_payload_type_id();
    let pair = super::types::ordinary_map_owned_any_v2_get_result_type_id();
    for operation in [
        MirGlueOperation::MoveOut,
        MirGlueOperation::Clone,
        MirGlueOperation::Drop,
    ] {
        catalog.validate_ordinary_map_owned_any_v2_payload_glue(&payload, operation)?;
        catalog.validate_ordinary_map_owned_any_v2_get_result_glue(&pair, operation)?;
    }
    catalog.validate_copy_scalar(&crate::core::ir::primitive_type_id(PrimitiveType::I32))?;
    catalog.validate_owned_string(&crate::core::ir::primitive_type_id(PrimitiveType::String))?;
    Ok(())
}

pub(super) fn managed_any_list_i32_string_v1_type_id(source: &ResolvedTypeId) -> ResolvedTypeId {
    ResolvedTypeId::synthetic(&format!("mimi-mir:ManagedAnyList:v1:{}", source.as_str()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirManagedAnyOperation {
    Clone,
    Drop,
    /// Borrow one exact canonical Any v1 owner for `println`. The source
    /// remains live; lowering emits a separate Drop for the fresh argument
    /// owner after output completes.
    Println,
    /// Materialize one Checker-certified implicit `i32 -> Any` injection as
    /// the canonical generation-bearing ManagedAny v1 owner.
    PackI32,
    UnpackI32,
    UnpackString,
}
impl MirManagedAnyOperation {
    pub const fn expected_kind(self) -> Option<u32> {
        match self {
            Self::PackI32 | Self::UnpackI32 => Some(1),
            Self::UnpackString => Some(2),
            Self::Clone | Self::Drop | Self::Println => None,
        }
    }
    pub const fn canonical_text(self) -> &'static str {
        match self {
            Self::Clone => "clone",
            Self::Drop => "drop",
            Self::Println => "println",
            Self::PackI32 => "pack_i32",
            Self::UnpackI32 => "unpack_i32",
            Self::UnpackString => "unpack_string",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirManagedAnyContract {
    pub schema_version: u32,
    pub owner: NodeId,
    pub origin: NodeId,
    pub source: MirValueId,
    pub result: Option<MirValueId>,
    pub source_type: ResolvedTypeId,
    pub target_type: Option<ResolvedTypeId>,
    /// Exact ResolvedBody conversion authority. Present only for PackI32;
    /// this prevents a TypeDesc pair or arbitrary MIR Convert from claiming
    /// that the Checker admitted a DynamicAny injection.
    pub checker_conversion: Option<CheckedConversion>,
    /// Raw Checker argument type for the one `println(Any)` borrowing
    /// operation. The private full-body seal binds this source identity to the
    /// exact call-origin and owner operand; it is absent for every other op.
    pub checker_source_type: Option<ResolvedTypeId>,
    pub expected_kind: Option<u32>,
    pub descriptor_version: u32,
}

/// This cannot be constructed outside canonical MIR. Exact typed values and
/// complete boundary shapes prevent changing the graph after Checker lowering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MirCheckerManagedAnySeal {
    pub(super) owner: NodeId,
    pub(super) value_types: BTreeMap<MirValueId, ResolvedTypeId>,
    pub(super) parameters: Vec<MirValueId>,
    pub(super) parameter_permissions: Option<Vec<Option<crate::core::ir::Permission>>>,
    pub(super) result: ResolvedTypeId,
    pub(super) managed_values: BTreeSet<MirValueId>,
    /// Full Checker-lowered body snapshot. Block identity is part of the
    /// commitment so moving an instruction across a branch/join cannot keep
    /// the same flattened instruction sequence.
    pub(super) block_parameters: Vec<(MirBlockId, Vec<super::MirBlockParameter>)>,
    pub(super) instructions: Vec<(MirBlockId, NodeId, MirInstruction)>,
    pub(super) terminators: Vec<(MirBlockId, MirTerminator)>,
    pub(super) source_cfg: Option<crate::core::ManagedAnyCfgReceipt>,
    /// Preserve the exact Checker authority for the fixed fallible
    /// New/Set -> legacy tuple-valued `map_get` slice independently of the
    /// mutable MIR ownership ledger.
    pub(super) ordinary_map_owned_get_any_tuple_result_v1_snapshot:
        Option<crate::core::ownership::OrdinaryMapOwnedGetAnyTupleResultV1Receipt>,
    /// Original Checker snapshot for the one branch-local persistent-Set
    /// diamond. This is captured only at initial CheckedBody lowering and is
    /// carried across later body-seal refreshes; a refresh must never derive
    /// operation authority again from the mutable MIR ownership receipt.
    pub(super) branch_local_set_diamond_v2_snapshot:
        Option<Box<crate::core::OrdinaryMapOwnedAnyV2CfgReceipt>>,
    /// Preserve the Checker scalar AST commitment independently of the
    /// mutable instruction snapshot; resealing instructions cannot rewrite
    /// the source meaning of a settled tail.
    pub(super) v2_scalar_tail:
        Option<crate::core::ownership::OrdinaryMapOwnedAnyV2ScalarTailReceipt>,
    /// Independent lowering snapshot of source result/observation facts.
    /// Rebinding a public result receipt cannot manufacture a stdout borrow.
    pub(super) v2_println: Vec<crate::core::OrdinaryMapOwnedAnyV2ResultReceipt>,
    /// Exact Checker L1 authority for the owned Keys snapshot. This is
    /// captured before post-lowering validation and carried unchanged through
    /// mechanical body-seal refreshes.
    pub(super) v2_keys_snapshot_receipts:
        Vec<crate::core::OrdinaryMapOwnedAnyV2KeysSnapshotReceipt>,
    /// DynamicKeysGet owns a Keys snapshot as a nested source receipt rather
    /// than as a static KeysSnapshot profile. Preserve that exact Checker
    /// authority independently across mechanical body-seal refreshes.
    pub(super) v2_dynamic_keys_get_receipts:
        Vec<crate::core::OrdinaryMapOwnedAnyV2DynamicKeysGetReceipt>,
    /// Preserve the Checker authority for the exact nested Result
    /// New/Set/Values/index branch whose Values owner is validated by its
    /// independent MIR route replay rather than the generic Values result
    /// receipt.
    pub(super) v2_values_local_index_i32_result_branch:
        Option<crate::core::ownership::OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultBranchReceipt>,
    /// Exact Checker-owned cross-function Values caller edge for this body.
    /// Refreshes carry this snapshot forward unchanged.
    pub(super) v2_values_producer_caller:
        Option<crate::core::OrdinaryMapOwnedAnyV2ValuesProducerCallerReceipt>,
    /// Exact Checker source authority for the independent cross-call Values
    /// String caller profile. This remains separate from the local
    /// ValuesProducerCaller receipt and is carried unchanged across refreshes.
    pub(super) v2_values_crosscall_string_caller:
        Option<crate::core::ownership::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerReceipt>,
    /// Exact Checker authority for the Result-returning Values producer to
    /// `len()`-only caller profile. It remains separate from the local and
    /// cross-call String caller receipts.
    pub(super) v2_values_result_producer_caller:
        Option<crate::core::ownership::OrdinaryMapOwnedAnyV2ValuesResultProducerCallerReceipt>,
    /// Independent exact Map-root producer return proof. Present only on the
    /// producer body and only together with its validated unique caller.
    pub(super) v2_map_root_producer_return:
        Option<crate::core::OrdinaryMapOwnedAnyV2MapRootProducerReturnReceipt>,
    /// Exact Checker-owned Map-root producer/caller bridge edge. The caller
    /// copy is separate from the producer CFG receipt and survives refreshes.
    pub(super) v2_map_root_producer_caller:
        Option<crate::core::OrdinaryMapOwnedAnyV2MapRootProducerCallerReceipt>,
    /// Exact Checker snapshot for the Map-root → GetAny caller profile. On
    /// the producer this mirrors its CFG receipt; on the caller it mirrors
    /// the caller-local ownership receipt. It remains separate from Map-size.
    pub(super) v2_map_root_get_any_caller:
        Option<crate::core::OrdinaryMapOwnedAnyV2MapRootGetAnyCallerReceipt>,
    pub(super) generic_identity: Option<MirCheckerManagedAnyGenericIdentitySeal>,
    pub(super) generic_trap_cleanup: Option<MirCheckerManagedAnyGenericTrapCleanupSeal>,
}

/// Private Checker-lowering proof for the one generic body admitted to carry
/// a managed Any owner across an instance boundary. The public instance
/// contract is only a profile tag; this seal binds that tag to the exact
/// canonical specialization and the closed Clone/Drop body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MirCheckerManagedAnyGenericIdentitySeal {
    pub(super) template: NodeId,
    pub(super) instance: MirInstanceId,
    /// Exact type argument selected by Checker (normally the source
    /// DynamicAny identity).
    pub(super) type_argument: ResolvedTypeId,
    /// Canonical executable TypeDesc after ManagedAny materialization.
    pub(super) managed_type_argument: ResolvedTypeId,
    pub(super) parameter: MirValueId,
    pub(super) return_value: MirValueId,
    pub(super) clone_instruction: MirInstructionId,
    pub(super) drop_instruction: MirInstructionId,
    pub(super) clone_origin: NodeId,
    pub(super) drop_origin: NodeId,
}

/// Authority captured from the untouched Checker template, before concrete
/// Any values exist in MIR. A polymorphic `T` body has no ManagedAny body
/// seal; only this exact direct identity may acquire one during specialization.
/// The fields stay private so a mutable MIR body cannot manufacture a proof.
pub(super) struct MirCheckedManagedAnyIdentityTemplate {
    source_cfg: crate::core::ManagedAnyCfgReceipt,
    identity: MirCheckerManagedAnyGenericIdentitySeal,
}

pub(super) fn checked_managed_any_identity_template(
    callable: &crate::core::ResolvedCallable,
    program: &crate::core::CheckedProgram,
    catalog: &MirTypeCatalog,
    function: &MirFunction,
    arguments: &[ResolvedTypeId],
) -> Result<MirCheckedManagedAnyIdentityTemplate, String> {
    use crate::core::ir::{ResolvedExprKind, ResolvedType};

    let [binder] = callable.signature.generic_parameters.as_slice() else {
        return Err("ManagedAny identity template requires one Checker generic binder".into());
    };
    let [parameter] = callable.signature.parameters.as_slice() else {
        return Err("ManagedAny identity template requires one Checker parameter".into());
    };
    let [local] = callable.body.parameters.as_slice() else {
        return Err("ManagedAny identity template requires one Checker formal local".into());
    };
    let result = callable
        .body
        .root
        .result
        .as_deref()
        .ok_or("ManagedAny identity template requires the direct Checker result expression")?;
    if callable.signature.owner != callable.owner
        || callable.body.owner != callable.owner
        || callable.cfg.owner != callable.owner
        || callable.resources.owner != callable.owner
        || !matches!(program.resolved_types().get(&parameter.ty), Some(ResolvedType::GenericParameter(found)) if found == binder)
        || callable.signature.result != parameter.ty
        || parameter.mutable
        || parameter.permission.is_some()
        || parameter.has_default
        || !callable.signature.effects.is_empty()
        || !callable.contracts.is_empty()
        || !callable.body.captures.is_empty()
        || !callable.body.place_inputs.is_empty()
        || !callable.body.default_values.is_empty()
        || !callable.body.resolved_calls.is_empty()
        || callable.body.locals.len() != 1
        || callable
            .body
            .locals
            .get(local)
            .is_none_or(|value| value.ty != parameter.ty || value.mutable)
        || !callable.body.root.statements.is_empty()
        || callable.body.root.ty != parameter.ty
        || result.ty != parameter.ty
        || !result.effects.is_empty()
        || !result.backend_requirements.is_empty()
        || !matches!(&result.kind, ResolvedExprKind::Load(place) if place.base == *local && place.projections.is_empty())
    {
        return Err(
            "ManagedAny identity template is not the exact owned direct Checker identity".into(),
        );
    }
    let source_cfg = callable
        .resources
        .managed_any_cfg_receipt
        .as_ref()
        .ok_or("ManagedAny identity template has no Checker source-CFG receipt")?;
    if source_cfg != &crate::core::ManagedAnyCfgReceipt::from_cfg(&callable.cfg, &callable.body)
        || source_cfg.owner != callable.owner
        || callable.cfg.blocks.len() != 1
        || !callable.cfg.edges.is_empty()
        || source_cfg.reachable_blocks.len() != 1
        || !source_cfg.reachable_edges.is_empty()
        || !source_cfg.match_arms.is_empty()
        || source_cfg.reachable_blocks.get(&source_cfg.entry).is_none_or(|block| {
            !matches!(&block.terminator, crate::core::cfg::Terminator::Return { value: Some(value), implicit: true } if value == &result.node_id)
                || block.points.iter().filter(|point| point.source.node == result.node_id && point.kind == crate::core::cfg::CfgPointKind::Expression).count() != 1
        })
        || function.owner != callable.owner
        || function.ownership.checker_managed_any.is_some()
        || function.ownership.checker_managed_any_cfg_receipt.as_ref() != Some(source_cfg)
        || function.parameter_permissions.as_deref() != Some(&[None])
        || !function.contracts.is_empty()
    {
        return Err("ManagedAny identity template disagrees with its exact Checker source-CFG receipt".into());
    }
    super::validate_generic_identity_shape(function, &parameter.ty)?;
    let block = function
        .blocks
        .get(&function.entry)
        .ok_or("ManagedAny identity template has no MIR entry block")?;
    let [clone] = block.instructions.as_slice() else {
        return Err("ManagedAny identity template must lower to one direct Clone".into());
    };
    let MirInstructionKind::Clone {
        source,
        result: return_value,
    } = &clone.kind
    else {
        return Err("ManagedAny identity template must lower to one direct Clone".into());
    };
    if function.blocks.len() != 1
        || source.as_str() != format!("local:{}", local.0 .0)
        || return_value.as_str() != format!("expr:{}", result.node_id.0)
        || clone.id.as_str() != format!("inst:clone:{}", result.node_id.0)
    {
        return Err(
            "ManagedAny identity template MIR disagrees with the Checker formal/result identities"
                .into(),
        );
    }
    let managed_type_argument = catalog.validate_managed_any_identity_source_argument(arguments)?;
    let instance = MirInstanceId::for_template(&callable.owner, arguments)
        .map_err(|error| error.to_string())?;
    let drop_instruction = MirInstructionId::new(format!(
        "inst:managed_any_identity_drop:{}",
        instance.as_str()
    ))
    .map_err(|error| error.to_string())?;
    Ok(MirCheckedManagedAnyIdentityTemplate {
        source_cfg: source_cfg.clone(),
        identity: MirCheckerManagedAnyGenericIdentitySeal {
            template: callable.owner.clone(),
            instance,
            type_argument: arguments[0].clone(),
            managed_type_argument,
            parameter: source.clone(),
            return_value: return_value.clone(),
            clone_instruction: clone.id.clone(),
            drop_instruction,
            clone_origin: result.node_id.clone(),
            drop_origin: callable.body.root.node_id.clone(),
        },
    })
}

/// Transfer only the immutable direct-identity proof to its closed concrete
/// Clone/Drop body. No operation authority is recaptured from MIR receipts.
pub(super) fn seal_checked_managed_any_identity_instance(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    template: MirCheckedManagedAnyIdentityTemplate,
) -> Result<Box<MirCheckerManagedAnySeal>, String> {
    if function.ownership.checker_managed_any.is_some()
        || function.ownership.checker_managed_any_cfg_receipt.is_some()
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .is_some()
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_values_caller_receipt
            .is_some()
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_values_crosscall_string_caller_receipt
            .is_some()
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_values_result_producer_caller_receipt
            .is_some()
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_map_root_caller_receipt
            .is_some()
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_map_root_get_any_caller_receipt
            .is_some()
        || template.source_cfg.owner != template.identity.template
    {
        return Err(
            "ManagedAny identity specialization cannot acquire unrelated Checker authority".into(),
        );
    }
    let origins = BTreeMap::from([
        (
            template.identity.clone_instruction.clone(),
            template.identity.clone_origin.clone(),
        ),
        (
            template.identity.drop_instruction.clone(),
            template.identity.drop_origin.clone(),
        ),
    ]);
    let mut seal = seal_checked_lowering_with_snapshots(
        function,
        catalog,
        &origins,
        None,
        None,
        None,
        None,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .ok_or("ManagedAny identity specialization produced no owner seal")?;
    seal.generic_identity = Some(template.identity);
    let mut candidate = function.clone();
    candidate.ownership.checker_managed_any = Some(seal.clone());
    catalog.validate_managed_any_function(&candidate)?;
    Ok(seal)
}

/// Independent Checker-lowering receipt for a generic ManagedAny parameter
/// that the exact body must release before one guaranteed arithmetic trap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MirCheckerManagedAnyGenericTrapCleanupSeal {
    pub(super) template: NodeId,
    pub(super) instance: MirInstanceId,
    pub(super) type_argument: ResolvedTypeId,
    pub(super) managed_type_argument: ResolvedTypeId,
    pub(super) parameter: MirValueId,
    pub(super) trap: super::MirManagedAnyTrapKind,
    pub(super) setup_instructions: [MirInstructionId; 4],
    pub(super) drop_instruction: MirInstructionId,
    pub(super) trap_instruction: MirInstructionId,
    pub(super) trap_result: MirValueId,
    pub(super) drop_origin: NodeId,
    pub(super) trap_origin: NodeId,
    /// The generic source body has no concrete Any value seal. Preserve the
    /// exact Checker CFG snapshot that authorized the closed trap template.
    pub(super) source_cfg: crate::core::ManagedAnyCfgReceipt,
    pub(super) source_parameter_local: crate::core::ir::ResolvedLocalId,
    pub(super) source_binding_local: crate::core::ir::ResolvedLocalId,
    pub(super) setup_origins: [NodeId; 4],
    /// Complete generic template value catalog, before the source DynamicAny
    /// parameter is specialized to its executable ManagedAny TypeDesc.
    pub(super) source_value_types: BTreeMap<MirValueId, ResolvedTypeId>,
}

/// Immutable source-template proof captured from the Checked callable before
/// owner/type specialization or the synthetic Any Drop is inserted.
#[derive(Debug)]
pub(super) struct MirCheckedManagedAnyGenericTrapCleanupTemplate {
    receipt: MirCheckerManagedAnyGenericTrapCleanupSeal,
    source_entry: MirBlockId,
    source_block_parameters: Vec<super::MirBlockParameter>,
    source_instructions: Vec<MirInstruction>,
    source_terminator: MirTerminator,
}

/// Derive the executable cleanup specialization from the immutable Checker
/// source proof. The only permitted changes are the owner/type substitution,
/// removal of the generic CFG receipt, and one receipted Drop inserted
/// between the four frozen setup instructions and the source trap.
pub(super) fn seal_checked_managed_any_generic_trap_cleanup_instance(
    function: &mut MirFunction,
    catalog: &MirTypeCatalog,
    template: MirCheckedManagedAnyGenericTrapCleanupTemplate,
) -> Result<(), String> {
    let receipt = template.receipt;
    let expected_owner = NodeId(format!("function:mir:{}", receipt.instance.as_str()));
    if receipt.source_cfg.owner != receipt.template
        || function.owner != expected_owner
        || function.parameters.as_slice() != [receipt.parameter.clone()]
        || function.entry != template.source_entry
        || function.blocks.len() != 1
        || function.ownership.checker_managed_any.is_some()
        || function.ownership.checker_managed_any_cfg_receipt.as_ref() != Some(&receipt.source_cfg)
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .is_some()
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_values_caller_receipt
            .is_some()
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_values_crosscall_string_caller_receipt
            .is_some()
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_values_result_producer_caller_receipt
            .is_some()
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_map_root_caller_receipt
            .is_some()
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_map_root_get_any_caller_receipt
            .is_some()
        || !function
            .ownership
            .checker_ordinary_map_owned_any_v2
            .is_empty()
    {
        return Err(
            "ManagedAny trap-cleanup specialization is not derived from its exact sealed Checker template"
                .into(),
        );
    }
    let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
    let expected_values = receipt
        .source_value_types
        .iter()
        .map(|(value, source_type)| {
            (
                value.clone(),
                if value == &receipt.parameter {
                    receipt.managed_type_argument.clone()
                } else {
                    source_type.clone()
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    if function.parameter_permissions.as_deref() != Some(&[None])
        || function.result != i32_type
        || function.contracts.len() != 0
        || function.values.len() != expected_values.len()
        || function
            .values
            .iter()
            .map(|(value, info)| (value.clone(), info.ty.clone()))
            .collect::<BTreeMap<_, _>>()
            != expected_values
    {
        return Err(
            "ManagedAny trap-cleanup specialization changed the Checker source value map or ABI"
                .into(),
        );
    }
    let block = function
        .blocks
        .get(&template.source_entry)
        .ok_or("ManagedAny trap-cleanup specialized source entry is absent")?;
    if block.parameters != template.source_block_parameters {
        return Err(
            "ManagedAny trap-cleanup specialization changed source block parameters".into(),
        );
    }
    if block.terminator != template.source_terminator {
        return Err("ManagedAny trap-cleanup specialization changed source terminator".into());
    }
    if block.instructions != template.source_instructions {
        return Err(
            "ManagedAny trap-cleanup specialization changed frozen source instructions".into(),
        );
    }
    let drop_contract = super::MirManagedAnyContract {
        schema_version: MIR_MANAGED_ANY_SCHEMA_VERSION,
        descriptor_version: MIR_MANAGED_ANY_DESCRIPTOR_VERSION,
        owner: function.owner.clone(),
        origin: receipt.drop_origin.clone(),
        source: receipt.parameter.clone(),
        result: None,
        source_type: receipt.managed_type_argument.clone(),
        target_type: None,
        checker_conversion: None,
        checker_source_type: None,
        expected_kind: None,
    };
    let drop = MirInstruction {
        id: receipt.drop_instruction.clone(),
        kind: MirInstructionKind::ManagedAny {
            result: None,
            source: receipt.parameter.clone(),
            operation: MirManagedAnyOperation::Drop,
            contract: Some(drop_contract),
        },
    };
    if block.instructions[4].id != receipt.trap_instruction {
        return Err("ManagedAny trap-cleanup insertion point is not the frozen source trap".into());
    }
    function
        .blocks
        .get_mut(&template.source_entry)
        .expect("entry block was validated above")
        .instructions
        .insert(4, drop);
    function.ownership.checker_managed_any_cfg_receipt = None;

    let origins = receipt
        .setup_instructions
        .iter()
        .cloned()
        .zip(receipt.setup_origins.iter().cloned())
        .chain(std::iter::once((
            receipt.drop_instruction.clone(),
            receipt.drop_origin.clone(),
        )))
        .chain(std::iter::once((
            receipt.trap_instruction.clone(),
            receipt.trap_origin.clone(),
        )))
        .collect::<BTreeMap<_, _>>();
    let mut seal = seal_checked_lowering_with_snapshots(
        function,
        catalog,
        &origins,
        None,
        None,
        None,
        None,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .ok_or("ManagedAny trap-cleanup specialization produced no final owner seal")?;
    seal.generic_trap_cleanup = Some(receipt);
    function.ownership.checker_managed_any = Some(seal);
    catalog.validate_managed_any_function(function)
}

/// Bind the exact Checker-owned generic trap body and its pre-specialization
/// MIR identities. Generic `T` itself has no ManagedAny owner seal, so this
/// proof is the only authority from which the concrete cleanup body may be
/// derived.
pub(super) fn checked_managed_any_generic_trap_cleanup_template(
    callable: &crate::core::ResolvedCallable,
    program: &crate::core::CheckedProgram,
    catalog: &MirTypeCatalog,
    function: &MirFunction,
    arguments: &[ResolvedTypeId],
    trap: super::MirManagedAnyTrapKind,
) -> Result<MirCheckedManagedAnyGenericTrapCleanupTemplate, String> {
    use crate::core::cfg::{CfgPointKind, Terminator};
    use crate::core::ir::{ResolvedExprKind, ResolvedPatternKind, ResolvedStmtKind, ResolvedType};

    let [binder] = callable.signature.generic_parameters.as_slice() else {
        return Err("ManagedAny trap-cleanup source requires one Checker generic binder".into());
    };
    let [parameter] = callable.signature.parameters.as_slice() else {
        return Err("ManagedAny trap-cleanup source requires one Checker parameter".into());
    };
    let [parameter_local] = callable.body.parameters.as_slice() else {
        return Err("ManagedAny trap-cleanup source requires one formal local".into());
    };
    let generic_type = parameter.ty.clone();
    let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
    if callable.signature.owner != callable.owner
        || callable.body.owner != callable.owner
        || callable.cfg.owner != callable.owner
        || callable.resources.owner != callable.owner
        || !matches!(program.resolved_types().get(&generic_type), Some(ResolvedType::GenericParameter(found)) if found == binder)
        || parameter.mutable
        || parameter.permission.is_some()
        || parameter.has_default
        || callable.signature.result != i32_type
        || !callable.signature.effects.is_empty()
        || !callable.contracts.is_empty()
        || !callable.body.captures.is_empty()
        || !callable.body.place_inputs.is_empty()
        || !callable.body.default_values.is_empty()
        || !callable.body.resolved_calls.is_empty()
        || callable.body.locals.len() != 2
        || callable
            .body
            .locals
            .get(parameter_local)
            .is_none_or(|local| local.ty != generic_type || local.mutable)
        || callable.body.root.statements.len() != 1
        || callable.body.root.ty != i32_type
    {
        return Err(
            "ManagedAny trap-cleanup source is not the exact owned generic Checker callable".into(),
        );
    }

    let statement = callable
        .body
        .root
        .statements
        .first()
        .ok_or("ManagedAny trap-cleanup source has no binding statement")?;
    let ResolvedStmtKind::Bind {
        pattern,
        initializer: Some(initializer),
    } = &statement.kind
    else {
        return Err("ManagedAny trap-cleanup source must bind one integer initializer".into());
    };
    let ResolvedPatternKind::Binding {
        local: binding_local,
        by_reference: None,
    } = &pattern.kind
    else {
        return Err("ManagedAny trap-cleanup source binding must be an owned local".into());
    };
    if binding_local == parameter_local
        || pattern.ty != i32_type
        || callable
            .body
            .locals
            .get(binding_local)
            .is_none_or(|local| local.ty != i32_type || local.mutable)
        || !statement.backend_requirements.is_empty()
        || initializer.ty != i32_type
        || !initializer.effects.is_empty()
        || !initializer.backend_requirements.is_empty()
    {
        return Err(
            "ManagedAny trap-cleanup binding differs from its Checker local/type contract".into(),
        );
    }
    let ResolvedExprKind::Literal(ResolvedLiteral::Int(initializer_value)) = &initializer.kind
    else {
        return Err("ManagedAny trap-cleanup initializer must be an integer literal".into());
    };
    let result = callable
        .body
        .root
        .result
        .as_deref()
        .ok_or("ManagedAny trap-cleanup source has no arithmetic result")?;
    if result.ty != i32_type
        || !result.effects.is_empty()
        || !result.backend_requirements.is_empty()
    {
        return Err("ManagedAny trap-cleanup result must be effect-free i32".into());
    }
    let ResolvedExprKind::Binary { op, left, right } = &result.kind else {
        return Err("ManagedAny trap-cleanup source result must be one binary operation".into());
    };
    if left.ty != i32_type
        || right.ty != i32_type
        || !left.effects.is_empty()
        || !left.backend_requirements.is_empty()
        || !right.effects.is_empty()
        || !right.backend_requirements.is_empty()
    {
        return Err("ManagedAny trap-cleanup operands must be effect-free i32".into());
    }
    let is_local_load = |expression: &crate::core::ir::ResolvedExpr| {
        matches!(
            &expression.kind,
            ResolvedExprKind::Load(place)
                if place.base == *binding_local && place.projections.is_empty()
        )
    };
    let is_int_literal = |expression: &crate::core::ir::ResolvedExpr, expected: i64| {
        matches!(
            &expression.kind,
            ResolvedExprKind::Literal(ResolvedLiteral::Int(value)) if *value == expected
        )
    };
    match trap {
        super::MirManagedAnyTrapKind::DivisionByZeroI32
            if *initializer_value == 0
                && *op == ResolvedBinaryOp::Divide
                && is_int_literal(left, 1)
                && is_local_load(right) => {}
        super::MirManagedAnyTrapKind::SignedOverflowI32
            if *initializer_value == i32::MAX as i64
                && *op == ResolvedBinaryOp::Add
                && is_local_load(left)
                && is_int_literal(right, 1) => {}
        _ => {
            return Err(
                "ManagedAny trap-cleanup Checker body does not prove its exact trap".into(),
            );
        }
    }

    let source_cfg = crate::core::ManagedAnyCfgReceipt::from_cfg(&callable.cfg, &callable.body);
    if callable.resources.managed_any_cfg_receipt.as_ref() != Some(&source_cfg)
        || source_cfg.owner != callable.owner
        || source_cfg.entry.0 .0 != format!("{}/cfg:block:entry", callable.body.root.node_id.0)
        || source_cfg.reachable_blocks.len() != 1
        || !source_cfg.reachable_edges.is_empty()
        || !source_cfg.match_arms.is_empty()
        || callable.cfg.blocks.len() != 1
        || callable.cfg.entry.0 .0 != format!("{}/cfg:block:entry", callable.body.root.node_id.0)
        || callable.cfg.reachable != BTreeSet::from([callable.cfg.entry.clone()])
        || !callable.cfg.edges.is_empty()
    {
        return Err(
            "ManagedAny trap-cleanup source CFG differs from the exact Checked callable".into(),
        );
    }
    let source_block = source_cfg
        .reachable_blocks
        .get(&source_cfg.entry)
        .ok_or("ManagedAny trap-cleanup source CFG entry is not reachable")?;
    let (third, fourth) = match trap {
        super::MirManagedAnyTrapKind::DivisionByZeroI32 => (left, right),
        super::MirManagedAnyTrapKind::SignedOverflowI32 => (left, right),
    };
    let setup_origins = [
        initializer.node_id.clone(),
        statement.node_id.clone(),
        third.node_id.clone(),
        fourth.node_id.clone(),
    ];
    let trap_origin = result.node_id.clone();
    let expected_points = [
        (&setup_origins[0], CfgPointKind::Expression),
        (&setup_origins[1], CfgPointKind::Binding),
        (&setup_origins[2], CfgPointKind::Expression),
        (&setup_origins[3], CfgPointKind::Expression),
        (&trap_origin, CfgPointKind::Expression),
    ];
    if source_block.source.node != callable.body.root.node_id
        || !matches!(
            &source_block.terminator,
            Terminator::Return { value: Some(value), implicit: true } if value == &trap_origin
        )
        || source_block.points.len() != expected_points.len()
        || source_block
            .points
            .iter()
            .zip(expected_points)
            .any(|(point, (node, kind))| point.source.node != *node || point.kind != kind)
    {
        return Err(
            "ManagedAny trap-cleanup source CFG entry points or implicit Return are not exact"
                .into(),
        );
    }

    let managed_type_argument = catalog.validate_managed_any_identity_source_argument(arguments)?;
    let [type_argument] = arguments else {
        return Err(
            "ManagedAny trap-cleanup source requires one exact DynamicAny type argument".into(),
        );
    };
    let instance = MirInstanceId::for_template(&callable.owner, arguments)
        .map_err(|error| error.to_string())?;
    let parameter_value = MirValueId::new(format!("local:{}", parameter_local.0 .0))
        .map_err(|error| error.to_string())?;
    let binding_value = MirValueId::new(format!("local:{}", binding_local.0 .0))
        .map_err(|error| error.to_string())?;
    let expr_value = |node: &NodeId| MirValueId::new(format!("expr:{}", node.0));
    let initializer_value_id =
        expr_value(&initializer.node_id).map_err(|error| error.to_string())?;
    let left_value = expr_value(&left.node_id).map_err(|error| error.to_string())?;
    let right_value = expr_value(&right.node_id).map_err(|error| error.to_string())?;
    let trap_result = expr_value(&trap_origin).map_err(|error| error.to_string())?;
    let mut expected_source_value_types = BTreeMap::new();
    for (value, ty) in [
        (parameter_value.clone(), generic_type.clone()),
        (binding_value.clone(), i32_type.clone()),
        (initializer_value_id.clone(), i32_type.clone()),
        (left_value.clone(), i32_type.clone()),
        (right_value.clone(), i32_type.clone()),
        (trap_result.clone(), i32_type.clone()),
    ] {
        if expected_source_value_types.insert(value, ty).is_some() {
            return Err("ManagedAny trap-cleanup Checker source identities are not unique".into());
        }
    }
    let source_value_types = function
        .values
        .iter()
        .map(|(value, info)| (value.clone(), info.ty.clone()))
        .collect::<BTreeMap<_, _>>();
    let setup_instructions = [
        MirInstructionId::new(format!("inst:const:{}", initializer.node_id.0))
            .map_err(|error| error.to_string())?,
        MirInstructionId::new(format!("inst:bind:{}", statement.node_id.0))
            .map_err(|error| error.to_string())?,
        MirInstructionId::new(format!(
            "inst:{}:{}",
            if matches!(trap, super::MirManagedAnyTrapKind::DivisionByZeroI32) {
                "const"
            } else {
                "clone"
            },
            left.node_id.0
        ))
        .map_err(|error| error.to_string())?,
        MirInstructionId::new(format!(
            "inst:{}:{}",
            if matches!(trap, super::MirManagedAnyTrapKind::DivisionByZeroI32) {
                "clone"
            } else {
                "const"
            },
            right.node_id.0
        ))
        .map_err(|error| error.to_string())?,
    ];
    let trap_instruction = MirInstructionId::new(format!("inst:binary:{}", trap_origin.0))
        .map_err(|error| error.to_string())?;
    let source_entry = MirBlockId::new("bb.entry").map_err(|error| error.to_string())?;
    let block = function
        .blocks
        .get(&source_entry)
        .ok_or("ManagedAny trap-cleanup generic MIR has no canonical entry block")?;
    if function.owner != callable.owner
        || function.parameters.as_slice() != [parameter_value.clone()]
        || function.parameter_permissions.as_deref() != Some(&[None])
        || function.result != i32_type
        || function.contracts.len() != 0
        || function.blocks.len() != 1
        || function.values.len() != expected_source_value_types.len()
        || source_value_types != expected_source_value_types
        || function.ownership.checker_managed_any.is_some()
        || function.ownership.checker_managed_any_cfg_receipt.as_ref() != Some(&source_cfg)
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .is_some()
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_values_caller_receipt
            .is_some()
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_values_crosscall_string_caller_receipt
            .is_some()
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_values_result_producer_caller_receipt
            .is_some()
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_map_root_caller_receipt
            .is_some()
        || !function
            .ownership
            .checker_ordinary_map_owned_any_v2
            .is_empty()
        || !block.parameters.is_empty()
        || block.instructions.len() != 5
        || !matches!(
            &block.terminator,
            MirTerminator::Return { value: Some(value) } if value == &trap_result
        )
    {
        return Err(
            "ManagedAny trap-cleanup generic MIR identities differ from the Checked source body"
                .into(),
        );
    }
    let [first, second, third, fourth, final_trap] = block.instructions.as_slice() else {
        unreachable!("generic trap-cleanup instruction count checked above")
    };
    let observed_setup = [
        first.id.clone(),
        second.id.clone(),
        third.id.clone(),
        fourth.id.clone(),
    ];
    if observed_setup != setup_instructions
        || final_trap.id != trap_instruction
        || !matches!(
            &first.kind,
            MirInstructionKind::Const { result, literal: ResolvedLiteral::Int(value) }
                if result == &initializer_value_id && *value == *initializer_value
        )
        || !matches!(
            &second.kind,
            MirInstructionKind::Move { result, source }
                if result == &binding_value && source == &initializer_value_id
        )
    {
        return Err(
            "ManagedAny trap-cleanup generic MIR setup does not map to Checker source identities"
                .into(),
        );
    }
    match trap {
        super::MirManagedAnyTrapKind::DivisionByZeroI32 => {
            if !matches!(
                &third.kind,
                MirInstructionKind::Const { result, literal: ResolvedLiteral::Int(1) }
                    if result == &left_value
            ) || !matches!(
                &fourth.kind,
                MirInstructionKind::Clone { result, source }
                    if result == &right_value && source == &binding_value
            ) {
                return Err(
                    "division trap-cleanup generic MIR does not map operands to Checker nodes"
                        .into(),
                );
            }
        }
        super::MirManagedAnyTrapKind::SignedOverflowI32 => {
            if !matches!(
                &third.kind,
                MirInstructionKind::Clone { result, source }
                    if result == &left_value && source == &binding_value
            ) || !matches!(
                &fourth.kind,
                MirInstructionKind::Const { result, literal: ResolvedLiteral::Int(1) }
                    if result == &right_value
            ) {
                return Err(
                    "overflow trap-cleanup generic MIR does not map operands to Checker nodes"
                        .into(),
                );
            }
        }
    }
    if !matches!(
        &final_trap.kind,
        MirInstructionKind::Binary { result, op: observed, left: observed_left, right: observed_right }
            if result == &trap_result
                && observed == op
                && observed_left == &left_value
                && observed_right == &right_value
    ) {
        return Err(
            "ManagedAny trap-cleanup generic MIR result does not map to the Checker expression"
                .into(),
        );
    }

    let drop_instruction = MirInstructionId::new(format!(
        "inst:managed_any_trap_cleanup_drop:{}",
        instance.as_str()
    ))
    .map_err(|error| error.to_string())?;
    Ok(MirCheckedManagedAnyGenericTrapCleanupTemplate {
        receipt: MirCheckerManagedAnyGenericTrapCleanupSeal {
            template: callable.owner.clone(),
            instance,
            type_argument: type_argument.clone(),
            managed_type_argument,
            parameter: parameter_value,
            trap,
            setup_instructions,
            drop_instruction,
            trap_instruction,
            trap_result,
            drop_origin: callable.body.root.node_id.clone(),
            trap_origin,
            source_cfg,
            source_parameter_local: parameter_local.clone(),
            source_binding_local: binding_local.clone(),
            setup_origins,
            source_value_types: expected_source_value_types,
        },
        source_entry,
        source_block_parameters: block.parameters.clone(),
        source_instructions: block.instructions.clone(),
        source_terminator: block.terminator.clone(),
    })
}

impl MirTypeCatalog {
    pub fn is_managed_any_i32_string_v1(&self, ty: &ResolvedTypeId) -> bool {
        ty == &managed_any_i32_string_v1_type_id() && self.validate_managed_any_glue(ty).is_ok()
    }

    /// Admit this exact TypeDesc only for the separately sealed direct
    /// generic identity specialization. This does not broaden the general
    /// scalar/variant identity contract or authorize Any elsewhere.
    pub fn validate_managed_any_identity_source_argument(
        &self,
        arguments: &[ResolvedTypeId],
    ) -> Result<ResolvedTypeId, String> {
        if arguments.len() != 1 {
            return Err(format!(
                "ManagedAny identity requires one canonical type argument, got {}",
                arguments.len()
            ));
        }
        let argument = arguments
            .first()
            .ok_or_else(|| "ManagedAny identity argument is absent".to_string())?;
        let source = self
            .get(argument)
            .ok_or("ManagedAny identity source TypeDesc is missing")?;
        if source.kind != MirTypeKind::DynamicAny
            || source.layout != MirLayout::Opaque
            || source.abi != MirAbiClass::OpaqueHandle
            || source.ownership != MirOwnership::Move
        {
            return Err(
                "ManagedAny identity source argument must be the Checker DynamicAny TypeDesc"
                    .into(),
            );
        }
        let dynamic_any_ids = self
            .iter()
            .filter_map(|(id, descriptor)| {
                (descriptor.kind == MirTypeKind::DynamicAny).then_some(id)
            })
            .collect::<Vec<_>>();
        if dynamic_any_ids.len() != 1 || dynamic_any_ids.first().copied() != Some(argument) {
            return Err(
                "ManagedAny identity source argument is not the unique Checker DynamicAny TypeDesc in this catalog"
                    .into(),
            );
        }
        let managed = self.canonical_managed_any_type(argument);
        self.validate_managed_any_glue(&managed)?;
        Ok(managed)
    }

    /// Validate the public instance-table entry against its private Checker
    /// seal. Consumers call this before interpreting a generic ManagedAny
    /// body; canonical type identity or body shape alone is insufficient.
    pub fn validate_managed_any_identity_instance(
        &self,
        instance: &MirInstance,
        function: &MirFunction,
    ) -> Result<(), String> {
        if instance.contract != MirGenericInstanceContract::ManagedAnyIdentity {
            return Err("MIR instance is not the ManagedAny identity profile".into());
        }
        let managed_argument =
            self.validate_managed_any_identity_source_argument(&instance.arguments)?;
        let expected_id = MirInstanceId::for_template(&instance.template, &instance.arguments)
            .map_err(|error| error.to_string())?;
        if instance.id != expected_id
            || instance.function != function.owner
            || function.owner != NodeId(format!("function:mir:{}", expected_id.as_str()))
        {
            return Err(
                "ManagedAny identity instance disagrees with its canonical template/argument/owner identity"
                    .into(),
            );
        }
        self.validate_managed_any_function(function)?;
        let seal = function
            .ownership
            .checker_managed_any
            .as_ref()
            .and_then(|seal| seal.generic_identity.as_ref())
            .ok_or("ManagedAny identity instance has no private generic Checker seal")?;
        self.validate_managed_any_generic_identity_shape(function, seal)?;
        if seal.template != instance.template
            || seal.instance != instance.id
            || seal.type_argument != instance.arguments[0]
            || seal.managed_type_argument != managed_argument
        {
            return Err(
                "ManagedAny identity private Checker seal disagrees with its instance-table entry"
                    .into(),
            );
        }
        Ok(())
    }

    /// Validate one exact generic arithmetic-trap instance that releases its
    /// canonical ManagedAny parameter in the callee before the trap.
    pub fn validate_managed_any_trap_cleanup_instance(
        &self,
        instance: &MirInstance,
        function: &MirFunction,
    ) -> Result<(), String> {
        let trap = match &instance.contract {
            MirGenericInstanceContract::ManagedAnyTrapCleanup { trap } => *trap,
            _ => return Err("MIR instance is not the ManagedAny trap-cleanup profile".into()),
        };
        let managed_argument =
            self.validate_managed_any_identity_source_argument(&instance.arguments)?;
        let expected_id = MirInstanceId::for_template(&instance.template, &instance.arguments)
            .map_err(|error| error.to_string())?;
        if instance.id != expected_id
            || instance.function != function.owner
            || function.owner != NodeId(format!("function:mir:{}", expected_id.as_str()))
        {
            return Err(
                "ManagedAny trap-cleanup instance disagrees with its canonical template/argument/owner identity"
                    .into(),
            );
        }
        self.validate_managed_any_function(function)?;
        let seal = function
            .ownership
            .checker_managed_any
            .as_ref()
            .and_then(|seal| seal.generic_trap_cleanup.as_ref())
            .ok_or("ManagedAny trap-cleanup instance has no private generic Checker seal")?;
        self.validate_managed_any_generic_trap_cleanup_shape(function, seal)?;
        if seal.template != instance.template
            || seal.instance != instance.id
            || seal.type_argument != instance.arguments[0]
            || seal.managed_type_argument != managed_argument
            || seal.trap != trap
        {
            return Err(
                "ManagedAny trap-cleanup private Checker seal disagrees with its instance-table entry"
                    .into(),
            );
        }
        Ok(())
    }

    fn validate_managed_any_generic_identity_shape(
        &self,
        function: &MirFunction,
        identity: &MirCheckerManagedAnyGenericIdentitySeal,
    ) -> Result<(), String> {
        let managed_type_argument = self.validate_managed_any_identity_source_argument(
            std::slice::from_ref(&identity.type_argument),
        )?;
        let expected_instance = MirInstanceId::for_template(
            &identity.template,
            std::slice::from_ref(&identity.type_argument),
        )
        .map_err(|error| error.to_string())?;
        let expected_owner = NodeId(format!("function:mir:{}", expected_instance.as_str()));
        if identity.instance != expected_instance || function.owner != expected_owner {
            return Err(
                "ManagedAny identity private specialization owner/template/type argument is inconsistent"
                    .into(),
            );
        }
        if !function.contracts.is_empty() {
            return Err("ManagedAny identity body does not admit additional contracts".into());
        }
        let [parameter] = function.parameters.as_slice() else {
            return Err("ManagedAny identity function must have exactly one parameter".into());
        };
        if parameter != &identity.parameter
            || identity.managed_type_argument != managed_type_argument
            || function.result != managed_type_argument
            || function
                .values
                .get(parameter)
                .is_none_or(|value| value.ty != managed_type_argument)
        {
            return Err(
                "ManagedAny identity parameter/result does not use the sealed canonical owner type"
                    .into(),
            );
        }
        let expected_managed_values =
            BTreeSet::from([identity.parameter.clone(), identity.return_value.clone()]);
        if function.blocks.len() != 1
            || function
                .ownership
                .checker_managed_any
                .as_ref()
                .is_none_or(|seal| seal.managed_values != expected_managed_values)
        {
            return Err("ManagedAny identity must be a closed one-block owner transfer".into());
        }
        let block = function
            .blocks
            .get(&function.entry)
            .ok_or_else(|| "ManagedAny identity entry block is absent".to_string())?;
        if !block.parameters.is_empty() || identity.return_value == identity.parameter {
            return Err(
                "ManagedAny identity requires a fresh return owner and no block captures".into(),
            );
        }
        let [clone, drop] = block.instructions.as_slice() else {
            return Err("ManagedAny identity body must contain exactly a Clone then Drop".into());
        };
        let MirInstructionKind::ManagedAny {
            result: Some(clone_result),
            source: clone_source,
            operation: MirManagedAnyOperation::Clone,
            contract: Some(clone_contract),
        } = &clone.kind
        else {
            return Err("ManagedAny identity body must start with its receipted Clone".into());
        };
        if clone.id != identity.clone_instruction
            || clone_source != &identity.parameter
            || clone_result != &identity.return_value
            || clone_contract.owner != function.owner
            || clone_contract.origin != identity.clone_origin
            || clone_contract.source != identity.parameter
            || clone_contract.result.as_ref() != Some(&identity.return_value)
            || clone_contract.source_type != identity.managed_type_argument
            || clone_contract.target_type.as_ref() != Some(&identity.managed_type_argument)
            || clone_contract.schema_version != MIR_MANAGED_ANY_SCHEMA_VERSION
            || clone_contract.descriptor_version != MIR_MANAGED_ANY_DESCRIPTOR_VERSION
            || clone_contract.expected_kind.is_some()
        {
            return Err(
                "ManagedAny identity Clone disagrees with its private Checker value/operation receipt"
                    .into(),
            );
        }
        let MirInstructionKind::ManagedAny {
            result: None,
            source: drop_source,
            operation: MirManagedAnyOperation::Drop,
            contract: Some(drop_contract),
        } = &drop.kind
        else {
            return Err(
                "ManagedAny identity body must settle its input with a receipted Drop".into(),
            );
        };
        if drop.id != identity.drop_instruction
            || drop_source != &identity.parameter
            || drop_contract.owner != function.owner
            || drop_contract.origin != identity.drop_origin
            || drop_contract.source != identity.parameter
            || drop_contract.result.is_some()
            || drop_contract.source_type != identity.managed_type_argument
            || drop_contract.target_type.is_some()
            || drop_contract.schema_version != MIR_MANAGED_ANY_SCHEMA_VERSION
            || drop_contract.descriptor_version != MIR_MANAGED_ANY_DESCRIPTOR_VERSION
            || drop_contract.expected_kind.is_some()
        {
            return Err(
                "ManagedAny identity Drop disagrees with its private Checker value/operation receipt"
                    .into(),
            );
        }
        if !matches!(
            &block.terminator,
            MirTerminator::Return { value: Some(value) } if value == &identity.return_value
        ) {
            return Err("ManagedAny identity must return exactly the cloned owner".into());
        }
        Ok(())
    }

    fn validate_managed_any_generic_trap_cleanup_shape(
        &self,
        function: &MirFunction,
        receipt: &MirCheckerManagedAnyGenericTrapCleanupSeal,
    ) -> Result<(), String> {
        let managed_type_argument = self.validate_managed_any_identity_source_argument(
            std::slice::from_ref(&receipt.type_argument),
        )?;
        let expected_instance = MirInstanceId::for_template(
            &receipt.template,
            std::slice::from_ref(&receipt.type_argument),
        )
        .map_err(|error| error.to_string())?;
        let expected_owner = NodeId(format!("function:mir:{}", expected_instance.as_str()));
        if receipt.instance != expected_instance || function.owner != expected_owner {
            return Err(
                "ManagedAny trap-cleanup private specialization owner/template/type argument is inconsistent"
                    .into(),
            );
        }
        use crate::core::cfg::{CfgPointKind, Terminator};
        let expected_cfg_entry = format!("{}/cfg:block:entry", receipt.drop_origin.0);
        let source_cfg = &receipt.source_cfg;
        if source_cfg.owner != receipt.template
            || source_cfg.entry.0 .0 != expected_cfg_entry
            || source_cfg.reachable_blocks.len() != 1
            || !source_cfg.reachable_edges.is_empty()
            || !source_cfg.match_arms.is_empty()
        {
            return Err(
                "ManagedAny trap-cleanup source CFG is not the sealed canonical one-block template"
                    .into(),
            );
        }
        let source_block = source_cfg
            .reachable_blocks
            .get(&source_cfg.entry)
            .ok_or("ManagedAny trap-cleanup source CFG has no canonical entry block")?;
        let expected_source_points = [
            (&receipt.setup_origins[0], CfgPointKind::Expression),
            (&receipt.setup_origins[1], CfgPointKind::Binding),
            (&receipt.setup_origins[2], CfgPointKind::Expression),
            (&receipt.setup_origins[3], CfgPointKind::Expression),
            (&receipt.trap_origin, CfgPointKind::Expression),
        ];
        if source_block.source.node != receipt.drop_origin
            || source_block.points.len() != expected_source_points.len()
            || source_block
                .points
                .iter()
                .zip(expected_source_points)
                .any(|(point, (node, kind))| point.source.node != *node || point.kind != kind)
            || !matches!(
                &source_block.terminator,
                Terminator::Return { value: Some(value), implicit: true }
                    if value == &receipt.trap_origin
            )
        {
            return Err(
                "ManagedAny trap-cleanup source CFG root, ordered points, or implicit Return differs from its Checker proof"
                    .into(),
            );
        }
        let local_value = |local: &crate::core::ir::ResolvedLocalId| {
            MirValueId::new(format!("local:{}", local.0 .0))
        };
        let expr_value = |node: &NodeId| MirValueId::new(format!("expr:{}", node.0));
        let expected_parameter =
            local_value(&receipt.source_parameter_local).map_err(|error| error.to_string())?;
        let expected_binding =
            local_value(&receipt.source_binding_local).map_err(|error| error.to_string())?;
        let expected_initializer =
            expr_value(&receipt.setup_origins[0]).map_err(|error| error.to_string())?;
        let expected_left =
            expr_value(&receipt.setup_origins[2]).map_err(|error| error.to_string())?;
        let expected_right =
            expr_value(&receipt.setup_origins[3]).map_err(|error| error.to_string())?;
        let expected_trap_result =
            expr_value(&receipt.trap_origin).map_err(|error| error.to_string())?;
        let source_parameter_type = receipt
            .source_value_types
            .get(&expected_parameter)
            .cloned()
            .ok_or("ManagedAny trap-cleanup source value map omits its Checker parameter")?;
        let expected_source_values = BTreeMap::from([
            (expected_parameter.clone(), source_parameter_type.clone()),
            (
                expected_binding.clone(),
                crate::core::ir::primitive_type_id(PrimitiveType::I32),
            ),
            (
                expected_initializer.clone(),
                crate::core::ir::primitive_type_id(PrimitiveType::I32),
            ),
            (
                expected_left.clone(),
                crate::core::ir::primitive_type_id(PrimitiveType::I32),
            ),
            (
                expected_right.clone(),
                crate::core::ir::primitive_type_id(PrimitiveType::I32),
            ),
            (
                expected_trap_result.clone(),
                crate::core::ir::primitive_type_id(PrimitiveType::I32),
            ),
        ]);
        let expected_setup_instructions = [
            MirInstructionId::new(format!("inst:const:{}", receipt.setup_origins[0].0))
                .map_err(|error| error.to_string())?,
            MirInstructionId::new(format!("inst:bind:{}", receipt.setup_origins[1].0))
                .map_err(|error| error.to_string())?,
            MirInstructionId::new(format!(
                "inst:{}:{}",
                if receipt.trap == super::MirManagedAnyTrapKind::DivisionByZeroI32 {
                    "const"
                } else {
                    "clone"
                },
                receipt.setup_origins[2].0
            ))
            .map_err(|error| error.to_string())?,
            MirInstructionId::new(format!(
                "inst:{}:{}",
                if receipt.trap == super::MirManagedAnyTrapKind::DivisionByZeroI32 {
                    "clone"
                } else {
                    "const"
                },
                receipt.setup_origins[3].0
            ))
            .map_err(|error| error.to_string())?,
        ];
        let expected_trap_instruction =
            MirInstructionId::new(format!("inst:binary:{}", receipt.trap_origin.0))
                .map_err(|error| error.to_string())?;
        if receipt.parameter != expected_parameter
            || receipt.trap_result != expected_trap_result
            || receipt.setup_instructions != expected_setup_instructions
            || receipt.trap_instruction != expected_trap_instruction
            || source_parameter_type == crate::core::ir::primitive_type_id(PrimitiveType::I32)
            || receipt.source_value_types != expected_source_values
        {
            return Err(
                "ManagedAny trap-cleanup source values or instruction IDs do not map to Checker locals and expression nodes"
                    .into(),
            );
        }
        if function
            .ownership
            .checker_managed_any
            .as_ref()
            .is_some_and(|seal| seal.generic_identity.is_some())
        {
            return Err("ManagedAny trap-cleanup cannot share an identity receipt".into());
        }
        let final_seal = function
            .ownership
            .checker_managed_any
            .as_ref()
            .ok_or("ManagedAny trap-cleanup final owner seal is absent")?;
        if final_seal.branch_local_set_diamond_v2_snapshot.is_some()
            || final_seal.v2_scalar_tail.is_some()
            || !final_seal.v2_println.is_empty()
            || final_seal.v2_values_producer_caller.is_some()
            || final_seal.v2_values_crosscall_string_caller.is_some()
            || final_seal.v2_values_result_producer_caller.is_some()
            || final_seal.v2_map_root_producer_return.is_some()
            || final_seal.v2_map_root_producer_caller.is_some()
            || final_seal.v2_map_root_get_any_caller.is_some()
            || function
                .ownership
                .checker_ordinary_map_owned_any_v2_cfg_receipt
                .is_some()
            || function
                .ownership
                .checker_ordinary_map_owned_any_v2_values_caller_receipt
                .is_some()
            || function
                .ownership
                .checker_ordinary_map_owned_any_v2_values_crosscall_string_caller_receipt
                .is_some()
            || function
                .ownership
                .checker_ordinary_map_owned_any_v2_values_result_producer_caller_receipt
                .is_some()
            || function
                .ownership
                .checker_ordinary_map_owned_any_v2_map_root_caller_receipt
                .is_some()
            || function
                .ownership
                .checker_ordinary_map_owned_any_v2_map_root_get_any_caller_receipt
                .is_some()
            || !function
                .ownership
                .checker_ordinary_map_owned_any_v2
                .is_empty()
        {
            return Err(
                "ManagedAny trap-cleanup cannot carry pre-existing Map/Any v2 authority".into(),
            );
        }
        if !function.contracts.is_empty() {
            return Err("ManagedAny trap-cleanup body does not admit additional contracts".into());
        }
        let [parameter] = function.parameters.as_slice() else {
            return Err("ManagedAny trap-cleanup function must have exactly one parameter".into());
        };
        let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
        if parameter != &receipt.parameter
            || receipt.managed_type_argument != managed_type_argument
            || function.result != i32_type
            || function
                .values
                .get(parameter)
                .is_none_or(|value| value.ty != managed_type_argument)
        {
            return Err(
                "ManagedAny trap-cleanup parameter/result does not use its canonical ManagedAny/i32 ABI"
                    .into(),
            );
        }
        let expected_specialized_values = receipt
            .source_value_types
            .iter()
            .map(|(value, source_type)| {
                (
                    value.clone(),
                    if value == &receipt.parameter {
                        receipt.managed_type_argument.clone()
                    } else {
                        source_type.clone()
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        if function.values.len() != expected_specialized_values.len()
            || function
                .values
                .iter()
                .map(|(value, info)| (value.clone(), info.ty.clone()))
                .collect::<BTreeMap<_, _>>()
                != expected_specialized_values
        {
            return Err(
                "ManagedAny trap-cleanup specialized value map differs from its Checker template"
                    .into(),
            );
        }
        let expected_managed_values = BTreeSet::from([receipt.parameter.clone()]);
        if function.blocks.len() != 1
            || function
                .ownership
                .checker_managed_any
                .as_ref()
                .is_none_or(|seal| seal.managed_values != expected_managed_values)
        {
            return Err(
                "ManagedAny trap-cleanup must be a closed one-block callee-owned cleanup".into(),
            );
        }
        let block = function
            .blocks
            .get(&function.entry)
            .ok_or_else(|| "ManagedAny trap-cleanup entry block is absent".to_string())?;
        if !block.parameters.is_empty() {
            return Err("ManagedAny trap-cleanup block cannot capture block parameters".into());
        }
        let [first, second, third, fourth, drop, trap] = block.instructions.as_slice() else {
            return Err(
                "ManagedAny trap-cleanup body must have four pure setup ops, one Drop, and one trap op"
                    .into(),
            );
        };
        let observed_setup = [
            first.id.clone(),
            second.id.clone(),
            third.id.clone(),
            fourth.id.clone(),
        ];
        if observed_setup.as_slice() != receipt.setup_instructions.as_slice() {
            return Err(
                "ManagedAny trap-cleanup setup instruction identities disagree with the private Checker receipt"
                    .into(),
            );
        }
        fn const_value<'a>(
            instruction: &'a MirInstruction,
            function: &MirFunction,
            i32_type: &ResolvedTypeId,
        ) -> Option<(&'a MirValueId, i64)> {
            match &instruction.kind {
                MirInstructionKind::Const {
                    result,
                    literal: ResolvedLiteral::Int(value),
                } if function
                    .values
                    .get(result)
                    .is_some_and(|value| &value.ty == i32_type) =>
                {
                    Some((result, *value))
                }
                _ => None,
            }
        }
        let (initial_value, initial_constant) = const_value(first, function, &i32_type)
            .ok_or_else(|| {
                "ManagedAny trap-cleanup setup must begin with its Checker integer constant"
                    .to_string()
            })?;
        let MirInstructionKind::Move {
            result: bound_value,
            source: initializer,
        } = &second.kind
        else {
            return Err(
                "ManagedAny trap-cleanup binding must Move its literal into one local".into(),
            );
        };
        if initializer != initial_value
            || function
                .values
                .get(bound_value)
                .is_none_or(|value| value.ty != i32_type)
        {
            return Err(
                "ManagedAny trap-cleanup binding is not the exact i32 literal local".into(),
            );
        }
        let (left, right, expected_op, expected_constants) = match receipt.trap {
            super::MirManagedAnyTrapKind::DivisionByZeroI32 => {
                let Some((one_value, 1)) = const_value(third, function, &i32_type) else {
                    return Err(
                        "division cleanup receipt requires the exact i32 one numerator".into(),
                    );
                };
                let MirInstructionKind::Clone {
                    result: divisor_value,
                    source: divisor_local,
                } = &fourth.kind
                else {
                    return Err(
                        "division cleanup receipt requires a direct local divisor read".into(),
                    );
                };
                if initial_constant != 0
                    || divisor_local != bound_value
                    || function
                        .values
                        .get(divisor_value)
                        .is_none_or(|value| value.ty != i32_type)
                {
                    return Err(
                        "division cleanup divisor read does not use its exact zero local".into(),
                    );
                }
                (one_value, divisor_value, ResolvedBinaryOp::Divide, (0, 1))
            }
            super::MirManagedAnyTrapKind::SignedOverflowI32 => {
                let MirInstructionKind::Clone {
                    result: maximum_value,
                    source: maximum_local,
                } = &third.kind
                else {
                    return Err(
                        "overflow cleanup receipt requires a direct local maximum read".into(),
                    );
                };
                let Some((one_value, 1)) = const_value(fourth, function, &i32_type) else {
                    return Err("overflow cleanup receipt requires the exact i32 one addend".into());
                };
                if initial_constant != i32::MAX as i64
                    || maximum_local != bound_value
                    || function
                        .values
                        .get(maximum_value)
                        .is_none_or(|value| value.ty != i32_type)
                {
                    return Err(
                        "overflow cleanup operand does not use its exact i32::MAX local".into(),
                    );
                }
                (
                    maximum_value,
                    one_value,
                    ResolvedBinaryOp::Add,
                    (i32::MAX as i64, 1),
                )
            }
        };
        if initial_constant != expected_constants.0 {
            return Err("ManagedAny trap-cleanup bound local has the wrong exact constant".into());
        }
        if initial_value != &expected_initializer
            || bound_value != &expected_binding
            || left != &expected_left
            || right != &expected_right
        {
            return Err(
                "ManagedAny trap-cleanup setup and operands are not the exact Checker local/expression identities"
                    .into(),
            );
        }
        let MirInstructionKind::ManagedAny {
            result: None,
            source: drop_source,
            operation: MirManagedAnyOperation::Drop,
            contract: Some(drop_contract),
        } = &drop.kind
        else {
            return Err("ManagedAny trap must be preceded by its receipted owner Drop".into());
        };
        let expected_drop_id = MirInstructionId::new(format!(
            "inst:managed_any_trap_cleanup_drop:{}",
            receipt.instance.as_str()
        ))
        .map_err(|error| error.to_string())?;
        if drop.id != receipt.drop_instruction
            || drop.id != expected_drop_id
            || drop_source != &receipt.parameter
            || drop_contract.owner != function.owner
            || drop_contract.origin != receipt.drop_origin
            || drop_contract.source != receipt.parameter
            || drop_contract.result.is_some()
            || drop_contract.source_type != receipt.managed_type_argument
            || drop_contract.target_type.is_some()
            || drop_contract.schema_version != MIR_MANAGED_ANY_SCHEMA_VERSION
            || drop_contract.descriptor_version != MIR_MANAGED_ANY_DESCRIPTOR_VERSION
            || drop_contract.expected_kind.is_some()
        {
            return Err(
                "ManagedAny trap-cleanup Drop disagrees with its private Checker receipt".into(),
            );
        }
        let MirInstructionKind::Binary {
            result,
            op,
            left: observed_left,
            right: observed_right,
        } = &trap.kind
        else {
            return Err("ManagedAny trap-cleanup body must end with one arithmetic Binary".into());
        };
        let expected_trap_id =
            MirInstructionId::new(format!("inst:binary:{}", receipt.trap_origin.0))
                .map_err(|error| error.to_string())?;
        if trap.id != receipt.trap_instruction
            || trap.id != expected_trap_id
            || observed_left != left
            || observed_right != right
            || *op != expected_op
            || function
                .values
                .get(left)
                .is_none_or(|value| value.ty != i32_type)
            || function
                .values
                .get(right)
                .is_none_or(|value| value.ty != i32_type)
            || function
                .values
                .get(result)
                .is_none_or(|value| value.ty != i32_type)
            || receipt.trap_result != *result
        {
            return Err(
                "ManagedAny trap operation does not match the sealed exact i32 trap and cleanup order"
                    .into(),
            );
        }
        if !matches!(
            &block.terminator,
            MirTerminator::Return { value: Some(value) } if value == result
        ) {
            return Err(
                "ManagedAny trap-cleanup must return exactly the trapping i32 value".into(),
            );
        }
        let expected_origins = receipt
            .setup_instructions
            .iter()
            .cloned()
            .zip(receipt.setup_origins.iter().cloned())
            .chain(std::iter::once((
                receipt.drop_instruction.clone(),
                receipt.drop_origin.clone(),
            )))
            .chain(std::iter::once((
                receipt.trap_instruction.clone(),
                receipt.trap_origin.clone(),
            )))
            .collect::<BTreeMap<_, _>>();
        let observed_origins = final_seal
            .instructions
            .iter()
            .map(|(_, origin, instruction)| (instruction.id.clone(), origin.clone()))
            .collect::<BTreeMap<_, _>>();
        if observed_origins != expected_origins || observed_origins.len() != 6 {
            return Err(
                "ManagedAny trap-cleanup instruction origins are not frozen to the Checked source nodes"
                    .into(),
            );
        }
        Ok(())
    }

    pub(super) fn validate_managed_any_glue(&self, ty: &ResolvedTypeId) -> Result<(), String> {
        let expected = MirGlueContract {
            move_out: MirGlueKind::ManagedAnyI32StringV1,
            clone: MirGlueKind::ManagedAnyI32StringV1,
            drop: MirGlueKind::ManagedAnyI32StringV1,
        };
        let descriptor = self.get(ty).ok_or("ManagedAny TypeDesc is missing")?;
        if ty != &managed_any_i32_string_v1_type_id()
            || descriptor.id != *ty
            || descriptor.kind != MirTypeKind::ManagedAnyI32StringV1
            || descriptor.layout != MirLayout::ManagedAnyI32StringV1
            || descriptor.ownership != MirOwnership::Move
            || descriptor.abi != MirAbiClass::OpaqueHandle
            || descriptor.glue != expected
            || !descriptor.needs_clone_glue
            || !descriptor.needs_drop_glue
            || descriptor.session_protocol.is_some()
            || descriptor.drop_plan.is_some()
            || descriptor.variant_drop_plan.is_some()
        {
            return Err(
                "ManagedAny exact descriptor identity/version/ownership/glue is inconsistent"
                    .into(),
            );
        }
        Ok(())
    }

    /// Retarget only Any and concrete tuple products in a program carrying an
    /// exact GetAny Checker action. No other aggregate receives permission.
    pub(crate) fn canonical_managed_any_type(&self, ty: &ResolvedTypeId) -> ResolvedTypeId {
        if self.get(&managed_any_i32_string_v1_type_id()).is_none() {
            return ty.clone();
        }
        match self.get(ty).map(|descriptor| &descriptor.kind) {
            Some(MirTypeKind::DynamicAny) => managed_any_i32_string_v1_type_id(),
            Some(MirTypeKind::List) => {
                // Checker keeps the source spelling `List<Any>` in the
                // resolved type table.  The executable MIR boundary must
                // carry the materialized ManagedAny element identity so the
                // List glue/runtime consumers cannot infer an opaque element
                // kind from a handle alone.  Only the one-level list shape is
                // admitted here; nested or aggregate Any shapes remain
                // fail-closed until they receive an independent receipt.
                let Some(MirLayout::List { element }) =
                    self.get(ty).map(|descriptor| &descriptor.layout)
                else {
                    return ty.clone();
                };
                let mapped = self.canonical_managed_any_type(element);
                if mapped != *element && mapped == managed_any_i32_string_v1_type_id() {
                    // The synthetic descriptor is materialized only for a
                    // Checker-receipted `values()` result.  Refuse to
                    // canonicalize an unrelated List<Any> merely because a
                    // different operation in the same program introduced the
                    // ManagedAny leaf descriptor.
                    let canonical = managed_any_list_i32_string_v1_type_id(ty);
                    if self.get(&canonical).is_some() {
                        return canonical;
                    }
                }
                ty.clone()
            }
            Some(MirTypeKind::Tuple { .. }) => {
                if let Some(MirLayout::Tuple(elements)) =
                    self.get(ty).map(|descriptor| &descriptor.layout)
                {
                    let mapped = elements
                        .iter()
                        .map(|element| self.canonical_managed_any_type(element))
                        .collect::<Vec<_>>();
                    if mapped != *elements {
                        let boolean = crate::core::ir::primitive_type_id(PrimitiveType::Bool);
                        if mapped == vec![boolean, managed_any_i32_string_v1_type_id()] {
                            return ordinary_map_get_any_tuple_i32_string_v1_type_id();
                        }
                        return ResolvedTypeId::synthetic(&format!(
                            "mimi-mir:ManagedAnyTuple:v1:{}",
                            ty.as_str()
                        ));
                    }
                }
                ty.clone()
            }
            _ => ty.clone(),
        }
    }

    pub fn contains_managed_any(&self, ty: &ResolvedTypeId) -> bool {
        fn visit(
            catalog: &MirTypeCatalog,
            ty: &ResolvedTypeId,
            seen: &mut BTreeSet<ResolvedTypeId>,
        ) -> bool {
            if !seen.insert(ty.clone()) {
                return false;
            }
            if ty == &managed_any_i32_string_v1_type_id() {
                return true;
            }
            matches!(catalog.get(ty).map(|descriptor| &descriptor.layout), Some(MirLayout::Tuple(elements))
                if elements.iter().any(|child| visit(catalog, child, seen)))
                || matches!(catalog.get(ty).map(|descriptor| &descriptor.layout), Some(MirLayout::List { element })
                    if visit(catalog, element, seen))
        }
        visit(self, ty, &mut BTreeSet::new())
    }

    /// Validate the exact move-owned list shape returned by ordinary Map
    /// `values()`: one List handle whose element is the canonical
    /// generation-bearing ManagedAny owner.  This is deliberately separate
    /// from the generic Copy-scalar List validators so a backend cannot widen
    /// the ordinary list ABI merely because it sees a List layout.
    pub fn validate_managed_any_list_type(&self, ty: &ResolvedTypeId) -> Result<(), String> {
        let descriptor = self.get(ty).ok_or("ManagedAny List TypeDesc is missing")?;
        let MirLayout::List { element } = &descriptor.layout else {
            return Err("ManagedAny List result has no List layout".into());
        };
        if descriptor.kind != MirTypeKind::List || *element != managed_any_i32_string_v1_type_id() {
            return Err("ManagedAny List result must be List<ManagedAnyI32StringV1>".into());
        }
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            self.validate_list_glue(ty, operation).map_err(|message| {
                format!("ManagedAny List result has invalid {operation:?} glue: {message}")
            })?;
        }
        Ok(())
    }

    /// Detect either source DynamicAny or its canonical managed executable
    /// owner anywhere in a TypeDesc graph. This is intentionally separate
    /// from `contains_managed_any`, whose narrower behavior is already used
    /// by existing tuple-owner analyses. Missing child descriptors fail
    /// closed as unclassifiable; cycles are bounded by the visited-ID set.
    pub fn contains_dynamic_or_managed_any(&self, ty: &ResolvedTypeId) -> bool {
        fn visit(
            catalog: &MirTypeCatalog,
            ty: &ResolvedTypeId,
            seen: &mut BTreeSet<ResolvedTypeId>,
        ) -> bool {
            if !seen.insert(ty.clone()) {
                return false;
            }
            let Some(descriptor) = catalog.get(ty) else {
                return true;
            };
            if matches!(
                &descriptor.kind,
                MirTypeKind::DynamicAny
                    | MirTypeKind::ManagedAnyI32StringV1
                    | MirTypeKind::OrdinaryMapOwnedAnyV2
                    | MirTypeKind::OrdinaryMapOwnedAnyV2Payload
                    | MirTypeKind::OrdinaryMapOwnedAnyV2Values
                    | MirTypeKind::OrdinaryMapOwnedAnyV2ValuesElement
            ) || ty == &managed_any_i32_string_v1_type_id()
                || ty == &ordinary_map_owned_any_v2_type_id()
                || ty == &ordinary_map_owned_any_v2_values_element_type_id()
            {
                return true;
            }
            let contains_fields =
                |fields: &[super::types::MirFieldDesc], seen: &mut BTreeSet<ResolvedTypeId>| {
                    fields.iter().any(|field| visit(catalog, &field.ty, seen))
                };
            let contains_variants =
                |variants: &[super::types::MirVariantDesc], seen: &mut BTreeSet<ResolvedTypeId>| {
                    variants
                        .iter()
                        .any(|variant| contains_fields(&variant.fields, seen))
                };
            let layout_contains_any = match &descriptor.layout {
                MirLayout::ActorState { fields, .. } | MirLayout::Record { fields, .. } => {
                    contains_fields(fields, seen)
                }
                MirLayout::Pointer { target } => target
                    .as_ref()
                    .map_or(true, |child| visit(catalog, child, seen)),
                MirLayout::Tuple(elements) => {
                    elements.iter().any(|child| visit(catalog, child, seen))
                }
                MirLayout::Option { inner, variants } => {
                    visit(catalog, inner, seen) || contains_variants(variants, seen)
                }
                MirLayout::Result {
                    ok,
                    error,
                    variants,
                } => {
                    visit(catalog, ok, seen)
                        || visit(catalog, error, seen)
                        || contains_variants(variants, seen)
                }
                MirLayout::Enum { variants, .. } => contains_variants(variants, seen),
                MirLayout::Array { element, .. }
                | MirLayout::Newtype { inner: element, .. }
                | MirLayout::List { element }
                | MirLayout::Set { element } => visit(catalog, element, seen),
                MirLayout::Unit
                | MirLayout::Scalar
                | MirLayout::Handle
                | MirLayout::MapRoot
                | MirLayout::MapAnyOwnerV1
                | MirLayout::OrdinaryMapOwnerLifecycleI32StringV1
                | MirLayout::OrdinaryMapOwnedGetI32StringV1
                | MirLayout::OrdinaryMapOwnedAnyV2
                | MirLayout::ManagedAnyI32StringV1
                | MirLayout::Opaque => false,
                MirLayout::OrdinaryMapOwnedAnyV2Payload => true,
                MirLayout::OrdinaryMapOwnedAnyV2Values => true,
                MirLayout::OrdinaryMapOwnedAnyV2ValuesElement => true,
            };
            let resolved_children_contain_any = catalog
                .resolved_children
                .get(ty)
                .is_some_and(|children| children.iter().any(|child| visit(catalog, child, seen)));
            let unresolved_child_metadata = !catalog.resolved_children.contains_key(ty)
                && (matches!(
                    &descriptor.kind,
                    MirTypeKind::Function { .. } | MirTypeKind::Ownership(_)
                ) || (descriptor.kind == MirTypeKind::Nominal
                    && matches!(&descriptor.layout, MirLayout::Handle | MirLayout::Opaque)));
            let session_protocol_contains_any = descriptor
                .session_protocol
                .as_ref()
                .is_some_and(|child| visit(catalog, child, seen));
            let drop_plan_contains_any =
                descriptor.drop_plan.as_ref().is_some_and(|plan| {
                    plan.fields
                        .iter()
                        .any(|field| visit(catalog, &field.ty, seen))
                }) || descriptor.variant_drop_plan.as_ref().is_some_and(|plans| {
                    plans.iter().any(|plan| {
                        plan.fields
                            .iter()
                            .any(|field| visit(catalog, &field.ty, seen))
                    })
                });
            layout_contains_any
                || resolved_children_contain_any
                || unresolved_child_metadata
                || session_protocol_contains_any
                || drop_plan_contains_any
        }
        visit(self, ty, &mut BTreeSet::new())
    }

    pub fn validate_ordinary_map_owned_get_any_result_type(
        &self,
        ty: &ResolvedTypeId,
    ) -> Result<(), String> {
        let descriptor = self.get(ty).ok_or("GetAny result TypeDesc is missing")?;
        let boolean = crate::core::ir::primitive_type_id(PrimitiveType::Bool);
        if ty != &ordinary_map_get_any_tuple_i32_string_v1_type_id()
            || descriptor.kind != (MirTypeKind::Tuple { arity: 2 })
            || descriptor.layout
                != MirLayout::Tuple(vec![boolean, managed_any_i32_string_v1_type_id()])
        {
            return Err(
                "GetAny must return the exact owned (bool, ManagedAny) tuple descriptor".into(),
            );
        }
        self.validate_recursive_tuple_abi(ty)
    }

    /// Validate the independent AnyOwner-v2 GetAny product. Its payload token
    /// has a separate TypeDesc and must never be admitted by the V1
    /// ManagedAny tuple contract above.
    pub fn validate_ordinary_map_owned_any_v2_get_any_result_type(
        &self,
        ty: &ResolvedTypeId,
    ) -> Result<(), String> {
        let descriptor = self
            .get(ty)
            .ok_or("AnyOwner-v2 GetAny result TypeDesc is missing")?;
        let boolean = crate::core::ir::primitive_type_id(PrimitiveType::Bool);
        if ty != &ordinary_map_owned_any_v2_get_result_type_id()
            || descriptor.kind != (MirTypeKind::Tuple { arity: 2 })
            || descriptor.layout
                != MirLayout::Tuple(vec![
                    boolean.clone(),
                    ordinary_map_owned_any_v2_payload_type_id(),
                ])
        {
            return Err(
                "AnyOwner-v2 GetAny must return its exact owned (bool, V2 payload token) tuple"
                    .into(),
            );
        }
        self.validate_copy_scalar(&boolean)?;
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            self.validate_glue(ty, operation)?;
        }
        Ok(())
    }

    pub fn validate_managed_any_contract(
        &self,
        function: &MirFunction,
        source: &MirValueId,
        result: Option<&MirValueId>,
        operation: MirManagedAnyOperation,
        contract: Option<&MirManagedAnyContract>,
    ) -> Result<(), String> {
        let receipt =
            contract.ok_or("ManagedAny operation is missing its exact owner/type receipt")?;
        let source_type = function
            .values
            .get(source)
            .ok_or("ManagedAny source identity is missing")?
            .ty
            .clone();
        let target_type = result
            .map(|result| {
                function
                    .values
                    .get(result)
                    .map(|value| value.ty.clone())
                    .ok_or("ManagedAny result identity is missing")
            })
            .transpose()?;
        if operation != MirManagedAnyOperation::PackI32 {
            self.validate_managed_any_glue(&source_type)?;
            if receipt.checker_conversion.is_some() {
                return Err(
                    "ManagedAny non-pack operation carries a stray Checker conversion".into(),
                );
            }
        }
        let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
        let managed_type = managed_any_i32_string_v1_type_id();
        if operation == MirManagedAnyOperation::PackI32 {
            let conversion = receipt
                .checker_conversion
                .as_ref()
                .ok_or("ManagedAny PackI32 has no exact Checker DynamicAnyPack conversion")?;
            let dynamic_target_is_canonical = self
                .get(&conversion.to)
                .is_some_and(|descriptor| descriptor.kind == MirTypeKind::DynamicAny)
                && self.canonical_managed_any_type(&conversion.to) == managed_type;
            if source_type != i32_type
                || conversion.kind != CheckedConversionKind::DynamicAnyPack
                || conversion.from != source_type
                || !dynamic_target_is_canonical
                || target_type.as_ref() != Some(&managed_type)
            {
                return Err(
                    "ManagedAny PackI32 requires exact i32 -> Checker DynamicAny and canonical ManagedAny v1 types"
                        .into(),
                );
            }
        }
        let checker_source_valid = if operation == MirManagedAnyOperation::Println {
            let source = receipt
                .checker_source_type
                .as_ref()
                .ok_or("ManagedAny Println has no Checker DynamicAny source type")?;
            self.validate_managed_any_identity_source_argument(std::slice::from_ref(source))?
                == managed_type
        } else {
            receipt.checker_source_type.is_none()
        };
        let expected_target = match operation {
            MirManagedAnyOperation::Clone => Some(source_type.clone()),
            MirManagedAnyOperation::Drop => None,
            MirManagedAnyOperation::Println => {
                Some(crate::core::ir::primitive_type_id(PrimitiveType::Unit))
            }
            MirManagedAnyOperation::PackI32 => Some(managed_type),
            MirManagedAnyOperation::UnpackI32 => {
                Some(crate::core::ir::primitive_type_id(PrimitiveType::I32))
            }
            MirManagedAnyOperation::UnpackString => {
                Some(crate::core::ir::primitive_type_id(PrimitiveType::String))
            }
        };
        if receipt.schema_version != MIR_MANAGED_ANY_SCHEMA_VERSION
            || receipt.descriptor_version != MIR_MANAGED_ANY_DESCRIPTOR_VERSION
            || receipt.owner != function.owner
            || receipt.origin.0.trim().is_empty()
            || receipt.source != *source
            || receipt.result.as_ref() != result
            || receipt.source_type != source_type
            || receipt.target_type != target_type
            || target_type != expected_target
            || (operation == MirManagedAnyOperation::PackI32)
                != receipt.checker_conversion.is_some()
            || !checker_source_valid
            || (operation == MirManagedAnyOperation::Println)
                != receipt.checker_source_type.is_some()
            || receipt.expected_kind != operation.expected_kind()
            || result.is_some_and(|result| result == source)
        {
            return Err(
                "ManagedAny operation disagrees with exact owner/value/kind/version/borrow receipt"
                    .into(),
            );
        }
        let seal = function
            .ownership
            .checker_managed_any
            .as_ref()
            .ok_or("ManagedAny has no private Checker authority")?;
        let matches = seal.instructions.iter().filter(|(_, origin, instruction)| *origin == receipt.origin
            && matches!(&instruction.kind, MirInstructionKind::ManagedAny { source: actual_source,
                result: actual_result, operation: actual_operation, contract: Some(actual_contract) }
                if actual_source == source && actual_result.as_ref() == result
                    && *actual_operation == operation && actual_contract == receipt)).count();
        if matches != 1 {
            return Err(
                "ManagedAny operation has no unique sealed Checker conversion/owner receipt".into(),
            );
        }
        Ok(())
    }
}

/// Validate the dedicated Result-valued GetAny owner authority before the
/// generic ManagedAny analysis. The instruction returns a move-only envelope:
/// its Ok arm creates the pair/payload owner, while Err carries only a Copy
/// status. This authority must never be composed with the old tuple GetAny or
/// the broad AnyOwner-v2 action ledger.
fn validate_ordinary_map_set_get_any_result_i32_owner(
    function: &MirFunction,
) -> Result<(), String> {
    use crate::core::ownership::{
        OrdinaryMapOwnedAnyV2GetApi, OrdinaryMapOwnedAnyV2SetGetAnyResultI32LineageProfileVersion,
    };

    let receipt = function
        .ownership
        .checker_ordinary_map_owned_any_v2_set_get_any_result_i32_lineage
        .as_ref()
        .ok_or("ordinary Map GetAnyResult has no dedicated Checker receipt")?;
    if receipt.profile_version != OrdinaryMapOwnedAnyV2SetGetAnyResultI32LineageProfileVersion::V1
        || receipt.get_api != OrdinaryMapOwnedAnyV2GetApi::MapGetAnyResult
        || receipt.owner != function.owner
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_set_get_any_result_i32_lineage_commitment
            .as_ref()
            != Some(&format!("{receipt:?}"))
    {
        return Err(
            "ordinary Map GetAnyResult Checker receipt identity or commitment is invalid".into(),
        );
    }
    if function
        .ownership
        .checker_ordinary_map_owned_any_v2_set_get_any_i32_lineage
        .is_some()
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_set_get_i32_lineage
            .is_some()
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_set_get_string_lineage
            .is_some()
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_set_lineage
            .is_some()
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_new_result
            .is_some()
        || !function
            .ownership
            .checker_ordinary_map_owned_any_v2
            .is_empty()
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .is_some()
        || function.ownership.checker_managed_any.is_some()
        || function.ownership.checker_managed_any_cfg_receipt.is_some()
    {
        return Err(
            "ordinary Map GetAnyResult cannot mix the tuple GetAny, broad V2, or generic ManagedAny receipts".into(),
        );
    }

    let instructions = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .collect::<Vec<_>>();
    let get_any_result_count = instructions
        .iter()
        .filter(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::OrdinaryMapOwnedAnyV2GetAnyResult { .. }
            )
        })
        .count();
    let tuple_get_any_count = instructions
        .iter()
        .filter(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { .. }
            )
        })
        .count();
    if get_any_result_count != 1 || tuple_get_any_count != 0 {
        return Err(format!(
            "ordinary Map GetAnyResult requires exactly one Result-valued GetAny and no legacy tuple GetAny; observed {get_any_result_count}/{tuple_get_any_count}"
        ));
    }

    let local_value = |local: &crate::core::ir::ResolvedLocalId| {
        MirValueId::new(format!("local:{}", local.0 .0)).map_err(|error| error.to_string())
    };
    let expected_result = local_value(&receipt.get_result_local)?;
    let expected_root = local_value(&receipt.get_receiver_local)?;
    let expected_key = MirValueId::new(format!("literal:{}", receipt.get_key.0))
        .map_err(|error| error.to_string())?;
    let get_any_instruction = instructions
        .iter()
        .find(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::OrdinaryMapOwnedAnyV2GetAnyResult { .. }
            )
        })
        .ok_or("ordinary Map GetAnyResult instruction is absent")?;
    let MirInstructionKind::OrdinaryMapOwnedAnyV2GetAnyResult { result, root, key } =
        &get_any_instruction.kind
    else {
        return Err("ordinary Map GetAnyResult instruction changed kind".into());
    };
    let envelope_type = super::types::ordinary_map_owned_any_v2_get_any_result_envelope_type_id();
    let root_type = super::types::ordinary_map_owned_any_v2_type_id();
    let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
    if result != &expected_result
        || root != &expected_root
        || key != &expected_key
        || function
            .values
            .get(result)
            .is_none_or(|value| value.ty != envelope_type)
        || function
            .values
            .get(root)
            .is_none_or(|value| value.ty != root_type)
        || function
            .values
            .get(key)
            .is_none_or(|value| value.ty != string_type)
    {
        return Err(
            "ordinary Map GetAnyResult does not borrow its exact updated root/key into the canonical Result envelope".into(),
        );
    }

    let expected_pair = local_value(&receipt.get_ok_pair_local)?;
    let expected_unpack = MirValueId::new(format!("expr:{}", receipt.get_unpack_operation.0))
        .map_err(|error| error.to_string())?;
    let unpack_instructions = instructions
        .iter()
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack {
                result,
                pair,
                projection,
            } => Some((result, pair, projection)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let [unpack] = unpack_instructions.as_slice() else {
        return Err(format!(
            "ordinary Map GetAnyResult requires one exact pair unpack, observed {}",
            unpack_instructions.len()
        ));
    };
    if unpack.0 != &expected_unpack
        || unpack.1 != &expected_pair
        || unpack.2 != &receipt.get_pair_payload_projection
        || function.values.get(&expected_pair).is_none_or(|value| {
            value.ty != super::types::ordinary_map_owned_any_v2_get_result_type_id()
        })
        || function
            .values
            .get(&expected_unpack)
            .is_none_or(|value| value.ty != crate::core::ir::primitive_type_id(PrimitiveType::I32))
    {
        return Err(
            "ordinary Map GetAnyResult unpack does not consume only its Ok pair's exact field-one i32 conversion".into(),
        );
    }

    let pair_drop_count = instructions
        .iter()
        .filter(|instruction| {
            matches!(&instruction.kind, MirInstructionKind::Drop { value } if value == &expected_pair)
        })
        .count();
    let updated_root = local_value(&receipt.updated_root_local)?;
    let updated_root_drop_count = instructions
        .iter()
        .filter(|instruction| {
            matches!(&instruction.kind, MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { root } if root == &updated_root)
        })
        .count();
    if pair_drop_count != 1 || updated_root_drop_count != 2 {
        return Err(format!(
            "ordinary Map GetAnyResult must settle its Ok pair once and its borrowed root once on each outcome; observed pair/root drops {pair_drop_count}/{updated_root_drop_count}"
        ));
    }

    super::receipt::validate_ordinary_map_set_get_any_result_i32_result_shape(function)
}

pub(super) fn validate_managed_any_receipts(function: &MirFunction) -> Vec<MirValidationError> {
    let mut errors = Vec::new();
    let values_caller_profile_count = [
        function
            .ownership
            .checker_ordinary_map_owned_any_v2_values_caller_receipt
            .is_some(),
        function
            .ownership
            .checker_ordinary_map_owned_any_v2_values_crosscall_string_caller_receipt
            .is_some(),
        function
            .ownership
            .checker_ordinary_map_owned_any_v2_values_result_producer_caller_receipt
            .is_some(),
    ]
    .into_iter()
    .filter(|active| *active)
    .count();
    if values_caller_profile_count > 1 {
        errors.push(MirValidationError {
            subject: function.owner.0.clone(),
            message: "cannot mix multiple Values caller profiles on one MIR function".into(),
        });
    }
    if let Some(receipt) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_direct_print_result_receipt
        .as_ref()
    {
        // This closed branch-local profile has independent Checker source
        // authority and intentionally carries no generic ManagedAny body
        // seal. Do not let descriptor presence substitute for the receipt.
        let expected = super::lower::ordinary_map_direct_print_result_function(receipt);
        let valid = expected.is_ok_and(|expected| expected == *function);
        if !valid {
            errors.push(MirValidationError {
                subject: function.owner.0.clone(),
                message: "DirectPrintResult L1 owner graph differs from its exact Checker receipt"
                    .into(),
            });
        }
        return errors;
    }
    if let Some(receipt) = function
        .ownership
        .ordinary_map_direct_tuple_get_unpack_l1_receipt()
        .filter(|_| {
            function
                .ownership
                .ordinary_map_direct_tuple_get_unpack_l2_projection()
                .is_none()
        })
    {
        // This bounded profile has its own Checker receipt. Re-materialize
        // from that exact receipt here so ManagedAny TypeDesc presence alone
        // cannot authorize the owner-bearing graph.
        let expected = super::lower::ordinary_map_direct_tuple_get_unpack_l1_function(receipt);
        let valid = expected.is_ok_and(|expected| expected == *function);
        if !valid {
            errors.push(MirValidationError {
                subject: function.owner.0.clone(),
                message: "direct tuple Get L1 owner graph differs from its exact Checker receipt"
                    .into(),
            });
        }
        return errors;
    }
    if function
        .ownership
        .checker_ordinary_map_owned_any_v2_set_get_any_result_i32_lineage
        .is_some()
    {
        if let Err(message) = validate_ordinary_map_set_get_any_result_i32_owner(function) {
            errors.push(MirValidationError {
                subject: function.owner.0.clone(),
                message,
            });
        }
        return errors;
    }
    if function
        .ownership
        .checker_ordinary_map_owned_any_v2_set_get_any_i32_lineage
        .is_some()
    {
        if let Err(message) =
            super::receipt::validate_ordinary_map_set_get_any_i32_result_shape(function)
        {
            errors.push(MirValidationError {
                subject: function.owner.0.clone(),
                message,
            });
        }
        return errors;
    }
    let owner_type = managed_any_i32_string_v1_type_id();
    let owner_values = function
        .values
        .values()
        .filter(|value| {
            value.ty == owner_type
                || value.ty == ordinary_map_get_any_tuple_i32_string_v1_type_id()
                || function
                    .ownership
                    .checker_managed_any
                    .as_ref()
                    .is_some_and(|seal| seal.managed_values.contains(&value.id))
        })
        .map(|value| value.id.clone())
        .collect::<BTreeSet<_>>();
    let uses_owner = |kind: &MirInstructionKind| {
        instruction_value_ids(kind)
            .iter()
            .any(|id| owner_values.contains(id))
    };
    let observed = function
        .blocks
        .values()
        .flat_map(|block| {
            block
                .instructions
                .iter()
                .map(|instruction| (block.id.clone(), instruction.clone()))
        })
        .collect::<Vec<_>>();
    let has_managed_any_marker = !owner_values.is_empty()
        || function.ownership.checker_managed_any.is_some()
        || observed.iter().any(|(_, instruction)| {
            matches!(
                &instruction.kind,
                MirInstructionKind::ManagedAny { .. }
                    | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { .. }
                    | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { .. }
                    | MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { .. }
                    | MirInstructionKind::OrdinaryMapOwnedAnyV2GetAnyResult { .. }
                    | MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack { .. }
                    | MirInstructionKind::OrdinaryMapOwnedAnyV2Println { .. }
                    | MirInstructionKind::OrdinaryMapOwnedAnyV2Keys { .. }
                    | MirInstructionKind::OrdinaryMapOwnedAnyV2KeysUnpack { .. }
                    | MirInstructionKind::OrdinaryMapOwnedAnyV2ValuesUnpack { .. }
                    | MirInstructionKind::OrdinaryMapOwnedAnyV2Values { .. }
            )
        });
    if !has_managed_any_marker {
        return errors;
    }
    let mut error = |subject: String, message: &str| {
        errors.push(MirValidationError {
            subject,
            message: message.into(),
        })
    };
    let Some(seal) = &function.ownership.checker_managed_any else {
        error(
            function.owner.0.clone(),
            "ManagedAny TypeDesc/value presence cannot authorize an owner: private Checker seal is missing",
        );
        return errors;
    };
    if seal.owner != function.owner
        || seal.parameters != function.parameters
        || seal.block_parameters
            != function
                .blocks
                .values()
                .map(|block| (block.id.clone(), block.parameters.clone()))
                .collect::<Vec<_>>()
        || seal.parameter_permissions != function.parameter_permissions
        || seal.result != function.result
        || seal.source_cfg != function.ownership.checker_managed_any_cfg_receipt
        || seal.v2_keys_snapshot_receipts
            != function
                .ownership
                .checker_ordinary_map_owned_any_v2_cfg_receipt
                .as_ref()
                .map(|receipt| receipt.keys_snapshot_receipts.clone())
                .unwrap_or_default()
        || seal.v2_dynamic_keys_get_receipts
            != function
                .ownership
                .checker_ordinary_map_owned_any_v2_cfg_receipt
                .as_ref()
                .map(|receipt| receipt.dynamic_keys_get_receipts.clone())
                .unwrap_or_default()
        || seal.v2_values_local_index_i32_result_branch
            != function
                .ownership
                .checker_ordinary_map_owned_any_v2_cfg_receipt
                .as_ref()
                .and_then(|receipt| receipt.values_local_index_i32_result_branch_receipt.clone())
        || (function.blocks.len() > 1 && seal.source_cfg.is_none())
        || seal.value_types
            != function
                .values
                .iter()
                .map(|(id, value)| (id.clone(), value.ty.clone()))
                .collect()
    {
        error(
            function.owner.0.clone(),
            "ManagedAny value/parameter/return identities disagree with the private Checker seal",
        );
    }
    let expected_body = seal
        .instructions
        .iter()
        .map(|(block, _, instruction)| (block.clone(), instruction.clone()))
        .collect::<Vec<_>>();
    if observed != expected_body {
        error(
            function.owner.0.clone(),
            "MIR block/instruction/origin sequence disagrees with the complete private Checker body seal",
        );
    }
    let mut seen = BTreeSet::<MirInstructionId>::new();
    for (_, origin, expected) in &seal.instructions {
        if origin.0.trim().is_empty() || !seen.insert(expected.id.clone()) {
            error(
                expected.id.to_string(),
                "ManagedAny body seal has an empty origin or duplicate instruction identity",
            );
            continue;
        }
        // The full body snapshot binds all lowered instructions. This
        // profile-specific check additionally ensures owner-bearing operations
        // still fit the narrow typed ManagedAny/tuple/call vocabulary.
        if !uses_owner(&expected.kind)
            && !matches!(
                &expected.kind,
                MirInstructionKind::ManagedAny { .. }
                    | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { .. }
                    | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { .. }
            )
        {
            continue;
        }
        let permitted = match &expected.kind {
            MirInstructionKind::ManagedAny {
                result,
                source,
                operation,
                contract,
            } => contract.as_ref().is_some_and(|receipt| {
                let (source_type, target_type, conversion_valid) =
                    if *operation == MirManagedAnyOperation::PackI32 {
                        let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
                        (
                            i32_type.clone(),
                            Some(owner_type.clone()),
                            receipt
                                .checker_conversion
                                .as_ref()
                                .is_some_and(|conversion| {
                                    conversion.kind == CheckedConversionKind::DynamicAnyPack
                                        && conversion.from == i32_type
                                        && conversion.to != owner_type
                                }),
                        )
                    } else {
                        (
                            owner_type.clone(),
                            match operation {
                                MirManagedAnyOperation::Clone => Some(owner_type.clone()),
                                MirManagedAnyOperation::Drop => None,
                                MirManagedAnyOperation::Println => {
                                    Some(crate::core::ir::primitive_type_id(PrimitiveType::Unit))
                                }
                                MirManagedAnyOperation::PackI32 => unreachable!(),
                                MirManagedAnyOperation::UnpackI32 => {
                                    Some(crate::core::ir::primitive_type_id(PrimitiveType::I32))
                                }
                                MirManagedAnyOperation::UnpackString => {
                                    Some(crate::core::ir::primitive_type_id(PrimitiveType::String))
                                }
                            },
                            receipt.checker_conversion.is_none()
                                && ((*operation == MirManagedAnyOperation::Println)
                                    == receipt.checker_source_type.is_some()),
                        )
                    };
                receipt.owner == function.owner
                    && receipt.origin == *origin
                    && receipt.source == *source
                    && receipt.result == *result
                    && receipt.source_type == source_type
                    && receipt.target_type == target_type
                    && receipt.schema_version == MIR_MANAGED_ANY_SCHEMA_VERSION
                    && receipt.descriptor_version == MIR_MANAGED_ANY_DESCRIPTOR_VERSION
                    && receipt.expected_kind == operation.expected_kind()
                    && conversion_valid
            }),
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { .. } => true,
            MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { result, .. } => {
                has_checker_ordinary_map_owned_any_v2_action(function, expected)
                    && has_checker_ordinary_map_owned_any_v2_get_any_result(
                        function,
                        &expected.id,
                        result,
                    )
                    && function.values.get(result).is_some_and(|value| {
                        value.ty == ordinary_map_owned_any_v2_get_result_type_id()
                    })
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2GetOrDefaultI32 {
                result,
                root,
                key,
                default,
                callsite,
                call_target,
                type_argument,
            } => {
                has_checker_ordinary_map_owned_any_v2_get_or_default_i32_borrow(
                    function,
                    expected,
                    result,
                    root,
                    key,
                    default,
                    callsite,
                    call_target,
                    type_argument,
                ) && function.values.get(root).is_some_and(|value| {
                    value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                }) && function.values.get(key).is_some_and(|value| {
                    value.ty == crate::core::ir::primitive_type_id(PrimitiveType::String)
                }) && function.values.get(default).is_some_and(|value| {
                    value.ty == crate::core::ir::primitive_type_id(PrimitiveType::I32)
                }) && function.values.get(result).is_some_and(|value| {
                    value.ty == crate::core::ir::primitive_type_id(PrimitiveType::I32)
                })
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2GetOrDefaultString {
                result,
                root,
                key,
                default,
                callsite,
                call_target,
                type_argument,
            } => {
                has_checker_ordinary_map_owned_any_v2_get_or_default_string_borrow(
                    function,
                    expected,
                    result,
                    root,
                    key,
                    default,
                    callsite,
                    call_target,
                    type_argument,
                ) && function.values.get(root).is_some_and(|value| {
                    value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                }) && function.values.get(key).is_some_and(|value| {
                    value.ty == crate::core::ir::primitive_type_id(PrimitiveType::String)
                }) && function.values.get(default).is_some_and(|value| {
                    value.ty == crate::core::ir::primitive_type_id(PrimitiveType::String)
                }) && function.values.get(result).is_some_and(|value| {
                    value.ty == crate::core::ir::primitive_type_id(PrimitiveType::String)
                })
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Println { .. } => {
                has_checker_ordinary_map_owned_any_v2_println(function, expected)
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Values { result, .. } => {
                (has_checker_ordinary_map_owned_any_v2_values_result_producer_operation(
                    function, expected,
                ) || (has_checker_ordinary_map_owned_any_v2_action(function, expected)
                    && has_checker_ordinary_map_owned_any_v2_values_result(
                        function,
                        &expected.id,
                        result,
                    )))
                    && function
                        .values
                        .get(result)
                        .is_some_and(|value| value.ty == ordinary_map_owned_any_v2_values_type_id())
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Keys { result, root } => {
                has_checker_ordinary_map_owned_any_v2_keys_snapshot_operation(
                    function, expected, result, root, true,
                ) && function.values.get(root).is_some_and(|value| {
                    value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                })
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack {
                result,
                pair,
                projection,
            } => {
                has_checker_ordinary_map_owned_any_v2_unpack(
                    function,
                    &expected.id,
                    result,
                    pair,
                    projection,
                ) && function
                    .values
                    .get(pair)
                    .is_some_and(|value| value.ty == ordinary_map_owned_any_v2_get_result_type_id())
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2ValuesUnpack {
                result,
                values,
                index,
                projection,
            } => {
                has_checker_ordinary_map_owned_any_v2_values_unpack(
                    function,
                    &expected.id,
                    result,
                    values,
                    index,
                    projection,
                ) && function
                    .values
                    .get(values)
                    .is_some_and(|value| value.ty == ordinary_map_owned_any_v2_values_type_id())
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2SetAnyResult { .. } => {
                super::validate_ordinary_map_owned_any_v2_set_any_result_receipts(function)
                    .is_empty()
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2KeysUnpack {
                result,
                keys,
                projection,
            } => has_checker_ordinary_map_owned_any_v2_keys_unpack(
                function, expected, result, keys, projection,
            ),
            MirInstructionKind::OrdinaryMapOwnedAnyV2New { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2GetI32Result { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2SetStringResult { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2GetStringResult { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Set { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Remove { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Contains { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2IsEmpty { .. } => {
                if matches!(
                    &expected.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2New { .. }
                ) && function
                    .ownership
                    .checker_ordinary_map_owned_any_v2_cfg_receipt
                    .as_ref()
                    .is_some_and(|cfg| {
                        cfg.values_local_index_i32_result_branch_receipt.is_some()
                            || cfg.map_set_drop_control_result_branch_receipt.is_some()
                            || cfg.values_unpack_result_branch_receipt.is_some()
                            || cfg.result_receipts.iter().any(|result| {
                                result
                                    .computed_string_key_owner_chain
                                    .as_deref()
                                    .and_then(|chain| chain.result_branch.as_deref())
                                    .is_some()
                            })
                    })
                {
                    super::validate_ordinary_map_owned_any_v2_action_receipts(function).is_empty()
                } else if function
                    .ownership
                    .checker_ordinary_map_owned_any_v2_cfg_receipt
                    .as_ref()
                    .is_some_and(|cfg| cfg.values_local_index_i32_result_branch_receipt.is_some())
                {
                    super::validate_ordinary_map_owned_any_v2_action_receipts(function).is_empty()
                } else if matches!(
                    &expected.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2New { .. }
                        | MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult { .. }
                )
                    && has_checker_ordinary_map_owned_any_v2_values_result_producer_operation(
                        function, expected,
                    )
                {
                    true
                } else {
                    has_checker_ordinary_map_owned_any_v2_action(function, expected)
                }
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Size { result, root } => {
                has_checker_ordinary_map_owned_any_v2_action(function, expected)
                    || has_checker_ordinary_map_owned_any_v2_map_root_caller_size(
                        function, expected, result, root,
                    )
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { root } => {
                has_checker_ordinary_map_owned_any_v2_values_result_producer_operation(
                    function, expected,
                ) || has_checker_ordinary_map_owned_any_v2_action(function, expected)
                    || has_checker_ordinary_map_owned_any_v2_map_root_caller_drop(
                        function, expected, root,
                    )
            }
            MirInstructionKind::ListOp {
                operation: super::MirListOperation::Len,
                list,
                argument: None,
                ..
            } => seal.managed_values.contains(list),
            MirInstructionKind::Move { source, result }
            | MirInstructionKind::Clone { source, result } => {
                let moves_v2_map = [source, result].iter().any(|value| {
                    function.values.get(*value).is_some_and(|value| {
                        value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                    })
                });
                !moves_v2_map
                    || has_checker_ordinary_map_owned_any_v2_action(function, expected)
                    || matches!(&expected.kind, MirInstructionKind::Move { source, result }
                    if has_checker_ordinary_map_owned_any_v2_map_root_producer_return_move(
                        function, expected, source, result,
                    ) || has_checker_ordinary_map_owned_any_v2_map_root_caller_binding_move(
                        function, expected, source, result,
                    ) || has_checker_ordinary_map_owned_any_v2_map_root_get_any_caller_binding_move(
                        function, expected, source, result,
                    ))
            }
            MirInstructionKind::MoveProject {
                base, projection, ..
            } => {
                seal.managed_values.contains(base)
                    && matches!(projection, MirProjection::Field(_))
                    && has_checker_ordinary_map_owned_any_v2_unpack_lineage(function, base)
            }
            MirInstructionKind::Drop { value } => !function
                .values
                .get(value)
                .is_some_and(|value| value.ty == super::types::ordinary_map_owned_any_v2_type_id()),
            MirInstructionKind::Construct {
                kind: MirAggregateKind::Tuple,
                ..
            } => true,
            MirInstructionKind::ConstructList {
                result, elements, ..
            } => super::mir_function_authorizes_values_result_empty_list_construct(
                function,
                &expected.id,
                result,
                elements,
            ),
            MirInstructionKind::Project {
                base,
                result: _,
                projection: MirProjection::Tuple(_),
                ..
            } => {
                seal.managed_values.contains(base)
                    && (function.values.get(base).is_none_or(|value| {
                        value.ty != ordinary_map_owned_any_v2_get_result_type_id()
                    }) || matches!(
                        &expected.kind,
                        MirInstructionKind::Project {
                            base: project_base,
                            result,
                            projection: MirProjection::Tuple(0),
                            ..
                        } if project_base == base
                            && function.values.get(result).is_some_and(|value| {
                                value.ty == crate::core::ir::primitive_type_id(PrimitiveType::Bool)
                            })
                    ))
            }
            MirInstructionKind::Binary {
                result,
                op,
                left,
                right,
            } => managed_any_string_comparison_owner(
                function,
                result,
                *op,
                left,
                right,
                &owner_values,
            )
            .is_some(),
            MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::Function(callee),
                result,
                type_arguments,
                arguments,
                effect_receipts,
                variant_call_contract,
                ..
            } => {
                let transfers_map_root = arguments.iter().any(|argument| {
                    function.values.get(argument).is_some_and(|value| {
                        value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                    })
                }) || result.as_ref().is_some_and(|result| {
                    function.values.get(result).is_some_and(|value| {
                        value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                    })
                });
                !transfers_map_root
                    || result.as_ref().is_some_and(|result| {
                        has_checker_ordinary_map_owned_any_v2_map_root_caller_call(
                            function,
                            &expected.id,
                            callee,
                            type_arguments,
                            arguments,
                            effect_receipts,
                            variant_call_contract.as_ref(),
                            result,
                        ) || has_checker_ordinary_map_owned_any_v2_map_root_get_any_caller_call(
                            function,
                            &expected.id,
                            callee,
                            type_arguments,
                            arguments,
                            effect_receipts,
                            variant_call_contract.as_ref(),
                            result,
                        )
                    })
            }
            MirInstructionKind::ClosureInvoke {
                result, arguments, ..
            } => {
                !arguments.iter().any(|argument| {
                    function.values.get(argument).is_some_and(|value| {
                        value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                    })
                }) && !result.as_ref().is_some_and(|result| {
                    function.values.get(result).is_some_and(|value| {
                        value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                    })
                })
            }
            _ => false,
        };
        if !permitted {
            error(
                expected.id.to_string(),
                "ManagedAny operation is outside its typed owner/tuple/call contract",
            );
        }
    }
    let actual_terminators = function
        .blocks
        .values()
        .map(|block| (block.id.clone(), block.terminator.clone()))
        .collect::<Vec<_>>();
    if seal.terminators != actual_terminators {
        error(
            function.owner.0.clone(),
            "MIR terminator/block sequence disagrees with the complete private Checker body seal",
        );
    }
    errors
}

pub(super) fn instruction_value_ids(kind: &MirInstructionKind) -> Vec<MirValueId> {
    let mut ids = super::mir_instruction_value_uses(kind);
    ids.extend(instruction_result_values(kind).into_iter().cloned());
    ids
}

fn instruction_result_values(kind: &MirInstructionKind) -> Vec<&MirValueId> {
    match kind {
        MirInstructionKind::Actor { operation } => operation.result().into_iter().collect(),
        MirInstructionKind::ClosureBind { function_value, .. } => vec![function_value],
        MirInstructionKind::ClosureInvoke { result, .. }
        | MirInstructionKind::ManagedAny { result, .. }
        | MirInstructionKind::Call { result, .. } => result.iter().collect(),
        MirInstructionKind::MapRootNew { result }
        | MirInstructionKind::MapRootSet { result, .. }
        | MirInstructionKind::MapRootRemove { result, .. }
        | MirInstructionKind::MapRootSize { result, .. }
        | MirInstructionKind::MapRootContains { result, .. }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleNew { result }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleSet { result, .. }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleRemove { result, .. }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleSize { result, .. }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleContains { result, .. }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleObserverToJson { result, .. }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleObserverPrintln { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2New { result }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2SetAnyResult { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2GetI32Result { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2SetStringResult { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2GetStringResult { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2Set { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2Remove { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2Size { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2Contains { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2GetAnyResult { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2GetOrDefaultI32 { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2GetOrDefaultString { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2Println { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2ValuesUnpack { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2Values { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2Keys { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2KeysUnpack { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2DynamicArg0 { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2IsEmpty { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1New { result }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1NewResult { result }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Set { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1SetResult { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Remove { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Size { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Contains { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetI32 { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetString { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { result, .. }
        | MirInstructionKind::Const { result, .. }
        | MirInstructionKind::Load { result, .. }
        | MirInstructionKind::Copy { result, .. }
        | MirInstructionKind::Move { result, .. }
        | MirInstructionKind::Clone { result, .. }
        | MirInstructionKind::Borrow { result, .. }
        | MirInstructionKind::Project { result, .. }
        | MirInstructionKind::MoveProject { result, .. }
        | MirInstructionKind::MoveProjectDrop { result, .. }
        | MirInstructionKind::VariantProject { result, .. }
        | MirInstructionKind::VariantProjectOr { result, .. }
        | MirInstructionKind::VariantProjectMove { result, .. }
        | MirInstructionKind::Construct { result, .. }
        | MirInstructionKind::ConstructList { result, .. }
        | MirInstructionKind::ListOp { result, .. }
        | MirInstructionKind::VariantPredicate { result, .. }
        | MirInstructionKind::ConstructSet { result, .. }
        | MirInstructionKind::SetOp { result, .. }
        | MirInstructionKind::ConstructVariant { result, .. }
        | MirInstructionKind::ConstructVariantMove { result, .. }
        | MirInstructionKind::UpdateRecord { result, .. }
        | MirInstructionKind::Binary { result, .. }
        | MirInstructionKind::Unary { result, .. }
        | MirInstructionKind::FlowTransition { result, .. }
        | MirInstructionKind::BuiltinCall { result, .. }
        | MirInstructionKind::SessionCall { result, .. }
        | MirInstructionKind::Convert { result, .. } => vec![result],
        MirInstructionKind::SessionPairBind { lo, hi, .. } => vec![lo, hi],
        MirInstructionKind::MapRootDrop { .. }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleDrop { .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Drop { .. }
        | MirInstructionKind::Drop { .. }
        | MirInstructionKind::EndBorrow { .. }
        | MirInstructionKind::Nop => Vec::new(),
    }
}

/// Recognize the only ordinary Binary read admitted for a ManagedAny owner:
/// exact equality/inequality against an owned Mimi String, producing Bool.
/// The owner is borrowed and remains live; all other Binary use of an owner
/// remains outside the profile.
fn managed_any_string_comparison_owner<'a>(
    function: &'a MirFunction,
    result: &MirValueId,
    operation: ResolvedBinaryOp,
    left: &'a MirValueId,
    right: &'a MirValueId,
    owner_values: &BTreeSet<MirValueId>,
) -> Option<&'a MirValueId> {
    if !matches!(
        operation,
        ResolvedBinaryOp::Equal | ResolvedBinaryOp::NotEqual
    ) {
        return None;
    }
    let owner_type = managed_any_i32_string_v1_type_id();
    let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
    let bool_type = crate::core::ir::primitive_type_id(PrimitiveType::Bool);
    let is_owner = |value: &MirValueId| {
        owner_values.contains(value)
            && function
                .values
                .get(value)
                .is_some_and(|descriptor| descriptor.ty == owner_type)
    };
    if function
        .values
        .get(result)
        .is_none_or(|descriptor| descriptor.ty != bool_type)
    {
        return None;
    }
    match (is_owner(left), is_owner(right)) {
        (true, false)
            if function
                .values
                .get(right)
                .is_some_and(|descriptor| descriptor.ty == string_type) =>
        {
            Some(left)
        }
        (false, true)
            if function
                .values
                .get(left)
                .is_some_and(|descriptor| descriptor.ty == string_type) =>
        {
            Some(right)
        }
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManagedAnyCfgFlowState {
    owners: BTreeSet<MirValueId>,
    unpacked_strings: BTreeSet<MirValueId>,
}

#[derive(Debug, Clone)]
struct ManagedAnyCfgSuccessor {
    target: MirBlockId,
    arguments: Vec<MirValueId>,
    switch_move_payloads: BTreeSet<MirValueId>,
}

fn require_managed_any_cfg_owner(
    live: &BTreeSet<MirValueId>,
    managed_values: &BTreeSet<MirValueId>,
    id: &MirValueId,
) -> Result<(), String> {
    if managed_values.contains(id) && !live.contains(id) {
        Err(format!(
            "ManagedAny owner '{}' is used after transfer, drop, or before creation",
            id
        ))
    } else {
        Ok(())
    }
}

fn create_managed_any_cfg_owner(
    live: &mut BTreeSet<MirValueId>,
    managed_values: &BTreeSet<MirValueId>,
    id: &MirValueId,
) -> Result<(), String> {
    // MIR values are static SSA identities. A loop may execute the same
    // definition again after its previous dynamic owner was settled, so
    // creation is invalid only when that identity is still live on this path.
    if managed_values.contains(id) && !live.insert(id.clone()) {
        return Err(format!(
            "ManagedAny owner '{}' is recreated while its prior path value is still live",
            id
        ));
    }
    Ok(())
}

/// Bind the one Keys operation to its private Checker source receipt, full
/// action/attempt census, exact Map-root borrow, and owned List<String>
/// result. The optional private-seal requirement is disabled only while the
/// original lowering seal is being constructed; every validator requires
/// that captured snapshot.
fn has_checker_ordinary_map_owned_any_v2_keys_snapshot_operation(
    function: &MirFunction,
    instruction: &MirInstruction,
    result: &MirValueId,
    root: &MirValueId,
    require_private_seal: bool,
) -> bool {
    use crate::core::{
        OrdinaryMapOwnedAnyV2ActionKind as ActionKind,
        OrdinaryMapOwnedAnyV2AttemptKind as AttemptKind,
        OrdinaryMapOwnedAnyV2KeysSnapshotProfileVersion as KeysVersion,
        OrdinaryMapOwnedAnyV2ProfileVersion as ProfileVersion,
    };

    let Some(cfg) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
    else {
        return false;
    };
    if cfg.keys_snapshot_receipts.is_empty() {
        return has_checker_ordinary_map_owned_any_v2_dynamic_keys_snapshot_operation(
            function,
            instruction,
            result,
            root,
            require_private_seal,
            cfg,
        );
    }
    let [receipt] = cfg.keys_snapshot_receipts.as_slice() else {
        return false;
    };
    if !cfg.dynamic_keys_get_receipts.is_empty() {
        return false;
    }
    if require_private_seal
        && function
            .ownership
            .checker_managed_any
            .as_ref()
            .is_none_or(|seal| {
                seal.v2_keys_snapshot_receipts.as_slice() != cfg.keys_snapshot_receipts.as_slice()
                    || !seal.v2_dynamic_keys_get_receipts.is_empty()
            })
    {
        return false;
    }

    let Ok(expected_root) = MirValueId::new(format!("local:{}", receipt.source_local.0 .0)) else {
        return false;
    };
    let Ok(expected_result) = MirValueId::new(format!("expr:{}", receipt.keys_operation.0)) else {
        return false;
    };
    let Ok(expected_instruction) = MirInstructionId::new(format!(
        "inst:ordinary_map_owned_any_v2_keys:{expected_root}:{}",
        receipt.keys_operation.0
    )) else {
        return false;
    };
    let resource_identity = format!("ordinary-map-owned-any-v2:{}", receipt.source_local.0 .0);
    let matching_mir_actions = function
        .ownership
        .checker_ordinary_map_owned_any_v2
        .iter()
        .filter(|action| action.instruction == instruction.id)
        .collect::<Vec<_>>();
    let [mir_action] = matching_mir_actions.as_slice() else {
        return false;
    };
    let matching_cfg_actions = cfg
        .actions
        .iter()
        .filter(|action| action.kind == ActionKind::Keys && action.point == receipt.keys_operation)
        .collect::<Vec<_>>();
    let [cfg_action] = matching_cfg_actions.as_slice() else {
        return false;
    };
    let matching_attempts = cfg
        .attempts
        .iter()
        .filter(|attempt| {
            attempt.kind == AttemptKind::Keys && attempt.point == receipt.keys_operation
        })
        .collect::<Vec<_>>();
    let [attempt] = matching_attempts.as_slice() else {
        return false;
    };
    let expected_drop_count = match cfg.flow.as_ref().map(|flow| &flow.proof) {
        None => Some(2),
        Some(crate::core::ownership::OrdinaryMapOwnedAnyV2CfgFlowProof::ResultBranchV1(
            branches,
        )) if branches.len() == 2 => Some(3),
        Some(_) => None,
    };
    let action_kinds_are_exact = expected_drop_count.is_some_and(|drops| {
        cfg.actions.len() == 3 + drops
            && function.ownership.checker_ordinary_map_owned_any_v2.len() == 3 + drops
            && cfg.attempts.len() == 3 + drops
            && cfg
                .actions
                .iter()
                .filter(|action| action.kind == ActionKind::Drop)
                .count()
                == drops
    }) && cfg
        .actions
        .iter()
        .filter(|action| action.kind == ActionKind::New)
        .count()
        == 1
        && cfg
            .actions
            .iter()
            .filter(|action| action.kind == ActionKind::Set)
            .count()
            == 1
        && cfg
            .actions
            .iter()
            .filter(|action| action.kind == ActionKind::Keys)
            .count()
            == 1
        && cfg
            .attempts
            .iter()
            .filter(|attempt| attempt.kind == AttemptKind::Keys)
            .count()
            == 1;
    let attempt_census_is_exact = cfg.attempts.iter().all(|attempt| {
        let matching = cfg
            .actions
            .iter()
            .filter(|action| action.point == attempt.point)
            .collect::<Vec<_>>();
        let [action] = matching.as_slice() else {
            return false;
        };
        match (action.kind, attempt.kind) {
            (ActionKind::New, AttemptKind::New) => {
                attempt.arguments.is_empty()
                    && attempt.receiver_local.is_none()
                    && attempt.result_local.as_ref() == Some(&action.local)
            }
            (ActionKind::Set, AttemptKind::Set) => {
                attempt.arguments.len() == 3
                    && attempt.receiver_local == action.source_local
                    && attempt.result_local.as_ref() == Some(&action.local)
            }
            (ActionKind::Keys, AttemptKind::Keys) => {
                attempt.arguments.as_slice() == [receipt.source_node.clone()]
                    && attempt.receiver_local.as_ref() == Some(&receipt.source_local)
                    && attempt.result_local.as_ref() == Some(&receipt.snapshot_local)
            }
            (ActionKind::Drop, AttemptKind::Drop) => {
                attempt.arguments.as_slice() == [action.local.0.clone()]
                    && attempt.receiver_local.as_ref() == Some(&action.local)
                    && attempt.result_local.is_none()
            }
            _ => false,
        }
    });
    let expected_arguments = [receipt.source_node.clone()];
    let mirror_action_matches = mir_action.profile_version == ProfileVersion::V2
        && mir_action.kind == ActionKind::Keys
        && mir_action.point == receipt.keys_operation
        && mir_action.local == expected_root
        && mir_action.result.as_ref() == Some(&expected_result)
        && mir_action.root == resource_identity
        && mir_action.key_node.is_none()
        && mir_action.key.is_none()
        && mir_action.key_type.is_none()
        && mir_action.key_local.is_none()
        && mir_action.key_local_mutable.is_none()
        && mir_action.key_provenance.is_none()
        && mir_action.key_static_digest.is_none()
        && mir_action.value.is_none()
        && mir_action.value_type.is_none()
        && !mir_action.payload_clone
        && mir_action.source_local.as_ref() == Some(&expected_root)
        && mir_action.source.as_deref() == Some(resource_identity.as_str())
        && mir_action.source_node.as_ref() == Some(&receipt.source_node)
        && mir_action.source_type.as_ref() == Some(&receipt.source_type)
        && mir_action.result_source_type.as_ref() == Some(&receipt.snapshot_type)
        && mir_action.result_local.as_ref() == Some(&receipt.snapshot_local);
    let cfg_action_matches = cfg_action.profile_version == ProfileVersion::V2
        && cfg_action.kind == ActionKind::Keys
        && cfg_action.point == receipt.keys_operation
        && cfg_action.local == receipt.source_local
        && cfg_action.root.0 .0 == resource_identity
        && cfg_action.key.is_none()
        && cfg_action.key_type.is_none()
        && cfg_action.key_local.is_none()
        && cfg_action.key_local_mutable.is_none()
        && cfg_action.key_provenance.is_none()
        && cfg_action.key_static_digest.is_none()
        && cfg_action.value.is_none()
        && cfg_action.value_type.is_none()
        && !cfg_action.payload_clone
        && cfg_action.source_local.as_ref() == Some(&receipt.source_local)
        && cfg_action.source.as_ref() == Some(&receipt.source)
        && cfg_action.source_node.as_ref() == Some(&receipt.source_node)
        && cfg_action.source_type.as_ref() == Some(&receipt.source_type)
        && cfg_action.result_type.as_ref() == Some(&receipt.snapshot_type)
        && cfg_action.result_local.as_ref() == Some(&receipt.snapshot_local);
    let attempt_matches = attempt.arguments == expected_arguments
        && attempt.receiver_local.as_ref() == Some(&receipt.source_local)
        && attempt.result_local.as_ref() == Some(&receipt.snapshot_local);
    let snapshot_local = MirValueId::new(format!("local:{}", receipt.snapshot_local.0 .0));
    let root_value_type = function.values.get(root).map(|value| value.ty.clone());
    let result_value_type = function.values.get(result).map(|value| value.ty.clone());
    let snapshot_local_value_type = snapshot_local
        .clone()
        .ok()
        .and_then(|snapshot| function.values.get(&snapshot))
        .map(|value| value.ty.clone());
    let root_is_expected_type = root_value_type
        .as_ref()
        .is_some_and(|ty| ty == &super::types::ordinary_map_owned_any_v2_type_id());
    let result_matches_receipt_type = result_value_type.as_ref() == Some(&receipt.snapshot_type);
    let snapshot_matches_receipt_type =
        snapshot_local_value_type.as_ref() == Some(&receipt.snapshot_type);
    let values_match = root_is_expected_type
        && root == &expected_root
        && result_matches_receipt_type
        && result == &expected_result
        && snapshot_matches_receipt_type;

    let instruction_identity_matches = instruction.id == expected_instruction
        && matches!(
            &instruction.kind,
            MirInstructionKind::OrdinaryMapOwnedAnyV2Keys {
                result: actual_result,
                root: actual_root,
            } if actual_result == result && actual_root == root
        );
    let sealed_origin_matches = !require_private_seal
        || sealed_managed_any_instruction_origin(
            function,
            &instruction.id,
            &receipt.keys_operation,
        );

    action_kinds_are_exact
        && attempt_census_is_exact
        && cfg.owner == function.owner
        && cfg.profile_version == ProfileVersion::V2
        && receipt.owner == function.owner
        && receipt.profile_version == KeysVersion::V1
        && receipt.source.0 .0 == resource_identity
        && cfg_action_matches
        && mirror_action_matches
        && attempt_matches
        && values_match
        && instruction_identity_matches
        && sealed_origin_matches
}

fn has_checker_ordinary_map_owned_any_v2_dynamic_keys_snapshot_operation(
    function: &MirFunction,
    instruction: &MirInstruction,
    result: &MirValueId,
    root: &MirValueId,
    require_private_seal: bool,
    cfg: &crate::core::OrdinaryMapOwnedAnyV2CfgReceipt,
) -> bool {
    use crate::core::{
        OrdinaryMapOwnedAnyV2ActionKind as ActionKind,
        OrdinaryMapOwnedAnyV2AttemptKind as AttemptKind,
        OrdinaryMapOwnedAnyV2DynamicKeysGetProfileVersion as DynamicVersion,
        OrdinaryMapOwnedAnyV2ProfileVersion as ProfileVersion,
    };

    let [receipt] = cfg.dynamic_keys_get_receipts.as_slice() else {
        return false;
    };
    let private_seal_matches =
        function
            .ownership
            .checker_managed_any
            .as_ref()
            .is_some_and(|seal| {
                seal.v2_keys_snapshot_receipts.is_empty()
                    && seal.v2_dynamic_keys_get_receipts.as_slice()
                        == cfg.dynamic_keys_get_receipts.as_slice()
            });
    if require_private_seal && !private_seal_matches {
        return false;
    }

    let action = &receipt.keys_action;
    let snapshot = &receipt.keys_snapshot;
    let Ok(expected_root) = MirValueId::new(format!("local:{}", action.local.0 .0)) else {
        return false;
    };
    let Ok(expected_result) = MirValueId::new(format!("expr:{}", action.point.0)) else {
        return false;
    };
    let Ok(expected_instruction) = MirInstructionId::new(format!(
        "inst:ordinary_map_owned_any_v2_keys:{expected_root}:{}",
        action.point.0
    )) else {
        return false;
    };
    let resource_identity = format!("ordinary-map-owned-any-v2:{}", action.local.0 .0);
    let expected_actions = [
        &receipt.new_action,
        &receipt.miss_get_action,
        &receipt.set_action,
        &receipt.keys_action,
        &receipt.hit_get_action,
        &receipt.root_drops[0],
        &receipt.root_drops[1],
    ]
    .into_iter()
    .cloned()
    .chain(receipt.err_root_drops.iter().cloned())
    .collect::<BTreeSet<_>>();
    let actual_actions = cfg.actions.iter().cloned().collect::<BTreeSet<_>>();
    let matching_mir_actions = function
        .ownership
        .checker_ordinary_map_owned_any_v2
        .iter()
        .filter(|candidate| candidate.instruction == instruction.id)
        .collect::<Vec<_>>();
    let [mir_action] = matching_mir_actions.as_slice() else {
        return false;
    };
    let matching_cfg_actions = cfg
        .actions
        .iter()
        .filter(|candidate| candidate.kind == ActionKind::Keys && candidate.point == action.point)
        .collect::<Vec<_>>();
    let [cfg_action] = matching_cfg_actions.as_slice() else {
        return false;
    };
    let matching_attempts = cfg
        .attempts
        .iter()
        .filter(|candidate| candidate.kind == AttemptKind::Keys && candidate.point == action.point)
        .collect::<Vec<_>>();
    let [attempt] = matching_attempts.as_slice() else {
        return false;
    };
    let action_kinds_are_exact = cfg.actions.len() == 8
        && actual_actions.len() == 8
        && actual_actions == expected_actions
        && receipt.err_root_drops.len() == 1
        && function.ownership.checker_ordinary_map_owned_any_v2.len() == 8
        && cfg.attempts.len() == 8
        && cfg
            .flow
            .as_ref()
            .and_then(|flow| super::ordinary_map_owned_any_v2_flow_result_branches(flow))
            .is_some_and(|branches| branches.len() == 2)
        && cfg
            .actions
            .iter()
            .filter(|item| item.kind == ActionKind::New)
            .count()
            == 1
        && cfg
            .actions
            .iter()
            .filter(|item| item.kind == ActionKind::Set)
            .count()
            == 1
        && cfg
            .actions
            .iter()
            .filter(|item| item.kind == ActionKind::GetAny)
            .count()
            == 2
        && cfg
            .actions
            .iter()
            .filter(|item| item.kind == ActionKind::Keys)
            .count()
            == 1
        && cfg
            .actions
            .iter()
            .filter(|item| item.kind == ActionKind::Drop)
            .count()
            == 3
        && cfg
            .attempts
            .iter()
            .filter(|item| item.kind == AttemptKind::New)
            .count()
            == 1
        && cfg
            .attempts
            .iter()
            .filter(|item| item.kind == AttemptKind::Set)
            .count()
            == 1
        && cfg
            .attempts
            .iter()
            .filter(|item| item.kind == AttemptKind::GetAny)
            .count()
            == 2
        && cfg
            .attempts
            .iter()
            .filter(|item| item.kind == AttemptKind::Keys)
            .count()
            == 1
        && cfg
            .attempts
            .iter()
            .filter(|item| item.kind == AttemptKind::Drop)
            .count()
            == 3;
    let expected_attempt_matches =
        |expected_action: &crate::core::OrdinaryMapOwnedAnyV2Action,
         kind: AttemptKind,
         arguments: Option<Vec<crate::core::NodeId>>,
         receiver: Option<crate::core::ir::ResolvedLocalId>,
         result_local: Option<crate::core::ir::ResolvedLocalId>| {
            let Some(arguments) = arguments else {
                return false;
            };
            let matching = cfg
                .attempts
                .iter()
                .filter(|candidate| candidate.point == expected_action.point)
                .collect::<Vec<_>>();
            matches!(matching.as_slice(), [candidate]
            if candidate.kind == kind
                && candidate.arguments == arguments
                && candidate.receiver_local == receiver
                && candidate.result_local == result_local)
        };
    let arguments =
        |nodes: &[Option<crate::core::NodeId>]| nodes.iter().cloned().collect::<Option<Vec<_>>>();
    let attempts_are_exact = expected_attempt_matches(
        &receipt.new_action,
        AttemptKind::New,
        Some(Vec::new()),
        None,
        Some(receipt.new_action.local.clone()),
    ) && expected_attempt_matches(
        &receipt.miss_get_action,
        AttemptKind::GetAny,
        arguments(&[
            receipt.miss_get_action.source_node.clone(),
            receipt.miss_get_action.key.clone(),
        ]),
        receipt.miss_get_action.source_local.clone(),
        Some(receipt.miss_result.binding.clone()),
    ) && expected_attempt_matches(
        &receipt.set_action,
        AttemptKind::Set,
        arguments(&[
            receipt.set_action.source_node.clone(),
            receipt.set_action.key.clone(),
            receipt.set_action.value.clone(),
        ]),
        receipt.set_action.source_local.clone(),
        Some(receipt.set_action.local.clone()),
    ) && expected_attempt_matches(
        action,
        AttemptKind::Keys,
        arguments(std::slice::from_ref(&action.source_node)),
        action.source_local.clone(),
        Some(snapshot.snapshot_local.clone()),
    ) && expected_attempt_matches(
        &receipt.hit_get_action,
        AttemptKind::GetAny,
        arguments(&[
            receipt.hit_get_action.source_node.clone(),
            receipt.hit_get_action.key.clone(),
        ]),
        receipt.hit_get_action.source_local.clone(),
        Some(receipt.hit_result.binding.clone()),
    ) && receipt
        .root_drops
        .iter()
        .chain(receipt.err_root_drops.iter())
        .all(|drop| {
            expected_attempt_matches(
                drop,
                AttemptKind::Drop,
                Some(vec![drop.local.0.clone()]),
                Some(drop.local.clone()),
                None,
            )
        });
    let mirror_action_matches = mir_action.profile_version == ProfileVersion::V2
        && mir_action.kind == ActionKind::Keys
        && mir_action.point == action.point
        && mir_action.local == expected_root
        && mir_action.result.as_ref() == Some(&expected_result)
        && mir_action.root == resource_identity
        && mir_action.key_node.is_none()
        && mir_action.key.is_none()
        && mir_action.key_type.is_none()
        && mir_action.key_local.is_none()
        && mir_action.key_local_mutable.is_none()
        && mir_action.key_provenance.is_none()
        && mir_action.key_static_digest.is_none()
        && mir_action.value.is_none()
        && mir_action.value_type.is_none()
        && !mir_action.payload_clone
        && mir_action.source_local.as_ref() == Some(&expected_root)
        && mir_action.source.as_deref() == Some(resource_identity.as_str())
        && mir_action.source_node == action.source_node
        && mir_action.source_type == action.source_type
        && mir_action.result_source_type.as_ref() == Some(&snapshot.snapshot_type)
        && mir_action.result_local.as_ref() == Some(&snapshot.snapshot_local);
    let cfg_action_matches = cfg_action.profile_version == ProfileVersion::V2
        && cfg_action.kind == ActionKind::Keys
        && cfg_action.point == action.point
        && cfg_action.local == action.local
        && cfg_action.root.0 .0 == resource_identity
        && cfg_action.key.is_none()
        && cfg_action.key_type.is_none()
        && cfg_action.key_local.is_none()
        && cfg_action.key_local_mutable.is_none()
        && cfg_action.key_provenance.is_none()
        && cfg_action.key_static_digest.is_none()
        && cfg_action.value.is_none()
        && cfg_action.value_type.is_none()
        && !cfg_action.payload_clone
        && cfg_action.source_local == action.source_local
        && cfg_action.source == action.source
        && cfg_action.source_node == action.source_node
        && cfg_action.source_type == action.source_type
        && cfg_action.result_type.as_ref() == Some(&snapshot.snapshot_type)
        && cfg_action.result_local.as_ref() == Some(&snapshot.snapshot_local);
    let attempt_matches = arguments(std::slice::from_ref(&action.source_node))
        .is_some_and(|expected_arguments| attempt.arguments == expected_arguments)
        && attempt.receiver_local == action.source_local
        && attempt.result_local.as_ref() == Some(&snapshot.snapshot_local);
    let expected_snapshot_local =
        MirValueId::new(format!("local:{}", snapshot.snapshot_local.0 .0));
    let root_value_type = function.values.get(root).map(|value| value.ty.clone());
    let result_value_type = function.values.get(result).map(|value| value.ty.clone());
    let snapshot_local_value_type = expected_snapshot_local
        .as_ref()
        .ok()
        .and_then(|id| function.values.get(id))
        .map(|value| value.ty.clone());
    let root_matches = root == &expected_root;
    let root_type_matches =
        root_value_type.as_ref() == Some(&super::types::ordinary_map_owned_any_v2_type_id());
    let result_matches = result == &expected_result;
    let result_type_matches = result_value_type.as_ref() == Some(&snapshot.snapshot_type);
    let snapshot_local_type_matches =
        snapshot_local_value_type.as_ref() == Some(&snapshot.snapshot_type);
    let action_locals_match = action.source_local.as_ref() == Some(&action.local);
    let action_root_matches = action.root.0 .0 == resource_identity;
    let action_source_matches = action.source.as_ref() == Some(&action.root);
    // The Checker source type is the resolved stdlib Map record while MIR's
    // owner root uses a synthetic V2 TypeDesc. Bind this field across the
    // Checker action and both projected action ledgers instead of comparing
    // those deliberately distinct identities.
    let action_source_type_matches = action.source_type.is_some();
    let action_result_type_matches = action.result_type.as_ref() == Some(&snapshot.snapshot_type);
    let action_result_local_matches =
        action.result_local.as_ref() == Some(&snapshot.snapshot_local);
    let snapshot_binding_is_certified = receipt
        .source_identity_nodes
        .contains(&snapshot.snapshot_binding_point);
    let snapshot_drop_is_certified = receipt
        .source_identity_nodes
        .contains(&snapshot.snapshot_drop_point);
    let values_match = root_matches
        && root_type_matches
        && result_matches
        && result_type_matches
        && snapshot_local_type_matches
        && action_locals_match
        && action_root_matches
        && action_source_matches
        && action_source_type_matches
        && action_result_type_matches
        && action_result_local_matches
        && snapshot_binding_is_certified
        && snapshot_drop_is_certified;
    let instruction_identity_matches = instruction.id == expected_instruction
        && matches!(
            &instruction.kind,
            MirInstructionKind::OrdinaryMapOwnedAnyV2Keys {
                result: actual_result,
                root: actual_root,
            } if actual_result == result && actual_root == root
        );
    let sealed_origin_matches = !require_private_seal
        || sealed_managed_any_instruction_origin(function, &instruction.id, &action.point);

    action_kinds_are_exact
        && attempts_are_exact
        && cfg.owner == function.owner
        && cfg.profile_version == ProfileVersion::V2
        && cfg.keys_snapshot_receipts.is_empty()
        && receipt.owner == function.owner
        && receipt.profile_version == DynamicVersion::V1
        && action.point == receipt.keys_action.point
        && cfg_action_matches
        && mirror_action_matches
        && attempt_matches
        && values_match
        && instruction_identity_matches
        && sealed_origin_matches
}

fn has_checker_ordinary_map_owned_any_v2_keys_unpack(
    function: &MirFunction,
    instruction: &MirInstruction,
    result: &MirValueId,
    keys: &MirValueId,
    projection: &NodeId,
) -> bool {
    use crate::core::OrdinaryMapOwnedAnyV2KeysSnapshotProfileVersion as KeysVersion;

    let Some(cfg) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
    else {
        return false;
    };
    if cfg.keys_snapshot_receipts.is_empty() {
        let [receipt] = cfg.dynamic_keys_get_receipts.as_slice() else {
            return false;
        };
        let snapshot = &receipt.keys_snapshot;
        let action = &receipt.keys_action;
        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
        let Ok(keys_result) = MirValueId::new(format!("expr:{}", action.point.0)) else {
            return false;
        };
        let Ok(root) = MirValueId::new(format!("local:{}", action.local.0 .0)) else {
            return false;
        };
        let Ok(keys_instruction_id) = MirInstructionId::new(format!(
            "inst:ordinary_map_owned_any_v2_keys:{root}:{}",
            action.point.0
        )) else {
            return false;
        };
        let matching_keys_instructions = function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|candidate| candidate.id == keys_instruction_id)
            .collect::<Vec<_>>();
        let [keys_instruction] = matching_keys_instructions.as_slice() else {
            return false;
        };
        if cfg.owner != function.owner
            || cfg.profile_version != crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
            || receipt.owner != function.owner
            || receipt.profile_version
                != crate::core::OrdinaryMapOwnedAnyV2DynamicKeysGetProfileVersion::V1
            || !has_checker_ordinary_map_owned_any_v2_keys_snapshot_operation(
                function,
                keys_instruction,
                &keys_result,
                &root,
                true,
            )
        {
            return false;
        }
        let Ok(expected_result) =
            MirValueId::new(format!("expr:{}", snapshot.copied_key_projection.0))
        else {
            return false;
        };
        let Ok(expected_keys) = MirValueId::new(format!("local:{}", snapshot.snapshot_local.0 .0))
        else {
            return false;
        };
        let Ok(expected_instruction) = MirInstructionId::new(format!(
            "inst:ordinary_map_owned_any_v2_keys_unpack:{}",
            snapshot.copied_key_projection.0
        )) else {
            return false;
        };
        return snapshot.copied_key_type == string_type
            && instruction.id == expected_instruction
            && projection == &snapshot.copied_key_projection
            && result == &expected_result
            && keys == &expected_keys
            && function
                .values
                .get(keys)
                .is_some_and(|value| value.ty == snapshot.snapshot_type)
            && function
                .values
                .get(result)
                .is_some_and(|value| value.ty == string_type)
            && sealed_managed_any_instruction_origin(
                function,
                &instruction.id,
                &snapshot.copied_key_projection,
            )
            && matches!(
                &instruction.kind,
                MirInstructionKind::OrdinaryMapOwnedAnyV2KeysUnpack {
                    result: actual_result,
                    keys: actual_keys,
                    projection: actual_projection,
                } if actual_result == result
                    && actual_keys == keys
                    && actual_projection == projection
            );
    }
    if !cfg.dynamic_keys_get_receipts.is_empty() {
        return false;
    }
    let [receipt] = cfg.keys_snapshot_receipts.as_slice() else {
        return false;
    };
    let Ok(keys_result) = MirValueId::new(format!("expr:{}", receipt.keys_operation.0)) else {
        return false;
    };
    let Ok(root) = MirValueId::new(format!("local:{}", receipt.source_local.0 .0)) else {
        return false;
    };
    let Ok(keys_instruction_id) = MirInstructionId::new(format!(
        "inst:ordinary_map_owned_any_v2_keys:{root}:{}",
        receipt.keys_operation.0
    )) else {
        return false;
    };
    let matching_keys_instructions = function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter(|candidate| candidate.id == keys_instruction_id)
        .collect::<Vec<_>>();
    let [keys_instruction] = matching_keys_instructions.as_slice() else {
        return false;
    };
    if !has_checker_ordinary_map_owned_any_v2_keys_snapshot_operation(
        function,
        keys_instruction,
        &keys_result,
        &root,
        true,
    ) {
        return false;
    }
    let Ok(expected_result) = MirValueId::new(format!("expr:{}", receipt.index_projection.0))
    else {
        return false;
    };
    let Ok(expected_keys) = MirValueId::new(format!("local:{}", receipt.snapshot_local.0 .0))
    else {
        return false;
    };
    let Ok(expected_instruction) = MirInstructionId::new(format!(
        "inst:ordinary_map_owned_any_v2_keys_unpack:{}",
        receipt.index_projection.0
    )) else {
        return false;
    };
    let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
    receipt.profile_version == KeysVersion::V1
        && receipt.owner == function.owner
        && receipt.index_type == crate::core::ir::primitive_type_id(PrimitiveType::I32)
        && receipt.key_type == string_type
        && instruction.id == expected_instruction
        && projection == &receipt.index_projection
        && result == &expected_result
        && keys == &expected_keys
        && function
            .values
            .get(keys)
            .is_some_and(|value| value.ty == receipt.snapshot_type)
        && function
            .values
            .get(result)
            .is_some_and(|value| value.ty == string_type)
        && sealed_managed_any_instruction_origin(
            function,
            &instruction.id,
            &receipt.index_projection,
        )
        && matches!(
            &instruction.kind,
            MirInstructionKind::OrdinaryMapOwnedAnyV2KeysUnpack {
                result: actual_result,
                keys: actual_keys,
                projection: actual_projection,
            } if actual_result == result
                && actual_keys == keys
                && actual_projection == projection
        )
}

fn ordinary_map_owned_any_v2_result_branch_for_scrutinee<'a>(
    function: &'a MirFunction,
    scrutinee: &MirValueId,
) -> Option<(
    &'a crate::core::OrdinaryMapOwnedAnyV2CfgReceipt,
    &'a crate::core::ownership::OrdinaryMapOwnedAnyV2CfgFlowReceipt,
    &'a crate::core::OrdinaryMapOwnedAnyV2ResultBranchReceipt,
)> {
    let cfg = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()?;
    let flow = cfg.flow.as_ref()?;
    let branches = super::ordinary_map_owned_any_v2_flow_result_branches(flow)?;
    let branch = branches.iter().find(|branch| {
        MirValueId::new(format!("expr:{}", branch.operation_point.0))
            .is_ok_and(|expected| expected == *scrutinee)
    })?;
    Some((cfg, flow, branch))
}

fn has_ordinary_map_owned_any_v2_result_branch_v1_operation(
    function: &MirFunction,
    kind: crate::core::OrdinaryMapOwnedAnyV2ActionKind,
    result: &MirValueId,
) -> bool {
    ordinary_map_owned_any_v2_result_branch_for_scrutinee(function, result)
        .is_some_and(|(_, _, branch)| branch.kind == kind)
}

fn has_closed_values_unpack_result_branch_set(function: &MirFunction, result: &MirValueId) -> bool {
    sealed_branch_context(function).is_some_and(|(_, checker_cfg, managed_cfg)| {
        checker_cfg
            .values_unpack_result_branch_receipt
            .as_ref()
            .is_some_and(|branch| {
                branch.owner == function.owner
                    && branch.cfg == *managed_cfg
                    && branch.sets.iter().any(|set| {
                        MirValueId::new(format!("expr:{}", set.operation.0))
                            .is_ok_and(|expected| expected == *result)
                    })
            })
    })
}

fn has_checker_ordinary_map_owned_any_v2_values_result_producer_operation(
    function: &MirFunction,
    instruction: &MirInstruction,
) -> bool {
    let Some(receipt) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_values_result_producer_receipt
        .as_deref()
    else {
        return false;
    };
    if !super::validate_ordinary_map_owned_any_v2_action_receipts(function).is_empty() {
        return false;
    }
    match &instruction.kind {
        MirInstructionKind::OrdinaryMapOwnedAnyV2New { .. } => {
            instruction.id.as_str()
                == format!(
                    "inst:ordinary_map_values_result_producer:new:{}",
                    receipt.map_new_operation.0
                )
        }
        MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult { .. } => receipt
            .set_branches
            .iter()
            .enumerate()
            .any(|(index, branch)| {
                instruction.id.as_str()
                    == format!(
                        "inst:ordinary_map_values_result_producer:set_{index}:{}",
                        branch.operation.0
                    )
            }),
        MirInstructionKind::OrdinaryMapOwnedAnyV2Values { .. } => {
            instruction.id.as_str()
                == format!(
                    "inst:ordinary_map_owned_any_v2_values:{}:{}",
                    receipt.values_source_local.0 .0, receipt.values_operation.0
                )
        }
        MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { root } => {
            receipt.root_settlements.iter().any(|settlement| {
                MirValueId::new(format!("local:{}", settlement.local.0 .0))
                    .ok()
                    .as_ref()
                    == Some(root)
                    && instruction.id.as_str()
                        == format!(
                            "inst:ordinary_map_owned_any_v2_drop:{root}:{}",
                            settlement.point.0
                        )
            })
        }
        _ => false,
    }
}

/// Bind one owner-v2 MIR operation to the complete Checker action and CFG
/// census before the path ledger is allowed to create, borrow, or settle a
/// Map root. The generic ManagedAny body seal commits the MIR itself; this
/// second receipt is what grants the operation its Map-specific meaning.
fn has_checker_ordinary_map_owned_any_v2_action(
    function: &MirFunction,
    instruction: &MirInstruction,
) -> bool {
    use crate::core::{
        OrdinaryMapOwnedAnyV2ActionKind as ActionKind,
        OrdinaryMapOwnedAnyV2AttemptKind as AttemptKind,
        OrdinaryMapOwnedAnyV2ProfileVersion as ProfileVersion,
    };

    let (kind, root_operand, result_operand, key_operand, value_operand) = match &instruction.kind {
        MirInstructionKind::Clone { result, source } => {
            (ActionKind::Clone, source, Some(result), None, None)
        }
        MirInstructionKind::OrdinaryMapOwnedAnyV2New { result } => {
            (ActionKind::New, result, Some(result), None, None)
        }
        MirInstructionKind::OrdinaryMapOwnedAnyV2Set {
            result,
            source,
            key,
            value,
        } => (
            ActionKind::Set,
            source,
            Some(result),
            Some(key),
            Some(value),
        ),
        MirInstructionKind::OrdinaryMapOwnedAnyV2Remove {
            result,
            source,
            key,
        } => (ActionKind::Remove, source, Some(result), Some(key), None),
        MirInstructionKind::OrdinaryMapOwnedAnyV2Size { result, root } => {
            (ActionKind::Size, root, Some(result), None, None)
        }
        MirInstructionKind::OrdinaryMapOwnedAnyV2Contains { result, root, key } => {
            (ActionKind::HasKey, root, Some(result), Some(key), None)
        }
        MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { result, root, key } => {
            (ActionKind::GetAny, root, Some(result), Some(key), None)
        }
        MirInstructionKind::OrdinaryMapOwnedAnyV2Values { result, root } => {
            (ActionKind::Values, root, Some(result), None, None)
        }
        MirInstructionKind::OrdinaryMapOwnedAnyV2IsEmpty { result, root } => {
            (ActionKind::IsEmpty, root, Some(result), None, None)
        }
        MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { root } => {
            (ActionKind::Drop, root, None, None, None)
        }
        _ => return false,
    };

    let matching_actions = function
        .ownership
        .checker_ordinary_map_owned_any_v2
        .iter()
        .filter(|action| action.instruction == instruction.id)
        .collect::<Vec<_>>();
    let [action] = matching_actions.as_slice() else {
        return false;
    };
    if action.profile_version != ProfileVersion::V2 || action.kind != kind {
        return false;
    }

    let Some(cfg) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
    else {
        return false;
    };
    if cfg.owner != function.owner || cfg.profile_version != ProfileVersion::V2 {
        return false;
    }
    let get_or_default_i32_attempts = cfg
        .attempts
        .iter()
        .filter(|attempt| attempt.kind == AttemptKind::GetOrDefaultI32)
        .collect::<Vec<_>>();
    let get_or_default_string_attempts = cfg
        .attempts
        .iter()
        .filter(|attempt| attempt.kind == AttemptKind::GetOrDefaultString)
        .collect::<Vec<_>>();
    let get_or_default_attempt_is_receipted =
        match (
            (
                get_or_default_i32_attempts.as_slice(),
                cfg.get_or_default_i32_receipt.as_ref(),
            ),
            (
                get_or_default_string_attempts.as_slice(),
                cfg.get_or_default_string_receipt.as_ref(),
            ),
        ) {
            (([], None), ([], None)) => true,
            (([attempt], Some(receipt)), ([], None)) => receipt.profile_version
                == crate::core::ownership::OrdinaryMapOwnedAnyV2GetOrDefaultI32ProfileVersion::V1
                && receipt.caller_owner == function.owner
                && receipt.call_node == attempt.point
                && !receipt.call_target.0.trim().is_empty()
                && receipt.call_arguments == attempt.arguments
                && attempt.kind == AttemptKind::GetOrDefaultI32
                && attempt.receiver_local.as_ref() == Some(&receipt.caller_root_local)
                && attempt.result_local.as_ref() == Some(&receipt.caller_result_local),
            (([], None), ([attempt], Some(receipt))) => receipt.profile_version
                == crate::core::ownership::OrdinaryMapOwnedAnyV2GetOrDefaultStringProfileVersion::V1
                && receipt.caller_owner == function.owner
                && receipt.call_node == attempt.point
                && !receipt.call_target.0.trim().is_empty()
                && receipt.call_arguments == attempt.arguments
                && attempt.kind == AttemptKind::GetOrDefaultString
                && attempt.receiver_local.as_ref() == Some(&receipt.caller_root_local)
                && attempt.result_local.as_ref() == Some(&receipt.caller_result_local),
            _ => false,
        };
    let owner_attempt_count = cfg
        .attempts
        .iter()
        .filter(|attempt| {
            !matches!(
                attempt.kind,
                AttemptKind::GetOrDefaultI32 | AttemptKind::GetOrDefaultString
            )
        })
        .count();
    if cfg.actions.len() != function.ownership.checker_ordinary_map_owned_any_v2.len()
        || cfg.actions.len() != owner_attempt_count
        || !get_or_default_attempt_is_receipted
    {
        return false;
    }
    let matching_cfg_actions = cfg
        .actions
        .iter()
        .filter(|candidate| candidate.point == action.point && candidate.kind == kind)
        .collect::<Vec<_>>();
    let [cfg_action] = matching_cfg_actions.as_slice() else {
        return false;
    };

    let local = MirValueId::new(format!("local:{}", cfg_action.local.0 .0)).ok();
    let source_local = cfg_action
        .source_local
        .as_ref()
        .and_then(|local| MirValueId::new(format!("local:{}", local.0 .0)).ok());
    let key = cfg_action
        .key_local
        .as_ref()
        .and_then(|local| MirValueId::new(format!("local:{}", local.0 .0)).ok())
        .or_else(|| {
            cfg_action
                .key
                .as_ref()
                .and_then(|node| MirValueId::new(format!("expr:{}", node.0)).ok())
        });
    let value = cfg_action
        .value
        .as_ref()
        .and_then(|node| MirValueId::new(format!("expr:{}", node.0)).ok());
    let expected_expr_result = MirValueId::new(format!("expr:{}", action.point.0)).ok();
    let result_branch = if matches!(kind, ActionKind::New | ActionKind::Set) {
        expected_expr_result
            .as_ref()
            .and_then(|expr_id| {
                ordinary_map_owned_any_v2_result_branch_for_scrutinee(function, expr_id)
            })
            .map(|(_, _, branch)| branch)
            .filter(|branch| {
                branch.kind == kind
                    && branch.operation_point == action.point
                    && branch.output_root == cfg_action.local
            })
    } else {
        None
    };
    let values_unpack_result_branch = if matches!(kind, ActionKind::New | ActionKind::Set) {
        cfg.values_unpack_result_branch_receipt
            .as_ref()
            .is_some_and(|branch| match kind {
                ActionKind::New => {
                    branch.map_new_operation == action.point
                        && branch.initial_root == cfg_action.local
                }
                ActionKind::Set => branch.sets.iter().any(|set| {
                    set.operation == action.point
                        && set.output_root == cfg_action.local
                        && cfg_action.source_local.as_ref() == Some(&set.input_root)
                }),
                _ => false,
            })
    } else {
        false
    };
    let is_result_branch = result_branch.is_some() || values_unpack_result_branch;
    let expected_result = match kind {
        ActionKind::New | ActionKind::Clone | ActionKind::Set | ActionKind::Remove => local.clone(),
        ActionKind::Drop => None,
        ActionKind::Size
        | ActionKind::HasKey
        | ActionKind::IsEmpty
        | ActionKind::GetAny
        | ActionKind::Values => expected_expr_result.clone(),
        ActionKind::Keys => return false,
    };
    let canonical_root_identity = format!("ordinary-map-owned-any-v2:{}", cfg_action.local.0 .0);
    let expected_root_operand = if kind == ActionKind::New && is_result_branch {
        expected_expr_result.as_ref().unwrap_or(&action.local)
    } else {
        source_local.as_ref().unwrap_or(&action.local)
    };
    let expected_result_operand = if is_result_branch {
        expected_expr_result.as_ref()
    } else {
        action.result.as_ref()
    };
    let source_matches = match (&cfg_action.source_local, &cfg_action.source) {
        (Some(source_local), Some(source)) => {
            source.0 .0 == format!("ordinary-map-owned-any-v2:{}", source_local.0 .0)
        }
        (None, None) => true,
        _ => false,
    };
    if cfg_action.profile_version != ProfileVersion::V2
        || local.as_ref() != Some(&action.local)
        || action.root != canonical_root_identity
        || cfg_action.root.0 .0 != action.root
        || action.key_node != cfg_action.key
        || action.key_type != cfg_action.key_type
        || action.key_local != cfg_action.key_local
        || action.key_provenance != cfg_action.key_provenance
        || action.key != key
        || action.value != value
        || action.value_type != cfg_action.value_type
        || action.payload_clone != cfg_action.payload_clone
        || action.source_local != source_local
        || action.source != cfg_action.source.as_ref().map(|source| source.0 .0.clone())
        || !source_matches
        || action.source_node != cfg_action.source_node
        || action.source_type != cfg_action.source_type
        || action.result_source_type != cfg_action.result_type
        || action.result_local != cfg_action.result_local
        || action.result != expected_result
        || root_operand != expected_root_operand
        || result_operand != expected_result_operand
        || key_operand != action.key.as_ref()
        || value_operand != action.value.as_ref()
    {
        return false;
    }

    let attempt_kind = match kind {
        ActionKind::New => AttemptKind::New,
        ActionKind::Clone => AttemptKind::Clone,
        ActionKind::Set => AttemptKind::Set,
        ActionKind::Remove => AttemptKind::Remove,
        ActionKind::Size => AttemptKind::Size,
        ActionKind::HasKey => AttemptKind::HasKey,
        ActionKind::IsEmpty => AttemptKind::IsEmpty,
        ActionKind::GetAny => AttemptKind::GetAny,
        ActionKind::Values => AttemptKind::Values,
        ActionKind::Keys => return false,
        ActionKind::Drop => AttemptKind::Drop,
    };
    let matching_attempts = cfg
        .attempts
        .iter()
        .filter(|attempt| attempt.point == action.point && attempt.kind == attempt_kind)
        .collect::<Vec<_>>();
    let [attempt] = matching_attempts.as_slice() else {
        return false;
    };
    let expected_receiver = match kind {
        ActionKind::New => None,
        ActionKind::Clone | ActionKind::Set | ActionKind::Remove => {
            cfg_action.source_local.as_ref()
        }
        ActionKind::Size
        | ActionKind::HasKey
        | ActionKind::IsEmpty
        | ActionKind::GetAny
        | ActionKind::Values
        | ActionKind::Drop => Some(&cfg_action.local),
        ActionKind::Keys => return false,
    };
    let expected_attempt_result = match kind {
        ActionKind::New | ActionKind::Clone | ActionKind::Set | ActionKind::Remove => {
            Some(&cfg_action.local)
        }
        ActionKind::GetAny | ActionKind::Values => cfg_action.result_local.as_ref(),
        ActionKind::Size | ActionKind::HasKey | ActionKind::IsEmpty | ActionKind::Drop => None,
        ActionKind::Keys => return false,
    };
    let arguments_match = match kind {
        ActionKind::New => attempt.arguments.is_empty(),
        ActionKind::Clone => {
            attempt.arguments.len() == 1
                && attempt.arguments.first() == cfg_action.source_node.as_ref()
        }
        ActionKind::Set => {
            attempt.arguments.len() == 3
                && attempt.arguments.get(1) == cfg_action.key.as_ref()
                && attempt.arguments.get(2) == cfg_action.value.as_ref()
        }
        ActionKind::Remove | ActionKind::HasKey => {
            attempt.arguments.len() == 2 && attempt.arguments.get(1) == cfg_action.key.as_ref()
        }
        ActionKind::Size | ActionKind::IsEmpty => attempt.arguments.len() == 1,
        ActionKind::GetAny => {
            attempt.arguments.len() == 2
                && attempt.arguments.first() == cfg_action.source_node.as_ref()
                && attempt.arguments.get(1) == cfg_action.key.as_ref()
        }
        ActionKind::Values => {
            attempt.arguments.len() == 1
                && attempt.arguments.first() == cfg_action.source_node.as_ref()
        }
        ActionKind::Keys => return false,
        ActionKind::Drop => attempt.arguments.as_slice() == [cfg_action.local.0.clone()],
    };
    if attempt.receiver_local.as_ref() != expected_receiver
        || attempt.result_local.as_ref() != expected_attempt_result
        || !arguments_match
    {
        return false;
    }

    let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
    if key_operand.is_some_and(|key| {
        action.key_type.as_ref() != Some(&string_type)
            || function
                .values
                .get(key)
                .is_none_or(|value| value.ty != string_type)
    }) || value_operand.is_some_and(|payload| {
        action.value_type.as_ref().is_none_or(|expected| {
            function
                .values
                .get(payload)
                .is_none_or(|actual| &actual.ty != expected)
        })
    }) {
        return false;
    }

    let expected_root_value_type = if kind == ActionKind::New && is_result_branch {
        super::types::ordinary_map_owned_any_v2_root_result_type_id()
    } else {
        super::types::ordinary_map_owned_any_v2_type_id()
    };
    let expected_result_value_type = if is_result_branch {
        super::types::ordinary_map_owned_any_v2_root_result_type_id()
    } else {
        super::types::ordinary_map_owned_any_v2_type_id()
    };
    let Some(root_value) = function.values.get(root_operand) else {
        return false;
    };
    if root_value.ty != expected_root_value_type {
        return false;
    }
    if matches!(
        kind,
        ActionKind::New | ActionKind::Clone | ActionKind::Set | ActionKind::Remove
    ) && result_operand.is_none_or(|result| {
        function
            .values
            .get(result)
            .is_none_or(|value| value.ty != expected_result_value_type)
    }) {
        return false;
    }
    match (
        kind,
        result_operand.and_then(|result| function.values.get(result)),
    ) {
        (ActionKind::Size, Some(value)) => {
            value.ty == crate::core::ir::primitive_type_id(PrimitiveType::I32)
        }
        (ActionKind::HasKey | ActionKind::IsEmpty, Some(value)) => {
            value.ty == crate::core::ir::primitive_type_id(PrimitiveType::Bool)
        }
        (ActionKind::GetAny, Some(value)) => {
            value.ty == ordinary_map_owned_any_v2_get_result_type_id()
        }
        (ActionKind::Values, Some(value)) => value.ty == ordinary_map_owned_any_v2_values_type_id(),
        (ActionKind::Drop, None)
        | (ActionKind::New | ActionKind::Clone | ActionKind::Set | ActionKind::Remove, Some(_)) => {
            true
        }
        _ => false,
    }
}

fn has_checker_ordinary_map_owned_any_v2_get_any_result(
    function: &MirFunction,
    instruction: &MirInstructionId,
    result: &MirValueId,
) -> bool {
    use crate::core::OrdinaryMapOwnedAnyV2ActionKind as ActionKind;
    use crate::core::OrdinaryMapOwnedAnyV2ProfileVersion as ProfileVersion;
    use crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind as Settlement;

    let matching_actions = function
        .ownership
        .checker_ordinary_map_owned_any_v2
        .iter()
        .filter(|action| action.instruction == *instruction)
        .collect::<Vec<_>>();
    let [action] = matching_actions.as_slice() else {
        return false;
    };
    if action.profile_version != ProfileVersion::V2
        || action.kind != ActionKind::GetAny
        || action.result.as_ref() != Some(result)
        || action.result_source_type.is_none()
    {
        return false;
    }
    let Some(cfg) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
    else {
        return false;
    };
    if cfg.owner != function.owner || cfg.profile_version != ProfileVersion::V2 {
        return false;
    }
    let matching_cfg_actions = cfg
        .actions
        .iter()
        .filter(|candidate| candidate.point == action.point && candidate.kind == ActionKind::GetAny)
        .collect::<Vec<_>>();
    let [cfg_action] = matching_cfg_actions.as_slice() else {
        return false;
    };
    let cfg_local_value = MirValueId::new(format!("local:{}", cfg_action.local.0 .0)).ok();
    let cfg_key_value = cfg_action
        .key_local
        .as_ref()
        .and_then(|local| MirValueId::new(format!("local:{}", local.0 .0)).ok())
        .or_else(|| {
            cfg_action
                .key
                .as_ref()
                .and_then(|key| MirValueId::new(format!("expr:{}", key.0)).ok())
        });
    let action_matches = cfg_action.profile_version == ProfileVersion::V2
        && cfg_local_value.as_ref() == Some(&action.local)
        && cfg_action.root.0 .0 == action.root
        && cfg_action.source_local.as_ref() == Some(&cfg_action.local)
        && cfg_action.source.as_ref().map(|source| &source.0 .0) == Some(&action.root)
        && cfg_action.source_node == action.source_node
        && cfg_action.source_type == action.source_type
        && cfg_action.key.as_ref() == action.key_node.as_ref()
        && cfg_action.key_local == action.key_local
        && cfg_key_value.as_ref() == action.key.as_ref()
        && cfg_action.key_type == action.key_type
        && cfg_action.result_type == action.result_source_type
        && cfg_action.result_local == action.result_local;
    if !action_matches {
        return false;
    }
    if let Some(branch) = cfg.values_local_index_i32_result_branch_receipt.as_ref() {
        let expected_result = MirValueId::new(format!("expr:{}", branch.values_operation.0)).ok();
        let expected_root = MirValueId::new(format!("local:{}", branch.populated_root.0 .0)).ok();
        let private_seal_matches =
            function
                .ownership
                .checker_managed_any
                .as_ref()
                .is_some_and(|seal| {
                    seal.v2_values_local_index_i32_result_branch.as_ref() == Some(branch)
                });
        return branch.owner == function.owner
            && branch.profile_version
                == crate::core::ownership::OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultBranchProfileVersion::V2
            && branch.values_operation == action.point
            && expected_result.as_ref() == Some(result)
            && action.result.as_ref() == Some(result)
            && expected_root.as_ref() == Some(&action.local)
            && action.root == branch.populated_root.0 .0
            && action.result_local.as_ref() == Some(&branch.snapshot_local)
            && action.result_source_type.as_ref() == Some(&branch.snapshot_type)
            && branch.snapshot_binding_point.0.trim() != ""
            && cfg
                .result_receipts
                .iter()
                .all(|receipt| receipt.operation != branch.values_operation)
            && private_seal_matches;
    }
    let matching_results = cfg
        .result_receipts
        .iter()
        .filter(|receipt| receipt.operation == action.point && receipt.kind == ActionKind::GetAny)
        .collect::<Vec<_>>();
    let [receipt] = matching_results.as_slice() else {
        return false;
    };
    receipt.profile_version == ProfileVersion::V2
        && receipt.result == action.point
        && action
            .result_source_type
            .as_ref()
            .is_some_and(|ty| receipt.result_type == *ty)
        && Some(&receipt.binding) == action.result_local.as_ref()
        && receipt.source_local == cfg_action.local
        && receipt.source == cfg_action.root
        && cfg_action.source_node.as_ref() == Some(&receipt.source_node)
        && cfg_action.source_type.as_ref() == Some(&receipt.source_type)
        && receipt.key == cfg_action.key
        && receipt.key_type == cfg_action.key_type
        && receipt.key_local == cfg_action.key_local
        && receipt.settlement == Settlement::ExplicitDrop
        && receipt.settlement_point.is_some()
}

/// Validate the one checker-sealed borrowed Map edge fused from
/// `std::maps.get_or_default<i32>`. This is not a Map owner action: the
/// caller root remains live and its ordinary New/Set/Drop actions retain
/// their separate exact receipts.
fn has_checker_ordinary_map_owned_any_v2_get_or_default_i32_borrow(
    function: &MirFunction,
    instruction: &MirInstruction,
    result: &MirValueId,
    root: &MirValueId,
    key: &MirValueId,
    default: &MirValueId,
    callsite: &NodeId,
    call_target: &NodeId,
    type_argument: &ResolvedTypeId,
) -> bool {
    use crate::core::ownership::{
        OrdinaryMapOwnedAnyV2AttemptKind as AttemptKind,
        OrdinaryMapOwnedAnyV2GetOrDefaultI32ProfileVersion as GetOrDefaultProfile,
        OrdinaryMapOwnedAnyV2ProfileVersion,
    };

    let Some(cfg) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
    else {
        return false;
    };
    let Some(receipt) = cfg.get_or_default_i32_receipt.as_ref() else {
        return false;
    };
    let matching_attempts = cfg
        .attempts
        .iter()
        .filter(|attempt| attempt.kind == AttemptKind::GetOrDefaultI32)
        .collect::<Vec<_>>();
    let [attempt] = matching_attempts.as_slice() else {
        return false;
    };

    let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
    let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
    let expected_instruction = MirInstructionId::new(format!(
        "inst:ordinary_map_owned_any_v2_get_or_default_i32:{}",
        callsite.0
    ))
    .ok();
    let expected_result = MirValueId::new(format!("expr:{}", callsite.0)).ok();
    let expected_root = MirValueId::new(format!("local:{}", receipt.caller_root_local.0 .0)).ok();
    let expected_key = MirValueId::new(format!("expr:{}", receipt.caller_query_key.0)).ok();
    let expected_default = MirValueId::new(format!("expr:{}", receipt.caller_default.0)).ok();

    cfg.owner == function.owner
        && cfg.profile_version == OrdinaryMapOwnedAnyV2ProfileVersion::V2
        && receipt.profile_version == GetOrDefaultProfile::V1
        && receipt.caller_owner == function.owner
        && receipt.call_node == *callsite
        && receipt.call_target == *call_target
        && receipt.call_type_argument == *type_argument
        && receipt.call_type_argument == i32_type
        && receipt.call_result_type == i32_type
        && receipt.helper_map_permission == crate::core::Permission::View
        && receipt.call_argument_types.len() == 3
        && receipt.call_argument_types[0] == receipt.caller_root_type
        && receipt.call_argument_types[1] == string_type
        && receipt.call_argument_types[2] == i32_type
        && receipt.call_arguments.len() == 3
        && attempt.point == *callsite
        && attempt.receiver_local.as_ref() == Some(&receipt.caller_root_local)
        && attempt.result_local.as_ref() == Some(&receipt.caller_result_local)
        && attempt.arguments == receipt.call_arguments
        && expected_instruction.as_ref() == Some(&instruction.id)
        && expected_result.as_ref() == Some(result)
        && expected_root.as_ref() == Some(root)
        && expected_key.as_ref() == Some(key)
        && expected_default.as_ref() == Some(default)
}

/// Validate the checker-sealed borrowed Map edge fused from `std::maps.get_or_default<String>`.
fn has_checker_ordinary_map_owned_any_v2_get_or_default_string_borrow(
    function: &MirFunction,
    instruction: &MirInstruction,
    result: &MirValueId,
    root: &MirValueId,
    key: &MirValueId,
    default: &MirValueId,
    callsite: &NodeId,
    call_target: &NodeId,
    type_argument: &ResolvedTypeId,
) -> bool {
    use crate::core::ownership::{
        OrdinaryMapOwnedAnyV2AttemptKind as AttemptKind,
        OrdinaryMapOwnedAnyV2GetOrDefaultStringProfileVersion as GetOrDefaultProfile,
        OrdinaryMapOwnedAnyV2ProfileVersion,
    };

    let Some(cfg) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
    else {
        return false;
    };
    let Some(receipt) = cfg.get_or_default_string_receipt.as_ref() else {
        return false;
    };
    let matching_attempts = cfg
        .attempts
        .iter()
        .filter(|attempt| attempt.kind == AttemptKind::GetOrDefaultString)
        .collect::<Vec<_>>();
    let [attempt] = matching_attempts.as_slice() else {
        return false;
    };

    let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
    let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
    let expected_instruction = MirInstructionId::new(format!(
        "inst:ordinary_map_owned_any_v2_get_or_default_string:{}",
        callsite.0
    ))
    .ok();
    let expected_result = MirValueId::new(format!("expr:{}", callsite.0)).ok();
    let expected_root = MirValueId::new(format!("local:{}", receipt.caller_root_local.0 .0)).ok();
    let expected_key = MirValueId::new(format!("expr:{}", receipt.caller_query_key.0)).ok();
    let expected_default = MirValueId::new(format!("expr:{}", receipt.caller_default.0)).ok();

    cfg.owner == function.owner
        && cfg.profile_version == OrdinaryMapOwnedAnyV2ProfileVersion::V2
        && receipt.profile_version == GetOrDefaultProfile::V1
        && receipt.caller_owner == function.owner
        && receipt.call_node == *callsite
        && receipt.call_target == *call_target
        && receipt.call_type_argument == *type_argument
        && receipt.call_type_argument == string_type
        && receipt.call_result_type == string_type
        && receipt.miss_payload_type == i32_type
        && receipt.helper_map_permission == crate::core::Permission::View
        && receipt.call_argument_types.len() == 3
        && receipt.call_argument_types[0] == receipt.caller_root_type
        && receipt.call_argument_types[1] == string_type
        && receipt.call_argument_types[2] == string_type
        && receipt.call_arguments.len() == 3
        && attempt.point == *callsite
        && attempt.receiver_local.as_ref() == Some(&receipt.caller_root_local)
        && attempt.result_local.as_ref() == Some(&receipt.caller_result_local)
        && attempt.arguments == receipt.call_arguments
        && expected_instruction.as_ref() == Some(&instruction.id)
        && expected_result.as_ref() == Some(result)
        && expected_root.as_ref() == Some(root)
        && expected_key.as_ref() == Some(key)
        && expected_default.as_ref() == Some(default)
}

/// Exact V2 borrowed-observation authority. No raw Project, generic builtin,
/// or V1 Any identity can satisfy this independent source/MIR join.
pub(super) fn has_checker_ordinary_map_owned_any_v2_println(
    function: &MirFunction,
    instruction: &MirInstruction,
) -> bool {
    use crate::core::OrdinaryMapOwnedAnyV2ActionKind as Action;
    use crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind as Settlement;
    let MirInstructionKind::OrdinaryMapOwnedAnyV2Println {
        result,
        pair,
        projection,
    } = &instruction.kind
    else {
        return false;
    };
    let Some(cfg) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
    else {
        return false;
    };
    let Some(seal) = function.ownership.checker_managed_any.as_ref() else {
        return false;
    };
    let receipts = cfg
        .result_receipts
        .iter()
        .filter(|receipt| {
            receipt.payload_println.as_ref().is_some_and(|observation| {
                observation.projection == *projection
                    && instruction.id.as_str()
                        == format!(
                            "inst:ordinary_map_owned_any_v2_println:{}",
                            observation.call.0
                        )
            })
        })
        .collect::<Vec<_>>();
    let [receipt] = receipts.as_slice() else {
        return false;
    };
    let Some(observation) = receipt.payload_println.as_ref() else {
        return false;
    };
    if seal
        .v2_println
        .iter()
        .filter(|candidate| *candidate == *receipt)
        .count()
        != 1
        || cfg.owner != function.owner
        || receipt.kind != Action::GetAny
        || receipt.settlement != Settlement::ExplicitDrop
        || pair.as_str() != format!("local:{}", receipt.binding.0 .0)
        || result.as_str() != format!("expr:{}", observation.call.0)
        || function.values.get(result).map(|value| &value.ty) != Some(&observation.result_type)
        || observation.result_type != crate::core::ir::primitive_type_id(PrimitiveType::Unit)
        || function.values.get(pair).map(|value| &value.ty)
            != Some(&ordinary_map_owned_any_v2_get_result_type_id())
    {
        return false;
    }
    let Some(settlement) = receipt.settlement_point.as_ref() else {
        return false;
    };
    let Some(source_cfg) = seal.source_cfg.as_ref() else {
        return false;
    };
    let source_points = [
        &receipt.operation,
        &receipt.binding_point,
        projection,
        &observation.call,
        settlement,
    ];
    let mut positions = Vec::new();
    for point in source_points {
        let locations = source_cfg
            .reachable_blocks
            .iter()
            .flat_map(|(block, body)| {
                body.points
                    .iter()
                    .enumerate()
                    .filter_map(move |(index, candidate)| {
                        (&candidate.source.node == point).then_some((block, index))
                    })
            })
            .collect::<Vec<_>>();
        let [position] = locations.as_slice() else {
            return false;
        };
        positions.push(*position);
    }
    if !positions
        .windows(2)
        .all(|positions| positions[0].0 == positions[1].0 && positions[0].1 < positions[1].1)
    {
        return false;
    }
    let sealed = seal
        .instructions
        .iter()
        .filter(|(_, origin, candidate)| {
            candidate.id == instruction.id
                && *origin == observation.call
                && candidate == instruction
        })
        .collect::<Vec<_>>();
    let [(block, _, _)] = sealed.as_slice() else {
        return false;
    };
    let Some(mir_block) = function.blocks.get(block) else {
        return false;
    };
    let Some(print_index) = mir_block
        .instructions
        .iter()
        .position(|candidate| candidate == instruction)
    else {
        return false;
    };
    let get_any = mir_block.instructions.iter().enumerate().filter_map(|(index, candidate)| {
        matches!(&candidate.kind, MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { result, .. }
            if result.as_str() == format!("expr:{}", receipt.operation.0)
                && has_checker_ordinary_map_owned_any_v2_get_any_result(function, &candidate.id, result)).then_some(index)
    }).collect::<Vec<_>>();
    let [get_index] = get_any.as_slice() else {
        return false;
    };
    let binding = mir_block.instructions.iter().enumerate().filter_map(|(index, candidate)| {
        matches!(&candidate.kind, MirInstructionKind::Move { result, source } if result == pair && source.as_str() == format!("expr:{}", receipt.operation.0))
            .then_some(index)
    }).collect::<Vec<_>>();
    let [binding_index] = binding.as_slice() else {
        return false;
    };
    let uses = function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter(|candidate| super::mir_instruction_value_uses(&candidate.kind).contains(pair))
        .collect::<Vec<_>>();
    let drops = mir_block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            (candidate.id.as_str() == format!("inst:drop.0:{}", settlement.0)
                && matches!(&candidate.kind, MirInstructionKind::Drop { value } if value == pair))
            .then_some(index)
        })
        .collect::<Vec<_>>();
    let [drop_index] = drops.as_slice() else {
        return false;
    };
    uses.len() == 2
        && uses.contains(&instruction)
        && get_index < binding_index
        && *binding_index < print_index
        && print_index < *drop_index
        && !function
            .blocks
            .values()
            .any(|block| super::mir_terminator_value_uses(&block.terminator).contains(pair))
}

fn has_checker_ordinary_map_owned_any_v2_unpack(
    function: &MirFunction,
    instruction: &MirInstructionId,
    result: &MirValueId,
    pair: &MirValueId,
    projection: &crate::core::NodeId,
) -> bool {
    use crate::core::OrdinaryMapOwnedAnyV2ProfileVersion as ProfileVersion;
    use crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind as Settlement;

    let Some(cfg) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
    else {
        return false;
    };
    if cfg.owner != function.owner || cfg.profile_version != ProfileVersion::V2 {
        return false;
    }
    let matches = cfg
        .unpack_receipts
        .iter()
        .filter(|receipt| {
            receipt.profile_version == ProfileVersion::V2
                && receipt.conversion_kind
                    == crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
                && (receipt.conversion_to == crate::core::ir::primitive_type_id(PrimitiveType::I32)
                    || receipt.conversion_to
                        == crate::core::ir::primitive_type_id(PrimitiveType::String)
                    || function
                        .ownership
                        .checker_ordinary_map_owned_any_v2
                        .iter()
                        .any(|action| {
                            action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::Set
                                && action.value_type.as_ref() == Some(&receipt.conversion_to)
                        }))
                && receipt.settlement == Settlement::ExplicitDrop
                && receipt.projection == *projection
                && receipt.pair_binding.0 .0
                    == pair.as_str().strip_prefix("local:").unwrap_or_default()
                && receipt.cast.0 == result.as_str().strip_prefix("expr:").unwrap_or_default()
                && instruction.as_str()
                    == format!("inst:ordinary_map_owned_any_v2_unpack:{}", receipt.cast.0)
                && receipt.operation.0.trim() != ""
                && receipt.pair_binding_point.0.trim() != ""
                && receipt.unpack_binding_point.0.trim() != ""
                && receipt.settlement_point.0.trim() != ""
        })
        .collect::<Vec<_>>();
    let [receipt] = matches.as_slice() else {
        return false;
    };

    let action_matches = function
        .ownership
        .checker_ordinary_map_owned_any_v2
        .iter()
        .filter(|action| {
            action.point == receipt.operation
                && action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::GetAny
                && action.result_local.as_ref() == Some(&receipt.pair_binding)
                && action.result.is_some()
        })
        .collect::<Vec<_>>();
    let [action] = action_matches.as_slice() else {
        return false;
    };
    let get_any_instruction = action.instruction.clone();
    let get_any_result = action.result.as_ref().expect("checked GetAny result");
    has_checker_ordinary_map_owned_any_v2_get_any_result(
        function,
        &get_any_instruction,
        get_any_result,
    ) && function
        .values
        .get(pair)
        .is_some_and(|value| value.ty == ordinary_map_owned_any_v2_get_result_type_id())
        && function
            .values
            .get(result)
            .is_some_and(|value| value.ty == receipt.conversion_to)
}

fn has_checker_ordinary_map_owned_any_v2_values_unpack(
    function: &MirFunction,
    instruction: &MirInstructionId,
    result: &MirValueId,
    values: &MirValueId,
    index: &MirValueId,
    projection: &crate::core::NodeId,
) -> bool {
    if has_checker_ordinary_map_owned_any_v2_values_crosscall_string_caller_unpack(
        function,
        instruction,
        result,
        values,
        index,
        projection,
    ) {
        return true;
    }

    use crate::core::OrdinaryMapOwnedAnyV2ActionKind as ActionKind;
    use crate::core::OrdinaryMapOwnedAnyV2ProfileVersion as ProfileVersion;
    use crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind as Settlement;

    let Some(cfg) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
    else {
        return false;
    };
    if cfg.owner != function.owner || cfg.profile_version != ProfileVersion::V2 {
        return false;
    }
    let candidates = cfg
        .values_unpack_receipts
        .iter()
        .filter(|receipt| {
            let expected_index = if let Some(local) = &receipt.index_local {
                MirValueId::new(format!("local:{}", local.0 .0)).ok()
            } else {
                MirValueId::new(format!("expr:{}", receipt.index_expression.0)).ok()
            };
            receipt.profile_version == ProfileVersion::V2
                && receipt.conversion_kind
                    == crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
                && receipt.settlement == Settlement::ExplicitDrop
                && receipt.cast.0 == result.as_str().strip_prefix("expr:").unwrap_or_default()
                && receipt.snapshot_binding.0 .0
                    == values.as_str().strip_prefix("local:").unwrap_or_default()
                && expected_index.as_ref() == Some(index)
                && receipt.index_projection == *projection
                && instruction.as_str()
                    == format!(
                        "inst:ordinary_map_owned_any_v2_values_unpack:{}",
                        receipt.cast.0
                    )
                && receipt.index_disposition.is_canonical()
                && match receipt.index_disposition {
                    crate::core::OrdinaryMapOwnedAnyV2ValuesIndexDisposition::SelectedPayload => {
                        !receipt.payload_selection.is_empty()
                    }
                    crate::core::OrdinaryMapOwnedAnyV2ValuesIndexDisposition::AlwaysOutOfBounds {
                        ..
                    } => receipt.payload_selection.is_empty(),
                }
                && function
                    .values
                    .get(values)
                    .is_some_and(|value| value.ty == ordinary_map_owned_any_v2_values_type_id())
                && function
                    .values
                    .get(index)
                    .is_some_and(|value| value.ty == receipt.index_type)
                && function
                    .values
                    .get(result)
                    .is_some_and(|value| value.ty == receipt.conversion_to)
        })
        .collect::<Vec<_>>();
    let [receipt] = candidates.as_slice() else {
        return false;
    };
    let values_results = cfg
        .result_receipts
        .iter()
        .filter(|candidate| {
            candidate.operation == receipt.operation
                && candidate.kind == ActionKind::Values
                && candidate.binding == receipt.snapshot_binding
                && candidate.binding_point == receipt.snapshot_binding_point
                && candidate.result_type == receipt.snapshot_type
                && candidate.source_local == receipt.source_local
                && candidate.source == receipt.source
                && candidate.source_node == receipt.source_node
                && candidate.source_type == receipt.source_type
                && candidate.settlement == Settlement::ExplicitDrop
                && candidate.settlement_point.as_ref() == Some(&receipt.settlement_point)
        })
        .count();
    if values_results != 1 {
        return false;
    }

    let mut current = receipt.source_local.clone();
    let mut seen = BTreeSet::new();
    let mut lineage = Vec::new();
    loop {
        if !seen.insert(current.clone()) {
            return false;
        }
        let actions = cfg
            .actions
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
            return false;
        };
        match action.kind {
            ActionKind::New => {
                return matches!(
                    receipt.index_disposition,
                    crate::core::OrdinaryMapOwnedAnyV2ValuesIndexDisposition::AlwaysOutOfBounds { .. }
                ) && receipt.payload_selection.is_empty();
            }
            ActionKind::Set => {
                lineage.push(action.point.clone());
                let is_receipted_winning_set = matches!(
                    receipt.index_disposition,
                    crate::core::OrdinaryMapOwnedAnyV2ValuesIndexDisposition::SelectedPayload
                ) && receipt.payload_selection.last()
                    == Some(&action.point)
                    && action.payload_clone
                    && action.value_type.as_ref() == Some(&receipt.conversion_to);
                if is_receipted_winning_set {
                    return lineage == receipt.payload_selection;
                }
                let Some(source) = action.source_local.as_ref() else {
                    return false;
                };
                current = source.clone();
            }
            ActionKind::Remove => {
                lineage.push(action.point.clone());
                let Some(source) = action.source_local.as_ref() else {
                    return false;
                };
                current = source.clone();
            }
            ActionKind::Clone => {
                lineage.push(action.point.clone());
                let Some(source) = action.source_local.as_ref() else {
                    return false;
                };
                current = source.clone();
            }
            _ => unreachable!("root action filter is exhaustive"),
        }
    }
}

/// Validate the caller-side String extraction against the independent
/// cross-call receipt. This path intentionally does not synthesize a local
/// Values-unpack CFG receipt: the Checker receipt is a whole-program join
/// between a producer's returned List<DynamicAny> snapshot and this caller's
/// opaque List result.
fn has_checker_ordinary_map_owned_any_v2_values_crosscall_string_caller_unpack(
    function: &MirFunction,
    instruction: &MirInstructionId,
    result: &MirValueId,
    values: &MirValueId,
    index: &MirValueId,
    projection: &crate::core::NodeId,
) -> bool {
    use crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion as Profile;

    let Some(receipt) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_values_crosscall_string_caller_receipt
        .as_ref()
    else {
        return false;
    };
    let Some(seal) = function.ownership.checker_managed_any.as_ref() else {
        return false;
    };
    let expected_instruction = MirInstructionId::new(format!(
        "inst:ordinary_map_owned_any_v2_values_unpack:{}",
        receipt.unpack_cast.0
    ))
    .ok();
    let expected_result = MirValueId::new(format!("expr:{}", receipt.unpack_cast.0)).ok();
    let expected_values = MirValueId::new(format!("local:{}", receipt.snapshot_local.0 .0)).ok();
    let expected_index = MirValueId::new(format!("expr:{}", receipt.index_expression.0)).ok();
    let expected_string = MirValueId::new(format!("local:{}", receipt.unpack_binding.0 .0)).ok();
    let Some(index_value) = function.values.get(index) else {
        return false;
    };
    let Some(result_value) = function.values.get(result) else {
        return false;
    };
    let Some(values_value) = function.values.get(values) else {
        return false;
    };
    let bool_type = crate::core::ir::primitive_type_id(PrimitiveType::Bool);
    let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
    let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
    let (expected_count, expected_literal_index) = match receipt.profile_version {
        Profile::V1 | Profile::V3 => (1, 0),
        Profile::V2 | Profile::V4 => (2, 1),
    };
    let matching_index_constants = function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::Const {
                    result: constant,
                    literal: crate::core::ir::ResolvedLiteral::Int(value),
                } if constant == index && i32::try_from(*value).ok() == Some(expected_literal_index)
            )
        })
        .count();

    values_crosscall_string_caller_profile_matches(receipt)
        && receipt.values_element_count == expected_count
        && receipt.index_value == expected_literal_index
        && receipt.caller_owner == function.owner
        && receipt.producer_owner != function.owner
        && receipt.call_target == receipt.producer_owner
        && receipt.snapshot_type != receipt.call_result_type
        && receipt.index_type == i32_type
        && receipt.string_type == string_type
        && receipt.boolean_type == bool_type
        && function.result == bool_type
        && seal.v2_values_crosscall_string_caller.as_ref() == Some(receipt)
        && function
            .ownership
            .checker_ordinary_map_owned_any_v2_values_caller_receipt
            .is_none()
        && function
            .ownership
            .checker_ordinary_map_owned_any_v2
            .is_empty()
        && function
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .is_none_or(|cfg| {
                cfg.owner == function.owner
                    && cfg.actions.is_empty()
                    && cfg.attempts.is_empty()
                    && cfg.result_receipts.is_empty()
                    && cfg.values_producer_return_receipt.is_none()
                    && cfg.values_producer_caller_receipt.is_none()
                    && cfg.values_crosscall_string_caller_receipt.is_none()
                    && cfg.values_unpack_receipts.is_empty()
            })
        && expected_instruction.as_ref() == Some(instruction)
        && expected_result.as_ref() == Some(result)
        && expected_values.as_ref() == Some(values)
        && expected_index.as_ref() == Some(index)
        && matching_index_constants == 1
        && expected_string.as_ref().is_some_and(|string| {
            function
                .values
                .get(string)
                .is_some_and(|value| value.ty == receipt.string_type)
        })
        && receipt.index_projection == *projection
        && values_value.ty == ordinary_map_owned_any_v2_values_type_id()
        && index_value.ty == receipt.index_type
        && result_value.ty == receipt.string_type
}

fn values_crosscall_string_caller_profile_matches(
    receipt: &crate::core::ownership::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerReceipt,
) -> bool {
    use crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion as Profile;
    use crate::core::OrdinaryMapOwnedAnyV2ValuesResultProducerProfileVersion as ResultProducerProfile;

    let (expected_count, expected_index) = match receipt.profile_version {
        Profile::V1 | Profile::V3 => (1, 0),
        Profile::V2 | Profile::V4 => (2, 1),
    };
    let result_producer_matches = match receipt.profile_version {
        Profile::V1 | Profile::V2 => receipt.producer_result_receipt.is_none(),
        Profile::V3 => receipt
            .producer_result_receipt
            .as_ref()
            .is_some_and(|producer| {
                producer.profile_version == ResultProducerProfile::V1
                    && producer.owner == receipt.producer_owner
                    && producer.values_operation == receipt.producer_values_operation
                    && producer.success_result == receipt.producer_return
                    && producer.surface_return_type == receipt.call_result_type
                    && producer.snapshot_type == receipt.snapshot_type
                    && producer.set_branches.len() == 1
                    && receipt.producer_sorted_keys.len() == 1
                    && producer.set_branches[0].key_literal == receipt.producer_sorted_keys[0]
            }),
        Profile::V4 => receipt
            .producer_result_receipt
            .as_ref()
            .is_some_and(|producer| {
                producer.profile_version == ResultProducerProfile::V2
                    && producer.owner == receipt.producer_owner
                    && producer.values_operation == receipt.producer_values_operation
                    && producer.success_result == receipt.producer_return
                    && producer.surface_return_type == receipt.call_result_type
                    && producer.snapshot_type == receipt.snapshot_type
                    && producer.set_branches.len() == 2
                    && producer
                        .set_branches
                        .iter()
                        .map(|branch| branch.key_literal.as_str())
                        .collect::<std::collections::BTreeSet<_>>()
                        == std::collections::BTreeSet::from(["a", "b"])
            }),
    };
    receipt.values_element_count == expected_count
        && receipt.index_value == expected_index
        && receipt.producer_sorted_keys.len() == expected_count
        && receipt
            .producer_sorted_keys
            .windows(2)
            .all(|pair| pair[0] < pair[1])
        && match receipt.profile_version {
            Profile::V1 | Profile::V3 => receipt.producer_sorted_keys[0].trim() != "",
            Profile::V2 | Profile::V4 => receipt.producer_sorted_keys.as_slice() == ["a", "b"],
        }
        && result_producer_matches
        && receipt.producer_set_temporary_settlements.len()
            == if matches!(receipt.profile_version, Profile::V2 | Profile::V4) {
                expected_count * 2
            } else {
                0
            }
        && receipt.string_drop_place_index == 0
        && receipt.snapshot_drop_place_index == 0
        && receipt.string_source_uses
            == BTreeSet::from([
                receipt.comparison_load.clone(),
                receipt.string_drop_point.clone(),
            ])
        && receipt.snapshot_source_uses
            == BTreeSet::from([
                receipt.snapshot_load.clone(),
                receipt.snapshot_drop_point.clone(),
            ])
}

/// Recognize a value produced by an exact V2 GetAny or Values unpack,
/// optionally moved through same-type MIR locals. This lets the body receipt
/// validator admit a candidate consuming field projection while keeping
/// unrelated ManagedAny values outside its vocabulary. Type/layout/glue
/// validity is checked by the catalog's ManagedAny flow validator before any
/// consumer executes or emits the MIR.
fn has_checker_ordinary_map_owned_any_v2_unpack_lineage(
    function: &MirFunction,
    value: &MirValueId,
) -> bool {
    let instructions = function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();
    let mut unpacked_values = instructions
        .iter()
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack {
                result,
                pair,
                projection,
            } => has_checker_ordinary_map_owned_any_v2_unpack(
                function,
                &instruction.id,
                result,
                pair,
                projection,
            )
            .then(|| result.clone()),
            MirInstructionKind::OrdinaryMapOwnedAnyV2ValuesUnpack {
                result,
                values,
                index,
                projection,
            } => has_checker_ordinary_map_owned_any_v2_values_unpack(
                function,
                &instruction.id,
                result,
                values,
                index,
                projection,
            )
            .then(|| result.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();

    loop {
        let mut changed = false;
        for instruction in &instructions {
            let (source, result) = match &instruction.kind {
                MirInstructionKind::Move { source, result }
                | MirInstructionKind::Clone { source, result } => (source, result),
                _ => continue,
            };
            let same_type = function
                .values
                .get(source)
                .zip(function.values.get(result))
                .is_some_and(|(source, result)| source.ty == result.ty);
            if same_type && unpacked_values.contains(source) {
                changed |= unpacked_values.insert(result.clone());
            }
        }
        if !changed {
            break;
        }
    }

    unpacked_values.contains(value)
}

fn has_checker_ordinary_map_owned_any_v2_values_result(
    function: &MirFunction,
    instruction: &MirInstructionId,
    result: &MirValueId,
) -> bool {
    use crate::core::OrdinaryMapOwnedAnyV2ActionKind as ActionKind;
    use crate::core::OrdinaryMapOwnedAnyV2ProfileVersion as ProfileVersion;
    use crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind as Settlement;

    let matching_actions = function
        .ownership
        .checker_ordinary_map_owned_any_v2
        .iter()
        .filter(|action| action.instruction == *instruction)
        .collect::<Vec<_>>();
    let [action] = matching_actions.as_slice() else {
        return false;
    };
    if action.profile_version != ProfileVersion::V2
        || action.kind != ActionKind::Values
        || action.result.as_ref() != Some(result)
        || action.result_source_type.is_none()
        || action.key.is_some()
        || action.key_type.is_some()
        || action.key_node.is_some()
        || action.key_local.is_some()
        || action.value.is_some()
        || action.value_type.is_some()
        || action.payload_clone
    {
        return false;
    }
    let Some(cfg) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
    else {
        return false;
    };
    if cfg.owner != function.owner || cfg.profile_version != ProfileVersion::V2 {
        return false;
    }
    let matching_cfg_actions = cfg
        .actions
        .iter()
        .filter(|candidate| candidate.point == action.point && candidate.kind == ActionKind::Values)
        .collect::<Vec<_>>();
    let [cfg_action] = matching_cfg_actions.as_slice() else {
        return false;
    };
    let cfg_local_value = MirValueId::new(format!("local:{}", cfg_action.local.0 .0)).ok();
    let action_matches = cfg_action.profile_version == ProfileVersion::V2
        && cfg_local_value.as_ref() == Some(&action.local)
        && cfg_action.root.0 .0 == action.root
        && cfg_action.source_local.as_ref() == Some(&cfg_action.local)
        && cfg_action.source.as_ref().map(|source| &source.0 .0) == Some(&action.root)
        && cfg_action.source_node == action.source_node
        && cfg_action.source_type == action.source_type
        && cfg_action.key.is_none()
        && cfg_action.key_type.is_none()
        && cfg_action.value.is_none()
        && cfg_action.value_type.is_none()
        && !cfg_action.payload_clone
        && cfg_action.result_type == action.result_source_type
        && cfg_action.result_local == action.result_local;
    if !action_matches {
        return false;
    }
    let matching_results = cfg
        .result_receipts
        .iter()
        .filter(|receipt| receipt.operation == action.point && receipt.kind == ActionKind::Values)
        .collect::<Vec<_>>();
    let [receipt] = matching_results.as_slice() else {
        return false;
    };
    let settlement_matches = match receipt.settlement {
        Settlement::ExplicitDrop => receipt.settlement_point.is_some(),
        Settlement::Return => {
            let Some(producer) = cfg.values_producer_return_receipt.as_ref() else {
                return false;
            };
            let producer_return_matches = producer.matches_returned_result(receipt)
                && producer.owner == function.owner
                && producer.values_operation == action.point
                && cfg_action.result_local.as_ref() == Some(&producer.snapshot_local)
                && producer.snapshot_local == receipt.binding
                && producer.snapshot_binding_point == receipt.binding_point
                && producer.snapshot_type == receipt.result_type;
            let local_producer_caller_matches = match (
                cfg.values_producer_caller_receipt.as_ref(),
                cfg.values_crosscall_string_caller_receipt.as_ref(),
            ) {
                (Some(caller), None) => function
                    .ownership
                    .checker_managed_any
                    .as_ref()
                    .is_some_and(|seal| seal.v2_values_crosscall_string_caller.is_none())
                    && function
                        .ownership
                        .checker_ordinary_map_owned_any_v2_values_crosscall_string_caller_receipt
                        .is_none()
                    && caller.profile_version
                        == crate::core::OrdinaryMapOwnedAnyV2ValuesProducerCallerProfileVersion::V1
                    && caller.producer_owner == producer.owner
                    && caller.producer_values_operation == producer.values_operation
                    && caller.producer_implicit_return == producer.return_value
                    && caller.call_target == producer.owner
                    && caller.caller_owner != producer.owner
                    && caller.result_type == producer.surface_return_type
                    && caller.owner_type == producer.snapshot_type,
                _ => false,
            };
            let crosscall_string_caller_matches = match (
                cfg.values_producer_caller_receipt.as_ref(),
                cfg.values_crosscall_string_caller_receipt.as_ref(),
            ) {
                (None, Some(caller)) => {
                    has_checker_ordinary_map_owned_any_v2_values_crosscall_string_caller(
                        function, producer, caller,
                    )
                }
                _ => false,
            };
            producer_return_matches
                && (local_producer_caller_matches || crosscall_string_caller_matches)
        }
        _ => false,
    };
    receipt.profile_version == ProfileVersion::V2
        && receipt.result == action.point
        && action
            .result_source_type
            .as_ref()
            .is_some_and(|ty| receipt.result_type == *ty)
        && Some(&receipt.binding) == action.result_local.as_ref()
        && receipt.source_local == cfg_action.local
        && receipt.source == cfg_action.root
        && cfg_action.source_node.as_ref() == Some(&receipt.source_node)
        && cfg_action.source_type.as_ref() == Some(&receipt.source_type)
        && receipt.result == action.point
        && receipt.key.is_none()
        && receipt.key_type.is_none()
        && settlement_matches
}

/// Checker half of the independent cross-call String caller profile. Full
/// MIR value-use, owner settlement, and caller census checks are rederived
/// from the complete MirProgram by the cross-call receipt validator.
fn has_checker_ordinary_map_owned_any_v2_values_crosscall_string_caller(
    function: &MirFunction,
    producer: &crate::core::OrdinaryMapOwnedAnyV2ValuesProducerReturnReceipt,
    caller: &crate::core::ownership::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerReceipt,
) -> bool {
    use crate::core::ownership::OrdinaryMapOwnedAnyV2ValuesProducerReturnProfileVersion as ProducerProfile;
    use crate::core::OrdinaryMapOwnedAnyV2ActionKind as ActionKind;
    use crate::core::OrdinaryMapOwnedAnyV2ProfileVersion as ProfileVersion;

    let Some(cfg) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
    else {
        return false;
    };
    let Some(seal) = function.ownership.checker_managed_any.as_ref() else {
        return false;
    };
    let values_actions = cfg
        .actions
        .iter()
        .filter(|candidate| candidate.kind == ActionKind::Values)
        .collect::<Vec<_>>();
    let values_results = cfg
        .result_receipts
        .iter()
        .filter(|candidate| candidate.kind == ActionKind::Values)
        .collect::<Vec<_>>();
    let [values_action] = values_actions.as_slice() else {
        return false;
    };
    let [values_result] = values_results.as_slice() else {
        return false;
    };
    cfg.owner == function.owner
        && cfg.profile_version == ProfileVersion::V2
        && cfg.values_producer_caller_receipt.is_none()
        && function
            .ownership
            .checker_ordinary_map_owned_any_v2_values_caller_receipt
            .is_none()
        && function
            .ownership
            .checker_ordinary_map_owned_any_v2_values_crosscall_string_caller_receipt
            .is_none()
        && cfg.values_crosscall_string_caller_receipt.as_ref() == Some(caller)
        && seal.v2_values_crosscall_string_caller.as_ref() == Some(caller)
        && cfg.values_producer_return_receipt.as_ref() == Some(producer)
        && values_action.point == producer.values_operation
        && values_result.operation == producer.values_operation
        && producer.profile_version == ProducerProfile::V1
        && producer.settlement == crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::Return
        && producer.owner == function.owner
        && values_crosscall_string_caller_profile_matches(caller)
        && caller.producer_owner == function.owner
        && caller.producer_values_operation == producer.values_operation
        && caller.producer_return == producer.return_value
        && caller.call_target == function.owner
        && caller.caller_owner != function.owner
        && caller.snapshot_type == producer.snapshot_type
        && caller.call_result_type == producer.surface_return_type
        && caller.string_type == crate::core::ir::primitive_type_id(PrimitiveType::String)
        && caller.boolean_type == crate::core::ir::primitive_type_id(PrimitiveType::Bool)
        && caller.index_type == crate::core::ir::primitive_type_id(PrimitiveType::I32)
        && !caller.call_node.0.trim().is_empty()
        && !caller.snapshot_local.0 .0.trim().is_empty()
        && !caller.snapshot_binding_point.0.trim().is_empty()
        && !caller.producer_return.0.trim().is_empty()
}

fn split_role_caller_snapshot<T: Clone>(
    producer: Option<&T>,
    caller: Option<&T>,
) -> Option<Option<T>> {
    match (producer, caller) {
        (None, None) => Some(None),
        (Some(receipt), None) | (None, Some(receipt)) => Some(Some(receipt.clone())),
        (Some(_), Some(_)) => None,
    }
}

/// Snapshot the Checker cross-call receipt from exactly one role: the
/// producer CFG or this callable's caller-side ownership receipt. A function
/// carrying both copies is a mixed profile and cannot acquire a seal.
fn values_crosscall_string_caller_snapshot(
    function: &MirFunction,
) -> Option<Option<crate::core::ownership::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerReceipt>>
{
    let producer = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
        .and_then(|cfg| cfg.values_crosscall_string_caller_receipt.as_ref());
    let caller = function
        .ownership
        .checker_ordinary_map_owned_any_v2_values_crosscall_string_caller_receipt
        .as_deref();
    split_role_caller_snapshot(producer, caller)
}

/// Snapshot the Checker Result Values producer-to-length caller receipt from
/// either the producer CFG or the caller-side ownership summary, never both.
fn values_result_producer_caller_snapshot(
    function: &MirFunction,
) -> Option<Option<crate::core::ownership::OrdinaryMapOwnedAnyV2ValuesResultProducerCallerReceipt>>
{
    let producer = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
        .and_then(|cfg| cfg.values_result_producer_caller_receipt.as_ref());
    let caller = function
        .ownership
        .checker_ordinary_map_owned_any_v2_values_result_producer_caller_receipt
        .as_deref();
    split_role_caller_snapshot(producer, caller)
}

fn has_checker_ordinary_map_owned_any_v2_values_caller_call(
    function: &MirFunction,
    instruction: &MirInstructionId,
    callee: &NodeId,
    type_arguments: &[ResolvedTypeId],
    arguments: &[MirValueId],
    effect_receipts: &[super::types::MirCallEffectContract],
    variant_call_contract: Option<&super::types::MirVariantCallAbiContract>,
    result: &MirValueId,
) -> bool {
    use crate::core::OrdinaryMapOwnedAnyV2ValuesProducerCallerProfileVersion as ProfileVersion;

    let Some(receipt) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_values_caller_receipt
        .as_ref()
    else {
        return false;
    };
    let expected_instruction =
        MirInstructionId::new(format!("inst:call:{}", receipt.call_node.0)).ok();
    let expected_result = MirValueId::new(format!("expr:{}", receipt.call_node.0)).ok();
    let expected_binding = MirValueId::new(format!("local:{}", receipt.owner_local.0 .0)).ok();
    receipt.profile_version == ProfileVersion::V1
        && receipt.caller_owner == function.owner
        && function
            .ownership
            .checker_ordinary_map_owned_any_v2_values_crosscall_string_caller_receipt
            .is_none()
        && function
            .ownership
            .checker_managed_any
            .as_ref()
            .is_none_or(|seal| seal.v2_values_crosscall_string_caller.is_none())
        && receipt.call_target == receipt.producer_owner
        && receipt.result_binding == receipt.owner_local
        && receipt.result_type != receipt.owner_type
        && receipt.caller_return_type == function.result
        && expected_instruction.as_ref() == Some(instruction)
        && expected_result.as_ref() == Some(result)
        && function
            .values
            .get(result)
            .is_some_and(|value| value.ty == ordinary_map_owned_any_v2_values_type_id())
        && expected_binding.as_ref().is_some_and(|binding| {
            function
                .values
                .get(binding)
                .is_some_and(|value| value.ty == ordinary_map_owned_any_v2_values_type_id())
        })
        && callee == &receipt.call_target
        && type_arguments.is_empty()
        && arguments.is_empty()
        && effect_receipts.is_empty()
        && variant_call_contract.is_none()
}

fn has_checker_ordinary_map_owned_any_v2_values_crosscall_string_caller_call(
    function: &MirFunction,
    instruction: &MirInstructionId,
    callee: &NodeId,
    type_arguments: &[ResolvedTypeId],
    arguments: &[MirValueId],
    effect_receipts: &[super::types::MirCallEffectContract],
    variant_call_contract: Option<&super::types::MirVariantCallAbiContract>,
    result: &MirValueId,
) -> bool {
    let Some(receipt) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_values_crosscall_string_caller_receipt
        .as_ref()
    else {
        return false;
    };
    let Some(seal) = function.ownership.checker_managed_any.as_ref() else {
        return false;
    };
    let expected_instruction =
        MirInstructionId::new(format!("inst:call:{}", receipt.call_node.0)).ok();
    let expected_result = MirValueId::new(format!("expr:{}", receipt.call_node.0)).ok();
    let expected_binding = MirValueId::new(format!("local:{}", receipt.snapshot_local.0 .0)).ok();
    let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
    let bool_type = crate::core::ir::primitive_type_id(PrimitiveType::Bool);
    let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
    values_crosscall_string_caller_profile_matches(receipt)
        && receipt.caller_owner == function.owner
        && receipt.producer_owner != function.owner
        && receipt.call_target == receipt.producer_owner
        && receipt.call_result_type != receipt.snapshot_type
        && receipt.string_type == string_type
        && receipt.boolean_type == bool_type
        && receipt.index_type == i32_type
        && function.result == bool_type
        && seal.v2_values_crosscall_string_caller.as_ref() == Some(receipt)
        && function
            .ownership
            .checker_ordinary_map_owned_any_v2_values_caller_receipt
            .is_none()
        && function
            .ownership
            .checker_ordinary_map_owned_any_v2
            .is_empty()
        && function
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .is_none_or(|cfg| {
                cfg.owner == function.owner
                    && cfg.actions.is_empty()
                    && cfg.attempts.is_empty()
                    && cfg.result_receipts.is_empty()
                    && cfg.values_producer_return_receipt.is_none()
                    && cfg.values_crosscall_string_caller_receipt.is_none()
                    && cfg.values_producer_caller_receipt.is_none()
                    && cfg.values_unpack_receipts.is_empty()
            })
        && expected_instruction.as_ref() == Some(instruction)
        && expected_result.as_ref() == Some(result)
        && function
            .values
            .get(result)
            .is_some_and(|value| value.ty == ordinary_map_owned_any_v2_values_type_id())
        && expected_binding.as_ref().is_some_and(|binding| {
            function
                .values
                .get(binding)
                .is_some_and(|value| value.ty == ordinary_map_owned_any_v2_values_type_id())
        })
        && callee == &receipt.call_target
        && type_arguments.is_empty()
        && arguments.is_empty()
        && effect_receipts.is_empty()
        && variant_call_contract.is_none()
}

fn has_checker_ordinary_map_owned_any_v2_values_result_producer_caller_call(
    function: &MirFunction,
    instruction: &MirInstructionId,
    callee: &NodeId,
    type_arguments: &[ResolvedTypeId],
    arguments: &[MirValueId],
    effect_receipts: &[super::types::MirCallEffectContract],
    variant_call_contract: Option<&super::types::MirVariantCallAbiContract>,
    result: &MirValueId,
) -> bool {
    let Some(receipt) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_values_result_producer_caller_receipt
        .as_ref()
    else {
        return false;
    };
    let Some(seal) = function.ownership.checker_managed_any.as_ref() else {
        return false;
    };
    let expected_instruction =
        MirInstructionId::new(format!("inst:call:{}", receipt.producer_call.0)).ok();
    let expected_result = MirValueId::new(format!("expr:{}", receipt.producer_call.0)).ok();
    let expected_binding = MirValueId::new(format!("local:{}", receipt.snapshot_local.0 .0)).ok();
    let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
    receipt.profile_version
        == crate::core::ownership::OrdinaryMapOwnedAnyV2ValuesResultProducerCallerProfileVersion::V1
        && receipt.caller_owner == function.owner
        && receipt.producer_owner != function.owner
        && receipt.producer_target == receipt.producer_owner
        && receipt.call_result_type != receipt.snapshot_type
        && receipt.caller_return_type == i32_type
        && receipt.len.result_type == i32_type
        && function.result == i32_type
        && seal.v2_values_result_producer_caller.as_ref() == Some(receipt)
        && seal.v2_values_crosscall_string_caller.is_none()
        && seal.v2_values_producer_caller.is_none()
        && function
            .ownership
            .checker_ordinary_map_owned_any_v2_values_caller_receipt
            .is_none()
        && function
            .ownership
            .checker_ordinary_map_owned_any_v2_values_crosscall_string_caller_receipt
            .is_none()
        && function
            .ownership
            .checker_ordinary_map_owned_any_v2_values_result_producer_receipt
            .is_none()
        && function
            .ownership
            .checker_ordinary_map_owned_any_v2
            .is_empty()
        && function
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .is_none_or(|cfg| {
                cfg.owner == function.owner
                    && cfg.actions.is_empty()
                    && cfg.attempts.is_empty()
                    && cfg.result_receipts.is_empty()
                    && cfg.values_producer_return_receipt.is_none()
                    && cfg.values_crosscall_string_caller_receipt.is_none()
                    && cfg.values_producer_caller_receipt.is_none()
                    && cfg.values_result_producer_receipt.is_none()
                    && cfg.values_result_producer_caller_receipt.is_none()
                    && cfg.values_unpack_receipts.is_empty()
            })
        && expected_instruction.as_ref() == Some(instruction)
        && expected_result.as_ref() == Some(result)
        && function
            .values
            .get(result)
            .is_some_and(|value| value.ty == ordinary_map_owned_any_v2_values_type_id())
        && expected_binding.as_ref().is_some_and(|binding| {
            function
                .values
                .get(binding)
                .is_some_and(|value| value.ty == ordinary_map_owned_any_v2_values_type_id())
        })
        && callee == &receipt.producer_target
        && type_arguments.is_empty()
        && arguments.is_empty()
        && effect_receipts.is_empty()
        && variant_call_contract.is_none()
}

fn sealed_managed_any_instruction_origin(
    function: &MirFunction,
    instruction: &MirInstructionId,
    origin: &NodeId,
) -> bool {
    function
        .ownership
        .checker_managed_any
        .as_ref()
        .is_some_and(|seal| {
            let matching = seal
                .instructions
                .iter()
                .filter(|(_, _, candidate)| &candidate.id == instruction)
                .collect::<Vec<_>>();
            matches!(matching.as_slice(), [(_, sealed_origin, _)] if sealed_origin == origin)
        })
}

pub(super) fn has_checker_ordinary_map_owned_any_v2_map_root_producer_return(
    function: &MirFunction,
) -> bool {
    use crate::core::ownership::OrdinaryMapOwnedAnyV2MapRootProducerProfileVersion as Version;

    let Some((Some(producer), producer_caller, get_any_caller)) =
        map_root_boundary_seal_snapshots(function)
    else {
        return false;
    };
    let Some(seal) = function.ownership.checker_managed_any.as_ref() else {
        return false;
    };
    let boundary_receipt_matches = match (producer_caller.as_ref(), get_any_caller.as_ref()) {
        (Some(caller), None) => {
            seal.v2_map_root_producer_caller.as_ref() == Some(caller)
                && seal.v2_map_root_get_any_caller.is_none()
        }
        (None, Some(caller)) if caller.producer_owner == function.owner => {
            seal.v2_map_root_producer_caller.is_none()
                && seal.v2_map_root_get_any_caller.as_ref() == Some(caller)
        }
        _ => false,
    };
    let root_type = super::types::ordinary_map_owned_any_v2_type_id();
    let Ok(source) = MirValueId::new(format!("local:{}", producer.final_root_local.0 .0)) else {
        return false;
    };
    let Ok(result) = MirValueId::new(format!("expr:{}", producer.return_value.0)) else {
        return false;
    };
    let Ok(move_id) = MirInstructionId::new(format!("inst:move:{}", producer.return_value.0))
    else {
        return false;
    };
    let returns = function
        .blocks
        .values()
        .filter_map(|block| match &block.terminator {
            MirTerminator::Return { value } => Some(value.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let exact_moves = function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            instruction.id == move_id
                && matches!(
                    &instruction.kind,
                    MirInstructionKind::Move { source: actual_source, result: actual_result }
                        if actual_source == &source && actual_result == &result
                )
        })
        .count();
    let has_unreceipted_call_like_instruction = function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .any(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::Call { .. }
                    | MirInstructionKind::ClosureInvoke { .. }
                    | MirInstructionKind::FlowTransition { .. }
                    | MirInstructionKind::BuiltinCall { .. }
                    | MirInstructionKind::SessionCall { .. }
                    | MirInstructionKind::Actor { .. }
            )
        });
    producer.profile_version == Version::V1
        && producer.owner == function.owner
        && boundary_receipt_matches
        && function.parameters.is_empty()
        && function
            .ownership
            .checker_callable_generic_parameters
            .is_empty()
        && function.result == root_type
        && seal.v2_map_root_producer_return.as_ref() == Some(&producer)
        && seal.v2_map_root_producer_caller.as_ref() == producer_caller.as_ref()
        && seal.v2_map_root_get_any_caller.as_ref() == get_any_caller.as_ref()
        && function
            .values
            .get(&source)
            .is_some_and(|value| value.ty == root_type)
        && function
            .values
            .get(&result)
            .is_some_and(|value| value.ty == root_type)
        && seal.value_types.get(&source) == Some(&root_type)
        && seal.value_types.get(&result) == Some(&root_type)
        && !has_unreceipted_call_like_instruction
        && exact_moves == 1
        && sealed_managed_any_instruction_origin(function, &move_id, &producer.return_value)
        && returns.as_slice() == [Some(&result)]
        && function.blocks.len() == 1
}

fn has_checker_ordinary_map_owned_any_v2_map_root_producer_return_move(
    function: &MirFunction,
    instruction: &MirInstruction,
    source: &MirValueId,
    result: &MirValueId,
) -> bool {
    let Some((Some(producer), _, _)) = map_root_boundary_seal_snapshots(function) else {
        return false;
    };
    let Ok(expected_source) = MirValueId::new(format!("local:{}", producer.final_root_local.0 .0))
    else {
        return false;
    };
    let Ok(expected_result) = MirValueId::new(format!("expr:{}", producer.return_value.0)) else {
        return false;
    };
    let Ok(expected_id) = MirInstructionId::new(format!("inst:move:{}", producer.return_value.0))
    else {
        return false;
    };
    has_checker_ordinary_map_owned_any_v2_map_root_producer_return(function)
        && instruction.id == expected_id
        && matches!(
            &instruction.kind,
            MirInstructionKind::Move { source: actual_source, result: actual_result }
                if actual_source == &expected_source && actual_result == &expected_result
        )
        && source == &expected_source
        && result == &expected_result
        && sealed_managed_any_instruction_origin(function, &expected_id, &producer.return_value)
}

pub(super) fn has_checker_ordinary_map_owned_any_v2_map_root_get_any_caller_call(
    function: &MirFunction,
    instruction: &MirInstructionId,
    callee: &NodeId,
    type_arguments: &[ResolvedTypeId],
    arguments: &[MirValueId],
    effect_receipts: &[super::types::MirCallEffectContract],
    variant_call_contract: Option<&super::types::MirVariantCallAbiContract>,
    result: &MirValueId,
) -> bool {
    let Some(Some(receipt)) = map_root_get_any_direct_caller_seal_snapshot(function) else {
        return false;
    };
    let Some(seal) = function.ownership.checker_managed_any.as_ref() else {
        return false;
    };
    let Ok(expected_instruction) =
        MirInstructionId::new(format!("inst:call:{}", receipt.call_node.0))
    else {
        return false;
    };
    let Ok(expected_result) = MirValueId::new(format!("expr:{}", receipt.call_node.0)) else {
        return false;
    };
    let root_type = super::types::ordinary_map_owned_any_v2_type_id();
    receipt.caller_owner == function.owner
        && receipt.call_target == receipt.producer_owner
        && seal.v2_map_root_get_any_caller.as_ref() == Some(&receipt)
        && instruction == &expected_instruction
        && callee == &receipt.call_target
        && type_arguments.is_empty()
        && arguments.is_empty()
        && effect_receipts.is_empty()
        && variant_call_contract.is_none()
        && result == &expected_result
        && function
            .values
            .get(result)
            .is_some_and(|value| value.ty == root_type)
        && seal.value_types.get(result) == Some(&root_type)
        && sealed_managed_any_instruction_origin(function, instruction, &receipt.call_node)
}

pub(super) fn has_checker_ordinary_map_owned_any_v2_map_root_caller_call(
    function: &MirFunction,
    instruction: &MirInstructionId,
    callee: &NodeId,
    type_arguments: &[ResolvedTypeId],
    arguments: &[MirValueId],
    effect_receipts: &[super::types::MirCallEffectContract],
    variant_call_contract: Option<&super::types::MirVariantCallAbiContract>,
    result: &MirValueId,
) -> bool {
    use crate::core::ownership::OrdinaryMapOwnedAnyV2MapRootCallerProfileVersion as Version;

    let Some((None, Some(receipt))) = map_root_producer_caller_seal_snapshots(function) else {
        return false;
    };
    let Some(seal) = function.ownership.checker_managed_any.as_ref() else {
        return false;
    };
    let Ok(expected_instruction) =
        MirInstructionId::new(format!("inst:call:{}", receipt.call_node.0))
    else {
        return false;
    };
    let Ok(expected_result) = MirValueId::new(format!("expr:{}", receipt.call_node.0)) else {
        return false;
    };
    let Ok(expected_binding) = MirValueId::new(format!("local:{}", receipt.caller_root_local.0 .0))
    else {
        return false;
    };
    let Ok(binding_instruction) =
        MirInstructionId::new(format!("inst:bind:{}", receipt.result_binding_point.0))
    else {
        return false;
    };
    let root_type = super::types::ordinary_map_owned_any_v2_type_id();
    let binding_moves = function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            instruction.id == binding_instruction
                && matches!(
                    &instruction.kind,
                    MirInstructionKind::Move { source, result }
                        if source == &expected_result && result == &expected_binding
                )
        })
        .count();
    receipt.profile_version == Version::V1
        && receipt.caller_owner == function.owner
        && receipt.call_target == receipt.producer_owner
        && receipt.result_binding == receipt.caller_root_local
        && receipt.result_type == receipt.caller_root_type
        && receipt.caller_return_type == function.result
        && receipt.caller_return_type == receipt.size_type
        && function
            .ownership
            .checker_callable_generic_parameters
            .is_empty()
        && function.parameters.is_empty()
        && seal.v2_map_root_producer_return.is_none()
        && seal.v2_map_root_producer_caller.as_ref() == Some(&receipt)
        && &expected_instruction == instruction
        && &expected_result == result
        && function
            .values
            .get(result)
            .is_some_and(|value| value.ty == root_type)
        && function
            .values
            .get(&expected_binding)
            .is_some_and(|value| value.ty == root_type)
        && seal.value_types.get(result) == Some(&root_type)
        && seal.value_types.get(&expected_binding) == Some(&root_type)
        && callee == &receipt.call_target
        && type_arguments.is_empty()
        && arguments.is_empty()
        && effect_receipts.is_empty()
        && variant_call_contract.is_none()
        && binding_moves == 1
        && sealed_managed_any_instruction_origin(function, instruction, &receipt.call_node)
        && sealed_managed_any_instruction_origin(
            function,
            &binding_instruction,
            &receipt.result_binding_point,
        )
}

fn has_checker_ordinary_map_owned_any_v2_map_root_caller_binding_move(
    function: &MirFunction,
    instruction: &MirInstruction,
    source: &MirValueId,
    result: &MirValueId,
) -> bool {
    let Some((None, Some(receipt))) = map_root_producer_caller_seal_snapshots(function) else {
        return false;
    };
    let Some(seal) = function.ownership.checker_managed_any.as_ref() else {
        return false;
    };
    let Ok(expected_source) = MirValueId::new(format!("expr:{}", receipt.call_node.0)) else {
        return false;
    };
    let Ok(expected_result) = MirValueId::new(format!("local:{}", receipt.caller_root_local.0 .0))
    else {
        return false;
    };
    let Ok(expected_id) =
        MirInstructionId::new(format!("inst:bind:{}", receipt.result_binding_point.0))
    else {
        return false;
    };
    let root_type = super::types::ordinary_map_owned_any_v2_type_id();
    let count = function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter(|candidate| {
            candidate.id == expected_id
                && matches!(
                    &candidate.kind,
                    MirInstructionKind::Move { source, result }
                        if source == &expected_source && result == &expected_result
                )
        })
        .count();
    instruction.id == expected_id
        && matches!(
            &instruction.kind,
            MirInstructionKind::Move { source: actual_source, result: actual_result }
                if actual_source == &expected_source && actual_result == &expected_result
        )
        && source == &expected_source
        && result == &expected_result
        && function
            .values
            .get(source)
            .is_some_and(|value| value.ty == root_type)
        && function
            .values
            .get(result)
            .is_some_and(|value| value.ty == root_type)
        && seal.v2_map_root_producer_caller.as_ref() == Some(&receipt)
        && count == 1
        && sealed_managed_any_instruction_origin(
            function,
            &expected_id,
            &receipt.result_binding_point,
        )
}

pub(super) fn has_checker_ordinary_map_owned_any_v2_map_root_get_any_caller_binding_move(
    function: &MirFunction,
    instruction: &MirInstruction,
    source: &MirValueId,
    result: &MirValueId,
) -> bool {
    let Some((_, _, Some(receipt))) = map_root_boundary_seal_snapshots(function) else {
        return false;
    };
    let Some(seal) = function.ownership.checker_managed_any.as_ref() else {
        return false;
    };
    let Ok(expected_source) = MirValueId::new(format!("expr:{}", receipt.call_node.0)) else {
        return false;
    };
    let Ok(expected_result) = MirValueId::new(format!("local:{}", receipt.caller_root_local.0 .0))
    else {
        return false;
    };
    let Ok(expected_id) =
        MirInstructionId::new(format!("inst:bind:{}", receipt.caller_root_binding_point.0))
    else {
        return false;
    };
    let root_type = super::types::ordinary_map_owned_any_v2_type_id();
    let count = function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter(|candidate| {
            candidate.id == expected_id
                && matches!(
                    &candidate.kind,
                    MirInstructionKind::Move { source, result }
                        if source == &expected_source && result == &expected_result
                )
        })
        .count();
    instruction.id == expected_id
        && matches!(
            &instruction.kind,
            MirInstructionKind::Move { source: actual_source, result: actual_result }
                if actual_source == &expected_source && actual_result == &expected_result
        )
        && source == &expected_source
        && result == &expected_result
        && function
            .values
            .get(source)
            .is_some_and(|value| value.ty == root_type)
        && function
            .values
            .get(result)
            .is_some_and(|value| value.ty == root_type)
        && seal.v2_map_root_get_any_caller.as_ref() == Some(&receipt)
        && count == 1
        && sealed_managed_any_instruction_origin(
            function,
            &expected_id,
            &receipt.caller_root_binding_point,
        )
}

fn has_checker_ordinary_map_owned_any_v2_map_root_caller_return(function: &MirFunction) -> bool {
    let Some((None, Some(receipt))) = map_root_producer_caller_seal_snapshots(function) else {
        return false;
    };
    let Ok(returned) = MirValueId::new(format!("expr:{}", receipt.caller_return.0)) else {
        return false;
    };
    let Ok(size_binding) = MirValueId::new(format!("local:{}", receipt.size_binding.0 .0)) else {
        return false;
    };
    let expected_return_clone =
        MirInstructionId::new(format!("inst:clone:{}", receipt.caller_return.0));
    let Ok(expected_return_clone) = expected_return_clone else {
        return false;
    };
    let return_clones = function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            instruction.id == expected_return_clone
                && matches!(
                    &instruction.kind,
                    MirInstructionKind::Clone { source, result }
                        if source == &size_binding && result == &returned
                )
        })
        .count();
    let returns = function
        .blocks
        .values()
        .filter_map(|block| match &block.terminator {
            MirTerminator::Return { value } => Some(value.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>();
    function.blocks.len() == 1
        && receipt.caller_return_type == function.result
        && receipt.caller_return_type == receipt.size_type
        && function
            .values
            .get(&size_binding)
            .is_some_and(|value| value.ty == receipt.size_type)
        && function
            .values
            .get(&returned)
            .is_some_and(|value| value.ty == receipt.caller_return_type)
        && return_clones == 1
        && sealed_managed_any_instruction_origin(
            function,
            &expected_return_clone,
            &receipt.caller_return,
        )
        && returns.as_slice() == [Some(&returned)]
}

pub(super) fn validate_checker_ordinary_map_owned_any_v2_map_root_caller_shape(
    function: &MirFunction,
) -> Result<(), String> {
    let Some((None, Some(receipt))) = map_root_producer_caller_seal_snapshots(function) else {
        return Err("Map-root caller has no independent exact Checker caller receipt".into());
    };
    if !has_checker_ordinary_map_owned_any_v2_map_root_caller_return(function) {
        return Err(
            "Map-root caller Return differs from its exact Checker return binding/type".into(),
        );
    }
    let calls = function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::Call {
                result: Some(result),
                callee: crate::core::ir::ResolvedCallee::Function(callee),
                type_arguments,
                arguments,
                effect_receipts,
                variant_call_contract,
                ..
            } if instruction.id.0 == format!("inst:call:{}", receipt.call_node.0)
                && has_checker_ordinary_map_owned_any_v2_map_root_caller_call(
                    function,
                    &instruction.id,
                    callee,
                    type_arguments,
                    arguments,
                    effect_receipts,
                    variant_call_contract.as_ref(),
                    result,
                ) =>
            {
                Some(())
            }
            _ => None,
        })
        .count();
    let call_like_instructions = function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::Call { .. }
                    | MirInstructionKind::ClosureInvoke { .. }
                    | MirInstructionKind::FlowTransition { .. }
                    | MirInstructionKind::BuiltinCall { .. }
                    | MirInstructionKind::SessionCall { .. }
                    | MirInstructionKind::Actor { .. }
            )
        })
        .count();
    let (sizes, drops, caller_binds, size_binds) = function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .fold(
            (0, 0, 0, 0),
            |(sizes, drops, caller_binds, size_binds), instruction| {
                let sizes = sizes
                    + (matches!(
                        &instruction.kind,
                        MirInstructionKind::OrdinaryMapOwnedAnyV2Size { result, root }
                            if has_checker_ordinary_map_owned_any_v2_map_root_caller_size(
                                function, instruction, result, root
                            )
                    ) as usize);
                let drops = drops
                    + (matches!(
                        &instruction.kind,
                        MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { root }
                            if has_checker_ordinary_map_owned_any_v2_map_root_caller_drop(
                                function, instruction, root
                            )
                    ) as usize);
                let (caller_source, caller_result) = (
                    MirValueId::new(format!("expr:{}", receipt.call_node.0)).ok(),
                    MirValueId::new(format!("local:{}", receipt.caller_root_local.0 .0)).ok(),
                );
                let caller_binds = caller_binds
                    + (matches!(
                        (&instruction.kind, caller_source, caller_result),
                        (
                            MirInstructionKind::Move { source, result },
                            Some(expected_source),
                            Some(expected_result),
                        ) if has_checker_ordinary_map_owned_any_v2_map_root_caller_binding_move(
                            function,
                            instruction,
                            source,
                            result,
                        ) && source == &expected_source
                            && result == &expected_result
                    ) as usize);
                let (size_source, size_result) = (
                    MirValueId::new(format!("expr:{}", receipt.map_size_node.0)).ok(),
                    MirValueId::new(format!("local:{}", receipt.size_binding.0 .0)).ok(),
                );
                let size_binds = size_binds
                    + (matches!(
                        (&instruction.kind, size_source, size_result),
                        (
                            MirInstructionKind::Move { source, result },
                            Some(expected_source),
                            Some(expected_result),
                        ) if instruction.id.0
                            == format!("inst:bind:{}", receipt.size_binding_point.0)
                            && source == &expected_source
                            && result == &expected_result
                            && function
                                .values
                                .get(source)
                                .is_some_and(|value| value.ty == receipt.size_type)
                            && function
                                .values
                                .get(result)
                                .is_some_and(|value| value.ty == receipt.size_type)
                            && sealed_managed_any_instruction_origin(
                                function,
                                &instruction.id,
                                &receipt.size_binding_point,
                            )
                    ) as usize);
                (sizes, drops, caller_binds, size_binds)
            },
        );
    if calls == 1
        && call_like_instructions == 1
        && sizes == 1
        && drops == 1
        && caller_binds == 1
        && size_binds == 1
    {
        Ok(())
    } else {
        Err(format!(
            "Map-root caller requires one exact Call and no other call-like operation, binding, Size, size binding, and Drop; got call={calls}, call_like={call_like_instructions}, bind={caller_binds}, size={sizes}, size_bind={size_binds}, drop={drops}"
        ))
    }
}

pub(super) fn validate_checker_ordinary_map_owned_any_v2_map_root_get_any_caller_shape(
    function: &MirFunction,
) -> Result<(), String> {
    use crate::core::ir::ResolvedCallee;

    let seal = function
        .ownership
        .checker_managed_any
        .as_ref()
        .ok_or("Map-root GetAny-caller has no private ManagedAny seal")?;
    let receipt = seal
        .v2_map_root_get_any_caller
        .as_ref()
        .filter(|receipt| receipt.caller_owner == function.owner)
        .ok_or("Map-root GetAny-caller has no exact caller-local Checker receipt")?;
    let root_type = super::types::ordinary_map_owned_any_v2_type_id();
    let pair_type = ordinary_map_owned_any_v2_get_result_type_id();
    let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
    let scalar_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
    let root = MirValueId::new(format!("local:{}", receipt.caller_root_local.0 .0))
        .map_err(|error| error.to_string())?;
    let pair = MirValueId::new(format!("local:{}", receipt.pair_local.0 .0))
        .map_err(|error| error.to_string())?;
    let call_result = MirValueId::new(format!("expr:{}", receipt.call_node.0))
        .map_err(|error| error.to_string())?;
    let get_any_result = MirValueId::new(format!("expr:{}", receipt.get_any_node.0))
        .map_err(|error| error.to_string())?;
    let key = MirValueId::new(format!("expr:{}", receipt.get_any_key_node.0))
        .map_err(|error| error.to_string())?;
    let string = MirValueId::new(format!("local:{}", receipt.unpack_local.0 .0))
        .map_err(|error| error.to_string())?;
    let unpack_result = MirValueId::new(format!("expr:{}", receipt.unpack_cast.0))
        .map_err(|error| error.to_string())?;
    let scalar = MirValueId::new(format!("expr:{}", receipt.caller_return.0))
        .map_err(|error| error.to_string())?;
    let call_id = MirInstructionId::new(format!("inst:call:{}", receipt.call_node.0))
        .map_err(|error| error.to_string())?;
    let root_binding_id =
        MirInstructionId::new(format!("inst:bind:{}", receipt.caller_root_binding_point.0))
            .map_err(|error| error.to_string())?;
    let get_any_id = MirInstructionId::new(format!(
        "inst:ordinary_map_owned_any_v2_get_any:{root}:{}",
        receipt.get_any_node.0
    ))
    .map_err(|error| error.to_string())?;
    let pair_binding_id =
        MirInstructionId::new(format!("inst:bind:{}", receipt.pair_binding_point.0))
            .map_err(|error| error.to_string())?;
    let unpack_id = MirInstructionId::new(format!(
        "inst:ordinary_map_owned_any_v2_unpack:{}",
        receipt.unpack_cast.0
    ))
    .map_err(|error| error.to_string())?;
    let unpack_binding_id =
        MirInstructionId::new(format!("inst:bind:{}", receipt.unpack_binding_point.0))
            .map_err(|error| error.to_string())?;
    let root_drop_id = MirInstructionId::new(format!(
        "inst:ordinary_map_owned_any_v2_drop:{root}:{}",
        receipt.root_drop_point.0
    ))
    .map_err(|error| error.to_string())?;
    let pair_drop_id = MirInstructionId::new(format!("inst:drop.0:{}", receipt.pair_drop_point.0))
        .map_err(|error| error.to_string())?;
    let string_drop_id = MirInstructionId::new(format!(
        "inst:drop.{}:{}",
        receipt.string_drop_place_index, receipt.string_drop_point.0
    ))
    .map_err(|error| error.to_string())?;
    let return_id = MirInstructionId::new(format!("inst:const:{}", receipt.caller_return.0))
        .map_err(|error| error.to_string())?;

    let instructions = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .collect::<Vec<_>>();
    let matching_call = instructions
        .iter()
        .filter(|instruction| {
            instruction.id == call_id
                && matches!(
                    &instruction.kind,
                    MirInstructionKind::Call {
                        result: Some(result),
                        callee: ResolvedCallee::Function(callee),
                        type_arguments,
                        arguments,
                        effect_receipts,
                        variant_call_contract: None,
                    } if result == &call_result
                        && callee == &receipt.call_target
                        && type_arguments.is_empty()
                        && arguments.is_empty()
                        && effect_receipts.is_empty()
                )
        })
        .count();
    let call_like = instructions
        .iter()
        .filter(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::Call { .. }
                    | MirInstructionKind::ClosureInvoke { .. }
                    | MirInstructionKind::FlowTransition { .. }
                    | MirInstructionKind::BuiltinCall { .. }
                    | MirInstructionKind::SessionCall { .. }
                    | MirInstructionKind::Actor { .. }
            )
        })
        .count();
    let root_bindings = instructions
        .iter()
        .filter(|instruction| {
            instruction.id == root_binding_id
                && matches!(
                    &instruction.kind,
                    MirInstructionKind::Move { source, result }
                        if source == &call_result && result == &root
                )
        })
        .count();
    let pair_binding_matches = instructions
        .iter()
        .filter(|instruction| {
            instruction.id == pair_binding_id
                && matches!(
                    &instruction.kind,
                    MirInstructionKind::Move { source, result }
                        if source == &get_any_result && result == &pair
                )
        })
        .count();
    let get_any_matches = instructions
        .iter()
        .filter(|instruction| {
            instruction.id == get_any_id
                && matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { result, root: actual_root, key: actual_key }
                        if result == &get_any_result && actual_root == &root && actual_key == &key
                )
                && has_checker_ordinary_map_owned_any_v2_action(function, instruction)
                && has_checker_ordinary_map_owned_any_v2_get_any_result(
                    function,
                    &instruction.id,
                    &get_any_result,
                )
        })
        .count();
    let unpack_matches = instructions
        .iter()
        .filter(|instruction| {
            instruction.id == unpack_id
                && matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack { result, pair: actual_pair, projection }
                        if result == &unpack_result
                            && actual_pair == &pair
                            && projection == &receipt.unpack_projection
                )
                && has_checker_ordinary_map_owned_any_v2_unpack(
                    function,
                    &instruction.id,
                    &unpack_result,
                    &pair,
                    &receipt.unpack_projection,
                )
        })
        .count();
    let unpack_binding_matches = instructions
        .iter()
        .filter(|instruction| {
            instruction.id == unpack_binding_id
                && matches!(
                    &instruction.kind,
                    MirInstructionKind::Move { source, result }
                        if source == &unpack_result && result == &string
                )
        })
        .count();
    let root_drop_matches = instructions
        .iter()
        .filter(|instruction| {
            instruction.id == root_drop_id
                && matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { root: actual_root }
                        if actual_root == &root
                )
                && has_checker_ordinary_map_owned_any_v2_action(function, instruction)
        })
        .count();
    let pair_drop_matches = instructions
        .iter()
        .filter(|instruction| {
            instruction.id == pair_drop_id
                && matches!(&instruction.kind, MirInstructionKind::Drop { value } if value == &pair)
        })
        .count();
    let string_drop_matches = instructions
        .iter()
        .filter(|instruction| {
            instruction.id == string_drop_id
                && matches!(&instruction.kind, MirInstructionKind::Drop { value } if value == &string)
        })
        .count();
    let return_matches = instructions
        .iter()
        .filter(|instruction| {
            instruction.id == return_id
                && matches!(
                    &instruction.kind,
                    MirInstructionKind::Const { result, literal }
                        if result == &scalar && literal == &receipt.caller_return_literal
                )
        })
        .count();
    let returns = function
        .blocks
        .values()
        .filter_map(|block| match &block.terminator {
            MirTerminator::Return { value } => Some(value.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let value_types_match = function
        .values
        .get(&call_result)
        .is_some_and(|value| value.ty == root_type)
        && function
            .values
            .get(&root)
            .is_some_and(|value| value.ty == root_type)
        && function
            .values
            .get(&get_any_result)
            .is_some_and(|value| value.ty == pair_type)
        && function
            .values
            .get(&pair)
            .is_some_and(|value| value.ty == pair_type)
        && function
            .values
            .get(&unpack_result)
            .is_some_and(|value| value.ty == string_type)
        && function
            .values
            .get(&string)
            .is_some_and(|value| value.ty == string_type)
        && function
            .values
            .get(&scalar)
            .is_some_and(|value| value.ty == scalar_type);
    let origins_match =
        sealed_managed_any_instruction_origin(function, &call_id, &receipt.call_node)
            && sealed_managed_any_instruction_origin(
                function,
                &root_binding_id,
                &receipt.caller_root_binding_point,
            )
            && sealed_managed_any_instruction_origin(function, &get_any_id, &receipt.get_any_node)
            && sealed_managed_any_instruction_origin(
                function,
                &pair_binding_id,
                &receipt.pair_binding_point,
            )
            && sealed_managed_any_instruction_origin(function, &unpack_id, &receipt.unpack_cast)
            && sealed_managed_any_instruction_origin(
                function,
                &unpack_binding_id,
                &receipt.unpack_binding_point,
            )
            && sealed_managed_any_instruction_origin(
                function,
                &root_drop_id,
                &receipt.root_drop_point,
            )
            && sealed_managed_any_instruction_origin(
                function,
                &pair_drop_id,
                &receipt.pair_drop_point,
            )
            && sealed_managed_any_instruction_origin(
                function,
                &string_drop_id,
                &receipt.string_drop_point,
            )
            && sealed_managed_any_instruction_origin(function, &return_id, &receipt.caller_return);

    // The Checker root and pair types are source identities (`Record` and
    // `(bool, DynamicAny)`). MIR materializes separate owner-v2 handle and
    // payload-token tuple TypeDescs; `value_types_match` checks those exact
    // canonical identities below.
    let checks = [
        ("call target", receipt.call_target == receipt.producer_owner),
        (
            "distinct producer",
            receipt.producer_owner != function.owner,
        ),
        ("String TypeDesc", receipt.string_type == string_type),
        (
            "scalar return TypeDesc",
            receipt.caller_return_type == scalar_type,
        ),
        (
            "scalar return literal",
            receipt.caller_return_literal == crate::core::ir::ResolvedLiteral::Int(0),
        ),
        ("function result", function.result == scalar_type),
        ("zero parameters", function.parameters.is_empty()),
        (
            "non-generic signature",
            function
                .ownership
                .checker_callable_generic_parameters
                .is_empty(),
        ),
        ("single block", function.blocks.len() == 1),
        ("direct call", matching_call == 1),
        ("no extra call-like instruction", call_like == 1),
        ("root binding", root_bindings == 1),
        ("pair binding", pair_binding_matches == 1),
        ("GetAny", get_any_matches == 1),
        ("String unpack", unpack_matches == 1),
        ("String binding", unpack_binding_matches == 1),
        ("root drop", root_drop_matches == 1),
        ("pair drop", pair_drop_matches == 1),
        ("String drop", string_drop_matches == 1),
        ("return constant", return_matches == 1),
        ("return terminator", returns.as_slice() == [Some(&scalar)]),
        ("canonical MIR value types", value_types_match),
        ("sealed Checker origins", origins_match),
    ];
    let failed_checks = checks
        .iter()
        .filter_map(|(name, passes)| (!passes).then_some(*name))
        .collect::<Vec<_>>();
    if !failed_checks.is_empty() {
        return Err(format!(
            "Map-root GetAny-caller MIR does not match its exact Call/binding/GetAny/unpack/drop/Return receipt; failed checks: {}",
            failed_checks.join(", ")
        ));
    }
    Ok(())
}

fn has_checker_ordinary_map_owned_any_v2_map_root_caller_size(
    function: &MirFunction,
    instruction: &MirInstruction,
    result: &MirValueId,
    root: &MirValueId,
) -> bool {
    let Some((None, Some(receipt))) = map_root_producer_caller_seal_snapshots(function) else {
        return false;
    };
    let Some(seal) = function.ownership.checker_managed_any.as_ref() else {
        return false;
    };
    let Ok(expected_root) = MirValueId::new(format!("local:{}", receipt.caller_root_local.0 .0))
    else {
        return false;
    };
    let Ok(expected_result) = MirValueId::new(format!("expr:{}", receipt.map_size_node.0)) else {
        return false;
    };
    let Ok(expected_id) = MirInstructionId::new(format!(
        "inst:ordinary_map_owned_any_v2_size:{expected_root}:{}",
        receipt.map_size_node.0
    )) else {
        return false;
    };
    let Ok(size_binding) = MirValueId::new(format!("local:{}", receipt.size_binding.0 .0)) else {
        return false;
    };
    let Ok(binding_instruction) =
        MirInstructionId::new(format!("inst:bind:{}", receipt.size_binding_point.0))
    else {
        return false;
    };
    let binding_moves = function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter(|candidate| {
            candidate.id == binding_instruction
                && matches!(
                    &candidate.kind,
                    MirInstructionKind::Move { source, result }
                        if source == &expected_result && result == &size_binding
                )
        })
        .count();
    instruction.id == expected_id
        && matches!(
            &instruction.kind,
            MirInstructionKind::OrdinaryMapOwnedAnyV2Size {
                result: actual_result,
                root: actual_root,
            } if actual_result == &expected_result && actual_root == &expected_root
        )
        && result == &expected_result
        && root == &expected_root
        && function
            .values
            .get(root)
            .is_some_and(|value| value.ty == super::types::ordinary_map_owned_any_v2_type_id())
        && function
            .values
            .get(result)
            .is_some_and(|value| value.ty == receipt.size_type)
        && function
            .values
            .get(&size_binding)
            .is_some_and(|value| value.ty == receipt.size_type)
        && seal.value_types.get(result) == Some(&receipt.size_type)
        && binding_moves == 1
        && sealed_managed_any_instruction_origin(function, &instruction.id, &receipt.map_size_node)
        && sealed_managed_any_instruction_origin(
            function,
            &binding_instruction,
            &receipt.size_binding_point,
        )
}

fn has_checker_ordinary_map_owned_any_v2_map_root_caller_drop(
    function: &MirFunction,
    instruction: &MirInstruction,
    root: &MirValueId,
) -> bool {
    let Some((None, Some(receipt))) = map_root_producer_caller_seal_snapshots(function) else {
        return false;
    };
    let Some(_seal) = function.ownership.checker_managed_any.as_ref() else {
        return false;
    };
    let Ok(expected_root) = MirValueId::new(format!("local:{}", receipt.caller_root_local.0 .0))
    else {
        return false;
    };
    let Ok(expected_id) = MirInstructionId::new(format!(
        "inst:ordinary_map_owned_any_v2_drop:{expected_root}:{}",
        receipt.root_drop_point.0
    )) else {
        return false;
    };
    instruction.id == expected_id
        && matches!(
            &instruction.kind,
            MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { root: actual_root }
                if actual_root == &expected_root
        )
        && root == &expected_root
        && function
            .values
            .get(root)
            .is_some_and(|value| value.ty == super::types::ordinary_map_owned_any_v2_type_id())
        && sealed_managed_any_instruction_origin(
            function,
            &instruction.id,
            &receipt.root_drop_point,
        )
}

fn managed_any_values_for_function(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    static_closure_values: &BTreeSet<MirValueId>,
) -> BTreeSet<MirValueId> {
    let mut managed_values = function
        .values
        .values()
        .filter(|value| {
            !static_closure_values.contains(&value.id)
                && catalog.contains_dynamic_or_managed_any(&value.ty)
        })
        .map(|value| value.id.clone())
        .collect::<BTreeSet<_>>();
    // Keys returns an owned List<String> even though its TypeDesc does not
    // contain ManagedAny. Track only the exact expression result and direct
    // binding named by the Checker KeysSnapshot receipt so ordinary owner
    // flow proves that this independent snapshot is settled exactly once.
    for instruction in function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
    {
        let MirInstructionKind::OrdinaryMapOwnedAnyV2Keys { result, root } = &instruction.kind
        else {
            continue;
        };
        let Some(cfg) = function
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
        else {
            continue;
        };
        let matching_dynamic = cfg
            .dynamic_keys_get_receipts
            .iter()
            .filter(|receipt| {
                MirValueId::new(format!("expr:{}", receipt.keys_action.point.0))
                    .is_ok_and(|expected| &expected == result)
            })
            .collect::<Vec<_>>();
        if let [receipt] = matching_dynamic.as_slice() {
            let snapshot_type = &receipt.keys_snapshot.snapshot_type;
            if has_checker_ordinary_map_owned_any_v2_dynamic_keys_snapshot_operation(
                function,
                instruction,
                result,
                root,
                false,
                cfg,
            ) && catalog.validate_owned_string_list(snapshot_type).is_ok()
            {
                if function
                    .values
                    .get(result)
                    .is_some_and(|value| &value.ty == snapshot_type)
                {
                    managed_values.insert(result.clone());
                }
                if let Ok(snapshot_local) = MirValueId::new(format!(
                    "local:{}",
                    receipt.keys_snapshot.snapshot_local.0 .0
                )) {
                    if function
                        .values
                        .get(&snapshot_local)
                        .is_some_and(|value| &value.ty == snapshot_type)
                    {
                        managed_values.insert(snapshot_local);
                    }
                }
            }
            continue;
        }
        if !matching_dynamic.is_empty() {
            continue;
        }
        let matching = cfg
            .keys_snapshot_receipts
            .iter()
            .filter(|receipt| {
                MirValueId::new(format!("expr:{}", receipt.keys_operation.0))
                    .is_ok_and(|expected| &expected == result)
            })
            .collect::<Vec<_>>();
        let [receipt] = matching.as_slice() else {
            continue;
        };
        if has_checker_ordinary_map_owned_any_v2_keys_snapshot_operation(
            function,
            instruction,
            result,
            root,
            false,
        ) && catalog
            .validate_owned_string_list(&receipt.snapshot_type)
            .is_ok()
        {
            if function
                .values
                .get(result)
                .is_some_and(|value| value.ty == receipt.snapshot_type)
            {
                managed_values.insert(result.clone());
            }
            if let Ok(snapshot_local) =
                MirValueId::new(format!("local:{}", receipt.snapshot_local.0 .0))
            {
                if function
                    .values
                    .get(&snapshot_local)
                    .is_some_and(|value| value.ty == receipt.snapshot_type)
                {
                    managed_values.insert(snapshot_local);
                }
            }
        }
    }
    // A typed AnyOwner-v2 unpack can produce a move-only concrete payload
    // whose TypeDesc does not itself contain DynamicAny (for example the
    // canonical `Label { text: string }` ABI). Keep that exact Checker-sealed
    // result in the ownership ledger so a later field move cannot bypass the
    // aggregate owner's settlement checks. Existing DynamicAny-containing
    // values are already covered by the catalog predicate above.
    for instruction in function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
    {
        let MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack {
            result,
            pair,
            projection,
        } = &instruction.kind
        else {
            continue;
        };
        let Some(value) = function.values.get(result) else {
            continue;
        };
        let Some(descriptor) = catalog.get(&value.ty) else {
            continue;
        };
        if descriptor.ownership != MirOwnership::Copy
            && matches!(&descriptor.layout, MirLayout::Record { .. })
            && !catalog.contains_dynamic_or_managed_any(&value.ty)
            && has_checker_ordinary_map_owned_any_v2_unpack(
                function,
                &instruction.id,
                result,
                pair,
                projection,
            )
        {
            managed_values.insert(result.clone());
        }
    }
    for instruction in function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
    {
        let MirInstructionKind::OrdinaryMapOwnedAnyV2ValuesUnpack {
            result,
            values,
            index,
            projection,
        } = &instruction.kind
        else {
            continue;
        };
        let Some(value) = function.values.get(result) else {
            continue;
        };
        let Some(descriptor) = catalog.get(&value.ty) else {
            continue;
        };
        if descriptor.ownership != MirOwnership::Copy
            && matches!(&descriptor.layout, MirLayout::Record { .. })
            && !catalog.contains_dynamic_or_managed_any(&value.ty)
            && has_checker_ordinary_map_owned_any_v2_values_unpack(
                function,
                &instruction.id,
                result,
                values,
                index,
                projection,
            )
        {
            managed_values.insert(result.clone());
        }
    }
    // MIR materializes a let-bound cast as an exact Move from the typed cast
    // result into its local. Carry the receipt-derived aggregate owner through
    // those same-type owner transfers so the later field projection/Drop is
    // checked against the live binding, not the already-consumed expression.
    loop {
        let transferred_records = function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| {
                let (source, result) = match &instruction.kind {
                    MirInstructionKind::Move { source, result }
                    | MirInstructionKind::Clone { source, result } => (source, result),
                    _ => return None,
                };
                if !managed_values.contains(source) {
                    return None;
                }
                let source_type = function.values.get(source)?.ty.clone();
                let result_value = function.values.get(result)?;
                let result_type = &result_value.ty;
                let descriptor = catalog.get(result_type)?;
                (source_type == *result_type
                    && descriptor.ownership != MirOwnership::Copy
                    && matches!(&descriptor.layout, MirLayout::Record { .. }))
                .then(|| result.clone())
            })
            .collect::<Vec<_>>();
        let mut changed = false;
        for result in transferred_records {
            changed |= managed_values.insert(result);
        }
        if !changed {
            break;
        }
    }
    managed_values
}

fn sealed_branch_context(
    function: &MirFunction,
) -> Option<(
    &MirCheckerManagedAnySeal,
    &crate::core::OrdinaryMapOwnedAnyV2CfgReceipt,
    &crate::core::ManagedAnyCfgReceipt,
)> {
    let seal = function.ownership.checker_managed_any.as_ref()?;
    let checker_cfg = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()?;
    let managed_cfg = function
        .ownership
        .checker_managed_any_cfg_receipt
        .as_ref()?;
    (seal.owner == function.owner
        && managed_cfg.owner == function.owner
        && seal.source_cfg.as_ref() == Some(managed_cfg))
    .then_some((seal, checker_cfg, managed_cfg))
}

fn is_computed_string_key_result_branch_cfg(
    function: &MirFunction,
    receipt: &crate::core::ManagedAnyCfgReceipt,
) -> bool {
    function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
        .is_some_and(|cfg| {
            let mut branches = cfg.result_receipts.iter().filter_map(|result| {
                result
                    .computed_string_key_owner_chain
                    .as_deref()?
                    .result_branch
                    .as_deref()
            });
            let Some(branch) = branches.next() else {
                return false;
            };
            branches.next().is_none() && branch.owner == function.owner && &branch.cfg == receipt
        })
}

fn sealed_values_local_index_i32_result_branch(
    function: &MirFunction,
) -> Option<&crate::core::ownership::OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultBranchReceipt> {
    let seal = function.ownership.checker_managed_any.as_ref()?;
    let branch = seal.v2_values_local_index_i32_result_branch.as_ref()?;
    let checker_cfg = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()?;
    let managed_cfg = function
        .ownership
        .checker_managed_any_cfg_receipt
        .as_ref()?;
    (seal.owner == function.owner
        && branch.owner == function.owner
        && checker_cfg
            .values_local_index_i32_result_branch_receipt
            .as_ref()
            == Some(branch)
        && managed_cfg.owner == function.owner
        && managed_cfg.reachable_blocks.len() == 7
        && managed_cfg.reachable_edges.len() == 8
        && seal.source_cfg.as_ref() == Some(managed_cfg))
    .then_some(branch)
}

fn seal_checked_lowering_with_snapshots(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    origins: &BTreeMap<MirInstructionId, NodeId>,
    source_cfg: Option<&crate::core::ManagedAnyCfgReceipt>,
    ordinary_map_owned_get_any_tuple_result_v1_snapshot: Option<
        crate::core::ownership::OrdinaryMapOwnedGetAnyTupleResultV1Receipt,
    >,
    branch_local_set_diamond_v2_snapshot: Option<Box<crate::core::OrdinaryMapOwnedAnyV2CfgReceipt>>,
    v2_scalar_tail: Option<crate::core::ownership::OrdinaryMapOwnedAnyV2ScalarTailReceipt>,
    v2_println: Vec<crate::core::OrdinaryMapOwnedAnyV2ResultReceipt>,
    v2_keys_snapshot_receipts: Vec<crate::core::OrdinaryMapOwnedAnyV2KeysSnapshotReceipt>,
    v2_dynamic_keys_get_receipts: Vec<crate::core::OrdinaryMapOwnedAnyV2DynamicKeysGetReceipt>,
    v2_values_local_index_i32_result_branch: Option<
        crate::core::ownership::OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultBranchReceipt,
    >,
    v2_values_producer_caller: Option<
        crate::core::OrdinaryMapOwnedAnyV2ValuesProducerCallerReceipt,
    >,
    v2_values_crosscall_string_caller: Option<
        crate::core::ownership::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerReceipt,
    >,
    v2_values_result_producer_caller: Option<
        crate::core::ownership::OrdinaryMapOwnedAnyV2ValuesResultProducerCallerReceipt,
    >,
    v2_map_root_producer_return: Option<
        crate::core::OrdinaryMapOwnedAnyV2MapRootProducerReturnReceipt,
    >,
    v2_map_root_producer_caller: Option<
        crate::core::OrdinaryMapOwnedAnyV2MapRootProducerCallerReceipt,
    >,
    v2_map_root_get_any_caller: Option<
        crate::core::OrdinaryMapOwnedAnyV2MapRootGetAnyCallerReceipt,
    >,
) -> Option<Box<MirCheckerManagedAnySeal>> {
    // ClosureBind creates a static code identity. Its function signature may
    // recursively mention Any, but it is not a payload owner; the separate
    // whole-program closure receipt validates this exact value before any
    // consumer can execute or emit it.
    let static_closure_values = function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::ClosureBind { function_value, .. } => Some(function_value.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let managed_values = managed_any_values_for_function(function, catalog, &static_closure_values);
    if managed_values.is_empty()
        && v2_map_root_producer_return.is_none()
        && v2_values_local_index_i32_result_branch.is_none()
    {
        return None;
    }
    Some(Box::new(MirCheckerManagedAnySeal {
        v2_scalar_tail,
        owner: function.owner.clone(),
        value_types: function
            .values
            .iter()
            .map(|(id, value)| (id.clone(), value.ty.clone()))
            .collect(),
        parameters: function.parameters.clone(),
        parameter_permissions: function.parameter_permissions.clone(),
        result: function.result.clone(),
        managed_values: managed_values.clone(),
        block_parameters: function
            .blocks
            .values()
            .map(|block| (block.id.clone(), block.parameters.clone()))
            .collect(),
        instructions: function
            .blocks
            .values()
            .flat_map(|block| {
                block.instructions.iter().map(move |instruction| {
                    (
                        block.id.clone(),
                        origins
                            .get(&instruction.id)
                            .cloned()
                            .unwrap_or_else(|| function.owner.clone()),
                        instruction.clone(),
                    )
                })
            })
            .collect(),
        terminators: function
            .blocks
            .values()
            .map(|block| (block.id.clone(), block.terminator.clone()))
            .collect(),
        source_cfg: source_cfg.cloned(),
        ordinary_map_owned_get_any_tuple_result_v1_snapshot,
        branch_local_set_diamond_v2_snapshot,
        v2_println,
        v2_keys_snapshot_receipts,
        v2_dynamic_keys_get_receipts,
        v2_values_local_index_i32_result_branch,
        v2_values_producer_caller,
        v2_values_crosscall_string_caller,
        v2_values_result_producer_caller,
        v2_map_root_producer_return,
        v2_map_root_producer_caller,
        v2_map_root_get_any_caller,
        generic_identity: None,
        generic_trap_cleanup: None,
    }))
}

/// Capture Checker authority exactly once, while lowering the original
/// checked body. Later rewrites must use `refresh_checked_lowering` so they
/// cannot recapture operation authority from mutable MIR ownership fields.
pub(super) fn seal_initial_checked_lowering(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    origins: &BTreeMap<MirInstructionId, NodeId>,
) -> Option<Box<MirCheckerManagedAnySeal>> {
    let source_cfg = function.ownership.checker_managed_any_cfg_receipt.clone();
    let ordinary_map_owned_get_any_tuple_result_v1_snapshot = function
        .ownership
        .checker_ordinary_map_owned_get_any_tuple_result_v1_receipt
        .clone();
    let branch_local_set_diamond_v2_snapshot = branch_local_set_diamond_v2_snapshot(function);
    let v2_scalar_tail = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
        .and_then(|receipt| receipt.settled_scalar_tail().cloned());
    let v2_println = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .iter()
        .flat_map(|cfg| cfg.result_receipts.iter())
        .filter(|receipt| receipt.payload_println.is_some())
        .cloned()
        .collect();
    let v2_keys_snapshot_receipts = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
        .map(|receipt| receipt.keys_snapshot_receipts.clone())
        .unwrap_or_default();
    let v2_dynamic_keys_get_receipts = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
        .map(|receipt| receipt.dynamic_keys_get_receipts.clone())
        .unwrap_or_default();
    let v2_values_local_index_i32_result_branch = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
        .and_then(|receipt| receipt.values_local_index_i32_result_branch_receipt.clone());
    let v2_values_producer_caller = function
        .ownership
        .checker_ordinary_map_owned_any_v2_values_caller_receipt
        .as_deref()
        .cloned();
    let v2_values_crosscall_string_caller = values_crosscall_string_caller_snapshot(function)?;
    let v2_values_result_producer_caller = values_result_producer_caller_snapshot(function)?;
    let (v2_map_root_producer_return, v2_map_root_producer_caller, v2_map_root_get_any_caller) =
        map_root_boundary_seal_snapshots(function)?;
    seal_checked_lowering_with_snapshots(
        function,
        catalog,
        origins,
        source_cfg.as_ref(),
        ordinary_map_owned_get_any_tuple_result_v1_snapshot,
        branch_local_set_diamond_v2_snapshot,
        v2_scalar_tail,
        v2_println,
        v2_keys_snapshot_receipts,
        v2_dynamic_keys_get_receipts,
        v2_values_local_index_i32_result_branch,
        v2_values_producer_caller,
        v2_values_crosscall_string_caller,
        v2_values_result_producer_caller,
        v2_map_root_producer_return,
        v2_map_root_producer_caller,
        v2_map_root_get_any_caller,
    )
}

/// Refresh a body seal after a mechanical MIR rewrite. Every Checker snapshot
/// is copied from the immutable prior seal; the current ownership receipts
/// are never consulted to grant new authority.
pub(super) fn refresh_checked_lowering(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    origins: &BTreeMap<MirInstructionId, NodeId>,
    previous: &MirCheckerManagedAnySeal,
) -> Option<Box<MirCheckerManagedAnySeal>> {
    if (previous.v2_map_root_producer_return.is_some()
        || previous.v2_map_root_producer_caller.is_some()
        || previous.v2_map_root_get_any_caller.is_some())
        && !map_root_profile_body_matches_previous_seal(function, origins, previous)
    {
        return None;
    }
    if function.ownership.checker_managed_any_cfg_receipt != previous.source_cfg {
        return None;
    }
    if function
        .ownership
        .checker_ordinary_map_owned_get_any_tuple_result_v1_receipt
        != previous.ordinary_map_owned_get_any_tuple_result_v1_snapshot
    {
        return None;
    }
    if function
        .ownership
        .checker_ordinary_map_owned_any_v2_values_caller_receipt
        .as_deref()
        != previous.v2_values_producer_caller.as_ref()
    {
        return None;
    }
    let Some(current_values_crosscall_string_caller) =
        values_crosscall_string_caller_snapshot(function)
    else {
        return None;
    };
    if current_values_crosscall_string_caller != previous.v2_values_crosscall_string_caller {
        return None;
    }
    let Some(current_values_result_producer_caller) =
        values_result_producer_caller_snapshot(function)
    else {
        return None;
    };
    if current_values_result_producer_caller != previous.v2_values_result_producer_caller {
        return None;
    }
    let current_keys_snapshot_receipts = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
        .map(|receipt| receipt.keys_snapshot_receipts.as_slice())
        .unwrap_or_default();
    if current_keys_snapshot_receipts != previous.v2_keys_snapshot_receipts.as_slice() {
        return None;
    }
    let current_dynamic_keys_get_receipts = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
        .map(|receipt| receipt.dynamic_keys_get_receipts.as_slice())
        .unwrap_or_default();
    if current_dynamic_keys_get_receipts != previous.v2_dynamic_keys_get_receipts.as_slice() {
        return None;
    }
    let current_values_local_index_i32_result_branch = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
        .and_then(|receipt| {
            receipt
                .values_local_index_i32_result_branch_receipt
                .as_ref()
        });
    if current_values_local_index_i32_result_branch
        != previous.v2_values_local_index_i32_result_branch.as_ref()
    {
        return None;
    }
    let (map_root_producer_return, map_root_producer_caller, map_root_get_any_caller) =
        map_root_boundary_seal_snapshots(function)?;
    if map_root_producer_return != previous.v2_map_root_producer_return
        || map_root_producer_caller != previous.v2_map_root_producer_caller
        || map_root_get_any_caller != previous.v2_map_root_get_any_caller
    {
        return None;
    }
    seal_checked_lowering_with_snapshots(
        function,
        catalog,
        origins,
        previous.source_cfg.as_ref(),
        previous
            .ordinary_map_owned_get_any_tuple_result_v1_snapshot
            .clone(),
        previous.branch_local_set_diamond_v2_snapshot.clone(),
        previous.v2_scalar_tail.clone(),
        previous.v2_println.clone(),
        previous.v2_keys_snapshot_receipts.clone(),
        previous.v2_dynamic_keys_get_receipts.clone(),
        previous.v2_values_local_index_i32_result_branch.clone(),
        previous.v2_values_producer_caller.clone(),
        previous.v2_values_crosscall_string_caller.clone(),
        previous.v2_values_result_producer_caller.clone(),
        previous.v2_map_root_producer_return.clone(),
        previous.v2_map_root_producer_caller.clone(),
        previous.v2_map_root_get_any_caller.clone(),
    )
}

/// This bridge has no post-lowering rewrite. Re-sealing a changed body would
/// let a new ordinary call reuse a Checker expression origin (for example, a
/// String literal's node) and acquire fresh ManagedAny authority. Preserve
/// the complete original body, block topology, result ABI, and origin table
/// whenever either half of the Map-root bridge is present.
fn map_root_profile_body_matches_previous_seal(
    function: &MirFunction,
    origins: &BTreeMap<MirInstructionId, NodeId>,
    previous: &MirCheckerManagedAnySeal,
) -> bool {
    let observed_instructions = function
        .blocks
        .values()
        .flat_map(|block| {
            block
                .instructions
                .iter()
                .map(move |instruction| (block.id.clone(), instruction.clone()))
        })
        .collect::<Vec<_>>();
    let sealed_instructions = previous
        .instructions
        .iter()
        .map(|(block, _, instruction)| (block.clone(), instruction.clone()))
        .collect::<Vec<_>>();
    function.owner == previous.owner
        && function.parameters == previous.parameters
        && function.parameter_permissions == previous.parameter_permissions
        && function.result == previous.result
        && observed_instructions == sealed_instructions
        && function
            .blocks
            .values()
            .map(|block| (block.id.clone(), block.parameters.clone()))
            .collect::<Vec<_>>()
            == previous.block_parameters
        && function
            .blocks
            .values()
            .map(|block| (block.id.clone(), block.terminator.clone()))
            .collect::<Vec<_>>()
            == previous.terminators
        && function
            .values
            .iter()
            .map(|(id, value)| (id.clone(), value.ty.clone()))
            .collect::<BTreeMap<_, _>>()
            == previous.value_types
        && origins.len() == previous.instructions.len()
        && previous
            .instructions
            .iter()
            .all(|(_, origin, instruction)| origins.get(&instruction.id) == Some(origin))
}

/// Snapshot the Map-root cross-function authority only when the producer and
/// caller halves are present as one exact Checker pair. A caller-local copy
/// is independent from, and may not conflict with, a producer CFG copy.
fn map_root_producer_caller_seal_snapshots(
    function: &MirFunction,
) -> Option<(
    Option<crate::core::OrdinaryMapOwnedAnyV2MapRootProducerReturnReceipt>,
    Option<crate::core::OrdinaryMapOwnedAnyV2MapRootProducerCallerReceipt>,
)> {
    use crate::core::ownership::{
        OrdinaryMapOwnedAnyV2MapRootCallerProfileVersion as CallerVersion,
        OrdinaryMapOwnedAnyV2MapRootProducerProfileVersion as ProducerVersion,
    };

    let cfg = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref();
    let cfg_return = cfg.and_then(|receipt| receipt.map_root_producer_return_receipt.as_ref());
    let cfg_caller = cfg.and_then(|receipt| receipt.map_root_producer_caller_receipt.as_ref());
    if cfg_return.is_some() != cfg_caller.is_some() {
        return None;
    }
    let cfg_pair = match (cfg_return, cfg_caller) {
        (Some(producer), Some(caller)) => {
            if cfg.is_none_or(|cfg| {
                cfg.owner != producer.owner
                    || cfg.profile_version != crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
            }) || producer.profile_version != ProducerVersion::V1
                || caller.profile_version != CallerVersion::V1
                || producer.owner != function.owner
                || caller.producer_owner != producer.owner
                || caller.producer_map_set_operation != producer.map_set_operation
                || caller.producer_return_value != producer.return_value
                || caller.producer_final_local != producer.final_root_local
                || caller.producer_final_binding_point != producer.final_root_binding_point
                || caller.logical_root != producer.final_root
                || caller.caller_owner == producer.owner
                || caller.call_target != producer.owner
                || caller.result_binding != caller.caller_root_local
                || caller.result_type != producer.final_root_type
                || caller.caller_root_type != producer.final_root_type
            {
                return None;
            }
            Some((producer.clone(), caller.clone()))
        }
        (None, None) => None,
        _ => unreachable!("partial receipt pairs are rejected above"),
    };
    let direct_caller = function
        .ownership
        .checker_ordinary_map_owned_any_v2_map_root_caller_receipt
        .as_deref();
    if direct_caller.is_some_and(|caller| {
        caller.profile_version != CallerVersion::V1
            || caller.caller_owner != function.owner
            || caller.call_target != caller.producer_owner
            || caller.result_binding != caller.caller_root_local
            || caller.result_type != caller.caller_root_type
            || caller.caller_return_type != function.result
    }) || (cfg_pair.is_some() && direct_caller.is_some())
    {
        return None;
    }
    let (producer_return, producer_caller) = match cfg_pair {
        Some((producer, caller)) => (Some(producer), Some(caller)),
        None => (None, direct_caller.cloned()),
    };
    Some((producer_return, producer_caller))
}

/// Snapshot the distinct Map-root → GetAny Checker bridge. The receipt lives
/// on the producer's CFG and is admitted only with its matching producer
/// return; it does not borrow the older Map-size caller schema.
fn map_root_get_any_producer_caller_seal_snapshot(
    function: &MirFunction,
) -> Option<Option<crate::core::OrdinaryMapOwnedAnyV2MapRootGetAnyCallerReceipt>> {
    use crate::core::ownership::OrdinaryMapOwnedAnyV2MapRootGetAnyCallerProfileVersion as CallerVersion;

    let Some(cfg) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
    else {
        return Some(None);
    };
    let Some(caller) = cfg.map_root_get_any_caller_receipt.as_ref() else {
        return Some(None);
    };
    let producer = cfg.map_root_producer_return_receipt.as_ref()?;
    let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
    let scalar_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    // These Checker receipt types are source identities: the root is a
    // surface `Record` here. Its canonical V2 handle identity is introduced
    // by MIR lowering and is checked by the route and body validators below.
    if cfg.owner != function.owner
        || cfg.profile_version != crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
        || producer.profile_version
            != crate::core::ownership::OrdinaryMapOwnedAnyV2MapRootProducerProfileVersion::V1
        || producer.owner != function.owner
        || caller.profile_version != CallerVersion::V1
        || caller.producer_owner != function.owner
        || caller.producer_map_set_operation != producer.map_set_operation
        || caller.producer_return_value != producer.return_value
        || caller.producer_final_local != producer.final_root_local
        || caller.producer_final_binding_point != producer.final_root_binding_point
        || caller.logical_root != producer.final_root
        || caller.caller_owner == function.owner
        || caller.call_target != function.owner
        || caller.caller_root_type != producer.final_root_type
        || caller.string_type != string_type
        || caller.caller_return_type != scalar_type
        || caller.caller_return_literal != crate::core::ir::ResolvedLiteral::Int(0)
        || cfg.map_root_producer_caller_receipt.is_some()
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_map_root_get_any_caller_receipt
            .is_some()
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_map_root_caller_receipt
            .is_some()
    {
        return None;
    }
    Some(Some(caller.clone()))
}

/// Snapshot the caller-local mirror without conflating it with the producer
/// CFG copy. A function may carry one role only; the whole-program receipt
/// validator joins these two independent Checker locations.
fn map_root_get_any_direct_caller_seal_snapshot(
    function: &MirFunction,
) -> Option<Option<crate::core::OrdinaryMapOwnedAnyV2MapRootGetAnyCallerReceipt>> {
    use crate::core::ownership::OrdinaryMapOwnedAnyV2MapRootGetAnyCallerProfileVersion as CallerVersion;

    let Some(caller) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_map_root_get_any_caller_receipt
        .as_deref()
    else {
        return Some(None);
    };
    let cfg = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()?;
    // Keep the checker-owned root type in its source domain. The verifier
    // later joins this caller snapshot to the producer's Record receipt and
    // validates the corresponding canonical V2 handle in MIR.
    let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
    let scalar_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    if cfg.owner != function.owner
        || cfg.profile_version != crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
        || cfg.map_root_get_any_caller_receipt.is_some()
        || cfg.map_root_producer_return_receipt.is_some()
        || cfg.map_root_producer_caller_receipt.is_some()
        || caller.profile_version != CallerVersion::V1
        || caller.caller_owner != function.owner
        || caller.producer_owner != caller.call_target
        || caller.producer_owner == function.owner
        || caller.string_type != string_type
        || caller.caller_return_type != scalar_type
        || caller.caller_return_type != function.result
        || caller.caller_return_literal != crate::core::ir::ResolvedLiteral::Int(0)
        || function.parameters.len() != 0
        || !function
            .ownership
            .checker_callable_generic_parameters
            .is_empty()
        || function
            .ownership
            .checker_ordinary_map_owned_any_v2_map_root_caller_receipt
            .is_some()
    {
        return None;
    }
    Some(Some(caller.clone()))
}

/// Normalize the Map-root bridge snapshots while preserving the producer CFG
/// GetAny receipt and caller-local GetAny receipt as separate authorities.
fn map_root_boundary_seal_snapshots(
    function: &MirFunction,
) -> Option<(
    Option<crate::core::OrdinaryMapOwnedAnyV2MapRootProducerReturnReceipt>,
    Option<crate::core::OrdinaryMapOwnedAnyV2MapRootProducerCallerReceipt>,
    Option<crate::core::OrdinaryMapOwnedAnyV2MapRootGetAnyCallerReceipt>,
)> {
    let legacy = map_root_producer_caller_seal_snapshots(function);
    let producer_get_any = map_root_get_any_producer_caller_seal_snapshot(function)?;
    let direct_get_any = map_root_get_any_direct_caller_seal_snapshot(function)?;
    match (legacy, producer_get_any, direct_get_any) {
        (Some((producer, caller)), None, None) => Some((producer, caller, None)),
        (Some((None, None)), None, Some(caller)) => Some((None, None, Some(caller))),
        (None, Some(caller), None) => {
            let producer = function
                .ownership
                .checker_ordinary_map_owned_any_v2_cfg_receipt
                .as_ref()?
                .map_root_producer_return_receipt
                .as_ref()?
                .clone();
            Some((Some(producer), None, Some(caller)))
        }
        // No body may combine independently sourced GetAny copies, or mix
        // either GetAny role with the older Map-size bridge.
        _ => None,
    }
}

/// Capture the exact Checker-owned branch-local Set diamond receipt for the
/// initial body seal. Callers that refresh a seal must pass through the
/// already captured snapshot instead of calling this helper again.
fn branch_local_set_diamond_v2_snapshot(
    function: &MirFunction,
) -> Option<Box<crate::core::OrdinaryMapOwnedAnyV2CfgReceipt>> {
    let receipt = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_deref()?;
    matches!(
        receipt
            .flow
            .as_ref()
            .map(|flow| &flow.proof),
        Some(
            crate::core::ownership::OrdinaryMapOwnedAnyV2CfgFlowProof::BranchLocalPersistentSetDiamondV1(
                _
            )
        )
    )
    .then(|| Box::new(receipt.clone()))
}

/// Resolve a source block in the branch-specific hand-built Result MIR to the
/// same canonical block ID used by generic ManagedAny Checker replay.
pub(super) fn values_local_index_result_branch_mir_block_id(
    receipt: &crate::core::ManagedAnyCfgReceipt,
    source_id: &crate::core::cfg::BasicBlockId,
) -> Result<MirBlockId, String> {
    let source_block = receipt
        .reachable_blocks
        .get(source_id)
        .ok_or("ValuesLocalIndexI32 Result source block is absent")?;
    let role = source_id
        .0
         .0
        .rsplit_once("/cfg:block:")
        .map(|(_, role)| role)
        .ok_or("ValuesLocalIndexI32 Result source block has no canonical role")?;
    if role == "entry" {
        return MirBlockId::new(source_id.0 .0.clone()).map_err(|error| error.to_string());
    }
    let node = if role == "match-arm" {
        receipt
            .reachable_edges
            .values()
            .filter(|edge| {
                &edge.to == source_id && edge.kind == crate::core::cfg::EdgeKind::MatchArm
            })
            .map(|edge| &edge.source.node)
            .next()
            .ok_or("ValuesLocalIndexI32 Result match arm has no Checker selector")?
    } else {
        &source_block.source.node
    };
    MirBlockId::new(format!("bb:{}:{}", role.replace('-', "."), node.0))
        .map_err(|error| error.to_string())
}

pub(super) fn values_local_index_result_branch_mir_edge_id(
    receipt: &crate::core::ManagedAnyCfgReceipt,
    source_id: &crate::core::cfg::EdgeId,
) -> Result<super::MirEdgeId, String> {
    let edge = receipt
        .reachable_edges
        .get(source_id)
        .ok_or("ValuesLocalIndexI32 Result source edge is absent")?;
    let encoded_role = edge
        .id
        .0
         .0
        .rsplit_once("/cfg:edge:")
        .map(|(_, role)| role)
        .ok_or("ValuesLocalIndexI32 Result source edge has no canonical role")?;
    let (mir_role, mir_node) = match encoded_role {
        "if-then" | "if-else" => {
            let (if_prefix, mir_node) = managed_any_if_mir_anchor(receipt, &edge.source.node)?;
            let arm_role = if encoded_role == "if-then" {
                "then"
            } else {
                "else"
            };
            (format!("{if_prefix}.{arm_role}"), mir_node)
        }
        "if-then-join" | "if-else-join" => {
            let target = receipt
                .reachable_blocks
                .get(&edge.to)
                .ok_or("ManagedAny source-CFG if join edge targets an absent block")?;
            let (if_prefix, mir_node) = managed_any_if_mir_anchor(receipt, &target.source.node)?;
            let arm_role = if encoded_role == "if-then-join" {
                "then"
            } else {
                "else"
            };
            (format!("{if_prefix}.{arm_role}.join"), mir_node)
        }
        _ => {
            let edge_node = if encoded_role.contains("match-arm-join") {
                edge.source.node.clone()
            } else if encoded_role.contains("join") {
                receipt
                    .reachable_blocks
                    .get(&edge.to)
                    .ok_or("ManagedAny source-CFG join edge targets an absent block")?
                    .source
                    .node
                    .clone()
            } else {
                edge.source.node.clone()
            };
            (encoded_role.replace('-', "."), edge_node)
        }
    };
    super::MirEdgeId::new(format!("edge:{mir_role}:{}", mir_node.0))
        .map_err(|error| error.to_string())
}

pub(super) fn managed_any_if_mir_anchor(
    receipt: &crate::core::ManagedAnyCfgReceipt,
    source_node: &NodeId,
) -> Result<(&'static str, NodeId), String> {
    let join_id =
        crate::core::cfg::BasicBlockId(NodeId(format!("{}/cfg:block:if-join", source_node.0)));
    let join = receipt
        .reachable_blocks
        .get(&join_id)
        .ok_or("ManagedAny source-CFG if construct has no exact join block")?;
    if join.source.node != *source_node {
        return Err("ManagedAny source-CFG if join has a mismatched source anchor".into());
    }

    let expression_anchors = join
        .points
        .iter()
        .filter(|point| {
            point.source.node == *source_node
                && matches!(
                    point.kind,
                    crate::core::cfg::CfgPointKind::Expression
                        | crate::core::cfg::CfgPointKind::Condition
                )
        })
        .collect::<Vec<_>>();
    if expression_anchors.len() != 1 {
        return Err("ManagedAny source-CFG if join has no unique expression anchor".into());
    }

    let Some(statement_node) = source_node
        .0
        .strip_suffix("/control-expression")
        .map(|node| NodeId(node.to_string()))
    else {
        return Ok(("if", source_node.clone()));
    };
    let statement_anchors = join
        .points
        .iter()
        .enumerate()
        .filter(|(_, point)| {
            point.source.node == statement_node
                && point.kind == crate::core::cfg::CfgPointKind::Statement
        })
        .collect::<Vec<_>>();
    match statement_anchors.as_slice() {
        [] => Ok(("if", source_node.clone())),
        [(statement_index, _)] => {
            let expression_index = join
                .points
                .iter()
                .position(|point| {
                    point.source.node == *source_node
                        && matches!(
                            point.kind,
                            crate::core::cfg::CfgPointKind::Expression
                                | crate::core::cfg::CfgPointKind::Condition
                        )
                })
                .ok_or("ManagedAny source-CFG if expression anchor disappeared")?;
            if *statement_index <= expression_index {
                return Err(
                    "ManagedAny source-CFG statement-if anchor precedes its control expression"
                        .into(),
                );
            }
            Ok(("if.stmt", statement_node))
        }
        _ => Err("ManagedAny source-CFG if join has multiple statement anchors".into()),
    }
}

impl MirTypeCatalog {
    fn managed_any_value_type<'a>(
        function: &'a MirFunction,
        value: &MirValueId,
    ) -> Result<&'a ResolvedTypeId, String> {
        function
            .values
            .get(value)
            .map(|value| &value.ty)
            .ok_or_else(|| format!("ManagedAny MIR value '{}' is absent", value))
    }

    fn managed_any_is_string(&self, function: &MirFunction, value: &MirValueId) -> bool {
        function.values.get(value).is_some_and(|value| {
            value.ty == crate::core::ir::primitive_type_id(PrimitiveType::String)
        })
    }

    /// Prove that each ordinary owned String MIR identity is definitely live
    /// at every use in a ManagedAny-bearing callable. Free String values are
    /// not Checker-linear, so the resource ledger intentionally does not give
    /// them exactly-once semantics. They still have runtime ownership, though:
    /// reading an SSA identity after its `Drop` or `Move` is a use-after-free.
    ///
    /// This is a forward must-live analysis. At CFG joins it intersects the
    /// incoming owner sets, so a value is usable only when every path reaching
    /// the join preserves it. The finite set lattice also gives loops a real
    /// fixed point instead of treating a backedge as a second visit to a
    /// source-ordered block. Any-derived Strings continue to have the stricter
    /// path-local settlement checks below; this analysis supplements those
    /// obligations for all other exact String identities.
    fn validate_managed_any_string_liveness(
        &self,
        function: &MirFunction,
        successors: &BTreeMap<MirBlockId, Vec<ManagedAnyCfgSuccessor>>,
    ) -> Result<(), String> {
        let string_values = function
            .values
            .iter()
            .filter_map(|(id, value)| {
                (value.ty == crate::core::ir::primitive_type_id(PrimitiveType::String))
                    .then_some(id.clone())
            })
            .collect::<BTreeSet<_>>();
        if string_values.is_empty() {
            return Ok(());
        }

        let mut predecessors = BTreeMap::<MirBlockId, Vec<(MirBlockId, usize)>>::new();
        for (source, outgoing) in successors {
            for (index, edge) in outgoing.iter().enumerate() {
                predecessors
                    .entry(edge.target.clone())
                    .or_default()
                    .push((source.clone(), index));
            }
        }

        let initial = function
            .parameters
            .iter()
            .filter(|parameter| string_values.contains(*parameter))
            .cloned()
            .collect::<BTreeSet<_>>();
        let top = string_values.clone();
        let mut incoming = function
            .blocks
            .keys()
            .map(|block| (block.clone(), top.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut outgoing = incoming.clone();
        incoming.insert(function.entry.clone(), initial.clone());

        // Apply one instruction's String-owner effect. The non-diagnostic
        // pass computes the dataflow fixed point; the diagnostic replay below
        // checks every use and proves glue before committing the transfer.
        let transfer_instruction = |live: &mut BTreeSet<MirValueId>,
                                    instruction: &MirInstruction,
                                    check: bool|
         -> Result<(), String> {
            let uses = super::mir_instruction_value_uses(&instruction.kind);
            if check {
                for value in &uses {
                    if string_values.contains(value) && !live.contains(value) {
                        return Err(format!(
                            "ManagedAny String '{}' is not definitely live at instruction '{}' (it was dropped, moved, or is not yet created)",
                            value, instruction.id
                        ));
                    }
                }
            }

            let string_arguments = |arguments: &[MirValueId]| {
                arguments
                    .iter()
                    .filter(|value| string_values.contains(*value))
                    .cloned()
                    .collect::<Vec<_>>()
            };
            let consumed = match &instruction.kind {
                MirInstructionKind::Drop { value } => vec![value.clone()],
                MirInstructionKind::Move { source, .. } => vec![source.clone()],
                MirInstructionKind::Construct { fields, .. }
                | MirInstructionKind::ConstructList {
                    elements: fields, ..
                }
                | MirInstructionKind::ConstructSet {
                    elements: fields, ..
                } => fields.clone(),
                MirInstructionKind::ConstructVariant { fields, .. }
                | MirInstructionKind::ConstructVariantMove { fields, .. } => {
                    fields.iter().map(|(_, value)| value.clone()).collect()
                }
                MirInstructionKind::UpdateRecord { fields, .. } => fields.clone(),
                MirInstructionKind::Call { arguments, .. }
                | MirInstructionKind::ClosureInvoke { arguments, .. }
                | MirInstructionKind::FlowTransition { arguments, .. } => {
                    let arguments = string_arguments(arguments);
                    if check && !arguments.is_empty() {
                        return Err(format!(
                            "ManagedAny String call boundary '{}' has no Checker ownership-transfer summary",
                            instruction.id
                        ));
                    }
                    Vec::new()
                }
                MirInstructionKind::SessionCall { payload, .. } => {
                    let arguments = payload
                        .iter()
                        .filter(|value| string_values.contains(*value))
                        .cloned()
                        .collect::<Vec<_>>();
                    if check && !arguments.is_empty() {
                        return Err(format!(
                            "ManagedAny String SessionCall '{}' has no Checker ownership-transfer summary",
                            instruction.id
                        ));
                    }
                    Vec::new()
                }
                MirInstructionKind::Actor { operation } => {
                    let arguments = uses
                        .iter()
                        .filter(|value| string_values.contains(*value))
                        .cloned()
                        .collect::<Vec<_>>();
                    if check && !arguments.is_empty() {
                        return Err(format!(
                            "ManagedAny String Actor boundary '{}' has no Checker ownership-transfer summary",
                            instruction.id
                        ));
                    }
                    let _ = operation;
                    Vec::new()
                }
                MirInstructionKind::BuiltinCall {
                    kind, arguments, ..
                } => {
                    let arguments = string_arguments(arguments);
                    if check
                        && !arguments.is_empty()
                        && *kind != super::types::MirBuiltinKind::PrintlnString
                    {
                        return Err(format!(
                            "ManagedAny String builtin boundary '{}' has no Checker ownership-transfer summary",
                            instruction.id
                        ));
                    }
                    Vec::new()
                }
                MirInstructionKind::ListOp { .. }
                | MirInstructionKind::SetOp { .. }
                | MirInstructionKind::VariantProjectOr { .. } => {
                    let arguments = uses
                        .iter()
                        .filter(|value| string_values.contains(*value))
                        .cloned()
                        .collect::<Vec<_>>();
                    if check && !arguments.is_empty() {
                        return Err(format!(
                            "ManagedAny String operation '{}' has no exact borrow/transfer receipt",
                            instruction.id
                        ));
                    }
                    Vec::new()
                }
                MirInstructionKind::Copy { source, .. } if string_values.contains(source) => {
                    return Err(format!(
                        "ManagedAny String '{}' cannot be copied by instruction '{}'",
                        source, instruction.id
                    ));
                }
                _ => Vec::new(),
            };

            let consumed_by_move = !matches!(&instruction.kind, MirInstructionKind::Drop { .. });
            let mut consumed_string_ids = BTreeSet::new();
            for value in consumed {
                if string_values.contains(&value) {
                    if check && !consumed_string_ids.insert(value.clone()) {
                        return Err(format!(
                            "ManagedAny String '{}' is transferred more than once by instruction '{}'",
                            value, instruction.id
                        ));
                    }
                    if check && consumed_by_move {
                        self.validate_glue(
                            Self::managed_any_value_type(function, &value)?,
                            MirGlueOperation::MoveOut,
                        )?;
                    }
                    live.remove(&value);
                }
            }

            if let MirInstructionKind::Drop { value } = &instruction.kind {
                if string_values.contains(value) && check {
                    self.validate_glue(
                        Self::managed_any_value_type(function, value)?,
                        MirGlueOperation::Drop,
                    )?;
                }
            }
            if let MirInstructionKind::Move { source, result } = &instruction.kind {
                if string_values.contains(source) && string_values.contains(result) && check {
                    self.validate_glue(
                        Self::managed_any_value_type(function, result)?,
                        MirGlueOperation::Drop,
                    )?;
                }
            }
            if let MirInstructionKind::Clone { source, result } = &instruction.kind {
                if string_values.contains(source) {
                    if check {
                        self.validate_glue(
                            Self::managed_any_value_type(function, source)?,
                            MirGlueOperation::Clone,
                        )?;
                    }
                    if string_values.contains(result) && check {
                        self.validate_glue(
                            Self::managed_any_value_type(function, result)?,
                            MirGlueOperation::Drop,
                        )?;
                    }
                }
            }

            for result in instruction_result_values(&instruction.kind) {
                if string_values.contains(result) {
                    if check {
                        if live.contains(result) {
                            return Err(format!(
                                "ManagedAny String '{}' is redefined while its previous owner is still live at instruction '{}'",
                                result, instruction.id
                            ));
                        }
                        self.validate_glue(
                            Self::managed_any_value_type(function, result)?,
                            MirGlueOperation::Drop,
                        )?;
                    }
                    live.insert(result.clone());
                }
            }
            Ok(())
        };

        let transfer_block = |block: &super::MirBlock,
                              input: &BTreeSet<MirValueId>,
                              check: bool|
         -> Result<BTreeSet<MirValueId>, String> {
            let mut live = input.clone();
            for instruction in &block.instructions {
                transfer_instruction(&mut live, instruction, check)?;
            }
            Ok(live)
        };

        let transfer_edge = |live: &BTreeSet<MirValueId>,
                             source: &MirBlockId,
                             edge: &ManagedAnyCfgSuccessor,
                             check: bool|
         -> Result<BTreeSet<MirValueId>, String> {
            let target = function
                .blocks
                .get(&edge.target)
                .ok_or("ManagedAny String liveness edge target is absent")?;
            if edge.arguments.len() != target.parameters.len() {
                return Err(format!(
                    "ManagedAny String liveness edge '{}' -> '{}' has inconsistent arity",
                    source, edge.target
                ));
            }
            let mut edge_live = live.clone();
            let mut transferred_string_arguments = BTreeSet::new();
            for (argument, parameter) in edge.arguments.iter().zip(&target.parameters) {
                let argument_is_string = string_values.contains(argument);
                let parameter_is_string = string_values.contains(&parameter.value);
                if argument_is_string != parameter_is_string {
                    return Err(format!(
                        "ManagedAny String liveness edge '{}' -> '{}' changes String TypeDesc identity",
                        source, edge.target
                    ));
                }
                if argument_is_string {
                    if check && !transferred_string_arguments.insert(argument.clone()) {
                        return Err(format!(
                            "ManagedAny String edge '{}' -> '{}' transfers the same SSA owner to multiple block parameters",
                            source, edge.target
                        ));
                    }
                    let was_live = edge_live.remove(argument);
                    if was_live {
                        if check {
                            self.validate_glue(
                                Self::managed_any_value_type(function, argument)?,
                                MirGlueOperation::MoveOut,
                            )?;
                        }
                        if check && edge_live.contains(&parameter.value) {
                            return Err(format!(
                                "ManagedAny String block parameter '{}' replaces an unsettled owner on edge '{}' -> '{}'",
                                parameter.value, source, edge.target
                            ));
                        }
                        edge_live.insert(parameter.value.clone());
                    }
                }
            }
            Ok(edge_live)
        };

        loop {
            let mut changed = false;
            for (block_id, block) in &function.blocks {
                let next_in = if block_id == &function.entry {
                    let mut joined = initial.clone();
                    for (source, index) in predecessors.get(block_id).into_iter().flatten() {
                        let edge = successors
                            .get(source)
                            .and_then(|edges| edges.get(*index))
                            .ok_or("ManagedAny String entry backedge is absent")?;
                        let path = transfer_edge(
                            outgoing
                                .get(source)
                                .ok_or("ManagedAny String entry backedge state is absent")?,
                            source,
                            edge,
                            false,
                        )?;
                        joined = joined.intersection(&path).cloned().collect();
                    }
                    joined
                } else {
                    let incoming_edges = predecessors
                        .get(block_id)
                        .ok_or("ManagedAny String CFG block has no predecessor")?;
                    if incoming_edges.is_empty() {
                        return Err(format!(
                            "ManagedAny String CFG block '{}' has no incoming path",
                            block_id
                        ));
                    }
                    let mut paths = incoming_edges.iter().map(|(source, index)| {
                        let edge = successors
                            .get(source)
                            .and_then(|edges| edges.get(*index))
                            .ok_or("ManagedAny String CFG predecessor edge is absent")?;
                        transfer_edge(
                            outgoing
                                .get(source)
                                .ok_or("ManagedAny String CFG predecessor state is absent")?,
                            source,
                            edge,
                            false,
                        )
                    });
                    let mut joined = paths
                        .next()
                        .ok_or("ManagedAny String CFG block has no incoming path")??;
                    for path in paths {
                        joined = joined
                            .intersection(&path?)
                            .cloned()
                            .collect::<BTreeSet<_>>();
                    }
                    joined
                };
                let next_out = transfer_block(block, &next_in, false)?;
                if incoming.get(block_id) != Some(&next_in) {
                    incoming.insert(block_id.clone(), next_in);
                    changed = true;
                }
                if outgoing.get(block_id) != Some(&next_out) {
                    outgoing.insert(block_id.clone(), next_out);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        // Replay the converged definite-live states and check every MIR use,
        // including values in terminators and edge arguments. `incoming` is
        // the intersection of all actual predecessor paths, so a branch-local
        // Drop or a loop backedge that loses the peer cannot be hidden by a
        // different predecessor.
        for (block_id, block) in &function.blocks {
            let mut live = incoming
                .get(block_id)
                .cloned()
                .ok_or("ManagedAny String CFG block has no converged live state")?;
            for instruction in &block.instructions {
                transfer_instruction(&mut live, instruction, true)?;
            }
            for value in super::mir_terminator_value_uses(&block.terminator) {
                if string_values.contains(&value) && !live.contains(&value) {
                    return Err(format!(
                        "ManagedAny String '{}' is not definitely live at the terminator of '{}' (it was dropped, moved, or is not yet created)",
                        value, block_id
                    ));
                }
            }
            if let MirTerminator::Return { value: Some(value) } = &block.terminator {
                if string_values.contains(value) {
                    self.validate_glue(
                        Self::managed_any_value_type(function, value)?,
                        MirGlueOperation::MoveOut,
                    )?;
                    live.remove(value);
                }
            }
            for edge in successors.get(block_id).into_iter().flatten() {
                let _ = transfer_edge(&live, block_id, edge, true)?;
            }
        }
        Ok(())
    }

    fn managed_any_is_copy_value(&self, function: &MirFunction, value: &MirValueId) -> bool {
        function.values.get(value).is_some_and(|value| {
            self.get(&value.ty).is_some_and(|descriptor| {
                descriptor.ownership == MirOwnership::Copy
                    && matches!(
                        &descriptor.kind,
                        MirTypeKind::Primitive(primitive)
                            if *primitive != PrimitiveType::String
                                && *primitive != PrimitiveType::Unit
                    )
            })
        })
    }

    fn validate_managed_any_branch_local_set_diamond_snapshot(
        &self,
        function: &MirFunction,
        seal: &MirCheckerManagedAnySeal,
    ) -> Result<(), String> {
        use crate::core::cfg::CfgPointKind;
        use crate::core::ownership::{
            OrdinaryMapOwnedAnyV2ActionKind as ActionKind, OrdinaryMapOwnedAnyV2Attempt as Attempt,
            OrdinaryMapOwnedAnyV2AttemptKind as AttemptKind,
            OrdinaryMapOwnedAnyV2CfgFlowProof as FlowProof,
            OrdinaryMapOwnedAnyV2ProfileVersion as Profile,
        };

        let current = function
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_ref();
        let current_is_branch_local_diamond = current
            .and_then(|receipt| receipt.flow.as_ref())
            .is_some_and(|flow| {
                matches!(&flow.proof, FlowProof::BranchLocalPersistentSetDiamondV1(_))
            });
        let Some(snapshot) = seal.branch_local_set_diamond_v2_snapshot.as_ref() else {
            return if current_is_branch_local_diamond {
                Err("branch-local Set diamond has no original private Checker commitment".into())
            } else {
                Ok(())
            };
        };
        if current != Some(snapshot) {
            return Err(
                "branch-local Set diamond Checker receipt was removed or changed after initial lowering"
                    .into(),
            );
        }
        if let Some(error) = super::validate_ordinary_map_owned_any_v2_action_receipts(function)
            .into_iter()
            .chain(super::validate_ordinary_map_owned_any_v2_cfg_flow_receipt(
                function,
            ))
            .next()
        {
            return Err(format!(
                "branch-local Set diamond MIR no longer satisfies its Checker proof: {}",
                error.message
            ));
        }
        if snapshot.owner != function.owner || snapshot.profile_version != Profile::V2 {
            return Err(
                "branch-local Set diamond private Checker commitment has a wrong owner or profile"
                    .into(),
            );
        }
        let flow = snapshot
            .flow
            .as_ref()
            .ok_or("branch-local Set diamond private Checker commitment has no flow proof")?;
        let FlowProof::BranchLocalPersistentSetDiamondV1(proof) = &flow.proof else {
            return Err(
                "branch-local Set diamond private Checker commitment has the wrong proof kind"
                    .into(),
            );
        };
        if proof.owner != function.owner
            || proof.entry_match_split != flow.entry
            || proof.split != flow.split
            || proof.outer_join != flow.join
            || flow.split == flow.entry
            || proof.arms.len() != 2
            || proof.result_branches.len() != 3
            || flow.arm_edges.len() != 2
            || proof.arms.keys().cloned().collect::<BTreeSet<_>>() != flow.arm_edges
            || snapshot.actions.len() != 11
            || snapshot.attempts.len() != 11
            || function.ownership.checker_ordinary_map_owned_any_v2.len() != 11
        {
            return Err(
                "branch-local Set diamond private Checker commitment has an incomplete owner, arm, or action census"
                    .into(),
            );
        }

        let no_other_v2_receipts = snapshot.computed_key_string_get_receipts.is_empty()
            && snapshot.result_receipts.is_empty()
            && snapshot.values_producer_return_receipt.is_none()
            && snapshot.found_observation_receipts.is_empty()
            && snapshot.unpack_receipts.is_empty()
            && snapshot.values_unpack_receipts.is_empty()
            && snapshot.values_snapshot_replacement_receipts.is_empty()
            && snapshot.values_mixed_snapshot_receipts.is_empty()
            && snapshot.values_mixed_snapshot_remove_receipts.is_empty()
            && snapshot.temporary_root_receipts.is_empty()
            && snapshot.empty_root_root_queries_v1.is_none()
            && snapshot.zero_miss_i32_observation_receipt.is_none();
        if !no_other_v2_receipts {
            return Err(
                "branch-local Set diamond private Checker commitment carries unrelated V2 receipts"
                    .into(),
            );
        }

        let mut expected_actions = vec![&proof.base_new];
        let mut expected_attempts = BTreeSet::new();
        expected_attempts.insert(Attempt {
            kind: AttemptKind::New,
            point: proof.base_new.point.clone(),
            arguments: Vec::new(),
            receiver_local: None,
            result_local: Some(proof.base_local.clone()),
        });
        let mut payload_loads = BTreeSet::new();
        let mut child_locals = BTreeSet::new();
        for (edge, arm) in &proof.arms {
            expected_actions.extend([
                &arm.set,
                &arm.size,
                &arm.drop,
                &arm.ok_base_drop,
                &arm.err_base_drop,
            ]);
            expected_attempts.insert(Attempt {
                kind: AttemptKind::Set,
                point: arm.set.point.clone(),
                arguments: vec![
                    arm.set
                        .source_node
                        .clone()
                        .ok_or("branch-local Set action has no source node")?,
                    arm.set
                        .key
                        .clone()
                        .ok_or("branch-local Set action has no key node")?,
                    arm.payload_load.clone(),
                ],
                receiver_local: Some(proof.base_local.clone()),
                result_local: Some(arm.set.local.clone()),
            });
            expected_attempts.insert(Attempt {
                kind: AttemptKind::Size,
                point: arm.size.point.clone(),
                arguments: vec![arm
                    .size
                    .source_node
                    .clone()
                    .ok_or("branch-local Size action has no source node")?],
                receiver_local: Some(arm.set.local.clone()),
                result_local: None,
            });
            expected_attempts.insert(Attempt {
                kind: AttemptKind::Drop,
                point: arm.drop.point.clone(),
                arguments: vec![arm.set.local.0.clone()],
                receiver_local: Some(arm.set.local.clone()),
                result_local: None,
            });
            expected_attempts.insert(Attempt {
                kind: AttemptKind::Drop,
                point: arm.ok_base_drop.point.clone(),
                arguments: vec![proof.base_local.0.clone()],
                receiver_local: Some(proof.base_local.clone()),
                result_local: None,
            });
            expected_attempts.insert(Attempt {
                kind: AttemptKind::Drop,
                point: arm.err_base_drop.point.clone(),
                arguments: vec![proof.base_local.0.clone()],
                receiver_local: Some(proof.base_local.clone()),
                result_local: None,
            });

            if edge != &arm.edge
                || arm.set.kind != ActionKind::Set
                || arm.size.kind != ActionKind::Size
                || arm.drop.kind != ActionKind::Drop
                || arm.ok_base_drop.kind != ActionKind::Drop
                || arm.err_base_drop.kind != ActionKind::Drop
                || arm.set.profile_version != Profile::V2
                || arm.size.profile_version != Profile::V2
                || arm.drop.profile_version != Profile::V2
                || arm.ok_base_drop.profile_version != Profile::V2
                || arm.err_base_drop.profile_version != Profile::V2
                || arm.set.source_local.as_ref() != Some(&proof.base_local)
                || arm.set.local == proof.base_local
                || arm.set.local == proof.payload_local
                || arm.set.value.as_ref() != Some(&arm.payload_load)
                || arm.set.value_type.as_ref() != Some(&proof.payload_type)
                || !arm.set.payload_clone
                || arm.size.local != arm.set.local
                || arm.drop.local != arm.set.local
                || arm.ok_base_drop.local != proof.base_local
                || arm.err_base_drop.local != proof.base_local
                || !payload_loads.insert(arm.payload_load.clone())
                || !child_locals.insert(arm.set.local.clone())
            {
                return Err(
                    "branch-local Set diamond private Checker arm does not bind one direct typed payload clone"
                        .into(),
                );
            }
            for (point, expected_kind) in [
                (&arm.payload_load, CfgPointKind::Expression),
                (&arm.set.point, CfgPointKind::Condition),
            ] {
                let locations = flow
                    .reachable_blocks
                    .iter()
                    .flat_map(|(block_id, block)| {
                        block
                            .points
                            .iter()
                            .filter(move |(candidate, _)| candidate == point)
                            .map(move |(_, kind)| (block_id, *kind))
                    })
                    .collect::<Vec<_>>();
                if locations.len() != 1
                    || locations.first().is_none_or(|(block_id, kind)| {
                        *block_id != &arm.block || *kind != expected_kind
                    })
                {
                    return Err(
                        "branch-local Set payload Load and Set are not exact points in the same Checker arm"
                            .into(),
                    );
                }
            }
            let point_rank = |point: &NodeId| {
                flow.reachable_blocks.get(&arm.block).and_then(|block| {
                    block
                        .points
                        .iter()
                        .position(|(candidate, _)| candidate == point)
                })
            };
            if arm.payload_load == arm.set.point
                || !point_rank(&arm.payload_load)
                    .zip(point_rank(&arm.set.point))
                    .is_some_and(|(load, set)| load < set)
            {
                return Err(
                    "branch-local Set payload Load does not precede its exact Set point".into(),
                );
            }
        }
        if expected_actions.len() != snapshot.actions.len()
            || expected_actions.iter().any(|expected| {
                snapshot
                    .actions
                    .iter()
                    .filter(|candidate| *candidate == *expected)
                    .count()
                    != 1
            })
            || expected_attempts != snapshot.attempts
            || payload_loads.len() != 2
            || child_locals.len() != 2
        {
            return Err(
                "branch-local Set diamond private Checker actions or attempts were changed or duplicated"
                    .into(),
            );
        }

        let payload_type = self
            .get(&proof.payload_type)
            .ok_or("branch-local Set diamond payload TypeDesc is missing")?;
        if payload_type.kind != MirTypeKind::Nominal
            || !matches!(&payload_type.layout, MirLayout::Record { .. })
            || payload_type.ownership == MirOwnership::Copy
            || payload_type.abi != MirAbiClass::Aggregate
            || payload_type.glue.clone != MirGlueKind::Aggregate
            || payload_type.glue.drop != MirGlueKind::Aggregate
            || !payload_type.needs_clone_glue
            || !payload_type.needs_drop_glue
            || self.contains_dynamic_or_managed_any(&proof.payload_type)
        {
            return Err(
                "branch-local Set payload must be the exact cloneable, droppable nominal Record TypeDesc"
                    .into(),
            );
        }
        self.validate_glue(&proof.payload_type, MirGlueOperation::Clone)?;
        self.validate_glue(&proof.payload_type, MirGlueOperation::Drop)?;

        let checker_actions = &function.ownership.checker_ordinary_map_owned_any_v2;
        let root_result_type = super::types::ordinary_map_owned_any_v2_root_result_type_id();
        for arm in proof.arms.values() {
            let set_local = MirValueId::new(format!("local:{}", arm.set.local.0 .0))
                .map_err(|error| error.to_string())?;
            let set_result = MirValueId::new(format!("expr:{}", arm.set.point.0))
                .map_err(|error| error.to_string())?;
            let set_id = MirInstructionId::new(format!(
                "inst:ordinary_map_owned_any_v2_set:{set_local}:{}",
                arm.set.point.0
            ))
            .map_err(|error| error.to_string())?;
            let clone_id = MirInstructionId::new(format!("inst:clone:{}", arm.payload_load.0))
                .map_err(|error| error.to_string())?;
            let temporary_drop_id = MirInstructionId::new(format!(
                "inst:drop.ordinary-map-owned-any-v2-value:{}",
                arm.set.point.0
            ))
            .map_err(|error| error.to_string())?;
            let source = MirValueId::new(format!("local:{}", proof.payload_local.0 .0))
                .map_err(|error| error.to_string())?;
            let cloned = MirValueId::new(format!("expr:{}", arm.payload_load.0))
                .map_err(|error| error.to_string())?;
            let base = MirValueId::new(format!("local:{}", proof.base_local.0 .0))
                .map_err(|error| error.to_string())?;
            let mut locations =
                BTreeMap::<MirInstructionId, Vec<(MirBlockId, usize, &MirInstruction)>>::new();
            for (block_id, block) in &function.blocks {
                for (index, instruction) in block.instructions.iter().enumerate() {
                    if instruction.id == clone_id
                        || instruction.id == set_id
                        || instruction.id == temporary_drop_id
                    {
                        locations.entry(instruction.id.clone()).or_default().push((
                            block_id.clone(),
                            index,
                            instruction,
                        ));
                    }
                }
            }
            let [(_, clone_index, clone_instruction)] =
                locations.get(&clone_id).map(Vec::as_slice).unwrap_or(&[])
            else {
                return Err(
                    "branch-local Set arm does not contain exactly one exact payload Clone".into(),
                );
            };
            let [(_, set_index, set_instruction)] =
                locations.get(&set_id).map(Vec::as_slice).unwrap_or(&[])
            else {
                return Err(
                    "branch-local Set arm does not contain exactly one exact Checker Set".into(),
                );
            };
            let [(_, drop_index, drop_instruction)] = locations
                .get(&temporary_drop_id)
                .map(Vec::as_slice)
                .unwrap_or(&[])
            else {
                return Err(
                    "branch-local Set arm does not contain exactly one exact aggregate temporary Drop".into(),
                );
            };
            let clone_block = locations[&clone_id][0].0.clone();
            let set_block = locations[&set_id][0].0.clone();
            let drop_block = locations[&temporary_drop_id][0].0.clone();
            if clone_block != set_block
                || set_block != drop_block
                || clone_index >= set_index
                || set_index >= drop_index
                || !matches!(
                    &clone_instruction.kind,
                    MirInstructionKind::Clone { source: actual_source, result }
                        if actual_source == &source && result == &cloned
                )
                || !matches!(
                    &set_instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Set {
                        result,
                        source: actual_base,
                        value,
                        ..
                    } if result == &set_result && actual_base == &base && value == &cloned
                )
                || function
                    .values
                    .get(&set_result)
                    .is_none_or(|value| value.ty != root_result_type)
                || !has_checker_ordinary_map_owned_any_v2_action(function, set_instruction)
                || checker_actions
                    .iter()
                    .filter(|action| action.instruction == set_id)
                    .count()
                    != 1
                || !matches!(
                    &drop_instruction.kind,
                    MirInstructionKind::Drop { value } if value == &cloned
                )
                || function
                    .values
                    .get(&source)
                    .is_none_or(|value| value.ty != proof.payload_type)
                || function
                    .values
                    .get(&cloned)
                    .is_none_or(|value| value.ty != proof.payload_type)
            {
                return Err(
                    "branch-local Set arm Clone/Set/temporary-Drop instructions disagree with the exact Checker operands or order"
                        .into(),
                );
            }

            let sealed_origins = [
                (&clone_id, &arm.payload_load),
                (&set_id, &arm.set.point),
                (&temporary_drop_id, &arm.set.point),
            ];
            for (instruction_id, expected_origin) in sealed_origins {
                let matches = seal
                    .instructions
                    .iter()
                    .filter(|(block, origin, instruction)| {
                        block == &set_block
                            && origin == expected_origin
                            && instruction.id == *instruction_id
                    })
                    .count();
                if matches != 1 {
                    return Err(
                        "branch-local Set Clone/temporary-Drop origin is absent from the private body snapshot"
                            .into(),
                    );
                }
            }

            let mut temporary_uses = Vec::new();
            for block in function.blocks.values() {
                for instruction in &block.instructions {
                    temporary_uses.extend(
                        super::mir_instruction_value_uses(&instruction.kind)
                            .into_iter()
                            .filter(|value| value == &cloned)
                            .map(|_| instruction.id.clone()),
                    );
                }
                if super::mir_terminator_value_uses(&block.terminator)
                    .iter()
                    .any(|value| value == &cloned)
                {
                    return Err(
                        "branch-local Set payload Clone escapes through a terminator or block edge"
                            .into(),
                    );
                }
            }
            if temporary_uses != vec![set_id, temporary_drop_id] {
                return Err(
                    "branch-local Set payload Clone must be used only by its Set and one exact temporary Drop"
                        .into(),
                );
            }
        }
        Ok(())
    }

    fn managed_any_branch_local_set_temporary_instruction_is_authorized(
        &self,
        function: &MirFunction,
        instruction: &MirInstruction,
    ) -> bool {
        let Some(snapshot) = function
            .ownership
            .checker_managed_any
            .as_ref()
            .and_then(|seal| seal.branch_local_set_diamond_v2_snapshot.as_ref())
        else {
            return false;
        };
        let Some(flow) = snapshot.flow.as_ref() else {
            return false;
        };
        let crate::core::ownership::OrdinaryMapOwnedAnyV2CfgFlowProof::BranchLocalPersistentSetDiamondV1(
            proof,
        ) = &flow.proof
        else {
            return false;
        };
        proof.arms.values().any(|arm| {
            let clone_id = format!("inst:clone:{}", arm.payload_load.0);
            let drop_id = format!(
                "inst:drop.ordinary-map-owned-any-v2-value:{}",
                arm.set.point.0
            );
            match &instruction.kind {
                MirInstructionKind::Clone { source, result }
                    if instruction.id.as_str() == clone_id
                        && source.as_str() == format!("local:{}", proof.payload_local.0 .0)
                        && result.as_str() == format!("expr:{}", arm.payload_load.0) =>
                {
                    true
                }
                MirInstructionKind::Drop { value }
                    if instruction.id.as_str() == drop_id
                        && value.as_str() == format!("expr:{}", arm.payload_load.0) =>
                {
                    true
                }
                _ => false,
            }
        })
    }

    fn managed_any_branch_local_set_payload_drop_is_authorized(
        &self,
        function: &MirFunction,
        value: &MirValueId,
    ) -> bool {
        let Some(seal) = function.ownership.checker_managed_any.as_ref() else {
            return false;
        };
        let Some(snapshot) = seal.branch_local_set_diamond_v2_snapshot.as_ref() else {
            return false;
        };
        if function
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            != Some(snapshot)
        {
            return false;
        }
        let Some(flow) = snapshot.flow.as_ref() else {
            return false;
        };
        let crate::core::ownership::OrdinaryMapOwnedAnyV2CfgFlowProof::BranchLocalPersistentSetDiamondV1(
            proof,
        ) = &flow.proof
        else {
            return false;
        };
        snapshot.owner == function.owner
            && proof.owner == function.owner
            && value.as_str() == format!("local:{}", proof.payload_local.0 .0)
            && Self::managed_any_value_type(function, value) == Ok(&proof.payload_type)
    }

    fn managed_any_branch_local_set_instruction_is_authorized(
        &self,
        function: &MirFunction,
        instruction: &MirInstruction,
    ) -> bool {
        use crate::core::ownership::{
            OrdinaryMapOwnedAnyV2ActionKind as ActionKind,
            OrdinaryMapOwnedAnyV2CfgFlowProof as FlowProof,
            OrdinaryMapOwnedAnyV2ProfileVersion as Profile,
        };

        let Some(snapshot) = function
            .ownership
            .checker_managed_any
            .as_ref()
            .and_then(|seal| seal.branch_local_set_diamond_v2_snapshot.as_ref())
        else {
            return false;
        };
        if function
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            != Some(snapshot)
        {
            return false;
        }
        let Some(flow) = snapshot.flow.as_ref() else {
            return false;
        };
        let FlowProof::BranchLocalPersistentSetDiamondV1(proof) = &flow.proof else {
            return false;
        };
        if snapshot.owner != function.owner
            || snapshot.profile_version != Profile::V2
            || proof.owner != function.owner
            || proof.entry_match_split != flow.entry
            || proof.split != flow.split
            || proof.outer_join != flow.join
            || flow.split == flow.entry
            || proof.arms.len() != 2
            || proof.result_branches.len() != 3
            || flow.arm_edges.len() != 2
            || proof.arms.keys().cloned().collect::<BTreeSet<_>>() != flow.arm_edges
        {
            return false;
        }

        let snapshot_set_points = snapshot
            .actions
            .iter()
            .filter(|action| action.kind == ActionKind::Set)
            .map(|action| action.point.clone())
            .collect::<BTreeSet<_>>();
        let proof_set_points = proof
            .arms
            .values()
            .map(|arm| arm.set.point.clone())
            .collect::<BTreeSet<_>>();
        let set_instruction_count = function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|candidate| {
                matches!(
                    &candidate.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Set { .. }
                )
            })
            .count();
        if snapshot_set_points.len() != 2
            || proof_set_points.len() != 2
            || snapshot_set_points != proof_set_points
            || set_instruction_count != 2
        {
            return false;
        }

        let MirInstructionKind::OrdinaryMapOwnedAnyV2Set {
            result,
            source,
            key,
            value,
        } = &instruction.kind
        else {
            return false;
        };
        let matching_arms = proof
            .arms
            .values()
            .filter(|arm| {
                let set = &arm.set;
                if set.kind != ActionKind::Set
                    || set.profile_version != Profile::V2
                    || set.local == proof.base_local
                    || set.source_local.as_ref() != Some(&proof.base_local)
                    || set.value.as_ref() != Some(&arm.payload_load)
                    || set.value_type.as_ref() != Some(&proof.payload_type)
                    || !set.payload_clone
                {
                    return false;
                }
                let Some(set_local) = MirValueId::new(format!("local:{}", set.local.0 .0)).ok()
                else {
                    return false;
                };
                let Some(expected_result) = MirValueId::new(format!("expr:{}", set.point.0)).ok()
                else {
                    return false;
                };
                let Some(expected_source) =
                    MirValueId::new(format!("local:{}", proof.base_local.0 .0)).ok()
                else {
                    return false;
                };
                let expected_key = set
                    .key_local
                    .as_ref()
                    .and_then(|local| MirValueId::new(format!("local:{}", local.0 .0)).ok())
                    .or_else(|| {
                        set.key
                            .as_ref()
                            .and_then(|node| MirValueId::new(format!("expr:{}", node.0)).ok())
                    });
                let expected_value = MirValueId::new(format!("expr:{}", arm.payload_load.0)).ok();
                let expected_instruction = MirInstructionId::new(format!(
                    "inst:ordinary_map_owned_any_v2_set:{set_local}:{}",
                    set.point.0
                ))
                .ok();
                if expected_key.as_ref() != Some(key)
                    || expected_value.as_ref() != Some(value)
                    || result != &expected_result
                    || source != &expected_source
                    || expected_instruction.as_ref() != Some(&instruction.id)
                {
                    return false;
                }
                let matching_checker_actions = function
                    .ownership
                    .checker_ordinary_map_owned_any_v2
                    .iter()
                    .filter(|action| action.instruction == instruction.id)
                    .collect::<Vec<_>>();
                let [checker_action] = matching_checker_actions.as_slice() else {
                    return false;
                };
                checker_action.kind == ActionKind::Set
                    && checker_action.point == set.point
                    && checker_action.local == set_local
                    && checker_action.result.as_ref() == Some(&set_local)
                    && checker_action.source_local.as_ref() == Some(&expected_source)
                    && checker_action.key.as_ref() == expected_key.as_ref()
                    && checker_action.value.as_ref() == expected_value.as_ref()
            })
            .count();
        matching_arms == 1 && has_checker_ordinary_map_owned_any_v2_action(function, instruction)
    }

    fn validate_managed_any_read_arm_instruction(
        &self,
        function: &MirFunction,
        instruction: &MirInstruction,
        managed_values: &BTreeSet<MirValueId>,
        state: &ManagedAnyCfgFlowState,
    ) -> Result<(), String> {
        let ordinary_map_observation = match &instruction.kind {
            MirInstructionKind::OrdinaryMapOwnedAnyV2Size { result, root } => Some((
                result,
                root,
                crate::core::ir::primitive_type_id(PrimitiveType::I32),
                None,
            )),
            MirInstructionKind::OrdinaryMapOwnedAnyV2Contains { result, root, key } => Some((
                result,
                root,
                crate::core::ir::primitive_type_id(PrimitiveType::Bool),
                Some(key),
            )),
            MirInstructionKind::OrdinaryMapOwnedAnyV2IsEmpty { result, root } => Some((
                result,
                root,
                crate::core::ir::primitive_type_id(PrimitiveType::Bool),
                None,
            )),
            _ => None,
        };
        if let Some((result, root, expected_result_type, key)) = ordinary_map_observation {
            let root_type = super::types::ordinary_map_owned_any_v2_type_id();
            let has_checker_receipt =
                has_checker_ordinary_map_owned_any_v2_action(function, instruction);
            let root_is_live = managed_values.contains(root) && state.owners.contains(root);
            let actual_root_type = Self::managed_any_value_type(function, root)?;
            let actual_result_type = Self::managed_any_value_type(function, result)?;
            let key_is_string = key.is_none_or(|key| self.managed_any_is_string(function, key));
            if !has_checker_receipt
                || !root_is_live
                || actual_root_type != &root_type
                || actual_result_type != &expected_result_type
                || !key_is_string
            {
                return Err(format!(
                    "ManagedAny CFG branch Map observation '{}' lacks a live root, exact result/key type, or Checker borrow receipt (checker={}, live={}, root_type={}, result_type={}, expected_result_type={}, key_is_string={})",
                    instruction.id,
                    has_checker_receipt,
                    root_is_live,
                    actual_root_type.as_str(),
                    actual_result_type.as_str(),
                    expected_result_type.as_str(),
                    key_is_string,
                ));
            }
            return Ok(());
        }
        if self
            .managed_any_branch_local_set_temporary_instruction_is_authorized(function, instruction)
        {
            return Ok(());
        }
        if let MirInstructionKind::ManagedAny {
            result,
            source,
            operation: MirManagedAnyOperation::UnpackI32 | MirManagedAnyOperation::UnpackString,
            contract,
        } = &instruction.kind
        {
            if !managed_values.contains(source)
                || result
                    .as_ref()
                    .is_some_and(|result| managed_values.contains(result))
                || !state.owners.contains(source)
            {
                return Err(format!(
                    "ManagedAny CFG branch '{}' may only borrow-unpack a live owner",
                    instruction.id
                ));
            }
            self.validate_managed_any_contract(
                function,
                source,
                result.as_ref(),
                match &instruction.kind {
                    MirInstructionKind::ManagedAny { operation, .. } => *operation,
                    _ => unreachable!(),
                },
                contract.as_ref(),
            )?;
            return Ok(());
        }
        if instruction_value_ids(&instruction.kind)
            .iter()
            .any(|value| managed_values.contains(value))
        {
            return Err(format!(
                "ManagedAny CFG branch '{}' performs ownership work instead of a borrowed unpack",
                instruction.id
            ));
        }

        let scalar_or_string = |value: &MirValueId| {
            self.managed_any_is_copy_value(function, value)
                || self.managed_any_is_string(function, value)
        };
        match &instruction.kind {
            MirInstructionKind::Const { result, literal } => {
                if !scalar_or_string(result) {
                    return Err(format!(
                        "ManagedAny CFG branch constant '{}' has a non-scalar owned type",
                        instruction.id
                    ));
                }
                if matches!(literal, ResolvedLiteral::String(_)) {
                    self.validate_glue(
                        Self::managed_any_value_type(function, result)?,
                        MirGlueOperation::Drop,
                    )?;
                }
            }
            MirInstructionKind::Binary {
                result,
                left,
                right,
                ..
            } => {
                if !scalar_or_string(result) || !scalar_or_string(left) || !scalar_or_string(right)
                {
                    return Err(format!(
                        "ManagedAny CFG branch Binary '{}' is outside scalar/String read evaluation",
                        instruction.id
                    ));
                }
                if self.managed_any_is_string(function, result) {
                    self.validate_glue(
                        Self::managed_any_value_type(function, result)?,
                        MirGlueOperation::Drop,
                    )?;
                }
            }
            MirInstructionKind::Unary {
                result, operand, ..
            } => {
                if !self.managed_any_is_copy_value(function, result)
                    || !self.managed_any_is_copy_value(function, operand)
                {
                    return Err(format!(
                        "ManagedAny CFG branch Unary '{}' is outside the scalar read face",
                        instruction.id
                    ));
                }
            }
            MirInstructionKind::Convert { result, source } => {
                if !self.managed_any_is_copy_value(function, result)
                    || !self.managed_any_is_copy_value(function, source)
                {
                    return Err(format!(
                        "ManagedAny CFG branch conversion '{}' is outside the scalar read face",
                        instruction.id
                    ));
                }
            }
            MirInstructionKind::Move { result, source } => {
                if self.managed_any_is_string(function, source) {
                    if !self.managed_any_is_string(function, result) {
                        return Err(format!(
                            "ManagedAny CFG branch Move '{}' cannot transfer an unsealed String owner",
                            instruction.id
                        ));
                    }
                    self.validate_glue(
                        Self::managed_any_value_type(function, source)?,
                        MirGlueOperation::MoveOut,
                    )?;
                    self.validate_glue(
                        Self::managed_any_value_type(function, result)?,
                        MirGlueOperation::Drop,
                    )?;
                } else if !self.managed_any_is_copy_value(function, source)
                    || !self.managed_any_is_copy_value(function, result)
                {
                    return Err(format!(
                        "ManagedAny CFG branch Move '{}' is outside the scalar/String read face",
                        instruction.id
                    ));
                }
            }
            MirInstructionKind::Clone { result, source } => {
                if self.managed_any_is_string(function, source) {
                    if !self.managed_any_is_string(function, result) {
                        return Err(format!(
                            "ManagedAny CFG branch Clone '{}' cannot duplicate an unsealed String owner",
                            instruction.id
                        ));
                    }
                    self.validate_glue(
                        Self::managed_any_value_type(function, source)?,
                        MirGlueOperation::Clone,
                    )?;
                    self.validate_glue(
                        Self::managed_any_value_type(function, result)?,
                        MirGlueOperation::Drop,
                    )?;
                } else if !self.managed_any_is_copy_value(function, source)
                    || !self.managed_any_is_copy_value(function, result)
                {
                    return Err(format!(
                        "ManagedAny CFG branch Clone '{}' is outside the scalar/String read face",
                        instruction.id
                    ));
                }
            }
            MirInstructionKind::Drop { value } => {
                if self.managed_any_branch_local_set_payload_drop_is_authorized(function, value) {
                    self.validate_glue(
                        Self::managed_any_value_type(function, value)?,
                        MirGlueOperation::Drop,
                    )?;
                    return Ok(());
                }
                if !self.managed_any_is_string(function, value) {
                    return Err(format!(
                        "ManagedAny CFG branch Drop '{}' does not release a branch-owned String result",
                        instruction.id
                    ));
                }
                self.validate_glue(
                    Self::managed_any_value_type(function, value)?,
                    MirGlueOperation::Drop,
                )?;
            }
            _ => {
                return Err(format!(
                    "ManagedAny CFG branch instruction '{}' is outside the read-only arm contract",
                    instruction.id
                ));
            }
        }
        Ok(())
    }

    fn validate_ordinary_map_owned_any_v2_result_branch_switch_move(
        &self,
        function: &MirFunction,
        mir_block_id: &MirBlockId,
        scrutinee: &MirValueId,
        arms: &[super::MirSwitchArm],
    ) -> Result<Vec<ManagedAnyCfgSuccessor>, String> {
        use crate::core::cfg::{EdgeKind, Terminator as SourceTerminator};

        let fail = |reason: &str| format!("ManagedAny CFG AnyOwner-v2 Result SwitchMove {reason}");
        if !super::validate_ordinary_map_owned_any_v2_action_receipts(function).is_empty()
            || !super::validate_ordinary_map_owned_any_v2_cfg_flow_receipt_before_seal(function)
                .is_empty()
        {
            return Err(fail("lacks clean Checker AnyOwner-v2 flow receipts"));
        }
        let (_cfg, flow, branch) =
            ordinary_map_owned_any_v2_result_branch_for_scrutinee(function, scrutinee)
                .ok_or_else(|| fail("has no exact ResultBranchV1 receipt"))?;
        let root_result_type = super::types::ordinary_map_owned_any_v2_root_result_type_id();
        if function
            .values
            .get(scrutinee)
            .is_none_or(|value| value.ty != root_result_type)
        {
            return Err(fail("scrutinee is not the canonical Result<MapRoot, i32>"));
        }
        let mut source_blocks = flow
            .reachable_blocks
            .iter()
            .filter(|(source_id, source_block)| {
                let matches_block = if *source_id == &flow.entry {
                    mir_block_id == &function.entry
                } else if let Some((_, role)) = source_id.0 .0.rsplit_once("/cfg:block:") {
                    if role == "match-arm" {
                        flow.reachable_edges
                            .values()
                            .filter(|edge| {
                                &edge.to == *source_id && edge.kind == EdgeKind::MatchArm
                            })
                            .any(|edge| {
                                MirBlockId::new(format!("bb:match.arm:{}", edge.source.node.0))
                                    .is_ok_and(|expected| expected == *mir_block_id)
                            })
                    } else if matches!(role, "if-then" | "if-else") {
                        let expected_kind = if role == "if-then" {
                            EdgeKind::Then
                        } else {
                            EdgeKind::Else
                        };
                        let arm_role = if role == "if-then" { "then" } else { "else" };
                        flow.reachable_edges
                            .values()
                            .filter(|edge| &edge.to == *source_id && edge.kind == expected_kind)
                            .any(|edge| {
                                super::ordinary_map_owned_any_v2_if_mir_anchor(
                                    function,
                                    flow,
                                    &edge.source.node,
                                )
                                .and_then(|(if_prefix, mir_node)| {
                                    MirBlockId::new(format!(
                                        "bb:{if_prefix}.{arm_role}:{}",
                                        mir_node.0
                                    ))
                                    .map_err(|error| error.to_string())
                                })
                                .is_ok_and(|expected| expected == *mir_block_id)
                            })
                    } else {
                        MirBlockId::new(format!(
                            "bb:{}:{}",
                            role.replace('-', "."),
                            source_block.source.node.0
                        ))
                        .is_ok_and(|expected| expected == *mir_block_id)
                    }
                } else {
                    false
                };
                matches_block
                    && matches!(
                        &source_block.terminator,
                        SourceTerminator::Match { scrutinee: s, .. } if s == &branch.scrutinee_node
                    )
            });
        let Some((_, source_block)) = source_blocks.next() else {
            return Err(fail("does not map to an exact Checker Match block"));
        };
        if source_blocks.next().is_some() {
            return Err(fail("maps to more than one Checker Match block"));
        }
        let SourceTerminator::Match {
            scrutinee: source_scrutinee,
            arms: source_edges,
        } = &source_block.terminator
        else {
            return Err(fail("does not map to a Checker Match terminator"));
        };
        if source_scrutinee != &branch.scrutinee_node
            || source_edges.len() != 2
            || arms.len() != 2
            || !source_edges.contains(&branch.ok.arm_entry_edge)
            || !source_edges.contains(&branch.err.arm_entry_edge)
        {
            return Err(fail(
                "does not have the exact two-arm Checker Result dispatch",
            ));
        }

        let root_type = super::types::ordinary_map_owned_any_v2_type_id();
        let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
        let mut seen_variants = BTreeSet::new();
        let mut successors = Vec::with_capacity(2);
        for arm in arms {
            let super::MirSwitchCase::Variant(variant) = &arm.case else {
                return Err(fail("contains a non-variant SwitchMove arm"));
            };
            if !seen_variants.insert(variant.clone()) {
                return Err(fail("repeats a Result variant arm"));
            }
            if *variant == branch.ok_variant && variant.0 == "builtin:variant:Result::Ok" {
                let expected_edge =
                    super::MirEdgeId::new(format!("edge:match.arm:{}", branch.ok_arm.0))
                        .map_err(|error| error.to_string())?;
                let expected_target = MirBlockId::new(format!("bb:match.arm:{}", branch.ok_arm.0))
                    .map_err(|error| error.to_string())?;
                let field = NodeId("builtin:variant:Result::Ok/payload:0".into());
                let expected_projection = self.validated_variant_payload_projection_contract(
                    &root_result_type,
                    variant,
                    &field,
                    &root_type,
                )?;
                let expected_param = MirValueId::new(format!("local:{}", branch.output_root.0 .0))
                    .map_err(|error| error.to_string())?;
                let [binding] = arm.bindings.as_slice() else {
                    return Err(fail("Ok arm does not bind exactly one root payload"));
                };
                let target = function
                    .blocks
                    .get(&arm.target)
                    .ok_or_else(|| fail("Ok arm target block is absent"))?;
                if branch.ok_binding.as_ref() != Some(&branch.output_root)
                    || arm.edge != expected_edge
                    || arm.target != expected_target
                    || !arm.arguments.is_empty()
                    || binding.parameter != expected_param
                    || binding.projection != expected_projection
                    || binding.nested_tuple.is_some()
                    || target.parameters.len() != 1
                    || target.parameters[0].value != binding.parameter
                    || function
                        .values
                        .get(&binding.parameter)
                        .is_none_or(|value| value.ty != root_type)
                {
                    return Err(fail(
                        "MIR Ok arm binding, edge, target parameter, or root type differs from Checker",
                    ));
                }
                successors.push(ManagedAnyCfgSuccessor {
                    target: arm.target.clone(),
                    arguments: vec![binding.parameter.clone()],
                    switch_move_payloads: BTreeSet::from([binding.parameter.clone()]),
                });
            } else if *variant == branch.err_variant && variant.0 == "builtin:variant:Result::Err" {
                let expected_edge =
                    super::MirEdgeId::new(format!("edge:match.arm:{}", branch.err_arm.0))
                        .map_err(|error| error.to_string())?;
                let expected_target = MirBlockId::new(format!("bb:match.arm:{}", branch.err_arm.0))
                    .map_err(|error| error.to_string())?;
                let target = function
                    .blocks
                    .get(&arm.target)
                    .ok_or_else(|| fail("Err arm target block is absent"))?;
                if arm.edge != expected_edge
                    || arm.target != expected_target
                    || !arm.arguments.is_empty()
                {
                    return Err(fail("MIR Err arm edge or target differs from Checker"));
                }
                let arguments = if let Some(err_local) = &branch.err_binding {
                    let field = NodeId("builtin:variant:Result::Err/payload:0".into());
                    let expected_projection = self.validated_variant_payload_projection_contract(
                        &root_result_type,
                        variant,
                        &field,
                        &i32_type,
                    )?;
                    let expected_param = MirValueId::new(format!("local:{}", err_local.0 .0))
                        .map_err(|error| error.to_string())?;
                    let [binding] = arm.bindings.as_slice() else {
                        return Err(fail("Err arm does not bind its i32 payload"));
                    };
                    if binding.parameter != expected_param
                        || binding.projection != expected_projection
                        || binding.nested_tuple.is_some()
                        || target.parameters.len() != 1
                        || target.parameters[0].value != binding.parameter
                        || function
                            .values
                            .get(&binding.parameter)
                            .is_none_or(|value| value.ty != i32_type)
                    {
                        return Err(fail(
                            "MIR Err arm payload binding or parameter differs from Checker",
                        ));
                    }
                    vec![binding.parameter.clone()]
                } else {
                    if !arm.bindings.is_empty() || !target.parameters.is_empty() {
                        return Err(fail("wildcard Err arm unexpectedly binds a payload"));
                    }
                    Vec::new()
                };
                successors.push(ManagedAnyCfgSuccessor {
                    target: arm.target.clone(),
                    arguments,
                    switch_move_payloads: BTreeSet::new(),
                });
            } else {
                return Err(fail("has a non-Result variant arm"));
            }
        }
        if seen_variants
            != BTreeSet::from([
                NodeId("builtin:variant:Result::Ok".into()),
                NodeId("builtin:variant:Result::Err".into()),
            ])
        {
            return Err(fail("is not exhaustive over Result::Ok and Result::Err"));
        }
        Ok(successors)
    }

    fn sealed_result_branch_scrutinee_operation(
        function: &MirFunction,
        scrutinee: &MirValueId,
    ) -> Result<NodeId, String> {
        let fail = |reason: &str| format!("ManagedAny CFG Values Result SwitchMove {reason}");
        let matches_scrutinee = |operation: &NodeId| {
            MirValueId::new(format!("expr:{}", operation.0))
                .is_ok_and(|expected| expected == *scrutinee)
                .then(|| operation.clone())
        };
        if let Some(branch) = sealed_values_local_index_i32_result_branch(function) {
            return matches_scrutinee(&branch.map_new_operation)
                .or_else(|| matches_scrutinee(&branch.map_set_operation))
                .ok_or_else(|| fail("does not consume the exact New or Set Result"));
        }
        let (_, checker_cfg, managed_cfg) = sealed_branch_context(function)
            .ok_or_else(|| fail("has no exact private Checker branch receipt"))?;
        if let Some(chain_branch) = checker_cfg
            .result_receipts
            .iter()
            .find_map(|result| result.computed_string_key_owner_chain.as_deref())
            .and_then(|chain| chain.result_branch.as_deref())
        {
            if chain_branch.owner == function.owner && chain_branch.cfg == *managed_cfg {
                return matches_scrutinee(&chain_branch.map_new_operation)
                    .or_else(|| matches_scrutinee(&chain_branch.map_set_operation))
                    .ok_or_else(|| fail("does not consume the exact New or Set Result"));
            }
        }
        if let Some(drop_control) = checker_cfg
            .map_set_drop_control_result_branch_receipt
            .as_ref()
        {
            if drop_control.owner == function.owner && drop_control.cfg == *managed_cfg {
                return matches_scrutinee(&drop_control.map_new_operation)
                    .or_else(|| matches_scrutinee(&drop_control.map_set_operation))
                    .ok_or_else(|| fail("does not consume the exact New or Set Result"));
            }
        }
        if let Some(values_unpack) = checker_cfg.values_unpack_result_branch_receipt.as_ref() {
            if values_unpack.owner == function.owner && values_unpack.cfg == *managed_cfg {
                return matches_scrutinee(&values_unpack.map_new_operation)
                    .or_else(|| {
                        values_unpack
                            .sets
                            .iter()
                            .find_map(|set_branch| matches_scrutinee(&set_branch.operation))
                    })
                    .ok_or_else(|| fail("does not consume the exact New or Set Result"));
            }
        }
        Err(fail("has no exact private Checker branch receipt"))
    }

    fn validate_values_local_index_result_branch_switch_move(
        &self,
        function: &MirFunction,
        mir_block_id: &MirBlockId,
        scrutinee: &MirValueId,
        arms: &[super::MirSwitchArm],
    ) -> Result<Vec<ManagedAnyCfgSuccessor>, String> {
        use crate::core::cfg::Terminator as SourceTerminator;
        use crate::core::ManagedAnyCfgMatchCase;

        let fail = |reason: &str| format!("ManagedAny CFG Values Result SwitchMove {reason}");
        let expected_operation =
            Self::sealed_result_branch_scrutinee_operation(function, scrutinee)?;
        let root_result_type = super::types::ordinary_map_owned_any_v2_root_result_type_id();
        if function
            .values
            .get(scrutinee)
            .is_none_or(|value| value.ty != root_result_type)
        {
            return Err(fail("scrutinee is not the canonical Result<MapRoot, i32>"));
        }
        let source_cfg = function
            .ownership
            .checker_managed_any_cfg_receipt
            .as_ref()
            .ok_or_else(|| fail("has no private Checker source-CFG projection"))?;
        let exact_source_cfg_ids = is_computed_string_key_result_branch_cfg(function, source_cfg);
        let use_function_entry = !exact_source_cfg_ids
            && sealed_values_local_index_i32_result_branch(function).is_none();
        let source_block_to_mir = |id: &crate::core::cfg::BasicBlockId| {
            if exact_source_cfg_ids {
                MirBlockId::new(id.0 .0.clone()).map_err(|error| error.to_string())
            } else {
                values_local_index_result_branch_mir_block_id(source_cfg, id)
            }
        };
        let source_edge_to_mir = |id: &crate::core::cfg::EdgeId| {
            if exact_source_cfg_ids {
                super::MirEdgeId::new(id.0 .0.clone()).map_err(|error| error.to_string())
            } else {
                values_local_index_result_branch_mir_edge_id(source_cfg, id)
            }
        };
        let mut source_blocks = source_cfg.reachable_blocks.iter().filter(|(source_id, _)| {
            if use_function_entry && **source_id == source_cfg.entry {
                *mir_block_id == function.entry
            } else {
                source_block_to_mir(source_id).is_ok_and(|expected| expected == *mir_block_id)
            }
        });
        let (source_id, source_block) = source_blocks
            .next()
            .ok_or_else(|| fail("does not map to an exact Checker Match block"))?;
        if source_blocks.next().is_some() {
            return Err(fail("maps to more than one Checker Match block"));
        }
        let SourceTerminator::Match {
            scrutinee: source_scrutinee,
            arms: source_edges,
        } = &source_block.terminator
        else {
            return Err(fail("does not map to a Checker Match terminator"));
        };
        if source_scrutinee != &expected_operation || source_edges.len() != 2 || arms.len() != 2 {
            return Err(fail(
                "does not have exactly the sealed two-arm Result dispatch",
            ));
        }
        let source_arm_receipts = source_cfg
            .match_arms
            .get(source_id)
            .ok_or_else(|| fail("has no exact Checker Match arm payload receipt"))?;
        if source_arm_receipts.len() != 2 {
            return Err(fail("Checker Match arm payload receipt is incomplete"));
        }

        let root_type = super::types::ordinary_map_owned_any_v2_type_id();
        let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
        let mut seen_variants = BTreeSet::new();
        let mut successors = Vec::with_capacity(2);
        for ((source_edge_id, source_arm_receipt), arm) in
            source_edges.iter().zip(source_arm_receipts).zip(arms)
        {
            let source_edge = source_cfg
                .reachable_edges
                .get(source_edge_id)
                .ok_or_else(|| fail("Checker Match arm edge is absent"))?;
            let ManagedAnyCfgMatchCase::Variant(variant) = &source_arm_receipt.case else {
                return Err(fail("Checker Result Match arm is not an exact variant"));
            };
            if source_arm_receipt.edge != *source_edge_id
                || source_arm_receipt.source_arm != source_edge.source.node
                || source_arm_receipt.guarded
                || !seen_variants.insert(variant.clone())
            {
                return Err(fail(
                    "Checker Result Match arm identity is guarded or duplicated",
                ));
            }
            let (variant_name, payload_type) = match variant.0.as_str() {
                "builtin:variant:Result::Ok" => ("Ok", &root_type),
                "builtin:variant:Result::Err" => ("Err", &i32_type),
                _ => return Err(fail("Checker Result Match has a non-Result variant")),
            };
            let expected_case = super::MirSwitchCase::Variant(variant.clone());
            let expected_edge = source_edge_to_mir(source_edge_id)?;
            let expected_target = source_block_to_mir(&source_edge.to)?;
            let field = NodeId(format!("builtin:variant:Result::{variant_name}/payload:0"));
            let expected_projection = self.validated_variant_payload_projection_contract(
                &root_result_type,
                variant,
                &field,
                payload_type,
            )?;
            let [binding] = arm.bindings.as_slice() else {
                return Err(fail("Result arm does not bind exactly one payload"));
            };
            let target = function
                .blocks
                .get(&arm.target)
                .ok_or_else(|| fail("Result arm target block is absent"))?;
            if arm.case != expected_case
                || arm.edge != expected_edge
                || arm.target != expected_target
                || !arm.arguments.is_empty()
                || binding.projection != expected_projection
                || binding.nested_tuple.is_some()
                || target.parameters.len() != 1
                || target.parameters[0].value != binding.parameter
                || function
                    .values
                    .get(&binding.parameter)
                    .is_none_or(|value| value.ty != *payload_type)
            {
                return Err(fail(
                    "MIR Result arm projection, edge, target parameter, or payload type differs from Checker",
                ));
            }
            let mut arguments = arm.arguments.clone();
            arguments.push(binding.parameter.clone());
            let switch_move_payloads = if expected_projection.ownership.needs_drop() {
                BTreeSet::from([binding.parameter.clone()])
            } else {
                Default::default()
            };
            successors.push(ManagedAnyCfgSuccessor {
                target: arm.target.clone(),
                arguments,
                switch_move_payloads,
            });
        }
        if seen_variants
            != BTreeSet::from([
                NodeId("builtin:variant:Result::Ok".into()),
                NodeId("builtin:variant:Result::Err".into()),
            ])
        {
            return Err(fail(
                "is not exhaustive over exactly Result::Ok and Result::Err",
            ));
        }
        Ok(successors)
    }

    fn validate_managed_any_cfg_flow(
        &self,
        function: &MirFunction,
        managed_values: &BTreeSet<MirValueId>,
    ) -> Result<(), String> {
        let (map_root_producer_return, map_root_producer_caller, get_any_caller) =
            map_root_boundary_seal_snapshots(function).ok_or(
                "ManagedAny Map-root caller authority is partial or has conflicting Checker receipt pairs",
            )?;
        match (
            map_root_producer_return.as_ref(),
            map_root_producer_caller.as_ref(),
            get_any_caller.as_ref(),
        ) {
            (Some(_), Some(_), None) | (Some(_), None, Some(_))
                if !has_checker_ordinary_map_owned_any_v2_map_root_producer_return(function) =>
            {
                return Err(
                    "AnyOwner-v2 producer return differs from its exact Checker return/local transfer receipt"
                        .into(),
                );
            }
            (None, Some(_), None) => {
                validate_checker_ordinary_map_owned_any_v2_map_root_caller_shape(function)?;
            }
            (None, None, Some(receipt)) if receipt.caller_owner == function.owner => {
                validate_checker_ordinary_map_owned_any_v2_map_root_get_any_caller_shape(function)?;
            }
            (None, None, Some(_)) => {
                return Err(
                    "Map-root GetAny-caller receipt is not sealed by its producer return".into(),
                );
            }
            _ => {}
        }
        let mut successors = BTreeMap::<MirBlockId, Vec<ManagedAnyCfgSuccessor>>::new();
        let mut authorized_values_branch_switch_moves = BTreeSet::new();
        let mut authorized_owned_get_result_branch_v3_switch_moves = BTreeSet::new();
        for (block_id, block) in &function.blocks {
            let block_successors = match &block.terminator {
                MirTerminator::Goto {
                    target, arguments, ..
                } => vec![ManagedAnyCfgSuccessor {
                    target: target.clone(),
                    arguments: arguments.clone(),
                    switch_move_payloads: BTreeSet::new(),
                }],
                MirTerminator::Branch {
                    then_target,
                    then_arguments,
                    else_target,
                    else_arguments,
                    ..
                } => {
                    if then_target == else_target {
                        return Err(format!(
                            "ManagedAny CFG branch '{}' aliases its Then and Else targets",
                            block_id
                        ));
                    }
                    vec![
                        ManagedAnyCfgSuccessor {
                            target: then_target.clone(),
                            arguments: then_arguments.clone(),
                            switch_move_payloads: BTreeSet::new(),
                        },
                        ManagedAnyCfgSuccessor {
                            target: else_target.clone(),
                            arguments: else_arguments.clone(),
                            switch_move_payloads: BTreeSet::new(),
                        },
                    ]
                }
                MirTerminator::Switch { arms, .. } => {
                    if arms.iter().any(|arm| !arm.bindings.is_empty()) {
                        return Err(
                            "ManagedAny CFG Switch payload bindings need an exact per-arm owner projection receipt"
                                .into(),
                        );
                    }
                    arms.iter()
                        .map(|arm| ManagedAnyCfgSuccessor {
                            target: arm.target.clone(),
                            arguments: arm.arguments.clone(),
                            switch_move_payloads: BTreeSet::new(),
                        })
                        .collect()
                }
                MirTerminator::SwitchMove { scrutinee, arms } => {
                    if function.values.get(scrutinee).is_some_and(|value| {
                        value.ty
                            == super::types::ordinary_map_owned_get_i32_string_v1_root_result_type_id()
                    }) && function
                        .ownership
                        .has_ordinary_map_owned_get_result_branch_v3_receipt()
                        && arms.len() == 2
                        && arms.iter().all(|arm| {
                            arm.bindings.iter().all(|binding| {
                                !managed_values.contains(&binding.parameter)
                                    && !self.managed_any_is_string(function, &binding.parameter)
                                    && function.values.get(&binding.parameter).is_some_and(
                                        |value| !self.contains_dynamic_or_managed_any(&value.ty),
                                    )
                            })
                        })
                    {
                        // ResultBranchV3 wraps the continuation body inside Ok/Err
                        // arms; path-sensitive ManagedAny state still walks both arms,
                        // while only inner scalar Branch/Switch regions enter
                        // `read_arm_blocks`.
                        authorized_owned_get_result_branch_v3_switch_moves.insert(block_id.clone());
                        arms.iter()
                            .map(|arm| {
                                let mut arguments = arm.arguments.clone();
                                for binding in &arm.bindings {
                                    arguments.push(binding.parameter.clone());
                                }
                                ManagedAnyCfgSuccessor {
                                    target: arm.target.clone(),
                                    arguments,
                                    switch_move_payloads: BTreeSet::new(),
                                }
                            })
                            .collect()
                    } else if function.values.get(scrutinee).is_some_and(|value| {
                        value.ty
                            == super::types::ordinary_map_owned_get_i32_string_v1_root_result_type_id()
                    }) {
                        if function
                            .ownership
                            .checker_ordinary_map_owned_get_i32_string_v1_cfg_receipt
                            .is_none()
                        {
                            return Err(
                                "typed owned-get ManagedAny operation has no Map-specific Checker receipt"
                                    .into(),
                            );
                        }
                        return Err(
                            "typed owned-get ManagedAny Result SwitchMove requires an exact ResultBranchV3 Checker receipt and non-Managed bindings"
                                .into(),
                        );
                    } else if function.values.get(scrutinee).is_some_and(|value| {
                        value.ty == super::types::ordinary_map_owned_any_v2_root_result_type_id()
                    }) && ordinary_map_owned_any_v2_result_branch_for_scrutinee(
                        function, scrutinee,
                    )
                    .is_some()
                    {
                        let branch_successors = self
                            .validate_ordinary_map_owned_any_v2_result_branch_switch_move(
                                function, block_id, scrutinee, arms,
                            )?;
                        authorized_values_branch_switch_moves.insert(block_id.clone());
                        branch_successors
                    } else {
                        let branch_successors = self
                            .validate_values_local_index_result_branch_switch_move(
                                function, block_id, scrutinee, arms,
                            )?;
                        authorized_values_branch_switch_moves.insert(block_id.clone());
                        branch_successors
                    }
                }
                MirTerminator::Return { .. }
                | MirTerminator::Trap { .. }
                | MirTerminator::Fault { .. }
                | MirTerminator::Unreachable => Vec::new(),
            };
            for successor in &block_successors {
                if !function.blocks.contains_key(&successor.target) {
                    return Err(format!(
                        "ManagedAny CFG block '{}' targets missing block '{}'",
                        block_id, successor.target
                    ));
                }
            }
            successors.insert(block_id.clone(), block_successors);
        }
        let switch_move_payload_parameters = successors
            .values()
            .flatten()
            .flat_map(|successor| successor.switch_move_payloads.iter().cloned())
            .collect::<BTreeSet<_>>();

        // The operation-neutral Checker CFG cannot grant Map ownership by
        // itself. Any typed owned GetAny/Values still needs its independent
        // Map action and path receipt before this generic path analysis runs.
        let has_typed_get_any = function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { .. }
                        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { .. }
                )
            })
        });
        if has_typed_get_any {
            let actions = &function
                .ownership
                .checker_ordinary_map_owned_get_i32_string_v1;
            let cfg_receipt = function
                .ownership
                .checker_ordinary_map_owned_get_i32_string_v1_cfg_receipt
                .as_ref()
                .ok_or(
                    "typed owned-get ManagedAny operation has no Map-specific Checker receipt",
                )?;
            if let Some(error) =
                super::validate_ordinary_map_owned_get_cfg_receipt(function, actions, cfg_receipt)
                    .first()
            {
                return Err(format!(
                    "ManagedAny Map operation cannot trust its specific Checker receipt: {}",
                    error.message
                ));
            }
            if actions.is_empty() {
                return Err("typed owned-get ManagedAny operation has no Map action ledger".into());
            }
        }

        for (block_id, block) in &function.blocks {
            for parameter in &block.parameters {
                let value = function.values.get(&parameter.value).ok_or_else(|| {
                    format!(
                        "ManagedAny CFG block '{}' has an absent parameter",
                        block_id
                    )
                })?;
                if value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                    && !switch_move_payload_parameters.contains(&parameter.value)
                {
                    return Err(format!(
                        "ManagedAny CFG block '{}' has an AnyOwner-v2 root parameter without a callable transfer receipt",
                        block_id
                    ));
                }
                let is_string = self.managed_any_is_string(function, &parameter.value);
                let descriptor = self.get(&value.ty).ok_or_else(|| {
                    format!(
                        "ManagedAny CFG block '{}' parameter has no TypeDesc",
                        block_id
                    )
                })?;
                if managed_values.contains(&parameter.value)
                    || is_string
                    || (value.ty == super::types::ordinary_map_owned_get_i32_string_v1_type_id()
                        && function
                            .ownership
                            .has_ordinary_map_owned_get_result_branch_v3_receipt())
                {
                    self.validate_glue(&value.ty, MirGlueOperation::Drop)?;
                } else if self.contains_dynamic_or_managed_any(&value.ty)
                    || descriptor.ownership != MirOwnership::Copy
                {
                    return Err(format!(
                        "ManagedAny CFG block '{}' has an unsupported non-Copy block parameter",
                        block_id
                    ));
                }
            }
        }

        let mut reachable = BTreeSet::new();
        let mut discover = VecDeque::from([function.entry.clone()]);
        while let Some(block_id) = discover.pop_front() {
            if !reachable.insert(block_id.clone()) {
                continue;
            }
            for successor in successors.get(&block_id).into_iter().flatten() {
                discover.push_back(successor.target.clone());
            }
        }
        if reachable.len() != function.blocks.len() {
            return Err(
                "ManagedAny CFG contains unreachable blocks outside its sealed path proof".into(),
            );
        }

        self.validate_managed_any_string_liveness(function, &successors)?;

        // Mark control-flow regions reached from exactly one arm of a
        // Branch/Switch before their paths reconverge. These regions use the
        // narrow read-expression instruction whitelist for non-owner values;
        // Any owner transfers themselves are checked by the path ledger
        // below, so each arm can independently move or drop an owner.
        let mut read_arm_blocks = BTreeSet::new();
        for (control_id, block) in &function.blocks {
            let targets = match &block.terminator {
                MirTerminator::Branch {
                    condition,
                    then_target,
                    else_target,
                    ..
                } => {
                    let condition_value = function.values.get(condition).ok_or_else(|| {
                        format!(
                            "ManagedAny CFG branch '{}' references an absent condition value",
                            control_id
                        )
                    })?;
                    if managed_values.contains(condition)
                        || self.contains_dynamic_or_managed_any(&condition_value.ty)
                        || condition_value.ty
                            != crate::core::ir::primitive_type_id(PrimitiveType::Bool)
                    {
                        return Err(format!(
                            "ManagedAny CFG branch '{}' condition must be the exact Bool primitive and cannot inspect or move an Any owner",
                            control_id
                        ));
                    }
                    vec![then_target.clone(), else_target.clone()]
                }
                MirTerminator::Switch { scrutinee, arms } => {
                    let value = function.values.get(scrutinee).ok_or_else(|| {
                        format!(
                            "ManagedAny CFG switch '{}' references an absent scrutinee",
                            control_id
                        )
                    })?;
                    if managed_values.contains(scrutinee)
                        || self.contains_dynamic_or_managed_any(&value.ty)
                        || self
                            .get(&value.ty)
                            .is_none_or(|descriptor| descriptor.ownership != MirOwnership::Copy)
                    {
                        return Err(format!(
                            "ManagedAny CFG switch '{}' must inspect a Copy value without nested Any",
                            control_id
                        ));
                    }
                    arms.iter().map(|arm| arm.target.clone()).collect()
                }
                MirTerminator::SwitchMove { scrutinee, arms } => {
                    if authorized_values_branch_switch_moves.contains(control_id)
                        || authorized_owned_get_result_branch_v3_switch_moves.contains(control_id)
                    {
                        continue;
                    }
                    let value = function.values.get(scrutinee).ok_or_else(|| {
                        format!(
                            "ManagedAny CFG SwitchMove '{}' references an absent scrutinee",
                            control_id
                        )
                    })?;
                    if managed_values.contains(scrutinee)
                        || self.contains_dynamic_or_managed_any(&value.ty)
                        || self
                            .get(&value.ty)
                            .is_none_or(|descriptor| descriptor.ownership == MirOwnership::Copy)
                        || self.managed_any_is_string(function, scrutinee)
                        || arms.iter().any(|arm| {
                            arm.bindings.iter().any(|binding| {
                                managed_values.contains(&binding.parameter)
                                    || self.managed_any_is_string(function, &binding.parameter)
                                    || function.values.get(&binding.parameter).is_none_or(|value| {
                                        self.contains_dynamic_or_managed_any(&value.ty)
                                    })
                            })
                        })
                    {
                        return Err(format!(
                            "ManagedAny CFG SwitchMove '{}' requires an explicit owner-transfer receipt",
                            control_id
                        ));
                    }
                    arms.iter().map(|arm| arm.target.clone()).collect()
                }
                _ => continue,
            };
            let target_reachability = targets
                .iter()
                .map(|target| {
                    let mut seen = BTreeSet::new();
                    let mut worklist = VecDeque::from([target.clone()]);
                    while let Some(current) = worklist.pop_front() {
                        if !seen.insert(current.clone()) {
                            continue;
                        }
                        for successor in successors.get(&current).into_iter().flatten() {
                            worklist.push_back(successor.target.clone());
                        }
                    }
                    seen
                })
                .collect::<Vec<_>>();
            for (index, region) in target_reachability.iter().enumerate() {
                for candidate in region {
                    if !target_reachability
                        .iter()
                        .enumerate()
                        .any(|(other, other_region)| {
                            other != index && other_region.contains(candidate)
                        })
                    {
                        read_arm_blocks.insert(candidate.clone());
                    }
                }
            }
        }

        // Keep the identities of every String owner derived from an Any
        // unpack, even after its path-local owner has been consumed.  The
        // live set below is deliberately path-sensitive; this separate
        // lineage set lets the validator distinguish an unrelated String
        // operation from a stale use after Drop/Move/Return on a prior path.
        // Without it, an injected Drop after a diamond join is invisible once
        // the value has been removed from `unpacked_strings`.
        let mut unpacked_string_lineage = BTreeSet::new();
        loop {
            let mut changed = false;
            for (block_id, block) in &function.blocks {
                for instruction in &block.instructions {
                    let produced =
                        match &instruction.kind {
                            MirInstructionKind::ManagedAny {
                                result: Some(result),
                                operation: MirManagedAnyOperation::UnpackString,
                                ..
                            } => Some(result),
                            MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack { result, .. }
                                if self.managed_any_is_string(function, result) =>
                            {
                                Some(result)
                            }
                            MirInstructionKind::OrdinaryMapOwnedAnyV2KeysUnpack {
                                result, ..
                            } if self.managed_any_is_string(function, result) => Some(result),
                            MirInstructionKind::Const {
                                result,
                                literal: ResolvedLiteral::String(_),
                            } if read_arm_blocks.contains(block_id) => Some(result),
                            MirInstructionKind::Binary { result, .. }
                                if read_arm_blocks.contains(block_id)
                                    && self.managed_any_is_string(function, result) =>
                            {
                                Some(result)
                            }
                            MirInstructionKind::Move { source, result }
                            | MirInstructionKind::Clone { source, result }
                                if unpacked_string_lineage.contains(source)
                                    && self.managed_any_is_string(function, result) =>
                            {
                                Some(result)
                            }
                            MirInstructionKind::MoveProject { base, result, .. }
                            | MirInstructionKind::MoveProjectDrop { base, result, .. }
                                if (managed_values.contains(base)
                                    || unpacked_string_lineage.contains(base))
                                    && self.managed_any_is_string(function, result) =>
                            {
                                Some(result)
                            }
                            MirInstructionKind::Binary {
                                result,
                                left,
                                right,
                                ..
                            } if self.managed_any_is_string(function, result)
                                && (unpacked_string_lineage.contains(left)
                                    || unpacked_string_lineage.contains(right)) =>
                            {
                                Some(result)
                            }
                            MirInstructionKind::Call {
                                result: Some(result),
                                ..
                            }
                            | MirInstructionKind::ClosureInvoke {
                                result: Some(result),
                                ..
                            } if self.managed_any_is_string(function, result) => Some(result),
                            _ => None,
                        };
                    if let Some(produced) = produced {
                        changed |= unpacked_string_lineage.insert(produced.clone());
                    }
                }
                for successor in successors.get(block_id).into_iter().flatten() {
                    let Some(target) = function.blocks.get(&successor.target) else {
                        continue;
                    };
                    for (argument, parameter) in successor.arguments.iter().zip(&target.parameters)
                    {
                        if unpacked_string_lineage.contains(argument)
                            && self.managed_any_is_string(function, &parameter.value)
                        {
                            changed |= unpacked_string_lineage.insert(parameter.value.clone());
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }

        let initial = ManagedAnyCfgFlowState {
            owners: function
                .parameters
                .iter()
                .filter(|parameter| managed_values.contains(*parameter))
                .cloned()
                .collect(),
            unpacked_strings: BTreeSet::new(),
        };
        for parameter in &initial.owners {
            if !managed_values.contains(parameter) {
                return Err("ManagedAny CFG entry contains an unsealed owner parameter".into());
            }
        }
        let mut incoming_states = BTreeMap::from([(function.entry.clone(), initial.clone())]);
        let mut pending = VecDeque::from([function.entry.clone()]);
        let mut processed = BTreeSet::new();
        while let Some(block_id) = pending.pop_front() {
            if !processed.insert(block_id.clone()) {
                continue;
            }
            let block = function
                .blocks
                .get(&block_id)
                .ok_or("ManagedAny CFG worklist reached an absent block")?;
            let mut state = incoming_states
                .get(&block_id)
                .cloned()
                .ok_or("ManagedAny CFG block has no path-live input state")?;
            let in_read_arm = read_arm_blocks.contains(&block_id);

            for instruction in &block.instructions {
                for value in super::mir_instruction_value_uses(&instruction.kind) {
                    if unpacked_string_lineage.contains(&value)
                        && !state.unpacked_strings.contains(&value)
                    {
                        return Err(format!(
                            "ManagedAny-derived String '{}' is used after transfer, drop, or before creation at instruction '{}'",
                            value, instruction.id
                        ));
                    }
                }
                if in_read_arm
                    && !instruction_value_ids(&instruction.kind)
                        .iter()
                        .any(|value| managed_values.contains(value))
                {
                    self.validate_managed_any_read_arm_instruction(
                        function,
                        instruction,
                        managed_values,
                        &state,
                    )?;
                }
                match &instruction.kind {
                    MirInstructionKind::Clone { result, source }
                        if function
                            .values
                            .get(source)
                            .is_some_and(|value| {
                                value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                            }) =>
                    {
                        if in_read_arm {
                            return Err(format!(
                                "ManagedAny CFG read arm '{}' cannot clone an AnyOwner-v2 Map root",
                                instruction.id
                            ));
                        }
                        let owner_type = super::types::ordinary_map_owned_any_v2_type_id();
                        if !has_checker_ordinary_map_owned_any_v2_action(function, instruction)
                            || Self::managed_any_value_type(function, result)? != &owner_type
                        {
                            return Err(format!(
                                "AnyOwner-v2 Clone '{}' has no exact Checker root-owner receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, source)?;
                        self.validate_glue(&owner_type, MirGlueOperation::Clone)?;
                        self.validate_glue(&owner_type, MirGlueOperation::Drop)?;
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::OrdinaryMapOwnedAnyV2New { result } => {
                        let is_closed_result_branch_new = {
                            let matches_op = |operation: &NodeId| {
                                MirValueId::new(format!("expr:{}", operation.0))
                                    .is_ok_and(|expected| expected == *result)
                            };
                            if let Some(branch) =
                                sealed_values_local_index_i32_result_branch(function)
                            {
                                matches_op(&branch.map_new_operation)
                            } else if let Some((_, checker_cfg, managed_cfg)) =
                                sealed_branch_context(function)
                            {
                                checker_cfg
                                    .result_receipts
                                    .iter()
                                    .find_map(|r| {
                                        r.computed_string_key_owner_chain
                                            .as_deref()
                                            .and_then(|chain| chain.result_branch.as_deref())
                                    })
                                    .is_some_and(|branch| {
                                        branch.owner == function.owner
                                            && branch.cfg == *managed_cfg
                                            && matches_op(&branch.map_new_operation)
                                    })
                                    || checker_cfg
                                        .map_set_drop_control_result_branch_receipt
                                        .as_ref()
                                        .is_some_and(|branch| {
                                            branch.owner == function.owner
                                                && branch.cfg == *managed_cfg
                                                && matches_op(&branch.map_new_operation)
                                        })
                                    || checker_cfg
                                        .values_unpack_result_branch_receipt
                                        .as_ref()
                                        .is_some_and(|branch| {
                                            branch.owner == function.owner
                                                && branch.cfg == *managed_cfg
                                                && matches_op(&branch.map_new_operation)
                                        })
                            } else {
                                false
                            }
                        };
                        let is_general_result_branch_new =
                            has_ordinary_map_owned_any_v2_result_branch_v1_operation(
                                function,
                                crate::core::OrdinaryMapOwnedAnyV2ActionKind::New,
                                result,
                            );
                        let is_result_branch_new =
                            is_closed_result_branch_new || is_general_result_branch_new;
                        if in_read_arm && !is_result_branch_new {
                            return Err(format!(
                                "ManagedAny CFG read arm '{}' cannot create an AnyOwner-v2 Map root",
                                instruction.id
                            ));
                        }
                        let expected_result_type = if is_result_branch_new {
                            super::types::ordinary_map_owned_any_v2_root_result_type_id()
                        } else {
                            super::types::ordinary_map_owned_any_v2_type_id()
                        };
                        let has_action_receipt = if is_closed_result_branch_new {
                            super::validate_ordinary_map_owned_any_v2_action_receipts(function)
                                .is_empty()
                        } else {
                            has_checker_ordinary_map_owned_any_v2_action(function, instruction)
                        };
                        if !has_action_receipt
                            || Self::managed_any_value_type(function, result)?
                                != &expected_result_type
                        {
                            return Err(format!(
                                "AnyOwner-v2 New '{}' has no exact Checker root-owner receipt",
                                instruction.id
                            ));
                        }
                        self.validate_glue(&expected_result_type, MirGlueOperation::Drop)?;
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::OrdinaryMapOwnedAnyV2SetAnyResult {
                        result,
                        source,
                        key,
                        value,
                    } => {
                        let string_type =
                            crate::core::ir::primitive_type_id(PrimitiveType::String);
                        let missing_branch = || {
                            format!(
                                "AnyOwner-v2 SetAnyResult '{}' has no exact Checker branch receipt",
                                instruction.id
                            )
                        };
                        let (set_op, source_local, key_node, payload_node, payload_type) =
                            if let Some(branch) =
                                sealed_values_local_index_i32_result_branch(function)
                            {
                                (
                                    branch.map_set_operation.clone(),
                                    branch.initial_root.clone(),
                                    branch.key_node.clone(),
                                    branch.payload_node.clone(),
                                    branch.payload_type.clone(),
                                )
                            } else if let Some((_, checker_cfg, managed_cfg)) =
                                sealed_branch_context(function)
                            {
                                if let Some(chain) =
                                    checker_cfg.result_receipts.iter().find_map(|r| {
                                        r.computed_string_key_owner_chain
                                            .as_deref()
                                            .filter(|chain| {
                                                chain.result_branch.as_deref().is_some_and(
                                                    |branch| {
                                                        branch.owner == function.owner
                                                            && branch.cfg == *managed_cfg
                                                    },
                                                )
                                            })
                                    })
                                {
                                    let branch = chain
                                        .result_branch
                                        .as_deref()
                                        .expect("filtered above");
                                    (
                                        branch.map_set_operation.clone(),
                                        branch.initial_root.clone(),
                                        chain
                                            .map_set
                                            .key
                                            .clone()
                                            .ok_or_else(&missing_branch)?,
                                        chain.payload.set_value.clone(),
                                        chain.payload.set_value_type.clone(),
                                    )
                                } else if let Some(branch) = checker_cfg
                                    .map_set_drop_control_result_branch_receipt
                                    .as_ref()
                                    .filter(|branch| {
                                        branch.owner == function.owner && branch.cfg == *managed_cfg
                                    })
                                {
                                    let set_action = checker_cfg
                                        .actions
                                        .iter()
                                        .find(|action| {
                                            action.kind
                                                == crate::core::OrdinaryMapOwnedAnyV2ActionKind::Set
                                                && action.point == branch.map_set_operation
                                        })
                                        .ok_or_else(&missing_branch)?;
                                    (
                                        branch.map_set_operation.clone(),
                                        branch.initial_root.clone(),
                                        branch.key.clone(),
                                        set_action.value.clone().ok_or_else(&missing_branch)?,
                                        set_action.value_type.clone().ok_or_else(&missing_branch)?,
                                    )
                                } else {
                                    return Err(missing_branch());
                                }
                            } else {
                                return Err(missing_branch());
                            };
                        let expected_result = MirValueId::new(format!("expr:{}", set_op.0))
                            .map_err(|error| error.to_string())?;
                        let expected_source =
                            MirValueId::new(format!("local:{}", source_local.0 .0))
                                .map_err(|error| error.to_string())?;
                        let expected_key = MirValueId::new(format!("expr:{}", key_node.0))
                            .map_err(|error| error.to_string())?;
                        let expected_value = MirValueId::new(format!("expr:{}", payload_node.0))
                            .map_err(|error| error.to_string())?;
                        if result != &expected_result
                            || source != &expected_source
                            || key != &expected_key
                            || value != &expected_value
                            || !super::validate_ordinary_map_owned_any_v2_action_receipts(function)
                                .is_empty()
                            || !super::validate_ordinary_map_owned_any_v2_set_any_result_receipts(
                                function,
                            )
                            .is_empty()
                            || Self::managed_any_value_type(function, result)?
                                != &super::types::ordinary_map_owned_any_v2_root_result_type_id()
                            || Self::managed_any_value_type(function, source)?
                                != &super::types::ordinary_map_owned_any_v2_type_id()
                            || Self::managed_any_value_type(function, key)? != &string_type
                            || Self::managed_any_value_type(function, value)? != &payload_type
                        {
                            return Err(format!(
                                "AnyOwner-v2 SetAnyResult '{}' differs from its exact Checker branch operands",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, source)?;
                        self.validate_glue(
                            &super::types::ordinary_map_owned_any_v2_root_result_type_id(),
                            MirGlueOperation::Drop,
                        )?;
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Set {
                        result, source, ..
                    }
                    | MirInstructionKind::OrdinaryMapOwnedAnyV2Remove {
                        result, source, ..
                    } => {
                        let is_result_branch_set = matches!(
                            &instruction.kind,
                            MirInstructionKind::OrdinaryMapOwnedAnyV2Set { .. }
                        ) && (has_ordinary_map_owned_any_v2_result_branch_v1_operation(
                            function,
                            crate::core::OrdinaryMapOwnedAnyV2ActionKind::Set,
                            result,
                        ) || has_closed_values_unpack_result_branch_set(
                            function, result,
                        ));
                        if in_read_arm
                            && !has_closed_values_unpack_result_branch_set(function, result)
                            && !self
                                .managed_any_branch_local_set_instruction_is_authorized(
                                    function,
                                    instruction,
                                )
                        {
                            return Err(format!(
                                "ManagedAny CFG read arm '{}' cannot create an AnyOwner-v2 persistent root",
                                instruction.id
                            ));
                        }
                        let expected_result_type = if is_result_branch_set {
                            super::types::ordinary_map_owned_any_v2_root_result_type_id()
                        } else {
                            super::types::ordinary_map_owned_any_v2_type_id()
                        };
                        if !has_checker_ordinary_map_owned_any_v2_action(function, instruction)
                            || Self::managed_any_value_type(function, source)?
                                != &super::types::ordinary_map_owned_any_v2_type_id()
                            || Self::managed_any_value_type(function, result)?
                                != &expected_result_type
                        {
                            return Err(format!(
                                "AnyOwner-v2 persistent update '{}' has no exact Checker root lineage receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, source)?;
                        self.validate_glue(
                            Self::managed_any_value_type(function, result)?,
                            MirGlueOperation::Drop,
                        )?;
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Size { result, root } => {
                        if (!has_checker_ordinary_map_owned_any_v2_action(function, instruction)
                            && !has_checker_ordinary_map_owned_any_v2_map_root_caller_size(
                                function, instruction, result, root,
                            ))
                            || Self::managed_any_value_type(function, root)?
                                != &super::types::ordinary_map_owned_any_v2_type_id()
                        {
                            return Err(format!(
                                "AnyOwner-v2 root Size '{}' has no exact Checker borrow receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, root)?;
                        let result_type = Self::managed_any_value_type(function, result)?;
                        if result_type == &ordinary_map_owned_any_v2_type_id()
                            || self.contains_dynamic_or_managed_any(result_type)
                        {
                            return Err(format!(
                                "AnyOwner-v2 root Size '{}' has an owner-bearing result",
                                instruction.id
                            ));
                        }
                    }
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Contains { result, root, .. }
                    | MirInstructionKind::OrdinaryMapOwnedAnyV2IsEmpty { result, root } => {
                        if !has_checker_ordinary_map_owned_any_v2_action(function, instruction)
                            || Self::managed_any_value_type(function, root)?
                                != &super::types::ordinary_map_owned_any_v2_type_id()
                        {
                            return Err(format!(
                                "AnyOwner-v2 root observation '{}' has no exact Checker borrow receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, root)?;
                        let result_type = Self::managed_any_value_type(function, result)?;
                        if result_type == &ordinary_map_owned_any_v2_type_id()
                            || self.contains_dynamic_or_managed_any(result_type)
                        {
                            return Err(format!(
                                "AnyOwner-v2 root observation '{}' has an owner-bearing result",
                                instruction.id
                            ));
                        }
                    }
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { root } => {
                        if (!has_checker_ordinary_map_owned_any_v2_action(function, instruction)
                            && !has_checker_ordinary_map_owned_any_v2_map_root_caller_drop(
                                function, instruction, root,
                            ))
                            || Self::managed_any_value_type(function, root)?
                                != &super::types::ordinary_map_owned_any_v2_type_id()
                        {
                            return Err(format!(
                                "AnyOwner-v2 Drop '{}' has no exact Checker owner-disposition receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, root)?;
                        self.validate_glue(
                            Self::managed_any_value_type(function, root)?,
                            MirGlueOperation::Drop,
                        )?;
                        state.owners.remove(root);
                    }
                    MirInstructionKind::OrdinaryMapOwnedAnyV2GetOrDefaultI32 {
                        result,
                        root,
                        key,
                        default,
                        callsite,
                        call_target,
                        type_argument,
                    } => {
                        if in_read_arm
                            || !has_checker_ordinary_map_owned_any_v2_get_or_default_i32_borrow(
                                function,
                                instruction,
                                result,
                                root,
                                key,
                                default,
                                callsite,
                                call_target,
                                type_argument,
                            )
                            || Self::managed_any_value_type(function, root)?
                                != &super::types::ordinary_map_owned_any_v2_type_id()
                        {
                            return Err(format!(
                                "AnyOwner-v2 GetOrDefault<i32> '{}' has no exact Checker borrowed-root receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, root)?;
                        let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
                        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
                        if Self::managed_any_value_type(function, key)? != &string_type
                            || Self::managed_any_value_type(function, default)? != &i32_type
                            || Self::managed_any_value_type(function, result)? != &i32_type
                            || type_argument != &i32_type
                        {
                            return Err(format!(
                                "AnyOwner-v2 GetOrDefault<i32> '{}' has an invalid Checker key/default/result ABI",
                                instruction.id
                            ));
                        }
                        self.validate_copy_scalar(&i32_type)?;
                    }
                    MirInstructionKind::OrdinaryMapOwnedAnyV2GetOrDefaultString {
                        result,
                        root,
                        key,
                        default,
                        callsite,
                        call_target,
                        type_argument,
                    } => {
                        if in_read_arm
                            || !has_checker_ordinary_map_owned_any_v2_get_or_default_string_borrow(
                                function,
                                instruction,
                                result,
                                root,
                                key,
                                default,
                                callsite,
                                call_target,
                                type_argument,
                            )
                            || Self::managed_any_value_type(function, root)?
                                != &super::types::ordinary_map_owned_any_v2_type_id()
                        {
                            return Err(format!(
                                "AnyOwner-v2 GetOrDefault<String> '{}' has no exact Checker borrowed-root receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, root)?;
                        let string_type =
                            crate::core::ir::primitive_type_id(PrimitiveType::String);
                        if [key, default, result].iter().any(|value| {
                            Self::managed_any_value_type(function, value).ok() != Some(&string_type)
                        }) || type_argument != &string_type
                        {
                            return Err(format!(
                                "AnyOwner-v2 GetOrDefault<String> '{}' has an invalid Checker key/default/result ABI",
                                instruction.id
                            ));
                        }
                        self.validate_owned_string(&string_type)?;
                    }
                    MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny {
                        result, root, ..
                    } => {
                        if in_read_arm {
                            return Err(format!(
                                "ManagedAny CFG read arm '{}' cannot create an AnyOwner-v2 GetAny result",
                                instruction.id
                            ));
                        }
                        if !has_checker_ordinary_map_owned_any_v2_action(function, instruction)
                            || Self::managed_any_value_type(function, root)?
                                != &super::types::ordinary_map_owned_any_v2_type_id()
                        {
                            return Err(format!(
                                "AnyOwner-v2 GetAny '{}' has no exact Checker root borrow receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, root)?;
                        self.validate_ordinary_map_owned_any_v2_get_any_result_type(
                            Self::managed_any_value_type(function, result)?,
                        )?;
                        if !has_checker_ordinary_map_owned_any_v2_get_any_result(
                            function,
                            &instruction.id,
                            result,
                        ) {
                            return Err(format!(
                                "AnyOwner-v2 GetAny '{}' has no exact Checker CFG result/settlement receipt",
                                instruction.id
                            ));
                        }
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Println { result, pair, .. } => {
                        if !has_checker_ordinary_map_owned_any_v2_println(function, instruction) {
                            return Err(format!("AnyOwner-v2 println '{}' has no exact source/projection/borrow/Drop receipt", instruction.id));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, pair)?;
                        self.validate_ordinary_map_owned_any_v2_get_any_result_type(Self::managed_any_value_type(function, pair)?)?;
                        if Self::managed_any_value_type(function, result)? != &crate::core::ir::primitive_type_id(PrimitiveType::Unit) {
                            return Err("AnyOwner-v2 println result is not exact Unit".into());
                        }
                    }
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack {
                        result,
                        pair,
                        projection,
                    } => {
                        if !has_checker_ordinary_map_owned_any_v2_unpack(
                            function,
                            &instruction.id,
                            result,
                            pair,
                            projection,
                        ) || Self::managed_any_value_type(function, pair)?
                            != &ordinary_map_owned_any_v2_get_result_type_id()
                        {
                            return Err(format!(
                                "AnyOwner-v2 unpack '{}' has no exact Checker pair/projection/conversion/Drop receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, pair)?;
                        let result_type = Self::managed_any_value_type(function, result)?;
                        let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
                        let bool_type = crate::core::ir::primitive_type_id(PrimitiveType::Bool);
                        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
                        let selected_set_payload = function
                            .ownership
                            .checker_ordinary_map_owned_any_v2
                            .iter()
                            .any(|action| {
                                action.kind
                                    == crate::core::OrdinaryMapOwnedAnyV2ActionKind::Set
                                    && action.value_type.as_ref() == Some(result_type)
                            });
                        if result_type != &i32_type
                            && result_type != &string_type
                            && !selected_set_payload
                        {
                            return Err(format!(
                                "AnyOwner-v2 unpack '{}' target is outside the receipted i32/String/selected-Set profile",
                                instruction.id
                            ));
                        }
                        if result_type == &i32_type {
                            self.validate_copy_scalar(result_type)?;
                        } else if result_type == &bool_type {
                            self.validate_copy_scalar(result_type)?;
                        } else if result_type == &string_type {
                            self.validate_owned_string(result_type)?;
                        }
                        self.validate_glue(result_type, MirGlueOperation::Clone)?;
                        self.validate_glue(result_type, MirGlueOperation::Drop)?;
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                        if result_type == &string_type
                            && !state.unpacked_strings.insert(result.clone())
                        {
                            return Err(format!(
                                "AnyOwner-v2 unpacked String '{}' is produced more than once",
                                result
                            ));
                        }
                    }
                    MirInstructionKind::OrdinaryMapOwnedAnyV2ValuesUnpack {
                        result,
                        values,
                        index,
                        projection,
                    } => {
                        if !has_checker_ordinary_map_owned_any_v2_values_unpack(
                            function,
                            &instruction.id,
                            result,
                            values,
                            index,
                            projection,
                        ) || Self::managed_any_value_type(function, values)?
                            != &ordinary_map_owned_any_v2_values_type_id()
                        {
                            return Err(format!(
                                "AnyOwner-v2 Values unpack '{}' has no exact Checker snapshot/index/conversion/Drop receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, values)?;
                        let result_type = Self::managed_any_value_type(function, result)?;
                        let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
                        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
                        if result_type != &i32_type
                            && result_type != &string_type
                            && !function
                                .ownership
                                .checker_ordinary_map_owned_any_v2_cfg_receipt
                                .as_ref()
                                .is_some_and(|cfg| {
                                    cfg.values_unpack_receipts.iter().any(|receipt| {
                                        let disposition_matches_profile =
                                            receipt.index_disposition.is_canonical()
                                                && match receipt.index_disposition {
                                                    crate::core::OrdinaryMapOwnedAnyV2ValuesIndexDisposition::SelectedPayload => {
                                                        !receipt.payload_selection.is_empty()
                                                    }
                                                    crate::core::OrdinaryMapOwnedAnyV2ValuesIndexDisposition::AlwaysOutOfBounds {
                                                        ..
                                                    } => {
                                                        receipt.payload_selection.is_empty()
                                                            && super::reference::is_canonical_v2_label_type(
                                                                self,
                                                                result_type,
                                                            )
                                                    }
                                                };
                                        receipt.cast.0
                                            == result.as_str().strip_prefix("expr:").unwrap_or_default()
                                            && receipt.conversion_to == *result_type
                                            && disposition_matches_profile
                                    })
                                })
                        {
                            return Err(format!(
                                "AnyOwner-v2 Values unpack '{}' target is outside its selected Set payload profile",
                                instruction.id
                            ));
                        }
                        if result_type == &i32_type {
                            self.validate_copy_scalar(result_type)?;
                        } else if result_type == &string_type {
                            self.validate_owned_string(result_type)?;
                        }
                        self.validate_glue(result_type, MirGlueOperation::Clone)?;
                        self.validate_glue(result_type, MirGlueOperation::Drop)?;
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                        if result_type == &string_type
                            && !state.unpacked_strings.insert(result.clone())
                        {
                            return Err(format!(
                                "AnyOwner-v2 Values unpacked String '{}' is produced more than once",
                                result
                            ));
                        }
                    }
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Values { result, root } => {
                        if in_read_arm {
                            return Err(format!(
                                "ManagedAny CFG read arm '{}' cannot create an AnyOwner-v2 Values result",
                                instruction.id
                            ));
                        }
                        if !has_checker_ordinary_map_owned_any_v2_action(function, instruction)
                            || Self::managed_any_value_type(function, root)?
                                != &super::types::ordinary_map_owned_any_v2_type_id()
                        {
                            return Err(format!(
                                "AnyOwner-v2 Values '{}' has no exact Checker root borrow receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, root)?;
                        let result_type = Self::managed_any_value_type(function, result)?;
                        if result_type != &ordinary_map_owned_any_v2_values_type_id() {
                            return Err(format!(
                                "AnyOwner-v2 Values '{}' has a noncanonical MIR result TypeDesc",
                                instruction.id
                            ));
                        }
                        self.validate_list_glue(result_type, MirGlueOperation::MoveOut)?;
                        if !has_checker_ordinary_map_owned_any_v2_values_result(
                            function,
                            &instruction.id,
                            result,
                        ) {
                            return Err(format!(
                                "AnyOwner-v2 Values '{}' has no exact Checker CFG result/settlement receipt",
                                instruction.id
                            ));
                        }
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Keys { result, root } => {
                        if in_read_arm {
                            return Err(format!(
                                "ManagedAny CFG read arm '{}' cannot create an AnyOwner-v2 Keys snapshot",
                                instruction.id
                            ));
                        }
                        if !has_checker_ordinary_map_owned_any_v2_keys_snapshot_operation(
                            function,
                            instruction,
                            result,
                            root,
                            true,
                        ) || Self::managed_any_value_type(function, root)?
                            != &super::types::ordinary_map_owned_any_v2_type_id()
                        {
                            return Err(format!(
                                "AnyOwner-v2 Keys '{}' has no exact Checker root-borrow/snapshot receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, root)?;
                        let result_type = Self::managed_any_value_type(function, result)?;
                        self.validate_owned_string_list(result_type).map_err(|error| {
                            format!(
                                "AnyOwner-v2 Keys '{}' has an invalid owned List<String> result: {error}",
                                instruction.id
                            )
                        })?;
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::OrdinaryMapOwnedAnyV2KeysUnpack {
                        result,
                        keys,
                        projection,
                    } => {
                        if in_read_arm
                            || !has_checker_ordinary_map_owned_any_v2_keys_unpack(
                                function,
                                instruction,
                                result,
                                keys,
                                projection,
                            )
                        {
                            return Err(format!(
                                "AnyOwner-v2 Keys unpack '{}' has no exact Checker snapshot/index/String-copy receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, keys)?;
                        let result_type = Self::managed_any_value_type(function, result)?;
                        self.validate_owned_string(result_type)?;
                        self.validate_glue(result_type, MirGlueOperation::Clone)?;
                        self.validate_glue(result_type, MirGlueOperation::Drop)?;
                        if !state.unpacked_strings.insert(result.clone()) {
                            return Err(format!(
                                "AnyOwner-v2 Keys unpacked String '{}' is produced more than once",
                                result
                            ));
                        }
                    }
                    MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { result, .. } => {
                        if in_read_arm {
                            return Err(format!(
                                "ManagedAny CFG read arm '{}' cannot create a Map snapshot",
                                instruction.id
                            ));
                        }
                        self.validate_ordinary_map_owned_get_any_result_type(
                            Self::managed_any_value_type(function, result)?,
                        )?;
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { result, .. } => {
                        if in_read_arm {
                            return Err(format!(
                                "ManagedAny CFG read arm '{}' cannot create a Map values snapshot",
                                instruction.id
                            ));
                        }
                        self.validate_managed_any_list_type(Self::managed_any_value_type(
                            function, result,
                        )?)?;
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::Binary {
                        result,
                        op,
                        left,
                        right,
                    } if managed_values.contains(left) || managed_values.contains(right) => {
                        let owner = managed_any_string_comparison_owner(
                            function,
                            result,
                            *op,
                            left,
                            right,
                            managed_values,
                        )
                        .ok_or_else(|| {
                            format!(
                                "ManagedAny Binary '{}' is not an exact borrowed equality against String",
                                instruction.id
                            )
                        })?;
                        require_managed_any_cfg_owner(&state.owners, managed_values, owner)?;
                    }
                    MirInstructionKind::ManagedAny {
                        result,
                        source,
                        operation,
                        contract,
                    } => {
                        if *operation != MirManagedAnyOperation::PackI32 {
                            require_managed_any_cfg_owner(&state.owners, managed_values, source)?;
                        }
                        self.validate_managed_any_contract(
                            function,
                            source,
                            result.as_ref(),
                            *operation,
                            contract.as_ref(),
                        )?;
                        if *operation == MirManagedAnyOperation::Drop {
                            state.owners.remove(source);
                        }
                        if *operation == MirManagedAnyOperation::PackI32 {
                            let result = result
                                .as_ref()
                                .ok_or("ManagedAny PackI32 has no owner result")?;
                            create_managed_any_cfg_owner(
                                &mut state.owners,
                                managed_values,
                                result,
                            )?;
                        } else if let Some(result) = result {
                            create_managed_any_cfg_owner(
                                &mut state.owners,
                                managed_values,
                                result,
                            )?;
                        }
                        if *operation == MirManagedAnyOperation::UnpackString {
                            let result = result
                                .as_ref()
                                .ok_or("ManagedAny UnpackString has no owned String result")?;
                            self.validate_glue(
                                Self::managed_any_value_type(function, result)?,
                                MirGlueOperation::Drop,
                            )?;
                            if !state.unpacked_strings.insert(result.clone()) {
                                return Err(format!(
                                    "ManagedAny unpacked String '{}' is produced more than once",
                                    result
                                ));
                            }
                        }
                    }
                    MirInstructionKind::Move { source, result }
                        if managed_values.contains(source) || managed_values.contains(result) =>
                    {
                        if [source, result].iter().any(|value| {
                            function.values.get(*value).is_some_and(|value| {
                                value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                            })
                        }) {
                            let producer_transfer =
                                has_checker_ordinary_map_owned_any_v2_map_root_producer_return_move(
                                    function,
                                    instruction,
                                    source,
                                    result,
                                );
                            let caller_transfer =
                                has_checker_ordinary_map_owned_any_v2_map_root_caller_binding_move(
                                    function,
                                    instruction,
                                    source,
                                    result,
                                ) || has_checker_ordinary_map_owned_any_v2_map_root_get_any_caller_binding_move(
                                    function,
                                    instruction,
                                    source,
                                    result,
                                );
                            if !producer_transfer && !caller_transfer {
                                return Err(format!(
                                    "AnyOwner-v2 root Move '{}' has no exact Checker producer-return or caller-binding receipt",
                                    instruction.id
                                ));
                            }
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, source)?;
                        if Self::managed_any_value_type(function, source)?
                            != Self::managed_any_value_type(function, result)?
                        {
                            return Err("ManagedAny Move changes owner TypeDesc identity".into());
                        }
                        state.owners.remove(source);
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::Clone { source, result }
                        if managed_values.contains(source) || managed_values.contains(result) =>
                    {
                        if [source, result].iter().any(|value| {
                            function.values.get(*value).is_some_and(|value| {
                                value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                            })
                        }) {
                            return Err(format!(
                                "AnyOwner-v2 root Clone '{}' has no exact Checker clone receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, source)?;
                        if Self::managed_any_value_type(function, source)?
                            != Self::managed_any_value_type(function, result)?
                        {
                            return Err(
                                "ManagedAny tuple Clone changes owner TypeDesc identity".into()
                            );
                        }
                        self.validate_glue(
                            Self::managed_any_value_type(function, source)?,
                            MirGlueOperation::Clone,
                        )?;
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::Drop { value } if managed_values.contains(value) => {
                        if function.values.get(value).is_some_and(|value| {
                            value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                        }) {
                            return Err(format!(
                                "AnyOwner-v2 root Drop '{}' must use its dedicated Checker action",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, value)?;
                        self.validate_glue(
                            Self::managed_any_value_type(function, value)?,
                            MirGlueOperation::Drop,
                        )?;
                        state.owners.remove(value);
                    }
                    MirInstructionKind::ListOp {
                        result,
                        operation: super::MirListOperation::Len,
                        list,
                        argument: None,
                        list_operation_contract: Some(contract),
                    } if managed_values.contains(list) => {
                        require_managed_any_cfg_owner(&state.owners, managed_values, list)?;
                        let list_type = Self::managed_any_value_type(function, list)?;
                        if list_type == &ordinary_map_owned_any_v2_values_type_id() {
                            self.validate_list_glue(list_type, MirGlueOperation::MoveOut)?;
                        } else {
                            self.validate_managed_any_list_type(list_type)?;
                        }
                        self.validate_list_operation_receipt(
                            Self::managed_any_value_type(function, result)?,
                            list_type,
                            super::MirListOperation::Len,
                            contract,
                        )?;
                    }
                    MirInstructionKind::Construct {
                        result,
                        kind: MirAggregateKind::Tuple,
                        fields,
                    } if managed_values.contains(result) => {
                        if fields.iter().any(|field| {
                            function.values.get(field).is_some_and(|value| {
                                value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                            })
                        }) {
                            return Err(format!(
                                "AnyOwner-v2 root aggregate construction '{}' has no exact Checker transfer receipt",
                                instruction.id
                            ));
                        }
                        let result_type = Self::managed_any_value_type(function, result)?;
                        self.validate_recursive_tuple_abi(result_type)?;
                        let Some(MirLayout::Tuple(elements)) = self
                            .get(result_type)
                            .map(|descriptor| &descriptor.layout)
                        else {
                            return Err("ManagedAny Construct has no tuple descriptor".into());
                        };
                        if elements.len() != fields.len() {
                            return Err("ManagedAny tuple Construct fields disagree with exact TypeDesc identities".into());
                        }
                        for (field, ty) in fields.iter().zip(elements) {
                            if Self::managed_any_value_type(function, field)? != ty {
                                return Err("ManagedAny tuple Construct fields disagree with exact TypeDesc identities".into());
                            }
                        }
                        for field in fields {
                            require_managed_any_cfg_owner(&state.owners, managed_values, field)?;
                            if managed_values.contains(field) {
                                state.owners.remove(field);
                            }
                            if state.unpacked_strings.contains(field) {
                                self.validate_glue(
                                    Self::managed_any_value_type(function, field)?,
                                    MirGlueOperation::MoveOut,
                                )?;
                                state.unpacked_strings.remove(field);
                            }
                        }
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::Project {
                        result,
                        base,
                        projection: MirProjection::Tuple(index),
                        ..
                    } if managed_values.contains(base) => {
                        if Self::managed_any_value_type(function, result)?
                            == &super::types::ordinary_map_owned_any_v2_type_id()
                        {
                            return Err(format!(
                                "AnyOwner-v2 root projection '{}' has no exact Checker transfer receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, base)?;
                        let is_v2_get_any_pair = Self::managed_any_value_type(function, base)?
                            == &ordinary_map_owned_any_v2_get_result_type_id();
                        if is_v2_get_any_pair
                            && !(*index == 0
                                && Self::managed_any_value_type(function, result)?
                                    == &crate::core::ir::primitive_type_id(PrimitiveType::Bool))
                        {
                            return Err("AnyOwner-v2 GetAny permits only its non-owning bool result projection".into());
                        }
                        self.validate_projection(
                            Self::managed_any_value_type(function, base)?,
                            Self::managed_any_value_type(function, result)?,
                            &MirProjection::Tuple(*index),
                        )?;
                        if managed_values.contains(result) {
                            self.validate_glue(
                                Self::managed_any_value_type(function, result)?,
                                MirGlueOperation::Clone,
                            )?;
                        }
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::MoveProject {
                        result,
                        base,
                        projection,
                    } if managed_values.contains(base)
                        || managed_values.contains(result)
                        || self.managed_any_is_string(function, result) =>
                    {
                        if [base, result].iter().any(|value| {
                            function.values.get(*value).is_some_and(|value| {
                                value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                            })
                        }) {
                            return Err(format!(
                                "AnyOwner-v2 root projection move '{}' has no exact Checker transfer receipt",
                                instruction.id
                            ));
                        }
                        if !managed_values.contains(base) {
                            return Err(format!(
                                "ManagedAny MoveProject '{}' has no tracked aggregate owner",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, base)?;
                        self.validate_move_projection(
                            Self::managed_any_value_type(function, base)?,
                            Self::managed_any_value_type(function, result)?,
                            projection,
                        )?;
                        state.owners.remove(base);
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                        if self.managed_any_is_string(function, result) {
                            self.validate_glue(
                                Self::managed_any_value_type(function, result)?,
                                MirGlueOperation::Drop,
                            )?;
                            if !state.unpacked_strings.insert(result.clone()) {
                                return Err(format!(
                                    "ManagedAny MoveProject String result '{}' is already live",
                                    result
                                ));
                            }
                        }
                    }
                    MirInstructionKind::MoveProjectDrop {
                        result,
                        base,
                        projection,
                        contract,
                    } if managed_values.contains(base)
                        || managed_values.contains(result)
                        || self.managed_any_is_string(function, result) =>
                    {
                        if [base, result].iter().any(|value| {
                            function.values.get(*value).is_some_and(|value| {
                                value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                            })
                        }) {
                            return Err(format!(
                                "AnyOwner-v2 root residual projection '{}' has no exact Checker transfer receipt",
                                instruction.id
                            ));
                        }
                        if !managed_values.contains(base) {
                            return Err(format!(
                                "ManagedAny MoveProjectDrop '{}' has no tracked aggregate owner",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, base)?;
                        if !matches!(projection, MirProjection::Field(_)) {
                            return Err(
                                "ManagedAny MoveProjectDrop requires a direct Checker-sealed record field"
                                    .into(),
                            );
                        }
                        let contract = contract.as_ref().ok_or(
                            "ManagedAny MoveProjectDrop has no Checker residual-drop contract",
                        )?;
                        self.validate_record_move_projection_drop_receipt(
                            Self::managed_any_value_type(function, base)?,
                            Self::managed_any_value_type(function, result)?,
                            contract,
                        )?;
                        state.owners.remove(base);
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                        if self.managed_any_is_string(function, result) {
                            self.validate_glue(
                                Self::managed_any_value_type(function, result)?,
                                MirGlueOperation::Drop,
                            )?;
                            if !state.unpacked_strings.insert(result.clone()) {
                                return Err(format!(
                                    "ManagedAny MoveProjectDrop String result '{}' is already live",
                                    result
                                ));
                            }
                        }
                    }
                    MirInstructionKind::Call {
                        result: Some(result),
                        callee: crate::core::ir::ResolvedCallee::Function(callee),
                        type_arguments,
                        arguments,
                        effect_receipts,
                        variant_call_contract,
                        ..
                    } if has_checker_ordinary_map_owned_any_v2_map_root_get_any_caller_call(
                        function,
                        &instruction.id,
                        callee,
                        type_arguments,
                        arguments,
                        effect_receipts,
                        variant_call_contract.as_ref(),
                        result,
                    ) =>
                    {
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::Call {
                        result: Some(result),
                        callee: crate::core::ir::ResolvedCallee::Function(callee),
                        type_arguments,
                        arguments,
                        effect_receipts,
                        variant_call_contract,
                        ..
                    } if has_checker_ordinary_map_owned_any_v2_map_root_caller_call(
                        function,
                        &instruction.id,
                        callee,
                        type_arguments,
                        arguments,
                        effect_receipts,
                        variant_call_contract.as_ref(),
                        result,
                    ) =>
                    {
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::Call {
                        result: Some(result),
                        callee: crate::core::ir::ResolvedCallee::Function(callee),
                        type_arguments,
                        arguments,
                        effect_receipts,
                        variant_call_contract,
                        ..
                    } if has_checker_ordinary_map_owned_any_v2_values_caller_call(
                        function,
                        &instruction.id,
                        callee,
                        type_arguments,
                        arguments,
                        effect_receipts,
                        variant_call_contract.as_ref(),
                        result,
                    ) =>
                    {
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::Call {
                        result: Some(result),
                        callee: crate::core::ir::ResolvedCallee::Function(callee),
                        type_arguments,
                        arguments,
                        effect_receipts,
                        variant_call_contract,
                        ..
                    } if has_checker_ordinary_map_owned_any_v2_values_crosscall_string_caller_call(
                        function,
                        &instruction.id,
                        callee,
                        type_arguments,
                        arguments,
                        effect_receipts,
                        variant_call_contract.as_ref(),
                        result,
                    ) => {
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::Call {
                        result: Some(result),
                        callee: crate::core::ir::ResolvedCallee::Function(callee),
                        type_arguments,
                        arguments,
                        effect_receipts,
                        variant_call_contract,
                        ..
                    } if has_checker_ordinary_map_owned_any_v2_values_result_producer_caller_call(
                        function,
                        &instruction.id,
                        callee,
                        type_arguments,
                        arguments,
                        effect_receipts,
                        variant_call_contract.as_ref(),
                        result,
                    ) => {
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::Call {
                        result,
                        callee: crate::core::ir::ResolvedCallee::Function(_),
                        arguments,
                        ..
                    }
                    // This is only local owner accounting. The separate
                    // whole-program ClosureCallable receipt is mandatory
                    // before any consumer can execute or emit this transfer.
                    | MirInstructionKind::ClosureInvoke {
                        result, arguments, ..
                    } => {
                        if arguments.iter().chain(result.iter()).any(|id| {
                            function.values.get(id).is_some_and(|value| {
                                value.ty == ordinary_map_owned_any_v2_values_type_id()
                            })
                        }) {
                            return Err(format!(
                                "AnyOwner-v2 Values call '{}' has no exact Checker producer/caller owner-transfer receipt",
                                instruction.id
                            ));
                        }
                        if arguments.iter().any(|argument| {
                            function.values.get(argument).is_some_and(|value| {
                                value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                            })
                        }) || result.as_ref().is_some_and(|result| {
                            function.values.get(result).is_some_and(|value| {
                                value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                            })
                        }) {
                            return Err(format!(
                                "AnyOwner-v2 root callable transfer '{}' has no exact Checker boundary receipt",
                                instruction.id
                            ));
                        }
                        for argument in arguments {
                            require_managed_any_cfg_owner(&state.owners, managed_values, argument)?;
                            if managed_values.contains(argument) {
                                state.owners.remove(argument);
                            }
                            if state.unpacked_strings.contains(argument) {
                                self.validate_glue(
                                    Self::managed_any_value_type(function, argument)?,
                                    MirGlueOperation::MoveOut,
                                )?;
                                state.unpacked_strings.remove(argument);
                            }
                        }
                        if let Some(result) = result {
                            create_managed_any_cfg_owner(
                                &mut state.owners,
                                managed_values,
                                result,
                            )?;
                            if self.managed_any_is_string(function, result) {
                                self.validate_glue(
                                    Self::managed_any_value_type(function, result)?,
                                    MirGlueOperation::Drop,
                                )?;
                                if !state.unpacked_strings.insert(result.clone()) {
                                    return Err(format!(
                                        "ManagedAny call String result '{}' is already live",
                                        result
                                    ));
                                }
                            }
                        }
                    }
                    MirInstructionKind::BuiltinCall {
                        result,
                        kind: super::types::MirBuiltinKind::PrintlnString,
                        arguments,
                        string_field_contract: None,
                    } if arguments
                        .iter()
                        .any(|argument| state.unpacked_strings.contains(argument)) =>
                    {
                        let [argument] = arguments.as_slice() else {
                            return Err(format!(
                                "ManagedAny-derived String println '{}' does not have one exact argument",
                                instruction.id
                            ));
                        };
                        if !state.unpacked_strings.contains(argument)
                            || !self.managed_any_is_string(function, argument)
                            || function.values.get(result).is_none_or(|value| {
                                value.ty
                                    != crate::core::ir::primitive_type_id(PrimitiveType::Unit)
                            })
                        {
                            return Err(format!(
                                "ManagedAny-derived String println '{}' disagrees with its owned String/Unit ABI",
                                instruction.id
                            ));
                        }
                        self.validate_glue(
                            Self::managed_any_value_type(function, argument)?,
                            MirGlueOperation::MoveOut,
                        )?;
                        state.unpacked_strings.remove(argument);
                    }
                    MirInstructionKind::Drop { value }
                        if state.unpacked_strings.contains(value) =>
                    {
                        self.validate_glue(
                            Self::managed_any_value_type(function, value)?,
                            MirGlueOperation::Drop,
                        )?;
                        state.unpacked_strings.remove(value);
                    }
                    MirInstructionKind::Move { source, result }
                        if state.unpacked_strings.contains(source) =>
                    {
                        self.validate_glue(
                            Self::managed_any_value_type(function, source)?,
                            MirGlueOperation::MoveOut,
                        )?;
                        if !self.managed_any_is_string(function, result) {
                            return Err(
                                "ManagedAny unpacked String Move changes its owned result type"
                                    .into(),
                            );
                        }
                        state.unpacked_strings.remove(source);
                        if !state.unpacked_strings.insert(result.clone()) {
                            return Err(
                                "ManagedAny unpacked String Move duplicates an owner identity"
                                    .into(),
                            );
                        }
                    }
                    MirInstructionKind::Clone { source, result }
                        if state.unpacked_strings.contains(source) =>
                    {
                        self.validate_glue(
                            Self::managed_any_value_type(function, source)?,
                            MirGlueOperation::Clone,
                        )?;
                        if !self.managed_any_is_string(function, result) {
                            return Err(
                                "ManagedAny unpacked String Clone changes its owned result type"
                                    .into(),
                            );
                        }
                        self.validate_glue(
                            Self::managed_any_value_type(function, result)?,
                            MirGlueOperation::Drop,
                        )?;
                        if !state.unpacked_strings.insert(result.clone()) {
                            return Err(
                                "ManagedAny unpacked String Clone duplicates an owner identity"
                                    .into(),
                            );
                        }
                    }
                    other => {
                        let ids = instruction_value_ids(other);
                        if ids.iter().any(|value| managed_values.contains(value)) {
                            return Err(format!(
                                "ManagedAny operation '{}' lacks an admitted CFG owner/borrow/escape contract",
                                instruction.id
                            ));
                        }
                        let string_unary_or_conversion = match other {
                            MirInstructionKind::Unary {
                                result, operand, ..
                            } => {
                                self.managed_any_is_string(function, result)
                                    || self.managed_any_is_string(function, operand)
                            }
                            MirInstructionKind::Convert { result, source } => {
                                self.managed_any_is_string(function, result)
                                    || self.managed_any_is_string(function, source)
                            }
                            _ => false,
                        };
                        if string_unary_or_conversion {
                            return Err(format!(
                                "ManagedAny ownership proof does not admit String Unary/Convert instruction '{}'",
                                instruction.id
                            ));
                        }
                        let tracked_input = ids
                            .iter()
                            .any(|value| state.unpacked_strings.contains(value));
                        if tracked_input
                            && !matches!(
                                other,
                                MirInstructionKind::Binary { .. }
                                    | MirInstructionKind::Unary { .. }
                                    | MirInstructionKind::Convert { .. }
                            )
                        {
                            return Err(format!(
                                "ManagedAny unpacked String is used by unsupported operation '{}'",
                                instruction.id
                            ));
                        }
                        for result in instruction_result_values(other) {
                            if self.managed_any_is_string(function, result)
                                && (in_read_arm || unpacked_string_lineage.contains(result))
                            {
                                self.validate_glue(
                                    Self::managed_any_value_type(function, result)?,
                                    MirGlueOperation::Drop,
                                )?;
                                if !state.unpacked_strings.insert(result.clone()) {
                                    return Err(format!(
                                        "ManagedAny branch String '{}' is produced more than once",
                                        result
                                    ));
                                }
                            }
                        }
                    }
                }
            }

            for value in super::mir_terminator_value_uses(&block.terminator) {
                if managed_values.contains(&value) {
                    require_managed_any_cfg_owner(&state.owners, managed_values, &value)?;
                }
                if unpacked_string_lineage.contains(&value)
                    && !state.unpacked_strings.contains(&value)
                {
                    return Err(format!(
                        "ManagedAny-derived String '{}' is used after transfer, drop, or before creation at the terminator of '{}'",
                        value, block_id
                    ));
                }
            }

            match &block.terminator {
                MirTerminator::Goto { .. } => {}
                MirTerminator::Branch { condition, .. } => {
                    if managed_values.contains(condition)
                        || state.unpacked_strings.contains(condition)
                    {
                        return Err(format!(
                            "ManagedAny CFG branch '{}' inspects an owner-bearing condition",
                            block_id
                        ));
                    }
                }
                MirTerminator::Return { value } => {
                    if let Some(value) = value {
                        if function.values.get(value).is_some_and(|value| {
                            value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                        }) && !has_checker_ordinary_map_owned_any_v2_map_root_producer_return(
                            function,
                        ) {
                            return Err(format!(
                                "AnyOwner-v2 root return from '{}' has no exact Checker escape receipt",
                                block_id
                            ));
                        }
                        if managed_values.contains(value) {
                            require_managed_any_cfg_owner(&state.owners, managed_values, value)?;
                            state.owners.remove(value);
                        }
                        if state.unpacked_strings.contains(value) {
                            self.validate_glue(
                                Self::managed_any_value_type(function, value)?,
                                MirGlueOperation::MoveOut,
                            )?;
                            state.unpacked_strings.remove(value);
                        }
                    }
                    if !state.owners.is_empty() || !state.unpacked_strings.is_empty() {
                        return Err(format!(
                            "ManagedAny owners or unpacked String values remain live at return from '{}': owners={:?}, unpacked_strings={:?}",
                            block_id, state.owners, state.unpacked_strings
                        ));
                    }
                }
                MirTerminator::Trap { .. }
                | MirTerminator::Fault { .. }
                | MirTerminator::Unreachable => {
                    if !state.owners.is_empty() || !state.unpacked_strings.is_empty() {
                        return Err(format!(
                            "ManagedAny owners or unpacked String values remain live at exit from '{}'",
                            block_id
                        ));
                    }
                }
                MirTerminator::Switch { scrutinee, .. } => {
                    if managed_values.contains(scrutinee)
                        || state.unpacked_strings.contains(scrutinee)
                    {
                        return Err(format!(
                            "ManagedAny CFG switch '{}' cannot inspect an owned value",
                            block_id
                        ));
                    }
                }
                MirTerminator::SwitchMove { scrutinee, .. } => {
                    if authorized_values_branch_switch_moves.contains(&block_id) {
                        if !managed_values.contains(scrutinee) {
                            return Err(format!(
                                "ManagedAny CFG Values Result SwitchMove '{}' has no tracked Result owner",
                                block_id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, scrutinee)?;
                        state.owners.remove(scrutinee);
                    } else if managed_values.contains(scrutinee)
                        || state.unpacked_strings.contains(scrutinee)
                    {
                        return Err(format!(
                            "ManagedAny CFG SwitchMove '{}' cannot consume a tracked owner without a payload-transfer receipt",
                            block_id
                        ));
                    }
                }
            }

            for successor in successors.get(&block_id).into_iter().flatten() {
                let target = &successor.target;
                let mut edge_state = state.clone();
                let target_block = function
                    .blocks
                    .get(target)
                    .ok_or("ManagedAny CFG edge target is absent")?;
                if successor.arguments.len() != target_block.parameters.len() {
                    return Err(format!(
                        "ManagedAny CFG edge '{}' -> '{}' argument count disagrees with target parameters",
                        block_id, target
                    ));
                }
                for (argument, parameter) in
                    successor.arguments.iter().zip(&target_block.parameters)
                {
                    let argument_type = function
                        .values
                        .get(argument)
                        .ok_or("ManagedAny CFG edge argument is absent")?;
                    let parameter_type = function
                        .values
                        .get(&parameter.value)
                        .ok_or("ManagedAny CFG block parameter value is absent")?;
                    if argument_type.ty != parameter_type.ty {
                        return Err(format!(
                            "ManagedAny CFG edge '{}' -> '{}' changes block parameter TypeDesc identity",
                            block_id, target
                        ));
                    }
                    if successor.switch_move_payloads.contains(&parameter.value) {
                        if !managed_values.contains(&parameter.value)
                            || argument != &parameter.value
                        {
                            return Err(format!(
                                "ManagedAny CFG SwitchMove edge '{}' -> '{}' has a malformed owner payload binding",
                                block_id, target
                            ));
                        }
                        create_managed_any_cfg_owner(
                            &mut edge_state.owners,
                            managed_values,
                            &parameter.value,
                        )?;
                    } else if managed_values.contains(&parameter.value) {
                        require_managed_any_cfg_owner(
                            &edge_state.owners,
                            managed_values,
                            argument,
                        )?;
                        edge_state.owners.remove(argument);
                        create_managed_any_cfg_owner(
                            &mut edge_state.owners,
                            managed_values,
                            &parameter.value,
                        )?;
                    } else if unpacked_string_lineage.contains(&parameter.value) {
                        if !self.managed_any_is_string(function, argument)
                            || !edge_state.unpacked_strings.remove(argument)
                        {
                            return Err(format!(
                                "ManagedAny CFG edge '{}' -> '{}' does not transfer a live owned String into its block parameter",
                                block_id, target
                            ));
                        }
                        self.validate_glue(&parameter_type.ty, MirGlueOperation::Drop)?;
                        if !edge_state.unpacked_strings.insert(parameter.value.clone()) {
                            return Err(format!(
                                "ManagedAny CFG edge '{}' -> '{}' aliases a live String block parameter",
                                block_id, target
                            ));
                        }
                    } else if managed_values.contains(argument)
                        || edge_state.unpacked_strings.contains(argument)
                    {
                        return Err(format!(
                            "ManagedAny CFG edge '{}' -> '{}' transfers an owner without an owner-bearing block parameter",
                            block_id, target
                        ));
                    }
                }
                match incoming_states.get(target) {
                    Some(previous) if previous != &edge_state => {
                        return Err(format!(
                            "ManagedAny CFG live-owner state disagrees at join block '{}'",
                            target
                        ));
                    }
                    Some(_) => {}
                    None => {
                        incoming_states.insert(target.clone(), edge_state);
                        pending.push_back(target.clone());
                    }
                }
            }
        }

        if processed.len() != function.blocks.len() {
            return Err("ManagedAny CFG path-live worklist did not cover every block".into());
        }
        Ok(())
    }

    /// Bind canonical MIR's reachable edges and branch operands to the
    /// operation-neutral CFG snapshot emitted by Checker. This receipt
    /// establishes correspondence only; owner flow and cleanup are checked
    /// separately by `validate_managed_any_cfg_flow`.
    fn validate_managed_any_source_cfg(
        &self,
        function: &MirFunction,
        seal: &MirCheckerManagedAnySeal,
    ) -> Result<(), String> {
        let Some(receipt) = seal.source_cfg.as_ref() else {
            if function.blocks.len() > 1 {
                return Err(
                    "multi-block ManagedAny function has no Checker source-CFG receipt".into(),
                );
            }
            return Ok(());
        };
        if receipt.owner != function.owner
            || !receipt.reachable_blocks.contains_key(&receipt.entry)
            || receipt.reachable_blocks.len() != function.blocks.len()
        {
            return Err(
                "ManagedAny source-CFG owner, entry, or block coverage disagrees with MIR".into(),
            );
        }

        let entry_role = receipt
            .entry
            .0
             .0
            .rsplit_once("/cfg:block:")
            .map(|(_, role)| role);
        let is_values_local_index_result_entry = seal
            .v2_values_local_index_i32_result_branch
            .as_ref()
            .is_some_and(|branch| {
                if branch.owner != function.owner
                    || branch.cfg.owner != function.owner
                    || receipt.reachable_blocks.len() != 7
                    || receipt.reachable_edges.len() != 8
                {
                    return false;
                }
                let mut new_dispatches =
                    branch
                        .cfg
                        .reachable_blocks
                        .iter()
                        .filter_map(|(block_id, block)| match &block.terminator {
                            crate::core::cfg::Terminator::Match { scrutinee, .. }
                                if scrutinee == &branch.map_new_operation =>
                            {
                                Some(block_id)
                            }
                            _ => None,
                        });
                new_dispatches.next() == Some(&receipt.entry) && new_dispatches.next().is_none()
            });
        if entry_role != Some("entry") && !is_values_local_index_result_entry {
            return Err(
                "ManagedAny Checker source-CFG entry is not the canonical entry block".into(),
            );
        }

        let mut referenced_edges = BTreeSet::new();
        for (block_id, block) in &receipt.reachable_blocks {
            for edge_id in block.terminator.outgoing_edges() {
                let edge = receipt
                    .reachable_edges
                    .get(edge_id)
                    .ok_or("ManagedAny source-CFG terminator references an absent edge")?;
                if &edge.from != block_id
                    || !receipt.reachable_blocks.contains_key(&edge.to)
                    || !referenced_edges.insert(edge_id.clone())
                {
                    return Err(
                        "ManagedAny source-CFG edge endpoint or reference is inconsistent".into(),
                    );
                }
            }
        }
        if referenced_edges.len() != receipt.reachable_edges.len() {
            return Err(
                "ManagedAny source-CFG has an edge outside its reachable terminators".into(),
            );
        }
        let mut reachable = BTreeSet::new();
        let mut worklist = VecDeque::from([receipt.entry.clone()]);
        while let Some(block) = worklist.pop_front() {
            if !reachable.insert(block.clone()) {
                continue;
            }
            let source_block = receipt
                .reachable_blocks
                .get(&block)
                .ok_or("ManagedAny source-CFG traversal reached an absent block")?;
            for edge_id in source_block.terminator.outgoing_edges() {
                let edge = receipt
                    .reachable_edges
                    .get(edge_id)
                    .ok_or("ManagedAny source-CFG traversal found an absent edge")?;
                worklist.push_back(edge.to.clone());
            }
        }
        if reachable.len() != receipt.reachable_blocks.len() {
            return Err("ManagedAny source-CFG snapshot contains unreachable blocks".into());
        }

        let is_direct_values_result_producer = function
            .ownership
            .checker_ordinary_map_owned_any_v2_values_result_producer_receipt
            .as_deref()
            .is_some_and(|producer| {
                producer.owner == function.owner
                    && &producer.cfg == receipt
                    && function
                        .ownership
                        .checker_ordinary_map_owned_any_v2_cfg_receipt
                        .as_ref()
                        .and_then(|cfg| cfg.values_result_producer_receipt.as_ref())
                        == Some(producer)
            });

        let uses_exact_source_cfg_identities = is_direct_values_result_producer
            || is_computed_string_key_result_branch_cfg(function, receipt);

        let mut source_to_mir = BTreeMap::new();
        let mut mapped_mir_blocks = BTreeSet::new();
        for (source_id, source_block) in &receipt.reachable_blocks {
            let mir_block = if uses_exact_source_cfg_identities {
                MirBlockId::new(source_id.0 .0.clone()).map_err(|error| error.to_string())?
            } else {
                let source_role = source_id
                    .0
                     .0
                    .rsplit_once("/cfg:block:")
                    .map(|(_, role)| role)
                    .ok_or("ManagedAny source-CFG block has no canonical role")?;
                if source_role == "entry" {
                    function.entry.clone()
                } else if source_role == "if-join" {
                    let source_node = NodeId(
                        source_id
                            .0
                             .0
                            .rsplit_once("/cfg:block:")
                            .expect("canonical source role was parsed")
                            .0
                            .to_string(),
                    );
                    let (if_prefix, mir_node) = managed_any_if_mir_anchor(receipt, &source_node)?;
                    MirBlockId::new(format!("bb:{if_prefix}.join:{}", mir_node.0))
                        .map_err(|error| error.to_string())?
                } else if matches!(source_role, "if-then" | "if-else") {
                    let expected_kind = if source_role == "if-then" {
                        crate::core::cfg::EdgeKind::Then
                    } else {
                        crate::core::cfg::EdgeKind::Else
                    };
                    let mut incoming = receipt
                        .reachable_edges
                        .values()
                        .filter(|edge| &edge.to == source_id && edge.kind == expected_kind);
                    let selector = incoming
                        .next()
                        .ok_or("ManagedAny source-CFG if arm has no canonical selector edge")?;
                    if incoming.next().is_some() {
                        return Err(
                            "ManagedAny source-CFG if arm has multiple canonical selector edges"
                                .into(),
                        );
                    }
                    let (if_prefix, mir_node) =
                        managed_any_if_mir_anchor(receipt, &selector.source.node)?;
                    let arm_role = if source_role == "if-then" {
                        "then"
                    } else {
                        "else"
                    };
                    MirBlockId::new(format!("bb:{if_prefix}.{arm_role}:{}", mir_node.0))
                        .map_err(|error| error.to_string())?
                } else {
                    let construct_target =
                        matches!(source_role, "iflet-then" | "iflet-else" | "match-arm")
                            || source_role.ends_with("-body")
                            || source_role.ends_with("-header");
                    let node = if construct_target {
                        let incoming = receipt
                            .reachable_edges
                            .values()
                            .filter(|edge| &edge.to == source_id)
                            .find(|edge| {
                                matches!(
                                    edge.kind,
                                    crate::core::cfg::EdgeKind::Then
                                        | crate::core::cfg::EdgeKind::Else
                                        | crate::core::cfg::EdgeKind::MatchArm
                                        | crate::core::cfg::EdgeKind::LoopBody
                                        | crate::core::cfg::EdgeKind::Fallthrough
                                        | crate::core::cfg::EdgeKind::Backedge
                                )
                            })
                            .ok_or(
                                "ManagedAny source-CFG control block has no incoming construct edge",
                            )?;
                        &incoming.source.node
                    } else {
                        &source_block.source.node
                    };
                    MirBlockId::new(format!("bb:{}:{}", source_role.replace('-', "."), node.0))
                        .map_err(|error| error.to_string())?
                }
            };
            if !function.blocks.contains_key(&mir_block)
                || !mapped_mir_blocks.insert(mir_block.clone())
            {
                return Err("ManagedAny Checker block has no unique canonical MIR block".into());
            }
            source_to_mir.insert(source_id.clone(), mir_block);
        }
        if mapped_mir_blocks != function.blocks.keys().cloned().collect() {
            return Err(
                "ManagedAny source-CFG block identities do not cover MIR blocks exactly".into(),
            );
        }
        if source_to_mir.get(&receipt.entry) != Some(&function.entry) {
            return Err(
                "ManagedAny Checker source-CFG entry does not map to the MIR entry block".into(),
            );
        }

        let source_edge_id_to_mir = |source_edge: &crate::core::cfg::EdgeId| {
            if uses_exact_source_cfg_identities {
                super::MirEdgeId::new(source_edge.0 .0.clone()).map_err(|error| error.to_string())
            } else {
                values_local_index_result_branch_mir_edge_id(receipt, source_edge)
            }
        };
        let mut expected_edges = BTreeMap::<super::MirEdgeId, (MirBlockId, MirBlockId)>::new();
        for edge in receipt.reachable_edges.values() {
            let mir_edge = source_edge_id_to_mir(&edge.id)?;
            let from = source_to_mir
                .get(&edge.from)
                .cloned()
                .ok_or("ManagedAny source-CFG edge has no mapped MIR source")?;
            let to = source_to_mir
                .get(&edge.to)
                .cloned()
                .ok_or("ManagedAny source-CFG edge has no mapped MIR target")?;
            if let Some(previous) = expected_edges.insert(mir_edge.clone(), (from, to)) {
                return Err(format!(
                    "ManagedAny source-CFG edge roles collide in canonical MIR: {} ({:?})",
                    mir_edge.as_str(),
                    previous
                ));
            }
        }

        let mut actual_edges = BTreeMap::<super::MirEdgeId, (MirBlockId, MirBlockId)>::new();
        let mut actual_branches = Vec::new();
        let mut actual_switches = Vec::new();
        for block in function.blocks.values() {
            let mut insert = |edge: &super::MirEdgeId, target: &MirBlockId| {
                actual_edges
                    .insert(edge.clone(), (block.id.clone(), target.clone()))
                    .is_none()
            };
            match &block.terminator {
                MirTerminator::Goto { edge, target, .. } => {
                    if !insert(edge, target) {
                        return Err("ManagedAny MIR repeats a CFG edge identity".into());
                    }
                }
                MirTerminator::Branch {
                    condition,
                    then_edge,
                    then_target,
                    else_edge,
                    else_target,
                    ..
                } => {
                    if !insert(then_edge, then_target) || !insert(else_edge, else_target) {
                        return Err("ManagedAny MIR repeats a CFG branch edge identity".into());
                    }
                    actual_branches.push((block.id.clone(), condition.clone()));
                }
                MirTerminator::Switch { scrutinee, arms }
                | MirTerminator::SwitchMove { scrutinee, arms } => {
                    for arm in arms {
                        if !insert(&arm.edge, &arm.target) {
                            return Err("ManagedAny MIR repeats a switch edge identity".into());
                        }
                    }
                    actual_switches.push((block.id.clone(), scrutinee.clone()));
                }
                MirTerminator::Return { .. }
                | MirTerminator::Trap { .. }
                | MirTerminator::Fault { .. }
                | MirTerminator::Unreachable => {}
            }
        }
        if actual_edges != expected_edges {
            return Err(
                "ManagedAny MIR edge identities/targets disagree with the exact Checker CFG".into(),
            );
        }

        let mut expected_branches = Vec::new();
        let mut expected_switches = Vec::new();
        for (source_id, block) in &receipt.reachable_blocks {
            let mir_block = source_to_mir
                .get(source_id)
                .cloned()
                .ok_or("ManagedAny source-CFG selector block has no MIR mapping")?;
            match &block.terminator {
                crate::core::cfg::Terminator::Branch { condition, .. } => {
                    let value = MirValueId::new(format!("expr:{}", condition.0))
                        .map_err(|error| error.to_string())?;
                    let descriptor = function
                        .values
                        .get(&value)
                        .ok_or("ManagedAny Checker branch condition has no canonical MIR value")?;
                    if descriptor.ty != crate::core::ir::primitive_type_id(PrimitiveType::Bool) {
                        return Err(
                            "ManagedAny Checker branch condition is not the exact Bool primitive"
                                .into(),
                        );
                    }
                    expected_branches.push((mir_block, value));
                }
                crate::core::cfg::Terminator::Match { scrutinee, .. } => {
                    expected_switches.push((
                        mir_block,
                        MirValueId::new(format!("expr:{}", scrutinee.0))
                            .map_err(|error| error.to_string())?,
                    ));
                }
                _ => {}
            }
        }
        expected_branches.sort();
        actual_branches.sort();
        expected_switches.sort();
        actual_switches.sort();
        if actual_branches != expected_branches || actual_switches != expected_switches {
            return Err("ManagedAny MIR branch/match selector block and value identities disagree with Checker source expressions".into());
        }
        for (_, condition) in actual_branches {
            let value = function
                .values
                .get(&condition)
                .ok_or("ManagedAny MIR branch references an absent condition value")?;
            if value.ty != crate::core::ir::primitive_type_id(PrimitiveType::Bool) {
                return Err(
                    "ManagedAny MIR branch condition must use the exact Bool primitive".into(),
                );
            }
        }

        for (source_id, source_block) in &receipt.reachable_blocks {
            let mir_id = source_to_mir
                .get(source_id)
                .ok_or("ManagedAny source-CFG terminator block has no MIR mapping")?;
            let mir_block = function
                .blocks
                .get(mir_id)
                .ok_or("ManagedAny source-CFG terminator maps to an absent MIR block")?;
            match (&source_block.terminator, &mir_block.terminator) {
                (
                    crate::core::cfg::Terminator::Goto { edge }
                    | crate::core::cfg::Terminator::Break { edge }
                    | crate::core::cfg::Terminator::Continue { edge },
                    MirTerminator::Goto {
                        edge: actual_edge,
                        target,
                        ..
                    },
                ) => {
                    let source_edge = receipt
                        .reachable_edges
                        .get(edge)
                        .ok_or("ManagedAny Checker Goto edge is absent")?;
                    if source_edge_id_to_mir(edge)? != *actual_edge
                        || source_to_mir.get(&source_edge.to) != Some(target)
                    {
                        return Err(
                            "ManagedAny MIR Goto disagrees with its exact Checker edge and target"
                                .into(),
                        );
                    }
                }
                (
                    crate::core::cfg::Terminator::Branch {
                        condition,
                        then_edge,
                        else_edge,
                    },
                    MirTerminator::Branch {
                        condition: actual_condition,
                        then_edge: actual_then_edge,
                        else_edge: actual_else_edge,
                        ..
                    },
                ) => {
                    let expected_condition = MirValueId::new(format!("expr:{}", condition.0))
                        .map_err(|error| error.to_string())?;
                    if actual_condition != &expected_condition
                        || source_edge_id_to_mir(then_edge)? != *actual_then_edge
                        || source_edge_id_to_mir(else_edge)? != *actual_else_edge
                    {
                        return Err("ManagedAny MIR Branch disagrees with its exact Checker condition or edge roles".into());
                    }
                }
                (
                    crate::core::cfg::Terminator::Match { scrutinee, arms },
                    MirTerminator::Switch {
                        scrutinee: actual_scrutinee,
                        arms: actual_arms,
                    }
                    | MirTerminator::SwitchMove {
                        scrutinee: actual_scrutinee,
                        arms: actual_arms,
                    },
                ) => {
                    let expected_scrutinee = MirValueId::new(format!("expr:{}", scrutinee.0))
                        .map_err(|error| error.to_string())?;
                    let expected_arm_edges = arms
                        .iter()
                        .map(source_edge_id_to_mir)
                        .collect::<Result<Vec<_>, _>>()?;
                    let actual_arm_edges = actual_arms
                        .iter()
                        .map(|arm| arm.edge.clone())
                        .collect::<Vec<_>>();
                    let source_arm_receipts = receipt
                        .match_arms
                        .get(source_id)
                        .ok_or("ManagedAny Checker Match has no sealed pattern/case receipt")?;
                    if source_arm_receipts.len() != arms.len()
                        || source_arm_receipts.len() != actual_arms.len()
                    {
                        return Err(
                            "ManagedAny Checker Match arm receipt has incomplete case coverage"
                                .into(),
                        );
                    }
                    let mut expected_arm_cases = Vec::with_capacity(source_arm_receipts.len());
                    for ((source_edge_id, arm_receipt), actual_arm) in
                        arms.iter().zip(source_arm_receipts).zip(actual_arms)
                    {
                        let source_edge = receipt
                            .reachable_edges
                            .get(source_edge_id)
                            .ok_or("ManagedAny Checker Match arm edge is absent")?;
                        if arm_receipt.edge != *source_edge_id
                            || arm_receipt.source_arm != source_edge.source.node
                            || arm_receipt.guarded
                        {
                            return Err(
                                "ManagedAny Checker Match arm identity or guard is not admitted"
                                    .into(),
                            );
                        }
                        let expected_case = match &arm_receipt.case {
                            crate::core::ManagedAnyCfgMatchCase::Default => {
                                super::MirSwitchCase::Default
                            }
                            crate::core::ManagedAnyCfgMatchCase::Literal(literal) => {
                                super::MirSwitchCase::Literal(literal.clone())
                            }
                            crate::core::ManagedAnyCfgMatchCase::Variant(variant) => {
                                super::MirSwitchCase::Variant(variant.clone())
                            }
                            crate::core::ManagedAnyCfgMatchCase::Unsupported => {
                                return Err(
                                    "ManagedAny Checker Match pattern has no supported exact MIR switch case"
                                        .into(),
                                )
                            }
                        };
                        if actual_arm.edge != source_edge_id_to_mir(source_edge_id)?
                            || actual_arm.case != expected_case
                        {
                            return Err(
                                "ManagedAny MIR Switch case/edge disagrees with its exact Checker Match arm"
                                    .into(),
                            );
                        }
                        expected_arm_cases.push(expected_case);
                    }
                    if actual_scrutinee != &expected_scrutinee
                        || actual_arm_edges != expected_arm_edges
                        || actual_arms
                            .iter()
                            .map(|arm| arm.case.clone())
                            .collect::<Vec<_>>()
                            != expected_arm_cases
                    {
                        return Err("ManagedAny MIR Switch disagrees with its exact Checker scrutinee, cases, or arm edges".into());
                    }
                }
                (
                    crate::core::cfg::Terminator::Return { value, .. },
                    MirTerminator::Return {
                        value: actual_value,
                    },
                ) => {
                    let values_branch_join_return = seal
                        .v2_values_local_index_i32_result_branch
                        .as_ref()
                        .is_some_and(|branch| {
                            branch.owner == function.owner
                                && source_block.source.node == branch.map_new_match
                                && value.as_ref() == Some(&branch.map_new_match)
                                && function.result
                                    == crate::core::ir::primitive_type_id(PrimitiveType::I32)
                        });
                    let values_result_producer_join_return = is_direct_values_result_producer
                        .then_some(())
                        .and(
                            function
                                .ownership
                                .checker_ordinary_map_owned_any_v2_values_result_producer_receipt
                                .as_deref(),
                        )
                        .filter(|producer| {
                            value.is_some()
                                && mir_block.parameters.len() == 1
                                && mir_block.parameters[0].value.as_str()
                                    == format!("expr:{}", producer.map_new_match.0)
                                && source_block
                                    .points
                                    .iter()
                                    .any(|point| Some(&point.source.node) == value.as_ref())
                        });
                    let expected_value = if values_branch_join_return {
                        Some(
                            MirValueId::new(format!("join:{}:i32", source_id.0 .0))
                                .map_err(|error| error.to_string())?,
                        )
                    } else if let Some(producer) = values_result_producer_join_return {
                        Some(
                            MirValueId::new(format!("expr:{}", producer.map_new_match.0))
                                .map_err(|error| error.to_string())?,
                        )
                    } else {
                        value
                            .as_ref()
                            .map(|node| MirValueId::new(format!("expr:{}", node.0)))
                            .transpose()
                            .map_err(|error| error.to_string())?
                    };
                    let matched_return_unpack = actual_value.as_ref().is_some_and(|actual_id| {
                        value.is_some()
                            && actual_id.as_str().starts_with("return.unpack:")
                            && mir_block.instructions.iter().any(|instruction| {
                                matches!(
                                    &instruction.kind,
                                    MirInstructionKind::ManagedAny {
                                        result: Some(res),
                                        operation: MirManagedAnyOperation::UnpackI32
                                            | MirManagedAnyOperation::UnpackString,
                                        contract: Some(contract),
                                        ..
                                    } if res == actual_id
                                        && contract.owner == function.owner
                                        && actual_id.as_str()
                                            == format!("return.unpack:{}", contract.origin.0)
                                        && source_block
                                            .points
                                            .iter()
                                            .any(|point| point.source.node == contract.origin)
                                )
                            })
                    });
                    if actual_value != &expected_value && !matched_return_unpack {
                        return Err("ManagedAny MIR Return value disagrees with its exact Checker return node".into());
                    }
                }
                (
                    crate::core::cfg::Terminator::Diverge,
                    MirTerminator::Trap { .. }
                    | MirTerminator::Fault { .. }
                    | MirTerminator::Unreachable,
                )
                | (crate::core::cfg::Terminator::Unreachable, MirTerminator::Unreachable) => {}
                _ => {
                    return Err("ManagedAny MIR terminator kind disagrees with the mapped Checker CFG block".into());
                }
            }
        }

        // Bind every sealed instruction to an origin point in the exact
        // Checker block that maps to its MIR block. This also prevents moving
        // an instruction from one sibling arm to another while retaining its
        // private full-body seal.
        for (source_id, source_block) in &receipt.reachable_blocks {
            let mir_id = source_to_mir
                .get(source_id)
                .ok_or("ManagedAny Checker block has no MIR block mapping")?;
            let mir_block = function
                .blocks
                .get(mir_id)
                .ok_or("ManagedAny Checker block maps to an absent MIR block")?;
            for instruction in &mir_block.instructions {
                let origin = seal
                    .instructions
                    .iter()
                    .find(|(block, _, sealed)| block == mir_id && sealed.id == instruction.id)
                    .map(|(_, origin, _)| origin)
                    .ok_or("ManagedAny instruction is missing from its private body seal")?;
                let checker_point_matches = source_block
                    .points
                    .iter()
                    .any(|point| &point.source.node == origin);
                let synthesized_owned_string_drop = origin == &function.owner
                    && matches!(
                        &instruction.kind,
                        MirInstructionKind::Drop { value }
                            if function.values.get(value).is_some_and(|descriptor| {
                                self.owns_managed_string_drop_target(&descriptor.ty)
                            })
                    );
                if !checker_point_matches && !synthesized_owned_string_drop {
                    return Err(format!(
                        "ManagedAny instruction '{}' has no Checker origin point in its own source block",
                        instruction.id
                    ));
                }
            }
        }
        Ok(())
    }

    /// Whole-function owner accounting is shared by every consumer, including
    /// tuple-only and cross-frame callables with no direct GetAny operation.
    pub fn validate_managed_any_function(&self, function: &MirFunction) -> Result<(), String> {
        let static_closure_values = function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| match &instruction.kind {
                MirInstructionKind::ClosureBind { function_value, .. } => {
                    Some(function_value.clone())
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        self.validate_managed_any_function_with_static_closure_values(
            function,
            &static_closure_values,
        )
    }

    /// A Checker-lowered `ClosureBind` produces a static code identity, not an
    /// owned runtime Any payload. The local owner proof excludes those output
    /// identities; before execution or emission, the whole-program preflight
    /// must still validate the private closure receipt and invocation census.
    pub(super) fn validate_managed_any_function_with_static_closure_values(
        &self,
        function: &MirFunction,
        static_closure_values: &BTreeSet<MirValueId>,
    ) -> Result<(), String> {
        let v2_root_type = super::types::ordinary_map_owned_any_v2_type_id();
        if (function.result == v2_root_type
            && !has_checker_ordinary_map_owned_any_v2_map_root_producer_return(function))
            || function.parameters.iter().any(|parameter| {
                function
                    .values
                    .get(parameter)
                    .is_some_and(|value| value.ty == v2_root_type)
            })
        {
            return Err(
                "AnyOwner-v2 callable root parameters/returns need an exact Checker boundary receipt"
                    .into(),
            );
        }
        if function
            .ownership
            .ordinary_map_direct_tuple_get_unpack_l1_receipt()
            .is_some()
            && function
                .ownership
                .ordinary_map_direct_tuple_get_unpack_l2_projection()
                .is_none()
        {
            if !static_closure_values.is_empty() {
                return Err("direct tuple Get L1 graph cannot carry static closure values".into());
            }
            return validate_ordinary_map_direct_tuple_get_unpack_l1_types(function, self);
        }
        if function
            .ownership
            .checker_ordinary_map_owned_any_v2_direct_print_result_receipt
            .is_some()
        {
            if !static_closure_values.is_empty() {
                return Err("DirectPrintResult L1 graph cannot carry static closure values".into());
            }
            return validate_ordinary_map_direct_print_result_l1_types(function, self);
        }
        if function
            .ownership
            .checker_ordinary_map_owned_any_v2_set_get_any_result_i32_lineage
            .is_some()
        {
            validate_ordinary_map_set_get_any_result_i32_owner(function)?;
            return super::receipt::validate_ordinary_map_set_get_any_result_i32_result_types(
                function, self,
            );
        }
        if function
            .ownership
            .checker_ordinary_map_owned_any_v2_set_get_any_i32_lineage
            .is_some()
        {
            if function.ownership.checker_managed_any.is_some()
                || function.ownership.checker_managed_any_cfg_receipt.is_some()
            {
                return Err(
                    "ordinary Map Set/GetAny-i32 cannot mix its exact lineage receipt with generic ManagedAny seals".into(),
                );
            }
            return super::receipt::validate_ordinary_map_set_get_any_i32_result_types(
                function, self,
            );
        }
        let managed_values = managed_any_values_for_function(function, self, static_closure_values);
        let any_in_signature = self.contains_dynamic_or_managed_any(&function.result)
            || function.parameters.iter().any(|parameter| {
                function
                    .values
                    .get(parameter)
                    .is_some_and(|value| self.contains_dynamic_or_managed_any(&value.ty))
            });
        if managed_values.is_empty()
            && !any_in_signature
            && function.ownership.checker_managed_any.is_none()
        {
            return Ok(());
        }
        // Set/Get-String is projected mechanically from its own Checker
        // receipt. Its Get Ok payload is a detached OwnedString, while the
        // updated root remains live until the exact source cleanup point.
        if function
            .ownership
            .checker_ordinary_map_owned_any_v2_set_get_string_lineage
            .is_some()
        {
            if function.ownership.checker_managed_any.is_some()
                || function.ownership.checker_managed_any_cfg_receipt.is_some()
            {
                return Err(
                    "ordinary Map Set/Get-String cannot mix its exact lineage receipt with generic ManagedAny seals".into(),
                );
            }
            return super::receipt::validate_ordinary_map_set_get_string_result_types(
                function, self,
            );
        }
        // Set/Get-i32 is projected mechanically from its distinct full-source
        // receipt. It carries no generic ManagedAny authority or contract
        // semantics; validate only its exact closed MIR graph and TypeDesc.
        if function
            .ownership
            .checker_ordinary_map_owned_any_v2_set_get_i32_lineage
            .is_some()
        {
            if function.ownership.checker_managed_any.is_some()
                || function.ownership.checker_managed_any_cfg_receipt.is_some()
            {
                return Err(
                    "ordinary Map Set/Get-i32 cannot mix its exact lineage receipt with generic ManagedAny seals".into(),
                );
            }
            return super::receipt::validate_ordinary_map_set_get_i32_result_types(function, self);
        }
        // The SetResult execution island is projected mechanically from its
        // independent Checker SetLineage receipt rather than from the generic
        // CheckedBody lowering path, so it intentionally has no generic
        // ManagedAny body seal. Admit only that exact closed projection here;
        // the SetLineage receipt binds the complete source CFG and the MIR
        // validator below replays the entire operation graph and TypeDesc
        // contract. This branch carries no generic authority and remains
        // neutral for proof obligations.
        if function
            .ownership
            .checker_ordinary_map_owned_any_v2_set_lineage
            .is_some()
        {
            if function.ownership.checker_managed_any.is_some()
                || function.ownership.checker_managed_any_cfg_receipt.is_some()
            {
                return Err(
                    "ordinary Map SetResult cannot mix its exact lineage receipt with generic ManagedAny seals".into(),
                );
            }
            return super::receipt::validate_ordinary_map_set_result_types(function, self);
        }
        if let Some(branch) = function
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_deref()
            .and_then(|cfg| cfg.map_set_drop_control_result_branch_receipt.as_ref())
        {
            if !static_closure_values.is_empty() {
                return Err(
                    "ordinary Map MapSetDropControlResult graph cannot carry static closure values"
                        .into(),
                );
            }
            if function.ownership.checker_managed_any.is_some()
                || function.ownership.checker_managed_any_cfg_receipt.is_some()
            {
                return Err(
                    "ordinary Map MapSetDropControlResult cannot mix its exact Result-branch receipt with generic ManagedAny seals".into(),
                );
            }
            let result_type = super::types::ordinary_map_owned_any_v2_root_result_type_id();
            let root_type = super::types::ordinary_map_owned_any_v2_type_id();
            let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
            let Some(cfg) = function
                .ownership
                .checker_ordinary_map_owned_any_v2_cfg_receipt
                .as_deref()
            else {
                return Err(
                    "ordinary Map MapSetDropControlResult has no owner-v2 CFG receipt".into(),
                );
            };
            let source_result_type = cfg
                .actions
                .iter()
                .find(|action| {
                    action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::New
                        && action.point == branch.map_new_operation
                })
                .and_then(|action| action.source_type.as_ref())
                .ok_or_else(|| {
                    "ordinary Map MapSetDropControlResult New action has no source Result type"
                        .to_string()
                })?;
            self.validate_ordinary_map_owned_any_v2_root_result(&result_type, source_result_type)?;
            let new_result = super::MirValueId::new(format!("expr:{}", branch.map_new_operation.0))
                .map_err(|error| error.to_string())?;
            let set_result = super::MirValueId::new(format!("expr:{}", branch.map_set_operation.0))
                .map_err(|error| error.to_string())?;
            let initial_root =
                super::MirValueId::new(format!("local:{}", branch.initial_root.0 .0))
                    .map_err(|error| error.to_string())?;
            let populated_root =
                super::MirValueId::new(format!("local:{}", branch.populated_root.0 .0))
                    .map_err(|error| error.to_string())?;
            if function.result != i32_type
                || function
                    .values
                    .get(&new_result)
                    .is_none_or(|value| value.ty != result_type)
                || function
                    .values
                    .get(&set_result)
                    .is_none_or(|value| value.ty != result_type)
                || function
                    .values
                    .get(&initial_root)
                    .is_none_or(|value| value.ty != root_type)
                || function
                    .values
                    .get(&populated_root)
                    .is_none_or(|value| value.ty != root_type)
            {
                return Err(
                    "ordinary Map MapSetDropControlResult values do not use the exact root/Result TypeDesc profile".into(),
                );
            }
            return Ok(());
        }
        for (index, parameter) in function.parameters.iter().enumerate() {
            let is_any = function
                .values
                .get(parameter)
                .is_some_and(|value| self.contains_dynamic_or_managed_any(&value.ty));
            let permission = function
                .parameter_permissions
                .as_ref()
                .and_then(|permissions| permissions.get(index))
                .copied()
                .flatten();
            if is_any
                && matches!(
                    permission,
                    Some(crate::core::ir::Permission::View | crate::core::ir::Permission::Mutate)
                )
            {
                return Err(format!(
                    "ManagedAny parameter '{}' has a borrowed permission without a Checker loan/escape receipt",
                    parameter
                ));
            }
        }
        let seal = function
            .ownership
            .checker_managed_any
            .as_ref()
            .ok_or("ManagedAny tuple/parameter has no private Checker seal")?;
        let sealed_runtime_values = seal
            .managed_values
            .difference(static_closure_values)
            .cloned()
            .collect::<BTreeSet<_>>();
        if seal.generic_identity.is_some() && seal.generic_trap_cleanup.is_some() {
            return Err("ManagedAny function cannot carry two generic profile seals".into());
        }
        let observed = function
            .blocks
            .values()
            .flat_map(|block| {
                block
                    .instructions
                    .iter()
                    .map(move |instruction| (block.id.clone(), instruction.clone()))
            })
            .collect::<Vec<_>>();
        let actual_terminators = function
            .blocks
            .values()
            .map(|block| (block.id.clone(), block.terminator.clone()))
            .collect::<Vec<_>>();
        let (
            v2_map_root_producer_return,
            v2_map_root_producer_caller,
            v2_map_root_get_any_caller,
        ) = map_root_boundary_seal_snapshots(function).ok_or(
            "ManagedAny Map-root caller authority is partial or has conflicting Checker receipt pairs",
        )?;
        let v2_values_crosscall_string_caller = values_crosscall_string_caller_snapshot(function)
            .ok_or(
            "ManagedAny Values cross-call authority is present in mixed producer/caller roles",
        )?;
        let v2_values_result_producer_caller = values_result_producer_caller_snapshot(function)
            .ok_or(
                "ManagedAny Values Result producer/caller authority is present in mixed roles",
            )?;
        if seal.owner != function.owner
            || seal.parameters != function.parameters
            || seal.block_parameters
                != function
                    .blocks
                    .values()
                    .map(|block| (block.id.clone(), block.parameters.clone()))
                    .collect::<Vec<_>>()
            || seal.parameter_permissions != function.parameter_permissions
            || seal.result != function.result
            || sealed_runtime_values != managed_values
            || seal.source_cfg != function.ownership.checker_managed_any_cfg_receipt
            || seal.ordinary_map_owned_get_any_tuple_result_v1_snapshot
                != function
                    .ownership
                    .checker_ordinary_map_owned_get_any_tuple_result_v1_receipt
            || seal.v2_keys_snapshot_receipts
                != function
                    .ownership
                    .checker_ordinary_map_owned_any_v2_cfg_receipt
                    .as_ref()
                    .map(|receipt| receipt.keys_snapshot_receipts.clone())
                    .unwrap_or_default()
            || seal.v2_dynamic_keys_get_receipts
                != function
                    .ownership
                    .checker_ordinary_map_owned_any_v2_cfg_receipt
                    .as_ref()
                    .map(|receipt| receipt.dynamic_keys_get_receipts.clone())
                    .unwrap_or_default()
            || seal.v2_values_local_index_i32_result_branch
                != function
                    .ownership
                    .checker_ordinary_map_owned_any_v2_cfg_receipt
                    .as_ref()
                    .and_then(|receipt| {
                        receipt.values_local_index_i32_result_branch_receipt.clone()
                    })
            || seal.v2_values_producer_caller.as_ref()
                != function
                    .ownership
                    .checker_ordinary_map_owned_any_v2_values_caller_receipt
                    .as_deref()
            || seal.v2_values_crosscall_string_caller != v2_values_crosscall_string_caller
            || seal.v2_values_result_producer_caller != v2_values_result_producer_caller
            || seal.v2_map_root_producer_return != v2_map_root_producer_return
            || seal.v2_map_root_producer_caller != v2_map_root_producer_caller
            || seal.v2_map_root_get_any_caller != v2_map_root_get_any_caller
            || seal.terminators != actual_terminators
            || seal.value_types
                != function
                    .values
                    .iter()
                    .map(|(id, value)| (id.clone(), value.ty.clone()))
                    .collect()
            || observed
                != seal
                    .instructions
                    .iter()
                    .map(|(block, _, instruction)| (block.clone(), instruction.clone()))
                    .collect::<Vec<_>>()
        {
            return Err("ManagedAny private Checker seal disagrees with exact value/operation/parameter/return coverage".into());
        }
        self.validate_managed_any_source_cfg(function, seal)?;
        self.validate_managed_any_branch_local_set_diamond_snapshot(function, seal)?;
        if seal
            .v2_map_root_get_any_caller
            .as_ref()
            .is_some_and(|receipt| receipt.caller_owner == function.owner)
        {
            validate_checker_ordinary_map_owned_any_v2_map_root_get_any_caller_shape(function)?;
        }
        if let Some(identity) = &seal.generic_identity {
            self.validate_managed_any_generic_identity_shape(function, identity)?;
        }
        if let Some(trap_cleanup) = &seal.generic_trap_cleanup {
            self.validate_managed_any_generic_trap_cleanup_shape(function, trap_cleanup)?;
        }
        if let Some(producer) = function
            .ownership
            .checker_ordinary_map_owned_any_v2_values_result_producer_receipt
            .as_deref()
        {
            if let Some(error) = super::validate_ordinary_map_owned_any_v2_action_receipts(function)
                .into_iter()
                .next()
            {
                return Err(error.message);
            }
            let Some(cfg) = function
                .ownership
                .checker_ordinary_map_owned_any_v2_cfg_receipt
                .as_deref()
            else {
                return Err("ValuesResultProducer has no Checker CFG receipt".into());
            };
            let source_result_type = cfg
                .actions
                .iter()
                .find(|action| {
                    action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::New
                        && action.point == producer.map_new_operation
                })
                .and_then(|action| action.source_type.as_ref())
                .ok_or("ValuesResultProducer New action has no source Result type")?;
            self.validate_ordinary_map_owned_any_v2_root_result(
                &super::types::ordinary_map_owned_any_v2_root_result_type_id(),
                source_result_type,
            )?;
            self.validated_values_result_empty_list_construct_contract(
                &ordinary_map_owned_any_v2_values_type_id(),
            )?;
            return Ok(());
        }
        self.validate_managed_any_cfg_flow(function, &managed_values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ir::ResolvedType;
    use crate::core::mir::reference::MirProgram;
    use crate::core::mir::types::{MirFieldDesc, MirTypeDesc, MirVariantDesc};
    fn checked(source: &str) -> crate::core::CheckedProgram {
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("lex managed Any fixture");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse managed Any fixture");
        crate::core::check_program(&file).expect("check managed Any fixture")
    }

    fn keys_snapshot_fixture() -> (MirFunction, MirTypeCatalog) {
        let checked = checked(
            r#"
func main() -> i32 {
    match map_new() {
        Ok(empty) => {
            match map_set(empty, "answer", 41) {
                Ok(root) => {
                    let snapshot = keys(root)
                    drop(empty)
                    drop(root)
                    let key = snapshot[0]
                    drop(snapshot)
                    println(key)
                    drop(key)
                    0
                },
                Err(status) => {
                    drop(empty)
                    status
                },
            }
        },
        Err(status) => { status },
    }
}
"#,
        );
        let catalog =
            MirTypeCatalog::from_checked_program(&checked).expect("KeysSnapshot MIR type catalog");
        let callable = checked
            .callable(&NodeId("function:main".into()))
            .expect("KeysSnapshot main callable");
        let function =
            crate::core::mir::lower::lower_callable_with_type_catalog(callable, &catalog)
                .expect("Checker-owned KeysSnapshot lowers to MIR");
        (function, catalog)
    }

    fn values_crosscall_string_program() -> MirProgram {
        let checked = checked(include_str!(
            "../../../tests/fixtures/mir_ordinary_map_owned_any_v2_values_crosscall_string_caller.mimi"
        ));
        MirProgram::from_checked_program(&checked)
            .expect("exact cross-call Values String profile materializes")
    }

    fn values_crosscall_string_index1_program() -> MirProgram {
        let checked = checked(include_str!(
            "../../../tests/fixtures/mir_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1.mimi"
        ));
        MirProgram::from_checked_program(&checked)
            .expect("exact cross-call Values String Index1 profile materializes")
    }

    fn keys_snapshot_managed_values(
        function: &MirFunction,
        catalog: &MirTypeCatalog,
    ) -> BTreeSet<MirValueId> {
        managed_any_values_for_function(function, catalog, &BTreeSet::new())
    }

    fn assert_keys_snapshot_cfg_rejected(function: &MirFunction, catalog: &MirTypeCatalog) {
        let managed_values = keys_snapshot_managed_values(function, catalog);
        let error = catalog
            .validate_managed_any_cfg_flow(function, &managed_values)
            .expect_err("tampered KeysSnapshot owner contract is rejected");
        assert!(!error.is_empty());
    }

    fn keys_snapshot_values(function: &MirFunction) -> (MirValueId, MirValueId, MirValueId) {
        let cfg = function
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .expect("KeysSnapshot Checker CFG receipt");
        let [receipt] = cfg.keys_snapshot_receipts.as_slice() else {
            panic!("KeysSnapshot has one private source receipt");
        };
        let root = MirValueId::new(format!("local:{}", receipt.source_local.0 .0))
            .expect("Map-root local identity");
        let snapshot = MirValueId::new(format!("local:{}", receipt.snapshot_local.0 .0))
            .expect("snapshot local identity");
        let snapshot_result = MirValueId::new(format!("expr:{}", receipt.keys_operation.0))
            .expect("snapshot expression identity");
        (root, snapshot, snapshot_result)
    }

    fn keys_snapshot_key_local(function: &MirFunction) -> MirValueId {
        let cfg = function
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .expect("KeysSnapshot Checker CFG receipt");
        let [receipt] = cfg.keys_snapshot_receipts.as_slice() else {
            panic!("KeysSnapshot has one private source receipt");
        };
        MirValueId::new(format!("local:{}", receipt.key_local.0 .0))
            .expect("copied String key local identity")
    }

    #[test]
    fn keys_snapshot_cfg_borrows_root_and_settles_owned_list_snapshot() {
        let (function, catalog) = keys_snapshot_fixture();
        let (root, snapshot, snapshot_result) = keys_snapshot_values(&function);
        let managed_values = keys_snapshot_managed_values(&function, &catalog);
        let seal = function
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("private ManagedAny/KeysSnapshot seal");
        assert!(managed_values.contains(&snapshot_result));
        assert!(managed_values.contains(&snapshot));
        assert!(seal.managed_values.contains(&snapshot_result));
        assert!(seal.managed_values.contains(&snapshot));
        catalog
            .validate_managed_any_function(&function)
            .expect("Keys borrows the live Map root and its List<String> result is settled");
        assert!(function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(
                &instruction.kind,
                MirInstructionKind::OrdinaryMapOwnedAnyV2Keys {
                    result,
                    root: source,
                } if result == &snapshot_result && source == &root
            )));
    }

    #[test]
    fn managed_any_values_crosscall_string_return_uses_its_own_sealed_checker_receipt() {
        use crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion as Profile;

        let program = values_crosscall_string_program();
        let producer = program
            .functions()
            .get(&NodeId("function:snapshot_values".into()))
            .expect("cross-call Values producer");
        let cfg_receipt = producer
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .and_then(|cfg| cfg.values_crosscall_string_caller_receipt.as_ref())
            .expect("independent Checker cross-call receipt");
        let result_producer_receipt = producer
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .and_then(|cfg| cfg.values_result_producer_receipt.as_ref())
            .expect("independent Result producer receipt");
        assert_eq!(cfg_receipt.profile_version, Profile::V3);
        let seal = producer
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("private ManagedAny Checker seal");
        assert_eq!(
            seal.v2_values_crosscall_string_caller.as_ref(),
            Some(cfg_receipt),
            "the producer seal snapshots the source receipt"
        );
        assert_eq!(
            producer
                .ownership
                .checker_ordinary_map_owned_any_v2_values_result_producer_receipt
                .as_deref(),
            Some(result_producer_receipt),
        );
        assert!(producer
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .unwrap()
            .values_producer_caller_receipt
            .is_none());

        let catalog = program.type_catalog();
        catalog
            .validate_managed_any_function(producer)
            .expect("producer Values result passes the Result Values gate");

        let receipts = program
            .ordinary_map_owned_any_v2_values_result_crosscall_string_caller_receipts()
            .expect("full V3 producer/caller MIR receipt rederives");
        let [receipt] = receipts.as_slice() else {
            panic!("one exact V3 cross-call String receipt")
        };
        receipt
            .validate_against_program(&program)
            .expect("full V3 cross-call receipt revalidation succeeds");
        let caller = program
            .functions()
            .get(&receipt.caller_owner)
            .expect("cross-call String caller");
        let caller_seal = caller
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("private caller ManagedAny Checker seal");
        assert_eq!(
            caller_seal.v2_values_crosscall_string_caller.as_ref(),
            Some(&receipt.checker_caller),
            "the caller seal snapshots the same exact Checker receipt"
        );
        catalog
            .validate_managed_any_function(caller)
            .expect("caller Values Call passes its separate cross-call gate");
        catalog
            .validate_managed_any_cfg_flow(caller, &caller_seal.managed_values)
            .expect("caller receives and settles the exact Values owner");
    }

    #[test]
    fn managed_any_values_crosscall_string_index1_seals_only_the_sorted_two_entry_profile() {
        use crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion as Profile;

        let program = values_crosscall_string_index1_program();
        let receipts = program
            .ordinary_map_owned_any_v2_values_result_crosscall_string_caller_receipts()
            .expect("full V4 Index1 receipt rederives");
        let [receipt] = receipts.as_slice() else {
            panic!("one exact V4 Index1 cross-call receipt")
        };
        assert_eq!(receipt.checker_caller.profile_version, Profile::V4);
        assert_eq!(receipt.checker_caller.values_element_count, 2);
        assert_eq!(receipt.checker_caller.index_value, 1);
        assert_eq!(receipt.checker_caller.producer_sorted_keys, ["a", "b"]);
        receipt
            .validate_against_program(&program)
            .expect("V4 Index1 receipt remains bound to the exact MIR graph");

        let catalog = program.type_catalog();
        let producer = program
            .functions()
            .get(&receipt.producer_owner)
            .expect("Index1 producer");
        let producer_seal = producer
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("producer ManagedAny seal");
        assert_eq!(
            producer_seal.v2_values_crosscall_string_caller.as_ref(),
            Some(&receipt.checker_caller)
        );
        catalog
            .validate_managed_any_function(producer)
            .expect("producer seal accepts the exact V4 caller profile");

        let caller = program
            .functions()
            .get(&receipt.caller_owner)
            .expect("Index1 caller");
        let caller_seal = caller
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("caller ManagedAny seal");
        assert_eq!(
            caller_seal.v2_values_crosscall_string_caller.as_ref(),
            Some(&receipt.checker_caller)
        );
        catalog
            .validate_managed_any_function(caller)
            .expect("caller seal accepts index one only for the exact V4 profile");
        catalog
            .validate_managed_any_cfg_flow(caller, &caller_seal.managed_values)
            .expect("caller receives and settles the exact Values owner");

        let mut mixed = producer.clone();
        mixed
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_mut()
            .expect("producer CFG receipt")
            .values_crosscall_string_caller_receipt
            .as_mut()
            .expect("producer cross-call receipt")
            .profile_version = Profile::V3;
        assert!(
            catalog.validate_managed_any_function(&mixed).is_err(),
            "two entries at index one cannot be relabeled as V3"
        );
    }

    #[test]
    fn managed_any_values_crosscall_string_return_rejects_missing_tampered_and_mixed_receipts() {
        let program = values_crosscall_string_program();
        let producer_owner = NodeId("function:snapshot_values".into());
        let producer = program.functions().get(&producer_owner).unwrap().clone();
        let crosscall_receipts = program
            .ordinary_map_owned_any_v2_values_result_crosscall_string_caller_receipts()
            .expect("full V3 cross-call receipt rederives");
        let [crosscall_receipt] = crosscall_receipts.as_slice() else {
            panic!("one V3 cross-call receipt")
        };
        let caller_owner = crosscall_receipt.caller_owner.clone();
        let caller = program.functions().get(&caller_owner).unwrap().clone();
        let catalog = program.type_catalog();
        let previous = producer
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("producer's original private seal")
            .clone();

        let mut missing = producer.clone();
        missing
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_mut()
            .unwrap()
            .values_crosscall_string_caller_receipt = None;
        assert!(catalog.validate_managed_any_function(&missing).is_err());
        assert!(refresh_checked_lowering(&missing, catalog, &BTreeMap::new(), &previous).is_none());
        let mut missing_program = program.clone();
        missing_program.replace_function_for_test_only(missing);
        assert!(missing_program
            .ordinary_map_owned_any_v2_values_result_crosscall_string_caller_receipts()
            .is_err());

        let caller_previous = caller
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("caller's original private seal")
            .clone();
        let mut missing_caller = caller.clone();
        missing_caller
            .ownership
            .checker_ordinary_map_owned_any_v2_values_crosscall_string_caller_receipt = None;
        assert!(catalog
            .validate_managed_any_function(&missing_caller)
            .is_err());
        assert!(missing_caller.validate().is_err());
        assert!(refresh_checked_lowering(
            &missing_caller,
            catalog,
            &BTreeMap::new(),
            &caller_previous
        )
        .is_none());

        let mut tampered_caller = caller.clone();
        tampered_caller
            .ownership
            .checker_ordinary_map_owned_any_v2_values_crosscall_string_caller_receipt
            .as_mut()
            .unwrap()
            .call_target = NodeId("function:forged_producer".into());
        assert!(catalog
            .validate_managed_any_function(&tampered_caller)
            .is_err());
        assert!(tampered_caller.validate().is_err());

        let mut tampered = producer.clone();
        tampered
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_mut()
            .unwrap()
            .values_crosscall_string_caller_receipt
            .as_mut()
            .unwrap()
            .caller_owner = NodeId("function:forged_caller".into());
        assert!(catalog.validate_managed_any_function(&tampered).is_err());
        assert!(
            refresh_checked_lowering(&tampered, catalog, &BTreeMap::new(), &previous).is_none()
        );
        let mut tampered_program = program.clone();
        tampered_program.replace_function_for_test_only(tampered);
        assert!(tampered_program
            .ordinary_map_owned_any_v2_values_result_crosscall_string_caller_receipts()
            .is_err());

        let local_program = MirProgram::from_checked_program(&checked(include_str!(
            "../../../tests/fixtures/mir_ordinary_map_owned_any_v2_values_producer_caller.mimi"
        )))
        .expect("independent local ValuesResultProducerCaller fixture");
        let local_receipt = local_program
            .functions()
            .get(&producer_owner)
            .and_then(|function| {
                function
                    .ownership
                    .checker_ordinary_map_owned_any_v2_cfg_receipt
                    .as_ref()
            })
            .and_then(|cfg| cfg.values_result_producer_caller_receipt.clone())
            .expect("local Result producer-caller receipt");
        let local_caller_receipt = local_program
            .functions()
            .get(&caller_owner)
            .and_then(|function| {
                function
                    .ownership
                    .checker_ordinary_map_owned_any_v2_values_result_producer_caller_receipt
                    .clone()
            })
            .expect("local caller-side ValuesResultProducerCaller receipt");
        let mut mixed_caller = caller;
        mixed_caller
            .ownership
            .checker_ordinary_map_owned_any_v2_values_result_producer_caller_receipt =
            Some(local_caller_receipt);
        assert!(catalog
            .validate_managed_any_function(&mixed_caller)
            .is_err());
        assert!(mixed_caller.validate().is_err());

        let mut mixed = producer;
        mixed
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_mut()
            .unwrap()
            .values_result_producer_caller_receipt = Some(local_receipt);
        assert!(catalog.validate_managed_any_function(&mixed).is_err());
        let mut mixed_program = program.clone();
        mixed_program.replace_function_for_test_only(mixed);
        assert!(mixed_program
            .ordinary_map_owned_any_v2_values_result_crosscall_string_caller_receipts()
            .is_err());

        let mut duplicate_role = program.functions().get(&producer_owner).unwrap().clone();
        let checker_crosscall = duplicate_role
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .and_then(|cfg| cfg.values_crosscall_string_caller_receipt.clone())
            .expect("producer cross-call receipt");
        duplicate_role
            .ownership
            .checker_ordinary_map_owned_any_v2_values_crosscall_string_caller_receipt =
            Some(Box::new(checker_crosscall));
        assert!(catalog
            .validate_managed_any_function(&duplicate_role)
            .is_err());
        assert!(
            refresh_checked_lowering(&duplicate_role, catalog, &BTreeMap::new(), &previous)
                .is_none()
        );
    }

    #[test]
    fn keys_snapshot_cfg_rejects_mismatched_action_attempt_type_or_root() {
        use crate::core::{
            OrdinaryMapOwnedAnyV2ActionKind as ActionKind,
            OrdinaryMapOwnedAnyV2AttemptKind as AttemptKind,
        };

        let (original, catalog) = keys_snapshot_fixture();

        let mut wrong_action = original.clone();
        let cfg = wrong_action
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_mut()
            .expect("KeysSnapshot CFG receipt");
        let action = cfg
            .actions
            .iter_mut()
            .find(|action| action.kind == ActionKind::Keys)
            .expect("Keys source action");
        action.kind = ActionKind::Values;
        assert_keys_snapshot_cfg_rejected(&wrong_action, &catalog);

        let mut wrong_attempt = original.clone();
        let cfg = wrong_attempt
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_mut()
            .expect("KeysSnapshot CFG receipt");
        let original_attempt = cfg
            .attempts
            .iter()
            .find(|attempt| attempt.kind == AttemptKind::Keys)
            .expect("Keys source attempt")
            .clone();
        cfg.attempts.remove(&original_attempt);
        let mut forged_attempt = original_attempt;
        forged_attempt.receiver_local = None;
        cfg.attempts.insert(forged_attempt);
        assert_keys_snapshot_cfg_rejected(&wrong_attempt, &catalog);

        let mut wrong_type_function = original.clone();
        let (_, snapshot, snapshot_result) = keys_snapshot_values(&wrong_type_function);
        let wrong_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
        wrong_type_function
            .values
            .get_mut(&snapshot)
            .expect("snapshot local value")
            .ty = wrong_type.clone();
        wrong_type_function
            .values
            .get_mut(&snapshot_result)
            .expect("snapshot expression value")
            .ty = wrong_type;
        assert_keys_snapshot_cfg_rejected(&wrong_type_function, &catalog);

        let mut wrong_root = original;
        let (root, _, _) = keys_snapshot_values(&wrong_root);
        let other_root = wrong_root
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .find_map(|instruction| match &instruction.kind {
                MirInstructionKind::OrdinaryMapOwnedAnyV2New { result } => Some(result.clone()),
                _ => None,
            })
            .expect("base Map root");
        let keys = wrong_root
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Keys { root: actual, .. }
                        if actual == &root
                )
            })
            .expect("Keys operation");
        let MirInstructionKind::OrdinaryMapOwnedAnyV2Keys { root, .. } = &mut keys.kind else {
            unreachable!("selected Keys operation")
        };
        *root = other_root;
        assert_keys_snapshot_cfg_rejected(&wrong_root, &catalog);
    }

    #[test]
    fn keys_snapshot_cfg_rejects_root_escape_and_missing_or_duplicate_settlement() {
        let (original, catalog) = keys_snapshot_fixture();
        let (root, snapshot, _) = keys_snapshot_values(&original);
        let keys_block_id = original
            .blocks
            .iter()
            .find_map(|(id, block)| {
                block
                    .instructions
                    .iter()
                    .any(|inst| {
                        matches!(
                            &inst.kind,
                            MirInstructionKind::OrdinaryMapOwnedAnyV2Keys { .. }
                        )
                    })
                    .then(|| id.clone())
            })
            .expect("KeysSnapshot leaf block");

        let mut root_not_live = original.clone();
        let instructions = &mut root_not_live
            .blocks
            .get_mut(&keys_block_id)
            .expect("KeysSnapshot leaf block")
            .instructions;
        let keys_index = instructions
            .iter()
            .position(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Keys { .. }
                )
            })
            .expect("Keys operation");
        let root_drop_index = instructions
            .iter()
            .position(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { root: actual }
                        if actual == &root
                )
            })
            .expect("final Map-root Drop");
        instructions.swap(keys_index, root_drop_index);
        assert_keys_snapshot_cfg_rejected(&root_not_live, &catalog);

        let mut root_escape = original.clone();
        root_escape
            .blocks
            .get_mut(&keys_block_id)
            .expect("KeysSnapshot leaf block")
            .terminator = MirTerminator::Return {
            value: Some(root.clone()),
        };
        assert_keys_snapshot_cfg_rejected(&root_escape, &catalog);

        let mut missing_drop = original.clone();
        let block = missing_drop
            .blocks
            .get_mut(&keys_block_id)
            .expect("KeysSnapshot leaf block");
        block.instructions.retain(|instruction| {
            !matches!(&instruction.kind, MirInstructionKind::Drop { value } if value == &snapshot)
        });
        assert_keys_snapshot_cfg_rejected(&missing_drop, &catalog);

        let mut duplicate_drop = original;
        let block = duplicate_drop
            .blocks
            .get_mut(&keys_block_id)
            .expect("KeysSnapshot leaf block");
        let snapshot_drop = block
            .instructions
            .iter()
            .find(|instruction| {
                matches!(&instruction.kind, MirInstructionKind::Drop { value } if value == &snapshot)
            })
            .expect("snapshot Drop")
            .clone();
        let mut duplicate = snapshot_drop;
        duplicate.id = MirInstructionId::new("inst:drop:duplicate_keys_snapshot")
            .expect("unique duplicate Drop instruction ID");
        block.instructions.push(duplicate);
        assert_keys_snapshot_cfg_rejected(&duplicate_drop, &catalog);
    }

    #[test]
    fn keys_snapshot_cfg_requires_independent_copied_key_string_settlement() {
        let (original, catalog) = keys_snapshot_fixture();
        let key = keys_snapshot_key_local(&original);
        let keys_block_id = original
            .blocks
            .iter()
            .find_map(|(id, block)| {
                block
                    .instructions
                    .iter()
                    .any(|inst| {
                        matches!(
                            &inst.kind,
                            MirInstructionKind::OrdinaryMapOwnedAnyV2Keys { .. }
                        )
                    })
                    .then(|| id.clone())
            })
            .expect("KeysSnapshot leaf block");

        let mut missing_drop = original.clone();
        missing_drop
            .blocks
            .get_mut(&keys_block_id)
            .expect("KeysSnapshot leaf block")
            .instructions
            .retain(|instruction| {
                !matches!(&instruction.kind, MirInstructionKind::Drop { value } if value == &key)
            });
        assert_keys_snapshot_cfg_rejected(&missing_drop, &catalog);

        let mut duplicate_drop = original;
        let block = duplicate_drop
            .blocks
            .get_mut(&keys_block_id)
            .expect("KeysSnapshot leaf block");
        let key_drop = block
            .instructions
            .iter()
            .find(|instruction| {
                matches!(&instruction.kind, MirInstructionKind::Drop { value } if value == &key)
            })
            .expect("copied key Drop")
            .clone();
        let mut duplicate = key_drop;
        duplicate.id = MirInstructionId::new("inst:drop:duplicate_keys_string")
            .expect("unique duplicate key Drop instruction ID");
        block.instructions.push(duplicate);
        assert_keys_snapshot_cfg_rejected(&duplicate_drop, &catalog);
    }

    fn branch_local_set_diamond_fixture() -> (MirFunction, MirTypeCatalog) {
        let checked = checked(include_str!(
            "../../../tests/fixtures/mir_branch_local_persistent_map_set_diamond_true.mimi"
        ));
        let program = MirProgram::from_checked_program(&checked)
            .expect("branch-local persistent Set diamond MIR");
        (get_main(&program), program.type_catalog().clone())
    }

    fn branch_local_set_instructions(function: &MirFunction) -> Vec<MirInstruction> {
        function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Set { .. }
                )
            })
            .cloned()
            .collect()
    }

    fn generic_trap_cleanup_fixture() -> (
        crate::core::CheckedProgram,
        MirTypeCatalog,
        crate::core::ResolvedCallable,
        MirFunction,
        ResolvedTypeId,
    ) {
        let checked = checked(
            r#"
func fail<T>(value: T) -> i32 { let zero = 0; 1 / zero }
func main() -> i32 {
    match map_new() {
        Ok(empty) => {
            match map_set(empty, "key", "payload") {
                Ok(root) => {
                    let pair = map_get(root, "key")
                    drop(empty)
                    drop(root)
                    drop(pair)
                    0
                },
                Err(status) => {
                    drop(empty)
                    status
                },
            }
        },
        Err(status) => { status },
    }
}
"#,
        );
        let catalog = MirTypeCatalog::from_checked_program(&checked)
            .expect("generic trap cleanup type catalog");
        let callable = checked
            .callable(&NodeId("function:fail".into()))
            .expect("generic fail Checker callable")
            .clone();
        let function =
            crate::core::mir::lower::lower_callable_with_type_catalog(&callable, &catalog)
                .expect("generic fail template MIR");
        let type_argument = checked
            .resolved_types()
            .iter()
            .find_map(|(id, ty)| matches!(ty, ResolvedType::DynamicAny { .. }).then(|| id.clone()))
            .expect("Checker selected DynamicAny for Map.get");
        (checked, catalog, callable, function, type_argument)
    }

    #[test]
    fn generic_managed_any_trap_cleanup_seal_captures_checker_source_before_specialization() {
        let (checked, catalog, callable, function, type_argument) = generic_trap_cleanup_fixture();
        let proof = checked_managed_any_generic_trap_cleanup_template(
            &callable,
            &checked,
            &catalog,
            &function,
            std::slice::from_ref(&type_argument),
            super::super::MirManagedAnyTrapKind::DivisionByZeroI32,
        )
        .expect("exact generic Checker trap template is captured");

        assert_eq!(proof.receipt.template, callable.owner);
        assert_eq!(proof.receipt.type_argument, type_argument);
        assert_eq!(
            proof.receipt.source_parameter_local,
            callable.body.parameters[0]
        );
        assert_eq!(
            proof.receipt.parameter,
            MirValueId::new(format!("local:{}", callable.body.parameters[0].0 .0))
                .expect("parameter local identity")
        );
        assert_eq!(proof.receipt.source_cfg.owner, callable.owner);
        assert!(function.ownership.checker_managed_any.is_none());
    }

    #[test]
    fn generic_managed_any_trap_cleanup_seal_rejects_isomorphic_forged_value_identity() {
        let (checked, catalog, callable, function, type_argument) = generic_trap_cleanup_fixture();
        let mut forged = function;
        let entry = forged.entry.clone();
        let instructions = &mut forged
            .blocks
            .get_mut(&entry)
            .expect("generic entry")
            .instructions;
        let MirInstructionKind::Const { result, .. } = &mut instructions[0].kind else {
            panic!("initializer is a Const")
        };
        let original = result.clone();
        let forged_value =
            MirValueId::new("expr:forged-isomorphic-initializer").expect("forged stable value id");
        let mut info = forged
            .values
            .remove(&original)
            .expect("initializer value info");
        info.id = forged_value.clone();
        forged.values.insert(forged_value.clone(), info);
        *result = forged_value.clone();
        let MirInstructionKind::Move { source, .. } = &mut instructions[1].kind else {
            panic!("binding is a Move")
        };
        *source = forged_value;

        let error = checked_managed_any_generic_trap_cleanup_template(
            &callable,
            &checked,
            &catalog,
            &forged,
            std::slice::from_ref(&type_argument),
            super::super::MirManagedAnyTrapKind::DivisionByZeroI32,
        )
        .expect_err("isomorphic MIR renumbering cannot forge Checker NodeId identity");
        assert!(error.contains("identities"), "{error}");
    }

    #[test]
    fn generic_managed_any_trap_cleanup_seal_rejects_resealed_cfg_entry_node_forgery() {
        let (checked, catalog, mut callable, mut function, type_argument) =
            generic_trap_cleanup_fixture();
        let entry = callable.cfg.entry.clone();
        callable
            .cfg
            .blocks
            .get_mut(&entry)
            .expect("Checker CFG entry")
            .source
            .node = NodeId("forged:cfg-entry-node".into());
        let forged_cfg = crate::core::ManagedAnyCfgReceipt::from_cfg(&callable.cfg, &callable.body);
        callable.resources.managed_any_cfg_receipt = Some(forged_cfg.clone());
        function.ownership.checker_managed_any_cfg_receipt = Some(forged_cfg);

        let error = checked_managed_any_generic_trap_cleanup_template(
            &callable,
            &checked,
            &catalog,
            &function,
            std::slice::from_ref(&type_argument),
            super::super::MirManagedAnyTrapKind::DivisionByZeroI32,
        )
        .expect_err("copying changed CFG into its receipt cannot forge root-node mapping");
        assert!(
            error.contains("entry points") || error.contains("CFG"),
            "{error}"
        );
    }

    fn reseal_after_set_mutation(
        function: &mut MirFunction,
        catalog: &MirTypeCatalog,
        previous: &MirCheckerManagedAnySeal,
        mutate: impl FnOnce(&mut MirInstruction),
    ) {
        let original_set = branch_local_set_instructions(function)
            .into_iter()
            .next()
            .expect("first branch-local Set");
        let origin = previous
            .instructions
            .iter()
            .find(|(_, _, instruction)| instruction.id == original_set.id)
            .map(|(_, origin, _)| origin.clone())
            .expect("original Set source origin");
        let set = function
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| instruction.id == original_set.id)
            .expect("first branch-local Set");
        mutate(set);
        let refreshed_id = set.id.clone();
        let mut origins = previous
            .instructions
            .iter()
            .map(|(_, origin, instruction)| (instruction.id.clone(), origin.clone()))
            .collect::<BTreeMap<_, _>>();
        origins.insert(refreshed_id, origin);
        function.ownership.checker_managed_any = Some(
            refresh_checked_lowering(function, catalog, &origins, previous)
                .expect("re-seal mutation with the original Checker snapshots"),
        );
    }

    fn refresh_after_fixture_mutation(
        function: &mut MirFunction,
        catalog: &MirTypeCatalog,
        previous: &MirCheckerManagedAnySeal,
        additional_origins: &[(MirInstructionId, NodeId)],
    ) {
        let mut origins = previous
            .instructions
            .iter()
            .map(|(_, origin, instruction)| (instruction.id.clone(), origin.clone()))
            .collect::<BTreeMap<_, _>>();
        origins.extend(additional_origins.iter().cloned());
        let refreshed = refresh_checked_lowering(function, catalog, &origins, previous)
            .expect("refresh only from the original Checker snapshot");
        assert_eq!(
            refreshed.branch_local_set_diamond_v2_snapshot,
            previous.branch_local_set_diamond_v2_snapshot,
            "body refresh must preserve the original private V2 snapshot"
        );
        function.ownership.checker_managed_any = Some(refreshed);
    }

    fn validate_cfg_flow(function: &MirFunction, catalog: &MirTypeCatalog) -> Result<(), String> {
        let managed_values = managed_any_values_for_function(function, catalog, &BTreeSet::new());
        catalog.validate_managed_any_cfg_flow(function, &managed_values)
    }

    #[test]
    fn branch_local_set_diamond_authorizes_only_its_two_exact_checker_sets() {
        let (function, catalog) = branch_local_set_diamond_fixture();
        let sets = branch_local_set_instructions(&function);
        assert_eq!(sets.len(), 2);
        for instruction in &sets {
            assert!(catalog
                .managed_any_branch_local_set_instruction_is_authorized(&function, instruction));
        }
        validate_cfg_flow(&function, &catalog)
            .expect("the sealed two-arm Set diamond advances each live root");

        let mut ordinary_branch = function.clone();
        ordinary_branch
            .ownership
            .checker_managed_any
            .as_mut()
            .expect("private ManagedAny body seal")
            .branch_local_set_diamond_v2_snapshot = None;
        let ordinary_set = branch_local_set_instructions(&ordinary_branch)
            .into_iter()
            .next()
            .expect("Set remains in ordinary branch");
        assert!(
            !catalog.managed_any_branch_local_set_instruction_is_authorized(
                &ordinary_branch,
                &ordinary_set
            )
        );

        let mut remove = sets[0].clone();
        if let MirInstructionKind::OrdinaryMapOwnedAnyV2Set {
            result,
            source,
            key,
            ..
        } = &remove.kind
        {
            remove.kind = MirInstructionKind::OrdinaryMapOwnedAnyV2Remove {
                result: result.clone(),
                source: source.clone(),
                key: key.clone(),
            };
        }
        assert!(!catalog.managed_any_branch_local_set_instruction_is_authorized(&function, &remove));
    }

    #[test]
    fn branch_local_set_guard_rejects_mutated_set_instruction_id_and_operands_after_resealing() {
        let (original, catalog) = branch_local_set_diamond_fixture();
        let previous = original
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("original private body seal")
            .clone();
        for mutation in ["instruction", "result", "source", "key", "value"] {
            let mut forged = original.clone();
            reseal_after_set_mutation(&mut forged, &catalog, &previous, |instruction| {
                let MirInstructionKind::OrdinaryMapOwnedAnyV2Set {
                    result,
                    source,
                    key,
                    value,
                } = &mut instruction.kind
                else {
                    unreachable!("fixture's first Set instruction")
                };
                match mutation {
                    "instruction" => {
                        instruction.id =
                            MirInstructionId::new(format!("forged:{}", instruction.id.as_str()))
                                .expect("forged instruction id");
                    }
                    "result" => {
                        *result =
                            MirValueId::new("local:forged-set-result").expect("forged Set result");
                    }
                    "source" => {
                        *source =
                            MirValueId::new("local:forged-set-source").expect("forged Set source");
                    }
                    "key" => {
                        *key = MirValueId::new("expr:forged-set-key").expect("forged Set key");
                    }
                    "value" => {
                        *value =
                            MirValueId::new("expr:forged-set-value").expect("forged Set value");
                    }
                    _ => unreachable!(),
                }
            });
            assert_eq!(
                forged
                    .ownership
                    .checker_ordinary_map_owned_any_v2_cfg_receipt
                    .as_ref(),
                previous.branch_local_set_diamond_v2_snapshot.as_ref(),
                "the original Checker receipt remains unchanged for {mutation}"
            );
            let forged_set = branch_local_set_instructions(&forged)
                .into_iter()
                .next()
                .expect("mutated Set opcode remains present");
            assert!(!catalog
                .managed_any_branch_local_set_instruction_is_authorized(&forged, &forged_set));
            let error = validate_cfg_flow(&forged, &catalog)
                .expect_err("a forged arm Set fails its dedicated read-arm or Result gate");
            assert!(
                error.contains("cannot create an AnyOwner-v2 persistent root")
                    || error.contains(
                        "ManagedAny CFG AnyOwner-v2 Result SwitchMove lacks clean Checker AnyOwner-v2 flow receipts"
                    ),
                "mutation {mutation} must fail at the ManagedAny Set authorization gate: {error}"
            );
        }
    }

    #[test]
    fn branch_local_set_guard_rejects_missing_or_rewritten_private_arm_snapshot() {
        let (original, catalog) = branch_local_set_diamond_fixture();
        let previous = original
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("original private body seal")
            .clone();
        let mut missing_arm = original.clone();
        let origins = previous
            .instructions
            .iter()
            .map(|(_, origin, instruction)| (instruction.id.clone(), origin.clone()))
            .collect::<BTreeMap<_, _>>();
        let refreshed = refresh_checked_lowering(&missing_arm, &catalog, &origins, &previous)
            .expect("refresh with original snapshot before adversarial rewrite");
        missing_arm.ownership.checker_managed_any = Some(refreshed);
        let seal = missing_arm
            .ownership
            .checker_managed_any
            .as_mut()
            .expect("refreshed body seal");
        let snapshot = seal
            .branch_local_set_diamond_v2_snapshot
            .as_mut()
            .expect("private persistent Set snapshot");
        let flow = snapshot.flow.as_mut().expect("snapshot flow");
        let crate::core::ownership::OrdinaryMapOwnedAnyV2CfgFlowProof::BranchLocalPersistentSetDiamondV1(
            proof,
        ) = &mut flow.proof
        else {
            unreachable!("fixture's private proof kind")
        };
        let missing_edge = proof.arms.keys().next().expect("first arm edge").clone();
        proof.arms.remove(&missing_edge);
        let instruction = branch_local_set_instructions(&missing_arm)
            .into_iter()
            .next()
            .expect("original MIR Set");
        assert!(!catalog
            .managed_any_branch_local_set_instruction_is_authorized(&missing_arm, &instruction));
        assert!(catalog
            .validate_managed_any_function(&missing_arm)
            .expect_err("rewritten private snapshot disagrees with the original Checker receipt")
            .contains("receipt was removed or changed"));

        let mut wrong_arm = original.clone();
        let refreshed = refresh_checked_lowering(&wrong_arm, &catalog, &origins, &previous)
            .expect("refresh with original snapshot before adversarial rewrite");
        wrong_arm.ownership.checker_managed_any = Some(refreshed);
        let snapshot = wrong_arm
            .ownership
            .checker_managed_any
            .as_mut()
            .expect("refreshed body seal")
            .branch_local_set_diamond_v2_snapshot
            .as_mut()
            .expect("private persistent Set snapshot");
        let flow = snapshot.flow.as_mut().expect("snapshot flow");
        let crate::core::ownership::OrdinaryMapOwnedAnyV2CfgFlowProof::BranchLocalPersistentSetDiamondV1(
            proof,
        ) = &mut flow.proof
        else {
            unreachable!("fixture's private proof kind")
        };
        let first_arm = proof.arms.values_mut().next().expect("first proof arm");
        first_arm.set.point = NodeId("forged:branch-local-set-point".into());
        let instruction = branch_local_set_instructions(&wrong_arm)
            .into_iter()
            .next()
            .expect("original MIR Set");
        assert!(!catalog
            .managed_any_branch_local_set_instruction_is_authorized(&wrong_arm, &instruction));
        assert!(catalog
            .validate_managed_any_function(&wrong_arm)
            .expect_err("wrong private arm disagrees with original Checker receipt")
            .contains("receipt was removed or changed"));

        let mut no_snapshot = original;
        no_snapshot
            .ownership
            .checker_managed_any
            .as_mut()
            .expect("private body seal")
            .branch_local_set_diamond_v2_snapshot = None;
        let ordinary_set = branch_local_set_instructions(&no_snapshot)
            .into_iter()
            .next()
            .expect("Map Set in an unsealed ordinary branch");
        assert!(!catalog
            .managed_any_branch_local_set_instruction_is_authorized(&no_snapshot, &ordinary_set));
        assert!(catalog
            .validate_managed_any_function(&no_snapshot)
            .expect_err(
                "a branch-local profile without its original private commitment is rejected"
            )
            .contains("no original private Checker commitment"));
    }

    #[test]
    fn branch_local_set_refresh_preserves_original_snapshot_after_v2_receipt_replacement_or_removal(
    ) {
        let (original, catalog) = branch_local_set_diamond_fixture();
        let previous = original
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("original private body seal")
            .clone();

        let mut replaced = original.clone();
        let current = replaced
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_mut()
            .expect("current V2 CFG receipt");
        current.actions[0].point = NodeId("forged:v2-action-point".into());
        refresh_after_fixture_mutation(&mut replaced, &catalog, &previous, &[]);
        let replaced_seal = replaced
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("refreshed body seal");
        assert_eq!(
            replaced_seal.branch_local_set_diamond_v2_snapshot,
            previous.branch_local_set_diamond_v2_snapshot
        );
        assert!(catalog
            .validate_managed_any_function(&replaced)
            .expect_err("replaced mutable V2 receipt cannot refresh private authority")
            .contains("receipt was removed or changed after initial lowering"));

        let mut removed = original;
        removed
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt = None;
        refresh_after_fixture_mutation(&mut removed, &catalog, &previous, &[]);
        let removed_seal = removed
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("refreshed body seal");
        assert_eq!(
            removed_seal.branch_local_set_diamond_v2_snapshot,
            previous.branch_local_set_diamond_v2_snapshot
        );
        assert!(catalog
            .validate_managed_any_function(&removed)
            .expect_err("removed mutable V2 receipt cannot refresh private authority")
            .contains("receipt was removed or changed after initial lowering"));
    }

    #[test]
    fn branch_local_set_refresh_rejects_reordered_arm_clone_set_and_temporary_drop() {
        let (original, catalog) = branch_local_set_diamond_fixture();
        let previous = original
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("original private body seal")
            .clone();
        let snapshot = previous
            .branch_local_set_diamond_v2_snapshot
            .as_ref()
            .expect("original branch-local Set snapshot");
        let flow = snapshot.flow.as_ref().expect("snapshot flow");
        let crate::core::ownership::OrdinaryMapOwnedAnyV2CfgFlowProof::BranchLocalPersistentSetDiamondV1(
            proof,
        ) = &flow.proof
        else {
            unreachable!("fixture's private proof kind")
        };
        let arm = proof.arms.values().next().expect("one proof arm");
        let clone_id = MirInstructionId::new(format!("inst:clone:{}", arm.payload_load.0))
            .expect("exact payload Clone id");
        let set_result = MirValueId::new(format!("local:{}", arm.set.local.0 .0))
            .expect("exact child root value");
        let set_id = MirInstructionId::new(format!(
            "inst:ordinary_map_owned_any_v2_set:{set_result}:{}",
            arm.set.point.0
        ))
        .expect("exact arm Set id");
        let temporary_drop_id = MirInstructionId::new(format!(
            "inst:drop.ordinary-map-owned-any-v2-value:{}",
            arm.set.point.0
        ))
        .expect("exact temporary Drop id");

        let mut reordered = original;
        let arm_block_id = reordered
            .blocks
            .iter()
            .find_map(|(block_id, block)| {
                block
                    .instructions
                    .iter()
                    .any(|instruction| instruction.id == set_id)
                    .then(|| block_id.clone())
            })
            .expect("MIR block containing the exact sealed arm Set");
        let block = reordered
            .blocks
            .get_mut(&arm_block_id)
            .expect("exact MIR arm block");
        let clone_index = block
            .instructions
            .iter()
            .position(|instruction| instruction.id == clone_id)
            .expect("payload Clone");
        let set_index = block
            .instructions
            .iter()
            .position(|instruction| instruction.id == set_id)
            .expect("branch Set");
        let drop_index = block
            .instructions
            .iter()
            .position(|instruction| instruction.id == temporary_drop_id)
            .expect("temporary Drop");
        assert!(clone_index < set_index && set_index < drop_index);
        block.instructions.swap(clone_index, set_index);
        refresh_after_fixture_mutation(&mut reordered, &catalog, &previous, &[]);
        let error = catalog
            .validate_managed_any_function(&reordered)
            .expect_err("refreshed seal does not authorize a reordered Set arm");
        assert!(
            error.contains("branch-local Set diamond MIR no longer satisfies its Checker proof")
                || error.contains(
                    "branch-local Set arm Clone/Set/temporary-Drop instructions disagree"
                ),
            "{error}"
        );
    }

    #[test]
    fn branch_local_set_refresh_rejects_a_third_unreceipted_set_in_an_arm() {
        let (original, catalog) = branch_local_set_diamond_fixture();
        let previous = original
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("original private body seal")
            .clone();
        let snapshot = previous
            .branch_local_set_diamond_v2_snapshot
            .as_ref()
            .expect("original branch-local Set snapshot");
        let flow = snapshot.flow.as_ref().expect("snapshot flow");
        let crate::core::ownership::OrdinaryMapOwnedAnyV2CfgFlowProof::BranchLocalPersistentSetDiamondV1(
            proof,
        ) = &flow.proof
        else {
            unreachable!("fixture's private proof kind")
        };
        let arm = proof.arms.values().next().expect("one proof arm");
        let set_result = MirValueId::new(format!("local:{}", arm.set.local.0 .0))
            .expect("exact child root value");
        let set_id = MirInstructionId::new(format!(
            "inst:ordinary_map_owned_any_v2_set:{set_result}:{}",
            arm.set.point.0
        ))
        .expect("exact arm Set id");
        let arm_block_id = original
            .blocks
            .iter()
            .find_map(|(block_id, block)| {
                block
                    .instructions
                    .iter()
                    .any(|instruction| instruction.id == set_id)
                    .then(|| block_id.clone())
            })
            .expect("MIR block containing the exact sealed arm Set");
        let block = original
            .blocks
            .get(&arm_block_id)
            .expect("exact MIR arm block");
        let set_index = block
            .instructions
            .iter()
            .position(|instruction| instruction.id == set_id)
            .expect("branch Set");
        let mut extra_set = block.instructions[set_index].clone();
        extra_set.id = MirInstructionId::new("inst:forged_branch_local_set_extra")
            .expect("unique synthetic Set id");

        let mut forged = original;
        forged
            .blocks
            .get_mut(&arm_block_id)
            .expect("exact MIR arm block")
            .instructions
            .insert(set_index + 1, extra_set.clone());
        refresh_after_fixture_mutation(
            &mut forged,
            &catalog,
            &previous,
            &[(extra_set.id.clone(), arm.set.point.clone())],
        );
        assert!(
            !catalog.managed_any_branch_local_set_instruction_is_authorized(&forged, &extra_set)
        );
        let error = catalog
            .validate_managed_any_function(&forged)
            .expect_err("the exact Checker action census rejects the third Set");
        assert!(
            error.contains("MIR no longer satisfies its Checker proof"),
            "{error}"
        );
        assert!(
            error.contains("receipt count 11 disagrees with operation count 12"),
            "{error}"
        );
    }

    #[test]
    fn branch_local_set_refresh_rejects_an_extra_payload_clone_value_use() {
        let (original, catalog) = branch_local_set_diamond_fixture();
        let previous = original
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("original private body seal")
            .clone();
        let snapshot = previous
            .branch_local_set_diamond_v2_snapshot
            .as_ref()
            .expect("original branch-local Set snapshot");
        let flow = snapshot.flow.as_ref().expect("snapshot flow");
        let crate::core::ownership::OrdinaryMapOwnedAnyV2CfgFlowProof::BranchLocalPersistentSetDiamondV1(
            proof,
        ) = &flow.proof
        else {
            unreachable!("fixture's private proof kind")
        };
        let arm = proof.arms.values().next().expect("one proof arm");
        let cloned = MirValueId::new(format!("expr:{}", arm.payload_load.0))
            .expect("exact cloned payload value");
        let temporary_drop_id = MirInstructionId::new(format!(
            "inst:drop.ordinary-map-owned-any-v2-value:{}",
            arm.set.point.0
        ))
        .expect("exact temporary Drop id");
        let arm_block_id = original
            .blocks
            .iter()
            .find_map(|(block_id, block)| {
                block
                    .instructions
                    .iter()
                    .any(|instruction| instruction.id == temporary_drop_id)
                    .then(|| block_id.clone())
            })
            .expect("MIR block containing the exact sealed temporary Drop");
        let block = original
            .blocks
            .get(&arm_block_id)
            .expect("exact MIR arm block");
        let drop_index = block
            .instructions
            .iter()
            .position(|instruction| instruction.id == temporary_drop_id)
            .expect("temporary Drop");
        let extra_drop = MirInstruction {
            id: MirInstructionId::new("inst:forged_branch_local_payload_drop")
                .expect("unique synthetic payload Drop id"),
            kind: MirInstructionKind::Drop {
                value: cloned.clone(),
            },
        };

        let mut forged = original;
        forged
            .blocks
            .get_mut(&arm_block_id)
            .expect("exact MIR arm block")
            .instructions
            .insert(drop_index + 1, extra_drop.clone());
        refresh_after_fixture_mutation(
            &mut forged,
            &catalog,
            &previous,
            &[(extra_drop.id.clone(), arm.set.point.clone())],
        );
        let error = catalog
            .validate_managed_any_function(&forged)
            .expect_err("the exact payload-clone value-use census rejects an extra Drop");
        assert!(
            error.contains(
                "payload Clone must be used only by its Set and one exact temporary Drop"
            ),
            "{error}"
        );
    }

    #[test]
    fn ordinary_scalar_functions_skip_managed_any_receipt_gate_but_any_markers_do_not() {
        let program = MirProgram::from_checked_program(&checked(
            r#"
func main() -> i32 {
    let one = 1
    one + 2
}
"#,
        ))
        .expect("ordinary scalar MIR must not require a ManagedAny Checker seal");
        let scalar = get_main(&program);
        assert!(scalar.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|instruction| matches!(&instruction.kind, MirInstructionKind::Const { .. }))
                && block.instructions.iter().any(|instruction| {
                    matches!(&instruction.kind, MirInstructionKind::Binary { .. })
                })
        }));
        assert!(validate_managed_any_receipts(&scalar).is_empty());

        let source = scalar
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .find_map(|instruction| match &instruction.kind {
                MirInstructionKind::Const { result, .. } => Some(result.clone()),
                _ => None,
            })
            .expect("scalar Const operand");
        let mut forged_opcode = scalar.clone();
        let entry_id = forged_opcode.entry.clone();
        let entry = forged_opcode
            .blocks
            .get_mut(&entry_id)
            .expect("scalar entry block");
        entry.instructions.push(MirInstruction {
            id: MirInstructionId::new("forged:bare-managed-any-drop").expect("forged operation id"),
            kind: MirInstructionKind::ManagedAny {
                result: None,
                source,
                operation: MirManagedAnyOperation::Drop,
                contract: None,
            },
        });
        assert!(validate_managed_any_receipts(&forged_opcode)
            .iter()
            .any(|error| error.message.contains("private Checker seal is missing")));

        let mut forged_tuple_marker = scalar;
        let marker =
            MirValueId::new("forged:ordinary-map-get-any-tuple").expect("tuple marker value id");
        forged_tuple_marker.values.insert(
            marker.clone(),
            super::super::MirValue {
                id: marker,
                ty: ordinary_map_get_any_tuple_i32_string_v1_type_id(),
            },
        );
        assert!(validate_managed_any_receipts(&forged_tuple_marker)
            .iter()
            .any(|error| error.message.contains("private Checker seal is missing")));
    }

    #[test]
    fn managed_any_source_cfg_receipt_binds_match_arm_cases_in_checker_order() {
        let checked = checked(
            r#"
func main() -> i32 {
    let selector = 5
    match selector {
        5 => 17,
        _ => 23,
    }
}
"#,
        );
        let owner = NodeId("function:main".into());
        let cfg = checked.callable_cfg(&owner).expect("main source CFG");
        let body = checked.resolved_body(&owner).expect("main ResolvedBody");
        let receipt = crate::core::ManagedAnyCfgReceipt::from_cfg(cfg, body);
        assert_eq!(receipt.match_arms.len(), 1);
        let arms = receipt
            .match_arms
            .values()
            .next()
            .expect("one Checker match with its sealed arm sequence");
        assert_eq!(arms.len(), 2);
        assert!(matches!(
            arms[0].case,
            crate::core::ManagedAnyCfgMatchCase::Literal(ResolvedLiteral::Int(5))
        ));
        assert!(matches!(
            arms[1].case,
            crate::core::ManagedAnyCfgMatchCase::Default
        ));
        assert_eq!(cfg.edges[&arms[0].edge].source.node, arms[0].source_arm);
        assert!(!arms[0].guarded && !arms[1].guarded);
    }

    const SOURCE: &str = r#"
func carry<T>(value: T) -> T { value }
func main() -> i32 {
    match map_new() {
        Ok(empty) => {
            match map_set(empty, "k\0🧪", "owned\0🧪值") {
                Ok(root) => {
                    let pair = map_get(root, "k\0🧪")
                    drop(empty)
                    drop(root)

                    let found = pair.0
                    let payload = pair.1
                    let carried = carry(payload)
                    let equal = if found { carried == "owned\0🧪值" } else { false }
                    println(found)
                    println(equal)
                    drop(pair)
                    drop(payload)
                    drop(carried)

                    match map_new() {
                        Ok(number_empty) => {
                            match map_set(number_empty, "zero", 0) {
                                Ok(number_root) => {
                                    let number_pair = map_get(number_root, "zero")
                                    drop(number_empty)
                                    drop(number_root)

                                    let number_found = number_pair.0
                                    let zero_payload = number_pair.1
                                    let zero_carried = carry(zero_payload)
                                    let zero = if number_found { zero_carried } else { 0 }
                                    println(number_found)
                                    println(zero)
                                    drop(number_pair)
                                    drop(zero_payload)
                                    drop(zero_carried)
                                    0
                                },
                                Err(status) => {
                                    drop(number_empty)
                                    status
                                },
                            }
                        },
                        Err(status) => { status },
                    }
                },
                Err(status) => {
                    drop(empty)
                    status
                },
            }
        },
        Err(status) => { status },
    }
}
"#;

    #[test]
    fn managed_any_println_uses_checker_dynamic_any_borrow_then_drops_only_its_temporary() {
        let program = MirProgram::from_checked_program(&checked(SOURCE))
            .expect("Checker-sealed Any println fixture");
        let function = get_main(&program);
        let catalog = program.type_catalog();
        let managed_type = managed_any_i32_string_v1_type_id();
        let unit_type = crate::core::ir::primitive_type_id(PrimitiveType::Unit);
        let mut found = false;
        for block in function.blocks.values() {
            for (index, instruction) in block.instructions.iter().enumerate() {
                let MirInstructionKind::ManagedAny {
                    result: Some(result),
                    source,
                    operation: MirManagedAnyOperation::Println,
                    contract: Some(contract),
                } = &instruction.kind
                else {
                    continue;
                };
                found = true;
                assert_eq!(contract.owner, function.owner);
                assert_eq!(
                    contract.origin,
                    NodeId("function:main/node:expr.call@unknown-source:35:37-35:50".into())
                );
                assert_eq!(&contract.source, source);
                assert_eq!(contract.result.as_ref(), Some(result));
                assert_eq!(contract.source_type, managed_type);
                assert_eq!(contract.target_type.as_ref(), Some(&unit_type));
                assert_eq!(contract.checker_conversion, None);
                assert_eq!(contract.expected_kind, None);
                let checker_source = contract
                    .checker_source_type
                    .as_ref()
                    .expect("exact Checker Any argument identity");
                let descriptor = catalog
                    .get(checker_source)
                    .expect("Checker source TypeDesc");
                assert_eq!(descriptor.kind, MirTypeKind::DynamicAny);
                assert_eq!(
                    catalog.canonical_managed_any_type(checker_source),
                    managed_type
                );
                assert_eq!(
                    function.values.get(source).map(|value| &value.ty),
                    Some(&managed_type)
                );
                assert_eq!(
                    function.values.get(result).map(|value| &value.ty),
                    Some(&unit_type)
                );
                assert!(matches!(
                    block.instructions.get(index + 1).map(|next| &next.kind),
                    Some(MirInstructionKind::ManagedAny {
                        source: dropped,
                        operation: MirManagedAnyOperation::Drop,
                        ..
                    }) if dropped == source
                ));
                assert!(
                    !block.instructions.iter().any(|other| matches!(
                        &other.kind,
                        MirInstructionKind::BuiltinCall {
                            kind: super::super::types::MirBuiltinKind::PrintlnInt
                                | super::super::types::MirBuiltinKind::PrintlnString,
                            arguments,
                            ..
                        } if arguments.contains(source)
                    )),
                    "Any output must not escape as a scalar/string BuiltinCall"
                );
                catalog
                    .validate_managed_any_contract(
                        &function,
                        source,
                        Some(result),
                        MirManagedAnyOperation::Println,
                        Some(contract),
                    )
                    .expect("the borrowed print contract is exact");
            }
        }
        assert!(
            found,
            "the Any println must lower to its dedicated borrow operation"
        );
        catalog
            .validate_managed_any_function(&function)
            .expect("the complete Checker body and owner flow are sealed");
    }

    #[test]
    fn managed_any_println_rejects_dropping_the_same_snapshot_before_the_borrow() {
        let program = MirProgram::from_checked_program(&checked(SOURCE))
            .expect("Checker-sealed Any println fixture");
        let catalog = program.type_catalog();
        let valid_function = get_main(&program);
        catalog
            .validate_managed_any_function(&valid_function)
            .expect("the live-println-drop control shape is admitted");

        let mut forged_function = valid_function;
        let mut dropped_snapshot = None;
        for block in forged_function.blocks.values_mut() {
            for index in 0..block.instructions.len().saturating_sub(1) {
                let Some((source, checker_source)) =
                    block
                        .instructions
                        .get(index)
                        .and_then(|instruction| match &instruction.kind {
                            MirInstructionKind::ManagedAny {
                                source,
                                operation: MirManagedAnyOperation::Println,
                                contract: Some(contract),
                                ..
                            } => Some((source.clone(), contract.checker_source_type.clone())),
                            _ => None,
                        })
                else {
                    continue;
                };
                let checker_source = checker_source.expect("sealed Checker source TypeId");
                if !catalog
                    .get(&checker_source)
                    .is_some_and(|descriptor| descriptor.kind == MirTypeKind::DynamicAny)
                {
                    continue;
                }
                assert!(matches!(
                    block.instructions.get(index + 1).map(|next| &next.kind),
                    Some(MirInstructionKind::ManagedAny {
                        source: dropped,
                        operation: MirManagedAnyOperation::Drop,
                        ..
                    }) if dropped == &source
                ));
                assert!(
                    dropped_snapshot.replace(source.clone()).is_none(),
                    "the fixture has one exact DynamicAny Println snapshot"
                );
                // This is the same argument snapshot owner that Println borrows,
                // not its still-live source local or an independently-owned clone.
                block.instructions.swap(index, index + 1);
                break;
            }
            if dropped_snapshot.is_some() {
                break;
            }
        }
        let dropped_snapshot = dropped_snapshot.expect("DynamicAny Println snapshot");

        let managed_values = forged_function
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("the original body has an exact Checker seal")
            .managed_values
            .clone();
        assert!(managed_values.contains(&dropped_snapshot));
        let flow_error = catalog
            .validate_managed_any_cfg_flow(&forged_function, &managed_values)
            .expect_err("the exact owner is dead when Println attempts to borrow it");
        assert!(
            flow_error.contains("used after transfer, drop, or before creation"),
            "unexpected owner-flow rejection: {flow_error}"
        );

        let mut forged_program = program.clone();
        forged_program.replace_function_for_test_only(forged_function);
        let route_receipt = forged_program
            .route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND);
        let interpreter =
            crate::core::mir::reference::MirReferenceInterpreter::new(&forged_program)
                .with_route_receipt(&route_receipt);
        let error = interpreter
            .execute_with_output(&NodeId("function:main".into()), &[])
            .expect_err("the source-bound MIR snapshot cannot be dropped before Println");
        assert!(error.to_string().contains("ManagedAny"), "{error}");
        assert_eq!(
            interpreter.captured_output(),
            "",
            "whole-program receipt preflight rejects the forged body before earlier Println effects"
        );
    }

    #[test]
    fn managed_any_println_preserves_the_string_payload_tag_and_source_owner() {
        let program = MirProgram::from_checked_program(&checked(
            r#"
func main() -> i32 {
    match map_new() {
        Ok(empty) => {
            match map_set(empty, "k", "string payload") {
                Ok(root) => {
                    let pair = map_get(root, "k")
                    drop(empty)
                    drop(root)
                    let payload = pair.1
                    println(payload)
                    drop(pair)
                    drop(payload)
                    0
                },
                Err(status) => {
                    drop(empty)
                    status
                },
            }
        },
        Err(status) => { status },
    }
}
"#,
        ))
        .expect("Checker-sealed String-tag Any println fixture");
        let function = get_main(&program);
        let catalog = program.type_catalog();
        let unit_type = crate::core::ir::primitive_type_id(PrimitiveType::Unit);
        let print = function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .find_map(|instruction| match &instruction.kind {
                MirInstructionKind::ManagedAny {
                    result: Some(result),
                    source,
                    operation: MirManagedAnyOperation::Println,
                    contract: Some(contract),
                } => Some((source, result, contract)),
                _ => None,
            })
            .expect("Any String println uses the typed operation");
        assert_eq!(print.2.source_type, managed_any_i32_string_v1_type_id());
        assert_eq!(print.2.target_type.as_ref(), Some(&unit_type));
        assert!(catalog
            .get(
                print
                    .2
                    .checker_source_type
                    .as_ref()
                    .expect("Any source type")
            )
            .is_some_and(|descriptor| descriptor.kind == MirTypeKind::DynamicAny));
        assert!(function
            .blocks
            .values()
            .any(|block| block.instructions.windows(2).any(|pair| {
                matches!(
                    &pair[0].kind,
                    MirInstructionKind::ManagedAny {
                        source,
                        operation: MirManagedAnyOperation::Println,
                        ..
                    } if source == print.0
                ) && (matches!(
                    &pair[1].kind,
                    MirInstructionKind::Drop { value } if value == print.0
                ) || matches!(
                    &pair[1].kind,
                    MirInstructionKind::ManagedAny {
                        source,
                        operation: MirManagedAnyOperation::Drop,
                        ..
                    } if source == print.0
                ))
            })));
        catalog
            .validate_managed_any_function(&function)
            .expect("the borrowed String-tag print leaves the source owner available");
    }

    #[test]
    fn managed_any_println_rejects_missing_or_non_dynamic_checker_source_identity() {
        let program = MirProgram::from_checked_program(&checked(SOURCE))
            .expect("Checker-sealed Any println fixture");
        let mut function = get_main(&program);
        let unit_type = crate::core::ir::primitive_type_id(PrimitiveType::Unit);
        let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
        let (source, result, contract) = function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .find_map(|instruction| match &instruction.kind {
                MirInstructionKind::ManagedAny {
                    result: Some(result),
                    source,
                    operation: MirManagedAnyOperation::Println,
                    contract: Some(contract),
                } => Some((source.clone(), result.clone(), contract.clone())),
                _ => None,
            })
            .expect("Checker-sealed Any println");
        let catalog = program.type_catalog();
        let mut missing = contract.clone();
        missing.checker_source_type = None;
        assert!(catalog
            .validate_managed_any_contract(
                &function,
                &source,
                Some(&result),
                MirManagedAnyOperation::Println,
                Some(&missing),
            )
            .is_err());
        let mut wrong = contract;
        wrong.checker_source_type = Some(i32_type);
        assert!(catalog
            .validate_managed_any_contract(
                &function,
                &source,
                Some(&result),
                MirManagedAnyOperation::Println,
                Some(&wrong),
            )
            .is_err());
        assert_eq!(
            function.values.get(&result).map(|value| &value.ty),
            Some(&unit_type)
        );
        let forged_instruction = function
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::ManagedAny {
                        operation: MirManagedAnyOperation::Println,
                        ..
                    }
                )
            })
            .expect("Any println to forge");
        let MirInstructionKind::ManagedAny {
            contract: Some(actual),
            ..
        } = &mut forged_instruction.kind
        else {
            unreachable!()
        };
        actual.checker_source_type = Some(crate::core::ir::primitive_type_id(PrimitiveType::I32));
        assert!(catalog.validate_managed_any_function(&function).is_err());

        // Replacing the exact Any operation with a type-only builtin must be
        // rejected by the Checker body seal before any output can occur.
        let mut forged_builtin = get_main(&program);
        let block = forged_builtin
            .blocks
            .values_mut()
            .find(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        &instruction.kind,
                        MirInstructionKind::ManagedAny {
                            operation: MirManagedAnyOperation::Println,
                            ..
                        }
                    )
                })
            })
            .expect("println block");
        let print = block
            .instructions
            .iter_mut()
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::ManagedAny {
                        operation: MirManagedAnyOperation::Println,
                        ..
                    }
                )
            })
            .expect("dedicated Any output instruction");
        let (source, result) = match &print.kind {
            MirInstructionKind::ManagedAny {
                source,
                result: Some(result),
                operation: MirManagedAnyOperation::Println,
                ..
            } => (source.clone(), result.clone()),
            _ => unreachable!(),
        };
        print.kind = MirInstructionKind::BuiltinCall {
            result,
            kind: super::super::types::MirBuiltinKind::PrintlnInt,
            arguments: vec![source],
            string_field_contract: None,
        };
        assert!(catalog
            .validate_managed_any_function(&forged_builtin)
            .is_err());
    }

    // ManagedAny Println exercises the exact one-owner Map/Any route. Keep
    // this source separate from SOURCE, which intentionally combines two
    // Map lineages, generic calls, and conditional control flow for broader
    // checker/receipt tests outside that narrow production profile.
    const MANAGED_ANY_PRINTLN_SOURCE: &str = r#"
func main() -> i32 {
    match map_new() {
        Ok(empty) => {
            match map_set(empty, "k", "string payload") {
                Ok(root) => {
                    let pair = map_get(root, "k")
                    drop(empty)
                    drop(root)
                    println(pair.1)
                    drop(pair)
                    0
                },
                Err(status) => {
                    drop(empty)
                    status
                },
            }
        },
        Err(status) => { status },
    }
}
"#;

    #[test]
    fn managed_any_v2_println_uses_checker_dynamic_any_borrow_then_drops_only_its_temporary() {
        let checked_program = checked(MANAGED_ANY_PRINTLN_SOURCE);
        let calls = checked_program
            .call_sites_sorted()
            .into_iter()
            .filter(|site| site.callee == "println")
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 1);
        let program = MirProgram::from_checked_program(&checked_program)
            .expect("source-sealed V2 println fixture");
        let receipts = program
            .ordinary_map_owned_any_v2_println_receipts()
            .expect("source-bound V2 println receipts");
        let [receipt] = receipts.as_slice() else {
            panic!("one direct stdout borrow")
        };
        assert_eq!(receipt.call, calls[0].node_id);
        assert_eq!(receipt.pair, receipt.get_any.binding);
        let function = get_main(&program);
        let catalog = program.type_catalog();
        assert_eq!(
            function.values[&receipt.pair].ty,
            ordinary_map_owned_any_v2_get_result_type_id()
        );
        assert_eq!(
            function.values[&receipt.result].ty,
            crate::core::ir::primitive_type_id(PrimitiveType::Unit)
        );
        assert_eq!(
            catalog
                .get(&receipt.checker_payload_type)
                .expect("source Any")
                .kind,
            MirTypeKind::DynamicAny
        );
        assert!(
            function
                .blocks
                .values()
                .flat_map(|block| &block.instructions)
                .all(|instruction| !matches!(&instruction.kind,
            MirInstructionKind::Project { base, .. } if base == &receipt.pair)),
            "V2 borrowed stdout never creates a retained or v1 payload owner"
        );
        receipt
            .validate_against_program(&program)
            .expect("exact source/borrow/drop/selection receipt");
        catalog
            .validate_managed_any_function(&function)
            .expect("the complete Checker body and owner flow remain sealed");
    }

    #[test]
    fn managed_any_v2_println_receipt_does_not_authorize_a_payload_projection() {
        let program = MirProgram::from_checked_program(&checked(MANAGED_ANY_PRINTLN_SOURCE))
            .expect("source-sealed V2 println fixture");
        let receipt = program
            .ordinary_map_owned_any_v2_println_receipts()
            .expect("source-bound V2 println receipts")
            .remove(0);
        receipt
            .validate_against_program(&program)
            .expect("the dedicated DirectPrint receipt is valid");

        let mut forged = get_main(&program);
        let projected =
            MirValueId::new("test:v2_payload_project").expect("synthetic payload value id");
        let project = MirInstruction {
            id: MirInstructionId::new("inst:test_v2_payload_project")
                .expect("synthetic payload projection id"),
            kind: MirInstructionKind::Project {
                result: projected.clone(),
                base: receipt.pair.clone(),
                projection: MirProjection::Tuple(1),
                list_index_contract: None,
            },
        };
        forged.values.insert(
            projected.clone(),
            crate::core::mir::MirValue {
                id: projected.clone(),
                ty: ordinary_map_owned_any_v2_payload_type_id(),
            },
        );
        let (block_id, print_index) = forged
            .blocks
            .iter()
            .find_map(|(id, block)| {
                block
                    .instructions
                    .iter()
                    .position(|instruction| instruction.id == receipt.instruction)
                    .map(|index| (id.clone(), index))
            })
            .expect("exact DirectPrint instruction");
        let block = forged.blocks.get_mut(&block_id).expect("DirectPrint block");
        block.instructions.insert(print_index, project.clone());

        let seal = forged
            .ownership
            .checker_managed_any
            .as_mut()
            .expect("private Checker body seal");
        seal.value_types
            .insert(projected, ordinary_map_owned_any_v2_payload_type_id());
        let seal_print_index = seal
            .instructions
            .iter()
            .position(|(_, _, instruction)| instruction.id == receipt.instruction)
            .expect("sealed DirectPrint instruction");
        seal.instructions.insert(
            seal_print_index,
            (block_id, receipt.projection, project.clone()),
        );

        let errors = validate_managed_any_receipts(&forged);
        assert!(
            errors
                .iter()
                .any(|error| error.subject == project.id.to_string()),
            "a valid DirectPrint receipt cannot authorize a synthetic V2 payload Project: {errors:?}"
        );
    }

    #[test]
    fn managed_any_v2_println_rejects_dropping_the_same_snapshot_before_the_borrow() {
        let program = MirProgram::from_checked_program(&checked(MANAGED_ANY_PRINTLN_SOURCE))
            .expect("source-sealed V2 println fixture");
        let receipt = program
            .ordinary_map_owned_any_v2_println_receipts()
            .expect("print receipts")
            .remove(0);
        let mut function = get_main(&program);
        let block = function
            .blocks
            .values_mut()
            .find(|block| {
                block
                    .instructions
                    .iter()
                    .any(|instruction| instruction.id == receipt.instruction)
            })
            .expect("DirectPrint block");
        let print_index = block
            .instructions
            .iter()
            .position(|instruction| instruction.id == receipt.instruction)
            .expect("exact borrowed stdout");
        let drop_index = block
            .instructions
            .iter()
            .position(|instruction| instruction.id == receipt.get_any.settlement_instruction)
            .expect("exact whole-pair Drop");
        block.instructions.swap(print_index, drop_index);
        assert!(program
            .type_catalog()
            .validate_managed_any_function(&function)
            .is_err());
        let mut forged = program.clone();
        forged.replace_function_for_test_only(function);
        let interpreter = crate::core::mir::reference::MirReferenceInterpreter::new(&forged);
        interpreter
            .execute_with_output(&NodeId("function:main".into()), &[])
            .expect_err("the source-bound pair cannot be dropped before stdout borrow");
        assert_eq!(
            interpreter.captured_output(),
            "",
            "preflight rejects the forged graph before stdout"
        );
    }

    #[test]
    fn managed_any_v2_println_preserves_the_string_payload_tag_and_source_owner() {
        let program = MirProgram::from_checked_program(&checked(MANAGED_ANY_PRINTLN_SOURCE))
            .expect("source-sealed V2 String println fixture");
        let receipt = program
            .ordinary_map_owned_any_v2_println_receipts()
            .expect("print receipts")
            .remove(0);
        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
        assert!(receipt
            .selected_types
            .iter()
            .any(|selected| selected.type_id == string_type));
        let function = get_main(&program);
        assert_eq!(
            function.values[&receipt.pair].ty,
            ordinary_map_owned_any_v2_get_result_type_id()
        );
        let interpreter = crate::core::mir::reference::MirReferenceInterpreter::new(&program);
        interpreter
            .execute_with_output(&NodeId("function:main".into()), &[])
            .expect("borrowed String output");
        assert_eq!(interpreter.captured_output(), "string payload\n");
        // A V2 pair is never a valid V1 ManagedAny source, even with a
        // plausible-looking public stdout contract.
        let v1_contract = MirManagedAnyContract {
            schema_version: MIR_MANAGED_ANY_SCHEMA_VERSION,
            descriptor_version: MIR_MANAGED_ANY_DESCRIPTOR_VERSION,
            owner: function.owner.clone(),
            origin: receipt.call.clone(),
            source: receipt.pair.clone(),
            result: Some(receipt.result.clone()),
            source_type: managed_any_i32_string_v1_type_id(),
            target_type: Some(crate::core::ir::primitive_type_id(PrimitiveType::Unit)),
            checker_conversion: None,
            checker_source_type: Some(receipt.checker_payload_type.clone()),
            expected_kind: None,
        };
        assert!(program
            .type_catalog()
            .validate_managed_any_contract(
                &function,
                &receipt.pair,
                Some(&receipt.result),
                MirManagedAnyOperation::Println,
                Some(&v1_contract)
            )
            .is_err());
    }

    #[test]
    fn managed_any_v2_println_rejects_missing_or_non_dynamic_checker_source_identity() {
        let program = MirProgram::from_checked_program(&checked(MANAGED_ANY_PRINTLN_SOURCE))
            .expect("source-sealed V2 println fixture");
        let function = get_main(&program);
        for case in 0..3 {
            let mut forged = function.clone();
            let source = forged
                .ownership
                .checker_ordinary_map_owned_any_v2_cfg_receipt
                .as_mut()
                .expect("V2 source receipt")
                .result_receipts
                .first_mut()
                .expect("pair receipt");
            match case {
                0 => source.payload_println = None,
                1 => {
                    source
                        .payload_println
                        .as_mut()
                        .expect("stdout observation")
                        .checker_payload_type =
                        crate::core::ir::primitive_type_id(PrimitiveType::I32)
                }
                _ => {
                    source
                        .payload_println
                        .as_mut()
                        .expect("stdout observation")
                        .call = source.operation.clone()
                }
            }
            assert!(
                program
                    .type_catalog()
                    .validate_managed_any_function(&forged)
                    .is_err(),
                "forged source identity case {case}"
            );
        }
        let mut forged_builtin = function.clone();
        let print = forged_builtin
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Println { .. }
                )
            })
            .expect("exact V2 stdout");
        let MirInstructionKind::OrdinaryMapOwnedAnyV2Println { result, pair, .. } =
            print.kind.clone()
        else {
            unreachable!()
        };
        print.kind = MirInstructionKind::BuiltinCall {
            result,
            kind: super::super::types::MirBuiltinKind::PrintlnInt,
            arguments: vec![pair],
            string_field_contract: None,
        };
        assert!(program
            .type_catalog()
            .validate_managed_any_function(&forged_builtin)
            .is_err());
    }

    fn get_main(program: &MirProgram) -> MirFunction {
        program
            .functions()
            .values()
            .find(|function| function.owner.0.ends_with("main"))
            .expect("GetAny fixture main")
            .clone()
    }

    fn managed_any_flow_function(blocks: Vec<crate::core::mir::MirBlock>) -> MirFunction {
        let owner = MirValueId::new("param:owner").expect("owner value id");
        let condition = MirValueId::new("const:condition").expect("condition value id");
        MirFunction {
            owner: NodeId("managed-any-flow-test".into()),
            parameters: vec![owner.clone()],
            parameter_permissions: Some(vec![None]),
            result: crate::core::ir::primitive_type_id(PrimitiveType::Unit),
            entry: MirBlockId::new("bb.entry").expect("entry block id"),
            values: BTreeMap::from([
                (
                    owner.clone(),
                    crate::core::mir::MirValue {
                        id: owner,
                        ty: managed_any_i32_string_v1_type_id(),
                    },
                ),
                (
                    condition.clone(),
                    crate::core::mir::MirValue {
                        id: condition,
                        ty: crate::core::ir::primitive_type_id(PrimitiveType::Bool),
                    },
                ),
            ]),
            blocks: blocks
                .into_iter()
                .map(|block| (block.id.clone(), block))
                .collect(),
            contracts: Vec::new(),
            proof_worklist: None,
            ownership: crate::core::mir::MirOwnershipSummary::default(),
        }
    }

    /// Build a small branch whose then arm creates a read-arm String value.
    /// The source fixture used to obtain this shape through an explicit
    /// `Any as string` cast, but that cast is intentionally outside the
    /// resolved MIR slice.  The CFG tests only need the path-local String
    /// ownership shape, so keep the test at canonical MIR level instead.
    fn managed_any_branch_string_fixture(
        drop_string_in_then: bool,
    ) -> (MirFunction, BTreeSet<MirValueId>, MirValueId) {
        let owner = MirValueId::new("param:owner").expect("owner value id");
        let string = MirValueId::new("local:branch-string").expect("string value id");
        let mut then_instructions = vec![MirInstruction {
            id: MirInstructionId::new("inst:branch-string.const").expect("const instruction id"),
            kind: MirInstructionKind::Const {
                result: string.clone(),
                literal: ResolvedLiteral::String("branch".into()),
            },
        }];
        if drop_string_in_then {
            then_instructions.push(flow_test_drop(
                "local:branch-string",
                "inst:branch-string.drop",
            ));
        }
        let entry = flow_test_block(
            "bb.entry",
            Vec::new(),
            MirTerminator::Branch {
                condition: MirValueId::new("const:condition").expect("condition value id"),
                then_edge: crate::core::mir::MirEdgeId::new("edge:branch.then")
                    .expect("then edge id"),
                then_target: MirBlockId::new("bb.then").expect("then block id"),
                then_arguments: Vec::new(),
                else_edge: crate::core::mir::MirEdgeId::new("edge:branch.else")
                    .expect("else edge id"),
                else_target: MirBlockId::new("bb.else").expect("else block id"),
                else_arguments: Vec::new(),
            },
        );
        let then_block = flow_test_block(
            "bb.then",
            then_instructions,
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:then.join")
                    .expect("then join edge id"),
                target: MirBlockId::new("bb.join").expect("join block id"),
                arguments: Vec::new(),
            },
        );
        let else_block = flow_test_block(
            "bb.else",
            Vec::new(),
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:else.join")
                    .expect("else join edge id"),
                target: MirBlockId::new("bb.join").expect("join block id"),
                arguments: Vec::new(),
            },
        );
        let join_block =
            flow_test_block("bb.join", Vec::new(), MirTerminator::Return { value: None });
        let mut function =
            managed_any_flow_function(vec![entry, then_block, else_block, join_block]);
        function.values.insert(
            string.clone(),
            crate::core::mir::MirValue {
                id: string.clone(),
                ty: crate::core::ir::primitive_type_id(PrimitiveType::String),
            },
        );
        (function, BTreeSet::from([owner]), string)
    }

    fn flow_test_block(
        id: &str,
        instructions: Vec<MirInstruction>,
        terminator: MirTerminator,
    ) -> crate::core::mir::MirBlock {
        crate::core::mir::MirBlock {
            id: MirBlockId::new(id).expect("block id"),
            parameters: Vec::new(),
            instructions,
            terminator,
        }
    }

    fn flow_test_drop(value: &str, id: &str) -> MirInstruction {
        MirInstruction {
            id: MirInstructionId::new(id).expect("instruction id"),
            kind: MirInstructionKind::Drop {
                value: MirValueId::new(value).expect("drop value"),
            },
        }
    }

    fn string_test_value(id: &str, ty: ResolvedTypeId) -> crate::core::mir::MirValue {
        let id = MirValueId::new(id).expect("test value id");
        crate::core::mir::MirValue { id, ty }
    }

    fn string_liveness_test_function(
        parameters: Vec<MirValueId>,
        values: Vec<crate::core::mir::MirValue>,
        blocks: Vec<crate::core::mir::MirBlock>,
    ) -> MirFunction {
        MirFunction {
            owner: NodeId("managed-any-string-liveness-test".into()),
            parameters,
            parameter_permissions: None,
            result: crate::core::ir::primitive_type_id(PrimitiveType::Unit),
            entry: MirBlockId::new("bb.entry").expect("entry block id"),
            values: values
                .into_iter()
                .map(|value| (value.id.clone(), value))
                .collect(),
            blocks: blocks
                .into_iter()
                .map(|block| (block.id.clone(), block))
                .collect(),
            contracts: Vec::new(),
            proof_worklist: None,
            ownership: crate::core::mir::MirOwnershipSummary::default(),
        }
    }

    fn string_liveness_test_successors(
        edges: &[(&str, &str)],
    ) -> BTreeMap<MirBlockId, Vec<ManagedAnyCfgSuccessor>> {
        let mut successors = BTreeMap::<MirBlockId, Vec<ManagedAnyCfgSuccessor>>::new();
        for (source, target) in edges {
            successors
                .entry(MirBlockId::new(*source).expect("source block id"))
                .or_default()
                .push(ManagedAnyCfgSuccessor {
                    target: MirBlockId::new(*target).expect("target block id"),
                    arguments: Vec::new(),
                    switch_move_payloads: BTreeSet::new(),
                });
        }
        successors
    }

    #[test]
    fn managed_any_string_liveness_tracks_clone_move_and_drop_identity() {
        let program = MirProgram::from_checked_program(&checked(
            r#"
func main() -> i32 {
    let text = "catalog"
    println(text)
    0
}
"#,
        ))
        .expect("String catalog MIR");
        let catalog = program.type_catalog();
        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
        let bool_type = crate::core::ir::primitive_type_id(PrimitiveType::Bool);
        let peer = MirValueId::new("param:peer").unwrap();
        let alias = MirValueId::new("local:alias").unwrap();
        let equality = MirValueId::new("result:equality").unwrap();
        let entry = flow_test_block(
            "bb.entry",
            vec![
                MirInstruction {
                    id: MirInstructionId::new("inst:clone.alias").unwrap(),
                    kind: MirInstructionKind::Clone {
                        result: alias.clone(),
                        source: peer.clone(),
                    },
                },
                flow_test_drop("param:peer", "inst:drop.original"),
                MirInstruction {
                    id: MirInstructionId::new("inst:compare.alias").unwrap(),
                    kind: MirInstructionKind::Binary {
                        result: equality.clone(),
                        op: ResolvedBinaryOp::Equal,
                        left: alias.clone(),
                        right: alias.clone(),
                    },
                },
                flow_test_drop("local:alias", "inst:drop.alias"),
            ],
            MirTerminator::Return { value: None },
        );
        let function = string_liveness_test_function(
            vec![peer.clone()],
            vec![
                string_test_value("param:peer", string_type.clone()),
                string_test_value("local:alias", string_type.clone()),
                string_test_value("result:equality", bool_type.clone()),
            ],
            vec![entry],
        );
        catalog
            .validate_managed_any_string_liveness(
                &function,
                &BTreeMap::from([(function.entry.clone(), Vec::new())]),
            )
            .expect("Clone creates an independent live String which survives Drop of its source");

        let moved_peer = MirValueId::new("param:peer").unwrap();
        let moved_alias = MirValueId::new("local:alias").unwrap();
        let stale_equality = MirValueId::new("result:stale").unwrap();
        let entry = flow_test_block(
            "bb.entry",
            vec![
                MirInstruction {
                    id: MirInstructionId::new("inst:move.alias").unwrap(),
                    kind: MirInstructionKind::Move {
                        result: moved_alias.clone(),
                        source: moved_peer.clone(),
                    },
                },
                MirInstruction {
                    id: MirInstructionId::new("inst:compare.stale").unwrap(),
                    kind: MirInstructionKind::Binary {
                        result: stale_equality,
                        op: ResolvedBinaryOp::Equal,
                        left: moved_peer.clone(),
                        right: moved_alias.clone(),
                    },
                },
            ],
            MirTerminator::Return { value: None },
        );
        let function = string_liveness_test_function(
            vec![moved_peer.clone()],
            vec![
                string_test_value("param:peer", string_type.clone()),
                string_test_value("local:alias", string_type.clone()),
                string_test_value("result:stale", bool_type),
            ],
            vec![entry],
        );
        let error = catalog
            .validate_managed_any_string_liveness(
                &function,
                &BTreeMap::from([(function.entry.clone(), Vec::new())]),
            )
            .expect_err("Move consumes its old SSA owner and a later read must fail");
        assert!(error.contains("not definitely live"), "{error}");
    }

    #[test]
    fn managed_any_string_liveness_rejects_duplicate_owner_transfers() {
        let program = MirProgram::from_checked_program(&checked(
            r#"
func main() -> i32 {
    let text = "catalog"
    println(text)
    0
}
"#,
        ))
        .expect("String catalog MIR");
        let catalog = program.type_catalog();
        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
        let peer = MirValueId::new("param:peer").unwrap();
        let aggregate = MirValueId::new("result:aggregate").unwrap();
        let entry = flow_test_block(
            "bb.entry",
            vec![MirInstruction {
                id: MirInstructionId::new("inst:duplicate.construct").unwrap(),
                kind: MirInstructionKind::Construct {
                    result: aggregate.clone(),
                    kind: MirAggregateKind::Tuple,
                    fields: vec![peer.clone(), peer.clone()],
                },
            }],
            MirTerminator::Return { value: None },
        );
        let function = string_liveness_test_function(
            vec![peer.clone()],
            vec![
                string_test_value("param:peer", string_type.clone()),
                string_test_value(
                    "result:aggregate",
                    crate::core::ir::primitive_type_id(PrimitiveType::I32),
                ),
            ],
            vec![entry],
        );
        let error = catalog
            .validate_managed_any_string_liveness(
                &function,
                &BTreeMap::from([(function.entry.clone(), Vec::new())]),
            )
            .expect_err("a single aggregate cannot consume one String owner twice");
        assert!(error.contains("transferred more than once"), "{error}");

        let first_parameter = MirValueId::new("block:first").unwrap();
        let second_parameter = MirValueId::new("block:second").unwrap();
        let target_id = MirBlockId::new("bb.target").unwrap();
        let entry = flow_test_block(
            "bb.entry",
            Vec::new(),
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:duplicate").unwrap(),
                target: target_id.clone(),
                arguments: vec![peer.clone(), peer.clone()],
            },
        );
        let mut target = flow_test_block(
            "bb.target",
            Vec::new(),
            MirTerminator::Return { value: None },
        );
        target.parameters = vec![
            crate::core::mir::MirBlockParameter {
                value: first_parameter.clone(),
            },
            crate::core::mir::MirBlockParameter {
                value: second_parameter.clone(),
            },
        ];
        let function = string_liveness_test_function(
            vec![peer.clone()],
            vec![
                string_test_value("param:peer", string_type.clone()),
                string_test_value("block:first", string_type.clone()),
                string_test_value("block:second", string_type),
            ],
            vec![entry, target],
        );
        let successors = BTreeMap::from([
            (
                function.entry.clone(),
                vec![ManagedAnyCfgSuccessor {
                    target: target_id.clone(),
                    arguments: vec![peer.clone(), peer],
                    switch_move_payloads: BTreeSet::new(),
                }],
            ),
            (target_id, Vec::new()),
        ]);
        let error = catalog
            .validate_managed_any_string_liveness(&function, &successors)
            .expect_err("one String SSA owner cannot initialize two block parameters");
        assert!(error.contains("same SSA owner"), "{error}");
    }

    #[test]
    fn managed_any_string_liveness_intersects_branch_and_loop_paths() {
        let program = MirProgram::from_checked_program(&checked(
            r#"
func main() -> i32 {
    let text = "catalog"
    println(text)
    0
}
"#,
        ))
        .expect("String catalog MIR");
        let catalog = program.type_catalog();
        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
        let bool_type = crate::core::ir::primitive_type_id(PrimitiveType::Bool);
        let peer = MirValueId::new("param:peer").unwrap();
        let condition = MirValueId::new("param:condition").unwrap();
        let comparison = MirValueId::new("result:equality").unwrap();
        let entry = flow_test_block(
            "bb.entry",
            Vec::new(),
            MirTerminator::Branch {
                condition: condition.clone(),
                then_edge: crate::core::mir::MirEdgeId::new("edge:then").unwrap(),
                then_target: MirBlockId::new("bb.then").unwrap(),
                then_arguments: Vec::new(),
                else_edge: crate::core::mir::MirEdgeId::new("edge:else").unwrap(),
                else_target: MirBlockId::new("bb.else").unwrap(),
                else_arguments: Vec::new(),
            },
        );
        let then_block = flow_test_block(
            "bb.then",
            Vec::new(),
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:then.join").unwrap(),
                target: MirBlockId::new("bb.join").unwrap(),
                arguments: Vec::new(),
            },
        );
        let else_block = flow_test_block(
            "bb.else",
            Vec::new(),
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:else.join").unwrap(),
                target: MirBlockId::new("bb.join").unwrap(),
                arguments: Vec::new(),
            },
        );
        let join = flow_test_block(
            "bb.join",
            vec![MirInstruction {
                id: MirInstructionId::new("inst:join.compare").unwrap(),
                kind: MirInstructionKind::Binary {
                    result: comparison.clone(),
                    op: ResolvedBinaryOp::Equal,
                    left: peer.clone(),
                    right: peer.clone(),
                },
            }],
            MirTerminator::Return { value: None },
        );
        let values = vec![
            string_test_value("param:peer", string_type.clone()),
            string_test_value("param:condition", bool_type.clone()),
            string_test_value("result:equality", bool_type.clone()),
        ];
        let parameters = vec![peer.clone(), condition.clone()];
        let successors = string_liveness_test_successors(&[
            ("bb.entry", "bb.then"),
            ("bb.entry", "bb.else"),
            ("bb.then", "bb.join"),
            ("bb.else", "bb.join"),
        ]);
        let function = string_liveness_test_function(
            parameters.clone(),
            values.clone(),
            vec![
                entry.clone(),
                then_block.clone(),
                else_block.clone(),
                join.clone(),
            ],
        );
        catalog
            .validate_managed_any_string_liveness(&function, &successors)
            .expect("the peer is live on both arms and at their join");

        let mut consuming_then = then_block;
        consuming_then
            .instructions
            .push(flow_test_drop("param:peer", "inst:then.drop"));
        let function = string_liveness_test_function(
            parameters.clone(),
            values.clone(),
            vec![entry.clone(), consuming_then, else_block, join.clone()],
        );
        let error = catalog
            .validate_managed_any_string_liveness(&function, &successors)
            .expect_err("the join intersects live peers from each incoming path");
        assert!(error.contains("not definitely live"), "{error}");

        let header = flow_test_block(
            "bb.header",
            Vec::new(),
            MirTerminator::Branch {
                condition: condition.clone(),
                then_edge: crate::core::mir::MirEdgeId::new("edge:body").unwrap(),
                then_target: MirBlockId::new("bb.body").unwrap(),
                then_arguments: Vec::new(),
                else_edge: crate::core::mir::MirEdgeId::new("edge:exit").unwrap(),
                else_target: MirBlockId::new("bb.exit").unwrap(),
                else_arguments: Vec::new(),
            },
        );
        let entry = flow_test_block(
            "bb.entry",
            Vec::new(),
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:entry.header").unwrap(),
                target: MirBlockId::new("bb.header").unwrap(),
                arguments: Vec::new(),
            },
        );
        let loop_body = flow_test_block(
            "bb.body",
            vec![MirInstruction {
                id: MirInstructionId::new("inst:loop.compare").unwrap(),
                kind: MirInstructionKind::Binary {
                    result: comparison,
                    op: ResolvedBinaryOp::Equal,
                    left: peer.clone(),
                    right: peer.clone(),
                },
            }],
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:backedge").unwrap(),
                target: MirBlockId::new("bb.header").unwrap(),
                arguments: Vec::new(),
            },
        );
        let exit = flow_test_block("bb.exit", Vec::new(), MirTerminator::Return { value: None });
        let loop_values = vec![
            string_test_value("param:peer", string_type.clone()),
            string_test_value("param:condition", bool_type.clone()),
            string_test_value("result:equality", bool_type.clone()),
        ];
        let loop_parameters = vec![peer.clone(), condition.clone()];
        let loop_successors = string_liveness_test_successors(&[
            ("bb.entry", "bb.header"),
            ("bb.header", "bb.body"),
            ("bb.header", "bb.exit"),
            ("bb.body", "bb.header"),
        ]);
        let function = string_liveness_test_function(
            loop_parameters.clone(),
            loop_values.clone(),
            vec![
                entry.clone(),
                header.clone(),
                loop_body.clone(),
                exit.clone(),
            ],
        );
        catalog
            .validate_managed_any_string_liveness(&function, &loop_successors)
            .expect("a loop fixed point preserves the peer when every backedge preserves it");

        let mut dropping_body = loop_body;
        dropping_body
            .instructions
            .insert(0, flow_test_drop("param:peer", "inst:loop.drop"));
        let function = string_liveness_test_function(
            loop_parameters,
            loop_values,
            vec![entry, header, dropping_body, exit],
        );
        let error = catalog
            .validate_managed_any_string_liveness(&function, &loop_successors)
            .expect_err("a loop backedge that consumes the peer invalidates the next iteration");
        assert!(error.contains("not definitely live"), "{error}");
    }

    #[test]
    fn managed_any_cfg_accepts_balanced_owner_drop_on_each_branch() {
        let entry = flow_test_block(
            "bb.entry",
            Vec::new(),
            MirTerminator::Branch {
                condition: MirValueId::new("const:condition").unwrap(),
                then_edge: crate::core::mir::MirEdgeId::new("edge:then").unwrap(),
                then_target: MirBlockId::new("bb.then").unwrap(),
                then_arguments: Vec::new(),
                else_edge: crate::core::mir::MirEdgeId::new("edge:else").unwrap(),
                else_target: MirBlockId::new("bb.else").unwrap(),
                else_arguments: Vec::new(),
            },
        );
        let then_block = flow_test_block(
            "bb.then",
            vec![flow_test_drop("param:owner", "inst:drop.then")],
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:then.join").unwrap(),
                target: MirBlockId::new("bb.join").unwrap(),
                arguments: Vec::new(),
            },
        );
        let else_block = flow_test_block(
            "bb.else",
            vec![flow_test_drop("param:owner", "inst:drop.else")],
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:else.join").unwrap(),
                target: MirBlockId::new("bb.join").unwrap(),
                arguments: Vec::new(),
            },
        );
        let join = flow_test_block("bb.join", Vec::new(), MirTerminator::Return { value: None });
        let function = managed_any_flow_function(vec![entry, then_block, else_block, join]);
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let catalog = program.type_catalog();
        let managed_values = BTreeSet::from([MirValueId::new("param:owner").unwrap()]);

        catalog
            .validate_managed_any_cfg_flow(&function, &managed_values)
            .expect("every mutually exclusive path drops the owner exactly once");
    }

    #[test]
    fn managed_any_cfg_rejects_owner_state_mismatch_between_match_arms() {
        let entry = flow_test_block(
            "bb.entry",
            Vec::new(),
            MirTerminator::Branch {
                condition: MirValueId::new("const:condition").unwrap(),
                then_edge: crate::core::mir::MirEdgeId::new("edge:then").unwrap(),
                then_target: MirBlockId::new("bb.then").unwrap(),
                then_arguments: Vec::new(),
                else_edge: crate::core::mir::MirEdgeId::new("edge:else").unwrap(),
                else_target: MirBlockId::new("bb.else").unwrap(),
                else_arguments: Vec::new(),
            },
        );
        let then_block = flow_test_block(
            "bb.then",
            vec![flow_test_drop("param:owner", "inst:drop.then")],
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:then.join").unwrap(),
                target: MirBlockId::new("bb.join").unwrap(),
                arguments: Vec::new(),
            },
        );
        let else_block = flow_test_block(
            "bb.else",
            Vec::new(),
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:else.join").unwrap(),
                target: MirBlockId::new("bb.join").unwrap(),
                arguments: Vec::new(),
            },
        );
        let join = flow_test_block(
            "bb.join",
            vec![flow_test_drop("param:owner", "inst:drop.join")],
            MirTerminator::Return { value: None },
        );
        let function = managed_any_flow_function(vec![entry, then_block, else_block, join]);
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let catalog = program.type_catalog();
        let managed_values = BTreeSet::from([MirValueId::new("param:owner").unwrap()]);

        let error = catalog
            .validate_managed_any_cfg_flow(&function, &managed_values)
            .expect_err("branch paths with different live owners must fail at their join");
        assert!(error.contains("state disagrees at join"), "{error}");
    }

    #[test]
    fn managed_any_cfg_accepts_loop_with_stable_owner_fixed_point() {
        let entry = flow_test_block(
            "bb.entry",
            Vec::new(),
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:entry.header").unwrap(),
                target: MirBlockId::new("bb.header").unwrap(),
                arguments: Vec::new(),
            },
        );
        let header = flow_test_block(
            "bb.header",
            Vec::new(),
            MirTerminator::Branch {
                condition: MirValueId::new("const:condition").unwrap(),
                then_edge: crate::core::mir::MirEdgeId::new("edge:header.body").unwrap(),
                then_target: MirBlockId::new("bb.body").unwrap(),
                then_arguments: Vec::new(),
                else_edge: crate::core::mir::MirEdgeId::new("edge:header.exit").unwrap(),
                else_target: MirBlockId::new("bb.exit").unwrap(),
                else_arguments: Vec::new(),
            },
        );
        let body = flow_test_block(
            "bb.body",
            Vec::new(),
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:body.header").unwrap(),
                target: MirBlockId::new("bb.header").unwrap(),
                arguments: Vec::new(),
            },
        );
        let exit = flow_test_block(
            "bb.exit",
            vec![flow_test_drop("param:owner", "inst:drop.exit")],
            MirTerminator::Return { value: None },
        );
        let function = managed_any_flow_function(vec![entry, header, body, exit]);
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let catalog = program.type_catalog();
        let managed_values = BTreeSet::from([MirValueId::new("param:owner").unwrap()]);

        catalog
            .validate_managed_any_cfg_flow(&function, &managed_values)
            .expect("loop backedge preserves the exact owner state until exit cleanup");
    }

    #[test]
    fn managed_any_cfg_rejects_loop_backedge_that_loses_owner_state() {
        let entry = flow_test_block(
            "bb.entry",
            Vec::new(),
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:entry.header").unwrap(),
                target: MirBlockId::new("bb.header").unwrap(),
                arguments: Vec::new(),
            },
        );
        let header = flow_test_block(
            "bb.header",
            Vec::new(),
            MirTerminator::Branch {
                condition: MirValueId::new("const:condition").unwrap(),
                then_edge: crate::core::mir::MirEdgeId::new("edge:header.body").unwrap(),
                then_target: MirBlockId::new("bb.body").unwrap(),
                then_arguments: Vec::new(),
                else_edge: crate::core::mir::MirEdgeId::new("edge:header.exit").unwrap(),
                else_target: MirBlockId::new("bb.exit").unwrap(),
                else_arguments: Vec::new(),
            },
        );
        let body = flow_test_block(
            "bb.body",
            vec![flow_test_drop("param:owner", "inst:drop.body")],
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:body.header").unwrap(),
                target: MirBlockId::new("bb.header").unwrap(),
                arguments: Vec::new(),
            },
        );
        let exit = flow_test_block("bb.exit", Vec::new(), MirTerminator::Return { value: None });
        let function = managed_any_flow_function(vec![entry, header, body, exit]);
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let catalog = program.type_catalog();
        let managed_values = BTreeSet::from([MirValueId::new("param:owner").unwrap()]);

        let error = catalog
            .validate_managed_any_cfg_flow(&function, &managed_values)
            .expect_err("a loop backedge cannot silently lose a live owner");
        assert!(error.contains("state disagrees at join"), "{error}");
    }

    #[test]
    fn managed_any_string_equality_is_a_sealed_borrowed_read() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let main = get_main(&program);
        let managed_type = managed_any_i32_string_v1_type_id();
        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
        let bool_type = crate::core::ir::primitive_type_id(PrimitiveType::Bool);
        let (block, position, comparison, owner) = main
            .blocks
            .values()
            .find_map(|block| {
                block
                    .instructions
                    .iter()
                    .enumerate()
                    .find_map(|(position, instruction)| {
                        let MirInstructionKind::Binary {
                            result,
                            op: ResolvedBinaryOp::Equal,
                            left,
                            right,
                        } = &instruction.kind
                        else {
                            return None;
                        };
                        if main
                            .values
                            .get(result)
                            .is_none_or(|value| value.ty != bool_type)
                        {
                            return None;
                        }
                        let owner = if main
                            .values
                            .get(left)
                            .is_some_and(|value| value.ty == managed_type)
                            && main
                                .values
                                .get(right)
                                .is_some_and(|value| value.ty == string_type)
                        {
                            left
                        } else if main
                            .values
                            .get(right)
                            .is_some_and(|value| value.ty == managed_type)
                            && main
                                .values
                                .get(left)
                                .is_some_and(|value| value.ty == string_type)
                        {
                            right
                        } else {
                            return None;
                        };
                        Some((block, position, instruction, owner.clone()))
                    })
            })
            .expect("Any == String lowers through the exact borrowed comparison profile");
        assert!(comparison.id.0.contains("binary.managed_any_string_read"));
        assert!(
            block.instructions[position + 1..]
                .iter()
                .any(|instruction| {
                    matches!(
                        &instruction.kind,
                        MirInstructionKind::Drop { value } if value == &owner
                    ) || matches!(
                        &instruction.kind,
                        MirInstructionKind::ManagedAny {
                            source,
                            operation: MirManagedAnyOperation::Drop,
                            ..
                        } if source == &owner
                    )
                })
                || main.blocks.values().any(|later_block| {
                    later_block.instructions.iter().any(|instruction| {
                        matches!(
                            &instruction.kind,
                            MirInstructionKind::ManagedAny {
                                source,
                                operation: MirManagedAnyOperation::Drop,
                                ..
                            } if source == &owner
                        )
                    })
                }),
            "the borrowed comparison leaves its owner available for the source Drop"
        );
        assert!(
            main.ownership
                .checker_managed_any
                .as_ref()
                .is_some_and(|seal| seal.instructions.iter().any(|(_, _, instruction)| {
                    instruction.id == comparison.id && instruction.kind == comparison.kind
                })),
            "the borrowed comparison is part of the complete Checker-origin body seal"
        );

        let MirInstructionKind::Binary {
            result,
            left,
            right,
            ..
        } = &comparison.kind
        else {
            panic!("selected comparison instruction must stay Binary");
        };
        let managed_values = &main
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("private Any seal")
            .managed_values;
        assert!(
            managed_any_string_comparison_owner(
                &main,
                result,
                ResolvedBinaryOp::Add,
                left,
                right,
                managed_values,
            )
            .is_none(),
            "other operators cannot borrow an Any owner"
        );
    }

    #[test]
    fn managed_any_i32_pack_requires_the_exact_implicit_checker_conversion() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let catalog = program.type_catalog();
        let main = get_main(&program);
        let (source, result, contract) = main
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .find_map(|instruction| match &instruction.kind {
                MirInstructionKind::ManagedAny {
                    source,
                    result: Some(result),
                    operation: MirManagedAnyOperation::PackI32,
                    contract: Some(contract),
                } => Some((source.clone(), result.clone(), contract.clone())),
                _ => None,
            })
            .expect("the scalar Any fallback lowers to one checked PackI32");
        let conversion = contract
            .checker_conversion
            .as_ref()
            .expect("the pack carries ResolvedBody conversion evidence");
        assert_eq!(conversion.kind, CheckedConversionKind::DynamicAnyPack);
        assert_eq!(
            conversion.from,
            crate::core::ir::primitive_type_id(PrimitiveType::I32)
        );
        assert_eq!(contract.source_type, conversion.from);
        assert!(catalog
            .get(&conversion.to)
            .is_some_and(|descriptor| { descriptor.kind == MirTypeKind::DynamicAny }));
        assert_eq!(
            contract.target_type,
            Some(managed_any_i32_string_v1_type_id())
        );
        catalog
            .validate_managed_any_contract(
                &main,
                &source,
                Some(&result),
                MirManagedAnyOperation::PackI32,
                Some(&contract),
            )
            .expect("the exact checked i32 -> Any conversion has a valid v1 materialization");

        let mut missing_conversion = contract.clone();
        missing_conversion.checker_conversion = None;
        let error = catalog
            .validate_managed_any_contract(
                &main,
                &source,
                Some(&result),
                MirManagedAnyOperation::PackI32,
                Some(&missing_conversion),
            )
            .expect_err("a type-only i32/Any pair cannot authorize packing");
        assert!(error.contains("no exact Checker DynamicAnyPack"), "{error}");
    }

    #[test]
    fn managed_any_free_string_drop_preserves_source_binding_and_both_equality_orders() {
        let checked = checked(
            r#"
func carry<T>(value: T) -> T { value }
func main() -> i32 {
    match map_new() {
        Ok(empty) => {
            match map_set(empty, "k", "payload") {
                Ok(root) => {
                    let pair = map_get(root, "k")
                    drop(empty)
                    drop(root)
                    let found = pair.0
                    let payload = pair.1
                    let carried = carry(payload)
                    let peer = "payload"
                    drop(peer)
                    let left_first = if found { carried == peer } else { false }
                    let right_first = if found { peer == carried } else { false }
                    println(left_first)
                    println(right_first)
                    drop(pair)
                    drop(payload)
                    drop(carried)
                    0
                },
                Err(status) => {
                    drop(empty)
                    status
                },
            }
        },
        Err(status) => { status },
    }
}
"#,
        );
        let program = MirProgram::from_checked_program(&checked)
            .expect("Free String Drop must preserve the binding used by both comparisons");
        let main = get_main(&program);
        let catalog = program.type_catalog();
        let managed_type = managed_any_i32_string_v1_type_id();
        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
        let mut comparisons = Vec::new();
        let mut temporary = None;
        let mut dropped_temporary = false;
        for block in main.blocks.values() {
            for instruction in &block.instructions {
                match &instruction.kind {
                    MirInstructionKind::Binary {
                        op: ResolvedBinaryOp::Equal,
                        left,
                        right,
                        ..
                    } if (main
                        .values
                        .get(left)
                        .is_some_and(|value| value.ty == managed_type)
                        && main
                            .values
                            .get(right)
                            .is_some_and(|value| value.ty == string_type))
                        || (main
                            .values
                            .get(right)
                            .is_some_and(|value| value.ty == managed_type)
                            && main
                                .values
                                .get(left)
                                .is_some_and(|value| value.ty == string_type)) =>
                    {
                        comparisons.push((left.clone(), right.clone()));
                    }
                    MirInstructionKind::Clone { result, source }
                        if instruction.id.0.contains("drop.string.clone")
                            && main
                                .values
                                .get(source)
                                .is_some_and(|value| value.ty == string_type) =>
                    {
                        temporary = Some((result.clone(), source.clone()));
                    }
                    MirInstructionKind::Drop { value }
                        if temporary
                            .as_ref()
                            .is_some_and(|(result, _)| result == value) =>
                    {
                        dropped_temporary = true;
                    }
                    _ => {}
                }
            }
        }
        assert_eq!(
            comparisons.len(),
            2,
            "both operand orders must use the profile"
        );
        assert!(comparisons.iter().any(|(left, right)| {
            main.values
                .get(left)
                .is_some_and(|value| value.ty == managed_type)
                && main
                    .values
                    .get(right)
                    .is_some_and(|value| value.ty == string_type)
        }));
        assert!(comparisons.iter().any(|(left, right)| {
            main.values
                .get(right)
                .is_some_and(|value| value.ty == managed_type)
                && main
                    .values
                    .get(left)
                    .is_some_and(|value| value.ty == string_type)
        }));
        let (temporary, peer) = temporary.expect("source Drop evaluates a fresh String clone");
        assert!(
            dropped_temporary,
            "the fresh evaluation temporary is settled"
        );
        assert!(
            comparisons
                .iter()
                .all(|(left, right)| { left == &peer || right == &peer }),
            "both comparisons continue to read the original String binding"
        );
        assert_ne!(
            temporary, peer,
            "source Drop cannot consume the original owner"
        );
        catalog
            .validate_managed_any_function(&main)
            .expect("the exact source seal and path-liveness proof accept the legal no-op Drop");

        let mut forged = main.clone();
        let drop_instruction = forged
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Drop { value } if value == &temporary
                )
            })
            .expect("temporary Drop to forge");
        drop_instruction.kind = MirInstructionKind::Drop {
            value: peer.clone(),
        };
        let managed_values = forged
            .values
            .values()
            .filter(|value| catalog.contains_dynamic_or_managed_any(&value.ty))
            .map(|value| value.id.clone())
            .collect::<BTreeSet<_>>();
        let error = catalog
            .validate_managed_any_cfg_flow(&forged, &managed_values)
            .expect_err("a MIR Drop that consumes the binding before equality must be rejected");
        assert!(error.contains("not definitely live"), "{error}");
    }

    #[test]
    fn managed_any_string_equality_borrows_the_source_owner() {
        let checked = checked(
            r#"
func carry<T>(value: T) -> T { value }
func main() -> i32 {
    match map_new() {
        Ok(empty) => {
            match map_set(empty, "k", "payload") {
                Ok(root) => {
                    let pair = map_get(root, "k")
                    drop(empty)
                    drop(root)
                    let found = pair.0
                    let payload = pair.1
                    let carried = carry(payload)
                    let peer = "payload"
                    let equal = if found { carried == peer } else { false }
                    println(equal)
                    drop(pair)
                    drop(payload)
                    drop(carried)
                    drop(peer)
                    0
                },
                Err(status) => {
                    drop(empty)
                    status
                },
            }
        },
        Err(status) => { status },
    }
}
"#,
        );
        let program = MirProgram::from_checked_program(&checked)
            .expect("a checked Any owner can be borrowed by the String equality profile");
        let main = get_main(&program);
        let managed_type = managed_any_i32_string_v1_type_id();
        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
        let has_borrowed_any_string_equality = main.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Binary {
                        op: ResolvedBinaryOp::Equal,
                        left,
                        right,
                        ..
                    } if (main.values.get(left).is_some_and(|value| value.ty == managed_type)
                        && main.values.get(right).is_some_and(|value| value.ty == string_type))
                        || (main.values.get(right).is_some_and(|value| value.ty == managed_type)
                            && main.values.get(left).is_some_and(|value| value.ty == string_type))
                )
            })
        });
        let has_owned_string_drop = main.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Drop { value }
                        if main.values.get(value).is_some_and(|entry| entry.ty == string_type)
                )
            })
        });
        assert!(
            has_borrowed_any_string_equality && has_owned_string_drop,
            "the equality reads the Any owner beside an owned String peer"
        );
        program
            .type_catalog()
            .validate_managed_any_function(&main)
            .expect("the borrowed peer remains live and settles after the comparison");
    }

    #[test]
    fn managed_any_tuple_pattern_synthetics_map_to_the_checker_binding_point() {
        let checked = checked(
            r#"
func main() -> i32 {
    match map_new() {
        Ok(empty) => {
            match map_set(empty, "k", "payload") {
                Ok(root) => {
                    let pair = map_get(root, "k")
                    drop(empty)
                    drop(root)
                    let (found, payload) = pair
                    println(found)
                    drop(payload)
                    0
                },
                Err(status) => {
                    drop(empty)
                    status
                },
            }
        },
        Err(status) => { status },
    }
}
"#,
        );
        let program = MirProgram::from_checked_program(&checked)
            .expect("tuple patterns use the source binding point for synthetic MIR identities");
        let main = get_main(&program);
        let seal = main
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("private Checker ManagedAny body seal");
        let source_cfg = seal
            .source_cfg
            .as_ref()
            .expect("Checker source CFG receipt");
        let pattern_instructions = seal
            .instructions
            .iter()
            .filter(|(_, _, instruction)| {
                instruction.id.0.contains("managed_any_pattern_bind")
                    || instruction.id.0.contains("managed_any_tuple_project")
            })
            .collect::<Vec<_>>();
        assert!(!pattern_instructions.is_empty());
        for (_, origin, instruction) in pattern_instructions {
            assert!(
                source_cfg.reachable_blocks.values().any(|block| {
                    block
                        .points
                        .iter()
                        .any(|point| &point.source.node == origin)
                }),
                "synthetic pattern instruction '{}' must use its enclosing Checker binding point '{}'",
                instruction.id,
                origin.0
            );
        }
    }

    #[test]
    fn managed_any_tuple_project_clones_and_keeps_residual_bundle_live() {
        let source = r#"
func main() -> i32 {
    match map_new() {
        Ok(empty) => {
            match map_set(empty, "k", "payload") {
                Ok(root) => {
                    let pair = map_get(root, "k")
                    let payload = pair.1
                    let bundle = (payload, "owned sibling")
                    drop(pair)
                    drop(empty)
                    drop(root)
                    let selected = bundle.0
                    drop(selected)
                    0
                },
                Err(status) => {
                    drop(empty)
                    status
                },
            }
        },
        Err(status) => { status },
    }
}
"#;
        let checked = checked(source);
        let program = MirProgram::from_checked_program(&checked)
            .expect("ordinary Project clones the selected Any and preserves the bundle owner");
        let main = get_main(&program);
        let catalog = program.type_catalog();
        let managed_type = managed_any_i32_string_v1_type_id();
        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
        let (projection, bundle, selected) = main
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .find_map(|instruction| match &instruction.kind {
                MirInstructionKind::Project {
                    result,
                    base,
                    projection: MirProjection::Tuple(0),
                    ..
                } if main.values.get(base).is_some_and(|value| {
                    catalog.get(&value.ty).is_some_and(|descriptor| {
                        descriptor.layout
                            == MirLayout::Tuple(vec![managed_type.clone(), string_type.clone()])
                    })
                }) =>
                {
                    Some((instruction, base, result))
                }
                _ => None,
            })
            .expect("the source field read must lower to a non-consuming tuple Project");
        assert_eq!(
            main.values.get(selected).map(|value| &value.ty),
            Some(&managed_type)
        );
        catalog
            .validate_glue(&managed_type, MirGlueOperation::Clone)
            .expect("Project has an owned Any clone contract");
        assert!(
            main.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(&instruction.kind, MirInstructionKind::Drop { value } if value == bundle)
                })
            }),
            "the original tuple remains live and is explicitly dropped at function exit"
        );
        assert!(projection.id.0.contains("managed_any_tuple_load"));

        let bundle_type = &main.values.get(bundle).expect("bundle TypeDesc").ty;
        let tuple = catalog.get(bundle_type).expect("bundle descriptor");
        let MirLayout::Tuple(elements) = &tuple.layout else {
            panic!("bundle must retain its exact tuple layout");
        };
        assert_eq!(elements.as_slice(), &[managed_type, string_type]);
        for (selected_index, selected_type) in elements.iter().enumerate() {
            let error = catalog
                .validate_move_projection(
                    bundle_type,
                    selected_type,
                    &MirProjection::Tuple(selected_index),
                )
                .expect_err("consuming one field cannot discard the other owned field");
            assert!(
                error.contains("tuple leaves non-Copy sibling"),
                "selected field {selected_index}: {error}"
            );
        }
    }

    #[test]
    fn managed_any_cfg_rejects_a_stale_owner_in_a_fault_terminator() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let catalog = program.type_catalog();
        let owner = MirValueId::new("param:owner").expect("owner value");
        let block = flow_test_block(
            "bb.entry",
            vec![flow_test_drop("param:owner", "inst:drop.before.fault")],
            MirTerminator::Fault {
                value: Some(owner.clone()),
            },
        );
        let function = managed_any_flow_function(vec![block]);
        let managed_values = BTreeSet::from([owner]);

        let error = catalog
            .validate_managed_any_cfg_flow(&function, &managed_values)
            .expect_err("Fault cannot reference an Any owner already dropped on this path");
        assert!(
            error.contains("used after transfer, drop") || error.contains("not definitely live"),
            "{error}"
        );
    }

    #[test]
    fn managed_any_cfg_requires_the_checker_map_path_projection() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let mut main = get_main(&program);
        assert!(main.blocks.len() > 1);
        main.ownership
            .checker_ordinary_map_owned_get_i32_string_v1_cfg_receipt = None;

        let error = program
            .type_catalog()
            .validate_managed_any_function(&main)
            .expect_err("multi-block ManagedAny must be bound to Checker CFG evidence");
        assert!(error.contains("no Map-specific Checker receipt"), "{error}");
    }

    #[test]
    fn managed_any_multiblock_body_requires_the_generic_checker_cfg_projection() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let mut main = get_main(&program);
        let mut seal = main
            .ownership
            .checker_managed_any
            .clone()
            .expect("private ManagedAny body seal");
        main.ownership.checker_managed_any_cfg_receipt = None;
        seal.source_cfg = None;

        let error = program
            .type_catalog()
            .validate_managed_any_source_cfg(&main, &seal)
            .expect_err("a full-body seal does not substitute for source-CFG evidence");
        assert!(error.contains("no Checker source-CFG receipt"), "{error}");
    }

    #[test]
    fn managed_any_source_cfg_binds_entry_branch_and_instruction_origins() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let catalog = program.type_catalog();
        let main = get_main(&program);
        let seal = main
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("private ManagedAny body seal");

        let mut wrong_entry = seal.clone();
        let source_cfg = wrong_entry.source_cfg.as_mut().expect("Checker source CFG");
        let wrong_entry_block = source_cfg
            .reachable_blocks
            .keys()
            .find(|block| *block != &source_cfg.entry)
            .expect("non-entry source CFG block")
            .clone();
        source_cfg.entry = wrong_entry_block;
        let error = catalog
            .validate_managed_any_source_cfg(&main, &wrong_entry)
            .expect_err("the Checker entry is bound to the MIR entry");
        assert!(error.contains("entry"), "{error}");

        let (branch_block, then_target, else_target, original_condition) = main
            .blocks
            .iter()
            .find_map(|(id, block)| match &block.terminator {
                MirTerminator::Branch {
                    condition,
                    then_target,
                    else_target,
                    ..
                } => Some((
                    id.clone(),
                    then_target.clone(),
                    else_target.clone(),
                    condition.clone(),
                )),
                _ => None,
            })
            .expect("source fixture contains If branches");
        let other_bool = main
            .values
            .values()
            .find(|value| {
                value.id != original_condition
                    && value.ty == crate::core::ir::primitive_type_id(PrimitiveType::Bool)
            })
            .expect("source fixture has another distinct Bool value")
            .id
            .clone();
        let mut swapped_condition = main.clone();
        let MirTerminator::Branch { condition, .. } = &mut swapped_condition
            .blocks
            .get_mut(&branch_block)
            .expect("branch block")
            .terminator
        else {
            unreachable!();
        };
        *condition = other_bool;
        let error = catalog
            .validate_managed_any_source_cfg(&swapped_condition, seal)
            .expect_err("a dominating Bool cannot replace this source If condition");
        assert!(
            error.contains("condition") || error.contains("selector"),
            "{error}"
        );

        let then_origin = main
            .blocks
            .get(&then_target)
            .and_then(|block| block.instructions.first())
            .map(|instruction| instruction.id.clone())
            .expect("then arm instruction");
        let else_origin = main
            .blocks
            .get(&else_target)
            .and_then(|block| block.instructions.first())
            .and_then(|instruction| {
                seal.instructions
                    .iter()
                    .find(|(block, _, sealed)| block == &else_target && sealed.id == instruction.id)
                    .map(|(_, origin, _)| origin.clone())
            })
            .expect("else arm instruction has a sealed source origin");
        let mut crossed_origin = seal.clone();
        let (_, origin, _) = crossed_origin
            .instructions
            .iter_mut()
            .find(|(block, _, instruction)| block == &then_target && instruction.id == then_origin)
            .expect("then arm instruction has a sealed source origin");
        *origin = else_origin;
        let error = catalog
            .validate_managed_any_source_cfg(&main, &crossed_origin)
            .expect_err("an instruction origin from the opposite arm cannot authorize this block");
        assert!(error.contains("own source block"), "{error}");
    }

    #[test]
    fn managed_any_cfg_rejects_unsettled_string_from_one_if_arm_at_the_join() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let catalog = program.type_catalog();
        let (main, managed_values, _string) = managed_any_branch_string_fixture(false);

        let error = catalog
            .validate_managed_any_cfg_flow(&main, &managed_values)
            .expect_err("unsettled path-local String must fail at the diamond join");
        assert!(error.contains("state disagrees at join"), "{error}");
    }

    #[test]
    fn managed_any_cfg_rejects_stale_string_uses_after_an_arm_drop() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let catalog = program.type_catalog();
        for forged_kind in ["duplicate-drop", "stale-clone"] {
            let (mut main, managed_values, unpacked) = managed_any_branch_string_fixture(true);
            let join = MirBlockId::new("bb.join").expect("join block id");
            let forged_id = MirInstructionId::new(format!("forged:{forged_kind}"))
                .expect("synthetic test instruction id");
            let forged_instruction = match forged_kind {
                "duplicate-drop" => MirInstructionKind::Drop {
                    value: unpacked.clone(),
                },
                "stale-clone" => MirInstructionKind::Clone {
                    source: unpacked.clone(),
                    result: MirValueId::new("forged:stale-clone-result")
                        .expect("synthetic result id"),
                },
                _ => unreachable!(),
            };
            if forged_kind == "stale-clone" {
                let stale_result =
                    MirValueId::new("forged:stale-clone-result").expect("synthetic result id");
                main.values.insert(
                    stale_result.clone(),
                    crate::core::mir::MirValue {
                        id: stale_result,
                        ty: crate::core::ir::primitive_type_id(PrimitiveType::String),
                    },
                );
            }
            main.blocks
                .get_mut(&join)
                .expect("diamond join exists")
                .instructions
                .insert(
                    0,
                    MirInstruction {
                        id: forged_id,
                        kind: forged_instruction,
                    },
                );

            let error = catalog
                .validate_managed_any_cfg_flow(&main, &managed_values)
                .expect_err("an arm-consumed String is never live at the common join");
            assert!(
                error.contains("used after transfer, drop")
                    || error.contains("not definitely live"),
                "{error}"
            );
        }

        let (mut main, managed_values, unpacked) = managed_any_branch_string_fixture(true);
        let arm = MirBlockId::new("bb.then").expect("then block id");
        main.blocks
            .get_mut(&arm)
            .expect("read arm exists")
            .terminator = MirTerminator::Return {
            value: Some(unpacked),
        };
        let error = catalog
            .validate_managed_any_cfg_flow(&main, &managed_values)
            .expect_err("a stale String cannot be returned after its arm Drop");
        assert!(
            error.contains("used after transfer, drop") || error.contains("not definitely live"),
            "{error}"
        );
    }

    #[test]
    fn managed_any_cfg_tracks_string_binary_results_even_with_unrelated_inputs() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let catalog = program.type_catalog();
        let owner = MirValueId::new("param:owner").expect("ManagedAny parameter");
        let condition = MirValueId::new("const:condition").expect("branch condition");
        let left = MirValueId::new("const:left").expect("left String");
        let right = MirValueId::new("const:right").expect("right String");
        let result = MirValueId::new("result:branch-string").expect("String Binary result");
        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
        let make_function = |settle_result: bool| {
            let entry = flow_test_block(
                "bb.entry",
                vec![
                    MirInstruction {
                        id: MirInstructionId::new("inst:const.left").unwrap(),
                        kind: MirInstructionKind::Const {
                            result: left.clone(),
                            literal: ResolvedLiteral::String("left".into()),
                        },
                    },
                    MirInstruction {
                        id: MirInstructionId::new("inst:const.right").unwrap(),
                        kind: MirInstructionKind::Const {
                            result: right.clone(),
                            literal: ResolvedLiteral::String("right".into()),
                        },
                    },
                ],
                MirTerminator::Branch {
                    condition: condition.clone(),
                    then_edge: crate::core::mir::MirEdgeId::new("edge:then").unwrap(),
                    then_target: MirBlockId::new("bb.then").unwrap(),
                    then_arguments: Vec::new(),
                    else_edge: crate::core::mir::MirEdgeId::new("edge:else").unwrap(),
                    else_target: MirBlockId::new("bb.else").unwrap(),
                    else_arguments: Vec::new(),
                },
            );
            let mut then_instructions = vec![MirInstruction {
                id: MirInstructionId::new("inst:branch.concat").unwrap(),
                kind: MirInstructionKind::Binary {
                    result: result.clone(),
                    op: ResolvedBinaryOp::Add,
                    left: left.clone(),
                    right: right.clone(),
                },
            }];
            if settle_result {
                then_instructions.push(flow_test_drop(
                    "result:branch-string",
                    "inst:branch.concat.drop",
                ));
            }
            then_instructions.push(flow_test_drop("param:owner", "inst:owner.then.drop"));
            let then_block = flow_test_block(
                "bb.then",
                then_instructions,
                MirTerminator::Goto {
                    edge: crate::core::mir::MirEdgeId::new("edge:then.join").unwrap(),
                    target: MirBlockId::new("bb.join").unwrap(),
                    arguments: Vec::new(),
                },
            );
            let else_block = flow_test_block(
                "bb.else",
                vec![flow_test_drop("param:owner", "inst:owner.else.drop")],
                MirTerminator::Goto {
                    edge: crate::core::mir::MirEdgeId::new("edge:else.join").unwrap(),
                    target: MirBlockId::new("bb.join").unwrap(),
                    arguments: Vec::new(),
                },
            );
            let join =
                flow_test_block("bb.join", Vec::new(), MirTerminator::Return { value: None });
            let mut function = managed_any_flow_function(vec![entry, then_block, else_block, join]);
            for value in [
                super::super::MirValue {
                    id: left.clone(),
                    ty: string_type.clone(),
                },
                super::super::MirValue {
                    id: right.clone(),
                    ty: string_type.clone(),
                },
                super::super::MirValue {
                    id: result.clone(),
                    ty: string_type.clone(),
                },
            ] {
                function.values.insert(value.id.clone(), value);
            }
            function
        };
        let managed_values = BTreeSet::from([owner]);
        catalog
            .validate_managed_any_cfg_flow(&make_function(true), &managed_values)
            .expect("each read-arm String result is dropped on its path");
        let error = catalog
            .validate_managed_any_cfg_flow(&make_function(false), &managed_values)
            .expect_err("every owned String Binary result in an arm must settle on all paths");
        assert!(error.contains("state disagrees at join"), "{error}");
    }

    #[test]
    fn original_get_any_pure_i32_and_any_escape_are_checker_sealed() {
        let checked = checked(SOURCE);
        assert!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_get_i32_string_v1_complete()
        );
        let program =
            MirProgram::from_checked_program(&checked).expect("canonical pure i32 ordinary GetAny");
        program
            .validate_managed_any_identity_instances()
            .expect("closed generic ManagedAny identity table");
        assert!(program
            .type_catalog()
            .is_managed_any_i32_string_v1(&managed_any_i32_string_v1_type_id()));
        for function in program.functions().values() {
            program
                .type_catalog()
                .validate_managed_any_function(function)
                .expect("sealed owner ledger");
        }
    }

    #[test]
    fn original_get_any_pure_i32_and_any_escape_receipt_records_result_owners_and_cfg() {
        let checked = checked(SOURCE);
        let callable = checked
            .callable(&NodeId("function:main".into()))
            .expect("checked main callable");
        let receipt = callable
            .resources
            .ordinary_map_owned_get_any_tuple_result_v1_receipt
            .as_ref()
            .expect("dedicated legacy tuple Result-aware receipt");
        assert_eq!(receipt.owner, NodeId("function:main".into()));
        assert_eq!(receipt.result_branches.len(), 4);
        assert_eq!(receipt.owner_chains.len(), 2);
        assert_eq!(receipt.owned_get_attempts.len(), 6);
        assert_eq!(receipt.any_v2_attempts.len(), 6);
        assert_eq!(receipt.map_actions.len(), 18);
        let count_map_actions = |kind| {
            receipt
                .map_actions
                .iter()
                .filter(|action| action.kind == kind)
                .count()
        };
        use crate::core::ownership::OrdinaryMapOwnedGetAnyTupleResultV1MapActionKind as MapAction;
        assert_eq!(count_map_actions(MapAction::NewResult), 2);
        assert_eq!(count_map_actions(MapAction::SetResult), 2);
        assert_eq!(count_map_actions(MapAction::GetTuple), 2);
        assert_eq!(count_map_actions(MapAction::RootDrop), 6);
        assert_eq!(count_map_actions(MapAction::PairDrop), 2);
        assert_eq!(count_map_actions(MapAction::PayloadDrop), 2);
        assert_eq!(count_map_actions(MapAction::CarriedDrop), 2);
        assert_eq!(receipt.cfg.owner, NodeId("function:main".into()));
        assert!(!receipt.source_body_commitment.is_empty());
        let [new, set, second_new, second_set] = receipt.result_branches.as_slice() else {
            panic!("two New and two Set Result branches");
        };
        assert_eq!(new.operation_kind, MapAction::NewResult);
        for new_branch in [new, second_new] {
            assert!(new_branch.source_root.is_none());
            assert!(!new_branch.err_creates_root && !new_branch.err_publishes_updated_root);
            assert!(!new_branch.err_consumes_source_root);
            assert!(new_branch.ok_root_drop.is_none() && new_branch.err_root_drop.is_none());
        }
        assert_eq!(
            set.operation_kind,
            crate::core::ownership::OrdinaryMapOwnedGetAnyTupleResultV1MapActionKind::SetResult
        );
        for set_branch in [set, second_set] {
            assert!(set_branch.source_root.is_some());
            assert!(!set_branch.err_creates_root && !set_branch.err_publishes_updated_root);
            assert!(set_branch.err_consumes_source_root);
            assert!(set_branch.ok_root_drop.is_some() && set_branch.err_root_drop.is_some());
            assert_ne!(set_branch.ok_root_drop, set_branch.err_root_drop);
        }
        assert_eq!(second_new.operation_kind, new.operation_kind);
        assert_eq!(second_set.operation_kind, set.operation_kind);
        for chain in &receipt.owner_chains {
            assert!(!chain.key_node.0.is_empty());
            assert!(!chain.key_value.is_empty());
            assert_ne!(chain.pair_drop, chain.payload_drop);
            assert_ne!(chain.payload_drop, chain.carried_drop);
            assert_ne!(chain.root_drop, chain.pair_drop);
            assert_ne!(chain.root_local, chain.pair_local);
            assert_ne!(chain.pair_local, chain.payload_local);
            assert_ne!(chain.payload_local, chain.carried_local);
            assert_eq!(chain.observation_points.len(), 3);
        }
    }

    #[test]
    fn legacy_tuple_get_any_l2_route_replays_its_exact_source_receipt() {
        let checked_program = checked(SOURCE);
        let owner = NodeId("function:main".into());
        let admission = crate::core::mir::classify_canonical_mir_route_admission(&checked_program);
        assert!(
            !admission.ordinary_map_owned_get_i32_string_v1_complete(),
            "the tuple-valued map_get receipt cannot borrow typed-owned-get V1 admission"
        );
        assert!(
            !admission.ordinary_map_owned_any_v2_complete(),
            "the tuple L2 receipt does not authorize the broad V2 execution profile"
        );
        let source_receipt = checked_program
            .callable(&owner)
            .expect("checked main")
            .resources
            .ordinary_map_owned_get_any_tuple_result_v1_receipt
            .as_ref()
            .expect("dedicated exact Checker receipt");

        let (l2_program, l2_route) =
            crate::core::mir::materialize_ordinary_map_owned_get_any_tuple_l2_verifier_route(
                &checked_program,
            )
            .expect("the exact Checker receipt builds its dedicated L2 proof projection");
        assert_eq!(
            l2_route.profile,
            crate::core::mir::ORDINARY_MAP_OWNED_GET_ANY_TUPLE_L2_VERIFIER_PROFILE
        );
        assert_ne!(
            l2_route.profile,
            crate::core::mir::ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND
        );
        assert_eq!(
            l2_program.purpose(),
            crate::core::mir::reference::MirProgramPurpose::VerifierOnly
        );
        let l2_main = l2_program.functions().get(&owner).expect("L2 main");
        let projection = l2_main
            .ownership
            .ordinary_map_owned_get_any_tuple_l2_projection()
            .expect("dedicated L2 projection receipt");
        assert_eq!(projection.source_receipt, *source_receipt);
        assert_ne!(
            projection.root_type,
            crate::core::mir::types::ordinary_map_owned_get_any_tuple_result_v1_root_type_id()
        );
        assert_ne!(
            projection.root_type,
            crate::core::mir::types::ordinary_map_owned_any_v2_type_id()
        );
        assert_ne!(
            projection.result_type,
            crate::core::mir::types::ordinary_map_owned_get_any_tuple_result_v1_root_result_type_id(
            )
        );
        l2_route
            .validate_against_program(&l2_program)
            .expect("dedicated L2 route replays the marker receipt and TypeDesc catalog");
        let l2_verification =
            crate::verifier::verify_mir_with_ordinary_map_owned_get_any_tuple_l2_verifier_route(
                &checked_program,
                &l2_program,
                &l2_route,
                String::new(),
            )
            .expect("receipt-only L2 capability accepts the contract-free source");
        assert_eq!(
            l2_verification[0].status,
            crate::verifier::VerifStatus::NoObligations
        );
        assert!(l2_verification[0]
            .message
            .contains("runtime tuple ownership was not proven"));
        assert!(crate::verifier::verify_mir_with_route_receipt(
            &l2_program,
            &l2_route,
            String::new()
        )
        .is_err());

        let mut missing_marker = l2_program.clone();
        missing_marker
            .function_mut_for_test(&owner)
            .expect("L2 main")
            .blocks
            .values_mut()
            .next()
            .expect("L2 entry block")
            .instructions
            .pop();
        assert!(l2_route.validate_against_program(&missing_marker).is_err());
        assert!(
            crate::core::mir::validate_ordinary_map_owned_get_any_tuple_l2_verifier_route(
                &checked_program,
                &missing_marker
            )
            .is_err()
        );

        let mut forged_marker = l2_program.clone();
        forged_marker
            .function_mut_for_test(&owner)
            .expect("L2 main")
            .blocks
            .values_mut()
            .next()
            .expect("L2 entry block")
            .instructions[0]
            .id = crate::core::mir::MirInstructionId::new("inst:forged_l2_marker")
            .expect("forged marker id");
        assert!(l2_route.validate_against_program(&forged_marker).is_err());

        let mut stripped_projection = l2_program.clone();
        stripped_projection
            .function_mut_for_test(&owner)
            .expect("L2 main")
            .ownership
            .checker_ordinary_map_owned_get_any_tuple_l2_projection = None;
        assert!(l2_route
            .validate_against_program(&stripped_projection)
            .is_err());

        let changed_source = SOURCE.replacen(
            r#"map_get(root, "k\0🧪")"#,
            r#"map_get(root, "changed")"#,
            1,
        );
        let changed_checked = checked(&changed_source);
        assert!(
            crate::core::mir::materialize_ordinary_map_owned_get_any_tuple_l2_verifier_route(
                &changed_checked
            )
            .is_err()
        );
        assert!(changed_checked
            .callable(&owner)
            .expect("mutated checked main")
            .resources
            .ordinary_map_owned_get_any_tuple_result_v1_receipt
            .is_none());
        let changed_admission =
            crate::core::mir::classify_canonical_mir_route_admission(&changed_checked);
        assert!(
            !changed_admission.ordinary_map_owned_get_i32_string_v1_complete(),
            "generic typed-owned-get V1 cannot replace the missing tuple receipt"
        );
        assert_eq!(
            changed_admission.ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::IncompleteCoverage,
            "a changed tuple source leaves the broad V2 candidate fail-closed"
        );
        let catalog_errors = MirTypeCatalog::from_checked_program(&changed_checked)
            .expect_err("missing tuple receipt must keep the broad V2 candidate fail-closed");
        assert!(catalog_errors.iter().any(|error| {
            error.contains(
                "ordinary Map/Any owner-v2 TypeCatalog candidate has incomplete or conflicting coverage",
            )
        }), "unexpected TypeCatalog diagnostics: {catalog_errors:?}");
    }

    #[test]
    fn original_get_any_pure_i32_and_any_escape_receipt_rejects_source_mutations() {
        let source_root_cleanup =
            SOURCE.replacen("drop(empty)\n                    status", "status", 1);
        let extra_map_attempt = SOURCE.replace(
            r#"let pair = map_get(root, "k\0🧪")
                    drop(empty)"#,
            r#"let pair = map_get(root, "k\0🧪")
                    let extra_pair = map_get(root, "k\0🧪")
                    drop(extra_pair)
                    drop(empty)"#,
        );
        let missing_pair_drop = SOURCE.replacen(
            "drop(pair)\n                    drop(payload)",
            "drop(payload)",
            1,
        );
        let missing_payload_drop = SOURCE.replacen(
            "drop(payload)\n                    drop(carried)",
            "drop(carried)",
            1,
        );
        let missing_carried_drop = SOURCE.replacen(
            "drop(carried)\n\n                    match map_new()",
            "\n                    match map_new()",
            1,
        );
        let duplicate_pair_drop = SOURCE.replacen(
            "drop(pair)\n                    drop(payload)",
            "drop(pair)\n                    drop(pair)\n                    drop(payload)",
            1,
        );
        let duplicate_payload_drop = SOURCE.replacen(
            "drop(payload)\n                    drop(carried)",
            "drop(payload)\n                    drop(payload)\n                    drop(carried)",
            1,
        );
        let duplicate_carried_drop = SOURCE.replacen(
            "drop(carried)\n\n                    match map_new()",
            "drop(carried)\n                    drop(carried)\n\n                    match map_new()",
            1,
        );

        // The extra read is otherwise well-typed and fully dropped; it must
        // still miss the fixed six-attempt receipt profile.
        let checked_extra = checked(&extra_map_attempt);
        let extra_callable = checked_extra
            .callable(&NodeId("function:main".into()))
            .expect("extra-attempt source remains checked");
        assert!(extra_callable
            .resources
            .ordinary_map_owned_get_any_tuple_result_v1_receipt
            .is_none());

        for source in [
            source_root_cleanup,
            missing_pair_drop,
            missing_payload_drop,
            missing_carried_drop,
            duplicate_pair_drop,
            duplicate_payload_drop,
            duplicate_carried_drop,
        ] {
            assert_ne!(
                source.as_str(),
                SOURCE,
                "each negative case must alter SOURCE"
            );
            let tokens = crate::lexer::Lexer::new(&source)
                .tokenize()
                .expect("mutated source lexes");
            let file = crate::parser::Parser::new(tokens)
                .parse_file()
                .expect("mutated source parses");
            if let Ok(checked) = crate::core::check_program(&file) {
                let callable = checked
                    .callable(&NodeId("function:main".into()))
                    .expect("mutated checked main callable");
                assert!(
                    callable
                        .resources
                        .ordinary_map_owned_get_any_tuple_result_v1_receipt
                        .is_none(),
                    "source mutation must not receive a checked receipt"
                );
            }
        }
    }

    #[test]
    fn generic_any_identity_template_requires_exact_checker_source_receipt() {
        let checked = checked(SOURCE);
        let catalog = MirTypeCatalog::from_checked_program(&checked).expect("TypeDesc catalog");
        let callable = checked
            .callable(&NodeId("function:carry".into()))
            .expect("identity template");
        let function = super::super::lower::lower_callable_with_type_catalog(callable, &catalog)
            .expect("untouched polymorphic MIR");
        assert!(
            function.ownership.checker_managed_any.is_none(),
            "opaque T has no Any owner seal"
        );
        let argument = catalog
            .iter()
            .find_map(|(id, descriptor)| {
                (descriptor.kind == MirTypeKind::DynamicAny).then_some(id.clone())
            })
            .expect("Checker DynamicAny identity");
        let capture = |callable: &crate::core::ResolvedCallable| {
            checked_managed_any_identity_template(
                callable,
                &checked,
                &catalog,
                &function,
                std::slice::from_ref(&argument),
            )
        };
        assert!(
            capture(callable).is_ok(),
            "exact Checker template authorizes specialization"
        );
        for mutation in [
            "missing",
            "owner",
            "entry",
            "return",
            "point",
            "permission",
            "mutable",
            "body",
        ] {
            let mut forged = callable.clone();
            match mutation {
                "missing" => forged.resources.managed_any_cfg_receipt = None,
                "owner" => {
                    forged
                        .resources
                        .managed_any_cfg_receipt
                        .as_mut()
                        .unwrap()
                        .owner = NodeId("function:forged".into())
                }
                "entry" => {
                    forged
                        .resources
                        .managed_any_cfg_receipt
                        .as_mut()
                        .unwrap()
                        .entry = crate::core::cfg::BasicBlockId(NodeId("forged:entry".into()))
                }
                "return" => {
                    let receipt = forged.resources.managed_any_cfg_receipt.as_mut().unwrap();
                    receipt
                        .reachable_blocks
                        .get_mut(&receipt.entry)
                        .unwrap()
                        .terminator = crate::core::cfg::Terminator::Return {
                        value: None,
                        implicit: true,
                    };
                }
                "point" => {
                    let receipt = forged.resources.managed_any_cfg_receipt.as_mut().unwrap();
                    receipt
                        .reachable_blocks
                        .get_mut(&receipt.entry)
                        .unwrap()
                        .points
                        .clear();
                }
                "permission" => {
                    forged.signature.parameters[0].permission =
                        Some(crate::core::ir::Permission::View)
                }
                "mutable" => forged.signature.parameters[0].mutable = true,
                "body" => {
                    forged.body.root.result.as_deref_mut().unwrap().node_id =
                        NodeId("forged:result".into())
                }
                _ => unreachable!(),
            }
            assert!(
                capture(&forged).is_err(),
                "{mutation} must withhold template authority"
            );
        }
        for argument in [
            managed_any_i32_string_v1_type_id(),
            crate::core::ir::primitive_type_id(PrimitiveType::I32),
        ] {
            assert!(
                checked_managed_any_identity_template(
                    callable,
                    &checked,
                    &catalog,
                    &function,
                    &[argument]
                )
                .is_err(),
                "only the exact Checker DynamicAny argument is admitted"
            );
        }

        let program = MirProgram::from_checked_program(&checked).expect("sealed concrete identity");
        let instance = program
            .instances()
            .values()
            .find(|instance| instance.contract == MirGenericInstanceContract::ManagedAnyIdentity)
            .unwrap();
        let mut specialized = program.functions()[&instance.function].clone();
        specialized.ownership.checker_managed_any = None;
        let proof = capture(callable).expect("immutable source proof");
        seal_checked_managed_any_identity_instance(&specialized, &catalog, proof)
            .expect("exact closed specialization");
        for mutation in ["owner", "order", "origin", "type"] {
            let mut forged = specialized.clone();
            match mutation {
                "owner" => forged.owner = NodeId("function:forged".into()),
                "order" => forged
                    .blocks
                    .get_mut(&forged.entry)
                    .unwrap()
                    .instructions
                    .swap(0, 1),
                "origin" => {
                    let instruction =
                        &mut forged.blocks.get_mut(&forged.entry).unwrap().instructions[0];
                    let MirInstructionKind::ManagedAny {
                        contract: Some(contract),
                        ..
                    } = &mut instruction.kind
                    else {
                        panic!("identity clone")
                    };
                    contract.origin = NodeId("forged:origin".into());
                }
                "type" => forged.result = argument.clone(),
                _ => unreachable!(),
            }
            assert!(
                seal_checked_managed_any_identity_instance(
                    &forged,
                    &catalog,
                    capture(callable).unwrap()
                )
                .is_err(),
                "{mutation} cannot reuse the immutable template proof"
            );
        }
    }

    #[test]
    fn generic_any_non_direct_identity_remains_rejected() {
        for body in [
            "leaf(value)",
            "let rebound = value; rebound",
            "if true { value } else { value }",
        ] {
            let source = SOURCE.replace(
                "func carry<T>(value: T) -> T { value }",
                &format!("func leaf<T>(value: T) -> T {{ value }}\nfunc carry<T>(value: T) -> T {{ {body} }}"),
            );
            let checked = checked(&source);
            assert!(
                MirProgram::from_checked_program(&checked).is_err(),
                "non-direct body {body} must not acquire the exact identity profile"
            );
        }
    }

    #[test]
    fn managed_any_values_call_requires_exact_zero_argument_checker_receipt() {
        let checked = checked(include_str!(
            "../../../tests/fixtures/mir_ordinary_map_owned_any_v2_values_producer_caller.mimi"
        ));
        let program =
            MirProgram::from_checked_program(&checked).expect("Values producer/caller bridge");
        let caller = program
            .functions()
            .get(&NodeId("function:direct_caller".into()))
            .unwrap();
        let managed_values = &caller
            .ownership
            .checker_managed_any
            .as_ref()
            .unwrap()
            .managed_values;
        let catalog = program.type_catalog();
        catalog
            .validate_managed_any_cfg_flow(caller, managed_values)
            .expect("exact zero-argument producer edge");
        for mutation in ["missing", "receipt", "target", "type_argument", "argument"] {
            let mut forged = caller.clone();
            if mutation == "missing" {
                forged
                    .ownership
                    .checker_ordinary_map_owned_any_v2_values_result_producer_caller_receipt = None;
                if let Some(seal) = forged.ownership.checker_managed_any.as_mut() {
                    seal.v2_values_result_producer_caller = None;
                }
            } else if mutation == "receipt" {
                forged
                    .ownership
                    .checker_ordinary_map_owned_any_v2_values_result_producer_caller_receipt
                    .as_mut()
                    .unwrap()
                    .producer_target = NodeId("function:forged".into());
                if let Some(seal) = forged.ownership.checker_managed_any.as_mut() {
                    seal.v2_values_result_producer_caller
                        .as_mut()
                        .unwrap()
                        .producer_target = NodeId("function:forged".into());
                }
            } else {
                let instruction = forged.blocks.values_mut().flat_map(|block| &mut block.instructions).find(|instruction| matches!(&instruction.kind, MirInstructionKind::Call { result: Some(result), .. } if caller.values.get(result).is_some_and(|value| value.ty == ordinary_map_owned_any_v2_values_type_id()))).expect("Values producer Call");
                let MirInstructionKind::Call {
                    callee,
                    type_arguments,
                    arguments,
                    result: Some(result),
                    ..
                } = &mut instruction.kind
                else {
                    unreachable!()
                };
                match mutation {
                    "target" => {
                        *callee = crate::core::ir::ResolvedCallee::Function(NodeId(
                            "function:forged".into(),
                        ))
                    }
                    "type_argument" => {
                        type_arguments.push(crate::core::ir::primitive_type_id(PrimitiveType::I32))
                    }
                    "argument" => arguments.push(result.clone()),
                    _ => unreachable!(),
                }
            }
            let error = catalog
                .validate_managed_any_cfg_flow(&forged, managed_values)
                .expect_err("unreceipted Values transfer must fail closed");
            assert!(
                error.contains("Values call") && error.contains("Checker producer/caller"),
                "{mutation}: {error}"
            );
        }
    }

    #[test]
    fn managed_any_map_root_get_any_caller_seals_both_checker_receipt_copies() {
        let checked = checked(include_str!(
            "../../../tests/fixtures/mir_ordinary_map_owned_any_v2_map_root_get_any_caller.mimi"
        ));
        let program = MirProgram::from_checked_program(&checked)
            .expect("Map-root → GetAny producer/caller pair");
        let functions = program.functions();
        let producer = functions
            .get(&NodeId("function:make_root".into()))
            .expect("producer MIR");
        let caller = functions
            .get(&NodeId("function:direct_caller".into()))
            .expect("caller MIR");
        let producer_cfg_receipt = producer
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .and_then(|cfg| cfg.map_root_get_any_caller_receipt.as_ref())
            .expect("producer CFG GetAny-caller receipt");
        let direct_caller_receipt = caller
            .ownership
            .checker_ordinary_map_owned_any_v2_map_root_get_any_caller_receipt
            .as_deref()
            .expect("caller-local GetAny-caller receipt");
        assert_eq!(producer_cfg_receipt, direct_caller_receipt);
        assert_eq!(
            producer
                .ownership
                .checker_managed_any
                .as_ref()
                .unwrap()
                .v2_map_root_get_any_caller
                .as_ref(),
            Some(producer_cfg_receipt),
            "producer seal snapshots its CFG receipt"
        );
        assert_eq!(
            caller
                .ownership
                .checker_managed_any
                .as_ref()
                .unwrap()
                .v2_map_root_get_any_caller
                .as_ref(),
            Some(direct_caller_receipt),
            "caller seal snapshots its local receipt"
        );
        let catalog = program.type_catalog();
        catalog
            .validate_managed_any_function(producer)
            .expect("producer return is admitted by the independent GetAny bridge");
        catalog
            .validate_managed_any_function(caller)
            .expect("caller GetAny path agrees with its independent receipt and body seal");

        let mut missing_producer_receipt = producer.clone();
        missing_producer_receipt
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_mut()
            .unwrap()
            .map_root_get_any_caller_receipt = None;
        assert!(catalog
            .validate_managed_any_function(&missing_producer_receipt)
            .is_err());

        let mut missing_caller_receipt = caller.clone();
        missing_caller_receipt
            .ownership
            .checker_ordinary_map_owned_any_v2_map_root_get_any_caller_receipt = None;
        assert!(catalog
            .validate_managed_any_function(&missing_caller_receipt)
            .is_err());

        let mut altered_receipt = caller.clone();
        altered_receipt
            .ownership
            .checker_ordinary_map_owned_any_v2_map_root_get_any_caller_receipt
            .as_mut()
            .unwrap()
            .unpack_cast = NodeId("expr:forged_unpack_cast".into());
        assert!(catalog
            .validate_managed_any_function(&altered_receipt)
            .is_err());

        let mut altered_body = caller.clone();
        let get_any = altered_body
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { .. }
                )
            })
            .expect("receipted GetAny instruction");
        get_any.id = MirInstructionId::new("inst:forged_get_any").unwrap();
        assert!(catalog
            .validate_managed_any_function(&altered_body)
            .is_err());
    }

    #[test]
    fn managed_any_map_root_producer_caller_bridge_is_exact_and_separate_from_values() {
        let checked = checked(include_str!(
            "../../../tests/fixtures/mir_ordinary_map_owned_any_v2_map_root_producer_caller.mimi"
        ));
        let program =
            MirProgram::from_checked_program(&checked).expect("Map-root producer/caller bridge");
        let functions = program.functions();
        let producer = functions
            .get(&NodeId("function:make_root".into()))
            .expect("producer MIR");
        let caller = functions
            .get(&NodeId("function:direct_caller".into()))
            .expect("caller MIR");
        let cfg = producer
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .expect("producer V2 CFG receipt");
        let producer_return = cfg
            .map_root_producer_return_receipt
            .as_ref()
            .expect("Map-root producer receipt");
        let producer_caller = cfg
            .map_root_producer_caller_receipt
            .as_ref()
            .expect("paired unique caller receipt");
        let caller_receipt = caller
            .ownership
            .checker_ordinary_map_owned_any_v2_map_root_caller_receipt
            .as_deref()
            .expect("independent caller receipt");
        assert_eq!(caller_receipt, producer_caller);
        assert_eq!(
            producer
                .ownership
                .checker_ordinary_map_owned_any_v2_values_caller_receipt,
            None,
            "Map-root bridge authority stays separate from Values receipts"
        );
        assert_eq!(
            caller
                .ownership
                .checker_ordinary_map_owned_any_v2_values_caller_receipt,
            None,
            "caller stores Map-root authority in its own receipt slot"
        );
        assert_eq!(producer.owner, producer_return.owner);
        assert_eq!(producer.owner, producer_caller.producer_owner);
        assert_eq!(
            producer_return.return_value,
            producer_caller.producer_return_value
        );
        assert_eq!(
            producer_return.final_root_local,
            producer_caller.producer_final_local
        );
        let producer_seal = producer.ownership.checker_managed_any.as_ref().unwrap();
        assert_eq!(
            producer_seal.v2_map_root_producer_return.as_ref(),
            Some(producer_return)
        );
        assert_eq!(
            producer_seal.v2_map_root_producer_caller.as_ref(),
            Some(producer_caller)
        );
        let caller_seal = caller.ownership.checker_managed_any.as_ref().unwrap();
        assert_eq!(
            caller_seal.v2_map_root_producer_return, None,
            "caller seal does not adopt the producer CFG receipt as local authority"
        );
        assert_eq!(
            caller_seal.v2_map_root_producer_caller.as_ref(),
            Some(caller_receipt)
        );

        let caller_root =
            MirValueId::new(format!("local:{}", caller_receipt.caller_root_local.0 .0)).unwrap();
        let call_result = MirValueId::new(format!("expr:{}", caller_receipt.call_node.0)).unwrap();
        let size_result =
            MirValueId::new(format!("expr:{}", caller_receipt.map_size_node.0)).unwrap();
        let call_instruction_id =
            MirInstructionId::new(format!("inst:call:{}", caller_receipt.call_node.0)).unwrap();
        let bind_instruction_id = MirInstructionId::new(format!(
            "inst:bind:{}",
            caller_receipt.result_binding_point.0
        ))
        .unwrap();
        let size_instruction_id = MirInstructionId::new(format!(
            "inst:ordinary_map_owned_any_v2_size:{caller_root}:{}",
            caller_receipt.map_size_node.0
        ))
        .unwrap();
        let drop_instruction_id = MirInstructionId::new(format!(
            "inst:ordinary_map_owned_any_v2_drop:{caller_root}:{}",
            caller_receipt.root_drop_point.0
        ))
        .unwrap();
        let instructions = caller
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .collect::<Vec<_>>();
        assert!(instructions.iter().any(|instruction| {
            instruction.id == call_instruction_id
                && matches!(
                    &instruction.kind,
                    MirInstructionKind::Call {
                        result: Some(result),
                        callee: crate::core::ir::ResolvedCallee::Function(callee),
                        type_arguments,
                        arguments,
                        effect_receipts,
                        ..
                    } if result == &call_result
                        && callee == &caller_receipt.call_target
                        && type_arguments.is_empty()
                        && arguments.is_empty()
                        && effect_receipts.is_empty()
                )
        }));
        assert!(instructions.iter().any(|instruction| {
            instruction.id == bind_instruction_id
                && matches!(
                    &instruction.kind,
                    MirInstructionKind::Move { source, result }
                        if source == &call_result && result == &caller_root
                )
        }));
        assert!(instructions.iter().any(|instruction| {
            instruction.id == size_instruction_id
                && matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Size { result, root }
                        if result == &size_result && root == &caller_root
                )
        }));
        assert!(instructions.iter().any(|instruction| {
            instruction.id == drop_instruction_id
                && matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { root }
                        if root == &caller_root
                )
        }));
        let catalog = program.type_catalog();
        for function in [producer, caller] {
            catalog
                .validate_managed_any_function(function)
                .expect("exact Map-root bridge function seal");
            let managed_values = &function
                .ownership
                .checker_managed_any
                .as_ref()
                .unwrap()
                .managed_values;
            catalog
                .validate_managed_any_cfg_flow(function, managed_values)
                .expect("exact Map-root bridge ownership flow");
        }

        let managed_values = &caller_seal.managed_values;
        let mut missing_drop = caller.clone();
        for block in missing_drop.blocks.values_mut() {
            block.instructions.retain(|instruction| {
                !matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { root }
                        if root == &caller_root
                )
            });
        }
        let error = catalog
            .validate_managed_any_cfg_flow(&missing_drop, managed_values)
            .expect_err("the unique caller must explicitly settle the returned root");
        assert!(
            error.contains("Map-root caller") || error.contains("Drop"),
            "{error}"
        );

        let mut extra_operation = caller.clone();
        let mut extra_size = (**instructions
            .iter()
            .find(|instruction| instruction.id == size_instruction_id)
            .expect("receipted Size"))
        .clone();
        extra_size.id = MirInstructionId::new(format!(
            "inst:ordinary_map_owned_any_v2_size:{caller_root}:forged-extra"
        ))
        .unwrap();
        extra_operation
            .blocks
            .values_mut()
            .next()
            .unwrap()
            .instructions
            .push(extra_size);
        let error = catalog
            .validate_managed_any_cfg_flow(&extra_operation, managed_values)
            .expect_err("an extra unreceipted root operation must fail closed");
        assert!(
            error.contains("Size") || error.contains("receipt"),
            "{error}"
        );

        let mut wrong_caller = caller.clone();
        wrong_caller
            .ownership
            .checker_ordinary_map_owned_any_v2_map_root_caller_receipt
            .as_mut()
            .unwrap()
            .caller_owner = NodeId("function:other_caller".into());
        let error = catalog
            .validate_managed_any_cfg_flow(&wrong_caller, managed_values)
            .expect_err("a receipt owned by a different caller must fail closed");
        assert!(error.contains("Map-root caller"), "{error}");

        for mutation in [
            "callee",
            "result",
            "result_type",
            "binding",
            "type_argument",
            "argument",
            "effect",
        ] {
            let mut forged = caller.clone();
            if mutation == "binding" {
                let binding = forged
                    .blocks
                    .values_mut()
                    .flat_map(|block| &mut block.instructions)
                    .find(|instruction| instruction.id == bind_instruction_id)
                    .expect("caller root binding");
                let MirInstructionKind::Move { result, .. } = &mut binding.kind else {
                    unreachable!()
                };
                *result = call_result.clone();
                let error = catalog
                    .validate_managed_any_cfg_flow(&forged, managed_values)
                    .expect_err("the caller result binding must match the receipt");
                assert!(error.contains("Map-root caller"), "{mutation}: {error}");
                continue;
            }
            if mutation == "result_type" {
                forged
                    .values
                    .get_mut(&call_result)
                    .expect("receipted Call result")
                    .ty = crate::core::ir::primitive_type_id(PrimitiveType::I32);
                let error = catalog
                    .validate_managed_any_cfg_flow(&forged, managed_values)
                    .expect_err("the caller Call result type must match the root receipt");
                assert!(error.contains("Map-root caller"), "{mutation}: {error}");
                continue;
            }
            let call = forged
                .blocks
                .values_mut()
                .flat_map(|block| &mut block.instructions)
                .find(|instruction| instruction.id == call_instruction_id)
                .expect("exact producer Call");
            let MirInstructionKind::Call {
                callee,
                result: Some(result),
                type_arguments,
                arguments,
                effect_receipts,
                ..
            } = &mut call.kind
            else {
                unreachable!()
            };
            match mutation {
                "callee" => {
                    *callee = crate::core::ir::ResolvedCallee::Function(NodeId(
                        "function:forged_producer".into(),
                    ));
                }
                "result" => *result = caller_root.clone(),
                "type_argument" => {
                    type_arguments.push(crate::core::ir::primitive_type_id(PrimitiveType::I32));
                }
                "argument" => arguments.push(caller_root.clone()),
                "effect" => effect_receipts.push(super::super::types::MirCallEffectContract {
                    kind: super::super::types::MirCallEffectKind::TransferSession,
                    argument_index: 0,
                    argument_ty: super::super::types::ordinary_map_owned_any_v2_type_id(),
                }),
                _ => unreachable!(),
            }
            let error = catalog
                .validate_managed_any_cfg_flow(&forged, managed_values)
                .expect_err("an altered producer Call edge or result binding must fail closed");
            assert!(error.contains("Map-root caller"), "{mutation}: {error}");
        }

        let mut root_parameter = producer.clone();
        let root_parameter_id = MirValueId::new("param:forged_root").unwrap();
        root_parameter.values.insert(
            root_parameter_id.clone(),
            super::super::MirValue {
                id: root_parameter_id.clone(),
                ty: super::super::types::ordinary_map_owned_any_v2_type_id(),
            },
        );
        root_parameter.parameters.push(root_parameter_id);
        let producer_managed_values = &producer
            .ownership
            .checker_managed_any
            .as_ref()
            .unwrap()
            .managed_values;
        assert!(
            catalog
                .validate_managed_any_cfg_flow(&root_parameter, producer_managed_values)
                .is_err(),
            "producer root parameters are not admitted by the return bridge"
        );

        let mut generic_producer = producer.clone();
        generic_producer
            .ownership
            .checker_callable_generic_parameters
            .push(NodeId("generic:forged".into()));
        assert!(
            catalog
                .validate_managed_any_cfg_flow(&generic_producer, producer_managed_values)
                .is_err(),
            "generic producers are outside the exact zero-parameter profile"
        );

        let mut missing_pair = producer.clone();
        missing_pair
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_mut()
            .unwrap()
            .map_root_producer_caller_receipt = None;
        assert!(
            catalog
                .validate_managed_any_cfg_flow(&missing_pair, producer_managed_values)
                .is_err(),
            "producer root returns require the unique paired caller receipt"
        );
    }

    #[test]
    fn managed_any_map_root_refresh_rejects_calls_reusing_checker_origins() {
        let checked = checked(include_str!(
            "../../../tests/fixtures/mir_ordinary_map_owned_any_v2_map_root_producer_caller.mimi"
        ));
        let program =
            MirProgram::from_checked_program(&checked).expect("Map-root producer/caller bridge");
        let functions = program.functions();
        let producer = functions
            .get(&NodeId("function:make_root".into()))
            .expect("producer MIR");
        let caller = functions
            .get(&NodeId("function:direct_caller".into()))
            .expect("caller MIR");
        let catalog = program.type_catalog();
        let producer_seal = producer
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("producer private Checker seal");
        let producer_origins = producer_seal
            .instructions
            .iter()
            .map(|(_, origin, instruction)| (instruction.id.clone(), origin.clone()))
            .collect::<BTreeMap<_, _>>();

        let mut forged_producer = producer.clone();
        let payload_literal = forged_producer
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Const {
                        literal: ResolvedLiteral::String(value),
                        ..
                    } if value == "owned value"
                )
            })
            .expect("source payload literal");
        let payload_result = match &payload_literal.kind {
            MirInstructionKind::Const { result, .. } => result.clone(),
            _ => unreachable!(),
        };
        payload_literal.kind = MirInstructionKind::Call {
            result: Some(payload_result),
            callee: crate::core::ir::ResolvedCallee::Function(NodeId(
                "function:other_payload".into(),
            )),
            type_arguments: Vec::new(),
            arguments: Vec::new(),
            effect_receipts: Vec::new(),
            variant_call_contract: None,
        };
        assert!(
            super::refresh_checked_lowering(
                &forged_producer,
                catalog,
                &producer_origins,
                producer_seal,
            )
            .is_none(),
            "a helper Call cannot reuse the payload literal's original ID and Checker origin during refresh"
        );
        assert!(
            catalog
                .validate_managed_any_function(&forged_producer)
                .is_err(),
            "the original producer seal must reject a payload-initializer Call substitution"
        );
        let producer_managed_values = &producer_seal.managed_values;
        assert!(
            catalog
                .validate_managed_any_cfg_flow(&forged_producer, producer_managed_values)
                .is_err(),
            "the producer boundary gate must reject a payload-initializer Call substitution"
        );

        let caller_seal = caller
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("caller private Checker seal");
        let caller_receipt = caller
            .ownership
            .checker_ordinary_map_owned_any_v2_map_root_caller_receipt
            .as_ref()
            .expect("independent Map-root caller receipt");
        let mut forged_caller = caller.clone();
        let helper_result = MirValueId::new("expr:forged_scalar_helper_result").unwrap();
        let helper_call = MirInstructionId::new("inst:call:forged_scalar_helper").unwrap();
        let helper_origin = caller_receipt.map_size_node.clone();
        forged_caller.values.insert(
            helper_result.clone(),
            super::super::MirValue {
                id: helper_result.clone(),
                ty: crate::core::ir::primitive_type_id(PrimitiveType::I32),
            },
        );
        forged_caller
            .blocks
            .values_mut()
            .next()
            .expect("caller entry block")
            .instructions
            .push(MirInstruction {
                id: helper_call.clone(),
                kind: MirInstructionKind::Call {
                    result: Some(helper_result),
                    callee: crate::core::ir::ResolvedCallee::Function(NodeId(
                        "function:scalar_helper".into(),
                    )),
                    type_arguments: Vec::new(),
                    arguments: Vec::new(),
                    effect_receipts: Vec::new(),
                    variant_call_contract: None,
                },
            });
        let mut caller_origins = caller_seal
            .instructions
            .iter()
            .map(|(_, origin, instruction)| (instruction.id.clone(), origin.clone()))
            .collect::<BTreeMap<_, _>>();
        caller_origins.insert(helper_call, helper_origin);
        assert!(
            super::refresh_checked_lowering(&forged_caller, catalog, &caller_origins, caller_seal,)
                .is_none(),
            "a scalar helper Call cannot be admitted by reusing a Checker Map-size origin"
        );
        assert!(
            catalog
                .validate_managed_any_cfg_flow(&forged_caller, &caller_seal.managed_values)
                .expect_err("caller shape must reject every extra Call")
                .contains("Map-root caller"),
            "the caller profile census rejects a second, unreceipted Call"
        );
        assert!(
            catalog
                .validate_managed_any_function(&forged_caller)
                .is_err(),
            "the caller's original private seal also rejects an added helper Call"
        );
    }

    #[test]
    fn generic_any_identity_is_bound_to_checker_template_and_exact_owner_body() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let instance = program
            .instances()
            .values()
            .find(|instance| instance.contract == MirGenericInstanceContract::ManagedAnyIdentity)
            .expect("one sealed ManagedAny identity instance");
        let function = &program.functions()[&instance.function];
        let managed = program
            .type_catalog()
            .validate_managed_any_identity_source_argument(&instance.arguments)
            .expect("source Any maps to canonical ManagedAny");
        assert_eq!(instance.arguments.len(), 1);
        assert_ne!(instance.arguments[0], managed);
        assert!(program
            .type_catalog()
            .validate_managed_any_identity_source_argument(&[])
            .is_err());
        assert!(program
            .type_catalog()
            .validate_managed_any_identity_source_argument(std::slice::from_ref(&managed))
            .is_err());
        assert_eq!(function.result, managed);
        assert_eq!(function.values[&function.parameters[0]].ty, managed);
        assert!(matches!(
            function.blocks[&function.entry].instructions.as_slice(),
            [
                MirInstruction {
                    kind: MirInstructionKind::ManagedAny {
                        operation: MirManagedAnyOperation::Clone,
                        ..
                    },
                    ..
                },
                MirInstruction {
                    kind: MirInstructionKind::ManagedAny {
                        operation: MirManagedAnyOperation::Drop,
                        ..
                    },
                    ..
                }
            ]
        ));
        program
            .type_catalog()
            .validate_managed_any_identity_instance(instance, function)
            .expect("Checker seal binds template, source arg, owner, ABI and operations");
        program
            .validate_managed_any_identity_instances()
            .expect("every call and table entry matches");

        let mut forged_instance = instance.clone();
        forged_instance.template = NodeId("function:forged-template".into());
        assert!(program
            .type_catalog()
            .validate_managed_any_identity_instance(&forged_instance, function)
            .is_err());

        let mut forged = function.clone();
        let identity = forged
            .ownership
            .checker_managed_any
            .as_mut()
            .and_then(|seal| seal.generic_identity.as_mut())
            .expect("private generic identity seal");
        identity.drop_instruction = MirInstructionId::new("inst:forged-drop").unwrap();
        assert!(program
            .type_catalog()
            .validate_managed_any_identity_instance(instance, &forged)
            .is_err());

        let mut forged_source = function.clone();
        forged_source
            .ownership
            .checker_managed_any
            .as_mut()
            .and_then(|seal| seal.generic_identity.as_mut())
            .expect("private generic identity seal")
            .type_argument = managed.clone();
        assert!(program
            .type_catalog()
            .validate_managed_any_identity_instance(instance, &forged_source)
            .is_err());

        let mut missing_seal = function.clone();
        missing_seal.ownership.checker_managed_any = None;
        assert!(program
            .type_catalog()
            .validate_managed_any_identity_instance(instance, &missing_seal)
            .is_err());

        let mut reordered = function.clone();
        reordered
            .blocks
            .get_mut(&reordered.entry)
            .unwrap()
            .instructions
            .swap(0, 1);
        assert!(program
            .type_catalog()
            .validate_managed_any_identity_instance(instance, &reordered)
            .is_err());
    }

    #[test]
    fn recursive_any_detector_covers_every_aggregate_layout_and_cycles() {
        let checked = checked(SOURCE);
        let mut catalog = MirTypeCatalog::from_checked_program(&checked)
            .expect("checker-derived catalog with GetAny DynamicAny");
        let source_any = catalog
            .iter()
            .find_map(|(id, descriptor)| {
                (descriptor.kind == MirTypeKind::DynamicAny).then_some(id.clone())
            })
            .expect("one canonical source DynamicAny TypeDesc");
        let managed_any = managed_any_i32_string_v1_type_id();
        let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
        let install = |catalog: &mut MirTypeCatalog, name: &str, layout: MirLayout| {
            let id = ResolvedTypeId::synthetic(&format!("test:recursive-any:{name}"));
            catalog.replace_for_test_only(
                id.clone(),
                MirTypeDesc {
                    id: id.clone(),
                    kind: MirTypeKind::Nominal,
                    layout,
                    native_record_abi: None,
                    session_protocol: None,
                    ownership: MirOwnership::Copy,
                    abi: MirAbiClass::Aggregate,
                    needs_drop_glue: false,
                    needs_clone_glue: false,
                    glue: MirGlueContract {
                        move_out: MirGlueKind::Noop,
                        clone: MirGlueKind::Noop,
                        drop: MirGlueKind::Noop,
                    },
                    drop_plan: None,
                    variant_drop_plan: None,
                },
            );
            catalog.resolved_children.insert(id.clone(), Vec::new());
            id
        };
        let variant = |name: &str, ty: &ResolvedTypeId| MirVariantDesc {
            id: NodeId(format!("test:variant:{name}")),
            name: name.into(),
            discriminant: 0,
            fields: vec![MirFieldDesc {
                id: NodeId(format!("test:field:{name}")),
                name: "payload".into(),
                ty: ty.clone(),
            }],
        };

        let layouts = [
            ("tuple", MirLayout::Tuple(vec![source_any.clone()])),
            (
                "option-inner",
                MirLayout::Option {
                    inner: managed_any.clone(),
                    variants: vec![],
                },
            ),
            (
                "option-variant",
                MirLayout::Option {
                    inner: i32_type.clone(),
                    variants: vec![variant("option", &source_any)],
                },
            ),
            (
                "result",
                MirLayout::Result {
                    ok: i32_type.clone(),
                    error: source_any.clone(),
                    variants: vec![],
                },
            ),
            (
                "result-variant",
                MirLayout::Result {
                    ok: i32_type.clone(),
                    error: i32_type.clone(),
                    variants: vec![variant("result", &managed_any)],
                },
            ),
            (
                "enum",
                MirLayout::Enum {
                    nominal: crate::core::NominalTypeId::new("test:Enum").unwrap(),
                    variants: vec![variant("enum", &managed_any)],
                },
            ),
            (
                "array",
                MirLayout::Array {
                    element: source_any.clone(),
                    length: 2,
                },
            ),
            (
                "newtype",
                MirLayout::Newtype {
                    nominal: crate::core::NominalTypeId::new("test:Newtype").unwrap(),
                    inner: managed_any.clone(),
                },
            ),
            (
                "pointer",
                MirLayout::Pointer {
                    target: Some(source_any.clone()),
                },
            ),
            (
                "list",
                MirLayout::List {
                    element: managed_any.clone(),
                },
            ),
            (
                "set",
                MirLayout::Set {
                    element: source_any.clone(),
                },
            ),
            (
                "record",
                MirLayout::Record {
                    nominal: crate::core::NominalTypeId::new("test:Record").unwrap(),
                    fields: vec![variant("record", &managed_any).fields[0].clone()],
                },
            ),
            (
                "actor-state",
                MirLayout::ActorState {
                    actor: NodeId("test:Actor".into()),
                    fields: vec![variant("actor-state", &source_any).fields[0].clone()],
                },
            ),
        ];
        for (name, layout) in layouts {
            let wrapper = install(&mut catalog, name, layout);
            assert!(
                catalog.contains_dynamic_or_managed_any(&wrapper),
                "layout {name} must recursively detect Any"
            );
        }

        let scalar = install(&mut catalog, "scalar", MirLayout::Scalar);
        assert!(!catalog.contains_dynamic_or_managed_any(&scalar));
        for (name, layout) in [
            ("unit", MirLayout::Unit),
            ("handle", MirLayout::Handle),
            ("opaque", MirLayout::Opaque),
            ("map-root", MirLayout::MapRoot),
            ("map-any-owner", MirLayout::MapAnyOwnerV1),
            (
                "ordinary-map-owner",
                MirLayout::OrdinaryMapOwnerLifecycleI32StringV1,
            ),
            (
                "ordinary-map-get",
                MirLayout::OrdinaryMapOwnedGetI32StringV1,
            ),
        ] {
            let wrapper = install(&mut catalog, name, layout);
            assert!(
                !catalog.contains_dynamic_or_managed_any(&wrapper),
                "layout {name} with no Any descendants must be classified false"
            );
        }

        let known_empty_handle = install(&mut catalog, "known-empty-handle", MirLayout::Handle);
        assert!(
            !catalog.contains_dynamic_or_managed_any(&known_empty_handle),
            "a Handle with an explicit empty Checker child-edge row is known not to hide Any"
        );
        let mut missing_handle_edges = catalog.clone();
        missing_handle_edges
            .resolved_children
            .remove(&known_empty_handle);
        assert!(
            missing_handle_edges.contains_dynamic_or_managed_any(&known_empty_handle),
            "a Handle without its required source-child metadata is unclassifiable and fails closed"
        );

        let unknown_pointer = install(
            &mut catalog,
            "pointer-no-target",
            MirLayout::Pointer { target: None },
        );
        assert!(catalog.contains_dynamic_or_managed_any(&unknown_pointer));

        let protocol_only = install(&mut catalog, "session-protocol-edge", MirLayout::Handle);
        let mut protocol_descriptor = catalog.get(&protocol_only).unwrap().clone();
        protocol_descriptor.session_protocol = Some(source_any.clone());
        catalog.replace_for_test_only(protocol_only.clone(), protocol_descriptor);
        assert!(catalog.contains_dynamic_or_managed_any(&protocol_only));

        let drop_plan_only = install(&mut catalog, "drop-plan-edge", MirLayout::Handle);
        let mut drop_plan_descriptor = catalog.get(&drop_plan_only).unwrap().clone();
        drop_plan_descriptor.drop_plan = Some(crate::core::mir::types::MirDropGluePlan {
            fields: vec![crate::core::mir::types::MirDropGlueField {
                index: 0,
                ty: source_any.clone(),
                glue: MirGlueKind::Unsupported,
            }],
        });
        catalog.replace_for_test_only(drop_plan_only.clone(), drop_plan_descriptor);
        assert!(catalog.contains_dynamic_or_managed_any(&drop_plan_only));

        let variant_drop_plan_only =
            install(&mut catalog, "variant-drop-plan-edge", MirLayout::Handle);
        let mut variant_drop_plan_descriptor =
            catalog.get(&variant_drop_plan_only).unwrap().clone();
        variant_drop_plan_descriptor.variant_drop_plan =
            Some(vec![crate::core::mir::types::MirVariantDropGluePlan {
                variant: NodeId("test:variant-drop-edge".into()),
                fields: vec![crate::core::mir::types::MirDropGlueField {
                    index: 0,
                    ty: source_any.clone(),
                    glue: MirGlueKind::Unsupported,
                }],
            }]);
        catalog.replace_for_test_only(variant_drop_plan_only.clone(), variant_drop_plan_descriptor);
        assert!(catalog.contains_dynamic_or_managed_any(&variant_drop_plan_only));

        let cycle_a = ResolvedTypeId::synthetic("test:recursive-any:cycle-a");
        let cycle_b = ResolvedTypeId::synthetic("test:recursive-any:cycle-b");
        let cyclic_desc = |id: ResolvedTypeId, layout: MirLayout| MirTypeDesc {
            id,
            kind: MirTypeKind::Nominal,
            layout,
            native_record_abi: None,
            session_protocol: None,
            ownership: MirOwnership::Copy,
            abi: MirAbiClass::Aggregate,
            needs_drop_glue: false,
            needs_clone_glue: false,
            glue: MirGlueContract {
                move_out: MirGlueKind::Noop,
                clone: MirGlueKind::Noop,
                drop: MirGlueKind::Noop,
            },
            drop_plan: None,
            variant_drop_plan: None,
        };
        catalog.replace_for_test_only(
            cycle_a.clone(),
            cyclic_desc(
                cycle_a.clone(),
                MirLayout::Newtype {
                    nominal: crate::core::NominalTypeId::new("test:CycleA").unwrap(),
                    inner: cycle_b.clone(),
                },
            ),
        );
        catalog.replace_for_test_only(
            cycle_b.clone(),
            cyclic_desc(cycle_b.clone(), MirLayout::Tuple(vec![cycle_a.clone()])),
        );
        assert!(!catalog.contains_dynamic_or_managed_any(&cycle_a));
        catalog.replace_for_test_only(
            cycle_b.clone(),
            cyclic_desc(cycle_b, MirLayout::Tuple(vec![cycle_a.clone(), source_any])),
        );
        assert!(catalog.contains_dynamic_or_managed_any(&cycle_a));
    }

    #[test]
    fn recursive_any_detector_follows_erased_resolved_type_edges() {
        let checked = checked(SOURCE);
        let mut resolved = checked.resolved_types().clone();
        let source_any = resolved
            .iter()
            .find_map(|(id, ty)| {
                matches!(ty, ResolvedType::DynamicAny { .. }).then_some(id.clone())
            })
            .expect("Checker DynamicAny type");
        let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
        let unit_type = resolved
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::Unit))
            .expect("unit type");
        let function_parameter_any = resolved
            .intern_resolved(ResolvedType::Function {
                abi: crate::core::ir::FunctionTypeAbi::Mimi,
                parameters: vec![source_any.clone()],
                result: i32_type.clone(),
            })
            .expect("function parameter with Any");
        let function_result_any = resolved
            .intern_resolved(ResolvedType::Function {
                abi: crate::core::ir::FunctionTypeAbi::Mimi,
                parameters: vec![i32_type.clone()],
                result: source_any.clone(),
            })
            .expect("function result with Any");
        let ownership_target_any = resolved
            .intern_resolved(ResolvedType::Ownership {
                kind: crate::core::ir::OwnershipTypeKind::Shared,
                target: source_any.clone(),
            })
            .expect("Ownership<Any>");
        let weak_ownership_target_any = resolved
            .intern_resolved(ResolvedType::Ownership {
                kind: crate::core::ir::OwnershipTypeKind::Weak,
                target: source_any.clone(),
            })
            .expect("Weak<Any>");
        let opaque_nominal_any = resolved
            .intern_resolved(ResolvedType::Nominal {
                item: crate::core::NominalTypeId::new("test:OpaqueBox").unwrap(),
                arguments: vec![source_any.clone()],
                is_linear: false,
            })
            .expect("opaque nominal with Any argument");
        let function_without_any = resolved
            .intern_resolved(ResolvedType::Function {
                abi: crate::core::ir::FunctionTypeAbi::Mimi,
                parameters: vec![i32_type.clone()],
                result: unit_type,
            })
            .expect("function without Any");
        let ownership_without_any = resolved
            .intern_resolved(ResolvedType::Ownership {
                kind: crate::core::ir::OwnershipTypeKind::Shared,
                target: i32_type.clone(),
            })
            .expect("Ownership<i32>");
        let opaque_nominal_without_any = resolved
            .intern_resolved(ResolvedType::Nominal {
                item: crate::core::NominalTypeId::new("test:OpaquePlain").unwrap(),
                arguments: vec![i32_type.clone()],
                is_linear: false,
            })
            .expect("opaque nominal without Any");
        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
        let function_any_tuple = resolved
            .intern_resolved(ResolvedType::Tuple(vec![
                string_type.clone(),
                function_result_any.clone(),
            ]))
            .expect("tuple with owned String and function metadata containing Any");
        let reversed_function_any_tuple = resolved
            .intern_resolved(ResolvedType::Tuple(vec![
                function_parameter_any.clone(),
                string_type.clone(),
            ]))
            .expect("tuple with function metadata containing Any and owned String");
        let catalog = MirTypeCatalog::from_resolved_types(&resolved).expect("catalog");

        assert_eq!(
            catalog.resolved_type_children(&function_parameter_any),
            Some(&[source_any.clone(), i32_type.clone()][..]),
            "callers can serialize the exact canonical function parameter and result child IDs"
        );
        assert_eq!(
            catalog.resolved_type_children(&function_result_any),
            Some(&[i32_type.clone(), source_any.clone()][..]),
            "function result child identity is included after all parameter IDs"
        );
        assert_eq!(
            catalog.resolved_type_children(&ownership_target_any),
            Some(&[source_any.clone()][..])
        );
        assert_eq!(
            catalog.resolved_type_children(&weak_ownership_target_any),
            Some(&[source_any.clone()][..])
        );
        assert_eq!(
            catalog.resolved_type_children(&opaque_nominal_any),
            Some(&[source_any.clone()][..])
        );

        for ty in [
            function_parameter_any.clone(),
            function_result_any.clone(),
            ownership_target_any.clone(),
            weak_ownership_target_any.clone(),
            opaque_nominal_any.clone(),
        ] {
            assert!(
                catalog.contains_dynamic_or_managed_any(&ty),
                "hidden resolved edge in '{}' must be traversed",
                ty.as_str()
            );
        }
        for ty in [
            function_without_any.clone(),
            ownership_without_any.clone(),
            opaque_nominal_without_any.clone(),
        ] {
            assert!(
                !catalog.contains_dynamic_or_managed_any(&ty),
                "ordinary opaque handle '{}' must not be treated as Any",
                ty.as_str()
            );
        }
        let projection_error = catalog
            .validate_move_projection(
                &function_any_tuple,
                &string_type,
                &crate::core::mir::MirProjection::Tuple(0),
            )
            .expect_err(
                "a Copy function handle with recursive Any metadata is not a discardable residual",
            );
        assert!(
            projection_error.contains("tuple leaves non-Copy sibling"),
            "{projection_error}"
        );
        let reversed_projection_error = catalog
            .validate_move_projection(
                &reversed_function_any_tuple,
                &string_type,
                &crate::core::mir::MirProjection::Tuple(1),
            )
            .expect_err(
                "a reversed tuple cannot discard a Copy function handle whose signature contains Any",
            );
        assert!(
            reversed_projection_error.contains("tuple leaves non-Copy sibling"),
            "{reversed_projection_error}"
        );

        let mut absent_child_catalog = catalog.clone();
        let absent_child = ResolvedTypeId::synthetic("test:recursive-any:missing-child");
        absent_child_catalog
            .resolved_children
            .insert(i32_type, vec![absent_child]);
        assert!(absent_child_catalog.contains_dynamic_or_managed_any(
            &crate::core::ir::primitive_type_id(PrimitiveType::I32)
        ));

        let mut missing_function_metadata = catalog.clone();
        missing_function_metadata
            .resolved_children
            .remove(&function_without_any);
        assert!(missing_function_metadata.contains_dynamic_or_managed_any(&function_without_any));
        let mut missing_ownership_metadata = catalog.clone();
        missing_ownership_metadata
            .resolved_children
            .remove(&ownership_without_any);
        assert!(missing_ownership_metadata.contains_dynamic_or_managed_any(&ownership_without_any));
        let mut missing_opaque_nominal_metadata = catalog.clone();
        missing_opaque_nominal_metadata
            .resolved_children
            .remove(&opaque_nominal_without_any);
        assert!(missing_opaque_nominal_metadata
            .contains_dynamic_or_managed_any(&opaque_nominal_without_any));
    }

    #[test]
    fn same_type_desc_does_not_authorize_forged_or_missing_owner_receipts() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let original = program
            .functions()
            .values()
            .find(|function| function.ownership.checker_managed_any.is_some())
            .expect("Any function");
        let mut missing = original.clone();
        missing.ownership.checker_managed_any = None;
        assert!(program
            .type_catalog()
            .validate_managed_any_function(&missing)
            .is_err());
        let mut substituted = original.clone();
        let id = substituted
            .values
            .values()
            .find(|value| program.type_catalog().contains_managed_any(&value.ty))
            .unwrap()
            .id
            .clone();
        let same_type = substituted.values[&id].ty.clone();
        let forged = MirValueId::new("forged:same-managed-descriptor").unwrap();
        substituted.values.insert(
            forged.clone(),
            super::super::MirValue {
                id: forged,
                ty: same_type,
            },
        );
        assert!(program
            .type_catalog()
            .validate_managed_any_function(&substituted)
            .is_err());
    }
    #[test]
    fn managed_any_receipt_rejects_wrong_owner_kind_version_and_origin_before_consumption() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let original = program
            .functions()
            .values()
            .find(|function| {
                function.blocks.values().any(|block| {
                    block.instructions.iter().any(|instruction| {
                        matches!(
                            instruction.kind,
                            MirInstructionKind::ManagedAny {
                                operation: MirManagedAnyOperation::PackI32,
                                ..
                            }
                        )
                    })
                })
            })
            .expect("PackI32 function");
        for field in 0..4 {
            let mut forged = original.clone();
            let instruction = forged
                .blocks
                .values_mut()
                .flat_map(|block| &mut block.instructions)
                .find(|instruction| {
                    matches!(
                        instruction.kind,
                        MirInstructionKind::ManagedAny {
                            operation: MirManagedAnyOperation::PackI32,
                            ..
                        }
                    )
                })
                .unwrap();
            let MirInstructionKind::ManagedAny {
                contract: Some(receipt),
                ..
            } = &mut instruction.kind
            else {
                unreachable!()
            };
            match field {
                0 => receipt.owner = NodeId("function:forged".into()),
                1 => receipt.expected_kind = Some(2),
                2 => receipt.descriptor_version = 2,
                _ => receipt.origin = NodeId("node:forged".into()),
            }
            assert!(forged.validate().is_err());
            assert!(program
                .type_catalog()
                .validate_managed_any_function(&forged)
                .is_err());
        }
    }
    #[test]
    fn aggregate_any_owner_cannot_be_raw_copied_or_reorder_a_drop() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let original = program
            .functions()
            .values()
            .find(|function| {
                function.blocks.values().any(|block| {
                    block.instructions.iter().any(|instruction| {
                        matches!(
                            instruction.kind,
                            MirInstructionKind::ManagedAny {
                                operation: MirManagedAnyOperation::Clone,
                                ..
                            }
                        )
                    })
                })
            })
            .unwrap();
        let mut forged = original.clone();
        let instruction = forged
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    instruction.kind,
                    MirInstructionKind::ManagedAny {
                        operation: MirManagedAnyOperation::Clone,
                        ..
                    }
                )
            })
            .unwrap();
        let MirInstructionKind::ManagedAny {
            source,
            result: Some(result),
            ..
        } = &instruction.kind
        else {
            unreachable!()
        };
        instruction.kind = MirInstructionKind::Copy {
            source: source.clone(),
            result: result.clone(),
        };
        assert!(program
            .type_catalog()
            .validate_managed_any_function(&forged)
            .is_err());
        let clone_block_id = original
            .blocks
            .iter()
            .find(|(_, block)| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        MirInstructionKind::ManagedAny {
                            operation: MirManagedAnyOperation::Clone,
                            ..
                        }
                    )
                })
            })
            .map(|(id, _)| id.clone())
            .expect("the selected owner body contains its Clone");
        let mut reordered = original.clone();
        // The canonical ManagedAny identity specialization may return its
        // cloned owner directly, so its matching Drop can live in the caller
        // rather than this one-block helper. Reorder the sealed Clone itself
        // when a block has more than one instruction; the private body seal
        // must reject that mutation just as it rejects a reordered Drop.
        let block = reordered
            .blocks
            .get_mut(&clone_block_id)
            .expect("the selected owner body contains its clone block");
        let clone_index = block
            .instructions
            .iter()
            .position(|instruction| {
                matches!(
                    instruction.kind,
                    MirInstructionKind::ManagedAny {
                        operation: MirManagedAnyOperation::Clone,
                        ..
                    }
                )
            })
            .expect("the selected owner body contains its canonical Clone");
        if block.instructions.len() > 1 {
            let last_index = block.instructions.len() - 1;
            if clone_index == last_index {
                block.instructions.swap(clone_index, 0);
            } else {
                block.instructions.swap(clone_index, last_index);
            }
        } else {
            // A one-instruction helper has no legal in-block reorder. The
            // forged Copy above already demonstrates that raw duplication is
            // rejected; mutate the instruction identity so the sealed body is
            // still proven to reject a reordered/rewritten owner event.
            block.instructions[clone_index].id =
                MirInstructionId::new("forged:reordered-clone").expect("forged instruction id");
        }
        assert!(program
            .type_catalog()
            .validate_managed_any_function(&reordered)
            .is_err());
    }
    #[test]
    fn owned_any_parameter_receipt_cannot_be_rewritten_as_a_loan() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let managed_type = managed_any_i32_string_v1_type_id();
        // `Any` is an internal DynamicAny source shape.  The user-facing
        // fixture reaches it through the generic `carry<T>` identity, whose
        // canonical ManagedAny specialization is the only function with an
        // owned ManagedAny parameter.  The old test looked up a removed
        // `relay(Any)` declaration and panicked before exercising the seal.
        let original = program
            .functions()
            .values()
            .find(|function| {
                function.parameters.len() == 1
                    && function
                        .values
                        .get(&function.parameters[0])
                        .is_some_and(|value| value.ty == managed_type)
                    && function
                        .ownership
                        .checker_managed_any
                        .as_ref()
                        .is_some_and(|seal| seal.generic_identity.is_some())
            })
            .expect("the fixture has one sealed generic ManagedAny identity specialization");
        for permission in [
            crate::core::ir::Permission::View,
            crate::core::ir::Permission::Mutate,
        ] {
            let mut forged = original.clone();
            forged.parameter_permissions = Some(vec![Some(permission)]);
            assert!(program
                .type_catalog()
                .validate_managed_any_function(&forged)
                .is_err());
        }
        let mut missing = original.clone();
        missing.parameter_permissions = None;
        assert!(program
            .type_catalog()
            .validate_managed_any_function(&missing)
            .is_err());
    }

    #[test]
    fn view_and_mutate_any_parameters_stay_outside_owned_snapshot_profile() {
        for permission in ["view", "mutate"] {
            let source = format!(
                r#"
func inspect(value: {permission} Any) -> i32 {{ let number: i32 = value; number }}
func main() -> i32 {{
    match map_new() {{
        Ok(root) => {{
            match map_set(root, "n", 7) {{
                Ok(inserted) => {{
                    let (found, value) = map_get(inserted, "n")
                    let number = inspect(value)
                    let retained: i32 = value
                    drop(value)
                    drop(root)
                    drop(inserted)
                    number + retained
                }},
                Err(status) => {{
                    drop(root)
                    status
                }},
            }}
        }},
        Err(status) => {{ status }},
    }}
}}
"#
            );
            let tokens = crate::lexer::Lexer::new(&source).tokenize().unwrap();
            let file = crate::parser::Parser::new(tokens).parse_file().unwrap();
            if let Ok(checked) = crate::core::check_program(&file) {
                assert!(
                    MirProgram::from_checked_program(&checked).is_err(),
                    "borrowed Any needs a loan receipt before canonical owner cleanup can run"
                );
            }
        }
    }

    #[test]
    fn managed_any_ingress_does_not_erase_a_strict_linear_payload() {
        let source = r#"func main() -> i32 {
            match map_new() {
                Ok(root) => {
                    match map_set(root, "token", make_token()) {
                        Ok(inserted) => {
                            drop(root)
                            drop(inserted)
                            0
                        },
                        Err(status) => {
                            drop(root)
                            status
                        },
                    }
                },
                Err(status) => { status },
            }
        }"#;
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let file = crate::parser::Parser::new(tokens).parse_file().unwrap();
        let errors = crate::core::check_program(&file)
            .expect_err("linear payload cannot enter ordinary Any");
        assert!(errors
            .iter()
            .any(|error| error.code.as_deref() == Some("E0432")));
    }
}
