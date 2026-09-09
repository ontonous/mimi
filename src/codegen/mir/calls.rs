//! Canonical MIR call lowering for the native consumer.

use super::*;

impl<'a, 'ctx> NativeMirFunctionEmitter<'a, 'ctx> {
    pub(super) fn emit_session_pair_bind(
        &mut self,
        lo: &MirValueId,
        hi: &MirValueId,
        receipt: Option<&crate::core::mir::types::MirSessionPairBindContract>,
        subject: &str,
    ) -> Result<(BasicValueEnum<'ctx>, BasicValueEnum<'ctx>), NativeMirError> {
        let receipt = receipt.ok_or_else(|| {
            NativeMirError::new(
                subject,
                "typed session_pair binding has no canonical TypeDesc receipt",
            )
        })?;
        let lo_ty = self.value_type(lo, subject)?;
        let hi_ty = self.value_type(hi, subject)?;
        self.program
            .type_catalog()
            .validate_session_pair_bind_receipt(&receipt.pair_ty, &lo_ty, &hi_ty, receipt)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let pair = self
            .generator
            .get_runtime_fn("mimi_session_pair")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let packed = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(pair, &[], "mir_typed_session_pair_packed")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "session_pair returned void"))?
        .into_int_value();
        let lo_fn = self
            .generator
            .get_runtime_fn("mimi_session_lo")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let hi_fn = self
            .generator
            .get_runtime_fn("mimi_session_hi")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let lo_value = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    lo_fn,
                    &[BasicMetadataValueEnum::IntValue(packed)],
                    "mir_typed_session_pair_lo",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "session_lo returned void"))?;
        let hi_value = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    hi_fn,
                    &[BasicMetadataValueEnum::IntValue(packed)],
                    "mir_typed_session_pair_hi",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "session_hi returned void"))?;
        Ok((lo_value, hi_value))
    }

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
        let nested_mode = receipt.mode == crate::core::mir::types::MirListOperationMode::Nested;
        let runtime_name = match (operation, nested_mode) {
            (MirListOperation::Len, true) => "mimi_mir_list_len_nested",
            (MirListOperation::Len, false) => "mimi_mir_list_len_scalar",
            (MirListOperation::Reverse, true) => "mimi_mir_list_reverse_nested",
            (MirListOperation::Reverse, false) => "mimi_mir_list_reverse_scalar",
            (MirListOperation::Concat, true) => "mimi_mir_list_concat_nested",
            (MirListOperation::Concat, false) => "mimi_mir_list_concat_scalar",
        };
        if operation == MirListOperation::Concat && !nested_mode {
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
        if !nested_mode {
            call_arguments.push(BasicMetadataValueEnum::from(kind_value));
        }
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
        string_field_contract: Option<&crate::core::mir::types::MirStringFieldBorrowContract>,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        if kind == MirBuiltinKind::SessionPair {
            if !arguments.is_empty() {
                return Err(NativeMirError::new(
                    subject,
                    "builtin 'session_pair' has no value arguments",
                ));
            }
            let result_ty = self.value_type(result, subject)?;
            self.program
                .type_catalog()
                .validate_plain_session_pair(&result_ty)
                .map_err(|message| NativeMirError::new(subject, message))?;
            let pair = self
                .generator
                .get_runtime_fn("mimi_session_pair")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let packed = call_try_basic_value(
                &self
                    .generator
                    .builder
                    .build_call(pair, &[], "mir_session_pair_packed")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
            )
            .ok_or_else(|| NativeMirError::new(subject, "session_pair returned void"))?
            .into_int_value();
            let lo = self
                .generator
                .get_runtime_fn("mimi_session_lo")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let hi = self
                .generator
                .get_runtime_fn("mimi_session_hi")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let lo = call_try_basic_value(
                &self
                    .generator
                    .builder
                    .build_call(
                        lo,
                        &[BasicMetadataValueEnum::IntValue(packed)],
                        "mir_session_pair_lo",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
            )
            .ok_or_else(|| NativeMirError::new(subject, "session_lo returned void"))?;
            let hi = call_try_basic_value(
                &self
                    .generator
                    .builder
                    .build_call(
                        hi,
                        &[BasicMetadataValueEnum::IntValue(packed)],
                        "mir_session_pair_hi",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
            )
            .ok_or_else(|| NativeMirError::new(subject, "session_hi returned void"))?;
            let struct_ty = native_basic_type(
                self.generator.context,
                self.program.type_catalog(),
                &result_ty,
            )?
            .into_struct_type();
            let aggregate = struct_ty.get_undef();
            let aggregate = self
                .generator
                .builder
                .build_insert_value(aggregate, lo, 0, "mir_session_pair_insert_lo")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                .into_struct_value();
            let aggregate = self
                .generator
                .builder
                .build_insert_value(aggregate, hi, 1, "mir_session_pair_insert_hi")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                .into_struct_value();
            return Ok(aggregate.into());
        }
        if kind == MirBuiltinKind::SessionOpen {
            if !arguments.is_empty() {
                return Err(NativeMirError::new(
                    subject,
                    "builtin 'session_open' has no value arguments",
                ));
            }
            let result_ty = self.value_type(result, subject)?;
            self.program
                .type_catalog()
                .validate_session_channel(&result_ty)
                .map_err(|message| NativeMirError::new(subject, message))?;
            let pair = self
                .generator
                .get_runtime_fn("mimi_session_pair")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let packed = call_try_basic_value(
                &self
                    .generator
                    .builder
                    .build_call(pair, &[], "mir_session_open_pair")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
            )
            .ok_or_else(|| NativeMirError::new(subject, "session_pair returned void"))?
            .into_int_value();
            let lo = self
                .generator
                .get_runtime_fn("mimi_session_lo")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let value = call_try_basic_value(
                &self
                    .generator
                    .builder
                    .build_call(
                        lo,
                        &[BasicMetadataValueEnum::IntValue(packed)],
                        "mir_session_open_endpoint",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
            )
            .ok_or_else(|| NativeMirError::new(subject, "session_lo returned void"))?;
            return Ok(value);
        }
        if kind == MirBuiltinKind::PrintlnString {
            let argument = arguments
                .first()
                .ok_or_else(|| NativeMirError::new(subject, "builtin argument is absent"))?;
            let value = if let Some(receipt) = string_field_contract {
                let source_ty = self.value_type(argument, subject)?;
                self.program
                    .type_catalog()
                    .validate_string_field_borrow_receipt(&source_ty, receipt)
                    .map_err(|message| NativeMirError::new(subject, message))?;
                self.generator
                    .builder
                    .build_extract_value(
                        self.value(argument, subject)?.into_struct_value(),
                        receipt.projection.field_index as u32,
                        "mir_println_borrowed_string_field",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                    .into_struct_value()
            } else {
                self.value(argument, subject)?.into_struct_value()
            };
            let data = self
                .generator
                .builder
                .build_extract_value(value, 0, "mir_println_string_data")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                .into_pointer_value();
            let len = self
                .generator
                .builder
                .build_extract_value(value, 1, "mir_println_string_len")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                .into_int_value();
            let print_bytes = self
                .generator
                .get_runtime_fn("mimi_print_bytes")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            self.generator
                .builder
                .build_call(
                    print_bytes,
                    &[
                        BasicMetadataValueEnum::PointerValue(data),
                        BasicMetadataValueEnum::IntValue(len),
                    ],
                    "mir_println_string_bytes",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let newline = self
                .generator
                .builder
                .build_global_string_ptr("\n", "mir_println_string_newline")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            self.generator
                .builder
                .build_call(
                    print_bytes,
                    &[
                        BasicMetadataValueEnum::PointerValue(newline.as_pointer_value()),
                        BasicMetadataValueEnum::IntValue(
                            self.generator.context.i64_type().const_int(1, false),
                        ),
                    ],
                    "mir_println_string_newline_call",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            if string_field_contract.is_none() {
                // The MIR ownership ledger gives a direct String println a
                // transferred owned temporary (the lowerer inserts Clone
                // when the source local must remain live).  The borrowed
                // record-field receipt is the only non-consuming shape.
                self.emit_owned_string_drop_value(value.into(), subject)?;
            }
            return Ok(self.generator.context.i64_type().const_zero().into());
        }
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
                            MirBuiltinKind::PrintlnString => unreachable!(),
                            MirBuiltinKind::SessionOpen => unreachable!(),
                            MirBuiltinKind::SessionPair => unreachable!(),
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
            MirBuiltinKind::PrintlnString => {
                unreachable!("PrintlnString handled before scalar dispatch")
            }
            MirBuiltinKind::SessionOpen => {
                unreachable!("SessionOpen handled before scalar dispatch")
            }
            MirBuiltinKind::SessionPair => {
                unreachable!("SessionPair handled before scalar dispatch")
            }
        }
        .map(|value| {
            let _ = result;
            value
        })
    }

    pub(super) fn emit_session_call(
        &mut self,
        result: &MirValueId,
        operation: crate::core::mir::types::MirSessionOperation,
        endpoint: &MirValueId,
        payload: Option<&MirValueId>,
        contract: Option<&crate::core::mir::types::MirSessionCallContract>,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let contract = contract.ok_or_else(|| {
            NativeMirError::new(subject, "SessionCall has no canonical residual/ABI receipt")
        })?;
        let endpoint_ty = self.value_type(endpoint, subject)?;
        let result_ty = self.value_type(result, subject)?;
        self.program
            .type_catalog()
            .validate_session_call_contract(&endpoint_ty, &result_ty, contract)
            .map_err(|message| NativeMirError::new(subject, message))?;
        if operation != contract.operation {
            return Err(NativeMirError::new(
                subject,
                "SessionCall receipt disagrees with MIR operation",
            ));
        }
        if payload.is_some() != contract.payload_ty.is_some() {
            return Err(NativeMirError::new(
                subject,
                "SessionCall payload value disagrees with its receipt TypeDesc",
            ));
        }
        let endpoint = self.value(endpoint, subject)?.into_int_value();
        match operation {
            crate::core::mir::types::MirSessionOperation::Close => {
                let function = self
                    .generator
                    .get_runtime_fn("mimi_channel_drop")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                self.generator
                    .builder
                    .build_call(
                        function,
                        &[BasicMetadataValueEnum::IntValue(endpoint)],
                        "mir_session_close",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                Ok(self.generator.context.i64_type().const_zero().into())
            }
            crate::core::mir::types::MirSessionOperation::Send => {
                let payload = payload.ok_or_else(|| {
                    NativeMirError::new(subject, "session_send receipt has no payload MIR value")
                })?;
                let payload_ty = self.value_type(payload, subject)?;
                if contract.payload_ty.as_ref() != Some(&payload_ty) {
                    return Err(NativeMirError::new(
                        subject,
                        "SessionCall payload value disagrees with its receipt TypeDesc identity",
                    ));
                }
                let payload_desc =
                    self.program
                        .type_catalog()
                        .get(&payload_ty)
                        .ok_or_else(|| {
                            NativeMirError::new(subject, "SessionCall payload TypeDesc is absent")
                        })?;
                let payload = self.value(payload, subject)?.into_int_value();
                let payload = if matches!(payload_desc.abi, MirAbiClass::Integer { bits, .. } if bits < 64)
                {
                    self.generator
                        .builder
                        .build_int_s_extend(
                            payload,
                            self.generator.context.i64_type(),
                            "mir_session_send_sext",
                        )
                        .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                } else {
                    payload
                };
                let function = self
                    .generator
                    .get_runtime_fn("mimi_channel_send")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                self.generator
                    .builder
                    .build_call(
                        function,
                        &[
                            BasicMetadataValueEnum::IntValue(endpoint),
                            BasicMetadataValueEnum::IntValue(payload),
                        ],
                        "mir_session_send",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                Ok(self.generator.context.i64_type().const_zero().into())
            }
            crate::core::mir::types::MirSessionOperation::Recv => {
                if payload.is_some() {
                    return Err(NativeMirError::new(
                        subject,
                        "session_recv cannot carry a payload MIR value",
                    ));
                }
                let result_desc = self.program.type_catalog().get(&result_ty).ok_or_else(|| {
                    NativeMirError::new(subject, "SessionCall result TypeDesc is absent")
                })?;
                let function = self
                    .generator
                    .get_runtime_fn("mimi_channel_recv")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                let value = call_try_basic_value(
                    &self
                        .generator
                        .builder
                        .build_call(
                            function,
                            &[BasicMetadataValueEnum::IntValue(endpoint)],
                            "mir_session_recv",
                        )
                        .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
                )
                .ok_or_else(|| NativeMirError::new(subject, "session_recv returned void"))?
                .into_int_value();
                let value = match result_desc.abi {
                    MirAbiClass::Integer {
                        bits: 32,
                        signed: true,
                    } => self
                        .generator
                        .builder
                        .build_int_truncate(
                            value,
                            self.generator.context.i32_type(),
                            "mir_session_recv_trunc",
                        )
                        .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                        .into(),
                    MirAbiClass::Integer {
                        bits: 64,
                        signed: true,
                    } => value.into(),
                    _ => {
                        return Err(NativeMirError::new(
                            subject,
                            "session_recv result is outside the signed integer ABI",
                        ))
                    }
                };
                Ok(value)
            }
        }
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
        if matches!(callee, ResolvedCallee::Extern(_)) {
            return self.emit_ffi_call(
                result,
                callee,
                type_arguments,
                arguments,
                variant_call_contract,
                subject,
            );
        }
        let Some(owner) = crate::core::mir::canonical_protocol_call_target(callee) else {
            return Err(NativeMirError::new(
                subject,
                format!("callee {callee:?} is not a canonical function"),
            ));
        };
        if let Err(message) = crate::core::mir::validate_protocol_method_identity(callee) {
            return Err(NativeMirError::new(subject, message));
        }
        let function = *self.functions.get(&owner).ok_or_else(|| {
            NativeMirError::new(
                subject,
                format!("callee '{}' is absent from native declarations", owner.0),
            )
        })?;
        let target = self.program.functions().get(&owner).ok_or_else(|| {
            NativeMirError::new(
                subject,
                format!("callee '{}' is absent from MIR program", owner.0),
            )
        })?;
        if let Some(message) = crate::core::mir::validate_materialized_call_abi(
            callee,
            self.function,
            target,
            result,
            arguments,
        )
        .into_iter()
        .next()
        {
            return Err(NativeMirError::new(subject, message));
        }
        if let Some(message) = crate::core::mir::validate_materialized_call_result_presence(
            callee,
            target,
            result,
            self.program.type_catalog(),
        )
        .into_iter()
        .next()
        {
            return Err(NativeMirError::new(subject, message));
        }
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
            .validate_result_move_variant(&target.result)
            .is_ok();
        let recoverable_result = self
            .program
            .type_catalog()
            .validate_recoverable_result_variant(&target.result)
            .is_ok();
        let recoverable_transition_body = self
            .program
            .transitions()
            .get(&self.function.owner)
            .is_some_and(|contract| contract.effect.is_recoverable());
        if flat_variant_result || move_owned_result {
            let receipt = variant_call_contract.ok_or_else(|| {
                NativeMirError::new(
                    subject,
                    if flat_variant_result {
                        "call returning flat Copy Option/Result has no canonical ABI receipt"
                    } else {
                        "call returning move-owned managed Result has no canonical ABI receipt"
                    },
                )
            })?;
            self.program
                .type_catalog()
                .validate_variant_call_abi_receipt(
                    &owner,
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
        } else if recoverable_result {
            if !recoverable_transition_body {
                return Err(NativeMirError::new(
                    subject,
                    "recoverable aggregate Result call is only valid inside a recoverable Flow transition",
                ));
            }
            let receipt = variant_call_contract.ok_or_else(|| {
                NativeMirError::new(
                    subject,
                    "recoverable aggregate Result call has no canonical ABI receipt",
                )
            })?;
            if receipt.mode != crate::core::mir::types::MirVariantCallAbiMode::RecoverableAggregate
            {
                return Err(NativeMirError::new(
                    subject,
                    "recoverable aggregate Result call has the wrong ABI receipt mode",
                ));
            }
            self.program
                .type_catalog()
                .validate_variant_call_abi_receipt(
                    &owner,
                    type_arguments,
                    &parameter_types,
                    &target.result,
                    receipt,
                )
                .map_err(|message| NativeMirError::new(subject, message))?;
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

    fn emit_ffi_call(
        &mut self,
        result: Option<&MirValueId>,
        callee: &ResolvedCallee,
        type_arguments: &[crate::core::ResolvedTypeId],
        arguments: &[MirValueId],
        variant_call_contract: Option<&crate::core::mir::types::MirVariantCallAbiContract>,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let ResolvedCallee::Extern(_) = callee else {
            unreachable!("emit_ffi_call called for non-extern callee");
        };
        if !type_arguments.is_empty() {
            return Err(NativeMirError::new(
                subject,
                "canonical native FFI call cannot have type arguments",
            ));
        }
        if variant_call_contract.is_some() {
            return Err(NativeMirError::new(
                subject,
                "canonical native FFI call cannot carry a variant ABI receipt",
            ));
        }
        let instruction = crate::core::mir::MirInstructionId::new(subject.to_owned())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let receipt = self
            .program
            .ffi_calls()
            .get(&instruction)
            .ok_or_else(|| NativeMirError::new(subject, "extern call has no FFI receipt"))?;
        let function = self
            .ffi_functions
            .get(&receipt.symbol)
            .copied()
            .ok_or_else(|| {
                NativeMirError::new(
                    subject,
                    format!(
                        "FFI symbol '{}' is absent from native declarations",
                        receipt.symbol
                    ),
                )
            })?;
        if let Some(condition) = receipt
            .requires
            .as_ref()
            .filter(|_| self.generator.verify_ffi)
        {
            self.emit_ffi_requires(condition, subject)?;
        }
        self.emit_ffi_call_target(
            result,
            function,
            arguments,
            &receipt.parameter_conversions,
            receipt.result_conversion.as_ref(),
            subject,
        )?;
        if let Some(condition) = receipt
            .ensures
            .as_ref()
            .filter(|_| self.generator.verify_ffi)
        {
            self.emit_ffi_ensures(condition, subject)?;
        }
        Ok(())
    }

    pub(super) fn emit_flow_transition(
        &mut self,
        result: &MirValueId,
        transition: &crate::core::NodeId,
        arguments: &[MirValueId],
        effect_receipt: Option<&crate::core::mir::types::MirFlowEffectReceipt>,
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
        let recoverable = contract.effect.is_recoverable();
        if (!recoverable
            && !matches!(
                contract.effect,
                crate::core::mir::MirTransitionEffect::SilentLocal
                    | crate::core::mir::MirTransitionEffect::Boundary
            ))
            || contract.targets.len() != 1
            || (!recoverable && contract.failure.is_some())
            || contract.is_fallback
            || contract.is_ffi_pinned
            || (recoverable && contract.failure.is_none())
            || (!recoverable && contract.targets.first() != Some(&contract.result))
        {
            return Err(NativeMirError::new(
                subject,
                "FlowTransition is outside the silent-local native contract",
            ));
        }
        let argument_types = arguments
            .iter()
            .map(|argument| self.value_type(argument, subject))
            .collect::<Result<Vec<_>, _>>()?;
        let result_ty = self.value_type(result, subject)?;
        crate::core::mir::validate_flow_effect_receipt(
            transition,
            contract,
            &argument_types,
            &result_ty,
            effect_receipt,
        )
        .map_err(|message| NativeMirError::new(subject, message))?;
        let function = *self.functions.get(&contract.owner).ok_or_else(|| {
            NativeMirError::new(
                subject,
                format!(
                    "flow transition '{}' is absent from native declarations",
                    transition.0
                ),
            )
        })?;
        if recoverable {
            if function.get_type().get_return_type().is_none() {
                return Err(NativeMirError::new(
                    subject,
                    "recoverable FlowTransition target has no Result return ABI",
                ));
            }
        }
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

    fn emit_ffi_call_target(
        &mut self,
        result: Option<&MirValueId>,
        function: FunctionValue<'ctx>,
        arguments: &[MirValueId],
        parameter_conversions: &[crate::core::mir::MirFfiAbiConversion],
        result_conversion: Option<&crate::core::mir::MirFfiAbiConversion>,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        // Native admission has already proven the receipt conversion belongs
        // to the scalar FFI island.  The emitter keeps only physical checks
        // that depend on LLVM declarations and materialized values.
        let function_type = function.get_type();
        let parameter_types = function_type.get_param_types();
        if parameter_types.len() != parameter_conversions.len() {
            return Err(NativeMirError::new(
                subject,
                "FFI native declaration parameter count disagrees with conversion receipt",
            ));
        }
        for (index, (parameter_type, conversion)) in parameter_types
            .iter()
            .zip(parameter_conversions)
            .enumerate()
        {
            if !native_ffi_metadata_type_matches(*parameter_type, conversion.to) {
                return Err(NativeMirError::new(
                    subject,
                    format!(
                        "FFI parameter {index} conversion target ABI {:?} disagrees with native declaration",
                        conversion.to
                    ),
                ));
            }
        }
        let mut values = Vec::with_capacity(arguments.len());
        for (index, (argument, conversion)) in
            arguments.iter().zip(parameter_conversions).enumerate()
        {
            let value = self.value(argument, subject)?;
            let actual_type = self.value_desc(argument, subject)?;
            if actual_type.abi != conversion.from {
                return Err(NativeMirError::new(
                    subject,
                    format!(
                        "FFI parameter {index} ABI conversion receipt starts at {:?}, MIR value is {:?}",
                        conversion.from, actual_type.abi
                    ),
                ));
            }
            let value = self.coerce_ffi_value(
                value,
                actual_type.abi,
                conversion.to,
                subject,
                &format!("ffi_arg_{index}"),
            )?;
            values.push(value.into());
        }
        // Check the result-side receipt before building the foreign call.  A
        // malformed endpoint must never be discovered only after the call
        // has already produced an observable side effect.
        let result_conversion = if let Some(result) = result {
            let actual_type = self.value_desc(result, subject)?;
            if actual_type.abi == MirAbiClass::Unit {
                match result_conversion {
                    Some(conversion)
                        if conversion.from == MirAbiClass::Unit
                            && conversion.to == MirAbiClass::Unit => {}
                    Some(_) => {
                        return Err(NativeMirError::new(
                            subject,
                            "unit MIR result has a non-unit FFI conversion receipt",
                        ));
                    }
                    None => {
                        return Err(NativeMirError::new(
                            subject,
                            "unit MIR result has no FFI conversion receipt",
                        ));
                    }
                }
                if function_type.get_return_type().is_some() {
                    return Err(NativeMirError::new(
                        subject,
                        "unit MIR result has a non-void native declaration",
                    ));
                }
                None
            } else {
                let conversion = result_conversion.ok_or_else(|| {
                    NativeMirError::new(subject, "non-unit FFI result has no conversion receipt")
                })?;
                let Some(native_return_type) = function_type.get_return_type() else {
                    return Err(NativeMirError::new(
                        subject,
                        "non-unit FFI result has a void native declaration",
                    ));
                };
                if !native_ffi_basic_type_matches(&native_return_type, conversion.from) {
                    return Err(NativeMirError::new(
                        subject,
                        format!(
                            "FFI result conversion source ABI {:?} disagrees with native declaration",
                            conversion.from
                        ),
                    ));
                }
                if conversion.to != actual_type.abi {
                    return Err(NativeMirError::new(
                        subject,
                        format!(
                            "FFI result ABI conversion receipt ends at {:?}, MIR result is {:?}",
                            conversion.to, actual_type.abi
                        ),
                    ));
                }
                Some((result, conversion))
            }
        } else {
            if result_conversion.is_some() {
                return Err(NativeMirError::new(
                    subject,
                    "unit MIR call has an FFI result conversion receipt",
                ));
            }
            if function_type.get_return_type().is_some() {
                return Err(NativeMirError::new(
                    subject,
                    "unit MIR call has a non-void native declaration",
                ));
            }
            None
        };
        let call = self
            .generator
            .builder
            .build_call(function, &values, "mir_ffi_call")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let Some((result, conversion)) = result_conversion else {
            return Ok(());
        };
        let value = call_try_basic_value(&call)
            .ok_or_else(|| NativeMirError::new(subject, "non-unit FFI call returned void"))?;
        let value =
            self.coerce_ffi_value(value, conversion.from, conversion.to, subject, "ffi_result")?;
        self.values.insert(result.clone(), value);
        Ok(())
    }

    fn coerce_ffi_value(
        &mut self,
        value: BasicValueEnum<'ctx>,
        from: MirAbiClass,
        to: MirAbiClass,
        subject: &str,
        name: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        if !native_ffi_value_matches(&value, from) {
            return Err(NativeMirError::new(
                subject,
                format!(
                    "FFI conversion source ABI {:?} disagrees with the native LLVM value",
                    from
                ),
            ));
        }
        if from == to {
            return Ok(value);
        }
        match (from, to, value) {
            (
                MirAbiClass::Integer {
                    bits: from_bits,
                    signed: true,
                },
                MirAbiClass::Integer {
                    bits: to_bits,
                    signed: true,
                },
                value,
            ) if from_bits < to_bits => self
                .generator
                .builder
                .build_int_s_extend(
                    value.into_int_value(),
                    match to_bits {
                        32 => self.generator.context.i32_type(),
                        64 => self.generator.context.i64_type(),
                        _ => {
                            return Err(NativeMirError::new(
                                subject,
                                format!("FFI integer width {to_bits} is unsupported"),
                            ))
                        }
                    },
                    name,
                )
                .map(BasicValueEnum::from)
                .map_err(|error| NativeMirError::new(subject, error.to_string())),
            (
                MirAbiClass::Integer {
                    bits: from_bits,
                    signed: true,
                },
                MirAbiClass::Integer {
                    bits: to_bits,
                    signed: true,
                },
                value,
            ) if from_bits > to_bits => {
                let value = value.into_int_value();
                let (minimum, maximum) = match to_bits {
                    32 => (i32::MIN as i64, i32::MAX as i64),
                    64 => (i64::MIN, i64::MAX),
                    _ => {
                        return Err(NativeMirError::new(
                            subject,
                            format!("FFI integer width {to_bits} is unsupported"),
                        ))
                    }
                };
                let i64_ty = self.generator.context.i64_type();
                let minimum = self
                    .generator
                    .builder
                    .build_int_compare(
                        IntPredicate::SGE,
                        value,
                        i64_ty.const_int(minimum as u64, true),
                        "ffi_result_min",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                let maximum = self
                    .generator
                    .builder
                    .build_int_compare(
                        IntPredicate::SLE,
                        value,
                        i64_ty.const_int(maximum as u64, true),
                        "ffi_result_max",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                let valid = self
                    .generator
                    .builder
                    .build_and(minimum, maximum, "ffi_result_in_range")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                self.emit_ffi_guard(
                    valid,
                    "[E0802] FFI integer result conversion out of range",
                    subject,
                )?;
                self.generator
                    .builder
                    .build_int_truncate(
                        value,
                        match to_bits {
                            32 => self.generator.context.i32_type(),
                            64 => self.generator.context.i64_type(),
                            _ => unreachable!("integer width checked above"),
                        },
                        name,
                    )
                    .map(BasicValueEnum::from)
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))
            }
            (MirAbiClass::Integer { signed: true, .. }, MirAbiClass::Float { bits: 64 }, value) => {
                self.generator
                    .builder
                    .build_signed_int_to_float(
                        value.into_int_value(),
                        self.generator.context.f64_type(),
                        name,
                    )
                    .map(BasicValueEnum::from)
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))
            }
            (
                MirAbiClass::Float { bits: 64 },
                MirAbiClass::Integer {
                    bits: 32 | 64,
                    signed: true,
                },
                value,
            ) => {
                let value = value.into_float_value();
                let (lower, upper) = match to {
                    MirAbiClass::Integer { bits: 32, .. } => (i32::MIN as f64, 2_147_483_648.0),
                    MirAbiClass::Integer { bits: 64, .. } => {
                        (-9_223_372_036_854_775_808.0, 9_223_372_036_854_775_808.0)
                    }
                    _ => unreachable!("matched integer result ABI above"),
                };
                let lower = self
                    .generator
                    .builder
                    .build_float_compare(
                        FloatPredicate::OGE,
                        value,
                        self.generator.context.f64_type().const_float(lower),
                        "ffi_result_lower",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                let upper = self
                    .generator
                    .builder
                    .build_float_compare(
                        FloatPredicate::OLT,
                        value,
                        self.generator.context.f64_type().const_float(upper),
                        "ffi_result_upper",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                let valid = self
                    .generator
                    .builder
                    .build_and(lower, upper, "ffi_result_in_range")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                self.emit_ffi_guard(
                    valid,
                    "[E0802] FFI integer result conversion out of range",
                    subject,
                )?;
                self.generator
                    .builder
                    .build_float_to_signed_int(
                        value,
                        match to {
                            MirAbiClass::Integer { bits: 32, .. } => {
                                self.generator.context.i32_type()
                            }
                            MirAbiClass::Integer { bits: 64, .. } => {
                                self.generator.context.i64_type()
                            }
                            _ => unreachable!("matched integer result ABI above"),
                        },
                        name,
                    )
                    .map(BasicValueEnum::from)
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))
            }
            _ => Err(NativeMirError::new(
                subject,
                format!("FFI ABI conversion from {from:?} to {to:?} is unsupported"),
            )),
        }
    }
}

fn native_ffi_value_matches<'ctx>(value: &BasicValueEnum<'ctx>, abi: MirAbiClass) -> bool {
    match (abi, value) {
        (
            MirAbiClass::Integer {
                bits: 32,
                signed: true,
            },
            BasicValueEnum::IntValue(value),
        ) => value.get_type().get_bit_width() == 32,
        (
            MirAbiClass::Integer {
                bits: 64,
                signed: true,
            },
            BasicValueEnum::IntValue(value),
        ) => value.get_type().get_bit_width() == 64,
        (MirAbiClass::Bool, BasicValueEnum::IntValue(value)) => {
            value.get_type().get_bit_width() == 1
        }
        (MirAbiClass::Float { bits: 64 }, BasicValueEnum::FloatValue(value)) => {
            value.get_type().get_bit_width() == 64
        }
        _ => false,
    }
}

fn native_ffi_metadata_type_matches<'ctx>(
    value: BasicMetadataTypeEnum<'ctx>,
    abi: MirAbiClass,
) -> bool {
    match (abi, value) {
        (
            MirAbiClass::Integer {
                bits: 32,
                signed: true,
            },
            BasicMetadataTypeEnum::IntType(value),
        ) => value.get_bit_width() == 32,
        (
            MirAbiClass::Integer {
                bits: 64,
                signed: true,
            },
            BasicMetadataTypeEnum::IntType(value),
        ) => value.get_bit_width() == 64,
        (MirAbiClass::Bool, BasicMetadataTypeEnum::IntType(value)) => value.get_bit_width() == 1,
        (MirAbiClass::Float { bits: 64 }, BasicMetadataTypeEnum::FloatType(value)) => {
            value.get_bit_width() == 64
        }
        _ => false,
    }
}

fn native_ffi_basic_type_matches<'ctx>(value: &BasicTypeEnum<'ctx>, abi: MirAbiClass) -> bool {
    match (abi, value) {
        (
            MirAbiClass::Integer {
                bits: 32,
                signed: true,
            },
            BasicTypeEnum::IntType(value),
        ) => value.get_bit_width() == 32,
        (
            MirAbiClass::Integer {
                bits: 64,
                signed: true,
            },
            BasicTypeEnum::IntType(value),
        ) => value.get_bit_width() == 64,
        (MirAbiClass::Bool, BasicTypeEnum::IntType(value)) => value.get_bit_width() == 1,
        (MirAbiClass::Float { bits: 64 }, BasicTypeEnum::FloatType(value)) => {
            value.get_bit_width() == 64
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        native_ffi_basic_type_matches, native_ffi_metadata_type_matches, native_ffi_value_matches,
    };
    use crate::core::mir::types::MirAbiClass;
    use inkwell::context::Context;
    use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
    use inkwell::values::BasicValueEnum;

    #[test]
    fn native_ffi_value_shape_matches_receipt_endpoint() {
        let context = Context::create();
        let i32_value: BasicValueEnum<'_> = context.i32_type().const_zero().into();
        let i64_value: BasicValueEnum<'_> = context.i64_type().const_zero().into();
        let bool_value: BasicValueEnum<'_> = context.bool_type().const_zero().into();
        let f64_value: BasicValueEnum<'_> = context.f64_type().const_zero().into();

        assert!(native_ffi_value_matches(
            &i32_value,
            MirAbiClass::Integer {
                bits: 32,
                signed: true,
            }
        ));
        assert!(!native_ffi_value_matches(
            &i32_value,
            MirAbiClass::Integer {
                bits: 64,
                signed: true,
            }
        ));
        assert!(native_ffi_value_matches(
            &i64_value,
            MirAbiClass::Integer {
                bits: 64,
                signed: true,
            }
        ));
        assert!(native_ffi_value_matches(&bool_value, MirAbiClass::Bool));
        assert!(native_ffi_value_matches(
            &f64_value,
            MirAbiClass::Float { bits: 64 }
        ));
        assert!(!native_ffi_value_matches(&f64_value, MirAbiClass::Bool));
    }

    #[test]
    fn native_ffi_declaration_shape_matches_receipt_endpoint() {
        let context = Context::create();
        let i32_type: BasicMetadataTypeEnum<'_> = context.i32_type().into();
        let i64_type: BasicMetadataTypeEnum<'_> = context.i64_type().into();
        let bool_type: BasicMetadataTypeEnum<'_> = context.bool_type().into();
        let f64_type: BasicMetadataTypeEnum<'_> = context.f64_type().into();
        assert!(native_ffi_metadata_type_matches(
            i32_type,
            MirAbiClass::Integer {
                bits: 32,
                signed: true,
            }
        ));
        assert!(native_ffi_metadata_type_matches(
            bool_type,
            MirAbiClass::Bool
        ));
        assert!(native_ffi_metadata_type_matches(
            f64_type,
            MirAbiClass::Float { bits: 64 }
        ));
        assert!(!native_ffi_metadata_type_matches(
            i32_type,
            MirAbiClass::Integer {
                bits: 64,
                signed: true,
            }
        ));
        assert!(!native_ffi_metadata_type_matches(
            i64_type,
            MirAbiClass::Bool
        ));

        let i32_basic: BasicTypeEnum<'_> = context.i32_type().into();
        assert!(native_ffi_basic_type_matches(
            &i32_basic,
            MirAbiClass::Integer {
                bits: 32,
                signed: true,
            }
        ));
        assert!(!native_ffi_basic_type_matches(
            &i32_basic,
            MirAbiClass::Float { bits: 64 }
        ));
    }
}
