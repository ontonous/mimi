//! Canonical MIR call lowering for the native consumer.

use super::*;

impl<'a, 'ctx> NativeMirFunctionEmitter<'a, 'ctx> {
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
