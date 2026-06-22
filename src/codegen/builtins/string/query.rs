use crate::codegen::CodeGenerator;
use inkwell::types::BasicMetadataTypeEnum;
use crate::codegen::CallSiteValueExt;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use crate::error::{CompileError, MimiResult};

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn compile_str_contains(
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
                    .map_err(|e| CompileError::LlvmError(format!("strstr error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("strstr returned void")?;
                let cmp = self.builder.build_is_not_null(result.into_pointer_value(), "found")
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                let ext: BasicValueEnum = self.builder.build_int_z_extend(cmp, self.context.i64_type(), "result")
                    .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?.into();
                Ok(ext)

    }

    pub(in crate::codegen) fn compile_str_starts_with(
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
                    .map_err(|e| CompileError::LlvmError(format!("strlen error: {}", e)))?
                    .try_as_basic_value_opt()
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
                    .map_err(|e| CompileError::LlvmError(format!("strncmp error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("strncmp returned void")?;
                let zero = self.context.i32_type().const_int(0, false);
                let eq = self.builder.build_int_compare(inkwell::IntPredicate::EQ, cmp_result.into_int_value(), zero, "starts_with")
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                let ext: BasicValueEnum = self.builder.build_int_z_extend(eq, self.context.i64_type(), "result")
                    .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?.into();
                Ok(ext)

    }

    pub(in crate::codegen) fn compile_str_ends_with(
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
                    .map_err(|e| CompileError::LlvmError(format!("strlen error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("strlen returned void")?
                    .into_int_value();
                let suffix_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(suffix_ptr),
                ], "suffix_len")
                    .map_err(|e| CompileError::LlvmError(format!("strlen error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("strlen returned void")?
                    .into_int_value();
                // If suffix_len > s_len, return false
                let gt = self.builder.build_int_compare(inkwell::IntPredicate::SGT, suffix_len, s_len, "gt")
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                let function = self.current_function().ok_or_else(|| "codegen: no current function for str_ends_with".to_string())?;
                let check_bb = self.context.append_basic_block(function, "check_suffix");
                let false_bb = self.context.append_basic_block(function, "suffix_false");
                let merge_bb = self.context.append_basic_block(function, "suffix_done");
                self.builder.build_conditional_branch(gt, false_bb, check_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                // Compare s + (s_len - suffix_len) with suffix
                self.builder.position_at_end(check_bb);
                let start_pos = self.builder.build_int_sub(s_len, suffix_len, "start_pos")
                    .map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?;
                // SAFETY: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let s_suffix_ptr = unsafe {
                    self.builder.build_gep(i8_ty, s_ptr, &[start_pos], "s_suffix")
                }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
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
                    .map_err(|e| CompileError::LlvmError(format!("strncmp error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("strncmp returned void")?;
                let zero = self.context.i32_type().const_int(0, false);
                let eq = self.builder.build_int_compare(inkwell::IntPredicate::EQ, cmp_result.into_int_value(), zero, "ends_with")
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                let eq_ext = self.builder.build_int_z_extend(eq, i64_ty, "ext")
                    .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?;
                self.builder.build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                // False path
                self.builder.position_at_end(false_bb);
                self.builder.build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                // Merge
                self.builder.position_at_end(merge_bb);
                let phi = self.builder.build_phi(i64_ty, "result")
                    .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
                phi.add_incoming(&[
                    (&self.context.i64_type().const_int(0, false), false_bb),
                    (&eq_ext, check_bb),
                ]);
                Ok(phi.as_basic_value())

    }
    pub(in crate::codegen) fn compile_regex_match(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 { return Err(CompileError::WrongArgCount("regex_match expects 2 arguments (text, pattern)".to_string())); }
        let text_ptr = match args[0] {
            BasicMetadataValueEnum::PointerValue(pv) => pv,
            _ => return Err(CompileError::TypeMismatch("regex_match: first arg must be string".to_string())),
        };
        let pattern_ptr = match args[1] {
            BasicMetadataValueEnum::PointerValue(pv) => pv,
            _ => return Err(CompileError::TypeMismatch("regex_match: second arg must be string".to_string())),
        };
        let func = self.module.get_function("mimi_regex_match")
            .ok_or("mimi_regex_match not declared")?;
        let result = self.builder.build_call(func, &[
            BasicMetadataValueEnum::PointerValue(text_ptr),
            BasicMetadataValueEnum::PointerValue(pattern_ptr),
        ], "regex_match_call")
            .map_err(|e| CompileError::LlvmError(format!("regex_match error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_regex_match returned void")?;
        let cmp = self.builder.build_int_compare(
            inkwell::IntPredicate::NE,
            result.into_int_value(),
            self.context.i32_type().const_int(0, false),
            "regex_match_bool",
        ).map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let ext = self.builder.build_int_z_extend(cmp, self.context.i64_type(), "result")
            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?;
        Ok(ext.into())
    }

    pub(in crate::codegen) fn compile_regex_find(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 { return Err(CompileError::WrongArgCount("regex_find expects 2 arguments (text, pattern)".to_string())); }
        let text_ptr = match args[0] {
            BasicMetadataValueEnum::PointerValue(pv) => pv,
            _ => return Err(CompileError::TypeMismatch("regex_find: first arg must be string".to_string())),
        };
        let pattern_ptr = match args[1] {
            BasicMetadataValueEnum::PointerValue(pv) => pv,
            _ => return Err(CompileError::TypeMismatch("regex_find: second arg must be string".to_string())),
        };
        let func = self.module.get_function("mimi_regex_find")
            .ok_or("mimi_regex_find not declared")?;
        let result = self.builder.build_call(func, &[
            BasicMetadataValueEnum::PointerValue(text_ptr),
            BasicMetadataValueEnum::PointerValue(pattern_ptr),
        ], "regex_find_call")
            .map_err(|e| CompileError::LlvmError(format!("regex_find error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_regex_find returned void")?;
        Ok(result)
    }

    pub(in crate::codegen) fn compile_regex_replace(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 3 { return Err(CompileError::WrongArgCount("regex_replace expects 3 arguments (text, pattern, replacement)".to_string())); }
        let text_ptr = match args[0] {
            BasicMetadataValueEnum::PointerValue(pv) => pv,
            _ => return Err(CompileError::TypeMismatch("regex_replace: first arg must be string".to_string())),
        };
        let pattern_ptr = match args[1] {
            BasicMetadataValueEnum::PointerValue(pv) => pv,
            _ => return Err(CompileError::TypeMismatch("regex_replace: second arg must be string".to_string())),
        };
        let replacement_ptr = match args[2] {
            BasicMetadataValueEnum::PointerValue(pv) => pv,
            _ => return Err(CompileError::TypeMismatch("regex_replace: third arg must be string".to_string())),
        };
        let func = self.module.get_function("mimi_regex_replace")
            .ok_or("mimi_regex_replace not declared")?;
        let result = self.builder.build_call(func, &[
            BasicMetadataValueEnum::PointerValue(text_ptr),
            BasicMetadataValueEnum::PointerValue(pattern_ptr),
            BasicMetadataValueEnum::PointerValue(replacement_ptr),
        ], "regex_replace_call")
            .map_err(|e| CompileError::LlvmError(format!("regex_replace error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_regex_replace returned void")?;
        Ok(result)
    }

    pub(in crate::codegen) fn compile_str_index_of(
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
                    .map_err(|e| CompileError::LlvmError(format!("strstr error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("strstr returned void")?
                    .into_pointer_value();
                // found - s = index
                let found_int = self.builder.build_ptr_to_int(found, i64_ty, "found_int")
                    .map_err(|e| CompileError::LlvmError(format!("ptr_to_int error: {}", e)))?;
                let s_int = self.builder.build_ptr_to_int(s_ptr, i64_ty, "s_int")
                    .map_err(|e| CompileError::LlvmError(format!("ptr_to_int error: {}", e)))?;
                let idx = self.builder.build_int_sub(found_int, s_int, "index")
                    .map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?;
                Ok(idx.into())

    }
}
