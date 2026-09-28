//! Dedicated native operations for the Checker-receipted MapRoot profile.

use super::*;
use crate::core::mir::types::MirGlueOperation;

impl<'a, 'ctx> NativeMirFunctionEmitter<'a, 'ctx> {
    fn validate_map_root_value(
        &self,
        value: &MirValueId,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let ty = self.value_type(value, subject)?;
        if ty != crate::core::mir::types::map_root_type_id() {
            return Err(NativeMirError::new(
                subject,
                "MapRoot operand does not use the canonical MapRoot TypeDesc",
            ));
        }
        self.program
            .type_catalog()
            .validate_glue(&ty, MirGlueOperation::MoveOut)
            .map_err(|message| NativeMirError::new(subject, message))
    }

    fn emit_root_guard(
        &mut self,
        condition: inkwell::values::IntValue<'ctx>,
        message: &str,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let valid = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_map_root_valid");
        let invalid = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_map_root_invalid");
        self.generator
            .builder
            .build_conditional_branch(condition, valid, invalid)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(invalid);
        self.emit_abort_with_message(message, subject)?;
        self.generator.builder.position_at_end(valid);
        Ok(())
    }

    fn emit_live_root_handle(
        &mut self,
        value: &MirValueId,
        subject: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, NativeMirError> {
        self.validate_map_root_value(value, subject)?;
        let handle = self.value(value, subject)?.into_int_value();
        let nonzero = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                handle,
                self.generator.context.i64_type().const_zero(),
                "mir_map_root_handle_nonzero",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_root_guard(
            nonzero,
            "[E0800] canonical MIR MapRoot handle is invalid",
            subject,
        )?;
        Ok(handle)
    }

    pub(super) fn emit_map_root_new(
        &mut self,
        result: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        self.validate_map_root_value(result, subject)?;
        let new_fn = self
            .generator
            .get_runtime_fn("mimi_mir_map_root_new")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let handle = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(new_fn, &[], "mir_map_root_new")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "MapRoot New returned void"))?
        .into_int_value();
        let nonzero = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                handle,
                self.generator.context.i64_type().const_zero(),
                "mir_map_root_new_nonzero",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_root_guard(
            nonzero,
            "[E0800] canonical MIR MapRoot allocation failed",
            subject,
        )?;
        Ok(handle.into())
    }

    pub(super) fn emit_map_root_set(
        &mut self,
        result: &MirValueId,
        source: &MirValueId,
        key: &str,
        value: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        self.validate_map_root_value(result, subject)?;
        let handle = self.emit_live_root_handle(source, subject)?;
        if key.contains('\0') {
            return Err(NativeMirError::new(subject, "MapRoot key contains NUL"));
        }
        let value_ty = self.value_type(value, subject)?;
        let descriptor =
            self.program.type_catalog().get(&value_ty).ok_or_else(|| {
                NativeMirError::new(subject, "MapRoot Set value TypeDesc is absent")
            })?;
        let key_len = i64::try_from(key.len()).map_err(|_| {
            NativeMirError::new(
                subject,
                "MapRoot static key exceeds the native i64 length ABI",
            )
        })?;
        let key_ptr = self
            .generator
            .builder
            .build_global_string_ptr(key, "mir_map_root_static_key")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let key_len = self
            .generator
            .context
            .i64_type()
            .const_int(key_len as u64, true);
        let value = self.value(value, subject)?;
        let (set_name, args) = match descriptor.kind {
            MirTypeKind::Primitive(crate::core::PrimitiveType::I32) => {
                self.program
                    .type_catalog()
                    .validate_copy_scalar(&value_ty)
                    .map_err(|message| NativeMirError::new(subject, message))?;
                (
                    "mimi_mir_map_root_set",
                    vec![
                        BasicMetadataValueEnum::from(handle),
                        BasicMetadataValueEnum::from(key_ptr.as_pointer_value()),
                        BasicMetadataValueEnum::from(key_len),
                        BasicMetadataValueEnum::from(value.into_int_value()),
                    ],
                )
            }
            MirTypeKind::Primitive(crate::core::PrimitiveType::String) => {
                self.program
                    .type_catalog()
                    .validate_owned_string(&value_ty)
                    .map_err(|message| NativeMirError::new(subject, message))?;
                let string = value.into_struct_value();
                let data = self
                    .generator
                    .builder
                    .build_extract_value(string, 0, "mir_map_root_string_data")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                    .into_pointer_value();
                let len = self
                    .generator
                    .builder
                    .build_extract_value(string, 1, "mir_map_root_string_len")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                    .into_int_value();
                let args = vec![
                    BasicMetadataValueEnum::from(handle),
                    BasicMetadataValueEnum::from(key_ptr.as_pointer_value()),
                    BasicMetadataValueEnum::from(key_len),
                    BasicMetadataValueEnum::from(data),
                    BasicMetadataValueEnum::from(len),
                ];
                ("mimi_mir_map_root_set_string", args)
            }
            _ => {
                return Err(NativeMirError::new(
                    subject,
                    "MapRoot Set value is outside the receipted i32/String TypeDesc profile",
                ));
            }
        };
        let set_fn = self
            .generator
            .get_runtime_fn(set_name)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let updated = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(set_fn, &args, "mir_map_root_set")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "MapRoot Set returned void"))?
        .into_int_value();
        let preserved_identity = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                updated,
                handle,
                "mir_map_root_set_identity",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_root_guard(
            preserved_identity,
            "[E0800] canonical MIR MapRoot Set changed the consumed root identity",
            subject,
        )?;
        Ok(updated.into())
    }

    pub(super) fn emit_map_root_size(
        &mut self,
        result: &MirValueId,
        root: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        self.emit_live_root_handle(root, subject)?;
        let result_ty = self.value_type(result, subject)?;
        let i32_ty = self
            .program
            .type_catalog()
            .iter()
            .find_map(|(ty, descriptor)| {
                (descriptor.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::I32))
                    .then(|| ty.clone())
            })
            .ok_or_else(|| NativeMirError::new(subject, "canonical i32 TypeDesc is absent"))?;
        if result_ty != i32_ty {
            return Err(NativeMirError::new(
                subject,
                "MapRoot Size result is not the canonical i32 TypeDesc",
            ));
        }
        self.program
            .type_catalog()
            .validate_copy_scalar(&i32_ty)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let size_fn = self
            .generator
            .get_runtime_fn("mimi_mir_map_root_size")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let size = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    size_fn,
                    &[BasicMetadataValueEnum::from(
                        self.value(root, subject)?.into_int_value(),
                    )],
                    "mir_map_root_size",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "MapRoot Size returned void"))?;
        Ok(size)
    }

    pub(super) fn emit_map_root_drop(
        &mut self,
        root: &MirValueId,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let handle = self.emit_live_root_handle(root, subject)?;
        let ty = self.value_type(root, subject)?;
        self.program
            .type_catalog()
            .validate_glue(&ty, MirGlueOperation::Drop)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let drop_fn = self
            .generator
            .get_runtime_fn("mimi_mir_map_root_drop")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(
                drop_fn,
                &[BasicMetadataValueEnum::from(handle)],
                "mir_map_root_drop",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        Ok(())
    }
}
