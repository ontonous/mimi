use crate::ast::*;
use crate::codegen::{CallSiteValueExt, CodeGenerator, VarEntry};
use crate::error::CompileError;

use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{
    AggregateValueEnum, BasicMetadataValueEnum, BasicValueEnum, FloatValue, IntValue, PointerValue,
};
use std::collections::HashMap;

impl<'ctx> CodeGenerator<'ctx> {
    /// Wrap a raw C string pointer into a Mimi string struct `{ ptr, i64 }`.
    /// Calls `strlen` to compute the length, then builds the struct.
    pub(in crate::codegen) fn wrap_c_string(
        &self,
        raw_ptr: PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let string_struct_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr_ty),
                BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        );

        // Call strlen to get the length
        let strlen_fn = self.get_runtime_fn("strlen")?;
        let length = self
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(raw_ptr)],
                "strlen_call",
            )?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("strlen returned void".into()))?
            .into_int_value();

        // Build the struct { data_ptr, len }
        let str_val = self
            .builder
            .build_insert_value(string_struct_ty.get_undef(), raw_ptr, 0, "str_data")
            .map_err(|e| CompileError::LlvmError(format!("insert str ptr: {}", e)))?;
        let str_val = self
            .builder
            .build_insert_value(str_val, length, 1, "str_len")
            .map_err(|e| CompileError::LlvmError(format!("insert str len: {}", e)))?;

        Ok(str_val.into_struct_value().into())
    }

    /// Extract a string data pointer from a Mimi string value.
    pub(in crate::codegen) fn extract_string_ptr(
        &self,
        val: &BasicValueEnum<'ctx>,
    ) -> Option<PointerValue<'ctx>> {
        match val {
            BasicValueEnum::PointerValue(pv) => Some(*pv),
            BasicValueEnum::StructValue(sv) => {
                if let Ok(BasicValueEnum::PointerValue(pv)) =
                    self.build_extract_value(AggregateValueEnum::StructValue(*sv), 0, "str_data")
                {
                    Some(pv)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub(in crate::codegen) fn compile_binary_expr(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // full-audit 2026-08-05 §7 (VERIFIED CRITICAL, L1): `and`/`or` were
        // compiled eagerly — both sides evaluated, then bitwise build_and /
        // build_or. The bytecode VM short-circuits (compile_short_circuit in
        // interp/bytecode/compiler.rs), so eager lowering trapped on effects
        // (e.g. div-by-zero) the VM never reaches and evaluated side effects
        // on the skipped branch. Lower through the short-circuit machine.
        if matches!(op, BinOp::And | BinOp::Or) {
            return self.compile_short_circuit_expr(op, lhs, rhs, vars);
        }
        let l = self.compile_expr(lhs, vars)?;
        let r = self.compile_expr(rhs, vars)?;
        // Legacy (raw AST) has no canonical types; let compile_binop use the
        // 0.34.34 operand-width heuristic (None).
        self.compile_binop(op, l, r, None)
    }

    /// Short-circuit lowering for `and`/`or`, matching the bytecode VM
    /// (`compile_short_circuit`, interp/bytecode/compiler.rs):
    ///
    /// - `l and r`: evaluate `l`; falsy → `false` without touching `r`;
    ///   truthy → the value of `r` (evaluated only on this path).
    /// - `l or r`: evaluate `l`; truthy → `true` without touching `r`;
    ///   falsy → the value of `r`.
    ///
    /// The checker restricts `and`/`or` operands to bool (E0202); i64-bool
    /// results from builtins (`contains` and friends) are normalized by
    /// `coerce_condition_value`, which mirrors the VM's `is_truthy`.
    fn compile_short_circuit_expr(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let function = self.current_function().ok_or_else(|| {
            CompileError::LlvmError("no current function for short-circuit op".into())
        })?;
        let l = self.compile_expr(lhs, vars)?;
        let cond = self.coerce_condition_value(l)?;

        let rhs_bb = self.context.append_basic_block(function, "sc_rhs");
        let const_bb = self.context.append_basic_block(function, "sc_const");
        let merge_bb = self.context.append_basic_block(function, "sc_merge");

        // and: truthy LHS → evaluate RHS; falsy LHS → constant false.
        // or:  truthy LHS → constant true;  falsy LHS → evaluate RHS.
        let (truthy_bb, falsy_bb) = match op {
            BinOp::And => (rhs_bb, const_bb),
            BinOp::Or => (const_bb, rhs_bb),
            _ => return Err(format!("unsupported short-circuit operator {:?}", op).into()),
        };
        self.builder
            .build_conditional_branch(cond, truthy_bb, falsy_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

        // RHS arm — compiled ONLY on the branch that needs it.
        self.builder.position_at_end(rhs_bb);
        let r = self.compile_expr(rhs, vars)?;
        let result_ty = r.get_type();
        let rhs_reaches = !self.block_has_terminator();
        if rhs_reaches {
            self.builder
                .build_unconditional_branch(merge_bb)
                .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        }
        let rhs_bb_end = rhs_reaches
            .then(|| self.builder.get_insert_block())
            .flatten();

        // Constant arm: `false` for `and`, `true` for `or`, in the RHS
        // value's own type so the merge phi is well-typed. The VM yields
        // Value::Bool(false/true) here; for wider i64-bool RHS values the
        // 0/1 constant is truthiness-equivalent.
        self.builder.position_at_end(const_bb);
        let const_val = self.short_circuit_const(op, result_ty)?;
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

        self.builder.position_at_end(merge_bb);
        let phi = self
            .builder
            .build_phi(result_ty, "sc_result")
            .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
        if let Some(bb) = rhs_bb_end {
            phi.add_incoming(&[(&r as &dyn inkwell::values::BasicValue, bb)]);
        }
        phi.add_incoming(&[(&const_val as &dyn inkwell::values::BasicValue, const_bb)]);
        Ok(phi.as_basic_value())
    }

    /// The short-circuit result constant in the RHS value's type.
    fn short_circuit_const(
        &self,
        op: BinOp,
        ty: BasicTypeEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let bit = match op {
            BinOp::And => 0u64, // LHS falsy → false
            BinOp::Or => 1u64,  // LHS truthy → true
            _ => return Err(format!("unsupported short-circuit operator {:?}", op).into()),
        };
        match ty {
            BasicTypeEnum::IntType(it) => Ok(it.const_int(bit, false).into()),
            other => Err(CompileError::Generic(format!(
                "'and'/'or' result must be boolean, got {}",
                type_description(&other)
            ))),
        }
    }

    /// Coerce a value to an i1 branch condition, mirroring the bytecode VM's
    /// `is_truthy` (interp/value.rs): bools pass through, integers are
    /// `!= 0`, floats are `x != 0.0 && !isnan(x)` (ordered fcmp ONE against
    /// 0.0 is exactly that — ordered comparisons are false on NaN).
    fn coerce_condition_value(
        &self,
        val: BasicValueEnum<'ctx>,
    ) -> Result<IntValue<'ctx>, CompileError> {
        match val {
            BasicValueEnum::IntValue(iv) => {
                if iv.get_type().get_bit_width() == 1 {
                    Ok(iv)
                } else {
                    // Some builtins (e.g. contains) return i64 for bool.
                    let zero = iv.get_type().const_int(0, false);
                    Ok(self
                        .builder
                        .build_int_compare(inkwell::IntPredicate::NE, iv, zero, "truthy")
                        .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?)
                }
            }
            BasicValueEnum::FloatValue(fv) => {
                let zero = fv.get_type().const_float(0.0);
                Ok(self
                    .builder
                    .build_float_compare(inkwell::FloatPredicate::ONE, fv, zero, "truthy")
                    .map_err(|e| CompileError::LlvmError(format!("fcmp error: {}", e)))?)
            }
            other => Err(CompileError::Generic(format!(
                "'and'/'or' condition requires boolean, got {}",
                type_description(&other.get_type())
            ))),
        }
    }

    pub(in crate::codegen) fn compile_unary_expr(
        &mut self,
        op: UnOp,
        inner: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        if matches!(op, UnOp::Ref | UnOp::RefMut)
            && matches!(
                inner.unlocated(),
                Expr::Ident(_)
                    | Expr::Field(..)
                    | Expr::TupleIndex(..)
                    | Expr::Index(..)
                    | Expr::Unary(UnOp::Deref, _)
            )
        {
            return self
                .compile_place_addr(inner, vars)
                .map(|(pointer, _)| pointer.into());
        }
        let v = self.compile_expr(inner, vars)?;
        match op {
            UnOp::Neg => {
                if let BasicValueEnum::IntValue(iv) = v {
                    // SD-7 (0.34.34): negation is `0 - x` and MUST go through
                    // compile_binop's checked path. A raw `sub` wraps silently:
                    // -i32::MIN would not trap, and a promoted-width neg would
                    // lose the i32 range guard. compile_binop also detects
                    // i32 operand width from `iv` directly.
                    let zero = iv.get_type().const_int(0, true);
                    return self.compile_binop(BinOp::Sub, zero.into(), iv.into(), None);
                } else if let BasicValueEnum::FloatValue(fv) = v {
                    let zero = self.context.f64_type().const_float(0.0);
                    Ok(self
                        .builder
                        .build_float_sub(zero, fv, "fneg")
                        .map_err(|e| CompileError::LlvmError(format!("neg error: {}", e)))?
                        .into())
                } else {
                    let ty_desc = type_description(&v.get_type());
                    Err(format!("negation requires numeric type, got {}", ty_desc).into())
                }
            }
            UnOp::Not => {
                if let BasicValueEnum::IntValue(iv) = v {
                    if iv.get_type().get_bit_width() == 1 {
                        Ok(self
                            .builder
                            .build_not(iv, "not")
                            .map_err(|e| CompileError::LlvmError(format!("not error: {}", e)))?
                            .into())
                    } else {
                        // Some builtins (e.g. contains) return i64 for bool.
                        // Normalize to i1 with `x == 0` so it can feed `if`.
                        let zero = iv.get_type().const_int(0, false);
                        Ok(self
                            .builder
                            .build_int_compare(inkwell::IntPredicate::EQ, iv, zero, "not")
                            .map_err(|e| CompileError::LlvmError(format!("not error: {}", e)))?
                            .into())
                    }
                } else {
                    let ty_desc = type_description(&v.get_type());
                    Err(format!("'not' requires bool, got {}", ty_desc).into())
                }
            }
            UnOp::Ref | UnOp::RefMut => {
                let ty = v.get_type();
                let alloca = self.build_alloca(ty, "ref")?;
                self.build_store(alloca, v)?;
                Ok(alloca.into())
            }
            UnOp::Deref => {
                if let BasicValueEnum::PointerValue(ptr) = v {
                    // full-audit 2026-08-05 §7: fail-closed pointee resolution.
                    // The previous chain guessed i64 for every unknown pointee,
                    // silently loading garbage for narrower/wider referents.
                    // Derive the load type from tracked types only; anything
                    // underivable is a compile error, never a guess.
                    let pointee_ty = match inner.unlocated() {
                        Expr::Ident(name) => self.resolve_deref_pointee_type(name, vars)?,
                        other => {
                            return Err(CompileError::Unsupported(format!(
                                "dereference of {:?} is unsupported in codegen: pointee type must be derivable from a tracked variable",
                                other
                            )));
                        }
                    };
                    Ok(self.build_load(pointee_ty, ptr, "deref")?)
                } else {
                    let ty_desc = type_description(&v.get_type());
                    Err(format!("dereference requires pointer type, got {}", ty_desc).into())
                }
            }
        }
    }

    /// Derive the pointee type for `*name` without guessing
    /// (full-audit 2026-08-05 §7, fail-closed):
    ///
    /// 1. `var_types` — borrow parameters register the POINTED-TO type
    ///    (func.rs inserts the Ref/RefMut inner for `&T`/`&mut T` params);
    ///    annotated local refs may carry the `Type::Ref(_, inner)` wrapper
    ///    itself, so unwrap it before lowering.
    /// 2. The variable's storage type when it is not itself a pointer.
    /// 3. Pointer-typed slots with no tracked pointee keep the legacy i64
    ///    slot convention: the corpus only reaches this arm for let-bound
    ///    borrows of i64-width slots (list elements, i64 locals — see
    ///    tests/real_world/ownership_cfg.mimi). A true fix needs VM-style
    ///    borrow-alias tracking at the let-bind site (block.rs), which is
    ///    outside this group's ownership — Wave-2 follow-up.
    fn resolve_deref_pointee_type(
        &self,
        name: &str,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicTypeEnum<'ctx>, CompileError> {
        if let Some(ast_ty) = self.var_types.get(name) {
            let pointee_ast = match ast_ty.unlocated() {
                Type::Ref(_, inner) | Type::RefMut(_, inner) => (**inner).clone(),
                _ => ast_ty.clone(),
            };
            if let Some(ty) = self.llvm_type_for(&pointee_ast) {
                return match ty {
                    BasicTypeEnum::PointerType(_) => Err(CompileError::Generic(format!(
                        "cannot dereference '{}': pointee type is itself a pointer \
                         (double indirection has no derivable referent in codegen)",
                        name
                    ))),
                    other => Ok(other),
                };
            }
        }
        match vars.get(name).copied() {
            Some((_, BasicTypeEnum::PointerType(_))) => {
                Ok(BasicTypeEnum::IntType(self.context.i64_type()))
            }
            Some((_, ty)) => Ok(ty),
            None => Err(CompileError::Generic(format!(
                "cannot dereference '{}': pointee type is unknown and codegen never guesses",
                name
            ))),
        }
    }

    pub(in crate::codegen) fn compile_binop(
        &mut self,
        op: BinOp,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
        i32_ctx_hint: Option<bool>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // 0.34.34 (SD-7 / L1): i32 width context. promote_binop_operands
        // widens mixed operands to i64, which silently loses the i32 range:
        // `x: i32 + 1` (literal i64) became a checked i64 add that never
        // traps at i32::MAX + 1, diverging from the bytecode VM and from
        // native checked-i32 lowering. The declared width is recoverable
        // from the PRE-promotion operands.
        //
        // 0.34.35 (audit §6-#57 / L1): operand bit width is NOT a reliable
        // width oracle — `1 + x: i32` (64+32) is i32-width while
        // `let xs: List<i64> = [...]; xs[0] + 1` (64+32 too) is i64-width.
        // The two only differ in which side is the literal. So:
        //   - resolved emitter (which has checker-finalized canonical types)
        //     passes Some(expression.ty is i32) — exact, never wrong;
        //   - legacy emitter (raw AST, no canonical types) passes None and
        //     falls back to the 0.34.34 `||` heuristic. Legacy stores every
        //     int slot as i64 and i32-annotated slots as true i32 (0.34.34),
        //     so its only mixed pair is i32-slot vs i64-literal — `||` is
        //     correct there and the List<i64>-element case cannot occur
        //     (legacy list elements are i64 slots).
        let i32_ctx = match i32_ctx_hint {
            Some(is_i32) => is_i32,
            None => {
                matches!(lhs, BasicValueEnum::IntValue(l) if l.get_type().get_bit_width() == 32)
                    || matches!(rhs, BasicValueEnum::IntValue(r) if r.get_type().get_bit_width() == 32)
            }
        };
        let (lhs, rhs) = self.promote_binop_operands(lhs, rhs)?;
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                // i32::MIN / -1 overflows i32 but NOT the promoted i64
                // division — guard the operand pair before the op (matches
                // the native i32 path's MIN/-1 check and the VM's
                // CheckI32DivRem guard).
                if i32_ctx && matches!(op, BinOp::Div | BinOp::Mod) {
                    if let (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) = (lhs, rhs) {
                        self.emit_i32_min_neg1_guard(l, r)?;
                    }
                }
                let result = self.compile_arithmetic_binop(op, lhs, rhs)?;
                // Div results only overflow via MIN/-1 (guarded above);
                // add/sub/mul need the post-op range check.
                if i32_ctx && op != BinOp::Div {
                    if let BasicValueEnum::IntValue(rv) = result {
                        if rv.get_type().get_bit_width() > 32 {
                            let name = match op {
                                BinOp::Add => "addition",
                                BinOp::Sub => "subtraction",
                                BinOp::Mul => "multiplication",
                                _ => "operation",
                            };
                            self.emit_i32_range_guard(rv, name)?;
                        }
                    }
                }
                Ok(result)
            }
            BinOp::Mod => {
                if i32_ctx {
                    if let (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) = (lhs, rhs) {
                        self.emit_i32_min_neg1_guard(l, r)?;
                    }
                }
                self.compile_mod_binop(lhs, rhs)
            }
            BinOp::EqCmp | BinOp::NeCmp => self.compile_equality_binop(op, lhs, rhs),
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                self.compile_comparison_binop(op, lhs, rhs)
            }
            BinOp::And | BinOp::Or => self.compile_logical_binop(op, lhs, rhs),
            BinOp::Range => self.compile_range_binop(lhs, rhs),
            BinOp::Pow => {
                let result = self.compile_pow_binop(lhs, rhs)?;
                // pow at i32 width computes in i64 then narrows with wrap
                // (observed codegen semantics: 2 ** 31 -> i32::MIN, no trap).
                if i32_ctx {
                    if let BasicValueEnum::IntValue(rv) = result {
                        if rv.get_type().get_bit_width() > 32 {
                            let wrapped = self.wrap_i32_result(rv)?;
                            return Ok(wrapped.into());
                        }
                    }
                }
                Ok(result)
            }
            BinOp::Shl | BinOp::Shr => self.compile_shift_binop(op, lhs, rhs, i32_ctx),
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                self.compile_bitwise_binop(op, lhs, rhs)
            }
        }
    }

    /// Post-op i32 range guard for promoted-width arithmetic (0.34.34).
    /// Traps with the same E0802 message text as the native checked-i32
    /// path; in fallible multi-target transitions the overflow is absorbed
    /// into the Fault variant (same as the intrinsic overflow path).
    pub(in crate::codegen) fn emit_i32_range_guard(
        &mut self,
        val: IntValue<'ctx>,
        op_name: &str,
    ) -> Result<(), CompileError> {
        let ty = val.get_type();
        let function = self
            .current_function()
            .ok_or_else(|| CompileError::LlvmError("no current function for i32 guard".into()))?;
        let min32 = ty.const_int(i32::MIN as u64, false);
        let max32 = ty.const_int(i32::MAX as u64, false);
        let lt = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, val, min32, "i32_lt_min")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let gt = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, val, max32, "i32_gt_max")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let oob = self
            .builder
            .build_or(lt, gt, "i32_oob")
            .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?;
        let trap_bb = self.context.append_basic_block(function, "trap_i32_ovf");
        let ok_bb = self.context.append_basic_block(function, "i32_ok");
        self.builder
            .build_conditional_branch(oob, trap_bb, ok_bb)
            .map_err(|e| CompileError::LlvmError(format!("br error: {}", e)))?;
        self.builder.position_at_end(trap_bb);
        if self.in_fallible_multi_target() {
            self.emit_panic_fault_return("E0802")?;
        } else {
            let trap_fn = self.get_runtime_fn("mimi_trap_overflow")?;
            let op_cstr = self
                .builder
                .build_global_string_ptr(op_name, "op_name")
                .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
            self.builder
                .build_call(
                    trap_fn,
                    &[BasicMetadataValueEnum::PointerValue(
                        op_cstr.as_pointer_value(),
                    )],
                    "",
                )
                .map_err(|e| CompileError::LlvmError(format!("call error: {}", e)))?;
            self.builder
                .build_unreachable()
                .map_err(|e| CompileError::LlvmError(format!("unreachable error: {}", e)))?;
        }
        self.builder.position_at_end(ok_bb);
        Ok(())
    }

    /// i32 MIN / -1 operand guard for div/mod at promoted width (0.34.34).
    fn emit_i32_min_neg1_guard(
        &mut self,
        l: IntValue<'ctx>,
        r: IntValue<'ctx>,
    ) -> Result<(), CompileError> {
        let ty = l.get_type();
        if ty.get_bit_width() <= 32 {
            return Ok(()); // native i32 path already checks MIN/-1
        }
        let function = self.current_function().ok_or_else(|| {
            CompileError::LlvmError("no current function for i32 div guard".into())
        })?;
        let min32 = ty.const_int(i32::MIN as u64, false);
        let neg1 = ty.const_all_ones();
        let l_min = self
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, l, min32, "l_is_i32_min")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let r_neg1 = self
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, r, neg1, "r_is_neg1")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let both = self
            .builder
            .build_and(l_min, r_neg1, "i32_min_div_neg1")
            .map_err(|e| CompileError::LlvmError(format!("and error: {}", e)))?;
        let trap_bb = self
            .context
            .append_basic_block(function, "trap_i32_div_ovf");
        let ok_bb = self.context.append_basic_block(function, "i32_div_ok");
        self.builder
            .build_conditional_branch(both, trap_bb, ok_bb)
            .map_err(|e| CompileError::LlvmError(format!("br error: {}", e)))?;
        self.builder.position_at_end(trap_bb);
        if self.in_fallible_multi_target() {
            self.emit_panic_fault_return("E0802")?;
        } else {
            let trap_fn = self.get_runtime_fn("mimi_trap_div_overflow")?;
            self.builder
                .build_call(trap_fn, &[], "")
                .map_err(|e| CompileError::LlvmError(format!("call error: {}", e)))?;
            self.builder
                .build_unreachable()
                .map_err(|e| CompileError::LlvmError(format!("unreachable error: {}", e)))?;
        }
        self.builder.position_at_end(ok_bb);
        Ok(())
    }

    /// Truncate-and-sign-extend back to the pipeline's i64 convention,
    /// giving the i32-wrapped value (0.34.34). SEXT (not ZEXT): the pipeline
    /// stores narrow integers sign-extended in i64, so 0x80000000 must wrap
    /// to -2147483648 (i32::MIN), matching native i32 value semantics.
    fn wrap_i32_result(&mut self, v: IntValue<'ctx>) -> Result<IntValue<'ctx>, CompileError> {
        let ty64 = v.get_type();
        let i32_ty = self.context.i32_type();
        let t = self
            .builder
            .build_int_truncate(v, i32_ty, "wrap_i32")
            .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))?;
        let z = self
            .builder
            .build_int_s_extend(t, ty64, "wrap_i32_sext")
            .map_err(|e| CompileError::LlvmError(format!("sext error: {}", e)))?;
        Ok(z)
    }

    /// Shifts with hardware-mask semantics (0.34.34).
    ///
    /// The shift amount is masked modulo the operand width BEFORE shifting:
    /// unmasked out-of-range shifts are poison in LLVM IR (O1 constant
    /// folding leaks garbage, e.g. `1 << 65`), while x86 SHL/SAR and
    /// aarch64 LSL/ASR mask the amount in hardware — O0 codegen already
    /// observed the masked behavior. For promoted i32 contexts the amount
    /// masks modulo 32 and the result wraps into the i32 width, matching
    /// native checked-i32 lowering and the bytecode VM's MaskShiftAmt/WrapI32.
    fn compile_shift_binop(
        &mut self,
        op: BinOp,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
        i32_ctx: bool,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match (lhs, rhs) {
            (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) => {
                let ty = l.get_type();
                let eff_width: u64 = if i32_ctx {
                    32
                } else {
                    ty.get_bit_width() as u64
                };
                let mask = ty.const_int(eff_width - 1, false);
                let r_masked = self
                    .builder
                    .build_and(r, mask, "shift_amt_masked")
                    .map_err(|e| CompileError::LlvmError(format!("shift mask error: {}", e)))?;
                let shifted = match op {
                    BinOp::Shl => self.builder.build_left_shift(l, r_masked, "shl"),
                    _ => self.builder.build_right_shift(l, r_masked, true, "shr"),
                }
                .map_err(|e| CompileError::LlvmError(format!("shift error: {}", e)))?;
                if i32_ctx && ty.get_bit_width() > 32 {
                    let wrapped = self.wrap_i32_result(shifted)?;
                    Ok(wrapped.into())
                } else {
                    Ok(shifted.into())
                }
            }
            _ => Err("shifts require matching integer types".into()),
        }
    }

    /// Promote integer operands to a common width and integer operands to float
    /// when mixed with a float operand.
    fn promote_binop_operands(
        &self,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> Result<(BasicValueEnum<'ctx>, BasicValueEnum<'ctx>), CompileError> {
        match (lhs, rhs) {
            (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) => {
                let lw = l.get_type().get_bit_width();
                let rw = r.get_type().get_bit_width();
                if lw == rw {
                    Ok((lhs, rhs))
                } else if lw < rw {
                    // i1 (bool) values must be zero-extended so `true` stays 1,
                    // not sign-extended which would produce -1 (all 1s).
                    let ext = if lw == 1 {
                        self.builder.build_int_z_extend(l, r.get_type(), "promote")
                    } else {
                        self.builder.build_int_s_extend(l, r.get_type(), "promote")
                    }
                    .map_err(|e| CompileError::LlvmError(format!("int promote error: {}", e)))?;
                    Ok((ext.into(), rhs))
                } else {
                    let ext = if rw == 1 {
                        self.builder.build_int_z_extend(r, l.get_type(), "promote")
                    } else {
                        self.builder.build_int_s_extend(r, l.get_type(), "promote")
                    }
                    .map_err(|e| CompileError::LlvmError(format!("int promote error: {}", e)))?;
                    Ok((lhs, ext.into()))
                }
            }
            // Mixed integer/float operands: promote the integer side to float.
            (BasicValueEnum::IntValue(i), BasicValueEnum::FloatValue(f)) => {
                let promoted = self
                    .builder
                    .build_signed_int_to_float(i, f.get_type(), "promote_float")
                    .map_err(|e| CompileError::LlvmError(format!("float promote error: {}", e)))?;
                Ok((promoted.into(), f.into()))
            }
            (BasicValueEnum::FloatValue(f), BasicValueEnum::IntValue(i)) => {
                let promoted = self
                    .builder
                    .build_signed_int_to_float(i, f.get_type(), "promote_float")
                    .map_err(|e| CompileError::LlvmError(format!("float promote error: {}", e)))?;
                Ok((f.into(), promoted.into()))
            }
            _ => Ok((lhs, rhs)),
        }
    }

    /// Dispatch arithmetic operators (`+`, `-`, `*`, `/`) to the appropriate
    /// integer, float, or string implementation.
    fn compile_arithmetic_binop(
        &mut self,
        op: BinOp,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match (lhs, rhs) {
            (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) => {
                self.compile_int_binop(op, l, r)
            }
            (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) => {
                self.compile_float_binop(op, l, r)
            }
            (BasicValueEnum::PointerValue(l), BasicValueEnum::PointerValue(r))
                if op == BinOp::Add =>
            {
                self.compile_string_binop(l, r)
            }
            _ => {
                if op == BinOp::Add {
                    if let (Some(l), Some(r)) =
                        (self.extract_string_ptr(&lhs), self.extract_string_ptr(&rhs))
                    {
                        return self.compile_string_binop(l, r);
                    }
                }
                let msg = match op {
                    BinOp::Add => "add requires same numeric types",
                    BinOp::Sub => "sub requires same numeric types",
                    BinOp::Mul => "mul requires same numeric types",
                    BinOp::Div => "div requires same numeric types",
                    _ => "arithmetic requires same numeric types",
                };
                Err(msg.into())
            }
        }
    }

    /// Integer arithmetic (`+`, `-`, `*`, `/`).
    ///
    /// SD-7 (0.31.51a): add/sub/mul use LLVM checked arithmetic intrinsics.
    /// On overflow, calls mimi_trap_overflow (E0802 — unified by audit-codegen
    /// M1; E0801 is reserved for division by zero).
    /// SD-8 (0.31.51a): div/mod check for zero divisor and MIN/-1.
    /// On violation, calls mimi_trap_div_by_zero / mimi_trap_div_overflow.
    fn compile_int_binop(
        &mut self,
        op: BinOp,
        l: IntValue<'ctx>,
        r: IntValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let int_ty = l.get_type();
        let bit_width = int_ty.get_bit_width();
        let function = self
            .current_function()
            .ok_or_else(|| CompileError::LlvmError("no current function for int binop".into()))?;

        // SD-8: Division and modulo — check for zero divisor and MIN/-1.
        if matches!(op, BinOp::Div | BinOp::Mod) {
            let zero = int_ty.const_int(0, false);
            let is_zero = self
                .builder
                .build_int_compare(inkwell::IntPredicate::EQ, r, zero, "div_by_zero")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;

            // Trap block: call mimi_trap_div_by_zero (unreachable after).
            let trap_bb = self.context.append_basic_block(function, "trap_div_zero");
            let cont_bb = self.context.append_basic_block(function, "div_cont");
            let chk_br = self
                .builder
                .build_conditional_branch(is_zero, trap_bb, cont_bb)
                .map_err(|e| CompileError::LlvmError(format!("br error: {}", e)))?;
            // 0.35.4 L2: trap 分支 cold 权重（分支布局优化，不改语义）
            crate::codegen::float_chain::mark_cold_trap_branch(self.context, chk_br);

            // Emit trap call — or absorb into Fault in a fallible transition
            // (v0.34.18a: `-> S | Fault` bottoms out a div-by-zero to the Fault
            // variant instead of aborting; mirrors the bytecode VM absorption).
            self.builder.position_at_end(trap_bb);
            if self.in_fallible_multi_target() {
                self.emit_panic_fault_return("E0801")?;
            } else {
                let trap_fn = self.get_runtime_fn("mimi_trap_div_by_zero")?;
                self.builder
                    .build_call(trap_fn, &[], "")
                    .map_err(|e| CompileError::LlvmError(format!("call error: {}", e)))?;
                self.builder
                    .build_unreachable()
                    .map_err(|e| CompileError::LlvmError(format!("unreachable error: {}", e)))?;
            }

            // Continue with safe division.
            self.builder.position_at_end(cont_bb);

            // SD-8: MIN/-1 check. MIN / -1 overflows (result = MIN, not MAX+1).
            // K-1 (full-audit 2026-08-05 §3.6): build the MIN constant with a
            // shift computed in LLVM's constant domain at the TARGET width.
            // The old `int_ty.const_int(1 << (bit_width - 1), false)` computed
            // the shift in Rust u64: for widths > 64 (i128) `1u64 << 127`
            // overflows (debug: panic/ICE; release: wraps to `1 << 63`),
            // seeding the wrong constant and silently defeating the MIN/-1
            // guard (`sdiv i128 MIN, -1` → poison). const_shl on a width-bw
            // type yields the sign-bit pattern for any width 1..=128.
            let min_val = int_ty
                .const_int(1, false)
                .const_shl(int_ty.const_int((bit_width - 1) as u64, false)); // e.g. i32::MIN
            let neg_one = int_ty.const_all_ones(); // -1 in two's complement
            let l_is_min = self
                .builder
                .build_int_compare(inkwell::IntPredicate::EQ, l, min_val, "l_is_min")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
            let r_is_neg1 = self
                .builder
                .build_int_compare(inkwell::IntPredicate::EQ, r, neg_one, "r_is_neg1")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
            let min_div_neg1 = self
                .builder
                .build_and(l_is_min, r_is_neg1, "min_div_neg1")
                .map_err(|e| CompileError::LlvmError(format!("and error: {}", e)))?;

            let trap_ovf_bb = self.context.append_basic_block(function, "trap_div_ovf");
            let safe_bb = self.context.append_basic_block(function, "div_safe");
            let ovf_br = self
                .builder
                .build_conditional_branch(min_div_neg1, trap_ovf_bb, safe_bb)
                .map_err(|e| CompileError::LlvmError(format!("br error: {}", e)))?;
            // 0.35.4 L2: trap 分支 cold 权重
            crate::codegen::float_chain::mark_cold_trap_branch(self.context, ovf_br);

            self.builder.position_at_end(trap_ovf_bb);
            if self.in_fallible_multi_target() {
                // M1 (audit-codegen 2026-08-03): MIN/-1 is integer OVERFLOW,
                // not div-by-zero — E0802 per docs/error-codes.md (the
                // bytecode VM also reports E0802 IntegerOverflow here).
                self.emit_panic_fault_return("E0802")?;
            } else {
                let trap_ovf_fn = self.get_runtime_fn("mimi_trap_div_overflow")?;
                self.builder
                    .build_call(trap_ovf_fn, &[], "")
                    .map_err(|e| CompileError::LlvmError(format!("call error: {}", e)))?;
                self.builder
                    .build_unreachable()
                    .map_err(|e| CompileError::LlvmError(format!("unreachable error: {}", e)))?;
            }

            self.builder.position_at_end(safe_bb);

            let result = if op == BinOp::Div {
                self.builder
                    .build_int_signed_div(l, r, "div")
                    .map_err(|e| CompileError::LlvmError(format!("div error: {}", e)))?
            } else {
                self.builder
                    .build_int_signed_rem(l, r, "rem")
                    .map_err(|e| CompileError::LlvmError(format!("rem error: {}", e)))?
            };
            return Ok(result.into());
        }

        // SD-7: Checked add/sub/mul via LLVM overflow intrinsics.
        let intrinsic_name = match op {
            BinOp::Add => format!("llvm.sadd.with.overflow.i{}", bit_width),
            BinOp::Sub => format!("llvm.ssub.with.overflow.i{}", bit_width),
            BinOp::Mul => format!("llvm.smul.with.overflow.i{}", bit_width),
            _ => return Err(format!("unsupported integer arithmetic operator {:?}", op).into()),
        };

        // Declare the intrinsic: {iN, i1} @llvm.sX.with.overflow.iN(iN, iN)
        let struct_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(int_ty),
                BasicTypeEnum::IntType(self.context.bool_type()),
            ],
            false,
        );
        let fn_type = struct_ty.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(int_ty),
                BasicMetadataTypeEnum::IntType(int_ty),
            ],
            false,
        );
        let intrinsic_fn = self
            .module
            .get_function(&intrinsic_name)
            .unwrap_or_else(|| {
                self.module.add_function(
                    &intrinsic_name,
                    fn_type,
                    Some(inkwell::module::Linkage::External),
                )
            });

        let call = self
            .builder
            .build_call(
                intrinsic_fn,
                &[
                    BasicMetadataValueEnum::IntValue(l),
                    BasicMetadataValueEnum::IntValue(r),
                ],
                "checked_op",
            )
            .map_err(|e| CompileError::LlvmError(format!("intrinsic call error: {}", e)))?;

        let result_struct = call
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("intrinsic returned void".into()))?;

        // Extract value (field 0) and overflow flag (field 1).
        let result_val = self
            .builder
            .build_extract_value(result_struct.into_struct_value(), 0, "op_result")
            .map_err(|e| CompileError::LlvmError(format!("extract value error: {}", e)))?
            .into_int_value();
        let overflow_flag = self
            .builder
            .build_extract_value(result_struct.into_struct_value(), 1, "op_overflow")
            .map_err(|e| CompileError::LlvmError(format!("extract flag error: {}", e)))?
            .into_int_value();

        // Branch on overflow: trap or continue.
        let trap_bb = self.context.append_basic_block(function, "trap_overflow");
        let ok_bb = self.context.append_basic_block(function, "op_ok");
        let ovf_chk = self
            .builder
            .build_conditional_branch(overflow_flag, trap_bb, ok_bb)
            .map_err(|e| CompileError::LlvmError(format!("br error: {}", e)))?;
        // 0.35.4 L2: trap 分支 cold 权重
        crate::codegen::float_chain::mark_cold_trap_branch(self.context, ovf_chk);

        // Trap block — or absorb into Fault in a fallible transition (v0.34.18a).
        self.builder.position_at_end(trap_bb);
        if self.in_fallible_multi_target() {
            // M1 (audit-codegen 2026-08-03): add/sub/mul overflow is E0802
            // (integer overflow) per docs/error-codes.md — E0801 is reserved
            // for division by zero.
            self.emit_panic_fault_return("E0802")?;
        } else {
            let trap_fn = self.get_runtime_fn("mimi_trap_overflow")?;
            let op_name_str = match op {
                BinOp::Add => "addition",
                BinOp::Sub => "subtraction",
                BinOp::Mul => "multiplication",
                _ => "operation",
            };
            let op_cstr = self
                .builder
                .build_global_string_ptr(op_name_str, "op_name")
                .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
            self.builder
                .build_call(
                    trap_fn,
                    &[BasicMetadataValueEnum::PointerValue(
                        op_cstr.as_pointer_value(),
                    )],
                    "",
                )
                .map_err(|e| CompileError::LlvmError(format!("call error: {}", e)))?;
            self.builder
                .build_unreachable()
                .map_err(|e| CompileError::LlvmError(format!("unreachable error: {}", e)))?;
        }

        // OK block: result_val is already available (trap block is unreachable).
        self.builder.position_at_end(ok_bb);

        Ok(result_val.into())
    }

    /// Floating-point arithmetic (`+`, `-`, `*`, `/`).
    ///
    /// SD-9 (0.31.51b): finiteness invariant — result must not be NaN or Inf.
    /// Matches interpreter behavior (interp/ops.rs:27 traps on NaN/Inf, E0813).
    /// The `**` operator routes through the same guard via `check_float_finite`
    /// (full-audit 2026-08-05 §7).
    fn compile_float_binop(
        &mut self,
        op: BinOp,
        l: FloatValue<'ctx>,
        r: FloatValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let res = match op {
            BinOp::Add => self.builder.build_float_add(l, r, "fadd"),
            BinOp::Sub => self.builder.build_float_sub(l, r, "fsub"),
            BinOp::Mul => self.builder.build_float_mul(l, r, "fmul"),
            BinOp::Div => self.builder.build_float_div(l, r, "fdiv"),
            _ => return Err(format!("unsupported float arithmetic operator {:?}", op).into()),
        };
        let result =
            res.map_err(|e| CompileError::LlvmError(format!("{} error: {}", op_name(op), e)))?;

        let op_label = match op {
            BinOp::Add => "addition",
            BinOp::Sub => "subtraction",
            BinOp::Mul => "multiplication",
            BinOp::Div => "division",
            _ => "operation",
        };
        self.check_float_finite(result, op_label)?;
        Ok(result.into())
    }

    /// SD-9 finiteness guard shared by all float result producers (basic
    /// arithmetic and `**`). Traps E0813 when the result is NaN or Inf.
    ///
    /// v0.34.10a (SD-9): inside `ieee_float { }` the finiteness invariant is
    /// suspended — IEEE 754 NaN/Inf are legitimate there, so the value passes
    /// through untouched (`check_float` in the bytecode VM honors the same
    /// per-frame `ieee_depth`).
    ///
    /// full-audit 2026-08-05 §7 (HIGH): float `**` previously returned the
    /// raw `llvm.pow.f64` result and bypassed this guard entirely
    /// ((-1.0)**0.5 produced a silent NaN instead of E0813).
    pub(in crate::codegen) fn check_float_finite(
        &mut self,
        result: FloatValue<'ctx>,
        op_name_str: &str,
    ) -> Result<(), CompileError> {
        if self.ieee_depth > 0 {
            return Ok(());
        }
        let f64_ty = result.get_type();
        let function = self.current_function().ok_or_else(|| {
            CompileError::LlvmError("no current function for float finiteness check".into())
        })?;

        // SD-9: Check for NaN (unordered comparison with self) and Inf.
        // NaN: fcmp uno x, x → true only for NaN.
        let is_nan = self
            .builder
            .build_float_compare(inkwell::FloatPredicate::UNO, result, result, "is_nan")
            .map_err(|e| CompileError::LlvmError(format!("fcmp error: {}", e)))?;
        // Inf: |x| == Inf. Use LLVM intrinsic llvm.fabs.f64 then compare.
        let fabs_name = format!("llvm.fabs.f{}", f64_ty.get_bit_width());
        let fabs_fn = self.module.get_function(&fabs_name).unwrap_or_else(|| {
            self.module.add_function(
                &fabs_name,
                f64_ty.fn_type(&[BasicMetadataTypeEnum::FloatType(f64_ty)], false),
                Some(inkwell::module::Linkage::External),
            )
        });
        let abs_val = self
            .builder
            .build_call(
                fabs_fn,
                &[BasicMetadataValueEnum::FloatValue(result)],
                "fabs",
            )
            .map_err(|e| CompileError::LlvmError(format!("fabs call error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("fabs returned void".into()))?
            .into_float_value();
        let inf_const = f64_ty.const_float(f64::INFINITY);
        let is_inf = self
            .builder
            .build_float_compare(inkwell::FloatPredicate::OEQ, abs_val, inf_const, "is_inf")
            .map_err(|e| CompileError::LlvmError(format!("fcmp error: {}", e)))?;
        let not_finite = self
            .builder
            .build_or(is_nan, is_inf, "not_finite")
            .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?;

        // Branch: trap or continue.
        let trap_bb = self.context.append_basic_block(function, "trap_float");
        let ok_bb = self.context.append_basic_block(function, "float_ok");
        let fin_br = self
            .builder
            .build_conditional_branch(not_finite, trap_bb, ok_bb)
            .map_err(|e| CompileError::LlvmError(format!("br error: {}", e)))?;
        // 0.35.4 L2: trap 分支 cold 权重
        crate::codegen::float_chain::mark_cold_trap_branch(self.context, fin_br);

        // Trap block — or absorb into Fault in a fallible transition (v0.34.18a).
        self.builder.position_at_end(trap_bb);
        if self.in_fallible_multi_target() {
            self.emit_panic_fault_return("E0813")?;
        } else {
            let trap_fn = self.get_runtime_fn("mimi_trap_float_not_finite")?;
            let op_cstr = self
                .builder
                .build_global_string_ptr(op_name_str, "float_op_name")
                .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
            self.builder
                .build_call(
                    trap_fn,
                    &[BasicMetadataValueEnum::PointerValue(
                        op_cstr.as_pointer_value(),
                    )],
                    "",
                )
                .map_err(|e| CompileError::LlvmError(format!("call error: {}", e)))?;
            self.builder
                .build_unreachable()
                .map_err(|e| CompileError::LlvmError(format!("unreachable error: {}", e)))?;
        }

        // OK block.
        self.builder.position_at_end(ok_bb);
        Ok(())
    }

    /// Integer remainder (`%`).
    /// SD-8 (0.31.51a): delegates to compile_int_binop which handles
    /// zero-divisor and MIN/-1 traps.
    fn compile_mod_binop(
        &mut self,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match (lhs, rhs) {
            (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) => {
                self.compile_int_binop(BinOp::Mod, l, r)
            }
            _ => Err("mod requires integer types".into()),
        }
    }

    /// String concatenation (`+`).
    fn compile_string_binop(
        &self,
        l: PointerValue<'ctx>,
        r: PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let concat_fn = self.get_runtime_fn("mimi_str_concat")?;
        let raw_result = self
            .build_call(concat_fn, &[l.into(), r.into()], "str_concat")?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("mimi_str_concat returned void".to_string()))?;
        let raw_ptr = raw_result.into_pointer_value();
        // Register the heap allocation so it is freed at scope exit when the
        // result is used directly. `let` bindings transfer ownership by popping
        // this entry and registering the variable slot instead.
        self.register_heap_alloc(raw_ptr);
        self.wrap_c_string(raw_ptr)
    }

    /// Equality and inequality (`==`, `!=`).
    fn compile_equality_binop(
        &self,
        op: BinOp,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match (self.extract_string_ptr(&lhs), self.extract_string_ptr(&rhs)) {
            (Some(l), Some(r)) => self.compile_string_comparison_binop(op, l, r),
            _ => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) => {
                    let pred = match op {
                        BinOp::EqCmp => inkwell::IntPredicate::EQ,
                        BinOp::NeCmp => inkwell::IntPredicate::NE,
                        _ => return Err(format!("unsupported equality operator {:?}", op).into()),
                    };
                    Ok(self
                        .builder
                        .build_int_compare(pred, l, r, "eq")
                        .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                        .into())
                }
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) => {
                    let pred = match op {
                        BinOp::EqCmp => inkwell::FloatPredicate::OEQ,
                        BinOp::NeCmp => inkwell::FloatPredicate::ONE,
                        _ => return Err(format!("unsupported equality operator {:?}", op).into()),
                    };
                    Ok(self
                        .builder
                        .build_float_compare(pred, l, r, "feq")
                        .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                        .into())
                }
                _ => Err("eq requires same types".into()),
            },
        }
    }

    /// Ordered comparison (`<`, `>`, `<=`, `>=`).
    fn compile_comparison_binop(
        &self,
        op: BinOp,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match (self.extract_string_ptr(&lhs), self.extract_string_ptr(&rhs)) {
            (Some(l), Some(r)) => self.compile_string_comparison_binop(op, l, r),
            _ => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) => {
                    let pred = match op {
                        BinOp::Lt => inkwell::IntPredicate::SLT,
                        BinOp::Gt => inkwell::IntPredicate::SGT,
                        BinOp::Le => inkwell::IntPredicate::SLE,
                        BinOp::Ge => inkwell::IntPredicate::SGE,
                        _ => return Err(format!("unsupported comparison operator {:?}", op).into()),
                    };
                    Ok(self
                        .builder
                        .build_int_compare(pred, l, r, cmp_name(op))
                        .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                        .into())
                }
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) => {
                    let pred = match op {
                        BinOp::Lt => inkwell::FloatPredicate::OLT,
                        BinOp::Gt => inkwell::FloatPredicate::OGT,
                        BinOp::Le => inkwell::FloatPredicate::OLE,
                        BinOp::Ge => inkwell::FloatPredicate::OGE,
                        _ => return Err(format!("unsupported comparison operator {:?}", op).into()),
                    };
                    Ok(self
                        .builder
                        .build_float_compare(pred, l, r, fcmp_name(op))
                        .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                        .into())
                }
                _ => Err("lt requires same numeric types".into()),
            },
        }
    }

    /// String comparison using `strcmp`.
    fn compile_string_comparison_binop(
        &self,
        op: BinOp,
        l: PointerValue<'ctx>,
        r: PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let strcmp_fn = self.get_runtime_fn("strcmp")?;
        let result = self
            .build_call(strcmp_fn, &[l.into(), r.into()], "strcmp_call")?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("strcmp returned void".into()))?
            .into_int_value();
        let zero = self.context.i32_type().const_int(0, false);
        let pred = match op {
            BinOp::EqCmp => inkwell::IntPredicate::EQ,
            BinOp::NeCmp => inkwell::IntPredicate::NE,
            BinOp::Lt => inkwell::IntPredicate::SLT,
            BinOp::Gt => inkwell::IntPredicate::SGT,
            BinOp::Le => inkwell::IntPredicate::SLE,
            BinOp::Ge => inkwell::IntPredicate::SGE,
            _ => return Err(format!("unsupported string comparison operator {:?}", op).into()),
        };
        let name = match op {
            BinOp::EqCmp => "streq",
            BinOp::NeCmp => "strne",
            BinOp::Lt => "strlt",
            BinOp::Gt => "strgt",
            BinOp::Le => "strle",
            BinOp::Ge => "strge",
            _ => "strcmp",
        };
        Ok(self
            .builder
            .build_int_compare(pred, result, zero, name)
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
            .into())
    }

    /// Boolean logical operators (`&&`, `||`).
    fn compile_logical_binop(
        &self,
        op: BinOp,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match (lhs, rhs) {
            (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) => {
                let res = match op {
                    BinOp::And => self.builder.build_and(l, r, "and"),
                    BinOp::Or => self.builder.build_or(l, r, "or"),
                    _ => return Err(format!("unsupported logical operator {:?}", op).into()),
                };
                Ok(res
                    .map_err(|e| CompileError::LlvmError(format!("{} error: {}", op_name(op), e)))?
                    .into())
            }
            _ => {
                let msg = match op {
                    BinOp::And => "and requires boolean types",
                    BinOp::Or => "or requires boolean types",
                    _ => "logical operator requires boolean types",
                };
                Err(msg.into())
            }
        }
    }

    /// Range constructor (`..`).
    fn compile_range_binop(
        &self,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let start_iv = match lhs {
            BasicValueEnum::IntValue(iv) => iv,
            _ => return Err("range start must be i64".into()),
        };
        let end_iv = match rhs {
            BasicValueEnum::IntValue(iv) => iv,
            _ => return Err("range end must be i64".into()),
        };
        // Create a range struct { start: i64, end: i64 }
        let i64_ty = self.context.i64_type();
        let range_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(i64_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let alloca = self.build_alloca(range_ty, "range")?;
        let start_gep = self
            .gep()
            .build_struct_gep(range_ty, alloca, 0, "range_start")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(start_gep, start_iv)?;
        let end_gep = self
            .gep()
            .build_struct_gep(range_ty, alloca, 1, "range_end")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(end_gep, end_iv)?;
        Ok(alloca.into())
    }

    /// Power operator (`**`).
    fn compile_pow_binop(
        &mut self,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match (lhs, rhs) {
            (BasicValueEnum::IntValue(base), BasicValueEnum::IntValue(exp)) => {
                // Runtime pow function is i64 — extend i32 operands to i64 first.
                let i64_ty = self.context.i64_type();
                let base_i64 = if base.get_type().get_bit_width() < 64 {
                    self.builder
                        .build_int_s_extend(base, i64_ty, "pow_base_ext")
                        .map_err(|e| CompileError::LlvmError(format!("s_ext error: {}", e)))?
                } else {
                    base
                };
                let exp_i64 = if exp.get_type().get_bit_width() < 64 {
                    self.builder
                        .build_int_s_extend(exp, i64_ty, "pow_exp_ext")
                        .map_err(|e| CompileError::LlvmError(format!("s_ext error: {}", e)))?
                } else {
                    exp
                };
                let pow_fn_name = "__mimi_pow_i64";
                let fn_ty = i64_ty.fn_type(&[i64_ty.into(), i64_ty.into()], false);
                let pow_fn = self.module.get_function(pow_fn_name).unwrap_or_else(|| {
                    self.module.add_function(
                        pow_fn_name,
                        fn_ty,
                        Some(inkwell::module::Linkage::External),
                    )
                });
                let result = self
                    .build_call(pow_fn, &[base_i64.into(), exp_i64.into()], "pow_i64_call")?
                    .try_as_basic_value_opt()
                    .ok_or_else(|| CompileError::LlvmError("pow returned void".into()))?;
                // If the original operands were i32, truncate the result back.
                if base.get_type().get_bit_width() < 64 {
                    Ok(self
                        .builder
                        .build_int_truncate(result.into_int_value(), base.get_type(), "pow_trunc")
                        .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))?
                        .into())
                } else {
                    Ok(result)
                }
            }
            (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) => {
                // Central fix 2026-08-05: `llvm.pow.f64` is an LLVM intrinsic,
                // NOT a runtime symbol — get_runtime_fn() failed to resolve it
                // ("llvm.pow.f64 not declared"). Use libc `pow` exactly like
                // builtins/math.rs compile_pow does.
                let f64_ty = self.context.f64_type();
                let pow_fn = self.module.get_function("pow").unwrap_or_else(|| {
                    let ty = f64_ty.fn_type(
                        &[
                            BasicMetadataTypeEnum::FloatType(f64_ty),
                            BasicMetadataTypeEnum::FloatType(f64_ty),
                        ],
                        false,
                    );
                    self.module
                        .add_function("pow", ty, Some(inkwell::module::Linkage::External))
                });
                let result = self
                    .build_call(pow_fn, &[l.into(), r.into()], "pow_f64")?
                    .try_as_basic_value_opt()
                    .ok_or_else(|| CompileError::LlvmError("pow returned void".into()))?
                    .into_float_value();
                // full-audit 2026-08-05 §7 (HIGH) / SD-9: `**` must honor the
                // finiteness invariant like every other float result —
                // (-1.0)**0.5 is NaN and must trap E0813 outside
                // `ieee_float { }` (matches Op::PowFloat in the bytecode VM).
                self.check_float_finite(result, "power")?;
                Ok(result.into())
            }
            _ => Err("pow requires matching numeric types".into()),
        }
    }

    /// Bitwise operators (`&`, `|`, `^`, `<<`, `>>`).
    fn compile_bitwise_binop(
        &self,
        op: BinOp,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match (lhs, rhs) {
            (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) => {
                let res = match op {
                    BinOp::BitAnd => self.builder.build_and(l, r, "bitand"),
                    BinOp::BitOr => self.builder.build_or(l, r, "bitor"),
                    BinOp::BitXor => self.builder.build_xor(l, r, "bitxor"),
                    // Shl/Shr moved to compile_shift_binop (0.34.34):
                    // hardware-mask semantics + i32 width context handling.
                    _ => return Err(format!("unsupported bitwise operator {:?}", op).into()),
                };
                let name = match op {
                    BinOp::BitAnd => "and",
                    BinOp::BitOr => "or",
                    BinOp::BitXor => "xor",
                    _ => "bitwise",
                };
                Ok(res
                    .map_err(|e| CompileError::LlvmError(format!("{} error: {}", name, e)))?
                    .into())
            }
            _ => {
                let msg = match op {
                    BinOp::BitAnd => "bitand requires integer types",
                    BinOp::BitOr => "bitor requires integer types",
                    BinOp::BitXor => "bitxor requires integer types",
                    BinOp::Shl => "shl requires integer types",
                    BinOp::Shr => "shr requires integer types",
                    _ => "bitwise operator requires integer types",
                };
                Err(msg.into())
            }
        }
    }
}

