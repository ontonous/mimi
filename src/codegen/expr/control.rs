use crate::ast::*;
use crate::codegen::{CodeGenerator, VarEntry};
use crate::error::CompileError;

use inkwell::types::BasicTypeEnum;
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
            iv
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
        let (else_val, else_reaches) = if let Some(eb) = else_ {
            let mut else_vars = vars.clone();
            let v = self
                .compile_block_last_val(eb, &mut else_vars)
                .map_err(|e| CompileError::Generic(e.to_string()))?;
            let reaches = !self.block_has_terminator();
            if reaches {
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
            }
            (Some(v), reaches)
        } else {
            let reaches = !self.block_has_terminator();
            if reaches {
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
            }
            (None, reaches)
        };
        let else_bb_end = else_reaches
            .then(|| self.builder.get_insert_block())
            .flatten();
        // Merge with phi (only from blocks that actually reach merge)
        self.builder.position_at_end(merge_bb);
        let ty = then_val.get_type();
        let phi = self
            .builder
            .build_phi(ty, "ifexpr_result")
            .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
        // BUG-2 fix: Add incoming values one at a time to avoid lifetime issues
        // from storing &dyn BasicValue references that borrow from local variables.
        if let Some(bb) = then_bb_end {
            phi.add_incoming(&[(&then_val as &dyn inkwell::values::BasicValue, bb)]);
        }
        // Only add else incoming when we have an actual else value.
        // When else_val is None (no else block), the implicit unit value should not
        // be phi'd with a then_val of different type (e.g. struct).
        if let (Some(bb), Some(ev)) = (else_bb_end, else_val) {
            phi.add_incoming(&[(&ev as &dyn inkwell::values::BasicValue, bb)]);
        }
        Ok(phi.as_basic_value())
    }

    pub(in crate::codegen) fn compile_range_expr(
        &mut self,
        start: &Expr,
        end: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let start_val = self.compile_expr(start, vars)?;
        let end_val = self.compile_expr(end, vars)?;
        let start_iv = match start_val {
            BasicValueEnum::IntValue(iv) => iv,
            _ => return Err("range start must be i64".into()),
        };
        let end_iv = match end_val {
            BasicValueEnum::IntValue(iv) => iv,
            _ => return Err("range end must be i64".into()),
        };
        // A1: widen i32 to i64 — range struct stores {i64, i64}.
        let i64_ty = self.context.i64_type();
        let start_iv = if start_iv.get_type().get_bit_width() < 64 {
            self.builder
                .build_int_s_extend(start_iv, i64_ty, "range_start_sext")
                .map_err(|e| CompileError::LlvmError(format!("s_ext error: {}", e)))?
        } else {
            start_iv
        };
        let end_iv = if end_iv.get_type().get_bit_width() < 64 {
            self.builder
                .build_int_s_extend(end_iv, i64_ty, "range_end_sext")
                .map_err(|e| CompileError::LlvmError(format!("s_ext error: {}", e)))?
        } else {
            end_iv
        };
        // Create a range struct { start: i64, end: i64 }
        let range_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(self.context.i64_type()),
                BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        );
        let alloca = self
            .builder
            .build_alloca(range_ty, "range")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        let start_gep = self
            .gep()
            .build_struct_gep(range_ty, alloca, 0, "range_start")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(start_gep, start_iv)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let end_gep = self
            .gep()
            .build_struct_gep(range_ty, alloca, 1, "range_end")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(end_gep, end_iv)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        Ok(alloca.into())
    }

    pub(in crate::codegen) fn compile_slice_expr(
        &mut self,
        target: &Expr,
        start: &Option<Box<Expr>>,
        end: &Option<Box<Expr>>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // Slice: arr[start..end] — compile target, compute slice offset and length
        let target_val = self.compile_expr(target, vars)?;
        let target_ptr = match target_val {
            BasicValueEnum::PointerValue(pv) => pv,
            _ => return Err("slice target must be a list/array pointer".into()),
        };
        // Get list length from struct field 0
        let list_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(self.context.i64_type()),
                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
            ],
            false,
        );
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
        // Compute start index (default 0)
        let i64_ty = self.context.i64_type();
        let start_idx = match start {
            Some(e) => self.compile_expr(e, vars)?.into_int_value(),
            None => i64_ty.const_int(0, false),
        };
        // Compute end index (default: list length)
        let end_idx = match end {
            Some(e) => self.compile_expr(e, vars)?.into_int_value(),
            None => list_len,
        };
        // A1: widen i32 indices to i64 — slice arithmetic uses i64 throughout.
        let start_idx = if start_idx.get_type().get_bit_width() < 64 {
            self.builder
                .build_int_s_extend(start_idx, i64_ty, "start_sext")
                .map_err(|e| CompileError::LlvmError(format!("s_ext error: {}", e)))?
        } else {
            start_idx
        };
        let end_idx = if end_idx.get_type().get_bit_width() < 64 {
            self.builder
                .build_int_s_extend(end_idx, i64_ty, "end_sext")
                .map_err(|e| CompileError::LlvmError(format!("s_ext error: {}", e)))?
        } else {
            end_idx
        };
        // Compute new length = end - start (clamped to 0 if start > end)
        let start_gt_end = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SGT,
                start_idx,
                end_idx,
                "slice_start_gt_end",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let safe_end = self
            .builder
            .build_select(start_gt_end, start_idx, end_idx, "slice_safe_end")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        let new_len = self
            .builder
            .build_int_sub(safe_end, start_idx, "slice_len")
            .map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?;
        // Compute new data pointer: data + start * sizeof(i64)
        let elem_size = i64_ty.const_int(8, false);
        let byte_offset = self
            .builder
            .build_int_mul(start_idx, elem_size, "slice_offset")
            .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let data_i8 = self
            .builder
            .build_pointer_cast(data_ptr, i8_ptr, "data_as_i8")
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?;
        let new_data_i8 = {
            self.gep().build_in_bounds_gep(
                self.context.i8_type(),
                data_i8,
                &[byte_offset],
                "new_data",
            )
        }
        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let new_data_ptr = self
            .builder
            .build_pointer_cast(
                new_data_i8,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                "new_data_void",
            )
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?;
        // Build new list struct { new_len, new_data_ptr }
        let result_alloca = self
            .builder
            .build_alloca(list_ty, "slice_result")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        let rlen_gep = self
            .gep()
            .build_struct_gep(list_ty, result_alloca, 0, "rlen")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(rlen_gep, new_len)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let rdata_gep = self
            .gep()
            .build_struct_gep(list_ty, result_alloca, 1, "rdata")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(rdata_gep, new_data_ptr)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        Ok(result_alloca.into())
    }
}
