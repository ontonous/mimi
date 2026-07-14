use super::super::CallSiteValueExt;
use super::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

impl<'ctx> CodeGenerator<'ctx> {
    pub(super) fn compile_to_json(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "to_json expects 1 argument".into(),
            ));
        }
        let i64_ty = self.context.i64_type();
        // B3: Use snprintf instead of sprintf for buffer safety.
        // B4: allocations go through malloc_or_abort.
        // CG-C3: snprintf returns i32, not i8*.
        let snprintf_fn = self.module.get_function("snprintf").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let i32_ty = self.context.i32_type();
            let ty = i32_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ],
                true,
            );
            self.module
                .add_function("snprintf", ty, Some(inkwell::module::Linkage::External))
        });
        let strcpy_fn = self
            .module
            .get_function("strcpy")
            .ok_or_else(|| "strcpy not declared".to_string())?;
        let alloc_size = match &args[0] {
            BasicMetadataValueEnum::StructValue(sv) => {
                let str_len = self
                    .builder
                    .build_extract_value(*sv, 1, "str_len")
                    .map_err(|e| format!("extract str_len error: {}", e))?;
                let three = i64_ty.const_int(3, false);
                match str_len {
                    BasicValueEnum::IntValue(iv) => self
                        .builder
                        .build_int_add(iv, three, "buf_size")
                        .map_err(|e| format!("add error: {}", e))?,
                    _ => {
                        return Err(CompileError::Generic(
                            "string length field is not i64".into(),
                        ))
                    }
                }
            }
            _ => i64_ty.const_int(512, false), // B3: was 64, %f can produce 317+ chars
        };
        let buf = self.malloc_or_abort(alloc_size, "json_malloc")?;
        // NOTE: not registered — returned value owns the allocation
        match args[0] {
            BasicMetadataValueEnum::FloatValue(fv) => {
                let fmt = self
                    .builder
                    .build_global_string_ptr("%f", "json_float_fmt")
                    .map_err(|e| format!("fmt error: {}", e))?;
                self.builder
                    .build_call(
                        snprintf_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::IntValue(alloc_size),
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::FloatValue(fv),
                        ],
                        "json_snprintf",
                    )
                    .map_err(|e| format!("snprintf error: {}", e))?;
                Ok(buf.into())
            }
            BasicMetadataValueEnum::IntValue(iv) if iv.get_type().get_bit_width() == 1 => {
                // Bool: true→"true", false→"false"
                let true_str = self
                    .builder
                    .build_global_string_ptr("true", "json_true")
                    .map_err(|e| format!("fmt error: {}", e))?;
                let false_str = self
                    .builder
                    .build_global_string_ptr("false", "json_false")
                    .map_err(|e| format!("fmt error: {}", e))?;
                let cmp = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::NE,
                        iv,
                        self.context.bool_type().const_int(0, false),
                        "is_true",
                    )
                    .map_err(|e| format!("cmp error: {}", e))?;
                let function = self.current_function().ok_or(CompileError::CodegenJson(
                    "to_json: no enclosing function".into(),
                ))?;
                let true_bb = self.context.append_basic_block(function, "json_true_bb");
                let false_bb = self.context.append_basic_block(function, "json_false_bb");
                let merge_bb = self.context.append_basic_block(function, "json_merge_bb");
                self.builder
                    .build_conditional_branch(cmp, true_bb, false_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(true_bb);
                // strcpy from known-valid static string to freshly allocated buffer.
                self.builder
                    .build_call(
                        strcpy_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::PointerValue(true_str.as_pointer_value()),
                        ],
                        "json_strcpy_true",
                    )
                    .map_err(|e| format!("strcpy error: {}", e))?;
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(false_bb);
                // strcpy from known-valid static string to freshly allocated buffer.
                self.builder
                    .build_call(
                        strcpy_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::PointerValue(false_str.as_pointer_value()),
                        ],
                        "json_strcpy_false",
                    )
                    .map_err(|e| format!("strcpy error: {}", e))?;
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(merge_bb);
                Ok(buf.into())
            }
            BasicMetadataValueEnum::IntValue(iv) => {
                // Integer: snprintf(buf, size, "%ld", iv)
                let fmt = self
                    .builder
                    .build_global_string_ptr("%ld", "json_int_fmt")
                    .map_err(|e| format!("fmt error: {}", e))?;
                self.builder
                    .build_call(
                        snprintf_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::IntValue(alloc_size),
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::IntValue(iv),
                        ],
                        "json_snprintf_int",
                    )
                    .map_err(|e| format!("snprintf error: {}", e))?;
                Ok(buf.into())
            }
            _ => {
                // String: use mimi_json_escape_string to properly escape special chars.
                // DAT-C2 (deep audit): sprintf("\"%s\"", str) does not escape
                // backslash, quotes, newlines — producing invalid JSON and enabling
                // JSON injection. Use the runtime escape function instead.
                if let Ok(raw_ptr) = self.extract_raw_str_ptr(&args[0]) {
                    let escape_fn = self.get_runtime_fn("mimi_json_escape_string")?;
                    let escaped = self
                        .build_call(
                            escape_fn,
                            &[BasicMetadataValueEnum::PointerValue(raw_ptr)],
                            "json_escaped",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or("mimi_json_escape_string returned void")?
                        .into_pointer_value();
                    // Copy escaped string into buf (which is already allocated with str_len+3).
                    // The escaped string may be longer than the original if it contains
                    // special chars. Use strcpy which copies until null terminator.
                    self.builder
                        .build_call(
                            strcpy_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(buf),
                                BasicMetadataValueEnum::PointerValue(escaped),
                            ],
                            "json_strcpy_escaped",
                        )
                        .map_err(|e| format!("strcpy error: {}", e))?;
                    // Free the escaped string (it's heap-allocated by the runtime).
                    let free_fn = self
                        .module
                        .get_function("free")
                        .ok_or_else(|| CompileError::LlvmError("free not declared".into()))?;
                    let _ = self.build_call(
                        free_fn,
                        &[BasicMetadataValueEnum::PointerValue(escaped)],
                        "free_escaped",
                    );
                    Ok(buf.into())
                } else {
                    // CG-H2 (audit): pointer values in `to_json` are List/Record/Map/Set,
                    // not C strings. Return a compile-time error rather than silently
                    // treating them as raw C strings (which would read garbage).
                    // TODO(#v0.31-codegen): implement recursive JSON serialization for
                    // List<T>, Record, Map<K,V> via heap traversal.
                    Err(CompileError::Generic(
                        "to_json: complex types (List/Record/Map/Set) not yet supported \
                         in codegen; cast to string first or use std::json::get_* helpers"
                            .into(),
                    ))
                }
            }
        }
    }

    pub(super) fn compile_is_valid_json(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "json_is_valid expects 1 argument".into(),
            ));
        }
        let raw_ptr = self.extract_raw_str_ptr(&args[0])?;
        let func = self
            .module
            .get_function("mimi_is_valid_json")
            .ok_or_else(|| "codegen: mimi_is_valid_json not declared".to_string())?;
        let result = self
            .builder
            .build_call(
                func,
                &[BasicMetadataValueEnum::PointerValue(raw_ptr)],
                "is_valid_json_call",
            )
            .map_err(|e| format!("is_valid_json error: {}", e))?
            .try_as_basic_value_opt()
            .ok_or("mimi_is_valid_json returned void")?
            .into_int_value();
        // mimi_is_valid_json returns i32 — extend to Mimi bool (i1)
        let zero = self.context.i32_type().const_int(0, false);
        let cmp = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, result, zero, "valid")
            .map_err(|e| format!("cmp error: {}", e))?;
        Ok(cmp.into())
    }

    pub(super) fn compile_from_json(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "from_json expects 1 argument".into(),
            ));
        }
        let raw_ptr = self.extract_raw_str_ptr(&args[0])?;
        let from_json_fn = self
            .module
            .get_function("mimi_from_json")
            .ok_or_else(|| "codegen: mimi_from_json not declared".to_string())?;
        let result = self
            .builder
            .build_call(
                from_json_fn,
                &[BasicMetadataValueEnum::PointerValue(raw_ptr)],
                "from_json_call",
            )
            .map_err(|e| format!("from_json error: {}", e))?
            .try_as_basic_value_opt()
            .ok_or("mimi_from_json returned void")?
            .into_pointer_value();
        // Return the raw C string pointer directly (matches how string literals work in codegen)
        Ok(result.into())
    }

    pub(super) fn compile_json_get_string(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "json_get_string expects 2 arguments".into(),
            ));
        }
        let json_ptr = self.extract_raw_str_ptr(&args[0])?;
        let key_ptr = self.extract_raw_str_ptr(&args[1])?;
        let func = self
            .module
            .get_function("json_get_string")
            .ok_or_else(|| "codegen: json_get_string not declared".to_string())?;
        let result = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(json_ptr),
                    BasicMetadataValueEnum::PointerValue(key_ptr),
                ],
                "json_get_string_call",
            )
            .map_err(|e| format!("json_get_string error: {}", e))?
            .try_as_basic_value_opt()
            .ok_or("json_get_string returned void")?
            .into_pointer_value();
        Ok(result.into())
    }

    pub(super) fn compile_json_get_int(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "json_get_int expects 2 arguments".into(),
            ));
        }
        let json_ptr = self.extract_raw_str_ptr(&args[0])?;
        let key_ptr = self.extract_raw_str_ptr(&args[1])?;
        let func = self
            .module
            .get_function("json_get_int")
            .ok_or_else(|| "codegen: json_get_int not declared".to_string())?;
        let result = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(json_ptr),
                    BasicMetadataValueEnum::PointerValue(key_ptr),
                ],
                "json_get_int_call",
            )
            .map_err(|e| format!("json_get_int error: {}", e))?;
        self.expect_basic_value(&result, "json_get_int")
    }

    pub(super) fn compile_json_array_length(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "json_array_length expects 1 argument".into(),
            ));
        }
        let json_ptr = self.extract_raw_str_ptr(&args[0])?;
        let func = self
            .module
            .get_function("json_array_length")
            .ok_or_else(|| "codegen: json_array_length not declared".to_string())?;
        let result = self
            .builder
            .build_call(
                func,
                &[BasicMetadataValueEnum::PointerValue(json_ptr)],
                "json_array_length_call",
            )
            .map_err(|e| format!("json_array_length error: {}", e))?
            .try_as_basic_value_opt()
            .ok_or("json_array_length returned void")?;
        Ok(result)
    }

    pub(super) fn compile_json_get_element(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "json_get_element expects 2 arguments".into(),
            ));
        }
        let json_ptr = self.extract_raw_str_ptr(&args[0])?;
        let index = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "json_get_element: index must be i32".into(),
                ))
            }
        };
        let func = self
            .module
            .get_function("json_get_element")
            .ok_or_else(|| "codegen: json_get_element not declared".to_string())?;
        let result = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(json_ptr),
                    BasicMetadataValueEnum::IntValue(index),
                ],
                "json_get_element_call",
            )
            .map_err(|e| format!("json_get_element error: {}", e))?
            .try_as_basic_value_opt()
            .ok_or("json_get_element returned void")?
            .into_pointer_value();
        Ok(result.into())
    }

    /// CRITICAL #18 fix: compile json_has_key(json, key) -> i64 (1 or 0).
    pub(super) fn compile_json_has_key(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "json_has_key expects 2 arguments".into(),
            ));
        }
        let json_ptr = self.extract_raw_str_ptr(&args[0])?;
        let key_ptr = self.extract_raw_str_ptr(&args[1])?;
        let func = self
            .module
            .get_function("json_has_key")
            .ok_or_else(|| "codegen: json_has_key not declared".to_string())?;
        let result = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(json_ptr),
                    BasicMetadataValueEnum::PointerValue(key_ptr),
                ],
                "json_has_key_call",
            )
            .map_err(|e| format!("json_has_key error: {}", e))?
            .try_as_basic_value_opt()
            .ok_or("json_has_key returned void")?;
        Ok(result)
    }
}
