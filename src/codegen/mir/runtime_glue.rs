//! Runtime-backed List and aggregate glue used by the native MIR emitter.

use super::*;
use crate::core::mir::types::MirGlueOperation;

impl<'a, 'ctx> NativeMirFunctionEmitter<'a, 'ctx> {
    pub(super) fn emit_list_construct(
        &mut self,
        result: &MirValueId,
        elements: &[MirValueId],
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let result_ty = self.value_type(result, subject)?;
        let catalog = self.program.type_catalog();
        let result_desc = catalog
            .get(&result_ty)
            .ok_or_else(|| NativeMirError::new(subject, "List result TypeDesc is absent"))?;
        let MirLayout::List { element } = &result_desc.layout else {
            return Err(NativeMirError::new(
                subject,
                "List construction result has no canonical List layout",
            ));
        };
        catalog
            .validate_list_construct(
                &result_ty,
                &elements
                    .iter()
                    .map(|value| self.value_type(value, subject))
                    .collect::<Result<Vec<_>, _>>()?,
            )
            .map_err(|message| NativeMirError::new(subject, message))?;
        let kind = native_list_kind(catalog, &result_ty)?;
        let kind_value = self
            .generator
            .context
            .i8_type()
            .const_int(kind as u64, false);
        let new_fn = self
            .generator
            .get_runtime_fn("mimi_mir_list_new_scalar")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let list = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    new_fn,
                    &[BasicMetadataValueEnum::from(kind_value)],
                    "mir_list_new",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "List constructor returned void"))?
        .into_pointer_value();
        self.emit_list_null_abort(list, subject, "canonical MIR List allocation failed")?;

        let push_fn = self
            .generator
            .get_runtime_fn("mimi_mir_list_push_scalar")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let element_desc = catalog
            .get(element)
            .ok_or_else(|| NativeMirError::new(subject, "List element TypeDesc is absent"))?;
        for value in elements {
            let scalar = self.emit_list_scalar_as_i64(value, element_desc, subject)?;
            let status = call_try_basic_value(
                &self
                    .generator
                    .builder
                    .build_call(
                        push_fn,
                        &[
                            BasicMetadataValueEnum::from(list),
                            BasicMetadataValueEnum::from(kind_value),
                            BasicMetadataValueEnum::from(scalar),
                        ],
                        "mir_list_push",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
            )
            .ok_or_else(|| NativeMirError::new(subject, "List append returned void"))?
            .into_int_value();
            let failed = self
                .generator
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    status,
                    self.generator.context.i8_type().const_zero(),
                    "mir_list_push_failed",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let fail = self
                .generator
                .context
                .append_basic_block(self.llvm_function, "mir_list_push_abort");
            let ok = self
                .generator
                .context
                .append_basic_block(self.llvm_function, "mir_list_push_ok");
            self.generator
                .builder
                .build_conditional_branch(failed, fail, ok)
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            self.generator.builder.position_at_end(fail);
            self.emit_abort_with_message("[E0800] canonical MIR List append failed", subject)?;
            self.generator.builder.position_at_end(ok);
        }
        Ok(list.into())
    }

    pub(super) fn emit_list_clone(
        &mut self,
        source: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let source_ty = self.value_type(source, subject)?;
        let kind = native_list_kind(self.program.type_catalog(), &source_ty)?;
        let source_value = self.value(source, subject)?.into_pointer_value();
        let kind_value = self
            .generator
            .context
            .i8_type()
            .const_int(kind as u64, false);
        let clone_fn = self
            .generator
            .get_runtime_fn("mimi_mir_list_clone_scalar")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let clone = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    clone_fn,
                    &[
                        BasicMetadataValueEnum::from(source_value),
                        BasicMetadataValueEnum::from(kind_value),
                    ],
                    "mir_list_clone",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "List clone returned void"))?
        .into_pointer_value();
        self.emit_list_null_abort(clone, subject, "canonical MIR List clone failed")?;
        Ok(clone.into())
    }

    pub(super) fn emit_set_construct(
        &mut self,
        result: &MirValueId,
        elements: &[MirValueId],
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let result_ty = self.value_type(result, subject)?;
        let element_types = elements
            .iter()
            .map(|value| self.value_type(value, subject))
            .collect::<Result<Vec<_>, _>>()?;
        self.program
            .type_catalog()
            .validate_set_construct(&result_ty, &element_types)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let element = match self
            .program
            .type_catalog()
            .get(&result_ty)
            .map(|descriptor| &descriptor.layout)
        {
            Some(MirLayout::Set { element }) => element.clone(),
            _ => {
                return Err(NativeMirError::new(
                    subject,
                    "Set construction result has no canonical Set<T> layout",
                ))
            }
        };
        let new_fn = self
            .generator
            .get_runtime_fn("mimi_set_new")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let set = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(new_fn, &[], "mir_set_new")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "Set constructor returned void"))?
        .into_int_value();
        self.emit_set_handle_null_abort(set, subject, "canonical MIR Set allocation failed")?;

        let insert_fn = self
            .generator
            .get_runtime_fn("mimi_set_insert")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let element_desc = self
            .program
            .type_catalog()
            .get(&element)
            .ok_or_else(|| NativeMirError::new(subject, "Set element TypeDesc is absent"))?;
        for value in elements {
            let scalar = self.emit_set_scalar_as_i64(value, element_desc, subject)?;
            self.generator
                .builder
                .build_call(
                    insert_fn,
                    &[
                        BasicMetadataValueEnum::from(set),
                        BasicMetadataValueEnum::from(scalar),
                    ],
                    "mir_set_insert",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        }
        Ok(set.into())
    }

    pub(super) fn emit_set_clone(
        &mut self,
        source: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let source_ty = self.value_type(source, subject)?;
        self.program
            .type_catalog()
            .validate_set_glue(&source_ty, MirGlueOperation::Clone)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let clone_fn = self
            .generator
            .get_runtime_fn("mimi_set_clone_scalar")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let clone = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    clone_fn,
                    &[BasicMetadataValueEnum::from(
                        self.value(source, subject)?.into_int_value(),
                    )],
                    "mir_set_clone",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "Set clone returned void"))?
        .into_int_value();
        self.emit_set_handle_null_abort(clone, subject, "canonical MIR Set clone failed")?;
        Ok(clone.into())
    }

    pub(super) fn emit_set_drop_value(
        &mut self,
        value: BasicValueEnum<'ctx>,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let destroy_fn = self
            .generator
            .get_runtime_fn("mimi_set_destroy")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(
                destroy_fn,
                &[BasicMetadataValueEnum::from(value.into_int_value())],
                "mir_set_drop",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        Ok(())
    }

    pub(super) fn emit_drop_value(
        &mut self,
        value: BasicValueEnum<'ctx>,
        ty: &crate::core::ResolvedTypeId,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let (ownership, glue, layout, plan) = {
            let descriptor = self
                .program
                .type_catalog()
                .get(ty)
                .ok_or_else(|| NativeMirError::new(subject, "drop TypeDesc is absent"))?;
            (
                descriptor.ownership,
                descriptor.glue.drop,
                descriptor.layout.clone(),
                descriptor.drop_plan.clone(),
            )
        };
        if ownership == MirOwnership::Copy {
            return Ok(());
        }
        match glue {
            MirGlueKind::OwnedString => self.emit_owned_string_drop_value(value, subject),
            MirGlueKind::Set => self.emit_set_drop_value(value, subject),
            MirGlueKind::Aggregate => {
                if matches!(layout, MirLayout::Option { .. } | MirLayout::Result { .. }) {
                    return self.emit_drop_variant_value(value, ty, subject);
                }
                validate_native_product_type(self.program.type_catalog(), ty)
                    .map_err(|message| NativeMirError::new(subject, message))?;
                let plan = plan.ok_or_else(|| {
                    NativeMirError::new(
                        subject,
                        "aggregate drop TypeDesc has no canonical drop plan",
                    )
                })?;
                if !matches!(layout, MirLayout::Tuple(_) | MirLayout::Record { .. }) {
                    return Err(NativeMirError::new(
                        subject,
                        "aggregate drop layout is outside the native product ABI",
                    ));
                }
                let aggregate = value.into_struct_value();
                for field in plan.fields {
                    let child = self
                        .generator
                        .builder
                        .build_extract_value(
                            aggregate,
                            field.index as u32,
                            "mir_product_drop_field",
                        )
                        .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                    self.emit_drop_value(child, &field.ty, subject)?;
                }
                Ok(())
            }
            glue => Err(NativeMirError::new(
                subject,
                format!("drop glue {glue:?} is outside the recursive tuple ABI"),
            )),
        }
    }

    pub(super) fn emit_drop_variant_value(
        &mut self,
        value: BasicValueEnum<'ctx>,
        ty: &crate::core::ResolvedTypeId,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let (variant_abi, _) = native_variant_abi(self.program.type_catalog(), ty, true)?;
        let variants = self
            .program
            .type_catalog()
            .variant_layout(ty)
            .map(|(_, variants)| variants.to_vec())
            .ok_or_else(|| NativeMirError::new(subject, "variant TypeDesc layout is absent"))?;
        let aggregate = value.into_struct_value();
        let tag = self
            .generator
            .builder
            .build_extract_value(aggregate, variant_abi.tag_field, "mir_variant_drop_tag")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let done = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_variant_drop_done");
        let invalid = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_variant_drop_invalid");
        let mut check = self
            .generator
            .builder
            .get_insert_block()
            .ok_or_else(|| NativeMirError::new(subject, "variant drop has no LLVM block"))?;
        for (index, variant) in variants.iter().enumerate() {
            self.generator.builder.position_at_end(check);
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
                    "mir_variant_drop_case",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let active = self
                .generator
                .context
                .append_basic_block(self.llvm_function, "mir_variant_drop_active");
            let next = if index + 1 == variants.len() {
                invalid
            } else {
                self.generator
                    .context
                    .append_basic_block(self.llvm_function, "mir_variant_drop_next")
            };
            self.generator
                .builder
                .build_conditional_branch(condition, active, next)
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            self.generator.builder.position_at_end(active);
            if let Some(payload_slot) = variant_abi.payload_slot(&variant.id) {
                let physical_field = payload_slot.physical_field;
                let payload_ty = payload_slot.ty.clone();
                let payload = self
                    .generator
                    .builder
                    .build_extract_value(aggregate, physical_field, "mir_variant_drop_value")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                self.emit_drop_value(payload, &payload_ty, subject)?;
            }
            self.generator
                .builder
                .build_unconditional_branch(done)
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            check = next;
        }
        self.generator.builder.position_at_end(invalid);
        self.emit_abort_with_message("[E0800] canonical MIR variant tag is invalid", subject)?;
        self.generator.builder.position_at_end(done);
        Ok(())
    }

    pub(super) fn emit_owned_string_drop_value(
        &mut self,
        value: BasicValueEnum<'ctx>,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let string = value.into_struct_value();
        let data = self
            .generator
            .builder
            .build_extract_value(string, 0, "mir_string_drop_data")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_pointer_value();
        let free_fn = self
            .generator
            .get_runtime_fn("mimi_string_free")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(
                free_fn,
                &[BasicMetadataValueEnum::from(data)],
                "mir_string_drop",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        Ok(())
    }

    pub(super) fn emit_drop(
        &mut self,
        value: &MirValueId,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let ty = self.value_type(value, subject)?;
        let (ownership, glue) = {
            let desc = self
                .program
                .type_catalog()
                .get(&ty)
                .ok_or_else(|| NativeMirError::new(subject, "drop TypeDesc is absent"))?;
            (desc.ownership, desc.glue.drop)
        };
        if ownership == MirOwnership::Copy {
            return Ok(());
        }
        if glue == MirGlueKind::OwnedString {
            return self.emit_owned_string_drop_value(self.value(value, subject)?, subject);
        }
        if glue == MirGlueKind::Set {
            self.program
                .type_catalog()
                .validate_set_glue(&ty, MirGlueOperation::Drop)
                .map_err(|message| NativeMirError::new(subject, message))?;
            return self.emit_set_drop_value(self.value(value, subject)?, subject);
        }
        if glue == MirGlueKind::Aggregate {
            return self.emit_drop_value(self.value(value, subject)?, &ty, subject);
        }
        let kind = native_list_kind(self.program.type_catalog(), &ty)?;
        let function = self
            .generator
            .get_runtime_fn("mimi_mir_list_drop_scalar")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let kind_value = self
            .generator
            .context
            .i8_type()
            .const_int(kind as u64, false);
        self.generator
            .builder
            .build_call(
                function,
                &[
                    BasicMetadataValueEnum::from(self.value(value, subject)?.into_pointer_value()),
                    BasicMetadataValueEnum::from(kind_value),
                ],
                "mir_list_drop",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        Ok(())
    }

    pub(super) fn emit_list_null_abort(
        &mut self,
        value: inkwell::values::PointerValue<'ctx>,
        subject: &str,
        message: &str,
    ) -> Result<(), NativeMirError> {
        let is_null = self
            .generator
            .builder
            .build_is_null(value, "mir_list_is_null")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let fail = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_list_null_abort");
        let ok = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_list_nonnull");
        self.generator
            .builder
            .build_conditional_branch(is_null, fail, ok)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(fail);
        self.emit_abort_with_message(message, subject)?;
        self.generator.builder.position_at_end(ok);
        Ok(())
    }

    pub(super) fn emit_list_scalar_as_i64(
        &mut self,
        value: &MirValueId,
        element_desc: &MirTypeDesc,
        subject: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, NativeMirError> {
        let value = self.value(value, subject)?.into_int_value();
        match element_desc.abi {
            MirAbiClass::Integer {
                bits: 64,
                signed: true,
            } => Ok(value),
            MirAbiClass::Integer {
                bits: 32,
                signed: true,
            } => self
                .generator
                .builder
                .build_int_s_extend(value, self.generator.context.i64_type(), "mir_list_i32")
                .map_err(|error| NativeMirError::new(subject, error.to_string())),
            MirAbiClass::Bool => self
                .generator
                .builder
                .build_int_z_extend(value, self.generator.context.i64_type(), "mir_list_bool")
                .map_err(|error| NativeMirError::new(subject, error.to_string())),
            abi => Err(NativeMirError::new(
                subject,
                format!("List element ABI {abi:?} is not scalar native storage"),
            )),
        }
    }

    pub(super) fn emit_set_scalar_as_i64(
        &mut self,
        value: &MirValueId,
        element_desc: &MirTypeDesc,
        subject: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, NativeMirError> {
        self.emit_list_scalar_as_i64(value, element_desc, subject)
    }

    fn emit_set_handle_null_abort(
        &mut self,
        value: inkwell::values::IntValue<'ctx>,
        subject: &str,
        message: &str,
    ) -> Result<(), NativeMirError> {
        let is_null = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                value,
                self.generator.context.i64_type().const_zero(),
                "mir_set_is_null",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let fail = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_set_null_abort");
        let ok = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_set_nonnull");
        self.generator
            .builder
            .build_conditional_branch(is_null, fail, ok)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(fail);
        self.emit_abort_with_message(message, subject)?;
        self.generator.builder.position_at_end(ok);
        Ok(())
    }
}
