use crate::codegen::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::{BasicMetadataValueEnum, IntValue, PointerValue};
use inkwell::AddressSpace;

impl<'ctx> CodeGenerator<'ctx> {
    /// The canonical Mimi list struct type: `{ i64 len, i8* data }`.
    pub(in crate::codegen) fn list_struct_type(&self) -> StructType<'ctx> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.i8_type().ptr_type(AddressSpace::default());
        self.context.struct_type(
            &[
                BasicTypeEnum::IntType(i64_ty),
                BasicTypeEnum::PointerType(i8_ptr),
            ],
            false,
        )
    }

    /// Size of a list element slot (i64) in bytes.
    pub(in crate::codegen) fn list_elem_size(&self) -> IntValue<'ctx> {
        self.context.i64_type().const_int(8, false)
    }

    /// Load the `len` field of a list struct pointer.
    pub(in crate::codegen) fn load_list_len(
        &self,
        list_ptr: PointerValue<'ctx>,
    ) -> MimiResult<IntValue<'ctx>> {
        let list_struct_ty = self.list_struct_type();
        let len_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, list_ptr, 0, "list_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let len = self
            .builder
            .build_load(self.context.i64_type(), len_gep, "list_len_val")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        Ok(len)
    }

    /// Load the `data` field of a list struct pointer as an `i8*`.
    pub(in crate::codegen) fn load_list_data_raw(
        &self,
        list_ptr: PointerValue<'ctx>,
    ) -> MimiResult<PointerValue<'ctx>> {
        let list_struct_ty = self.list_struct_type();
        let i8_ptr = self.context.i8_type().ptr_type(AddressSpace::default());
        let data_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, list_ptr, 1, "list_data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let data_i8 = self
            .builder
            .build_load(i8_ptr, data_gep, "list_data_val")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_pointer_value();
        Ok(data_i8)
    }

    /// Load the `data` field of a list struct pointer and bitcast it to `i64*`.
    pub(in crate::codegen) fn load_list_data_i64(
        &self,
        list_ptr: PointerValue<'ctx>,
    ) -> MimiResult<PointerValue<'ctx>> {
        let data_i8 = self.load_list_data_raw(list_ptr)?;
        let data_ptr = self
            .builder
            .build_bit_cast(
                data_i8,
                self.context.i64_type().ptr_type(AddressSpace::default()),
                "list_data_i64",
            )
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
            .into_pointer_value();
        Ok(data_ptr)
    }

    /// Allocate a list result struct and populate it with `len` and `data` (`i8*`).
    pub(in crate::codegen) fn alloc_list_result(
        &self,
        len: IntValue<'ctx>,
        data_ptr: PointerValue<'ctx>,
    ) -> MimiResult<PointerValue<'ctx>> {
        let list_struct_ty = self.list_struct_type();
        let result_alloca = self
            .builder
            .build_alloca(list_struct_ty, "list_result")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        let len_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, result_alloca, 0, "list_result_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(len_gep, len)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let data_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, result_alloca, 1, "list_result_data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(data_gep, data_ptr)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        Ok(result_alloca)
    }

    /// Check that `idx` is within bounds `[0, len)` for a list operation.
    /// On OOB, calls `mimi_runtime_abort` with a descriptive message.
    pub(in crate::codegen) fn check_list_bounds(
        &self,
        list_ptr: PointerValue<'ctx>,
        idx: IntValue<'ctx>,
        operation: &str,
    ) -> MimiResult<()> {
        let len = self.load_list_len(list_ptr)?;
        let function = self.builder.get_insert_block()
            .ok_or_else(|| "check_list_bounds: no insert block".to_string())?
            .get_parent()
            .ok_or_else(|| "check_list_bounds: no parent function".to_string())?;
        let pass_bb = self.context.append_basic_block(function, "bounds_ok");
        let fail_bb = self.context.append_basic_block(function, "bounds_fail");
        let oob = self.builder.build_int_compare(
            inkwell::IntPredicate::UGE, idx, len, "oob",
        ).map_err(|e| CompileError::LlvmError(format!("oob compare: {}", e)))?;
        self.builder.build_conditional_branch(oob, fail_bb, pass_bb)
            .map_err(|e| CompileError::LlvmError(format!("oob branch: {}", e)))?;
        // Fail block: abort with message
        self.builder.position_at_end(fail_bb);
        let msg = format!("list index out of bounds: {} (idx >= len)", operation);
        let msg_ptr = self.builder.build_global_string_ptr(&msg, "oob_msg")
            .map_err(|e| CompileError::LlvmError(format!("oob msg: {}", e)))?;
        let abort_fn = self.module.get_function("mimi_runtime_abort")
            .unwrap_or_else(|| {
                let i8_ptr = self.context.i8_type().ptr_type(AddressSpace::default());
                let ty = self.context.void_type().fn_type(&[
                    inkwell::types::BasicMetadataTypeEnum::PointerType(i8_ptr),
                ], false);
                self.module.add_function("mimi_runtime_abort", ty, Some(inkwell::module::Linkage::External))
            });
        self.builder.build_call(abort_fn, &[
            BasicMetadataValueEnum::PointerValue(msg_ptr.as_pointer_value()),
        ], "oob_abort")
            .map_err(|e| CompileError::LlvmError(format!("oob abort: {}", e)))?;
        self.builder.build_unconditional_branch(pass_bb)
            .map_err(|e| CompileError::LlvmError(format!("oob branch: {}", e)))?;
        // Continue at pass block
        self.builder.position_at_end(pass_bb);
        Ok(())
    }
}
