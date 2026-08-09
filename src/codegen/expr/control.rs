use crate::ast::*;
use crate::codegen::{CallSiteValueExt, CodeGenerator, VarEntry};
use crate::error::CompileError;

use inkwell::types::BasicTypeEnum;
use inkwell::values::BasicValue;
use inkwell::values::BasicValueEnum;
use std::collections::HashMap;

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn compile_if_expr(
        &mut self,
        cond: &Expr,
        then_: &Block,
        else_: &Option<Block>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let cond_val = self.compile_expr(cond, vars)?;
        let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
            // H-22 (full-audit 2026-08-05 §2.6): builtin predicates (contains,
            // str_contains, …) return i64 0/1 booleans. Passing an i64 straight
            // to `br` emits invalid IR (`br i64`, instruction-selection crash);
            // normalize to i1 first. The if-statement/while family in
            // block.rs:1191 / func.rs:2432 already applies this same
            // normalization — the if-EXPRESSION path had been missed.
            if iv.get_type().get_bit_width() == 1 {
                iv
            } else {
                let zero = iv.get_type().const_int(0, false);
                self.builder
                    .build_int_compare(inkwell::IntPredicate::NE, iv, zero, "ifexpr_cond")
                    .map_err(|e| CompileError::LlvmError(format!("cond normalize: {}", e)))?
            }
        } else {
            return Err("if expression condition must be boolean".into());
        };
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for if expr".to_string())?;
        let then_bb = self.context.append_basic_block(function, "ifexpr_then");
        let else_bb = self.context.append_basic_block(function, "ifexpr_else");
        let merge_bb = self.context.append_basic_block(function, "ifexpr_merge");
        self.builder
            .build_conditional_branch(cond_bool, then_bb, else_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        // Then branch
        self.builder.position_at_end(then_bb);
        let mut then_vars = vars.clone();
        let then_val = self
            .compile_block_last_val(then_, &mut then_vars)
            .map_err(|e| CompileError::Generic(e.to_string()))?;
        // 0.35.23 deep-eval: a string-literal branch (e.g. `else { "ERROR" }`
        // in mimi-log's level fallback chain) yields a raw C-string pointer
        // while a sibling branch yields the {ptr,i64} struct — the mismatch
        // made the merge phi E0200 ("branches have incompatible types").
        // Mirror the func.rs if-statement path's normalization.
        let then_val = self.normalize_block_last_string(then_val, then_)?;
        let then_reaches = !self.block_has_terminator();
        if then_reaches {
            self.builder
                .build_unconditional_branch(merge_bb)
                .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        }
        let then_bb_end = then_reaches
            .then(|| self.builder.get_insert_block())
            .flatten();
        // Else branch
        self.builder.position_at_end(else_bb);
        let (mut else_val, else_reaches) = if let Some(eb) = else_ {
            let mut else_vars = vars.clone();
            let mut v = self
                .compile_block_last_val(eb, &mut else_vars)
                .map_err(|e| CompileError::Generic(e.to_string()))?;
            // 0.35.23 deep-eval: same string-literal normalization as the
            // then branch (mimi-log `else { "INFO" }` vs `{ lvl }`).
            v = self.normalize_block_last_string(v, eb)?;
            let reaches = !self.block_has_terminator();
            if reaches {
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
            }
            (Some(v), reaches)
        } else {
            // CG-H5: no else arm — produce a zero/unit value of then type so the
            // PHI has an incoming from else_bb (LLVM requires all predecessors).
            let reaches = !self.block_has_terminator();
            let zero = self.const_zero_for_type(then_val.get_type());
            if reaches {
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
            }
            (Some(zero), reaches)
        };
        let else_bb_end = else_reaches
            .then(|| self.builder.get_insert_block())
            .flatten();
        // Merge with phi (only from blocks that actually reach merge)
        self.builder.position_at_end(merge_bb);
        let mut ty = then_val.get_type();
        let mut then_val = then_val;
        // Unify Option branch layouts: a bare `None` arm compiles to a narrow
        // {i1,i64} struct, while `Some(string)`/`Some(record)` arms carry the
        // full payload layout {i1,{ptr,i64}}. The VM unifies these branches
        // fine; native used to refuse with E0200 (L1 divergence). Widen the
        // narrow None arm to the sibling arm's layout (payload zero-filled —
        // safe, a None arm's payload is meaningless).
        if let Some(ev) = else_val {
            if ev.get_type() != ty {
                if let Some(w) = self.widen_option_none_to_layout(ev, ty)? {
                    else_val = Some(w);
                } else if let Some(w) = self.widen_option_none_to_layout(then_val, ev.get_type())? {
                    then_val = w;
                }
            }
        }
        // Unify integer widths (mirror of the block.rs if-statement path):
        // branches may produce different-width integers (e.g. i64 literal vs
        // i32 expression — mimi-log percentile `if idx < 0 { 0 } else if … {
        // len(..) - 1 } else { idx }`). Extend the narrower value in its OWN
        // predecessor block (before the terminator) so the phi stays
        // type-uniform without SSA dominance violations.
        let then_bw = match &then_val {
            BasicValueEnum::IntValue(iv) => iv.get_type().get_bit_width(),
            _ => 0,
        };
        let else_bw = match &else_val {
            Some(BasicValueEnum::IntValue(iv)) => iv.get_type().get_bit_width(),
            _ => 0,
        };
        if then_bw > 0 && else_bw > 0 && then_bw != else_bw && then_reaches && else_reaches {
            let target_bw = then_bw.max(else_bw);
            let nz_bw = std::num::NonZeroU32::new(target_bw as u32)
                .ok_or_else(|| CompileError::LlvmError("ifexpr target width is zero".into()))?;
            let target_ty = self
                .context
                .custom_width_int_type(nz_bw)
                .map_err(|e| CompileError::LlvmError(format!("ifexpr target width: {}", e)))?;
            if then_bw < target_bw {
                let bb =
                    then_bb_end.ok_or_else(|| CompileError::LlvmError("ifexpr then bb".into()))?;
                self.builder.position_at_end(bb);
                if let Some(term) = bb.get_terminator() {
                    self.builder.position_before(&term);
                }
                let tv = then_val.into_int_value();
                let widened = if tv.get_type().get_bit_width() == 1 {
                    self.builder
                        .build_int_z_extend(tv, target_ty, "ifexpr_then_zext")
                        .map_err(|e| CompileError::LlvmError(format!("ifexpr z_ext: {}", e)))?
                } else {
                    self.builder
                        .build_int_s_extend(tv, target_ty, "ifexpr_then_sext")
                        .map_err(|e| CompileError::LlvmError(format!("ifexpr s_ext: {}", e)))?
                };
                then_val = BasicValueEnum::IntValue(widened);
            }
            if else_bw < target_bw {
                let bb =
                    else_bb_end.ok_or_else(|| CompileError::LlvmError("ifexpr else bb".into()))?;
                self.builder.position_at_end(bb);
                if let Some(term) = bb.get_terminator() {
                    self.builder.position_before(&term);
                }
                let ev = else_val
                    .ok_or_else(|| {
                        CompileError::LlvmError("ifexpr-else ext: missing value".into())
                    })?
                    .into_int_value();
                let widened = if ev.get_type().get_bit_width() == 1 {
                    self.builder
                        .build_int_z_extend(ev, target_ty, "ifexpr_else_zext")
                        .map_err(|e| CompileError::LlvmError(format!("ifexpr z_ext: {}", e)))?
                } else {
                    self.builder
                        .build_int_s_extend(ev, target_ty, "ifexpr_else_sext")
                        .map_err(|e| CompileError::LlvmError(format!("ifexpr s_ext: {}", e)))?
                };
                else_val = Some(BasicValueEnum::IntValue(widened));
            }
            self.builder.position_at_end(merge_bb);
        }
        // Record-literal branches compile to alloca POINTERS while a sibling
        // branch (function return, nested expr) yields the struct VALUE
        // directly (mimi-make `if n > 0 { parse_one(..) } else { Rule { .. } }`
        // — the Rule literal is a ptr, parse_one returns the struct). Load the
        // pointer branch inside its own predecessor block so the phi unifies.
        let (then_val, else_val) = match (&then_val, &else_val) {
            (BasicValueEnum::PointerValue(tv), Some(BasicValueEnum::StructValue(ev)))
                if then_reaches =>
            {
                let bb =
                    then_bb_end.ok_or_else(|| CompileError::LlvmError("ifexpr rec bb".into()))?;
                self.builder.position_at_end(bb);
                if let Some(term) = bb.get_terminator() {
                    self.builder.position_before(&term);
                }
                let loaded = self.build_load(
                    BasicTypeEnum::StructType(ev.get_type()),
                    *tv,
                    "ifexpr_rec_then",
                )?;
                (loaded, else_val)
            }
            (BasicValueEnum::StructValue(tv), Some(BasicValueEnum::PointerValue(ev)))
                if else_reaches =>
            {
                let bb =
                    else_bb_end.ok_or_else(|| CompileError::LlvmError("ifexpr rec bb".into()))?;
                self.builder.position_at_end(bb);
                if let Some(term) = bb.get_terminator() {
                    self.builder.position_before(&term);
                }
                let loaded = self.build_load(
                    BasicTypeEnum::StructType(tv.get_type()),
                    *ev,
                    "ifexpr_rec_else",
                )?;
                (then_val, Some(loaded))
            }
            _ => (then_val, else_val),
        };
        self.builder.position_at_end(merge_bb);
        ty = then_val.get_type();
        let phi = self
            .builder
            .build_phi(ty, "ifexpr_result")
            .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
        // BUG-2 fix: Add incoming values one at a time to avoid lifetime issues
        // from storing &dyn BasicValue references that borrow from local variables.
        if let Some(bb) = then_bb_end {
            phi.add_incoming(&[(&then_val as &dyn inkwell::values::BasicValue, bb)]);
        }
        if let (Some(bb), Some(ev)) = (else_bb_end, else_val) {
            // When types differ (rare after zero fill), refuse silent mismatch.
            if ev.get_type() != ty {
                return Err(CompileError::TypeMismatch(format!(
                    "if expression branches have incompatible types ({:?} vs {:?})",
                    ty,
                    ev.get_type()
                )));
            }
            phi.add_incoming(&[(&ev as &dyn inkwell::values::BasicValue, bb)]);
        }
        Ok(phi.as_basic_value())
    }

    /// Widen a narrow Option branch value `{i1,i64}` (bare `None` constructor)
    /// to a target layout `{i1,payload}` when the sibling branch carried a real
    /// payload (`Some(string)`, `Some(record)`, …). The payload is zero-filled:
    /// safe, because a `None` arm's payload is meaningless. Returns `None` when
    /// the value is not a narrow Option struct (never masks a genuine mismatch).
    fn widen_option_none_to_layout(
        &self,
        val: BasicValueEnum<'ctx>,
        target: BasicTypeEnum<'ctx>,
    ) -> Result<Option<BasicValueEnum<'ctx>>, CompileError> {
        let (BasicValueEnum::StructValue(sv), BasicTypeEnum::StructType(target_sty)) =
            (val, target)
        else {
            return Ok(None);
        };
        let actual = sv.get_type();
        let af = actual.get_field_types();
        let tf = target_sty.get_field_types();
        // Only Option-shaped 2-field structs with i1 discriminants.
        if af.len() != 2 || tf.len() != 2 {
            return Ok(None);
        }
        let is_i1 =
            |t: &BasicTypeEnum| matches!(t, BasicTypeEnum::IntType(it) if it.get_bit_width() == 1);
        if !is_i1(&af[0]) || !is_i1(&tf[0]) {
            return Ok(None);
        }
        if af[1] == tf[1] {
            return Ok(None); // already compatible
        }
        // Only widen when the value side carries the None zero-pad (plain i64).
        if !matches!(af[1], BasicTypeEnum::IntType(it) if it.get_bit_width() == 64) {
            return Ok(None);
        }
        // Build the widened value in pure SSA (insertvalue) rather than
        // alloca+partial-store+load: the alloca form made LLVM 18's CVP pass
        // crash when the widened value fed a PHI (CalledValuePropagationPass
        // SIGSEGV in visitPHINode).
        let disc = self.build_extract_value(sv.into(), 0, "opt_none_disc")?;
        let zero = self.const_zero_for_type(BasicTypeEnum::StructType(target_sty));
        let widened = self
            .builder
            .build_insert_value(zero.into_struct_value(), disc, 0, "opt_none_widened")
            .map_err(|e| CompileError::LlvmError(format!("insertvalue: {}", e)))?;
        Ok(Some(widened.as_basic_value_enum()))
    }

    /// Slice: `target[start..end]`.
    ///
    /// Semantics are pinned to the bytecode VM's `__slice` builtin
    /// (interp/bytecode/builtins/list.rs `builtin_slice`; P-0 ruling: VM is
    /// reference). The pre-Wave-2 legacy emission diverged on four axes:
    ///
    /// 1. OOB indices were CLAMPED to [0, len] — VM traps (E0814 slice error).
    /// 2. Negative indices were clamped to 0 — VM wraps Python-style:
    ///    `idx < 0 → (len + idx).max(0)`.
    /// 3. String targets were reinterpreted as lists (garbage) — VM slices
    ///    strings by CHARACTER index (`mimi_str_substring` is char-based).
    /// 4. The result ALIASED the source data buffer (no copy; double-free /
    ///    mutation-through hazard) — VM copies (`l[start..end].to_vec()`).
    ///
    /// This emission resolves negatives, fails loud on OOB with the VM's
    /// messages, and copies the slice into a fresh buffer registered for
    /// scope-exit free (same discipline as the match `..rest` copy,
    /// expr/match.rs `build_list_struct`).
    pub(in crate::codegen) fn compile_slice_expr(
        &mut self,
        target: &Expr,
        start: &Option<Box<Expr>>,
        end: &Option<Box<Expr>>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let target_val = self.compile_expr(target, vars)?;
        if self.infer_object_type(target, vars) == "string" {
            return self.compile_string_slice(target_val, start, end, vars);
        }
        self.compile_list_slice(target_val, start, end, vars)
    }

    /// Compile a slice index expression (`Some` → value, `None` → `default`),
    /// sign-extend sub-i64 indices, then resolve a negative index Python-style
    /// exactly like the VM: `idx < 0 → (len + idx).max(0)`.
    fn resolve_slice_index(
        &mut self,
        idx_expr: &Option<Box<Expr>>,
        default: inkwell::values::IntValue<'ctx>,
        len: inkwell::values::IntValue<'ctx>,
        vars: &HashMap<String, VarEntry<'ctx>>,
        name: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, CompileError> {
        let i64_ty = self.context.i64_type();
        let raw = match idx_expr {
            Some(e) => self.compile_expr(e, vars)?.into_int_value(),
            None => default,
        };
        // A1: widen i32 indices to i64 — slice arithmetic uses i64 throughout.
        let raw = if raw.get_type().get_bit_width() < 64 {
            self.builder
                .build_int_s_extend(raw, i64_ty, &format!("{}_sext", name))
                .map_err(|e| CompileError::LlvmError(format!("s_ext error: {}", e)))?
        } else {
            raw
        };
        let zero = i64_ty.const_int(0, false);
        let is_neg = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                raw,
                zero,
                &format!("{}_neg", name),
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        // len + idx (idx negative); clamp the wrap to >= 0 (VM .max(0)).
        let wrapped = self
            .builder
            .build_int_add(len, raw, &format!("{}_wrap", name))
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        let wrap_neg = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                wrapped,
                zero,
                &format!("{}_wrap_neg", name),
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let wrapped = self
            .builder
            .build_select(wrap_neg, zero, wrapped, &format!("{}_wrap_max0", name))
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        self.builder
            .build_select(is_neg, wrapped, raw, &format!("{}_resolved", name))
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))
            .map(|v| v.into_int_value())
    }

    /// Fail loud on a violated slice invariant (VM E0814 parity). Inside a
    /// fallible multi-target transition the abort is absorbed into the Fault
    /// variant (mirrors the div/mod trap sites, expr/operator.rs); elsewhere
    /// abort with the exact VM message.
    fn emit_slice_bounds_trap(
        &mut self,
        bad: inkwell::values::IntValue<'ctx>,
        msg: &str,
    ) -> Result<(), CompileError> {
        let function = self
            .current_function()
            .ok_or_else(|| CompileError::LlvmError("no current function for slice trap".into()))?;
        let ok_bb = self.context.append_basic_block(function, "slice_bounds_ok");
        let fail_bb = self
            .context
            .append_basic_block(function, "slice_bounds_fail");
        self.builder
            .build_conditional_branch(bad, fail_bb, ok_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(fail_bb);
        if self.in_fallible_multi_target() {
            self.emit_panic_fault_return("E0814")?;
        } else {
            let msg_ptr = self
                .builder
                .build_global_string_ptr(msg, "slice_msg")
                .map_err(|e| CompileError::LlvmError(format!("slice msg: {}", e)))?;
            let abort_fn = self.get_or_declare_abort_fn();
            self.builder
                .build_call(
                    abort_fn,
                    &[inkwell::values::BasicMetadataValueEnum::PointerValue(
                        msg_ptr.as_pointer_value(),
                    )],
                    "slice_abort",
                )
                .map_err(|e| CompileError::LlvmError(format!("slice abort call: {}", e)))?;
            self.builder
                .build_unreachable()
                .map_err(|e| CompileError::LlvmError(format!("unreachable error: {}", e)))?;
        }
        self.builder.position_at_end(ok_bb);
        Ok(())
    }

    /// Strict VM-parity bounds gate over already-resolved indices:
    /// start > len, end > len and start > end each trap (E0814 messages
    /// verbatim from `builtin_slice`).
    fn check_slice_bounds(
        &mut self,
        start: inkwell::values::IntValue<'ctx>,
        end: inkwell::values::IntValue<'ctx>,
        len: inkwell::values::IntValue<'ctx>,
    ) -> Result<(), CompileError> {
        let start_oob = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, start, len, "slice_start_oob")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.emit_slice_bounds_trap(start_oob, "slice start out of bounds")?;
        let end_oob = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, end, len, "slice_end_oob")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.emit_slice_bounds_trap(end_oob, "slice end out of bounds")?;
        let start_gt_end = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, start, end, "slice_start_gt_end")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.emit_slice_bounds_trap(start_gt_end, "slice start > end")?;
        Ok(())
    }

    /// List slice: bounds-checked, COPIED result (no aliasing into the source
    /// buffer — VM `l[start..end].to_vec()` parity).
    fn compile_list_slice(
        &mut self,
        target_val: BasicValueEnum<'ctx>,
        start: &Option<Box<Expr>>,
        end: &Option<Box<Expr>>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let target_ptr = match target_val {
            BasicValueEnum::PointerValue(pv) => pv,
            _ => return Err("slice target must be a list/array pointer".into()),
        };
        // Get list length from struct field 0
        let list_ty = self.list_struct_type();
        let len_gep = self
            .gep()
            .build_struct_gep(list_ty, target_ptr, 0, "slice_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let list_len = self
            .builder
            .build_load(
                BasicTypeEnum::IntType(self.context.i64_type()),
                len_gep,
                "len",
            )
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, target_ptr, 1, "slice_data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let data_ptr = self
            .builder
            .build_load(
                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                data_gep,
                "data",
            )
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_pointer_value();

        let i64_ty = self.context.i64_type();
        let zero = i64_ty.const_int(0, false);
        // Defaults: start = 0, end = len (VM compiler-side defaults).
        let start_idx = self.resolve_slice_index(start, zero, list_len, vars, "start")?;
        let end_idx = self.resolve_slice_index(end, list_len, list_len, vars, "end")?;
        self.check_slice_bounds(start_idx, end_idx, list_len)?;

        // new_len = end - start; guaranteed >= 0 by the bounds gate above.
        let new_len = self
            .builder
            .build_int_sub(end_idx, start_idx, "slice_len")
            .map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?;

        // Empty slice → empty list, no allocation (VM returns Vec::new()).
        let function = self
            .current_function()
            .ok_or_else(|| CompileError::LlvmError("no current function for slice".into()))?;
        let is_empty = self
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, new_len, zero, "slice_empty_cmp")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let empty_bb = self.context.append_basic_block(function, "slice_empty_bb");
        let copy_bb = self.context.append_basic_block(function, "slice_copy_bb");
        let merge_bb = self.context.append_basic_block(function, "slice_merge_bb");
        self.builder
            .build_conditional_branch(is_empty, empty_bb, copy_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

        self.builder.position_at_end(empty_bb);
        let null_data = self
            .context
            .ptr_type(inkwell::AddressSpace::default())
            .const_null();
        let empty_list = self.build_list_struct(zero, null_data)?;
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        let empty_end = self
            .builder
            .get_insert_block()
            .ok_or_else(|| CompileError::LlvmError("no insert block after empty slice".into()))?;

        // Copy path: malloc a fresh buffer and memcpy the element range.
        self.builder.position_at_end(copy_bb);
        let elem_size = i64_ty.const_int(8, false);
        let bytes = self
            .builder
            .build_int_mul(new_len, elem_size, "slice_bytes")
            .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;
        let dest = self.malloc_or_abort(bytes, "slice_data")?;
        let byte_offset = self
            .builder
            .build_int_mul(start_idx, elem_size, "slice_offset")
            .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let data_i8 = self
            .builder
            .build_pointer_cast(data_ptr, i8_ptr, "data_as_i8")
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?;
        let src_i8 = self
            .gep()
            .build_in_bounds_gep(self.context.i8_type(), data_i8, &[byte_offset], "slice_src")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        // SAFETY: bounds gate proved start..end ⊆ [0, len), so src covers
        // `bytes` bytes inside the list data allocation; dest is a fresh
        // `bytes`-sized malloc_or_abort allocation; regions are disjoint.
        let memcpy_fn = self.get_runtime_fn("memcpy")?;
        self.builder
            .build_call(
                memcpy_fn,
                &[
                    inkwell::values::BasicMetadataValueEnum::PointerValue(dest),
                    inkwell::values::BasicMetadataValueEnum::PointerValue(src_i8),
                    inkwell::values::BasicMetadataValueEnum::IntValue(bytes),
                ],
                "slice_memcpy",
            )
            .map_err(|e| CompileError::LlvmError(format!("memcpy: {}", e)))?;
        // build_list_struct registers the data slot for scope-exit free; the
        // copy OWNS its buffer (no aliasing, no double-free of the source).
        let copy_list = self.build_list_struct(new_len, dest)?;
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        let copy_end = self
            .builder
            .get_insert_block()
            .ok_or_else(|| CompileError::LlvmError("no insert block after slice copy".into()))?;

        self.builder.position_at_end(merge_bb);
        let phi = self
            .builder
            .build_phi(empty_list.get_type(), "slice_result_phi")
            .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
        phi.add_incoming(&[
            (&empty_list as &dyn inkwell::values::BasicValue, empty_end),
            (&copy_list as &dyn inkwell::values::BasicValue, copy_end),
        ]);
        Ok(phi.as_basic_value())
    }

    /// String slice: CHARACTER-indexed (VM `s.chars()` parity). Defaults,
    /// negative wrap and bounds gate are identical to the list path; the
    /// substring itself is produced by the runtime's char-based
    /// `mimi_str_substring` (fresh allocation, aborts are unreachable after
    /// the gate), wrapped back into the canonical {ptr, len} string struct.
    fn compile_string_slice(
        &mut self,
        target_val: BasicValueEnum<'ctx>,
        start: &Option<Box<Expr>>,
        end: &Option<Box<Expr>>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // Extract the raw C pointer; keep the byte length when the target is
        // the canonical {ptr, i64} string struct so the char count stays
        // bounded (never a NUL walk, which would truncate embedded NULs).
        let (str_ptr, byte_bound) = match target_val {
            BasicValueEnum::PointerValue(pv) => (pv, None),
            BasicValueEnum::StructValue(sv) => {
                let fields = sv.get_type().get_field_types();
                let is_string_struct = matches!(
                    fields.as_slice(),
                    [BasicTypeEnum::PointerType(_), BasicTypeEnum::IntType(t)]
                        if t.get_bit_width() == 64
                );
                if !is_string_struct {
                    return Err("slice target must be a list/array pointer".into());
                }
                let ptr = self
                    .builder
                    .build_extract_value(sv, 0, "str_data_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("extract error: {}", e)))?
                    .into_pointer_value();
                let byte_len = self
                    .builder
                    .build_extract_value(sv, 1, "str_byte_len")
                    .map_err(|e| CompileError::LlvmError(format!("extract error: {}", e)))?
                    .into_int_value();
                (ptr, Some(byte_len))
            }
            _ => return Err("slice target must be a list/array pointer".into()),
        };
        // VM index space is Unicode scalar values — count chars, not bytes.
        let char_len = self.count_utf8_chars(str_ptr, byte_bound)?;
        let i64_ty = self.context.i64_type();
        let zero = i64_ty.const_int(0, false);
        let start_idx = self.resolve_slice_index(start, zero, char_len, vars, "start")?;
        let end_idx = self.resolve_slice_index(end, char_len, char_len, vars, "end")?;
        self.check_slice_bounds(start_idx, end_idx, char_len)?;
        let sub_fn = self.get_runtime_fn("mimi_str_substring")?;
        let sub_ptr = self
            .build_call(
                sub_fn,
                &[
                    inkwell::values::BasicMetadataValueEnum::PointerValue(str_ptr),
                    inkwell::values::BasicMetadataValueEnum::IntValue(start_idx),
                    inkwell::values::BasicMetadataValueEnum::IntValue(end_idx),
                ],
                "str_slice",
            )?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("mimi_str_substring returned void".into()))?
            .into_pointer_value();
        self.wrap_c_string(sub_ptr)
    }
}
