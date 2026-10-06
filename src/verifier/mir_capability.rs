//! Static capability gate for the verifier's Canonical MIR consumer.
//!
//! `verify_mir` is intentionally a contract engine: functions without an
//! obligation are not symbolically executed.  That is useful for keeping the
//! proof engine small, but it must not be mistaken for whole-program
//! capability.  The default route therefore runs this structural gate over
//! every function before admitting an island.  The gate consumes only the
//! canonical MIR program and its TypeDesc catalog; it never asks the frontend
//! to rediscover a type, ownership, or call fact.

use std::collections::{BTreeSet, HashSet};

use crate::core::ir::{
    ResolvedBinaryOp, ResolvedCallee, ResolvedLiteral, ResolvedTypeId, ResolvedUnaryOp,
};
use crate::core::mir::reference::MirProgram;
use crate::core::mir::types::{
    MirAbiClass, MirGlueKind, MirGlueOperation, MirLayout, MirOwnership, MirTypeCatalog,
    MirTypeKind,
};
use crate::core::mir::{
    MirContractKind, MirFunction, MirGenericInstanceContract, MirInstance, MirInstructionKind,
    MirManagedAnyOperation, MirProjection, MirProofLoopEdgeKind, MirSwitchCase, MirTerminator,
    MirValueId,
};
use crate::verifier::checked_power::{CheckedPowerProfile, POWER_PROFILE_0_1_11_COMPAT};

/// Validate the subset of canonical MIR that the MIR verifier can safely
/// coexist with on the default route.
///
/// This is deliberately a capability result rather than a verification
/// result.  It answers whether every compiled MIR shape has a verifier
/// interpretation.  A function with no contracts is still scanned; otherwise
/// an unsupported body could silently enter a native/bytecode island merely
/// because the verifier had no obligation to print.
pub fn validate_mir_capabilities(program: &MirProgram) -> Result<(), Vec<String>> {
    // The source census and canonical function shape are mandatory inputs to
    // capability decisions. Run these before the Actor fast path, DFS cycle
    // cuts, solver facts, or any per-function capability effects.
    program.validate_proof_source_census().map_err(|error| {
        vec![format!(
            "canonical MIR proof source census rejected: {error:?}"
        )]
    })?;
    let mut structural_errors = Vec::new();
    for (owner, function) in program.functions() {
        if let Err(errors) = function.validate() {
            structural_errors.extend(errors.into_iter().map(|error| {
                format!(
                    "canonical MIR structural preflight rejected for '{}': {error}",
                    owner.0
                )
            }));
        }
    }
    if !structural_errors.is_empty() {
        return Err(structural_errors);
    }
    for (owner, function) in program.functions() {
        let Some(worklist) = function.proof_worklist.as_ref() else {
            continue;
        };
        if worklist_has_mutable_loop(worklist) {
            // A mutable-loop certificate authorizes CFG cycles. Recompute its
            // owner coverage, typed receipt, and exact MIR edge correspondence
            // from the independent source census before the DFS can cut them.
            program
                .validate_owner_proof_worklist(owner)
                .map_err(|error| {
                    vec![format!("canonical MIR loop preflight rejected: {error:?}")]
                })?;
        }
    }
    // ClosureBind is metadata only; ClosureInvoke is admitted only by the
    // checked source seal and direct-call receipt.
    program.validate_closure_callable_receipts()?;
    program.validate_actor_contracts()?;
    program.validate_managed_any_trap_cleanup_instances()?;
    if program.actor_execution_plan().is_some() {
        // The finite Actor receipt validates executable structure and lifetime
        // boundaries. It does not introduce a symbolic mailbox effect model.
        if program
            .functions()
            .values()
            .any(|function| !function.contracts.is_empty())
        {
            return Err(vec![
                "canonical Actor mailbox effects have no trusted contract proof model".into(),
            ]);
        }
        // The Actor fast path does not enter the ordinary per-function gate,
        // so run the recursive Any boundary preflight before returning its
        // neutral capability result. Keep non-Actor diagnostic aggregation
        // in CapabilityGate below unchanged.
        program.validate_managed_any_identity_instances()?;
        return Ok(());
    }
    let mut gate = CapabilityGate::new(program);
    if let Err(errors) = program.validate_managed_any_identity_instances() {
        for message in errors {
            gate.error(message);
        }
    }
    for message in
        crate::core::mir::validate_ffi_receipt_table(program.functions(), program.ffi_calls())
    {
        gate.error(message);
    }
    for message in crate::core::mir::validate_ffi_symbol_declaration_shapes(program.ffi_calls()) {
        gate.error(message);
    }
    gate.validate();
    if gate.errors.is_empty() {
        Ok(())
    } else {
        Err(gate.errors.into_iter().collect())
    }
}

fn worklist_has_mutable_loop(worklist: &crate::core::mir::MirProofWorklist) -> bool {
    fn block_has_mutable_loop(block: &crate::core::mir::MirProofBlock) -> bool {
        block.operations.iter().any(|operation| match operation {
            crate::core::mir::MirProofOperation::Loop {
                stationary: false, ..
            } => true,
            crate::core::mir::MirProofOperation::Branch {
                then_block,
                else_block,
                ..
            } => block_has_mutable_loop(then_block) || block_has_mutable_loop(else_block),
            crate::core::mir::MirProofOperation::Scope(inner) => block_has_mutable_loop(inner),
            crate::core::mir::MirProofOperation::Loop { body, .. } => block_has_mutable_loop(body),
            crate::core::mir::MirProofOperation::Bind { .. }
            | crate::core::mir::MirProofOperation::Call(_)
            | crate::core::mir::MirProofOperation::Eval { .. }
            | crate::core::mir::MirProofOperation::Assign { .. }
            | crate::core::mir::MirProofOperation::Math(_)
            | crate::core::mir::MirProofOperation::Break { .. }
            | crate::core::mir::MirProofOperation::Continue { .. }
            | crate::core::mir::MirProofOperation::Return { .. }
            | crate::core::mir::MirProofOperation::Exit => false,
        })
    }
    block_has_mutable_loop(&worklist.root)
}

struct CapabilityGate<'a> {
    program: &'a MirProgram,
    errors: BTreeSet<String>,
    checked_types: HashSet<crate::core::ResolvedTypeId>,
    /// A move-owned Result constructor is admitted only when the same
    /// canonical graph contains the promoted generic Result projection
    /// receipt. This keeps the generic managed-payload extension local: an
    /// unrelated direct Result helper cannot silently widen the older mixed
    /// record/variant route before its own whole-program consumer matrix is
    /// closed.
    allow_result_move_variant: bool,
    allow_recoverable_flow_result: bool,
    /// A non-Copy tuple payload is admitted only when this canonical graph
    /// contains the explicit nested tuple projection receipt.  This keeps a
    /// bare `Option<(...)>` constructor/drop body outside the profile.
    allow_nested_option_tuple_variant: bool,
    /// Exact Result<string, i32> identities created by this profile's typed
    /// String getter. This managed Result shape is admitted only for these
    /// checked result values.
    ordinary_map_owned_get_result_string_types: HashSet<crate::core::ResolvedTypeId>,
}

