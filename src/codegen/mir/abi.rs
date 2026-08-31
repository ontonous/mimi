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
    if descriptor.ownership != MirOwnership::Move {
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
    let descriptor = catalog
        .get(ty)
        .ok_or_else(|| format!("type '{}' is absent from MIR TypeDesc catalog", ty.as_str()))?;
    let MirLayout::Tuple(elements) = &descriptor.layout else {
        return Err(format!(
            "type '{}' is not a canonical tuple layout",
            ty.as_str()
        ));
    };
    if elements.is_empty() {
        return Err(format!(
            "tuple TypeDesc '{}' has no fields in the native ABI",
            ty.as_str()
        ));
    }
    if !matches!(
        &descriptor.kind,
        MirTypeKind::Tuple { arity } if *arity == elements.len()
    ) {
        return Err(format!(
            "tuple TypeDesc '{}' kind/layout arity disagrees",
            ty.as_str()
        ));
    }
    if descriptor.abi != MirAbiClass::Aggregate {
        return Err(format!(
            "tuple TypeDesc '{}' has ABI {:?}, expected Aggregate",
            ty.as_str(),
            descriptor.abi
        ));
    }

    let noop = MirGlueContract {
        move_out: MirGlueKind::Noop,
        clone: MirGlueKind::Noop,
        drop: MirGlueKind::Noop,
    };
    if descriptor.ownership == MirOwnership::Copy {
        if descriptor.glue != noop
            || descriptor.needs_drop_glue
            || descriptor.needs_clone_glue
            || descriptor.drop_plan.is_some()
        {
            return Err(format!(
                "Copy tuple TypeDesc '{}' does not carry the canonical no-op glue contract",
                ty.as_str()
            ));
        }
    } else {
        if descriptor.ownership != MirOwnership::Move {
            return Err(format!(
                "tuple TypeDesc '{}' ownership {:?} is outside the native Move contract",
                ty.as_str(),
                descriptor.ownership
            ));
        }
        let aggregate = MirGlueContract {
            move_out: MirGlueKind::Aggregate,
            clone: MirGlueKind::Aggregate,
            drop: MirGlueKind::Aggregate,
        };
        if descriptor.glue != aggregate
            || !descriptor.needs_drop_glue
            || !descriptor.needs_clone_glue
            || descriptor.drop_plan.is_none()
        {
            return Err(format!(
                "tuple TypeDesc '{}' aggregate glue/drop plan is incomplete",
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
    }

    for (index, element) in elements.iter().enumerate() {
        let element_desc = catalog.get(element).ok_or_else(|| {
            format!(
                "tuple '{}' field {} TypeDesc '{}' is absent",
                ty.as_str(),
                index,
                element.as_str()
            )
        })?;
        let is_owned_string = matches!(
            &element_desc.kind,
            MirTypeKind::Primitive(crate::core::PrimitiveType::String)
        );
        let supported = if is_native_scalar_descriptor(element_desc) {
            true
        } else if is_owned_string {
            catalog.validate_owned_string(element).is_ok()
        } else if matches!(element_desc.layout, MirLayout::Tuple(_)) {
            validate_native_recursive_tuple_type(catalog, element).is_ok()
        } else {
            false
        };
        if !supported {
            return Err(format!(
                "tuple '{}' field {} type '{}' is outside the scalar/String/tuple ABI",
                ty.as_str(),
                index,
                element.as_str()
            ));
        }
    }
    Ok(())
}

pub(super) fn is_native_scalar_descriptor(desc: &MirTypeDesc) -> bool {
    desc.layout == MirLayout::Scalar
        && matches!(
            desc.abi,
            MirAbiClass::Integer {
                bits: 32 | 64,
                signed: true,
            } | MirAbiClass::Bool
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

/// Return the one native payload type shared by a bounded built-in variant.
///
/// The physical representation is deliberately narrower than the general MIR
/// variant contract: `{ i8 discriminant, scalar payload }`.  The payload slot
/// is present even for a zero-field variant and is zero-filled by the emitter;
/// that keeps the LLVM ABI stable while making Option/Result shapes with
/// owned, nested, mixed, or unit payloads fail closed here.
pub(super) fn native_copy_variant_payload_type(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<crate::core::ResolvedTypeId, NativeMirError> {
    let desc = catalog
        .get(ty)
        .ok_or_else(|| NativeMirError::new(ty.as_str(), "variant TypeDesc is absent"))?;
    let variants = match &desc.layout {
        MirLayout::Option { variants, .. } | MirLayout::Result { variants, .. } => variants,
        layout => {
            return Err(NativeMirError::new(
                ty.as_str(),
                format!("layout {layout:?} is outside the flat Copy variant contract"),
            ))
        }
    };
    if desc.abi != MirAbiClass::Aggregate {
        return Err(NativeMirError::new(
            ty.as_str(),
            format!(
                "variant ABI {:?} is outside the flat Copy variant contract",
                desc.abi
            ),
        ));
    }
    if desc.ownership != MirOwnership::Copy {
        return Err(NativeMirError::new(
            ty.as_str(),
            format!(
                "variant ownership {:?} requires explicit native glue and is outside the flat Copy variant contract",
                desc.ownership
            ),
        ));
    }
    if desc.glue
        != (MirGlueContract {
            move_out: MirGlueKind::Noop,
            clone: MirGlueKind::Noop,
            drop: MirGlueKind::Noop,
        })
    {
        return Err(NativeMirError::new(
            ty.as_str(),
            "variant TypeDesc does not carry the canonical no-op glue contract",
        ));
    }
    if variants.is_empty() {
        return Err(NativeMirError::new(
            ty.as_str(),
            "variant TypeDesc has no variants in the flat Copy variant contract",
        ));
    }

    let mut discriminants = BTreeSet::new();
    let mut variant_ids = BTreeSet::new();
    let mut field_ids = BTreeSet::new();
    let mut payload_type: Option<crate::core::ResolvedTypeId> = None;
    for variant in variants {
        if !discriminants.insert(variant.discriminant) {
            return Err(NativeMirError::new(
                ty.as_str(),
                format!(
                    "variant discriminant {} is duplicated in the flat Copy variant contract",
                    variant.discriminant
                ),
            ));
        }
        if variant.discriminant > u8::MAX as u16 {
            return Err(NativeMirError::new(
                ty.as_str(),
                format!(
                    "variant discriminant {} does not fit the native i8 tag contract",
                    variant.discriminant
                ),
            ));
        }
        if !variant_ids.insert(variant.id.clone()) {
            return Err(NativeMirError::new(
                ty.as_str(),
                format!(
                    "variant identity '{}' is duplicated in the flat Copy variant contract",
                    variant.id.0
                ),
            ));
        }
        if variant.fields.len() > 1 {
            return Err(NativeMirError::new(
                ty.as_str(),
                format!(
                    "variant '{}' has {} payload fields; the flat Copy variant contract allows at most one",
                    variant.name,
                    variant.fields.len()
                ),
            ));
        }
        let Some(field) = variant.fields.first() else {
            continue;
        };
        if !field_ids.insert(field.id.clone()) {
            return Err(NativeMirError::new(
                ty.as_str(),
                format!(
                    "variant payload field identity '{}' is duplicated in the flat Copy variant contract",
                    field.id.0
                ),
            ));
        }
        let field_desc = catalog.get(&field.ty).ok_or_else(|| {
            NativeMirError::new(
                ty.as_str(),
                format!("variant payload TypeDesc '{}' is absent", field.ty.as_str()),
            )
        })?;
        if !is_native_scalar_descriptor(field_desc) {
            return Err(NativeMirError::new(
                ty.as_str(),
                format!(
                    "variant '{}' payload ABI {:?}/layout {:?} is outside the flat Copy variant contract",
                    variant.name, field_desc.abi, field_desc.layout
                ),
            ));
        }
        if let Some(expected) = &payload_type {
            if expected != &field.ty {
                return Err(NativeMirError::new(
                    ty.as_str(),
                    format!(
                        "variant payload type '{}' disagrees with '{}'; mixed payload ABI is outside the flat Copy variant contract",
                        field.ty.as_str(),
                        expected.as_str()
                    ),
                ));
            }
        } else {
            payload_type = Some(field.ty.clone());
        }
    }
    payload_type.ok_or_else(|| {
        NativeMirError::new(
            ty.as_str(),
            "variant has no scalar payload; unit/zero-payload variants are outside the flat Copy variant contract",
        )
    })
}

/// Return the payload type for the first native move-owned variant contract.
///
/// This slice intentionally admits exactly `Option<string>`: the canonical
/// TypeDesc/drop plan proves the active payload is an owned String, while the
/// physical ABI remains `{ i8 discriminant, StringHandle payload }`.  Result,
/// nested, mixed, unit-payload, and user-defined variants remain fail-closed
/// until their own MIR glue/effect contracts are promoted.
pub(super) fn native_non_copy_variant_payload_type(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<crate::core::ResolvedTypeId, NativeMirError> {
    let desc = catalog
        .get(ty)
        .ok_or_else(|| NativeMirError::new(ty.as_str(), "variant TypeDesc is absent"))?;
    let (inner, variants) = match &desc.layout {
        MirLayout::Option { inner, variants } => (inner, variants),
        layout => {
            return Err(NativeMirError::new(
                ty.as_str(),
                format!(
                "layout {layout:?} is outside the native non-Copy Option<string> variant contract"
            ),
            ))
        }
    };
    if !matches!(&desc.kind, MirTypeKind::Option)
        || desc.abi != MirAbiClass::Aggregate
        || desc.ownership != MirOwnership::Move
    {
        return Err(NativeMirError::new(
            ty.as_str(),
            format!(
                "variant TypeDesc kind/ABI/ownership ({:?}/{:?}/{:?}) is outside the native non-Copy Option<string> variant contract",
                desc.kind, desc.abi, desc.ownership
            ),
        ));
    }
    let expected = MirGlueContract {
        move_out: MirGlueKind::Aggregate,
        clone: MirGlueKind::Aggregate,
        drop: MirGlueKind::Aggregate,
    };
    if desc.glue != expected
        || !desc.needs_drop_glue
        || !desc.needs_clone_glue
        || desc.variant_drop_plan.is_none()
    {
        return Err(NativeMirError::new(
            ty.as_str(),
            "variant TypeDesc aggregate glue/drop plan is incomplete for the native non-Copy Option<string> variant contract",
        ));
    }
    for operation in [
        crate::core::mir::types::MirGlueOperation::MoveOut,
        crate::core::mir::types::MirGlueOperation::Clone,
        crate::core::mir::types::MirGlueOperation::Drop,
    ] {
        catalog
            .validate_glue(ty, operation)
            .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
    }
    if variants.len() != 2 {
        return Err(NativeMirError::new(
            ty.as_str(),
            format!(
                "Option TypeDesc has {} variants; the native non-Copy Option<string> contract requires None and Some",
                variants.len()
            ),
        ));
    }
    let none = variants.iter().find(|variant| {
        variant.id.0 == "builtin:variant:Option::None"
            && variant.name == "None"
            && variant.discriminant == 0
            && variant.fields.is_empty()
    });
    let some = variants.iter().find(|variant| {
        variant.id.0 == "builtin:variant:Option::Some"
            && variant.name == "Some"
            && variant.discriminant == 1
            && variant.fields.len() == 1
    });
    if none.is_none() || some.is_none() {
        return Err(NativeMirError::new(
            ty.as_str(),
            "Option TypeDesc variants do not match the canonical None/Some native non-Copy contract",
        ));
    }
    let field = &some.expect("checked above").fields[0];
    if field.id.0 != "builtin:variant:Option::Some/payload:0" || field.ty != *inner {
        return Err(NativeMirError::new(
            ty.as_str(),
            "Option Some payload identity/type disagrees with the canonical native non-Copy contract",
        ));
    }
    catalog
        .validate_owned_string(inner)
        .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
    Ok(inner.clone())
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
            MirLayout::Option { .. } | MirLayout::Result { .. } => {
                let payload_ty = if desc.ownership == MirOwnership::Copy {
                    native_copy_variant_payload_type(catalog, ty)?
                } else {
                    native_non_copy_variant_payload_type(catalog, ty)?
                };
                let payload = native_basic_type(context, catalog, &payload_ty)?;
                Ok(context
                    .struct_type(&[context.i8_type().into(), payload], false)
                    .into())
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
