use super::CodeGenerator;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use crate::error::{CompileError, MimiResult};

impl<'ctx> CodeGenerator<'ctx> {

    pub(super) fn compile_to_string(
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
                            .map_err(|e| CompileError::Generic(format!("malloc error: {}", e)))?
                            .try_as_basic_value().left()
                            .ok_or("malloc returned void")?
                            .into_pointer_value();
                        let fmt_global = self.builder.build_global_string_ptr("%ld", "int_fmt")
                            .map_err(|e| CompileError::Generic(format!("fmt error: {}", e)))?;
                        let sprintf_fn = self.module.get_function("sprintf")
                            .ok_or_else(|| "sprintf not declared".to_string())?;
                        self.builder.build_call(sprintf_fn, &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                            BasicMetadataValueEnum::IntValue(iv),
                        ], "sprintf_call")
                            .map_err(|e| CompileError::Generic(format!("sprintf error: {}", e)))?;
                        // Return raw C string pointer (matches string literal representation)
                        Ok(buf.into())
                    }
                    BasicMetadataValueEnum::FloatValue(fv) => {
                        let alloc_size = self.context.i64_type().const_int(32, false);
                        let malloc_fn = self.module.get_function("malloc")
                            .ok_or_else(|| "malloc not declared".to_string())?;
                        let buf = self.builder.build_call(malloc_fn, &[
                            BasicMetadataValueEnum::IntValue(alloc_size),
                        ], "malloc_call")
                            .map_err(|e| CompileError::Generic(format!("malloc error: {}", e)))?
                            .try_as_basic_value().left()
                            .ok_or("malloc returned void")?
                            .into_pointer_value();
                        let fmt_global = self.builder.build_global_string_ptr("%f", "float_fmt")
                            .map_err(|e| CompileError::Generic(format!("fmt error: {}", e)))?;
                        let sprintf_fn = self.module.get_function("sprintf")
                            .ok_or_else(|| "sprintf not declared".to_string())?;
                        self.builder.build_call(sprintf_fn, &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                            BasicMetadataValueEnum::FloatValue(fv),
                        ], "sprintf_call")
                            .map_err(|e| CompileError::Generic(format!("sprintf error: {}", e)))?;
                        Ok(buf.into())
                    }
                    _ => Err(CompileError::TypeMismatch("to_string: unsupported type".to_string())),
                }

    }

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
                    .map_err(|e| CompileError::Generic(format!("malloc error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                // gep str_ptr + index (indexing into string struct { ptr, len })
                let data_ptr_gep = self.builder.build_struct_gep(
                    self.context.struct_type(&[
                        BasicTypeEnum::PointerType(i8_ptr_ty),
                        BasicTypeEnum::IntType(self.context.i64_type()),
                    ], false),
                    str_ptr, 0, "str_data_ptr"
                ).map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                let data_ptr = self.builder.build_load(
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    data_ptr_gep,
                    "data_ptr"
                ).map_err(|e| CompileError::Generic(format!("load error: {}", e)))?.into_pointer_value();
                // char = data_ptr[index]
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let char_ptr = unsafe {
                    self.builder.build_gep(
                        BasicTypeEnum::IntType(self.context.i8_type()),
                        data_ptr,
                        &[index],
                        "char_ptr"
                    )
                }.map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                let char_val = self.builder.build_load(
                    BasicTypeEnum::IntType(self.context.i8_type()),
                    char_ptr,
                    "char_val"
                ).map_err(|e| CompileError::Generic(format!("load error: {}", e)))?;
                // Store char + null
                self.builder.build_store(buf, char_val)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let null_gep = unsafe {
                    self.builder.build_gep(
                        BasicTypeEnum::IntType(self.context.i8_type()),
                        buf,
                        &[self.context.i64_type().const_int(1, false)],
                        "null_byte"
                    )
                }.map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                self.builder.build_store(null_gep, self.context.i8_type().const_int(0, false))
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                // Build string struct { i8*, i64 }
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    BasicTypeEnum::IntType(self.context.i64_type()),
                ], false);
                let str_alloca = self.builder.build_alloca(string_ty, "char_str")
                    .map_err(|e| CompileError::Generic(format!("alloca error: {}", e)))?;
                let ptr_gep = self.builder.build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                    .map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                self.builder.build_store(ptr_gep, buf)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                let len_gep = self.builder.build_struct_gep(string_ty, str_alloca, 1, "str_len")
                    .map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                self.builder.build_store(len_gep, self.context.i64_type().const_int(1, false))
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                Ok(str_alloca.into())

    }

    pub(super) fn compile_str_contains(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 { return Err(CompileError::WrongArgCount("str_contains expects 2 arguments".to_string())); }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("str_contains: first arg must be string".to_string())),
                };
                let sub_ptr = match args[1] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("str_contains: second arg must be string".to_string())),
                };
                // strstr(s, sub) -> i8* (or NULL if not found)
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let strstr_fn = self.module.get_function("strstr")
                    .or_else(|| {
                        let ty = i8_ptr.fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ], false);
                        Some(self.module.add_function("strstr", ty, Some(inkwell::module::Linkage::External)))
                    }).ok_or_else(|| "failed to get or create strstr function".to_string())?;
                let result = self.builder.build_call(strstr_fn, &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::PointerValue(sub_ptr),
                ], "strstr_call")
                    .map_err(|e| CompileError::Generic(format!("strstr error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("strstr returned void")?;
                let cmp = self.builder.build_is_not_null(result.into_pointer_value(), "found")
                    .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?;
                let ext: BasicValueEnum = self.builder.build_int_z_extend(cmp, self.context.i64_type(), "result")
                    .map_err(|e| CompileError::Generic(format!("zext error: {}", e)))?.into();
                Ok(ext)

    }

    pub(super) fn compile_str_starts_with(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 { return Err(CompileError::WrongArgCount("str_starts_with expects 2 arguments".to_string())); }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("str_starts_with: first arg must be string".to_string())),
                };
                let prefix_ptr = match args[1] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("str_starts_with: second arg must be string".to_string())),
                };
                let i8_ty = self.context.i8_type();
                let i8_ptr = i8_ty.ptr_type(inkwell::AddressSpace::default());
                // Call C helper: strncmp(s, prefix, strlen(prefix)) == 0
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let prefix_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(prefix_ptr),
                ], "prefix_len")
                    .map_err(|e| CompileError::Generic(format!("strlen error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("strlen returned void")?
                    .into_int_value();
                let strncmp_fn = self.module.get_function("strncmp")
                    .or_else(|| {
                        let ty = self.context.i32_type().fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                        ], false);
                        Some(self.module.add_function("strncmp", ty, Some(inkwell::module::Linkage::External)))
                    }).ok_or_else(|| "failed to get or create strncmp function".to_string())?;
                let cmp_result = self.builder.build_call(strncmp_fn, &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::PointerValue(prefix_ptr),
                    BasicMetadataValueEnum::IntValue(prefix_len),
                ], "strncmp_call")
                    .map_err(|e| CompileError::Generic(format!("strncmp error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("strncmp returned void")?;
                let zero = self.context.i32_type().const_int(0, false);
                let eq = self.builder.build_int_compare(inkwell::IntPredicate::EQ, cmp_result.into_int_value(), zero, "starts_with")
                    .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?;
                let ext: BasicValueEnum = self.builder.build_int_z_extend(eq, self.context.i64_type(), "result")
                    .map_err(|e| CompileError::Generic(format!("zext error: {}", e)))?.into();
                Ok(ext)

    }

    pub(super) fn compile_str_ends_with(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 { return Err(CompileError::WrongArgCount("str_ends_with expects 2 arguments".to_string())); }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("str_ends_with: first arg must be string".to_string())),
                };
                let suffix_ptr = match args[1] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("str_ends_with: second arg must be string".to_string())),
                };
                let i8_ty = self.context.i8_type();
                let i8_ptr = i8_ty.ptr_type(inkwell::AddressSpace::default());
                let i64_ty = self.context.i64_type();
                // s_len = strlen(s), suffix_len = strlen(suffix)
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let s_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                ], "s_len")
                    .map_err(|e| CompileError::Generic(format!("strlen error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("strlen returned void")?
                    .into_int_value();
                let suffix_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(suffix_ptr),
                ], "suffix_len")
                    .map_err(|e| CompileError::Generic(format!("strlen error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("strlen returned void")?
                    .into_int_value();
                // If suffix_len > s_len, return false
                let gt = self.builder.build_int_compare(inkwell::IntPredicate::SGT, suffix_len, s_len, "gt")
                    .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?;
                let function = self.current_function().ok_or_else(|| "codegen: no current function for str_ends_with".to_string())?;
                let check_bb = self.context.append_basic_block(function, "check_suffix");
                let false_bb = self.context.append_basic_block(function, "suffix_false");
                let merge_bb = self.context.append_basic_block(function, "suffix_done");
                self.builder.build_conditional_branch(gt, false_bb, check_bb)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;
                // Compare s + (s_len - suffix_len) with suffix
                self.builder.position_at_end(check_bb);
                let start_pos = self.builder.build_int_sub(s_len, suffix_len, "start_pos")
                    .map_err(|e| CompileError::Generic(format!("sub error: {}", e)))?;
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let s_suffix_ptr = unsafe {
                    self.builder.build_gep(i8_ty, s_ptr, &[start_pos], "s_suffix")
                }.map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                let strncmp_fn = self.module.get_function("strncmp")
                    .or_else(|| {
                        let ty = self.context.i32_type().fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64_ty),
                        ], false);
                        Some(self.module.add_function("strncmp", ty, Some(inkwell::module::Linkage::External)))
                    }).ok_or_else(|| "failed to get or create strncmp function".to_string())?;
                let cmp_result = self.builder.build_call(strncmp_fn, &[
                    BasicMetadataValueEnum::PointerValue(s_suffix_ptr),
                    BasicMetadataValueEnum::PointerValue(suffix_ptr),
                    BasicMetadataValueEnum::IntValue(suffix_len),
                ], "strncmp_call")
                    .map_err(|e| CompileError::Generic(format!("strncmp error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("strncmp returned void")?;
                let zero = self.context.i32_type().const_int(0, false);
                let eq = self.builder.build_int_compare(inkwell::IntPredicate::EQ, cmp_result.into_int_value(), zero, "ends_with")
                    .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?;
                let eq_ext = self.builder.build_int_z_extend(eq, i64_ty, "ext")
                    .map_err(|e| CompileError::Generic(format!("zext error: {}", e)))?;
                self.builder.build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;
                // False path
                self.builder.position_at_end(false_bb);
                self.builder.build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;
                // Merge
                self.builder.position_at_end(merge_bb);
                let phi = self.builder.build_phi(i64_ty, "result")
                    .map_err(|e| CompileError::Generic(format!("phi error: {}", e)))?;
                phi.add_incoming(&[
                    (&self.context.i64_type().const_int(0, false), false_bb),
                    (&eq_ext, check_bb),
                ]);
                Ok(phi.as_basic_value())

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
                            .map_err(|e| CompileError::Generic(format!("to_int float->int error: {}", e)))?;
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
                    .map_err(|e| CompileError::Generic(format!("strtol error: {}", e)))?;
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
                            .map_err(|e| CompileError::Generic(format!("to_float int->float error: {}", e)))?;
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
                    .map_err(|e| CompileError::Generic(format!("strtod error: {}", e)))?;
                Ok(self.expect_basic_value(&call, "strtod")?)

    }

    pub(super) fn compile_str_index_of(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 { return Err(CompileError::WrongArgCount("str_index_of expects 2 arguments".to_string())); }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("str_index_of: first arg must be string".to_string())),
                };
                let sub_ptr = match args[1] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("str_index_of: second arg must be string".to_string())),
                };
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let i64_ty = self.context.i64_type();
                // strstr(s, sub) -> pointer or NULL
                let strstr_fn = self.module.get_function("strstr")
                    .or_else(|| {
                        let ty = i8_ptr.fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ], false);
                        Some(self.module.add_function("strstr", ty, Some(inkwell::module::Linkage::External)))
                    }).ok_or_else(|| "failed to get or create strstr function".to_string())?;
                let found = self.builder.build_call(strstr_fn, &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::PointerValue(sub_ptr),
                ], "strstr_call")
                    .map_err(|e| CompileError::Generic(format!("strstr error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("strstr returned void")?
                    .into_pointer_value();
                // found - s = index
                let found_int = self.builder.build_ptr_to_int(found, i64_ty, "found_int")
                    .map_err(|e| CompileError::Generic(format!("ptr_to_int error: {}", e)))?;
                let s_int = self.builder.build_ptr_to_int(s_ptr, i64_ty, "s_int")
                    .map_err(|e| CompileError::Generic(format!("ptr_to_int error: {}", e)))?;
                let idx = self.builder.build_int_sub(found_int, s_int, "index")
                    .map_err(|e| CompileError::Generic(format!("sub error: {}", e)))?;
                Ok(idx.into())

    }

    pub(super) fn compile_str_repeat(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 { return Err(CompileError::WrongArgCount("str_repeat expects 2 arguments".to_string())); }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("str_repeat: first arg must be string".to_string())),
                };
                let n = match args[1] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err(CompileError::TypeMismatch("str_repeat: second arg must be integer count".to_string())),
                };
                let i8_ty = self.context.i8_type();
                let i8_ptr = i8_ty.ptr_type(inkwell::AddressSpace::default());
                let i64_ty = self.context.i64_type();
                // strlen(s)
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let s_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                ], "s_len")
                    .map_err(|e| CompileError::Generic(format!("strlen error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("strlen returned void")?
                    .into_int_value();
                // total = s_len * n + 1 (null)
                let total = self.builder.build_int_mul(s_len, n, "total")
                    .map_err(|e| CompileError::Generic(format!("mul error: {}", e)))?;
                let one = i64_ty.const_int(1, false);
                let alloc_size = self.builder.build_int_add(total, one, "alloc_size")
                    .map_err(|e| CompileError::Generic(format!("add error: {}", e)))?;
                // malloc(total)
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let buf = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "malloc_call")
                    .map_err(|e| CompileError::Generic(format!("malloc error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                // memcpy loop (simplified: one copy + multiple memcpy)
                // First copy: memcpy(buf, s, s_len)
                let memcpy_fn = self.module.get_function("memcpy")
                    .ok_or_else(|| "memcpy not declared".to_string())?;
                self.builder.build_call(memcpy_fn, &[
                    BasicMetadataValueEnum::PointerValue(buf),
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::IntValue(s_len),
                ], "memcpy_first")
                    .map_err(|e| CompileError::Generic(format!("memcpy error: {}", e)))?;
                // For remaining repeats, copy from buf to buf+(i*s_len)
                let function = self.current_function().ok_or_else(|| "codegen: no current function for str_repeat loop".to_string())?;
                let loop_bb = self.context.append_basic_block(function, "repeat_loop");
                let body_bb = self.context.append_basic_block(function, "repeat_body");
                let done_bb = self.context.append_basic_block(function, "repeat_done");
                // i = 1 (first copy already done)
                let i_alloca = self.builder.build_alloca(i64_ty, "ri")
                    .map_err(|e| CompileError::Generic(format!("alloca error: {}", e)))?;
                self.builder.build_store(i_alloca, i64_ty.const_int(1, false))
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;
                self.builder.position_at_end(loop_bb);
                let i = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), i_alloca, "i")
                    .map_err(|e| CompileError::Generic(format!("load error: {}", e)))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, i, n, "repeat_cmp")
                    .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?;
                self.builder.build_conditional_branch(cmp, body_bb, done_bb)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;
                self.builder.position_at_end(body_bb);
                // dst = buf + i * s_len
                let offset = self.builder.build_int_mul(i, s_len, "offset")
                    .map_err(|e| CompileError::Generic(format!("mul error: {}", e)))?;
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let dst = unsafe {
                    self.builder.build_gep(i8_ty, buf, &[offset], "dst")
                }.map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                self.builder.build_call(memcpy_fn, &[
                    BasicMetadataValueEnum::PointerValue(dst),
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::IntValue(s_len),
                ], "memcpy_loop")
                    .map_err(|e| CompileError::Generic(format!("memcpy error: {}", e)))?;
                // i++
                let next = self.builder.build_int_add(i, i64_ty.const_int(1, false), "next")
                    .map_err(|e| CompileError::Generic(format!("add error: {}", e)))?;
                self.builder.build_store(i_alloca, next)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;
                self.builder.position_at_end(done_bb);
                // Null-terminate
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let null_pos = unsafe {
                    self.builder.build_gep(i8_ty, buf, &[total], "null_pos")
                }.map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                self.builder.build_store(null_pos, i8_ty.const_int(0, false))
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                // Return string struct { i8*, i64 }
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr),
                    BasicTypeEnum::IntType(i64_ty),
                ], false);
                let str_alloca = self.builder.build_alloca(string_ty, "repeat_str")
                    .map_err(|e| CompileError::Generic(format!("alloca error: {}", e)))?;
                let ptr_gep = self.builder.build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                    .map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                self.builder.build_store(ptr_gep, buf)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                let len_gep = self.builder.build_struct_gep(string_ty, str_alloca, 1, "str_len")
                    .map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                self.builder.build_store(len_gep, total)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                Ok(str_alloca.into())

    }

    pub(super) fn compile_str_trim(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 { return Err(CompileError::WrongArgCount("str_trim expects 1 argument".to_string())); }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("str_trim: first arg must be string".to_string())),
                };
                let i8_ty = self.context.i8_type();
                let i8_ptr = i8_ty.ptr_type(inkwell::AddressSpace::default());
                let i64_ty = self.context.i64_type();
                // strlen(s)
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let s_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                ], "strlen_call")
                    .map_err(|e| CompileError::Generic(format!("strlen error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("strlen returned void")?
                    .into_int_value();
                let zero = i64_ty.const_int(0, false);
                // Scan forward for first non-space
                let function = self.current_function().ok_or_else(|| "codegen: no current function for str_trim".to_string())?;
                let fwd_loop = self.context.append_basic_block(function, "trim_fwd");
                let fwd_body = self.context.append_basic_block(function, "trim_fwd_body");
                let fwd_done = self.context.append_basic_block(function, "trim_fwd_done");
                let start_alloca = self.builder.build_alloca(i64_ty, "start")
                    .map_err(|e| CompileError::Generic(format!("alloca error: {}", e)))?;
                self.builder.build_store(start_alloca, zero)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(fwd_loop)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;
                self.builder.position_at_end(fwd_loop);
                let start = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), start_alloca, "start")
                    .map_err(|e| CompileError::Generic(format!("load error: {}", e)))?.into_int_value();
                let fwd_cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, start, s_len, "fwd_cmp")
                    .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?;
                self.builder.build_conditional_branch(fwd_cmp, fwd_body, fwd_done)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;
                self.builder.position_at_end(fwd_body);
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let ch_ptr = unsafe {
                    self.builder.build_gep(i8_ty, s_ptr, &[start], "ch")
                }.map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                let ch = self.builder.build_load(BasicTypeEnum::IntType(i8_ty), ch_ptr, "ch_val")
                    .map_err(|e| CompileError::Generic(format!("load error: {}", e)))?;
                // isspace check: ch == ' ' || ch == '\t' || ch == '\n' || ch == '\r'
                let space = i8_ty.const_int(b' ' as u64, false);
                let tab = i8_ty.const_int(b'\t' as u64, false);
                let nl = i8_ty.const_int(b'\n' as u64, false);
                let cr = i8_ty.const_int(b'\r' as u64, false);
                let is_space = self.builder.build_int_compare(inkwell::IntPredicate::EQ, ch.into_int_value(), space, "is_space")
                    .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?;
                let is_tab = self.builder.build_int_compare(inkwell::IntPredicate::EQ, ch.into_int_value(), tab, "is_tab")
                    .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?;
                let is_nl = self.builder.build_int_compare(inkwell::IntPredicate::EQ, ch.into_int_value(), nl, "is_nl")
                    .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?;
                let is_cr = self.builder.build_int_compare(inkwell::IntPredicate::EQ, ch.into_int_value(), cr, "is_cr")
                    .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?;
                let is_ws1 = self.builder.build_or(is_space, is_tab, "is_ws1")
                    .map_err(|e| CompileError::Generic(format!("or error: {}", e)))?;
                let is_ws2 = self.builder.build_or(is_nl, is_cr, "is_ws2")
                    .map_err(|e| CompileError::Generic(format!("or error: {}", e)))?;
                let is_ws = self.builder.build_or(is_ws1, is_ws2, "is_ws")
                    .map_err(|e| CompileError::Generic(format!("or error: {}", e)))?;
                let next = self.builder.build_int_add(start, i64_ty.const_int(1, false), "next")
                    .map_err(|e| CompileError::Generic(format!("add error: {}", e)))?;
                // if is_ws: continue; else: done
                self.builder.build_store(start_alloca, next)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                self.builder.build_conditional_branch(is_ws, fwd_loop, fwd_done)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;
                self.builder.position_at_end(fwd_done);
                // Scan backward for last non-space
                let bwd_loop = self.context.append_basic_block(function, "trim_bwd");
                let bwd_body = self.context.append_basic_block(function, "trim_bwd_body");
                let bwd_done = self.context.append_basic_block(function, "trim_bwd_done");
                let end_alloca = self.builder.build_alloca(i64_ty, "end")
                    .map_err(|e| CompileError::Generic(format!("alloca error: {}", e)))?;
                self.builder.build_store(end_alloca, s_len)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(bwd_loop)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;
                self.builder.position_at_end(bwd_loop);
                let end = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), end_alloca, "end")
                    .map_err(|e| CompileError::Generic(format!("load error: {}", e)))?.into_int_value();
                let bwd_cmp = self.builder.build_int_compare(inkwell::IntPredicate::SGT, end, zero, "bwd_cmp")
                    .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?;
                self.builder.build_conditional_branch(bwd_cmp, bwd_body, bwd_done)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;
                self.builder.position_at_end(bwd_body);
                let prev = self.builder.build_int_sub(end, i64_ty.const_int(1, false), "prev")
                    .map_err(|e| CompileError::Generic(format!("sub error: {}", e)))?;
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let ch_ptr2 = unsafe {
                    self.builder.build_gep(i8_ty, s_ptr, &[prev], "ch")
                }.map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                let ch2 = self.builder.build_load(BasicTypeEnum::IntType(i8_ty), ch_ptr2, "ch_val")
                    .map_err(|e| CompileError::Generic(format!("load error: {}", e)))?;
                let is_ws2_1 = self.builder.build_int_compare(inkwell::IntPredicate::EQ, ch2.into_int_value(), space, "is_space")
                    .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?;
                let is_ws2_2 = self.builder.build_int_compare(inkwell::IntPredicate::EQ, ch2.into_int_value(), tab, "is_tab")
                    .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?;
                let is_ws2_3 = self.builder.build_int_compare(inkwell::IntPredicate::EQ, ch2.into_int_value(), nl, "is_nl")
                    .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?;
                let is_ws2_4 = self.builder.build_int_compare(inkwell::IntPredicate::EQ, ch2.into_int_value(), cr, "is_cr")
                    .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?;
                let is_ws2a = self.builder.build_or(is_ws2_1, is_ws2_2, "is_ws_a")
                    .map_err(|e| CompileError::Generic(format!("or error: {}", e)))?;
                let is_ws2b = self.builder.build_or(is_ws2_3, is_ws2_4, "is_ws_b")
                    .map_err(|e| CompileError::Generic(format!("or error: {}", e)))?;
                let is_ws2 = self.builder.build_or(is_ws2a, is_ws2b, "is_ws")
                    .map_err(|e| CompileError::Generic(format!("or error: {}", e)))?;
                self.builder.build_store(end_alloca, prev)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                self.builder.build_conditional_branch(is_ws2, bwd_loop, bwd_done)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;
                self.builder.position_at_end(bwd_done);
                // result = substr(start, end - start)
                let trimmed_len = self.builder.build_int_sub(end, start, "trimmed_len")
                    .map_err(|e| CompileError::Generic(format!("sub error: {}", e)))?;
                // malloc + memcpy
                let alloc_size = self.builder.build_int_add(trimmed_len, i64_ty.const_int(1, false), "alloc_size")
                    .map_err(|e| CompileError::Generic(format!("add error: {}", e)))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let buf = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "malloc_call")
                    .map_err(|e| CompileError::Generic(format!("malloc error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let src = unsafe {
                    self.builder.build_gep(i8_ty, s_ptr, &[start], "src")
                }.map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                let memcpy_fn = self.module.get_function("memcpy")
                    .ok_or_else(|| "memcpy not declared".to_string())?;
                self.builder.build_call(memcpy_fn, &[
                    BasicMetadataValueEnum::PointerValue(buf),
                    BasicMetadataValueEnum::PointerValue(src),
                    BasicMetadataValueEnum::IntValue(trimmed_len),
                ], "memcpy_call")
                    .map_err(|e| CompileError::Generic(format!("memcpy error: {}", e)))?;
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let null_pos = unsafe {
                    self.builder.build_gep(i8_ty, buf, &[trimmed_len], "null")
                }.map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                self.builder.build_store(null_pos, i8_ty.const_int(0, false))
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                // Build string struct { i8*, i64 }
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr),
                    BasicTypeEnum::IntType(i64_ty),
                ], false);
                let str_alloca = self.builder.build_alloca(string_ty, "trim_str")
                    .map_err(|e| CompileError::Generic(format!("alloca error: {}", e)))?;
                let ptr_gep = self.builder.build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                    .map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                self.builder.build_store(ptr_gep, buf)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                let len_gep = self.builder.build_struct_gep(string_ty, str_alloca, 1, "str_len")
                    .map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                self.builder.build_store(len_gep, trimmed_len)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                Ok(str_alloca.into())

    }

    pub(super) fn compile_str_to_upper(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 { return Err(CompileError::WrongArgCount("str_to_upper expects 1 argument".to_string())); }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("str_to_upper: first arg must be string".to_string())),
                };
                let i8_ty = self.context.i8_type();
                let i8_ptr = i8_ty.ptr_type(inkwell::AddressSpace::default());
                let i64_ty = self.context.i64_type();
                // strlen, malloc copy + toupper each char
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let s_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                ], "strlen_call")
                    .map_err(|e| CompileError::Generic(format!("strlen error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("strlen returned void")?
                    .into_int_value();
                let alloc_size = self.builder.build_int_add(s_len, i64_ty.const_int(1, false), "alloc_size")
                    .map_err(|e| CompileError::Generic(format!("add error: {}", e)))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let buf = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "malloc_call")
                    .map_err(|e| CompileError::Generic(format!("malloc error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                // Copy s to buf first, then transform
                let memcpy_fn = self.module.get_function("memcpy")
                    .ok_or_else(|| "memcpy not declared".to_string())?;
                self.builder.build_call(memcpy_fn, &[
                    BasicMetadataValueEnum::PointerValue(buf),
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "memcpy_call")
                    .map_err(|e| CompileError::Generic(format!("memcpy error: {}", e)))?;
                // Loop: for i = 0..s_len: if buf[i] in 'a'..'z', buf[i] -= 32
                let function = self.current_function().ok_or_else(|| "codegen: no current function for str_to_upper loop".to_string())?;
                let loop_bb = self.context.append_basic_block(function, "upper_loop");
                let body_bb = self.context.append_basic_block(function, "upper_body");
                let done_bb = self.context.append_basic_block(function, "upper_done");
                let i_alloca = self.builder.build_alloca(i64_ty, "ui")
                    .map_err(|e| CompileError::Generic(format!("alloca error: {}", e)))?;
                self.builder.build_store(i_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;
                self.builder.position_at_end(loop_bb);
                let i = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), i_alloca, "i")
                    .map_err(|e| CompileError::Generic(format!("load error: {}", e)))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, i, s_len, "upper_cmp")
                    .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?;
                self.builder.build_conditional_branch(cmp, body_bb, done_bb)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;
                self.builder.position_at_end(body_bb);
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let ch_ptr = unsafe {
                    self.builder.build_gep(i8_ty, buf, &[i], "ch")
                }.map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                let ch = self.builder.build_load(BasicTypeEnum::IntType(i8_ty), ch_ptr, "ch_val")
                    .map_err(|e| CompileError::Generic(format!("load error: {}", e)))?.into_int_value();
                // Check 'a' <= ch <= 'z'
                let a = i8_ty.const_int(b'a' as u64, false);
                let z = i8_ty.const_int(b'z' as u64, false);
                let is_lower1 = self.builder.build_int_compare(inkwell::IntPredicate::SGE, ch, a, "ge_a")
                    .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?;
                let is_lower2 = self.builder.build_int_compare(inkwell::IntPredicate::SLE, ch, z, "le_z")
                    .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?;
                let is_lower = self.builder.build_and(is_lower1, is_lower2, "is_lower")
                    .map_err(|e| CompileError::Generic(format!("and error: {}", e)))?;
                let upper_ch = self.builder.build_int_sub(ch, i8_ty.const_int(32, false), "upper")
                    .map_err(|e| CompileError::Generic(format!("sub error: {}", e)))?;
                let result_ch = self.builder.build_select(is_lower, upper_ch, ch, "result_ch")
                    .map_err(|e| CompileError::Generic(format!("select error: {}", e)))?;
                self.builder.build_store(ch_ptr, result_ch)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                let next = self.builder.build_int_add(i, i64_ty.const_int(1, false), "next")
                    .map_err(|e| CompileError::Generic(format!("add error: {}", e)))?;
                self.builder.build_store(i_alloca, next)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;
                self.builder.position_at_end(done_bb);
                // Return string struct
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr),
                    BasicTypeEnum::IntType(i64_ty),
                ], false);
                let str_alloca = self.builder.build_alloca(string_ty, "upper_str")
                    .map_err(|e| CompileError::Generic(format!("alloca error: {}", e)))?;
                let ptr_gep = self.builder.build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                    .map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                self.builder.build_store(ptr_gep, buf)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                let len_gep = self.builder.build_struct_gep(string_ty, str_alloca, 1, "str_len")
                    .map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                self.builder.build_store(len_gep, s_len)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                Ok(str_alloca.into())

    }

    pub(super) fn compile_str_to_lower(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 { return Err(CompileError::WrongArgCount("str_to_lower expects 1 argument".to_string())); }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("str_to_lower: first arg must be string".to_string())),
                };
                let i8_ty = self.context.i8_type();
                let i8_ptr = i8_ty.ptr_type(inkwell::AddressSpace::default());
                let i64_ty = self.context.i64_type();
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let s_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                ], "strlen_call")
                    .map_err(|e| CompileError::Generic(format!("strlen error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("strlen returned void")?
                    .into_int_value();
                let alloc_size = self.builder.build_int_add(s_len, i64_ty.const_int(1, false), "alloc_size")
                    .map_err(|e| CompileError::Generic(format!("add error: {}", e)))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let buf = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "malloc_call")
                    .map_err(|e| CompileError::Generic(format!("malloc error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                let memcpy_fn = self.module.get_function("memcpy")
                    .ok_or_else(|| "memcpy not declared".to_string())?;
                self.builder.build_call(memcpy_fn, &[
                    BasicMetadataValueEnum::PointerValue(buf),
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "memcpy_call")
                    .map_err(|e| CompileError::Generic(format!("memcpy error: {}", e)))?;
                let function = self.current_function().ok_or_else(|| "codegen: no current function for str_to_lower loop".to_string())?;
                let loop_bb = self.context.append_basic_block(function, "lower_loop");
                let body_bb = self.context.append_basic_block(function, "lower_body");
                let done_bb = self.context.append_basic_block(function, "lower_done");
                let i_alloca = self.builder.build_alloca(i64_ty, "li")
                    .map_err(|e| CompileError::Generic(format!("alloca error: {}", e)))?;
                self.builder.build_store(i_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;
                self.builder.position_at_end(loop_bb);
                let i = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), i_alloca, "i")
                    .map_err(|e| CompileError::Generic(format!("load error: {}", e)))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, i, s_len, "lower_cmp")
                    .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?;
                self.builder.build_conditional_branch(cmp, body_bb, done_bb)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;
                self.builder.position_at_end(body_bb);
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let ch_ptr = unsafe {
                    self.builder.build_gep(i8_ty, buf, &[i], "ch")
                }.map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                let ch = self.builder.build_load(BasicTypeEnum::IntType(i8_ty), ch_ptr, "ch_val")
                    .map_err(|e| CompileError::Generic(format!("load error: {}", e)))?.into_int_value();
                let a_up = i8_ty.const_int(b'A' as u64, false);
                let z_up = i8_ty.const_int(b'Z' as u64, false);
                let is_upper1 = self.builder.build_int_compare(inkwell::IntPredicate::SGE, ch, a_up, "ge_A")
                    .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?;
                let is_upper2 = self.builder.build_int_compare(inkwell::IntPredicate::SLE, ch, z_up, "le_Z")
                    .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?;
                let is_upper = self.builder.build_and(is_upper1, is_upper2, "is_upper")
                    .map_err(|e| CompileError::Generic(format!("and error: {}", e)))?;
                let lower_ch = self.builder.build_int_add(ch, i8_ty.const_int(32, false), "lower")
                    .map_err(|e| CompileError::Generic(format!("add error: {}", e)))?;
                let result_ch = self.builder.build_select(is_upper, lower_ch, ch, "result_ch")
                    .map_err(|e| CompileError::Generic(format!("select error: {}", e)))?;
                self.builder.build_store(ch_ptr, result_ch)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                let next = self.builder.build_int_add(i, i64_ty.const_int(1, false), "next")
                    .map_err(|e| CompileError::Generic(format!("add error: {}", e)))?;
                self.builder.build_store(i_alloca, next)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;
                self.builder.position_at_end(done_bb);
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr),
                    BasicTypeEnum::IntType(i64_ty),
                ], false);
                let str_alloca = self.builder.build_alloca(string_ty, "lower_str")
                    .map_err(|e| CompileError::Generic(format!("alloca error: {}", e)))?;
                let ptr_gep = self.builder.build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                    .map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                self.builder.build_store(ptr_gep, buf)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                let len_gep = self.builder.build_struct_gep(string_ty, str_alloca, 1, "str_len")
                    .map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                self.builder.build_store(len_gep, s_len)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                Ok(str_alloca.into())

    }

    pub(super) fn compile_str_substring(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 3 { return Err(CompileError::WrongArgCount("str_substring expects 3 arguments (s, start, end)".to_string())); }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("str_substring: first arg must be string".to_string())),
                };
                let start = match args[1] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err(CompileError::TypeMismatch("str_substring: second arg must be integer start".to_string())),
                };
                let end = match args[2] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err(CompileError::TypeMismatch("str_substring: third arg must be integer end".to_string())),
                };
                let i8_ty = self.context.i8_type();
                let i8_ptr = i8_ty.ptr_type(inkwell::AddressSpace::default());
                let i64_ty = self.context.i64_type();
                // len = end - start
                let sub_len = self.builder.build_int_sub(end, start, "sub_len")
                    .map_err(|e| CompileError::Generic(format!("sub error: {}", e)))?;
                let alloc_size = self.builder.build_int_add(sub_len, i64_ty.const_int(1, false), "alloc_size")
                    .map_err(|e| CompileError::Generic(format!("add error: {}", e)))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let buf = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "malloc_call")
                    .map_err(|e| CompileError::Generic(format!("malloc error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                // src = s + start
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let src = unsafe {
                    self.builder.build_gep(i8_ty, s_ptr, &[start], "src")
                }.map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                // memcpy(buf, src, sub_len)
                let memcpy_fn = self.module.get_function("memcpy")
                    .ok_or_else(|| "memcpy not declared".to_string())?;
                self.builder.build_call(memcpy_fn, &[
                    BasicMetadataValueEnum::PointerValue(buf),
                    BasicMetadataValueEnum::PointerValue(src),
                    BasicMetadataValueEnum::IntValue(sub_len),
                ], "memcpy_call")
                    .map_err(|e| CompileError::Generic(format!("memcpy error: {}", e)))?;
                // Null-terminate
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let null_pos = unsafe {
                    self.builder.build_gep(i8_ty, buf, &[sub_len], "null")
                }.map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                self.builder.build_store(null_pos, i8_ty.const_int(0, false))
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                // Build string struct
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr),
                    BasicTypeEnum::IntType(i64_ty),
                ], false);
                let str_alloca = self.builder.build_alloca(string_ty, "sub_str")
                    .map_err(|e| CompileError::Generic(format!("alloca error: {}", e)))?;
                let ptr_gep = self.builder.build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                    .map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                self.builder.build_store(ptr_gep, buf)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                let len_gep = self.builder.build_struct_gep(string_ty, str_alloca, 1, "str_len")
                    .map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                self.builder.build_store(len_gep, sub_len)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                Ok(str_alloca.into())

    }

    pub(super) fn compile_str_split(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 { return Err(CompileError::WrongArgCount("str_split expects 2 arguments (string, delimiter)".to_string())); }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("str_split: first arg must be string".to_string())),
                };
                let delim_ptr = match args[1] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("str_split: second arg must be string".to_string())),
                };
                let func = self.module.get_function("mimi_str_split")
                    .ok_or("mimi_str_split not declared")?;
                let result_ptr = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::PointerValue(delim_ptr),
                ], "str_split_call")
                    .map_err(|e| CompileError::Generic(format!("str_split error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("mimi_str_split returned void")?
                    .into_pointer_value();
                // MimiList* is {i64 len, const char** data} — same layout as our list struct
                let i64_ty = self.context.i64_type();
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let list_struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(i8_ptr),
                ], false);
                let list_ptr = self.builder.build_bit_cast(result_ptr,
                    list_struct_ty.ptr_type(inkwell::AddressSpace::default()), "list_ptr")
                    .map_err(|e| CompileError::Generic(format!("bitcast error: {}", e)))?
                    .into_pointer_value();
                let len_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 0, "len")
                    .map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                let data_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 1, "data")
                    .map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                let len_val = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "len_val")
                    .map_err(|e| CompileError::Generic(format!("load error: {}", e)))?;
                let data_val = self.builder.build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "data_val")
                    .map_err(|e| CompileError::Generic(format!("load error: {}", e)))?;
                let result_struct = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(i8_ptr),
                ], false);
                let result_alloca = self.builder.build_alloca(result_struct, "str_split_result")
                    .map_err(|e| CompileError::Generic(format!("alloca error: {}", e)))?;
                let r_len_gep = self.builder.build_struct_gep(result_struct, result_alloca, 0, "r_len")
                    .map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                let r_data_gep = self.builder.build_struct_gep(result_struct, result_alloca, 1, "r_data")
                    .map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                self.builder.build_store(r_len_gep, len_val)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                self.builder.build_store(r_data_gep, data_val)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                Ok(result_alloca.into())

    }

    pub(super) fn compile_str_join(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 { return Err(CompileError::WrongArgCount("str_join expects 2 arguments (list, separator)".to_string())); }
                let list_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("str_join: first arg must be list".to_string())),
                };
                let sep_ptr = match args[1] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("str_join: second arg must be string".to_string())),
                };
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                // Bitcast list pointer to i8* for C function
                let c_list_ptr = self.builder.build_bit_cast(list_ptr,
                    i8_ptr, "c_list_ptr")
                    .map_err(|e| CompileError::Generic(format!("bitcast error: {}", e)))?
                    .into_pointer_value();
                let func = self.module.get_function("mimi_str_join")
                    .ok_or("mimi_str_join not declared")?;
                let result = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::PointerValue(c_list_ptr),
                    BasicMetadataValueEnum::PointerValue(sep_ptr),
                ], "str_join_call")
                    .map_err(|e| CompileError::Generic(format!("str_join error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("mimi_str_join returned void")?;
                Ok(result)

    }

    pub(super) fn compile_str_replace(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 3 { return Err(CompileError::WrongArgCount("str_replace expects 3 arguments".to_string())); }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("str_replace: first arg must be string".to_string())),
                };
                let from_ptr = match args[1] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("str_replace: second arg must be string".to_string())),
                };
                let to_ptr = match args[2] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("str_replace: third arg must be string".to_string())),
                };
                let func = self.module.get_function("mimi_str_replace")
                    .ok_or("mimi_str_replace not declared")?;
                let result = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::PointerValue(from_ptr),
                    BasicMetadataValueEnum::PointerValue(to_ptr),
                ], "str_replace_call")
                    .map_err(|e| CompileError::Generic(format!("str_replace error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("mimi_str_replace returned void")?;
                Ok(result)

    }

    pub(super) fn compile_str_to_c_str(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                // Extract the raw C string pointer from a Mimi string
                if args.len() != 1 { return Err(CompileError::WrongArgCount("str_to_c_str expects 1 argument".to_string())); }
                let c_ptr = self.extract_raw_str_ptr(&args[0])?;
                Ok(c_ptr.into())

    }

    pub(super) fn compile_c_str_to_string(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                // Wrap a raw C string pointer into a Mimi string struct {i8*, i64}
                if args.len() != 1 { return Err(CompileError::WrongArgCount("c_str_to_string expects 1 argument".to_string())); }
                let raw_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("c_str_to_string: argument must be a raw C string pointer".to_string())),
                };
                let i8_ptr_ty = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    BasicTypeEnum::IntType(self.context.i64_type()),
                ], false);
                let str_alloca = self.builder.build_alloca(string_ty, "cstr_str")
                    .map_err(|e| CompileError::Generic(format!("alloca error: {}", e)))?;
                let ptr_gep = self.builder.build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                    .map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                self.builder.build_store(ptr_gep, raw_ptr)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                let len_gep = self.builder.build_struct_gep(string_ty, str_alloca, 1, "str_len")
                    .map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let str_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(raw_ptr),
                ], "strlen_call")
                    .map_err(|e| CompileError::Generic(format!("strlen error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("strlen returned void")?;
                self.builder.build_store(len_gep, str_len)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                Ok(str_alloca.into())

    }

}
