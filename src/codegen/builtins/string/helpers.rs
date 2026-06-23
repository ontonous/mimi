use crate::codegen::CodeGenerator;
use inkwell::types::BasicTypeEnum;
use crate::codegen::CallSiteValueExt;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use crate::error::{CompileError, MimiResult};

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn compile_str_to_c_str(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                // Extract the raw C string pointer from a Mimi string
                if args.len() != 1 { return Err(CompileError::WrongArgCount("str_to_c_str expects 1 argument".to_string())); }
                let c_ptr = self.extract_raw_str_ptr(&args[0])?;
                Ok(c_ptr.into())

    }

    pub(in crate::codegen) fn compile_c_str_to_string(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                // Wrap a raw C string pointer into a Mimi string struct {i8*, i64}
                if args.len() != 1 { return Err(CompileError::WrongArgCount("c_str_to_string expects 1 argument".to_string())); }
                let raw_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("c_str_to_string: argument must be a raw C string pointer".to_string())),
                };
                let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    BasicTypeEnum::IntType(self.context.i64_type()),
                ], false);
                let str_alloca = self.builder.build_alloca(string_ty, "cstr_str")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                let ptr_gep = self.gep().build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(ptr_gep, raw_ptr)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let len_gep = self.gep().build_struct_gep(string_ty, str_alloca, 1, "str_len")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let str_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(raw_ptr),
                ], "strlen_call")
                    .map_err(|e| CompileError::LlvmError(format!("strlen error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("strlen returned void")?;
                self.builder.build_store(len_gep, str_len)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                Ok(str_alloca.into())

    }
}
