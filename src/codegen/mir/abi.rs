//! LLVM-facing ABI materialization for admitted Canonical MIR types.
//!
//! These helpers consume TypeDesc/layout/glue contracts.  They do not infer
//! ownership from LLVM types and deliberately remain narrower than the
//! backend-independent MIR contract.

use super::*;

/// Validate the native recursive product ABI before LLVM sees a declaration.
///
/// This is deliberately narrower than the backend-independent aggregate glue
/// contract: this slice materializes only scalar leaves, owned Strings, tuples,
/// and concrete records with those children.  A product containing a List,
/// variant, reference, generic, or another unmodelled shape remains fail-closed
/// even when another consumer could represent it.
pub(super) fn validate_native_product_type(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<(), String> {
    let descriptor = catalog
        .get(ty)
        .ok_or_else(|| format!("type '{}' is absent from MIR TypeDesc catalog", ty.as_str()))?;
    match &descriptor.layout {
        MirLayout::Tuple(_) => validate_native_recursive_tuple_type(catalog, ty),
        MirLayout::Record { .. } => validate_native_non_copy_record_type(catalog, ty),
        layout => Err(format!(
            "type '{}' layout {layout:?} is outside the native product ABI",
            ty.as_str()
        )),
    }
}

/// Validate the native concrete non-Copy record ABI before LLVM sees a
/// declaration.  Stable field identities and declaration order remain MIR
/// facts; the native side only chooses the final anonymous struct shape after
/// this contract succeeds.
pub(super) fn validate_native_non_copy_record_type(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<(), String> {
    let descriptor = catalog
        .get(ty)
        .ok_or_else(|| format!("type '{}' is absent from MIR TypeDesc catalog", ty.as_str()))?;
    let MirLayout::Record { fields, .. } = &descriptor.layout else {
        return Err(format!(
            "type '{}' is not a canonical record layout",
            ty.as_str()
        ));
    };
    if !matches!(&descriptor.kind, MirTypeKind::Nominal) {
        return Err(format!(
            "record TypeDesc '{}' has non-nominal kind {:?}",
            ty.as_str(),
            descriptor.kind
        ));
    }
    if fields.is_empty() {
        return Err(format!(
            "record TypeDesc '{}' has no fields in the native ABI",
            ty.as_str()
        ));
    }
    if descriptor.abi != MirAbiClass::Aggregate {
        return Err(format!(
            "record TypeDesc '{}' has ABI {:?}, expected Aggregate",
            ty.as_str(),
            descriptor.abi
        ));
    }
    if !matches!(
        descriptor.ownership,
        MirOwnership::Move | MirOwnership::Linear
    ) {
        return Err(format!(
            "record TypeDesc '{}' ownership {:?} is outside the native non-Copy record contract",
            ty.as_str(),
            descriptor.ownership
        ));
    }
    let expected = MirGlueContract {
        move_out: MirGlueKind::Aggregate,
        clone: MirGlueKind::Aggregate,
        drop: MirGlueKind::Aggregate,
    };
    if descriptor.glue != expected
        || !descriptor.needs_drop_glue
        || !descriptor.needs_clone_glue
        || descriptor.drop_plan.is_none()
    {
        return Err(format!(
            "record TypeDesc '{}' aggregate glue/drop plan is incomplete",
            ty.as_str()
        ));
    }
    for operation in [
        crate::core::mir::types::MirGlueOperation::MoveOut,
        crate::core::mir::types::MirGlueOperation::Clone,
        crate::core::mir::types::MirGlueOperation::Drop,
    ] {
        catalog.validate_glue(ty, operation)?;
    }

    let mut field_ids = BTreeSet::new();
    for field in fields {
        if !field_ids.insert(field.id.clone()) {
            return Err(format!(
                "record field identity '{}' is duplicated in the native non-Copy record contract",
                field.id.0
            ));
        }
        let field_desc = catalog.get(&field.ty).ok_or_else(|| {
            format!(
                "record field '{}' TypeDesc '{}' is absent",
                field.name,
                field.ty.as_str()
            )
        })?;
        let is_owned_string = matches!(
            &field_desc.kind,
            MirTypeKind::Primitive(crate::core::PrimitiveType::String)
        );
        let supported = is_native_scalar_descriptor(field_desc)
            || (is_owned_string && catalog.validate_owned_string(&field.ty).is_ok())
            || (matches!(field_desc.layout, MirLayout::Tuple(_))
                && validate_native_recursive_tuple_type(catalog, &field.ty).is_ok());
        if !supported {
            return Err(format!(
                "record '{}' field '{}' type '{}' is outside the scalar/String/tuple ABI",
                ty.as_str(),
                field.name,
                field.ty.as_str()
            ));
        }
    }
    Ok(())
}

/// Validate the native recursive tuple ABI before LLVM sees a declaration.
pub(super) fn validate_native_recursive_tuple_type(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<(), String> {
    catalog.validate_recursive_tuple_abi(ty)
}

pub(super) fn is_native_scalar_descriptor(desc: &MirTypeDesc) -> bool {
    desc.layout == MirLayout::Scalar
        && matches!(
            desc.abi,
            MirAbiClass::Integer {
                bits: 32 | 64,
                signed: true,
            } | MirAbiClass::Float { bits: 32 | 64 }
                | MirAbiClass::Bool
        )
        && desc.ownership == MirOwnership::Copy
        && desc.glue
            == (MirGlueContract {
                move_out: MirGlueKind::Noop,
                clone: MirGlueKind::Noop,
                drop: MirGlueKind::Noop,
            })
}

/// Map a TypeDesc-proven canonical scalar List element to the native runtime
/// tag.  This is the only place where the native ABI names the runtime's
/// serialized `ListElementKind` values; it never infers the element type from
/// an LLVM pointer or from surface syntax.
pub(super) fn native_list_kind(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<i8, NativeMirError> {
    catalog
        .validate_list_glue(ty, crate::core::mir::types::MirGlueOperation::MoveOut)
        .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
    let desc = catalog
        .get(ty)
        .ok_or_else(|| NativeMirError::new(ty.as_str(), "List TypeDesc is absent"))?;
    let MirLayout::List { element } = &desc.layout else {
        return Err(NativeMirError::new(
            ty.as_str(),
            "native List ABI requested for a non-List TypeDesc",
        ));
    };
    let element_desc = catalog.get(element).ok_or_else(|| {
        NativeMirError::new(
            ty.as_str(),
            format!("List element TypeDesc '{}' is absent", element.as_str()),
        )
    })?;
    match element_desc.abi {
        MirAbiClass::Integer {
            bits: 32 | 64,
            signed: true,
        } => Ok(1),
        MirAbiClass::Bool => Ok(3),
        abi => Err(NativeMirError::new(
            ty.as_str(),
            format!("List element ABI {abi:?} is outside the native scalar List ABI"),
        )),
    }
}

/// Return the one native payload type shared by a bounded flat Copy variant.
///
/// The physical representation is deliberately narrower than the general MIR
/// variant contract: `{ i8 discriminant, scalar payload }`.  The payload slot
/// is present even for a zero-field variant and is zero-filled by the emitter;
/// that keeps the LLVM ABI stable while making owned, nested, mixed, or
/// all-zero-payload shapes fail closed here. Built-in Option/Result and
/// checker-materialized user-enum layouts share this bounded contract.
pub(super) fn native_copy_variant_payload_type(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<crate::core::ResolvedTypeId, NativeMirError> {
    // The general flat-Copy validator intentionally keeps its historical
    // signed-integer/bool boundary.  A concrete builtin Option<f32/f64> is
    // the one promoted floating variant island, so route it through the
    // opt-in TypeDesc contract before returning its payload identity.
    if let Some(expected) = catalog.get(ty).and_then(|descriptor| {
        let MirLayout::Option { inner, .. } = &descriptor.layout else {
            return None;
        };
        catalog.get(inner).and_then(|inner| match inner.kind {
            MirTypeKind::Primitive(crate::core::PrimitiveType::F32) => {
                Some(crate::core::PrimitiveType::F32)
            }
            MirTypeKind::Primitive(crate::core::PrimitiveType::F64) => {
                Some(crate::core::PrimitiveType::F64)
            }
            _ => None,
        })
    }) {
        catalog
            .validate_copy_option_variant(ty, expected)
            .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
        if let MirLayout::Option { inner, .. } = &catalog
            .get(ty)
            .expect("validated Option TypeDesc remains present")
            .layout
        {
            return Ok(inner.clone());
        }
    }
    catalog
        .validate_flat_copy_variant(ty)
        .map_err(|message| NativeMirError::new(ty.as_str(), message))
}

/// Return the first active payload type for the native move-owned variant
/// contract. Result has a second physical payload slot; callers that need the
/// complete ABI must use `native_variant_abi` below.
///
/// The admitted native move-owned profiles are `Option<string>`,
/// `Option<List<Copy scalar>>`, `Result<string, i32>` and
/// `Result<List<Copy scalar>, i32>`. Their canonical TypeDesc/drop plans prove
/// the active payload glue; the physical ABI is
/// `{ i8 discriminant, managed payload }` for Option and
/// `{ i8 discriminant, managed ok_payload, i32 err_payload }` for Result.
/// Nested, mixed, unit-payload, and user-defined variants remain fail-closed
/// until their own contracts are promoted.
pub(super) fn native_non_copy_variant_payload_type(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<crate::core::ResolvedTypeId, NativeMirError> {
    let contract_name = catalog
        .get(ty)
        .map(|descriptor| match &descriptor.layout {
            MirLayout::Result { ok, .. }
                if catalog
                    .get(ok)
                    .is_some_and(|ok| matches!(ok.layout, MirLayout::List { .. })) =>
            {
                "Result<List<Copy scalar>, i32>"
            }
            MirLayout::Result { .. } => "Result<string, i32>",
            MirLayout::Option { inner, .. }
                if catalog
                    .get(inner)
                    .is_some_and(|inner| matches!(inner.layout, MirLayout::List { .. })) =>
            {
                "Option<List<Copy scalar>>"
            }
            MirLayout::Option { .. } => "Option<string>",
            _ => "Option<string>",
        })
        .unwrap_or("Option/Result");
    catalog
        .validate_non_copy_variant_contract(ty)
        .map_err(|message| {
            NativeMirError::new(
                ty.as_str(),
                format!("native non-Copy {contract_name} variant contract: {message}"),
            )
        })?;
    let descriptor = catalog
        .get(ty)
        .ok_or_else(|| NativeMirError::new(ty.as_str(), "variant TypeDesc is absent"))?;
    match &descriptor.layout {
        MirLayout::Option { inner, .. } => Ok(inner.clone()),
        MirLayout::Result { ok, .. } => Ok(ok.clone()),
        layout => Err(NativeMirError::new(
            ty.as_str(),
            format!("native non-Copy variant layout {layout:?} is outside contract"),
        )),
    }
}

/// Physical field positions for the native variant ABI.  The semantic
/// variant identity, discriminant, payload type, and glue remain TypeDesc
/// facts; this adapter owns only the target struct slots used after those
/// facts have been validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NativeVariantPayloadSlot {
    pub(super) variant: crate::core::NodeId,
    pub(super) field: crate::core::NodeId,
    pub(super) physical_field: u32,
    pub(super) ty: crate::core::ResolvedTypeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NativeVariantAbi {
    pub(super) tag_field: u32,
    /// Kept as the first payload position for the existing flat/Option tests;
    /// Result uses `payload_fields` to route Ok and Err to separate slots.
    pub(super) payload_field: u32,
    pub(super) payload_types: Vec<crate::core::ResolvedTypeId>,
    pub(super) payload_fields: Vec<NativeVariantPayloadSlot>,
}

impl NativeVariantAbi {
    pub(super) fn payload_slot(
        &self,
        variant: &crate::core::NodeId,
    ) -> Option<&NativeVariantPayloadSlot> {
        self.payload_fields
            .iter()
            .find(|slot| slot.variant == *variant)
    }
}

/// Materialize the target-facing field contract for a native variant value.
/// `moving` selects the already-promoted Copy or non-Copy TypeDesc contract;
/// it never widens the set of admitted shapes. Result receives one physical
/// slot per alternative payload so a String is never reinterpreted as an i64.
pub(super) fn native_variant_abi(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
    moving: bool,
) -> Result<(NativeVariantAbi, crate::core::ResolvedTypeId), NativeMirError> {
    let descriptor = catalog
        .get(ty)
        .ok_or_else(|| NativeMirError::new(ty.as_str(), "variant TypeDesc is absent"))?;
    let payload_types = if moving {
        native_non_copy_variant_payload_type(catalog, ty)?;
        match &descriptor.layout {
            MirLayout::Option { inner, .. } => vec![inner.clone()],
            MirLayout::Result { ok, error, .. } => vec![ok.clone(), error.clone()],
            layout => {
                return Err(NativeMirError::new(
                    ty.as_str(),
                    format!("native non-Copy variant layout {layout:?} is outside contract"),
                ))
            }
        }
    } else if matches!(descriptor.layout, MirLayout::Result { .. })
        && catalog.validate_copy_result_scalar_variant(ty).is_ok()
    {
        let MirLayout::Result { ok, error, .. } = &descriptor.layout else {
            unreachable!("Result layout checked above");
        };
        vec![ok.clone(), error.clone()]
    } else {
        vec![native_copy_variant_payload_type(catalog, ty)?]
    };
    let variants = match &descriptor.layout {
        MirLayout::Option { variants, .. }
        | MirLayout::Result { variants, .. }
        | MirLayout::Enum { variants, .. } => variants,
        layout => {
            return Err(NativeMirError::new(
                ty.as_str(),
                format!("native variant layout {layout:?} is outside contract"),
            ))
        }
    };
    let mut payload_fields = Vec::new();
    for variant in variants {
        for field in &variant.fields {
            let physical_field =
                if descriptor.kind == MirTypeKind::Result && payload_types.len() == 2 {
                    match variant.id.0.as_str() {
                        "builtin:variant:Result::Ok" => 1,
                        "builtin:variant:Result::Err" => 2,
                        _ => {
                            return Err(NativeMirError::new(
                                ty.as_str(),
                                format!(
                                    "variant '{}' is outside the native canonical Result ABI",
                                    variant.id.0
                                ),
                            ))
                        }
                    }
                } else {
                    1
                };
            payload_fields.push(NativeVariantPayloadSlot {
                variant: variant.id.clone(),
                field: field.id.clone(),
                physical_field,
                ty: field.ty.clone(),
            });
        }
    }
    let first_payload_type = payload_types
        .first()
        .cloned()
        .ok_or_else(|| NativeMirError::new(ty.as_str(), "variant payload type is absent"))?;
    Ok((
        NativeVariantAbi {
            tag_field: 0,
            payload_field: 1,
            payload_types,
            payload_fields,
        },
        first_payload_type,
    ))
}

pub(super) fn native_basic_type<'ctx>(
    context: &'ctx Context,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<BasicTypeEnum<'ctx>, NativeMirError> {
    let desc = catalog
        .get(ty)
        .ok_or_else(|| NativeMirError::new(ty.as_str(), "TypeDesc is absent"))?;
    match desc.abi {
        MirAbiClass::Integer {
            bits: 32,
            signed: true,
        } => Ok(context.i32_type().into()),
        MirAbiClass::Integer {
            bits: 64,
            signed: true,
        } => Ok(context.i64_type().into()),
        MirAbiClass::Float { bits: 32 } => Ok(context.f32_type().into()),
        MirAbiClass::Float { bits: 64 } => Ok(context.f64_type().into()),
        MirAbiClass::Bool => Ok(context.bool_type().into()),
        MirAbiClass::StringHandle => {
            catalog
                .validate_owned_string(ty)
                .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
            let i8_ptr = context.ptr_type(inkwell::AddressSpace::default());
            Ok(context
                .struct_type(
                    &[
                        BasicTypeEnum::PointerType(i8_ptr),
                        BasicTypeEnum::IntType(context.i64_type()),
                    ],
                    false,
                )
                .into())
        }
        MirAbiClass::Pointer if matches!(&desc.kind, MirTypeKind::Reference { mutable: false }) => {
            catalog
                .validate_reference_type(ty)
                .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
            Ok(context.ptr_type(inkwell::AddressSpace::default()).into())
        }
        MirAbiClass::OpaqueHandle => match &desc.layout {
            MirLayout::List { .. } => Ok(context.ptr_type(inkwell::AddressSpace::default()).into()),
            layout => Err(NativeMirError::new(
                ty.as_str(),
                format!("opaque-handle layout {layout:?} is outside native contract"),
            )),
        },
        MirAbiClass::SetHandle => {
            catalog
                .validate_set_glue(ty, crate::core::mir::types::MirGlueOperation::MoveOut)
                .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
            Ok(context.i64_type().into())
        }
        MirAbiClass::Aggregate => match &desc.layout {
            MirLayout::Tuple(elements) => {
                validate_native_recursive_tuple_type(catalog, ty)
                    .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
                let field_types = elements
                    .iter()
                    .map(|element| native_basic_type(context, catalog, element))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(context.struct_type(&field_types, false).into())
            }
            MirLayout::Record { fields, .. } if !fields.is_empty() => {
                let non_copy = catalog
                    .get(ty)
                    .is_some_and(|descriptor| descriptor.ownership != MirOwnership::Copy);
                if non_copy {
                    validate_native_non_copy_record_type(catalog, ty)
                        .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
                }
                let mut field_types = Vec::with_capacity(fields.len());
                for field in fields {
                    let field_desc = catalog.get(&field.ty).ok_or_else(|| {
                        NativeMirError::new(
                            field.name.clone(),
                            "record field TypeDesc is absent from native catalog",
                        )
                    })?;
                    let supported = if non_copy {
                        is_native_scalar_descriptor(field_desc)
                            || (matches!(
                                &field_desc.kind,
                                MirTypeKind::Primitive(crate::core::PrimitiveType::String)
                            ) && catalog.validate_owned_string(&field.ty).is_ok())
                            || matches!(field_desc.layout, MirLayout::Tuple(_))
                    } else {
                        is_native_scalar_descriptor(field_desc)
                    };
                    if !supported {
                        return Err(NativeMirError::new(
                            field.name.clone(),
                            "record field is outside the native product ABI",
                        ));
                    }
                    field_types.push(native_basic_type(context, catalog, &field.ty)?);
                }
                Ok(context.struct_type(&field_types, false).into())
            }
            MirLayout::Option { .. } | MirLayout::Result { .. } | MirLayout::Enum { .. } => {
                let (variant_abi, _) =
                    native_variant_abi(catalog, ty, desc.ownership != MirOwnership::Copy)?;
                let mut fields = vec![context.i8_type().into()];
                fields.extend(
                    variant_abi
                        .payload_types
                        .iter()
                        .map(|payload_ty| native_basic_type(context, catalog, payload_ty))
                        .collect::<Result<Vec<_>, _>>()?,
                );
                Ok(context.struct_type(&fields, false).into())
            }
            layout => Err(NativeMirError::new(
                ty.as_str(),
                format!("aggregate layout {layout:?} is outside native contract"),
            )),
        },
        MirAbiClass::Unit => Err(NativeMirError::new(
            ty.as_str(),
            "unit has no LLVM BasicType",
        )),
        abi => Err(NativeMirError::new(
            ty.as_str(),
            format!("ABI {abi:?} is outside native scalar contract"),
        )),
    }
}
