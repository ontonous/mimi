use crate::codegen::CallSiteValueExt;
use crate::codegen::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::types::BasicTypeEnum;
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
        let (s_ptr, s_len) = self.extract_string_arg_ptr_len(&args[0], "str_contains")?;
        let (sub_ptr, sub_len) = self.extract_string_arg_ptr_len(&args[1], "str_contains")?;
        // Explicit-length search: `mimi_str_index_of` handles embedded NUL
        // bytes that C `strstr` would truncate at (P1-13).
        let idx_fn = self
            .module
            .get_function("mimi_str_index_of")
            .ok_or_else(|| "mimi_str_index_of not declared".to_string())?;
        let idx = self
            .builder
            .build_call(
                idx_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::IntValue(s_len),
                    BasicMetadataValueEnum::PointerValue(sub_ptr),
                    BasicMetadataValueEnum::IntValue(sub_len),
                ],
                "str_contains_idx",
            )
            .map_err(|e| CompileError::LlvmError(format!("str_contains call: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_str_index_of returned void")?
            .into_int_value();
        let zero = self.context.i64_type().const_int(0, false);
        let cmp = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGE, idx, zero, "str_contains_found")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        // 2026-08-06 (audit 1): return i1 (bool) — the checker infers `bool`
        // for str_contains; zext to i64 made `println(str_contains(..))` print
        // "1" on the native backend vs "true" on the VM (L1 divergence).
        Ok(cmp.into())
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
        let (s_ptr, s_len) = self.extract_string_arg_ptr_len(&args[0], "str_starts_with")?;
        let (prefix_ptr, prefix_len) =
            self.extract_string_arg_ptr_len(&args[1], "str_starts_with")?;
        // Explicit-length prefix check keeps embedded NUL bytes from acting
        // as string terminators (P1-13).
        let starts_fn = self
            .module
            .get_function("mimi_str_starts_with")
            .ok_or_else(|| "mimi_str_starts_with not declared".to_string())?;
        let result = self
            .builder
            .build_call(
                starts_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::IntValue(s_len),
                    BasicMetadataValueEnum::PointerValue(prefix_ptr),
                    BasicMetadataValueEnum::IntValue(prefix_len),
                ],
                "starts_with_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("starts_with call: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_str_starts_with returned void")?
            .into_int_value();
        let zero = self.context.i64_type().const_int(0, false);
        let eq = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, result, zero, "starts_with")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        // 2026-08-06 (audit 1): return i1 (bool) — checker infers `bool`;
        // zext to i64 made native print "1" vs VM "true" (L1 divergence).
        Ok(eq.into())
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
        let (s_ptr, s_len) = self.extract_string_arg_ptr_len(&args[0], "str_ends_with")?;
        let (suffix_ptr, suffix_len) =
            self.extract_string_arg_ptr_len(&args[1], "str_ends_with")?;
        // Explicit-length suffix check keeps embedded NUL bytes from acting
        // as string terminators (P1-13).
        let ends_fn = self
            .module
            .get_function("mimi_str_ends_with")
            .ok_or_else(|| "mimi_str_ends_with not declared".to_string())?;
        let result = self
            .builder
            .build_call(
                ends_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::IntValue(s_len),
                    BasicMetadataValueEnum::PointerValue(suffix_ptr),
                    BasicMetadataValueEnum::IntValue(suffix_len),
                ],
                "ends_with_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("ends_with call: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_str_ends_with returned void")?
            .into_int_value();
        let zero = self.context.i64_type().const_int(0, false);
        let eq = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, result, zero, "ends_with")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        // 2026-08-06 (audit 1): return i1 (bool) — checker infers `bool`;
        // zext to i64 made native print "1" vs VM "true" (L1 divergence).
        Ok(eq.into())
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
        let text_ptr = self.extract_string_arg(&args[0], "regex_match")?;
        let pattern_ptr = self.extract_string_arg(&args[1], "regex_match")?;
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
        // 2026-08-06 (audit 1): return i1 (bool) — checker infers `bool`;
        // zext to i64 made native print "1" vs VM "true" (L1 divergence).
        Ok(cmp.into())
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
        let text_ptr = self.extract_string_arg(&args[0], "regex_find")?;
        let pattern_ptr = self.extract_string_arg(&args[1], "regex_find")?;
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
        let result_ptr = match result {
            BasicValueEnum::PointerValue(pv) => pv,
            _ => return Err("mimi_regex_find should return a pointer".into()),
        };
        self.register_heap_alloc(result_ptr);
        self.wrap_c_string(result_ptr)
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
        let text_ptr = self.extract_string_arg(&args[0], "regex_replace")?;
        let pattern_ptr = self.extract_string_arg(&args[1], "regex_replace")?;
        let replacement_ptr = self.extract_string_arg(&args[2], "regex_replace")?;
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
        let result_ptr = match result {
            BasicValueEnum::PointerValue(pv) => pv,
            _ => return Err("mimi_regex_replace should return a pointer".into()),
        };
        self.register_heap_alloc(result_ptr);
        self.wrap_c_string(result_ptr)
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
        let (s_ptr, s_len) = self.extract_string_arg_ptr_len(&args[0], "str_index_of")?;
        let (sub_ptr, sub_len) = self.extract_string_arg_ptr_len(&args[1], "str_index_of")?;
        let i64_ty = self.context.i64_type();
        // Explicit-length search returning a byte offset, preserving embedded
        // NUL bytes that C `strstr` would truncate at (P1-13).
        let idx_fn = self
            .module
            .get_function("mimi_str_index_of")
            .ok_or_else(|| "mimi_str_index_of not declared".to_string())?;
        let byte_idx = self
            .builder
            .build_call(
                idx_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::IntValue(s_len),
                    BasicMetadataValueEnum::PointerValue(sub_ptr),
                    BasicMetadataValueEnum::IntValue(sub_len),
                ],
                "str_index_of_idx",
            )
            .map_err(|e| CompileError::LlvmError(format!("str_index_of call: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_str_index_of returned void")?
            .into_int_value();
        // Check if the helper returned -1 (not found).
        let zero = i64_ty.const_int(0, false);
        let is_null = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                byte_idx,
                zero,
                "str_index_of_not_found",
            )
            .map_err(|e| CompileError::LlvmError(format!("not found compare: {}", e)))?;
        // Byte offset → char index: the VM returns `s[..byte_idx].chars().count()`
        // (interp/bytecode/builtins/string.rs builtin_str_index_of), not the byte
        // offset. Count UTF-8 leading bytes in s[0..byte_idx]. When not found,
        // clamp the scan bound to 0 so the loop never iterates; the payload is
        // unobserved (disc=0).
        let char_bound = self
            .builder
            .build_select(is_null, zero, byte_idx, "char_bound")
            .map_err(|e| CompileError::LlvmError(format!("select: {}", e)))?
            .into_int_value();
        let char_idx = self.count_utf8_chars(s_ptr, Some(char_bound))?;
        // Wrap in Option<i32> — codegen's Option convention is {i1 disc, i64
        // payload} (see compile_none_constructor); the i64 char count is the
        // payload WITHOUT narrowing (narrowing here corrupted option_value_or,
        // which reads the {i1,i64} layout — full-audit 2026-08-05 central fix).
        let bool_ty = self.context.bool_type();
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
        let payload = char_idx;
        let opt_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(bool_ty),
                BasicTypeEnum::IntType(i64_ty),
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
        self.register_heap_alloc(raw_ptr);
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
        self.register_heap_alloc(raw_ptr);
        self.wrap_c_string(raw_ptr)
    }

    pub(in crate::codegen) fn compile_str_count_substring(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "str_count_substring expects 2 arguments".to_string(),
            ));
        }
        let (s_ptr, s_len) = self.extract_string_arg_ptr_len(&args[0], "str_count_substring")?;
        let (sub_ptr, sub_len) =
            self.extract_string_arg_ptr_len(&args[1], "str_count_substring")?;
        let func = self
            .module
            .get_function("mimi_str_count_substring")
            .ok_or_else(|| "mimi_str_count_substring not declared".to_string())?;
        let result = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::IntValue(s_len),
                    BasicMetadataValueEnum::PointerValue(sub_ptr),
                    BasicMetadataValueEnum::IntValue(sub_len),
                ],
                "str_count_substring_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("str_count_substring error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_str_count_substring returned void")?
            .into_int_value();
        Ok(result.into())
    }
}
