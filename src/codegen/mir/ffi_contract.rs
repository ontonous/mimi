//! Runtime FFI predicates consumed directly from canonical MIR predicates.

use super::*;
use crate::core::mir::{MirContractBinaryOp as Op, MirContractExpr as Expr, MirContractUnaryOp};
use inkwell::values::IntValue;

impl<'a, 'ctx> NativeMirFunctionEmitter<'a, 'ctx> {
    pub(super) fn emit_ffi_requires(
        &mut self,
        condition: &Expr,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let condition = self.emit_ffi_predicate(condition, subject, "precondition")?;
        self.emit_ffi_guard(condition, "[E0808] FFI precondition failed", subject)
    }

    pub(super) fn emit_ffi_ensures(
        &mut self,
        condition: &Expr,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let condition = self.emit_ffi_predicate(condition, subject, "postcondition")?;
        self.emit_ffi_guard(condition, "[E0808] FFI postcondition failed", subject)
    }

    fn emit_ffi_guard(
        &mut self,
        condition: IntValue<'ctx>,
        message: &str,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let ok = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "ffi_requires_ok");
        let trap = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "ffi_requires_trap");
        self.generator
            .builder
            .build_conditional_branch(condition, ok, trap)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(trap);
        self.emit_abort_with_message(message, subject)?;
        self.generator.builder.position_at_end(ok);
        Ok(())
    }

    fn emit_ffi_predicate(
        &mut self,
        expression: &Expr,
        subject: &str,
        phase: &str,
    ) -> Result<IntValue<'ctx>, NativeMirError> {
        let error =
            |error: inkwell::builder::BuilderError| NativeMirError::new(subject, error.to_string());
        let i64_ty = self.generator.context.i64_type();
        match expression {
            Expr::Int(value) => Ok(i64_ty.const_int(*value as u64, true)),
            Expr::Bool(value) => Ok(self
                .generator
                .context
                .bool_type()
                .const_int(u64::from(*value), false)),
            Expr::Value(id) => {
                let value = self.value(id, subject)?.into_int_value();
                if value.get_type().get_bit_width() == 32 {
                    self.generator
                        .builder
                        .build_int_s_extend(value, i64_ty, "ffi_requires_i64")
                        .map_err(error)
                } else {
                    Ok(value)
                }
            }
            Expr::Unary { op, operand } => {
                let operand = self.emit_ffi_predicate(operand, subject, phase)?;
                match op {
                    MirContractUnaryOp::Not => self
                        .generator
                        .builder
                        .build_not(operand, "ffi_requires_not")
                        .map_err(error),
                    MirContractUnaryOp::Negate => {
                        let valid = self
                            .generator
                            .builder
                            .build_int_compare(
                                IntPredicate::NE,
                                operand,
                                i64_ty.const_int(i64::MIN as u64, true),
                                "ffi_neg_defined",
                            )
                            .map_err(error)?;
                        self.emit_ffi_guard(
                            valid,
                            &format!("[E0802] integer overflow in FFI {phase}"),
                            subject,
                        )?;
                        self.generator
                            .builder
                            .build_int_neg(operand, "ffi_requires_neg")
                            .map_err(error)
                    }
                }
            }
            Expr::Binary {
                op: Op::LogicalAnd | Op::LogicalOr,
                left,
                right,
            } => {
                let left = self.emit_ffi_predicate(left, subject, phase)?;
                let predecessor =
                    self.generator.builder.get_insert_block().ok_or_else(|| {
                        NativeMirError::new(subject, "FFI predicate has no block")
                    })?;
                let rhs = self
                    .generator
                    .context
                    .append_basic_block(self.llvm_function, "ffi_requires_rhs");
                let merge = self
                    .generator
                    .context
                    .append_basic_block(self.llvm_function, "ffi_requires_merge");
                let conjunction = matches!(
                    expression,
                    Expr::Binary {
                        op: Op::LogicalAnd,
                        ..
                    }
                );
                let (yes, no) = if conjunction {
                    (rhs, merge)
                } else {
                    (merge, rhs)
                };
                self.generator
                    .builder
                    .build_conditional_branch(left, yes, no)
                    .map_err(error)?;
                self.generator.builder.position_at_end(rhs);
                let right = self.emit_ffi_predicate(right, subject, phase)?;
                let rhs_end = self.generator.builder.get_insert_block().ok_or_else(|| {
                    NativeMirError::new(subject, "FFI predicate RHS has no block")
                })?;
                self.generator
                    .builder
                    .build_unconditional_branch(merge)
                    .map_err(error)?;
                self.generator.builder.position_at_end(merge);
                let phi = self
                    .generator
                    .builder
                    .build_phi(self.generator.context.bool_type(), "ffi_requires_bool")
                    .map_err(error)?;
                phi.add_incoming(&[(&left, predecessor), (&right, rhs_end)]);
                Ok(phi.as_basic_value().into_int_value())
            }
            Expr::Binary { op, left, right } => {
                let left = self.emit_ffi_predicate(left, subject, phase)?;
                let right = self.emit_ffi_predicate(right, subject, phase)?;
                let predicate = match op {
                    Op::Equal => Some(IntPredicate::EQ),
                    Op::NotEqual => Some(IntPredicate::NE),
                    Op::Less => Some(IntPredicate::SLT),
                    Op::LessEqual => Some(IntPredicate::SLE),
                    Op::Greater => Some(IntPredicate::SGT),
                    Op::GreaterEqual => Some(IntPredicate::SGE),
                    _ => None,
                };
                if let Some(predicate) = predicate {
                    return self
                        .generator
                        .builder
                        .build_int_compare(predicate, left, right, "ffi_requires_cmp")
                        .map_err(error);
                }
                if matches!(op, Op::Divide | Op::Remainder) {
                    let nonzero = self
                        .generator
                        .builder
                        .build_int_compare(
                            IntPredicate::NE,
                            right,
                            i64_ty.const_zero(),
                            "ffi_div_nonzero",
                        )
                        .map_err(error)?;
                    self.emit_ffi_guard(
                        nonzero,
                        &format!("[E0801] division by zero in FFI {phase}"),
                        subject,
                    )?;
                    let is_min = self
                        .generator
                        .builder
                        .build_int_compare(
                            IntPredicate::EQ,
                            left,
                            i64_ty.const_int(i64::MIN as u64, true),
                            "ffi_div_min",
                        )
                        .map_err(error)?;
                    let is_minus_one = self
                        .generator
                        .builder
                        .build_int_compare(
                            IntPredicate::EQ,
                            right,
                            i64_ty.const_int(u64::MAX, true),
                            "ffi_div_minus_one",
                        )
                        .map_err(error)?;
                    let overflow = self
                        .generator
                        .builder
                        .build_and(is_min, is_minus_one, "ffi_div_overflow")
                        .map_err(error)?;
                    let valid = self
                        .generator
                        .builder
                        .build_not(overflow, "ffi_div_defined")
                        .map_err(error)?;
                    self.emit_ffi_guard(
                        valid,
                        &format!("[E0802] integer overflow in FFI {phase}"),
                        subject,
                    )?;
                    return if *op == Op::Divide {
                        self.generator
                            .builder
                            .build_int_signed_div(left, right, "ffi_requires_div")
                    } else {
                        self.generator
                            .builder
                            .build_int_signed_rem(left, right, "ffi_requires_rem")
                    }
                    .map_err(error);
                }
                // i64 addition/subtraction/product fit exactly in i128. The
                // round-trip guard rejects overflow before narrowing to i64.
                let wide_ty = self.generator.context.i128_type();
                let wide_left = self
                    .generator
                    .builder
                    .build_int_s_extend(left, wide_ty, "ffi_wide_left")
                    .map_err(error)?;
                let wide_right = self
                    .generator
                    .builder
                    .build_int_s_extend(right, wide_ty, "ffi_wide_right")
                    .map_err(error)?;
                let wide = match op {
                    Op::Add => {
                        self.generator
                            .builder
                            .build_int_add(wide_left, wide_right, "ffi_wide_add")
                    }
                    Op::Subtract => {
                        self.generator
                            .builder
                            .build_int_sub(wide_left, wide_right, "ffi_wide_sub")
                    }
                    Op::Multiply => {
                        self.generator
                            .builder
                            .build_int_mul(wide_left, wide_right, "ffi_wide_mul")
                    }
                    _ => {
                        return Err(NativeMirError::new(
                            subject,
                            "unsupported FFI predicate operator",
                        ))
                    }
                }
                .map_err(error)?;
                let value = self
                    .generator
                    .builder
                    .build_int_truncate(wide, i64_ty, "ffi_requires_arithmetic")
                    .map_err(error)?;
                let extended = self
                    .generator
                    .builder
                    .build_int_s_extend(value, wide_ty, "ffi_arithmetic_roundtrip")
                    .map_err(error)?;
                let valid = self
                    .generator
                    .builder
                    .build_int_compare(IntPredicate::EQ, wide, extended, "ffi_arithmetic_defined")
                    .map_err(error)?;
                self.emit_ffi_guard(
                    valid,
                    &format!("[E0802] integer overflow in FFI {phase}"),
                    subject,
                )?;
                Ok(value)
            }
            Expr::Result | Expr::Old(_) | Expr::Project { .. } => Err(NativeMirError::new(
                subject,
                "unsupported FFI predicate expression",
            )),
        }
    }
}
