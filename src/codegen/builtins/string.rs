mod format;
mod helpers;
mod query;
mod transform;

use crate::codegen::CallSiteValueExt;
use crate::codegen::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, PointerValue};

impl<'ctx> CodeGenerator<'ctx> {
    pub(super) fn compile_str_char_at(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "str_char_at expects 2 arguments".to_string(),
            ));
        }
        let index = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "str_char_at: second arg must be integer index".to_string(),
                ))
            }
        };
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        // Allocate 2 bytes: char + null terminator
        let malloc_fn = self
            .module
            .get_function("malloc")
            .ok_or_else(|| "malloc not declared".to_string())?;
        let buf = self
            .builder
            .build_call(
                malloc_fn,
                &[BasicMetadataValueEnum::IntValue(
                    self.context.i64_type().const_int(2, false),
                )],
                "char_malloc",
            )
            .map_err(|e| CompileError::LlvmError(format!("malloc error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("malloc returned void")?
            .into_pointer_value();
        // Handle both string representations:
        // - PointerValue: char* directly (literal strings)
        // - StructValue: {i8*, i64} (builtin function results)
        let data_ptr = match &args[0] {
            BasicMetadataValueEnum::PointerValue(pv) => *pv,
            BasicMetadataValueEnum::StructValue(sv) => self
                .builder
                .build_extract_value(*sv, 0, "str_data_ptr")
                .map_err(|e| CompileError::LlvmError(format!("extract str data: {}", e)))?
                .into_pointer_value(),
            _ => {
                return Err(CompileError::TypeMismatch(
                    "str_char_at: first arg must be string".to_string(),
                ))
            }
        };
        // char = data_ptr[index]
        let char_ptr = {
            self.gep().build_in_bounds_gep(
                BasicTypeEnum::IntType(self.context.i8_type()),
                data_ptr,
                &[index],
                "char_ptr",
            )
        }
        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let char_val = self
            .builder
            .build_load(
                BasicTypeEnum::IntType(self.context.i8_type()),
                char_ptr,
                "char_val",
            )
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
        // Store char + null
        self.builder
            .build_store(buf, char_val)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let null_gep = {
            self.gep().build_in_bounds_gep(
                BasicTypeEnum::IntType(self.context.i8_type()),
                buf,
                &[self.context.i64_type().const_int(1, false)],
                "null_byte",
            )
        }
        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(null_gep, self.context.i8_type().const_int(0, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        // Build string struct { i8*, i64 }
        let string_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr_ty),
                BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        );
        let str_alloca = self.build_entry_alloca(string_ty, "char_str")?;
        let ptr_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(ptr_gep, buf)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.register_heap_slot(str_alloca, string_ty, 0);
        let len_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 1, "str_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(len_gep, self.context.i64_type().const_int(1, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let result = self
            .builder
            .build_load(
                BasicTypeEnum::StructType(string_ty),
                str_alloca,
                "str_char_at_result",
            )
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
        Ok(result)
    }
    pub(super) fn compile_char_code(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "char_code expects 2 arguments".to_string(),
            ));
        }
        let index = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "char_code: second arg must be integer index".to_string(),
                ))
            }
        };
        // Handle both string representations:
        // - PointerValue: char* directly (literal strings)
        // - StructValue: {i8*, i64} (builtin function results)
        let data_ptr = match &args[0] {
            BasicMetadataValueEnum::PointerValue(pv) => {
                // Literal string: pv is already a char*
                *pv
            }
            BasicMetadataValueEnum::StructValue(sv) => {
                // Builtin string struct {i8*, i64}: extract field 0 (data pointer)
                self.builder
                    .build_extract_value(*sv, 0, "str_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("extract str ptr: {}", e)))?
                    .into_pointer_value()
            }
            _ => {
                return Err(CompileError::TypeMismatch(
                    "char_code: first arg must be string".to_string(),
                ))
            }
        };
        // char = data_ptr[index]
        let char_ptr = self
            .gep()
            .build_in_bounds_gep(
                BasicTypeEnum::IntType(self.context.i8_type()),
                data_ptr,
                &[index],
                "char_ptr",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let char_val = self
            .builder
            .build_load(
                BasicTypeEnum::IntType(self.context.i8_type()),
                char_ptr,
                "char_val",
            )
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
        // Zero-extend i8 to i64 for return
        let i64_ty = self.context.i64_type();
        let result = self
            .builder
            .build_int_z_extend(char_val.into_int_value(), i64_ty, "char_code_ext")
            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?;
        Ok(result.into())
    }

    pub(super) fn compile_chr(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "chr expects 1 argument".to_string(),
            ));
        }
        let code = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "chr: first arg must be integer code point".to_string(),
                ))
            }
        };
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        // Allocate 2 bytes: char + null terminator
        let malloc_fn = self
            .module
            .get_function("malloc")
            .ok_or_else(|| "malloc not declared".to_string())?;
        let buf = self
            .builder
            .build_call(
                malloc_fn,
                &[BasicMetadataValueEnum::IntValue(
                    self.context.i64_type().const_int(2, false),
                )],
                "chr_malloc",
            )
            .map_err(|e| CompileError::LlvmError(format!("malloc error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("malloc returned void")?
            .into_pointer_value();
        // Truncate i64 to i8 for storage
        let i8_ty = self.context.i8_type();
        let char_byte = self
            .builder
            .build_int_truncate(code, i8_ty, "chr_trunc")
            .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))?;
        self.builder
            .build_store(buf, char_byte)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        // Store null terminator at buf+1
        let null_gep = self
            .gep()
            .build_in_bounds_gep(
                BasicTypeEnum::IntType(i8_ty),
                buf,
                &[self.context.i64_type().const_int(1, false)],
                "null_byte",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(null_gep, i8_ty.const_int(0, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        // Build string struct { i8*, i64 }
        let string_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr_ty),
                BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        );
        let str_alloca = self.build_entry_alloca(string_ty, "chr_str")?;
        let ptr_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(ptr_gep, buf)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.register_heap_slot(str_alloca, string_ty, 0);
        let len_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 1, "str_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(len_gep, self.context.i64_type().const_int(1, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let result = self
            .builder
            .build_load(
                BasicTypeEnum::StructType(string_ty),
                str_alloca,
                "chr_result",
            )
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
        Ok(result)
    }

    /// Parse a C string with strtol and return (ok, value).
    /// ok is true when at least one digit was consumed and the rest of the
    /// string is the null terminator (whole-string parse).
    fn emit_strtol(
        &self,
        s_ptr: PointerValue<'ctx>,
    ) -> MimiResult<(
        inkwell::values::IntValue<'ctx>,
        inkwell::values::IntValue<'ctx>,
    )> {
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let i8_ty = self.context.i8_type();
        let strtol_fn = self
            .module
            .get_function("strtol")
            .or_else(|| {
                let ty = i64_ty.fn_type(
                    &[
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                        BasicMetadataTypeEnum::IntType(self.context.i32_type()),
                    ],
                    false,
                );
                Some(self.module.add_function(
                    "strtol",
                    ty,
                    Some(inkwell::module::Linkage::External),
                ))
            })
            .ok_or_else(|| "failed to get or create strtol function".to_string())?;
        let endptr_alloca = self.build_alloca(i8_ptr, "strtol_endptr")?;
        self.build_store(endptr_alloca, i8_ptr.const_null())?;
        let call = self
            .builder
            .build_call(
                strtol_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::PointerValue(endptr_alloca),
                    BasicMetadataValueEnum::IntValue(self.context.i32_type().const_int(10, false)),
                ],
                "strtol_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("strtol error: {}", e)))?;
        let value = self.expect_basic_value(&call, "strtol")?.into_int_value();
        let endptr = self
            .builder
            .build_load(i8_ptr, endptr_alloca, "strtol_endptr_load")
            .map_err(|e| CompileError::LlvmError(format!("load endptr: {}", e)))?
            .into_pointer_value();
        let end_i = self
            .builder
            .build_ptr_to_int(endptr, i64_ty, "end_i")
            .map_err(|e| CompileError::LlvmError(format!("ptrtoint endptr: {}", e)))?;
        let s_i = self
            .builder
            .build_ptr_to_int(s_ptr, i64_ty, "s_i")
            .map_err(|e| CompileError::LlvmError(format!("ptrtoint s: {}", e)))?;
        let consumed = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, end_i, s_i, "strtol_consumed")
            .map_err(|e| CompileError::LlvmError(format!("icmp consumed: {}", e)))?;
        let end_byte = self
            .builder
            .build_load(BasicTypeEnum::IntType(i8_ty), endptr, "strtol_end_byte")
            .map_err(|e| CompileError::LlvmError(format!("load end byte: {}", e)))?
            .into_int_value();
        let end_null = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                end_byte,
                i8_ty.const_int(0, false),
                "strtol_end_null",
            )
            .map_err(|e| CompileError::LlvmError(format!("icmp end null: {}", e)))?;
        let ok = self
            .builder
            .build_and(consumed, end_null, "strtol_ok")
            .map_err(|e| CompileError::LlvmError(format!("and ok: {}", e)))?;
        Ok((ok, value))
    }

    /// Build a (bool, i64) tuple value from a success flag and an i64.
    fn build_parse_int_tuple(
        &self,
        ok: inkwell::values::IntValue<'ctx>,
        value: inkwell::values::IntValue<'ctx>,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let tuple_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(self.context.bool_type()),
                BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        );
        let alloca = self.build_alloca(tuple_ty, "parse_int_tuple")?;
        let ok_gep = self
            .gep()
            .build_struct_gep(tuple_ty, alloca, 0, "parse_ok")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(ok_gep, ok)?;
        let val_gep = self
            .gep()
            .build_struct_gep(tuple_ty, alloca, 1, "parse_val")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(val_gep, value)?;
        self.build_load(tuple_ty, alloca, "parse_int_tuple_val")
    }

    /// Extract a C string pointer from a string argument (raw pointer or {ptr,len} struct).
    fn extract_string_arg_ptr(
        &self,
        arg: &BasicMetadataValueEnum<'ctx>,
        caller: &str,
    ) -> MimiResult<PointerValue<'ctx>> {
        match arg {
            BasicMetadataValueEnum::PointerValue(pv) => Ok(*pv),
            BasicMetadataValueEnum::StructValue(sv) => {
                let ptr = self
                    .builder
                    .build_extract_value(*sv, 0, "str_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("extract str ptr: {}", e)))?
                    .into_pointer_value();
                Ok(ptr)
            }
            _ => Err(CompileError::TypeMismatch(format!(
                "{}: first arg must be string, int, or float",
                caller
            ))),
        }
    }

    pub(super) fn compile_to_int(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "to_int expects 1 argument".to_string(),
            ));
        }
        let i64_ty = self.context.i64_type();
        match &args[0] {
            BasicMetadataValueEnum::IntValue(iv) => return Ok((*iv).into()),
            BasicMetadataValueEnum::FloatValue(fv) => {
                let iv = self
                    .builder
                    .build_float_to_signed_int(*fv, i64_ty, "to_int_f")
                    .map_err(|e| {
                        CompileError::LlvmError(format!("to_int float->int error: {}", e))
                    })?;
                return Ok(iv.into());
            }
            _ => {}
        }
        let s_ptr = self.extract_string_arg_ptr(&args[0], "to_int")?;
        let (_ok, value) = self.emit_strtol(s_ptr)?;
        Ok(value.into())
    }

    pub(super) fn compile_str_parse_int(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "str_parse_int expects 1 argument".to_string(),
            ));
        }
        let i64_ty = self.context.i64_type();
        let true_val = self.context.bool_type().const_int(1, false);
        match &args[0] {
            BasicMetadataValueEnum::IntValue(iv) => {
                return self.build_parse_int_tuple(true_val, *iv);
            }
            BasicMetadataValueEnum::FloatValue(fv) => {
                let iv = self
                    .builder
                    .build_float_to_signed_int(*fv, i64_ty, "to_int_f")
                    .map_err(|e| {
                        CompileError::LlvmError(format!("str_parse_int float->int error: {}", e))
                    })?;
                return self.build_parse_int_tuple(true_val, iv);
            }
            _ => {}
        }
        let s_ptr = self.extract_string_arg_ptr(&args[0], "str_parse_int")?;
        let (ok, value) = self.emit_strtol(s_ptr)?;
        self.build_parse_int_tuple(ok, value)
    }

    /// Parse a C string with strtod and return (ok, value).
    fn emit_strtod(
        &self,
        s_ptr: PointerValue<'ctx>,
    ) -> MimiResult<(
        inkwell::values::IntValue<'ctx>,
        inkwell::values::FloatValue<'ctx>,
    )> {
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let i8_ty = self.context.i8_type();
        let f64_ty = self.context.f64_type();
        let strtod_fn = self
            .module
            .get_function("strtod")
            .or_else(|| {
                let ty = f64_ty.fn_type(
                    &[
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                    ],
                    false,
                );
                Some(self.module.add_function(
                    "strtod",
                    ty,
                    Some(inkwell::module::Linkage::External),
                ))
            })
            .ok_or_else(|| "failed to get or create strtod function".to_string())?;
        let endptr_alloca = self.build_alloca(i8_ptr, "strtod_endptr")?;
        self.build_store(endptr_alloca, i8_ptr.const_null())?;
        let call = self
            .builder
            .build_call(
                strtod_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::PointerValue(endptr_alloca),
                ],
                "strtod_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("strtod error: {}", e)))?;
        let value = self.expect_basic_value(&call, "strtod")?.into_float_value();
        let endptr = self
            .builder
            .build_load(i8_ptr, endptr_alloca, "strtod_endptr_load")
            .map_err(|e| CompileError::LlvmError(format!("load endptr: {}", e)))?
            .into_pointer_value();
        let end_i = self
            .builder
            .build_ptr_to_int(endptr, i64_ty, "end_i")
            .map_err(|e| CompileError::LlvmError(format!("ptrtoint endptr: {}", e)))?;
        let s_i = self
            .builder
            .build_ptr_to_int(s_ptr, i64_ty, "s_i")
            .map_err(|e| CompileError::LlvmError(format!("ptrtoint s: {}", e)))?;
        let consumed = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, end_i, s_i, "strtod_consumed")
            .map_err(|e| CompileError::LlvmError(format!("icmp consumed: {}", e)))?;
        let end_byte = self
            .builder
            .build_load(BasicTypeEnum::IntType(i8_ty), endptr, "strtod_end_byte")
            .map_err(|e| CompileError::LlvmError(format!("load end byte: {}", e)))?
            .into_int_value();
        let end_null = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                end_byte,
                i8_ty.const_int(0, false),
                "strtod_end_null",
            )
            .map_err(|e| CompileError::LlvmError(format!("icmp end null: {}", e)))?;
        let ok = self
            .builder
            .build_and(consumed, end_null, "strtod_ok")
            .map_err(|e| CompileError::LlvmError(format!("and ok: {}", e)))?;
        Ok((ok, value))
    }

    fn build_parse_float_tuple(
        &self,
        ok: inkwell::values::IntValue<'ctx>,
        value: inkwell::values::FloatValue<'ctx>,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let tuple_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(self.context.bool_type()),
                BasicTypeEnum::FloatType(self.context.f64_type()),
            ],
            false,
        );
        let alloca = self.build_alloca(tuple_ty, "parse_float_tuple")?;
        let ok_gep = self
            .gep()
            .build_struct_gep(tuple_ty, alloca, 0, "parse_ok")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(ok_gep, ok)?;
        let val_gep = self
            .gep()
            .build_struct_gep(tuple_ty, alloca, 1, "parse_val")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(val_gep, value)?;
        self.build_load(tuple_ty, alloca, "parse_float_tuple_val")
    }

    pub(super) fn compile_to_float(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "to_float expects 1 argument".to_string(),
            ));
        }
        let f64_ty = self.context.f64_type();
        match &args[0] {
            BasicMetadataValueEnum::FloatValue(fv) => return Ok((*fv).into()),
            BasicMetadataValueEnum::IntValue(iv) => {
                let fv = self
                    .builder
                    .build_signed_int_to_float(*iv, f64_ty, "to_float_i")
                    .map_err(|e| {
                        CompileError::LlvmError(format!("to_float int->float error: {}", e))
                    })?;
                return Ok(fv.into());
            }
            _ => {}
        }
        let s_ptr = self.extract_string_arg_ptr(&args[0], "to_float")?;
        let (_ok, value) = self.emit_strtod(s_ptr)?;
        Ok(value.into())
    }

    pub(super) fn compile_str_parse_float(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "str_parse_float expects 1 argument".to_string(),
            ));
        }
        let f64_ty = self.context.f64_type();
        let true_val = self.context.bool_type().const_int(1, false);
        match &args[0] {
            BasicMetadataValueEnum::FloatValue(fv) => {
                return self.build_parse_float_tuple(true_val, *fv);
            }
            BasicMetadataValueEnum::IntValue(iv) => {
                let fv = self
                    .builder
                    .build_signed_int_to_float(*iv, f64_ty, "to_float_i")
                    .map_err(|e| {
                        CompileError::LlvmError(format!("str_parse_float int->float error: {}", e))
                    })?;
                return self.build_parse_float_tuple(true_val, fv);
            }
            _ => {}
        }
        let s_ptr = self.extract_string_arg_ptr(&args[0], "str_parse_float")?;
        let (ok, value) = self.emit_strtod(s_ptr)?;
        self.build_parse_float_tuple(ok, value)
    }
}
