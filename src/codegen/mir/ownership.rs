//! Explicit ownership, borrow, clone, and move glue for native MIR values.

use super::*;

impl<'a, 'ctx> NativeMirFunctionEmitter<'a, 'ctx> {
    pub(super) fn emit_borrow(
        &mut self,
        result: &MirValueId,
        source: &MirValueId,
        mutable: bool,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let source_ty = self.value_type(source, subject)?;
        let result_ty = self.value_type(result, subject)?;
        self.program
            .type_catalog()
            .validate_borrow(&source_ty, &result_ty, mutable)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let target_ty = self
            .program
            .type_catalog()
            .validate_reference_type(&result_ty)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let target_llvm = native_basic_type(
            self.generator.context,
            self.program.type_catalog(),
            &target_ty,
        )?;
        let slot = self
            .generator
            .builder
            .build_alloca(target_llvm, "mir_borrow_slot")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(slot, self.value(source, subject)?)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        Ok(slot.into())
    }

    pub(super) fn emit_end_borrow(
        &mut self,
        borrow: &MirValueId,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let ty = self.value_type(borrow, subject)?;
        self.program
            .type_catalog()
            .validate_reference_type(&ty)
            .map_err(|message| NativeMirError::new(subject, message))?;
        // The canonical reference storage is an entry-local alloca.  Its
        // lifetime is bounded by the native function, while the MIR
        // EndBorrow effect remains explicit and validated above.  We do not
        // emit a runtime call or infer ownership glue from the pointer.
        Ok(())
    }

    pub(super) fn emit_clone_value(
        &mut self,
        value: BasicValueEnum<'ctx>,
        ty: &crate::core::ResolvedTypeId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let (ownership, glue, layout) = {
            let descriptor = self
                .program
                .type_catalog()
                .get(ty)
                .ok_or_else(|| NativeMirError::new(subject, "clone TypeDesc is absent"))?;
            (
                descriptor.ownership,
                descriptor.glue.clone,
                descriptor.layout.clone(),
            )
        };
        if ownership == MirOwnership::Copy {
            return Ok(value);
        }
        match glue {
            MirGlueKind::OwnedString => self.emit_owned_string_clone_value(value, subject),
            MirGlueKind::Set => self.emit_set_clone_value(value, ty, subject),
            MirGlueKind::Aggregate => {
                if matches!(layout, MirLayout::Option { .. } | MirLayout::Result { .. }) {
                    return self.emit_clone_variant_value(value, ty, subject);
                }
                validate_native_product_type(self.program.type_catalog(), ty)
                    .map_err(|message| NativeMirError::new(subject, message))?;
                let elements = match layout {
                    MirLayout::Tuple(elements) => elements.clone(),
                    MirLayout::Record { fields, .. } => {
                        fields.into_iter().map(|field| field.ty).collect()
                    }
                    layout => {
                        return Err(NativeMirError::new(
                            subject,
                            format!(
                            "aggregate clone layout {layout:?} is outside the native product ABI"
                        ),
                        ))
                    }
                };
                let mut aggregate = value.into_struct_value();
                for (index, element_ty) in elements.iter().enumerate() {
                    let field = self
                        .generator
                        .builder
                        .build_extract_value(aggregate, index as u32, "mir_product_clone_field")
                        .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                    let cloned = self.emit_clone_value(field, element_ty, subject)?;
                    aggregate = self
                        .generator
                        .builder
                        .build_insert_value(
                            aggregate,
                            cloned,
                            index as u32,
                            "mir_product_clone_insert",
                        )
                        .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                        .into_struct_value();
                }
                Ok(aggregate.into())
            }
            glue => Err(NativeMirError::new(
                subject,
                format!("clone glue {glue:?} is outside the recursive tuple ABI"),
            )),
        }
    }

