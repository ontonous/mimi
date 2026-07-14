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
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "str_trim expects 1 argument".to_string(),
            ));
        }
        let s_ptr = self.extract_string_arg(&args[0], "str_trim")?;
        let i8_ty = self.context.i8_type();
        let i64_ty = self.context.i64_type();
        let zero = i64_ty.const_int(0, false);
        let s_len = self.string_len(s_ptr)?;

        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for str_trim".to_string())?;

        // Forward scan
        let start = self.scan_whitespace(function, s_ptr, s_len, zero, true)?;
        // Backward scan
        let end = self.scan_whitespace(function, s_ptr, s_len, s_len, false)?;

        let trimmed_len = self
            .builder
            .build_int_sub(end, start, "trimmed_len")
            .map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?;
        let is_negative = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLE, trimmed_len, zero, "is_neg")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let safe_len = self
            .builder
            .build_select(is_negative, zero, trimmed_len, "safe_len")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();

        let alloc_size = self
            .builder
            .build_int_add(safe_len, i64_ty.const_int(1, false), "alloc_size")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        let buf = self.malloc_buffer(alloc_size)?;
        let src = self.build_in_bounds_gep(i8_ty, s_ptr, &[start], "src")?;

        let should_copy = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, safe_len, zero, "should_copy")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let copy_bb = self.context.append_basic_block(function, "trim_copy");
        let done_bb = self.context.append_basic_block(function, "trim_done");
        self.build_cond_br(should_copy, copy_bb, done_bb)?;

        self.builder.position_at_end(copy_bb);
        self.memcpy_buffer(buf, src, safe_len, "memcpy_call")?;
        self.build_br(done_bb)?;

        self.builder.position_at_end(done_bb);
        self.null_terminate(buf, safe_len)?;
        Ok(buf.into())
    }

    /// Scan from `start` forward or backward skipping whitespace.
    /// Returns the index of the first non-whitespace character (forward)
    /// or the index after the last non-whitespace character (backward).
    fn scan_whitespace(
        &self,
        function: inkwell::values::FunctionValue<'ctx>,
        s_ptr: PointerValue<'ctx>,
        s_len: IntValue<'ctx>,
        start: IntValue<'ctx>,
        forward: bool,
    ) -> MimiResult<IntValue<'ctx>> {
        let i8_ty = self.context.i8_type();
        let i64_ty = self.context.i64_type();
        let zero = i64_ty.const_int(0, false);
        let loop_bb = self.context.append_basic_block(function, "trim_loop");
        let body_bb = self.context.append_basic_block(function, "trim_body");
        let done_bb = self.context.append_basic_block(function, "trim_done");

        let idx_alloca = self.entry_alloca(BasicTypeEnum::IntType(i64_ty), "idx")?;
        // Extend start to i64 if it's i32 — the index alloca is always i64.
        let start_i64 = if start.get_type().get_bit_width() < 64 {
            self.builder
                .build_int_s_extend(start, i64_ty, "start_sext")
                .map_err(|e| CompileError::LlvmError(format!("s_ext error: {}", e)))?
        } else {
            start
        };
        self.build_store(idx_alloca, start_i64)?;
        self.build_br(loop_bb)?;

        self.builder.position_at_end(loop_bb);
        let idx = self.build_load(BasicTypeEnum::IntType(i64_ty), idx_alloca, "idx")?;
        let idx_iv = idx.into_int_value();
        let cmp = if forward {
            self.builder
                .build_int_compare(inkwell::IntPredicate::SLT, idx_iv, s_len, "trim_cmp")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
        } else {
            self.builder
                .build_int_compare(inkwell::IntPredicate::SGT, idx_iv, zero, "trim_cmp")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
        };
        self.build_cond_br(cmp, body_bb, done_bb)?;

        self.builder.position_at_end(body_bb);
        let ch_idx = if forward {
            idx_iv
        } else {
            self.builder
                .build_int_sub(idx_iv, i64_ty.const_int(1, false), "prev")
                .map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?
        };
        let ch_ptr = self.build_in_bounds_gep(i8_ty, s_ptr, &[ch_idx], "ch")?;
        let ch = self.build_load(BasicTypeEnum::IntType(i8_ty), ch_ptr, "ch_val")?;
        let is_ws = self.is_whitespace(ch.into_int_value(), i8_ty)?;
        let next = if forward {
            self.builder
                .build_int_add(idx_iv, i64_ty.const_int(1, false), "next")
                .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?
        } else {
            self.builder
                .build_int_sub(idx_iv, i64_ty.const_int(1, false), "next")
                .map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?
        };
        self.build_store(idx_alloca, next)?;
        self.build_cond_br(is_ws, loop_bb, done_bb)?;

        self.builder.position_at_end(done_bb);
        Ok(idx_iv)
    }

    fn is_whitespace(
        &self,
        ch: IntValue<'ctx>,
        i8_ty: inkwell::types::IntType<'ctx>,
    ) -> MimiResult<IntValue<'ctx>> {
        let space = i8_ty.const_int(b' ' as u64, false);
        let tab = i8_ty.const_int(b'\t' as u64, false);
        let nl = i8_ty.const_int(b'\n' as u64, false);
        let cr = i8_ty.const_int(b'\r' as u64, false);
        let is_space = self
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, ch, space, "is_space")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let is_tab = self
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, ch, tab, "is_tab")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let is_nl = self
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, ch, nl, "is_nl")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let is_cr = self
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, ch, cr, "is_cr")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let is_ws1 = self
            .builder
            .build_or(is_space, is_tab, "is_ws1")
            .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?;
        let is_ws2 = self
            .builder
            .build_or(is_nl, is_cr, "is_ws2")
            .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?;
        self.builder
            .build_or(is_ws1, is_ws2, "is_ws")
            .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))
    }

    pub(in crate::codegen) fn compile_str_to_upper(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        self.compile_str_case_transform(args, true, "str_to_upper")
    }

    pub(in crate::codegen) fn compile_str_to_lower(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        self.compile_str_case_transform(args, false, "str_to_lower")
    }

    fn compile_str_case_transform(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
        to_upper: bool,
        name: &str,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(format!(
                "{} expects 1 argument",
                name
            )));
        }
        let s_ptr = self.extract_string_arg(&args[0], name)?;
        let i8_ty = self.context.i8_type();
        let i64_ty = self.context.i64_type();
        let s_len = self.string_len(s_ptr)?;
        let alloc_size = self
            .builder
            .build_int_add(s_len, i64_ty.const_int(1, false), "alloc_size")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        let buf = self.malloc_buffer(alloc_size)?;
        self.memcpy_buffer(buf, s_ptr, alloc_size, "memcpy_call")?;

        let function = self
            .current_function()
            .ok_or_else(|| format!("codegen: no current function for {} loop", name))?;
        let loop_bb = self.context.append_basic_block(function, "case_loop");
        let body_bb = self.context.append_basic_block(function, "case_body");
        let done_bb = self.context.append_basic_block(function, "case_done");

        let i_alloca = self.build_alloca(i64_ty, "ci")?;
        self.build_store(i_alloca, i64_ty.const_int(0, false))?;
        self.build_br(loop_bb)?;

        self.builder.position_at_end(loop_bb);
        let i = self.build_load(BasicTypeEnum::IntType(i64_ty), i_alloca, "i")?;
        let cmp = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                i.into_int_value(),
                s_len,
                "case_cmp",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.build_cond_br(cmp, body_bb, done_bb)?;

        self.builder.position_at_end(body_bb);
        let ch_ptr = self.build_in_bounds_gep(i8_ty, buf, &[i.into_int_value()], "ch")?;
        let ch = self
            .build_load(BasicTypeEnum::IntType(i8_ty), ch_ptr, "ch_val")?
            .into_int_value();
        let transformed = if to_upper {
            self.transform_case(ch, i8_ty, b'a', b'z', -32)?
        } else {
            self.transform_case(ch, i8_ty, b'A', b'Z', 32)?
        };
        self.build_store(ch_ptr, transformed)?;
        let next = self
            .builder
            .build_int_add(i.into_int_value(), i64_ty.const_int(1, false), "next")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.build_store(i_alloca, next)?;
        self.build_br(loop_bb)?;

        self.builder.position_at_end(done_bb);
        Ok(buf.into())
    }

    fn transform_case(
        &self,
        ch: IntValue<'ctx>,
        i8_ty: inkwell::types::IntType<'ctx>,
        lo: u8,
        hi: u8,
        delta: i8,
    ) -> MimiResult<IntValue<'ctx>> {
        let lo_val = i8_ty.const_int(lo as u64, false);
        let hi_val = i8_ty.const_int(hi as u64, false);
        let in_range1 = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGE, ch, lo_val, "ge_lo")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let in_range2 = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLE, ch, hi_val, "le_hi")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let in_range = self
            .builder
            .build_and(in_range1, in_range2, "in_range")
            .map_err(|e| CompileError::LlvmError(format!("and error: {}", e)))?;
        let delta_abs = delta.unsigned_abs() as u64;
        let transformed = if delta < 0 {
            self.builder
                .build_int_sub(ch, i8_ty.const_int(delta_abs, false), "case_result")
                .map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?
        } else {
            self.builder
                .build_int_add(ch, i8_ty.const_int(delta_abs, false), "case_result")
                .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?
        };
        self.builder
            .build_select(in_range, transformed, ch, "result_ch")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))
            .map(|v| v.into_int_value())
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
        let s_ptr = self.extract_string_arg(&args[0], "str_substring")?;
        let start = require_int_arg(&args[1], "str_substring: start must be integer")?;
        let end = require_int_arg(&args[2], "str_substring: end must be integer")?;

        let i8_ty = self.context.i8_type();
        let i64_ty = self.context.i64_type();
        let s_len = self.string_len(s_ptr)?;

        // MEM-C5 (deep audit): clamp start and end to [0, s_len] and ensure end >= start.
        // Extend i32 start/end to i64 for comparison with string length.
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

        let zero = i64_ty.const_int(0, false);
        let start_neg = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, start, zero, "start_neg")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let start_oob = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, start, s_len, "start_oob")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let start_clamped = self
            .builder
            .build_select(start_neg, zero, start, "start_clamped_lo")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        let start_clamped = self
            .builder
            .build_select(start_oob, s_len, start_clamped, "start_clamped_hi")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();

        let end_neg = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                end,
                start_clamped,
                "end_lt_start",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let end_oob = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, end, s_len, "end_oob")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let end_clamped = self
            .builder
            .build_select(end_neg, start_clamped, end, "end_clamped_lo")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        let end_clamped = self
            .builder
            .build_select(end_oob, s_len, end_clamped, "end_clamped_hi")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();

        let sub_len = self
            .builder
            .build_int_sub(end_clamped, start_clamped, "sub_len")
            .map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?;
        let alloc_size = self
            .builder
            .build_int_add(sub_len, i64_ty.const_int(1, false), "alloc_size")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        let buf = self.malloc_buffer(alloc_size)?;
        let src = self.build_in_bounds_gep(i8_ty, s_ptr, &[start_clamped], "src")?;
        self.memcpy_buffer(buf, src, sub_len, "memcpy_call")?;
        self.null_terminate(buf, sub_len)?;
        Ok(buf.into())
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
    fn extract_string_arg(
        &self,
        arg: &BasicMetadataValueEnum<'ctx>,
        context: &str,
    ) -> MimiResult<PointerValue<'ctx>> {
        match arg {
            BasicMetadataValueEnum::PointerValue(pv) => Ok(*pv),
            BasicMetadataValueEnum::StructValue(sv) => self
                .build_extract_value((*sv).into(), 0, "str_ptr")
                .map(|v| v.into_pointer_value())
                .map_err(|e| CompileError::LlvmError(format!("extract str ptr: {}", e))),
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
        let _ = self.build_call(
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
