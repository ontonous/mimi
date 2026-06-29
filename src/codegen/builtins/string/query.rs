use crate::codegen::CallSiteValueExt;
use crate::codegen::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn compile_str_contains(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "str_contains expects 2 arguments".to_string(),
            ));
        }
        let s_ptr = match &args[0] {
            BasicMetadataValueEnum::PointerValue(pv) => *pv,
            BasicMetadataValueEnum::StructValue(sv) => self
                .builder
                .build_extract_value(*sv, 0, "str_ptr")
                .map_err(|e| CompileError::LlvmError(format!("extract str ptr: {}", e)))?
                .into_pointer_value(),
            _ => {
                return Err(CompileError::TypeMismatch(
                    "str_contains: first arg must be string".to_string(),
                ))
            }
        };
        let sub_ptr = match &args[1] {
            BasicMetadataValueEnum::PointerValue(pv) => *pv,
            BasicMetadataValueEnum::StructValue(sv) => self
                .builder
                .build_extract_value(*sv, 0, "str_ptr")
                .map_err(|e| CompileError::LlvmError(format!("extract str ptr: {}", e)))?
                .into_pointer_value(),
            _ => {
                return Err(CompileError::TypeMismatch(
                    "str_contains: second arg must be string".to_string(),
                ))
            }
        };
        // strstr(s, sub) -> i8* (or NULL if not found)
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let strstr_fn = self
            .module
            .get_function("strstr")
            .or_else(|| {
                let ty = i8_ptr.fn_type(
                    &[
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                    ],
                    false,
                );
                Some(self.module.add_function(
                    "strstr",
                    ty,
                    Some(inkwell::module::Linkage::External),
                ))
            })
            .ok_or_else(|| "failed to get or create strstr function".to_string())?;
        let result = self
            .builder
            .build_call(
                strstr_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::PointerValue(sub_ptr),
                ],
                "strstr_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("strstr error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("strstr returned void")?;
        let cmp = self
            .builder
            .build_is_not_null(result.into_pointer_value(), "found")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let ext: BasicValueEnum = self
            .builder
            .build_int_z_extend(cmp, self.context.i64_type(), "result")
            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?
            .into();
        Ok(ext)
    }

    pub(in crate::codegen) fn compile_str_starts_with(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "str_starts_with expects 2 arguments".to_string(),
            ));
        }
        let s_ptr = match &args[0] {
            BasicMetadataValueEnum::PointerValue(pv) => *pv,
            BasicMetadataValueEnum::StructValue(sv) => self
                .builder
                .build_extract_value(*sv, 0, "str_ptr")
                .map_err(|e| CompileError::LlvmError(format!("extract str ptr: {}", e)))?
                .into_pointer_value(),
            _ => {
                return Err(CompileError::TypeMismatch(
                    "str_starts_with: first arg must be string".to_string(),
                ))
            }
        };
        let prefix_ptr = match &args[1] {
            BasicMetadataValueEnum::PointerValue(pv) => *pv,
            BasicMetadataValueEnum::StructValue(sv) => self
                .builder
                .build_extract_value(*sv, 0, "str_ptr")
                .map_err(|e| CompileError::LlvmError(format!("extract str ptr: {}", e)))?
                .into_pointer_value(),
            _ => {
                return Err(CompileError::TypeMismatch(
                    "str_starts_with: second arg must be string".to_string(),
                ))
            }
        };
        let _i8_ty = self.context.i8_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        // Call C helper: strncmp(s, prefix, strlen(prefix)) == 0
        let strlen_fn = self
            .module
            .get_function("strlen")
            .ok_or_else(|| "strlen not declared".to_string())?;
        let prefix_len = self
            .builder
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(prefix_ptr)],
                "prefix_len",
            )
            .map_err(|e| CompileError::LlvmError(format!("strlen error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("strlen returned void")?
            .into_int_value();
        let strncmp_fn = self
            .module
            .get_function("strncmp")
            .or_else(|| {
                let ty = self.context.i32_type().fn_type(
                    &[
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                        BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                    ],
                    false,
                );
                Some(self.module.add_function(
                    "strncmp",
                    ty,
                    Some(inkwell::module::Linkage::External),
                ))
            })
            .ok_or_else(|| "failed to get or create strncmp function".to_string())?;
        let cmp_result = self
            .builder
            .build_call(
                strncmp_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::PointerValue(prefix_ptr),
                    BasicMetadataValueEnum::IntValue(prefix_len),
                ],
                "strncmp_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("strncmp error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("strncmp returned void")?;
        let zero = self.context.i32_type().const_int(0, false);
        let eq = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                cmp_result.into_int_value(),
                zero,
                "starts_with",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let ext: BasicValueEnum = self
            .builder
            .build_int_z_extend(eq, self.context.i64_type(), "result")
            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?
            .into();
        Ok(ext)
    }

    pub(in crate::codegen) fn compile_str_ends_with(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "str_ends_with expects 2 arguments".to_string(),
            ));
        }
        let s_ptr = match &args[0] {
            BasicMetadataValueEnum::PointerValue(pv) => *pv,
            BasicMetadataValueEnum::StructValue(sv) => self
                .builder
                .build_extract_value(*sv, 0, "str_ptr")
                .map_err(|e| CompileError::LlvmError(format!("extract str ptr: {}", e)))?
                .into_pointer_value(),
            _ => {
                return Err(CompileError::TypeMismatch(
                    "str_ends_with: first arg must be string".to_string(),
                ))
            }
        };
        let suffix_ptr = match &args[1] {
            BasicMetadataValueEnum::PointerValue(pv) => *pv,
            BasicMetadataValueEnum::StructValue(sv) => self
                .builder
                .build_extract_value(*sv, 0, "str_ptr")
                .map_err(|e| CompileError::LlvmError(format!("extract str ptr: {}", e)))?
                .into_pointer_value(),
            _ => {
                return Err(CompileError::TypeMismatch(
                    "str_ends_with: second arg must be string".to_string(),
                ))
            }
        };
        let i8_ty = self.context.i8_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        // s_len = strlen(s), suffix_len = strlen(suffix)
        let strlen_fn = self
            .module
            .get_function("strlen")
            .ok_or_else(|| "strlen not declared".to_string())?;
        let s_len = self
            .builder
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(s_ptr)],
                "s_len",
            )
            .map_err(|e| CompileError::LlvmError(format!("strlen error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("strlen returned void")?
            .into_int_value();
        let suffix_len = self
            .builder
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(suffix_ptr)],
                "suffix_len",
            )
            .map_err(|e| CompileError::LlvmError(format!("strlen error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("strlen returned void")?
            .into_int_value();
        // If suffix_len > s_len, return false
        let gt = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, suffix_len, s_len, "gt")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for str_ends_with".to_string())?;
        let check_bb = self.context.append_basic_block(function, "check_suffix");
        let false_bb = self.context.append_basic_block(function, "suffix_false");
        let merge_bb = self.context.append_basic_block(function, "suffix_done");
        self.builder
            .build_conditional_branch(gt, false_bb, check_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        // Compare s + (s_len - suffix_len) with suffix
        self.builder.position_at_end(check_bb);
        let start_pos = self
            .builder
            .build_int_sub(s_len, suffix_len, "start_pos")
            .map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?;
        let s_suffix_ptr = {
            self.gep()
                .build_in_bounds_gep(i8_ty, s_ptr, &[start_pos], "s_suffix")
        }
        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let strncmp_fn = self
            .module
            .get_function("strncmp")
            .or_else(|| {
                let ty = self.context.i32_type().fn_type(
                    &[
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                        BasicMetadataTypeEnum::IntType(i64_ty),
                    ],
                    false,
                );
                Some(self.module.add_function(
                    "strncmp",
                    ty,
                    Some(inkwell::module::Linkage::External),
                ))
            })
            .ok_or_else(|| "failed to get or create strncmp function".to_string())?;
        let cmp_result = self
            .builder
            .build_call(
                strncmp_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(s_suffix_ptr),
                    BasicMetadataValueEnum::PointerValue(suffix_ptr),
                    BasicMetadataValueEnum::IntValue(suffix_len),
                ],
                "strncmp_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("strncmp error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("strncmp returned void")?;
        let zero = self.context.i32_type().const_int(0, false);
        let eq = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                cmp_result.into_int_value(),
                zero,
                "ends_with",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let eq_ext = self
            .builder
            .build_int_z_extend(eq, i64_ty, "ext")
            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?;
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        // False path
        self.builder.position_at_end(false_bb);
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        // Merge
        self.builder.position_at_end(merge_bb);
        let phi = self
            .builder
            .build_phi(i64_ty, "result")
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
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "regex_match expects 2 arguments (text, pattern)".to_string(),
            ));
        }
        let text_ptr = match &args[0] {
            BasicMetadataValueEnum::PointerValue(pv) => *pv,
            BasicMetadataValueEnum::StructValue(sv) => self
                .builder
                .build_extract_value(*sv, 0, "str_ptr")
                .map_err(|e| CompileError::LlvmError(format!("extract str ptr: {}", e)))?
                .into_pointer_value(),
            _ => {
                return Err(CompileError::TypeMismatch(
                    "regex_match: first arg must be string".to_string(),
                ))
            }
        };
        let pattern_ptr = match &args[1] {
            BasicMetadataValueEnum::PointerValue(pv) => *pv,
            BasicMetadataValueEnum::StructValue(sv) => self
                .builder
                .build_extract_value(*sv, 0, "str_ptr")
                .map_err(|e| CompileError::LlvmError(format!("extract str ptr: {}", e)))?
                .into_pointer_value(),
            _ => {
                return Err(CompileError::TypeMismatch(
                    "regex_match: second arg must be string".to_string(),
                ))
            }
        };
        let func = self
            .module
            .get_function("mimi_regex_match")
            .ok_or("mimi_regex_match not declared")?;
        let result = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(text_ptr),
                    BasicMetadataValueEnum::PointerValue(pattern_ptr),
                ],
                "regex_match_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("regex_match error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_regex_match returned void")?;
        let cmp = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::NE,
                result.into_int_value(),
                self.context.i32_type().const_int(0, false),
                "regex_match_bool",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let ext = self
            .builder
            .build_int_z_extend(cmp, self.context.i64_type(), "result")
            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?;
        Ok(ext.into())
    }

    pub(in crate::codegen) fn compile_regex_find(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "regex_find expects 2 arguments (text, pattern)".to_string(),
            ));
        }
        let text_ptr = match &args[0] {
            BasicMetadataValueEnum::PointerValue(pv) => *pv,
            BasicMetadataValueEnum::StructValue(sv) => self
                .builder
                .build_extract_value(*sv, 0, "str_ptr")
                .map_err(|e| CompileError::LlvmError(format!("extract str ptr: {}", e)))?
                .into_pointer_value(),
            _ => {
                return Err(CompileError::TypeMismatch(
                    "regex_find: first arg must be string".to_string(),
                ))
            }
        };
        let pattern_ptr = match &args[1] {
            BasicMetadataValueEnum::PointerValue(pv) => *pv,
            BasicMetadataValueEnum::StructValue(sv) => self
                .builder
                .build_extract_value(*sv, 0, "str_ptr")
                .map_err(|e| CompileError::LlvmError(format!("extract str ptr: {}", e)))?
                .into_pointer_value(),
            _ => {
                return Err(CompileError::TypeMismatch(
                    "regex_find: second arg must be string".to_string(),
                ))
            }
        };
        let func = self
            .module
            .get_function("mimi_regex_find")
            .ok_or("mimi_regex_find not declared")?;
        let result = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(text_ptr),
                    BasicMetadataValueEnum::PointerValue(pattern_ptr),
                ],
                "regex_find_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("regex_find error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_regex_find returned void")?;
        Ok(result)
    }

    pub(in crate::codegen) fn compile_regex_replace(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 3 {
            return Err(CompileError::WrongArgCount(
                "regex_replace expects 3 arguments (text, pattern, replacement)".to_string(),
            ));
        }
        let text_ptr = match &args[0] {
            BasicMetadataValueEnum::PointerValue(pv) => *pv,
            BasicMetadataValueEnum::StructValue(sv) => self
                .builder
                .build_extract_value(*sv, 0, "str_ptr")
                .map_err(|e| CompileError::LlvmError(format!("extract str ptr: {}", e)))?
                .into_pointer_value(),
            _ => {
                return Err(CompileError::TypeMismatch(
                    "regex_replace: first arg must be string".to_string(),
                ))
            }
        };
        let pattern_ptr = match &args[1] {
            BasicMetadataValueEnum::PointerValue(pv) => *pv,
            BasicMetadataValueEnum::StructValue(sv) => self
                .builder
                .build_extract_value(*sv, 0, "str_ptr")
                .map_err(|e| CompileError::LlvmError(format!("extract str ptr: {}", e)))?
                .into_pointer_value(),
            _ => {
                return Err(CompileError::TypeMismatch(
                    "regex_replace: second arg must be string".to_string(),
                ))
            }
        };
        let replacement_ptr = match &args[2] {
            BasicMetadataValueEnum::PointerValue(pv) => *pv,
            BasicMetadataValueEnum::StructValue(sv) => self
                .builder
                .build_extract_value(*sv, 0, "str_ptr")
                .map_err(|e| CompileError::LlvmError(format!("extract str ptr: {}", e)))?
                .into_pointer_value(),
            _ => {
                return Err(CompileError::TypeMismatch(
                    "regex_replace: third arg must be string".to_string(),
                ))
            }
        };
        let func = self
            .module
            .get_function("mimi_regex_replace")
            .ok_or("mimi_regex_replace not declared")?;
        let result = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(text_ptr),
                    BasicMetadataValueEnum::PointerValue(pattern_ptr),
                    BasicMetadataValueEnum::PointerValue(replacement_ptr),
                ],
                "regex_replace_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("regex_replace error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_regex_replace returned void")?;
        Ok(result)
    }

    pub(in crate::codegen) fn compile_str_index_of(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "str_index_of expects 2 arguments".to_string(),
            ));
        }
        let s_ptr = match &args[0] {
            BasicMetadataValueEnum::PointerValue(pv) => *pv,
            BasicMetadataValueEnum::StructValue(sv) => self
                .builder
                .build_extract_value(*sv, 0, "str_ptr")
                .map_err(|e| CompileError::LlvmError(format!("extract str ptr: {}", e)))?
                .into_pointer_value(),
            _ => {
                return Err(CompileError::TypeMismatch(
                    "str_index_of: first arg must be string".to_string(),
                ))
            }
        };
        let sub_ptr = match &args[1] {
            BasicMetadataValueEnum::PointerValue(pv) => *pv,
            BasicMetadataValueEnum::StructValue(sv) => self
                .builder
                .build_extract_value(*sv, 0, "str_ptr")
                .map_err(|e| CompileError::LlvmError(format!("extract str ptr: {}", e)))?
                .into_pointer_value(),
            _ => {
                return Err(CompileError::TypeMismatch(
                    "str_index_of: second arg must be string".to_string(),
                ))
            }
        };
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        // strstr(s, sub) -> pointer or NULL
        let strstr_fn = self
            .module
            .get_function("strstr")
            .or_else(|| {
                let ty = i8_ptr.fn_type(
                    &[
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                    ],
                    false,
                );
                Some(self.module.add_function(
                    "strstr",
                    ty,
                    Some(inkwell::module::Linkage::External),
                ))
            })
            .ok_or_else(|| "failed to get or create strstr function".to_string())?;
        let found = self
            .builder
            .build_call(
                strstr_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::PointerValue(sub_ptr),
                ],
                "strstr_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("strstr error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("strstr returned void")?
            .into_pointer_value();
        // found - s = index
        let found_int = self
            .builder
            .build_ptr_to_int(found, i64_ty, "found_int")
            .map_err(|e| CompileError::LlvmError(format!("ptr_to_int error: {}", e)))?;
        let s_int = self
            .builder
            .build_ptr_to_int(s_ptr, i64_ty, "s_int")
            .map_err(|e| CompileError::LlvmError(format!("ptr_to_int error: {}", e)))?;
        let idx = self
            .builder
            .build_int_sub(found_int, s_int, "index")
            .map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?;
        // Wrap in Option<i32> = {i1 disc, i32 payload}
        let bool_ty = self.context.bool_type();
        let i32_ty = self.context.i32_type();
        // Check if strstr returned NULL (not found)
        let is_null = self
            .builder
            .build_is_null(found, "is_null")
            .map_err(|e| CompileError::LlvmError(format!("is_null: {}", e)))?;
        let disc = self
            .builder
            .build_select(
                is_null,
                bool_ty.const_int(0, false),
                bool_ty.const_int(1, false),
                "opt_disc",
            )
            .map_err(|e| CompileError::LlvmError(format!("select: {}", e)))?
            .into_int_value();
        let payload = self
            .builder
            .build_int_truncate(idx, i32_ty, "opt_payload")
            .map_err(|e| CompileError::LlvmError(format!("trunc: {}", e)))?;
        let opt_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(bool_ty),
                BasicTypeEnum::IntType(i32_ty),
            ],
            false,
        );
        let opt_alloca = self
            .builder
            .build_alloca(BasicTypeEnum::StructType(opt_ty), "opt_alloca")
            .map_err(|e| CompileError::LlvmError(format!("alloca: {}", e)))?;
        let disc_gep = self
            .gep()
            .build_struct_gep(opt_ty, opt_alloca, 0, "disc_gep")
            .map_err(|e| CompileError::LlvmError(format!("disc_gep: {}", e)))?;
        self.builder
            .build_store(disc_gep, BasicValueEnum::IntValue(disc))
            .map_err(|e| CompileError::LlvmError(format!("disc store: {}", e)))?;
        let payload_gep = self
            .gep()
            .build_struct_gep(opt_ty, opt_alloca, 1, "payload_gep")
            .map_err(|e| CompileError::LlvmError(format!("payload_gep: {}", e)))?;
        self.builder
            .build_store(payload_gep, BasicValueEnum::IntValue(payload))
            .map_err(|e| CompileError::LlvmError(format!("payload store: {}", e)))?;
        let result = self
            .builder
            .build_load(BasicTypeEnum::StructType(opt_ty), opt_alloca, "opt_result")
            .map_err(|e| CompileError::LlvmError(format!("load opt: {}", e)))?;
        Ok(result)
    }

    pub(in crate::codegen) fn compile_regex_find_all(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "regex_find_all expects 2 arguments (text, pattern)".to_string(),
            ));
        }
        let text_ptr = self.extract_raw_str_ptr(&args[0])?;
        let pattern_ptr = self.extract_raw_str_ptr(&args[1])?;
        let func = self
            .module
            .get_function("mimi_regex_find_all")
            .ok_or_else(|| "mimi_regex_find_all not declared".to_string())?;
        let raw_ptr = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(text_ptr),
                    BasicMetadataValueEnum::PointerValue(pattern_ptr),
                ],
                "regex_find_all_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("regex_find_all error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_regex_find_all returned void")?
            .into_pointer_value();
        self.wrap_c_string(raw_ptr)
    }

    pub(in crate::codegen) fn compile_regex_capture_groups(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "regex_capture_groups expects 2 arguments (text, pattern)".to_string(),
            ));
        }
        let text_ptr = self.extract_raw_str_ptr(&args[0])?;
        let pattern_ptr = self.extract_raw_str_ptr(&args[1])?;
        let func = self
            .module
            .get_function("mimi_regex_capture_groups")
            .ok_or_else(|| "mimi_regex_capture_groups not declared".to_string())?;
        let raw_ptr = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(text_ptr),
                    BasicMetadataValueEnum::PointerValue(pattern_ptr),
                ],
                "regex_capture_groups_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("regex_capture_groups error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_regex_capture_groups returned void")?
            .into_pointer_value();
        self.wrap_c_string(raw_ptr)
    }
}
