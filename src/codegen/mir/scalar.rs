//! Scalar constants and checked arithmetic for native MIR.

use super::*;

impl<'a, 'ctx> NativeMirFunctionEmitter<'a, 'ctx> {
    pub(super) fn emit_const(
        &mut self,
        result: &MirValueId,
        literal: &ResolvedLiteral,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let ty = self.value_type(result, subject)?;
        let desc = self
            .program
            .type_catalog()
            .get(&ty)
            .ok_or_else(|| NativeMirError::new(subject, "constant TypeDesc is absent"))?;
        match (desc.abi, literal) {
            (MirAbiClass::Integer { bits: 32 | 64, .. }, ResolvedLiteral::Int(value)) => {
                let int_ty = match desc.abi {
                    MirAbiClass::Integer { bits: 32, .. } => self.generator.context.i32_type(),
                    _ => self.generator.context.i64_type(),
                };
                Ok(int_ty.const_int(*value as u64, true).into())
            }
            (MirAbiClass::Bool, ResolvedLiteral::Bool(value)) => Ok(self
                .generator
                .context
                .bool_type()
                .const_int(u64::from(*value), false)
                .into()),
            (MirAbiClass::Float { bits: 32 | 64 }, ResolvedLiteral::FloatBits(value)) => {
                let value = if matches!(desc.abi, MirAbiClass::Float { bits: 32 }) {
                    f32::from_bits(*value as u32) as f64
                } else {
                    f64::from_bits(*value)
                };
                let float_ty = if matches!(desc.abi, MirAbiClass::Float { bits: 32 }) {
                    self.generator.context.f32_type()
                } else {
                    self.generator.context.f64_type()
                };
                Ok(float_ty.const_float(value).into())
            }
            (MirAbiClass::StringHandle, ResolvedLiteral::String(value)) => {
                self.emit_owned_string_literal(result, value, subject)
            }
            _ => Err(NativeMirError::new(
                subject,
                "literal is outside native scalar ABI",
            )),
        }
    }

    pub(super) fn emit_owned_string_literal(
        &mut self,
        result: &MirValueId,
        value: &str,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let i8_ty = self.generator.context.i8_type();
        let mut bytes = value
            .as_bytes()
            .iter()
            .map(|byte| i8_ty.const_int(u64::from(*byte), false))
            .collect::<Vec<_>>();
        bytes.push(i8_ty.const_zero());
        let array_ty = i8_ty.array_type(bytes.len() as u32);
        let global_name = format!(
            "__mimi_mir_string_{}_{}",
            native_symbol_fragment(&self.function.owner.0),
            native_symbol_fragment(result.as_str())
        );
        let global = self
            .generator
            .module
            .add_global(array_ty, None, &global_name);
        global.set_initializer(&i8_ty.const_array(&bytes));
        global.set_constant(true);
        global.set_alignment(1);
        self.emit_owned_string_from_parts(
            global.as_pointer_value(),
            self.generator
                .context
                .i64_type()
                .const_int(value.len() as u64, false),
            subject,
        )
    }

