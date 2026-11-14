//! Native consumer for the independent persistent ordinary-Map owner profile.
//!
//! This identity has its own runtime ABI and TypeDesc. Set and Remove borrow
//! their source handle and must return a distinct owner handle; only the
//! explicit MIR Drop instruction releases one of these roots.

use super::*;
use crate::core::mir::types::MirGlueOperation;

impl<'a, 'ctx> NativeMirFunctionEmitter<'a, 'ctx> {
    fn validate_ordinary_map_root(
        &self,
        value: &MirValueId,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let ty = self.value_type(value, subject)?;
        if ty != crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id() {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map owner operand does not use the canonical lifecycle v1 TypeDesc",
            ));
        }
        self.program
            .type_catalog()
            .validate_glue(&ty, MirGlueOperation::MoveOut)
            .map_err(|message| NativeMirError::new(subject, message))
    }

    fn emit_ordinary_map_root_guard(
        &mut self,
        condition: inkwell::values::IntValue<'ctx>,
        message: &str,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let valid = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_ordinary_map_valid");
        let invalid = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_ordinary_map_invalid");
        self.generator
            .builder
            .build_conditional_branch(condition, valid, invalid)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(invalid);
        self.emit_abort_with_message(message, subject)?;
        self.generator.builder.position_at_end(valid);
        Ok(())
    }

    fn emit_live_ordinary_map_root(
        &mut self,
        value: &MirValueId,
        subject: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, NativeMirError> {
        self.validate_ordinary_map_root(value, subject)?;
        let handle = self.value(value, subject)?.into_int_value();
        let nonzero = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                handle,
                self.generator.context.i64_type().const_zero(),
                "mir_ordinary_map_root_nonzero",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_ordinary_map_root_guard(
            nonzero,
            "[E0800] ordinary Map lifecycle root is invalid",
            subject,
        )?;
        Ok(handle)
    }

    fn ordinary_map_string_parts(
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
                format!("ordinary Map {role} is not the canonical String TypeDesc"),
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
            .build_extract_value(string, 0, "mir_ordinary_map_string_data")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_pointer_value();
        let len = self
            .generator
            .builder
            .build_extract_value(string, 1, "mir_ordinary_map_string_len")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        Ok((data, len))
    }

    fn ordinary_map_i32_type(
        &self,
        subject: &str,
    ) -> Result<crate::core::ResolvedTypeId, NativeMirError> {
        self.program
            .type_catalog()
            .iter()
            .find_map(|(ty, descriptor)| {
                (descriptor.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::I32))
                    .then(|| ty.clone())
            })
            .ok_or_else(|| NativeMirError::new(subject, "canonical i32 TypeDesc is absent"))
    }

    fn ordinary_map_bool_type(
        &self,
        subject: &str,
    ) -> Result<crate::core::ResolvedTypeId, NativeMirError> {
        self.program
            .type_catalog()
            .iter()
            .find_map(|(ty, descriptor)| {
                (descriptor.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::Bool))
                    .then(|| ty.clone())
            })
            .ok_or_else(|| NativeMirError::new(subject, "canonical bool TypeDesc is absent"))
    }

    fn emit_ordinary_map_result_root(
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
                "mir_ordinary_map_result_nonzero",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_ordinary_map_root_guard(
            nonzero,
            "[E0800] ordinary Map lifecycle allocation failed",
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
                "mir_ordinary_map_result_distinct",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_ordinary_map_root_guard(
            distinct,
            &format!(
                "[E0800] ordinary Map lifecycle persistent {operation} aliased its source root"
            ),
            subject,
        )?;
        Ok(handle.into())
    }

    pub(super) fn emit_ordinary_map_new(
        &mut self,
        result: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        self.validate_ordinary_map_root(result, subject)?;
        let new_fn = self
            .generator
            .get_runtime_fn("mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_new")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let handle = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(new_fn, &[], "mir_ordinary_map_new")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "ordinary Map New returned void"))?
        .into_int_value();
        let nonzero = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                handle,
                self.generator.context.i64_type().const_zero(),
                "mir_ordinary_map_new_nonzero",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_ordinary_map_root_guard(
            nonzero,
            "[E0800] ordinary Map lifecycle allocation failed",
            subject,
        )?;
        Ok(handle.into())
    }

    pub(super) fn emit_ordinary_map_clone(
        &mut self,
        result: &MirValueId,
        source: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        self.validate_ordinary_map_root(result, subject)?;
        let source_handle = self.emit_live_ordinary_map_root(source, subject)?;
        let clone_fn = self
            .generator
            .get_runtime_fn("mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_clone")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let handle = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(clone_fn, &[source_handle.into()], "mir_ordinary_map_clone")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "ordinary Map lifecycle Clone returned void"))?
        .into_int_value();
        self.emit_ordinary_map_result_root(handle, source, subject, "Clone")
    }

    pub(super) fn emit_ordinary_map_set(
        &mut self,
        result: &MirValueId,
        source: &MirValueId,
        key: &MirValueId,
        value: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        self.validate_ordinary_map_root(result, subject)?;
        let source_handle = self.emit_live_ordinary_map_root(source, subject)?;
        let (key_data, key_len) = self.ordinary_map_string_parts(key, "Set key", subject)?;
        let value_ty = self.value_type(value, subject)?;
        let descriptor = self.program.type_catalog().get(&value_ty).ok_or_else(|| {
            NativeMirError::new(subject, "ordinary Map Set value TypeDesc is absent")
        })?;
        let is_i32 = matches!(
            &descriptor.kind,
            MirTypeKind::Primitive(crate::core::PrimitiveType::I32)
        );
        let is_string = matches!(
            &descriptor.kind,
            MirTypeKind::Primitive(crate::core::PrimitiveType::String)
        );
        let set_name = if is_i32 {
            self.program
                .type_catalog()
                .validate_copy_scalar(&value_ty)
                .map_err(|message| NativeMirError::new(subject, message))?;
            "mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_set_i32"
        } else if is_string {
            self.program
                .type_catalog()
                .validate_owned_string(&value_ty)
                .map_err(|message| NativeMirError::new(subject, message))?;
            "mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_set_string"
        } else {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map Set payload is outside the i32/String lifecycle profile",
            ));
        };
        let mut args = vec![
            BasicMetadataValueEnum::from(source_handle),
            BasicMetadataValueEnum::from(key_data),
            BasicMetadataValueEnum::from(key_len),
        ];
        if is_i32 {
            args.push(BasicMetadataValueEnum::from(
                self.value(value, subject)?.into_int_value(),
            ));
        } else {
            let (value_data, value_len) =
                self.ordinary_map_string_parts(value, "Set value", subject)?;
            args.push(BasicMetadataValueEnum::from(value_data));
            args.push(BasicMetadataValueEnum::from(value_len));
        }
        let set_fn = self
            .generator
            .get_runtime_fn(set_name)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let updated = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(set_fn, &args, "mir_ordinary_map_set")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "ordinary Map Set returned void"))?
        .into_int_value();
        self.emit_ordinary_map_result_root(updated, source, subject, "Set")
    }

    pub(super) fn emit_ordinary_map_remove(
        &mut self,
        result: &MirValueId,
        source: &MirValueId,
        key: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        self.validate_ordinary_map_root(result, subject)?;
        let source_handle = self.emit_live_ordinary_map_root(source, subject)?;
        let (key_data, key_len) = self.ordinary_map_string_parts(key, "Remove key", subject)?;
        let remove_fn = self
            .generator
            .get_runtime_fn("mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_remove")
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
                    "mir_ordinary_map_remove",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "ordinary Map Remove returned void"))?
        .into_int_value();
        self.emit_ordinary_map_result_root(removed, source, subject, "Remove")
    }

    pub(super) fn emit_ordinary_map_size(
        &mut self,
        result: &MirValueId,
        root: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        self.emit_live_ordinary_map_root(root, subject)?;
        let i32_ty = self.ordinary_map_i32_type(subject)?;
        if self.value_type(result, subject)? != i32_ty {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map Size result is not the canonical i32 TypeDesc",
            ));
        }
        self.program
            .type_catalog()
            .validate_copy_scalar(&i32_ty)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let out_size = self
            .generator
            .builder
            .build_alloca(
                self.generator.context.i32_type(),
                "mir_ordinary_map_size_out",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let size_fn = self
            .generator
            .get_runtime_fn("mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_size_status")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let status = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    size_fn,
                    &[
                        BasicMetadataValueEnum::from(self.value(root, subject)?.into_int_value()),
                        BasicMetadataValueEnum::from(out_size),
                    ],
                    "mir_ordinary_map_size_status",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "ordinary Map Size status returned void"))?
        .into_int_value();

        let success = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.generator.context.i32_type().const_zero(),
                "mir_ordinary_map_size_status_success",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let success_block = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_ordinary_map_size_success");
        let classify_block = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_ordinary_map_size_classify_failure");
        self.generator
            .builder
            .build_conditional_branch(success, success_block, classify_block)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        self.generator.builder.position_at_end(classify_block);
        let overflow = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.generator.context.i32_type().const_int(1, false),
                "mir_ordinary_map_size_status_overflow",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let overflow_block = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_ordinary_map_size_overflow");
        let failure_block = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_ordinary_map_size_failure");
        self.generator
            .builder
            .build_conditional_branch(overflow, overflow_block, failure_block)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        self.generator.builder.position_at_end(overflow_block);
        self.emit_abort_with_message(
            "[E0802] ordinary Map owner lifecycle v1 size overflows i32",
            subject,
        )?;
        self.generator.builder.position_at_end(failure_block);
        self.emit_abort_with_message("[E0800] ordinary Map lifecycle Size failed", subject)?;

        // The runtime writes this out parameter only for status=success. Keep
        // the load in the success block so failure statuses never expose an
        // uninitialized output value.
        self.generator.builder.position_at_end(success_block);
        let size = self
            .generator
            .builder
            .build_load(
                self.generator.context.i32_type(),
                out_size,
                "mir_ordinary_map_size",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        Ok(size.into())
    }

    pub(super) fn emit_ordinary_map_contains(
        &mut self,
        result: &MirValueId,
        root: &MirValueId,
        key: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        self.emit_live_ordinary_map_root(root, subject)?;
        let bool_ty = self.ordinary_map_bool_type(subject)?;
        if self.value_type(result, subject)? != bool_ty {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map Contains result is not the canonical bool TypeDesc",
            ));
        }
        self.program
            .type_catalog()
            .validate_copy_scalar(&bool_ty)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let (key_data, key_len) = self.ordinary_map_string_parts(key, "Contains key", subject)?;
        let contains_fn = self
            .generator
            .get_runtime_fn("mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_contains")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let contains = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    contains_fn,
                    &[
                        BasicMetadataValueEnum::from(self.value(root, subject)?.into_int_value()),
                        BasicMetadataValueEnum::from(key_data),
                        BasicMetadataValueEnum::from(key_len),
                    ],
                    "mir_ordinary_map_contains",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "ordinary Map Contains returned void"))?
        .into_int_value();
        let valid = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::ULE,
                contains,
                self.generator.context.i32_type().const_int(1, false),
                "mir_ordinary_map_contains_valid",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_ordinary_map_root_guard(
            valid,
            "[E0800] ordinary Map lifecycle Contains failed",
            subject,
        )?;
        let boolean = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                contains,
                self.generator.context.i32_type().const_zero(),
                "mir_ordinary_map_contains_bool",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        Ok(boolean.into())
    }

    fn ordinary_map_observer_callsite_is_valid(
        callsite: &crate::core::NodeId,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        if callsite.0.trim().is_empty() {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map observer has an empty Checker callsite identity",
            ));
        }
        Ok(())
    }

    fn emit_ordinary_map_observer_json_buffer(
        &mut self,
        root: &MirValueId,
        subject: &str,
    ) -> Result<inkwell::values::PointerValue<'ctx>, NativeMirError> {
        let root_handle = self.emit_live_ordinary_map_root(root, subject)?;
        let to_json = self
            .generator
            .get_runtime_fn("mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_to_json")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let json = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    to_json,
                    &[root_handle.into()],
                    "mir_ordinary_map_observer_json",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| {
            NativeMirError::new(subject, "ordinary Map lifecycle JSON helper returned void")
        })?
        .into_pointer_value();
        let valid = self
            .generator
            .builder
            .build_is_not_null(json, "mir_ordinary_map_observer_json_nonnull")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_ordinary_map_root_guard(
            valid,
            "[E0800] ordinary Map lifecycle JSON observation failed",
            subject,
        )?;
        Ok(json)
    }

    fn ordinary_map_observer_json_length(
        &mut self,
        json: inkwell::values::PointerValue<'ctx>,
        subject: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, NativeMirError> {
        // The runtime helper returns a detached NUL-terminated C allocation.
        // JSON string escaping guarantees the payload contains no raw NUL, so
        // strlen recovers the exact UTF-8 byte length for Mimi's fat String ABI.
        let strlen = self
            .generator
            .get_runtime_fn("strlen")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(strlen, &[json.into()], "mir_ordinary_map_observer_json_len")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .map(BasicValueEnum::into_int_value)
        .ok_or_else(|| NativeMirError::new(subject, "strlen returned void for Map JSON"))
    }

    pub(super) fn emit_ordinary_map_observer_to_json(
        &mut self,
        result: &MirValueId,
        root: &MirValueId,
        callsite: &crate::core::NodeId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        Self::ordinary_map_observer_callsite_is_valid(callsite, subject)?;
        let result_type = self.value_type(result, subject)?;
        let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        if result_type != string_type {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map to_json result is not the canonical owned String TypeDesc",
            ));
        }
        self.program
            .type_catalog()
            .validate_owned_string(&result_type)
            .map_err(|message| NativeMirError::new(subject, message))?;

        let json = self.emit_ordinary_map_observer_json_buffer(root, subject)?;
        let length = self.ordinary_map_observer_json_length(json, subject)?;
        let handle = self
            .generator
            .builder
            .build_ptr_to_int(
                json,
                self.generator.context.i64_type(),
                "mir_ordinary_map_observer_json_handle",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        // This adopts the runtime allocation as a normal Mimi String: it
        // registers the pointer with the ordinary Map String owner domain,
        // and subsequent canonical String Drop frees it exactly once.
        self.emit_owned_string_from_handle(handle, length, subject)
    }

    pub(super) fn emit_ordinary_map_observer_println(
        &mut self,
        result: &MirValueId,
        root: &MirValueId,
        callsite: &crate::core::NodeId,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        Self::ordinary_map_observer_callsite_is_valid(callsite, subject)?;
        let unit_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Unit);
        let result_type = self.value_type(result, subject)?;
        if result_type != unit_type
            || !self
                .program
                .type_catalog()
                .get(&result_type)
                .is_some_and(MirTypeDesc::is_canonical_ffi_unit)
        {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map println result is not the canonical Unit TypeDesc",
            ));
        }

        // This is a borrowed observer. The JSON allocation remains a local
        // scratch buffer and is freed here after writing the exact bytes and
        // newline; the Map root is neither cloned nor consumed.
        let json = self.emit_ordinary_map_observer_json_buffer(root, subject)?;
        let length = self.ordinary_map_observer_json_length(json, subject)?;
        let print_bytes = self
            .generator
            .get_runtime_fn("mimi_print_bytes")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(
                print_bytes,
                &[json.into(), length.into()],
                "mir_ordinary_map_observer_print_bytes",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let newline = self
            .generator
            .builder
            .build_global_string_ptr("\n", "mir_ordinary_map_observer_newline")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(
                print_bytes,
                &[
                    newline.as_pointer_value().into(),
                    self.generator.context.i64_type().const_int(1, false).into(),
                ],
                "mir_ordinary_map_observer_print_newline",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let free = self
            .generator
            .get_runtime_fn("mimi_string_free")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(
                free,
                &[json.into()],
                "mir_ordinary_map_observer_scratch_free",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        // Unit has no physical LLVM value. The MIR result remains only in the
        // ownership/type ledger, matching other effectful native instructions.
        Ok(())
    }

    pub(super) fn emit_ordinary_map_drop(
        &mut self,
        root: &MirValueId,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let handle = self.emit_live_ordinary_map_root(root, subject)?;
        let ty = self.value_type(root, subject)?;
        self.program
            .type_catalog()
            .validate_glue(&ty, MirGlueOperation::Drop)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let drop_fn = self
            .generator
            .get_runtime_fn("mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_drop")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(
                drop_fn,
                &[BasicMetadataValueEnum::from(handle)],
                "mir_ordinary_map_drop",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        Ok(())
    }
}
