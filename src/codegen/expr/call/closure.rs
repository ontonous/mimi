use crate::codegen::{call_try_basic_value, CodeGenerator};
use crate::error::CompileError;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn compile_closure_call(
        &self,
        closure_val: BasicValueEnum<'ctx>,
        arg: inkwell::values::IntValue<'ctx>,
    ) -> Result<inkwell::values::BasicValueEnum<'ctx>, CompileError> {
        let i64_ty = self.context.i64_type();
        let (fn_ptr, env_ptr) = match closure_val {
            BasicValueEnum::StructValue(sv) => {
                let fn_ptr = self.builder.build_extract_value(sv, 0, "fn_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("extract fn_ptr error: {}", e)))?.into_pointer_value();
                let env_ptr = self.builder.build_extract_value(sv, 1, "env_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("extract env_ptr error: {}", e)))?.into_pointer_value();
                (fn_ptr, env_ptr)
            }
            BasicValueEnum::PointerValue(pv) => {
                let closure_struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let loaded = self.builder.build_load(BasicTypeEnum::StructType(closure_struct_ty), pv, "closure_loaded")
                    .map_err(|e| CompileError::LlvmError(format!("load closure error: {}", e)))?.into_struct_value();
                let fn_ptr = self.builder.build_extract_value(loaded, 0, "fn_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("extract fn_ptr error: {}", e)))?.into_pointer_value();
                let env_ptr = self.builder.build_extract_value(loaded, 1, "env_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("extract env_ptr error: {}", e)))?.into_pointer_value();
                (fn_ptr, env_ptr)
            }
            _ => return Err(CompileError::Generic("expected a closure".into())),
        };
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let fn_type = i64_ty.fn_type(&[
            BasicMetadataTypeEnum::PointerType(i8_ptr),
            BasicMetadataTypeEnum::IntType(i64_ty),
        ], false);
        let fn_typed = self.builder.build_pointer_cast(
            fn_ptr, self.context.ptr_type(inkwell::AddressSpace::default()), "fn_typed"
        ).map_err(|e| CompileError::LlvmError(format!("pointer cast error: {}", e)))?;
        let call = self.builder.build_indirect_call(
            fn_type, fn_typed, &[
                BasicMetadataValueEnum::PointerValue(env_ptr),
                BasicMetadataValueEnum::IntValue(arg),
            ], "closure_call"
        ).map_err(|e| CompileError::LlvmError(format!("indirect call error: {}", e)))?;
        let result = call_try_basic_value(&call)
            .unwrap_or(BasicValueEnum::IntValue(i64_ty.const_int(0, false)));
        Ok(result)
    }
}
