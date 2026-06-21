use crate::codegen::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::{IntValue, PointerValue};
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
            .builder
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
            .builder
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
            .builder
            .build_struct_gep(list_struct_ty, result_alloca, 0, "list_result_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(len_gep, len)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let data_gep = self
            .builder
            .build_struct_gep(list_struct_ty, result_alloca, 1, "list_result_data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(data_gep, data_ptr)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        Ok(result_alloca)
    }
}