    fn emit_set_clone_value(
        &mut self,
        value: BasicValueEnum<'ctx>,
        ty: &crate::core::ResolvedTypeId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        self.program
            .type_catalog()
            .validate_set_glue(ty, crate::core::mir::types::MirGlueOperation::Clone)
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
                    &[BasicMetadataValueEnum::from(value.into_int_value())],
                    "mir_set_clone",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "Set clone returned void"))?
        .into_int_value();
        let is_null = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                clone,
                self.generator.context.i64_type().const_zero(),
                "mir_set_clone_failed",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let fail = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_set_clone_abort");
        let ok = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_set_clone_ok");
        self.generator
            .builder
            .build_conditional_branch(is_null, fail, ok)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(fail);
        self.emit_abort_with_message("[E0800] canonical MIR Set clone failed", subject)?;
        self.generator.builder.position_at_end(ok);
        Ok(clone.into())
    }

    pub(super) fn emit_clone_variant_value(
        &mut self,
        value: BasicValueEnum<'ctx>,
        ty: &crate::core::ResolvedTypeId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let payload_ty = native_non_copy_variant_payload_type(self.program.type_catalog(), ty)?;
        let (_, variants) = self
            .program
            .type_catalog()
            .variant_layout(ty)
            .ok_or_else(|| NativeMirError::new(subject, "variant clone has no TypeDesc layout"))?;
        let payload_variant = variants
            .iter()
            .find(|variant| variant.fields.len() == 1)
            .ok_or_else(|| {
                NativeMirError::new(subject, "variant clone has no owned payload variant")
            })?;
        let empty_variant = variants
            .iter()
            .find(|variant| variant.fields.is_empty())
            .ok_or_else(|| NativeMirError::new(subject, "variant clone has no empty variant"))?;
        if payload_ty != payload_variant.fields[0].ty {
            return Err(NativeMirError::new(
                subject,
                "variant clone payload disagrees with the canonical TypeDesc field",
            ));
        }
        let aggregate = value.into_struct_value();
        let tag = self
            .generator
            .builder
            .build_extract_value(aggregate, 0, "mir_variant_clone_tag")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let payload_tag = self
            .generator
            .context
            .i8_type()
            .const_int(u64::from(payload_variant.discriminant), false);
        let empty_tag = self
            .generator
            .context
            .i8_type()
            .const_int(u64::from(empty_variant.discriminant), false);
        let is_payload = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                tag,
                payload_tag,
                "mir_variant_clone_payload",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let clone_payload = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_variant_clone_payload");
        let check_empty = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_variant_clone_empty");
        let merge = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_variant_clone_merge");
        let invalid = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_variant_clone_invalid");
        self.generator
            .builder
            .build_conditional_branch(is_payload, clone_payload, check_empty)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        self.generator.builder.position_at_end(clone_payload);
        let payload = self
            .generator
            .builder
            .build_extract_value(aggregate, 1, "mir_variant_clone_value")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let cloned_payload = self.emit_owned_string_clone_value(payload, subject)?;
        let cloned_aggregate = self
            .generator
            .builder
            .build_insert_value(aggregate, cloned_payload, 1, "mir_variant_clone_insert")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_struct_value();
        let payload_block =
            self.generator.builder.get_insert_block().ok_or_else(|| {
                NativeMirError::new(subject, "variant clone payload has no block")
            })?;
        self.generator
            .builder
            .build_unconditional_branch(merge)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        self.generator.builder.position_at_end(check_empty);
        let is_empty = self
            .generator
            .builder
            .build_int_compare(IntPredicate::EQ, tag, empty_tag, "mir_variant_clone_empty")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let empty_block = self
            .generator
            .builder
            .get_insert_block()
            .ok_or_else(|| NativeMirError::new(subject, "variant clone empty has no block"))?;
        self.generator
            .builder
            .build_conditional_branch(is_empty, merge, invalid)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        self.generator.builder.position_at_end(invalid);
        self.emit_abort_with_message("[E0800] canonical MIR variant tag is invalid", subject)?;
        self.generator.builder.position_at_end(merge);
        let mut cloned = self
            .generator
            .builder
            .build_phi(aggregate.get_type(), "mir_variant_clone_result")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        cloned.add_incoming(&[
            (&cloned_aggregate, payload_block),
            (&aggregate, empty_block),
        ]);
        Ok(cloned.as_basic_value())
    }

    pub(super) fn emit_owned_string_clone_value(
        &mut self,
        value: BasicValueEnum<'ctx>,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let source = value.into_struct_value();
        let data = self
            .generator
            .builder
            .build_extract_value(source, 0, "mir_string_clone_data")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_pointer_value();
        let len = self
            .generator
            .builder
            .build_extract_value(source, 1, "mir_string_clone_len")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let clone_fn = self
            .generator
            .get_runtime_fn("mimi_str_clone")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let handle = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    clone_fn,
                    &[
                        BasicMetadataValueEnum::from(data),
                        BasicMetadataValueEnum::from(len),
                    ],
                    "mir_string_clone",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "String clone returned void"))?
        .into_int_value();
        self.emit_owned_string_from_handle(handle, len, subject)
    }

    pub(super) fn emit_owned_string_from_parts(
        &mut self,
        data: inkwell::values::PointerValue<'ctx>,
        len: inkwell::values::IntValue<'ctx>,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let clone_fn = self
            .generator
            .get_runtime_fn("mimi_str_clone")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let handle = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    clone_fn,
                    &[
                        BasicMetadataValueEnum::from(data),
                        BasicMetadataValueEnum::from(len),
                    ],
                    "mir_string_alloc",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "String allocation returned void"))?
        .into_int_value();
        self.emit_owned_string_from_handle(handle, len, subject)
    }

    pub(super) fn emit_owned_string_from_handle(
        &mut self,
        handle: inkwell::values::IntValue<'ctx>,
        len: inkwell::values::IntValue<'ctx>,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let is_empty = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                len,
                len.get_type().const_zero(),
                "mir_string_empty",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let is_null = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                handle,
                handle.get_type().const_zero(),
                "mir_string_alloc_null",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let not_empty = self
            .generator
            .builder
            .build_not(is_empty, "mir_string_not_empty")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let failed = self
            .generator
            .builder
            .build_and(not_empty, is_null, "mir_string_alloc_failed")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let fail = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_string_alloc_abort");
        let ok = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_string_alloc_ok");
        self.generator
            .builder
            .build_conditional_branch(failed, fail, ok)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(fail);
        self.emit_abort_with_message("[E0800] canonical MIR String allocation failed", subject)?;
        self.generator.builder.position_at_end(ok);
        let pointer_type = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default());
        let data = self
            .generator
            .builder
            .build_int_to_ptr(handle, pointer_type, "mir_string_data")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .build_string_struct(data, len)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))
    }
}