/// Human-readable description of an LLVM basic type.
fn type_description(ty: &BasicTypeEnum<'_>) -> &'static str {
    match ty {
        BasicTypeEnum::IntType(_) => "int",
        BasicTypeEnum::FloatType(_) => "float",
        BasicTypeEnum::PointerType(_) => "pointer",
        BasicTypeEnum::ArrayType(_) => "array",
        BasicTypeEnum::StructType(_) => "struct",
        BasicTypeEnum::VectorType(_) => "vector",
        BasicTypeEnum::ScalableVectorType(_) => "scalable_vector",
    }
}

/// Short operator name used in LLVM instruction names / error messages.
fn op_name(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "add",
        BinOp::Sub => "sub",
        BinOp::Mul => "mul",
        BinOp::Div => "div",
        BinOp::And => "and",
        BinOp::Or => "or",
        _ => "op",
    }
}

/// LLVM instruction name for an integer comparison operator.
fn cmp_name(op: BinOp) -> &'static str {
    match op {
        BinOp::Lt => "lt",
        BinOp::Gt => "gt",
        BinOp::Le => "le",
        BinOp::Ge => "ge",
        _ => "cmp",
    }
}

/// LLVM instruction name for a floating-point comparison operator.
fn fcmp_name(op: BinOp) -> &'static str {
    match op {
        BinOp::Lt => "flt",
        BinOp::Gt => "fgt",
        BinOp::Le => "fle",
        BinOp::Ge => "fge",
        _ => "fcmp",
    }
}
