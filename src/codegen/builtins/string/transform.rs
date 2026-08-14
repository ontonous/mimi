use crate::codegen::CallSiteValueExt;
use crate::codegen::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, IntValue, PointerValue};

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn compile_str_repeat(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "str_repeat expects 2 arguments".to_string(),
            ));
        }
        let s_ptr = self.extract_string_arg(&args[0], "str_repeat")?;
        let n_raw = require_int_arg(&args[1], "str_repeat: second arg must be integer count")?;

        let i8_ty = self.context.i8_type();
        let i64_ty = self.context.i64_type();
        // A1: widen i32 to i64 — trait impl methods may pass i32 params.
        let n = if n_raw.get_type().get_bit_width() < 64 {
            self.builder
                .build_int_s_extend(n_raw, i64_ty, "n_sext")
                .map_err(|e| CompileError::LlvmError(format!("s_ext error: {}", e)))?
        } else {
            n_raw
        };
        let s_len = self.string_len(s_ptr)?;
        // CG-H2 (deep audit): guard against negative / overflowing repeat counts.
        // A negative `n` or an `s_len * n` that overflows i64 would yield a
        // negative alloc_size and out-of-bounds writes. Clamp `n` to a
        // non-negative value and cap the total size so the product cannot
        // overflow i64 nor drive an unbounded allocation.
        let zero = i64_ty.const_int(0, false);
        let max_total = i64_ty.const_int(1u64 << 33, false); // 8 GiB cap
        let n_is_neg = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, n, zero, "n_neg")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let n_clamped = self
            .builder
            .build_select(n_is_neg, zero, n, "n_clamped")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        // n_safe = min(n_clamped, max_total / max(s_len, 1)). The divisor is
        // clamped to >= 1 so the division can never be by zero.
        let s_len_zero = self
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, s_len, zero, "s_len_zero")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let s_len_divisor = self
            .builder
            .build_select(s_len_zero, i64_ty.const_int(1, false), s_len, "s_len_div")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        let max_count = self
            .builder
            .build_int_signed_div(max_total, s_len_divisor, "max_count")
            .map_err(|e| CompileError::LlvmError(format!("div error: {}", e)))?;
        let n_too_big = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SGT,
                n_clamped,
                max_count,
                "n_too_big",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let n_safe = self
            .builder
            .build_select(n_too_big, max_count, n_clamped, "n_safe")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        let total = self
            .builder
            .build_int_mul(s_len, n_safe, "total")
            .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;
        let alloc_size = self
            .builder
            .build_int_add(total, i64_ty.const_int(1, false), "alloc_size")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        let buf = self.malloc_buffer(alloc_size)?;
        self.memcpy_buffer(buf, s_ptr, s_len, "memcpy_first")?;

        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for str_repeat loop".to_string())?;
        let loop_bb = self.context.append_basic_block(function, "repeat_loop");
        let body_bb = self.context.append_basic_block(function, "repeat_body");
        let done_bb = self.context.append_basic_block(function, "repeat_done");

        let i_alloca = self.build_alloca(i64_ty, "ri")?;
        self.build_store(i_alloca, i64_ty.const_int(1, false))?;
        self.build_br(loop_bb)?;

        self.builder.position_at_end(loop_bb);
        let i = self.build_load(BasicTypeEnum::IntType(i64_ty), i_alloca, "i")?;
        let cmp = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                i.into_int_value(),
                n_safe,
                "repeat_cmp",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.build_cond_br(cmp, body_bb, done_bb)?;

        self.builder.position_at_end(body_bb);
        let offset = self
            .builder
            .build_int_mul(i.into_int_value(), s_len, "offset")
            .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;
        let dst = self.build_in_bounds_gep(i8_ty, buf, &[offset], "dst")?;
        self.memcpy_buffer(dst, s_ptr, s_len, "memcpy_loop")?;
        let next = self
            .builder
            .build_int_add(i.into_int_value(), i64_ty.const_int(1, false), "next")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.build_store(i_alloca, next)?;
        self.build_br(loop_bb)?;

        self.builder.position_at_end(done_bb);
        self.null_terminate(buf, total)?;
        Ok(buf.into())
    }

    pub(in crate::codegen) fn compile_str_trim(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        // Audit fix 10 (full-audit-2026-08-05, coordination-b): delegate to
        // the unicode-aware runtime helper `mimi_str_trim` (VM parity: Rust
        // `str::trim()`, interp/bytecode/builtins/string.rs builtin_str_trim).
        // The old inline scan only stripped ASCII space/tab/nl/cr.
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "str_trim expects 1 argument".to_string(),
            ));
        }
        self.compile_str_unary_rt_call(&args[0], "mimi_str_trim", "str_trim")
    }

    /// Shared emitter for unicode-aware unary string transforms
    /// (`mimi_str_trim` / `mimi_str_to_upper` / `mimi_str_to_lower`,
    /// src/runtime/mod.rs audit-wave1 — ptr+len ABI, each returns a freshly
    /// heap-allocated string). Full VM parity with Rust `str::trim` /
    /// `to_uppercase` / `to_lowercase`; the old inline byte scans were
    /// ASCII-only and diverged on every non-ASCII input.
    fn compile_str_unary_rt_call(
        &self,
        arg: &BasicMetadataValueEnum<'ctx>,
        rt_name: &str,
        name: &str,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let (data_ptr, byte_len) = self.extract_string_arg_ptr_len(arg, name)?;
        let rt_fn = self.get_or_declare_ptr_len_str_fn(rt_name, 0)?;
        let raw_result = self
            .build_call(
                rt_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(data_ptr),
                    BasicMetadataValueEnum::IntValue(byte_len),
                ],
                &format!("{}_call", name),
            )?
            .try_as_basic_value_opt()
            .ok_or_else(|| format!("{} returned void", rt_name))?
            .into_pointer_value();
        self.register_heap_alloc(raw_result);
        self.wrap_c_string(raw_result)
    }

    /// Extract `(data_ptr, byte_len)` from a string argument.
    /// StructValue `{i8*, i64}` carries the byte length in field 1; a raw
    /// PointerValue (string literal) is NUL-terminated, so strlen supplies it.
    /// 2026-08-06 (audit 1): a non-string struct (e.g. `List` whose layout is
    /// `{i64, ptr}`) used to reach `into_pointer_value()` and PANIC the
    /// compiler instead of failing loud — match the field types and return a
    /// TypeMismatch so codegen degrades gracefully (VM parity: E0800 at
    /// runtime; the checker deliberately does not constrain these params).
    fn extract_string_arg_ptr_len(
        &self,
        arg: &BasicMetadataValueEnum<'ctx>,
        context: &str,
    ) -> MimiResult<(PointerValue<'ctx>, IntValue<'ctx>)> {
        match arg {
            BasicMetadataValueEnum::PointerValue(pv) => {
                let len = self.string_len(*pv)?;
                Ok((*pv, len))
            }
            BasicMetadataValueEnum::StructValue(sv) => {
                let ftys = sv.get_type().get_field_types();
                let is_str_layout = ftys.len() == 2
                    && matches!(ftys[0], BasicTypeEnum::PointerType(_))
                    && matches!(ftys[1], BasicTypeEnum::IntType(it) if it.get_bit_width() == 64);
                if !is_str_layout {
                    return Err(CompileError::TypeMismatch(format!(
                        "{}: string argument expected (found a non-string struct)",
                        context
                    )));
                }
                let ptr = self
                    .build_extract_value((*sv).into(), 0, "str_ptr")
                    .map(|v| v.into_pointer_value())
                    .map_err(|e| CompileError::LlvmError(format!("extract str ptr: {}", e)))?;
                let len = self
                    .build_extract_value((*sv).into(), 1, "str_len")
                    .map(|v| v.into_int_value())
                    .map_err(|e| CompileError::LlvmError(format!("extract str len: {}", e)))?;
                Ok((ptr, len))
            }
            _ => Err(CompileError::TypeMismatch(format!(
                "{}: string argument expected",
                context
            ))),
        }
    }

    /// Get or declare a runtime string helper with the `(i8*, i64 [, i64...])
    /// → i8*` ABI (ptr+len string contract). `extra_i64_args` appends i64
    /// parameters beyond (ptr, len) — e.g. 2 for substring_clamp(start, end).
    fn get_or_declare_ptr_len_str_fn(
        &self,
        name: &str,
        extra_i64_args: usize,
    ) -> MimiResult<inkwell::values::FunctionValue<'ctx>> {
        if let Some(f) = self.module.get_function(name) {
            return Ok(f);
        }
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let mut params = vec![
            inkwell::types::BasicMetadataTypeEnum::PointerType(i8_ptr),
            inkwell::types::BasicMetadataTypeEnum::IntType(i64_ty),
        ];
        for _ in 0..extra_i64_args {
            params.push(inkwell::types::BasicMetadataTypeEnum::IntType(i64_ty));
        }
        let ty = i8_ptr.fn_type(&params, false);
        Ok(self
            .module
            .add_function(name, ty, Some(inkwell::module::Linkage::External)))
    }

    pub(in crate::codegen) fn compile_str_to_upper(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "str_to_upper expects 1 argument".to_string(),
            ));
        }
        // Audit fix 10 (coordination-b): unicode-aware runtime helper
        // (VM parity: `s.to_uppercase()`).
        self.compile_str_unary_rt_call(&args[0], "mimi_str_to_upper", "str_to_upper")
    }

    pub(in crate::codegen) fn compile_str_to_lower(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "str_to_lower expects 1 argument".to_string(),
            ));
        }
        // Audit fix 10 (coordination-b): unicode-aware runtime helper
        // (VM parity: `s.to_lowercase()`).
        self.compile_str_unary_rt_call(&args[0], "mimi_str_to_lower", "str_to_lower")
    }

    pub(in crate::codegen) fn compile_str_substring(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 3 {
            return Err(CompileError::WrongArgCount(
                "str_substring expects 3 arguments (s, start, end)".to_string(),
            ));
        }
        let start = require_int_arg(&args[1], "str_substring: start must be integer")?;
        let end = require_int_arg(&args[2], "str_substring: end must be integer")?;

        // CG-H2: Unicode scalar indices via runtime (matches interpreter).
        let i64_ty = self.context.i64_type();
        let start = if start.get_type().get_bit_width() < 64 {
            self.builder
                .build_int_s_extend(start, i64_ty, "start_sext")
                .map_err(|e| CompileError::LlvmError(format!("s_ext error: {}", e)))?
        } else {
            start
        };
        let end = if end.get_type().get_bit_width() < 64 {
            self.builder
                .build_int_s_extend(end, i64_ty, "end_sext")
                .map_err(|e| CompileError::LlvmError(format!("s_ext error: {}", e)))?
        } else {
            end
        };

        // Audit fix 5 (full-audit-2026-08-05, coordination-a): the FUNCTION
        // form `str_substring(s, start, end)` CLAMPS indices to the char
        // count — VM reference interp/bytecode/builtins/string.rs
        // builtin_str_substring (aborts only on `start > end` after
        // clamping). Runtime helper `mimi_str_substring_clamp(ptr,len,start,end)`
        // (src/runtime/mod.rs:2022, audit-wave1) implements exactly this.
        //
        // The `.substring()` METHOD form is STRICT in both the VM and codegen:
        // method.rs routes it to `str_substring_strict` (2026-08-06, D-5),
        // whose emitter calls `mimi_str_substring` instead of this clamp path.
        let (data_ptr, byte_len) = self.extract_string_arg_ptr_len(&args[0], "str_substring")?;
        let sub_fn = self.get_or_declare_ptr_len_str_fn("mimi_str_substring_clamp", 2)?;
        let raw_result = self
            .build_call(
                sub_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(data_ptr),
                    BasicMetadataValueEnum::IntValue(byte_len),
                    BasicMetadataValueEnum::IntValue(start),
                    BasicMetadataValueEnum::IntValue(end),
                ],
                "str_substring_clamp_call",
            )?
            .try_as_basic_value_opt()
            .ok_or("mimi_str_substring_clamp returned void")?
            .into_pointer_value();
        self.register_heap_alloc(raw_result);
        self.wrap_c_string(raw_result)
    }

    /// Method-form `.substring(start, end)` — STRICT bounds (VM
    /// builtin_substring_method parity): end beyond the char count traps
    /// instead of clamping. 2026-08-06 (D-5 remainder): the method dispatch
    /// funneled into compile_str_substring (clamping), so `s.substring(1,
    /// 100)` silently returned "ello" while the VM trapped E0800 — a silent
    /// error (red-line #2). The runtime already had the strict
    /// `mimi_str_substring(ptr, start, end)` helper; this emitter calls it.
    pub(in crate::codegen) fn compile_str_substring_strict(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 3 {
            return Err(CompileError::WrongArgCount(
                "str_substring_strict expects 3 arguments (s, start, end)".to_string(),
            ));
        }
        let start = require_int_arg(&args[1], "str_substring_strict: start must be integer")?;
        let end = require_int_arg(&args[2], "str_substring_strict: end must be integer")?;
        let i64_ty = self.context.i64_type();
        let start = if start.get_type().get_bit_width() < 64 {
            self.builder
                .build_int_s_extend(start, i64_ty, "start_sext")
                .map_err(|e| CompileError::LlvmError(format!("s_ext error: {}", e)))?
        } else {
            start
        };
        let end = if end.get_type().get_bit_width() < 64 {
            self.builder
                .build_int_s_extend(end, i64_ty, "end_sext")
                .map_err(|e| CompileError::LlvmError(format!("s_ext error: {}", e)))?
        } else {
            end
        };
        let data_ptr = self.extract_string_arg(&args[0], "str_substring_strict")?;
        let sub_fn = self.get_runtime_fn("mimi_str_substring")?;
        let raw_result = self
            .build_call(
                sub_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(data_ptr),
                    BasicMetadataValueEnum::IntValue(start),
                    BasicMetadataValueEnum::IntValue(end),
                ],
                "str_substring_strict_call",
            )?
            .try_as_basic_value_opt()
            .ok_or("mimi_str_substring returned void")?
            .into_pointer_value();
        self.register_heap_alloc(raw_result);
        self.wrap_c_string(raw_result)
    }

    pub(in crate::codegen) fn compile_str_split(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "str_split expects 2 arguments (string, delimiter)".to_string(),
            ));
        }
        let s_ptr = self.extract_string_arg(&args[0], "str_split")?;
        let delim_ptr = self.extract_string_arg(&args[1], "str_split")?;
        let func = self.get_runtime_fn("mimi_str_split")?;
        let result_ptr = self
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::PointerValue(delim_ptr),
                ],
                "str_split_call",
            )?
            .try_as_basic_value_opt()
            .ok_or("mimi_str_split returned void")?
            .into_pointer_value();
        self.copy_list_struct_fields(result_ptr)
    }

    pub(in crate::codegen) fn compile_str_join(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "str_join expects 2 arguments (list, delimiter)".to_string(),
            ));
        }
        let list_ptr = self.coerce_list_to_ptr(args[0], "str_join")?;
        let delim_ptr = self.extract_string_arg(&args[1], "str_join")?;
        let func = self.get_runtime_fn("mimi_str_join")?;
        let result_ptr = self
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(list_ptr),
                    BasicMetadataValueEnum::PointerValue(delim_ptr),
                ],
                "str_join_call",
            )?
            .try_as_basic_value_opt()
            .ok_or("mimi_str_join returned void")?
            .into_pointer_value();
        Ok(result_ptr.into())
    }

    pub(in crate::codegen) fn compile_str_replace(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 3 {
            return Err(CompileError::WrongArgCount(
                "str_replace expects 3 arguments (s, old, new)".to_string(),
            ));
        }
        let s_ptr = self.extract_string_arg(&args[0], "str_replace")?;
        let old_ptr = self.extract_string_arg(&args[1], "str_replace")?;
        let new_ptr = self.extract_string_arg(&args[2], "str_replace")?;
        let func = self.get_runtime_fn("mimi_str_replace")?;
        let result_ptr = self
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::PointerValue(old_ptr),
                    BasicMetadataValueEnum::PointerValue(new_ptr),
                ],
                "str_replace_call",
            )?
            .try_as_basic_value_opt()
            .ok_or("mimi_str_replace returned void")?
            .into_pointer_value();
        Ok(result_ptr.into())
    }

    // -------------------------------------------------------------------------
    // String transform helpers
    // -------------------------------------------------------------------------

    /// Extract a raw string pointer from a PointerValue or StructValue argument.
    /// 2026-08-06 (audit 1): a non-string struct (e.g. `List` with `{i64, ptr}`
    /// layout) used to reach `into_pointer_value()` and PANIC the compiler —
    /// match the field types and fail loud instead (VM parity: E0800 runtime).
    pub(in crate::codegen) fn extract_string_arg(
        &self,
        arg: &BasicMetadataValueEnum<'ctx>,
        context: &str,
    ) -> MimiResult<PointerValue<'ctx>> {
        match arg {
            BasicMetadataValueEnum::PointerValue(pv) => Ok(*pv),
            BasicMetadataValueEnum::StructValue(sv) => {
                let ftys = sv.get_type().get_field_types();
                let is_str_layout = ftys.len() == 2
                    && matches!(ftys[0], BasicTypeEnum::PointerType(_))
                    && matches!(ftys[1], BasicTypeEnum::IntType(it) if it.get_bit_width() == 64);
                if !is_str_layout {
                    return Err(CompileError::TypeMismatch(format!(
                        "{}: string argument expected (found a non-string struct)",
                        context
                    )));
                }
                self.build_extract_value((*sv).into(), 0, "str_ptr")
                    .map(|v| v.into_pointer_value())
                    .map_err(|e| CompileError::LlvmError(format!("extract str ptr: {}", e)))
            }
            _ => Err(CompileError::TypeMismatch(format!(
                "{}: string argument expected",
                context
            ))),
        }
    }

    /// Call strlen on a raw string pointer.
    pub(super) fn string_len(&self, ptr: PointerValue<'ctx>) -> MimiResult<IntValue<'ctx>> {
        let strlen_fn = self.get_runtime_fn("strlen")?;
        Ok(self
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(ptr)],
                "s_len",
            )?
            .try_as_basic_value_opt()
            .ok_or("strlen returned void")?
            .into_int_value())
    }

    /// Allocate a buffer of `size` bytes via malloc.
    /// B4: includes NULL check — aborts on OOM.
    fn malloc_buffer(&self, size: IntValue<'ctx>) -> MimiResult<PointerValue<'ctx>> {
        self.malloc_or_abort(size, "str_buf")
    }

    /// Copy `len` bytes from `src` to `dst`.
    fn memcpy_buffer(
        &self,
        dst: PointerValue<'ctx>,
        src: PointerValue<'ctx>,
        len: IntValue<'ctx>,
        name: &str,
    ) -> MimiResult<()> {
        let memcpy_fn = self.get_runtime_fn("memcpy")?;
        self.build_call(
            memcpy_fn,
            &[
                BasicMetadataValueEnum::PointerValue(dst),
                BasicMetadataValueEnum::PointerValue(src),
                BasicMetadataValueEnum::IntValue(len),
            ],
            name,
        )?;
        Ok(())
    }

    /// Write a null byte at `buf[offset]`.
    fn null_terminate(&self, buf: PointerValue<'ctx>, offset: IntValue<'ctx>) -> MimiResult<()> {
        let i8_ty = self.context.i8_type();
        let null_pos = self.build_in_bounds_gep(i8_ty, buf, &[offset], "null_pos")?;
        self.build_store(null_pos, i8_ty.const_int(0, false))
    }

    /// Copy the `{len, data}` fields from a MimiList* pointer into a freshly
    /// allocated on-stack list struct and return the struct alloca.
    fn copy_list_struct_fields(
        &self,
        result_ptr: PointerValue<'ctx>,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_struct_ty = self.list_struct_type();
        let list_ptr = self.build_pointer_cast(
            result_ptr,
            self.context.ptr_type(inkwell::AddressSpace::default()),
            "list_ptr",
        )?;
        let len_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, list_ptr, 0, "len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let data_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, list_ptr, 1, "data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let len_val = self.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "len_val")?;
        let data_val = self.build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "data_val")?;
        let result_alloca = self.build_alloca(list_struct_ty, "str_split_result")?;
        let r_len_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, result_alloca, 0, "r_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let r_data_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, result_alloca, 1, "r_data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(r_len_gep, len_val)?;
        self.build_store(r_data_gep, data_val)?;
        Ok(result_alloca.into())
    }
}

fn require_int_arg<'ctx>(
    arg: &BasicMetadataValueEnum<'ctx>,
    message: &str,
) -> MimiResult<IntValue<'ctx>> {
    match arg {
        BasicMetadataValueEnum::IntValue(iv) => Ok(*iv),
        _ => Err(CompileError::TypeMismatch(message.to_string())),
    }
}
