//! CFG, switch, edge, and trap lowering for native MIR.

use super::*;

impl<'a, 'ctx> NativeMirFunctionEmitter<'a, 'ctx> {
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
                self.generator
                    .builder
                    .build_unconditional_branch(*self.blocks.get(target).expect("validated target"))
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
                self.generator
                    .builder
                    .build_conditional_branch(
                        condition,
                        *self.blocks.get(then_target).expect("validated target"),
                        *self.blocks.get(else_target).expect("validated target"),
                    )
                    .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
            }
            MirTerminator::Switch { scrutinee, arms } => {
                self.emit_switch(scrutinee, arms, subject)?;
            }
            MirTerminator::SwitchMove { scrutinee, arms } => {
                self.emit_switch_move(scrutinee, arms, subject)?;
            }
            MirTerminator::Return { value } => match value {
                Some(value) => {
                    let value = self.value(value, &subject.to_string())?;
                    self.generator
                        .builder
                        .build_return(Some(&value as &dyn BasicValue))
                        .map_err(|error| {
                            NativeMirError::new(subject.to_string(), error.to_string())
                        })?;
                }
                None => {
                    self.generator.builder.build_return(None).map_err(|error| {
                        NativeMirError::new(subject.to_string(), error.to_string())
                    })?;
                }
            },
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
                ))
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
        let (variant_abi, payload_ty) =
            native_variant_abi(self.program.type_catalog(), &scrutinee_ty, true)?;
        let (_, payload_variant, _) = self
            .program
            .type_catalog()
            .validated_option_string_variants(&scrutinee_ty)
            .map_err(|message| NativeMirError::new(subject.to_string(), message))?;
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
                    "native Option<string> SwitchMove requires explicit variant arms",
                ));
            };
            let variant = self
                .program
                .type_catalog()
                .validated_option_string_variant(&scrutinee_ty, variant_id)
                .map_err(|message| NativeMirError::new(subject.to_string(), message))?;
            let has_payload = variant.id == payload_variant.id;
            if has_payload
                && variant
                    .fields
                    .first()
                    .is_some_and(|field| field.ty != payload_ty)
            {
                return Err(NativeMirError::new(
                    subject.to_string(),
                    "switch-move payload field disagrees with the native Option<string> TypeDesc contract",
                ));
            }
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
                let payload = self
                    .generator
                    .builder
                    .build_extract_value(
                        scrutinee_value.into_struct_value(),
                        variant_abi.payload_field,
                        "mir_variant_move_drop_payload",
                    )
                    .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
                self.emit_owned_string_drop_value(payload, &subject.to_string())?;
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
                    variant,
                    variant_abi,
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
                    variant,
                    variant_abi,
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
            self.generator
                .builder
                .build_unconditional_branch(
                    *self
                        .blocks
                        .get(&default_arm.target)
                        .expect("validated default target"),
                )
                .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
            return Ok(());
        }

        let (variant_abi, _) =
            native_variant_abi(self.program.type_catalog(), &scrutinee_ty, false)?;

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
                unreachable!("variant arms were filtered above")
            };
            let variant = self
                .program
                .type_catalog()
                .validated_flat_copy_variant(&scrutinee_ty, variant_id)
                .map_err(|message| NativeMirError::new(subject.to_string(), message))?
                .clone();
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
                variant_abi,
                scrutinee_value,
                current,
                subject,
            )?;
            let target = *self
                .blocks
                .get(&arm.target)
                .expect("validated variant target");
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
                self.generator
                    .builder
                    .build_conditional_branch(
                        condition,
                        target,
                        *self
                            .blocks
                            .get(&default_arm.target)
                            .expect("validated default target"),
                    )
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
        for (parameter, argument) in block.parameters.iter().zip(arguments) {
            self.pending_incoming.push((
                parameter.value.clone(),
                NativePhiSource::Mir(argument.clone()),
                predecessor,
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
        variant_abi: NativeVariantAbi,
        scrutinee: BasicValueEnum<'ctx>,
        predecessor: BasicBlock<'ctx>,
        subject: &MirBlockId,
    ) -> Result<(), NativeMirError> {
        let block = self
            .function
            .blocks
            .get(target)
            .ok_or_else(|| NativeMirError::new(subject.to_string(), "edge target is absent"))?;
        if block.parameters.len() != arguments.len() + bindings.len() {
            return Err(NativeMirError::new(
                subject.to_string(),
                "variant edge does not match target block parameter arity",
            ));
        }
        for (parameter, argument) in block.parameters.iter().zip(arguments) {
            self.pending_incoming.push((
                parameter.value.clone(),
                NativePhiSource::Mir(argument.clone()),
                predecessor,
            ));
        }
        let payload = if bindings.is_empty() {
            None
        } else {
            let parameter = block
                .parameters
                .get(arguments.len())
                .and_then(|parameter| self.function.values.get(&parameter.value))
                .ok_or_else(|| {
                    NativeMirError::new(
                        subject.to_string(),
                        "variant payload binding target type is absent",
                    )
                })?;
            let field_index = self
                .program
                .type_catalog()
                .validate_variant_payload_projection(
                    scrutinee_ty,
                    &variant.id,
                    &bindings[0].field,
                    &parameter.ty,
                )
                .map_err(|message| NativeMirError::new(subject.to_string(), message))?;
            if bindings.len() != 1 || field_index != 0 || variant.fields.len() != 1 {
                return Err(NativeMirError::new(
                    subject.to_string(),
                    "variant payload binding is outside the single-payload native contract",
                ));
            }
            Some(
                self.generator
                    .builder
                    .build_extract_value(
                        scrutinee.into_struct_value(),
                        variant_abi.payload_field,
                        "mir_variant_payload_load",
                    )
                    .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?,
            )
        };
        if let Some(payload) = payload {
            for (index, binding) in bindings.iter().enumerate() {
                let parameter = &block.parameters[arguments.len() + index];
                if index != 0 || binding.parameter != parameter.value {
                    return Err(NativeMirError::new(
                        subject.to_string(),
                        "variant payload binding parameter disagrees with target block parameter",
                    ));
                }
                self.pending_incoming.push((
                    parameter.value.clone(),
                    NativePhiSource::Value(payload),
                    predecessor,
                ));
            }
        }
        Ok(())
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

    pub(super) fn emit_overflow_trap(
        &mut self,
        block: BasicBlock<'ctx>,
        operation: &str,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        self.generator.builder.position_at_end(block);
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
}
