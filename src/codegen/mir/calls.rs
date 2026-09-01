//! Canonical MIR call lowering for the native consumer.

use super::*;

impl<'a, 'ctx> NativeMirFunctionEmitter<'a, 'ctx> {
    pub(super) fn emit_set_op(
        &mut self,
        result: &MirValueId,
        operation: MirSetOperation,
        set: &MirValueId,
        argument: Option<&MirValueId>,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let result_ty = self.value_type(result, subject)?;
        let set_ty = self.value_type(set, subject)?;
        let argument_ty = argument
            .map(|value| self.value_type(value, subject))
            .transpose()?;
        self.program
            .type_catalog()
            .validate_set_operation(&result_ty, &set_ty, argument_ty.as_ref(), operation)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let set_handle = self.value(set, subject)?.into_int_value();
        let set_desc = self
            .program
            .type_catalog()
            .get(&set_ty)
            .ok_or_else(|| NativeMirError::new(subject, "Set TypeDesc is absent"))?;
        let element_ty = match &set_desc.layout {
            MirLayout::Set { element } => element.clone(),
            layout => {
                return Err(NativeMirError::new(
                    subject,
                    format!("Set operation receiver has non-Set layout {layout:?}"),
                ))
            }
        };
        let element_desc = self
            .program
            .type_catalog()
            .get(&element_ty)
            .ok_or_else(|| NativeMirError::new(subject, "Set element TypeDesc is absent"))?;
        let size_fn = || {
            self.generator
                .get_runtime_fn("mimi_set_size")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))
        };
        match operation {
            MirSetOperation::Size => {
                let value = call_try_basic_value(
                    &self
                        .generator
                        .builder
                        .build_call(
                            size_fn()?,
                            &[BasicMetadataValueEnum::from(set_handle)],
                            "mir_set_size",
                        )
                        .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
                )
                .ok_or_else(|| NativeMirError::new(subject, "Set.size returned void"))?
                .into_int_value();
                self.generator
                    .builder
                    .build_int_truncate(
                        value,
                        self.generator.context.i32_type(),
                        "mir_set_size_i32",
                    )
                    .map(BasicValueEnum::from)
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))
            }
            MirSetOperation::IsEmpty => {
                let value = call_try_basic_value(
                    &self
                        .generator
                        .builder
                        .build_call(
                            size_fn()?,
                            &[BasicMetadataValueEnum::from(set_handle)],
                            "mir_set_size",
                        )
                        .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
                )
                .ok_or_else(|| NativeMirError::new(subject, "Set.is_empty returned void"))?
                .into_int_value();
                self.generator
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        value,
                        self.generator.context.i64_type().const_zero(),
                        "mir_set_is_empty",
                    )
                    .map(BasicValueEnum::from)
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))
            }
            MirSetOperation::Contains => {
                let argument = argument.ok_or_else(|| {
                    NativeMirError::new(subject, "Set.contains argument is absent")
                })?;
                let scalar = self.emit_set_scalar_as_i64(argument, element_desc, subject)?;
                let function = self
                    .generator
                    .get_runtime_fn("mimi_set_contains")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                let value = call_try_basic_value(
                    &self
                        .generator
                        .builder
                        .build_call(
                            function,
                            &[
                                BasicMetadataValueEnum::from(set_handle),
                                BasicMetadataValueEnum::from(scalar),
                            ],
                            "mir_set_contains",
                        )
                        .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
                )
                .ok_or_else(|| NativeMirError::new(subject, "Set.contains returned void"))?
                .into_int_value();
                self.generator
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        value,
                        self.generator.context.i64_type().const_zero(),
                        "mir_set_contains_bool",
                    )
                    .map(BasicValueEnum::from)
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))
            }
            MirSetOperation::Insert | MirSetOperation::Remove => {
                let argument = argument.ok_or_else(|| {
                    NativeMirError::new(subject, "Set transformation argument is absent")
                })?;
                let scalar = self.emit_set_scalar_as_i64(argument, element_desc, subject)?;
                let name = if operation == MirSetOperation::Insert {
                    "mimi_set_insert"
                } else {
                    "mimi_set_remove"
                };
                let function = self
                    .generator
                    .get_runtime_fn(name)
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                let value = call_try_basic_value(
                    &self
                        .generator
                        .builder
                        .build_call(
                            function,
                            &[
                                BasicMetadataValueEnum::from(set_handle),
                                BasicMetadataValueEnum::from(scalar),
                            ],
                            "mir_set_transform",
                        )
                        .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
                )
                .ok_or_else(|| NativeMirError::new(subject, "Set transformation returned void"))?;
                self.emit_set_handle_result_abort(value.into_int_value(), subject)?;
                Ok(value)
            }
            MirSetOperation::ToList => {
                let kind = native_list_kind(self.program.type_catalog(), &result_ty)?;
                let kind_value = self
                    .generator
                    .context
                    .i8_type()
                    .const_int(kind as u64, false);
                let function = self
                    .generator
                    .get_runtime_fn("mimi_mir_set_to_list_scalar")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                let value = call_try_basic_value(
                    &self
                        .generator
                        .builder
                        .build_call(
                            function,
                            &[
                                BasicMetadataValueEnum::from(set_handle),
                                BasicMetadataValueEnum::from(kind_value),
                            ],
                            "mir_set_to_list",
                        )
                        .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
                )
                .ok_or_else(|| NativeMirError::new(subject, "Set.to_list returned void"))?
                .into_pointer_value();
                self.emit_list_null_abort(
                    value,
                    subject,
                    "canonical MIR Set.to_list allocation failed",
                )?;
                Ok(value.into())
            }
        }
    }

    fn emit_set_handle_result_abort(
        &mut self,
        value: inkwell::values::IntValue<'ctx>,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let failed = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                value,
                self.generator.context.i64_type().const_zero(),
                "mir_set_transform_failed",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let fail = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_set_transform_abort");
        let ok = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_set_transform_ok");
        self.generator
            .builder
            .build_conditional_branch(failed, fail, ok)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(fail);
        self.emit_abort_with_message("[E0800] canonical MIR Set operation failed", subject)?;
        self.generator.builder.position_at_end(ok);
        Ok(())
    }

    pub(super) fn emit_builtin(
        &mut self,
        result: &MirValueId,
        kind: MirBuiltinKind,
        arguments: &[MirValueId],
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let left = self
            .value(
                arguments
                    .first()
                    .ok_or_else(|| NativeMirError::new(subject, "builtin argument is absent"))?,
                subject,
            )?
            .into_int_value();
        match kind {
            MirBuiltinKind::Abs => {
                let min = left
                    .get_type()
                    .const_int(1u64 << (left.get_type().get_bit_width() - 1), false);
                let is_min = self
                    .generator
                    .builder
                    .build_int_compare(IntPredicate::EQ, left, min, "mir_abs_min")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                let function = self.llvm_function;
                let trap = self
                    .generator
                    .context
                    .append_basic_block(function, "mir_abs_overflow");
                let ok = self
                    .generator
                    .context
                    .append_basic_block(function, "mir_abs_ok");
                self.generator
                    .builder
                    .build_conditional_branch(is_min, trap, ok)
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                self.emit_overflow_trap(trap, "abs", subject)?;
                self.generator.builder.position_at_end(ok);
                let negated = self
                    .generator
                    .builder
                    .build_int_sub(left.get_type().const_zero(), left, "mir_abs_negated")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                let is_nonnegative = self
                    .generator
                    .builder
                    .build_int_compare(
                        IntPredicate::SGE,
                        left,
                        left.get_type().const_zero(),
                        "mir_abs_nonnegative",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                self.generator
                    .builder
                    .build_select(is_nonnegative, left, negated, "mir_abs")
                    .map(BasicValueEnum::from)
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))
            }
            MirBuiltinKind::Min | MirBuiltinKind::Max => {
                let right = self
                    .value(
                        arguments.get(1).ok_or_else(|| {
                            NativeMirError::new(subject, "builtin right argument is absent")
                        })?,
                        subject,
                    )?
                    .into_int_value();
                let predicate = if kind == MirBuiltinKind::Min {
                    IntPredicate::SLT
                } else {
                    IntPredicate::SGT
                };
                let condition = self
                    .generator
                    .builder
                    .build_int_compare(predicate, left, right, "mir_minmax_cmp")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                self.generator
                    .builder
                    .build_select(
                        condition,
                        left,
                        right,
                        match kind {
                            MirBuiltinKind::Min => "mir_min",
                            MirBuiltinKind::Max => "mir_max",
                            MirBuiltinKind::Abs => unreachable!(),
                        },
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))
            }
        }
        .map(|value| {
            let _ = result;
            value
        })
    }

    pub(super) fn emit_call(
        &mut self,
        result: Option<&MirValueId>,
        callee: &ResolvedCallee,
        arguments: &[MirValueId],
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let ResolvedCallee::Function(owner) = callee else {
            return Err(NativeMirError::new(
                subject,
                format!("callee {callee:?} is not a canonical function"),
            ));
        };
        let function = *self.functions.get(owner).ok_or_else(|| {
            NativeMirError::new(
                subject,
                format!("callee '{}' is absent from native declarations", owner.0),
            )
        })?;
        let values = arguments
            .iter()
            .map(|argument| {
                self.value(argument, subject)
                    .map(BasicMetadataValueEnum::from)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let call = self
            .generator
            .builder
            .build_call(function, &values, "mir_call")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        if let Some(result) = result {
            let desc = self.value_desc(result, subject)?;
            if desc.abi != MirAbiClass::Unit {
                let value = call_try_basic_value(&call).ok_or_else(|| {
                    NativeMirError::new(subject, "non-unit MIR call returned void")
                })?;
                self.values.insert(result.clone(), value);
            }
        }
        Ok(())
    }
}
