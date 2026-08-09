use crate::codegen::{CallSiteValueExt, CodeGenerator};
use crate::error::{CompileError, MimiResult};
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn compile_sum(
        &mut self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        // Audit fix 8 (full-audit-2026-08-05): the accumulation is CHECKED —
        // SD-7 / VM parity: interp/bytecode/builtins/list.rs builtin_sum uses
        // `checked_add` and errors with the exact message "sum overflow"
        // (no silent wrap). The accumulator stays i64, matching the int path
        // of the VM implementation.
        //
        // Audit wave2 (D-5a): element-type dispatch landed. Codegen list
        // slots are type-erased i64 (f64 rides in via bitcast), so the
        // element type arrives through the `pending_sum_elem_type` channel
        // set at the call site (mirrors `pending_push_elem_type`): for
        // List<f64>/List<f32> the slots are interpreted as f64 bit patterns
        // and accumulated with fadd + a final SD-9 finiteness gate
        // (ieee_depth-gated via check_float_finite), matching the VM's
        // float path (float_sum + int_sum promotion; homogeneous typed
        // lists never mix the two). Without the channel (unknown element
        // type) the i64 path is retained.
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "sum expects 1 argument (list)".to_string(),
            ));
        }
        let elem_type = self.pending_sum_elem_type.take();
        let list_ptr = self.require_list_pointer(args[0], "sum")?;
        let i64_ty = self.context.i64_type();
        let list_len = self.load_list_len(list_ptr)?;
        let data_ptr = self.load_list_data_i64(list_ptr)?;
        if matches!(elem_type.as_deref(), Some("f64") | Some("f32")) {
            return self.compile_sum_f64_loop(list_len, data_ptr);
        }
        // Loop through list elements and sum
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for sum loop".to_string())?;
        let loop_bb = self.context.append_basic_block(function, "sum_loop");
        let body_bb = self.context.append_basic_block(function, "sum_body");
        let done_bb = self.context.append_basic_block(function, "sum_done");
        let idx_alloca = self
            .builder
            .build_alloca(i64_ty, "si")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        let sum_alloca = self
            .builder
            .build_alloca(i64_ty, "sum")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        self.builder
            .build_store(idx_alloca, i64_ty.const_int(0, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder
            .build_store(sum_alloca, i64_ty.const_int(0, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(loop_bb);
        let idx = self
            .builder
            .build_load(i64_ty, idx_alloca, "idx")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let cmp = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, idx, list_len, "cmp")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.builder
            .build_conditional_branch(cmp, body_bb, done_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(body_bb);
        let elem_ptr = {
            self.gep()
                .build_in_bounds_gep(i64_ty, data_ptr, &[idx], "elem")
        }
        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let elem = self
            .builder
            .build_load(i64_ty, elem_ptr, "elem_val")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let sum = self
            .builder
            .build_load(i64_ty, sum_alloca, "sum")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        // SD-7 checked add: {i64, i1} @llvm.sadd.with.overflow.i64 — same
        // intrinsic pattern as compile_int_binop (expr/operator.rs).
        let saddle_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(i64_ty),
                BasicTypeEnum::IntType(self.context.bool_type()),
            ],
            false,
        );
        let saddle_fn = self
            .module
            .get_function("llvm.sadd.with.overflow.i64")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "llvm.sadd.with.overflow.i64",
                    saddle_ty.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64_ty),
                            BasicMetadataTypeEnum::IntType(i64_ty),
                        ],
                        false,
                    ),
                    Some(inkwell::module::Linkage::External),
                )
            });
        let saddle_call = self
            .builder
            .build_call(
                saddle_fn,
                &[
                    BasicMetadataValueEnum::IntValue(sum),
                    BasicMetadataValueEnum::IntValue(elem),
                ],
                "sum_checked",
            )
            .map_err(|e| CompileError::LlvmError(format!("sadd.with.overflow error: {}", e)))?;
        let saddle_val = saddle_call
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("sadd.with.overflow returned void".to_string()))?
            .into_struct_value();
        let new_sum = self
            .builder
            .build_extract_value(saddle_val, 0, "sum_result")
            .map_err(|e| CompileError::LlvmError(format!("extract error: {}", e)))?
            .into_int_value();
        let overflow = self
            .builder
            .build_extract_value(saddle_val, 1, "sum_overflow")
            .map_err(|e| CompileError::LlvmError(format!("extract error: {}", e)))?
            .into_int_value();
        let sum_ok_bb = self.context.append_basic_block(function, "sum_ok");
        let sum_ovf_bb = self
            .context
            .append_basic_block(function, "sum_overflow_trap");
        self.builder
            .build_conditional_branch(overflow, sum_ovf_bb, sum_ok_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        // Trap block: VM-exact message "sum overflow".
        self.builder.position_at_end(sum_ovf_bb);
        let ovf_msg = self
            .builder
            .build_global_string_ptr("sum overflow", "sum_ovf_msg")
            .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
        let abort_fn = self.get_or_declare_abort_fn();
        self.builder
            .build_call(
                abort_fn,
                &[BasicMetadataValueEnum::PointerValue(
                    ovf_msg.as_pointer_value(),
                )],
                "sum_ovf_abort",
            )
            .map_err(|e| CompileError::LlvmError(format!("call error: {}", e)))?;
        // SAFETY: mimi_runtime_abort is noreturn; this block is unreachable.
        self.builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("unreachable error: {}", e)))?;
        self.builder.position_at_end(sum_ok_bb);
        self.builder
            .build_store(sum_alloca, new_sum)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let next = self
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "next")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.builder
            .build_store(idx_alloca, next)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(done_bb);
        let result = self
            .builder
            .build_load(i64_ty, sum_alloca, "result_sum")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
        Ok(result)
    }

    /// Audit wave2 (D-5a): f64 accumulation for `sum(List<f64>)` /
    /// `sum(List<f32>)`. Slots hold f64 bit patterns; reinterpreting them
    /// as i64 (the old path) printed bit-pattern garbage. VM reference
    /// (builtin_sum): plain float accumulation, result Float. A single
    /// SD-9 gate after the loop is behaviorally equivalent to per-element
    /// gating (non-finiteness, once produced by fadd, persists through all
    /// later fadds — cancellation yields NaN, never a finite value).
    fn compile_sum_f64_loop(
        &mut self,
        list_len: inkwell::values::IntValue<'ctx>,
        data_ptr: inkwell::values::PointerValue<'ctx>,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let i64_ty = self.context.i64_type();
        let f64_ty = self.context.f64_type();
        let function = self.current_function().ok_or_else(|| {
            CompileError::LlvmError("codegen: no current function for sum".into())
        })?;
        let loop_bb = self.context.append_basic_block(function, "sumf_loop");
        let body_bb = self.context.append_basic_block(function, "sumf_body");
        let done_bb = self.context.append_basic_block(function, "sumf_done");
        let idx_alloca = self.build_alloca(i64_ty, "sfi")?;
        let sum_alloca = self.build_alloca(f64_ty, "sumf")?;
        self.build_store(idx_alloca, i64_ty.const_int(0, false))?;
        self.build_store(sum_alloca, f64_ty.const_float(0.0))?;
        self.build_br(loop_bb)?;
        self.builder.position_at_end(loop_bb);
        let idx = self
            .build_load(BasicTypeEnum::IntType(i64_ty), idx_alloca, "sfi_val")?
            .into_int_value();
        let cmp = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, idx, list_len, "sumf_cmp")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.build_cond_br(cmp, body_bb, done_bb)?;
        self.builder.position_at_end(body_bb);
        let elem_ptr = self
            .gep()
            .build_in_bounds_gep(i64_ty, data_ptr, &[idx], "sumf_elem")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let raw = self
            .build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, "sumf_raw")?
            .into_int_value();
        let elem_f = self
            .build_bit_cast(
                raw.into(),
                BasicTypeEnum::FloatType(f64_ty),
                "sumf_elem_f64",
            )
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
            .into_float_value();
        let acc = self
            .build_load(BasicTypeEnum::FloatType(f64_ty), sum_alloca, "sumf_acc")?
            .into_float_value();
        let new_acc = self
            .builder
            .build_float_add(acc, elem_f, "sumf_add")
            .map_err(|e| CompileError::LlvmError(format!("fadd error: {}", e)))?;
        self.build_store(sum_alloca, new_acc)?;
        let next = self
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "sumf_next")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.build_store(idx_alloca, next)?;
        self.build_br(loop_bb)?;
        self.builder.position_at_end(done_bb);
        let result = self
            .build_load(BasicTypeEnum::FloatType(f64_ty), sum_alloca, "sumf_result")?
            .into_float_value();
        // SD-9 finiteness gate (E0813), suspended inside ieee_float{}.
        self.check_float_finite(result, "sum")?;
        Ok(result.into())
    }

    pub(in crate::codegen) fn compile_flatten(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "flatten expects 1 argument (list of lists)".to_string(),
            ));
        }
        let list_ptr = self.require_list_pointer(args[0], "flatten")?;
        let i64_ty = self.context.i64_type();
        let list_struct_ty = self.list_struct_type();
        let outer_len = self.load_list_len(list_ptr)?;
        let data_i8 = self.load_list_data_raw(list_ptr)?;
        let data_ptr = self
            .builder
            .build_bit_cast(
                data_i8,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                "data_list_ptr",
            )
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
            .into_pointer_value();
        // First pass: count total elements
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for flatten loop".to_string())?;
        let count_loop_bb = self
            .context
            .append_basic_block(function, "flatten_count_loop");
        let count_body_bb = self
            .context
            .append_basic_block(function, "flatten_count_body");
        let count_done_bb = self
            .context
            .append_basic_block(function, "flatten_count_done");
        let idx_alloca = self
            .builder
            .build_alloca(i64_ty, "fi")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        let total_alloca = self
            .builder
            .build_alloca(i64_ty, "total")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        self.builder
            .build_store(idx_alloca, i64_ty.const_int(0, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder
            .build_store(total_alloca, i64_ty.const_int(0, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder
            .build_unconditional_branch(count_loop_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(count_loop_bb);
        let idx = self
            .builder
            .build_load(i64_ty, idx_alloca, "idx")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let cmp = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, idx, outer_len, "cmp")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.builder
            .build_conditional_branch(cmp, count_body_bb, count_done_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(count_body_bb);
        let inner_list_ptr = {
            self.gep()
                .build_in_bounds_gep(list_struct_ty, data_ptr, &[idx], "inner_list")
        }
        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let inner_len = self.load_list_len(inner_list_ptr)?;
        let total = self
            .builder
            .build_load(i64_ty, total_alloca, "total")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let new_total = self
            .builder
            .build_int_add(total, inner_len, "new_total")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.builder
            .build_store(total_alloca, new_total)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let next = self
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "next")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.builder
            .build_store(idx_alloca, next)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder
            .build_unconditional_branch(count_loop_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(count_done_bb);
        let total_len = self
            .builder
            .build_load(i64_ty, total_alloca, "total_len")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        // Allocate new array
        let sizeof_i64 = self.list_elem_size();
        let alloc_size = self
            .builder
            .build_int_mul(total_len, sizeof_i64, "alloc_size")
            .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;
        let new_data = self.malloc_or_abort(alloc_size, "malloc_call")?;
        let new_data_i64 = self
            .builder
            .build_bit_cast(
                new_data,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                "new_data_i64",
            )
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
            .into_pointer_value();
        // Second pass: copy elements
        let copy_outer_bb = self
            .context
            .append_basic_block(function, "flatten_copy_outer");
        let copy_outer_body_bb = self
            .context
            .append_basic_block(function, "flatten_copy_outer_body");
        let copy_inner_bb = self
            .context
            .append_basic_block(function, "flatten_copy_inner");
        let copy_inner_body_bb = self
            .context
            .append_basic_block(function, "flatten_copy_inner_body");
        let copy_done_bb = self
            .context
            .append_basic_block(function, "flatten_copy_done");
        let outer_idx_alloca = self
            .builder
            .build_alloca(i64_ty, "foi")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        let inner_idx_alloca = self
            .builder
            .build_alloca(i64_ty, "fii")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        let dest_idx_alloca = self
            .builder
            .build_alloca(i64_ty, "fdi")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        self.builder
            .build_store(outer_idx_alloca, i64_ty.const_int(0, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder
            .build_store(dest_idx_alloca, i64_ty.const_int(0, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder
            .build_unconditional_branch(copy_outer_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(copy_outer_bb);
        let outer_idx = self
            .builder
            .build_load(i64_ty, outer_idx_alloca, "outer_idx")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let outer_cmp = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                outer_idx,
                outer_len,
                "outer_cmp",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.builder
            .build_conditional_branch(outer_cmp, copy_outer_body_bb, copy_done_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(copy_outer_body_bb);
        let inner_list_ptr = {
            self.gep()
                .build_in_bounds_gep(list_struct_ty, data_ptr, &[outer_idx], "inner_list")
        }
        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let inner_len = self.load_list_len(inner_list_ptr)?;
        let inner_data_ptr = self.load_list_data_i64(inner_list_ptr)?;
        self.builder
            .build_store(inner_idx_alloca, i64_ty.const_int(0, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder
            .build_unconditional_branch(copy_inner_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(copy_inner_bb);
        let inner_idx = self
            .builder
            .build_load(i64_ty, inner_idx_alloca, "inner_idx")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let inner_cmp = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                inner_idx,
                inner_len,
                "inner_cmp",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.builder
            .build_conditional_branch(inner_cmp, copy_inner_body_bb, copy_outer_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(copy_inner_body_bb);
        let src_ptr = {
            self.gep()
                .build_in_bounds_gep(i64_ty, inner_data_ptr, &[inner_idx], "inner_elem")
        }
        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let src_val = self
            .builder
            .build_load(i64_ty, src_ptr, "inner_elem_val")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let dest_idx = self
            .builder
            .build_load(i64_ty, dest_idx_alloca, "dest_idx")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let dest_ptr = {
            self.gep()
                .build_in_bounds_gep(i64_ty, new_data_i64, &[dest_idx], "dest_elem")
        }
        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(dest_ptr, src_val)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let next_dest = self
            .builder
            .build_int_add(dest_idx, i64_ty.const_int(1, false), "next_dest")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.builder
            .build_store(dest_idx_alloca, next_dest)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let next_inner = self
            .builder
            .build_int_add(inner_idx, i64_ty.const_int(1, false), "next_inner")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.builder
            .build_store(inner_idx_alloca, next_inner)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder
            .build_unconditional_branch(copy_inner_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        // After inner loop: increment outer_idx and continue
        self.builder.position_at_end(copy_outer_bb);
        let next_outer = self
            .builder
            .build_int_add(outer_idx, i64_ty.const_int(1, false), "next_outer")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.builder
            .build_store(outer_idx_alloca, next_outer)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder.position_at_end(copy_done_bb);
        // Build result list struct
        let result_alloca = self.alloc_list_result(total_len, new_data)?;
        Ok(result_alloca.into())
    }

    pub(in crate::codegen) fn compile_enumerate(
        &mut self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "enumerate expects 1 argument (list)".to_string(),
            ));
        }
        let list_ptr = self.require_list_pointer(args[0], "enumerate")?;
        let i64_ty = self.context.i64_type();
        let list_len = self.load_list_len(list_ptr)?;
        let data_ptr = self.load_list_data_i64(list_ptr)?;
        // Heap-pack layout: each result element is an 8-byte pointer to a
        // 16-byte pair {i64 i, i64 v} on the heap. This matches the product-
        // tuple formatter's heap-pack assumption (List<(i32, T)> elements are
        // pointers, see emit_list_product_tuple_to_string) and the push builtin's
        // struct-copy path — raw 16-byte inline pairs made enumerate display
        // segfault (formatter dereferenced the first slot as a pointer).
        let alloc_size = self
            .builder
            .build_int_mul(list_len, i64_ty.const_int(8, false), "enum_alloc_size")
            .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;
        let result_data = self.malloc_or_abort(alloc_size, "enum_malloc")?;
        let result_data_i64 = self
            .builder
            .build_bit_cast(
                result_data,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                "enum_result_i64",
            )
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
            .into_pointer_value();
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for enumerate loop".to_string())?;
        let loop_bb = self.context.append_basic_block(function, "enum_loop");
        let body_bb = self.context.append_basic_block(function, "enum_body");
        let done_bb = self.context.append_basic_block(function, "enum_done");
        let idx_alloca = self
            .builder
            .build_alloca(i64_ty, "enum_idx")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        self.builder
            .build_store(idx_alloca, i64_ty.const_int(0, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(loop_bb);
        let idx = self
            .builder
            .build_load(i64_ty, idx_alloca, "enum_idx_val")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let cmp = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, idx, list_len, "enum_cmp")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.builder
            .build_conditional_branch(cmp, body_bb, done_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(body_bb);
        let elem_ptr = {
            self.gep()
                .build_in_bounds_gep(i64_ty, data_ptr, &[idx], "enum_elem")
        }
        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let elem = self
            .builder
            .build_load(i64_ty, elem_ptr, "enum_elem_val")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        // 0.35.20 (#6): heap-pack the pair with the formatter's tuple layout
        // (string fields inline {ptr,len}) so enumerate display matches
        // bytecode. Falls back to two raw i64 slots when the type is unknown.
        let pair_ty = self.pending_zip_pair_type.take();
        let pair_heap = self.build_zip_pair(idx, elem, pair_ty.as_deref())?;
        let pair_i64 = self
            .builder
            .build_bit_cast(
                pair_heap,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                "enum_pair_i64",
            )
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
            .into_pointer_value();
        let pair_slot = {
            self.gep()
                .build_in_bounds_gep(i64_ty, result_data_i64, &[idx], "enum_pair_slot")
        }
        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(pair_slot, pair_i64)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let next = self
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "enum_next")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.builder
            .build_store(idx_alloca, next)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(done_bb);
        let result_alloca = self.alloc_list_result(list_len, result_data)?;
        Ok(result_alloca.into())
    }

    pub(in crate::codegen) fn compile_zip(
        &mut self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "zip expects 2 arguments (list, list)".to_string(),
            ));
        }
        let (list_ptr_a, list_ptr_b) = match (&args[0], &args[1]) {
            (
                BasicMetadataValueEnum::PointerValue(pv_a),
                BasicMetadataValueEnum::PointerValue(pv_b),
            ) => (pv_a, pv_b),
            _ => {
                return Err(CompileError::TypeMismatch(
                    "zip: both args must be lists".to_string(),
                ))
            }
        };
        let i64_ty = self.context.i64_type();
        let len_a = self.load_list_len(*list_ptr_a)?;
        let len_b = self.load_list_len(*list_ptr_b)?;
        let min_len = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, len_a, len_b, "zip_min")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let min_len = self
            .builder
            .build_select(min_len, len_a, len_b, "zip_min_len")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        let data_ptr_a = self.load_list_data_i64(*list_ptr_a)?;
        let data_ptr_b = self.load_list_data_i64(*list_ptr_b)?;
        // Heap-pack layout: each result element is an 8-byte pointer to a
        // 16-byte pair {i64 a, i64 b} on the heap — matching the product-tuple
        // formatter's heap-pack assumption and the push builtin's struct-copy
        // path. The previous raw 16-byte inline pairs made zip display print
        // empty (formatter read the pair's first slot as a pointer into the
        // second slot's integer bits).
        let alloc_size = self
            .builder
            .build_int_mul(min_len, i64_ty.const_int(8, false), "zip_alloc_size")
            .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;
        let result_data = self.malloc_or_abort(alloc_size, "zip_malloc")?;
        let result_data_i64 = self
            .builder
            .build_bit_cast(
                result_data,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                "zip_result_i64",
            )
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
            .into_pointer_value();
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for zip loop".to_string())?;
        let loop_bb = self.context.append_basic_block(function, "zip_loop");
        let body_bb = self.context.append_basic_block(function, "zip_body");
        let done_bb = self.context.append_basic_block(function, "zip_done");
        let idx_alloca = self
            .builder
            .build_alloca(i64_ty, "zip_idx")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        self.builder
            .build_store(idx_alloca, i64_ty.const_int(0, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(loop_bb);
        let idx = self
            .builder
            .build_load(i64_ty, idx_alloca, "zip_idx_val")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let cmp = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, idx, min_len, "zip_cmp")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.builder
            .build_conditional_branch(cmp, body_bb, done_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(body_bb);
        let elem_a_ptr = {
            self.gep()
                .build_in_bounds_gep(i64_ty, data_ptr_a, &[idx], "zip_elem_a")
        }
        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let elem_a = self
            .builder
            .build_load(i64_ty, elem_a_ptr, "zip_elem_a_val")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let elem_b_ptr = {
            self.gep()
                .build_in_bounds_gep(i64_ty, data_ptr_b, &[idx], "zip_elem_b")
        }
        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let elem_b = self
            .builder
            .build_load(i64_ty, elem_b_ptr, "zip_elem_b_val")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        // 0.35.20 (#6): heap-pack the pair with the formatter's tuple layout
        // (string fields inline {ptr,len}) so zip display matches bytecode.
        // Falls back to two raw i64 slots when the type is unknown.
        let pair_ty = self.pending_zip_pair_type.take();
        let pair_heap = self.build_zip_pair(elem_a, elem_b, pair_ty.as_deref())?;
        let pair_i64 = self
            .builder
            .build_bit_cast(
                pair_heap,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                "zip_pair_i64",
            )
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
            .into_pointer_value();
        let pair_slot = {
            self.gep()
                .build_in_bounds_gep(i64_ty, result_data_i64, &[idx], "zip_pair_slot")
        }
        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(pair_slot, pair_i64)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let next = self
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "zip_next")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.builder
            .build_store(idx_alloca, next)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(done_bb);
        let result_alloca = self.alloc_list_result(min_len, result_data)?;
        Ok(result_alloca.into())
    }

    /// 0.35.20 (#6): heap-pack a {a, b} pair with the LLVM layout of
    /// `pair_ty_str` (e.g. "(string, i32)") so the product-tuple formatter
    /// reads it correctly. String fields are inlined as {ptr, len} (strlen),
    /// matching the tuple-literal layout; nested Lists are loaded by value;
    /// floats are bitcast; narrow ints truncated. Falls back to two raw i64
    /// slots when the type is unknown (callers that did not thread the type
    /// through the pending_zip_pair_type channel).
    fn build_zip_pair(
        &self,
        elem_a: inkwell::values::IntValue<'ctx>,
        elem_b: inkwell::values::IntValue<'ctx>,
        pair_ty_str: Option<&str>,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let pair_ty = pair_ty_str
            .and_then(|ts| crate::codegen::expr::call::helpers::parse_type_str(ts))
            .and_then(|ty| self.llvm_type_for(&ty));
        if let Some(BasicTypeEnum::StructType(sty)) = pair_ty {
            let size = self.llvm_type_size_bytes(BasicTypeEnum::StructType(sty));
            let pair_heap = self.malloc_or_abort(
                i64_ty.const_int(size, false),
                "zip_pair_heap",
            )?;
            let fields = sty.get_field_types();
            let srcs = [elem_a, elem_b];
            for (i, ft) in fields.iter().enumerate() {
                let src = srcs[i];
                let field_gep = self
                    .gep()
                    .build_struct_gep(sty, pair_heap, i as u32, &format!("zip_pair_f{}", i))
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                match ft {
                    BasicTypeEnum::IntType(it) => {
                        let bw = it.get_bit_width();
                        let v: BasicValueEnum<'ctx> = if bw == 64 {
                            BasicValueEnum::IntValue(src)
                        } else if bw == 1 {
                            BasicValueEnum::IntValue(
                                self.builder
                                    .build_int_truncate(src, *it, "zip_pair_bool")
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("trunc: {}", e))
                                    })?,
                            )
                        } else {
                            BasicValueEnum::IntValue(
                                self.builder
                                    .build_int_truncate(src, *it, "zip_pair_int")
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("trunc: {}", e))
                                    })?,
                            )
                        };
                        self.builder
                            .build_store(field_gep, v)
                            .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                    }
                    BasicTypeEnum::FloatType(ftt) => {
                        let f = self
                            .builder
                            .build_bit_cast(src, *ftt, "zip_pair_float")
                            .map_err(|e| CompileError::LlvmError(format!("bitcast: {}", e)))?;
                        self.builder
                            .build_store(field_gep, f)
                            .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                    }
                    BasicTypeEnum::PointerType(pt) => {
                        let p = self
                            .builder
                            .build_int_to_ptr(src, *pt, "zip_pair_ptr")
                            .map_err(|e| CompileError::LlvmError(format!("inttoptr: {}", e)))?;
                        self.builder
                            .build_store(field_gep, p)
                            .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                    }
                    BasicTypeEnum::StructType(fsty) => {
                        let ffs = fsty.get_field_types();
                        if ffs.len() == 2
                            && matches!(ffs[0], BasicTypeEnum::PointerType(_))
                            && matches!(
                                ffs[1],
                                BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                            )
                        {
                            // string {ptr, len}: ptr + strlen.
                            // 0.35.20 FIX: the inner {ptr,i64} GEPs must be
                            // based on field_gep (this field's address inside
                            // the pair), NOT pair_heap. Using pair_heap with a
                            // {ptr,i64} type made field-0 GEPs land at offset 0
                            // regardless of the field index — for zip(string,i32)
                            // the string is field 0 so the write happened to
                            // land on its own slot (accidentally correct), but
                            // enumerate(i32,string) wrote ptr+len over the idx
                            // and ptr slots, so the formatter read ptr=1 and
                            // strlen(0x1) SIGSEGV'd. zip+enumerate in one
                            // function exposed it (type-aware 24B path only
                            // engaged when a prior zip call primed the pair
                            // type channel).
                            let p = self
                                .builder
                                .build_int_to_ptr(src, i8_ptr, "zip_pair_str_ptr")
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("inttoptr: {}", e))
                                })?;
                            let ptr_gep = self
                                .gep()
                                .build_struct_gep(*fsty, field_gep, 0, "zip_pair_str_p")
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("gep error: {}", e))
                                })?;
                            self.builder
                                .build_store(ptr_gep, p)
                                .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                            let strlen_fn = self.get_runtime_fn("strlen")?;
                            let len = self
                                .builder
                                .build_call(
                                    strlen_fn,
                                    &[BasicMetadataValueEnum::PointerValue(p)],
                                    "zip_pair_strlen",
                                )
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("strlen: {}", e))
                                })?
                                .try_as_basic_value_opt()
                                .ok_or("strlen returned void")?
                                .into_int_value();
                            let len_gep = self
                                .gep()
                                .build_struct_gep(*fsty, field_gep, 1, "zip_pair_str_l")
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("gep error: {}", e))
                                })?;
                            self.builder
                                .build_store(len_gep, len)
                                .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                        } else if ffs.len() == 2
                            && matches!(
                                ffs[0],
                                BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                            )
                            && matches!(ffs[1], BasicTypeEnum::PointerType(_))
                        {
                            // Nested List {i64 len, ptr data}: the slot holds a
                            // pointer to the list struct — load by value into
                            // the tuple field (matches tuple-literal layout).
                            let p = self
                                .builder
                                .build_int_to_ptr(src, i8_ptr, "zip_pair_list_ptr")
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("inttoptr: {}", e))
                                })?;
                            let loaded = self
                                .build_load(
                                    BasicTypeEnum::StructType(*fsty),
                                    p,
                                    "zip_pair_list_ld",
                                )?
                                .into_struct_value();
                            self.builder
                                .build_store(field_gep, loaded)
                                .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                        } else {
                            // Other struct (Option/Record element): store the
                            // slot bits as a pointer to the struct (best effort;
                            // these element kinds are rare in zip/enumerate).
                            let p = self
                                .builder
                                .build_int_to_ptr(src, i8_ptr, "zip_pair_rec_ptr")
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("inttoptr: {}", e))
                                })?;
                            self.builder
                                .build_store(field_gep, p)
                                .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                        }
                    }
                    _ => {
                        self.builder
                            .build_store(field_gep, src)
                            .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                    }
                }
            }
            return Ok(pair_heap);
        }
        // Fallback: two raw i64 slots (16 bytes).
        let pair_heap = self.malloc_or_abort(i64_ty.const_int(16, false), "zip_pair_heap")?;
        let pair_i64 = self
            .builder
            .build_bit_cast(
                pair_heap,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                "zip_pair_i64",
            )
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
            .into_pointer_value();
        let pair_a_ptr = self
            .gep()
            .build_in_bounds_gep(i64_ty, pair_i64, &[i64_ty.const_int(0, false)], "zip_pair_a")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(pair_a_ptr, elem_a)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let pair_b_ptr = self
            .gep()
            .build_in_bounds_gep(
                i64_ty,
                pair_i64,
                &[i64_ty.const_int(1, false)],
                "zip_pair_b",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(pair_b_ptr, elem_b)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        Ok(pair_heap)
    }
}
