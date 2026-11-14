//! Scalar constants and checked arithmetic for native MIR.

use super::*;
use crate::codegen::CallSiteValueExt;

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
                // ResolvedLiteral::FloatBits is the shared semantic f64 slot,
                // including when the MIR value's physical ABI is f32. The
                // f32 consumer must round the decoded f64 value through Rust
                // f32, just like reference and bytecode, rather than treating
                // the low 32 bits of the f64 bit pattern as an f32 payload.
                let value = if matches!(desc.abi, MirAbiClass::Float { bits: 32 }) {
                    f64::from_bits(*value) as f32 as f64
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
        let array_len = self.u32_abi(bytes.len(), "String literal byte length", subject)?;
        let array_ty = i8_ty.array_type(array_len);
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
            self.generator.context.i64_type().const_int(
                u64::try_from(value.len()).map_err(|_| {
                    NativeMirError::new(
                        subject,
                        "String literal length exceeds the native u64 representation",
                    )
                })?,
                false,
            ),
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
        .inspect(|_| {
            let _ = result;
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
        let managed_any_ty = crate::core::mir::types::managed_any_i32_string_v1_type_id();
        let string_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        let bool_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Bool);
        if op == ResolvedBinaryOp::Add
            && result_ty == string_ty
            && left_ty == string_ty
            && right_ty == string_ty
        {
            return self.emit_owned_string_concat(left, right, &string_ty, subject);
        }
        if matches!(op, ResolvedBinaryOp::Equal | ResolvedBinaryOp::NotEqual)
            && result_ty == bool_ty
            && ((left_ty == managed_any_ty && right_ty == string_ty)
                || (right_ty == managed_any_ty && left_ty == string_ty))
        {
            let (owner, string) = if left_ty == managed_any_ty {
                (left, right)
            } else {
                (right, left)
            };
            return self.emit_managed_any_string_comparison(
                op,
                owner,
                string,
                &managed_any_ty,
                &string_ty,
                subject,
            );
        }
        if matches!(op, ResolvedBinaryOp::Equal | ResolvedBinaryOp::NotEqual)
            && result_ty == bool_ty
            && left_ty == string_ty
            && right_ty == string_ty
        {
            self.program
                .type_catalog()
                .validate_owned_string(&string_ty)
                .map_err(|message| NativeMirError::new(subject, message))?;
            let left_value = self.value(left, subject)?.into_struct_value();
            let right_value = self.value(right, subject)?.into_struct_value();
            let equal = self.emit_string_equality(left_value, right_value, subject)?;
            let compared = if op == ResolvedBinaryOp::Equal {
                equal
            } else {
                self.generator
                    .builder
                    .build_not(equal, "mir_string_not_equal")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            };
            return Ok(compared.into());
        }
        if matches!(left_desc.abi, MirAbiClass::Float { bits: 32 | 64 }) {
            self.program
                .type_catalog()
                .validate_copy_float_binary(&result_ty, &left_ty, &right_ty, op)
                .map_err(|message| NativeMirError::new(subject, message))?;
            let left_value = self.value(left, subject)?.into_float_value();
            let right_value = self.value(right, subject)?.into_float_value();
            // R6-1068: comparisons emit the plain IEEE ordered fcmp the AST
            // native emitter produces (OLT/OGT/OLE/OGE and OEQ for `==`;
            // `!=` is emitted as UNE — the exact negation of OEQ — matching
            // the bytecode VM's generic Ne and the verifier's eq_fpa.not()
            // model; the AST native's ONE differs only on NaN, which
            // default-mode programs cannot materialize).  No finiteness
            // guard: the AST backends trap nothing on comparisons either.
            let predicate = match op {
                ResolvedBinaryOp::Equal => Some(inkwell::FloatPredicate::OEQ),
                ResolvedBinaryOp::NotEqual => Some(inkwell::FloatPredicate::UNE),
                ResolvedBinaryOp::Less => Some(inkwell::FloatPredicate::OLT),
                ResolvedBinaryOp::Greater => Some(inkwell::FloatPredicate::OGT),
                ResolvedBinaryOp::LessEqual => Some(inkwell::FloatPredicate::OLE),
                ResolvedBinaryOp::GreaterEqual => Some(inkwell::FloatPredicate::OGE),
                _ => None,
            };
            if let Some(predicate) = predicate {
                let compared = self
                    .generator
                    .builder
                    .build_float_compare(predicate, left_value, right_value, "mir_fcmp")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                return Ok(compared.into());
            }
            // R6-1067: the zero divisor is the language-level E0801
            // division-definedness violation (small-step §3, SD-8 family) —
            // the same `mimi_trap_float_div_by_zero` face the AST emitter
            // and bytecode VM report.  Overflow stays IEEE-defined (±inf)
            // and is owned by the shared finiteness guard below.
            if op == ResolvedBinaryOp::Divide {
                self.emit_float_div_zero_guard(right_value, subject)?;
            }
            let operation = match op {
                ResolvedBinaryOp::Add => "add",
                ResolvedBinaryOp::Subtract => "subtract",
                ResolvedBinaryOp::Multiply => "mul",
                ResolvedBinaryOp::Divide => "div",
                _ => {
                    return Err(NativeMirError::new(
                        subject,
                        format!("float binary operator {op:?} is outside the finite-only contract"),
                    ));
                }
            };
            let result = match op {
                ResolvedBinaryOp::Add => {
                    self.generator
                        .builder
                        .build_float_add(left_value, right_value, "mir_fadd")
                }
                ResolvedBinaryOp::Subtract => {
                    self.generator
                        .builder
                        .build_float_sub(left_value, right_value, "mir_fsub")
                }
                ResolvedBinaryOp::Multiply => {
                    self.generator
                        .builder
                        .build_float_mul(left_value, right_value, "mir_fmul")
                }
                ResolvedBinaryOp::Divide => {
                    self.generator
                        .builder
                        .build_float_div(left_value, right_value, "mir_fdiv")
                }
                _ => {
                    return Err(NativeMirError::new(
                        subject,
                        format!("float binary operator {op:?} is outside the finite-only contract"),
                    ));
                }
            }
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            self.emit_float_finite_guard(result, operation, subject)?;
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
            ResolvedBinaryOp::Multiply => self
                .emit_checked_mul(left_value, right_value, subject)
                .map(BasicValueEnum::from),
            ResolvedBinaryOp::Divide => self
                .emit_checked_div_rem(left_value, right_value, true, subject)
                .map(BasicValueEnum::from),
            ResolvedBinaryOp::Remainder => self
                .emit_checked_div_rem(left_value, right_value, false, subject)
                .map(BasicValueEnum::from),
            ResolvedBinaryOp::Power => {
                if left_ty != right_ty || result_ty != left_ty {
                    return Err(NativeMirError::new(
                        subject,
                        "checked Power requires identical base, exponent, and result TypeDesc identities",
                    ));
                }
                let operand_ty = left_value.get_type();
                let operand_width = operand_ty.get_bit_width();
                if !matches!(operand_width, 32 | 64) {
                    return Err(NativeMirError::new(
                        subject,
                        "checked Power requires a signed i32 or i64 TypeDesc",
                    ));
                }
                let i64_ty = self.generator.context.i64_type();
                let base_i64 = if operand_width == 32 {
                    self.generator
                        .builder
                        .build_int_s_extend(left_value, i64_ty, "mir_pow_base_ext")
                        .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                } else {
                    left_value
                };
                let exponent_i64 = if operand_width == 32 {
                    self.generator
                        .builder
                        .build_int_s_extend(right_value, i64_ty, "mir_pow_exponent_ext")
                        .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                } else {
                    right_value
                };
                let checked = if super::actor_status::is_actor_tagged(
                    self.program,
                    &self.function.owner,
                ) {
                    // Actor-tagged workers must report trap codes through the
                    // {status,value} ABI so the root can drain and join the
                    // domain before reporting failure. The ordinary helper
                    // exits the process and is therefore unsafe on this path.
                    let i32_ty = self.generator.context.i32_type();
                    let out_pointer_ty =
                        self.generator.context.ptr_type(inkwell::AddressSpace::default());
                    let status_helper = self
                        .generator
                        .module
                        .get_function("__mimi_pow_i64_status")
                        .unwrap_or_else(|| {
                            self.generator.module.add_function(
                                "__mimi_pow_i64_status",
                                i32_ty.fn_type(
                                    &[i64_ty.into(), i64_ty.into(), out_pointer_ty.into()],
                                    false,
                                ),
                                Some(inkwell::module::Linkage::External),
                            )
                        });
                    let out_slot = self
                        .generator
                        .builder
                        .build_alloca(i64_ty, "mir_pow_status_out")
                        .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                    let status = self
                        .generator
                        .builder
                        .build_call(
                            status_helper,
                            &[base_i64.into(), exponent_i64.into(), out_slot.into()],
                            "mir_pow_status",
                        )
                        .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                        .try_as_basic_value_opt()
                        .ok_or_else(|| {
                            NativeMirError::new(subject, "checked Power status helper returned void")
                        })?
                        .into_int_value();
                    self.emit_actor_status_guard(status, subject)?;
                    self.generator
                        .builder
                        .build_load(i64_ty, out_slot, "mir_pow_checked_i64")
                        .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                        .into_int_value()
                } else {
                    let helper = self
                        .generator
                        .module
                        .get_function("__mimi_pow_i64")
                        .unwrap_or_else(|| {
                            self.generator.module.add_function(
                                "__mimi_pow_i64",
                                i64_ty.fn_type(&[i64_ty.into(), i64_ty.into()], false),
                                Some(inkwell::module::Linkage::External),
                            )
                        });
                    self.generator
                        .builder
                        .build_call(
                            helper,
                            &[base_i64.into(), exponent_i64.into()],
                            "mir_pow_checked_i64",
                        )
                        .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                        .try_as_basic_value_opt()
                        .ok_or_else(|| {
                            NativeMirError::new(subject, "checked Power runtime helper returned void")
                        })?
                        .into_int_value()
                };
                if operand_width == 32 {
                    self.generator
                        .builder
                        .build_int_truncate(checked, operand_ty, "mir_pow_wrap_i32")
                        .map(BasicValueEnum::from)
                        .map_err(|error| NativeMirError::new(subject, error.to_string()))
                } else {
                    Ok(checked.into())
                }
            }
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
        .inspect(|_| {
            let _ = result;
        })
    }

    fn emit_owned_string_concat(
        &mut self,
        left: &MirValueId,
        right: &MirValueId,
        string_ty: &crate::core::ResolvedTypeId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        self.program
            .type_catalog()
            .validate_owned_string(string_ty)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let left_value = self.value(left, subject)?.into_struct_value();
        let right_value = self.value(right, subject)?.into_struct_value();
        let left_data = self
            .generator
            .builder
            .build_extract_value(left_value, 0, "mir_string_concat_left_data")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_pointer_value();
        let left_len = self
            .generator
            .builder
            .build_extract_value(left_value, 1, "mir_string_concat_left_len")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let right_data = self
            .generator
            .builder
            .build_extract_value(right_value, 0, "mir_string_concat_right_data")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_pointer_value();
        let right_len = self
            .generator
            .builder
            .build_extract_value(right_value, 1, "mir_string_concat_right_len")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let concat = self
            .generator
            .get_runtime_fn("mimi_str_concat_ll")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let data = self
            .generator
            .builder
            .build_call(
                concat,
                &[
                    BasicMetadataValueEnum::from(left_data),
                    BasicMetadataValueEnum::from(left_len),
                    BasicMetadataValueEnum::from(right_data),
                    BasicMetadataValueEnum::from(right_len),
                ],
                "mir_string_concat_data",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .try_as_basic_value_opt()
            .ok_or_else(|| NativeMirError::new(subject, "String concatenation returned void"))?
            .into_pointer_value();
        let free = self
            .generator
            .get_runtime_fn("mimi_string_free")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        let failed = self
            .generator
            .builder
            .build_is_null(data, "mir_string_concat_failed")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let failure = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_string_concat_failure");
        let success = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_string_concat_success");
        self.generator
            .builder
            .build_conditional_branch(failed, failure, success)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(failure);
        self.generator
            .builder
            .build_call(
                free,
                &[BasicMetadataValueEnum::from(left_data)],
                "mir_string_concat_drop_left_on_failure",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        if left != right {
            self.generator
                .builder
                .build_call(
                    free,
                    &[BasicMetadataValueEnum::from(right_data)],
                    "mir_string_concat_drop_right_on_failure",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        }
        self.emit_abort_with_message("[E0800] String concatenation allocation failed", subject)?;
        self.generator.builder.position_at_end(success);

        let length = self
            .generator
            .builder
            .build_int_add(left_len, right_len, "mir_string_concat_length")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_ordinary_map_domain_own_string(data, length, subject)?;

        // The second checkpoint is after the completed allocation has been
        // registered with the execution domain, but before the String value
        // becomes visible to later MIR instructions. If publication fails,
        // retire the unexposed result and both consumed operands first. The
        // free hook also removes any domain receipt, so the subsequent trap
        // cleanup cannot release the same allocation again.
        let checkpoint_name = "mimi_mir_string_concat_checkpoint";
        let checkpoint = self
            .generator
            .module
            .get_function(checkpoint_name)
            .unwrap_or_else(|| {
                let i32_ty = self.generator.context.i32_type();
                self.generator.module.add_function(
                    checkpoint_name,
                    i32_ty.fn_type(&[i32_ty.into()], false),
                    Some(inkwell::module::Linkage::External),
                )
            });
        let checkpoint_status = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    checkpoint,
                    &[self.generator.context.i32_type().const_int(1, false).into()],
                    "mir_string_concat_publication_status",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| {
            NativeMirError::new(subject, "String concatenation checkpoint returned void")
        })?
        .into_int_value();
        let publication_failed = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                checkpoint_status,
                self.generator.context.i32_type().const_zero(),
                "mir_string_concat_publication_failed",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let publication_failure = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_string_concat_publication_failure");
        let publication_success = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_string_concat_publication_success");
        self.generator
            .builder
            .build_conditional_branch(publication_failed, publication_failure, publication_success)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(publication_failure);
        self.generator
            .builder
            .build_call(
                free,
                &[BasicMetadataValueEnum::from(data)],
                "mir_string_concat_drop_result_on_publication_failure",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(
                free,
                &[BasicMetadataValueEnum::from(left_data)],
                "mir_string_concat_drop_left_on_publication_failure",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        if left != right {
            self.generator
                .builder
                .build_call(
                    free,
                    &[BasicMetadataValueEnum::from(right_data)],
                    "mir_string_concat_drop_right_on_publication_failure",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        }
        self.emit_abort_with_message("[E0800] String concatenation publication failed", subject)?;
        self.generator.builder.position_at_end(publication_success);

        let string_llvm_type = self.generator.context.struct_type(
            &[
                self.generator
                    .context
                    .ptr_type(inkwell::AddressSpace::default())
                    .into(),
                self.generator.context.i64_type().into(),
            ],
            false,
        );
        let string = self
            .generator
            .builder
            .build_insert_value(
                string_llvm_type.get_undef(),
                data,
                0,
                "mir_string_concat_result_data",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_struct_value();
        let string = self
            .generator
            .builder
            .build_insert_value(string, length, 1, "mir_string_concat_result_length")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_struct_value();
        Ok(string.into())
    }

    /// Compare the exact ManagedAny-i32/String profile by borrowing a fresh
    /// length-bearing String snapshot from the runtime. The owner itself is
    /// never cloned, dropped, or transferred by equality; its Checker-sealed
    /// ownership ledger remains live after this read.
    fn emit_managed_any_string_comparison(
        &mut self,
        op: ResolvedBinaryOp,
        owner: &MirValueId,
        string: &MirValueId,
        managed_any_ty: &crate::core::ResolvedTypeId,
        string_ty: &crate::core::ResolvedTypeId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        super::ordinary_map_any_get::validate_managed_any_type(
            self.program.type_catalog(),
            managed_any_ty,
        )
        .map_err(|message| NativeMirError::new(subject, message))?;
        self.program
            .type_catalog()
            .validate_owned_string(string_ty)
            .map_err(|message| NativeMirError::new(subject, message))?;

        let c = self.generator.context;
        let ptr_ty = c.ptr_type(inkwell::AddressSpace::default());
        let owner_value = self.value(owner, subject)?.into_int_value();
        let string_value = self.value(string, subject)?.into_struct_value();
        let peer_data = self
            .generator
            .builder
            .build_extract_value(string_value, 0, "mir_any_string_peer_data")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_pointer_value();
        let peer_len = self
            .generator
            .builder
            .build_extract_value(string_value, 1, "mir_any_string_peer_len")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();

        let out_data = self
            .generator
            .builder
            .build_alloca(ptr_ty, "mir_any_string_copy_data_out")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let out_len = self
            .generator
            .builder
            .build_alloca(c.i64_type(), "mir_any_string_copy_len_out")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(out_data, ptr_ty.const_null())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(out_len, c.i64_type().const_zero())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        let copy_fn = self
            .generator
            .get_runtime_fn("mimi_mir_managed_any_i32_string_v1_copy_string")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let copy_status = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    copy_fn,
                    &[
                        owner_value.into(),
                        c.i32_type().const_int(2, false).into(),
                        c.i32_type().const_int(1, false).into(),
                        out_data.into(),
                        out_len.into(),
                    ],
                    "mir_any_string_copy_status",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "ManagedAny String copy returned void"))?
        .into_int_value();
        let is_ok = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                copy_status,
                c.i32_type().const_zero(),
                "mir_any_string_copy_ok",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let is_kind_mismatch = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                copy_status,
                c.i32_type().const_int(2, false),
                "mir_any_string_copy_kind_mismatch",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let status_is_supported = self
            .generator
            .builder
            .build_or(is_ok, is_kind_mismatch, "mir_any_string_copy_status_valid")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_ordinary_map_owned_get_root_guard(
            status_is_supported,
            "[E0800] ManagedAny String equality copy failed",
            subject,
        )?;

        let mismatch = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_any_string_wrong_kind");
        let compare = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_any_string_compare");
        let result_merge = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_any_string_result");
        self.generator
            .builder
            .build_conditional_branch(is_kind_mismatch, mismatch, compare)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        self.generator.builder.position_at_end(mismatch);
        let mismatch_result = c
            .bool_type()
            .const_int(u64::from(op == ResolvedBinaryOp::NotEqual), false);
        let mismatch_end = self.generator.builder.get_insert_block().ok_or_else(|| {
            NativeMirError::new(
                subject,
                "ManagedAny wrong-kind block has no insertion point",
            )
        })?;
        self.generator
            .builder
            .build_unconditional_branch(result_merge)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        self.generator.builder.position_at_end(compare);
        let copied_data = self
            .generator
            .builder
            .build_load(ptr_ty, out_data, "mir_any_string_copy_data")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_pointer_value();
        let copied_len = self
            .generator
            .builder
            .build_load(c.i64_type(), out_len, "mir_any_string_copy_len")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let copied_nonnull = self
            .generator
            .builder
            .build_is_not_null(copied_data, "mir_any_string_copy_nonnull")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let copied_len_nonnegative = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::SGE,
                copied_len,
                c.i64_type().const_zero(),
                "mir_any_string_copy_len_nonnegative",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let copied_abi_valid = self
            .generator
            .builder
            .build_and(
                copied_nonnull,
                copied_len_nonnegative,
                "mir_any_string_copy_abi_valid",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_ordinary_map_owned_get_root_guard(
            copied_abi_valid,
            "[E0800] malformed ManagedAny String equality copy",
            subject,
        )?;
        self.emit_ordinary_map_domain_own_string(copied_data, copied_len, subject)?;

        let peer_len_nonnegative = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::SGE,
                peer_len,
                c.i64_type().const_zero(),
                "mir_any_string_peer_len_nonnegative",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_ordinary_map_owned_get_root_guard(
            peer_len_nonnegative,
            "[E0800] malformed String operand in ManagedAny equality",
            subject,
        )?;
        let peer_is_empty = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                peer_len,
                c.i64_type().const_zero(),
                "mir_any_string_peer_is_empty",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let peer_data_nonnull = self
            .generator
            .builder
            .build_is_not_null(peer_data, "mir_any_string_peer_nonnull")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let peer_data_valid = self
            .generator
            .builder
            .build_or(
                peer_is_empty,
                peer_data_nonnull,
                "mir_any_string_peer_data_valid",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_ordinary_map_owned_get_root_guard(
            peer_data_valid,
            "[E0800] malformed String pointer in ManagedAny equality",
            subject,
        )?;

        let same_length = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                copied_len,
                peer_len,
                "mir_any_string_lengths_equal",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let compare_bytes = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_any_string_compare_bytes");
        let lengths_differ = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_any_string_lengths_differ");
        let compare_merge = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_any_string_compare_result");
        self.generator
            .builder
            .build_conditional_branch(same_length, compare_bytes, lengths_differ)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        self.generator.builder.position_at_end(lengths_differ);
        let different_length_equal = c.bool_type().const_zero();
        let lengths_differ_end = self.generator.builder.get_insert_block().ok_or_else(|| {
            NativeMirError::new(
                subject,
                "ManagedAny length-mismatch block has no insertion point",
            )
        })?;
        self.generator
            .builder
            .build_unconditional_branch(compare_merge)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        self.generator.builder.position_at_end(compare_bytes);
        let is_empty = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                copied_len,
                c.i64_type().const_zero(),
                "mir_any_string_equal_length_is_zero",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let equal_empty = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_any_string_equal_empty");
        let compare_nonempty = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_any_string_compare_nonempty");
        self.generator
            .builder
            .build_conditional_branch(is_empty, equal_empty, compare_nonempty)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        self.generator.builder.position_at_end(equal_empty);
        let empty_equal = c.bool_type().const_int(1, false);
        let empty_end = self.generator.builder.get_insert_block().ok_or_else(|| {
            NativeMirError::new(
                subject,
                "ManagedAny empty-string block has no insertion point",
            )
        })?;
        self.generator
            .builder
            .build_unconditional_branch(compare_merge)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        self.generator.builder.position_at_end(compare_nonempty);
        let memcmp_fn = self
            .generator
            .module
            .get_function("memcmp")
            .unwrap_or_else(|| {
                self.generator.module.add_function(
                    "memcmp",
                    c.i32_type()
                        .fn_type(&[ptr_ty.into(), ptr_ty.into(), c.i64_type().into()], false),
                    Some(inkwell::module::Linkage::External),
                )
            });
        let byte_cmp = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    memcmp_fn,
                    &[copied_data.into(), peer_data.into(), copied_len.into()],
                    "mir_any_string_memcmp",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "memcmp returned void"))?
        .into_int_value();
        let bytes_equal = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                byte_cmp,
                c.i32_type().const_zero(),
                "mir_any_string_bytes_equal",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let bytes_end = self.generator.builder.get_insert_block().ok_or_else(|| {
            NativeMirError::new(
                subject,
                "ManagedAny byte-comparison block has no insertion point",
            )
        })?;
        self.generator
            .builder
            .build_unconditional_branch(compare_merge)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        self.generator.builder.position_at_end(compare_merge);
        let payload_equal = self
            .generator
            .builder
            .build_phi(c.bool_type(), "mir_any_string_payload_equal")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        payload_equal.add_incoming(&[
            (&different_length_equal, lengths_differ_end),
            (&empty_equal, empty_end),
            (&bytes_equal, bytes_end),
        ]);
        let equal_value = payload_equal.as_basic_value().into_int_value();
        let comparison_result = if op == ResolvedBinaryOp::Equal {
            equal_value
        } else {
            self.generator
                .builder
                .build_not(equal_value, "mir_any_string_payload_not_equal")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?
        };
        let copied_string = self
            .generator
            .build_string_struct(copied_data, copied_len)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_owned_string_drop_value(copied_string, subject)?;
        let success_end = self.generator.builder.get_insert_block().ok_or_else(|| {
            NativeMirError::new(
                subject,
                "ManagedAny comparison success has no insertion point",
            )
        })?;
        self.generator
            .builder
            .build_unconditional_branch(result_merge)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        self.generator.builder.position_at_end(result_merge);
        let final_result = self
            .generator
            .builder
            .build_phi(c.bool_type(), "mir_any_string_comparison")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        final_result.add_incoming(&[
            (&mismatch_result, mismatch_end),
            (&comparison_result, success_end),
        ]);
        Ok(final_result.as_basic_value())
    }

    /// SD-8 family: a ±0.0 float divisor traps E0801 through the same
    /// `mimi_trap_float_div_by_zero` runtime face the AST emitter uses, so
    /// the trap code and wording agree across the bytecode VM, the AST
    /// native path, and this MIR path.
    fn emit_float_div_zero_guard(
        &mut self,
        right: inkwell::values::FloatValue<'ctx>,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let zero = right.get_type().const_float(0.0);
        let is_zero = self
            .generator
            .builder
            .build_float_compare(inkwell::FloatPredicate::OEQ, right, zero, "mir_fdiv_zero")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let function = self.llvm_function;
        let trap = self
            .generator
            .context
            .append_basic_block(function, "mir_fdiv_zero_trap");
        let ok = self
            .generator
            .context
            .append_basic_block(function, "mir_fdiv_zero_ok");
        self.generator
            .builder
            .build_conditional_branch(is_zero, trap, ok)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(trap);
        self.emit_ordinary_map_trap_cleanup(subject)?;
        let trap_fn = self
            .generator
            .get_runtime_fn("mimi_trap_float_div_by_zero")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(trap_fn, &[], "mir_fdiv_zero_trap_call")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_unreachable()
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(ok);
        Ok(())
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
        self.emit_ordinary_map_trap_cleanup(subject)?;
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

    /// SD-7 checked signed multiply: the overflow intrinsic feeds the same
    /// E0802 trap as add/subtract, matching the reference oracle and the
    /// bytecode VM (`Op::CheckI32` kind 2 at i32 width).
    pub(super) fn emit_checked_mul(
        &mut self,
        left: inkwell::values::IntValue<'ctx>,
        right: inkwell::values::IntValue<'ctx>,
        subject: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, NativeMirError> {
        use crate::codegen::CallSiteValueExt;
        let int_ty = left.get_type();
        let intrinsic_name = format!("llvm.smul.with.overflow.i{}", int_ty.get_bit_width());
        let struct_ty = self.generator.context.struct_type(
            &[
                BasicTypeEnum::IntType(int_ty),
                BasicTypeEnum::IntType(self.generator.context.bool_type()),
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
            .generator
            .module
            .get_function(&intrinsic_name)
            .unwrap_or_else(|| {
                self.generator.module.add_function(
                    &intrinsic_name,
                    fn_type,
                    Some(Linkage::External),
                )
            });
        let call = self
            .generator
            .builder
            .build_call(
                intrinsic_fn,
                &[
                    BasicMetadataValueEnum::IntValue(left),
                    BasicMetadataValueEnum::IntValue(right),
                ],
                "mir_checked_mul",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let result_struct = call
            .try_as_basic_value_opt()
            .ok_or_else(|| NativeMirError::new(subject, "checked multiply returned void"))?
            .into_struct_value();
        let result = self
            .generator
            .builder
            .build_extract_value(result_struct, 0, "mir_mul_result")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let overflow = self
            .generator
            .builder
            .build_extract_value(result_struct, 1, "mir_mul_overflow")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let function = self.llvm_function;
        let trap = self
            .generator
            .context
            .append_basic_block(function, "mir_mul_overflow");
        let ok = self
            .generator
            .context
            .append_basic_block(function, "mir_mul_ok");
        self.generator
            .builder
            .build_conditional_branch(overflow, trap, ok)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_overflow_trap(trap, "mul", subject)?;
        self.generator.builder.position_at_end(ok);
        Ok(result)
    }

    /// SD-8 checked signed division/remainder: a zero divisor traps E0801
    /// and `MIN / -1` traps E0802 before the `sdiv`/`srem` can poison, so
    /// the trap faces agree with the reference oracle and the bytecode VM.
    pub(super) fn emit_checked_div_rem(
        &mut self,
        left: inkwell::values::IntValue<'ctx>,
        right: inkwell::values::IntValue<'ctx>,
        divide: bool,
        subject: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, NativeMirError> {
        let int_ty = left.get_type();
        let bit_width = int_ty.get_bit_width();
        let zero = int_ty.const_zero();
        let is_zero = self
            .generator
            .builder
            .build_int_compare(IntPredicate::EQ, right, zero, "mir_div_zero_check")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let function = self.llvm_function;
        let zero_trap = self
            .generator
            .context
            .append_basic_block(function, "mir_div_zero_trap");
        let zero_ok = self
            .generator
            .context
            .append_basic_block(function, "mir_div_zero_ok");
        self.generator
            .builder
            .build_conditional_branch(is_zero, zero_trap, zero_ok)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(zero_trap);
        if super::actor_status::is_actor_tagged(self.program, &self.function.owner) {
            self.emit_actor_trap_status(801, subject)?;
        } else {
            self.emit_ordinary_map_trap_cleanup(subject)?;
            let trap_fn = self
                .generator
                .get_runtime_fn("mimi_trap_div_by_zero")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            self.generator
                .builder
                .build_call(trap_fn, &[], "mir_div_zero_trap_call")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            self.generator
                .builder
                .build_unreachable()
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        }
        self.generator.builder.position_at_end(zero_ok);
        let min_val = int_ty
            .const_int(1, false)
            .const_shl(int_ty.const_int((bit_width - 1) as u64, false));
        let neg_one = int_ty.const_all_ones();
        let left_is_min = self
            .generator
            .builder
            .build_int_compare(IntPredicate::EQ, left, min_val, "mir_div_left_is_min")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let right_is_neg_one = self
            .generator
            .builder
            .build_int_compare(IntPredicate::EQ, right, neg_one, "mir_div_right_is_neg_one")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let min_over_neg_one = self
            .generator
            .builder
            .build_and(left_is_min, right_is_neg_one, "mir_div_min_over_neg_one")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let overflow_trap = self
            .generator
            .context
            .append_basic_block(function, "mir_div_overflow_trap");
        let safe = self
            .generator
            .context
            .append_basic_block(function, "mir_div_safe");
        self.generator
            .builder
            .build_conditional_branch(min_over_neg_one, overflow_trap, safe)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(overflow_trap);
        if super::actor_status::is_actor_tagged(self.program, &self.function.owner) {
            self.emit_actor_trap_status(802, subject)?;
        } else {
            self.emit_ordinary_map_trap_cleanup(subject)?;
            let overflow_fn = self
                .generator
                .get_runtime_fn("mimi_trap_div_overflow")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            self.generator
                .builder
                .build_call(overflow_fn, &[], "mir_div_overflow_trap_call")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            self.generator
                .builder
                .build_unreachable()
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        }
        self.generator.builder.position_at_end(safe);
        if divide {
            self.generator
                .builder
                .build_int_signed_div(left, right, "mir_sdiv")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))
        } else {
            self.generator
                .builder
                .build_int_signed_rem(left, right, "mir_srem")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))
        }
    }
}
