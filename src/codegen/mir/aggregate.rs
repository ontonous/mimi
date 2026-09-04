//! Aggregate construction and projection for the admitted native MIR slice.

use super::*;

impl<'a, 'ctx> NativeMirFunctionEmitter<'a, 'ctx> {
    pub(super) fn emit_construct(
        &mut self,
        result: &MirValueId,
        kind: &MirAggregateKind,
        fields: &[MirValueId],
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let result_ty = self.value_type(result, subject)?;
        if matches!(kind, MirAggregateKind::Tuple) {
            let elements = match &self
                .program
                .type_catalog()
                .get(&result_ty)
                .ok_or_else(|| NativeMirError::new(subject, "tuple result TypeDesc is absent"))?
                .layout
            {
                MirLayout::Tuple(elements) => elements.clone(),
                _ => {
                    return Err(NativeMirError::new(
                        subject,
                        "tuple construction result has no canonical tuple layout",
                    ))
                }
            };
            if elements.len() != fields.len() {
                return Err(NativeMirError::new(
                    subject,
                    "tuple construction does not match its TypeDesc layout",
                ));
            }
            let struct_ty = native_basic_type(
                self.generator.context,
                self.program.type_catalog(),
                &result_ty,
            )?
            .into_struct_type();
            let mut aggregate = struct_ty.get_undef();
            for (index, source) in fields.iter().enumerate() {
                let source_ty = self.value_type(source, subject)?;
                if source_ty != elements[index] {
                    return Err(NativeMirError::new(
                        subject,
                        format!(
                            "tuple field {} type '{}' disagrees with TypeDesc type '{}'",
                            index,
                            source_ty.as_str(),
                            elements[index].as_str()
                        ),
                    ));
                }
                aggregate = self
                    .generator
                    .builder
                    .build_insert_value(
                        aggregate,
                        self.value(source, subject)?,
                        index as u32,
                        "mir_tuple_insert",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                    .into_struct_value();
            }
            return Ok(aggregate.into());
        }
        let MirAggregateKind::Record {
            nominal,
            fields: field_ids,
        } = kind
        else {
            return Err(NativeMirError::new(
                subject,
                "aggregate construction reached the native record emitter",
            ));
        };
        let descriptor = self
            .program
            .type_catalog()
            .get(&result_ty)
            .ok_or_else(|| NativeMirError::new(subject, "record result TypeDesc is absent"))?;
        let MirLayout::Record {
            nominal: expected_nominal,
            fields: layout_fields,
        } = &descriptor.layout
        else {
            return Err(NativeMirError::new(
                subject,
                "record construction result has no canonical record layout",
            ));
        };
        if nominal != expected_nominal || field_ids.len() != fields.len() {
            return Err(NativeMirError::new(
                subject,
                "record construction does not match its TypeDesc layout",
            ));
        }
        let struct_ty = native_basic_type(
            self.generator.context,
            self.program.type_catalog(),
            &result_ty,
        )?
        .into_struct_type();
        let mut aggregate = struct_ty.get_undef();
        for (field_id, source) in field_ids.iter().zip(fields) {
            let index = layout_fields
                .iter()
                .position(|field| field.id == *field_id)
                .ok_or_else(|| {
                    NativeMirError::new(
                        subject,
                        format!("record field '{}' is absent from TypeDesc", field_id.0),
                    )
                })?;
            let value = self.value(source, subject)?;
            aggregate = self
                .generator
                .builder
                .build_insert_value(aggregate, value, index as u32, "mir_record_insert")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                .into_struct_value();
        }
        Ok(aggregate.into())
    }

    pub(super) fn emit_update_record(
        &mut self,
        result: &MirValueId,
        base: &MirValueId,
        kind: &MirAggregateKind,
        fields: &[MirValueId],
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let MirAggregateKind::Record {
            nominal,
            fields: field_ids,
        } = kind
        else {
            return Err(NativeMirError::new(
                subject,
                "record update reached the flat record emitter with a non-record kind",
            ));
        };
        let result_ty = self.value_type(result, subject)?;
        let base_ty = self.value_type(base, subject)?;
        if result_ty != base_ty {
            return Err(NativeMirError::new(
                subject,
                "record update base and result types disagree",
            ));
        }
        let descriptor = self
            .program
            .type_catalog()
            .get(&result_ty)
            .ok_or_else(|| NativeMirError::new(subject, "record update TypeDesc is absent"))?;
        let MirLayout::Record {
            nominal: expected_nominal,
            fields: layout_fields,
        } = &descriptor.layout
        else {
            return Err(NativeMirError::new(
                subject,
                "record update has no canonical record layout",
            ));
        };
        if nominal != expected_nominal || field_ids.len() != fields.len() {
            return Err(NativeMirError::new(
                subject,
                "record update does not match its TypeDesc layout",
            ));
        }
        let mut aggregate = self.value(base, subject)?.into_struct_value();
        for (field_id, source) in field_ids.iter().zip(fields) {
            let field = layout_fields
                .iter()
                .find(|field| field.id == *field_id)
                .ok_or_else(|| {
                    NativeMirError::new(
                        subject,
                        format!(
                            "record update field '{}' is absent from TypeDesc",
                            field_id.0
                        ),
                    )
                })?;
            let source_ty = self.value_type(source, subject)?;
            if source_ty != field.ty {
                return Err(NativeMirError::new(
                    subject,
                    format!(
                        "record update field '{}' type '{}' disagrees with layout type '{}'",
                        field_id.0,
                        source_ty.as_str(),
                        field.ty.as_str()
                    ),
                ));
            }
            let index = layout_fields
                .iter()
                .position(|candidate| candidate.id == *field_id)
                .expect("record update field was found above");
            aggregate = self
                .generator
                .builder
                .build_insert_value(
                    aggregate,
                    self.value(source, subject)?,
                    index as u32,
                    "mir_record_update",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                .into_struct_value();
        }
        Ok(aggregate.into())
    }

    pub(super) fn emit_construct_variant(
        &mut self,
        result: &MirValueId,
        nominal: &crate::core::NominalTypeId,
        variant: &crate::core::NodeId,
        fields: &[(crate::core::NodeId, MirValueId)],
        subject: &str,
        moving: bool,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let result_ty = self.value_type(result, subject)?;
        let field_ids = fields
            .iter()
            .map(|(field, _)| field.clone())
            .collect::<Vec<_>>();
        let field_types = fields
            .iter()
            .map(|(_, value)| self.value_type(value, subject))
            .collect::<Result<Vec<_>, _>>()?;
        let variant_desc = self
            .program
            .type_catalog()
            .validated_variant_construct(&result_ty, nominal, variant, &field_ids, &field_types)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let (variant_abi, _) = native_variant_abi(self.program.type_catalog(), &result_ty, moving)?;
        let struct_ty = native_basic_type(
            self.generator.context,
            self.program.type_catalog(),
            &result_ty,
        )?
        .into_struct_type();
        let mut aggregate = struct_ty.get_undef();
        for (index, payload_ty) in variant_abi.payload_types.iter().enumerate() {
            let zero = native_basic_type(
                self.generator.context,
                self.program.type_catalog(),
                payload_ty,
            )?
            .const_zero();
            aggregate = self
                .generator
                .builder
                .build_insert_value(
                    aggregate,
                    zero,
                    index as u32 + 1,
                    "mir_variant_zero_payload",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                .into_struct_value();
        }
        aggregate = self
            .generator
            .builder
            .build_insert_value(
                aggregate,
                self.generator
                    .context
                    .i8_type()
                    .const_int(u64::from(variant_desc.discriminant), false),
                variant_abi.tag_field,
                "mir_variant_tag",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_struct_value();
        if let Some(payload_slot) = variant_abi.payload_slot(variant) {
            let (_, value) = fields
                .iter()
                .find(|(field, _)| field == &payload_slot.field)
                .ok_or_else(|| NativeMirError::new(subject, "variant payload value is absent"))?;
            let value_ty = self.value_type(value, subject)?;
            if value_ty != payload_slot.ty {
                return Err(NativeMirError::new(
                    subject,
                    "variant payload value disagrees with the native ABI receipt",
                ));
            }
            aggregate = self
                .generator
                .builder
                .build_insert_value(
                    aggregate,
                    self.value(value, subject)?,
                    payload_slot.physical_field,
                    "mir_variant_payload",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                .into_struct_value();
        }
        Ok(aggregate.into())
    }

    pub(super) fn emit_project(
        &mut self,
        result: &MirValueId,
        base: &MirValueId,
        projection: &MirProjection,
        list_index_contract: Option<&crate::core::mir::types::MirListIndexProjectionContract>,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        if matches!(projection, MirProjection::Dereference) {
            let base_ty = self.value_type(base, subject)?;
            let result_ty = self.value_type(result, subject)?;
            self.program
                .type_catalog()
                .validate_dereference(&base_ty, &result_ty)
                .map_err(|message| NativeMirError::new(subject, message))?;
            let result_llvm = native_basic_type(
                self.generator.context,
                self.program.type_catalog(),
                &result_ty,
            )?;
            let pointer = self.value(base, subject)?.into_pointer_value();
            return self
                .generator
                .builder
                .build_load(result_llvm, pointer, "mir_dereference")
                .map_err(|error| NativeMirError::new(subject, error.to_string()));
        }
        if let MirProjection::Index(index) = projection {
            let base_ty = self.value_type(base, subject)?;
            let result_ty = self.value_type(result, subject)?;
            let catalog = self.program.type_catalog();
            let receipt = list_index_contract.ok_or_else(|| {
                NativeMirError::new(subject, "List index projection has no canonical receipt")
            })?;
            let index_ty = self.value_type(index, subject)?;
            if receipt.list_ty != base_ty
                || receipt.element_ty != result_ty
                || receipt.result_ty != result_ty
                || receipt.index_ty != index_ty
            {
                return Err(NativeMirError::new(
                    subject,
                    "List index projection receipt disagrees with MIR value types",
                ));
            }
            catalog.get(&receipt.index_ty).ok_or_else(|| {
                NativeMirError::new(subject, "List index receipt TypeDesc is absent")
            })?;
            catalog.get(&receipt.element_ty).ok_or_else(|| {
                NativeMirError::new(subject, "List element receipt TypeDesc is absent")
            })?;
            catalog.get(&receipt.list_ty).ok_or_else(|| {
                NativeMirError::new(subject, "List source receipt TypeDesc is absent")
            })?;
            catalog
                .validate_list_index_projection_receipt(&base_ty, &index_ty, &result_ty, receipt)
                .map_err(|message| NativeMirError::new(subject, message))?;
            let kind = native_list_kind(catalog, &base_ty)?;
            let index_desc = catalog
                .get(&index_ty)
                .ok_or_else(|| NativeMirError::new(subject, "List index TypeDesc is absent"))?;
            let index_value = self.value(index, subject)?.into_int_value();
            let index_value = match index_desc.abi {
                MirAbiClass::Integer {
                    bits: 64,
                    signed: true,
                } => index_value,
                MirAbiClass::Integer {
                    bits: 32,
                    signed: true,
                } => self
                    .generator
                    .builder
                    .build_int_s_extend(
                        index_value,
                        self.generator.context.i64_type(),
                        "mir_list_index_i32",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
                _ => {
                    return Err(NativeMirError::new(
                        subject,
                        "List index is outside signed integer native storage",
                    ))
                }
            };
            let kind_value = self
                .generator
                .context
                .i8_type()
                .const_int(kind as u64, false);
            let get_fn = self
                .generator
                .get_runtime_fn("mimi_mir_list_get_scalar")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let raw = call_try_basic_value(
                &self
                    .generator
                    .builder
                    .build_call(
                        get_fn,
                        &[
                            BasicMetadataValueEnum::from(
                                self.value(base, subject)?.into_pointer_value(),
                            ),
                            BasicMetadataValueEnum::from(kind_value),
                            BasicMetadataValueEnum::from(index_value),
                        ],
                        "mir_list_get",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
            )
            .ok_or_else(|| NativeMirError::new(subject, "List projection returned void"))?
            .into_int_value();
            let result_desc = catalog
                .get(&result_ty)
                .ok_or_else(|| NativeMirError::new(subject, "List result TypeDesc is absent"))?;
            return match result_desc.abi {
                MirAbiClass::Integer {
                    bits: 64,
                    signed: true,
                } => Ok(raw.into()),
                MirAbiClass::Integer {
                    bits: 32,
                    signed: true,
                } => self
                    .generator
                    .builder
                    .build_int_truncate(
                        raw,
                        self.generator.context.i32_type(),
                        "mir_list_i32_result",
                    )
                    .map(BasicValueEnum::from)
                    .map_err(|error| NativeMirError::new(subject, error.to_string())),
                MirAbiClass::Bool => self
                    .generator
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        raw,
                        self.generator.context.i64_type().const_zero(),
                        "mir_list_bool_result",
                    )
                    .map(BasicValueEnum::from)
                    .map_err(|error| NativeMirError::new(subject, error.to_string())),
                _ => Err(NativeMirError::new(
                    subject,
                    "List projection result is outside scalar native storage",
                )),
            };
        }
        if let MirProjection::Tuple(field_index) = projection {
            let base_ty = self.value_type(base, subject)?;
            let result_ty = self.value_type(result, subject)?;
            let receipt = self
                .program
                .type_catalog()
                .validated_tuple_field_projection_contract(&base_ty, *field_index, &result_ty)
                .map_err(|message| NativeMirError::new(subject, message))?;
            let aggregate = self.value(base, subject)?.into_struct_value();
            return self
                .generator
                .builder
                .build_extract_value(aggregate, receipt.field_index as u32, "mir_tuple_project")
                .map_err(|error| NativeMirError::new(subject, error.to_string()));
        }
        let MirProjection::Field(field_id) = projection else {
            return Err(NativeMirError::new(
                subject,
                "projection shape is outside the native aggregate adapter",
            ));
        };
        let base_ty = self.value_type(base, subject)?;
        let result_ty = self.value_type(result, subject)?;
        let receipt = self
            .program
            .type_catalog()
            .validated_record_field_projection_contract(&base_ty, field_id, &result_ty)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let index = receipt.field_index;
        let aggregate = self.value(base, subject)?.into_struct_value();
        self.generator
            .builder
            .build_extract_value(aggregate, index as u32, "mir_record_project")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))
    }

    /// Consume a concrete record and transfer its one owned String field.
    /// The canonical validator proves that every sibling is Copy, so the
    /// extracted `{ptr, len}` value is a move boundary rather than a clone;
    /// the source record must not be used again along any valid MIR path.
    pub(super) fn emit_move_project(
        &mut self,
        result: &MirValueId,
        base: &MirValueId,
        projection: &MirProjection,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let base_ty = self.value_type(base, subject)?;
        let result_ty = self.value_type(result, subject)?;
        self.program
            .type_catalog()
            .validate_move_projection(&base_ty, &result_ty, projection)
            .map_err(|message| NativeMirError::new(subject, message))?;
        validate_native_non_copy_record_type(self.program.type_catalog(), &base_ty)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let MirProjection::Field(field_id) = projection else {
            return Err(NativeMirError::new(
                subject,
                "native MoveProject requires a direct record field",
            ));
        };
        let receipt = self
            .program
            .type_catalog()
            .validated_record_field_projection_contract(&base_ty, field_id, &result_ty)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let index = receipt.field_index;
        let aggregate = self.value(base, subject)?.into_struct_value();
        self.generator
            .builder
            .build_extract_value(aggregate, index as u32, "mir_record_move_project")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))
    }

    /// Consume a non-Copy record, explicitly dropping every residual field
    /// described by the TypeDesc receipt, then transfer the selected field.
    /// The aggregate itself is an immutable SSA value; ownership is expressed
    /// by the MIR ledger and the child drop glue, never by re-reading LLVM
    /// layout or silently cloning a sibling.
    pub(super) fn emit_move_project_drop(
        &mut self,
        result: &MirValueId,
        base: &MirValueId,
        projection: &MirProjection,
        contract: Option<&crate::core::mir::types::MirRecordMoveProjectionDropContract>,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let base_ty = self.value_type(base, subject)?;
        let result_ty = self.value_type(result, subject)?;
        let receipt = contract.ok_or_else(|| {
            NativeMirError::new(
                subject,
                "record move/drop projection has no canonical residual receipt",
            )
        })?;
        self.program
            .type_catalog()
            .validate_record_move_projection_drop_receipt(&base_ty, &result_ty, receipt)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let MirProjection::Field(field_id) = projection else {
            return Err(NativeMirError::new(
                subject,
                "native MoveProjectDrop requires a direct record field",
            ));
        };
        if field_id != &receipt.projection.field {
            return Err(NativeMirError::new(
                subject,
                "native MoveProjectDrop field disagrees with its receipt",
            ));
        }
        validate_native_non_copy_record_type(self.program.type_catalog(), &base_ty)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let aggregate = self.value(base, subject)?.into_struct_value();
        for residual in &receipt.residual {
            let child = self
                .generator
                .builder
                .build_extract_value(
                    aggregate,
                    residual.index as u32,
                    "mir_record_move_drop_residual",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            self.emit_drop_value(child, &residual.ty, subject)?;
        }
        self.generator
            .builder
            .build_extract_value(
                aggregate,
                receipt.projection.field_index as u32,
                "mir_record_move_drop_project",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))
    }

    /// Read a Copy payload from a direct variant projection.  The canonical
    /// receipt supplies the discriminant, physical tag identity, and trap
    /// classification; the native emitter only materializes the checked
    /// `{tag, payload}` ABI and never infers a variant from the LLVM struct.
    pub(super) fn emit_variant_project(
        &mut self,
        result: &MirValueId,
        base: &MirValueId,
        contract: Option<&crate::core::mir::types::MirVariantProjectionTrapContract>,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let base_ty = self.value_type(base, subject)?;
        let result_ty = self.value_type(result, subject)?;
        let receipt = contract.ok_or_else(|| {
            NativeMirError::new(
                subject,
                "direct variant projection has no canonical trap receipt",
            )
        })?;
        self.program
            .type_catalog()
            .validate_variant_projection_trap_receipt(&base_ty, &result_ty, receipt)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let (variant_abi, _) = native_variant_abi(self.program.type_catalog(), &base_ty, false)?;
        let payload_slot = variant_abi
            .payload_slot(&receipt.projection.variant)
            .ok_or_else(|| {
                NativeMirError::new(
                    subject,
                    "direct variant projection has no native payload slot",
                )
            })?;
        if payload_slot.field != receipt.projection.field
            || payload_slot.ty != result_ty
            || receipt.projection.field_index != 0
        {
            return Err(NativeMirError::new(
                subject,
                "direct variant projection receipt disagrees with native payload ABI",
            ));
        }

        let aggregate = self.value(base, subject)?.into_struct_value();
        let tag = self
            .generator
            .builder
            .build_extract_value(aggregate, variant_abi.tag_field, "mir_variant_project_tag")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let active = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                tag,
                self.generator
                    .context
                    .i8_type()
                    .const_int(u64::from(receipt.discriminant), false),
                "mir_variant_project_active",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let trap = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_variant_project_trap");
        let ok = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_variant_project_ok");
        self.generator
            .builder
            .build_conditional_branch(active, ok, trap)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(trap);
        self.emit_abort_with_message(
            &format!(
                "[{}] canonical MIR direct variant projection expected active variant '{}'",
                receipt.trap_code, receipt.variant_name
            ),
            subject,
        )?;
        self.generator.builder.position_at_end(ok);
        self.generator
            .builder
            .build_extract_value(
                aggregate,
                payload_slot.physical_field,
                "mir_variant_project_payload",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))
    }

    /// Read the canonical `Ok` payload or select the explicit fallback value
    /// for `Err`. The TypeDesc receipt proves both variant identities and the
    /// scalar ABI before native LLVM sees the aggregate.
    pub(super) fn emit_variant_project_or(
        &mut self,
        result: &MirValueId,
        base: &MirValueId,
        fallback: &MirValueId,
        contract: Option<&crate::core::mir::types::MirVariantProjectionFallbackContract>,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let base_ty = self.value_type(base, subject)?;
        let result_ty = self.value_type(result, subject)?;
        let fallback_ty = self.value_type(fallback, subject)?;
        let receipt = contract.ok_or_else(|| {
            NativeMirError::new(
                subject,
                "variant projection fallback has no canonical receipt",
            )
        })?;
        self.program
            .type_catalog()
            .validate_variant_projection_fallback_receipt(
                &base_ty,
                &result_ty,
                &fallback_ty,
                receipt,
            )
            .map_err(|message| NativeMirError::new(subject, message))?;
        let (variant_abi, _) = native_variant_abi(self.program.type_catalog(), &base_ty, false)?;
        let payload_slot = variant_abi
            .payload_slot(&receipt.projection.variant)
            .ok_or_else(|| {
                NativeMirError::new(
                    subject,
                    "variant projection fallback has no native Ok payload slot",
                )
            })?;
        if payload_slot.field != receipt.projection.field
            || payload_slot.ty != result_ty
            || receipt.projection.field_index != 0
        {
            return Err(NativeMirError::new(
                subject,
                "variant projection fallback receipt disagrees with native payload ABI",
            ));
        }
        // Result::Err carries a second payload slot, while Option::None is a
        // zero-field alternate.  The checker-owned receipt proves which case
        // applies; never require a physical payload slot merely to select an
        // explicit fallback operand.
        if let Some(fallback_slot) = variant_abi.payload_slot(&receipt.fallback_variant) {
            if fallback_slot.ty != fallback_ty {
                return Err(NativeMirError::new(
                    subject,
                    "variant projection fallback receipt disagrees with native fallback ABI",
                ));
            }
        }
        let aggregate = self.value(base, subject)?.into_struct_value();
        let tag = self
            .generator
            .builder
            .build_extract_value(
                aggregate,
                variant_abi.tag_field,
                "mir_variant_project_or_tag",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let active_ok = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                tag,
                self.generator
                    .context
                    .i8_type()
                    .const_int(u64::from(receipt.discriminant), false),
                "mir_variant_project_or_is_ok",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let projected = self
            .generator
            .builder
            .build_extract_value(
                aggregate,
                payload_slot.physical_field,
                "mir_variant_project_or_payload",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let fallback_value = self.value(fallback, subject)?;
        self.generator
            .builder
            .build_select(
                active_ok,
                projected,
                fallback_value,
                "mir_variant_project_or_select",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))
    }

    /// Consume a non-Copy variant and move its owned payload field out.  The
    /// `moving=true` ABI is mandatory here: the source aggregate is consumed
    /// by the canonical MIR ownership ledger, so a Copy-layout projection
    /// would silently alias the payload.
    pub(super) fn emit_variant_project_move(
        &mut self,
        result: &MirValueId,
        base: &MirValueId,
        contract: Option<&crate::core::mir::types::MirVariantProjectionTrapContract>,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let base_ty = self.value_type(base, subject)?;
        let result_ty = self.value_type(result, subject)?;
        let receipt = contract.ok_or_else(|| {
            NativeMirError::new(
                subject,
                "consuming direct variant projection has no canonical move receipt",
            )
        })?;
        self.program
            .type_catalog()
            .validate_variant_move_projection_trap_receipt(&base_ty, &result_ty, receipt)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let (variant_abi, _) = native_variant_abi(self.program.type_catalog(), &base_ty, true)?;
        let payload_slot = variant_abi
            .payload_slot(&receipt.projection.variant)
            .ok_or_else(|| {
                NativeMirError::new(
                    subject,
                    "consuming direct variant projection has no native payload slot",
                )
            })?;
        if payload_slot.field != receipt.projection.field
            || payload_slot.ty != result_ty
            || receipt.projection.field_index != 0
        {
            return Err(NativeMirError::new(
                subject,
                "consuming direct variant projection receipt disagrees with native payload ABI",
            ));
        }

        let aggregate = self.value(base, subject)?.into_struct_value();
        let tag = self
            .generator
            .builder
            .build_extract_value(
                aggregate,
                variant_abi.tag_field,
                "mir_variant_move_project_tag",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let active = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                tag,
                self.generator
                    .context
                    .i8_type()
                    .const_int(u64::from(receipt.discriminant), false),
                "mir_variant_move_project_active",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let trap = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_variant_move_project_trap");
        let ok = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_variant_move_project_ok");
        self.generator
            .builder
            .build_conditional_branch(active, ok, trap)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(trap);
        self.emit_abort_with_message(
            &format!(
                "[{}] canonical MIR consuming direct variant projection expected active variant '{}'",
                receipt.trap_code, receipt.variant_name
            ),
            subject,
        )?;
        self.generator.builder.position_at_end(ok);
        self.generator
            .builder
            .build_extract_value(
                aggregate,
                payload_slot.physical_field,
                "mir_variant_move_project_payload",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))
    }
}
