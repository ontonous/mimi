//! ABI-v3 String List construction. Each slot owns a detached `{ptr,len}` box.

use super::*;
use inkwell::values::{IntValue, PointerValue};

impl<'a, 'ctx> NativeMirFunctionEmitter<'a, 'ctx> {
    pub(super) fn emit_owned_string_list_construct(
        &mut self,
        elements: &[MirValueId],
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let new_fn = self
            .generator
            .get_runtime_fn("mimi_mir_list_new_string")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let list = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(new_fn, &[], "mir_string_list_new")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "owned String List constructor returned void"))?
        .into_pointer_value();
        let failed = self
            .generator
            .builder
            .build_is_null(list, "mir_string_list_new_failed")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_owned_string_list_abort_if(failed, None, elements, subject)?;
        let push_fn = self
            .generator
            .get_runtime_fn("mimi_mir_list_push_string")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        for (index, element) in elements.iter().enumerate() {
            let string = self.value(element, subject)?.into_struct_value();
            let data = self
                .generator
                .builder
                .build_extract_value(string, 0, "mir_string_list_data")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let length = self
                .generator
                .builder
                .build_extract_value(string, 1, "mir_string_list_length")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let status = call_try_basic_value(
                &self
                    .generator
                    .builder
                    .build_call(
                        push_fn,
                        &[
                            BasicMetadataValueEnum::from(list),
                            data.into(),
                            length.into(),
                        ],
                        "mir_string_list_push",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
            )
            .ok_or_else(|| NativeMirError::new(subject, "owned String List append returned void"))?
            .into_int_value();
            let failed = self
                .generator
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    status,
                    self.generator.context.i8_type().const_zero(),
                    "mir_string_list_push_failed",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            self.emit_owned_string_list_abort_if(failed, Some(list), &elements[index..], subject)?;
            // ConstructList transfers this input. The runtime copies its
            // bytes into an independent ABI-v3 box; retire the moved input
            // String after the append succeeds, never leaving two owners of
            // the same allocation or leaking the input's old allocation.
            self.emit_owned_string_drop_value(string.into(), subject)?;
        }
        Ok(list.into())
    }

    fn emit_owned_string_list_abort_if(
        &mut self,
        failed: IntValue<'ctx>,
        partial_list: Option<PointerValue<'ctx>>,
        unconsumed: &[MirValueId],
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let fail = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_string_list_abort");
        let ok = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_string_list_ok");
        self.generator
            .builder
            .build_conditional_branch(failed, fail, ok)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(fail);
        if let Some(list) = partial_list {
            let drop_fn = self
                .generator
                .get_runtime_fn("mimi_mir_list_drop_string")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            self.generator
                .builder
                .build_call(drop_fn, &[list.into()], "mir_string_list_partial_drop")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        }
        for element in unconsumed {
            self.emit_owned_string_drop_value(self.value(element, subject)?, subject)?;
        }
        self.emit_abort_with_message(
            "[E0800] canonical owned String List construction failed",
            subject,
        )?;
        self.generator.builder.position_at_end(ok);
        Ok(())
    }
}