impl<'a> CapabilityGate<'a> {
    fn new(program: &'a MirProgram) -> Self {
        let has_managed_result_call = program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    let (result, has_move_receipt) = match &instruction.kind {
                        MirInstructionKind::Call {
                            result: Some(result),
                            variant_call_contract: Some(receipt),
                            ..
                        } => (
                            result,
                            receipt.mode
                                == crate::core::mir::types::MirVariantCallAbiMode::MoveOwned,
                        ),
                        MirInstructionKind::ClosureInvoke { .. } => return false,
                        _ => return false,
                    };
                    has_move_receipt
                        && function.values.get(result).is_some_and(|value| {
                            program
                                .type_catalog()
                                .get(&value.ty)
                                .is_some_and(|descriptor| descriptor.kind == MirTypeKind::Result)
                        })
                })
            })
        });
        Self {
            program,
            errors: BTreeSet::new(),
            checked_types: HashSet::new(),
            allow_result_move_variant: program.instances().values().any(|instance| match &instance
                .contract
            {
                MirGenericInstanceContract::ScalarVariantProjection { contract } => {
                    contract.projection.nominal.as_str() == "builtin:type:Result"
                        && contract.projection.ownership == MirOwnership::Move
                        && matches!(
                            contract.projection.move_out_glue,
                            MirGlueKind::OwnedString | MirGlueKind::List
                        )
                }
                MirGenericInstanceContract::ScalarVariantProjectionFallback { contract } => {
                    contract.projection.nominal.as_str() == "builtin:type:Result"
                        && contract.projection.ownership == MirOwnership::Move
                        && matches!(
                            contract.projection.move_out_glue,
                            MirGlueKind::OwnedString | MirGlueKind::List
                        )
                }
                _ => false,
            }) || has_managed_result_call,
            allow_recoverable_flow_result: program
                .transitions()
                .values()
                .any(|transition| transition.effect.is_recoverable()),
            allow_nested_option_tuple_variant: program.functions().values().any(|function| {
                function.blocks.values().any(|block| {
                    matches!(
                        &block.terminator,
                        MirTerminator::SwitchMove { arms, .. }
                            if arms.iter().any(|arm| arm.bindings.iter().any(|binding| binding.nested_tuple.is_some()))
                    )
                })
            }),
            ordinary_map_owned_get_result_string_types: program
                .functions()
                .values()
                .flat_map(|function| {
                    function.blocks.values().flat_map(move |block| {
                        block.instructions.iter().filter_map(move |instruction| {
                            let MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetString {
                                result,
                                ..
                            } = &instruction.kind
                            else {
                                return None;
                            };
                            function.values.get(result).map(|value| value.ty.clone())
                        })
                    })
                })
                .collect(),
        }
    }

    fn validate(&mut self) {
        let has_map_root_type = self
            .program
            .type_catalog()
            .iter()
            .any(|(_, descriptor)| descriptor.kind == MirTypeKind::MapRoot);
        let has_map_root_receipt = self
            .program
            .functions()
            .values()
            .any(|function| function.ownership.has_checker_map_root_actions());
        if has_map_root_type != has_map_root_receipt {
            self.error(
                "MapRoot TypeDesc presence must match Checker-owned MapRoot action receipts".into(),
            );
        }
        let map_any_owner_type = crate::core::mir::types::map_any_owner_v1_type_id();
        let has_map_any_owner_type = self.program.type_catalog().iter().any(|(id, descriptor)| {
            id == &map_any_owner_type
                || descriptor.kind == MirTypeKind::MapAnyOwnerV1
                || descriptor.layout == MirLayout::MapAnyOwnerV1
        });
        let has_map_any_owner_receipt = self
            .program
            .functions()
            .values()
            .any(|function| function.ownership.has_checker_map_any_owner_actions());
        if has_map_any_owner_type != has_map_any_owner_receipt {
            self.error(
                "MapAnyOwner v1 TypeDesc presence must match independent Checker owner receipts"
                    .into(),
            );
        }
        if has_map_any_owner_type {
            self.validate_map_any_owner_descriptor();
        }
        let has_managed_any_type = self.program.type_catalog().iter().any(|(_, descriptor)| {
            descriptor.kind == MirTypeKind::ManagedAnyI32StringV1
                || descriptor.layout == MirLayout::ManagedAnyI32StringV1
        });
        // Both the tuple-valued `map_get` and the owned `values()` snapshot
        // materialize the same checker-authorized ManagedAny element
        // descriptor.  Keep the capability census aligned with the TypeDesc
        // materializer: a values-only callable still has a legitimate
        // ManagedAny producer even though it has no GetAny tuple instruction.
        let has_get_any = self.program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { .. }
                            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { .. }
                    )
                })
            })
        });
        let has_checked_i32_pack = self.program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        MirInstructionKind::ManagedAny {
                            operation: MirManagedAnyOperation::PackI32,
                            contract: Some(_),
                            ..
                        }
                    )
                })
            })
        });
        if has_managed_any_type != (has_get_any || has_checked_i32_pack) {
            self.error("ManagedAny TypeDesc presence must match a separately Checker-receipted GetAny/Values or exact i32-pack producer".into());
        }
        let ordinary_map_owner_ty =
            crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id();
        let has_ordinary_map_owner_type = self.program.type_catalog().iter().any(|(id, desc)| {
            id == &ordinary_map_owner_ty
                || desc.kind == MirTypeKind::OrdinaryMapOwnerLifecycleI32StringV1
                || desc.layout == MirLayout::OrdinaryMapOwnerLifecycleI32StringV1
        });
        let has_ordinary_map_owner_receipt = self.program.functions().values().any(|function| {
            function
                .ownership
                .has_checker_ordinary_map_owner_lifecycle_v1_actions()
        });
        if has_ordinary_map_owner_type != has_ordinary_map_owner_receipt {
            self.error(
                "ordinary Map owner lifecycle v1 TypeDesc presence must match its independent Checker receipt".into(),
            );
        }
        if has_ordinary_map_owner_type {
            self.validate_ordinary_map_owner_descriptor();
        }
        let ordinary_map_owned_get_ty =
            crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id();
        let has_ordinary_map_owned_get_type =
            self.program.type_catalog().iter().any(|(id, descriptor)| {
                id == &ordinary_map_owned_get_ty
                    || descriptor.kind == MirTypeKind::OrdinaryMapOwnedGetI32StringV1
                    || descriptor.layout == MirLayout::OrdinaryMapOwnedGetI32StringV1
            });
        let has_ordinary_map_owned_get_receipt =
            self.program.functions().values().any(|function| {
                function
                    .ownership
                    .has_checker_ordinary_map_owned_get_i32_string_v1_actions()
            });
        if has_ordinary_map_owned_get_type != has_ordinary_map_owned_get_receipt {
            self.error(
                "ordinary Map owned-get v1 TypeDesc presence must match its independent Checker receipt".into(),
            );
        }
        if has_ordinary_map_owned_get_type {
            self.validate_ordinary_map_owned_get_descriptor();
        }
        if (has_map_root_type && has_map_any_owner_type)
            || (has_map_root_type && has_ordinary_map_owner_type)
            || (has_map_any_owner_type && has_ordinary_map_owner_type)
            || (has_ordinary_map_owned_get_type
                && (has_map_root_type || has_map_any_owner_type || has_ordinary_map_owner_type))
        {
            self.error(
                "MapRoot, MapAnyOwner v1, ordinary Map owner lifecycle v1, and ordinary Map owned-get v1 descriptors are independent profiles and cannot share one MIR program".into(),
            );
        }
        for function in self.program.functions().values() {
            self.validate_function(function);
        }
        for instance in self.program.instances().values() {
            let Some(function) = self.program.functions().get(&instance.function) else {
                self.error(format!(
                    "instance '{}' executable '{}' is absent",
                    instance.id, instance.function.0
                ));
                continue;
            };
            match &instance.contract {
                MirGenericInstanceContract::ManagedAnyIdentity => {
                    if let Err(message) = self
                        .program
                        .type_catalog()
                        .validate_managed_any_identity_instance(instance, function)
                    {
                        self.error(format!(
                            "instance '{}' ManagedAny identity receipt rejected: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::ManagedAnyTrapCleanup { .. } => {
                    if let Err(message) = self
                        .program
                        .type_catalog()
                        .validate_managed_any_trap_cleanup_instance(instance, function)
                    {
                        self.error(format!(
                            "instance '{}' ManagedAny trap-cleanup receipt rejected: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::ScalarIdentity
                | MirGenericInstanceContract::OwnedStringIdentity => {
                    if let Err(message) = self
                        .program
                        .type_catalog()
                        .validate_generic_identity_arguments(&instance.arguments)
                    {
                        self.error(format!(
                            "instance '{}' identity TypeDesc contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                    self.validate_identity_instance(function, instance.id.as_str());
                }
                MirGenericInstanceContract::OwnedRecordUpdate { contract } => {
                    if let Some(concrete) = instance.arguments.first() {
                        if let Err(message) = self
                            .program
                            .type_catalog()
                            .validate_owned_record_update_generic_argument(concrete)
                        {
                            self.error(format!(
                                "instance '{}' owned record update argument is unsupported: {message}",
                                instance.id
                            ));
                        }
                    } else {
                        self.error(format!(
                            "instance '{}' owned record update has no concrete argument",
                            instance.id
                        ));
                    }
                    if let Err(message) = crate::core::mir::lower::validate_owned_record_update_mir(
                        function,
                        self.program.type_catalog(),
                        contract,
                    ) {
                        self.error(format!(
                            "instance '{}' owned record update contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::ScalarSetFacade { operation } => {
                    if let Err(message) = crate::core::mir::lower::validate_scalar_set_facade_mir(
                        function,
                        self.program.type_catalog(),
                        *operation,
                    ) {
                        self.error(format!(
                            "instance '{}' Set facade contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::ScalarListFacade { operation } => {
                    if let Err(message) = crate::core::mir::lower::validate_scalar_list_facade_mir(
                        function,
                        self.program.type_catalog(),
                        *operation,
                    ) {
                        self.error(format!(
                            "instance '{}' List facade contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::ScalarListConstruct { contract } => {
                    if let Err(message) =
                        crate::core::mir::lower::validate_scalar_list_construct_mir(
                            function,
                            self.program.type_catalog(),
                            contract,
                        )
                    {
                        self.error(format!(
                            "instance '{}' List construction contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::ScalarListProjection {
                    contract,
                    index_value,
                } => {
                    if let Err(message) =
                        crate::core::mir::lower::validate_scalar_list_projection_mir(
                            function,
                            self.program.type_catalog(),
                            contract,
                            *index_value,
                        )
                    {
                        self.error(format!(
                            "instance '{}' List projection contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::ScalarRecordProjection { contract } => {
                    if let Err(message) =
                        crate::core::mir::lower::validate_scalar_record_projection_mir(
                            function,
                            self.program.type_catalog(),
                            contract,
                        )
                    {
                        self.error(format!(
                            "instance '{}' record projection contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::ScalarRecordUpdate { contract } => {
                    if let Err(message) = crate::core::mir::lower::validate_scalar_record_update_mir(
                        function,
                        self.program.type_catalog(),
                        contract,
                    ) {
                        self.error(format!(
                            "instance '{}' record update contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::ScalarTupleProjection { contract } => {
                    if let Err(message) =
                        crate::core::mir::lower::validate_scalar_tuple_projection_mir(
                            function,
                            self.program.type_catalog(),
                            contract,
                        )
                    {
                        self.error(format!(
                            "instance '{}' tuple projection contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::OwnedRecordProjection { contract } => {
                    if let Some(concrete) = instance.arguments.first() {
                        if let Err(message) = self
                            .program
                            .type_catalog()
                            .validate_move_owned_record_payload(concrete)
                        {
                            self.error(format!(
                                "instance '{}' owned record projection argument is unsupported: {message}",
                                instance.id
                            ));
                        }
                    } else {
                        self.error(format!(
                            "instance '{}' owned record projection has no concrete argument",
                            instance.id
                        ));
                    }
                    if let Err(message) =
                        crate::core::mir::lower::validate_owned_record_projection_mir(
                            function,
                            self.program.type_catalog(),
                            contract,
                        )
                    {
                        self.error(format!(
                            "instance '{}' owned record projection contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::OwnedRecordProjectionDrop { contract } => {
                    if let Some(concrete) = instance.arguments.first() {
                        if let Err(message) = self
                            .program
                            .type_catalog()
                            .validate_move_owned_record_payload(concrete)
                        {
                            self.error(format!(
                                "instance '{}' owned record move/drop projection argument is unsupported: {message}",
                                instance.id
                            ));
                        }
                    } else {
                        self.error(format!(
                            "instance '{}' owned record move/drop projection has no concrete argument",
                            instance.id
                        ));
                    }
                    if let Err(message) =
                        crate::core::mir::lower::validate_owned_record_projection_drop_mir(
                            function,
                            self.program.type_catalog(),
                            contract,
                        )
                    {
                        self.error(format!(
                            "instance '{}' owned record move/drop projection contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::ScalarVariantPredicate { contract } => {
                    if let Err(message) =
                        crate::core::mir::lower::validate_scalar_variant_predicate_mir(
                            function,
                            self.program.type_catalog(),
                            contract,
                        )
                    {
                        self.error(format!(
                            "instance '{}' Option predicate contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::ScalarVariantProjection { contract } => {
                    if contract.projection.ownership == MirOwnership::Move {
                        if !matches!(
                            contract.projection.nominal.as_str(),
                            "builtin:type:Option" | "builtin:type:Result"
                        ) {
                            self.error(format!(
                                "instance '{}' consuming generic variant projection is not the admitted managed Option/Result shape",
                                instance.id
                            ));
                        } else if instance.arguments.len() != 1 {
                            self.error(format!(
                                "instance '{}' owned generic variant projection requires one concrete argument",
                                instance.id
                            ));
                        } else if let Err(message) = self
                            .program
                            .type_catalog()
                            .validate_move_owned_payload(&instance.arguments[0])
                        {
                            self.error(format!(
                                "instance '{}' owned generic variant projection argument is unsupported: {message}",
                                instance.id
                            ));
                        }
                    }
                    if let Err(message) =
                        crate::core::mir::lower::validate_scalar_variant_projection_mir(
                            function,
                            self.program.type_catalog(),
                            contract,
                        )
                    {
                        self.error(format!(
                            "instance '{}' generic variant projection contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
                MirGenericInstanceContract::ScalarVariantProjectionFallback { contract } => {
                    if let Err(message) =
                        crate::core::mir::lower::validate_scalar_variant_projection_fallback_mir(
                            function,
                            self.program.type_catalog(),
                            contract,
                        )
                    {
                        self.error(format!(
                            "instance '{}' Option fallback projection contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
            }
            if matches!(
                instance.contract,
                MirGenericInstanceContract::OwnedStringIdentity
            ) {
                if let Some(concrete) = instance.arguments.first() {
                    if let Err(message) =
                        self.program.type_catalog().validate_owned_string(concrete)
                    {
                        self.error(format!(
                            "instance '{}' owned String TypeDesc contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                    if let Err(message) =
                        crate::core::mir::validate_owned_string_identity_shape(function, concrete)
                    {
                        self.error(format!(
                            "instance '{}' owned String identity shape is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
            }
        }
    }

    fn validate_function(&mut self, function: &MirFunction) {
        let has_managed_any = crate::verifier::mir::direct_call_uses_managed_any(
            function,
            self.program.type_catalog(),
        );
        if has_managed_any
            || function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(instruction.kind, MirInstructionKind::ManagedAny { .. })
                })
            })
        {
            if let Err(errors) = function.validate() {
                for error in errors {
                    self.error(format!(
                        "function '{}' ManagedAny sealed Checker receipt rejected: {error}",
                        function.owner.0
                    ));
                }
            }
            if let Err(message) = self
                .program
                .type_catalog()
                .validate_managed_any_function(function)
            {
                self.error(format!(
                    "function '{}' ManagedAny owner ledger rejected: {message}",
                    function.owner.0
                ));
            }
        }
        let map_any_owner_ty = crate::core::mir::types::map_any_owner_v1_type_id();
        let has_map_any_owner = function.result == map_any_owner_ty
            || function
                .values
                .values()
                .any(|value| value.ty == map_any_owner_ty)
            || function.ownership.has_checker_map_any_owner_actions()
            || function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    instruction_touches_map_any_owner(function, &instruction.kind)
                })
            });
        let has_map_root = function.result == crate::core::mir::types::map_root_type_id()
            || function
                .values
                .values()
                .any(|value| value.ty == crate::core::mir::types::map_root_type_id())
            || function.ownership.has_checker_map_root_actions()
            || function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    is_map_root_operation(&instruction.kind)
                        && !instruction_touches_map_any_owner(function, &instruction.kind)
                })
            });
        let ordinary_map_owner_ty =
            crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id();
        let has_ordinary_map_owner = function.result == ordinary_map_owner_ty
            || function
                .values
                .values()
                .any(|value| value.ty == ordinary_map_owner_ty)
            || function
                .ownership
                .has_checker_ordinary_map_owner_lifecycle_v1_actions()
            || function.blocks.values().any(|block| {
                block
                    .instructions
                    .iter()
                    .any(|instruction| instruction_touches_ordinary_map_owner(&instruction.kind))
            });
        let ordinary_map_owned_get_ty =
            crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id();
        let has_ordinary_map_owned_get = function.result == ordinary_map_owned_get_ty
            || function
                .values
                .values()
                .any(|value| value.ty == ordinary_map_owned_get_ty)
            || function
                .ownership
                .has_checker_ordinary_map_owned_get_i32_string_v1_actions()
            || function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    instruction_touches_ordinary_map_owned_get(&instruction.kind)
                })
            });
        if (has_map_any_owner && has_map_root)
            || (has_ordinary_map_owner && has_map_any_owner)
            || (has_ordinary_map_owner && has_map_root)
            || (has_ordinary_map_owned_get
                && (has_ordinary_map_owner || has_map_any_owner || has_map_root))
        {
            self.error(format!(
                "function '{}' mixes independent MapRoot, MapAnyOwner v1, ordinary Map owner lifecycle v1, or ordinary Map owned-get v1 ownership profiles",
                function.owner.0
            ));
        }
        if has_map_any_owner {
            if !function.ownership.has_checker_map_any_owner_actions() {
                self.error(format!(
                    "function '{}' uses MapAnyOwner v1 without an independent Checker owner receipt",
                    function.owner.0
                ));
            }
            if function.ownership.has_checker_map_root_actions() {
                self.error(format!(
                    "function '{}' raw MapRoot receipt cannot authorize MapAnyOwner v1 operations",
                    function.owner.0
                ));
            }
            if let Err(errors) = function.validate() {
                for error in errors {
                    self.error(format!(
                        "function '{}' MapAnyOwner v1 MIR validation failed: {error}",
                        function.owner.0
                    ));
                }
            }
        }
        if has_map_root {
            if let Err(errors) = function.validate() {
                for error in errors {
                    self.error(format!(
                        "function '{}' MapRoot MIR validation failed: {error}",
                        function.owner.0
                    ));
                }
            }
        }
        if has_ordinary_map_owner {
            if !function
                .ownership
                .has_checker_ordinary_map_owner_lifecycle_v1_actions()
            {
                self.error(format!(
                    "function '{}' uses ordinary Map owner lifecycle v1 without its independent Checker receipt",
                    function.owner.0
                ));
            }
            if function.ownership.has_checker_map_root_actions()
                || function.ownership.has_checker_map_any_owner_actions()
            {
                self.error(format!(
                    "function '{}' ordinary Map owner lifecycle v1 cannot borrow another Map profile receipt",
                    function.owner.0
                ));
            }
            if let Err(errors) = function.validate() {
                for error in errors {
                    self.error(format!(
                        "function '{}' ordinary Map owner lifecycle v1 MIR validation failed: {error}",
                        function.owner.0
                    ));
                }
            }
        }
        if has_ordinary_map_owned_get {
            if !function
                .ownership
                .has_checker_ordinary_map_owned_get_i32_string_v1_actions()
            {
                self.error(format!(
                    "function '{}' uses ordinary Map owned-get v1 without its independent Checker receipt",
                    function.owner.0
                ));
            }
            if function.ownership.has_checker_map_root_actions()
                || function.ownership.has_checker_map_any_owner_actions()
                || function
                    .ownership
                    .has_checker_ordinary_map_owner_lifecycle_v1_actions()
            {
                self.error(format!(
                    "function '{}' ordinary Map owned-get v1 cannot borrow another Map profile receipt",
                    function.owner.0
                ));
            }
            if let Err(errors) = function.validate() {
                for error in errors {
                    self.error(format!(
                        "function '{}' ordinary Map owned-get v1 MIR validation failed: {error}",
                        function.owner.0
                    ));
                }
            }
        }
        for value in function.values.values() {
            self.validate_type(&value.ty, &format!("function '{}' value", function.owner.0));
        }
        self.validate_type(
            &function.result,
            &format!("function '{}' result", function.owner.0),
        );
        if let Err(message) = crate::core::mir::validate_entry_invariant_shapes(function) {
            self.error(format!("function '{}' {message}", function.owner.0));
        }
        self.validate_acyclic_cfg(function);
        for block in function.blocks.values() {
            for instruction in &block.instructions {
                self.validate_instruction(
                    function,
                    &instruction.id,
                    &instruction.kind,
                    instruction.id.as_str(),
                );
            }
            self.validate_terminator(function, &block.terminator, block.id.as_str());
        }
        for event in &function.ownership.events {
            let unsupported =
                if event.kind == crate::core::mir::MirOwnershipEventKind::TransferSession {
                    !function.blocks.values().any(|block| {
                        block.instructions.iter().any(|instruction| {
                            let point = instruction
                                .id
                                .as_str()
                                .split_once(':')
                                .and_then(|(_, rest)| rest.split_once(':'))
                                .map(|(_, point)| point);
                            matches!(
                                &instruction.kind,
                                MirInstructionKind::SessionCall {
                                    endpoint,
                                    contract: Some(contract),
                                    ..
                                } if point == Some(event.point.0.as_str())
                                    && event.value.as_ref() == Some(endpoint)
                                    && !contract.terminal
                            )
                        })
                    })
                } else {
                    matches!(
                        event.kind,
                        crate::core::mir::MirOwnershipEventKind::TransferChild
                            | crate::core::mir::MirOwnershipEventKind::BorrowMut
                    )
                };
            if unsupported {
                self.error(format!(
                    "function '{}' ownership effect '{}' is outside the MIR verifier capability",
                    function.owner.0,
                    event.kind.as_str()
                ));
            }
        }
        if has_map_any_owner {
            self.validate_map_any_owner_function(function);
        }
        if has_ordinary_map_owner {
            self.validate_ordinary_map_owner_function(function);
        }
        if has_ordinary_map_owned_get {
            self.validate_ordinary_map_owned_get_function(function);
        }
    }

    fn validate_map_root_type(
        &mut self,
        function: &MirFunction,
        value: &crate::core::mir::MirValueId,
        subject: &str,
        role: &str,
    ) {
        if function
            .values
            .get(value)
            .is_none_or(|value| value.ty != crate::core::mir::types::map_root_type_id())
        {
            self.error(format!(
                "{subject} MapRoot {role} does not use the canonical MapRoot TypeDesc"
            ));
        }
    }

    fn validate_map_any_owner_descriptor(&mut self) {
        let ty = crate::core::mir::types::map_any_owner_v1_type_id();
        let catalog = self.program.type_catalog();
        for operation in [MirGlueOperation::MoveOut, MirGlueOperation::Drop] {
            if let Err(message) = catalog.validate_glue(&ty, operation) {
                self.error(format!("MapAnyOwner v1 TypeDesc rejected: {message}"));
            }
        }
        if catalog.validate_glue(&ty, MirGlueOperation::Clone).is_ok() {
            self.error("MapAnyOwner v1 TypeDesc unexpectedly exposes generic Clone glue".into());
        }
    }

    fn validate_ordinary_map_owner_descriptor(&mut self) {
        let ty = crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id();
        let catalog = self.program.type_catalog();
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            if let Err(message) = catalog.validate_glue(&ty, operation) {
                self.error(format!(
                    "ordinary Map owner lifecycle v1 TypeDesc rejected: {message}"
                ));
            }
        }
    }

    fn validate_ordinary_map_owned_get_descriptor(&mut self) {
        let ty = crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id();
        let catalog = self.program.type_catalog();
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            if let Err(message) = catalog.validate_glue(&ty, operation) {
                self.error(format!(
                    "ordinary Map owned-get v1 TypeDesc rejected: {message}"
                ));
            }
        }
    }

    fn validate_ordinary_map_owned_get_function(&mut self, function: &MirFunction) {
        let owner_ty = crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id();
        if function.result == owner_ty {
            self.error(format!(
                "function '{}' cannot return an ordinary Map owned-get v1 root",
                function.owner.0
            ));
        }
        for parameter in &function.parameters {
            if function
                .values
                .get(parameter)
                .is_some_and(|value| value.ty == owner_ty)
            {
                self.error(format!(
                    "function '{}' cannot receive an ordinary Map owned-get v1 root as a parameter",
                    function.owner.0
                ));
            }
        }
        for block in function.blocks.values() {
            for parameter in &block.parameters {
                if function
                    .values
                    .get(&parameter.value)
                    .is_some_and(|value| value.ty == owner_ty)
                {
                    self.error(format!(
                        "function '{}' cannot receive an ordinary Map owned-get v1 root as a block parameter",
                        function.owner.0
                    ));
                }
            }
            for instruction in &block.instructions {
                if instruction_touches_ordinary_map_owned_get(&instruction.kind)
                    || (matches!(&instruction.kind, MirInstructionKind::Clone { .. })
                        && instruction_touches_ordinary_map_owned_get_type(
                            function,
                            &instruction.kind,
                        ))
                {
                    self.validate_ordinary_map_owned_get_instruction(
                        function,
                        &instruction.kind,
                        instruction.id.as_str(),
                    );
                } else if instruction_touches_ordinary_map_owned_get_type(
                    function,
                    &instruction.kind,
                ) {
                    self.error(format!(
                        "{} ordinary Map owned-get v1 value is used outside its dedicated receipt-bound operation",
                        instruction.id
                    ));
                }
            }
        }
    }

    fn validate_ordinary_map_owned_get_instruction(
        &mut self,
        function: &MirFunction,
        instruction: &MirInstructionKind,
        subject: &str,
    ) {
        match instruction {
            MirInstructionKind::Clone { result, source } => {
                self.validate_ordinary_map_owned_get_type(
                    function,
                    result,
                    subject,
                    "Clone result",
                );
                self.validate_ordinary_map_owned_get_type(
                    function,
                    source,
                    subject,
                    "Clone source",
                );
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1New { result } => {
                self.validate_ordinary_map_owned_get_type(function, result, subject, "New result");
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Set {
                result,
                source,
                key,
                value,
            } => {
                self.validate_ordinary_map_owned_get_type(function, result, subject, "Set result");
                self.validate_ordinary_map_owned_get_type(function, source, subject, "Set source");
                self.validate_owned_get_string_value(function, key, subject, "Set key");
                self.validate_map_set_payload(
                    function,
                    value,
                    subject,
                    "ordinary Map owned-get v1",
                );
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Remove {
                result,
                source,
                key,
            } => {
                self.validate_ordinary_map_owned_get_type(
                    function,
                    result,
                    subject,
                    "Remove result",
                );
                self.validate_ordinary_map_owned_get_type(
                    function,
                    source,
                    subject,
                    "Remove source",
                );
                self.validate_owned_get_string_value(function, key, subject, "Remove key");
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Size { result, root } => {
                self.validate_ordinary_map_owned_get_type(function, root, subject, "Size root");
                self.validate_i32_value(
                    function,
                    result,
                    subject,
                    "ordinary Map owned-get v1 Size result",
                );
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Contains { result, root, key } => {
                self.validate_ordinary_map_owned_get_type(function, root, subject, "Contains root");
                self.validate_bool_value(
                    function,
                    result,
                    subject,
                    "ordinary Map owned-get v1 Contains result",
                );
                self.validate_owned_get_string_value(function, key, subject, "Contains key");
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetI32 { result, root, key } => {
                self.validate_ordinary_map_owned_get_type(function, root, subject, "GetI32 root");
                self.validate_owned_get_string_value(function, key, subject, "GetI32 key");
                self.validate_owned_get_result(function, result, subject, false);
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetString { result, root, key } => {
                self.validate_ordinary_map_owned_get_type(
                    function,
                    root,
                    subject,
                    "GetString root",
                );
                self.validate_owned_get_string_value(function, key, subject, "GetString key");
                self.validate_owned_get_result(function, result, subject, true);
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { result, root, key } => {
                self.validate_ordinary_map_owned_get_type(function, root, subject, "GetAny root");
                self.validate_owned_get_string_value(function, key, subject, "GetAny key");
                let validation = value_type(function, result)
                    .ok_or_else(|| "GetAny result value is absent".to_string())
                    .and_then(|ty| {
                        self.program
                            .type_catalog()
                            .validate_ordinary_map_owned_get_any_result_type(&ty)
                    });
                if let Err(message) = validation {
                    self.error(format!(
                        "{subject} ordinary Map GetAny result rejected: {message}"
                    ));
                }
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { result, root } => {
                self.validate_ordinary_map_owned_get_type(function, root, subject, "Values root");
                self.validate_owned_get_values_result(function, result, subject);
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Drop { root } => {
                self.validate_ordinary_map_owned_get_type(function, root, subject, "Drop operand");
            }
            _ => self.error(format!(
                "{subject} is not a dedicated ordinary Map owned-get v1 operation"
            )),
        }
    }

    fn validate_ordinary_map_owned_get_type(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
        role: &str,
    ) {
        if function.values.get(value).is_none_or(|value| {
            value.ty != crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id()
        }) {
            self.error(format!(
                "{subject} ordinary Map owned-get v1 {role} does not use its distinct canonical TypeDesc"
            ));
        }
    }

    fn validate_owned_get_string_value(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
        role: &str,
    ) {
        let valid = function
            .values
            .get(value)
            .and_then(|value| self.program.type_catalog().get(&value.ty))
            .is_some_and(|descriptor| {
                descriptor.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::String)
                    && self
                        .program
                        .type_catalog()
                        .validate_owned_string(&descriptor.id)
                        .is_ok()
            });
        if !valid {
            self.error(format!(
                "{subject} ordinary Map owned-get v1 {role} is not the canonical owned String TypeDesc"
            ));
        }
    }

    fn validate_owned_get_result(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        subject: &str,
        string_payload: bool,
    ) {
        let Some(ty) = value_type(function, result) else {
            self.error(format!("{subject} ordinary Map owned-get result is absent"));
            return;
        };
        let validation = if string_payload {
            self.program
                .type_catalog()
                .validate_result_string_i32_variant(&ty)
        } else {
            self.program
                .type_catalog()
                .validate_copy_result_i32_variant(&ty)
        };
        if let Err(message) = validation {
            let shape = if string_payload {
                "Result<string, i32>"
            } else {
                "Result<i32, i32>"
            };
            self.error(format!(
                "{subject} ordinary Map owned-get result must use canonical {shape}: {message}"
            ));
        }
    }

    fn validate_owned_get_values_result(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        subject: &str,
    ) {
        let Some(ty) = value_type(function, result) else {
            self.error(format!("{subject} ordinary Map Values result is absent"));
            return;
        };
        if !is_canonical_managed_any_list_type(self.program.type_catalog(), &ty) {
            self.error(format!(
                "{subject} ordinary Map Values result must use canonical owned List<ManagedAny> TypeDesc"
            ));
        }
    }

    fn validate_ordinary_map_owner_function(&mut self, function: &MirFunction) {
        let owner_ty =
            crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id();
        if function.result == owner_ty {
            self.error(format!(
                "function '{}' cannot return an ordinary Map owner lifecycle v1 root",
                function.owner.0
            ));
        }
        for parameter in &function.parameters {
            if function
                .values
                .get(parameter)
                .is_some_and(|value| value.ty == owner_ty)
            {
                self.error(format!(
                    "function '{}' cannot receive an ordinary Map owner lifecycle v1 root as a parameter",
                    function.owner.0
                ));
            }
        }
        for block in function.blocks.values() {
            for parameter in &block.parameters {
                if function
                    .values
                    .get(&parameter.value)
                    .is_some_and(|value| value.ty == owner_ty)
                {
                    self.error(format!(
                        "function '{}' cannot receive an ordinary Map owner lifecycle v1 root as a block parameter",
                        function.owner.0
                    ));
                }
            }
            for instruction in &block.instructions {
                let is_owner_clone = matches!(&instruction.kind, MirInstructionKind::Clone { .. })
                    && instruction_touches_ordinary_type(function, &instruction.kind);
                if instruction_touches_ordinary_map_owner(&instruction.kind) || is_owner_clone {
                    self.validate_ordinary_map_owner_instruction(
                        function,
                        &instruction.kind,
                        instruction.id.as_str(),
                    );
                }
                if instruction_touches_ordinary_type(function, &instruction.kind)
                    && !instruction_touches_ordinary_map_owner(&instruction.kind)
                    && !is_owner_clone
                {
                    self.error(format!(
                        "{} ordinary Map owner lifecycle v1 value is used outside its dedicated receipt-bound operation",
                        instruction.id
                    ));
                }
            }
        }
    }

    fn validate_ordinary_map_owner_instruction(
        &mut self,
        function: &MirFunction,
        instruction: &MirInstructionKind,
        subject: &str,
    ) {
        match instruction {
            MirInstructionKind::Clone { result, source } => {
                self.validate_ordinary_map_owner_type(function, result, subject, "Clone result");
                self.validate_ordinary_map_owner_type(function, source, subject, "Clone source");
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleNew { result } => {
                self.validate_ordinary_map_owner_type(function, result, subject, "New result");
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleSet {
                result,
                source,
                key,
                value,
            } => {
                self.validate_ordinary_map_owner_type(function, result, subject, "Set result");
                self.validate_ordinary_map_owner_type(function, source, subject, "Set source");
                self.validate_owned_string_value(function, key, subject, "Set key");
                self.validate_map_set_payload(
                    function,
                    value,
                    subject,
                    "ordinary Map owner lifecycle v1",
                );
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleRemove {
                result,
                source,
                key,
            } => {
                self.validate_ordinary_map_owner_type(function, result, subject, "Remove result");
                self.validate_ordinary_map_owner_type(function, source, subject, "Remove source");
                self.validate_owned_string_value(function, key, subject, "Remove key");
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleSize { result, root } => {
                self.validate_ordinary_map_owner_type(function, root, subject, "Size root");
                self.validate_i32_value(
                    function,
                    result,
                    subject,
                    "ordinary Map owner lifecycle v1 Size result",
                );
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleContains { result, root, key } => {
                self.validate_ordinary_map_owner_type(function, root, subject, "Contains root");
                self.validate_bool_value(
                    function,
                    result,
                    subject,
                    "ordinary Map owner lifecycle v1 Contains result",
                );
                self.validate_owned_string_value(function, key, subject, "Contains key");
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleDrop { root } => {
                self.validate_ordinary_map_owner_type(function, root, subject, "Drop operand");
            }
            _ => self.error(format!(
                "{subject} is not a dedicated ordinary Map owner lifecycle v1 operation"
            )),
        }
    }

    fn validate_ordinary_map_owner_type(
        &mut self,
        function: &MirFunction,
        value: &crate::core::mir::MirValueId,
        subject: &str,
        role: &str,
    ) {
        if function.values.get(value).is_none_or(|value| {
            value.ty
                != crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id()
        }) {
            self.error(format!(
                "{subject} ordinary Map owner lifecycle v1 {role} does not use its canonical TypeDesc"
            ));
        }
    }

    fn validate_owned_string_value(
        &mut self,
        function: &MirFunction,
        value: &crate::core::mir::MirValueId,
        subject: &str,
        role: &str,
    ) {
        let valid = function
            .values
            .get(value)
            .and_then(|value| self.program.type_catalog().get(&value.ty))
            .is_some_and(|descriptor| {
                descriptor.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::String)
                    && self
                        .program
                        .type_catalog()
                        .validate_owned_string(&descriptor.id)
                        .is_ok()
            });
        if !valid {
            self.error(format!(
                "{subject} ordinary Map owner lifecycle v1 {role} is not the canonical owned String TypeDesc"
            ));
        }
    }

    fn validate_map_any_owner_function(&mut self, function: &MirFunction) {
        let owner_ty = crate::core::mir::types::map_any_owner_v1_type_id();
        if function.result == owner_ty {
            self.error(format!(
                "function '{}' cannot return a MapAnyOwner v1 root",
                function.owner.0
            ));
        }
        for parameter in &function.parameters {
            if function
                .values
                .get(parameter)
                .is_some_and(|value| value.ty == owner_ty)
            {
                self.error(format!(
                    "function '{}' cannot receive a MapAnyOwner v1 root as a parameter",
                    function.owner.0
                ));
            }
        }
        for block in function.blocks.values() {
            for parameter in &block.parameters {
                if function
                    .values
                    .get(&parameter.value)
                    .is_some_and(|value| value.ty == owner_ty)
                {
                    self.error(format!(
                        "function '{}' cannot receive a MapAnyOwner v1 root as a block parameter",
                        function.owner.0
                    ));
                }
            }
            for instruction in &block.instructions {
                if instruction_touches_map_any_owner(function, &instruction.kind) {
                    self.validate_map_any_owner_instruction(
                        function,
                        &instruction.kind,
                        instruction.id.as_str(),
                    );
                }
            }
        }
    }

    fn validate_map_any_owner_instruction(
        &mut self,
        function: &MirFunction,
        instruction: &MirInstructionKind,
        subject: &str,
    ) {
        match instruction {
            MirInstructionKind::MapRootNew { result } => {
                self.validate_map_any_owner_type(function, result, subject, "New result");
            }
            MirInstructionKind::MapRootSet {
                result,
                source,
                key,
                value,
            } => {
                self.validate_map_any_owner_type(function, result, subject, "Set result");
                self.validate_map_any_owner_type(function, source, subject, "Set source");
                if key.contains('\0') {
                    self.error(format!("{subject} MapAnyOwner v1 key contains NUL"));
                }
                self.validate_map_any_owner_set_payload(function, value, subject);
            }
            MirInstructionKind::MapRootRemove {
                result,
                source,
                key,
            } => {
                self.validate_map_any_owner_type(function, result, subject, "Remove result");
                self.validate_map_any_owner_type(function, source, subject, "Remove source");
                if key.contains('\0') {
                    self.error(format!("{subject} MapAnyOwner v1 key contains NUL"));
                }
            }
            MirInstructionKind::MapRootSize { result, root } => {
                self.validate_map_any_owner_type(function, root, subject, "Size root");
                self.validate_i32_value(function, result, subject, "MapAnyOwner v1 Size result");
            }
            MirInstructionKind::MapRootContains { result, root, key } => {
                self.validate_map_any_owner_type(function, root, subject, "Contains root");
                self.validate_bool_value(
                    function,
                    result,
                    subject,
                    "MapAnyOwner v1 Contains result",
                );
                if key.contains('\0') {
                    self.error(format!("{subject} MapAnyOwner v1 key contains NUL"));
                }
            }
            MirInstructionKind::MapRootDrop { root } => {
                self.validate_map_any_owner_type(function, root, subject, "Drop operand");
            }
            _ => self.error(format!(
                "{subject} is not a dedicated MapAnyOwner v1 lifecycle operation"
            )),
        }
    }

    fn validate_map_any_owner_type(
        &mut self,
        function: &MirFunction,
        value: &crate::core::mir::MirValueId,
        subject: &str,
        role: &str,
    ) {
        if function
            .values
            .get(value)
            .is_none_or(|value| value.ty != crate::core::mir::types::map_any_owner_v1_type_id())
        {
            self.error(format!(
                "{subject} MapAnyOwner v1 {role} does not use the canonical v1 TypeDesc"
            ));
        }
    }

    fn validate_i32_value(
        &mut self,
        function: &MirFunction,
        value: &crate::core::mir::MirValueId,
        subject: &str,
        role: &str,
    ) {
        let valid = function
            .values
            .get(value)
            .and_then(|value| self.program.type_catalog().get(&value.ty))
            .is_some_and(|descriptor| {
                descriptor.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::I32)
                    && self
                        .program
                        .type_catalog()
                        .validate_copy_scalar(&descriptor.id)
                        .is_ok()
            });
        if !valid {
            self.error(format!(
                "{subject} {role} is not the canonical i32 TypeDesc"
            ));
        }
    }

    fn validate_bool_value(
        &mut self,
        function: &MirFunction,
        value: &crate::core::mir::MirValueId,
        subject: &str,
        role: &str,
    ) {
        let valid = function
            .values
            .get(value)
            .and_then(|value| self.program.type_catalog().get(&value.ty))
            .is_some_and(|descriptor| {
                descriptor.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::Bool)
                    && self
                        .program
                        .type_catalog()
                        .validate_copy_scalar(&descriptor.id)
                        .is_ok()
            });
        if !valid {
            self.error(format!(
                "{subject} {role} is not the canonical bool TypeDesc"
            ));
        }
    }

    fn validate_map_root_set_payload(
        &mut self,
        function: &MirFunction,
        value: &crate::core::mir::MirValueId,
        subject: &str,
    ) {
        self.validate_map_set_payload(function, value, subject, "MapRoot");
    }

    fn validate_map_any_owner_set_payload(
        &mut self,
        function: &MirFunction,
        value: &crate::core::mir::MirValueId,
        subject: &str,
    ) {
        self.validate_map_set_payload(function, value, subject, "MapAnyOwner v1");
    }

    fn validate_map_set_payload(
        &mut self,
        function: &MirFunction,
        value: &crate::core::mir::MirValueId,
        subject: &str,
        profile: &str,
    ) {
        let valid = function
            .values
            .get(value)
            .and_then(|value| self.program.type_catalog().get(&value.ty))
            .is_some_and(|descriptor| match descriptor.kind {
                MirTypeKind::Primitive(crate::core::PrimitiveType::I32) => self
                    .program
                    .type_catalog()
                    .validate_copy_scalar(&descriptor.id)
                    .is_ok(),
                MirTypeKind::Primitive(crate::core::PrimitiveType::String) => self
                    .program
                    .type_catalog()
                    .validate_owned_string(&descriptor.id)
                    .is_ok(),
                _ => false,
            });
        if !valid {
            self.error(format!(
                "{subject} {profile} Set payload is outside the receipted i32/String TypeDesc profile"
            ));
        }
    }

    fn validate_type(&mut self, ty: &crate::core::ResolvedTypeId, subject: &str) {
        if !self.checked_types.insert(ty.clone()) {
            return;
        }
        let result = self.validate_type_contract(ty);
        if let Err(message) = result {
            self.error(format!(
                "{subject} type '{}' rejected: {message}",
                ty.as_str()
            ));
        }
    }

    fn validate_type_contract(&mut self, ty: &crate::core::ResolvedTypeId) -> Result<(), String> {
        let catalog = self.program.type_catalog();
        let descriptor = catalog
            .get(ty)
            .ok_or_else(|| format!("TypeDesc '{}' is absent", ty.as_str()))?
            .clone();
        match &descriptor.layout {
            MirLayout::Unit => {
                if descriptor.is_canonical_ffi_unit() {
                    Ok(())
                } else {
                    Err("Unit TypeDesc has an inconsistent ABI/ownership/glue contract".into())
                }
            }
            MirLayout::Scalar => {
                if catalog.validate_copy_scalar(ty).is_ok()
                    || catalog.validate_copy_float_scalar(ty).is_ok()
                    || catalog.validate_owned_string(ty).is_ok()
                {
                    Ok(())
                } else {
                    Err("scalar TypeDesc is outside the verifier scalar contract".into())
                }
            }
            MirLayout::Handle => {
                if catalog.validate_session_channel(ty).is_ok()
                    || catalog.validate_owned_string(ty).is_ok()
                {
                    Ok(())
                } else {
                    Err("handle TypeDesc is not the canonical owned String contract".into())
                }
            }
            MirLayout::Pointer { .. } => {
                if catalog.validate_reference_type(ty).is_ok() {
                    Ok(())
                } else {
                    Err(
                        "pointer TypeDesc is outside the immutable scalar reference contract"
                            .into(),
                    )
                }
            }
            MirLayout::MapRoot => {
                catalog.validate_glue(ty, crate::core::mir::types::MirGlueOperation::MoveOut)?;
                catalog.validate_glue(ty, crate::core::mir::types::MirGlueOperation::Drop)?;
                if catalog
                    .validate_glue(ty, crate::core::mir::types::MirGlueOperation::Clone)
                    .is_ok()
                {
                    return Err("MapRoot TypeDesc unexpectedly exposes generic Clone glue".into());
                }
                Ok(())
            }
            MirLayout::MapAnyOwnerV1 => {
                catalog.validate_glue(ty, MirGlueOperation::MoveOut)?;
                catalog.validate_glue(ty, MirGlueOperation::Drop)?;
                if catalog.validate_glue(ty, MirGlueOperation::Clone).is_ok() {
                    return Err(
                        "MapAnyOwner v1 TypeDesc unexpectedly exposes generic Clone glue".into(),
                    );
                }
                Ok(())
            }
            MirLayout::OrdinaryMapOwnerLifecycleI32StringV1 => {
                catalog.validate_glue(ty, MirGlueOperation::MoveOut)?;
                catalog.validate_glue(ty, MirGlueOperation::Clone)?;
                catalog.validate_glue(ty, MirGlueOperation::Drop)?;
                Ok(())
            }
            MirLayout::OrdinaryMapOwnedGetI32StringV1 => {
                catalog.validate_glue(ty, MirGlueOperation::MoveOut)?;
                catalog.validate_glue(ty, MirGlueOperation::Clone)?;
                catalog.validate_glue(ty, MirGlueOperation::Drop)?;
                Ok(())
            }
            MirLayout::ManagedAnyI32StringV1 => {
                if !catalog.is_managed_any_i32_string_v1(ty) {
                    return Err(
                        "ManagedAny TypeDesc identity does not match its canonical owner profile"
                            .into(),
                    );
                }
                for operation in [
                    MirGlueOperation::MoveOut,
                    MirGlueOperation::Clone,
                    MirGlueOperation::Drop,
                ] {
                    catalog.validate_glue(ty, operation)?;
                }
                Ok(())
            }
            MirLayout::List { element } => {
                catalog
                    .validate_list_glue(ty, crate::core::mir::types::MirGlueOperation::MoveOut)?;
                self.validate_type(element, "List element");
                Ok(())
            }
            MirLayout::Set { element } => {
                catalog
                    .validate_set_glue(ty, crate::core::mir::types::MirGlueOperation::MoveOut)?;
                self.validate_type(element, "Set element");
                Ok(())
            }
            MirLayout::Tuple(elements) => {
                catalog.validate_recursive_tuple_abi(ty)?;
                for element in elements {
                    self.validate_type(element, "tuple element");
                }
                Ok(())
            }
            MirLayout::Record { fields, .. } => {
                match descriptor.ownership {
                    MirOwnership::Linear => self.require_linear_aggregate(ty, &descriptor)?,
                    MirOwnership::Copy => self.require_copy_aggregate(ty, &descriptor)?,
                    MirOwnership::Move => self.require_move_aggregate(ty, &descriptor)?,
                    ownership => {
                        return Err(format!(
                            "record '{}' ownership {:?} is outside the verifier capability",
                            ty.as_str(),
                            ownership
                        ));
                    }
                }
                for field in fields {
                    self.validate_type(&field.ty, "record field");
                }
                Ok(())
            }
            MirLayout::Option { variants, .. } | MirLayout::Result { variants, .. } => {
                if descriptor.ownership == MirOwnership::Copy {
                    self.require_copy_aggregate(ty, &descriptor)?;
                } else {
                    let validation = if self.allow_recoverable_flow_result
                        && matches!(descriptor.kind, MirTypeKind::Result)
                    {
                        catalog.validate_recoverable_result_variant(ty)
                    } else if self.ordinary_map_owned_get_result_string_types.contains(ty)
                        && matches!(descriptor.kind, MirTypeKind::Result)
                    {
                        catalog.validate_result_string_i32_variant(ty)
                    } else if self.allow_result_move_variant
                        && matches!(descriptor.kind, MirTypeKind::Result)
                    {
                        catalog
                            .validate_result_move_projection_variant(ty)
                            .map(|_| ())
                    } else {
                        catalog.validate_non_copy_variant_contract(ty)
                    };
                    validation.map_err(|message| {
                        format!(
                            "non-Copy variant TypeDesc is outside the verifier capability: {message}"
                        )
                    })?;
                }
                for variant in variants {
                    for field in &variant.fields {
                        self.validate_type(&field.ty, "variant field");
                    }
                }
                Ok(())
            }
            MirLayout::Enum { variants, .. } => {
                // Checker-materialized enum layouts (user enum type defs and
                // the multi-target Flow union) share the flat Copy variant
                // contract; a multi-target union additionally proves the
                // tagged-union contract (Copy scalars and owned Strings, one
                // payload per variant, complete glue) from the same
                // checker-owned TypeDesc regardless of combined ownership.
                // User enums stay flat-only.
                if descriptor.kind == MirTypeKind::FlowStateSet {
                    catalog
                        .validate_multi_target_union_variant(ty)
                        .map_err(|message| {
                            format!(
                                "multi-target union TypeDesc is outside the verifier capability: {message}"
                            )
                        })?;
                    // R6-1040: the compiler-owned Fault sink's payload fields
                    // (StateId/EventId/SystemTrace) sit outside the flat leaf
                    // families, but the union contract already proved each of
                    // them glue-complete; recursing into them as standalone
                    // enums would re-litigate shapes the sink owns.
                    for variant in variants {
                        if variant.name == "Fault" {
                            continue;
                        }
                        for field in &variant.fields {
                            self.validate_type(&field.ty, "variant field");
                        }
                    }
                    return Ok(());
                } else if descriptor.ownership != MirOwnership::Copy {
                    return Err(
                        "non-Copy enum TypeDesc is outside the verifier capability: only the flat Copy variant contract is admitted"
                            .into(),
                    );
                } else {
                    catalog.validate_flat_copy_variant(ty).map_err(|message| {
                        format!("Copy enum TypeDesc is outside the verifier capability: {message}")
                    })?;
                }
                for variant in variants {
                    for field in &variant.fields {
                        self.validate_type(&field.ty, "variant field");
                    }
                }
                Ok(())
            }
            layout => Err(format!(
                "layout {layout:?} is outside the verifier capability"
            )),
        }
    }

    fn require_copy_aggregate(
        &self,
        ty: &crate::core::ResolvedTypeId,
        descriptor: &crate::core::mir::types::MirTypeDesc,
    ) -> Result<(), String> {
        if descriptor.abi != MirAbiClass::Aggregate
            || !descriptor.has_canonical_copy_noop_metadata()
        {
            return Err(format!(
                "aggregate '{}' is not Copy with canonical no-op glue",
                ty.as_str()
            ));
        }
        Ok(())
    }

    fn require_linear_aggregate(
        &self,
        ty: &crate::core::ResolvedTypeId,
        descriptor: &crate::core::mir::types::MirTypeDesc,
    ) -> Result<(), String> {
        if descriptor.ownership != MirOwnership::Linear
            || descriptor.abi != MirAbiClass::Aggregate
            || descriptor.glue.move_out != MirGlueKind::Aggregate
            || descriptor.glue.clone != MirGlueKind::Aggregate
            || descriptor.glue.drop != MirGlueKind::Aggregate
            || descriptor.drop_plan.is_none()
        {
            return Err(format!(
                "linear aggregate '{}' lacks the canonical aggregate Move/Clone/Drop glue plan",
                ty.as_str()
            ));
        }
        Ok(())
    }

    fn require_move_aggregate(
        &self,
        ty: &crate::core::ResolvedTypeId,
        descriptor: &crate::core::mir::types::MirTypeDesc,
    ) -> Result<(), String> {
        if descriptor.ownership != MirOwnership::Move
            || descriptor.abi != MirAbiClass::Aggregate
            || descriptor.glue
                != (crate::core::mir::types::MirGlueContract {
                    move_out: MirGlueKind::Aggregate,
                    clone: MirGlueKind::Aggregate,
                    drop: MirGlueKind::Aggregate,
                })
            || !descriptor.needs_drop_glue
            || !descriptor.needs_clone_glue
            || descriptor.drop_plan.is_none()
        {
            return Err(format!(
                "move record '{}' lacks canonical aggregate glue/drop plan",
                ty.as_str()
            ));
        }
        Ok(())
    }

    fn validate_instruction(
        &mut self,
        function: &MirFunction,
        instruction_id: &crate::core::mir::MirInstructionId,
        instruction: &MirInstructionKind,
        subject: &str,
    ) {
        let catalog = self.program.type_catalog();
        match instruction {
            MirInstructionKind::ManagedAny { result, source, operation, contract } => {
                if let Err(message) = catalog.validate_managed_any_contract(
                    function, source, result.as_ref(), *operation, contract.as_ref())
                {
                    self.error(format!("{subject} ManagedAny contract rejected: {message}"));
                }
            }
            MirInstructionKind::Actor { .. } => self.error(format!("{subject}: Actor mailbox/state instruction requires its complete finite execution receipt and no proof obligations")),
            MirInstructionKind::MapRootNew { result } => {
                if instruction_touches_map_any_owner(function, instruction) {
                    self.validate_map_any_owner_instruction(function, instruction, subject);
                } else {
                    self.validate_map_root_type(function, result, subject, "New result");
                }
            }
            MirInstructionKind::MapRootSet {
                result,
                source,
                key,
                value,
            } => {
                if instruction_touches_map_any_owner(function, instruction) {
                    self.validate_map_any_owner_instruction(function, instruction, subject);
                } else {
                    self.validate_map_root_type(function, result, subject, "Set result");
                    self.validate_map_root_type(function, source, subject, "Set source");
                    if key.contains('\0') {
                        self.error(format!("{subject} MapRoot key contains NUL"));
                    }
                    self.validate_map_root_set_payload(function, value, subject);
                }
            }
            MirInstructionKind::MapRootRemove {
                result,
                source,
                key,
            } => {
                if instruction_touches_map_any_owner(function, instruction) {
                    self.validate_map_any_owner_instruction(function, instruction, subject);
                } else {
                    self.validate_map_root_type(function, result, subject, "Remove result");
                    self.validate_map_root_type(function, source, subject, "Remove source");
                    if key.contains('\0') {
                        self.error(format!("{subject} MapRoot key contains NUL"));
                    }
                }
            }
            MirInstructionKind::MapRootSize { result, root } => {
                if instruction_touches_map_any_owner(function, instruction) {
                    self.validate_map_any_owner_instruction(function, instruction, subject);
                } else {
                    self.validate_map_root_type(function, root, subject, "Size root");
                    self.validate_i32_value(function, result, subject, "MapRoot Size result");
                }
            }
            MirInstructionKind::MapRootContains { result, root, key } => {
                if instruction_touches_map_any_owner(function, instruction) {
                    self.validate_map_any_owner_instruction(function, instruction, subject);
                } else {
                    self.validate_map_root_type(function, root, subject, "Contains root");
                    self.validate_bool_value(function, result, subject, "MapRoot Contains result");
                    if key.contains('\0') {
                        self.error(format!("{subject} MapRoot key contains NUL"));
                    }
                }
            }
            MirInstructionKind::MapRootDrop { root } => {
                if instruction_touches_map_any_owner(function, instruction) {
                    self.validate_map_any_owner_instruction(function, instruction, subject);
                } else {
                    self.validate_map_root_type(function, root, subject, "Drop operand");
                }
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleNew { .. }
            | MirInstructionKind::OrdinaryMapOwnerLifecycleSet { .. }
            | MirInstructionKind::OrdinaryMapOwnerLifecycleRemove { .. }
            | MirInstructionKind::OrdinaryMapOwnerLifecycleSize { .. }
            | MirInstructionKind::OrdinaryMapOwnerLifecycleContains { .. }
            | MirInstructionKind::OrdinaryMapOwnerLifecycleDrop { .. } => {
                self.validate_ordinary_map_owner_instruction(function, instruction, subject);
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1New { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Set { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Remove { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Size { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Contains { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetI32 { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetString { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Drop { .. } => {
                self.validate_ordinary_map_owned_get_instruction(function, instruction, subject);
            }
            MirInstructionKind::Const { result, literal } => {
                let Some(ty) = value_type(function, result) else {
                    self.error(format!("{subject} constant result '{}' is absent", result));
                    return;
                };
                let supported = match literal {
                    ResolvedLiteral::Int(_) | ResolvedLiteral::Bool(_) => {
                        catalog.validate_copy_scalar(&ty).is_ok()
                    }
                    ResolvedLiteral::String(_) => catalog.validate_owned_string(&ty).is_ok(),
                    ResolvedLiteral::FloatBits(_) => {
                        catalog.validate_copy_float_scalar(&ty).is_ok()
                    }
                    ResolvedLiteral::Unit => false,
                };
                if !supported {
                    self.error(format!(
                        "{subject} literal {literal:?} is outside the verifier TypeDesc contract"
                    ));
                }
            }
            MirInstructionKind::Load { result, place } => {
                if place.projections.iter().any(|projection| {
                    matches!(
                        projection,
                        crate::core::ir::ResolvedProjection::Index { .. }
                    )
                }) {
                    self.error(format!(
                        "{subject} indexed Load is outside the verifier capability"
                    ));
                }
                if place.projections.iter().any(|projection| {
                    matches!(
                        projection,
                        crate::core::ir::ResolvedProjection::Deref { .. }
                    )
                }) {
                    self.error(format!(
                        "{subject} dereference Load is outside the verifier capability"
                    ));
                }
                let _ = result;
            }
            MirInstructionKind::Copy { result, source } => {
                self.require_same_type(function, result, source, subject);
                if let Some(ty) = value_type(function, source) {
                    if catalog.validate_copy_value(&ty).is_err() {
                        self.error(format!("{subject} Copy source '{}' is not Copy", source));
                    }
                }
            }
            MirInstructionKind::Move { result, source } => {
                self.require_same_type(function, result, source, subject);
                if let Some(ty) = value_type(function, source) {
                    let valid =
                        value_operation_is_supported(catalog, &ty, MirGlueOperation::MoveOut);
                    if !valid {
                        self.error(format!(
                            "{subject} Move source '{}' has no verifier glue",
                            source
                        ));
                    }
                }
            }
            MirInstructionKind::Clone { result, source } => {
                self.require_same_type(function, result, source, subject);
                if let Some(ty) = value_type(function, source) {
                    let valid = value_operation_is_supported(catalog, &ty, MirGlueOperation::Clone);
                    if !valid {
                        self.error(format!(
                            "{subject} Clone source '{}' has no verifier glue",
                            source
                        ));
                    }
                }
            }
            MirInstructionKind::Drop { value } => {
                if let Some(ty) = value_type(function, value) {
                    let valid = value_operation_is_supported(catalog, &ty, MirGlueOperation::Drop);
                    if !valid {
                        self.error(format!(
                            "{subject} Drop value '{}' has no verifier glue",
                            value
                        ));
                    }
                }
            }
            MirInstructionKind::Borrow {
                result,
                source,
                mutable,
                ..
            } => {
                let (Some(source_ty), Some(result_ty)) =
                    (value_type(function, source), value_type(function, result))
                else {
                    return;
                };
                if let Err(message) = catalog.validate_borrow(&source_ty, &result_ty, *mutable) {
                    self.error(format!("{subject} borrow rejected: {message}"));
                }
            }
            MirInstructionKind::EndBorrow { borrow } => {
                if let Some(ty) = value_type(function, borrow) {
                    if let Err(message) = catalog.validate_reference_type(&ty) {
                        self.error(format!("{subject} end-borrow rejected: {message}"));
                    }
                }
            }
            MirInstructionKind::Project {
                result,
                base,
                projection,
                list_index_contract,
            } => {
                let (Some(base_ty), Some(result_ty)) =
                    (value_type(function, base), value_type(function, result))
                else {
                    return;
                };
                match projection {
                    MirProjection::Index(index) => {
                        let Some(index_ty) = value_type(function, index) else {
                            return;
                        };
                        let Some(receipt) = list_index_contract else {
                            self.error(format!(
                                "{subject} List index projection has no canonical receipt"
                            ));
                            return;
                        };
                        if let Err(message) = catalog.validate_list_index_projection_receipt(
                            &base_ty, &index_ty, &result_ty, receipt,
                        ) {
                            self.error(format!("{subject} indexed projection rejected: {message}"));
                        }
                    }
                    MirProjection::Dereference => {
                        if let Err(message) = catalog.validate_dereference(&base_ty, &result_ty) {
                            self.error(format!("{subject} dereference rejected: {message}"));
                        }
                    }
                    MirProjection::ReadPath(receipt) => {
                        if list_index_contract.is_some() {
                            self.error(format!(
                                "{subject} List index receipt is attached to a read projection path"
                            ));
                        }
                        if let Err(message) =
                            catalog.validate_read_projection_receipt(&base_ty, &result_ty, receipt)
                        {
                            self.error(format!(
                                "{subject} read projection path rejected: {message}"
                            ));
                        }
                    }
                    MirProjection::Field(_) | MirProjection::Tuple(_) => {
                        if list_index_contract.is_some() {
                            self.error(format!(
                                "{subject} List index receipt is attached to a non-index projection"
                            ));
                            return;
                        }
                        if let Err(message) =
                            catalog.validate_projection(&base_ty, &result_ty, projection)
                        {
                            self.error(format!("{subject} projection rejected: {message}"));
                        }
                    }
                }
            }
            MirInstructionKind::MoveProject {
                result,
                base,
                projection,
            } => {
                let (Some(base_ty), Some(result_ty)) =
                    (value_type(function, base), value_type(function, result))
                else {
                    return;
                };
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_move_projection(&base_ty, &result_ty, projection)
                {
                    self.error(format!("{subject} MoveProject rejected: {message}"));
                }
            }
            MirInstructionKind::MoveProjectDrop {
                result,
                base,
                projection,
                contract,
            } => {
                let (Some(base_ty), Some(result_ty)) =
                    (value_type(function, base), value_type(function, result))
                else {
                    return;
                };
                let Some(receipt) = contract.as_ref() else {
                    self.error(format!(
                        "{subject} MoveProjectDrop has no canonical residual receipt"
                    ));
                    return;
                };
                if let Err(message) = catalog
                    .validate_record_move_projection_drop_receipt(&base_ty, &result_ty, receipt)
                {
                    self.error(format!("{subject} MoveProjectDrop rejected: {message}"));
                }
                if !matches!(projection, MirProjection::Field(_)) {
                    self.error(format!(
                        "{subject} MoveProjectDrop requires a direct record field projection"
                    ));
                }
            }
            MirInstructionKind::VariantProject {
                result,
                base,
                contract,
            } => {
                let (Some(base_ty), Some(result_ty)) =
                    (value_type(function, base), value_type(function, result))
                else {
                    return;
                };
                let Some(receipt) = contract.as_ref() else {
                    self.error(format!(
                        "{subject} direct variant projection has no canonical trap receipt"
                    ));
                    return;
                };
                let generic_result_copy_projection =
                    self.program.instances().values().any(|instance| {
                        instance.function == function.owner
                            && matches!(
                                &instance.contract,
                                MirGenericInstanceContract::ScalarVariantProjection {
                                    contract
                                } if contract.projection.nominal.as_str() == "builtin:type:Result"
                                    && contract.projection.ownership == MirOwnership::Copy
                            )
                    });
                let receipt_validation = if generic_result_copy_projection {
                    catalog.validate_generic_result_projection_trap_receipt(
                        &base_ty, &result_ty, receipt,
                    )
                } else {
                    catalog.validate_variant_projection_trap_receipt(&base_ty, &result_ty, receipt)
                };
                if let Err(message) = receipt_validation {
                    self.error(format!(
                        "{subject} direct variant projection rejected: {message}"
                    ));
                }
            }
            MirInstructionKind::VariantProjectOr {
                result,
                base,
                fallback,
                contract,
            } => {
                let (Some(base_ty), Some(result_ty), Some(fallback_ty)) = (
                    value_type(function, base),
                    value_type(function, result),
                    value_type(function, fallback),
                ) else {
                    return;
                };
                let Some(receipt) = contract.as_ref() else {
                    self.error(format!(
                        "{subject} variant projection fallback has no canonical receipt"
                    ));
                    return;
                };
                let generic_option_copy_fallback =
                    self.program.instances().values().any(|instance| {
                        instance.function == function.owner
                            && matches!(
                                &instance.contract,
                                MirGenericInstanceContract::ScalarVariantProjectionFallback {
                                    contract
                                } if contract.projection.nominal.as_str() == "builtin:type:Option"
                                    && contract.projection.ownership == MirOwnership::Copy
                            )
                    });
                let generic_result_copy_fallback =
                    self.program.instances().values().any(|instance| {
                        instance.function == function.owner
                            && matches!(
                                &instance.contract,
                                MirGenericInstanceContract::ScalarVariantProjectionFallback {
                                    contract
                                } if contract.projection.nominal.as_str() == "builtin:type:Result"
                                    && contract.projection.ownership == MirOwnership::Copy
                            )
                    });
                let receipt_validation = if generic_option_copy_fallback {
                    catalog
                        .validated_copy_option_generic_projection_fallback_contract(
                            &base_ty,
                            &receipt.projection.variant,
                            &receipt.projection.field,
                            &result_ty,
                            &fallback_ty,
                        )
                        .map(|_| ())
                } else if generic_result_copy_fallback
                    && catalog.get(&base_ty).is_some_and(|descriptor| {
                        matches!(
                            &descriptor.layout,
                            MirLayout::Result { ok, error, .. }
                                if ok == &result_ty && ok != error
                        )
                    })
                {
                    catalog
                        .validated_generic_result_scalar_projection_fallback_contract(
                            &base_ty,
                            &receipt.projection.variant,
                            &receipt.projection.field,
                            &result_ty,
                            &fallback_ty,
                        )
                        .map(|_| ())
                } else {
                    catalog.validate_variant_projection_fallback_receipt(
                        &base_ty,
                        &result_ty,
                        &fallback_ty,
                        receipt,
                    )
                };
                if let Err(message) = receipt_validation {
                    self.error(format!(
                        "{subject} variant projection fallback rejected: {message}"
                    ));
                }
            }
            MirInstructionKind::VariantProjectMove {
                result,
                base,
                contract,
            } => {
                let (Some(base_ty), Some(result_ty)) =
                    (value_type(function, base), value_type(function, result))
                else {
                    return;
                };
                let Some(receipt) = contract.as_ref() else {
                    self.error(format!(
                        "{subject} consuming direct variant projection has no canonical move receipt"
                    ));
                    return;
                };
                if let Err(message) = catalog
                    .validate_variant_move_projection_trap_receipt(&base_ty, &result_ty, receipt)
                {
                    self.error(format!(
                        "{subject} consuming direct variant projection rejected: {message}"
                    ));
                }
            }
            MirInstructionKind::Construct {
                result,
                kind,
                fields,
            } => {
                let Some(result_ty) = value_type(function, result) else {
                    return;
                };
                let field_types = fields
                    .iter()
                    .filter_map(|value| value_type(function, value))
                    .collect::<Vec<_>>();
                if field_types.len() != fields.len() {
                    self.error(format!("{subject} aggregate field is absent"));
                } else if let Err(message) =
                    catalog.validate_aggregate(&result_ty, kind, &field_types)
                {
                    self.error(format!("{subject} aggregate rejected: {message}"));
                }
            }
            MirInstructionKind::ConstructList {
                result,
                elements,
                list_construct_contract,
            } => {
                let Some(result_ty) = value_type(function, result) else {
                    return;
                };
                let element_types = elements
                    .iter()
                    .filter_map(|value| value_type(function, value))
                    .collect::<Vec<_>>();
                if element_types.len() != elements.len() {
                    self.error(format!("{subject} List element is absent"));
                } else if let Some(receipt) = list_construct_contract.as_ref() {
                    if let Err(message) =
                        catalog.validate_list_construct_receipt(&result_ty, &element_types, receipt)
                    {
                        self.error(format!("{subject} List construction rejected: {message}"));
                    }
                } else {
                    self.error(format!(
                        "{subject} List construction has no canonical receipt"
                    ));
                }
            }
            MirInstructionKind::ListOp {
                result,
                operation,
                list,
                argument,
                list_operation_contract,
            } => {
                let (Some(result_ty), Some(list_ty)) =
                    (value_type(function, result), value_type(function, list))
                else {
                    return;
                };
                let Some(receipt) = list_operation_contract.as_ref() else {
                    self.error(format!("{subject} List operation has no canonical receipt"));
                    return;
                };
                let argument_ty = argument
                    .as_ref()
                    .and_then(|value| function.values.get(value))
                    .map(|value| value.ty.clone());
                if let Err(message) = catalog.validate_list_operation_receipt_with_argument(
                    &result_ty,
                    &list_ty,
                    argument_ty.as_ref(),
                    *operation,
                    receipt,
                ) {
                    self.error(format!("{subject} List operation rejected: {message}"));
                }
            }
            MirInstructionKind::ConstructSet { result, elements } => {
                let Some(result_ty) = value_type(function, result) else {
                    return;
                };
                let element_types = elements
                    .iter()
                    .filter_map(|value| value_type(function, value))
                    .collect::<Vec<_>>();
                if element_types.len() != elements.len() {
                    self.error(format!("{subject} Set element is absent"));
                } else if let Err(message) =
                    catalog.validate_set_construct(&result_ty, &element_types)
                {
                    self.error(format!("{subject} Set construction rejected: {message}"));
                }
            }
            MirInstructionKind::SetOp {
                result,
                operation,
                set,
                argument,
            } => {
                let (Some(result_ty), Some(set_ty)) =
                    (value_type(function, result), value_type(function, set))
                else {
                    return;
                };
                let argument_ty = argument
                    .as_ref()
                    .and_then(|value| value_type(function, value));
                if argument.is_some() && argument_ty.is_none() {
                    self.error(format!("{subject} Set argument is absent"));
                } else if let Err(message) = catalog.validate_set_operation(
                    &result_ty,
                    &set_ty,
                    argument_ty.as_ref(),
                    *operation,
                ) {
                    self.error(format!("{subject} Set operation rejected: {message}"));
                }
            }
            MirInstructionKind::ConstructVariant {
                result,
                nominal,
                variant,
                fields,
            } => {
                let Some(result_ty) = value_type(function, result) else {
                    return;
                };
                let field_ids = fields
                    .iter()
                    .map(|(field, _)| field.clone())
                    .collect::<Vec<_>>();
                let field_types = fields
                    .iter()
                    .filter_map(|(_, value)| value_type(function, value))
                    .collect::<Vec<_>>();
                if field_types.len() != fields.len() {
                    self.error(format!("{subject} variant payload is absent"));
                } else if let Err(message) = catalog.validate_variant_construct(
                    &result_ty,
                    nominal,
                    variant,
                    &field_ids,
                    &field_types,
                ) {
                    self.error(format!(
                        "{subject} variant construction rejected: {message}"
                    ));
                } else if catalog.get(&result_ty).map_or(true, |descriptor| {
                    descriptor.ownership != MirOwnership::Copy
                }) {
                    self.error(format!("{subject} non-Copy variant construction is outside the verifier capability"));
                }
            }
            MirInstructionKind::ConstructVariantMove {
                result,
                nominal,
                variant,
                fields,
            } => {
                let Some(result_ty) = value_type(function, result) else {
                    self.error(format!("{subject} variant result is absent"));
                    return;
                };
                let move_variant = catalog
                    .validate_multi_target_union_variant(&result_ty)
                    .map(|_| ())
                    .or_else(|_| {
                        catalog.validate_option_move_variant(&result_ty).map(|_| ())
                    })
                    .or_else(|_| {
                        if self.allow_recoverable_flow_result {
                            catalog.validate_recoverable_result_variant(&result_ty)
                        } else {
                            Err("recoverable Flow Result construction is outside this verifier profile".into())
                        }
                    })
                    .or_else(|_| {
                        if self.allow_nested_option_tuple_variant {
                            catalog
                                .validate_option_nested_tuple_variant(&result_ty)
                                .map(|_| ())
                        } else {
                            Err("nested Option tuple construction requires the explicit projection receipt".into())
                        }
                    })
                    .or_else(|_| {
                        if self
                            .ordinary_map_owned_get_result_string_types
                            .contains(&result_ty)
                        {
                            catalog.validate_result_string_i32_variant(&result_ty)
                        } else {
                            Err("owned Map getter Result construction is outside this profile".into())
                        }
                    })
                    .or_else(|_| {
                        if self.allow_result_move_variant {
                            catalog
                                .validate_result_move_projection_variant(&result_ty)
                                .map(|_| ())
                        } else {
                            Err("move-owned Result construction requires the promoted generic Result projection receipt".into())
                        }
                    });
                if let Err(message) = move_variant {
                    self.error(format!(
                        "{subject} ConstructVariantMove rejected: {message}"
                    ));
                    return;
                }
                let field_ids = fields
                    .iter()
                    .map(|(field, _)| field.clone())
                    .collect::<Vec<_>>();
                let field_types = fields
                    .iter()
                    .filter_map(|(_, value)| value_type(function, value))
                    .collect::<Vec<_>>();
                if field_types.len() != fields.len() {
                    self.error(format!("{subject} variant payload is absent"));
                } else if let Err(message) = catalog.validate_variant_construct(
                    &result_ty,
                    nominal,
                    variant,
                    &field_ids,
                    &field_types,
                ) {
                    self.error(format!(
                        "{subject} ConstructVariantMove rejected: {message}"
                    ));
                }
            }
            MirInstructionKind::UpdateRecord {
                result,
                base,
                kind,
                fields,
                record_update_contract,
                record_update_move_contract,
            } => {
                let (Some(result_ty), Some(base_ty)) =
                    (value_type(function, result), value_type(function, base))
                else {
                    return;
                };
                let field_types = fields
                    .iter()
                    .filter_map(|value| value_type(function, value))
                    .collect::<Vec<_>>();
                if field_types.len() != fields.len() {
                    self.error(format!("{subject} record update field is absent"));
                } else {
                    // The shape-only validator is insufficient for an owned
                    // aggregate: without a move receipt, consumers cannot
                    // know whether the base is transferred or cloned. Keep
                    // this proof boundary closed even when the ordinary MIR
                    // builder intentionally preserves the operation.
                    if record_update_move_contract.is_none()
                        && catalog
                            .get(&result_ty)
                            .is_some_and(|descriptor| descriptor.ownership != MirOwnership::Copy)
                    {
                        self.error(format!(
                            "{subject} non-Copy record update requires a validated move receipt"
                        ));
                    }
                    let validation = if let Some(receipt) = record_update_move_contract {
                        catalog.validate_record_update_move_receipt(
                            &result_ty,
                            &base_ty,
                            kind,
                            &field_types,
                            receipt,
                        )
                    } else if let Some(receipt) = record_update_contract {
                        catalog.validate_record_update_receipt(
                            &result_ty,
                            &base_ty,
                            kind,
                            &field_types,
                            receipt,
                        )
                    } else {
                        catalog.validate_record_update(&result_ty, &base_ty, kind, &field_types)
                    };
                    if let Err(message) = validation {
                        self.error(format!("{subject} record update rejected: {message}"));
                    }
                }
            }
            MirInstructionKind::Binary {
                result,
                op,
                left,
                right,
            } => {
                self.validate_binary(function, result, *op, left, right, subject);
            }
            MirInstructionKind::Unary {
                result,
                op,
                operand,
            } => {
                self.require_same_type_if_unary(function, result, operand, *op, subject);
            }
            MirInstructionKind::BuiltinCall {
                kind,
                arguments,
                string_field_contract,
                ..
            } => {
                if !matches!(
                    kind,
                    crate::core::mir::types::MirBuiltinKind::Abs
                        | crate::core::mir::types::MirBuiltinKind::Min
                        | crate::core::mir::types::MirBuiltinKind::Max
                        | crate::core::mir::types::MirBuiltinKind::PrintlnBool
                        | crate::core::mir::types::MirBuiltinKind::PrintlnInt
                        | crate::core::mir::types::MirBuiltinKind::PrintlnString
                        | crate::core::mir::types::MirBuiltinKind::PrintlnFloat
                        | crate::core::mir::types::MirBuiltinKind::SessionOpen
                        | crate::core::mir::types::MirBuiltinKind::SessionPair
                ) {
                    self.error(format!(
                        "{subject} builtin is outside the verifier capability"
                    ));
                }
                if let Some(receipt) = string_field_contract {
                    if *kind != crate::core::mir::types::MirBuiltinKind::PrintlnString {
                        self.error(format!(
                            "{subject} String-field borrow receipt is attached to a non-String builtin"
                        ));
                    } else if arguments.len() != 1 {
                        self.error(format!(
                            "{subject} String-field borrow println requires one record source"
                        ));
                    } else if let Some(source_ty) = value_type(function, &arguments[0]) {
                        if let Err(message) =
                            catalog.validate_string_field_borrow_receipt(&source_ty, receipt)
                        {
                            self.error(format!(
                                "{subject} String-field borrow receipt rejected: {message}"
                            ));
                        }
                    }
                }
            }
            MirInstructionKind::SessionCall {
                result,
                operation,
                endpoint,
                payload,
                contract,
            } => {
                let (Some(endpoint_ty), Some(result_ty)) =
                    (value_type(function, endpoint), value_type(function, result))
                else {
                    return;
                };
                let Some(receipt) = contract.as_ref() else {
                    self.error(format!(
                        "{subject} SessionCall has no canonical residual/ABI receipt"
                    ));
                    return;
                };
                if let Err(message) =
                    catalog.validate_session_call_contract(&endpoint_ty, &result_ty, receipt)
                {
                    self.error(format!("{subject} SessionCall rejected: {message}"));
                }
                if payload.is_some() != receipt.payload_ty.is_some() {
                    self.error(format!(
                        "{subject} SessionCall payload disagrees with its receipt TypeDesc"
                    ));
                }
                if let Some(payload) = payload {
                    if let (Some(payload_ty), Some(receipt_ty)) =
                        (value_type(function, payload), receipt.payload_ty.as_ref())
                    {
                        if payload_ty != *receipt_ty {
                            self.error(format!(
                                "{subject} SessionCall payload value disagrees with its receipt TypeDesc identity"
                            ));
                        }
                    }
                }
                if receipt.operation != *operation {
                    self.error(format!(
                        "{subject} SessionCall receipt disagrees with MIR operation"
                    ));
                }
            }
            MirInstructionKind::SessionPairBind { lo, hi, contract } => {
                let (Some(lo_ty), Some(hi_ty)) =
                    (value_type(function, lo), value_type(function, hi))
                else {
                    return;
                };
                let Some(receipt) = contract.as_ref() else {
                    self.error(format!(
                        "{subject} typed session_pair binding has no canonical receipt"
                    ));
                    return;
                };
                if let Err(message) = catalog.validate_session_pair_bind_receipt(
                    &receipt.pair_ty,
                    &lo_ty,
                    &hi_ty,
                    receipt,
                ) {
                    self.error(format!(
                        "{subject} typed session_pair binding rejected: {message}"
                    ));
                }
            }
            MirInstructionKind::VariantPredicate {
                result,
                predicate,
                variant,
                contract,
            } => {
                let (Some(result_ty), Some(variant_ty)) =
                    (value_type(function, result), value_type(function, variant))
                else {
                    return;
                };
                let Some(receipt) = contract.as_ref() else {
                    self.error(format!(
                        "{subject} variant predicate has no canonical receipt"
                    ));
                    return;
                };
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_variant_predicate_receipt(
                        &result_ty,
                        &variant_ty,
                        *predicate,
                        receipt,
                    )
                {
                    self.error(format!("{subject} variant predicate rejected: {message}"));
                }
            }
            MirInstructionKind::Convert { result, source } => {
                let (Some(source_ty), Some(result_ty)) =
                    (value_type(function, source), value_type(function, result))
                else {
                    return;
                };
                match catalog.validate_conversion(&source_ty, &result_ty) {
                    Ok(contract)
                        if matches!(
                            contract.kind,
                            crate::core::mir::types::MirConversionKind::ScalarIdentity
                                | crate::core::mir::types::MirConversionKind::SignedI32ToI64
                                | crate::core::mir::types::MirConversionKind::SignedI32ToFloat64
                                | crate::core::mir::types::MirConversionKind::SignedI64ToFloat64
                                | crate::core::mir::types::MirConversionKind::Float64ToFloat32
                        ) => {}
                    Ok(_) => self.error(format!(
                        "{subject} conversion is outside the verifier capability"
                    )),
                    Err(message) => self.error(format!("{subject} conversion rejected: {message}")),
                }
            }
            MirInstructionKind::Call {
                result,
                callee,
                type_arguments,
                arguments,
                variant_call_contract,
                ..
            } => {
                self.validate_call(
                    function,
                    instruction_id,
                    result.as_ref(),
                    callee,
                    type_arguments,
                    arguments,
                    variant_call_contract.as_ref(),
                    subject,
                );
            }
            MirInstructionKind::ClosureBind {
                closure_local,
                function_value,
                abi,
                ..
            } => {
                // The bind defines only a Copy function-TypeDesc identity;
                // no runtime closure pointer is created.
                if let Err(message) = super::mir::validate_closure_callable_value(
                    self.program,
                    function,
                    closure_local,
                    function_value,
                    &abi.source_function_type,
                    abi.source_abi,
                    abi.source_parameter_types.len(),
                ) {
                    self.error(format!("{subject} closure bind callable value rejected: {message}"));
                }
            }
            MirInstructionKind::ClosureInvoke {
                closure_local,
                function_value,
                abi,
                result,
                callee,
                type_arguments,
                arguments,
            } => {
                if let Err(message) = super::mir::validate_closure_callable_value(
                    self.program,
                    function,
                    closure_local,
                    function_value,
                    &abi.source_function_type,
                    abi.source_abi,
                    abi.source_parameter_types.len(),
                ) {
                    self.error(format!("{subject} closure invoke callable value rejected: {message}"));
                }
                let direct = ResolvedCallee::Function(callee.clone());
                for argument in arguments {
                    if value_type(function, argument).is_some_and(|ty| {
                        self.program.type_catalog().validate_session_channel(&ty).is_ok()
                    }) {
                        self.error(format!("{subject} ClosureInvoke has no TransferSession effect receipt"));
                    }
                }
                self.validate_call(
                    function,
                    instruction_id,
                    result.as_ref(),
                    &direct,
                    type_arguments,
                    arguments,
                    None,
                    subject,
                );
            }
            MirInstructionKind::FlowTransition {
                result,
                transition,
                arguments,
                effect_receipt,
            } => self.validate_flow_transition(
                function,
                result,
                transition,
                arguments,
                effect_receipt.as_ref(),
                subject,
            ),
            MirInstructionKind::Nop => {}
        }
    }

    fn validate_flow_transition(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        transition: &crate::core::NodeId,
        arguments: &[MirValueId],
        effect_receipt: Option<&crate::core::mir::types::MirFlowEffectReceipt>,
        subject: &str,
    ) {
        let Some(contract) = self.program.transitions().get(transition) else {
            self.error(format!(
                "{subject} FlowTransition '{}' has no canonical contract",
                transition.0
            ));
            return;
        };
        let recoverable = contract.effect.is_recoverable();
        let union =
            crate::core::mir::multi_target_union_shape(contract, self.program.type_catalog());
        if (!recoverable
            && !matches!(
                contract.effect,
                crate::core::mir::MirTransitionEffect::SilentLocal
                    | crate::core::mir::MirTransitionEffect::Boundary
            ))
            || (contract.targets.len() != 1 && !union)
            || (!recoverable && contract.failure.is_some())
            || contract.is_fallback
            || contract.is_ffi_pinned
            || (recoverable && contract.failure.is_none())
        {
            self.error(format!(
                "{subject} FlowTransition is outside the silent-local/recoverable transition capability"
            ));
        }
        let argument_types = arguments
            .iter()
            .filter_map(|argument| function.values.get(argument).map(|value| value.ty.clone()))
            .collect::<Vec<_>>();
        if argument_types.len() == arguments.len() {
            let result_ty = function
                .values
                .get(result)
                .map(|value| value.ty.clone())
                .unwrap_or_else(|| contract.result.clone());
            if let Err(message) = crate::core::mir::validate_flow_effect_receipt(
                transition,
                contract,
                &argument_types,
                &result_ty,
                effect_receipt,
                self.program.type_catalog(),
            ) {
                self.error(format!("{subject} {message}"));
            }
        }
        let Some(target) = self.program.functions().get(&contract.owner) else {
            self.error(format!(
                "{subject} FlowTransition target '{}' is absent",
                contract.owner.0
            ));
            return;
        };
        if arguments.len() != target.parameters.len() {
            self.error(format!(
                "{subject} FlowTransition argument arity disagrees with its canonical body"
            ));
        }
        for (argument, parameter) in arguments.iter().zip(&target.parameters) {
            if value_type(function, argument) != value_type(target, parameter) {
                self.error(format!(
                    "{subject} FlowTransition argument TypeDesc disagrees with its canonical body"
                ));
            }
        }
        match value_type(function, result) {
            Some(actual)
                if actual == contract.result
                    && (recoverable || actual == target.result)
                    && (!recoverable || target.result == contract.result) => {}
            _ => self.error(format!(
                "{subject} FlowTransition result TypeDesc disagrees with its canonical contract"
            )),
        }
    }

    fn validate_call(
        &mut self,
        function: &MirFunction,
        instruction_id: &crate::core::mir::MirInstructionId,
        result: Option<&MirValueId>,
        callee: &ResolvedCallee,
        type_arguments: &[crate::core::ResolvedTypeId],
        arguments: &[MirValueId],
        variant_call_contract: Option<&crate::core::mir::types::MirVariantCallAbiContract>,
        subject: &str,
    ) {
        if let ResolvedCallee::Extern(owner) = callee {
            let Some(contract) = self.program.ffi_calls().get(instruction_id) else {
                self.error(format!(
                    "{subject} extern callee '{}' has no canonical FFI contract",
                    owner.0
                ));
                return;
            };
            let receipt_errors = crate::core::mir::validate_ffi_call_contract_receipt(
                self.program.type_catalog(),
                function,
                instruction_id,
                owner,
                result,
                arguments,
                contract,
            );
            for message in &receipt_errors {
                self.error(format!("{subject} {message}"));
            }
            if !receipt_errors.is_empty() {
                return;
            }
            if result.is_none() {
                self.error(format!(
                    "{subject} extern call has no canonical result value identity"
                ));
            }
            return;
        }
        let owner = match callee {
            ResolvedCallee::Function(owner) => owner.clone(),
            ResolvedCallee::ProtocolMethod { .. } => {
                if let Err(message) = crate::core::mir::validate_protocol_method_identity(callee) {
                    self.error(format!("{subject} {message}"));
                    return;
                }
                let Some(owner) = crate::core::mir::canonical_protocol_call_target(callee) else {
                    self.error(format!(
                        "{subject} callee is outside the canonical MIR verifier capability"
                    ));
                    return;
                };
                owner
            }
            _ => {
                self.error(format!(
                    "{subject} callee is outside the canonical MIR verifier capability"
                ));
                return;
            }
        };
        let Some(target) = self.program.functions().get(&owner) else {
            self.error(format!("{subject} callee '{}' is absent", owner.0));
            return;
        };
        let instance = self
            .program
            .instances()
            .values()
            .find(|instance| instance.function == owner);
        if let Some(instance) = instance {
            let managed_any_crossing = crate::verifier::mir::direct_call_uses_managed_any(
                target,
                self.program.type_catalog(),
            ) || type_arguments.iter().any(|ty| {
                self.program
                    .type_catalog()
                    .contains_dynamic_or_managed_any(ty)
            });
            if managed_any_crossing
                && !matches!(
                    &instance.contract,
                    MirGenericInstanceContract::ManagedAnyIdentity
                        | MirGenericInstanceContract::ManagedAnyTrapCleanup { .. }
                )
            {
                self.error(format!(
                    "{subject} ManagedAny cannot cross an unreceipted generic instance ABI"
                ));
                return;
            }
            if instance.arguments != type_arguments {
                self.error(format!(
                    "{subject} generic arguments disagree with instance '{}'",
                    instance.id
                ));
                return;
            }
            match instance.contract {
                MirGenericInstanceContract::ScalarIdentity
                | MirGenericInstanceContract::OwnedStringIdentity
                | MirGenericInstanceContract::ScalarSetFacade { .. }
                | MirGenericInstanceContract::ScalarListFacade { .. }
                | MirGenericInstanceContract::ScalarListConstruct { .. }
                | MirGenericInstanceContract::ScalarListProjection { .. }
                | MirGenericInstanceContract::ScalarRecordProjection { .. }
                | MirGenericInstanceContract::ScalarRecordUpdate { .. }
                | MirGenericInstanceContract::ScalarTupleProjection { .. }
                | MirGenericInstanceContract::OwnedRecordProjection { .. }
                | MirGenericInstanceContract::OwnedRecordProjectionDrop { .. }
                | MirGenericInstanceContract::OwnedRecordUpdate { .. }
                | MirGenericInstanceContract::ScalarVariantPredicate { .. }
                | MirGenericInstanceContract::ScalarVariantProjection { .. }
                | MirGenericInstanceContract::ScalarVariantProjectionFallback { .. } => {}
                MirGenericInstanceContract::ManagedAnyIdentity => {
                    if let Err(message) = self
                        .program
                        .type_catalog()
                        .validate_managed_any_identity_instance(instance, target)
                    {
                        self.error(format!(
                            "{subject} ManagedAny identity instance receipt rejected: {message}"
                        ));
                        return;
                    }
                }
                MirGenericInstanceContract::ManagedAnyTrapCleanup { .. } => {
                    if let Err(message) = self
                        .program
                        .type_catalog()
                        .validate_managed_any_trap_cleanup_instance(instance, target)
                    {
                        self.error(format!(
                            "{subject} ManagedAny trap-cleanup instance receipt rejected: {message}"
                        ));
                        return;
                    }
                }
            }
        } else if !type_arguments.is_empty() {
            self.error(format!(
                "{subject} generic arguments target a non-instance function"
            ));
            return;
        } else if function_has_ensures(function)
            && !variant_call_contract.is_some_and(|receipt| {
                receipt.mode == crate::core::mir::types::MirVariantCallAbiMode::MoveOwned
                    && result
                        .and_then(|result| function.values.get(result))
                        .is_some_and(|value| {
                            self.program
                                .type_catalog()
                                .get(&value.ty)
                                .is_some_and(|descriptor| descriptor.kind == MirTypeKind::Result)
                        })
            })
            // R6-1073: unless the call routes to the direct-scalar evaluator.
            // `eval_direct_scalar_call` symbolically executes the callee body
            // (explore_block, merged return paths), propagates the callee's
            // trap and FFI obligations into this function's accounting, and
            // rejects recursion, arity drift and out-of-ABI types itself — so
            // a Copy-scalar (or unit) call whose graph cannot reach back into
            // this function is exactly modeled even here.  The gate mirrors
            // the evaluator's own routing preconditions and admits nothing
            // the evaluator would not route: aggregate/String callees keep
            // the registered floor.
            && !self.call_routes_to_direct_scalar_evaluation(function, target, result)
            && !self.call_routes_to_direct_owned_string_evaluation(function, target, result)
            && !self.call_routes_to_direct_managed_any_evaluation(function, target, result)
            && !self.call_routes_to_managed_any_trap_cleanup_evaluation(
                function,
                target,
                instance,
                type_arguments,
                result,
            )
        {
            self.error(format!(
                "{subject} ordinary call in a contract-bearing function is outside the verifier capability"
            ));
            return;
        }
        if arguments.len() != target.parameters.len() {
            self.error(format!("{subject} call arity disagrees with callee"));
        }
        for (argument, parameter) in arguments.iter().zip(&target.parameters) {
            if value_type(function, argument) != value_type(target, parameter) {
                self.error(format!(
                    "{subject} call argument TypeDesc disagrees with callee"
                ));
            }
        }
        match (result, self.program.type_catalog().get(&target.result)) {
            (Some(result), Some(_))
                if value_type(function, result).as_ref() != Some(&target.result) =>
            {
                self.error(format!(
                    "{subject} call result TypeDesc disagrees with callee"
                ));
            }
            (None, Some(descriptor)) if !descriptor.is_canonical_ffi_unit() => {
                self.error(format!("{subject} non-unit call has no result value"));
            }
            _ => {}
        }
    }

    fn call_routes_to_direct_managed_any_evaluation(
        &self,
        function: &MirFunction,
        target: &MirFunction,
        result: Option<&MirValueId>,
    ) -> bool {
        let catalog = self.program.type_catalog();
        crate::verifier::mir::direct_call_uses_managed_any(target, catalog)
            && target.validate().is_ok()
            && catalog.validate_managed_any_function(target).is_ok()
            && target.blocks.len() == 1
            && crate::verifier::mir::is_direct_managed_any_call_type(catalog, &target.result)
            && target.parameters.iter().all(|parameter| {
                value_type(target, parameter).is_some_and(|ty| {
                    crate::verifier::mir::is_direct_managed_any_call_type(catalog, &ty)
                })
            })
            && !crate::verifier::mir::direct_call_graph_reaches(
                self.program,
                &target.owner,
                &function.owner,
                &mut BTreeSet::new(),
            )
            && (result.is_some()
                || catalog
                    .get(&target.result)
                    .is_some_and(|desc| desc.is_canonical_ffi_unit()))
    }

    /// A generic ManagedAny cleanup call is admitted here only for its exact
    /// sealed trap profile. Its evaluator records the sealed E0801 or E0802 trap
    /// and terminates the normal path, so the structural i32 return cannot be
    /// mistaken for a usable value in a contract-bearing caller.
    fn call_routes_to_managed_any_trap_cleanup_evaluation(
        &self,
        function: &MirFunction,
        target: &MirFunction,
        instance: Option<&MirInstance>,
        type_arguments: &[crate::core::ResolvedTypeId],
        result: Option<&MirValueId>,
    ) -> bool {
        let Some(instance) = instance else {
            return false;
        };
        if !matches!(
            &instance.contract,
            MirGenericInstanceContract::ManagedAnyTrapCleanup { .. }
        ) {
            return false;
        }
        instance.function == target.owner
            && instance.arguments == type_arguments
            && self
                .program
                .type_catalog()
                .validate_managed_any_trap_cleanup_instance(instance, target)
                .is_ok()
            && !crate::verifier::mir::direct_call_graph_reaches(
                self.program,
                &target.owner,
                &function.owner,
                &mut BTreeSet::new(),
            )
            && result.is_some()
    }

    /// R6-1073: mirror of the verifier's direct-scalar call routing.  The
    /// evaluator routes a non-instance call with no type arguments to
    /// `eval_direct_scalar_call` exactly when the callee result is inside
    /// the Copy scalar ABI (or unit) — and that evaluator refuses recursion
    /// and unit-result mismatches itself.  The gate re-checks the same
    /// preconditions so it admits only calls the evaluator actually routes,
    /// keeping the capability gate and the symbolic engine aligned instead
    /// of fail-closed against something the engine models.
    fn call_routes_to_direct_scalar_evaluation(
        &self,
        function: &MirFunction,
        target: &MirFunction,
        result: Option<&MirValueId>,
    ) -> bool {
        let catalog = self.program.type_catalog();
        !crate::verifier::mir::direct_call_uses_managed_any(target, catalog)
            && crate::verifier::mir::is_direct_scalar_call_type(catalog, &target.result)
            && !crate::verifier::mir::direct_call_graph_reaches(
                self.program,
                &target.owner,
                &function.owner,
                &mut BTreeSet::new(),
            )
            && (result.is_some()
                || catalog
                    .get(&target.result)
                    .is_some_and(|descriptor| descriptor.is_canonical_ffi_unit()))
    }

    /// R6-1074: mirror of the verifier's direct owned-String call routing.
    /// `eval_direct_owned_string_call` validates the owned-String return
    /// shape, consumes non-Copy arguments with MoveOut glue, executes the
    /// callee body, and requires exactly one trap-free return path — so an
    /// owned-String callee is exactly modeled in a contract-bearing
    /// function too.  R6-1077: the return-shape ledger now composes one-edge
    /// wrappers (`wrap() { greet() }`) whose callee candidacy it re-proves
    /// recursively, so this mirror follows automatically.  The recursion
    /// guard is load-bearing here (unlike the scalar route, whose evaluator
    /// re-checks it): the checker admits recursive bodies, and without the
    /// guard a contract-bearing caller of a recursive String callee would
    /// send the evaluator into unbounded exploration — the gate keeps that
    /// shape fail-closed.
    fn call_routes_to_direct_owned_string_evaluation(
        &self,
        function: &MirFunction,
        target: &MirFunction,
        result: Option<&MirValueId>,
    ) -> bool {
        let catalog = self.program.type_catalog();
        !crate::verifier::mir::direct_call_uses_managed_any(target, catalog)
            && catalog.validate_owned_string(&target.result).is_ok()
            && crate::core::mir::validate_owned_string_return_shape(
                target,
                self.program.functions(),
                catalog,
            )
            .is_ok()
            && !crate::verifier::mir::direct_call_graph_reaches(
                self.program,
                &target.owner,
                &function.owner,
                &mut BTreeSet::new(),
            )
            && result.is_some()
    }

    fn validate_identity_instance(&mut self, function: &MirFunction, subject: &str) {
        let [parameter] = function.parameters.as_slice() else {
            self.error(format!(
                "instance '{}' identity body has an invalid signature",
                subject
            ));
            return;
        };
        if value_type(function, parameter).as_ref() != Some(&function.result) {
            self.error(format!(
                "instance '{}' identity body has an invalid signature",
                subject
            ));
        }
    }

    fn validate_binary(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        op: ResolvedBinaryOp,
        left: &MirValueId,
        right: &MirValueId,
        subject: &str,
    ) {
        let (Some(result_ty), Some(left_ty), Some(right_ty)) = (
            value_type(function, result),
            value_type(function, left),
            value_type(function, right),
        ) else {
            return;
        };
        let catalog = self.program.type_catalog();
        // The ordinary Map/Any snapshot equality face deliberately keeps the
        // generation-bearing Any owner as one operand and the borrowed String
        // peer as the other.  Their TypeDesc identities are therefore
        // different by construction; the lowerer brands this exact
        // instruction as `binary.managed_any_string_read`, and the ManagedAny
        // owner ledger/Checker seal validates the owner side and its cleanup.
        // Do this narrow check before the scalar same-TypeDesc rule instead
        // of erasing the owner into a guessed String descriptor.
        let managed_any_string_read = subject.contains("binary.managed_any_string_read")
            && matches!(op, ResolvedBinaryOp::Equal | ResolvedBinaryOp::NotEqual)
            && catalog_is_managed_any_string_pair(catalog, &left_ty, &right_ty)
            && catalog.get(&result_ty).is_some_and(|descriptor| {
                descriptor.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::Bool)
                    && descriptor.is_canonical_copy_scalar(false)
            });
        if managed_any_string_read {
            return;
        }
        if left_ty != right_ty {
            self.error(format!(
                "{subject} binary operands have different TypeDesc identities"
            ));
            return;
        }
        let snapshot_string_operation = (function
            .ownership
            .has_checker_ordinary_map_owned_get_i32_string_v1_actions()
            || crate::verifier::mir::direct_call_uses_managed_any(function, catalog))
            && catalog.validate_owned_string(&left_ty).is_ok();
        if snapshot_string_operation {
            let valid = match op {
                ResolvedBinaryOp::Add => catalog.validate_owned_string(&result_ty).is_ok(),
                ResolvedBinaryOp::Equal | ResolvedBinaryOp::NotEqual => {
                    catalog.get(&result_ty).is_some_and(|desc| {
                        desc.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::Bool)
                            && catalog.validate_copy_scalar(&result_ty).is_ok()
                    })
                }
                _ => false,
            };
            if !valid {
                self.error(format!(
                    "{subject} String operator is outside the exact snapshot proof domain"
                ));
            }
            return;
        }
        let scalar = self.program.type_catalog().get(&left_ty);
        let Some(descriptor) = scalar else {
            return;
        };
        if !descriptor.is_canonical_copy_scalar(true) {
            self.error(format!(
                "{subject} binary operand TypeDesc is outside the complete Copy scalar contract"
            ));
            return;
        }
        let result_descriptor = self.program.type_catalog().get(&result_ty);
        let right_descriptor = self.program.type_catalog().get(&right_ty);
        let float_shape = [Some(descriptor), right_descriptor, result_descriptor]
            .iter()
            .flatten()
            .any(|descriptor| {
                descriptor.is_canonical_copy_scalar(true)
                    && matches!(descriptor.abi, MirAbiClass::Float { bits: 32 | 64 })
            });
        if float_shape {
            if let Err(message) = self
                .program
                .type_catalog()
                .validate_copy_float_binary(&result_ty, &left_ty, &right_ty, op)
            {
                self.error(format!("{subject} float binary rejected: {message}"));
            }
            return;
        }
        let valid = match descriptor.abi {
            MirAbiClass::Integer {
                bits: 32 | 64,
                signed: true,
            } => matches!(
                op,
                ResolvedBinaryOp::Add
                    | ResolvedBinaryOp::Subtract
                    | ResolvedBinaryOp::Multiply
                    | ResolvedBinaryOp::Divide
                    | ResolvedBinaryOp::Remainder
                    // Power belongs only to the signed i32/i64 Copy arm. The
                    // typed proof term uses the explicit VM/native checked-i64
                    // profile, wrapping only successful i32 results.
                    | ResolvedBinaryOp::Power
                    | ResolvedBinaryOp::Equal
                    | ResolvedBinaryOp::NotEqual
                    | ResolvedBinaryOp::Less
                    | ResolvedBinaryOp::Greater
                    | ResolvedBinaryOp::LessEqual
                    | ResolvedBinaryOp::GreaterEqual
            ),
            MirAbiClass::Bool => matches!(
                op,
                ResolvedBinaryOp::Equal
                    | ResolvedBinaryOp::NotEqual
                    | ResolvedBinaryOp::LogicalAnd
                    | ResolvedBinaryOp::LogicalOr
            ),
            _ => false,
        };
        let power_profile_supported = op != ResolvedBinaryOp::Power
            || matches!(
                POWER_PROFILE_0_1_11_COMPAT,
                CheckedPowerProfile::VmNativeI64ThenI32Wrap
            );
        let result_is_bool =
            self.program
                .type_catalog()
                .get(&result_ty)
                .is_some_and(|descriptor| {
                    descriptor.is_canonical_copy_scalar(false)
                        && descriptor.abi == MirAbiClass::Bool
                });
        let comparison = matches!(
            op,
            ResolvedBinaryOp::Equal
                | ResolvedBinaryOp::NotEqual
                | ResolvedBinaryOp::Less
                | ResolvedBinaryOp::Greater
                | ResolvedBinaryOp::LessEqual
                | ResolvedBinaryOp::GreaterEqual
        );
        if !valid
            || !power_profile_supported
            || (comparison && !result_is_bool)
            || (!comparison && result_ty != left_ty)
        {
            self.error(format!(
                "{subject} binary operator is outside the verifier capability"
            ));
        }
    }

    fn require_same_type_if_unary(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        operand: &MirValueId,
        op: ResolvedUnaryOp,
        subject: &str,
    ) {
        match op {
            ResolvedUnaryOp::Negate | ResolvedUnaryOp::Not => {
                if op == ResolvedUnaryOp::Negate
                    && value_type(function, result) != value_type(function, operand)
                {
                    self.error(format!(
                        "{subject} negate result TypeDesc disagrees with operand"
                    ));
                }
                if op == ResolvedUnaryOp::Negate {
                    let (Some(result_ty), Some(operand_ty)) =
                        (value_type(function, result), value_type(function, operand))
                    else {
                        return;
                    };
                    let is_float =
                        self.program
                            .type_catalog()
                            .get(&operand_ty)
                            .is_some_and(|descriptor| {
                                descriptor.is_canonical_copy_scalar(true)
                                    && matches!(
                                        descriptor.abi,
                                        MirAbiClass::Float { bits: 32 | 64 }
                                    )
                            });
                    if is_float {
                        if let Err(message) = self.program.type_catalog().validate_copy_float_unary(
                            &result_ty,
                            &operand_ty,
                            op,
                        ) {
                            self.error(format!("{subject} float negate rejected: {message}"));
                        }
                        return;
                    }
                }
                if op == ResolvedUnaryOp::Not
                    && !value_type(function, result)
                        .and_then(|ty| self.program.type_catalog().get(&ty))
                        .is_some_and(|descriptor| {
                            descriptor.is_canonical_copy_scalar(false)
                                && descriptor.abi == MirAbiClass::Bool
                        })
                {
                    self.error(format!("{subject} Not result is not a canonical bool"));
                }
            }
            ResolvedUnaryOp::BorrowShared
            | ResolvedUnaryOp::BorrowMutable
            | ResolvedUnaryOp::Dereference => self.error(format!(
                "{subject} unary {op:?} is outside the explicit MIR Borrow/Project capability"
            )),
        }
    }

    fn require_same_type(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        source: &MirValueId,
        subject: &str,
    ) {
        if value_type(function, result) != value_type(function, source) {
            self.error(format!(
                "{subject} result and source TypeDesc identities disagree"
            ));
        }
    }

    fn validate_terminator(
        &mut self,
        function: &MirFunction,
        terminator: &MirTerminator,
        subject: &str,
    ) {
        match terminator {
            MirTerminator::Goto { .. } | MirTerminator::Branch { .. } => {}
            MirTerminator::Switch { scrutinee, arms } => {
                let Some(ty) = value_type(function, scrutinee) else {
                    return;
                };
                if arms
                    .iter()
                    .any(|arm| matches!(arm.case, MirSwitchCase::Literal(_)))
                {
                    // Scalar literal switch (R6-1051): the shared catalog
                    // contract proves the Copy-scalar scrutinee, literal
                    // shapes, and exhaustive coverage before any consumer.
                    if let Err(message) = self
                        .program
                        .type_catalog()
                        .validate_scalar_switch(&ty, arms)
                    {
                        self.error(format!("{subject} Switch rejected: {message}"));
                    }
                } else if self.program.type_catalog().variant_layout(&ty).is_none() {
                    self.error(format!(
                        "{subject} Switch is outside the Copy variant verifier capability"
                    ));
                } else if let Err(message) = self.program.type_catalog().validate_switch(&ty, arms)
                {
                    self.error(format!("{subject} Switch rejected: {message}"));
                }
            }
            MirTerminator::SwitchMove { scrutinee, arms } => {
                let Some(scrutinee_ty) = value_type(function, scrutinee) else {
                    self.error(format!("{subject} SwitchMove scrutinee is absent"));
                    return;
                };
                let switch_shape = self
                    .program
                    .type_catalog()
                    .validate_multi_target_union_variant(&scrutinee_ty)
                    .map(|_| scrutinee_ty.clone())
                    .or_else(|_| {
                        self.program
                            .type_catalog()
                            .validate_option_string_variant(&scrutinee_ty)
                    })
                    .or_else(|_| {
                        if self.allow_recoverable_flow_result {
                            self.program
                                .type_catalog()
                                .validate_recoverable_result_variant(&scrutinee_ty)
                                .map(|_| scrutinee_ty.clone())
                        } else {
                            Err(
                                "recoverable Flow Result switch is outside this verifier profile"
                                    .into(),
                            )
                        }
                    })
                    .or_else(|_| {
                        if self.allow_nested_option_tuple_variant {
                            self.program
                                .type_catalog()
                                .validate_option_nested_tuple_variant(&scrutinee_ty)
                                .map(|_| scrutinee_ty.clone())
                        } else {
                            Err(
                                "nested Option tuple switch is outside this verifier profile"
                                    .into(),
                            )
                        }
                    })
                    .or_else(|_| {
                        if self
                            .ordinary_map_owned_get_result_string_types
                            .contains(&scrutinee_ty)
                        {
                            self.program
                                .type_catalog()
                                .validate_result_string_i32_variant(&scrutinee_ty)
                                .map(|_| scrutinee_ty.clone())
                        } else {
                            Err("owned Map getter Result switch is outside this profile".into())
                        }
                    })
                    .or_else(|_| {
                        if self.allow_result_move_variant {
                            self.program
                                .type_catalog()
                                .validate_result_move_projection_variant(&scrutinee_ty)
                                .map(|_| scrutinee_ty.clone())
                        } else {
                            Err("move-owned Result switch is outside this verifier profile".into())
                        }
                    });
                if let Err(message) = switch_shape {
                    self.error(format!("{subject} SwitchMove rejected: {message}"));
                    return;
                }
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_variant_switch_move_contract(&scrutinee_ty, arms)
                {
                    self.error(format!("{subject} SwitchMove rejected: {message}"));
                    return;
                }
                let Some((_, variants)) = self.program.type_catalog().variant_layout(&scrutinee_ty)
                else {
                    self.error(format!(
                        "{subject} SwitchMove has no canonical variant layout"
                    ));
                    return;
                };
                let required = variants
                    .iter()
                    .map(|variant| variant.id.clone())
                    .collect::<BTreeSet<_>>();
                let mut seen = BTreeSet::new();
                if arms.len() != required.len() {
                    self.error(format!(
                        "{subject} SwitchMove requires exactly one explicit arm for each TypeDesc variant"
                    ));
                }
                for arm in arms {
                    let MirSwitchCase::Variant(variant_id) = &arm.case else {
                        self.error(format!(
                            "{subject} SwitchMove requires explicit variant arms; default/literal cases are not covered"
                        ));
                        continue;
                    };
                    let Some(variant) = self
                        .program
                        .type_catalog()
                        .variant(&scrutinee_ty, variant_id)
                    else {
                        self.error(format!(
                            "{subject} SwitchMove variant '{}' is absent from TypeDesc",
                            variant_id.0
                        ));
                        continue;
                    };
                    if !seen.insert(variant.id.clone()) {
                        self.error(format!(
                            "{subject} SwitchMove variant '{}' is repeated",
                            variant.name
                        ));
                    }
                    let Some(target) = function.blocks.get(&arm.target) else {
                        self.error(format!(
                            "{subject} SwitchMove edge target '{}' is absent",
                            arm.target
                        ));
                        continue;
                    };
                    for (index, argument) in arm.arguments.iter().enumerate() {
                        if !function.values.contains_key(argument) {
                            self.error(format!(
                                "{subject} SwitchMove edge argument '{}' is absent",
                                argument
                            ));
                            continue;
                        }
                        let Some(parameter) = target
                            .parameters
                            .get(index)
                            .and_then(|parameter| function.values.get(&parameter.value))
                        else {
                            continue;
                        };
                        if function
                            .values
                            .get(argument)
                            .is_some_and(|value| value.ty != parameter.ty)
                        {
                            self.error(format!(
                                "{subject} SwitchMove edge argument type disagrees with block parameter"
                            ));
                        }
                    }
                    if target.parameters.len() != arm.arguments.len() + arm.bindings.len() {
                        self.error(format!(
                            "{subject} SwitchMove edge arguments and payload bindings disagree with block parameter arity"
                        ));
                    }
                    // A nested tuple receipt intentionally projects more than
                    // one leaf from the same outer Option::Some payload field.
                    // Keep duplicate detection at receipt granularity; a
                    // plain payload projection remains exactly-once.
                    let mut binding_fields = BTreeSet::new();
                    for (index, binding) in arm.bindings.iter().enumerate() {
                        let binding_key = (
                            binding.projection.field.clone(),
                            binding
                                .nested_tuple
                                .as_ref()
                                .map(|nested| nested.field_index),
                        );
                        if !binding_fields.insert(binding_key) {
                            self.error(format!(
                                "{subject} SwitchMove payload field '{}' is bound more than once",
                                binding.projection.field.0
                            ));
                        }
                        let Some(target_parameter) =
                            target.parameters.get(arm.arguments.len() + index)
                        else {
                            continue;
                        };
                        let Some(parameter) = function.values.get(&target_parameter.value) else {
                            continue;
                        };
                        if binding.parameter != target_parameter.value {
                            self.error(format!(
                                "{subject} SwitchMove binding parameter disagrees with target block parameter"
                            ));
                        }
                        let validation = if let Some(nested) = &binding.nested_tuple {
                            self.program
                                .type_catalog()
                                .validate_variant_nested_tuple_payload_projection_receipt(
                                    &scrutinee_ty,
                                    variant_id,
                                    &parameter.ty,
                                    &binding.projection,
                                    nested,
                                )
                        } else {
                            self.program
                                .type_catalog()
                                .validate_variant_payload_projection_receipt(
                                    &scrutinee_ty,
                                    variant_id,
                                    &parameter.ty,
                                    &binding.projection,
                                )
                        };
                        if let Err(message) = validation {
                            self.error(format!("{subject} SwitchMove rejected: {message}"));
                        }
                    }
                }
            }
            MirTerminator::Return { .. } | MirTerminator::Trap { .. } => {}
            MirTerminator::Fault { .. } => {
                self.error(format!(
                    "{subject} Fault is outside the verifier capability"
                ));
            }
            MirTerminator::Unreachable => {
                self.error(format!(
                    "{subject} Unreachable is outside the default verifier island"
                ));
            }
        }
    }

    fn validate_acyclic_cfg(&mut self, function: &MirFunction) {
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        let mut certified_back_edges = BTreeSet::new();
        if let Some(worklist) = &function.proof_worklist {
            fn collect(
                block: &crate::core::mir::MirProofBlock,
                edges: &mut BTreeSet<(crate::core::mir::MirBlockId, crate::core::mir::MirBlockId)>,
            ) {
                for operation in &block.operations {
                    match operation {
                        crate::core::mir::MirProofOperation::Loop {
                            header,
                            body_entry,
                            body,
                            stationary: true,
                            ..
                        } => {
                            edges.insert((body_entry.clone(), header.clone()));
                            collect(body, edges);
                        }
                        crate::core::mir::MirProofOperation::Loop {
                            header,
                            body,
                            stationary: false,
                            receipt: Some(receipt),
                            ..
                        } => {
                            for edge in &receipt.edges {
                                if matches!(
                                    edge.kind,
                                    MirProofLoopEdgeKind::BodyFallthrough
                                        | MirProofLoopEdgeKind::Continue
                                ) && edge.to_block.as_ref() == Some(header)
                                    && edge.mir_edge.is_some()
                                {
                                    edges.insert((edge.from_block.clone(), header.clone()));
                                }
                            }
                            collect(body, edges);
                        }
                        crate::core::mir::MirProofOperation::Branch {
                            then_block,
                            else_block,
                            ..
                        } => {
                            collect(then_block, edges);
                            collect(else_block, edges);
                        }
                        crate::core::mir::MirProofOperation::Scope(inner) => collect(inner, edges),
                        crate::core::mir::MirProofOperation::Bind { .. }
                        | crate::core::mir::MirProofOperation::Call(_)
                        | crate::core::mir::MirProofOperation::Eval { .. }
                        | crate::core::mir::MirProofOperation::Assign { .. }
                        | crate::core::mir::MirProofOperation::Math(_)
                        | crate::core::mir::MirProofOperation::Break { .. }
                        | crate::core::mir::MirProofOperation::Continue { .. }
                        | crate::core::mir::MirProofOperation::Return { .. }
                        | crate::core::mir::MirProofOperation::Loop { .. }
                        | crate::core::mir::MirProofOperation::Exit => {}
                    }
                }
            }
            collect(&worklist.root, &mut certified_back_edges);
        }
        for block in function.blocks.keys() {
            if !visited.contains(block)
                && self.visit_cfg(
                    function,
                    block,
                    &mut visiting,
                    &mut visited,
                    &certified_back_edges,
                )
            {
                self.error(format!(
                    "function '{}' contains a cyclic CFG outside the verifier capability",
                    function.owner.0
                ));
                return;
            }
        }
    }

    fn visit_cfg(
        &self,
        function: &MirFunction,
        block: &crate::core::mir::MirBlockId,
        visiting: &mut BTreeSet<crate::core::mir::MirBlockId>,
        visited: &mut BTreeSet<crate::core::mir::MirBlockId>,
        certified_back_edges: &BTreeSet<(
            crate::core::mir::MirBlockId,
            crate::core::mir::MirBlockId,
        )>,
    ) -> bool {
        if visited.contains(block) {
            return false;
        }
        visiting.insert(block.clone());
        let Some(block_data) = function.blocks.get(block) else {
            visiting.remove(block);
            return false;
        };
        let targets = match &block_data.terminator {
            MirTerminator::Goto { target, .. } => vec![target],
            MirTerminator::Branch {
                then_target,
                else_target,
                ..
            } => {
                vec![then_target, else_target]
            }
            MirTerminator::Switch { arms, .. } | MirTerminator::SwitchMove { arms, .. } => {
                arms.iter().map(|arm| &arm.target).collect()
            }
            MirTerminator::Return { .. }
            | MirTerminator::Trap { .. }
            | MirTerminator::Fault { .. }
            | MirTerminator::Unreachable => Vec::new(),
        };
        for target in targets {
            if visiting.contains(target) {
                if !certified_back_edges.contains(&(block.clone(), target.clone())) {
                    return true;
                }
            } else if !visited.contains(target)
                && self.visit_cfg(function, target, visiting, visited, certified_back_edges)
            {
                return true;
            }
        }
        visiting.remove(block);
        visited.insert(block.clone());
        false
    }

    fn error(&mut self, message: String) {
        self.errors.insert(message);
    }
}

fn catalog_is_managed_any_string_pair(
    catalog: &MirTypeCatalog,
    left: &ResolvedTypeId,
    right: &ResolvedTypeId,
) -> bool {
    (catalog.is_managed_any_i32_string_v1(left) && catalog.validate_owned_string(right).is_ok())
        || (catalog.is_managed_any_i32_string_v1(right)
            && catalog.validate_owned_string(left).is_ok())
}

fn value_type(function: &MirFunction, value: &MirValueId) -> Option<crate::core::ResolvedTypeId> {
    function.values.get(value).map(|value| value.ty.clone())
}

fn is_map_root_operation(instruction: &MirInstructionKind) -> bool {
    matches!(
        instruction,
        MirInstructionKind::MapRootNew { .. }
            | MirInstructionKind::MapRootSet { .. }
            | MirInstructionKind::MapRootRemove { .. }
            | MirInstructionKind::MapRootSize { .. }
            | MirInstructionKind::MapRootContains { .. }
            | MirInstructionKind::MapRootDrop { .. }
    )
}

/// Validate the exact list payload that the owned-get `values()` instruction
/// publishes.  A generic List TypeDesc is insufficient: every element is an
/// independently owned ManagedAny snapshot and the List itself must carry the
/// complete move/clone/drop contract consumed by the four MIR backends.
fn is_canonical_managed_any_list_type(catalog: &MirTypeCatalog, ty: &ResolvedTypeId) -> bool {
    let Some(descriptor) = catalog.get(ty) else {
        return false;
    };
    let MirLayout::List { element } = &descriptor.layout else {
        return false;
    };
    descriptor.kind == MirTypeKind::List
        && descriptor.abi == MirAbiClass::OpaqueHandle
        && descriptor.ownership == MirOwnership::Move
        && descriptor.needs_drop_glue
        && descriptor.needs_clone_glue
        && descriptor.session_protocol.is_none()
        && descriptor.drop_plan.is_none()
        && descriptor.variant_drop_plan.is_none()
        && descriptor.glue
            == (crate::core::mir::types::MirGlueContract {
                move_out: MirGlueKind::List,
                clone: MirGlueKind::List,
                drop: MirGlueKind::List,
            })
        && catalog.is_managed_any_i32_string_v1(element)
}

fn instruction_touches_ordinary_map_owner(instruction: &MirInstructionKind) -> bool {
    matches!(
        instruction,
        MirInstructionKind::OrdinaryMapOwnerLifecycleNew { .. }
            | MirInstructionKind::OrdinaryMapOwnerLifecycleSet { .. }
            | MirInstructionKind::OrdinaryMapOwnerLifecycleRemove { .. }
            | MirInstructionKind::OrdinaryMapOwnerLifecycleSize { .. }
            | MirInstructionKind::OrdinaryMapOwnerLifecycleContains { .. }
            | MirInstructionKind::OrdinaryMapOwnerLifecycleDrop { .. }
    )
}

fn instruction_touches_ordinary_map_owned_get(instruction: &MirInstructionKind) -> bool {
    matches!(
        instruction,
        MirInstructionKind::OrdinaryMapOwnedGetI32StringV1New { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Set { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Remove { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Size { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Contains { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetI32 { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetString { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Drop { .. }
    )
}

fn instruction_touches_ordinary_map_owned_get_type(
    function: &MirFunction,
    instruction: &MirInstructionKind,
) -> bool {
    let owner_ty = crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id();
    let is_owner = |value: &MirValueId| {
        function
            .values
            .get(value)
            .is_some_and(|value| value.ty == owner_ty)
    };
    match instruction {
        MirInstructionKind::OrdinaryMapOwnedGetI32StringV1New { .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Set { .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Remove { .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Size { .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Contains { .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetI32 { .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetString { .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Drop { .. } => true,
        MirInstructionKind::MapRootNew { result } => is_owner(result),
        MirInstructionKind::MapRootSet {
            result,
            source,
            value,
            ..
        } => is_owner(result) || is_owner(source) || is_owner(value),
        MirInstructionKind::MapRootRemove { result, source, .. } => {
            is_owner(result) || is_owner(source)
        }
        MirInstructionKind::MapRootSize { result, root }
        | MirInstructionKind::MapRootContains { result, root, .. } => {
            is_owner(result) || is_owner(root)
        }
        MirInstructionKind::MapRootDrop { root } => is_owner(root),
        MirInstructionKind::Copy { result, source }
        | MirInstructionKind::Move { result, source }
        | MirInstructionKind::Clone { result, source } => is_owner(result) || is_owner(source),
        MirInstructionKind::Drop { value } | MirInstructionKind::EndBorrow { borrow: value } => {
            is_owner(value)
        }
        MirInstructionKind::Call {
            result, arguments, ..
        } => result.as_ref().is_some_and(is_owner) || arguments.iter().any(is_owner),
        MirInstructionKind::ClosureInvoke {
            result,
            function_value,
            arguments,
            ..
        } => {
            is_owner(function_value)
                || result.as_ref().is_some_and(is_owner)
                || arguments.iter().any(is_owner)
        }
        MirInstructionKind::BuiltinCall {
            result, arguments, ..
        } => is_owner(result) || arguments.iter().any(is_owner),
        _ => false,
    }
}

fn instruction_touches_ordinary_type(
    function: &MirFunction,
    instruction: &MirInstructionKind,
) -> bool {
    let owner_ty = crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id();
    let is_owner = |value: &MirValueId| {
        function
            .values
            .get(value)
            .is_some_and(|value| value.ty == owner_ty)
    };
    match instruction {
        MirInstructionKind::OrdinaryMapOwnerLifecycleNew { .. }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleSet { .. }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleRemove { .. }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleSize { .. }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleContains { .. }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleDrop { .. } => true,
        MirInstructionKind::MapRootNew { result } => is_owner(result),
        MirInstructionKind::MapRootSet {
            result,
            source,
            value,
            ..
        } => is_owner(result) || is_owner(source) || is_owner(value),
        MirInstructionKind::MapRootRemove { result, source, .. } => {
            is_owner(result) || is_owner(source)
        }
        MirInstructionKind::MapRootSize { result, root }
        | MirInstructionKind::MapRootContains { result, root, .. } => {
            is_owner(result) || is_owner(root)
        }
        MirInstructionKind::MapRootDrop { root } => is_owner(root),
        MirInstructionKind::Copy { result, source }
        | MirInstructionKind::Move { result, source }
        | MirInstructionKind::Clone { result, source } => is_owner(result) || is_owner(source),
        MirInstructionKind::Drop { value } | MirInstructionKind::EndBorrow { borrow: value } => {
            is_owner(value)
        }
        MirInstructionKind::Call {
            result, arguments, ..
        } => result.as_ref().is_some_and(is_owner) || arguments.iter().any(is_owner),
        MirInstructionKind::ClosureInvoke {
            result,
            function_value,
            arguments,
            ..
        } => {
            is_owner(function_value)
                || result.as_ref().is_some_and(is_owner)
                || arguments.iter().any(is_owner)
        }
        MirInstructionKind::BuiltinCall {
            result, arguments, ..
        } => is_owner(result) || arguments.iter().any(is_owner),
        _ => false,
    }
}

/// MapAnyOwner v1 deliberately reuses the closed MapRoot instruction shapes,
/// while its distinct synthetic TypeDesc and private Checker receipt supply
/// the owner-token semantics. Select that route only from typed MIR identity;
/// a raw MapRoot receipt cannot classify an operation as v1.
fn instruction_touches_map_any_owner(
    function: &MirFunction,
    instruction: &MirInstructionKind,
) -> bool {
    let owner_ty = crate::core::mir::types::map_any_owner_v1_type_id();
    let is_owner = |value: &MirValueId| {
        function
            .values
            .get(value)
            .is_some_and(|value| value.ty == owner_ty)
    };
    match instruction {
        MirInstructionKind::MapRootNew { result } => is_owner(result),
        MirInstructionKind::MapRootSet {
            result,
            source,
            value,
            ..
        } => is_owner(result) || is_owner(source) || is_owner(value),
        MirInstructionKind::MapRootRemove { result, source, .. } => {
            is_owner(result) || is_owner(source)
        }
        MirInstructionKind::MapRootSize { result, root }
        | MirInstructionKind::MapRootContains { result, root, .. } => {
            is_owner(result) || is_owner(root)
        }
        MirInstructionKind::MapRootDrop { root } => is_owner(root),
        MirInstructionKind::ClosureInvoke {
            result,
            function_value,
            arguments,
            ..
        } => {
            is_owner(function_value)
                || result.as_ref().is_some_and(is_owner)
                || arguments.iter().any(is_owner)
        }
        _ => false,
    }
}

fn value_operation_is_supported(
    catalog: &crate::core::mir::types::MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
    operation: MirGlueOperation,
) -> bool {
    let Some(descriptor) = catalog.get(ty) else {
        return false;
    };
    if descriptor.ownership == MirOwnership::Copy {
        catalog.validate_copy_value(ty).is_ok()
    } else {
        catalog.validate_glue(ty, operation).is_ok()
    }
}

fn function_has_ensures(function: &MirFunction) -> bool {
    function
        .contracts
        .iter()
        .any(|contract| contract.kind == MirContractKind::Ensures)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ir::{PrimitiveType, ResolvedType};
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn canonical(source: &str) -> MirProgram {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        MirProgram::from_checked_program(&checked).expect("canonical MIR")
    }

    #[test]
    fn admits_only_typed_signed_i32_i64_power_across_the_full_mir_graph() {
        let program = canonical(
            r#"
                // These helpers are intentionally uncalled and contract-free:
                // capability is a whole-program property, not a reachable-call
                // or obligation-only property.
                func pow_i32(base: i32, exponent: i32) -> i32 { base ** exponent }
                func pow_i64(base: i64, exponent: i64) -> i64 { base ** exponent }
                func main() -> i32 { 0 }
            "#,
        );

        let mut saw_i32 = false;
        let mut saw_i64 = false;
        let mut power_count = 0;
        for function in program.functions().values() {
            for instruction in function
                .blocks
                .values()
                .flat_map(|block| &block.instructions)
            {
                let MirInstructionKind::Binary {
                    result,
                    op: ResolvedBinaryOp::Power,
                    ..
                } = &instruction.kind
                else {
                    continue;
                };
                power_count += 1;
                let value = function
                    .values
                    .get(result)
                    .expect("Power result is in the value table");
                let descriptor = program
                    .type_catalog()
                    .get(&value.ty)
                    .expect("Power result TypeDesc");
                match descriptor.abi {
                    MirAbiClass::Integer {
                        bits: 32,
                        signed: true,
                    } => saw_i32 = true,
                    MirAbiClass::Integer {
                        bits: 64,
                        signed: true,
                    } => saw_i64 = true,
                    other => panic!("Power result has unsupported ABI {other:?}"),
                }
            }
        }
        assert_eq!(
            power_count, 2,
            "the uncalled helpers remain in the MIR graph"
        );
        assert!(
            saw_i32 && saw_i64,
            "both supported source widths are present"
        );
        assert!(matches!(
            POWER_PROFILE_0_1_11_COMPAT,
            CheckedPowerProfile::VmNativeI64ThenI32Wrap
        ));
        validate_mir_capabilities(&program)
            .expect("signed i32/i64 Power must be admitted across every function");
    }

    #[test]
    fn rejects_float_unsigned_and_narrow_integer_power_profiles() {
        let float_program = canonical(
            "func pow_float(base: f64, exponent: f64) -> f64 { base ** exponent } func main() -> i32 { 0 }",
        );
        let float_errors = validate_mir_capabilities(&float_program)
            .expect_err("float Power remains outside the verifier capability");
        assert!(
            float_errors
                .iter()
                .any(|error| error.contains("float binary rejected")),
            "{float_errors:?}"
        );

        let signed_i32 = canonical(
            "func pow_i32(base: i32, exponent: i32) -> i32 { base ** exponent } func main() -> i32 { 0 }",
        );
        validate_mir_capabilities(&signed_i32)
            .expect("the real signed i32 source remains admitted");

        let mut power_type = None;
        for function in signed_i32.functions().values() {
            for instruction in function
                .blocks
                .values()
                .flat_map(|block| &block.instructions)
            {
                let MirInstructionKind::Binary {
                    result,
                    op: ResolvedBinaryOp::Power,
                    left,
                    right,
                } = &instruction.kind
                else {
                    continue;
                };
                let result_ty = function
                    .values
                    .get(result)
                    .expect("Power result value")
                    .ty
                    .clone();
                for operand in [left, right] {
                    assert_eq!(
                        function
                            .values
                            .get(operand)
                            .expect("Power operand value")
                            .ty,
                        result_ty,
                        "the source Power operands and result share one canonical TypeDesc ID"
                    );
                }
                assert!(
                    power_type.replace(result_ty).is_none(),
                    "single-function signed i32 source has exactly one Power result type"
                );
            }
        }
        let power_type = power_type.expect("signed i32 Power instruction exists");
        let canonical_descriptor = signed_i32
            .type_catalog()
            .get(&power_type)
            .expect("canonical signed i32 descriptor")
            .clone();
        assert_eq!(canonical_descriptor.id, power_type);
        assert_eq!(
            canonical_descriptor.abi,
            MirAbiClass::Integer {
                bits: 32,
                signed: true
            }
        );

        for forged_abi in [
            MirAbiClass::Integer {
                bits: 32,
                signed: false,
            },
            MirAbiClass::Integer {
                bits: 8,
                signed: true,
            },
            MirAbiClass::Integer {
                bits: 16,
                signed: true,
            },
        ] {
            let mut catalog = signed_i32.type_catalog().clone();
            let mut forged_descriptor = canonical_descriptor.clone();
            forged_descriptor.abi = forged_abi;
            catalog.replace_for_test_only(power_type.clone(), forged_descriptor);
            let forged_program =
                MirProgram::with_type_catalog(signed_i32.functions().clone(), catalog).expect(
                    "ABI-only mutation keeps the same canonical TypeDesc identity and MIR graph",
                );
            assert_eq!(
                forged_program.functions(),
                signed_i32.functions(),
                "the negative control must forge only the TypeDesc ABI metadata"
            );
            assert_eq!(
                forged_program
                    .type_catalog()
                    .get(&power_type)
                    .expect("forged descriptor keeps canonical ID")
                    .id,
                power_type
            );
            let errors = validate_mir_capabilities(&forged_program)
                .expect_err("unsigned and narrow integer profiles must fail the capability gate");
            assert!(
                errors.iter().any(|error| error
                    .contains("scalar TypeDesc is outside the verifier scalar contract")),
                "forged ABI {forged_abi:?} must be rejected by the MIR verifier capability gate, got {errors:?}"
            );
        }
    }

    #[test]
    fn rejects_non_copy_record_update_without_transfer_contract() {
        let program = canonical(
            r#"
                type Box { text: string, count: i32 }

                func rewrite(value: Box, next_count: i32) -> Box {
                    ensures: result.count == next_count
                    let updated = Box { count: next_count, ..value };
                    updated
                }

                func main() -> i32 { 0 }
            "#,
        );
        let errors = validate_mir_capabilities(&program)
            .expect_err("unreceipted non-Copy update must stay outside the proof island");
        assert!(errors.iter().any(|error| {
            error.contains("non-Copy record update requires a validated move receipt")
        }));
    }

    #[test]
    fn actor_fast_path_rejects_forged_any_body_before_neutral_return() {
        let mut program = canonical(
            r#"
                actor Counter {
                    count: i32 = 0;
                    func get() -> i32 { self.count }
                }

                func main() -> i32 {
                    let root = map_new()
                    let snapshot = map_get(root, "missing")
                    drop(root)
                    let found = snapshot.0
                    let payload = snapshot.1
                    drop(snapshot)
                    drop(payload)
                    if found { 1 } else { 0 }
                }
            "#,
        );
        assert!(program.actor_execution_plan().is_some());

        let any_function = program
            .functions()
            .values()
            .find(|function| {
                function.values.values().any(|value| {
                    program
                        .type_catalog()
                        .contains_dynamic_or_managed_any(&value.ty)
                })
            })
            .expect("Actor fixture includes an Any-bearing callable");
        let mut forged = any_function.clone();
        let instruction = forged
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .next()
            .expect("Any-bearing callable has a lowered instruction");
        instruction.id = crate::core::mir::MirInstructionId::new("forged:any-instruction")
            .expect("valid forged instruction identity");
        program.replace_function_for_test_only(forged);

        let errors = validate_mir_capabilities(&program)
            .expect_err("Actor's neutral fast path must still preflight Any ownership receipts");
        assert!(errors
            .iter()
            .any(|error| { error.contains("ManagedAny") || error.contains("managed-any") }));
    }

    #[test]
    fn admits_only_the_separately_receipted_map_any_owner_v1_type() {
        let program = canonical(
            r#"
                func main() -> i32 {
                    let root = map_new()
                    let inserted = map_set(root, "answer", 42)
                    let size = map_size(inserted)
                    drop(inserted)
                    size
                }
            "#,
        );
        assert!(program
            .functions()
            .values()
            .any(|function| { function.ownership.has_checker_map_any_owner_actions() }));
        let owner_ty = crate::core::mir::types::map_any_owner_v1_type_id();
        assert_eq!(
            program
                .type_catalog()
                .get(&owner_ty)
                .expect("MapAnyOwner v1 TypeDesc")
                .kind,
            MirTypeKind::MapAnyOwnerV1
        );
        assert!(program
            .type_catalog()
            .get(&crate::core::mir::types::map_root_type_id())
            .is_none());
        validate_mir_capabilities(&program)
            .expect("the independent MapAnyOwner v1 receipt and TypeDesc admit the profile");
    }

    #[test]
    fn rejects_orphan_or_forged_map_any_owner_v1_descriptors() {
        let owner_program = canonical(
            r#"
                func main() -> i32 {
                    let root = map_new()
                    let inserted = map_set(root, "answer", 42)
                    drop(inserted)
                    0
                }
            "#,
        );
        let owner_ty = crate::core::mir::types::map_any_owner_v1_type_id();
        let mut descriptor = owner_program
            .type_catalog()
            .get(&owner_ty)
            .expect("MapAnyOwner v1 TypeDesc")
            .clone();
        descriptor.glue.clone = MirGlueKind::MapAnyOwnerV1;

        let plain_program = canonical("func main() -> i32 { 0 }");
        let mut catalog = plain_program.type_catalog().clone();
        catalog.replace_for_test_only(owner_ty, descriptor);
        let orphan = MirProgram::with_type_catalog(plain_program.functions().clone(), catalog)
            .expect("unused synthetic TypeDesc remains structurally representable");
        let errors = validate_mir_capabilities(&orphan)
            .expect_err("an orphan owner descriptor cannot authorize the profile");
        assert!(errors.iter().any(|error| {
            error.contains(
                "MapAnyOwner v1 TypeDesc presence must match independent Checker owner receipts",
            )
        }));
        assert!(errors
            .iter()
            .any(|error| error.contains("MapAnyOwner v1 TypeDesc rejected")));
    }

    #[test]
    fn accepts_copy_record_and_scalar_call_graph() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_record_copy.mimi"
        ));
        validate_mir_capabilities(&program).expect("copy record island capability");
    }

    #[test]
    fn rejects_forged_scalar_descriptor_before_capability_value_checks() {
        let source = "func main() -> i64 { 0 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let i64_id = checked
            .resolved_types()
            .iter()
            .find_map(|(id, ty)| {
                matches!(ty, ResolvedType::Primitive(PrimitiveType::I64)).then_some(id.clone())
            })
            .expect("i64 TypeDesc identity");
        let descriptor = program
            .type_catalog()
            .get(&i64_id)
            .expect("i64 TypeDesc")
            .clone();
        assert!(descriptor.is_canonical_copy_scalar(false));
        let mut forged = descriptor;
        forged.kind = MirTypeKind::Nominal;
        let mut catalog = program.type_catalog().clone();
        catalog.replace_for_test_only(i64_id, forged);
        let scalar_id = checked
            .resolved_types()
            .iter()
            .find_map(|(id, ty)| {
                matches!(ty, ResolvedType::Primitive(PrimitiveType::I64)).then_some(id.clone())
            })
            .expect("forged scalar TypeDesc identity");
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            assert!(
                !value_operation_is_supported(&catalog, &scalar_id, operation),
                "malformed Copy scalar must not fall back to {:?} glue",
                operation
            );
        }
        let forged_program = MirProgram::with_type_catalog(program.functions().clone(), catalog)
            .expect("forged catalog remains structurally valid");

        let errors = validate_mir_capabilities(&forged_program)
            .expect_err("forged scalar TypeDesc must fail capability admission");
        assert!(errors.iter().any(|error| {
            error.contains("scalar TypeDesc is outside the verifier scalar contract")
        }));
    }

    #[test]
    fn rejects_forged_copy_aggregate_metadata_before_capability_consumption() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_record_copy.mimi"
        ));
        let record_id = program
            .type_catalog()
            .iter()
            .find_map(|(ty, descriptor)| {
                (matches!(descriptor.layout, MirLayout::Record { .. })
                    && descriptor.ownership == MirOwnership::Copy)
                    .then(|| ty.clone())
            })
            .expect("Copy record TypeDesc");
        let descriptor = program
            .type_catalog()
            .get(&record_id)
            .expect("Copy record descriptor")
            .clone();
        assert!(descriptor.has_canonical_copy_noop_metadata());

        for mutation in 0..4 {
            let mut catalog = program.type_catalog().clone();
            let mut forged = descriptor.clone();
            match mutation {
                0 => forged.session_protocol = Some(record_id.clone()),
                1 => forged.needs_drop_glue = true,
                2 => {
                    forged.drop_plan =
                        Some(crate::core::mir::types::MirDropGluePlan { fields: Vec::new() })
                }
                3 => forged.variant_drop_plan = Some(Vec::new()),
                _ => unreachable!(),
            }
            catalog.replace_for_test_only(record_id.clone(), forged);
            let forged_program =
                MirProgram::with_type_catalog(program.functions().clone(), catalog)
                    .expect("forged catalog remains structurally valid");
            let errors = validate_mir_capabilities(&forged_program)
                .expect_err("forged Copy aggregate metadata must fail capability admission");
            assert!(
                errors
                    .iter()
                    .any(|error| error.contains("is not Copy with canonical no-op glue")),
                "unexpected errors: {errors:?}"
            );
        }
    }

    #[test]
    fn accepts_non_copy_option_string_variant_contract() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_option_string.mimi"
        ));
        validate_mir_capabilities(&program).expect("Option<string> verifier capability");
    }

    #[test]
    fn accepts_recursive_owned_tuple_contract() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_recursive_tuple.mimi"
        ));
        validate_mir_capabilities(&program).expect("recursive tuple verifier capability");
        let tuple = program
            .type_catalog()
            .iter()
            .find_map(|(ty, descriptor)| {
                (matches!(descriptor.layout, MirLayout::Tuple(ref fields) if fields.len() == 2)
                    && descriptor.ownership == MirOwnership::Move)
                    .then(|| ty.clone())
            })
            .expect("recursive Move tuple TypeDesc");
        program
            .type_catalog()
            .validate_recursive_tuple_abi(&tuple)
            .expect("shared recursive tuple TypeDesc contract");
    }

    #[test]
    fn rejects_recursive_tuple_with_list_child_before_verifier_consumption() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_recursive_tuple_rejected.mimi"
        ));
        let errors = validate_mir_capabilities(&program)
            .expect_err("tuple with List child must remain outside verifier capability");
        assert!(errors
            .iter()
            .any(|error| { error.contains("scalar/String/tuple ABI") || error.contains("List") }));
    }

    #[test]
    fn rejects_unsupported_non_copy_variant_before_default_route() {
        let tokens = Lexer::new(
            "func main() -> i32 { let value: Option<List<string>> = Some([\"owned\"]); drop(value); 42 }",
        )
        .tokenize()
        .expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let error = MirProgram::from_checked_program(&checked)
            .expect_err("unsupported Option<List<string>> must fail before capability gate");
        let crate::core::mir::reference::MirProgramBuildError::Validation(errors) = error else {
            panic!("unsupported Option<List<string>> reached an unexpected MIR stage");
        };
        assert!(errors.iter().any(|error| error.message.contains("List")));
    }

    #[test]
    fn accepts_indexed_list_projection_with_materialized_receipt() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_list_index.mimi"
        ));
        validate_mir_capabilities(&program)
            .expect("List index receipt must satisfy verifier capability gate");
    }

    #[test]
    fn accepts_list_operation_with_materialized_receipt() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_list_len.mimi"
        ));
        validate_mir_capabilities(&program)
            .expect("List operation receipt must satisfy verifier capability gate");
    }

    #[test]
    fn accepts_one_level_nested_list_construct_and_recursive_glue() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_generic_list_nested_owned.mimi"
        ));
        validate_mir_capabilities(&program)
            .expect("nested List construction must satisfy verifier capability gate");
        let nested = program
            .type_catalog()
            .iter()
            .find_map(|(ty, descriptor)| {
                matches!(descriptor.layout, MirLayout::List { ref element }
                if program.type_catalog().get(element).is_some_and(|child| {
                    matches!(child.layout, MirLayout::List { .. })
                }))
                .then(|| ty.clone())
            })
            .expect("nested List TypeDesc");
        program
            .type_catalog()
            .validate_nested_list_payload(&nested)
            .expect("nested List TypeDesc glue");
    }

    #[test]
    fn accepts_nested_list_len_without_transfer_or_projection() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_nested_list_len.mimi"
        ));
        validate_mir_capabilities(&program)
            .expect("nested List.len must satisfy verifier capability gate");
        assert!(program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        MirInstructionKind::ListOp {
                            operation: crate::core::mir::MirListOperation::Len,
                            ..
                        }
                    )
                })
            })
        }));
    }

    #[test]
    fn accepts_nested_list_index_with_materialized_clone_receipt() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_nested_list_index.mimi"
        ));
        validate_mir_capabilities(&program)
            .expect("nested List index clone receipt must satisfy verifier capability gate");
        assert!(program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        MirInstructionKind::Project {
                            projection: crate::core::mir::MirProjection::Index(_),
                            list_index_contract: Some(ref receipt),
                            ..
                        } if receipt.mode
                            == crate::core::mir::types::MirListIndexProjectionMode::CloneNestedList
                    )
                })
            })
        }));
    }

    #[test]
    fn accepts_nested_list_reverse_with_materialized_recursive_clone_receipt() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_nested_list_reverse.mimi"
        ));
        validate_mir_capabilities(&program)
            .expect("nested List.reverse clone receipt must satisfy verifier capability gate");
        assert!(program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        MirInstructionKind::ListOp {
                            operation: crate::core::mir::MirListOperation::Reverse,
                            list_operation_contract: Some(ref receipt),
                            ..
                        } if receipt.mode
                            == crate::core::mir::types::MirListOperationMode::Nested
                    )
                })
            })
        }));
    }

    #[test]
    fn accepts_nested_list_concat_with_materialized_child_move_receipt() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_nested_list_concat.mimi"
        ));
        validate_mir_capabilities(&program)
            .expect("nested List.concat move receipt must satisfy verifier capability gate");
        assert!(program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        MirInstructionKind::ListOp {
                            operation: crate::core::mir::MirListOperation::Concat,
                            list_operation_contract: Some(ref receipt),
                            ..
                        } if receipt.mode
                            == crate::core::mir::types::MirListOperationMode::Nested
                    )
                })
            })
        }));
    }
}