    pub(super) fn emit_unary(
        &mut self,
        result: &MirValueId,
        op: ResolvedUnaryOp,
        operand: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let operand_ty = self.value_type(operand, subject)?;
        let result_ty = self.value_type(result, subject)?;
        let descriptor = self
            .program
            .type_catalog()
            .get(&operand_ty)
            .ok_or_else(|| NativeMirError::new(subject, "unary operand TypeDesc is absent"))?;
        if matches!(descriptor.abi, MirAbiClass::Float { bits: 32 | 64 }) {
            self.program
                .type_catalog()
                .validate_copy_float_unary(&result_ty, &operand_ty, op)
                .map_err(|message| NativeMirError::new(subject, message))?;
            let value = self.value(operand, subject)?.into_float_value();
            return self
                .generator
                .builder
                .build_float_neg(value, "mir_fneg")
                .map(BasicValueEnum::from)
                .map_err(|error| NativeMirError::new(subject, error.to_string()));
        }
        let value = self.value(operand, subject)?.into_int_value();
        match op {
            ResolvedUnaryOp::Not => self
                .generator
                .builder
                .build_not(value, "mir_not")
                .map(BasicValueEnum::from)
                .map_err(|error| NativeMirError::new(subject, error.to_string())),
            ResolvedUnaryOp::Negate => {
                let int_ty = value.get_type();
                let min = int_ty.const_int(1u64 << (int_ty.get_bit_width() - 1), false);
                let is_min = self
                    .generator
                    .builder
                    .build_int_compare(IntPredicate::EQ, value, min, "mir_abs_min")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                let function = self.llvm_function;
                let trap = self
                    .generator
                    .context
                    .append_basic_block(function, "mir_neg_overflow");
                let ok = self
                    .generator
                    .context
                    .append_basic_block(function, "mir_neg_ok");
                self.generator
                    .builder
                    .build_conditional_branch(is_min, trap, ok)
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                self.emit_overflow_trap(trap, "neg", subject)?;
                self.generator.builder.position_at_end(ok);
                self.generator
                    .builder
                    .build_int_sub(int_ty.const_zero(), value, "mir_neg")
                    .map(BasicValueEnum::from)
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))
            }
            _ => Err(NativeMirError::new(
                subject,
                format!("unary operator {op:?} is not emitted"),
            )),
        }
        .map(|value| {
            let _ = result;
            value
        })
    }

    pub(super) fn emit_binary(
        &mut self,
        result: &MirValueId,
        op: ResolvedBinaryOp,
        left: &MirValueId,
        right: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let result_ty = self.value_type(result, subject)?;
        let left_ty = self.value_type(left, subject)?;
        let right_ty = self.value_type(right, subject)?;
        let left_desc = self
            .program
            .type_catalog()
            .get(&left_ty)
            .ok_or_else(|| NativeMirError::new(subject, "binary left TypeDesc is absent"))?;
        if matches!(left_desc.abi, MirAbiClass::Float { bits: 32 | 64 }) {
            self.program
                .type_catalog()
                .validate_copy_float_binary(&result_ty, &left_ty, &right_ty, op)
                .map_err(|message| NativeMirError::new(subject, message))?;
            let left_value = self.value(left, subject)?.into_float_value();
            let right_value = self.value(right, subject)?.into_float_value();
            let result = self
                .generator
                .builder
                .build_float_add(left_value, right_value, "mir_fadd")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            self.emit_float_finite_guard(result, "add", subject)?;
            return Ok(result.into());
        }
        let left_value = self.value(left, subject)?.into_int_value();
        let right_value = self.value(right, subject)?.into_int_value();
        match op {
            ResolvedBinaryOp::Add => self
                .emit_checked_add_sub(left_value, right_value, false, subject)
                .map(BasicValueEnum::from),
            ResolvedBinaryOp::Subtract => self
                .emit_checked_add_sub(left_value, right_value, true, subject)
                .map(BasicValueEnum::from),
            ResolvedBinaryOp::Equal => {
                self.compare(IntPredicate::EQ, left_value, right_value, subject)
            }
            ResolvedBinaryOp::NotEqual => {
                self.compare(IntPredicate::NE, left_value, right_value, subject)
            }
            ResolvedBinaryOp::Less => {
                self.compare(IntPredicate::SLT, left_value, right_value, subject)
            }
            ResolvedBinaryOp::Greater => {
                self.compare(IntPredicate::SGT, left_value, right_value, subject)
            }
            ResolvedBinaryOp::LessEqual => {
                self.compare(IntPredicate::SLE, left_value, right_value, subject)
            }
            ResolvedBinaryOp::GreaterEqual => {
                self.compare(IntPredicate::SGE, left_value, right_value, subject)
            }
            ResolvedBinaryOp::LogicalAnd => self
                .generator
                .builder
                .build_and(left_value, right_value, "mir_and")
                .map(BasicValueEnum::from)
                .map_err(|error| NativeMirError::new(subject, error.to_string())),
            ResolvedBinaryOp::LogicalOr => self
                .generator
                .builder
                .build_or(left_value, right_value, "mir_or")
                .map(BasicValueEnum::from)
                .map_err(|error| NativeMirError::new(subject, error.to_string())),
            _ => Err(NativeMirError::new(
                subject,
                format!("binary operator {op:?} is not emitted"),
            )),
        }
        .map(|value| {
            let _ = result;
            value
        })
    }

    fn emit_float_finite_guard(
        &mut self,
        value: inkwell::values::FloatValue<'ctx>,
        operation: &str,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let float_ty = value.get_type();
        let is_nan = self
            .generator
            .builder
            .build_float_compare(inkwell::FloatPredicate::UNO, value, value, "mir_is_nan")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let pos_inf = float_ty.const_float(f64::INFINITY);
        let neg_inf = float_ty.const_float(f64::NEG_INFINITY);
        let is_pos_inf = self
            .generator
            .builder
            .build_float_compare(
                inkwell::FloatPredicate::OEQ,
                value,
                pos_inf,
                "mir_is_pos_inf",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let is_neg_inf = self
            .generator
            .builder
            .build_float_compare(
                inkwell::FloatPredicate::OEQ,
                value,
                neg_inf,
                "mir_is_neg_inf",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let is_inf = self
            .generator
            .builder
            .build_or(is_pos_inf, is_neg_inf, "mir_is_inf")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let not_finite = self
            .generator
            .builder
            .build_or(is_nan, is_inf, "mir_not_finite")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let function = self.llvm_function;
        let trap = self
            .generator
            .context
            .append_basic_block(function, "mir_float_not_finite");
        let ok = self
            .generator
            .context
            .append_basic_block(function, "mir_float_finite");
        self.generator
            .builder
            .build_conditional_branch(not_finite, trap, ok)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(trap);
        let trap_fn = self
            .generator
            .get_runtime_fn("mimi_trap_float_not_finite")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let operation = self
            .generator
            .builder
            .build_global_string_ptr(operation, "mir_float_operation")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(
                trap_fn,
                &[inkwell::values::BasicMetadataValueEnum::from(
                    operation.as_pointer_value(),
                )],
                "mir_float_not_finite_trap",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_unreachable()
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(ok);
        Ok(())
    }

    pub(super) fn compare(
        &self,
        predicate: IntPredicate,
        left: inkwell::values::IntValue<'ctx>,
        right: inkwell::values::IntValue<'ctx>,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        self.generator
            .builder
            .build_int_compare(predicate, left, right, "mir_cmp")
            .map(BasicValueEnum::from)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))
    }

    pub(super) fn emit_checked_add_sub(
        &mut self,
        left: inkwell::values::IntValue<'ctx>,
        right: inkwell::values::IntValue<'ctx>,
        subtract: bool,
        subject: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, NativeMirError> {
        let result = if subtract {
            self.generator.builder.build_int_sub(left, right, "mir_sub")
        } else {
            self.generator.builder.build_int_add(left, right, "mir_add")
        }
        .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let zero = left.get_type().const_zero();
        let left_nonnegative = self
            .generator
            .builder
            .build_int_compare(IntPredicate::SGE, left, zero, "mir_left_nonnegative")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let right_nonnegative = self
            .generator
            .builder
            .build_int_compare(IntPredicate::SGE, right, zero, "mir_right_nonnegative")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let result_nonnegative = self
            .generator
            .builder
            .build_int_compare(IntPredicate::SGE, result, zero, "mir_result_nonnegative")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let overflow = if subtract {
            let left_pos_right_neg = self
                .generator
                .builder
                .build_and(
                    left_nonnegative,
                    self.generator
                        .builder
                        .build_not(right_nonnegative, "mir_not_right_nonnegative")
                        .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
                    "mir_sub_case_a",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let left_neg_right_pos = self
                .generator
                .builder
                .build_and(
                    self.generator
                        .builder
                        .build_not(left_nonnegative, "mir_not_left_nonnegative")
                        .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
                    right_nonnegative,
                    "mir_sub_case_b",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let result_neg = self
                .generator
                .builder
                .build_not(result_nonnegative, "mir_not_result_nonnegative")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let result_pos = result_nonnegative;
            self.generator.builder.build_or(
                self.generator
                    .builder
                    .build_and(left_pos_right_neg, result_neg, "mir_sub_overflow_a")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
                self.generator
                    .builder
                    .build_and(left_neg_right_pos, result_pos, "mir_sub_overflow_b")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
                "mir_sub_overflow",
            )
        } else {
            let same_nonnegative = self
                .generator
                .builder
                .build_and(left_nonnegative, right_nonnegative, "mir_add_case_a")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let left_negative = self
                .generator
                .builder
                .build_not(left_nonnegative, "mir_not_left_nonnegative_add")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let right_negative = self
                .generator
                .builder
                .build_not(right_nonnegative, "mir_not_right_nonnegative_add")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let same_negative = self
                .generator
                .builder
                .build_and(left_negative, right_negative, "mir_add_case_b")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let result_negative = self
                .generator
                .builder
                .build_not(result_nonnegative, "mir_not_result_nonnegative_add")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            self.generator.builder.build_or(
                self.generator
                    .builder
                    .build_and(same_nonnegative, result_negative, "mir_add_overflow_a")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
                self.generator
                    .builder
                    .build_and(same_negative, result_nonnegative, "mir_add_overflow_b")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
                "mir_add_overflow",
            )
        }
        .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let function = self.llvm_function;
        let trap = self
            .generator
            .context
            .append_basic_block(function, "mir_arithmetic_overflow");
        let ok = self
            .generator
            .context
            .append_basic_block(function, "mir_arithmetic_ok");
        self.generator
            .builder
            .build_conditional_branch(overflow, trap, ok)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_overflow_trap(trap, if subtract { "sub" } else { "add" }, subject)?;
        self.generator.builder.position_at_end(ok);
        Ok(result)
    }
}
