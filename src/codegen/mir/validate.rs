//! Canonical MIR native admission validator.
//!
//! Validation is complete before LLVM declarations are created.  This module
//! is intentionally the only owner of native shape eligibility.

use super::*;

fn exceeds_native_u32(value: usize) -> bool {
    u64::try_from(value).map_or(true, |value| value > u64::from(u32::MAX))
}

pub(super) struct NativeMirValidator<'a> {
    program: &'a MirProgram,
    errors: Vec<NativeMirError>,
    symbols: BTreeSet<String>,
}

impl<'a> NativeMirValidator<'a> {
    pub(super) fn new(program: &'a MirProgram) -> Self {
        Self {
            program,
            errors: Vec::new(),
            symbols: BTreeSet::new(),
        }
    }

    pub(super) fn validate(mut self) -> Result<(), Vec<NativeMirError>> {
        // Closure binding metadata is authority-bearing: reject forged ABI,
        // capture, call-target, and bind/invoke relations before the emitter
        // is allowed to create any LLVM declaration or instruction effect.
        if let Err(messages) = self.program.validate_closure_callable_receipts() {
            for message in messages {
                self.errors.push(NativeMirError::new(
                    "closure-callable",
                    format!("pre-emission closure receipt validation failed: {message}"),
                ));
            }
        }
        self.validate_ordinary_map_owned_any_v2_get_any_receipts();
        if let Err(messages) =
            super::ordinary_map_owned_any_v2::validate_ordinary_map_owned_any_v2_unpack_native_contract(
                self.program,
            )
        {
            for message in messages {
                self.errors.push(NativeMirError::new(
                    "OrdinaryMapOwnedAnyV2.Unpack",
                    message,
                ));
            }
        }
        if let Err(messages) =
            super::ordinary_map_owned_any_v2::validate_ordinary_map_owned_any_v2_values_native_contract(
                self.program,
            )
        {
            for message in messages {
                self.errors.push(NativeMirError::new(
                    "OrdinaryMapOwnedAnyV2.Values",
                    message,
                ));
            }
        }
        if let Err(messages) = super::ordinary_map_owned_any_v2::validate_ordinary_map_owned_any_v2_values_unpack_native_contract(self.program)
        {
            for message in messages {
                self.errors.push(NativeMirError::new(
                    "OrdinaryMapOwnedAnyV2.ValuesUnpack",
                    message,
                ));
            }
        }
        self.validate_managed_any_generic_instances();
        self.errors
            .extend(super::actor_status::validate_actor_native_program(
                self.program,
            ));
        for message in crate::core::mir::validate_ffi_receipt_table(
            self.program.functions(),
            self.program.ffi_calls(),
        ) {
            self.errors.push(NativeMirError::new("ffi", message));
        }
        for message in
            crate::core::mir::validate_ffi_symbol_declaration_shapes(self.program.ffi_calls())
        {
            self.errors.push(NativeMirError::new("ffi", message));
        }
        let map_root_type = crate::core::mir::types::map_root_type_id();
        let has_map_root_type = self.program.type_catalog().iter().any(|(id, descriptor)| {
            id == &map_root_type
                || descriptor.kind == MirTypeKind::MapRoot
                || descriptor.layout == MirLayout::MapRoot
        });
        let has_map_root_receipt = self
            .program
            .functions()
            .values()
            .any(|function| function.ownership.has_checker_map_root_actions());
        let map_any_owner_v1_type = crate::core::mir::types::map_any_owner_v1_type_id();
        let has_map_any_owner_v1_type =
            self.program.type_catalog().iter().any(|(id, descriptor)| {
                id == &map_any_owner_v1_type
                    || descriptor.kind == MirTypeKind::MapAnyOwnerV1
                    || descriptor.layout == MirLayout::MapAnyOwnerV1
            });
        let has_map_any_owner_v1_receipt = self
            .program
            .functions()
            .values()
            .any(|function| function.ownership.has_checker_map_any_owner_actions());
        let ordinary_map_owner_lifecycle_v1_type =
            crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id();
        let has_ordinary_map_owner_lifecycle_v1_type =
            self.program.type_catalog().iter().any(|(id, descriptor)| {
                id == &ordinary_map_owner_lifecycle_v1_type
                    || descriptor.kind == MirTypeKind::OrdinaryMapOwnerLifecycleI32StringV1
                    || descriptor.layout == MirLayout::OrdinaryMapOwnerLifecycleI32StringV1
            });
        let has_ordinary_map_owner_lifecycle_v1_receipt =
            self.program.functions().values().any(|function| {
                function
                    .ownership
                    .has_checker_ordinary_map_owner_lifecycle_v1_actions()
            });
        let owned_get_type =
            crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id();
        let has_ordinary_map_owned_get_i32_string_v1_type =
            self.program.type_catalog().iter().any(|(id, descriptor)| {
                id == &owned_get_type
                    || descriptor.kind == MirTypeKind::OrdinaryMapOwnedGetI32StringV1
                    || descriptor.layout == MirLayout::OrdinaryMapOwnedGetI32StringV1
            });
        let has_ordinary_map_owned_get_i32_string_v1_receipt =
            self.program.functions().values().any(|function| {
                function
                    .ownership
                    .has_checker_ordinary_map_owned_get_i32_string_v1_actions()
            });
        let ordinary_map_owned_any_v2_type =
            crate::core::mir::types::ordinary_map_owned_any_v2_type_id();
        let has_ordinary_map_owned_any_v2_type =
            self.program.type_catalog().iter().any(|(id, descriptor)| {
                id == &ordinary_map_owned_any_v2_type
                    || descriptor.kind == MirTypeKind::OrdinaryMapOwnedAnyV2
                    || descriptor.layout == MirLayout::OrdinaryMapOwnedAnyV2
            });
        let has_ordinary_map_owned_any_v2_receipt =
            self.program.functions().values().any(|function| {
                function
                    .ownership
                    .has_checker_ordinary_map_owned_any_v2_actions()
            });
        if has_map_root_type != has_map_root_receipt {
            self.errors.push(NativeMirError::new(
                "MapRoot",
                "MapRoot TypeDesc presence must match Checker-owned MapRoot action receipts",
            ));
        }
        if has_map_any_owner_v1_type != has_map_any_owner_v1_receipt {
            self.errors.push(NativeMirError::new(
                "MapAnyOwner v1",
                "MapAnyOwner v1 TypeDesc presence must match its independent Checker receipts",
            ));
        }
        if has_ordinary_map_owner_lifecycle_v1_type != has_ordinary_map_owner_lifecycle_v1_receipt {
            self.errors.push(NativeMirError::new(
                "OrdinaryMapOwnerLifecycleV1",
                "ordinary Map owner lifecycle TypeDesc presence must match its independent Checker receipts",
            ));
        }
        if has_ordinary_map_owned_get_i32_string_v1_type
            != has_ordinary_map_owned_get_i32_string_v1_receipt
        {
            self.errors.push(NativeMirError::new(
                "OrdinaryMapOwnedGetI32StringV1",
                "ordinary Map owned-Get TypeDesc presence must match its independent Checker receipt",
            ));
        }
        if has_ordinary_map_owned_any_v2_type != has_ordinary_map_owned_any_v2_receipt {
            self.errors.push(NativeMirError::new(
                "OrdinaryMapOwnedAnyV2",
                "ordinary Map/Any owner-v2 TypeDesc presence must match its independent Checker receipt",
            ));
        }
        if has_map_root_type && has_map_any_owner_v1_type {
            self.errors.push(NativeMirError::new(
                "Map owner profile",
                "raw MapRoot and typed MapAnyOwner v1 descriptors cannot share one native MIR program",
            ));
        }
        let has_ordinary_map_owner_lifecycle_v1_profile =
            has_ordinary_map_owner_lifecycle_v1_type || has_ordinary_map_owner_lifecycle_v1_receipt;
        if has_ordinary_map_owner_lifecycle_v1_profile
            && (has_map_root_type
                || has_map_root_receipt
                || has_map_any_owner_v1_type
                || has_map_any_owner_v1_receipt)
        {
            self.errors.push(NativeMirError::new(
                "Map owner profile",
                "ordinary Map owner lifecycle receipts and TypeDesc cannot share authority with MapRoot or MapAnyOwner v1",
            ));
        }
        let has_ordinary_map_owned_get_i32_string_v1_profile =
            has_ordinary_map_owned_get_i32_string_v1_type
                || has_ordinary_map_owned_get_i32_string_v1_receipt;
        if has_ordinary_map_owned_get_i32_string_v1_profile
            && (has_map_root_type
                || has_map_root_receipt
                || has_map_any_owner_v1_type
                || has_map_any_owner_v1_receipt
                || has_ordinary_map_owner_lifecycle_v1_type
                || has_ordinary_map_owner_lifecycle_v1_receipt)
        {
            self.errors.push(NativeMirError::new(
                "Map owner profile",
                "ordinary Map owned-Get receipts and TypeDesc cannot share authority with MapRoot, MapAnyOwner v1, or ordinary Map owner lifecycle",
            ));
        }
        let has_ordinary_map_owned_any_v2_profile =
            has_ordinary_map_owned_any_v2_type || has_ordinary_map_owned_any_v2_receipt;
        if has_ordinary_map_owned_any_v2_profile
            && (has_map_root_type
                || has_map_root_receipt
                || has_map_any_owner_v1_type
                || has_map_any_owner_v1_receipt
                || has_ordinary_map_owner_lifecycle_v1_type
                || has_ordinary_map_owner_lifecycle_v1_receipt
                || has_ordinary_map_owned_get_i32_string_v1_type
                || has_ordinary_map_owned_get_i32_string_v1_receipt)
        {
            self.errors.push(NativeMirError::new(
                "Map owner profile",
                "ordinary Map/Any owner-v2 receipts and TypeDesc cannot share authority with another Map owner profile",
            ));
        }
        if has_ordinary_map_owner_lifecycle_v1_type {
            let owner =
                crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id();
            for operation in [
                crate::core::mir::types::MirGlueOperation::MoveOut,
                crate::core::mir::types::MirGlueOperation::Clone,
                crate::core::mir::types::MirGlueOperation::Drop,
            ] {
                if let Err(message) = self.program.type_catalog().validate_glue(&owner, operation) {
                    self.errors
                        .push(NativeMirError::new("OrdinaryMapOwnerLifecycleV1", message));
                }
            }
        }
        if has_ordinary_map_owned_get_i32_string_v1_type {
            let owner = crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id();
            let mut canonical_seen = false;
            for (id, descriptor) in self
                .program
                .type_catalog()
                .iter()
                .filter(|(id, descriptor)| {
                    *id == &owner
                        || descriptor.kind == MirTypeKind::OrdinaryMapOwnedGetI32StringV1
                        || descriptor.layout == MirLayout::OrdinaryMapOwnedGetI32StringV1
                })
            {
                let canonical = id == &owner
                    && descriptor.id == owner
                    && descriptor.kind == MirTypeKind::OrdinaryMapOwnedGetI32StringV1
                    && descriptor.layout == MirLayout::OrdinaryMapOwnedGetI32StringV1;
                canonical_seen |= canonical;
                if !canonical {
                    self.errors.push(NativeMirError::new(
                        "OrdinaryMapOwnedGetI32StringV1",
                        "ordinary Map owned-Get TypeDesc does not use its distinct canonical identity, kind, and layout",
                    ));
                }
            }
            if !canonical_seen {
                self.errors.push(NativeMirError::new(
                    "OrdinaryMapOwnedGetI32StringV1",
                    "ordinary Map owned-Get profile is missing its distinct canonical TypeDesc",
                ));
            }
            for operation in [
                crate::core::mir::types::MirGlueOperation::MoveOut,
                crate::core::mir::types::MirGlueOperation::Clone,
                crate::core::mir::types::MirGlueOperation::Drop,
            ] {
                if let Err(message) = self.program.type_catalog().validate_glue(&owner, operation) {
                    self.errors.push(NativeMirError::new(
                        "OrdinaryMapOwnedGetI32StringV1",
                        message,
                    ));
                }
            }
        }
        if has_ordinary_map_owned_any_v2_type {
            let owner = crate::core::mir::types::ordinary_map_owned_any_v2_type_id();
            let mut canonical_seen = false;
            for (id, descriptor) in self
                .program
                .type_catalog()
                .iter()
                .filter(|(id, descriptor)| {
                    *id == &owner
                        || descriptor.kind == MirTypeKind::OrdinaryMapOwnedAnyV2
                        || descriptor.layout == MirLayout::OrdinaryMapOwnedAnyV2
                })
            {
                let canonical = id == &owner
                    && descriptor.id == owner
                    && descriptor.kind == MirTypeKind::OrdinaryMapOwnedAnyV2
                    && descriptor.layout == MirLayout::OrdinaryMapOwnedAnyV2;
                canonical_seen |= canonical;
                if !canonical {
                    self.errors.push(NativeMirError::new(
                        "OrdinaryMapOwnedAnyV2",
                        "ordinary Map/Any owner-v2 TypeDesc does not use its distinct canonical identity, kind, and layout",
                    ));
                }
            }
            if !canonical_seen {
                self.errors.push(NativeMirError::new(
                    "OrdinaryMapOwnedAnyV2",
                    "ordinary Map/Any owner-v2 profile is missing its distinct canonical TypeDesc",
                ));
            }
            for operation in [
                crate::core::mir::types::MirGlueOperation::MoveOut,
                crate::core::mir::types::MirGlueOperation::Clone,
                crate::core::mir::types::MirGlueOperation::Drop,
            ] {
                if let Err(message) = self.program.type_catalog().validate_glue(&owner, operation) {
                    self.errors
                        .push(NativeMirError::new("OrdinaryMapOwnedAnyV2", message));
                }
            }
        }
        for function in self.program.functions().values() {
            if let Err(message) = self
                .program
                .type_catalog()
                .validate_managed_any_function(function)
            {
                self.errors
                    .push(NativeMirError::new(function.owner.0.clone(), message));
            }
            let symbol =
                match super::actor_status::actor_native_symbol(self.program, &function.owner) {
                    Ok(symbol) => symbol,
                    Err(message) => {
                        self.errors
                            .push(NativeMirError::new(function.owner.0.clone(), message));
                        continue;
                    }
                };
            if !self.symbols.insert(symbol.clone()) {
                self.errors.push(NativeMirError::new(
                    function.owner.0.clone(),
                    format!("native symbol '{symbol}' is duplicated"),
                ));
            }
            self.validate_function(function);
        }
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(self.errors)
        }
    }

    /// Recheck both sealed generic ManagedAny profiles before native admission
    /// can reach LLVM declarations or bodies.
    fn validate_managed_any_generic_instances(&mut self) {
        if let Err(messages) = self.program.validate_managed_any_identity_instances() {
            for message in messages {
                self.errors.push(NativeMirError::new(
                    "ManagedAny generic identity",
                    format!("pre-emission instance/call receipt validation failed: {message}"),
                ));
            }
        }
        if let Err(messages) = self.program.validate_managed_any_trap_cleanup_instances() {
            for message in messages {
                self.errors.push(NativeMirError::new(
                    "ManagedAny generic trap cleanup",
                    format!("pre-emission instance/call receipt validation failed: {message}"),
                ));
            }
        }
        let catalog = self.program.type_catalog();
        for instance in self.program.instances().values() {
            let is_identity = matches!(
                &instance.contract,
                crate::core::mir::MirGenericInstanceContract::ManagedAnyIdentity
            );
            let is_trap_cleanup = matches!(
                &instance.contract,
                crate::core::mir::MirGenericInstanceContract::ManagedAnyTrapCleanup { .. }
            );
            let is_managed_any_profile = is_identity || is_trap_cleanup;
            let carries_managed_any_source = instance
                .arguments
                .iter()
                .any(|argument| catalog.contains_dynamic_or_managed_any(argument));
            let Some(function) = self.program.functions().get(&instance.function) else {
                if carries_managed_any_source || is_managed_any_profile {
                    self.errors.push(NativeMirError::new(
                        instance.id.to_string(),
                        "ManagedAny generic instance has no executable target for recursive Any preflight",
                    ));
                }
                continue;
            };
            let carries_managed_any_abi = catalog.contains_dynamic_or_managed_any(&function.result)
                || function.parameters.iter().any(|parameter| {
                    function
                        .values
                        .get(parameter)
                        .is_some_and(|value| catalog.contains_dynamic_or_managed_any(&value.ty))
                });
            let exact_profile = match &instance.contract {
                crate::core::mir::MirGenericInstanceContract::ManagedAnyIdentity => catalog
                    .validate_managed_any_identity_instance(instance, function)
                    .is_ok(),
                crate::core::mir::MirGenericInstanceContract::ManagedAnyTrapCleanup { .. } => {
                    catalog
                        .validate_managed_any_trap_cleanup_instance(instance, function)
                        .is_ok()
                }
                _ => false,
            };
            if (carries_managed_any_source || carries_managed_any_abi) && !exact_profile {
                self.errors.push(NativeMirError::new(
                    instance.id.to_string(),
                    "native generic instances with recursive DynamicAny/ManagedAny source or ABI types require an exact sealed ManagedAnyIdentity or ManagedAnyTrapCleanup profile",
                ));
                continue;
            }
            if !is_managed_any_profile {
                continue;
            }
            let subject = instance.id.to_string();
            let target_instances = self
                .program
                .instances()
                .values()
                .filter(|candidate| candidate.function == instance.function)
                .count();
            if target_instances != 1 {
                self.errors.push(NativeMirError::new(
                    subject.clone(),
                    "ManagedAny generic target is not uniquely bound by the instance table",
                ));
                continue;
            }
            let validation = match &instance.contract {
                crate::core::mir::MirGenericInstanceContract::ManagedAnyIdentity => {
                    catalog.validate_managed_any_identity_instance(instance, function)
                }
                crate::core::mir::MirGenericInstanceContract::ManagedAnyTrapCleanup { .. } => {
                    catalog.validate_managed_any_trap_cleanup_instance(instance, function)
                }
                _ => unreachable!("profile discriminator was checked above"),
            };
            if let Err(message) = validation {
                self.errors.push(NativeMirError::new(
                    subject,
                    format!("ManagedAny generic receipt/shape validation failed: {message}"),
                ));
            }
        }
    }

    fn validate_function(&mut self, function: &MirFunction) {
        let catalog = self.program.type_catalog();
        let has_managed_any = catalog.contains_managed_any(&function.result)
            || function
                .values
                .values()
                .any(|v| catalog.contains_managed_any(&v.ty));
        if has_managed_any {
            if let Err(errors) = function.validate() {
                for error in errors {
                    self.errors.push(NativeMirError::new(
                        function.owner.0.clone(),
                        format!("ManagedAny sealed receipt validation failed: {error}"),
                    ));
                }
            }
        }
        let ordinary_map_owner_type =
            crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id();
        let ordinary_map_owned_get_type =
            crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id();
        let ordinary_map_owned_any_v2_type =
            crate::core::mir::types::ordinary_map_owned_any_v2_type_id();
        let has_ordinary_map_owner_lifecycle_v1 = function.result == ordinary_map_owner_type
            || function
                .values
                .values()
                .any(|value| value.ty == ordinary_map_owner_type)
            || function
                .ownership
                .has_checker_ordinary_map_owner_lifecycle_v1_actions()
            || function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        MirInstructionKind::OrdinaryMapOwnerLifecycleNew { .. }
                            | MirInstructionKind::OrdinaryMapOwnerLifecycleSet { .. }
                            | MirInstructionKind::OrdinaryMapOwnerLifecycleRemove { .. }
                            | MirInstructionKind::OrdinaryMapOwnerLifecycleSize { .. }
                            | MirInstructionKind::OrdinaryMapOwnerLifecycleContains { .. }
                            | MirInstructionKind::OrdinaryMapOwnerLifecycleDrop { .. }
                    )
                })
            });
        let has_ordinary_map_owned_get_i32_string_v1 = function.result
            == ordinary_map_owned_get_type
            || function
                .values
                .values()
                .any(|value| value.ty == ordinary_map_owned_get_type)
            || function
                .ownership
                .has_checker_ordinary_map_owned_get_i32_string_v1_actions()
            || function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
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
                })
            });
        let has_ordinary_map_owned_any_v2 = function.result == ordinary_map_owned_any_v2_type
            || function
                .values
                .values()
                .any(|value| value.ty == ordinary_map_owned_any_v2_type)
            || function
                .ownership
                .has_checker_ordinary_map_owned_any_v2_actions()
            || function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        MirInstructionKind::OrdinaryMapOwnedAnyV2New { .. }
                            | MirInstructionKind::OrdinaryMapOwnedAnyV2Set { .. }
                            | MirInstructionKind::OrdinaryMapOwnedAnyV2Remove { .. }
                            | MirInstructionKind::OrdinaryMapOwnedAnyV2Size { .. }
                            | MirInstructionKind::OrdinaryMapOwnedAnyV2Contains { .. }
                            | MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { .. }
                            | MirInstructionKind::OrdinaryMapOwnedAnyV2Values { .. }
                            | MirInstructionKind::OrdinaryMapOwnedAnyV2IsEmpty { .. }
                            | MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { .. }
                    )
                })
            });
        let has_map_root = function.result == crate::core::mir::types::map_root_type_id()
            || function.result == crate::core::mir::types::map_any_owner_v1_type_id()
            || has_ordinary_map_owner_lifecycle_v1
            || function.values.values().any(|value| {
                value.ty == crate::core::mir::types::map_root_type_id()
                    || value.ty == crate::core::mir::types::map_any_owner_v1_type_id()
            })
            || function.ownership.has_checker_map_root_actions()
            || function.ownership.has_checker_map_any_owner_actions()
            || function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        MirInstructionKind::MapRootNew { .. }
                            | MirInstructionKind::MapRootSet { .. }
                            | MirInstructionKind::MapRootRemove { .. }
                            | MirInstructionKind::MapRootSize { .. }
                            | MirInstructionKind::MapRootContains { .. }
                            | MirInstructionKind::MapRootDrop { .. }
                    )
                })
            });
        if has_map_root {
            if let Err(errors) = function.validate() {
                let profile = if has_ordinary_map_owner_lifecycle_v1 {
                    "OrdinaryMapOwnerLifecycleV1"
                } else if function.ownership.has_checker_map_any_owner_actions() {
                    "MapAnyOwner v1"
                } else {
                    "MapRoot"
                };
                for error in errors {
                    self.errors.push(NativeMirError::new(
                        function.owner.0.clone(),
                        format!("{profile} receipt/shape validation failed: {error}"),
                    ));
                }
            }
            let root = if has_ordinary_map_owner_lifecycle_v1 {
                ordinary_map_owner_type
            } else if function.ownership.has_checker_map_any_owner_actions() {
                crate::core::mir::types::map_any_owner_v1_type_id()
            } else {
                crate::core::mir::types::map_root_type_id()
            };
            for operation in [
                crate::core::mir::types::MirGlueOperation::MoveOut,
                crate::core::mir::types::MirGlueOperation::Drop,
            ] {
                if let Err(message) = catalog.validate_glue(&root, operation) {
                    self.errors
                        .push(NativeMirError::new(function.owner.0.clone(), message));
                }
            }
            let clone_glue =
                catalog.validate_glue(&root, crate::core::mir::types::MirGlueOperation::Clone);
            if has_ordinary_map_owner_lifecycle_v1 {
                if let Err(message) = clone_glue {
                    self.errors
                        .push(NativeMirError::new(function.owner.0.clone(), message));
                }
            } else if clone_glue.is_ok() {
                self.errors.push(NativeMirError::new(
                    function.owner.0.clone(),
                    "canonical Map owner must not expose generic Clone glue",
                ));
            }
        }
        if has_ordinary_map_owned_get_i32_string_v1 {
            if let Err(errors) = function.validate() {
                for error in errors {
                    self.errors.push(NativeMirError::new(
                        function.owner.0.clone(),
                        format!(
                            "OrdinaryMapOwnedGetI32StringV1 receipt/shape validation failed: {error}"
                        ),
                    ));
                }
            }
            for operation in [
                crate::core::mir::types::MirGlueOperation::MoveOut,
                crate::core::mir::types::MirGlueOperation::Clone,
                crate::core::mir::types::MirGlueOperation::Drop,
            ] {
                if let Err(message) = catalog.validate_glue(&ordinary_map_owned_get_type, operation)
                {
                    self.errors
                        .push(NativeMirError::new(function.owner.0.clone(), message));
                }
            }
        }
        if has_ordinary_map_owned_any_v2 {
            if let Err(errors) = function.validate() {
                for error in errors {
                    self.errors.push(NativeMirError::new(
                        function.owner.0.clone(),
                        format!("OrdinaryMapOwnedAnyV2 receipt/shape validation failed: {error}"),
                    ));
                }
            }
            for operation in [
                crate::core::mir::types::MirGlueOperation::MoveOut,
                crate::core::mir::types::MirGlueOperation::Clone,
                crate::core::mir::types::MirGlueOperation::Drop,
            ] {
                if let Err(message) =
                    catalog.validate_glue(&ordinary_map_owned_any_v2_type, operation)
                {
                    self.errors
                        .push(NativeMirError::new(function.owner.0.clone(), message));
                }
            }
        }
        for parameter in &function.parameters {
            self.validate_value(function, parameter, "parameter");
            self.reject_reference_callable_boundary(function, parameter, "reference parameter");
        }
        self.validate_signature_type(function, &function.result, "result", true);
        self.reject_reference_type(&function.result, "reference result");
        if crate::core::mir::is_owned_string_return_candidate(
            function,
            self.program.functions(),
            catalog,
        ) {
            if let Err(message) = crate::core::mir::validate_owned_string_return_shape(
                function,
                self.program.functions(),
                catalog,
            ) {
                self.errors
                    .push(NativeMirError::new(function.owner.0.clone(), message));
            }
        }

        for block in function.blocks.values() {
            for parameter in &block.parameters {
                self.validate_value(function, &parameter.value, "block parameter");
            }
            for instruction in &block.instructions {
                self.validate_instruction(
                    function,
                    instruction_kind(instruction),
                    instruction.id.as_str(),
                );
            }
            self.validate_terminator(function, &block.terminator, &block.id);
        }

        for event in &function.ownership.events {
            if event.kind == crate::core::mir::MirOwnershipEventKind::TransferSession {
                let matched = function.blocks.values().any(|block| {
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
                            } if Some(event.point.0.as_str()) == point
                                && Some(endpoint) == event.value.as_ref()
                                && !contract.terminal
                        )
                    })
                });
                if !matched {
                    self.errors.push(NativeMirError::new(
                        function.owner.0.clone(),
                        format!(
                            "ownership effect '{}' has no scalar native MIR contract",
                            event.kind.as_str()
                        ),
                    ));
                }
            } else if matches!(
                event.kind,
                crate::core::mir::MirOwnershipEventKind::TransferChild
                    | crate::core::mir::MirOwnershipEventKind::BorrowMut
            ) {
                self.errors.push(NativeMirError::new(
                    function.owner.0.clone(),
                    format!(
                        "ownership effect '{}' has no scalar native MIR contract",
                        event.kind.as_str()
                    ),
                ));
            }
            if let Some(value) = &event.value {
                self.validate_value(function, value, "ownership event");
            }
        }

        // Keep this local use to make the validator's TypeDesc dependency
        // explicit: the native backend never reconstructs ownership from an
        // LLVM type.  The detailed operation checks below use the same catalog.
        let _ = catalog;
    }

    fn reject_reference_callable_boundary(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
    ) {
        let Some(info) = function.values.get(value) else {
            return;
        };
        self.reject_reference_type(&info.ty, subject);
    }

    fn reject_reference_type(&mut self, ty: &crate::core::ResolvedTypeId, subject: &str) {
        if self
            .program
            .type_catalog()
            .get(ty)
            .is_some_and(|desc| matches!(&desc.kind, MirTypeKind::Reference { .. }))
        {
            self.errors.push(NativeMirError::new(
                subject,
                "borrowed pointer cannot cross the native callable ABI in the local-borrow contract",
            ));
        }
    }

    fn validate_map_root_value(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
        role: &str,
    ) {
        let root_type = if function.ownership.has_checker_map_any_owner_actions() {
            crate::core::mir::types::map_any_owner_v1_type_id()
        } else {
            crate::core::mir::types::map_root_type_id()
        };
        let valid = function
            .values
            .get(value)
            .is_some_and(|value| value.ty == root_type)
            && self
                .program
                .type_catalog()
                .validate_glue(
                    &root_type,
                    crate::core::mir::types::MirGlueOperation::MoveOut,
                )
                .is_ok();
        if !valid {
            self.errors.push(NativeMirError::new(
                subject,
                format!("Map owner {role} does not use the profile's canonical TypeDesc"),
            ));
        }
    }

    fn validate_map_root_i32(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
        role: &str,
    ) {
        let ty = function.values.get(value).map(|value| value.ty.clone());
        let valid = ty.is_some_and(|ty| {
            self.program
                .type_catalog()
                .get(&ty)
                .is_some_and(|descriptor| {
                    descriptor.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::I32)
                        && self
                            .program
                            .type_catalog()
                            .validate_copy_scalar(&ty)
                            .is_ok()
                })
        });
        if !valid {
            self.errors.push(NativeMirError::new(
                subject,
                format!("MapRoot {role} is not the canonical i32 TypeDesc"),
            ));
        }
    }

    fn validate_map_root_bool(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
    ) {
        let ty = function.values.get(value).map(|value| value.ty.clone());
        let valid = ty.is_some_and(|ty| {
            self.program
                .type_catalog()
                .get(&ty)
                .is_some_and(|descriptor| {
                    descriptor.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::Bool)
                        && self
                            .program
                            .type_catalog()
                            .validate_copy_scalar(&ty)
                            .is_ok()
                })
        });
        if !valid {
            self.errors.push(NativeMirError::new(
                subject,
                "MapRoot Contains result is not the canonical bool TypeDesc",
            ));
        }
    }

    fn validate_map_root_set_payload(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
    ) {
        let ty = function.values.get(value).map(|value| value.ty.clone());
        let valid = ty.is_some_and(|ty| {
            self.program
                .type_catalog()
                .get(&ty)
                .is_some_and(|descriptor| match descriptor.kind {
                    MirTypeKind::Primitive(crate::core::PrimitiveType::I32) => self
                        .program
                        .type_catalog()
                        .validate_copy_scalar(&ty)
                        .is_ok(),
                    MirTypeKind::Primitive(crate::core::PrimitiveType::String) => self
                        .program
                        .type_catalog()
                        .validate_owned_string(&ty)
                        .is_ok(),
                    _ => false,
                })
        });
        if !valid {
            self.errors.push(NativeMirError::new(
                subject,
                "MapRoot Set value is outside the receipted i32/String TypeDesc profile",
            ));
        }
    }

    fn validate_ordinary_map_owner_value(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
        role: &str,
    ) {
        let owner_type =
            crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id();
        let valid = function
            .values
            .get(value)
            .is_some_and(|value| value.ty == owner_type)
            && self
                .program
                .type_catalog()
                .validate_glue(
                    &owner_type,
                    crate::core::mir::types::MirGlueOperation::MoveOut,
                )
                .is_ok();
        if !valid {
            self.errors.push(NativeMirError::new(
                subject,
                format!(
                    "ordinary Map owner lifecycle {role} does not use its independent canonical TypeDesc"
                ),
            ));
        }
    }

    fn validate_ordinary_map_string_value(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
        role: &str,
    ) {
        let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        let valid = function
            .values
            .get(value)
            .is_some_and(|value| value.ty == string_type)
            && self
                .program
                .type_catalog()
                .validate_owned_string(&string_type)
                .is_ok();
        if !valid {
            self.errors.push(NativeMirError::new(
                subject,
                format!("ordinary Map {role} is not the canonical owned String TypeDesc"),
            ));
        }
    }

    fn validate_ordinary_map_set_payload(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
    ) {
        let ty = function.values.get(value).map(|value| value.ty.clone());
        let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        let valid = ty.as_ref() == Some(&i32_type)
            && self
                .program
                .type_catalog()
                .validate_copy_scalar(&i32_type)
                .is_ok()
            || ty.as_ref() == Some(&string_type)
                && self
                    .program
                    .type_catalog()
                    .validate_owned_string(&string_type)
                    .is_ok();
        if !valid {
            self.errors.push(NativeMirError::new(
                subject,
                "ordinary Map Set payload is outside the independent i32/String TypeDesc profile",
            ));
        }
    }

    fn validate_ordinary_map_owned_get_value(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
        role: &str,
    ) {
        let owner_type = crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id();
        let valid = function
            .values
            .get(value)
            .is_some_and(|value| value.ty == owner_type)
            && self
                .program
                .type_catalog()
                .validate_glue(
                    &owner_type,
                    crate::core::mir::types::MirGlueOperation::MoveOut,
                )
                .is_ok();
        if !valid {
            self.errors.push(NativeMirError::new(
                subject,
                format!(
                    "ordinary Map owned-Get {role} does not use its distinct canonical TypeDesc"
                ),
            ));
        }
    }

    fn validate_ordinary_map_owned_get_i32(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
        role: &str,
    ) {
        let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        let valid = function
            .values
            .get(value)
            .is_some_and(|value| value.ty == i32_type)
            && self
                .program
                .type_catalog()
                .validate_copy_scalar(&i32_type)
                .is_ok();
        if !valid {
            self.errors.push(NativeMirError::new(
                subject,
                format!("ordinary Map owned-Get {role} is not the canonical i32 TypeDesc"),
            ));
        }
    }

    fn validate_ordinary_map_owned_get_bool(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
    ) {
        let bool_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Bool);
        let valid = function
            .values
            .get(value)
            .is_some_and(|value| value.ty == bool_type)
            && self
                .program
                .type_catalog()
                .validate_copy_scalar(&bool_type)
                .is_ok();
        if !valid {
            self.errors.push(NativeMirError::new(
                subject,
                "ordinary Map owned-Get Contains result is not the canonical bool TypeDesc",
            ));
        }
    }

    fn validate_ordinary_map_owned_get_set_payload(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
    ) {
        let value_type = function.values.get(value).map(|value| value.ty.clone());
        let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        let valid = value_type.as_ref() == Some(&i32_type)
            && self
                .program
                .type_catalog()
                .validate_copy_scalar(&i32_type)
                .is_ok()
            || value_type.as_ref() == Some(&string_type)
                && self
                    .program
                    .type_catalog()
                    .validate_owned_string(&string_type)
                    .is_ok();
        if !valid {
            self.errors.push(NativeMirError::new(
                subject,
                "ordinary Map owned-Get Set value is outside the exact i32/String TypeDesc profile",
            ));
        }
    }

    fn validate_ordinary_map_owned_any_v2_value(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
        role: &str,
    ) {
        let owner_type = crate::core::mir::types::ordinary_map_owned_any_v2_type_id();
        let valid = function
            .values
            .get(value)
            .is_some_and(|value| value.ty == owner_type)
            && self
                .program
                .type_catalog()
                .validate_glue(
                    &owner_type,
                    crate::core::mir::types::MirGlueOperation::MoveOut,
                )
                .is_ok();
        if !valid {
            self.errors.push(NativeMirError::new(
                subject,
                format!(
                    "ordinary Map/Any owner-v2 {role} does not use its distinct canonical TypeDesc"
                ),
            ));
        }
    }

    fn validate_ordinary_map_owned_any_v2_values_list(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
        role: &str,
    ) {
        let values_type = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
        let valid = function
            .values
            .get(value)
            .is_some_and(|value| value.ty == values_type)
            && super::ordinary_map_owned_any_v2::validate_ordinary_map_owned_any_v2_values_list_type(
                self.program.type_catalog(),
                &values_type,
            )
            .is_ok();
        if !valid {
            self.errors.push(NativeMirError::new(
                subject,
                format!(
                    "ordinary Map/Any owner-v2 Values {role} does not use its dedicated canonical List TypeDesc"
                ),
            ));
        }
    }

    fn validate_ordinary_map_owned_any_v2_get_any_receipts(&mut self) {
        let subject = "OrdinaryMapOwnedAnyV2.GetAny";
        let get_any_instruction_count = self
            .program
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| {
                matches!(
                    instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { .. }
                )
            })
            .count();

        // New/Drop-only V2 programs do not require a payload-selection
        // receipt.  Once the Core selection accessor returns one, however,
        // validate the complete result-receipt census before LLVM declarations
        // are created.  A MIR GetAny without that receipt is always rejected.
        let selection = match self
            .program
            .ordinary_map_owned_any_v2_payload_selection_receipt()
        {
            Ok(Some(receipt)) => {
                if let Err(message) = receipt.validate_against_program(self.program) {
                    self.errors.push(NativeMirError::new(subject, message));
                    return;
                }
                Some(receipt)
            }
            Ok(None) if get_any_instruction_count == 0 => None,
            Ok(None) => {
                self.errors.push(NativeMirError::new(
                    subject,
                    "ordinary Map/Any owner-v2 GetAny has no Checker payload selection receipt",
                ));
                return;
            }
            Err(_message) if get_any_instruction_count == 0 => {
                // A malformed selection for an unrelated V2 operation is
                // diagnosed by that operation's own admission path.  It
                // grants no GetAny authority here.
                None
            }
            Err(message) => {
                self.errors.push(NativeMirError::new(subject, message));
                return;
            }
        };
        let Some(_selection) = selection else {
            return;
        };

        let receipts = match self
            .program
            .ordinary_map_owned_any_v2_get_any_result_receipts()
        {
            Ok(receipts) => receipts,
            Err(message) => {
                self.errors.push(NativeMirError::new(subject, message));
                return;
            }
        };
        if receipts.len() != get_any_instruction_count {
            self.errors.push(NativeMirError::new(
                subject,
                format!(
                    "Core GetAny result receipt count {} disagrees with exact MIR instruction count {}",
                    receipts.len(), get_any_instruction_count
                ),
            ));
            return;
        }

        let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        for receipt in receipts {
            if let Err(message) = receipt
                .validate()
                .and_then(|()| receipt.validate_against_program(self.program))
            {
                self.errors.push(NativeMirError::new(
                    receipt.instruction.as_str(),
                    format!("pre-emission Core result receipt validation failed: {message}"),
                ));
                continue;
            }
            let descriptor = self.program.type_catalog().get(&receipt.miss_payload_type);
            let descriptor_digest = descriptor.map(|descriptor| {
                blake3::hash(descriptor.canonical_text().as_bytes())
                    .to_hex()
                    .to_string()
            });
            if receipt.miss_payload_type != i32_type
                || descriptor.is_none_or(|descriptor| {
                    descriptor.id != i32_type
                        || descriptor.kind
                            != MirTypeKind::Primitive(crate::core::PrimitiveType::I32)
                        || descriptor.layout != MirLayout::Scalar
                        || descriptor.ownership != MirOwnership::Copy
                })
                || descriptor_digest.as_deref()
                    != Some(receipt.miss_payload_type_desc_digest.as_str())
                || self
                    .program
                    .type_catalog()
                    .validate_copy_scalar(&receipt.miss_payload_type)
                    .is_err()
            {
                self.errors.push(NativeMirError::new(
                    receipt.instruction.as_str(),
                    "Core GetAny miss receipt does not authorize the exact canonical i32 TypeDesc and Copy glue",
                ));
            }
        }
    }

    fn validate_ordinary_map_owned_any_v2_i32(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
        role: &str,
    ) {
        let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        let valid = function
            .values
            .get(value)
            .is_some_and(|value| value.ty == i32_type)
            && self
                .program
                .type_catalog()
                .validate_copy_scalar(&i32_type)
                .is_ok();
        if !valid {
            self.errors.push(NativeMirError::new(
                subject,
                format!("ordinary Map/Any owner-v2 {role} is not the canonical i32 TypeDesc"),
            ));
        }
    }

    fn validate_ordinary_map_owned_any_v2_unpack_result(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
    ) {
        let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        let valid = function.values.get(value).is_some_and(|value| {
            if value.ty == i32_type {
                self.program
                    .type_catalog()
                    .validate_copy_scalar(&i32_type)
                    .is_ok()
            } else if value.ty == string_type {
                let catalog = self.program.type_catalog();
                catalog.validate_owned_string(&string_type).is_ok()
                    && catalog
                        .validate_glue(
                            &string_type,
                            crate::core::mir::types::MirGlueOperation::Clone,
                        )
                        .is_ok()
                    && catalog
                        .validate_glue(
                            &string_type,
                            crate::core::mir::types::MirGlueOperation::Drop,
                        )
                        .is_ok()
            } else if super::ordinary_map_owned_any_v2::v2_values_is_label_type(
                self.program.type_catalog(),
                &value.ty,
            ) {
                [
                    crate::core::mir::types::MirGlueOperation::Clone,
                    crate::core::mir::types::MirGlueOperation::Drop,
                ]
                .into_iter()
                .all(|operation| {
                    self.program
                        .type_catalog()
                        .validate_glue(&value.ty, operation)
                        .is_ok()
                })
            } else {
                false
            }
        });
        if !valid {
            self.errors.push(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 unpack result is neither canonical i32, owned String, nor a fully-glued Label { text: string }",
            ));
        }
    }

    fn validate_ordinary_map_owned_any_v2_bool(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
    ) {
        let bool_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Bool);
        let valid = function
            .values
            .get(value)
            .is_some_and(|value| value.ty == bool_type)
            && self
                .program
                .type_catalog()
                .validate_copy_scalar(&bool_type)
                .is_ok();
        if !valid {
            self.errors.push(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Contains result is not the canonical bool TypeDesc",
            ));
        }
    }

    fn validate_ordinary_map_owned_any_v2_get_any_result(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        subject: &str,
    ) {
        let Some(result_value) = function.values.get(result) else {
            self.errors.push(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 GetAny result is absent from the MIR value catalog",
            ));
            return;
        };
        let result_ty = crate::core::mir::types::ordinary_map_owned_any_v2_get_result_type_id();
        if result_value.ty != result_ty {
            self.errors.push(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 GetAny result is not the exact Core (bool, DynamicAny) TypeDesc",
            ));
            return;
        }
        let payload_ty = crate::core::mir::types::ordinary_map_owned_any_v2_payload_type_id();
        let catalog = self.program.type_catalog();
        for ty in [&result_ty, &payload_ty] {
            for operation in [
                crate::core::mir::types::MirGlueOperation::MoveOut,
                crate::core::mir::types::MirGlueOperation::Clone,
                crate::core::mir::types::MirGlueOperation::Drop,
            ] {
                if let Err(message) = catalog.validate_glue(ty, operation) {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
        }
        if catalog.get(&payload_ty).is_none_or(|descriptor| {
            descriptor.id != payload_ty
                || descriptor.kind != MirTypeKind::OrdinaryMapOwnedAnyV2Payload
                || descriptor.layout != MirLayout::OrdinaryMapOwnedAnyV2Payload
                || descriptor.abi != crate::core::mir::types::MirAbiClass::OpaqueHandle
        }) {
            self.errors.push(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 GetAny payload does not use the distinct canonical V2 owner TypeDesc",
            ));
        }
    }

    fn validate_ordinary_map_owned_any_v2_set_payload(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
    ) {
        let Some(value_type) = function.values.get(value).map(|value| value.ty.clone()) else {
            self.errors.push(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Set value is absent from MIR value catalog",
            ));
            return;
        };
        let catalog = self.program.type_catalog();
        let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        let valid_i32 = value_type == i32_type && catalog.validate_copy_scalar(&i32_type).is_ok();
        let valid_string =
            value_type == string_type && catalog.validate_owned_string(&string_type).is_ok();
        let valid_label = catalog.get(&value_type).is_some_and(|descriptor| {
            let MirLayout::Record { fields, .. } = &descriptor.layout else {
                return false;
            };
            let string_type =
                crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
            fields.len() == 1
                && fields[0].name == "text"
                && fields[0].ty == string_type
                && catalog.validate_owned_string(&string_type).is_ok()
                && validate_native_non_copy_record_type(catalog, &value_type).is_ok()
        });
        if !valid_i32 && !valid_string && !valid_label {
            self.errors.push(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Set value is outside the canonical i32/String/Label TypeDesc profile",
            ));
        }
    }

    fn validate_ordinary_map_owned_get_result(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        ok_is_string: bool,
        subject: &str,
    ) {
        let valid = function.values.get(value).is_some_and(|value| {
            let catalog = self.program.type_catalog();
            let Some(descriptor) = catalog.get(&value.ty) else {
                return false;
            };
            if descriptor.kind != MirTypeKind::Result {
                return false;
            }
            let MirLayout::Result {
                ok,
                error,
                variants,
            } = &descriptor.layout
            else {
                return false;
            };
            let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
            let string_type =
                crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
            let ok_is_canonical = if ok_is_string {
                ok == &string_type && catalog.validate_owned_string(ok).is_ok()
            } else {
                ok == &i32_type && catalog.validate_copy_scalar(ok).is_ok()
            };
            let error_is_canonical =
                error == &i32_type && catalog.validate_copy_scalar(error).is_ok();
            if !ok_is_canonical || !error_is_canonical || variants.len() != 2 {
                return false;
            }
            let Some(ok_variant) = variants.iter().find(|variant| variant.name == "Ok") else {
                return false;
            };
            let Some(err_variant) = variants.iter().find(|variant| variant.name == "Err") else {
                return false;
            };
            let ([ok_field], [err_field]) =
                (ok_variant.fields.as_slice(), err_variant.fields.as_slice())
            else {
                return false;
            };
            ok_variant.id.0 == "builtin:variant:Result::Ok"
                && ok_variant.discriminant == 0
                && ok_field.id.0 == "builtin:variant:Result::Ok/payload:0"
                && ok_field.ty == *ok
                && err_variant.id.0 == "builtin:variant:Result::Err"
                && err_variant.discriminant == 1
                && err_field.id.0 == "builtin:variant:Result::Err/payload:0"
                && err_field.ty == *error
        });
        if !valid {
            self.errors.push(NativeMirError::new(
                subject,
                format!(
                    "ordinary Map owned-Get result is not canonical Result<{}, i32>",
                    if ok_is_string { "String" } else { "i32" }
                ),
            ));
        }
    }

    fn validate_ordinary_map_owned_get_values_result(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
    ) {
        let valid = function.values.get(value).is_some_and(|value| {
            let catalog = self.program.type_catalog();
            let Some(descriptor) = catalog.get(&value.ty) else {
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
                && catalog
                    .validate_list_glue(
                        &value.ty,
                        crate::core::mir::types::MirGlueOperation::MoveOut,
                    )
                    .is_ok()
        });
        if !valid {
            self.errors.push(NativeMirError::new(
                subject,
                "ordinary Map owned-Get Values result is not the canonical move-owned List<ManagedAny> TypeDesc",
            ));
        }
    }

    fn validate_value(&mut self, function: &MirFunction, value: &MirValueId, subject: &str) {
        let Some(info) = function.values.get(value) else {
            self.errors.push(NativeMirError::new(
                value.to_string(),
                format!("{subject} is absent from MIR value catalog"),
            ));
            return;
        };
        self.validate_signature_type(function, &info.ty, &format!("{subject} '{value}'"), false);
    }

    fn validate_signature_type(
        &mut self,
        function: &MirFunction,
        ty: &crate::core::ResolvedTypeId,
        subject: &str,
        _allow_unit_result: bool,
    ) {
        let Some(desc) = self.program.type_catalog().get(ty) else {
            self.errors.push(NativeMirError::new(
                subject,
                format!("type '{}' is absent from MIR TypeDesc catalog", ty.as_str()),
            ));
            return;
        };
        if matches!(
            desc.kind,
            MirTypeKind::ActorHandle { .. } | MirTypeKind::ActorState { .. }
        ) || matches!(desc.layout, MirLayout::ActorState { .. })
        {
            if let Err(error) = super::actor_status::validate_actor_signature_type(
                self.program,
                function,
                ty,
                subject,
            ) {
                self.errors.push(error);
            }
            return;
        }
        let is_list = matches!(desc.layout, MirLayout::List { .. });
        let is_set = matches!(desc.layout, MirLayout::Set { .. });
        let is_session = desc.glue.move_out == MirGlueKind::Session;
        let is_ordinary_map_owner = desc.kind == MirTypeKind::OrdinaryMapOwnerLifecycleI32StringV1
            || desc.layout == MirLayout::OrdinaryMapOwnerLifecycleI32StringV1;
        let is_ordinary_map_owned_get = desc.kind == MirTypeKind::OrdinaryMapOwnedGetI32StringV1
            || desc.layout == MirLayout::OrdinaryMapOwnedGetI32StringV1;
        let is_ordinary_map_owned_any_v2 = desc.kind == MirTypeKind::OrdinaryMapOwnedAnyV2
            || desc.layout == MirLayout::OrdinaryMapOwnedAnyV2;
        let is_map_owner = desc.kind == MirTypeKind::MapRoot
            || desc.layout == MirLayout::MapRoot
            || desc.kind == MirTypeKind::MapAnyOwnerV1
            || desc.layout == MirLayout::MapAnyOwnerV1
            || is_ordinary_map_owner
            || is_ordinary_map_owned_get
            || is_ordinary_map_owned_any_v2;
        let is_reference = matches!(&desc.kind, MirTypeKind::Reference { mutable: false });
        let is_owned_string = matches!(
            &desc.kind,
            MirTypeKind::Primitive(crate::core::PrimitiveType::String)
        );
        let is_managed_any = desc.kind == MirTypeKind::ManagedAnyI32StringV1
            || desc.layout == MirLayout::ManagedAnyI32StringV1;
        let is_record = matches!(desc.layout, MirLayout::Record { .. });
        let is_variant = matches!(
            desc.layout,
            MirLayout::Option { .. } | MirLayout::Result { .. } | MirLayout::Enum { .. }
        );
        let is_user_enum = matches!(desc.layout, MirLayout::Enum { .. });
        let is_unit = desc.is_canonical_ffi_unit();
        let supported = if is_managed_any {
            match super::ordinary_map_any_get::validate_managed_any_type(
                self.program.type_catalog(),
                ty,
            ) {
                Ok(()) => true,
                Err(message) => {
                    self.errors.push(NativeMirError::new(subject, message));
                    false
                }
            }
        } else if is_map_owner {
            let root = if is_ordinary_map_owner {
                crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id()
            } else if is_ordinary_map_owned_get {
                crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id()
            } else if is_ordinary_map_owned_any_v2 {
                crate::core::mir::types::ordinary_map_owned_any_v2_type_id()
            } else if desc.kind == MirTypeKind::MapAnyOwnerV1
                || desc.layout == MirLayout::MapAnyOwnerV1
            {
                crate::core::mir::types::map_any_owner_v1_type_id()
            } else {
                crate::core::mir::types::map_root_type_id()
            };
            if ty != &root {
                self.errors.push(NativeMirError::new(
                    subject,
                    "Map owner TypeDesc does not use the canonical synthetic identity",
                ));
                false
            } else {
                let move_ok = self
                    .program
                    .type_catalog()
                    .validate_glue(ty, crate::core::mir::types::MirGlueOperation::MoveOut);
                let drop_ok = self
                    .program
                    .type_catalog()
                    .validate_glue(ty, crate::core::mir::types::MirGlueOperation::Drop);
                let clone_glue = self
                    .program
                    .type_catalog()
                    .validate_glue(ty, crate::core::mir::types::MirGlueOperation::Clone);
                let clone_contract_matches = if is_ordinary_map_owner
                    || is_ordinary_map_owned_get
                    || is_ordinary_map_owned_any_v2
                {
                    clone_glue.is_ok()
                } else {
                    clone_glue.is_err()
                };
                match (move_ok, drop_ok, clone_contract_matches) {
                    (Ok(()), Ok(()), true) => true,
                    (Err(message), _, _) | (_, Err(message), _) => {
                        self.errors.push(NativeMirError::new(subject, message));
                        false
                    }
                    _ => {
                        self.errors.push(NativeMirError::new(
                            subject,
                            if is_ordinary_map_owner
                                || is_ordinary_map_owned_get
                                || is_ordinary_map_owned_any_v2
                            {
                                "ordinary Map owner TypeDesc lacks its canonical Clone glue"
                            } else {
                                "Map owner TypeDesc exposes unsupported generic clone glue"
                            },
                        ));
                        false
                    }
                }
            }
        } else if is_session {
            match self.program.type_catalog().validate_session_channel(ty) {
                Ok(()) => true,
                Err(message) => {
                    self.errors.push(NativeMirError::new(subject, message));
                    false
                }
            }
        } else if is_reference {
            match self.program.type_catalog().validate_reference_type(ty) {
                Ok(_) => true,
                Err(message) => {
                    self.errors.push(NativeMirError::new(subject, message));
                    false
                }
            }
        } else if is_owned_string {
            match self.program.type_catalog().validate_owned_string(ty) {
                Ok(()) => true,
                Err(message) => {
                    self.errors.push(NativeMirError::new(subject, message));
                    false
                }
            }
        } else if is_list
            && ty == &crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id()
        {
            match super::ordinary_map_owned_any_v2::validate_ordinary_map_owned_any_v2_values_list_type(
                self.program.type_catalog(),
                ty,
            ) {
                Ok(()) => true,
                Err(message) => {
                    self.errors.push(NativeMirError::new(subject, message));
                    false
                }
            }
        } else if is_list {
            match self
                .program
                .type_catalog()
                .validate_list_glue(ty, crate::core::mir::types::MirGlueOperation::MoveOut)
            {
                Ok(()) => true,
                Err(message) => {
                    self.errors.push(NativeMirError::new(subject, message));
                    false
                }
            }
        } else if is_set {
            match self
                .program
                .type_catalog()
                .validate_set_glue(ty, crate::core::mir::types::MirGlueOperation::MoveOut)
            {
                Ok(()) => true,
                Err(message) => {
                    self.errors.push(NativeMirError::new(subject, message));
                    false
                }
            }
        } else if matches!(desc.layout, MirLayout::Tuple(_)) {
            match validate_native_recursive_tuple_type(self.program.type_catalog(), ty) {
                Ok(()) => true,
                Err(message) => {
                    self.errors.push(NativeMirError::new(subject, message));
                    false
                }
            }
        } else if is_record {
            if desc.ownership == MirOwnership::Copy {
                if self.is_promoted_generic_record_projection(function, ty) {
                    self.validate_flat_copy_record_with_float(ty, subject)
                } else {
                    self.validate_flat_copy_record(ty, subject)
                }
            } else {
                match validate_native_non_copy_record_type(self.program.type_catalog(), ty) {
                    Ok(()) => true,
                    Err(message) => {
                        self.errors.push(NativeMirError::new(subject, message));
                        false
                    }
                }
            }
        } else if is_variant {
            if is_user_enum {
                if desc.kind == MirTypeKind::FlowStateSet {
                    match self
                        .program
                        .type_catalog()
                        .validate_multi_target_union_variant(ty)
                    {
                        Ok(()) => true,
                        Err(message) => {
                            self.errors.push(NativeMirError::new(subject, message));
                            false
                        }
                    }
                } else if self
                    .program
                    .type_catalog()
                    .is_zero_payload_flow_state_id_enum(ty)
                {
                    // Checker-owned flow state-id enum: the whole value is
                    // its i8 tag (R6-1040).
                    true
                } else if self.program.type_catalog().is_flow_event_id_enum(ty) {
                    // Checker-owned flow event-id enum: one i8 tag plus one
                    // owned-String slot (R6-1040).
                    true
                } else if desc.ownership == MirOwnership::Copy {
                    self.validate_flat_copy_variant(ty, subject, desc)
                } else {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "checker-materialized user enum has no native tagged-union ABI contract",
                    ));
                    false
                }
            } else if desc.ownership == MirOwnership::Copy {
                self.validate_flat_copy_variant(ty, subject, desc)
            } else {
                let native_variant = if matches!(desc.layout, MirLayout::Result { .. })
                    && self.program.transitions().is_empty()
                {
                    native_non_copy_variant_payload_type_strict(self.program.type_catalog(), ty)
                } else {
                    native_non_copy_variant_payload_type(self.program.type_catalog(), ty)
                };
                match native_variant {
                    Ok(_) => true,
                    Err(message) => {
                        let mut message = message;
                        message.subject = subject.to_owned();
                        self.errors.push(message);
                        false
                    }
                }
            }
        } else if matches!(
            desc.abi,
            MirAbiClass::Integer {
                bits: 32 | 64,
                signed: true,
            } | MirAbiClass::Float { bits: 32 | 64 }
                | MirAbiClass::Bool
        ) && desc.layout == MirLayout::Scalar
        {
            match self.program.type_catalog().validate_copy_value(ty) {
                Ok(()) => true,
                Err(message) => {
                    self.errors.push(NativeMirError::new(subject, message));
                    false
                }
            }
        } else {
            is_unit || self.validate_flat_copy_record(ty, subject)
        };
        if !supported {
            let contract = if is_owned_string {
                "native canonical owned String contract"
            } else if is_list {
                "native canonical List contract"
            } else if is_set {
                "native canonical Set<T> contract"
            } else if matches!(desc.layout, MirLayout::Tuple(_)) {
                "native canonical recursive tuple contract"
            } else if is_record {
                "native canonical non-Copy record contract"
            } else if is_variant {
                if is_user_enum {
                    if desc.ownership == MirOwnership::Copy {
                        "checker-materialized flat Copy user-enum contract"
                    } else if desc.kind == MirTypeKind::FlowStateSet {
                        "checker-materialized multi-target union tagged-union contract"
                    } else {
                        "checker-materialized user-enum native tagged-union contract"
                    }
                } else if desc.ownership == MirOwnership::Copy {
                    "flat Copy variant contract"
                } else if matches!(desc.layout, MirLayout::Result { .. }) {
                    "native non-Copy Result<string, i32> variant contract"
                } else if matches!(&desc.layout, MirLayout::Option { inner, .. } if self
                    .program
                    .type_catalog()
                    .get(inner)
                    .is_some_and(|inner| matches!(inner.layout, MirLayout::List { .. })))
                {
                    "native non-Copy Option<List<Copy scalar>> variant contract"
                } else {
                    "native non-Copy Option<string> variant contract"
                }
            } else {
                "Copy scalar native contract"
            };
            self.errors.push(NativeMirError::new(
                subject,
                format!(
                    "TypeDesc ABI {:?}, layout {:?}, ownership {:?} is outside the {contract}",
                    desc.abi, desc.layout, desc.ownership,
                ),
            ));
        }
        if !is_map_owner
            && !is_managed_any
            && !is_list
            && !is_set
            && !is_reference
            && !is_owned_string
            && !matches!(desc.layout, MirLayout::Tuple(_))
            && !is_record
            && !is_variant
            && !is_session
        {
            if desc.ownership != MirOwnership::Copy {
                self.errors.push(NativeMirError::new(
                    subject,
                    format!(
                        "ownership {:?} requires explicit native glue and is not in this scalar slice",
                        desc.ownership
                    ),
                ));
            }
            if desc.glue
                != (crate::core::mir::types::MirGlueContract {
                    move_out: MirGlueKind::Noop,
                    clone: MirGlueKind::Noop,
                    drop: MirGlueKind::Noop,
                })
            {
                self.errors.push(NativeMirError::new(
                    subject,
                    "Copy scalar TypeDesc does not carry the canonical no-op glue contract",
                ));
            }
        }
    }

    fn validate_flat_copy_record(
        &mut self,
        ty: &crate::core::ResolvedTypeId,
        subject: &str,
    ) -> bool {
        match self.program.type_catalog().validate_flat_copy_record(ty) {
            Ok(()) => true,
            Err(message) => {
                self.errors.push(NativeMirError::new(subject, message));
                false
            }
        }
    }

    fn validate_flat_copy_record_with_float(
        &mut self,
        ty: &crate::core::ResolvedTypeId,
        subject: &str,
    ) -> bool {
        match self
            .program
            .type_catalog()
            .validate_flat_copy_record_with_float(ty, true)
        {
            Ok(()) => true,
            Err(message) => {
                self.errors.push(NativeMirError::new(subject, message));
                false
            }
        }
    }

    fn is_promoted_generic_record_projection(
        &self,
        function: &MirFunction,
        ty: &crate::core::ResolvedTypeId,
    ) -> bool {
        self.program.instances().values().any(|instance| {
            matches!(
                &instance.contract,
                crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection { .. }
            ) && (instance.function == function.owner
                || function.blocks.values().any(|block| {
                    block.instructions.iter().any(|instruction| {
                        matches!(
                            &instruction.kind,
                            MirInstructionKind::Call {
                                callee: ResolvedCallee::Function(target),
                                ..
                            } if target == &instance.function
                        )
                    })
                }))
                && self
                    .program
                    .functions()
                    .get(&instance.function)
                    .and_then(|target| {
                        target
                            .parameters
                            .first()
                            .and_then(|parameter| target.values.get(parameter))
                    })
                    .is_some_and(|value| value.ty == *ty)
        })
    }

    fn validate_flat_copy_variant(
        &mut self,
        ty: &crate::core::ResolvedTypeId,
        subject: &str,
        _desc: &MirTypeDesc,
    ) -> bool {
        // Only a Result source can use the heterogeneous two-slot proof.  The
        // instance table also contains Option projection receipts; matching
        // those by source identity would incorrectly route an Option through
        // the Result validator and reject every existing generic Option test.
        let promoted_distinct_result = matches!(
            &_desc.layout,
            MirLayout::Result { ok, error, .. } if ok != error
        ) && self.program.instances().values().any(|instance| {
            matches!(
                &instance.contract,
                crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection {
                    contract
                } if contract.source_ty == *ty
            ) || matches!(
                &instance.contract,
                crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                    contract
                } if contract.source_ty == *ty
            )
        });
        if promoted_distinct_result {
            if let Some(receipt) =
                self.program
                    .instances()
                    .values()
                    .find_map(|instance| match &instance.contract {
                        crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection {
                            contract,
                        } if contract.source_ty == *ty
                            && contract.projection.nominal.as_str() == "builtin:type:Result"
                            && contract.projection.ownership == MirOwnership::Copy =>
                        {
                            Some(contract)
                        }
                        _ => None,
                    })
            {
                match self
                    .program
                    .type_catalog()
                    .validate_generic_result_projection_trap_receipt(
                        &receipt.source_ty,
                        &receipt.result_ty,
                        receipt,
                    ) {
                    Ok(()) => return true,
                    Err(message) => {
                        self.errors.push(NativeMirError::new(subject, message));
                        return false;
                    }
                }
            }
            if let Some(receipt) = self.program.instances().values().find_map(|instance| {
                match &instance.contract {
                    crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                        contract,
                    } if contract.source_ty == *ty
                        && contract.projection.nominal.as_str() == "builtin:type:Result"
                        && contract.projection.ownership == MirOwnership::Copy =>
                    {
                        Some(contract)
                    }
                    _ => None,
                }
            }) {
                match self
                    .program
                    .type_catalog()
                    .validated_generic_result_scalar_projection_fallback_contract(
                        &receipt.source_ty,
                        &receipt.projection.variant,
                        &receipt.projection.field,
                        &receipt.result_ty,
                        &receipt.fallback_ty,
                    ) {
                    Ok(_) => return true,
                    Err(message) => {
                        self.errors.push(NativeMirError::new(subject, message));
                        return false;
                    }
                }
            }
            match self
                .program
                .type_catalog()
                .validate_copy_result_scalar_variant(ty)
            {
                Ok(()) => return true,
                Err(message) => {
                    self.errors.push(NativeMirError::new(subject, message));
                    return false;
                }
            }
        }
        match native_copy_variant_payload_type(self.program.type_catalog(), ty) {
            Ok(_) => true,
            Err(message) => {
                let mut message = message;
                message.subject = subject.to_owned();
                self.errors.push(message);
                false
            }
        }
    }

    fn validate_copy_variant_type(
        &mut self,
        ty: &crate::core::ResolvedTypeId,
        subject: &str,
    ) -> bool {
        let Some(desc) = self.program.type_catalog().get(ty) else {
            self.errors.push(NativeMirError::new(
                subject,
                format!("type '{}' is absent from MIR TypeDesc catalog", ty.as_str()),
            ));
            return false;
        };
        self.validate_flat_copy_variant(ty, subject, desc)
    }

    /// Whether the type is the checker-owned multi-target Flow union.  Such
    /// scrutinees prove their shape through the promoted tagged-union contract
    /// instead of the flat Copy variant gate.
    fn union_tagged_scrutinee(&self, ty: &crate::core::ResolvedTypeId) -> bool {
        self.program.type_catalog().get(ty).is_some_and(|desc| {
            matches!(desc.layout, MirLayout::Enum { .. }) && desc.kind == MirTypeKind::FlowStateSet
        })
    }

    fn validate_instruction(
        &mut self,
        function: &MirFunction,
        instruction: &MirInstructionKind,
        subject: &str,
    ) {
        let catalog = self.program.type_catalog();
        match instruction {
            MirInstructionKind::Actor { operation } => {
                if let Err(error) = super::actor::validate_actor_operation(
                    self.program,
                    function,
                    operation,
                    subject,
                ) {
                    self.errors.push(error);
                }
            }
            MirInstructionKind::ManagedAny {
                result,
                source,
                operation,
                contract,
            } => {
                self.validate_value(function, source, "ManagedAny source");
                if let Some(result) = result {
                    self.validate_value(function, result, "ManagedAny result");
                }
                if let Err(error) = super::ordinary_map_any_get::validate_managed_any_operation(
                    self.program,
                    function,
                    result,
                    source,
                    *operation,
                    contract.as_ref(),
                    subject,
                ) {
                    self.errors.push(error);
                }
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2New { result } => {
                self.validate_value(function, result, "ordinary Map/Any owner-v2 New result");
                self.validate_ordinary_map_owned_any_v2_value(
                    function,
                    result,
                    subject,
                    "New result",
                );
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Set {
                result,
                source,
                key,
                value,
            } => {
                self.validate_value(function, result, "ordinary Map/Any owner-v2 Set result");
                self.validate_value(function, source, "ordinary Map/Any owner-v2 Set source");
                self.validate_value(function, key, "ordinary Map/Any owner-v2 Set key");
                self.validate_value(function, value, "ordinary Map/Any owner-v2 Set value");
                self.validate_ordinary_map_owned_any_v2_value(
                    function,
                    result,
                    subject,
                    "Set result",
                );
                self.validate_ordinary_map_owned_any_v2_value(
                    function,
                    source,
                    subject,
                    "Set source",
                );
                self.validate_ordinary_map_string_value(function, key, subject, "Set key");
                self.validate_ordinary_map_owned_any_v2_set_payload(function, value, subject);
                if result == source {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "ordinary Map/Any owner-v2 Set source and result must be distinct roots",
                    ));
                }
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Remove {
                result,
                source,
                key,
            } => {
                self.validate_value(function, result, "ordinary Map/Any owner-v2 Remove result");
                self.validate_value(function, source, "ordinary Map/Any owner-v2 Remove source");
                self.validate_value(function, key, "ordinary Map/Any owner-v2 Remove key");
                self.validate_ordinary_map_owned_any_v2_value(
                    function,
                    result,
                    subject,
                    "Remove result",
                );
                self.validate_ordinary_map_owned_any_v2_value(
                    function,
                    source,
                    subject,
                    "Remove source",
                );
                self.validate_ordinary_map_string_value(function, key, subject, "Remove key");
                if result == source {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "ordinary Map/Any owner-v2 Remove source and result must be distinct roots",
                    ));
                }
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Size { result, root } => {
                self.validate_value(function, result, "ordinary Map/Any owner-v2 Size result");
                self.validate_value(function, root, "ordinary Map/Any owner-v2 Size root");
                self.validate_ordinary_map_owned_any_v2_value(function, root, subject, "Size root");
                self.validate_ordinary_map_owned_any_v2_i32(
                    function,
                    result,
                    subject,
                    "Size result",
                );
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Contains { result, root, key } => {
                self.validate_value(
                    function,
                    result,
                    "ordinary Map/Any owner-v2 Contains result",
                );
                self.validate_value(function, root, "ordinary Map/Any owner-v2 Contains root");
                self.validate_value(function, key, "ordinary Map/Any owner-v2 Contains key");
                self.validate_ordinary_map_owned_any_v2_value(
                    function,
                    root,
                    subject,
                    "Contains root",
                );
                self.validate_ordinary_map_owned_any_v2_bool(function, result, subject);
                self.validate_ordinary_map_string_value(function, key, subject, "Contains key");
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { result, root, key } => {
                self.validate_value(function, result, "ordinary Map/Any owner-v2 GetAny result");
                self.validate_value(function, root, "ordinary Map/Any owner-v2 GetAny root");
                self.validate_value(function, key, "ordinary Map/Any owner-v2 GetAny key");
                self.validate_ordinary_map_owned_any_v2_value(
                    function,
                    root,
                    subject,
                    "GetAny root",
                );
                self.validate_ordinary_map_string_value(function, key, subject, "GetAny key");
                self.validate_ordinary_map_owned_any_v2_get_any_result(function, result, subject);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack { result, pair, .. } => {
                self.validate_value(function, result, "ordinary Map/Any owner-v2 unpack result");
                self.validate_value(function, pair, "ordinary Map/Any owner-v2 unpack pair");
                self.validate_ordinary_map_owned_any_v2_unpack_result(function, result, subject);
                let pair_type =
                    crate::core::mir::types::ordinary_map_owned_any_v2_get_result_type_id();
                if function
                    .values
                    .get(pair)
                    .is_none_or(|value| value.ty != pair_type)
                {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "ordinary Map/Any owner-v2 unpack pair is not the exact (bool, DynamicAny) result TypeDesc",
                    ));
                }
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Values { result, root } => {
                self.validate_value(function, result, "ordinary Map/Any owner-v2 Values result");
                self.validate_value(function, root, "ordinary Map/Any owner-v2 Values root");
                self.validate_ordinary_map_owned_any_v2_value(
                    function,
                    root,
                    subject,
                    "Values root",
                );
                self.validate_ordinary_map_owned_any_v2_values_list(
                    function, result, subject, "result",
                );
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2ValuesUnpack {
                result,
                values,
                index,
                ..
            } => {
                self.validate_value(
                    function,
                    result,
                    "ordinary Map/Any owner-v2 Values unpack result",
                );
                self.validate_value(
                    function,
                    values,
                    "ordinary Map/Any owner-v2 Values unpack snapshot",
                );
                self.validate_value(
                    function,
                    index,
                    "ordinary Map/Any owner-v2 Values unpack index",
                );
                self.validate_ordinary_map_owned_any_v2_values_list(
                    function,
                    values,
                    subject,
                    "unpack source",
                );
                // The program-wide ValuesUnpack preflight above binds the
                // signed i32/i64 ABI, target TypeDesc and direct binding to
                // the exact Checker receipt before any LLVM declarations.
                // This instruction arm keeps ValuesUnpack in the admitted
                // native shape once that whole-program contract succeeds.
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2IsEmpty { result, root } => {
                self.validate_value(function, result, "ordinary Map/Any owner-v2 IsEmpty result");
                self.validate_value(function, root, "ordinary Map/Any owner-v2 IsEmpty root");
                self.validate_ordinary_map_owned_any_v2_value(
                    function,
                    root,
                    subject,
                    "IsEmpty root",
                );
                self.validate_ordinary_map_owned_any_v2_bool(function, result, subject);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { root } => {
                self.validate_value(function, root, "ordinary Map/Any owner-v2 Drop operand");
                self.validate_ordinary_map_owned_any_v2_value(
                    function,
                    root,
                    subject,
                    "Drop operand",
                );
                if catalog
                    .validate_glue(
                        &crate::core::mir::types::ordinary_map_owned_any_v2_type_id(),
                        crate::core::mir::types::MirGlueOperation::Drop,
                    )
                    .is_err()
                {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "ordinary Map/Any owner-v2 Drop has no canonical explicit Drop glue",
                    ));
                }
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { result, root, key } => {
                self.validate_value(function, result, "ordinary Map Any snapshot result");
                self.validate_value(function, root, "ordinary Map Any snapshot root");
                self.validate_value(function, key, "ordinary Map Any snapshot key");
                self.validate_ordinary_map_owned_get_value(function, root, subject, "GetAny root");
                self.validate_ordinary_map_string_value(function, key, subject, "GetAny key");
                if let Some(value) = function.values.get(result) {
                    if let Err(message) =
                        catalog.validate_ordinary_map_owned_get_any_result_type(&value.ty)
                    {
                        self.errors.push(NativeMirError::new(subject, message));
                    }
                }
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { result, root } => {
                self.validate_value(function, result, "ordinary Map owned-Get Values result");
                self.validate_value(function, root, "ordinary Map owned-Get Values root");
                self.validate_ordinary_map_owned_get_value(function, root, subject, "Values root");
                self.validate_ordinary_map_owned_get_values_result(function, result, subject);
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1New { result } => {
                self.validate_value(function, result, "ordinary Map owned-Get New result");
                self.validate_ordinary_map_owned_get_value(function, result, subject, "New result");
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Set {
                result,
                source,
                key,
                value,
            } => {
                self.validate_value(function, result, "ordinary Map owned-Get Set result");
                self.validate_value(function, source, "ordinary Map owned-Get Set source");
                self.validate_value(function, key, "ordinary Map owned-Get Set key");
                self.validate_value(function, value, "ordinary Map owned-Get Set value");
                self.validate_ordinary_map_owned_get_value(function, result, subject, "Set result");
                self.validate_ordinary_map_owned_get_value(function, source, subject, "Set source");
                if result == source {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "ordinary Map owned-Get Set source and result must be distinct roots",
                    ));
                }
                self.validate_ordinary_map_string_value(
                    function,
                    key,
                    subject,
                    "owned-Get Set key",
                );
                self.validate_ordinary_map_owned_get_set_payload(function, value, subject);
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Remove {
                result,
                source,
                key,
            } => {
                self.validate_value(function, result, "ordinary Map owned-Get Remove result");
                self.validate_value(function, source, "ordinary Map owned-Get Remove source");
                self.validate_value(function, key, "ordinary Map owned-Get Remove key");
                self.validate_ordinary_map_owned_get_value(
                    function,
                    result,
                    subject,
                    "Remove result",
                );
                self.validate_ordinary_map_owned_get_value(
                    function,
                    source,
                    subject,
                    "Remove source",
                );
                if result == source {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "ordinary Map owned-Get Remove source and result must be distinct roots",
                    ));
                }
                self.validate_ordinary_map_string_value(
                    function,
                    key,
                    subject,
                    "owned-Get Remove key",
                );
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Size { result, root } => {
                self.validate_value(function, result, "ordinary Map owned-Get Size result");
                self.validate_value(function, root, "ordinary Map owned-Get Size root");
                self.validate_ordinary_map_owned_get_value(function, root, subject, "Size root");
                self.validate_ordinary_map_owned_get_i32(function, result, subject, "Size result");
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Contains { result, root, key } => {
                self.validate_value(function, result, "ordinary Map owned-Get Contains result");
                self.validate_value(function, root, "ordinary Map owned-Get Contains root");
                self.validate_value(function, key, "ordinary Map owned-Get Contains key");
                self.validate_ordinary_map_owned_get_value(
                    function,
                    root,
                    subject,
                    "Contains root",
                );
                self.validate_ordinary_map_owned_get_bool(function, result, subject);
                self.validate_ordinary_map_string_value(
                    function,
                    key,
                    subject,
                    "owned-Get Contains key",
                );
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetI32 { result, root, key } => {
                self.validate_value(function, result, "ordinary Map owned-Get GetI32 result");
                self.validate_value(function, root, "ordinary Map owned-Get GetI32 root");
                self.validate_value(function, key, "ordinary Map owned-Get GetI32 key");
                self.validate_ordinary_map_owned_get_value(function, root, subject, "GetI32 root");
                self.validate_ordinary_map_string_value(
                    function,
                    key,
                    subject,
                    "owned-Get GetI32 key",
                );
                self.validate_ordinary_map_owned_get_result(function, result, false, subject);
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetString { result, root, key } => {
                self.validate_value(function, result, "ordinary Map owned-Get GetString result");
                self.validate_value(function, root, "ordinary Map owned-Get GetString root");
                self.validate_value(function, key, "ordinary Map owned-Get GetString key");
                self.validate_ordinary_map_owned_get_value(
                    function,
                    root,
                    subject,
                    "GetString root",
                );
                self.validate_ordinary_map_string_value(
                    function,
                    key,
                    subject,
                    "owned-Get GetString key",
                );
                self.validate_ordinary_map_owned_get_result(function, result, true, subject);
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Drop { root } => {
                self.validate_value(function, root, "ordinary Map owned-Get Drop operand");
                self.validate_ordinary_map_owned_get_value(function, root, subject, "Drop operand");
                if self
                    .program
                    .type_catalog()
                    .validate_glue(
                        &crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id(),
                        crate::core::mir::types::MirGlueOperation::Drop,
                    )
                    .is_err()
                {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "ordinary Map owned-Get Drop has no canonical explicit Drop glue",
                    ));
                }
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleNew { result } => {
                self.validate_value(function, result, "ordinary Map owner lifecycle New result");
                self.validate_ordinary_map_owner_value(function, result, subject, "New result");
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleSet {
                result,
                source,
                key,
                value,
            } => {
                self.validate_value(function, result, "ordinary Map owner lifecycle Set result");
                self.validate_value(function, source, "ordinary Map owner lifecycle Set source");
                self.validate_value(function, key, "ordinary Map owner lifecycle Set key");
                self.validate_value(function, value, "ordinary Map owner lifecycle Set value");
                self.validate_ordinary_map_owner_value(function, result, subject, "Set result");
                self.validate_ordinary_map_owner_value(function, source, subject, "Set source");
                self.validate_ordinary_map_string_value(function, key, subject, "Set key");
                self.validate_ordinary_map_set_payload(function, value, subject);
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleRemove {
                result,
                source,
                key,
            } => {
                self.validate_value(
                    function,
                    result,
                    "ordinary Map owner lifecycle Remove result",
                );
                self.validate_value(
                    function,
                    source,
                    "ordinary Map owner lifecycle Remove source",
                );
                self.validate_value(function, key, "ordinary Map owner lifecycle Remove key");
                self.validate_ordinary_map_owner_value(function, result, subject, "Remove result");
                self.validate_ordinary_map_owner_value(function, source, subject, "Remove source");
                self.validate_ordinary_map_string_value(function, key, subject, "Remove key");
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleSize { result, root } => {
                self.validate_value(function, result, "ordinary Map owner lifecycle Size result");
                self.validate_value(function, root, "ordinary Map owner lifecycle Size root");
                self.validate_ordinary_map_owner_value(function, root, subject, "Size root");
                self.validate_map_root_i32(function, result, subject, "ordinary Map Size result");
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleContains { result, root, key } => {
                self.validate_value(
                    function,
                    result,
                    "ordinary Map owner lifecycle Contains result",
                );
                self.validate_value(function, root, "ordinary Map owner lifecycle Contains root");
                self.validate_value(function, key, "ordinary Map owner lifecycle Contains key");
                self.validate_ordinary_map_owner_value(function, root, subject, "Contains root");
                self.validate_map_root_bool(function, result, subject);
                self.validate_ordinary_map_string_value(function, key, subject, "Contains key");
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleDrop { root } => {
                self.validate_value(function, root, "ordinary Map owner lifecycle Drop operand");
                self.validate_ordinary_map_owner_value(function, root, subject, "Drop operand");
                if self
                    .program
                    .type_catalog()
                    .validate_glue(
                        &crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id(),
                        crate::core::mir::types::MirGlueOperation::Drop,
                    )
                    .is_err()
                {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "ordinary Map owner lifecycle Drop has no canonical explicit Drop glue",
                    ));
                }
            }
            MirInstructionKind::MapRootNew { result } => {
                self.validate_value(function, result, "MapRoot New result");
                self.validate_map_root_value(function, result, subject, "New result");
            }
            MirInstructionKind::MapRootSet {
                result,
                source,
                key,
                value,
            } => {
                self.validate_value(function, result, "MapRoot Set result");
                self.validate_value(function, source, "MapRoot Set source");
                self.validate_map_root_value(function, result, subject, "Set result");
                self.validate_map_root_value(function, source, subject, "Set source");
                if key.contains('\0') {
                    self.errors
                        .push(NativeMirError::new(subject, "MapRoot key contains NUL"));
                }
                self.validate_map_root_set_payload(function, value, subject);
            }
            MirInstructionKind::MapRootRemove {
                result,
                source,
                key,
            } => {
                self.validate_value(function, result, "MapRoot Remove result");
                self.validate_value(function, source, "MapRoot Remove source");
                self.validate_map_root_value(function, result, subject, "Remove result");
                self.validate_map_root_value(function, source, subject, "Remove source");
                if key.contains('\0') {
                    self.errors
                        .push(NativeMirError::new(subject, "MapRoot key contains NUL"));
                }
            }
            MirInstructionKind::MapRootSize { result, root } => {
                self.validate_value(function, result, "MapRoot Size result");
                self.validate_value(function, root, "MapRoot Size root");
                self.validate_map_root_value(function, root, subject, "Size root");
                self.validate_map_root_i32(function, result, subject, "Size result");
            }
            MirInstructionKind::MapRootContains { result, root, key } => {
                self.validate_value(function, result, "MapRoot Contains result");
                self.validate_value(function, root, "MapRoot Contains root");
                self.validate_map_root_value(function, root, subject, "Contains root");
                self.validate_map_root_bool(function, result, subject);
                if key.contains('\0') {
                    self.errors
                        .push(NativeMirError::new(subject, "MapRoot key contains NUL"));
                }
            }
            MirInstructionKind::MapRootDrop { root } => {
                self.validate_value(function, root, "MapRoot Drop operand");
                self.validate_map_root_value(function, root, subject, "Drop operand");
            }
            MirInstructionKind::Const { result, literal } => {
                self.validate_value(function, result, "constant result");
                let Some(desc) = function
                    .values
                    .get(result)
                    .and_then(|value| catalog.get(&value.ty))
                else {
                    return;
                };
                let valid = match (desc.abi, literal) {
                    (MirAbiClass::Integer { bits: 32 | 64, .. }, ResolvedLiteral::Int(value)) => {
                        desc.abi
                            != MirAbiClass::Integer {
                                bits: 32,
                                signed: true,
                            }
                            || i32::try_from(*value).is_ok()
                    }
                    (MirAbiClass::Bool, ResolvedLiteral::Bool(_)) => true,
                    (MirAbiClass::Float { bits: 32 | 64 }, ResolvedLiteral::FloatBits(_)) => true,
                    (MirAbiClass::StringHandle, ResolvedLiteral::String(_)) => {
                        let Some(value) = function.values.get(result) else {
                            self.errors.push(NativeMirError::new(
                                subject,
                                "constant result is absent from MIR value table",
                            ));
                            return;
                        };
                        if let Err(message) = catalog.validate_owned_string(&value.ty) {
                            self.errors.push(NativeMirError::new(subject, message));
                            false
                        } else {
                            true
                        }
                    }
                    _ => false,
                };
                if !valid {
                    self.errors.push(NativeMirError::new(
                        subject,
                        format!(
                            "literal {literal:?} does not match native TypeDesc ABI {:?}",
                            desc.abi
                        ),
                    ));
                }
            }
            MirInstructionKind::Copy { result, source }
            | MirInstructionKind::Move { result, source }
            | MirInstructionKind::Clone { result, source } => {
                self.validate_value(function, result, "result");
                self.validate_value(function, source, "source");
                let (Some(result_value), Some(source_value)) =
                    (function.values.get(result), function.values.get(source))
                else {
                    return;
                };
                if result_value.ty != source_value.ty {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "result and source types disagree",
                    ));
                }
                if catalog.is_managed_any_i32_string_v1(&source_value.ty)
                    && matches!(instruction, MirInstructionKind::Clone { .. })
                {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "ManagedAny Clone requires its dedicated exact operation receipt",
                    ));
                }
                if let Some(desc) = catalog.get(&source_value.ty) {
                    if desc.ownership != MirOwnership::Copy
                        && matches!(instruction, MirInstructionKind::Copy { .. })
                    {
                        let message = if matches!(desc.layout, MirLayout::List { .. }) {
                            "List values require explicit Move or Clone glue; shallow Copy is not permitted"
                        } else if matches!(desc.layout, MirLayout::Set { .. }) {
                            "Set values require explicit Move or Clone glue; shallow Copy is not permitted"
                        } else if matches!(
                            &desc.kind,
                            MirTypeKind::Primitive(crate::core::PrimitiveType::String)
                        ) {
                            "owned String values require explicit Move or Clone glue; shallow Copy is not permitted"
                        } else {
                            "non-Copy values require explicit Move or Clone glue; shallow Copy is not permitted"
                        };
                        self.errors.push(NativeMirError::new(subject, message));
                    } else if !matches!(instruction, MirInstructionKind::Copy { .. }) {
                        let operation = match instruction {
                            MirInstructionKind::Move { .. } => {
                                crate::core::mir::types::MirGlueOperation::MoveOut
                            }
                            MirInstructionKind::Clone { .. } => {
                                crate::core::mir::types::MirGlueOperation::Clone
                            }
                            _ => {
                                self.errors.push(NativeMirError::new(
                                    subject,
                                    "copy/move/clone instruction has an unsupported ownership operation",
                                ));
                                return;
                            }
                        };
                        if desc.ownership != MirOwnership::Copy {
                            if let Err(message) = catalog.validate_glue(&source_value.ty, operation)
                            {
                                self.errors.push(NativeMirError::new(subject, message));
                            }
                        }
                    }
                }
            }
            MirInstructionKind::Convert { result, source } => {
                self.validate_value(function, result, "conversion result");
                self.validate_value(function, source, "conversion source");
                let Some(source_ty) = function.values.get(source).map(|value| value.ty.clone())
                else {
                    return;
                };
                let Some(result_ty) = function.values.get(result).map(|value| value.ty.clone())
                else {
                    return;
                };
                match catalog.validate_conversion(&source_ty, &result_ty) {
                    Ok(contract) => {
                        if !matches!(
                            contract.kind,
                            MirConversionKind::ScalarIdentity
                                | MirConversionKind::SignedI32ToI64
                                | MirConversionKind::SignedI32ToFloat64
                                | MirConversionKind::SignedI64ToFloat64
                                | MirConversionKind::Float64ToFloat32
                        ) {
                            self.errors.push(NativeMirError::new(
                                subject,
                                "conversion kind is not in the native scalar contract",
                            ));
                        }
                    }
                    Err(message) => self.errors.push(NativeMirError::new(subject, message)),
                }
            }
            MirInstructionKind::Unary {
                result,
                op,
                operand,
            } => self.validate_unary(function, result, *op, operand, subject),
            MirInstructionKind::Binary {
                result,
                op,
                left,
                right,
            } => self.validate_binary(function, result, *op, left, right, subject),
            MirInstructionKind::Project {
                result,
                base,
                projection,
                list_index_contract,
            } => self.validate_project(
                function,
                result,
                base,
                projection,
                list_index_contract.as_ref(),
                subject,
            ),
            MirInstructionKind::MoveProject {
                result,
                base,
                projection,
            } => self.validate_move_project(function, result, base, projection, subject),
            MirInstructionKind::MoveProjectDrop {
                result,
                base,
                projection,
                contract,
            } => self.validate_move_project_drop(
                function,
                result,
                base,
                projection,
                contract.as_ref(),
                subject,
            ),
            MirInstructionKind::VariantProject {
                result,
                base,
                contract,
            } => self.validate_variant_project(function, result, base, contract.as_ref(), subject),
            MirInstructionKind::VariantProjectOr {
                result,
                base,
                fallback,
                contract,
            } => self.validate_variant_project_or(
                function,
                result,
                base,
                fallback,
                contract.as_ref(),
                subject,
            ),
            MirInstructionKind::VariantProjectMove {
                result,
                base,
                contract,
            } => self.validate_variant_project_move(
                function,
                result,
                base,
                contract.as_ref(),
                subject,
            ),
            MirInstructionKind::Borrow {
                result,
                source,
                mutable,
                ..
            } => {
                self.validate_value(function, result, "borrow result");
                self.validate_value(function, source, "borrow source");
                let (Some(result_value), Some(source_value)) =
                    (function.values.get(result), function.values.get(source))
                else {
                    return;
                };
                if let Err(message) =
                    catalog.validate_borrow(&source_value.ty, &result_value.ty, *mutable)
                {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
            MirInstructionKind::EndBorrow { borrow } => {
                self.validate_value(function, borrow, "end-borrow value");
                let Some(value) = function.values.get(borrow) else {
                    return;
                };
                if let Err(message) = catalog.validate_reference_type(&value.ty) {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
            MirInstructionKind::Drop { value } => {
                if function
                    .values
                    .get(value)
                    .is_some_and(|v| catalog.is_managed_any_i32_string_v1(&v.ty))
                {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "ManagedAny Drop requires its dedicated exact operation receipt",
                    ));
                }
                self.validate_value(function, value, "drop value");
                let Some(value) = function.values.get(value) else {
                    return;
                };
                if let Some(desc) = catalog.get(&value.ty) {
                    if desc.ownership != MirOwnership::Copy {
                        if let Err(message) = catalog.validate_glue(
                            &value.ty,
                            crate::core::mir::types::MirGlueOperation::Drop,
                        ) {
                            self.errors.push(NativeMirError::new(subject, message));
                        }
                        let admitted = match &desc.layout {
                            MirLayout::List { .. }
                            | MirLayout::Set { .. }
                            | MirLayout::Tuple(_)
                            | MirLayout::Record { .. }
                            | MirLayout::Option { .. }
                            | MirLayout::Result { .. } => true,
                            MirLayout::Enum { .. } if desc.kind == MirTypeKind::FlowStateSet => {
                                catalog
                                    .validate_multi_target_union_variant(&value.ty)
                                    .is_ok()
                            }
                            _ => matches!(
                                &desc.kind,
                                MirTypeKind::Primitive(crate::core::PrimitiveType::String)
                            ),
                        };
                        if !admitted {
                            self.errors.push(NativeMirError::new(
                                subject,
                                "only canonical owned String/List/Set/tuple/variant drop glue is emitted by this native slice",
                            ));
                        }
                    }
                }
            }
            MirInstructionKind::Construct {
                result,
                kind,
                fields,
            } => self.validate_construct(function, result, kind, fields, subject),
            MirInstructionKind::UpdateRecord {
                result,
                base,
                kind,
                fields,
                record_update_contract,
                record_update_move_contract,
            } => self.validate_update_record(
                function,
                result,
                base,
                kind,
                fields,
                record_update_contract.as_ref(),
                record_update_move_contract.as_ref(),
                subject,
            ),
            MirInstructionKind::ConstructVariant {
                result,
                nominal,
                variant,
                fields,
            } => {
                self.validate_construct_variant(function, result, nominal, variant, fields, subject)
            }
            MirInstructionKind::ConstructVariantMove {
                result,
                nominal,
                variant,
                fields,
            } => self.validate_construct_variant_move(
                function, result, nominal, variant, fields, subject,
            ),
            MirInstructionKind::ConstructList {
                result,
                elements,
                list_construct_contract,
            } => {
                self.validate_value(function, result, "List result");
                let element_types = elements
                    .iter()
                    .filter_map(|element| {
                        function.values.get(element).map(|value| value.ty.clone())
                    })
                    .collect::<Vec<_>>();
                if element_types.len() != elements.len() {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "List element is absent from MIR value catalog",
                    ));
                } else if let Some(result_value) = function.values.get(result) {
                    let Some(receipt) = list_construct_contract.as_ref() else {
                        self.errors.push(NativeMirError::new(
                            subject,
                            "List construction has no canonical receipt",
                        ));
                        return;
                    };
                    if let Err(message) = catalog.validate_list_construct_receipt(
                        &result_value.ty,
                        &element_types,
                        receipt,
                    ) {
                        self.errors.push(NativeMirError::new(subject, message));
                    }
                }
            }
            MirInstructionKind::ListOp {
                result,
                operation,
                list,
                argument,
                list_operation_contract,
            } => {
                self.validate_value(function, result, "List operation result");
                self.validate_value(function, list, "List operation receiver");
                if let Some(argument) = argument {
                    self.validate_value(function, argument, "List operation argument");
                }
                let (Some(result_value), Some(list_value)) =
                    (function.values.get(result), function.values.get(list))
                else {
                    return;
                };
                let Some(receipt) = list_operation_contract.as_ref() else {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "List operation has no canonical receipt",
                    ));
                    return;
                };
                let argument_ty = argument
                    .as_ref()
                    .and_then(|value| function.values.get(value))
                    .map(|value| value.ty.clone());
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_list_operation_receipt_with_argument(
                        &result_value.ty,
                        &list_value.ty,
                        argument_ty.as_ref(),
                        *operation,
                        receipt,
                    )
                {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
            MirInstructionKind::VariantPredicate {
                result,
                predicate,
                variant,
                contract,
            } => {
                self.validate_value(function, result, "variant predicate result");
                self.validate_value(function, variant, "variant predicate source");
                let (Some(result_value), Some(variant_value)) =
                    (function.values.get(result), function.values.get(variant))
                else {
                    return;
                };
                let Some(receipt) = contract.as_ref() else {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "variant predicate has no canonical receipt",
                    ));
                    return;
                };
                if let Err(message) = catalog.validate_variant_predicate_receipt(
                    &result_value.ty,
                    &variant_value.ty,
                    *predicate,
                    receipt,
                ) {
                    self.errors.push(NativeMirError::new(subject, message));
                }
                if let Err(message) = catalog.validate_flat_copy_variant(&variant_value.ty) {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
            MirInstructionKind::ConstructSet { result, elements } => {
                self.validate_value(function, result, "Set result");
                let element_types = elements
                    .iter()
                    .filter_map(|element| {
                        function.values.get(element).map(|value| value.ty.clone())
                    })
                    .collect::<Vec<_>>();
                if element_types.len() != elements.len() {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "Set element is absent from MIR value catalog",
                    ));
                } else if let Some(result_value) = function.values.get(result) {
                    if let Err(message) =
                        catalog.validate_set_construct(&result_value.ty, &element_types)
                    {
                        self.errors.push(NativeMirError::new(subject, message));
                    }
                }
            }
            MirInstructionKind::SetOp {
                result,
                operation,
                set,
                argument,
            } => {
                self.validate_value(function, result, "Set operation result");
                self.validate_value(function, set, "Set operation receiver");
                if let Some(argument) = argument {
                    self.validate_value(function, argument, "Set operation argument");
                }
                let (Some(result_value), Some(set_value)) =
                    (function.values.get(result), function.values.get(set))
                else {
                    return;
                };
                let argument_ty = argument
                    .as_ref()
                    .and_then(|value| function.values.get(value))
                    .map(|value| &value.ty);
                if let Err(message) = catalog.validate_set_operation(
                    &result_value.ty,
                    &set_value.ty,
                    argument_ty,
                    *operation,
                ) {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
            MirInstructionKind::BuiltinCall {
                result,
                kind,
                arguments,
                string_field_contract,
            } => {
                let contract = MirBuiltinContract::for_kind(*kind);
                if arguments.len() != contract.arity {
                    self.errors.push(NativeMirError::new(
                        subject,
                        format!(
                            "builtin '{}' has {} arguments; contract requires {}",
                            contract.name,
                            arguments.len(),
                            contract.arity
                        ),
                    ));
                }
                for argument in arguments {
                    self.validate_value(function, argument, "builtin argument");
                }
                self.validate_value(function, result, "builtin result");
                if *kind == MirBuiltinKind::SessionOpen {
                    let Some(result_value) = function.values.get(result) else {
                        return;
                    };
                    if let Err(message) = self
                        .program
                        .type_catalog()
                        .validate_session_channel(&result_value.ty)
                    {
                        self.errors.push(NativeMirError::new(
                            subject,
                            format!(
                                "builtin '{}' result is outside the canonical SessionChan contract: {message}",
                                contract.name
                            ),
                        ));
                    }
                    return;
                }
                if *kind == MirBuiltinKind::SessionPair {
                    let Some(result_value) = function.values.get(result) else {
                        return;
                    };
                    if let Err(message) = self
                        .program
                        .type_catalog()
                        .validate_plain_session_pair(&result_value.ty)
                    {
                        self.errors.push(NativeMirError::new(
                            subject,
                            format!(
                                "builtin '{}' result is outside the canonical session-pair contract: {message}",
                                contract.name
                            ),
                        ));
                    }
                    return;
                }
                let supported_kind = matches!(
                    kind,
                    MirBuiltinKind::Abs
                        | MirBuiltinKind::Min
                        | MirBuiltinKind::Max
                        | MirBuiltinKind::PrintlnBool
                        | MirBuiltinKind::PrintlnInt
                        | MirBuiltinKind::PrintlnString
                        | MirBuiltinKind::PrintlnFloat
                );
                if !supported_kind {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "builtin kind is not in native MIR contract",
                    ));
                }
                if let Some(receipt) = string_field_contract {
                    if *kind != MirBuiltinKind::PrintlnString {
                        self.errors.push(NativeMirError::new(
                            subject,
                            "String-field borrow receipt is attached to a non-String builtin",
                        ));
                    } else if arguments.len() == 1 {
                        let Some(argument) = arguments.first() else {
                            return;
                        };
                        let Some(source) = function.values.get(argument) else {
                            return;
                        };
                        if let Err(message) = self
                            .program
                            .type_catalog()
                            .validate_string_field_borrow_receipt(&source.ty, receipt)
                        {
                            self.errors.push(NativeMirError::new(subject, message));
                        }
                    }
                } else {
                    for (index, argument) in arguments.iter().enumerate() {
                        let Some(desc) = function
                            .values
                            .get(argument)
                            .and_then(|value| self.program.type_catalog().get(&value.ty))
                        else {
                            continue;
                        };
                        let Some(argument_value) = function.values.get(argument) else {
                            continue;
                        };
                        let ownership_ok = if *kind == MirBuiltinKind::PrintlnString {
                            self.program
                                .type_catalog()
                                .validate_owned_string(&argument_value.ty)
                                .is_ok()
                        } else {
                            desc.is_canonical_copy_scalar(true)
                        };
                        if !contract.accepts_abi(desc.abi)
                            || !contract.accepts_layout(&desc.layout)
                            || !ownership_ok
                            || (matches!(
                                kind,
                                MirBuiltinKind::Abs | MirBuiltinKind::Min | MirBuiltinKind::Max
                            ) && desc.abi
                                != MirAbiClass::Integer {
                                    bits: 64,
                                    signed: true,
                                })
                        {
                            self.errors.push(NativeMirError::new(
                            subject,
                            format!(
                                "builtin '{}' argument {index} TypeDesc/ABI is outside native scalar contract",
                                contract.name,
                            ),
                        ));
                        }
                    }
                }
                let Some(result_desc) = function
                    .values
                    .get(result)
                    .and_then(|value| self.program.type_catalog().get(&value.ty))
                else {
                    return;
                };
                let valid_result = if contract.result_must_be_unit {
                    result_desc.is_canonical_ffi_unit()
                } else {
                    contract.accepts_abi(result_desc.abi)
                        && contract.accepts_layout(&result_desc.layout)
                        && result_desc.is_canonical_copy_scalar(true)
                        && (!matches!(
                            kind,
                            MirBuiltinKind::Abs | MirBuiltinKind::Min | MirBuiltinKind::Max
                        ) || result_desc.abi
                            == MirAbiClass::Integer {
                                bits: 64,
                                signed: true,
                            })
                };
                if !valid_result {
                    self.errors.push(NativeMirError::new(
                        subject,
                        format!(
                            "builtin '{}' result TypeDesc/ABI is outside native scalar contract",
                            contract.name
                        ),
                    ));
                }
            }
            MirInstructionKind::SessionCall {
                result,
                operation,
                endpoint,
                payload,
                contract,
            } => {
                self.validate_value(function, endpoint, "SessionCall endpoint");
                self.validate_value(function, result, "SessionCall result");
                if let Some(payload) = payload {
                    self.validate_value(function, payload, "SessionCall payload");
                }
                let (Some(endpoint_ty), Some(result_ty)) = (
                    function.values.get(endpoint).map(|value| value.ty.clone()),
                    function.values.get(result).map(|value| value.ty.clone()),
                ) else {
                    return;
                };
                let Some(receipt) = contract.as_ref() else {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "SessionCall has no canonical residual/ABI receipt",
                    ));
                    return;
                };
                if let Err(message) =
                    catalog.validate_session_call_contract(&endpoint_ty, &result_ty, receipt)
                {
                    self.errors.push(NativeMirError::new(subject, message));
                }
                if payload.is_some() != receipt.payload_ty.is_some() {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "SessionCall payload value disagrees with its receipt TypeDesc",
                    ));
                }
                if let Some(payload) = payload {
                    if let (Some(payload_ty), Some(receipt_ty)) = (
                        function.values.get(payload).map(|value| &value.ty),
                        receipt.payload_ty.as_ref(),
                    ) {
                        if payload_ty != receipt_ty {
                            self.errors.push(NativeMirError::new(
                                subject,
                                "SessionCall payload value disagrees with its receipt TypeDesc identity",
                            ));
                        }
                    }
                }
                if *operation != receipt.operation {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "SessionCall receipt disagrees with MIR operation",
                    ));
                }
            }
            MirInstructionKind::SessionPairBind { lo, hi, contract } => {
                self.validate_value(function, lo, "typed session_pair lo endpoint");
                self.validate_value(function, hi, "typed session_pair hi endpoint");
                let (Some(lo_ty), Some(hi_ty)) = (
                    function.values.get(lo).map(|value| value.ty.clone()),
                    function.values.get(hi).map(|value| value.ty.clone()),
                ) else {
                    return;
                };
                let Some(receipt) = contract.as_ref() else {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "typed session_pair binding has no canonical TypeDesc receipt",
                    ));
                    return;
                };
                if let Err(message) = catalog.validate_session_pair_bind_receipt(
                    &receipt.pair_ty,
                    &lo_ty,
                    &hi_ty,
                    receipt,
                ) {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
            // All closure metadata and local binding relations are checked
            // program-wide above. Reuse the ordinary direct-call ABI/result
            // validator for the statically resolved target.
            MirInstructionKind::ClosureBind { .. } => {}
            MirInstructionKind::ClosureInvoke {
                result,
                callee,
                type_arguments,
                arguments,
                ..
            } => self.validate_call(
                function,
                result.as_ref(),
                &ResolvedCallee::Function(callee.clone()),
                type_arguments,
                arguments,
                None,
                subject,
            ),
            MirInstructionKind::Call {
                result,
                callee,
                type_arguments,
                arguments,
                variant_call_contract,
                ..
            } => self.validate_call(
                function,
                result.as_ref(),
                callee,
                type_arguments,
                arguments,
                variant_call_contract.as_ref(),
                subject,
            ),
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
            _ => self.errors.push(NativeMirError::new(
                subject,
                "instruction shape is not in the native scalar MIR slice",
            )),
        }
    }

    fn validate_same_copy_values(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        source: &MirValueId,
        subject: &str,
    ) {
        self.validate_value(function, result, "result");
        self.validate_value(function, source, "source");
        let (Some(result), Some(source)) =
            (function.values.get(result), function.values.get(source))
        else {
            return;
        };
        if result.ty != source.ty {
            self.errors.push(NativeMirError::new(
                subject,
                "result and source types disagree",
            ));
        }
    }

    fn validate_project(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        base: &MirValueId,
        projection: &MirProjection,
        list_index_contract: Option<&crate::core::mir::types::MirListIndexProjectionContract>,
        subject: &str,
    ) {
        self.validate_value(function, result, "projection result");
        self.validate_value(function, base, "projection base");
        let (Some(base_value), Some(result_value)) =
            (function.values.get(base), function.values.get(result))
        else {
            return;
        };
        match projection {
            MirProjection::Tuple(index) => {
                if let Err(message) = self.program.type_catalog().validate_projection(
                    &base_value.ty,
                    &result_value.ty,
                    projection,
                ) {
                    self.errors.push(NativeMirError::new(subject, message));
                }
                if exceeds_native_u32(*index) {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "tuple projection index exceeds native aggregate ABI",
                    ));
                }
            }
            MirProjection::Index(index) => {
                let Some(index_value) = function.values.get(index) else {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "List index is absent from MIR value catalog",
                    ));
                    return;
                };
                let Some(receipt) = list_index_contract else {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "List index projection has no canonical receipt",
                    ));
                    return;
                };
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_list_index_projection_receipt(
                        &base_value.ty,
                        &index_value.ty,
                        &result_value.ty,
                        receipt,
                    )
                {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
            MirProjection::Field(_) => {
                if list_index_contract.is_some() {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "List index receipt is attached to a non-index projection",
                    ));
                    return;
                }
                if let Err(message) = self.program.type_catalog().validate_projection(
                    &base_value.ty,
                    &result_value.ty,
                    projection,
                ) {
                    self.errors.push(NativeMirError::new(subject, message));
                }
                if self
                    .program
                    .type_catalog()
                    .get(&base_value.ty)
                    .is_some_and(|desc| {
                        desc.ownership != MirOwnership::Copy
                            && self
                                .program
                                .type_catalog()
                                .get(&result_value.ty)
                                .is_some_and(|result| result.ownership != MirOwnership::Copy)
                    })
                {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "non-Copy record field projection requires an explicit MoveProject contract",
                    ));
                }
            }
            MirProjection::Dereference => {
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_dereference(&base_value.ty, &result_value.ty)
                {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
            MirProjection::ReadPath(receipt) => {
                if list_index_contract.is_some() {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "List index receipt is attached to a read projection path",
                    ));
                }
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_read_projection_receipt(&base_value.ty, &result_value.ty, receipt)
                {
                    self.errors.push(NativeMirError::new(subject, message));
                }
                if receipt.steps.iter().any(|step| {
                    matches!(
                        step.projection,
                        crate::core::mir::types::MirReadProjectionKind::Tuple(index)
                            if exceeds_native_u32(index)
                    )
                }) {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "read projection tuple index exceeds native aggregate ABI",
                    ));
                }
            }
        }
    }

    fn validate_move_project(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        base: &MirValueId,
        projection: &MirProjection,
        subject: &str,
    ) {
        self.validate_value(function, result, "move projection result");
        self.validate_value(function, base, "move projection base");
        let (Some(base_value), Some(result_value)) =
            (function.values.get(base), function.values.get(result))
        else {
            return;
        };
        if let Err(message) = self.program.type_catalog().validate_move_projection(
            &base_value.ty,
            &result_value.ty,
            projection,
        ) {
            self.errors.push(NativeMirError::new(subject, message));
            return;
        }
        if matches!(projection, MirProjection::Tuple(_)) {
            if let Err(message) =
                validate_native_recursive_tuple_type(self.program.type_catalog(), &base_value.ty)
            {
                self.errors.push(NativeMirError::new(subject, message));
            }
            return;
        }
        if let Err(message) =
            validate_native_non_copy_record_type(self.program.type_catalog(), &base_value.ty)
        {
            self.errors.push(NativeMirError::new(subject, message));
        }
    }

    fn validate_move_project_drop(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        base: &MirValueId,
        projection: &MirProjection,
        contract: Option<&crate::core::mir::types::MirRecordMoveProjectionDropContract>,
        subject: &str,
    ) {
        self.validate_value(function, result, "record move/drop projection result");
        self.validate_value(function, base, "record move/drop projection base");
        let (Some(base_value), Some(result_value)) =
            (function.values.get(base), function.values.get(result))
        else {
            return;
        };
        let Some(receipt) = contract else {
            self.errors.push(NativeMirError::new(
                subject,
                "record move/drop projection has no canonical residual receipt",
            ));
            return;
        };
        if let Err(message) = self
            .program
            .type_catalog()
            .validate_record_move_projection_drop_receipt(&base_value.ty, &result_value.ty, receipt)
        {
            self.errors.push(NativeMirError::new(subject, message));
            return;
        }
        if !matches!(projection, MirProjection::Field(field) if field == &receipt.projection.field)
        {
            self.errors.push(NativeMirError::new(
                subject,
                "record move/drop projection requires the receipt's direct record field",
            ));
            return;
        }
        if let Err(message) =
            validate_native_non_copy_record_type(self.program.type_catalog(), &base_value.ty)
        {
            self.errors.push(NativeMirError::new(subject, message));
        }
    }

    fn validate_variant_project(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        base: &MirValueId,
        contract: Option<&crate::core::mir::types::MirVariantProjectionTrapContract>,
        subject: &str,
    ) {
        self.validate_value(function, result, "variant projection result");
        self.validate_value(function, base, "variant projection base");
        let (Some(base_value), Some(result_value)) =
            (function.values.get(base), function.values.get(result))
        else {
            return;
        };
        let Some(contract) = contract else {
            self.errors.push(NativeMirError::new(
                subject,
                "direct variant projection has no canonical trap receipt",
            ));
            return;
        };
        let generic_result_copy_projection = self.program.instances().values().any(|instance| {
            instance.function == function.owner
                && matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection {
                        contract
                    } if contract.projection.nominal.as_str() == "builtin:type:Result"
                        && contract.projection.ownership == MirOwnership::Copy
                )
        });
        let receipt_validation = if generic_result_copy_projection {
            self.program
                .type_catalog()
                .validate_generic_result_projection_trap_receipt(
                    &base_value.ty,
                    &result_value.ty,
                    contract,
                )
        } else {
            self.program
                .type_catalog()
                .validate_variant_projection_trap_receipt(
                    &base_value.ty,
                    &result_value.ty,
                    contract,
                )
        };
        if let Err(message) = receipt_validation {
            self.errors.push(NativeMirError::new(subject, message));
        }
    }

    fn validate_variant_project_or(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        base: &MirValueId,
        fallback: &MirValueId,
        contract: Option<&crate::core::mir::types::MirVariantProjectionFallbackContract>,
        subject: &str,
    ) {
        self.validate_value(function, result, "variant fallback projection result");
        self.validate_value(function, base, "variant fallback projection base");
        self.validate_value(function, fallback, "variant fallback projection operand");
        let (Some(base_value), Some(result_value), Some(fallback_value)) = (
            function.values.get(base),
            function.values.get(result),
            function.values.get(fallback),
        ) else {
            return;
        };
        let Some(contract) = contract else {
            self.errors.push(NativeMirError::new(
                subject,
                "variant projection fallback has no canonical receipt",
            ));
            return;
        };
        let generic_option_copy_fallback = self.program.instances().values().any(|instance| {
            instance.function == function.owner
                && matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                        contract
                    } if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership == MirOwnership::Copy
                )
        });
        let generic_result_copy_fallback = self.program.instances().values().any(|instance| {
            instance.function == function.owner
                && matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                        contract
                    } if contract.projection.nominal.as_str() == "builtin:type:Result"
                        && contract.projection.ownership == MirOwnership::Copy
                )
        });
        let receipt_validation = if generic_option_copy_fallback {
            self.program
                .type_catalog()
                .validated_copy_option_generic_projection_fallback_contract(
                    &base_value.ty,
                    &contract.projection.variant,
                    &contract.projection.field,
                    &result_value.ty,
                    &fallback_value.ty,
                )
                .map(|_| ())
        } else if generic_result_copy_fallback
            && self
                .program
                .type_catalog()
                .get(&base_value.ty)
                .is_some_and(|descriptor| {
                    matches!(
                        &descriptor.layout,
                        crate::core::mir::types::MirLayout::Result { ok, error, .. }
                            if ok == &result_value.ty && ok != error
                    )
                })
        {
            self.program
                .type_catalog()
                .validated_generic_result_scalar_projection_fallback_contract(
                    &base_value.ty,
                    &contract.projection.variant,
                    &contract.projection.field,
                    &result_value.ty,
                    &fallback_value.ty,
                )
                .map(|_| ())
        } else {
            self.program
                .type_catalog()
                .validate_variant_projection_fallback_receipt(
                    &base_value.ty,
                    &result_value.ty,
                    &fallback_value.ty,
                    contract,
                )
        };
        if let Err(message) = receipt_validation {
            self.errors.push(NativeMirError::new(subject, message));
        }
    }

    fn validate_construct(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        kind: &MirAggregateKind,
        fields: &[MirValueId],
        subject: &str,
    ) {
        self.validate_value(function, result, "aggregate result");
        for field in fields {
            self.validate_value(function, field, "aggregate field value");
        }
        if !matches!(
            kind,
            MirAggregateKind::Record { .. } | MirAggregateKind::Tuple
        ) {
            self.errors.push(NativeMirError::new(
                subject,
                "aggregate construction is outside the native MIR aggregate contract",
            ));
            return;
        }
        let Some(result_value) = function.values.get(result) else {
            return;
        };
        let field_types = fields
            .iter()
            .filter_map(|field| function.values.get(field).map(|value| value.ty.clone()))
            .collect::<Vec<_>>();
        if field_types.len() != fields.len() {
            return;
        }
        if let Err(message) =
            self.program
                .type_catalog()
                .validate_aggregate(&result_value.ty, kind, &field_types)
        {
            self.errors.push(NativeMirError::new(subject, message));
        }
    }

    fn validate_update_record(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        base: &MirValueId,
        kind: &MirAggregateKind,
        fields: &[MirValueId],
        record_update_contract: Option<&crate::core::mir::types::MirRecordUpdateContract>,
        record_update_move_contract: Option<&crate::core::mir::types::MirRecordUpdateMoveContract>,
        subject: &str,
    ) {
        self.validate_value(function, result, "record update result");
        self.validate_value(function, base, "record update base");
        for field in fields {
            self.validate_value(function, field, "record update field value");
        }
        let (Some(result_value), Some(base_value)) =
            (function.values.get(result), function.values.get(base))
        else {
            return;
        };
        if record_update_contract.is_none()
            && record_update_move_contract.is_none()
            && self
                .program
                .type_catalog()
                .get(&result_value.ty)
                .is_some_and(|desc| desc.ownership != MirOwnership::Copy)
        {
            self.errors.push(NativeMirError::new(
                subject,
                "non-Copy record update requires an explicit transfer/update contract",
            ));
            return;
        }
        if result_value.ty != base_value.ty {
            self.errors.push(NativeMirError::new(
                subject,
                "record update base and result types disagree",
            ));
            return;
        }
        let field_types = fields
            .iter()
            .filter_map(|field| function.values.get(field).map(|value| value.ty.clone()))
            .collect::<Vec<_>>();

        if field_types.len() != fields.len() {
            return;
        }
        let validation = if let Some(receipt) = record_update_move_contract {
            self.program
                .type_catalog()
                .validate_record_update_move_receipt(
                    &result_value.ty,
                    &base_value.ty,
                    kind,
                    &field_types,
                    receipt,
                )
        } else if let Some(receipt) = record_update_contract {
            self.program.type_catalog().validate_record_update_receipt(
                &result_value.ty,
                &base_value.ty,
                kind,
                &field_types,
                receipt,
            )
        } else {
            self.program.type_catalog().validate_record_update(
                &result_value.ty,
                &base_value.ty,
                kind,
                &field_types,
            )
        };
        if let Err(message) = validation {
            self.errors.push(NativeMirError::new(subject, message));
        }
    }

    fn validate_construct_variant(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        nominal: &crate::core::NominalTypeId,
        variant: &crate::core::NodeId,
        fields: &[(crate::core::NodeId, MirValueId)],
        subject: &str,
    ) {
        self.validate_value(function, result, "variant result");
        for (_, value) in fields {
            self.validate_value(function, value, "variant payload value");
        }
        let Some(result_value) = function.values.get(result) else {
            return;
        };
        let field_ids = fields
            .iter()
            .map(|(field, _)| field.clone())
            .collect::<Vec<_>>();
        let field_types = fields
            .iter()
            .filter_map(|(_, value)| function.values.get(value).map(|info| info.ty.clone()))
            .collect::<Vec<_>>();
        if field_types.len() != fields.len() {
            return;
        }
        if let Err(message) = self.program.type_catalog().validate_variant_construct(
            &result_value.ty,
            nominal,
            variant,
            &field_ids,
            &field_types,
        ) {
            self.errors.push(NativeMirError::new(subject, message));
        }
        if self.union_tagged_scrutinee(&result_value.ty) {
            match self
                .program
                .type_catalog()
                .validate_multi_target_union_variant(&result_value.ty)
            {
                Ok(()) => {}
                Err(message) => {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
        } else {
            self.validate_copy_variant_type(&result_value.ty, subject);
        }
    }

    fn validate_construct_variant_move(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        nominal: &crate::core::NominalTypeId,
        variant: &crate::core::NodeId,
        fields: &[(crate::core::NodeId, MirValueId)],
        subject: &str,
    ) {
        self.validate_value(function, result, "move variant result");
        for (_, value) in fields {
            self.validate_value(function, value, "move variant payload value");
        }
        let Some(result_value) = function.values.get(result) else {
            return;
        };
        let field_ids = fields
            .iter()
            .map(|(field, _)| field.clone())
            .collect::<Vec<_>>();
        let field_types = fields
            .iter()
            .filter_map(|(_, value)| function.values.get(value).map(|info| info.ty.clone()))
            .collect::<Vec<_>>();
        if field_types.len() != fields.len() {
            return;
        }
        if let Err(message) = self.program.type_catalog().validate_variant_construct(
            &result_value.ty,
            nominal,
            variant,
            &field_ids,
            &field_types,
        ) {
            self.errors.push(NativeMirError::new(subject, message));
        }
        if let Err(message) =
            native_non_copy_variant_payload_type(self.program.type_catalog(), &result_value.ty)
        {
            let mut message = message;
            message.subject = subject.to_owned();
            self.errors.push(message);
        }
    }

    fn validate_switch(
        &mut self,
        function: &MirFunction,
        scrutinee: &MirValueId,
        arms: &[MirSwitchArm],
        subject: &str,
    ) {
        self.validate_value(function, scrutinee, "switch scrutinee");
        let Some(scrutinee_value) = function.values.get(scrutinee) else {
            return;
        };
        let literal_switch = arms
            .iter()
            .any(|arm| matches!(arm.case, crate::core::mir::MirSwitchCase::Literal(_)));
        if self.union_tagged_scrutinee(&scrutinee_value.ty) {
            if let Err(message) = self
                .program
                .type_catalog()
                .validate_multi_target_union_variant(&scrutinee_value.ty)
            {
                self.errors.push(NativeMirError::new(subject, message));
                return;
            }
        } else if literal_switch {
            // Scalar literal switch (R6-1051): the shared catalog contract is
            // the same gate the verifier and bytecode consumer pass through.
            if let Err(message) = self
                .program
                .type_catalog()
                .validate_scalar_switch(&scrutinee_value.ty, arms)
            {
                self.errors.push(NativeMirError::new(subject, message));
                return;
            }
        } else if !self.validate_copy_variant_type(&scrutinee_value.ty, subject) {
            return;
        }
        if !literal_switch {
            if let Err(message) = self
                .program
                .type_catalog()
                .validate_switch(&scrutinee_value.ty, arms)
            {
                self.errors.push(NativeMirError::new(subject, message));
            }
        }
        for arm in arms {
            let Some(target) = function.blocks.get(&arm.target) else {
                self.errors.push(NativeMirError::new(
                    subject,
                    format!("switch edge target '{}' is absent", arm.target),
                ));
                continue;
            };
            for (index, argument) in arm.arguments.iter().enumerate() {
                self.validate_value(function, argument, "switch edge argument");
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
                    self.errors.push(NativeMirError::new(
                        subject,
                        "switch edge argument type disagrees with block parameter",
                    ));
                }
            }
            let binding_start = arm.arguments.len().checked_add(arm.bindings.len());
            if binding_start != Some(target.parameters.len()) {
                self.errors.push(NativeMirError::new(
                    subject,
                    "switch edge arguments and payload bindings disagree with block parameter arity",
                ));
            }
            let mut binding_fields = BTreeSet::new();
            let variant_id = match &arm.case {
                MirSwitchCase::Variant(variant) => Some(variant),
                MirSwitchCase::Default | MirSwitchCase::Literal(_) => None,
            };
            for (index, binding) in arm.bindings.iter().enumerate() {
                if !binding_fields.insert(binding.projection.field.clone()) {
                    self.errors.push(NativeMirError::new(
                        subject,
                        format!(
                            "switch payload field '{}' is bound more than once",
                            binding.projection.field.0
                        ),
                    ));
                }
                let Some(parameter_index) = arm.arguments.len().checked_add(index) else {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "switch binding parameter index overflows",
                    ));
                    continue;
                };
                let Some(parameter) = target
                    .parameters
                    .get(parameter_index)
                    .and_then(|parameter| function.values.get(&parameter.value))
                else {
                    continue;
                };
                let Some(variant_id) = variant_id else {
                    continue;
                };
                if binding.parameter != parameter.id {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "switch binding parameter disagrees with target block parameter",
                    ));
                }
                if let Err(message) = if self.union_tagged_scrutinee(&scrutinee_value.ty) {
                    self.program
                        .type_catalog()
                        .validate_variant_payload_projection_receipt(
                            &scrutinee_value.ty,
                            variant_id,
                            &parameter.ty,
                            &binding.projection,
                        )
                } else {
                    self.program
                        .type_catalog()
                        .validate_flat_copy_payload_projection_receipt(
                            &scrutinee_value.ty,
                            variant_id,
                            &parameter.ty,
                            &binding.projection,
                        )
                } {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
        }
    }

    fn validate_switch_move(
        &mut self,
        function: &MirFunction,
        scrutinee: &MirValueId,
        arms: &[MirSwitchArm],
        subject: &str,
    ) {
        self.validate_value(function, scrutinee, "switch-move scrutinee");
        let Some(scrutinee_value) = function.values.get(scrutinee) else {
            return;
        };
        if let Err(message) =
            native_non_copy_variant_payload_type(self.program.type_catalog(), &scrutinee_value.ty)
        {
            let mut message = message;
            message.subject = subject.to_owned();
            self.errors.push(message);
            return;
        }
        if let Err(message) = self
            .program
            .type_catalog()
            .validate_variant_switch_move_contract(&scrutinee_value.ty, arms)
        {
            self.errors.push(NativeMirError::new(subject, message));
            return;
        }

        let variants = match self
            .program
            .type_catalog()
            .variant_layout(&scrutinee_value.ty)
        {
            Some((_, variants)) => variants,
            None => {
                self.errors.push(NativeMirError::new(
                    subject,
                    "native non-Copy SwitchMove has no canonical variant layout",
                ));
                return;
            }
        };
        let required = variants
            .iter()
            .map(|variant| variant.id.clone())
            .collect::<BTreeSet<_>>();
        let mut seen = BTreeSet::new();
        if arms.len() != required.len() {
            self.errors.push(NativeMirError::new(
                subject,
                "native non-Copy SwitchMove requires exactly one explicit arm for each TypeDesc variant",
            ));
        }

        for arm in arms {
            let MirSwitchCase::Variant(variant_id) = &arm.case else {
                self.errors.push(NativeMirError::new(
                    subject,
                    "native non-Copy SwitchMove requires explicit variant arms; default/literal cases are not covered",
                ));
                continue;
            };
            let variant = match self
                .program
                .type_catalog()
                .validated_variant_switch_case(&scrutinee_value.ty, variant_id)
            {
                Ok((_, variant)) => variant,
                Err(message) => {
                    self.errors.push(NativeMirError::new(subject, message));
                    continue;
                }
            };
            if !seen.insert(variant.id.clone()) {
                self.errors.push(NativeMirError::new(
                    subject,
                    format!("switch-move variant '{}' is repeated", variant.name),
                ));
            }
            let Some(target) = function.blocks.get(&arm.target) else {
                self.errors.push(NativeMirError::new(
                    subject,
                    format!("switch-move edge target '{}' is absent", arm.target),
                ));
                continue;
            };
            for (index, argument) in arm.arguments.iter().enumerate() {
                self.validate_value(function, argument, "switch-move edge argument");
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
                    self.errors.push(NativeMirError::new(
                        subject,
                        "switch-move edge argument type disagrees with block parameter",
                    ));
                }
            }
            let binding_start = arm.arguments.len().checked_add(arm.bindings.len());
            if binding_start != Some(target.parameters.len()) {
                self.errors.push(NativeMirError::new(
                    subject,
                    "switch-move edge arguments and payload bindings disagree with block parameter arity",
                ));
            }
            // The native non-Copy TypeDesc gate has already proved the
            // complete admitted variant shape. A multi-field union arm binds
            // one distinct field per direct binding; the nested tuple group
            // keeps its own complete-group shape.
            let nested_group = arm.bindings.first().is_some_and(|first| {
                arm.bindings.len() > 1
                    && arm.bindings.iter().all(|binding| {
                        binding.nested_tuple.is_some()
                            && binding.projection.field == first.projection.field
                    })
            });
            let direct_multi_binding = self.union_tagged_scrutinee(&scrutinee_value.ty)
                && arm
                    .bindings
                    .iter()
                    .all(|binding| binding.nested_tuple.is_none());
            if arm.bindings.len() > 1 && !nested_group && !direct_multi_binding {
                self.errors.push(NativeMirError::new(
                    subject,
                    "native non-Copy SwitchMove supports one direct binding, one complete nested tuple binding group, or one direct binding per union payload field",
                ));
            }
            let mut binding_fields = BTreeSet::new();
            for (index, binding) in arm.bindings.iter().enumerate() {
                if binding.nested_tuple.is_none()
                    && !binding_fields.insert(binding.projection.field.clone())
                {
                    self.errors.push(NativeMirError::new(
                        subject,
                        format!(
                            "switch-move payload field '{}' is bound more than once",
                            binding.projection.field.0
                        ),
                    ));
                }
                let Some(parameter_index) = arm.arguments.len().checked_add(index) else {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "switch-move binding parameter index overflows",
                    ));
                    continue;
                };
                let Some(parameter) = target
                    .parameters
                    .get(parameter_index)
                    .and_then(|parameter| function.values.get(&parameter.value))
                else {
                    continue;
                };
                if binding.parameter != parameter.id {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "switch-move binding parameter disagrees with target block parameter",
                    ));
                }
                let validation = if let Some(nested) = &binding.nested_tuple {
                    self.program
                        .type_catalog()
                        .validate_variant_nested_tuple_payload_projection_receipt(
                            &scrutinee_value.ty,
                            variant_id,
                            &parameter.ty,
                            &binding.projection,
                            nested,
                        )
                } else {
                    self.program
                        .type_catalog()
                        .validate_variant_payload_projection_receipt(
                            &scrutinee_value.ty,
                            variant_id,
                            &parameter.ty,
                            &binding.projection,
                        )
                };
                if let Err(message) = validation {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
        }
        if seen != required {
            self.errors.push(NativeMirError::new(
                subject,
                "native non-Copy SwitchMove does not cover exactly the canonical TypeDesc variants",
            ));
        }
    }

    fn validate_unary(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        op: ResolvedUnaryOp,
        operand: &MirValueId,
        subject: &str,
    ) {
        self.validate_same_copy_values(function, result, operand, subject);
        let Some(operand_value) = function.values.get(operand) else {
            return;
        };
        let Some(desc) = self.program.type_catalog().get(&operand_value.ty) else {
            return;
        };
        if op == ResolvedUnaryOp::Negate && desc.abi == (MirAbiClass::Float { bits: 64 }) {
            let Some(result_ty) = function.values.get(result).map(|value| &value.ty) else {
                return;
            };
            if let Err(message) = self.program.type_catalog().validate_copy_float_unary(
                result_ty,
                &operand_value.ty,
                op,
            ) {
                self.errors.push(NativeMirError::new(subject, message));
            }
            return;
        }
        let valid = match op {
            ResolvedUnaryOp::Negate => matches!(
                desc.abi,
                MirAbiClass::Integer {
                    bits: 32 | 64,
                    signed: true
                }
            ),
            ResolvedUnaryOp::Not => matches!(
                desc.abi,
                MirAbiClass::Integer {
                    bits: 32 | 64,
                    signed: true
                } | MirAbiClass::Bool
            ),
            _ => false,
        };
        if !valid {
            self.errors.push(NativeMirError::new(
                subject,
                format!("unary operator {op:?} is outside native scalar contract"),
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
        self.validate_value(function, result, "binary result");
        self.validate_value(function, left, "binary left operand");
        self.validate_value(function, right, "binary right operand");
        let result_identity = result.clone();
        let (Some(result), Some(left), Some(right)) = (
            function.values.get(result),
            function.values.get(left),
            function.values.get(right),
        ) else {
            return;
        };
        let Some(left_desc) = self.program.type_catalog().get(&left.ty) else {
            return;
        };
        let Some(right_desc) = self.program.type_catalog().get(&right.ty) else {
            return;
        };
        let Some(result_desc) = self.program.type_catalog().get(&result.ty) else {
            return;
        };
        let managed_any_ty = crate::core::mir::types::managed_any_i32_string_v1_type_id();
        let string_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        let bool_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Bool);
        let managed_any_string_equality =
            matches!(op, ResolvedBinaryOp::Equal | ResolvedBinaryOp::NotEqual)
                && result.ty == bool_ty
                && ((left.ty == managed_any_ty && right.ty == string_ty)
                    || (right.ty == managed_any_ty && left.ty == string_ty));
        let string_equality = matches!(op, ResolvedBinaryOp::Equal | ResolvedBinaryOp::NotEqual)
            && result.ty == bool_ty
            && left.ty == string_ty
            && right.ty == string_ty;
        let string_concat = op == ResolvedBinaryOp::Add
            && result.ty == string_ty
            && left.ty == string_ty
            && right.ty == string_ty;
        if string_concat {
            if [result, left, right].iter().any(|value| {
                self.program
                    .type_catalog()
                    .validate_owned_string(&value.ty)
                    .is_err()
            }) || !crate::core::mir::validate_owned_string_concat_expression(
                function,
                &result_identity,
            ) {
                self.errors.push(NativeMirError::new(
                    subject,
                    "String Add operands/result do not have a checked owned-temporary lifetime",
                ));
            }
            return;
        }
        if managed_any_string_equality {
            if let Err(message) = super::ordinary_map_any_get::validate_managed_any_type(
                self.program.type_catalog(),
                &managed_any_ty,
            ) {
                self.errors.push(NativeMirError::new(subject, message));
            }
            if let Err(message) = self
                .program
                .type_catalog()
                .validate_owned_string(&string_ty)
            {
                self.errors.push(NativeMirError::new(subject, message));
            }
            return;
        }
        if string_equality {
            if let Err(message) = self
                .program
                .type_catalog()
                .validate_owned_string(&string_ty)
            {
                self.errors.push(NativeMirError::new(subject, message));
            }
            return;
        }
        let float_shape = [left_desc, right_desc, result_desc]
            .iter()
            .any(|desc| matches!(desc.abi, MirAbiClass::Float { bits: 32 | 64 }));
        if float_shape {
            if let Err(message) = self
                .program
                .type_catalog()
                .validate_copy_float_binary(&result.ty, &left.ty, &right.ty, op)
            {
                self.errors.push(NativeMirError::new(subject, message));
            }
            return;
        }
        let same_operands = left.ty == right.ty;
        let integer = matches!(
            left_desc.abi,
            MirAbiClass::Integer {
                bits: 32 | 64,
                signed: true
            }
        );
        let valid = match op {
            ResolvedBinaryOp::Add
            | ResolvedBinaryOp::Subtract
            | ResolvedBinaryOp::Multiply
            | ResolvedBinaryOp::Divide
            | ResolvedBinaryOp::Remainder
            | ResolvedBinaryOp::Power => same_operands && integer && result.ty == left.ty,
            ResolvedBinaryOp::Equal | ResolvedBinaryOp::NotEqual => {
                same_operands
                    && matches!(
                        left_desc.abi,
                        MirAbiClass::Integer {
                            bits: 32 | 64,
                            signed: true
                        } | MirAbiClass::Bool
                    )
                    && result_desc.abi == MirAbiClass::Bool
            }
            ResolvedBinaryOp::Less
            | ResolvedBinaryOp::Greater
            | ResolvedBinaryOp::LessEqual
            | ResolvedBinaryOp::GreaterEqual => {
                same_operands && integer && result_desc.abi == MirAbiClass::Bool
            }
            ResolvedBinaryOp::LogicalAnd | ResolvedBinaryOp::LogicalOr => {
                left_desc.abi == MirAbiClass::Bool
                    && right_desc.abi == MirAbiClass::Bool
                    && result_desc.abi == MirAbiClass::Bool
            }
            _ => false,
        };
        if !valid {
            self.errors.push(NativeMirError::new(
                subject,
                format!(
                    "binary operator {op:?} and TypeDesc shapes are outside native scalar contract"
                ),
            ));
        }
    }

    fn validate_call(
        &mut self,
        function: &MirFunction,
        result: Option<&MirValueId>,
        callee: &ResolvedCallee,
        type_arguments: &[crate::core::ResolvedTypeId],
        arguments: &[MirValueId],
        variant_call_contract: Option<&crate::core::mir::types::MirVariantCallAbiContract>,
        subject: &str,
    ) {
        if let ResolvedCallee::Extern(callee_owner) = callee {
            self.validate_ffi_call(
                function,
                result,
                callee_owner,
                type_arguments,
                arguments,
                variant_call_contract,
                subject,
            );
            return;
        }
        let Some(owner) = crate::core::mir::canonical_protocol_call_target(callee) else {
            self.errors.push(NativeMirError::new(
                subject,
                format!("callee {callee:?} is not a canonical function"),
            ));
            return;
        };
        if let Err(message) = crate::core::mir::validate_protocol_method_identity(callee) {
            self.errors.push(NativeMirError::new(subject, message));
            return;
        }
        let Some(target) = self.program.functions().get(&owner) else {
            self.errors.push(NativeMirError::new(
                subject,
                format!("callee '{}' is absent from MIR program", owner.0),
            ));
            return;
        };
        self.validate_managed_any_generic_call(
            function,
            result,
            callee,
            type_arguments,
            arguments,
            &owner,
            target,
            subject,
        );
        for message in crate::core::mir::validate_materialized_call_abi(
            callee, function, target, result, arguments,
        ) {
            self.errors.push(NativeMirError::new(subject, message));
        }
        for message in crate::core::mir::validate_materialized_call_result_presence(
            callee,
            target,
            result,
            self.program.type_catalog(),
        ) {
            self.errors.push(NativeMirError::new(subject, message));
        }
        let parameter_types = target
            .parameters
            .iter()
            .filter_map(|parameter| target.values.get(parameter))
            .map(|value| value.ty.clone())
            .collect::<Vec<_>>();
        let flat_variant_result = self
            .program
            .type_catalog()
            .validate_flat_copy_variant(&target.result)
            .is_ok();
        let move_owned_result = self
            .program
            .type_catalog()
            .validate_result_move_variant(&target.result)
            .is_ok();
        let recoverable_result = self
            .program
            .type_catalog()
            .validate_recoverable_result_variant(&target.result)
            .is_ok();
        let recoverable_transition_body = self
            .program
            .transitions()
            .get(&function.owner)
            .is_some_and(|contract| contract.effect.is_recoverable());
        if flat_variant_result || move_owned_result {
            let Some(receipt) = variant_call_contract else {
                self.errors.push(NativeMirError::new(
                    subject,
                    if flat_variant_result {
                        "call returning flat Copy Option/Result has no canonical ABI receipt"
                    } else {
                        "call returning move-owned managed Result has no canonical ABI receipt"
                    },
                ));
                return;
            };
            if let Err(message) = self
                .program
                .type_catalog()
                .validate_variant_call_abi_receipt(
                    &owner,
                    type_arguments,
                    &parameter_types,
                    &target.result,
                    receipt,
                )
            {
                self.errors.push(NativeMirError::new(subject, message));
            }
            if move_owned_result {
                if let Err(message) = crate::core::mir::validate_move_owned_result_return_merge(
                    target,
                    self.program.type_catalog(),
                ) {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
        } else if recoverable_result {
            if !recoverable_transition_body {
                self.errors.push(NativeMirError::new(
                    subject,
                    "recoverable aggregate Result call is only valid inside a recoverable Flow transition",
                ));
            } else {
                let Some(receipt) = variant_call_contract else {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "recoverable aggregate Result call has no canonical ABI receipt",
                    ));
                    return;
                };
                if receipt.mode
                    != crate::core::mir::types::MirVariantCallAbiMode::RecoverableAggregate
                {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "recoverable aggregate Result call has the wrong ABI receipt mode",
                    ));
                } else if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_variant_call_abi_receipt(
                        &owner,
                        type_arguments,
                        &parameter_types,
                        &target.result,
                        receipt,
                    )
                {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
        } else if variant_call_contract.is_some() {
            self.errors.push(NativeMirError::new(
                subject,
                "variant call ABI receipt is attached to an unsupported variant result",
            ));
        } else if self
            .program
            .type_catalog()
            .get(&target.result)
            .is_some_and(|descriptor| {
                descriptor.kind == MirTypeKind::Result && descriptor.ownership != MirOwnership::Copy
            })
        {
            self.errors.push(NativeMirError::new(
                subject,
                "non-Copy Result call result is outside the canonical call ABI contract",
            ));
        }
        for argument in arguments.iter().take(target.parameters.len()) {
            self.validate_value(function, argument, "call argument");
        }
        if let Some(result) = result {
            self.validate_value(function, result, "call result");
        }
    }

    fn validate_ffi_call(
        &mut self,
        function: &MirFunction,
        result: Option<&MirValueId>,
        callee_owner: &crate::core::NodeId,
        type_arguments: &[crate::core::ResolvedTypeId],
        arguments: &[MirValueId],
        variant_call_contract: Option<&crate::core::mir::types::MirVariantCallAbiContract>,
        subject: &str,
    ) {
        let instruction = match crate::core::mir::MirInstructionId::new(subject.to_owned()) {
            Ok(instruction) => instruction,
            Err(error) => {
                self.errors
                    .push(NativeMirError::new(subject, error.to_string()));
                return;
            }
        };
        let Some(receipt) = self.program.ffi_calls().get(&instruction) else {
            self.errors.push(NativeMirError::new(
                subject,
                "extern call has no FFI receipt",
            ));
            return;
        };
        let receipt_errors = crate::core::mir::validate_ffi_call_contract_receipt(
            self.program.type_catalog(),
            function,
            &instruction,
            callee_owner,
            result,
            arguments,
            receipt,
        );
        for message in receipt_errors {
            // Preserve the native validator's established ABI wording while
            // taking the decision itself from the shared receipt gate.
            let route_code = message
                .starts_with(crate::diagnostic::codes::MIR_FFI_DECLARATION_BOUNDARY_ERROR_CODE)
                .then_some(crate::diagnostic::codes::MIR_FFI_DECLARATION_BOUNDARY_ERROR_CODE);
            let message_without_code = message
                .strip_prefix(crate::diagnostic::codes::MIR_FFI_DECLARATION_BOUNDARY_ERROR_CODE)
                .and_then(|message| message.strip_prefix(": "))
                .unwrap_or(&message);
            let message = if let Some(rest) = message_without_code
                .strip_prefix("extern call FFI contract ABI '")
                .and_then(|rest| rest.strip_suffix("' is outside the canonical C ABI"))
            {
                let native_message =
                    format!("FFI ABI '{rest}' is outside the canonical native C ABI");
                route_code
                    .map(|code| format!("{code}: {native_message}"))
                    .unwrap_or(native_message)
            } else {
                message
            };
            let error = NativeMirError::new(subject, message);
            let error = if route_code.is_some() {
                error.with_code(crate::diagnostic::codes::MIR_FFI_DECLARATION_BOUNDARY_ERROR_CODE)
            } else {
                error
            };
            self.errors.push(error);
        }
        if !type_arguments.is_empty() {
            self.errors.push(NativeMirError::new(
                subject,
                "canonical native FFI call cannot have type arguments",
            ));
        }
        if variant_call_contract.is_some() {
            self.errors.push(NativeMirError::new(
                subject,
                "canonical native FFI call cannot carry a variant ABI receipt",
            ));
        }
        for argument in arguments {
            self.validate_ffi_scalar_value(function, argument, subject, false);
        }
        if let Some(result) = result {
            self.validate_ffi_scalar_value(function, result, subject, true);
        }
    }

    fn validate_ffi_scalar_value(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
        allow_unit_result: bool,
    ) {
        let Some(info) = function.values.get(value) else {
            self.errors.push(NativeMirError::new(
                subject,
                format!("FFI value '{}' is absent from the MIR value catalog", value),
            ));
            return;
        };
        let Some(desc) = self.program.type_catalog().get(&info.ty) else {
            self.errors.push(NativeMirError::new(
                subject,
                format!("FFI value '{}' has no TypeDesc", value),
            ));
            return;
        };
        if allow_unit_result && desc.is_canonical_ffi_unit() {
            return;
        }
        if !self
            .program
            .type_catalog()
            .is_canonical_ffi_endpoint(&info.ty, allow_unit_result)
        {
            self.errors.push(NativeMirError::new(
                subject,
                format!(
                    "FFI value '{}' has unsupported canonical ABI metadata: outside the complete canonical scalar TypeDesc contract",
                    value,
                ),
            ));
            return;
        }
        // Keep the validator on the same physical family map as declaration,
        // metadata, and value checks.  The receipt gate still owns semantic
        // conversion admission; this is only the native scalar shape guard.
        if native_ffi_scalar_shape(desc.abi).is_none() {
            self.errors.push(NativeMirError::new(
                subject,
                format!(
                    "FFI value '{}' has unsupported canonical ABI {:?}/layout {:?}",
                    value, desc.abi, desc.layout
                ),
            ));
        }
    }

    /// Keep every generic call edge carrying recursive Any tied to the exact
    /// identity or callee-cleanup instance; concrete nongeneric Any helpers
    /// continue through ordinary materialized ABI validation.
    fn validate_managed_any_generic_call(
        &mut self,
        caller: &MirFunction,
        result: Option<&MirValueId>,
        callee: &ResolvedCallee,
        type_arguments: &[crate::core::ResolvedTypeId],
        arguments: &[MirValueId],
        owner: &crate::core::NodeId,
        target: &MirFunction,
        subject: &str,
    ) {
        let catalog = self.program.type_catalog();
        let carries_dynamic_or_managed_any = type_arguments
            .iter()
            .any(|argument| catalog.contains_dynamic_or_managed_any(argument))
            || arguments.iter().any(|argument| {
                caller
                    .values
                    .get(argument)
                    .is_some_and(|value| catalog.contains_dynamic_or_managed_any(&value.ty))
            })
            || result.is_some_and(|result| {
                caller
                    .values
                    .get(result)
                    .is_some_and(|value| catalog.contains_dynamic_or_managed_any(&value.ty))
            })
            || catalog.contains_dynamic_or_managed_any(&target.result)
            || target.parameters.iter().any(|parameter| {
                target
                    .values
                    .get(parameter)
                    .is_some_and(|value| catalog.contains_dynamic_or_managed_any(&value.ty))
            });
        if !carries_dynamic_or_managed_any {
            return;
        }
        // This edge-level gate applies to generic bindings only. The two
        // whole-program seal validators above catch a generic Checker receipt
        // whose call type arguments and instance row were both erased.
        let generic_call = !type_arguments.is_empty()
            || self
                .program
                .instances()
                .values()
                .any(|instance| instance.function == *owner);
        if !generic_call {
            return;
        }
        let target_instances = self
            .program
            .instances()
            .values()
            .filter(|instance| instance.function == *owner)
            .collect::<Vec<_>>();
        let [instance] = target_instances.as_slice() else {
            self.errors.push(NativeMirError::new(
                subject,
                "native generic call carrying recursive DynamicAny/ManagedAny has no unique sealed ManagedAny instance",
            ));
            return;
        };
        let is_identity = matches!(
            &instance.contract,
            crate::core::mir::MirGenericInstanceContract::ManagedAnyIdentity
        );
        let is_trap_cleanup = matches!(
            &instance.contract,
            crate::core::mir::MirGenericInstanceContract::ManagedAnyTrapCleanup { .. }
        );
        if !is_identity && !is_trap_cleanup {
            self.errors.push(NativeMirError::new(
                subject,
                "native generic call carrying recursive DynamicAny/ManagedAny has no exact ManagedAnyIdentity or ManagedAnyTrapCleanup instance",
            ));
            return;
        }
        let any_ty =
            match catalog.validate_managed_any_identity_source_argument(&instance.arguments) {
                Ok(any_ty) => any_ty,
                Err(message) => {
                    self.errors.push(NativeMirError::new(
                        subject,
                        format!("ManagedAny generic source argument rejected: {message}"),
                    ));
                    return;
                }
            };
        let profile_validation = if is_identity {
            catalog.validate_managed_any_identity_instance(instance, target)
        } else {
            catalog.validate_managed_any_trap_cleanup_instance(instance, target)
        };
        if let Err(message) = profile_validation {
            self.errors.push(NativeMirError::new(
                subject,
                format!("ManagedAny generic Checker receipt/shape rejected: {message}"),
            ));
            return;
        }
        let direct_target = matches!(
            callee,
            ResolvedCallee::Function(called_owner) if called_owner == &instance.function
        );
        let exact_type_arguments = type_arguments == instance.arguments.as_slice();
        let result_ty = if is_identity {
            any_ty.clone()
        } else {
            crate::core::ir::primitive_type_id(crate::core::ir::PrimitiveType::I32)
        };
        let exact_signature = target.result == result_ty
            && matches!(target.parameters.as_slice(), [parameter]
                if target.values.get(parameter).is_some_and(|value| value.ty == any_ty));
        let exact_argument = matches!(arguments, [argument]
            if caller.values.get(argument).is_some_and(|value| value.ty == any_ty));
        let exact_result = result.is_some_and(|result| {
            caller
                .values
                .get(result)
                .is_some_and(|value| value.ty == result_ty)
        });
        if !(direct_target
            && exact_type_arguments
            && exact_signature
            && exact_argument
            && exact_result)
        {
            let profile_name = if is_identity {
                "ManagedAny generic identity"
            } else {
                "ManagedAny generic trap-cleanup"
            };
            self.errors.push(NativeMirError::new(
                subject,
                format!(
                    "{profile_name} call disagrees with its sealed instance, direct target, preserved source type argument, or owned ABI values"
                ),
            ));
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
            self.errors.push(NativeMirError::new(
                subject,
                format!(
                    "flow transition '{}' has no canonical contract",
                    transition.0
                ),
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
            || (!recoverable && !union && contract.targets.first() != Some(&contract.result))
        {
            self.errors.push(NativeMirError::new(
                subject,
                "FlowTransition is outside the silent-local native contract",
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
                self.errors.push(NativeMirError::new(subject, message));
            }
        }
        let Some(target) = self.program.functions().get(&contract.owner) else {
            self.errors.push(NativeMirError::new(
                subject,
                format!("flow transition '{}' target body is absent", transition.0),
            ));
            return;
        };
        if arguments.len() != target.parameters.len() {
            self.errors.push(NativeMirError::new(
                subject,
                "FlowTransition argument arity disagrees with its canonical body",
            ));
        }
        for (argument, parameter) in arguments.iter().zip(&target.parameters) {
            self.validate_value(function, argument, "FlowTransition argument");
            let Some(argument) = function.values.get(argument) else {
                continue;
            };
            let Some(parameter) = target.values.get(parameter) else {
                continue;
            };
            if argument.ty != parameter.ty {
                self.errors.push(NativeMirError::new(
                    subject,
                    "FlowTransition argument TypeDesc disagrees with its canonical body",
                ));
            }
        }
        self.validate_value(function, result, "FlowTransition result");
        if function.values.get(result).is_some_and(|value| {
            value.ty != contract.result
                || (!recoverable && value.ty != target.result)
                || (recoverable && target.result != contract.result)
        }) {
            self.errors.push(NativeMirError::new(
                subject,
                "FlowTransition result TypeDesc disagrees with its canonical contract",
            ));
        }
    }

    fn validate_terminator(
        &mut self,
        function: &MirFunction,
        terminator: &MirTerminator,
        subject: &MirBlockId,
    ) {
        let subject = subject.to_string();
        match terminator {
            MirTerminator::Goto {
                target, arguments, ..
            } => self.validate_edge(function, target, arguments, &subject),
            MirTerminator::Branch {
                condition,
                then_target,
                then_arguments,
                else_target,
                else_arguments,
                ..
            } => {
                self.validate_bool(function, condition, &subject);
                self.validate_edge(function, then_target, then_arguments, &subject);
                self.validate_edge(function, else_target, else_arguments, &subject);
            }
            MirTerminator::Switch { scrutinee, arms } => {
                self.validate_switch(function, scrutinee, arms, &subject);
            }
            MirTerminator::SwitchMove { scrutinee, arms } => {
                self.validate_switch_move(function, scrutinee, arms, &subject);
            }
            MirTerminator::Return { value } => match value {
                Some(value) => {
                    self.validate_value(function, value, "return value");
                    if function
                        .values
                        .get(value)
                        .is_some_and(|value| value.ty != function.result)
                    {
                        self.errors.push(NativeMirError::new(
                            subject,
                            "return value type disagrees with function result",
                        ));
                    }
                }
                None if self
                    .program
                    .type_catalog()
                    .get(&function.result)
                    .is_some_and(|desc| !desc.is_canonical_ffi_unit()) =>
                {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "non-unit function has a void MIR return",
                    ));
                }
                None => {}
            },
            MirTerminator::Trap { code } => {
                if let Err(message) = crate::core::mir::types::validate_trap_code(code) {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
            MirTerminator::Fault { .. } | MirTerminator::Unreachable => {
                self.errors.push(NativeMirError::new(
                    subject,
                    "fault/unreachable terminator has no native scalar contract",
                ));
            }
        }
    }

    fn validate_variant_project_move(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        base: &MirValueId,
        contract: Option<&crate::core::mir::types::MirVariantProjectionTrapContract>,
        subject: &str,
    ) {
        self.validate_value(function, result, "variant move projection result");
        self.validate_value(function, base, "variant move projection base");
        let (Some(base_value), Some(result_value)) =
            (function.values.get(base), function.values.get(result))
        else {
            return;
        };
        let Some(contract) = contract else {
            self.errors.push(NativeMirError::new(
                subject,
                "consuming direct variant projection has no canonical move receipt",
            ));
            return;
        };
        if let Err(message) = self
            .program
            .type_catalog()
            .validate_variant_move_projection_trap_receipt(
                &base_value.ty,
                &result_value.ty,
                contract,
            )
        {
            self.errors.push(NativeMirError::new(subject, message));
        }
    }

    fn validate_bool(&mut self, function: &MirFunction, value: &MirValueId, subject: &str) {
        self.validate_value(function, value, "branch condition");
        if function
            .values
            .get(value)
            .and_then(|value| self.program.type_catalog().get(&value.ty))
            .is_some_and(|desc| desc.abi != MirAbiClass::Bool)
        {
            self.errors.push(NativeMirError::new(
                subject,
                "branch condition is not canonical bool",
            ));
        }
    }

    fn validate_edge(
        &mut self,
        function: &MirFunction,
        target: &MirBlockId,
        arguments: &[MirValueId],
        subject: &str,
    ) {
        let Some(block) = function.blocks.get(target) else {
            self.errors.push(NativeMirError::new(
                subject,
                format!("edge target '{}' is absent", target),
            ));
            return;
        };
        if arguments.len() != block.parameters.len() {
            self.errors.push(NativeMirError::new(
                subject,
                "CFG edge arguments disagree with block parameter arity",
            ));
        }
        for (argument, parameter) in arguments.iter().zip(&block.parameters) {
            self.validate_value(function, argument, "edge argument");
            let (Some(argument), Some(parameter)) = (
                function.values.get(argument),
                function.values.get(&parameter.value),
            ) else {
                continue;
            };
            if argument.ty != parameter.ty {
                self.errors.push(NativeMirError::new(
                    subject,
                    "CFG edge argument type disagrees with block parameter",
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::CodeGenerator;
    use crate::core::mir::reference::MirProgram;
    use crate::lexer::Lexer;
    use crate::parser::Parser;
    use inkwell::context::Context;

    fn canonical_program(source: &str) -> MirProgram {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        MirProgram::from_checked_program(&checked).expect("canonical MIR")
    }

    fn canonical_owned_get_program() -> MirProgram {
        canonical_program(
            r#"
func i32_or_zero(value: Result<i32, i32>) -> i32 {
    match value {
        Ok(number) => number,
        Err(_) => 0,
    }
}

func main() -> i32 {
    let empty = map_new()
    let full = map_set(empty, "answer", 42)
    let result = map_get_i32(full, "answer")
    drop(empty)
    drop(full)
    i32_or_zero(result)
}
"#,
        )
    }

    fn canonical_owned_any_values_program() -> MirProgram {
        canonical_program(
            r#"
func main() -> i32 {
    let empty = map_new()
    let populated = map_set(empty, "answer", 42)
    let snapshot = values(populated)
    let count = len(snapshot)
    drop(snapshot)
    drop(empty)
    drop(populated)
    count
}
"#,
        )
    }

    #[test]
    fn ordinary_map_owned_any_v2_get_any_uses_exact_core_result_receipt() {
        let program = canonical_program(
            r#"
type Label { text: string }
func main() -> i32 {
    let label = Label { text: "answer" }
    let empty = map_new()
    let populated = map_set(empty, "answer", label)
    let pair = map_get(populated, "answer")
    let _found = pair.0
    drop(pair)
    drop(empty)
    drop(populated)
    drop(label)
    0
}
"#,
        );
        let receipts = program
            .ordinary_map_owned_any_v2_get_any_result_receipts()
            .expect("Core derives the V2 GetAny result receipt");
        let [receipt] = receipts.as_slice() else {
            panic!("expected one exact V2 GetAny result receipt, got {receipts:#?}");
        };
        receipt
            .validate_against_program(&program)
            .expect("receipt is bound to Checker actions and exact MIR");
        assert_eq!(receipt.owner.0, "function:main");
        assert!(receipt
            .instruction
            .as_str()
            .starts_with("inst:ordinary_map_owned_any_v2_get_any:"));
        assert_eq!(
            receipt.root.as_str(),
            format!("local:{}", receipt.source_local.0 .0)
        );
        assert_eq!(
            receipt.binding.as_str(),
            format!("local:{}", receipt.binding_local.0 .0)
        );
        assert_eq!(
            receipt.settlement,
            crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
        );
        assert_eq!(
            receipt.miss_payload_type,
            crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32)
        );

        let mut forged = receipt.clone();
        forged.miss_payload_type_desc_digest = "0".repeat(64);
        assert!(
            forged.validate_against_program(&program).is_err(),
            "native admission must reject a forged miss TypeDesc receipt"
        );
    }

    #[test]
    fn native_owned_any_values_materializes_v2_list_from_exact_receipts() {
        let program = canonical_owned_any_values_program();
        let receipts = program
            .ordinary_map_owned_any_v2_values_result_receipts()
            .expect("Core derives the V2 Values result receipt");
        let [receipt] = receipts.as_slice() else {
            panic!("expected one exact V2 Values result receipt, got {receipts:#?}");
        };
        receipt
            .validate_against_program(&program)
            .expect("Values receipt binds the exact source/result/binding/drop");
        let selection = program
            .ordinary_map_owned_any_v2_payload_selection_receipt()
            .expect("Core derives payload-selection evidence")
            .expect("Set payloads require a selection receipt");
        selection
            .validate_against_program(&program)
            .expect("selector is bound to exact Checker-selected TypeDescs");
        assert_eq!(receipt.payload_selection_digest, selection.selection_digest);
        NativeMirValidator::new(&program)
            .validate()
            .expect("exact V2 Values MIR and owner List shape admit native emission");

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_owned_any_values_v2");
        generator
            .compile_mir_native(&program)
            .expect("native Values emits a real V2-owned MimiList");
        generator
            .module
            .verify()
            .expect("native V2 Values module verifies");
        for symbol in [
            "mimi_ordinary_map_owned_any_v2_values_list",
            "mimi_mir_list_len_managed_any_v2",
            "mimi_mir_list_drop_managed_any_v2",
        ] {
            assert!(
                generator.module.get_function(symbol).is_some(),
                "expected the dedicated Values List ABI symbol {symbol}"
            );
        }
    }

    #[test]
    fn native_owned_any_values_rejects_forged_result_shape_before_declarations() {
        let mut program = canonical_owned_any_values_program();
        let receipts = program
            .ordinary_map_owned_any_v2_values_result_receipts()
            .expect("exact Core Values receipt");
        let [receipt] = receipts.as_slice() else {
            panic!("expected one exact V2 Values receipt");
        };
        let mut function = program
            .functions()
            .get(&receipt.owner)
            .expect("Values owner MIR")
            .clone();
        function
            .values
            .get_mut(&receipt.result)
            .expect("Values MIR result")
            .ty = receipt.checker_result_type.clone();
        program.replace_function_for_test_only(function);

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_values_v2_forged_result");
        let diagnostics = generator
            .compile_mir_native(&program)
            .expect_err("source List<DynamicAny> cannot replace the dedicated V2 MIR List");
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic.message.contains("Values")
                    && (diagnostic.message.contains("TypeDesc")
                        || diagnostic.message.contains("TypeDescs"))
            }),
            "native diagnostics: {diagnostics:#?}"
        );
        assert!(generator.module.get_function("main").is_none());
        assert!(generator
            .module
            .get_function("mimi_ordinary_map_owned_any_v2_values_list")
            .is_none());
    }

    #[test]
    fn native_owned_any_values_rejects_non_len_list_use_before_declarations() {
        let mut program = canonical_owned_any_values_program();
        let receipts = program
            .ordinary_map_owned_any_v2_values_result_receipts()
            .expect("exact Core Values receipt");
        let [receipt] = receipts.as_slice() else {
            panic!("expected one exact V2 Values receipt");
        };
        let mut function = program
            .functions()
            .get(&receipt.owner)
            .expect("Values owner MIR")
            .clone();
        let list_len = function
            .blocks
            .values_mut()
            .flat_map(|block| block.instructions.iter_mut())
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::ListOp {
                        operation: MirListOperation::Len,
                        list,
                        argument: None,
                        ..
                    } if list == &receipt.binding
                )
            })
            .expect("the receipted Values Len observation");
        let MirInstructionKind::ListOp { operation, .. } = &mut list_len.kind else {
            unreachable!("located ListOp")
        };
        *operation = MirListOperation::Reverse;
        program.replace_function_for_test_only(function);

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_values_v2_unsupported_use");
        let diagnostics = generator
            .compile_mir_native(&program)
            .expect_err("Values element access/conversion remains fail-closed");
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic.message.contains("unsupported use")
                    || diagnostic.message.contains("receipt")
            }),
            "native diagnostics: {diagnostics:#?}"
        );
        assert!(generator.module.get_function("main").is_none());
    }

    fn concrete_any_snapshot_program() -> MirProgram {
        canonical_program(
            r#"
func snapshot() {
    let root = map_new()
    let numbers = map_set(root, "number", 42)
    let mixed = map_set(numbers, "label", "owned")
    let result = map_get(mixed, "label")
    drop(root)
    drop(numbers)
    drop(mixed)
    result
}

func main() -> i32 {
    let snapshot_value = snapshot()
    drop(snapshot_value)
    0
}
"#,
        )
    }

    fn generic_any_identity_program() -> MirProgram {
        canonical_program(
            r#"
func carry<T>(value: T) -> T { value }

func main() -> string {
    let root = map_new()
    let numbers = map_set(root, "number", 42)
    let mixed = map_set(numbers, "label", "owned")
    let snapshot_value = map_get(mixed, "label")
    drop(root)
    drop(numbers)
    drop(mixed)
    let first = snapshot_value.1
    let alias = first
    let second = carry(alias)
    let text: string = second
    drop(first)
    drop(alias)
    drop(second)
    drop(snapshot_value)
    text
}
"#,
        )
    }

    #[test]
    fn native_accepts_concrete_nongeneric_any_snapshot_helper_before_emission() {
        let program = concrete_any_snapshot_program();
        NativeMirValidator::new(&program)
            .validate()
            .expect("concrete snapshot helpers are not generic identity instances");

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_concrete_any_snapshot");
        generator
            .compile_mir_native(&program)
            .expect("native should emit the concrete owned Any snapshot helper");
        assert!(generator.module.get_function("snapshot").is_some());
        assert!(generator.module.get_function("main").is_some());
    }

    #[test]
    fn native_rejects_generic_any_call_with_erased_type_arguments_before_llvm() {
        let mut program = generic_any_identity_program();
        let main_owner = crate::core::NodeId("function:main".into());
        let mut main = program
            .functions()
            .get(&main_owner)
            .expect("main MIR")
            .clone();
        let identity_target = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ManagedAnyIdentity
                )
            })
            .expect("sealed generic identity instance")
            .function
            .clone();
        let call = main
            .blocks
            .values_mut()
            .flat_map(|block| block.instructions.iter_mut())
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Call {
                        callee: ResolvedCallee::Function(target),
                        ..
                    } if target == &identity_target
                )
            })
            .expect("generic identity call");
        let MirInstructionKind::Call { type_arguments, .. } = &mut call.kind else {
            unreachable!("located generic call")
        };
        type_arguments.clear();
        program.replace_function_for_test_only(main);

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_any_missing_call_type_args");
        let diagnostics = generator
            .compile_mir_native(&program)
            .expect_err("erased generic Any type arguments must fail pre-emission");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("type arguments disagree with instance")
        }));
        assert!(generator.module.get_function("main").is_none());
        assert!(generator.module.get_function("snapshot").is_none());
    }

    #[test]
    fn missing_or_forged_generic_any_instance_rows_fail_program_construction() {
        let program = generic_any_identity_program();
        let identity = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ManagedAnyIdentity
                )
            })
            .expect("sealed generic identity instance")
            .clone();

        let mut missing_rows = program.instances().clone();
        missing_rows.remove(&identity.id);
        let missing = MirProgram::with_type_catalog_and_instances(
            program.functions().clone(),
            program.type_catalog().clone(),
            missing_rows,
        )
        .expect_err("a generic Any call cannot outlive its instance-table row");
        assert!(!missing.is_empty());

        let mut forged_rows = program.instances().clone();
        forged_rows
            .get_mut(&identity.id)
            .expect("identity instance row")
            .contract = crate::core::mir::MirGenericInstanceContract::ScalarIdentity;
        let forged = MirProgram::with_type_catalog_and_instances(
            program.functions().clone(),
            program.type_catalog().clone(),
            forged_rows,
        )
        .expect_err("an identity seal cannot be relabeled as another generic profile");
        assert!(!forged.is_empty());
    }

    #[test]
    fn native_accepts_sealed_generic_managed_any_trap_cleanup_before_emission() {
        for source in [
            include_str!(
                "../../../tests/fixtures/mir_ordinary_map_any_get_cross_callee_division.mimi"
            ),
            include_str!(
                "../../../tests/fixtures/mir_ordinary_map_any_get_cross_callee_overflow.mimi"
            ),
        ] {
            let program = canonical_program(source);
            assert!(program.instances().values().any(|instance| matches!(
                &instance.contract,
                crate::core::mir::MirGenericInstanceContract::ManagedAnyTrapCleanup { .. }
            )));
            NativeMirValidator::new(&program)
                .validate()
                .expect("only the sealed generic callee-cleanup profile admits Any here");

            let context = Context::create();
            let mut generator = CodeGenerator::new(&context, "mir_native_any_trap_cleanup");
            generator
                .compile_mir_native(&program)
                .expect("native should lower the moved Any owner and exact checked trap");
            assert!(generator.module.get_function("main").is_some());
        }
    }

    #[test]
    fn native_rejects_managed_any_trap_call_tampering_before_llvm_declarations() {
        let mut program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_ordinary_map_any_get_cross_callee_division.mimi"
        ));
        let trap_target = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ManagedAnyTrapCleanup { .. }
                )
            })
            .expect("sealed trap-cleanup instance")
            .function
            .clone();
        let main_owner = program
            .functions()
            .values()
            .find(|function| function.owner.0.ends_with("main"))
            .expect("main MIR")
            .owner
            .clone();
        let mut main = program.functions()[&main_owner].clone();
        let call = main
            .blocks
            .values_mut()
            .flat_map(|block| block.instructions.iter_mut())
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Call {
                        callee: ResolvedCallee::Function(target),
                        ..
                    } if target == &trap_target
                )
            })
            .expect("generic trap-cleanup call");
        let MirInstructionKind::Call { type_arguments, .. } = &mut call.kind else {
            unreachable!("located generic trap call")
        };
        type_arguments.clear();
        program.replace_function_for_test_only(main);

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_any_trap_missing_type_args");
        let diagnostics = generator
            .compile_mir_native(&program)
            .expect_err("erased raw DynamicAny call arguments must fail before emission");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("type arguments disagree with instance")
        }));
        assert!(generator.module.get_function("main").is_none());
    }

    #[test]
    fn native_rejects_managed_any_trap_body_reordering_before_llvm_declarations() {
        let mut program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_ordinary_map_any_get_cross_callee_division.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ManagedAnyTrapCleanup { .. }
                )
            })
            .expect("sealed trap-cleanup instance")
            .clone();
        let mut function = program.functions()[&instance.function].clone();
        function
            .blocks
            .get_mut(&function.entry)
            .expect("trap-cleanup entry")
            .instructions
            .swap(4, 5);
        program.replace_function_for_test_only(function);

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_any_trap_body_tamper");
        let diagnostics = generator
            .compile_mir_native(&program)
            .expect_err("tampered Checker-sealed order must fail before emission");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("pre-emission instance/call receipt validation failed")
        }));
        assert!(generator.module.get_function("main").is_none());
    }

    #[test]
    fn native_u32_index_boundary_check_is_width_stable() {
        assert!(!exceeds_native_u32(0));
        if let Ok(max) = usize::try_from(u32::MAX) {
            assert!(!exceeds_native_u32(max));
        }
        assert_eq!(exceeds_native_u32(usize::MAX), usize::BITS > 32);
    }

    #[test]
    fn native_owned_get_requires_its_private_checker_receipt() {
        let mut program = canonical_owned_get_program();
        let owner = crate::core::NodeId("function:main".into());
        let mut function = program.functions().get(&owner).expect("main MIR").clone();
        function
            .ownership
            .clear_checker_ordinary_map_owned_get_i32_string_v1_for_test_only();
        program.replace_function_for_test_only(function);

        let errors = NativeMirValidator::new(&program)
            .validate()
            .expect_err("owned-get MIR without its Checker receipt must be rejected");
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("independent Checker receipt")),
            "native validator diagnostics: {errors:#?}"
        );
    }

    #[test]
    fn native_owned_get_getter_result_requires_exact_result_typedesc() {
        let mut program = canonical_owned_get_program();
        let owner = crate::core::NodeId("function:main".into());
        let mut function = program.functions().get(&owner).expect("main MIR").clone();
        let getter_result = function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .find_map(|instruction| match &instruction.kind {
                MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetI32 { result, .. } => {
                    Some(result.clone())
                }
                _ => None,
            })
            .expect("GetI32 instruction result");
        let forged_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        function
            .values
            .get_mut(&getter_result)
            .expect("getter result value")
            .ty = forged_type;
        program.replace_function_for_test_only(function);

        let errors = NativeMirValidator::new(&program)
            .validate()
            .expect_err("GetI32 must not accept a scalar in place of Result<i32, i32>");
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("canonical Result<i32, i32>")),
            "native validator diagnostics: {errors:#?}"
        );
    }

    #[test]
    fn native_owned_get_rejects_mixed_map_owner_authority() {
        let owned_get = canonical_owned_get_program();
        let legacy = canonical_program(
            r#"
func old_map_profile() -> i32 {
    let root = map_new()
    drop(root)
    0
}
func main() -> i32 { 0 }
"#,
        );
        let legacy_owner = legacy
            .type_catalog()
            .iter()
            .find(|(_, descriptor)| {
                matches!(
                    &descriptor.kind,
                    MirTypeKind::MapRoot
                        | MirTypeKind::MapAnyOwnerV1
                        | MirTypeKind::OrdinaryMapOwnerLifecycleI32StringV1
                )
            })
            .map(|(id, descriptor)| (id.clone(), descriptor.clone()))
            .expect("legacy Map profile TypeDesc");
        let mut functions = owned_get.functions().clone();
        let old_function_owner = crate::core::NodeId("function:old_map_profile".into());
        functions.insert(
            old_function_owner.clone(),
            legacy
                .functions()
                .get(&old_function_owner)
                .expect("legacy Map profile function")
                .clone(),
        );
        let mut catalog = owned_get.type_catalog().clone();
        catalog.replace_for_test_only(legacy_owner.0, legacy_owner.1);
        let mixed = MirProgram::with_type_catalog(functions, catalog)
            .expect("cross-function profile mix is structurally valid before native admission");

        let errors = NativeMirValidator::new(&mixed)
            .validate()
            .expect_err("different Map owner profiles cannot share native authority");
        assert!(
            errors.iter().any(|error| {
                error.subject == "Map owner profile"
                    && error.message.contains("cannot share authority")
            }),
            "native validator diagnostics: {errors:#?}"
        );
    }

    #[test]
    fn ffi_scalar_value_requires_complete_copy_metadata() {
        let program = canonical_program("func main() -> i64 { 1 }");
        let owner = crate::core::NodeId("function:main".into());
        let function = program.functions().get(&owner).expect("main MIR");
        let scalar_id = function.result.clone();
        let value = function
            .values
            .values()
            .find(|value| value.ty == scalar_id)
            .expect("scalar MIR value")
            .id
            .clone();

        let mut validator = NativeMirValidator::new(&program);
        validator.validate_ffi_scalar_value(function, &value, "canonical FFI", false);
        assert!(validator.errors.is_empty(), "{:#?}", validator.errors);

        let descriptor = program
            .type_catalog()
            .get(&scalar_id)
            .expect("scalar TypeDesc")
            .clone();
        for mutation in 0..4 {
            let mut catalog = program.type_catalog().clone();
            let mut forged = descriptor.clone();
            match mutation {
                0 => forged.session_protocol = Some(scalar_id.clone()),
                1 => forged.needs_drop_glue = true,
                2 => {
                    forged.drop_plan =
                        Some(crate::core::mir::types::MirDropGluePlan { fields: Vec::new() })
                }
                3 => forged.variant_drop_plan = Some(Vec::new()),
                _ => unreachable!(),
            }
            catalog.replace_for_test_only(scalar_id.clone(), forged);
            let forged_program =
                MirProgram::with_type_catalog(program.functions().clone(), catalog)
                    .expect("forged scalar catalog remains structurally valid");
            let forged_function = forged_program.functions().get(&owner).expect("main MIR");
            let mut validator = NativeMirValidator::new(&forged_program);
            validator.validate_ffi_scalar_value(forged_function, &value, "forged FFI", false);
            assert!(
                validator.errors.iter().any(|error| error
                    .message
                    .contains("complete canonical scalar TypeDesc contract")),
                "unexpected forged FFI diagnostics: {:?}",
                validator.errors
            );
        }
    }

    #[test]
    fn native_builtin_scalar_values_require_complete_copy_metadata() {
        let program = canonical_program(
            r#"
func main() -> i64 {
    println(1)
    0
}
"#,
        );
        let owner = crate::core::NodeId("function:main".into());
        let function = program.functions().get(&owner).expect("main MIR");
        let (builtin_kind, arguments, result, instruction_id) = function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .find_map(|instruction| {
                let MirInstructionKind::BuiltinCall {
                    kind,
                    arguments,
                    result,
                    ..
                } = &instruction.kind
                else {
                    return None;
                };
                Some((
                    *kind,
                    arguments.clone(),
                    result.clone(),
                    instruction.id.clone(),
                ))
            })
            .expect("println builtin MIR instruction");
        assert_eq!(builtin_kind, MirBuiltinKind::PrintlnInt);
        let argument = arguments.first().expect("println argument");
        let scalar_id = function
            .values
            .get(argument)
            .expect("println argument value")
            .ty
            .clone();
        let descriptor = program
            .type_catalog()
            .get(&scalar_id)
            .expect("println scalar TypeDesc")
            .clone();
        let instruction = MirInstructionKind::BuiltinCall {
            kind: builtin_kind,
            arguments,
            result,
            string_field_contract: None,
        };

        for mutation in 0..4 {
            let mut catalog = program.type_catalog().clone();
            let mut forged = descriptor.clone();
            match mutation {
                0 => forged.session_protocol = Some(scalar_id.clone()),
                1 => forged.needs_drop_glue = true,
                2 => {
                    forged.drop_plan =
                        Some(crate::core::mir::types::MirDropGluePlan { fields: Vec::new() })
                }
                3 => forged.variant_drop_plan = Some(Vec::new()),
                _ => unreachable!(),
            }
            catalog.replace_for_test_only(scalar_id.clone(), forged);
            let forged_program =
                MirProgram::with_type_catalog(program.functions().clone(), catalog)
                    .expect("forged builtin scalar catalog remains structurally valid");
            let forged_function = forged_program.functions().get(&owner).expect("main MIR");
            let mut validator = NativeMirValidator::new(&forged_program);
            validator.validate_instruction(forged_function, &instruction, instruction_id.as_str());
            assert!(
                validator.errors.iter().any(|error| {
                    error.message.contains("builtin 'println' argument")
                        && error.message.contains("native scalar contract")
                }),
                "unexpected forged builtin diagnostics: {:?}",
                validator.errors
            );
        }
    }
}
