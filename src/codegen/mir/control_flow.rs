//! CFG, switch, edge, and trap lowering for native MIR.

use super::*;

impl<'a, 'ctx> NativeMirFunctionEmitter<'a, 'ctx> {
    fn emit_main_managed_any_v2_list_guard(&self, subject: &str) -> Result<(), NativeMirError> {
        if self.function.owner.0 != "function:main" {
            return Ok(());
        }
        let guard = self
            .generator
            .module
            .get_function("mimi_mir_assert_no_live_managed_any_v2_lists")
            .ok_or_else(|| {
                NativeMirError::new(
                    subject,
                    "canonical MIR main is missing the Values-list lifecycle guard",
                )
            })?;
        self.generator
            .builder
            .build_call(guard, &[], "mir_main_values_list_guard")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        Ok(())
    }

    pub(super) fn emit_terminator(
        &mut self,
        terminator: &MirTerminator,
        subject: &MirBlockId,
    ) -> Result<(), NativeMirError> {
        let current = self.generator.builder.get_insert_block().ok_or_else(|| {
            NativeMirError::new(
                subject.to_string(),
                "terminator has no LLVM insertion block",
            )
        })?;
        match terminator {
            MirTerminator::Goto {
                target, arguments, ..
            } => {
                self.queue_edge(target, arguments, current, subject)?;
                let target_block = *self.blocks.get(target).ok_or_else(|| {
                    NativeMirError::new(subject.to_string(), "goto target LLVM block is absent")
                })?;
                self.generator
                    .builder
                    .build_unconditional_branch(target_block)
                    .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
            }
            MirTerminator::Branch {
                condition,
                then_target,
                then_arguments,
                else_target,
                else_arguments,
                ..
            } => {
                let condition = self
                    .value(condition, &subject.to_string())?
                    .into_int_value();
                self.queue_edge(then_target, then_arguments, current, subject)?;
                self.queue_edge(else_target, else_arguments, current, subject)?;
                let then_block = *self.blocks.get(then_target).ok_or_else(|| {
                    NativeMirError::new(subject.to_string(), "then target LLVM block is absent")
                })?;
                let else_block = *self.blocks.get(else_target).ok_or_else(|| {
                    NativeMirError::new(subject.to_string(), "else target LLVM block is absent")
                })?;
                self.generator
                    .builder
                    .build_conditional_branch(condition, then_block, else_block)
                    .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
            }
            MirTerminator::Switch { scrutinee, arms } => {
                self.emit_switch(scrutinee, arms, subject)?;
            }
            MirTerminator::SwitchMove { scrutinee, arms } => {
                self.emit_switch_move(scrutinee, arms, subject)?;
            }
            MirTerminator::Return { value } => {
                self.emit_main_managed_any_v2_list_guard(&subject.to_string())?;
                if super::actor_status::is_actor_tagged(self.program, &self.function.owner) {
                    let value = value.as_ref().ok_or_else(|| {
                        NativeMirError::new(
                            subject.to_string(),
                            "tagged Actor return has no i32 result",
                        )
                    })?;
                    let value = self.value(value, &subject.to_string())?.into_int_value();
                    return self.emit_actor_status_return(
                        self.generator.context.i32_type().const_zero(),
                        value,
                        &subject.to_string(),
                    );
                }
                // Unit calls still carry a semantic MIR result identity so the
                // reference/bytecode consumers can preserve call ordering, but
                // their native ABI has no LLVM value to insert into the return.
                // Use the declared function ABI as the source of truth instead
                // of looking up that phantom unit value in `self.values`.
                if self.llvm_function.get_type().get_return_type().is_none() {
                    self.generator.builder.build_return(None).map_err(|error| {
                        NativeMirError::new(subject.to_string(), error.to_string())
                    })?;
                } else if let Some(value) = value {
                    let value = self.value(value, &subject.to_string())?;
                    self.generator
                        .builder
                        .build_return(Some(&value as &dyn BasicValue))
                        .map_err(|error| {
                            NativeMirError::new(subject.to_string(), error.to_string())
                        })?;
                } else {
                    self.generator.builder.build_return(None).map_err(|error| {
                        NativeMirError::new(subject.to_string(), error.to_string())
                    })?;
                }
            }
            MirTerminator::Trap { code } => {
                if let Err(message) = crate::core::mir::types::validate_trap_code(code) {
                    return Err(NativeMirError::new(subject.to_string(), message));
                }
                self.emit_abort_with_message(code, &subject.to_string())?;
            }
            _ => {
                return Err(NativeMirError::new(
                    subject.to_string(),
                    "unvalidated terminator reached native emitter",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn emit_switch_move(
        &mut self,
        scrutinee: &MirValueId,
        arms: &[MirSwitchArm],
        subject: &MirBlockId,
    ) -> Result<(), NativeMirError> {
        let scrutinee_value = self.value(scrutinee, &subject.to_string())?;
        let scrutinee_ty = self.value_type(scrutinee, &subject.to_string())?;
        let is_ordinary_map_set_result = self.ordinary_map_set_result_l1_authorized
            && self
                .function
                .ownership
                .ordinary_map_set_lineage_receipt()
                .is_some();
        let is_ordinary_map_set_get_i32_result = self.ordinary_map_set_get_i32_l1_authorized
            && self
                .function
                .ownership
                .ordinary_map_set_get_i32_lineage_receipt()
                .is_some();
        let is_ordinary_map_set_get_any_i32_result = self
            .ordinary_map_set_get_any_i32_l1_authorized
            && self
                .function
                .ownership
                .ordinary_map_set_get_any_i32_lineage_receipt()
                .is_some();
        let is_ordinary_map_set_get_any_result_i32 = self
            .ordinary_map_set_get_any_result_i32_l1_authorized
            && self
                .function
                .ownership
                .ordinary_map_owned_any_v2_set_get_any_result_i32_lineage_receipt()
                .is_some();
        let is_ordinary_map_set_get_string_result = self.ordinary_map_set_get_string_l1_authorized
            && self
                .function
                .ownership
                .ordinary_map_set_get_string_lineage_receipt()
                .is_some();
        let is_ordinary_map_new_result = self.ordinary_map_new_result_l1_authorized
            && self
                .function
                .ownership
                .ordinary_map_new_result_receipt()
                .is_some();
        let is_ordinary_map_direct_tuple_get_l1_result = self
            .ordinary_map_direct_tuple_get_unpack_l1_authorized
            && self
                .function
                .ownership
                .ordinary_map_direct_tuple_get_unpack_l1_receipt()
                .is_some();
        let is_ordinary_map_direct_print_result = self.ordinary_map_direct_print_result_authorized
            && self
                .function
                .ownership
                .ordinary_map_owned_any_v2_direct_print_result_receipt()
                .is_some();
        let is_ordinary_map_owned_get_result_branch_v3 = self
            .ordinary_map_owned_get_result_branch_v3_authorized
            && scrutinee_ty
                == crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_root_result_type_id(
                )
            && self
                .function
                .ownership
                .ordinary_map_owned_get_result_branch_v3_receipt()
                .is_some();
        let computed_owner_chain_result_ty =
            crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id();
        let is_values_local_index_i32_result_branch = self
            .values_local_index_i32_result_branch_l1_authorized
            && scrutinee_ty == computed_owner_chain_result_ty
            && self
                .function
                .ownership
                .ordinary_map_owned_any_v2_cfg_receipt()
                .and_then(|receipt| {
                    receipt
                        .values_local_index_i32_result_branch_receipt
                        .as_ref()
                })
                .is_some();
        let is_values_result_crosscall_string_caller = self.values_result_authorization.is_some()
            && scrutinee_ty == computed_owner_chain_result_ty
            && self
                .function
                .ownership
                .ordinary_map_owned_any_v2_values_result_producer_receipt()
                .is_some();
        let is_map_set_drop_control_result_branch = self.map_set_drop_control_result_l1_authorized
            && scrutinee_ty == computed_owner_chain_result_ty
            && self
                .function
                .ownership
                .ordinary_map_owned_any_v2_cfg_receipt()
                .and_then(|receipt| receipt.map_set_drop_control_result_branch_receipt.as_ref())
                .is_some();
        let is_computed_string_key_owner_chain_result = self
            .computed_string_key_owner_chain_l1_authorized
            && scrutinee_ty == computed_owner_chain_result_ty;
        if is_computed_string_key_owner_chain_result {
            self.validate_computed_string_key_owner_chain_switch_move(
                scrutinee,
                arms,
                &scrutinee_ty,
                subject,
            )?;
        }
        let is_dedicated_root_result_route = is_computed_string_key_owner_chain_result
            || is_ordinary_map_set_get_i32_result
            || is_ordinary_map_set_get_any_i32_result
            || is_ordinary_map_set_get_any_result_i32
            || is_ordinary_map_set_get_string_result
            || is_ordinary_map_set_result
            || is_ordinary_map_new_result
            || is_ordinary_map_direct_tuple_get_l1_result
            || is_ordinary_map_direct_print_result
            || is_values_local_index_i32_result_branch
            || is_values_result_crosscall_string_caller
            || is_map_set_drop_control_result_branch;
        let is_result_branch_v1 = scrutinee_ty == computed_owner_chain_result_ty
            && !is_dedicated_root_result_route
            && crate::core::mir::validate_ordinary_map_owned_any_v2_result_branch_v1_types(
                self.program.type_catalog(),
                self.function,
            )
            .is_ok();
        if scrutinee_ty == computed_owner_chain_result_ty
            && !is_dedicated_root_result_route
            && !is_result_branch_v1
        {
            return Err(NativeMirError::new(
                subject.to_string(),
                "synthetic Result<owner-v2 root, i32> SwitchMove requires one of its exact CheckedProgram-bound branch routes",
            ));
        }
        if is_ordinary_map_direct_print_result {
            self.validate_nested_direct_print_result_switch_move(
                scrutinee,
                arms,
                &scrutinee_ty,
                subject,
            )?;
        }
        let (variant_abi, _) = if is_computed_string_key_owner_chain_result {
            super::native_ordinary_map_computed_string_key_owner_chain_set_any_result_variant_abi(
                self.function,
                self.program.type_catalog(),
                &scrutinee_ty,
            )?
        } else if is_ordinary_map_direct_print_result {
            let (variant_abi, _result_llvm_type) =
                super::ordinary_map_owned_any_v2::native_direct_print_result_root_result_abi(
                    self.generator.context,
                    self.function,
                    self.program.type_catalog(),
                    &scrutinee_ty,
                    subject.as_str(),
                )?;
            // This branch returns the concrete LLVM struct layout; the other
            // native variant helpers return the canonical MIR type identity.
            // SwitchMove uses only the variant ABI, so normalize the ignored
            // second tuple item to the same identity contract as those helpers.
            (variant_abi, scrutinee_ty.clone())
        } else if is_values_local_index_i32_result_branch {
            super::ordinary_map_owned_any_v2::native_values_local_index_i32_result_branch_variant_abi(
                self.function,
                self.program.type_catalog(),
                &scrutinee_ty,
            )?
        } else if is_values_result_crosscall_string_caller {
            super::ordinary_map_owned_any_v2::native_values_result_crosscall_string_caller_variant_abi(
                self.function,
                self.program.type_catalog(),
                &scrutinee_ty,
            )?
        } else if is_map_set_drop_control_result_branch {
            super::abi::native_ordinary_map_set_drop_control_result_variant_abi(
                self.function,
                self.program.type_catalog(),
                &scrutinee_ty,
            )?
        } else if is_ordinary_map_set_get_any_result_i32 {
            if scrutinee_ty
                == crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id()
            {
                super::native_ordinary_map_set_get_any_result_i32_root_variant_abi(
                    self.function,
                    self.program.type_catalog(),
                    &scrutinee_ty,
                )?
            } else if scrutinee_ty
                == crate::core::mir::types::ordinary_map_owned_any_v2_get_any_result_envelope_type_id()
            {
                super::native_ordinary_map_set_get_any_result_i32_variant_abi(
                    self.function,
                    self.program.type_catalog(),
                    &scrutinee_ty,
                )?
            } else {
                return Err(NativeMirError::new(
                    subject.to_string(),
                    "Result-valued GetAny SwitchMove has an unauthorized Result TypeDesc",
                ));
            }
        } else if is_ordinary_map_set_get_any_i32_result {
            super::native_ordinary_map_set_get_any_i32_result_variant_abi(
                self.function,
                self.program.type_catalog(),
                &scrutinee_ty,
            )?
        } else if is_ordinary_map_set_get_i32_result {
            super::native_ordinary_map_set_result_variant_abi(
                self.function,
                self.program.type_catalog(),
                &scrutinee_ty,
            )?
        } else if is_ordinary_map_set_get_string_result {
            super::native_ordinary_map_set_get_string_result_variant_abi(
                self.function,
                self.program.type_catalog(),
                &scrutinee_ty,
            )?
        } else if is_ordinary_map_set_result {
            super::native_ordinary_map_set_result_variant_abi(
                self.function,
                self.program.type_catalog(),
                &scrutinee_ty,
            )?
        } else if is_ordinary_map_new_result {
            super::native_ordinary_map_new_result_variant_abi(
                self.function,
                self.program.type_catalog(),
                &scrutinee_ty,
            )?
        } else if is_ordinary_map_direct_tuple_get_l1_result {
            super::native_ordinary_map_direct_tuple_get_l1_result_variant_abi(
                self.function,
                self.program.type_catalog(),
                &scrutinee_ty,
            )?
        } else if is_ordinary_map_owned_get_result_branch_v3 {
            super::native_ordinary_map_owned_get_result_branch_v3_variant_abi(
                self.function,
                self.program.type_catalog(),
                &scrutinee_ty,
            )?
        } else if is_result_branch_v1 {
            super::native_ordinary_map_result_branch_v1_variant_abi(
                self.function,
                self.program.type_catalog(),
                &scrutinee_ty,
            )?
        } else {
            native_variant_abi(self.program.type_catalog(), &scrutinee_ty, true)?
        };
        let tag = self
            .generator
            .builder
            .build_extract_value(
                scrutinee_value.into_struct_value(),
                variant_abi.tag_field,
                "mir_variant_move_tag_load",
            )
            .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?
            .into_int_value();

        for (index, arm) in arms.iter().enumerate() {
            let MirSwitchCase::Variant(variant_id) = &arm.case else {
                return Err(NativeMirError::new(
                    subject.to_string(),
                    "native non-Copy SwitchMove requires explicit variant arms",
                ));
            };
            let variant = self
                .program
                .type_catalog()
                .validated_variant_switch_case(&scrutinee_ty, variant_id)
                .map_err(|message| NativeMirError::new(subject.to_string(), message))?
                .1
                .clone();
            let payload_slots: Vec<_> = variant_abi.payload_slots(&variant.id).cloned().collect();
            let has_payload = !payload_slots.is_empty();
            let condition = self
                .generator
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    tag,
                    self.generator
                        .context
                        .i8_type()
                        .const_int(u64::from(variant.discriminant), false),
                    "mir_variant_move_case",
                )
                .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
            let target = *self.blocks.get(&arm.target).ok_or_else(|| {
                NativeMirError::new(subject.to_string(), "switch-move target is absent")
            })?;
            let current = self.generator.builder.get_insert_block().ok_or_else(|| {
                NativeMirError::new(subject.to_string(), "switch-move case has no LLVM block")
            })?;
            let next = if index + 1 < arms.len() {
                Some(
                    self.generator
                        .context
                        .append_basic_block(self.llvm_function, "mir_variant_move_next"),
                )
            } else {
                None
            };

            if has_payload && arm.bindings.is_empty() {
                if payload_slots.is_empty() {
                    return Err(NativeMirError::new(
                        subject.to_string(),
                        "payload variant has no native ABI payload slot",
                    ));
                }
                let drop_payload = self
                    .generator
                    .context
                    .append_basic_block(self.llvm_function, "mir_variant_move_drop_payload");
                let false_target = next.unwrap_or_else(|| {
                    self.generator
                        .context
                        .append_basic_block(self.llvm_function, "mir_variant_move_invalid")
                });
                self.generator
                    .builder
                    .build_conditional_branch(condition, drop_payload, false_target)
                    .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
                self.generator.builder.position_at_end(drop_payload);
                // Multi-field variants own one obligation per payload slot;
                // the consuming arm without bindings settles all of them.
                for payload_slot in &payload_slots {
                    let payload = self
                        .generator
                        .builder
                        .build_extract_value(
                            scrutinee_value.into_struct_value(),
                            payload_slot.physical_field,
                            "mir_variant_move_drop_payload",
                        )
                        .map_err(|error| {
                            NativeMirError::new(subject.to_string(), error.to_string())
                        })?;
                    self.emit_drop_value(payload, &payload_slot.ty, &subject.to_string())?;
                }
                let drop_predecessor =
                    self.generator.builder.get_insert_block().ok_or_else(|| {
                        NativeMirError::new(
                            subject.to_string(),
                            "switch-move drop block has no LLVM insertion block",
                        )
                    })?;
                self.queue_variant_edge(
                    &arm.target,
                    &arm.arguments,
                    &arm.bindings,
                    &scrutinee_ty,
                    &variant,
                    &variant_abi,
                    false,
                    scrutinee_value,
                    drop_predecessor,
                    subject,
                )?;
                self.generator
                    .builder
                    .build_unconditional_branch(target)
                    .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
                if let Some(next) = next {
                    self.generator.builder.position_at_end(next);
                } else {
                    self.generator.builder.position_at_end(false_target);
                    self.emit_abort_with_message(
                        "[E0800] canonical MIR variant tag is invalid",
                        &subject.to_string(),
                    )?;
                }
            } else {
                self.queue_variant_edge(
                    &arm.target,
                    &arm.arguments,
                    &arm.bindings,
                    &scrutinee_ty,
                    &variant,
                    &variant_abi,
                    false,
                    scrutinee_value,
                    current,
                    subject,
                )?;
                let false_target = next.unwrap_or_else(|| {
                    self.generator
                        .context
                        .append_basic_block(self.llvm_function, "mir_variant_move_invalid")
                });
                self.generator
                    .builder
                    .build_conditional_branch(condition, target, false_target)
                    .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
                if let Some(next) = next {
                    self.generator.builder.position_at_end(next);
                } else {
                    self.generator.builder.position_at_end(false_target);
                    self.emit_abort_with_message(
                        "[E0800] canonical MIR variant tag is invalid",
                        &subject.to_string(),
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Bind the current SwitchMove to the exact Checker branch receipt before
    /// the synthetic Result<root, i32> ABI is selected. The full MIR route
    /// replay checks both Result arm projections and all CFG edges; this local
    /// check makes the emitter prove that this block consumes the corresponding
    /// branch operation, rather than admitting the TypeDesc globally.
    fn validate_computed_string_key_owner_chain_switch_move(
        &self,
        scrutinee: &MirValueId,
        arms: &[MirSwitchArm],
        scrutinee_ty: &crate::core::ResolvedTypeId,
        subject: &MirBlockId,
    ) -> Result<(), NativeMirError> {
        use crate::core::ownership::{
            OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainProfileVersion as ChainVersion,
            OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainResultBranchProfileVersion as BranchVersion,
        };
        use crate::core::{cfg::Terminator, ownership::ManagedAnyCfgMatchCase};

        let fail = |message: &str| {
            NativeMirError::new(
                subject.to_string(),
                format!(
                    "ComputedStringKeyOwnerChain SwitchMove is not bound to its exact branch receipt: {message}"
                ),
            )
        };
        let expected_result_ty =
            crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id();
        if scrutinee_ty != &expected_result_ty {
            return Err(fail("scrutinee is not Result<synthetic root, i32>"));
        }
        let source_result_ty =
            crate::core::mir::types::ordinary_map_owned_any_v2_source_result_type_id()
                .map_err(|message| fail(&message))?;
        self.program
            .type_catalog()
            .validate_ordinary_map_owned_any_v2_root_result(&expected_result_ty, &source_result_ty)
            .map_err(|message| fail(&message))?;

        let cfg = self
            .function
            .ownership
            .ordinary_map_owned_any_v2_cfg_receipt()
            .ok_or_else(|| fail("function has no Checker-owned owner-v2 CFG receipt"))?;
        if cfg.owner != self.function.owner || arms.len() != 2 {
            return Err(fail("owner or Result arm count differs"));
        }
        let branches = cfg
            .result_receipts
            .iter()
            .filter_map(|receipt| {
                receipt
                    .computed_string_key_owner_chain
                    .as_deref()
                    .and_then(|chain| chain.result_branch.as_deref().map(|branch| (chain, branch)))
            })
            .collect::<Vec<_>>();
        let [(chain, branch)] = branches.as_slice() else {
            return Err(fail(
                "there is not exactly one embedded Result-branch receipt",
            ));
        };
        if chain.owner != self.function.owner
            || branch.owner != self.function.owner
            || chain.profile_version != ChainVersion::V1
            || branch.profile_version != BranchVersion::V1
            || branch.map_new_operation != chain.map_new.point
            || branch.map_set_operation != chain.map_set.point
            || branch.initial_root != chain.map_new.local
            || branch.populated_root != chain.map_set.local
        {
            return Err(fail(
                "receipt owner, profile, or New/Set operation identity differs",
            ));
        }

        let new_value = MirValueId::new(format!("expr:{}", branch.map_new_operation.0))
            .map_err(|error| fail(&error.to_string()))?;
        let set_value = MirValueId::new(format!("expr:{}", branch.map_set_operation.0))
            .map_err(|error| fail(&error.to_string()))?;
        let (match_node, operation, ok_arm, err_arm) = if scrutinee == &new_value {
            (
                &branch.map_new_match,
                &branch.map_new_operation,
                &branch.map_new_ok_arm,
                &branch.map_new_err_arm,
            )
        } else if scrutinee == &set_value {
            (
                &branch.map_set_match,
                &branch.map_set_operation,
                &branch.map_set_ok_arm,
                &branch.map_set_err_arm,
            )
        } else {
            return Err(fail("scrutinee does not name the exact New or Set result"));
        };
        if self
            .function
            .values
            .get(scrutinee)
            .is_none_or(|value| value.ty != expected_result_ty)
        {
            return Err(fail("scrutinee value has a different result TypeDesc"));
        }

        crate::core::mir::validate_ordinary_map_owned_any_v2_computed_string_key_owner_chain_mir_route(
            self.program,
        )
        .map_err(|message| fail(&message))?;

        let wanted_arms = std::collections::BTreeSet::from([ok_arm.clone(), err_arm.clone()]);
        let dispatches = branch
            .cfg
            .match_arms
            .iter()
            .filter(|(block_id, source_arms)| {
                let observed = source_arms
                    .iter()
                    .map(|arm| arm.source_arm.clone())
                    .collect::<std::collections::BTreeSet<_>>();
                let Some(source_block) = branch.cfg.reachable_blocks.get(*block_id) else {
                    return false;
                };
                let match_join_exists = branch
                    .cfg
                    .reachable_blocks
                    .values()
                    .any(|block| block.source.node == *match_node);
                let match_scrutinee_is_exact = matches!(
                    &source_block.terminator,
                    Terminator::Match { scrutinee, arms }
                        if scrutinee == operation && arms.len() == 2
                );
                observed == wanted_arms && match_join_exists && match_scrutinee_is_exact
            })
            .collect::<Vec<_>>();
        let [(dispatch, source_arms)] = dispatches.as_slice() else {
            return Err(fail(
                "source match does not have one exact Checker dispatch block",
            ));
        };
        let dispatch_id =
            MirBlockId::new(dispatch.0 .0.clone()).map_err(|error| fail(&error.to_string()))?;
        if subject != &dispatch_id
            || source_arms.len() != 2
            || arms.iter().any(|arm| {
                !matches!(
                    &arm.case,
                    MirSwitchCase::Variant(variant)
                        if variant == &crate::core::NodeId("builtin:variant:Result::Ok".into())
                            || variant == &crate::core::NodeId("builtin:variant:Result::Err".into())
                )
            })
        {
            return Err(fail(
                "MIR switch block or Result::Ok/Err case census differs",
            ));
        }
        for (source_arm_id, expected_case) in [
            (
                ok_arm,
                crate::core::NodeId("builtin:variant:Result::Ok".into()),
            ),
            (
                err_arm,
                crate::core::NodeId("builtin:variant:Result::Err".into()),
            ),
        ] {
            let source_arm = source_arms
                .iter()
                .find(|arm| arm.source_arm == *source_arm_id)
                .ok_or_else(|| fail("branch arm ID is absent from the Checker receipt"))?;
            if source_arm.guarded
                || source_arm.case != ManagedAnyCfgMatchCase::Variant(expected_case.clone())
            {
                return Err(fail("Checker Result arm variant or guard differs"));
            }
            let edge = branch
                .cfg
                .reachable_edges
                .get(&source_arm.edge)
                .ok_or_else(|| fail("Checker arm edge is absent"))?;
            let expected_edge = crate::core::mir::MirEdgeId::new(source_arm.edge.0 .0.clone())
                .map_err(|error| fail(&error.to_string()))?;
            let expected_target =
                MirBlockId::new(edge.to.0 .0.clone()).map_err(|error| fail(&error.to_string()))?;
            let matching = arms
                .iter()
                .filter(|arm| {
                    arm.edge == expected_edge
                        && arm.target == expected_target
                        && arm.case == MirSwitchCase::Variant(expected_case.clone())
                })
                .count();
            if matching != 1 {
                return Err(fail(
                    "MIR Result arm edge or target differs from the Checker path",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn emit_switch(
        &mut self,
        scrutinee: &MirValueId,
        arms: &[MirSwitchArm],
        subject: &MirBlockId,
    ) -> Result<(), NativeMirError> {
        let scrutinee_value = self.value(scrutinee, &subject.to_string())?;
        let scrutinee_ty = self.value_type(scrutinee, &subject.to_string())?;
        let variant_arms = arms
            .iter()
            .filter(|arm| matches!(arm.case, MirSwitchCase::Variant(_)))
            .cloned()
            .collect::<Vec<_>>();
        let default_arm = arms
            .iter()
            .find(|arm| matches!(arm.case, MirSwitchCase::Default))
            .cloned();

        if variant_arms.is_empty() {
            let has_literal_arms = arms
                .iter()
                .any(|arm| matches!(arm.case, MirSwitchCase::Literal(_)));
            if has_literal_arms {
                // Scalar literal switch (R6-1051): the shared catalog contract
                // proved the Copy-scalar scrutinee and exhaustive coverage in
                // validate_mir_native; this is the mechanical lowering.
                return self.emit_literal_switch(
                    &scrutinee_value,
                    &scrutinee_ty,
                    arms,
                    default_arm.as_ref(),
                    subject,
                );
            }
            let default_arm = default_arm.ok_or_else(|| {
                NativeMirError::new(subject.to_string(), "variant switch has no native arm")
            })?;
            let current = self.generator.builder.get_insert_block().ok_or_else(|| {
                NativeMirError::new(subject.to_string(), "switch has no LLVM insertion block")
            })?;
            self.queue_edge(
                &default_arm.target,
                &default_arm.arguments,
                current,
                subject,
            )?;
            let default_block = *self.blocks.get(&default_arm.target).ok_or_else(|| {
                NativeMirError::new(subject.to_string(), "default target LLVM block is absent")
            })?;
            self.generator
                .builder
                .build_unconditional_branch(default_block)
                .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
            return Ok(());
        }

        let allow_generic_result = self.program.instances().values().any(|instance| {
            matches!(
                &instance.contract,
                crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection {
                    contract
                } if contract.source_ty == scrutinee_ty
                    && contract.projection.nominal.as_str() == "builtin:type:Result"
                    && contract.projection.ownership == MirOwnership::Copy
            )
        });
        let (variant_abi, _) = native_variant_abi_with_generic_result(
            self.program.type_catalog(),
            &scrutinee_ty,
            false,
            allow_generic_result,
        )?;

        let tag = self
            .generator
            .builder
            .build_extract_value(
                scrutinee_value.into_struct_value(),
                variant_abi.tag_field,
                "mir_variant_tag_load",
            )
            .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?
            .into_int_value();
        for (index, arm) in variant_arms.iter().enumerate() {
            let MirSwitchCase::Variant(variant_id) = &arm.case else {
                return Err(NativeMirError::new(
                    subject.to_string(),
                    "native variant switch arm lost its variant case",
                ));
            };
            // The promoted multi-target union contract resolves its cases by
            // layout identity; payload-shape proof already happened at the
            // TypeDesc instruction gate. Flat Copy variants keep their own
            // per-case resolver.
            let union_tagged =
                self.program
                    .type_catalog()
                    .get(&scrutinee_ty)
                    .is_some_and(|descriptor| {
                        matches!(descriptor.layout, MirLayout::Enum { .. })
                            && descriptor.kind == MirTypeKind::FlowStateSet
                    });
            let variant = if union_tagged {
                self.program
                    .type_catalog()
                    .validated_variant_switch_case(&scrutinee_ty, variant_id)
                    .map_err(|message| NativeMirError::new(subject.to_string(), message))?
                    .1
                    .clone()
            } else {
                self.program
                    .type_catalog()
                    .validated_flat_copy_variant(&scrutinee_ty, variant_id)
                    .map_err(|message| NativeMirError::new(subject.to_string(), message))?
                    .clone()
            };
            let condition = self
                .generator
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    tag,
                    self.generator
                        .context
                        .i8_type()
                        .const_int(u64::from(variant.discriminant), false),
                    "mir_variant_case",
                )
                .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
            let current = self.generator.builder.get_insert_block().ok_or_else(|| {
                NativeMirError::new(subject.to_string(), "switch case has no LLVM block")
            })?;
            self.queue_variant_edge(
                &arm.target,
                &arm.arguments,
                &arm.bindings,
                &scrutinee_ty,
                &variant,
                &variant_abi,
                true,
                scrutinee_value,
                current,
                subject,
            )?;
            let target = *self.blocks.get(&arm.target).ok_or_else(|| {
                NativeMirError::new(subject.to_string(), "variant target LLVM block is absent")
            })?;
            if index + 1 < variant_arms.len() {
                let next = self
                    .generator
                    .context
                    .append_basic_block(self.llvm_function, "mir_variant_next");
                self.generator
                    .builder
                    .build_conditional_branch(condition, target, next)
                    .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
                self.generator.builder.position_at_end(next);
            } else if let Some(default_arm) = &default_arm {
                self.queue_edge(
                    &default_arm.target,
                    &default_arm.arguments,
                    current,
                    subject,
                )?;
                let default_block = *self.blocks.get(&default_arm.target).ok_or_else(|| {
                    NativeMirError::new(subject.to_string(), "default target LLVM block is absent")
                })?;
                self.generator
                    .builder
                    .build_conditional_branch(condition, target, default_block)
                    .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
            } else {
                let unreachable = self
                    .generator
                    .context
                    .append_basic_block(self.llvm_function, "mir_variant_unreachable");
                self.generator
                    .builder
                    .build_conditional_branch(condition, target, unreachable)
                    .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
                self.generator.builder.position_at_end(unreachable);
                self.generator
                    .builder
                    .build_unreachable()
                    .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
            }
        }
        Ok(())
    }

    /// Lower a scalar literal switch (R6-1051). The structural validator
    /// proved the scrutinee is a Copy scalar with admitted Bool/signed-integer
    /// ABI, every case is a Literal or trailing Default, and coverage is
    /// exhaustive — so the false edge of the final literal either lands on the
    /// default arm or on a defensively unreachable abort.
    fn emit_literal_switch(
        &mut self,
        scrutinee_value: &BasicValueEnum<'ctx>,
        scrutinee_ty: &crate::core::ResolvedTypeId,
        arms: &[MirSwitchArm],
        default_arm: Option<&MirSwitchArm>,
        subject: &MirBlockId,
    ) -> Result<(), NativeMirError> {
        let descriptor = self
            .program
            .type_catalog()
            .get(scrutinee_ty)
            .ok_or_else(|| {
                NativeMirError::new(subject.to_string(), "scalar switch TypeDesc is absent")
            })?;
        let literal_arms: Vec<&MirSwitchArm> = arms
            .iter()
            .filter(|arm| matches!(arm.case, MirSwitchCase::Literal(_)))
            .collect();
        for (index, arm) in literal_arms.iter().enumerate() {
            let MirSwitchCase::Literal(literal) = &arm.case else {
                return Err(NativeMirError::new(
                    subject.to_string(),
                    "native scalar switch arm lost its literal case",
                ));
            };
            let constant = match (descriptor.abi, literal) {
                (MirAbiClass::Bool, ResolvedLiteral::Bool(value)) => self
                    .generator
                    .context
                    .bool_type()
                    .const_int(u64::from(*value), false),
                (
                    MirAbiClass::Integer {
                        bits: 32,
                        signed: true,
                    },
                    ResolvedLiteral::Int(value),
                ) => self
                    .generator
                    .context
                    .i32_type()
                    .const_int(*value as u64, true),
                (
                    MirAbiClass::Integer {
                        bits: 64,
                        signed: true,
                    },
                    ResolvedLiteral::Int(value),
                ) => self
                    .generator
                    .context
                    .i64_type()
                    .const_int(*value as u64, true),
                _ => {
                    return Err(NativeMirError::new(
                        subject.to_string(),
                        "scalar switch case is outside the native scalar ABI",
                    ));
                }
            };
            let condition = self
                .generator
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    scrutinee_value.into_int_value(),
                    constant,
                    "mir_scalar_case",
                )
                .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
            let target = *self.blocks.get(&arm.target).ok_or_else(|| {
                NativeMirError::new(subject.to_string(), "scalar switch target is absent")
            })?;
            let current = self.generator.builder.get_insert_block().ok_or_else(|| {
                NativeMirError::new(subject.to_string(), "scalar switch case has no LLVM block")
            })?;
            self.queue_edge(&arm.target, &arm.arguments, current, subject)?;
            if index + 1 < literal_arms.len() {
                let next = self
                    .generator
                    .context
                    .append_basic_block(self.llvm_function, "mir_scalar_next");
                self.generator
                    .builder
                    .build_conditional_branch(condition, target, next)
                    .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
                self.generator.builder.position_at_end(next);
            } else if let Some(default_arm) = default_arm {
                self.queue_edge(
                    &default_arm.target,
                    &default_arm.arguments,
                    current,
                    subject,
                )?;
                let default_block = *self.blocks.get(&default_arm.target).ok_or_else(|| {
                    NativeMirError::new(subject.to_string(), "default target LLVM block is absent")
                })?;
                self.generator
                    .builder
                    .build_conditional_branch(condition, target, default_block)
                    .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
            } else {
                let no_match = self
                    .generator
                    .context
                    .append_basic_block(self.llvm_function, "mir_scalar_no_match");
                self.generator
                    .builder
                    .build_conditional_branch(condition, target, no_match)
                    .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
                self.generator.builder.position_at_end(no_match);
                self.emit_abort_with_message(
                    "[E0800] canonical MIR scalar switch has no matching arm",
                    &subject.to_string(),
                )?;
            }
        }
        Ok(())
    }

    pub(super) fn queue_edge(
        &mut self,
        target: &MirBlockId,
        arguments: &[MirValueId],
        predecessor: BasicBlock<'ctx>,
        subject: &MirBlockId,
    ) -> Result<(), NativeMirError> {
        let block = self
            .function
            .blocks
            .get(target)
            .ok_or_else(|| NativeMirError::new(subject.to_string(), "edge target is absent"))?;
        if block.parameters.len() != arguments.len() {
            return Err(NativeMirError::new(
                subject.to_string(),
                "edge arguments do not match target block parameter arity",
            ));
        }
        for (parameter, argument) in block.parameters.iter().zip(arguments) {
            if self.is_unit_value(&parameter.value) {
                continue;
            }
            self.pending_incoming.push((
                parameter.value.clone(),
                NativePhiSource::Mir(argument.clone()),
                predecessor,
            ));
        }
        Ok(())
    }

    fn validate_nested_direct_print_result_switch_move(
        &self,
        scrutinee: &MirValueId,
        arms: &[MirSwitchArm],
        scrutinee_ty: &crate::core::ResolvedTypeId,
        subject: &MirBlockId,
    ) -> Result<(), NativeMirError> {
        let receipt = self
            .function
            .ownership
            .ordinary_map_owned_any_v2_direct_print_result_receipt()
            .ok_or_else(|| {
                NativeMirError::new(
                    subject.to_string(),
                    "nested Result DirectPrint receipt disappeared during SwitchMove",
                )
            })?;
        let (
            operation,
            expected_dispatch,
            ok_edge,
            ok_target,
            ok_local,
            err_edge,
            err_target,
            err_local,
        ) = if subject.as_str() == receipt.new_dispatch.0 .0 {
            (
                &receipt.new_operation,
                &receipt.new_dispatch,
                &receipt.new_ok_edge,
                &receipt.new_ok_target,
                &receipt.initial_root_local,
                &receipt.new_error_edge,
                &receipt.new_error_target,
                &receipt.new_error_local,
            )
        } else if subject.as_str() == receipt.set_dispatch.0 .0 {
            (
                &receipt.set_operation,
                &receipt.set_dispatch,
                &receipt.set_ok_edge,
                &receipt.set_ok_target,
                &receipt.updated_root_local,
                &receipt.set_error_edge,
                &receipt.set_error_target,
                &receipt.set_error_local,
            )
        } else {
            return Err(NativeMirError::new(
                subject.to_string(),
                "nested Result DirectPrint SwitchMove is outside its two receipted Result dispatches",
            ));
        };
        let expected_scrutinee = MirValueId::new(format!("expr:{}", operation.0))
            .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
        let expected_root = crate::core::mir::types::ordinary_map_owned_any_v2_type_id();
        let expected_result_type =
            crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id();
        let i32_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        let nominal = crate::core::NominalTypeId::new("builtin:type:Result")
            .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
        let matches_arm = |arm: &MirSwitchArm,
                           variant_name: &str,
                           edge: &crate::core::cfg::EdgeId,
                           target: &crate::core::cfg::BasicBlockId,
                           local: &crate::core::ir::ResolvedLocalId,
                           field_ty: &crate::core::ResolvedTypeId,
                           ownership: crate::core::mir::types::MirOwnership,
                           move_out_glue: crate::core::mir::types::MirGlueKind|
         -> bool {
            let variant = crate::core::NodeId(format!("builtin:variant:Result::{variant_name}"));
            let field =
                crate::core::NodeId(format!("builtin:variant:Result::{variant_name}/payload:0"));
            let expected_parameter = MirValueId::new(format!("local:{}", local.0 .0)).ok();
            arm.edge.as_str() == edge.0 .0
                && arm.target.as_str() == target.0 .0
                && arm.arguments.is_empty()
                && arm.case == MirSwitchCase::Variant(variant.clone())
                && matches!(arm.bindings.as_slice(), [binding]
                    if Some(&binding.parameter) == expected_parameter.as_ref()
                        && binding.nested_tuple.is_none()
                        && binding.projection.nominal == nominal
                        && binding.projection.variant == variant
                        && binding.projection.field == field
                        && binding.projection.field_index == 0
                        && binding.projection.arity == 1
                        && binding.projection.field_ty == *field_ty
                        && binding.projection.ownership == ownership
                        && binding.projection.move_out_glue == move_out_glue)
        };
        let ok_matches = arms.iter().filter(|arm| {
            matches_arm(
                arm,
                "Ok",
                ok_edge,
                ok_target,
                ok_local,
                &expected_root,
                crate::core::mir::types::MirOwnership::Move,
                crate::core::mir::types::MirGlueKind::OrdinaryMapOwnedAnyV2,
            )
        });
        let err_matches = arms.iter().filter(|arm| {
            matches_arm(
                arm,
                "Err",
                err_edge,
                err_target,
                err_local,
                &i32_ty,
                crate::core::mir::types::MirOwnership::Copy,
                crate::core::mir::types::MirGlueKind::Noop,
            )
        });
        if self.function.owner != receipt.owner
            || expected_dispatch.0 .0 != subject.as_str()
            || scrutinee != &expected_scrutinee
            || scrutinee_ty != &expected_result_type
            || arms.len() != 2
            || ok_matches.count() != 1
            || err_matches.count() != 1
        {
            return Err(NativeMirError::new(
                subject.to_string(),
                "nested Result DirectPrint switch scrutinee, Result arms, payload locals, or projection contracts differ from the dedicated Checker receipt",
            ));
        }
        Ok(())
    }

    pub(super) fn queue_variant_edge(
        &mut self,
        target: &MirBlockId,
        arguments: &[MirValueId],
        bindings: &[crate::core::mir::MirSwitchBinding],
        scrutinee_ty: &crate::core::ResolvedTypeId,
        variant: &crate::core::mir::types::MirVariantDesc,
        variant_abi: &NativeVariantAbi,
        flat_copy: bool,
        scrutinee: BasicValueEnum<'ctx>,
        predecessor: BasicBlock<'ctx>,
        subject: &MirBlockId,
    ) -> Result<(), NativeMirError> {
        let block = self
            .function
            .blocks
            .get(target)
            .ok_or_else(|| NativeMirError::new(subject.to_string(), "edge target is absent"))?;
        let binding_start = arguments.len().checked_add(bindings.len()).ok_or_else(|| {
            NativeMirError::new(
                subject.to_string(),
                "variant edge parameter arity overflows",
            )
        })?;
        if block.parameters.len() != binding_start {
            return Err(NativeMirError::new(
                subject.to_string(),
                "variant edge does not match target block parameter arity",
            ));
        }
        for (parameter, argument) in block.parameters.iter().zip(arguments) {
            if self.is_unit_value(&parameter.value) {
                continue;
            }
            self.pending_incoming.push((
                parameter.value.clone(),
                NativePhiSource::Mir(argument.clone()),
                predecessor,
            ));
        }
        if bindings
            .iter()
            .any(|binding| binding.nested_tuple.is_some())
        {
            if flat_copy {
                return Err(NativeMirError::new(
                    subject.to_string(),
                    "nested tuple payload binding requires consuming SwitchMove",
                ));
            }
            let payload_slot = variant_abi.payload_slot(&variant.id).ok_or_else(|| {
                NativeMirError::new(
                    subject.to_string(),
                    "nested tuple binding has no native ABI payload slot",
                )
            })?;
            let payload = self
                .generator
                .builder
                .build_extract_value(
                    scrutinee.into_struct_value(),
                    payload_slot.physical_field,
                    "mir_nested_variant_payload_load",
                )
                .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
            for (index, binding) in bindings.iter().enumerate() {
                let Some(nested) = &binding.nested_tuple else {
                    return Err(NativeMirError::new(
                        subject.to_string(),
                        "nested tuple binding group mixes direct and nested projections",
                    ));
                };
                let parameter = block
                    .parameters
                    .get(arguments.len().checked_add(index).ok_or_else(|| {
                        NativeMirError::new(
                            subject.to_string(),
                            "nested tuple binding parameter index overflows",
                        )
                    })?)
                    .and_then(|parameter| self.function.values.get(&parameter.value))
                    .ok_or_else(|| {
                        NativeMirError::new(
                            subject.to_string(),
                            "nested tuple binding target type is absent",
                        )
                    })?;
                self.program
                    .type_catalog()
                    .validate_variant_nested_tuple_payload_projection_receipt(
                        scrutinee_ty,
                        &variant.id,
                        &parameter.ty,
                        &binding.projection,
                        nested,
                    )
                    .map_err(|message| NativeMirError::new(subject.to_string(), message))?;
                if binding.parameter != parameter.id {
                    return Err(NativeMirError::new(
                        subject.to_string(),
                        "nested tuple binding parameter disagrees with target block parameter",
                    ));
                }
                let field_index = self.u32_abi(
                    nested.field_index,
                    "nested tuple payload field index",
                    &subject.to_string(),
                )?;
                let element = self
                    .generator
                    .builder
                    .build_extract_value(
                        payload.into_struct_value(),
                        field_index,
                        "mir_nested_tuple_payload_load",
                    )
                    .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
                self.pending_incoming.push((
                    parameter.id.clone(),
                    NativePhiSource::Value(element),
                    predecessor,
                ));
            }
            return Ok(());
        }
        // Each binding transports its own payload field; the promoted
        // multi-target union contract admits multi-field variants, so the
        // edge resolves one physical slot per binding instead of assuming
        // the single historical payload slot.  A union scrutinee proves its
        // bindings through the promoted payload receipts even on a read-only
        // Switch, where flat Copy variants keep the historical receipt.
        let union_tagged =
            self.program
                .type_catalog()
                .get(scrutinee_ty)
                .is_some_and(|descriptor| {
                    matches!(descriptor.layout, MirLayout::Enum { .. })
                        && descriptor.kind == MirTypeKind::FlowStateSet
                });
        for (index, binding) in bindings.iter().enumerate() {
            let parameter_index = arguments.len().checked_add(index).ok_or_else(|| {
                NativeMirError::new(
                    subject.to_string(),
                    "variant payload binding parameter index overflows",
                )
            })?;
            let block_parameter = block.parameters.get(parameter_index).ok_or_else(|| {
                NativeMirError::new(
                    subject.to_string(),
                    "variant payload binding target parameter is absent",
                )
            })?;
            let parameter = self
                .function
                .values
                .get(&block_parameter.value)
                .ok_or_else(|| {
                    NativeMirError::new(
                        subject.to_string(),
                        "variant payload binding target type is absent",
                    )
                })?;
            if flat_copy && !union_tagged {
                self.program
                    .type_catalog()
                    .validate_flat_copy_payload_projection_receipt(
                        scrutinee_ty,
                        &variant.id,
                        &parameter.ty,
                        &binding.projection,
                    )
                    .map_err(|message| NativeMirError::new(subject.to_string(), message))?;
            } else {
                self.program
                    .type_catalog()
                    .validate_variant_payload_projection_receipt(
                        scrutinee_ty,
                        &variant.id,
                        &parameter.ty,
                        &binding.projection,
                    )
                    .map_err(|message| NativeMirError::new(subject.to_string(), message))?;
            }
            if binding.parameter != block_parameter.value {
                return Err(NativeMirError::new(
                    subject.to_string(),
                    "variant payload binding parameter disagrees with target block parameter",
                ));
            }
            let payload_slot = variant_abi
                .payload_slots(&variant.id)
                .find(|slot| slot.field == binding.projection.field)
                .ok_or_else(|| {
                    NativeMirError::new(
                        subject.to_string(),
                        "variant payload binding has no native ABI payload slot",
                    )
                })?;
            let payload = self
                .generator
                .builder
                .build_extract_value(
                    scrutinee.into_struct_value(),
                    payload_slot.physical_field,
                    "mir_variant_payload_load",
                )
                .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
            self.pending_incoming.push((
                block_parameter.value.clone(),
                NativePhiSource::Value(payload),
                predecessor,
            ));
        }
        Ok(())
    }

    fn is_unit_value(&self, value: &MirValueId) -> bool {
        self.function
            .values
            .get(value)
            .and_then(|mir_value| self.program.type_catalog().get(&mir_value.ty))
            .is_some_and(MirTypeDesc::is_canonical_ffi_unit)
    }

    pub(super) fn add_phi_incomings(&mut self) -> Result<(), NativeMirError> {
        for (parameter, source, predecessor) in &self.pending_incoming {
            let value = match source {
                NativePhiSource::Mir(source) => *self.values.get(source).ok_or_else(|| {
                    NativeMirError::new(source.to_string(), "phi incoming value was not emitted")
                })?,
                NativePhiSource::Value(value) => *value,
            };
            let phi = self.phis.get(parameter).ok_or_else(|| {
                NativeMirError::new(parameter.to_string(), "phi parameter is absent")
            })?;
            phi.add_incoming(&[(&value as &dyn BasicValue, *predecessor)]);
        }
        Ok(())
    }

    /// Trap-only cleanup is derived from the MIR prefix and never updates the
    /// emitter's normal-path values or the checked ownership ledger.
    pub(super) fn emit_ordinary_map_trap_cleanup(
        &mut self,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let has_computed_string_key_owner_chain_result_branch = self
            .computed_string_key_owner_chain_l1_authorized
            && self
                .function
                .ownership
                .ordinary_map_owned_any_v2_cfg_receipt()
                .is_some_and(|cfg| {
                    use crate::core::ownership::{
                        OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainProfileVersion as ChainVersion,
                        OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainResultBranchProfileVersion as BranchVersion,
                    };

                    if cfg.owner != self.function.owner {
                        return false;
                    }
                    let branches = cfg
                        .result_receipts
                        .iter()
                        .filter_map(|receipt| {
                            receipt
                                .computed_string_key_owner_chain
                                .as_deref()
                                .and_then(|chain| {
                                    chain
                                        .result_branch
                                        .as_deref()
                                        .map(|branch| (chain, branch))
                                })
                        })
                        .collect::<Vec<_>>();
                    let [(chain, branch)] = branches.as_slice() else {
                        return false;
                    };
                    let Ok(new_result) = MirValueId::new(format!(
                        "expr:{}",
                        branch.map_new_operation.0
                    )) else {
                        return false;
                    };
                    let Ok(set_result) = MirValueId::new(format!(
                        "expr:{}",
                        branch.map_set_operation.0
                    )) else {
                        return false;
                    };
                    let expected_result_ty =
                        crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id();
                    let new_result_is_exact = self
                        .function
                        .values
                        .get(&new_result)
                        .is_some_and(|value| value.ty == expected_result_ty);
                    let set_result_is_exact = self
                        .function
                        .values
                        .get(&set_result)
                        .is_some_and(|value| value.ty == expected_result_ty);
                    chain.owner == self.function.owner
                        && branch.owner == self.function.owner
                        && chain.profile_version == ChainVersion::V1
                        && branch.profile_version == BranchVersion::V1
                        && branch.map_new_operation == chain.map_new.point
                        && branch.map_set_operation == chain.map_set.point
                        && branch.initial_root == chain.map_new.local
                        && branch.populated_root == chain.map_set.local
                        && chain.map_set.source_local.as_ref() == Some(&branch.initial_root)
                        && new_result_is_exact
                        && set_result_is_exact
                });
        let has_values_local_index_i32_result_branch = self
            .values_local_index_i32_result_branch_l1_authorized
            && self
                .function
                .ownership
                .ordinary_map_owned_any_v2_cfg_receipt()
                .and_then(|cfg| cfg.values_local_index_i32_result_branch_receipt.as_ref())
                .is_some();
        let has_values_result_crosscall_string_caller = self.values_result_authorization.is_some()
            && self
                .function
                .ownership
                .ordinary_map_owned_any_v2_values_result_producer_receipt()
                .is_some();
        let has_map_set_drop_control_result_branch = self.map_set_drop_control_result_l1_authorized
            && self
                .function
                .ownership
                .ordinary_map_owned_any_v2_cfg_receipt()
                .and_then(|cfg| cfg.map_set_drop_control_result_branch_receipt.as_ref())
                .is_some();
        if !self
            .function
            .ownership
            .has_checker_ordinary_map_owner_lifecycle_v1_actions()
            && !self
                .function
                .ownership
                .has_checker_ordinary_map_owned_get_i32_string_v1_actions()
            && !self
                .function
                .ownership
                .has_checker_ordinary_map_owned_any_v2_actions()
            && !(self.ordinary_map_new_result_l1_authorized
                && self
                    .function
                    .ownership
                    .ordinary_map_new_result_receipt()
                    .is_some())
            && !(self.ordinary_map_set_result_l1_authorized
                && self
                    .function
                    .ownership
                    .ordinary_map_set_lineage_receipt()
                    .is_some())
            && !(self.ordinary_map_set_get_i32_l1_authorized
                && self
                    .function
                    .ownership
                    .ordinary_map_set_get_i32_lineage_receipt()
                    .is_some())
            && !(self.ordinary_map_set_get_any_i32_l1_authorized
                && self
                    .function
                    .ownership
                    .ordinary_map_set_get_any_i32_lineage_receipt()
                    .is_some())
            && !(self.ordinary_map_set_get_any_result_i32_l1_authorized
                && self
                    .function
                    .ownership
                    .ordinary_map_owned_any_v2_set_get_any_result_i32_lineage_receipt()
                    .is_some())
            && !has_values_local_index_i32_result_branch
            && !has_values_result_crosscall_string_caller
            && !has_map_set_drop_control_result_branch
            && !has_computed_string_key_owner_chain_result_branch
        {
            return Ok(());
        }
        // Map/Any ownership is now admitted alongside ordinary scalar branch
        // tails.  Trap sites can therefore live in a multi-block MIR graph;
        // cleanup must use the current block's SSA prefix rather than reject
        // the whole native route.  Values that cross a CFG edge are explicit
        // block parameters, so seeding from those parameters plus the local
        // prefix remains path-local and cannot invent a predecessor owner.
        let block = self
            .function
            .blocks
            .values()
            .find(|candidate| {
                candidate.id.to_string() == subject
                    || candidate
                        .instructions
                        .iter()
                        .any(|instruction| instruction.id.as_str() == subject)
            })
            .ok_or_else(|| {
                NativeMirError::new(subject, "ordinary Map trap cleanup block is absent")
            })?;
        let mut live_roots = Vec::new();
        let mut subject_found = subject == block.id.to_string();
        for parameter in &block.parameters {
            if self
                .function
                .values
                .get(&parameter.value)
                .is_some_and(|value| {
                    value.ty
                    == crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id()
                    || value.ty
                        == crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id()
                    || value.ty
                        == crate::core::mir::types::ordinary_map_owned_any_v2_type_id()
                })
            {
                live_roots.push(parameter.value.clone());
            }
        }
        for instruction in &block.instructions {
            if instruction.id.as_str() == subject {
                subject_found = true;
                break;
            }
            match &instruction.kind {
                MirInstructionKind::OrdinaryMapOwnerLifecycleNew { result }
                | MirInstructionKind::OrdinaryMapOwnerLifecycleSet { result, .. }
                | MirInstructionKind::OrdinaryMapOwnerLifecycleRemove { result, .. }
                | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1New { result }
                | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Set { result, .. }
                | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Remove { result, .. }
                | MirInstructionKind::OrdinaryMapOwnedAnyV2Remove { result, .. } => {
                    live_roots.push(result.clone());
                }
                MirInstructionKind::OrdinaryMapOwnedAnyV2Set { result, .. }
                    if crate::core::mir::ordinary_map_owned_any_v2_result_branch_v1_for_op(
                        self.function,
                        crate::core::OrdinaryMapOwnedAnyV2ActionKind::Set,
                        result,
                    )
                    .is_none() =>
                {
                    live_roots.push(result.clone());
                }
                MirInstructionKind::OrdinaryMapOwnedAnyV2New { result }
                    if !((self.ordinary_map_new_result_l1_authorized
                        && self
                            .function
                            .ownership
                            .ordinary_map_new_result_receipt()
                            .is_some())
                        || (self.ordinary_map_set_result_l1_authorized
                            && self
                                .function
                                .ownership
                                .ordinary_map_set_lineage_receipt()
                                .is_some())
                        || (self.ordinary_map_set_get_i32_l1_authorized
                            && self
                                .function
                                .ownership
                                .ordinary_map_set_get_i32_lineage_receipt()
                                .is_some())
                        || (self.ordinary_map_set_get_any_i32_l1_authorized
                            && self
                                .function
                                .ownership
                                .ordinary_map_set_get_any_i32_lineage_receipt()
                                .is_some())
                        || (self.ordinary_map_set_get_any_result_i32_l1_authorized
                            && self
                                .function
                                .ownership
                                .ordinary_map_owned_any_v2_set_get_any_result_i32_lineage_receipt()
                                .is_some())
                        || has_values_local_index_i32_result_branch
                        || has_values_result_crosscall_string_caller
                        || has_map_set_drop_control_result_branch
                        || has_computed_string_key_owner_chain_result_branch
                        || crate::core::mir::ordinary_map_owned_any_v2_result_branch_v1_for_op(
                            self.function,
                            crate::core::OrdinaryMapOwnedAnyV2ActionKind::New,
                            result,
                        )
                        .is_some()) =>
                {
                    live_roots.push(result.clone());
                }
                // Synthetic Result carriers are not owner-v2 Map roots. The
                // exact branch receipts keep the live source root in the
                // current block parameters, which this cleanup tracks and
                // drops on a trap.
                MirInstructionKind::OrdinaryMapOwnedAnyV2SetAnyResult { .. }
                | MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult { .. }
                | MirInstructionKind::OrdinaryMapOwnedAnyV2SetStringResult { .. } => {}
                MirInstructionKind::Clone { result, .. }
                    if self.function.values.get(result).is_some_and(|value| {
                        value.ty == crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id()
                            || value.ty == crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id()
                            || value.ty == crate::core::mir::types::ordinary_map_owned_any_v2_type_id()
                    }) =>
                {
                    live_roots.push(result.clone());
                }
                MirInstructionKind::OrdinaryMapOwnerLifecycleDrop { root }
                | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Drop { root }
                | MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { root } => {
                    live_roots.retain(|live| live != root);
                }
                _ => {}
            }
        }
        if !subject_found {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map trap has no MIR prefix anchor",
            ));
        }
        for root in live_roots.into_iter().rev() {
            let ty = self.value_type(&root, subject)?;
            let symbol = if ty
                == crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id()
            {
                "mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_drop"
            } else if ty == crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id()
            {
                "mimi_mir_ordinary_map_owned_get_i32_string_v1_drop"
            } else {
                "mimi_ordinary_map_owned_any_v2_drop"
            };
            let drop_fn = self
                .generator
                .get_runtime_fn(symbol)
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let handle = self.value(&root, subject)?.into_int_value();
            // Use the raw drop ABI so cleanup cannot recursively emit a guard.
            self.generator
                .builder
                .build_call(drop_fn, &[handle.into()], "mir_ordinary_map_trap_drop")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        }
        Ok(())
    }

    pub(super) fn emit_overflow_trap(
        &mut self,
        block: BasicBlock<'ctx>,
        operation: &str,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        self.generator.builder.position_at_end(block);
        if super::actor_status::is_actor_tagged(self.program, &self.function.owner) {
            return self.emit_actor_trap_status(802, subject);
        }
        self.emit_ordinary_map_trap_cleanup(subject)?;
        let function = self
            .generator
            .get_runtime_fn("mimi_trap_overflow")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let message = self
            .generator
            .builder
            .build_global_string_ptr(operation, "mir_overflow_operation")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(
                function,
                &[BasicMetadataValueEnum::from(message.as_pointer_value())],
                "mir_overflow_trap",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_unreachable()
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        Ok(())
    }

    pub(super) fn emit_abort_with_message(
        &mut self,
        message: &str,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        if super::actor_status::is_actor_tagged(self.program, &self.function.owner) {
            let status = crate::core::mir::actor_trap_status(message)
                .map_err(|message| NativeMirError::new(subject, message))?;
            return self.emit_actor_trap_status(status, subject);
        }
        self.emit_ordinary_map_trap_cleanup(subject)?;
        let function = self.generator.get_or_declare_abort_fn();
        let message = self
            .generator
            .builder
            .build_global_string_ptr(message, "mir_trap_message")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(
                function,
                &[BasicMetadataValueEnum::from(message.as_pointer_value())],
                "mir_trap",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_unreachable()
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        Ok(())
    }

    /// Emit a terminal runtime abort without running ordinary Map cleanup.
    ///
    /// Receipt-proven failures such as a statically out-of-bounds Values
    /// index must report their canonical error before touching any Map owner
    /// state. The process terminates at the abort, so draining unrelated live
    /// roots here would both violate that ordering and risk masking the
    /// receipt-proven error with a cleanup failure.
    pub(super) fn emit_abort_without_cleanup_with_message(
        &mut self,
        message: &str,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        if super::actor_status::is_actor_tagged(self.program, &self.function.owner) {
            return Err(NativeMirError::new(
                subject,
                "cleanup-free runtime abort is unsupported by the Actor finite-turn status ABI",
            ));
        }
        let function = self.generator.get_or_declare_abort_fn();
        let message = self
            .generator
            .builder
            .build_global_string_ptr(message, "mir_trap_message")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(
                function,
                &[BasicMetadataValueEnum::from(message.as_pointer_value())],
                "mir_trap_without_cleanup",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_unreachable()
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        Ok(())
    }
}
