mod format;
mod helpers;
mod query;
mod transform;

use crate::codegen::CodeGenerator;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use crate::codegen::CallSiteValueExt;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use crate::error::{CompileError, MimiResult};

impl<'ctx> CodeGenerator<'ctx> {
    pub(super) fn compile_str_char_at(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 { return Err(CompileError::WrongArgCount("str_char_at expects 2 arguments".to_string())); }
                let str_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("str_char_at: first arg must be string".to_string())),
                };
                let index = match args[1] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err(CompileError::TypeMismatch("str_char_at: second arg must be integer index".to_string())),
                };
                let i8_ptr_ty = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                // Allocate 2 bytes: char + null terminator
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let buf = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(self.context.i64_type().const_int(2, false)),
                ], "char_malloc")
                    .map_err(|e| CompileError::LlvmError(format!("malloc error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                // gep str_ptr + index (indexing into string struct { ptr, len })
                let data_ptr_gep = self.gep().build_struct_gep(
                    self.context.struct_type(&[
                        BasicTypeEnum::PointerType(i8_ptr_ty),
                        BasicTypeEnum::IntType(self.context.i64_type()),
                    ], false),
                    str_ptr, 0, "str_data_ptr"
                ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let data_ptr = self.builder.build_load(
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    data_ptr_gep,
                    "data_ptr"
                ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_pointer_value();
                // char = data_ptr[index]
                                let char_ptr = {
                    self.gep().build_gep(
                        BasicTypeEnum::IntType(self.context.i8_type()),
                        data_ptr,
                        &[index],
                        "char_ptr"
                    )
                }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let char_val = self.builder.build_load(
                    BasicTypeEnum::IntType(self.context.i8_type()),
                    char_ptr,
                    "char_val"
                ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                // Store char + null
                self.builder.build_store(buf, char_val)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                                let null_gep = {
                    self.gep().build_gep(
                        BasicTypeEnum::IntType(self.context.i8_type()),
                        buf,
                        &[self.context.i64_type().const_int(1, false)],
                        "null_byte"
                    )
                }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(null_gep, self.context.i8_type().const_int(0, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                // Build string struct { i8*, i64 }
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    BasicTypeEnum::IntType(self.context.i64_type()),
                ], false);
                let str_alloca = self.builder.build_alloca(string_ty, "char_str")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                let ptr_gep = self.gep().build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(ptr_gep, buf)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.register_heap_gep(ptr_gep);
                let len_gep = self.gep().build_struct_gep(string_ty, str_alloca, 1, "str_len")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(len_gep, self.context.i64_type().const_int(1, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                Ok(str_alloca.into())

    }
    pub(super) fn compile_str_parse_int(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 { return Err(CompileError::WrongArgCount("str_parse_int/to_int expects 1 argument".to_string())); }
                let i64_ty = self.context.i64_type();
                // Handle numeric types directly (int → int, float → int trunc)
                match &args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => {
                        return Ok((*iv).into());
                    }
                    BasicMetadataValueEnum::FloatValue(fv) => {
                        let iv = self.builder.build_float_to_signed_int(*fv, i64_ty, "to_int_f")
                            .map_err(|e| CompileError::LlvmError(format!("to_int float->int error: {}", e)))?;
                        return Ok(iv.into());
                    }
                    _ => {}
                }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("str_parse_int: first arg must be string, int, or float".to_string())),
                };
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                // strtol(s, NULL, 10)
                let strtol_fn = self.module.get_function("strtol")
                    .or_else(|| {
                        let ty = self.context.i64_type().fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::PointerType(i8_ptr.ptr_type(inkwell::AddressSpace::default())),
                            BasicMetadataTypeEnum::IntType(self.context.i32_type()),
                        ], false);
                        Some(self.module.add_function("strtol", ty, Some(inkwell::module::Linkage::External)))
                    }).ok_or_else(|| "failed to get or create strtol function".to_string())?;
                let null_ptr = i8_ptr.const_null();
                let call = self.builder.build_call(strtol_fn, &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::PointerValue(null_ptr),
                    BasicMetadataValueEnum::IntValue(self.context.i32_type().const_int(10, false)),
                ], "strtol_call")
                    .map_err(|e| CompileError::LlvmError(format!("strtol error: {}", e)))?;
                Ok(self.expect_basic_value(&call, "strtol")?)

    }
    pub(super) fn compile_str_parse_float(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 { return Err(CompileError::WrongArgCount("str_parse_float/to_float expects 1 argument".to_string())); }
                let f64_ty = self.context.f64_type();
                // Handle numeric types directly (float → float, int → float)
                match &args[0] {
                    BasicMetadataValueEnum::FloatValue(fv) => {
                        return Ok((*fv).into());
                    }
                    BasicMetadataValueEnum::IntValue(iv) => {
                        let fv = self.builder.build_signed_int_to_float(*iv, f64_ty, "to_float_i")
                            .map_err(|e| CompileError::LlvmError(format!("to_float int->float error: {}", e)))?;
                        return Ok(fv.into());
                    }
                    _ => {}
                }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("str_parse_float: first arg must be string, int, or float".to_string())),
                };
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                // strtod(s, NULL)
                let strtod_fn = self.module.get_function("strtod")
                    .or_else(|| {
                        let ty = self.context.f64_type().fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::PointerType(i8_ptr.ptr_type(inkwell::AddressSpace::default())),
                        ], false);
                        Some(self.module.add_function("strtod", ty, Some(inkwell::module::Linkage::External)))
                    }).ok_or_else(|| "failed to get or create strtod function".to_string())?;
                let null_ptr = i8_ptr.const_null();
                let call = self.builder.build_call(strtod_fn, &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::PointerValue(null_ptr),
                ], "strtod_call")
                    .map_err(|e| CompileError::LlvmError(format!("strtod error: {}", e)))?;
                Ok(self.expect_basic_value(&call, "strtod")?)

    }
}
