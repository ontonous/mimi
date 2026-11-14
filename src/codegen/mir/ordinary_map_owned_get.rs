//! Native consumer for the independent ordinary-Map owned-Get profile.
//!
//! The profile has an identity-specific persistent root and runtime ABI. Its
//! getters return owned canonical `Result` values; they never expose the
//! runtime's storage or use the legacy raw-bit Map representation.

use super::*;
use crate::core::mir::types::{MirGlueOperation, MirLayout};

const OWNED_GET_PREFIX: &str = "mimi_mir_ordinary_map_owned_get_i32_string_v1_";

impl<'a, 'ctx> NativeMirFunctionEmitter<'a, 'ctx> {
    fn validate_ordinary_map_owned_get_root(
        &self,
        value: &MirValueId,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let ty = self.value_type(value, subject)?;
        if ty != crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id() {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map owned-Get operand does not use its canonical v1 TypeDesc",
            ));
        }
        self.program
            .type_catalog()
            .validate_glue(&ty, MirGlueOperation::MoveOut)
            .map_err(|message| NativeMirError::new(subject, message))
    }

    pub(super) fn emit_ordinary_map_owned_get_root_guard(
        &mut self,
        condition: inkwell::values::IntValue<'ctx>,
        message: &str,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let valid = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_owned_get_root_valid");
        let invalid = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_owned_get_root_invalid");
        self.generator
            .builder
            .build_conditional_branch(condition, valid, invalid)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(invalid);
        self.emit_abort_with_message(message, subject)?;
        self.generator.builder.position_at_end(valid);
        Ok(())
    }

    pub(super) fn emit_live_ordinary_map_owned_get_root(
        &mut self,
        value: &MirValueId,
        subject: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, NativeMirError> {
        self.validate_ordinary_map_owned_get_root(value, subject)?;
        let handle = self.value(value, subject)?.into_int_value();
        let nonzero = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                handle,
                self.generator.context.i64_type().const_zero(),
                "mir_owned_get_root_nonzero",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_ordinary_map_owned_get_root_guard(
            nonzero,
            "[E0800] ordinary Map owned-Get root is invalid",
            subject,
        )?;
        Ok(handle)
    }

    fn ordinary_map_owned_get_primitive_type(
        &self,
        primitive: crate::core::PrimitiveType,
        subject: &str,
    ) -> Result<crate::core::ResolvedTypeId, NativeMirError> {
        self.program
            .type_catalog()
            .iter()
            .find_map(|(ty, descriptor)| {
                (descriptor.kind == MirTypeKind::Primitive(primitive)).then(|| ty.clone())
            })
            .ok_or_else(|| {
                NativeMirError::new(
                    subject,
                    format!("canonical {primitive:?} TypeDesc is absent"),
                )
            })
    }

    pub(super) fn ordinary_map_owned_get_string_parts(
        &mut self,
        value: &MirValueId,
        role: &str,
        subject: &str,
    ) -> Result<
        (
            inkwell::values::PointerValue<'ctx>,
            inkwell::values::IntValue<'ctx>,
        ),
        NativeMirError,
    > {
        let ty = self.value_type(value, subject)?;
        let is_string = self
            .program
            .type_catalog()
            .get(&ty)
            .is_some_and(|descriptor| {
                descriptor.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::String)
            });
        if !is_string {
            return Err(NativeMirError::new(
                subject,
                format!("ordinary Map owned-Get {role} is not canonical String"),
            ));
        }
        self.program
            .type_catalog()
            .validate_owned_string(&ty)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let string = self.value(value, subject)?.into_struct_value();
        let data = self
            .generator
            .builder
            .build_extract_value(string, 0, "mir_owned_get_string_data")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_pointer_value();
        let len = self
            .generator
            .builder
            .build_extract_value(string, 1, "mir_owned_get_string_len")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        Ok((data, len))
    }

    fn validate_ordinary_map_owned_get_result(
        &self,
        result: &MirValueId,
        expected_ok: &crate::core::ResolvedTypeId,
        subject: &str,
    ) -> Result<crate::core::ResolvedTypeId, NativeMirError> {
        let result_ty = self.value_type(result, subject)?;
        let descriptor = self.program.type_catalog().get(&result_ty).ok_or_else(|| {
            NativeMirError::new(subject, "ordinary Map owned-Get Result TypeDesc is absent")
        })?;
        let MirLayout::Result {
            ok,
            error,
            variants,
        } = &descriptor.layout
        else {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map owned-Get result does not have canonical Result layout",
            ));
        };
        let i32_ty =
            self.ordinary_map_owned_get_primitive_type(crate::core::PrimitiveType::I32, subject)?;
        if descriptor.kind != MirTypeKind::Result || ok != expected_ok || error != &i32_ty {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map owned-Get result TypeDesc is not Result<requested payload, i32>",
            ));
        }
        let ok_variant = crate::core::NodeId("builtin:variant:Result::Ok".into());
        let err_variant = crate::core::NodeId("builtin:variant:Result::Err".into());
        let result_nominal = crate::core::NominalTypeId::new("builtin:type:Result")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let ok_field = crate::core::NodeId("builtin:variant:Result::Ok/payload:0".into());
        let err_field = crate::core::NodeId("builtin:variant:Result::Err/payload:0".into());
        self.program
            .type_catalog()
            .validated_variant_construct(
                &result_ty,
                &result_nominal,
                &ok_variant,
                std::slice::from_ref(&ok_field),
                std::slice::from_ref(expected_ok),
            )
            .map_err(|message| NativeMirError::new(subject, message))?;
        self.program
            .type_catalog()
            .validated_variant_construct(
                &result_ty,
                &result_nominal,
                &err_variant,
                std::slice::from_ref(&err_field),
                std::slice::from_ref(&i32_ty),
            )
            .map_err(|message| NativeMirError::new(subject, message))?;
        if variants.len() != 2 {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map owned-Get Result has a noncanonical alternative count",
            ));
        }
        let moving = descriptor.ownership != MirOwnership::Copy;
        native_variant_abi(self.program.type_catalog(), &result_ty, moving)?;
        Ok(result_ty)
    }

    fn ordinary_map_owned_get_result_variant(
        &mut self,
        result_ty: &crate::core::ResolvedTypeId,
        variant_id: &crate::core::NodeId,
        payload: BasicValueEnum<'ctx>,
        payload_ty: &crate::core::ResolvedTypeId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let catalog = self.program.type_catalog();
        let descriptor = catalog.get(result_ty).ok_or_else(|| {
            NativeMirError::new(
                subject,
                "ordinary Map owned-Get Result TypeDesc disappeared",
            )
        })?;
        let MirLayout::Result { variants, .. } = &descriptor.layout else {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map owned-Get Result layout changed during emission",
            ));
        };
        let variant = variants
            .iter()
            .find(|variant| variant.id == *variant_id)
            .ok_or_else(|| {
                NativeMirError::new(subject, "Result variant is absent from TypeDesc")
            })?;
        if !matches!(variant.fields.as_slice(), [field] if field.ty == *payload_ty) {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map owned-Get Result payload disagrees with its TypeDesc",
            ));
        }
        let moving = descriptor.ownership != MirOwnership::Copy;
        let (variant_abi, _) = native_variant_abi(catalog, result_ty, moving)?;
        let llvm_ty =
            native_basic_type(self.generator.context, catalog, result_ty)?.into_struct_type();
        let mut aggregate = llvm_ty.get_undef();
        for (index, ty) in variant_abi.payload_types.iter().enumerate() {
            let payload_type = native_basic_type(self.generator.context, catalog, ty)?;
            let field = self.u32_abi(index + 1, "Result payload field index", subject)?;
            aggregate = self
                .generator
                .builder
                .build_insert_value(
                    aggregate,
                    payload_type.const_zero(),
                    field,
                    "mir_owned_get_result_zero_payload",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                .into_struct_value();
        }
        aggregate = self
            .generator
            .builder
            .build_insert_value(
                aggregate,
                self.generator
                    .context
                    .i8_type()
                    .const_int(u64::from(variant.discriminant), false),
                variant_abi.tag_field,
                "mir_owned_get_result_tag",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_struct_value();
        let payload_slot = variant_abi
            .payload_slot(variant_id)
            .ok_or_else(|| NativeMirError::new(subject, "Result payload slot is absent"))?;
        if payload_slot.ty != *payload_ty {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map owned-Get Result payload slot changed TypeDesc",
            ));
        }
        aggregate = self
            .generator
            .builder
            .build_insert_value(
                aggregate,
                payload,
                payload_slot.physical_field,
                "mir_owned_get_result_payload",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_struct_value();
        Ok(aggregate.into())
    }

    fn emit_ordinary_map_owned_get_status_result(
        &mut self,
        result_ty: &crate::core::ResolvedTypeId,
        ok_payload: BasicValueEnum<'ctx>,
        ok_type: &crate::core::ResolvedTypeId,
        status: inkwell::values::IntValue<'ctx>,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let catalog = self.program.type_catalog();
        let result_llvm_ty = native_basic_type(self.generator.context, catalog, result_ty)?;
        let result_slot = self
            .generator
            .builder
            .build_alloca(result_llvm_ty, "mir_owned_get_result_slot")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let ok_block = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_owned_get_status_ok");
        let non_ok_block = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_owned_get_status_non_ok");
        let err_block = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_owned_get_status_err");
        let invalid_block = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_owned_get_status_invalid");
        let merge_block = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_owned_get_status_merge");
        let zero = self.generator.context.i32_type().const_zero();
        let is_ok = self
            .generator
            .builder
            .build_int_compare(IntPredicate::EQ, status, zero, "mir_owned_get_status_is_ok")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_conditional_branch(is_ok, ok_block, non_ok_block)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        let ok_id = crate::core::NodeId("builtin:variant:Result::Ok".into());
        let err_id = crate::core::NodeId("builtin:variant:Result::Err".into());
        let i32_ty =
            self.ordinary_map_owned_get_primitive_type(crate::core::PrimitiveType::I32, subject)?;
        self.generator.builder.position_at_end(ok_block);
        let ok = self.ordinary_map_owned_get_result_variant(
            result_ty, &ok_id, ok_payload, ok_type, subject,
        )?;
        self.generator
            .builder
            .build_store(result_slot, ok)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_unconditional_branch(merge_block)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        self.generator.builder.position_at_end(non_ok_block);
        let one = self.generator.context.i32_type().const_int(1, false);
        let three = self.generator.context.i32_type().const_int(3, false);
        let at_least_missing = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::UGE,
                status,
                one,
                "mir_owned_get_status_at_least_missing",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let at_most_capacity = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::ULE,
                status,
                three,
                "mir_owned_get_status_at_most_capacity",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let known_error = self
            .generator
            .builder
            .build_and(
                at_least_missing,
                at_most_capacity,
                "mir_owned_get_status_known_error",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_conditional_branch(known_error, err_block, invalid_block)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        self.generator.builder.position_at_end(invalid_block);
        self.emit_abort_with_message(
            "[E0800] ordinary Map owned-Get returned an invalid status",
            subject,
        )?;

        self.generator.builder.position_at_end(err_block);
        let err = self.ordinary_map_owned_get_result_variant(
            result_ty,
            &err_id,
            status.into(),
            &i32_ty,
            subject,
        )?;
        self.generator
            .builder
            .build_store(result_slot, err)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_unconditional_branch(merge_block)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        self.generator.builder.position_at_end(merge_block);
        self.generator
            .builder
            .build_load(result_llvm_ty, result_slot, "mir_owned_get_result")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))
    }

    fn emit_ordinary_map_owned_get_capacity_result(
        &mut self,
        result: &MirValueId,
        status: inkwell::values::IntValue<'ctx>,
        out_root: inkwell::values::PointerValue<'ctx>,
        source: Option<&MirValueId>,
        capacity_status: u64,
        operation: &str,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let result_ty = self.value_type(result, subject)?;
        if result_ty
            != crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_root_result_type_id()
        {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map owned-Get ResultBranchV3 operation has a noncanonical Result TypeDesc",
            ));
        }
        let catalog = self.program.type_catalog();
        let result_llvm_ty = native_basic_type(self.generator.context, catalog, &result_ty)?;
        let result_slot = self
            .generator
            .builder
            .build_alloca(result_llvm_ty, "mir_owned_get_capacity_result_slot")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let ok_block = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_owned_get_capacity_ok");
        let non_ok_block = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_owned_get_capacity_non_ok");
        let err_block = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_owned_get_capacity_err");
        let invalid_block = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_owned_get_capacity_invalid");
        let merge_block = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_owned_get_capacity_merge");
        let is_ok = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.generator.context.i32_type().const_zero(),
                "mir_owned_get_capacity_is_ok",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_conditional_branch(is_ok, ok_block, non_ok_block)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        let ok_id = crate::core::NodeId("builtin:variant:Result::Ok".into());
        let err_id = crate::core::NodeId("builtin:variant:Result::Err".into());
        let root_ty = crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id();
        let i32_ty =
            self.ordinary_map_owned_get_primitive_type(crate::core::PrimitiveType::I32, subject)?;

        self.generator.builder.position_at_end(ok_block);
        let handle = self
            .generator
            .builder
            .build_load(
                self.generator.context.i64_type(),
                out_root,
                "mir_owned_get_capacity_root",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let root = if let Some(source) = source {
            self.emit_ordinary_map_owned_get_result_root(handle, source, subject, operation)?
        } else {
            let nonzero = self
                .generator
                .builder
                .build_int_compare(
                    IntPredicate::NE,
                    handle,
                    self.generator.context.i64_type().const_zero(),
                    "mir_owned_get_capacity_root_nonzero",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            self.emit_ordinary_map_owned_get_root_guard(
                nonzero,
                "[E0800] ordinary Map owned-Get NewResult returned an invalid root",
                subject,
            )?;
            handle.into()
        };
        let ok = self
            .ordinary_map_owned_get_result_variant(&result_ty, &ok_id, root, &root_ty, subject)?;
        self.generator
            .builder
            .build_store(result_slot, ok)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_unconditional_branch(merge_block)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        self.generator.builder.position_at_end(non_ok_block);
        let expected_capacity = self
            .generator
            .context
            .i32_type()
            .const_int(capacity_status, false);
        let is_capacity = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                expected_capacity,
                "mir_owned_get_capacity_is_expected_failure",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_conditional_branch(is_capacity, err_block, invalid_block)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        self.generator.builder.position_at_end(err_block);
        let error_root = self
            .generator
            .builder
            .build_load(
                self.generator.context.i64_type(),
                out_root,
                "mir_owned_get_capacity_error_root",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let error_root_is_zero = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                error_root,
                self.generator.context.i64_type().const_zero(),
                "mir_owned_get_capacity_error_root_is_zero",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_ordinary_map_owned_get_root_guard(
            error_root_is_zero,
            "[E0800] ordinary Map owned-Get recoverable failure published a root",
            subject,
        )?;
        let err = self.ordinary_map_owned_get_result_variant(
            &result_ty,
            &err_id,
            status.into(),
            &i32_ty,
            subject,
        )?;
        self.generator
            .builder
            .build_store(result_slot, err)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_unconditional_branch(merge_block)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        self.generator.builder.position_at_end(invalid_block);
        self.emit_abort_with_message(
            "[E0800] ordinary Map owned-Get ResultBranchV3 returned an invalid status",
            subject,
        )?;

        self.generator.builder.position_at_end(merge_block);
        self.generator
            .builder
            .build_load(result_llvm_ty, result_slot, "mir_owned_get_capacity_result")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))
    }

    fn emit_ordinary_map_owned_get_result_root(
        &mut self,
        handle: inkwell::values::IntValue<'ctx>,
        source: &MirValueId,
        subject: &str,
        operation: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let nonzero = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                handle,
                self.generator.context.i64_type().const_zero(),
                "mir_owned_get_result_nonzero",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_ordinary_map_owned_get_root_guard(
            nonzero,
            "[E0800] ordinary Map owned-Get allocation failed",
            subject,
        )?;
        let source_handle = self.value(source, subject)?.into_int_value();
        let distinct = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                handle,
                source_handle,
                "mir_owned_get_result_distinct",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_ordinary_map_owned_get_root_guard(
            distinct,
            &format!(
                "[E0800] ordinary Map owned-Get persistent {operation} aliased its source root"
            ),
            subject,
        )?;
        Ok(handle.into())
    }

    pub(super) fn emit_ordinary_map_owned_get_new(
        &mut self,
        result: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        self.validate_ordinary_map_owned_get_root(result, subject)?;
        let new_fn = self
            .generator
            .get_runtime_fn(&format!("{OWNED_GET_PREFIX}new"))
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let handle = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(new_fn, &[], "mir_owned_get_new")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "ordinary Map owned-Get New returned void"))?
        .into_int_value();
        let nonzero = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                handle,
                self.generator.context.i64_type().const_zero(),
                "mir_owned_get_new_nonzero",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_ordinary_map_owned_get_root_guard(
            nonzero,
            "[E0800] ordinary Map owned-Get allocation failed",
            subject,
        )?;
        Ok(handle.into())
    }

    pub(super) fn emit_ordinary_map_owned_get_new_result(
        &mut self,
        result: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let out_root = self
            .generator
            .builder
            .build_alloca(
                self.generator.context.i64_type(),
                "mir_owned_get_new_result_root",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(out_root, self.generator.context.i64_type().const_zero())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let new_fn = self
            .generator
            .get_runtime_fn(&format!("{OWNED_GET_PREFIX}new_result"))
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let status = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    new_fn,
                    &[BasicMetadataValueEnum::from(out_root)],
                    "mir_owned_get_new_result_status",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| {
            NativeMirError::new(subject, "ordinary Map owned-Get NewResult returned void")
        })?
        .into_int_value();
        self.emit_ordinary_map_owned_get_capacity_result(
            result, status, out_root, None, 3, "New", subject,
        )
    }

    pub(super) fn emit_ordinary_map_owned_get_clone(
        &mut self,
        result: &MirValueId,
        source: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        self.validate_ordinary_map_owned_get_root(result, subject)?;
        let source_handle = self.emit_live_ordinary_map_owned_get_root(source, subject)?;
        let clone_fn = self
            .generator
            .get_runtime_fn("mimi_mir_ordinary_map_owned_get_i32_string_v1_clone")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let handle = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(clone_fn, &[source_handle.into()], "mir_ordinary_map_clone")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "ordinary Map owned-Get Clone returned void"))?
        .into_int_value();
        self.emit_ordinary_map_owned_get_result_root(handle, source, subject, "Clone")
    }

    pub(super) fn emit_ordinary_map_owned_get_set(
        &mut self,
        result: &MirValueId,
        source: &MirValueId,
        key: &MirValueId,
        value: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        self.validate_ordinary_map_owned_get_root(result, subject)?;
        let source_handle = self.emit_live_ordinary_map_owned_get_root(source, subject)?;
        let (key_data, key_len) =
            self.ordinary_map_owned_get_string_parts(key, "Set key", subject)?;
        let value_ty = self.value_type(value, subject)?;
        let value_desc = self.program.type_catalog().get(&value_ty).ok_or_else(|| {
            NativeMirError::new(
                subject,
                "ordinary Map owned-Get Set value TypeDesc is absent",
            )
        })?;
        let is_i32 = value_desc.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::I32);
        let is_string =
            value_desc.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::String);
        let (function_suffix, args) = if is_i32 {
            self.program
                .type_catalog()
                .validate_copy_scalar(&value_ty)
                .map_err(|message| NativeMirError::new(subject, message))?;
            (
                "set_i32",
                vec![
                    BasicMetadataValueEnum::from(source_handle),
                    BasicMetadataValueEnum::from(key_data),
                    BasicMetadataValueEnum::from(key_len),
                    BasicMetadataValueEnum::from(self.value(value, subject)?.into_int_value()),
                ],
            )
        } else if is_string {
            self.program
                .type_catalog()
                .validate_owned_string(&value_ty)
                .map_err(|message| NativeMirError::new(subject, message))?;
            let (value_data, value_len) =
                self.ordinary_map_owned_get_string_parts(value, "Set value", subject)?;
            (
                "set_string",
                vec![
                    BasicMetadataValueEnum::from(source_handle),
                    BasicMetadataValueEnum::from(key_data),
                    BasicMetadataValueEnum::from(key_len),
                    BasicMetadataValueEnum::from(value_data),
                    BasicMetadataValueEnum::from(value_len),
                ],
            )
        } else {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map owned-Get Set payload is outside its i32/String profile",
            ));
        };
        let set_fn = self
            .generator
            .get_runtime_fn(&format!("{OWNED_GET_PREFIX}{function_suffix}"))
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let updated = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(set_fn, &args, "mir_owned_get_set")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "ordinary Map owned-Get Set returned void"))?
        .into_int_value();
        self.emit_ordinary_map_owned_get_result_root(updated, source, subject, "Set")
    }

    pub(super) fn emit_ordinary_map_owned_get_set_result(
        &mut self,
        result: &MirValueId,
        source: &MirValueId,
        key: &MirValueId,
        value: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let source_handle = self.emit_live_ordinary_map_owned_get_root(source, subject)?;
        let (key_data, key_len) =
            self.ordinary_map_owned_get_string_parts(key, "SetResult key", subject)?;
        let value_ty = self.value_type(value, subject)?;
        let value_desc = self.program.type_catalog().get(&value_ty).ok_or_else(|| {
            NativeMirError::new(
                subject,
                "ordinary Map owned-Get SetResult value TypeDesc is absent",
            )
        })?;
        let is_i32 = value_desc.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::I32);
        let is_string =
            value_desc.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::String);
        let (function_suffix, value_arguments) = if is_i32 {
            self.program
                .type_catalog()
                .validate_copy_scalar(&value_ty)
                .map_err(|message| NativeMirError::new(subject, message))?;
            (
                "set_i32_result",
                vec![BasicMetadataValueEnum::from(
                    self.value(value, subject)?.into_int_value(),
                )],
            )
        } else if is_string {
            self.program
                .type_catalog()
                .validate_owned_string(&value_ty)
                .map_err(|message| NativeMirError::new(subject, message))?;
            let (value_data, value_len) =
                self.ordinary_map_owned_get_string_parts(value, "SetResult value", subject)?;
            (
                "set_string_result",
                vec![
                    BasicMetadataValueEnum::from(value_data),
                    BasicMetadataValueEnum::from(value_len),
                ],
            )
        } else {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map owned-Get SetResult payload is outside its i32/String profile",
            ));
        };
        let out_root = self
            .generator
            .builder
            .build_alloca(
                self.generator.context.i64_type(),
                "mir_owned_get_set_result_root",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(out_root, self.generator.context.i64_type().const_zero())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let mut arguments = vec![
            BasicMetadataValueEnum::from(source_handle),
            BasicMetadataValueEnum::from(key_data),
            BasicMetadataValueEnum::from(key_len),
        ];
        arguments.extend(value_arguments);
        arguments.push(BasicMetadataValueEnum::from(out_root));
        let set_fn = self
            .generator
            .get_runtime_fn(&format!("{OWNED_GET_PREFIX}{function_suffix}"))
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let status = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(set_fn, &arguments, "mir_owned_get_set_result_status")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| {
            NativeMirError::new(subject, "ordinary Map owned-Get SetResult returned void")
        })?
        .into_int_value();
        self.emit_ordinary_map_owned_get_capacity_result(
            result,
            status,
            out_root,
            Some(source),
            2,
            "Set",
            subject,
        )
    }

    pub(super) fn emit_ordinary_map_owned_get_remove(
        &mut self,
        result: &MirValueId,
        source: &MirValueId,
        key: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        self.validate_ordinary_map_owned_get_root(result, subject)?;
        let source_handle = self.emit_live_ordinary_map_owned_get_root(source, subject)?;
        let (key_data, key_len) =
            self.ordinary_map_owned_get_string_parts(key, "Remove key", subject)?;
        let remove_fn = self
            .generator
            .get_runtime_fn(&format!("{OWNED_GET_PREFIX}remove"))
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let removed = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    remove_fn,
                    &[
                        BasicMetadataValueEnum::from(source_handle),
                        BasicMetadataValueEnum::from(key_data),
                        BasicMetadataValueEnum::from(key_len),
                    ],
                    "mir_owned_get_remove",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "ordinary Map owned-Get Remove returned void"))?
        .into_int_value();
        self.emit_ordinary_map_owned_get_result_root(removed, source, subject, "Remove")
    }

    pub(super) fn emit_ordinary_map_owned_get_size(
        &mut self,
        result: &MirValueId,
        root: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let handle = self.emit_live_ordinary_map_owned_get_root(root, subject)?;
        let i32_ty =
            self.ordinary_map_owned_get_primitive_type(crate::core::PrimitiveType::I32, subject)?;
        if self.value_type(result, subject)? != i32_ty {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map owned-Get Size result is not canonical i32",
            ));
        }
        self.program
            .type_catalog()
            .validate_copy_scalar(&i32_ty)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let size_fn = self
            .generator
            .get_runtime_fn(&format!("{OWNED_GET_PREFIX}size"))
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let size = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    size_fn,
                    &[BasicMetadataValueEnum::from(handle)],
                    "mir_owned_get_size",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "ordinary Map owned-Get Size returned void"))?
        .into_int_value();
        let valid = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::SGE,
                size,
                self.generator.context.i32_type().const_zero(),
                "mir_owned_get_size_valid",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_ordinary_map_owned_get_root_guard(
            valid,
            "[E0800] ordinary Map owned-Get Size failed",
            subject,
        )?;
        Ok(size.into())
    }

    pub(super) fn emit_ordinary_map_owned_get_contains(
        &mut self,
        result: &MirValueId,
        root: &MirValueId,
        key: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let handle = self.emit_live_ordinary_map_owned_get_root(root, subject)?;
        let bool_ty =
            self.ordinary_map_owned_get_primitive_type(crate::core::PrimitiveType::Bool, subject)?;
        if self.value_type(result, subject)? != bool_ty {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map owned-Get Contains result is not canonical bool",
            ));
        }
        self.program
            .type_catalog()
            .validate_copy_scalar(&bool_ty)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let (key_data, key_len) =
            self.ordinary_map_owned_get_string_parts(key, "Contains key", subject)?;
        let contains_fn = self
            .generator
            .get_runtime_fn(&format!("{OWNED_GET_PREFIX}contains"))
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let contains = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    contains_fn,
                    &[
                        BasicMetadataValueEnum::from(handle),
                        BasicMetadataValueEnum::from(key_data),
                        BasicMetadataValueEnum::from(key_len),
                    ],
                    "mir_owned_get_contains",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| {
            NativeMirError::new(subject, "ordinary Map owned-Get Contains returned void")
        })?
        .into_int_value();
        let valid = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::ULE,
                contains,
                self.generator.context.i32_type().const_int(1, false),
                "mir_owned_get_contains_valid",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_ordinary_map_owned_get_root_guard(
            valid,
            "[E0800] ordinary Map owned-Get Contains failed",
            subject,
        )?;
        let boolean = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                contains,
                self.generator.context.i32_type().const_zero(),
                "mir_owned_get_contains_bool",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        Ok(boolean.into())
    }

    pub(super) fn emit_ordinary_map_owned_get_values(
        &mut self,
        result: &MirValueId,
        root: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let handle = self.emit_live_ordinary_map_owned_get_root(root, subject)?;
        let result_ty = self.value_type(result, subject)?;
        self.program
            .type_catalog()
            .validate_list_glue(&result_ty, MirGlueOperation::MoveOut)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let kind = super::abi::native_list_kind(self.program.type_catalog(), &result_ty)?;
        if kind != crate::runtime::ListElementKind::ManagedAny as i8 {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map owned-Get Values result is not List<ManagedAny>",
            ));
        }
        let values_fn = self
            .generator
            .get_runtime_fn(&format!("{OWNED_GET_PREFIX}values"))
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let list = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    values_fn,
                    &[BasicMetadataValueEnum::from(handle)],
                    "mir_owned_get_values",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "ordinary Map owned-Get Values returned void"))?
        .into_pointer_value();
        self.emit_list_null_abort(list, subject, "ordinary Map owned-Get Values failed")?;
        Ok(list.into())
    }

    pub(super) fn emit_ordinary_map_owned_get_i32(
        &mut self,
        result: &MirValueId,
        root: &MirValueId,
        key: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let handle = self.emit_live_ordinary_map_owned_get_root(root, subject)?;
        let i32_ty =
            self.ordinary_map_owned_get_primitive_type(crate::core::PrimitiveType::I32, subject)?;
        self.validate_ordinary_map_owned_get_result(result, &i32_ty, subject)?;
        self.program
            .type_catalog()
            .validate_copy_scalar(&i32_ty)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let (key_data, key_len) =
            self.ordinary_map_owned_get_string_parts(key, "GetI32 key", subject)?;
        let out_value = self
            .generator
            .builder
            .build_alloca(self.generator.context.i32_type(), "mir_owned_get_i32_out")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(out_value, self.generator.context.i32_type().const_zero())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let get_fn = self
            .generator
            .get_runtime_fn(&format!("{OWNED_GET_PREFIX}get_i32"))
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let status = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    get_fn,
                    &[
                        BasicMetadataValueEnum::from(handle),
                        BasicMetadataValueEnum::from(key_data),
                        BasicMetadataValueEnum::from(key_len),
                        BasicMetadataValueEnum::from(out_value),
                    ],
                    "mir_owned_get_i32_status",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "ordinary Map owned-Get i32 returned void"))?
        .into_int_value();
        let payload = self
            .generator
            .builder
            .build_load(
                self.generator.context.i32_type(),
                out_value,
                "mir_owned_get_i32",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let result_ty = self.value_type(result, subject)?;
        self.emit_ordinary_map_owned_get_status_result(
            &result_ty, payload, &i32_ty, status, subject,
        )
    }

    pub(super) fn emit_ordinary_map_owned_get_string(
        &mut self,
        result: &MirValueId,
        root: &MirValueId,
        key: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let handle = self.emit_live_ordinary_map_owned_get_root(root, subject)?;
        let string_ty = self
            .ordinary_map_owned_get_primitive_type(crate::core::PrimitiveType::String, subject)?;
        self.program
            .type_catalog()
            .validate_owned_string(&string_ty)
            .map_err(|message| NativeMirError::new(subject, message))?;
        self.validate_ordinary_map_owned_get_result(result, &string_ty, subject)?;
        let (key_data, key_len) =
            self.ordinary_map_owned_get_string_parts(key, "GetString key", subject)?;
        let i8_ptr = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default());
        let out_data = self
            .generator
            .builder
            .build_alloca(i8_ptr, "mir_owned_get_string_out_ptr")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let out_len = self
            .generator
            .builder
            .build_alloca(
                self.generator.context.i64_type(),
                "mir_owned_get_string_out_len",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(out_data, i8_ptr.const_null())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(out_len, self.generator.context.i64_type().const_zero())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let get_fn = self
            .generator
            .get_runtime_fn(&format!("{OWNED_GET_PREFIX}get_string"))
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let status = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    get_fn,
                    &[
                        BasicMetadataValueEnum::from(handle),
                        BasicMetadataValueEnum::from(key_data),
                        BasicMetadataValueEnum::from(key_len),
                        BasicMetadataValueEnum::from(out_data),
                        BasicMetadataValueEnum::from(out_len),
                    ],
                    "mir_owned_get_string_status",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "ordinary Map owned-Get String returned void"))?
        .into_int_value();
        let payload = self.emit_ordinary_map_owned_get_string_payload(
            out_data, out_len, &string_ty, status, subject,
        )?;
        let result_ty = self.value_type(result, subject)?;
        self.emit_ordinary_map_owned_get_status_result(
            &result_ty, payload, &string_ty, status, subject,
        )
    }

    fn emit_ordinary_map_owned_get_string_payload(
        &mut self,
        out_data: inkwell::values::PointerValue<'ctx>,
        out_len: inkwell::values::PointerValue<'ctx>,
        string_ty: &crate::core::ResolvedTypeId,
        status: inkwell::values::IntValue<'ctx>,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let i8_ptr = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default());
        let data = self
            .generator
            .builder
            .build_load(i8_ptr, out_data, "mir_owned_get_string_data")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_pointer_value();
        let len = self
            .generator
            .builder
            .build_load(
                self.generator.context.i64_type(),
                out_len,
                "mir_owned_get_string_len",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();

        // A successful runtime String result is an owned allocation, including
        // the empty String case.  Reject a malformed successful ABI before it
        // can enter the canonical Result and later be treated as owned glue.
        let is_ok = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.generator.context.i32_type().const_zero(),
                "mir_owned_get_string_is_ok",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let ptr_nonnull = self
            .generator
            .builder
            .build_is_not_null(data, "mir_owned_get_string_data_nonnull")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let len_nonnegative = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::SGE,
                len,
                self.generator.context.i64_type().const_zero(),
                "mir_owned_get_string_len_nonnegative",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let valid_payload = self
            .generator
            .builder
            .build_and(
                ptr_nonnull,
                len_nonnegative,
                "mir_owned_get_string_payload_valid",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let payload_invalid = self
            .generator
            .builder
            .build_not(valid_payload, "mir_owned_get_string_payload_invalid")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let malformed = self
            .generator
            .builder
            .build_and(
                is_ok,
                payload_invalid,
                "mir_owned_get_string_payload_malformed",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let valid_block = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_owned_get_string_payload_valid");
        let invalid_block = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_owned_get_string_payload_invalid");
        self.generator
            .builder
            .build_conditional_branch(malformed, invalid_block, valid_block)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(invalid_block);
        self.emit_abort_with_message(
            "[E0800] ordinary Map owned-Get String returned malformed owned payload",
            subject,
        )?;
        self.generator.builder.position_at_end(valid_block);

        let register_block = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_owned_get_string_register");
        let ready_block = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_owned_get_string_ready");
        self.generator
            .builder
            .build_conditional_branch(is_ok, register_block, ready_block)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(register_block);
        self.emit_ordinary_map_domain_own_string(data, len, subject)?;
        self.generator
            .builder
            .build_unconditional_branch(ready_block)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(ready_block);

        let string_llvm_ty = native_basic_type(
            self.generator.context,
            self.program.type_catalog(),
            string_ty,
        )?
        .into_struct_type();
        let mut string = string_llvm_ty.get_undef();
        string = self
            .generator
            .builder
            .build_insert_value(string, data, 0, "mir_owned_get_string_data_field")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_struct_value();
        string = self
            .generator
            .builder
            .build_insert_value(string, len, 1, "mir_owned_get_string_len_field")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_struct_value();
        Ok(string.into())
    }

    pub(super) fn emit_ordinary_map_owned_get_drop(
        &mut self,
        root: &MirValueId,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let handle = self.emit_live_ordinary_map_owned_get_root(root, subject)?;
        let ty = self.value_type(root, subject)?;
        self.program
            .type_catalog()
            .validate_glue(&ty, MirGlueOperation::Drop)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let drop_fn = self
            .generator
            .get_runtime_fn(&format!("{OWNED_GET_PREFIX}drop"))
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(
                drop_fn,
                &[BasicMetadataValueEnum::from(handle)],
                "mir_owned_get_drop",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::codegen::CodeGenerator;
    use crate::core::mir::reference::{MirProgram, MirReferenceInterpreter, MirRuntimeValue};
    use crate::interp::bytecode::{compile_mir_program, BytecodeVM};
    use crate::interp::Value;
    use crate::lexer::Lexer;
    use crate::parser::Parser;
    use inkwell::context::Context;

    fn canonical_program(source: &str) -> MirProgram {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        MirProgram::from_checked_program(&checked).expect("canonical MIR")
    }

    fn with_owned_get_failure<T>(stages_before_failure: usize, operation: impl FnOnce() -> T) -> T {
        crate::runtime::with_ordinary_map_owned_get_allocation_failure_for_test(
            stages_before_failure,
            || {
                let result = operation();
                assert!(
                    crate::runtime::ordinary_map_owned_get_allocation_failure_triggered_for_test(),
                    "the getter must actually reach the injected allocation failure"
                );
                assert_eq!(
                    crate::runtime::ordinary_map_owned_get_allocation_attempts_for_test(),
                    stages_before_failure + 1,
                    "all consumers must fail at exactly the selected String staging step"
                );
                result
            },
        )
    }

    #[test]
    fn ordinary_map_owned_get_v1_capacity_is_one_mir_across_reference_bytecode_and_native() {
        let program = canonical_program(
            r#"
func snapshot_status(value: Result<string, i32>) -> i32 {
    match value {
        Ok(text) => {
            drop(text)
            7
        },
        Err(status) => status,
    }
}

func main() -> i32 {
    let empty = map_new()
    let root = map_set(empty, "snapshot-key", "snapshot-value")
    let snapshot = map_get_string(root, "snapshot-key")
    drop(empty)
    drop(root)
    snapshot_status(snapshot)
}
"#,
        );
        let digest = program.canonical_digest();
        crate::codegen::mir::validate_mir_native(&program)
            .expect("failure injection must preserve the exact owned-Get admission contract");
        let main = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program);
        let bytecode = compile_mir_program(&program).expect("same MIR AST-free owned-Get bytecode");
        assert!(
            bytecode.ast.is_none(),
            "the failure oracle must not retain a surface AST"
        );
        let mut vm = BytecodeVM::new(bytecode);

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_owned_get_capacity_same_program");
        generator
            .compile_mir_native(&program)
            .expect("same MIR native owned-Get lowering");
        generator
            .module
            .verify()
            .expect("same MIR native module verifies");
        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("call i32 @mimi_mir_ordinary_map_owned_get_i32_string_v1_get_string"));
        assert!(!ir.contains("call i64 @mimi_map_get"));
        assert!(!ir.contains("call i64 @mimi_mir_map_root_"));

        // Linked native subprocesses contain the production runtime without
        // cfg(test) hooks. Execute the unmodified emitted module in-process,
        // binding its declarations to the real runtime functions here, so
        // native observes the same thread-local allocation oracle. No getter
        // shim substitutes its Result status or payload.
        let engine = generator
            .module
            .create_jit_execution_engine(inkwell::OptimizationLevel::None)
            .expect("native owned-Get in-process execution engine");
        for (symbol, address) in [
            (
                "mimi_mir_ordinary_map_owned_get_i32_string_v1_new",
                crate::runtime::mimi_mir_ordinary_map_owned_get_i32_string_v1_new as *const ()
                    as usize,
            ),
            (
                "mimi_mir_ordinary_map_owned_get_i32_string_v1_set_string",
                crate::runtime::mimi_mir_ordinary_map_owned_get_i32_string_v1_set_string
                    as *const () as usize,
            ),
            (
                "mimi_mir_ordinary_map_owned_get_i32_string_v1_get_string",
                crate::runtime::mimi_mir_ordinary_map_owned_get_i32_string_v1_get_string
                    as *const () as usize,
            ),
            (
                "mimi_mir_ordinary_map_owned_get_i32_string_v1_drop",
                crate::runtime::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop as *const ()
                    as usize,
            ),
            (
                "mimi_mir_ordinary_map_domain_own_string",
                crate::runtime::mimi_mir_ordinary_map_domain_own_string as *const () as usize,
            ),
            (
                "mimi_mir_ordinary_map_domain_register_shutdown",
                crate::runtime::mimi_mir_ordinary_map_domain_register_shutdown as *const ()
                    as usize,
            ),
            (
                "mimi_str_clone",
                crate::runtime::mimi_str_clone as *const () as usize,
            ),
            (
                "mimi_string_free",
                crate::runtime::mimi_string_free as *const () as usize,
            ),
            (
                "mimi_runtime_abort",
                crate::runtime::mimi_runtime_abort as *const () as usize,
            ),
            (
                "mimi_mir_assert_no_live_managed_any_v2_lists",
                crate::runtime::mimi_mir_assert_no_live_managed_any_v2_lists as *const () as usize,
            ),
        ] {
            let declaration = generator.module.get_function(symbol).expect(symbol);
            assert_eq!(
                declaration.count_basic_blocks(),
                0,
                "{symbol} must remain the real runtime import"
            );
            engine.add_global_mapping(&declaration, address);
        }
        // MIR main has no parameters and returns canonical i32; its native
        // declaration must agree before crossing this unsafe JIT ABI.
        let native_main = generator.module.get_function("main").expect("native main");
        assert_eq!(native_main.count_params(), 0);
        assert_eq!(
            native_main.get_type().get_return_type(),
            Some(context.i32_type().into())
        );
        // SAFETY: The parameter and return-type assertions above verify the
        // emitted main's exact C ABI, and the engine outlives every call below.
        let native = unsafe {
            engine
                // SAFETY: This function pointer type matches the checked main
                // declaration above; no arguments or borrowed payloads cross it.
                .get_function::<unsafe extern "C" fn() -> i32>("main")
                .expect("native main has the verified no-argument i32 ABI")
        };
        crate::codegen::assert_execution_engine_linked(
            &engine,
            "calling ordinary Map owned-Get main",
        );

        // Each failure returns the canonical Err payload through the real
        // Result match, after both roots have been dropped. Ok selects 7,
        // Missing selects 1, and TypeMismatch selects 2, so only Err(3) can
        // satisfy this observation. Check key, stored String snapshot, and
        // canonical String allocation independently on the same MirProgram.
        for stage in 0..3 {
            let observed =
                with_owned_get_failure(stage, || reference.execute_with_output(&main, &[]))
                    .expect("reference capacity remains a recoverable Result");
            assert_eq!(
                observed.value,
                MirRuntimeValue::Int(3),
                "reference stage {stage}"
            );
            assert_eq!(observed.output, "", "reference stage {stage}");

            let observed = with_owned_get_failure(stage, || vm.run_value())
                .expect("AST-free bytecode capacity remains a recoverable Result");
            assert_eq!(observed, Value::Int(3), "bytecode stage {stage}");
            assert_eq!(vm.take_stdout(), "", "bytecode stage {stage}");

            // SAFETY: The JIT function has the verified no-argument i32 C ABI,
            // and its owning engine remains live throughout the failpoint call.
            let native_domain_before = crate::runtime::ordinary_map_domain_live_counts_for_test()
                .expect("ordinary-Map ownership registries are observable in tests");
            // SAFETY: `native` is the verified zero-argument i32 main entry;
            // its engine remains live and this closure passes no pointers.
            let observed = with_owned_get_failure(stage, || unsafe { native.call() });
            assert_eq!(observed, 3, "native stage {stage}");
            assert_eq!(
                crate::runtime::ordinary_map_domain_live_counts_for_test()
                    .expect("ordinary-Map ownership registries remain healthy"),
                native_domain_before,
                "native stage {stage} must drain roots, domain receipts, and AnyOwner tokens"
            );
            assert_eq!(
                program.canonical_digest(),
                digest,
                "a consumer mutated the shared MIR"
            );
        }

        // Reuse the same consumers after restoring the failpoint. A failure
        // must not poison Map ownership, subsequent String allocation, or
        // Result decoding; the successful owned snapshot crosses root drop.
        let observed = reference
            .execute_with_output(&main, &[])
            .expect("reference recovery");
        assert_eq!(observed.value, MirRuntimeValue::Int(7));
        assert_eq!(observed.output, "");
        assert_eq!(vm.run_value().expect("bytecode recovery"), Value::Int(7));
        assert_eq!(vm.take_stdout(), "");
        // SAFETY: The same checked main ABI and live engine apply after the
        // failpoint is reset; this invocation passes no pointers or arguments.
        let native_domain_before = crate::runtime::ordinary_map_domain_live_counts_for_test()
            .expect("ordinary-Map ownership registries are observable in tests");
        // SAFETY: the checked zero-argument i32 entry and its engine remain
        // live after resetting the failpoint; the call passes no pointers.
        assert_eq!(unsafe { native.call() }, 7, "native recovery");
        assert_eq!(
            crate::runtime::ordinary_map_domain_live_counts_for_test()
                .expect("ordinary-Map ownership registries remain healthy"),
            native_domain_before,
            "successful native owned-Get execution must return every tracked count to baseline"
        );
        assert_eq!(program.canonical_digest(), digest);
    }

    #[test]
    fn ordinary_map_owned_get_v1_emits_canonical_owned_results() {
        let program = canonical_program(
            r#"
func i32_or_status(value: Result<i32, i32>) -> i32 {
    match value {
        Ok(number) => number,
        Err(status) => -status,
    }
}

func print_string_or_status(value: Result<string, i32>) -> i32 {
    match value {
        Ok(text) => {
            println(text)
            7
        },
        Err(status) => -status,
    }
}

func main() -> i32 {
    let empty = map_new()
    let numbers = map_set(empty, "number", 42)
    let text = map_set(numbers, "text", "hello")
    let full = map_set(text, "empty", "")
    let removed = map_remove(full, "number")

    println(i32_or_status(map_get_i32(full, "number")))
    println(i32_or_status(map_get_i32(full, "text")))
    println(i32_or_status(map_get_i32(full, "missing")))
    println(print_string_or_status(map_get_string(full, "text")))
    println(print_string_or_status(map_get_string(full, "number")))
    println(print_string_or_status(map_get_string(full, "missing")))
    println(print_string_or_status(map_get_string(full, "empty")))
    println(i32_or_status(map_get_i32(removed, "number")))
    println(i32_or_status(map_get_i32(full, "number")))
    println(map_size(full))
    println(map_size(removed))
    println(has_key(full, "text"))
    println(has_key(removed, "number"))

    let snapshot_seed = map_new()
    let snapshot_root = map_set(snapshot_seed, "detached", "after-root-drop")
    let detached_snapshot = map_get_string(snapshot_root, "detached")
    drop(snapshot_seed)
    drop(snapshot_root)
    println(print_string_or_status(detached_snapshot))

    drop(empty)
    drop(numbers)
    drop(text)
    drop(full)
    drop(removed)
    0
}
"#,
        );
        crate::codegen::mir::validate_mir_native(&program)
            .expect("native admission accepts the owned-Get profile");

        let main = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute_with_output(&main, &[])
            .expect("reference owned-Get execution");
        assert_eq!(reference.value, MirRuntimeValue::Int(0));
        assert_eq!(
            reference.output,
            "42\n-2\n-1\nhello\n7\n-2\n-1\n\n7\n-1\n42\n3\n2\ntrue\nfalse\nafter-root-drop\n7\n"
        );

        let mut vm =
            BytecodeVM::new(compile_mir_program(&program).expect("AST-free owned-Get bytecode"));
        assert_eq!(
            vm.run_value().expect("bytecode owned-Get execution"),
            Value::Int(0)
        );
        assert_eq!(vm.take_stdout(), reference.output);

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_ordinary_map_owned_get_v1");
        generator
            .compile_mir_native(&program)
            .expect("receipt-backed owned-Get native lowering");
        generator
            .module
            .verify()
            .expect("owned-Get native module verifies");
        let native_ir = generator.module.print_to_string().to_string();
        for helper in [
            "_new",
            "_set_i32",
            "_set_string",
            "_remove",
            "_size",
            "_contains",
            "_get_i32",
            "_get_string",
            "_drop",
        ] {
            assert!(
                native_ir.contains(&format!(
                    "@mimi_mir_ordinary_map_owned_get_i32_string_v1{helper}"
                )),
                "native module does not contain owned-Get helper {helper}"
            );
        }
        // The reference, Bytecode VM, and linked native run above/below all
        // traverse both Ok and Err values and compare their selected payloads.
        // LLVM may fold aggregate insert names, so the emitted temporary name
        // is not a stable assertion for the canonical Result discriminant.
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("same MIR native owned-Get process");
        assert_eq!(native.exit_code, Some(0), "{}", native.stderr);
        assert_eq!(native.stdout, reference.output);
        assert_eq!(native.stderr, "");
    }
    const OWNER_ALIAS_SOURCE: &str = r#"
func alias_read_after_source_drop() -> i32 {
    ensures: result == 2
    let empty = map_new()
    let numbers = map_set(empty, "number", 42)
    let source = map_set(numbers, "text", "owned")
    let alias = source
    drop(source)
    map_size(alias)
}

func main() -> i32 {
    println(101)
    println(alias_read_after_source_drop())
    0
}
"#;

    const OWNED_GET_ALIAS_SOURCE: &str = r#"
func alias_read_after_source_drop() -> i32 {
    ensures: result == 2
    let empty = map_new()
    let numbers = map_set(empty, "number", 42)
    let source = map_set(numbers, "text", "owned")
    let alias = source
    drop(source)
    let snapshot = map_get_string(alias, "text")
    drop(snapshot)
    map_size(alias)
}

func main() -> i32 {
    println(101)
    println(alias_read_after_source_drop())
    0
}
"#;

    const OWNER_NUL_ALIAS_SOURCE: &str = r#"
func alias_read_after_source_drop() -> i32 {
    ensures: result == 1111
    let empty = map_new()
    let source = map_set(empty, "key\0🧪值", "owned")
    let alias = source
    drop(source)
    let overwritten = map_set(alias, "key\0🧪值", 42)
    let prefix_removed = map_remove(overwritten, "key")
    let without_nul_removed = map_remove(overwritten, "key🧪值")
    let removed = map_remove(overwritten, "key\0🧪值")
    map_size(alias) + 10 * map_size(overwritten) + 100 * map_size(prefix_removed) + 1000 * map_size(without_nul_removed) + 10000 * map_size(removed)
}

func main() -> i32 {
    println(101)
    println(alias_read_after_source_drop())
    0
}
"#;

    const OWNED_GET_NUL_ALIAS_SOURCE: &str = r#"
func alias_read_after_source_drop() -> i32 {
    ensures: result == 1111
    let empty = map_new()
    let source = map_set(empty, "key\0🧪值", "owned")
    let alias = source
    drop(source)
    let snapshot = map_get_string(alias, "key\0🧪值")
    drop(snapshot)
    let overwritten = map_set(alias, "key\0🧪值", 42)
    let prefix_removed = map_remove(overwritten, "key")
    let without_nul_removed = map_remove(overwritten, "key🧪值")
    let removed = map_remove(overwritten, "key\0🧪值")
    map_size(alias) + 10 * map_size(overwritten) + 100 * map_size(prefix_removed) + 1000 * map_size(without_nul_removed) + 10000 * map_size(removed)
}

func main() -> i32 {
    println(101)
    println(alias_read_after_source_drop())
    0
}
"#;

    #[test]
    fn ordinary_map_alias_same_mir_survives_source_drop_and_implicit_cleanup() {
        use crate::core::mir::MirInstructionKind as I;
        use crate::interp::bytecode::instr::Op;

        for (profile, source, clone_symbol, drop_count, output) in [
            (
                "owner-lifecycle",
                OWNER_ALIAS_SOURCE,
                "mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_clone",
                4,
                "101\n2\n",
            ),
            (
                "owned-get",
                OWNED_GET_ALIAS_SOURCE,
                "mimi_mir_ordinary_map_owned_get_i32_string_v1_clone",
                4,
                "101\n2\n",
            ),
            (
                "owner-lifecycle-nul",
                OWNER_NUL_ALIAS_SOURCE,
                "mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_clone",
                7,
                "101\n1111\n",
            ),
            (
                "owned-get-nul",
                OWNED_GET_NUL_ALIAS_SOURCE,
                "mimi_mir_ordinary_map_owned_get_i32_string_v1_clone",
                7,
                "101\n1111\n",
            ),
        ] {
            let program = canonical_program(source);
            let digest = program.canonical_digest();
            let main = crate::core::NodeId("function:main".into());
            let owner = crate::core::NodeId("function:alias_read_after_source_drop".into());
            let function = program
                .functions()
                .get(&owner)
                .expect("alias proof function");
            let owner_type = if profile.starts_with("owned-get") {
                crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id()
            } else {
                crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id()
            };
            let instructions = &function
                .blocks
                .values()
                .next()
                .expect("one MIR block")
                .instructions;
            assert_eq!(
                function.blocks.len(),
                1,
                "{profile} remains a closed straight-line profile"
            );
            let (clone_index, source_root, alias_root) = instructions
                .iter()
                .enumerate()
                .find_map(|(index, instruction)| match &instruction.kind {
                    I::Clone { result, source }
                        if function
                            .values
                            .get(result)
                            .is_some_and(|value| value.ty == owner_type) =>
                    {
                        Some((index, source, result))
                    }
                    _ => None,
                })
                .expect("the local alias must have an explicit MIR Clone");
            let source_drop_index = instructions
                .iter()
                .position(|instruction| match &instruction.kind {
                    I::OrdinaryMapOwnerLifecycleDrop { root }
                    | I::OrdinaryMapOwnedGetI32StringV1Drop { root } => root == source_root,
                    _ => false,
                })
                .expect("explicit source destruction");
            let alias_read_index = instructions
                .iter()
                .position(|instruction| match &instruction.kind {
                    I::OrdinaryMapOwnerLifecycleSize { root, .. }
                    | I::OrdinaryMapOwnedGetI32StringV1Size { root, .. } => root == alias_root,
                    _ => false,
                })
                .expect("alias read after source destruction");
            let alias_drop_index = instructions
                .iter()
                .position(|instruction| match &instruction.kind {
                    I::OrdinaryMapOwnerLifecycleDrop { root }
                    | I::OrdinaryMapOwnedGetI32StringV1Drop { root } => root == alias_root,
                    _ => false,
                })
                .expect("implicit alias scope cleanup");
            assert!(clone_index < source_drop_index && source_drop_index < alias_read_index);
            assert!(alias_read_index < alias_drop_index);
            assert_eq!(
                instructions
                    .iter()
                    .filter(|instruction| matches!(
                        &instruction.kind,
                        I::OrdinaryMapOwnerLifecycleDrop { .. }
                            | I::OrdinaryMapOwnedGetI32StringV1Drop { .. }
                    ))
                    .count(),
                drop_count,
                "{profile}: only source has an explicit Drop; all remaining roots settle implicitly"
            );

            let reference = MirReferenceInterpreter::new(&program)
                .execute_with_output(&main, &[])
                .expect("reference alias survives source drop");
            assert_eq!(reference.value, MirRuntimeValue::Int(0), "{profile}");
            assert_eq!(reference.output, output, "{profile}");

            let bytecode = compile_mir_program(&program).expect("same MIR AST-free alias bytecode");
            assert!(
                bytecode.ast.is_none(),
                "{profile} bytecode retains no surface AST"
            );
            assert!(
                bytecode
                    .functions
                    .iter()
                    .flat_map(|function| &function.code)
                    .any(|op| matches!(
                        op,
                        Op::MirOrdinaryMapOwnerLifecycleI32StringV1Clone { .. }
                            | Op::MirOrdinaryMapOwnedGetI32StringV1Clone { .. }
                    )),
                "{profile} requires the independent receipt-bound Clone opcode"
            );
            let mut vm = BytecodeVM::new(bytecode);
            assert_eq!(
                vm.run_value().expect("bytecode alias survives source drop"),
                Value::Int(0),
                "{profile}"
            );
            assert_eq!(vm.take_stdout(), reference.output, "{profile}");

            let verification =
                crate::verifier::verify_mir(&program, format!("ordinary-map-alias-{profile}"))
                    .expect("verifier consumes the same alias/cleanup MIR");
            let proof = verification
                .iter()
                .find(|result| result.func_name.ends_with("alias_read_after_source_drop"))
                .expect("alias size ensures proof");
            if profile.starts_with("owner-lifecycle") {
                let expected = "canonical MIR body can reach trap 'E0800' under requires";
                assert_eq!(
                    proof.status,
                    crate::verifier::VerifStatus::Disproven,
                    "{profile}: {proof:?}"
                );
                assert_eq!(
                    proof.message, expected,
                    "{profile}: unexpected failure reason"
                );
                let diagnostic = proof
                    .diagnostic
                    .as_ref()
                    .expect("reachable allocator E0800 has a source diagnostic");
                assert_eq!(diagnostic.message, expected, "{profile}");
                let map_new_call = program
                    .proof_source_census()
                    .owner(&owner)
                    .expect("alias callable's independent source census")
                    .body_calls()
                    .find(|call| call.callee_identity().contains("map_new"))
                    .expect("the exact source Map-root constructor call");
                let expected_call_suffix = map_new_call
                    .call_node()
                    .0
                    .rsplit_once("/node:")
                    .map(|(_, suffix)| format!("/node:{suffix}"))
                    .expect("source call identity has a canonical expression node");
                let diagnostic_call = diagnostic
                    .origin
                    .as_ref()
                    .and_then(|origin| origin.parent_node_id.as_deref())
                    .expect("E0800 diagnostic records its source call identity");
                assert!(
                    diagnostic_call.ends_with(&expected_call_suffix),
                    "{profile}: E0800 must identify map_new at {}, got {diagnostic_call}",
                    map_new_call.call_node().0
                );
                assert_eq!(
                    proof.trusted_subset_domain,
                    Some(crate::verifier::TrustedSubsetDomain::Body),
                    "{profile}: a reachable runtime trap is a body failure"
                );
                let artifact = proof
                    .artifact
                    .as_ref()
                    .expect("definitive E0800 result retains MIR provenance");
                assert_eq!(artifact.engine, crate::verifier::ProofArtifact::ENGINE_MIR);
                assert_eq!(artifact.mir_hash, digest, "{profile}: artifact MIR digest");
            } else {
                assert_eq!(
                    proof.status,
                    crate::verifier::VerifStatus::Proven,
                    "{profile}: {}",
                    proof.message
                );
                assert_eq!(
                    proof.message, "canonical MIR ensures contract proven",
                    "{profile}: unexpected proof reason"
                );
                let artifact = proof
                    .artifact
                    .as_ref()
                    .expect("successful alias Ensures proof retains MIR provenance");
                assert_eq!(artifact.engine, crate::verifier::ProofArtifact::ENGINE_MIR);
                assert_eq!(artifact.mir_hash, digest, "{profile}: artifact MIR digest");
            }

            let context = Context::create();
            let mut generator = CodeGenerator::new(&context, "mir_ordinary_map_alias_same_program");
            generator
                .compile_mir_native(&program)
                .expect("same MIR native alias lowering");
            generator
                .module
                .verify()
                .expect("native alias module verifies");
            let ir = generator.module.print_to_string().to_string();
            assert!(
                ir.contains(&format!("call i64 @{clone_symbol}")),
                "{profile}: native alias has its fresh-root ABI"
            );
            let native = crate::tests::link_and_observe_canonical_mir(&generator)
                .expect("same MIR linked native alias process");
            assert_eq!(native.exit_code, Some(0), "{profile}: {}", native.stderr);
            assert_eq!(native.stdout, reference.output, "{profile}");
            assert_eq!(native.stderr, "", "{profile}");
            assert_eq!(
                program.canonical_digest(),
                digest,
                "{profile}: consumer mutated shared MIR"
            );
        }
    }

    #[test]
    fn ordinary_map_alias_clone_receipt_preflight_rejects_missing_and_wrong_sources() {
        use crate::core::mir::MirInstructionKind as I;

        for (profile, source) in [
            ("owner-lifecycle", OWNER_ALIAS_SOURCE),
            ("owned-get", OWNED_GET_ALIAS_SOURCE),
        ] {
            for missing in [true, false] {
                let mut program = canonical_program(source);
                let owner = crate::core::NodeId("function:alias_read_after_source_drop".into());
                let mut function = program
                    .functions()
                    .get(&owner)
                    .expect("alias proof function")
                    .clone();
                let wrong_source = function
                    .blocks
                    .values()
                    .flat_map(|block| &block.instructions)
                    .find_map(|instruction| match &instruction.kind {
                        I::OrdinaryMapOwnerLifecycleNew { result }
                        | I::OrdinaryMapOwnedGetI32StringV1New { result } => Some(result.clone()),
                        _ => None,
                    })
                    .expect("a distinct live root of the same TypeDesc");
                let owner_type = &function.values.get(&wrong_source).expect("root value").ty;
                let clone_result = function
                    .blocks
                    .values()
                    .flat_map(|block| &block.instructions)
                    .find_map(|instruction| match &instruction.kind {
                        I::Clone { result, .. }
                            if function
                                .values
                                .get(result)
                                .is_some_and(|value| &value.ty == owner_type) =>
                        {
                            Some(result.clone())
                        }
                        _ => None,
                    })
                    .expect("ordinary Map Clone result");
                if missing {
                    function
                        .ownership
                        .clear_checker_ordinary_map_clone_actions_for_test_only();
                } else {
                    let cloned_source = function
                        .blocks
                        .values_mut()
                        .flat_map(|block| &mut block.instructions)
                        .find_map(|instruction| match &mut instruction.kind {
                            I::Clone { result, source } if *result == clone_result => Some(source),
                            _ => None,
                        })
                        .expect("MIR Clone source");
                    assert_ne!(*cloned_source, wrong_source);
                    *cloned_source = wrong_source;
                }
                let reason = if missing {
                    "only Clone receipt removed"
                } else {
                    "Clone source disagrees with retained receipt"
                };
                program.replace_function_for_test_only(function);

                let constructor_errors = MirProgram::with_type_catalog(
                    program.functions().clone(),
                    program.type_catalog().clone(),
                )
                .expect_err("reference program boundary must reject forged Clone evidence");
                assert!(
                    constructor_errors
                        .iter()
                        .any(|error| error.message.contains("receipt")),
                    "{profile} {reason}: {constructor_errors:#?}"
                );

                let reference = MirReferenceInterpreter::new(&program);
                let reference_error = reference
                    .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
                    .expect_err(
                        "reference execution must recheck Clone receipts before main effects",
                    );
                assert!(
                    reference_error.message.contains("receipt"),
                    "{profile} {reason}: {reference_error:?}"
                );
                assert_eq!(
                    reference.captured_output(),
                    "",
                    "{profile} {reason}: reference executed before preflight"
                );

                let bytecode_errors = compile_mir_program(&program)
                    .expect_err("bytecode adapter must reject forged Clone evidence");
                assert!(
                    bytecode_errors
                        .iter()
                        .any(|error| error.message.contains("receipt")),
                    "{profile} {reason}: {bytecode_errors:#?}"
                );

                let context = Context::create();
                let mut generator =
                    CodeGenerator::new(&context, "mir_ordinary_map_alias_forged_receipt");
                let native_error = generator
                    .compile_mir_native(&program)
                    .expect_err("native preflight must reject forged Clone evidence");
                assert!(
                    native_error
                        .iter()
                        .any(|diagnostic| diagnostic.message.contains("receipt")),
                    "{profile} {reason}: {native_error:?}"
                );
                assert!(
                    generator.module.get_function("main").is_none(),
                    "{profile} {reason}: native began callable emission"
                );

                let verifier_results = crate::verifier::verify_mir(
                    &program,
                    format!("ordinary-map-alias-forgery-{profile}"),
                )
                .expect("public verifier reports unsupported forged Clone evidence");
                assert!(
                    verifier_results.iter().any(|result| {
                        result.func_name == "function:alias_read_after_source_drop"
                    }),
                    "{profile} {reason}: missing refusal for the contracted alias function"
                );
                for result in &verifier_results {
                    assert_eq!(
                        result.status,
                        crate::verifier::VerifStatus::NotInTrustedSubset,
                        "{profile} {reason}: {}: {}",
                        result.func_name,
                        result.message
                    );
                    assert!(
                        result.message.contains("receipt"),
                        "{profile} {reason}: {}: {}",
                        result.func_name,
                        result.message
                    );
                    assert_eq!(
                        result.constraint_count, 0,
                        "{profile} {reason}: verifier began a solver proof"
                    );
                    assert!(
                        result.artifact.is_none(),
                        "{profile} {reason}: refusal produced a proof artifact"
                    );
                }
            }
        }
    }

    #[test]
    fn ordinary_map_alias_bytecode_clone_tampering_rejects_before_output() {
        use crate::interp::bytecode::instr::Op;
        use std::sync::Arc;

        for (profile, source) in [
            ("owner-lifecycle", OWNER_ALIAS_SOURCE),
            ("owned-get", OWNED_GET_ALIAS_SOURCE),
        ] {
            let program = canonical_program(source);
            let bytecode = compile_mir_program(&program).expect("canonical alias bytecode");
            let mut forged = (*bytecode).clone();
            let function = forged
                .functions
                .iter_mut()
                .find(|function| {
                    function.code.iter().any(|op| {
                        matches!(
                            op,
                            Op::MirOrdinaryMapOwnerLifecycleI32StringV1Clone { .. }
                                | Op::MirOrdinaryMapOwnedGetI32StringV1Clone { .. }
                        )
                    })
                })
                .expect("function with dedicated Clone opcode");
            let wrong_source = function
                .code
                .iter()
                .find_map(|op| match op {
                    Op::MirOrdinaryMapOwnerLifecycleI32StringV1New { rd }
                    | Op::MirOrdinaryMapOwnedGetI32StringV1New { rd } => Some(*rd),
                    _ => None,
                })
                .expect("same-profile live source register");
            let source_register = function
                .code
                .iter_mut()
                .find_map(|op| match op {
                    Op::MirOrdinaryMapOwnerLifecycleI32StringV1Clone { ra, .. }
                    | Op::MirOrdinaryMapOwnedGetI32StringV1Clone { ra, .. } => Some(ra),
                    _ => None,
                })
                .expect("dedicated Clone source register");
            assert_ne!(*source_register, wrong_source);
            *source_register = wrong_source;
            let mut vm = BytecodeVM::new(Arc::new(forged));
            let error = vm
                .run_value()
                .expect_err("changed Clone operands invalidate the private snapshot");
            assert!(
                error.message().contains("receipt snapshot"),
                "{profile}: {}",
                error.message()
            );
            assert_eq!(
                vm.take_stdout(),
                "",
                "{profile}: snapshot rejection must precede main output"
            );

            let mut missing = (*bytecode).clone();
            missing.canonical_map_any_owner_v1_receipt = None;
            let mut vm = BytecodeVM::new(Arc::new(missing));
            let error = vm
                .run_value()
                .expect_err("dedicated Clone cannot run without its private profile snapshot");
            assert!(
                error.message().contains("independent Checker receipt"),
                "{profile}: {}",
                error.message()
            );
            assert_eq!(
                vm.take_stdout(),
                "",
                "{profile}: receipt rejection must precede main output"
            );
        }
    }

    #[test]
    fn ordinary_map_alias_branch_shape_remains_closed() {
        for getter in [
            "",
            "let snapshot = map_get_string(alias, \"missing\")\n    drop(snapshot)",
        ] {
            let source = format!(
                r#"
func forked(flag: bool) -> i32 {{
    let source = map_new()
    let alias = source
    {getter}
    if flag {{
        let left = map_set(alias, "left", 1)
        drop(left)
    }} else {{
        let right = map_set(alias, "right", 2)
        drop(right)
    }}
    drop(source)
    map_size(alias)
}}
func main() -> i32 {{ forked(true) }}
"#
            );
            let tokens = Lexer::new(&source)
                .tokenize()
                .expect("lex branch alias source");
            let file = Parser::new(tokens)
                .parse_file()
                .expect("parse branch alias source");
            let checked =
                crate::core::check_program(&file).expect("free Map branch program checks");
            let admission = crate::core::mir::classify_canonical_mir_route_admission(&checked);
            assert!(!admission.ordinary_map_owner_lifecycle_i32_string_v1_complete());
            assert!(!admission.ordinary_map_owned_get_i32_string_v1_complete());
            assert!(
                MirProgram::from_checked_program(&checked).is_err(),
                "aliases must not reopen the branch-shaped Map profile"
            );
        }
    }
}
