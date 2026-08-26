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
        match args[0] {
            BasicMetadataValueEnum::FloatValue(fv) => {
                // Audit 2026-08-05 §8 FIX-3 — RFC 8259 compliance: JSON
                // represents only finite numbers, so the old `%f` path emitted
                // INVALID JSON for NaN/Inf ("nan"/"inf") and padded finite
                // values ("1.500000"). Mirror the bytecode VM exactly
                // (interp/bytecode/builtins/misc.rs value_to_json):
                // serde_json::Number::from_f64 maps non-finite → Null
                // (serialized "null") and finite values use the shortest
                // round-trip form ("1.5"). mimi_to_string_f64 is Rust's
                // Display — the same shortest round-trip formatter the
                // println family already uses, so finite output matches.
                let function = self.current_function().ok_or(CompileError::CodegenJson(
                    "to_json: no enclosing function".into(),
                ))?;
                // Normalize to f64 first (f32 must widen before the libc/
                // runtime calls below, which are declared with f64 params).
                let f64_ty = self.context.f64_type();
                let fv64 = if fv.get_type().get_bit_width() == 64 {
                    fv
                } else {
                    self.builder
                        .build_float_ext(fv, f64_ty, "to_json_f64")
                        .map_err(|e| format!("float ext error: {}", e))?
                };
                // Finiteness classification: NaN via unordered self-compare,
                // Inf via |x| == Inf (same shape as SD-9 in expr/operator.rs).
                let is_nan = self
                    .builder
                    .build_float_compare(inkwell::FloatPredicate::UNO, fv64, fv64, "json_f_nan")
                    .map_err(|e| format!("cmp error: {}", e))?;
                let fabs_fn = self
                    .module
                    .get_function("llvm.fabs.f64")
                    .unwrap_or_else(|| {
                        self.module.add_function(
                            "llvm.fabs.f64",
                            f64_ty.fn_type(&[BasicMetadataTypeEnum::FloatType(f64_ty)], false),
                            Some(inkwell::module::Linkage::External),
                        )
                    });
                let abs_val = self
                    .builder
                    .build_call(
                        fabs_fn,
                        &[BasicMetadataValueEnum::FloatValue(fv64)],
                        "json_f_abs",
                    )
                    .map_err(|e| format!("fabs error: {}", e))?
                    .try_as_basic_value_opt()
                    .ok_or("llvm.fabs.f64 returned void")?
                    .into_float_value();
                let inf_const = f64_ty.const_float(f64::INFINITY);
                let is_inf = self
                    .builder
                    .build_float_compare(
                        inkwell::FloatPredicate::OEQ,
                        abs_val,
                        inf_const,
                        "json_f_inf",
                    )
                    .map_err(|e| format!("cmp error: {}", e))?;
                let not_finite = self
                    .builder
                    .build_or(is_nan, is_inf, "json_f_not_finite")
                    .map_err(|e| format!("or error: {}", e))?;

                let finite_bb = self
                    .context
                    .append_basic_block(function, "to_json_finite_bb");
                let null_bb = self.context.append_basic_block(function, "to_json_null_bb");
                let merge_bb = self
                    .context
                    .append_basic_block(function, "to_json_merge_bb");
                self.builder
                    .build_conditional_branch(not_finite, null_bb, finite_bb)
                    .map_err(|e| format!("branch error: {}", e))?;

                // Finite: shortest round-trip decimal via the runtime.
                self.builder.position_at_end(finite_bb);
                // Use the dedicated JSON float formatter (serde_json shortest
                // round-trip: "1.0" for whole numbers, "null" for non-finite),
                // matching the bytecode VM's value_to_json exactly.
                let to_str_fn = self.get_runtime_fn("mimi_to_json_f64")?;
                let finite_str = self
                    .builder
                    .build_call(
                        to_str_fn,
                        &[BasicMetadataValueEnum::FloatValue(fv64)],
                        "to_json_f64_str",
                    )
                    .map_err(|e| format!("mimi_to_json_f64 error: {}", e))?
                    .try_as_basic_value_opt()
                    .ok_or("mimi_to_json_f64 returned void")?
                    .into_pointer_value();
                let finite_end = self
                    .builder
                    .get_insert_block()
                    .ok_or("to_json: lost finite block")?;
                // NOTE: not registered — returned value owns the allocation.
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| format!("branch error: {}", e))?;

                // Non-finite: RFC 8259 has no NaN/Inf literals — emit `null`,
                // the VM's serde mapping. Never emit "nan"/"inf".
                self.builder.position_at_end(null_bb);
                let null_buf = self.malloc_or_abort(i64_ty.const_int(5, false), "json_null")?;
                let null_lit = self
                    .builder
                    .build_global_string_ptr("null", "json_null_lit")
                    .map_err(|e| format!("fmt error: {}", e))?;
                // strcpy from a known-valid 4-char static into a fresh 5-byte buf.
                self.builder
                    .build_call(
                        strcpy_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(null_buf),
                            BasicMetadataValueEnum::PointerValue(null_lit.as_pointer_value()),
                        ],
                        "json_strcpy_null",
                    )
                    .map_err(|e| format!("strcpy error: {}", e))?;
                // NOTE: not registered — returned value owns the allocation.
                let null_end = self
                    .builder
                    .get_insert_block()
                    .ok_or("to_json: lost null block")?;
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| format!("branch error: {}", e))?;

                self.builder.position_at_end(merge_bb);
                let phi = self
                    .builder
                    .build_phi(null_buf.get_type(), "to_json_float_phi")
                    .map_err(|e| format!("phi error: {}", e))?;
                phi.add_incoming(&[
                    (&finite_str as &dyn inkwell::values::BasicValue, finite_end),
                    (&null_buf as &dyn inkwell::values::BasicValue, null_end),
                ]);
                Ok(phi.as_basic_value())
            }
            BasicMetadataValueEnum::IntValue(iv) if iv.get_type().get_bit_width() == 1 => {
                // Bool: true→"true", false→"false"
                let alloc_size = i64_ty.const_int(512, false);
                let buf = self.malloc_or_abort(alloc_size, "json_malloc")?;
                // NOTE: not registered — returned value owns the allocation.
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
                let alloc_size = i64_ty.const_int(512, false);
                let buf = self.malloc_or_abort(alloc_size, "json_malloc")?;
                // NOTE: not registered — returned value owns the allocation.
                let fmt = self
                    .builder
                    .build_global_string_ptr("%ld", "json_int_fmt")
                    .map_err(|e| format!("fmt error: {}", e))?;
                // C varargs `%ld` requires an i64 value; passing a narrow i32
                // would be undefined behavior on non-x86_64 ABI. Sign-extend
                // Mimi integers to i64 before snprintf.
                let iv64 = if iv.get_type().get_bit_width() < 64 {
                    self.builder
                        .build_int_s_extend(iv, i64_ty, "json_int_sext")
                        .map_err(|e| format!("sext error: {}", e))?
                } else {
                    iv
                };
                self.builder
                    .build_call(
                        snprintf_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::IntValue(alloc_size),
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::IntValue(iv64),
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
                    // The runtime already returns an exactly-sized owned allocation.
                    // Returning it directly avoids a second allocation and prevents
                    // escaped content from overflowing an input-length-sized buffer.
                    Ok(escaped.into())
                } else {
                    // Untyped pointer path: List/Record/Map/Set are handled in compile_call
                    // (simple.rs) before reaching here when type names are known.
                    // Remaining pointers are opaque handles — refuse silent C-string cast.
                    Err(CompileError::Generic(
                        "to_json: untyped pointer values are not supported in codegen; \
                         use typed List/Record/Map/Set paths"
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
        // Audit 2026-08-05 §8 [VERIFIED CRITICAL] FIX-1: mimi_from_json
        // returns NULL on malformed input; passing the raw pointer through
        // makes downstream puts/strlen dereference NULL (UB). VM semantics:
        // from_json parse error is an ERROR (bytecode builtin_from_json), so
        // trap loud with a VM-style message instead of handing NULL to any
        // consumer. (Agent H's runtime fail-loud does not cover mimi_from_json
        // as of 2026-08-05 — this guard is the primary enforcement.)
        self.require_nonnull_json_result(
            result,
            "from_json parse error: invalid JSON",
            "from_json",
        )?;
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
        // Audit 2026-08-05 §8 FIX-1/FIX-2: codegen-side enforcement —
        // malformed input fails LOUD (VM: "json_get_string parse error: …")
        // instead of degenerating into the runtime's old NULL sentinel.
        self.require_valid_json_input(json_ptr, "json_get_string")?;
        // Audit wave2 (roadmap #10, P-0 ruling: ALIGN TO VM): the runtime
        // accessor aborts on a MISSING key ("json_get_string: key 'k' not
        // found"), but the VM reference (bytecode builtin_json_get_string)
        // returns the EMPTY STRING for `None`. Probe the key with
        // json_has_key FIRST: it returns 0 for an absent key (its
        // documented purpose) and aborts loud on malformed input, so the
        // get_string call below only runs when the key is present and can
        // no longer hit the runtime's missing-key abort. (Balanced-but-
        // invalid documents that slip past mimi_is_valid_json's brace
        // scanner abort inside json_has_key — still loud, message prefix
        // "json_has_key parse error" instead of "json_get_string".)
        let has_key_fn = self
            .module
            .get_function("json_has_key")
            .ok_or_else(|| "codegen: json_has_key not declared".to_string())?;
        let has_key = self
            .builder
            .build_call(
                has_key_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(json_ptr),
                    BasicMetadataValueEnum::PointerValue(key_ptr),
                ],
                "json_get_string_has_key_call",
            )
            .map_err(|e| format!("json_has_key error: {}", e))?
            .try_as_basic_value_opt()
            .ok_or("json_has_key returned void")?
            .into_int_value();
        let function = self.current_function().ok_or(CompileError::CodegenJson(
            "json_get_string: no enclosing function".into(),
        ))?;
        let has_key_bool = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::NE,
                has_key,
                has_key.get_type().const_zero(),
                "json_get_string_has_key",
            )
            .map_err(|e| format!("cmp error: {}", e))?;
        let missing_bb = self
            .context
            .append_basic_block(function, "json_get_string_missing_bb");
        let present_bb = self
            .context
            .append_basic_block(function, "json_get_string_present_bb");
        let final_bb = self
            .context
            .append_basic_block(function, "json_get_string_final_bb");
        self.builder
            .build_conditional_branch(has_key_bool, present_bb, missing_bb)
            .map_err(|e| format!("branch error: {}", e))?;

        // Missing key → heap "" (VM parity).
        self.builder.position_at_end(missing_bb);
        let missing_empty = self.build_empty_heap_string("json_get_string_missing_empty")?;
        // NOTE: not registered — returned value owns the allocation.
        let missing_end = self
            .builder
            .get_insert_block()
            .ok_or("json_get_string: lost missing block")?;
        self.builder
            .build_unconditional_branch(final_bb)
            .map_err(|e| format!("branch error: {}", e))?;

        // Present key → runtime accessor (cannot hit its missing-key abort
        // now). Defense in depth: a NULL result still maps to "" instead of
        // escaping downstream (puts/strlen UB) — mirrors FIX-1.
        self.builder.position_at_end(present_bb);
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
        let is_null = self
            .builder
            .build_is_null(result, "json_get_string_is_null")
            .map_err(|e| format!("is_null error: {}", e))?;
        let empty_bb = self
            .context
            .append_basic_block(function, "json_get_string_empty_bb");
        let ok_bb = self
            .context
            .append_basic_block(function, "json_get_string_ok_bb");
        let present_merge_bb = self
            .context
            .append_basic_block(function, "json_get_string_present_merge_bb");
        self.builder
            .build_conditional_branch(is_null, empty_bb, ok_bb)
            .map_err(|e| format!("branch error: {}", e))?;
        self.builder.position_at_end(empty_bb);
        let empty_str = self.build_empty_heap_string("json_get_string_empty")?;
        // NOTE: not registered — returned value owns the allocation.
        let empty_end = self
            .builder
            .get_insert_block()
            .ok_or("json_get_string: lost empty block")?;
        self.builder
            .build_unconditional_branch(present_merge_bb)
            .map_err(|e| format!("branch error: {}", e))?;
        self.builder.position_at_end(ok_bb);
        let ok_end = self
            .builder
            .get_insert_block()
            .ok_or("json_get_string: lost ok block")?;
        self.builder
            .build_unconditional_branch(present_merge_bb)
            .map_err(|e| format!("branch error: {}", e))?;
        self.builder.position_at_end(present_merge_bb);
        let present_phi = self
            .builder
            .build_phi(result.get_type(), "json_get_string_phi")
            .map_err(|e| format!("phi error: {}", e))?;
        present_phi.add_incoming(&[
            (&empty_str as &dyn inkwell::values::BasicValue, empty_end),
            (&result as &dyn inkwell::values::BasicValue, ok_end),
        ]);
        let present_ptr = present_phi.as_basic_value();
        let present_end = self
            .builder
            .get_insert_block()
            .ok_or("json_get_string: lost present-merge block")?;

        // Outer merge: missing-key "" vs present-key accessor result.
        self.builder
            .build_unconditional_branch(final_bb)
            .map_err(|e| format!("branch error: {}", e))?;
        self.builder.position_at_end(final_bb);
        let final_phi = self
            .builder
            .build_phi(result.get_type(), "json_get_string_final_phi")
            .map_err(|e| format!("phi error: {}", e))?;
        final_phi.add_incoming(&[
            (
                &missing_empty as &dyn inkwell::values::BasicValue,
                missing_end,
            ),
            (
                &present_ptr as &dyn inkwell::values::BasicValue,
                present_end,
            ),
        ]);
        Ok(final_phi.as_basic_value())
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
        // Audit 2026-08-05 §8 [VERIFIED CRITICAL] FIX-2: codegen-side
        // enforcement — malformed input fails LOUD (VM: "json_get_int parse
        // error: …") instead of riding the runtime's old sentinel-0 return.
        self.require_valid_json_input(json_ptr, "json_get_int")?;
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
        // No codegen fallback-to-0 anywhere: missing key / not-a-number /
        // not-an-integer are loud runtime failures with VM-matching messages
        // (agent H, runtime/mod.rs json_get_int). Codegen passes the i64
        // through untouched and must not mask those errors with a default.
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
        // Audit 2026-08-05 §8 FIX-2: malformed input must fail LOUD
        // (VM: "json_array_length parse error: …"), not appear as a
        // partial-count / parse-failure-as-0 sentinel. No codegen-side
        // default remains: the i64 result passes through as-is (non-array
        // shape is likewise a loud runtime failure, agent H).
        self.require_valid_json_input(json_ptr, "json_array_length")?;
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
        // Audit 2026-08-05 §8 FIX-1/FIX-2: malformed input fails LOUD
        // (VM: "json_get_element parse error: …") before reaching the
        // accessor's old NULL-sentinel contract.
        self.require_valid_json_input(json_ptr, "json_get_element")?;
        let index = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => {
                // The runtime signature takes i64; match it exactly so a
                // narrow (i32) index literal is not mis-passed through a
                // variadic-less call of a different width.
                let bw = iv.get_type().get_bit_width();
                if bw == 64 {
                    iv
                } else if bw < 64 {
                    self.builder
                        .build_int_s_extend(iv, self.context.i64_type(), "json_idx_i64")
                        .map_err(|e| format!("json_get_element index widen error: {}", e))?
                } else {
                    return Err(CompileError::TypeMismatch(
                        "json_get_element: index wider than i64".into(),
                    ));
                }
            }
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
        // Audit 2026-08-05 §8 [VERIFIED CRITICAL] FIX-1 (defense in depth):
        // NULL would make downstream puts/strlen UB. After the validity
        // guard, NULL can only mean an out-of-bounds index — which the VM
        // reports as an ERROR ("json_get_element: index N out of bounds"),
        // so trap loud instead of returning the raw pointer.
        self.require_nonnull_json_result(
            result,
            "json_get_element: index out of bounds",
            "json_get_element",
        )?;
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
        // Audit 2026-08-05 §8 FIX-2: malformed input fails LOUD
        // (VM: "json_has_key parse error: …") instead of masquerading as
        // "key absent" (the runtime's old 0-sentinel). A genuinely missing
        // key still yields false — that is the function's purpose.
        self.require_valid_json_input(json_ptr, "json_has_key")?;
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

    // ── Audit 2026-08-05 §8 FIX-1/FIX-2: fail-loud JSON guards ─────────
    //
    // Coordination with agent H (runtime/mod.rs, audit §10): the runtime
    // accessors abort with VM-matching messages on malformed input instead
    // of returning sentinel 0/NULL. These codegen guards are DEFENSE IN
    // DEPTH — they make the accessors loud even before/without the runtime
    // change, and they ensure no NULL pointer is EVER handed to a consumer
    // (puts(NULL)/strlen(NULL) is UB). No guard here depends on a sentinel
    // value: validity is re-derived from mimi_is_valid_json.

    /// Trap with a VM-style message via `mimi_runtime_abort` (noreturn).
    /// Leaves the builder at the end of a block terminated by `unreachable`;
    /// callers arrange control flow around it.
    fn emit_json_trap(&self, message: &str, label: &str) -> MimiResult<()> {
        let abort_fn = self.get_or_declare_abort_fn();
        let msg = self
            .builder
            .build_global_string_ptr(message, &format!("{}_msg", label))
            .map_err(|e| format!("global string error: {}", e))?;
        self.build_call(
            abort_fn,
            &[BasicMetadataValueEnum::PointerValue(msg.as_pointer_value())],
            &format!("{}_abort", label),
        )?;
        // SAFETY: mimi_runtime_abort is noreturn (declared with the noreturn
        // attribute); this block is unreachable.
        self.builder
            .build_unreachable()
            .map_err(|e| format!("unreachable error: {}", e))?;
        Ok(())
    }

    /// Parse-validate a JSON document before calling an accessor. Malformed
    /// input traps with the VM's "<builtin> parse error: …" shape instead of
    /// reaching the accessor's sentinel contract. Leaves the builder in the
    /// continuation block.
    fn require_valid_json_input(
        &self,
        json_ptr: inkwell::values::PointerValue<'ctx>,
        builtin: &str,
    ) -> MimiResult<()> {
        let valid_fn = self
            .module
            .get_function("mimi_is_valid_json")
            .ok_or_else(|| "codegen: mimi_is_valid_json not declared".to_string())?;
        let result = self
            .builder
            .build_call(
                valid_fn,
                &[BasicMetadataValueEnum::PointerValue(json_ptr)],
                &format!("{}_valid_call", builtin),
            )
            .map_err(|e| format!("mimi_is_valid_json error: {}", e))?
            .try_as_basic_value_opt()
            .ok_or("mimi_is_valid_json returned void")?
            .into_int_value();
        let valid = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::NE,
                result,
                result.get_type().const_zero(),
                &format!("{}_valid", builtin),
            )
            .map_err(|e| format!("cmp error: {}", e))?;
        let function = self.current_function().ok_or_else(|| {
            CompileError::LlvmError(format!("{}: no enclosing function", builtin))
        })?;
        let ok_bb = self
            .context
            .append_basic_block(function, &format!("{}_valid_ok_bb", builtin));
        let trap_bb = self
            .context
            .append_basic_block(function, &format!("{}_invalid_trap_bb", builtin));
        self.builder
            .build_conditional_branch(valid, ok_bb, trap_bb)
            .map_err(|e| format!("branch error: {}", e))?;
        self.builder.position_at_end(trap_bb);
        let msg = format!("{} parse error: invalid JSON", builtin);
        self.emit_json_trap(&msg, &format!("{}_invalid", builtin))?;
        self.builder.position_at_end(ok_bb);
        Ok(())
    }

    /// Require a runtime pointer result to be non-NULL, otherwise trap with
    /// the given VM-style message. Leaves the builder in the continuation
    /// (non-null) block.
    fn require_nonnull_json_result(
        &self,
        ptr: inkwell::values::PointerValue<'ctx>,
        trap_message: &str,
        label: &str,
    ) -> MimiResult<()> {
        let function = self
            .current_function()
            .ok_or_else(|| CompileError::LlvmError(format!("{}: no enclosing function", label)))?;
        let is_null = self
            .builder
            .build_is_null(ptr, &format!("{}_is_null", label))
            .map_err(|e| format!("is_null error: {}", e))?;
        let ok_bb = self
            .context
            .append_basic_block(function, &format!("{}_nonnull_ok_bb", label));
        let trap_bb = self
            .context
            .append_basic_block(function, &format!("{}_null_trap_bb", label));
        self.builder
            .build_conditional_branch(is_null, trap_bb, ok_bb)
            .map_err(|e| format!("branch error: {}", e))?;
        self.builder.position_at_end(trap_bb);
        self.emit_json_trap(trap_message, &format!("{}_null", label))?;
        self.builder.position_at_end(ok_bb);
        Ok(())
    }

    /// Heap-allocated empty C-string (1 malloc'd NUL byte) — VM parity for
    /// `json_get_string` on a missing key (bytecode VM returns "").
    fn build_empty_heap_string(
        &self,
        label: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let buf = self.malloc_or_abort(self.context.i64_type().const_int(1, false), label)?;
        let nul = self.context.i8_type().const_zero();
        // buf is a fresh malloc'd byte; store the NUL terminator directly.
        self.build_store(buf, nul)?;
        Ok(buf)
    }
}
