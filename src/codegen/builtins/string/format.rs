use crate::codegen::CodeGenerator;
use inkwell::types::BasicTypeEnum;
use crate::codegen::CallSiteValueExt;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use crate::error::{CompileError, MimiResult};

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn compile_to_string(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 {
                    return Err(CompileError::WrongArgCount("to_string expects 1 argument".to_string()));
                }
                match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => {
                        let alloc_size = self.context.i64_type().const_int(21, false);
                        let malloc_fn = self.module.get_function("malloc")
                            .ok_or_else(|| "malloc not declared".to_string())?;
                        let buf = self.builder.build_call(malloc_fn, &[
                            BasicMetadataValueEnum::IntValue(alloc_size),
                        ], "malloc_call")
                            .map_err(|e| CompileError::LlvmError(format!("malloc error: {}", e)))?
                            .try_as_basic_value_opt()
                            .ok_or("malloc returned void")?
                            .into_pointer_value();
                        let fmt_global = self.builder.build_global_string_ptr("%ld", "int_fmt")
                            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
                        let sprintf_fn = self.module.get_function("sprintf")
                            .ok_or_else(|| "sprintf not declared".to_string())?;
                        self.builder.build_call(sprintf_fn, &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                            BasicMetadataValueEnum::IntValue(iv),
                        ], "sprintf_call")
                            .map_err(|e| CompileError::LlvmError(format!("sprintf error: {}", e)))?;
                        // Build {i8*, i64} struct from the buffer
                        let str_ty = self.context.struct_type(&[
                            BasicTypeEnum::PointerType(
                                self.context.ptr_type(inkwell::AddressSpace::default())
                            ),
                            BasicTypeEnum::IntType(self.context.i64_type()),
                        ], false);
                        let alloca = self.builder.build_alloca(str_ty, "str_result")
                            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                        let ptr_gep = self.gep().build_struct_gep(str_ty, alloca, 0, "str_ptr")
                            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                        self.builder.build_store(ptr_gep, buf)
                            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                        self.register_heap_gep(ptr_gep);
                        let strlen_fn = self.module.get_function("strlen")
                            .ok_or_else(|| "strlen not declared".to_string())?;
                        let len = self.builder.build_call(strlen_fn, &[BasicMetadataValueEnum::PointerValue(buf)], "strlen_to_s")
                            .map_err(|e| CompileError::LlvmError(format!("strlen error: {}", e)))?
                            .try_as_basic_value_opt()
                            .ok_or_else(|| CompileError::LlvmError("strlen returned void".to_string()))?
                            .into_int_value();
                        let len_gep = self.gep().build_struct_gep(str_ty, alloca, 1, "str_len")
                            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                        self.builder.build_store(len_gep, len)
                            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                        let result = self.builder.build_load(
                            BasicTypeEnum::StructType(str_ty),
                            alloca,
                            "str_result"
                        ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                        Ok(result)
                    }
                    BasicMetadataValueEnum::FloatValue(fv) => {
                        let alloc_size = self.context.i64_type().const_int(32, false);
                        let malloc_fn = self.module.get_function("malloc")
                            .ok_or_else(|| "malloc not declared".to_string())?;
                        let buf = self.builder.build_call(malloc_fn, &[
                            BasicMetadataValueEnum::IntValue(alloc_size),
                        ], "malloc_call")
                            .map_err(|e| CompileError::LlvmError(format!("malloc error: {}", e)))?
                            .try_as_basic_value_opt()
                            .ok_or("malloc returned void")?
                            .into_pointer_value();
                        let fmt_global = self.builder.build_global_string_ptr("%f", "float_fmt")
                            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
                        let sprintf_fn = self.module.get_function("sprintf")
                            .ok_or_else(|| "sprintf not declared".to_string())?;
                        self.builder.build_call(sprintf_fn, &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                            BasicMetadataValueEnum::FloatValue(fv),
                        ], "sprintf_call")
                            .map_err(|e| CompileError::LlvmError(format!("sprintf error: {}", e)))?;
                        // Build {i8*, i64} struct from the buffer
                        let str_ty = self.context.struct_type(&[
                            BasicTypeEnum::PointerType(
                                self.context.ptr_type(inkwell::AddressSpace::default())
                            ),
                            BasicTypeEnum::IntType(self.context.i64_type()),
                        ], false);
                        let alloca = self.builder.build_alloca(str_ty, "str_result")
                            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                        let ptr_gep = self.gep().build_struct_gep(str_ty, alloca, 0, "str_ptr")
                            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                        self.builder.build_store(ptr_gep, buf)
                            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                        self.register_heap_gep(ptr_gep);
                        let strlen_fn = self.module.get_function("strlen")
                            .ok_or_else(|| "strlen not declared".to_string())?;
                        let len = self.builder.build_call(strlen_fn, &[BasicMetadataValueEnum::PointerValue(buf)], "strlen_to_s")
                            .map_err(|e| CompileError::LlvmError(format!("strlen error: {}", e)))?
                            .try_as_basic_value_opt()
                            .ok_or_else(|| CompileError::LlvmError("strlen returned void".to_string()))?
                            .into_int_value();
                        let len_gep = self.gep().build_struct_gep(str_ty, alloca, 1, "str_len")
                            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                        self.builder.build_store(len_gep, len)
                            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                        let result = self.builder.build_load(
                            BasicTypeEnum::StructType(str_ty),
                            alloca,
                            "str_result"
                        ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                        Ok(result)
                    }
                    _ => Err(CompileError::TypeMismatch("to_string: unsupported type".to_string())),
                }

    }
}
