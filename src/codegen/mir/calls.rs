//! Canonical MIR call lowering for the native consumer.

use super::*;

impl<'a, 'ctx> NativeMirFunctionEmitter<'a, 'ctx> {
    pub(super) fn emit_variant_predicate(
        &mut self,
        result: &MirValueId,
        predicate: MirVariantPredicate,
        variant: &MirValueId,
        receipt: Option<&crate::core::mir::types::MirVariantPredicateContract>,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let result_ty = self.value_type(result, subject)?;
        let variant_ty = self.value_type(variant, subject)?;
        let receipt = receipt.ok_or_else(|| {
            NativeMirError::new(subject, "variant predicate has no canonical receipt")
        })?;
        self.program
            .type_catalog()
            .validate_variant_predicate_receipt(&result_ty, &variant_ty, predicate, receipt)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let (variant_abi, _) = native_variant_abi(self.program.type_catalog(), &variant_ty, false)?;
        let value = self.value(variant, subject)?.into_struct_value();
        let tag = self
            .generator
            .builder
            .build_extract_value(value, variant_abi.tag_field, "mir_variant_predicate_tag")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let expected = self
            .generator
            .context
            .i8_type()
            .const_int(u64::from(receipt.discriminant), false);
        self.generator
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                tag,
                expected,
                "mir_variant_predicate",
            )
            .map(BasicValueEnum::from)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))
    }

    pub(super) fn emit_list_op(
        &mut self,
        result: &MirValueId,
        operation: MirListOperation,
        list: &MirValueId,
        argument: Option<&MirValueId>,
        list_operation_contract: Option<&crate::core::mir::types::MirListOperationContract>,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let result_ty = self.value_type(result, subject)?;
        let list_ty = self.value_type(list, subject)?;
        let argument_ty = argument
            .map(|value| self.value_type(value, subject))
            .transpose()?;
        let receipt = list_operation_contract.ok_or_else(|| {
            NativeMirError::new(subject, "List operation has no canonical receipt")
        })?;
        self.program
            .type_catalog()
            .validate_list_operation_receipt_with_argument(
                &result_ty,
                &list_ty,
                argument_ty.as_ref(),
                operation,
                receipt,
            )
            .map_err(|message| NativeMirError::new(subject, message))?;
        let list_desc = self
            .program
            .type_catalog()
            .get(&list_ty)
            .ok_or_else(|| NativeMirError::new(subject, "List TypeDesc is absent"))?;
        let MirLayout::List { .. } = &list_desc.layout else {
            return Err(NativeMirError::new(
                subject,
                "List operation receiver has a non-List layout",
            ));
        };
        let list_handle = self.value(list, subject)?.into_pointer_value();
        let argument_handle = argument
            .map(|value| {
                self.value(value, subject)
                    .map(|value| value.into_pointer_value())
            })
            .transpose()?;
        let kind = native_list_kind(self.program.type_catalog(), &receipt.list_ty)?;
        let kind_value = self
            .generator
            .context
            .i8_type()
            .const_int(kind as u64, false);
        let runtime_name = match operation {
            MirListOperation::Len => "mimi_mir_list_len_scalar",
            MirListOperation::Reverse => "mimi_mir_list_reverse_scalar",
            MirListOperation::Concat => "mimi_mir_list_concat_scalar",
        };
        if operation == MirListOperation::Concat {
            crate::codegen::builtins::register_mir_list_concat_runtime(
                &self.generator.module,
                self.generator.context,
            );
        }
        let function = self
            .generator
            .get_runtime_fn(runtime_name)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let mut call_arguments = vec![BasicMetadataValueEnum::from(list_handle)];
        if let Some(argument_handle) = argument_handle {
            call_arguments.push(BasicMetadataValueEnum::from(argument_handle));
        }
        call_arguments.push(BasicMetadataValueEnum::from(kind_value));
        let value = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    function,
                    &call_arguments,
                    match operation {
                        MirListOperation::Len => "mir_list_len",
                        MirListOperation::Reverse => "mir_list_reverse",
                        MirListOperation::Concat => "mir_list_concat",
                    },
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "List operation returned void"))?;
        Ok(value)
    }

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
                            MirBuiltinKind::PrintlnBool => unreachable!(),
                            MirBuiltinKind::PrintlnInt => unreachable!(),
                        },
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))
            }
            MirBuiltinKind::PrintlnBool => {
                let is_true = self
                    .generator
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        left,
                        left.get_type().const_zero(),
                        "mir_println_bool",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                let true_text = self
                    .generator
                    .builder
                    .build_global_string_ptr("true", "mir_println_true")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                let false_text = self
                    .generator
                    .builder
                    .build_global_string_ptr("false", "mir_println_false")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                let text = self
                    .generator
                    .builder
                    .build_select(
                        is_true,
                        true_text.as_pointer_value(),
                        false_text.as_pointer_value(),
                        "mir_println_text",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                    .into_pointer_value();
                let puts = self
                    .generator
                    .get_runtime_fn("puts")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                self.generator
                    .builder
                    .build_call(
                        puts,
                        &[BasicMetadataValueEnum::PointerValue(text)],
                        "mir_println_call",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                // Unit has no physical LLVM value.  The MIR result slot is
                // never observed by a valid caller; keep a harmless scalar
                // placeholder for the emitter's value table.
                Ok(self.generator.context.i64_type().const_zero().into())
            }
            MirBuiltinKind::PrintlnInt => {
                let value = if left.get_type().get_bit_width() < 64 {
                    self.generator
                        .builder
                        .build_int_s_extend(
                            left,
                            self.generator.context.i64_type(),
                            "mir_println_int_sext",
                        )
                        .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                } else {
                    left
                };
                let format = self
                    .generator
                    .builder
                    .build_global_string_ptr("%ld\n", "mir_println_int_format")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                let printf = self
                    .generator
                    .get_runtime_fn("printf")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                self.generator
                    .builder
                    .build_call(
                        printf,
                        &[
                            BasicMetadataValueEnum::PointerValue(format.as_pointer_value()),
                            BasicMetadataValueEnum::IntValue(value),
                        ],
                        "mir_println_int_call",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                // Unit has no physical LLVM value. Keep the same inert
                // placeholder convention as PrintlnBool for the value map.
                Ok(self.generator.context.i64_type().const_zero().into())
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
        type_arguments: &[crate::core::ResolvedTypeId],
        arguments: &[MirValueId],
        variant_call_contract: Option<&crate::core::mir::types::MirVariantCallAbiContract>,
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
        let target = self.program.functions().get(owner).ok_or_else(|| {
            NativeMirError::new(
                subject,
                format!("callee '{}' is absent from MIR program", owner.0),
            )
        })?;
        let parameter_types = target
            .parameters
            .iter()
            .filter_map(|parameter| target.values.get(parameter))
            .map(|value| value.ty.clone())
            .collect::<Vec<_>>();
        let flat_variant_result = self
            .program
            .type_catalog()
            .validate_flat_copy_variant(&target.result)
            .is_ok();
        let move_owned_result = self
            .program
            .type_catalog()
            .validate_result_string_i32_variant(&target.result)
            .is_ok();
        if flat_variant_result || move_owned_result {
            let receipt = variant_call_contract.ok_or_else(|| {
                NativeMirError::new(
                    subject,
                    if flat_variant_result {
                        "call returning flat Copy Option/Result has no canonical ABI receipt"
                    } else {
                        "call returning move-owned Result<string, i32> has no canonical ABI receipt"
                    },
                )
            })?;
            self.program
                .type_catalog()
                .validate_variant_call_abi_receipt(
                    owner,
                    type_arguments,
                    &parameter_types,
                    &target.result,
                    receipt,
                )
                .map_err(|message| NativeMirError::new(subject, message))?;
            if move_owned_result {
                crate::core::mir::validate_move_owned_result_return_merge(
                    target,
                    self.program.type_catalog(),
                )
                .map_err(|message| NativeMirError::new(subject, message))?;
            }
        } else if variant_call_contract.is_some() {
            return Err(NativeMirError::new(
                subject,
                "variant call ABI receipt is attached to an unsupported variant result",
            ));
        } else if self
            .program
            .type_catalog()
            .get(&target.result)
            .is_some_and(|descriptor| {
                descriptor.kind == MirTypeKind::Result && descriptor.ownership != MirOwnership::Copy
            })
        {
            return Err(NativeMirError::new(
                subject,
                "non-Copy Result call result is outside the canonical call ABI contract",
            ));
        }
        self.emit_call_target(result, function, arguments, subject)
    }

    pub(super) fn emit_flow_transition(
        &mut self,
        result: &MirValueId,
        transition: &crate::core::NodeId,
        arguments: &[MirValueId],
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let contract = self.program.transitions().get(transition).ok_or_else(|| {
            NativeMirError::new(
                subject,
                format!(
                    "flow transition '{}' has no canonical contract",
                    transition.0
                ),
            )
        })?;
        if contract.effect != crate::core::mir::MirTransitionEffect::SilentLocal
            || contract.targets.len() != 1
            || contract.failure.is_some()
            || contract.is_fallback
            || contract.is_ffi_pinned
            || contract.targets.first() != Some(&contract.result)
        {
            return Err(NativeMirError::new(
                subject,
                "FlowTransition is outside the silent-local native contract",
            ));
        }
        let function = *self.functions.get(&contract.owner).ok_or_else(|| {
            NativeMirError::new(
                subject,
                format!(
                    "flow transition '{}' is absent from native declarations",
                    transition.0
                ),
            )
        })?;
        self.emit_call_target(Some(result), function, arguments, subject)
    }

    fn emit_call_target(
        &mut self,
        result: Option<&MirValueId>,
        function: FunctionValue<'ctx>,
        arguments: &[MirValueId],
        subject: &str,
    ) -> Result<(), NativeMirError> {
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
