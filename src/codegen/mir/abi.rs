//! LLVM-facing ABI materialization for admitted Canonical MIR types.
//!
//! These helpers consume TypeDesc/layout/glue contracts.  They do not infer
//! ownership from LLVM types and deliberately remain narrower than the
//! backend-independent MIR contract.

use super::*;
use crate::core::mir::types::MirGlueOperation;

/// Validate the native recursive product ABI before LLVM sees a declaration.
///
/// This is deliberately narrower than the backend-independent aggregate glue
/// contract: this slice materializes only scalar leaves, owned Strings,
/// `List<Copy scalar>` handles, tuples, and concrete records with those
/// children. A product containing a variant, reference, generic, nested List,
/// or another unmodelled shape remains fail-closed even when another consumer
/// could represent it.
pub(super) fn validate_native_product_type(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<(), String> {
    if ty == &crate::core::mir::types::ordinary_map_owned_any_v2_get_result_type_id() {
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            catalog.validate_glue(ty, operation)?;
        }
        let descriptor = catalog
            .get(ty)
            .ok_or_else(|| format!("type '{}' is absent from MIR TypeDesc catalog", ty.as_str()))?;
        let expected = MirLayout::Tuple(vec![
            crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Bool),
            crate::core::mir::types::ordinary_map_owned_any_v2_payload_type_id(),
        ]);
        if descriptor.kind != (MirTypeKind::Tuple { arity: 2 }) || descriptor.layout != expected {
            return Err(format!(
                "type '{}' is not the exact ordinary Map/Any owner-v2 GetAny result product",
                ty.as_str()
            ));
        }
        return Ok(());
    }
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
        let is_owned_list = matches!(field_desc.layout, MirLayout::List { .. })
            && catalog.validate_move_owned_list_payload(&field.ty).is_ok();
        let is_owned_set = matches!(field_desc.layout, MirLayout::Set { .. })
            && catalog
                .validate_set_glue(&field.ty, MirGlueOperation::MoveOut)
                .is_ok();
        let supported = is_native_scalar_descriptor(field_desc)
            || (is_owned_string && catalog.validate_owned_string(&field.ty).is_ok())
            || is_owned_list
            || is_owned_set
            || (matches!(field_desc.layout, MirLayout::Tuple(_))
                && validate_native_recursive_tuple_type(catalog, &field.ty).is_ok())
            || (matches!(field_desc.layout, MirLayout::Record { .. })
                && validate_native_non_copy_record_type(catalog, &field.ty).is_ok());
        if !supported {
            return Err(format!(
                "record '{}' field '{}' type '{}' is outside the scalar/String/List<Copy scalar>/Set<Copy scalar>/tuple ABI",
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
    desc.is_canonical_copy_scalar(true)
}

/// Map a TypeDesc-proven canonical List element to the native runtime tag.
/// This is the only place where the native ABI names the runtime's serialized
/// `ListElementKind` values; it never infers the element type from an LLVM
/// pointer or from surface syntax. `ListElementKind::List` is admitted only
/// for the one-level nested `List<List<Copy scalar>>` glue contract.
pub(super) fn native_list_kind(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<i8, NativeMirError> {
    if ty == &crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id() {
        super::ordinary_map_owned_any_v2::validate_ordinary_map_owned_any_v2_values_list_type(
            catalog, ty,
        )
        .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
        return Ok(crate::runtime::ListElementKind::ManagedAnyV2 as i8);
    }
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
    if catalog.validate_owned_string_list(ty).is_ok() {
        return Ok(crate::runtime::ListElementKind::String as i8);
    }
    if matches!(element_desc.layout, MirLayout::List { .. }) {
        return Ok(crate::runtime::ListElementKind::List as i8);
    }
    // Legacy Values snapshots use a move-owned List<ManagedAny> whose
    // elements are generation-bearing i64 owner tokens. The TypeDesc (and
    // list glue validator) proves this exact element identity; the native
    // adapter must never infer it from the raw i64 storage.
    if element_desc.kind == MirTypeKind::ManagedAnyI32StringV1
        || matches!(element_desc.layout, MirLayout::ManagedAnyI32StringV1)
    {
        catalog
            .validate_glue(element, crate::core::mir::types::MirGlueOperation::MoveOut)
            .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
        catalog
            .validate_glue(element, crate::core::mir::types::MirGlueOperation::Clone)
            .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
        catalog
            .validate_glue(element, crate::core::mir::types::MirGlueOperation::Drop)
            .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
        return Ok(crate::runtime::ListElementKind::ManagedAny as i8);
    }
    if element_desc.kind == MirTypeKind::OrdinaryMapOwnedAnyV2ValuesElement
        || matches!(
            element_desc.layout,
            MirLayout::OrdinaryMapOwnedAnyV2ValuesElement
        )
    {
        return Err(NativeMirError::new(
            ty.as_str(),
            "V2 Values owner elements are admitted only in their dedicated canonical Values List",
        ));
    }
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
        let descriptor = catalog.get(ty).ok_or_else(|| {
            NativeMirError::new(ty.as_str(), "validated Option TypeDesc is absent")
        })?;
        if let MirLayout::Option { inner, .. } = &descriptor.layout {
            return Ok(inner.clone());
        }
        return Err(NativeMirError::new(
            ty.as_str(),
            "validated Option TypeDesc is no longer an Option layout",
        ));
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
/// `Option<List<Copy scalar>>`, bounded scalar/String/tuple products,
/// `Result<string, i32>` and `Result<List<Copy scalar>, i32>` and the generic projection-only
/// `Result<managed, bool>` shape. Their canonical TypeDesc/drop plans prove
/// the active payload glue; the physical ABI is
/// `{ i8 discriminant, managed payload }` for Option and
/// `{ i8 discriminant, managed ok_payload, i32 err_payload }` for Result.
/// Nested List, mixed, unit-payload, and user-defined variants remain
/// fail-closed until their own contracts are promoted.
pub(super) fn native_non_copy_variant_payload_type(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<crate::core::ResolvedTypeId, NativeMirError> {
    native_non_copy_variant_payload_type_with_flow(catalog, ty, true)
}

/// Validate a non-Copy variant at a non-Flow native boundary.  Recoverable
/// Result payloads are admitted only when the canonical program contains the
/// matching Flow transition contract; a free-standing `Result<string,string>`
/// must not inherit the Flow aggregate validator merely because its TypeDesc
/// has recursive ownership glue.
pub(super) fn native_non_copy_variant_payload_type_strict(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<crate::core::ResolvedTypeId, NativeMirError> {
    native_non_copy_variant_payload_type_with_flow(catalog, ty, false)
}

fn native_non_copy_variant_payload_type_with_flow(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
    allow_recoverable_flow: bool,
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
            MirLayout::Enum { .. } => "multi-target union",
            _ => "Option<string>",
        })
        .unwrap_or("Option/Result");
    let validation = catalog.validate_non_copy_variant_contract(ty).or_else(|_| {
        catalog
            .validate_result_move_projection_variant(ty)
            .map(|_| ())
    });
    let validation = if allow_recoverable_flow {
        validation.or_else(|_| catalog.validate_recoverable_result_variant(ty))
    } else {
        validation
    };
    validation.map_err(|message| {
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
        MirLayout::Enum { variants, .. } => variants
            .iter()
            .flat_map(|variant| variant.fields.iter().map(|field| field.ty.clone()))
            .next()
            .ok_or_else(|| {
                NativeMirError::new(
                    ty.as_str(),
                    "native non-Copy union variant contract has no payload slot",
                )
            }),
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

    /// Every payload slot carried by one variant.  The promoted multi-target
    /// union contract admits multi-field variants, so consumers bind and drop
    /// per field instead of assuming the single historical slot.
    pub(super) fn payload_slots(
        &self,
        variant: &crate::core::NodeId,
    ) -> impl Iterator<Item = &NativeVariantPayloadSlot> + '_ {
        let variant = variant.clone();
        self.payload_fields
            .iter()
            .filter(move |slot| slot.variant == variant)
    }
}

/// Native ABI for the isolated, separately receipted NewResult constructor.
/// This never changes `native_variant_abi`: the latter continues to reject
/// non-Copy Results whose Clone glue is unsupported.
pub(super) fn native_ordinary_map_new_result_variant_abi(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<(NativeVariantAbi, crate::core::ResolvedTypeId), NativeMirError> {
    function
        .ownership
        .ordinary_map_new_result_receipt()
        .ok_or_else(|| {
            NativeMirError::new(
                function.owner.0.clone(),
                "native NewResult ABI has no separate Checker receipt",
            )
        })?;
    crate::core::mir::validate_ordinary_map_new_result_types(function, catalog)
        .map_err(|message| NativeMirError::new(function.owner.0.clone(), message))?;
    let result_ty = crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id();
    if ty != &result_ty {
        return Err(NativeMirError::new(
            ty.as_str(),
            "native NewResult ABI is outside the exact checker-receipted Result<root, i32> shape",
        ));
    }
    let descriptor = catalog
        .get(ty)
        .ok_or_else(|| NativeMirError::new(ty.as_str(), "NewResult TypeDesc is absent"))?;
    let MirLayout::Result {
        ok,
        error,
        variants,
    } = &descriptor.layout
    else {
        return Err(NativeMirError::new(
            ty.as_str(),
            "native NewResult TypeDesc is not a Result layout",
        ));
    };
    let mut payload_fields = Vec::with_capacity(2);
    for (name, expected_type, physical_field) in [("Ok", ok, 1), ("Err", error, 2)] {
        let id = crate::core::NodeId(format!("builtin:variant:Result::{name}"));
        let variant = variants
            .iter()
            .find(|variant| variant.id == id)
            .ok_or_else(|| {
                NativeMirError::new(ty.as_str(), format!("NewResult {name} variant is absent"))
            })?;
        let field = match variant.fields.as_slice() {
            [field] if field.ty == *expected_type => field,
            _ => {
                return Err(NativeMirError::new(
                    ty.as_str(),
                    format!("NewResult {name} payload differs from its canonical TypeDesc"),
                ));
            }
        };
        payload_fields.push(NativeVariantPayloadSlot {
            variant: variant.id.clone(),
            field: field.id.clone(),
            physical_field,
            ty: field.ty.clone(),
        });
    }
    let first_payload_type = ok.clone();
    Ok((
        NativeVariantAbi {
            tag_field: 0,
            payload_field: 1,
            payload_types: vec![ok.clone(), error.clone()],
            payload_fields,
        },
        first_payload_type,
    ))
}

/// Native ABI for the direct tuple-Get L1 route's operational Root Result.
/// Its Checker receipt is independent from the standalone NewResult receipt,
/// so the route contract is replayed here before exposing the same bounded
/// `{ i8, root-handle, i32 }` carrier to `SwitchMove` emission.
pub(super) fn native_ordinary_map_direct_tuple_get_l1_result_variant_abi(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<(NativeVariantAbi, crate::core::ResolvedTypeId), NativeMirError> {
    if function
        .ownership
        .ordinary_map_direct_tuple_get_unpack_l1_receipt()
        .is_none()
    {
        return Err(NativeMirError::new(
            function.owner.0.clone(),
            "direct tuple Get L1 Result ABI has no Checker route receipt",
        ));
    }
    crate::core::mir::validate_ordinary_map_direct_tuple_get_unpack_l1_types(function, catalog)
        .map_err(|message| NativeMirError::new(function.owner.0.clone(), message))?;

    let result_ty = crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id();
    if ty != &result_ty {
        return Err(NativeMirError::new(
            ty.as_str(),
            "direct tuple Get L1 Result ABI is outside the exact operational Root Result TypeDesc",
        ));
    }
    let descriptor = catalog.get(ty).ok_or_else(|| {
        NativeMirError::new(ty.as_str(), "direct tuple Get L1 Result TypeDesc is absent")
    })?;
    let MirLayout::Result {
        ok,
        error,
        variants,
    } = &descriptor.layout
    else {
        return Err(NativeMirError::new(
            ty.as_str(),
            "direct tuple Get L1 TypeDesc is not a Result layout",
        ));
    };
    if ok != &crate::core::mir::types::ordinary_map_owned_any_v2_type_id()
        || error != &crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32)
        || variants.len() != 2
    {
        return Err(NativeMirError::new(
            ty.as_str(),
            "direct tuple Get L1 Result TypeDesc does not carry the exact Root/i32 payload pair",
        ));
    }

    let mut payload_fields = Vec::with_capacity(2);
    for (name, expected_type, physical_field) in [("Ok", ok, 1), ("Err", error, 2)] {
        let id = crate::core::NodeId(format!("builtin:variant:Result::{name}"));
        let variant = variants
            .iter()
            .find(|variant| variant.id == id)
            .ok_or_else(|| {
                NativeMirError::new(
                    ty.as_str(),
                    format!("direct tuple Get L1 {name} variant is absent"),
                )
            })?;
        let field = match variant.fields.as_slice() {
            [field] if field.ty == *expected_type => field,
            _ => {
                return Err(NativeMirError::new(
                    ty.as_str(),
                    format!(
                        "direct tuple Get L1 {name} payload differs from its canonical TypeDesc"
                    ),
                ));
            }
        };
        payload_fields.push(NativeVariantPayloadSlot {
            variant: id,
            field: field.id.clone(),
            physical_field,
            ty: field.ty.clone(),
        });
    }
    Ok((
        NativeVariantAbi {
            tag_field: 0,
            payload_field: 1,
            payload_types: vec![ok.clone(), error.clone()],
            payload_fields,
        },
        ok.clone(),
    ))
}

/// Physical `{ i8, i64, i32 }` layout for only the exact Checker NewResult
/// receipt. It is deliberately separate from `native_basic_type` so no
/// ordinary or generic Result gains a new admission path.
pub(super) fn native_ordinary_map_new_result_llvm_type<'ctx>(
    context: &'ctx Context,
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<BasicTypeEnum<'ctx>, NativeMirError> {
    let (variant_abi, _) = native_ordinary_map_new_result_variant_abi(function, catalog, ty)?;
    let mut fields = vec![context.i8_type().into()];
    fields.extend(
        variant_abi
            .payload_types
            .iter()
            .map(|payload| native_basic_type(context, catalog, payload))
            .collect::<Result<Vec<_>, _>>()?,
    );
    Ok(context.struct_type(&fields, false).into())
}

/// Native ABI for the independently receipted ResultBranchV3 owned-get root
/// carrier (`Result<v1-root, i32>`). Deliberately kept separate from
/// `native_variant_abi` so arbitrary non-Copy Results keep failing closed.
pub(super) fn native_ordinary_map_owned_get_result_branch_v3_variant_abi(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<(NativeVariantAbi, crate::core::ResolvedTypeId), NativeMirError> {
    let receipt = function
        .ownership
        .ordinary_map_owned_get_result_branch_v3_receipt()
        .ok_or_else(|| {
            NativeMirError::new(
                function.owner.0.clone(),
                "native ResultBranchV3 ABI has no Checker source receipt",
            )
        })?;
    if receipt.owner != function.owner {
        return Err(NativeMirError::new(
            function.owner.0.clone(),
            "native ResultBranchV3 ABI receipt owner differs from MIR function",
        ));
    }
    let result_ty =
        crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_root_result_type_id();
    if ty != &result_ty {
        return Err(NativeMirError::new(
            ty.as_str(),
            "native ResultBranchV3 ABI is outside the exact Result<v1-root, i32> shape",
        ));
    }
    let descriptor = catalog
        .get(ty)
        .ok_or_else(|| NativeMirError::new(ty.as_str(), "ResultBranchV3 TypeDesc is absent"))?;
    let MirLayout::Result {
        ok,
        error,
        variants,
    } = &descriptor.layout
    else {
        return Err(NativeMirError::new(
            ty.as_str(),
            "native ResultBranchV3 TypeDesc is not a Result layout",
        ));
    };
    if ok != &crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id()
        || error != &crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32)
    {
        return Err(NativeMirError::new(
            ty.as_str(),
            "native ResultBranchV3 Result payload differs from Result<v1-root, i32>",
        ));
    }
    let mut payload_fields = Vec::with_capacity(2);
    for (name, expected_type, physical_field) in [("Ok", ok, 1), ("Err", error, 2)] {
        let id = crate::core::NodeId(format!("builtin:variant:Result::{name}"));
        let variant = variants
            .iter()
            .find(|variant| variant.id == id)
            .ok_or_else(|| {
                NativeMirError::new(
                    ty.as_str(),
                    format!("ResultBranchV3 {name} variant is absent"),
                )
            })?;
        let field = match variant.fields.as_slice() {
            [field] if field.ty == *expected_type => field,
            _ => {
                return Err(NativeMirError::new(
                    ty.as_str(),
                    format!("ResultBranchV3 {name} payload differs from its canonical TypeDesc"),
                ));
            }
        };
        payload_fields.push(NativeVariantPayloadSlot {
            variant: variant.id.clone(),
            field: field.id.clone(),
            physical_field,
            ty: field.ty.clone(),
        });
    }
    let first_payload_type = ok.clone();
    Ok((
        NativeVariantAbi {
            tag_field: 0,
            payload_field: 1,
            payload_types: vec![ok.clone(), error.clone()],
            payload_fields,
        },
        first_payload_type,
    ))
}

/// Physical `{ i8, i64, i32 }` layout for only the Checker-bound ResultBranchV3
/// owned-get root result carrier.
pub(super) fn native_ordinary_map_owned_get_result_branch_v3_llvm_type<'ctx>(
    context: &'ctx Context,
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<BasicTypeEnum<'ctx>, NativeMirError> {
    let (variant_abi, _) =
        native_ordinary_map_owned_get_result_branch_v3_variant_abi(function, catalog, ty)?;
    let mut fields = vec![context.i8_type().into()];
    fields.extend(
        variant_abi
            .payload_types
            .iter()
            .map(|payload| native_basic_type(context, catalog, payload))
            .collect::<Result<Vec<_>, _>>()?,
    );
    Ok(context.struct_type(&fields, false).into())
}

/// Native ABI for the independent New/SetResult SetLineage profile. Its
/// source authority is the complete SetLineage receipt; it must not borrow
/// the standalone NewResult receipt as route authorization.
pub(super) fn native_ordinary_map_set_result_variant_abi(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<(NativeVariantAbi, crate::core::ResolvedTypeId), NativeMirError> {
    let result_ty = crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id();
    if ty != &result_ty {
        return Err(NativeMirError::new(
            ty.as_str(),
            "native SetResult ABI is outside its canonical Result<root, i32> shape",
        ));
    }
    if let Some(receipt) = function
        .ownership
        .ordinary_map_set_get_i32_lineage_receipt()
    {
        if receipt.owner != function.owner {
            return Err(NativeMirError::new(
                function.owner.0.clone(),
                "native SetResult ABI is outside its exact Checker Set/Get-i32 lineage receipt",
            ));
        }
        crate::core::mir::validate_ordinary_map_set_get_i32_result_types(function, catalog)
            .map_err(|message| NativeMirError::new(function.owner.0.clone(), message))?;
        catalog
            .validate_ordinary_map_owned_any_v2_root_result(ty, &receipt.set_result_type)
            .map_err(|message| NativeMirError::new(function.owner.0.clone(), message))?;
    } else {
        let receipt = function
            .ownership
            .ordinary_map_set_lineage_receipt()
            .ok_or_else(|| {
                NativeMirError::new(
                    function.owner.0.clone(),
                    "native SetResult ABI has no independent Checker SetLineage receipt",
                )
            })?;
        if receipt.owner != function.owner {
            return Err(NativeMirError::new(
                function.owner.0.clone(),
                "native SetResult ABI is outside its exact Checker SetLineage receipt",
            ));
        }
        catalog
            .validate_ordinary_map_owned_any_v2_root_result(ty, &receipt.new_result_type)
            .map_err(|message| NativeMirError::new(function.owner.0.clone(), message))?;
    }
    let descriptor = catalog
        .get(ty)
        .ok_or_else(|| NativeMirError::new(ty.as_str(), "SetResult TypeDesc is absent"))?;
    let MirLayout::Result {
        ok,
        error,
        variants,
    } = &descriptor.layout
    else {
        return Err(NativeMirError::new(
            ty.as_str(),
            "native SetResult TypeDesc is not a Result layout",
        ));
    };
    let mut payload_fields = Vec::with_capacity(2);
    for (name, expected_type, physical_field) in [("Ok", ok, 1), ("Err", error, 2)] {
        let id = crate::core::NodeId(format!("builtin:variant:Result::{name}"));
        let variant = variants
            .iter()
            .find(|variant| variant.id == id)
            .ok_or_else(|| {
                NativeMirError::new(ty.as_str(), format!("SetResult {name} variant is absent"))
            })?;
        let field = match variant.fields.as_slice() {
            [field] if field.ty == *expected_type => field,
            _ => {
                return Err(NativeMirError::new(
                    ty.as_str(),
                    format!("SetResult {name} payload differs from its canonical TypeDesc"),
                ));
            }
        };
        payload_fields.push(NativeVariantPayloadSlot {
            variant: variant.id.clone(),
            field: field.id.clone(),
            physical_field,
            ty: field.ty.clone(),
        });
    }
    Ok((
        NativeVariantAbi {
            tag_field: 0,
            payload_field: 1,
            payload_types: vec![ok.clone(), error.clone()],
            payload_fields,
        },
        ok.clone(),
    ))
}

/// Native ABI for the fallible Any-payload Set used only by the explicit
/// computed String-key owner-chain branch profile. Its source authority is
/// the exact Checker result-branch receipt; this helper deliberately does
/// not reuse or widen the narrow i32 SetResult receipt.
pub(super) fn native_ordinary_map_computed_string_key_owner_chain_set_any_result_variant_abi(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<(NativeVariantAbi, crate::core::ResolvedTypeId), NativeMirError> {
    use crate::core::ownership::{
        OrdinaryMapOwnedAnyV2ActionKind as ActionKind,
        OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainProfileVersion as ChainVersion,
        OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainResultBranchProfileVersion as BranchVersion,
    };

    let result_ty = crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id();
    if ty != &result_ty {
        return Err(NativeMirError::new(
            ty.as_str(),
            "computed String-key owner-chain SetAnyResult ABI requires the exact Result<owner-v2 root, i32> TypeDesc",
        ));
    }
    let cfg = function
        .ownership
        .ordinary_map_owned_any_v2_cfg_receipt()
        .ok_or_else(|| {
            NativeMirError::new(
                function.owner.0.clone(),
                "computed String-key owner-chain SetAnyResult ABI has no Checker CFG receipt",
            )
        })?;
    if cfg.owner != function.owner {
        return Err(NativeMirError::new(
            function.owner.0.clone(),
            "computed String-key owner-chain SetAnyResult CFG owner differs from its MIR function",
        ));
    }
    let branches = cfg
        .result_receipts
        .iter()
        .filter_map(|result| {
            result
                .computed_string_key_owner_chain
                .as_deref()
                .and_then(|chain| chain.result_branch.as_deref().map(|branch| (chain, branch)))
        })
        .collect::<Vec<_>>();
    let [(chain, branch)] = branches.as_slice() else {
        return Err(NativeMirError::new(
            function.owner.0.clone(),
            "computed String-key owner-chain SetAnyResult ABI requires one exact Checker branch receipt",
        ));
    };
    if chain.owner != function.owner
        || branch.owner != function.owner
        || chain.profile_version != ChainVersion::V1
        || branch.profile_version != BranchVersion::V1
        || chain.map_new.kind != ActionKind::New
        || chain.map_set.kind != ActionKind::Set
        || branch.map_new_operation != chain.map_new.point
        || branch.map_set_operation != chain.map_set.point
        || branch.initial_root != chain.map_new.local
        || branch.populated_root != chain.map_set.local
        || chain.map_set.source_local.as_ref() != Some(&branch.initial_root)
        || chain.map_set.key.is_none()
        || chain.map_set.value.is_none()
    {
        return Err(NativeMirError::new(
            function.owner.0.clone(),
            "computed String-key owner-chain SetAnyResult ABI is outside the exact New/Set branch identities",
        ));
    }
    let root_ty = crate::core::mir::types::ordinary_map_owned_any_v2_type_id();
    let error_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    native_exact_ordinary_map_result_variant_abi(catalog, ty, &root_ty, &error_ty)
}

/// Native ABI for the independent three-path MapSetDropControl Result
/// profile. It reads only that profile's Checker branch receipt and never
/// borrows the computed-string owner-chain receipt.
pub(super) fn native_ordinary_map_set_drop_control_result_variant_abi(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<(NativeVariantAbi, crate::core::ResolvedTypeId), NativeMirError> {
    use crate::core::ownership::OrdinaryMapOwnedAnyV2MapSetDropControlResultBranchProfileVersion as BranchVersion;

    let result_ty = crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id();
    if ty != &result_ty {
        return Err(NativeMirError::new(
            ty.as_str(),
            "MapSetDropControlResult ABI requires the exact Result<owner-v2 root, i32> TypeDesc",
        ));
    }
    let cfg = function
        .ownership
        .ordinary_map_owned_any_v2_cfg_receipt()
        .ok_or_else(|| {
            NativeMirError::new(
                function.owner.0.clone(),
                "MapSetDropControlResult ABI has no Checker CFG receipt",
            )
        })?;
    let branch = cfg
        .map_set_drop_control_result_branch_receipt
        .as_ref()
        .ok_or_else(|| {
            NativeMirError::new(
                function.owner.0.clone(),
                "MapSetDropControlResult ABI has no independent Checker branch receipt",
            )
        })?;
    if cfg.owner != function.owner
        || branch.owner != function.owner
        || branch.profile_version != BranchVersion::V1
        || branch.map_new_operation == branch.map_set_operation
        || branch.initial_root == branch.populated_root
    {
        return Err(NativeMirError::new(
            function.owner.0.clone(),
            "MapSetDropControlResult ABI is outside its exact Checker owner/New/Set receipt",
        ));
    }
    let root_ty = crate::core::mir::types::ordinary_map_owned_any_v2_type_id();
    let error_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    native_exact_ordinary_map_result_variant_abi(catalog, ty, &root_ty, &error_ty)
}

/// Physical `{ i8, i64, i32 }` layout for only the independent
/// MapSetDropControl Result-branch receipt.
pub(super) fn native_ordinary_map_set_drop_control_result_llvm_type<'ctx>(
    context: &'ctx Context,
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<BasicTypeEnum<'ctx>, NativeMirError> {
    let (variant_abi, _) =
        native_ordinary_map_set_drop_control_result_variant_abi(function, catalog, ty)?;
    let mut fields = vec![context.i8_type().into()];
    fields.extend(
        variant_abi
            .payload_types
            .iter()
            .map(|payload| native_basic_type(context, catalog, payload))
            .collect::<Result<Vec<_>, _>>()?,
    );
    Ok(context.struct_type(&fields, false).into())
}

pub(super) fn native_ordinary_map_result_branch_v1_variant_abi(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<(NativeVariantAbi, crate::core::ResolvedTypeId), NativeMirError> {
    let result_ty = crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id();
    if ty != &result_ty {
        return Err(NativeMirError::new(
            ty.as_str(),
            "ResultBranchV1 ABI requires the exact Result<owner-v2 root, i32> TypeDesc",
        ));
    }
    crate::core::mir::validate_ordinary_map_owned_any_v2_result_branch_v1_types(catalog, function)
        .map_err(|message| NativeMirError::new(function.owner.0.clone(), message))?;
    let root_ty = crate::core::mir::types::ordinary_map_owned_any_v2_type_id();
    let error_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    native_exact_ordinary_map_result_variant_abi(catalog, ty, &root_ty, &error_ty)
}

pub(super) fn native_ordinary_map_result_branch_v1_llvm_type<'ctx>(
    context: &'ctx Context,
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<BasicTypeEnum<'ctx>, NativeMirError> {
    let (variant_abi, _) = native_ordinary_map_result_branch_v1_variant_abi(function, catalog, ty)?;
    let mut fields = vec![context.i8_type().into()];
    fields.extend(
        variant_abi
            .payload_types
            .iter()
            .map(|payload| native_basic_type(context, catalog, payload))
            .collect::<Result<Vec<_>, _>>()?,
    );
    Ok(context.struct_type(&fields, false).into())
}

/// Physical `{ i8, root-handle, i32 }` carrier for only the exact
/// computed String-key owner-chain SetAnyResult branch receipt.
pub(super) fn native_ordinary_map_computed_string_key_owner_chain_set_any_result_llvm_type<'ctx>(
    context: &'ctx Context,
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<BasicTypeEnum<'ctx>, NativeMirError> {
    let (variant_abi, _) =
        native_ordinary_map_computed_string_key_owner_chain_set_any_result_variant_abi(
            function, catalog, ty,
        )?;
    let mut fields = vec![context.i8_type().into()];
    fields.extend(
        variant_abi
            .payload_types
            .iter()
            .map(|payload| native_basic_type(context, catalog, payload))
            .collect::<Result<Vec<_>, _>>()?,
    );
    Ok(context.struct_type(&fields, false).into())
}

/// Physical `{ i8, i64, i32 }` layout authorized by the independent
/// Checker SetLineage receipt.
pub(super) fn native_ordinary_map_set_result_llvm_type<'ctx>(
    context: &'ctx Context,
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<BasicTypeEnum<'ctx>, NativeMirError> {
    let (variant_abi, _) = native_ordinary_map_set_result_variant_abi(function, catalog, ty)?;
    let mut fields = vec![context.i8_type().into()];
    fields.extend(
        variant_abi
            .payload_types
            .iter()
            .map(|payload| native_basic_type(context, catalog, payload))
            .collect::<Result<Vec<_>, _>>()?,
    );
    Ok(context.struct_type(&fields, false).into())
}

/// Native Result ABI for the composite fallible New/Set/GetAny/unpack-i32
/// lineage. Its authorization is bound only to the dedicated composite
/// Checker receipt, even though the physical Result carrier matches the
/// SetResult profile.
pub(super) fn native_ordinary_map_set_get_any_i32_result_variant_abi(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<(NativeVariantAbi, crate::core::ResolvedTypeId), NativeMirError> {
    let receipt = function
        .ownership
        .ordinary_map_set_get_any_i32_lineage_receipt()
        .ok_or_else(|| {
            NativeMirError::new(
                function.owner.0.clone(),
                "native Set/GetAny-i32 Result ABI has no composite Checker receipt",
            )
        })?;
    let expected_result = crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id();
    if receipt.owner != function.owner || ty != &expected_result {
        return Err(NativeMirError::new(
            function.owner.0.clone(),
            "native Set/GetAny-i32 Result ABI is outside its exact composite Checker receipt",
        ));
    }
    catalog
        .validate_ordinary_map_owned_any_v2_root_result(ty, &receipt.new_result_type)
        .map_err(|message| NativeMirError::new(function.owner.0.clone(), message))?;
    catalog
        .validate_ordinary_map_owned_any_v2_root_result(ty, &receipt.set_result_type)
        .map_err(|message| NativeMirError::new(function.owner.0.clone(), message))?;

    let descriptor = catalog.get(ty).ok_or_else(|| {
        NativeMirError::new(ty.as_str(), "Set/GetAny-i32 Result TypeDesc is absent")
    })?;
    let MirLayout::Result {
        ok,
        error,
        variants,
    } = &descriptor.layout
    else {
        return Err(NativeMirError::new(
            ty.as_str(),
            "native Set/GetAny-i32 TypeDesc is not a Result layout",
        ));
    };
    let expected_ok = crate::core::mir::types::ordinary_map_owned_any_v2_type_id();
    let expected_error = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    if descriptor.id != *ty
        || descriptor.kind != MirTypeKind::Result
        || ok != &expected_ok
        || error != &expected_error
        || variants.len() != 2
    {
        return Err(NativeMirError::new(
            ty.as_str(),
            "native Set/GetAny-i32 Result TypeDesc is not exactly Result<MapRoot, i32>",
        ));
    }
    let mut payload_fields = Vec::with_capacity(2);
    for (name, expected_type, physical_field) in [("Ok", ok, 1_u32), ("Err", error, 2_u32)] {
        let variant_id = crate::core::NodeId(format!("builtin:variant:Result::{name}"));
        let (_, variant) = catalog
            .validated_variant_switch_case(ty, &variant_id)
            .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
        let field = match variant.fields.as_slice() {
            [field] if field.ty == *expected_type => field,
            _ => {
                return Err(NativeMirError::new(
                    ty.as_str(),
                    format!("Set/GetAny-i32 {name} payload differs from its canonical TypeDesc"),
                ));
            }
        };
        payload_fields.push(NativeVariantPayloadSlot {
            variant: variant_id,
            field: field.id.clone(),
            physical_field,
            ty: field.ty.clone(),
        });
    }
    Ok((
        NativeVariantAbi {
            tag_field: 0,
            payload_field: 1,
            payload_types: vec![ok.clone(), error.clone()],
            payload_fields,
        },
        ok.clone(),
    ))
}

/// Physical `{ i8, i64, i32 }` carrier for the exact composite
/// Set/GetAny-i32 receipt. This helper has no fallback to SetResult,
/// SetGet-i32, CheckedGet, or a generic Result ABI.
pub(super) fn native_ordinary_map_set_get_any_i32_result_llvm_type<'ctx>(
    context: &'ctx Context,
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<BasicTypeEnum<'ctx>, NativeMirError> {
    let (variant_abi, _) =
        native_ordinary_map_set_get_any_i32_result_variant_abi(function, catalog, ty)?;
    let mut fields = vec![context.i8_type().into()];
    fields.extend(
        variant_abi
            .payload_types
            .iter()
            .map(|payload| native_basic_type(context, catalog, payload))
            .collect::<Result<Vec<_>, _>>()?,
    );
    Ok(context.struct_type(&fields, false).into())
}

/// Native ABI for the fallible `map_get_any` composite lineage's Map-root
/// `Result`. This shares the physical Map-root carrier with the older
/// Set/GetAny-i32 profile, but its authorization is bound only to the
/// Result-valued GetAny receipt.
pub(super) fn native_ordinary_map_set_get_any_result_i32_root_variant_abi(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<(NativeVariantAbi, crate::core::ResolvedTypeId), NativeMirError> {
    let receipt = function
        .ownership
        .ordinary_map_owned_any_v2_set_get_any_result_i32_lineage_receipt()
        .ok_or_else(|| {
            NativeMirError::new(
                function.owner.0.clone(),
                "native Set/GetAny-Result-i32 root ABI has no dedicated Checker receipt",
            )
        })?;
    if receipt.owner != function.owner
        || ty != &crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id()
    {
        return Err(NativeMirError::new(
            function.owner.0.clone(),
            "native Set/GetAny-Result-i32 root ABI is outside its exact Checker receipt",
        ));
    }
    crate::core::mir::validate_ordinary_map_set_get_any_result_i32_result_types(function, catalog)
        .map_err(|message| NativeMirError::new(function.owner.0.clone(), message))?;
    let root_ty = crate::core::mir::types::ordinary_map_owned_any_v2_type_id();
    let error_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    native_exact_ordinary_map_result_variant_abi(catalog, ty, &root_ty, &error_ty)
}

/// Native ABI for `Result<(bool, V2-owned Any), i32>` returned by the
/// fallible GetAny instruction. This envelope has its own TypeDesc and may be
/// materialized only under its exact source receipt.
pub(super) fn native_ordinary_map_set_get_any_result_i32_variant_abi(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<(NativeVariantAbi, crate::core::ResolvedTypeId), NativeMirError> {
    let receipt = function
        .ownership
        .ordinary_map_owned_any_v2_set_get_any_result_i32_lineage_receipt()
        .ok_or_else(|| {
            NativeMirError::new(
                function.owner.0.clone(),
                "native GetAny Result ABI has no dedicated Checker receipt",
            )
        })?;
    let expected_ty =
        crate::core::mir::types::ordinary_map_owned_any_v2_get_any_result_envelope_type_id();
    if receipt.owner != function.owner || ty != &expected_ty {
        return Err(NativeMirError::new(
            function.owner.0.clone(),
            "native GetAny Result ABI is outside its exact Checker receipt",
        ));
    }
    crate::core::mir::validate_ordinary_map_set_get_any_result_i32_result_types(function, catalog)
        .map_err(|message| NativeMirError::new(function.owner.0.clone(), message))?;
    catalog
        .validate_ordinary_map_owned_any_v2_get_any_result_envelope(
            ty,
            crate::core::mir::types::MirGlueOperation::Drop,
        )
        .map_err(|message| NativeMirError::new(function.owner.0.clone(), message))?;
    let pair_ty = crate::core::mir::types::ordinary_map_owned_any_v2_get_result_type_id();
    let error_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    native_exact_ordinary_map_result_variant_abi(catalog, ty, &pair_ty, &error_ty)
}

fn native_exact_ordinary_map_result_variant_abi(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
    expected_ok: &crate::core::ResolvedTypeId,
    expected_error: &crate::core::ResolvedTypeId,
) -> Result<(NativeVariantAbi, crate::core::ResolvedTypeId), NativeMirError> {
    let descriptor = catalog
        .get(ty)
        .ok_or_else(|| NativeMirError::new(ty.as_str(), "exact Map Result TypeDesc is absent"))?;
    let MirLayout::Result {
        ok,
        error,
        variants,
    } = &descriptor.layout
    else {
        return Err(NativeMirError::new(
            ty.as_str(),
            "exact Map Result TypeDesc does not use a Result layout",
        ));
    };
    if descriptor.id != *ty
        || descriptor.kind != MirTypeKind::Result
        || ok != expected_ok
        || error != expected_error
        || variants.len() != 2
    {
        return Err(NativeMirError::new(
            ty.as_str(),
            "exact Map Result TypeDesc has unexpected Ok/Err payload identities",
        ));
    }
    let mut payload_fields = Vec::with_capacity(2);
    for (name, expected_type, physical_field) in [("Ok", ok, 1_u32), ("Err", error, 2_u32)] {
        let variant_id = crate::core::NodeId(format!("builtin:variant:Result::{name}"));
        let (_, variant) = catalog
            .validated_variant_switch_case(ty, &variant_id)
            .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
        let field = match variant.fields.as_slice() {
            [field] if field.ty == *expected_type => field,
            _ => {
                return Err(NativeMirError::new(
                    ty.as_str(),
                    format!("Map Result {name} field differs from its exact TypeDesc"),
                ));
            }
        };
        payload_fields.push(NativeVariantPayloadSlot {
            variant: variant_id,
            field: field.id.clone(),
            physical_field,
            ty: field.ty.clone(),
        });
    }
    Ok((
        NativeVariantAbi {
            tag_field: 0,
            payload_field: 1,
            payload_types: vec![ok.clone(), error.clone()],
            payload_fields,
        },
        ok.clone(),
    ))
}

/// Physical `{ i8, root, i32 }` carrier for the Result-valued GetAny
/// profile's persistent Map roots.
pub(super) fn native_ordinary_map_set_get_any_result_i32_root_llvm_type<'ctx>(
    context: &'ctx Context,
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<BasicTypeEnum<'ctx>, NativeMirError> {
    let (variant_abi, _) =
        native_ordinary_map_set_get_any_result_i32_root_variant_abi(function, catalog, ty)?;
    let mut fields = vec![context.i8_type().into()];
    fields.extend(
        variant_abi
            .payload_types
            .iter()
            .map(|payload| native_basic_type(context, catalog, payload))
            .collect::<Result<Vec<_>, _>>()?,
    );
    Ok(context.struct_type(&fields, false).into())
}

/// Physical `{ i8, (bool, V2 Any), i32 }` GetAny Result carrier.
pub(super) fn native_ordinary_map_set_get_any_result_i32_llvm_type<'ctx>(
    context: &'ctx Context,
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<BasicTypeEnum<'ctx>, NativeMirError> {
    let (variant_abi, _) =
        native_ordinary_map_set_get_any_result_i32_variant_abi(function, catalog, ty)?;
    let mut fields = vec![context.i8_type().into()];
    fields.extend(
        variant_abi
            .payload_types
            .iter()
            .map(|payload| native_basic_type(context, catalog, payload))
            .collect::<Result<Vec<_>, _>>()?,
    );
    Ok(context.struct_type(&fields, false).into())
}

/// Physical Set/Get-String result ABIs authorized by the independent String
/// lineage receipt. The root result is the same `{i8, i64, i32}` carrier as
/// SetResult, but its authority is deliberately kept separate from the i32
/// and Set-only receipts. The detached String result uses the canonical
/// move-owned `Result<String, i32>` ABI.
pub(super) fn native_ordinary_map_set_get_string_result_variant_abi(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<(NativeVariantAbi, crate::core::ResolvedTypeId), NativeMirError> {
    let receipt = function
        .ownership
        .ordinary_map_set_get_string_lineage_receipt()
        .ok_or_else(|| {
            NativeMirError::new(
                function.owner.0.clone(),
                "native Set/Get-String ABI has no independent Checker lineage receipt",
            )
        })?;
    if receipt.owner != function.owner {
        return Err(NativeMirError::new(
            function.owner.0.clone(),
            "native Set/Get-String ABI is outside its exact Checker lineage receipt",
        ));
    }
    crate::core::mir::validate_ordinary_map_set_get_string_result_types(function, catalog)
        .map_err(|message| NativeMirError::new(function.owner.0.clone(), message))?;

    if ty == &crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id() {
        catalog
            .validate_ordinary_map_owned_any_v2_root_result(ty, &receipt.set_result_type)
            .map_err(|message| NativeMirError::new(function.owner.0.clone(), message))?;
        let descriptor = catalog.get(ty).ok_or_else(|| {
            NativeMirError::new(ty.as_str(), "Set/Get-String root Result TypeDesc is absent")
        })?;
        let MirLayout::Result {
            ok,
            error,
            variants,
        } = &descriptor.layout
        else {
            return Err(NativeMirError::new(
                ty.as_str(),
                "Set/Get-String root Result TypeDesc is not a Result layout",
            ));
        };
        if ok != &crate::core::mir::types::ordinary_map_owned_any_v2_type_id()
            || error != &crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32)
        {
            return Err(NativeMirError::new(
                ty.as_str(),
                "Set/Get-String root Result payloads are not the exact MapRoot/i32 pair",
            ));
        }
        let mut payload_fields = Vec::with_capacity(2);
        for (name, expected_type, physical_field) in [("Ok", ok, 1_u32), ("Err", error, 2_u32)] {
            let id = crate::core::NodeId(format!("builtin:variant:Result::{name}"));
            let variant = variants
                .iter()
                .find(|variant| variant.id == id)
                .ok_or_else(|| {
                    NativeMirError::new(
                        ty.as_str(),
                        format!("Set/Get-String {name} variant is absent"),
                    )
                })?;
            let field = match variant.fields.as_slice() {
                [field] if field.ty == *expected_type => field,
                _ => {
                    return Err(NativeMirError::new(
                        ty.as_str(),
                        format!(
                            "Set/Get-String {name} payload differs from its canonical TypeDesc"
                        ),
                    ));
                }
            };
            if variant.fields.len() != 1 {
                return Err(NativeMirError::new(
                    ty.as_str(),
                    format!("Set/Get-String {name} variant has a noncanonical payload arity"),
                ));
            }
            payload_fields.push(NativeVariantPayloadSlot {
                variant: id,
                field: field.id.clone(),
                physical_field,
                ty: field.ty.clone(),
            });
        }
        return Ok((
            NativeVariantAbi {
                tag_field: 0,
                payload_field: 1,
                payload_types: vec![ok.clone(), error.clone()],
                payload_fields,
            },
            ok.clone(),
        ));
    }

    if ty == &receipt.get_result_type {
        catalog
            .validate_result_string_i32_variant(ty)
            .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
        return native_variant_abi(catalog, ty, true);
    }

    Err(NativeMirError::new(
        ty.as_str(),
        "native Set/Get-String ABI type is outside its exact String lineage receipt",
    ))
}

pub(super) fn native_ordinary_map_set_get_string_result_llvm_type<'ctx>(
    context: &'ctx Context,
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<BasicTypeEnum<'ctx>, NativeMirError> {
    let (variant_abi, _) =
        native_ordinary_map_set_get_string_result_variant_abi(function, catalog, ty)?;
    let mut fields = vec![context.i8_type().into()];
    fields.extend(
        variant_abi
            .payload_types
            .iter()
            .map(|payload| native_basic_type(context, catalog, payload))
            .collect::<Result<Vec<_>, _>>()?,
    );
    Ok(context.struct_type(&fields, false).into())
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
    native_variant_abi_with_generic_result(catalog, ty, moving, false)
}

/// Materialize a variant ABI for a function whose MIR instance table has
/// already admitted the heterogeneous generic `Result<T, i32|bool>` Copy
/// projection island. The direct concrete variant ABI remains narrow; callers
/// must opt into this helper only after carrying the checker-owned instance
/// identity into their backend context.
pub(super) fn native_variant_abi_with_generic_result(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
    moving: bool,
    allow_generic_result: bool,
) -> Result<(NativeVariantAbi, crate::core::ResolvedTypeId), NativeMirError> {
    let descriptor = catalog
        .get(ty)
        .ok_or_else(|| NativeMirError::new(ty.as_str(), "variant TypeDesc is absent"))?;
    if descriptor.abi == MirAbiClass::Aggregate
        && descriptor.ownership == MirOwnership::Copy
        && !descriptor.has_canonical_copy_noop_metadata()
    {
        return Err(NativeMirError::new(
            ty.as_str(),
            "Copy variant TypeDesc is outside the complete no-op metadata contract",
        ));
    }
    let payload_types = if catalog.is_flow_event_id_enum(ty) {
        // The checker-owned flow event-id enum (R6-1040): one i8 tag plus a
        // single owned-String slot for the compiler-generated Panic payload;
        // zero-payload variants carry no slot.
        let MirLayout::Enum { variants, .. } = &descriptor.layout else {
            return Err(NativeMirError::new(
                ty.as_str(),
                "event-id enum TypeDesc layout changed during native ABI materialization",
            ));
        };
        variants
            .iter()
            .flat_map(|variant| variant.fields.iter().map(|field| field.ty.clone()))
            .collect()
    } else if matches!(descriptor.layout, MirLayout::Enum { .. })
        && descriptor.kind == MirTypeKind::FlowStateSet
        && catalog.validate_multi_target_union_variant(ty).is_ok()
    {
        // The promoted multi-target union contract: one physical slot per
        // variant payload, in name-sorted variant order.  The TypeDesc
        // contract above proved every payload is a Copy scalar or an owned
        // String with complete glue, so the flat struct below can inline
        // each payload without a shared-type requirement.
        let MirLayout::Enum { variants, .. } = &descriptor.layout else {
            return Err(NativeMirError::new(
                ty.as_str(),
                "union TypeDesc layout changed during native ABI materialization",
            ));
        };
        variants
            .iter()
            .flat_map(|variant| variant.fields.iter().map(|field| field.ty.clone()))
            .collect()
    } else if moving {
        native_non_copy_variant_payload_type(catalog, ty)?;
        match &descriptor.layout {
            MirLayout::Option { inner, .. } => vec![inner.clone()],
            MirLayout::Result { ok, error, .. } => vec![ok.clone(), error.clone()],
            layout => {
                return Err(NativeMirError::new(
                    ty.as_str(),
                    format!("native non-Copy variant layout {layout:?} is outside contract"),
                ));
            }
        }
    } else if matches!(descriptor.layout, MirLayout::Result { .. })
        && (catalog.validate_copy_result_scalar_variant(ty).is_ok()
            || (allow_generic_result && generic_result_copy_variant(catalog, ty)))
    {
        let MirLayout::Result { ok, error, .. } = &descriptor.layout else {
            return Err(NativeMirError::new(
                ty.as_str(),
                "Result TypeDesc layout changed during native ABI materialization",
            ));
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
            ));
        }
    };
    let mut payload_fields = Vec::new();
    let union_tagged = matches!(descriptor.layout, MirLayout::Enum { .. })
        && descriptor.kind == MirTypeKind::FlowStateSet;
    let mut union_slot = 1u32;
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
                            ));
                        }
                    }
                } else if union_tagged {
                    // One slot per variant payload in name-sorted order,
                    // matching the payload_types ordering above.
                    let slot = union_slot;
                    union_slot = union_slot.checked_add(1).ok_or_else(|| {
                        NativeMirError::new(ty.as_str(), "union slot index overflows")
                    })?;
                    slot
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

fn generic_result_copy_variant(catalog: &MirTypeCatalog, ty: &crate::core::ResolvedTypeId) -> bool {
    let Some(descriptor) = catalog.get(ty) else {
        return false;
    };
    let MirLayout::Result {
        variants,
        ok,
        error,
        ..
    } = &descriptor.layout
    else {
        return false;
    };
    let Some(selected) = variants.iter().find(|variant| {
        let Some(field) = variant.fields.first() else {
            return false;
        };
        variant.id.0 == "builtin:variant:Result::Ok"
            && variant.name == "Ok"
            && variant.discriminant == 0
            && variant.fields.len() == 1
            && field.ty == *ok
    }) else {
        return false;
    };
    let Some(selected_field) = selected.fields.first() else {
        return false;
    };
    catalog
        .validated_generic_result_scalar_projection_trap_contract(
            ty,
            &selected.id,
            &selected_field.id,
            ok,
        )
        .is_ok()
        && catalog.validate_copy_scalar(error).is_ok()
}

pub(super) fn native_basic_type<'ctx>(
    context: &'ctx Context,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<BasicTypeEnum<'ctx>, NativeMirError> {
    let desc = catalog
        .get(ty)
        .ok_or_else(|| NativeMirError::new(ty.as_str(), "TypeDesc is absent"))?;
    if let Some(ty) = super::actor::actor_descriptor_native_type(context, catalog, ty)? {
        return Ok(ty);
    }
    if matches!(
        desc.abi,
        MirAbiClass::Integer { .. } | MirAbiClass::Float { .. } | MirAbiClass::Bool
    ) && !desc.is_canonical_copy_scalar(true)
    {
        return Err(NativeMirError::new(
            ty.as_str(),
            "scalar TypeDesc is outside the complete Copy scalar TypeDesc contract",
        ));
    }
    if desc.abi == MirAbiClass::Aggregate
        && desc.ownership == MirOwnership::Copy
        && !desc.has_canonical_copy_noop_metadata()
    {
        return Err(NativeMirError::new(
            ty.as_str(),
            "Copy aggregate TypeDesc is outside the complete no-op metadata contract",
        ));
    }
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
            MirLayout::List { .. } => {
                if ty == &crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id() {
                    super::ordinary_map_owned_any_v2::validate_ordinary_map_owned_any_v2_values_list_type(catalog, ty)
                        .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
                }
                Ok(context.ptr_type(inkwell::AddressSpace::default()).into())
            }
            MirLayout::ManagedAnyI32StringV1 => {
                super::ordinary_map_any_get::validate_managed_any_type(catalog, ty)
                    .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
                Ok(context.i64_type().into())
            }
            MirLayout::OrdinaryMapOwnerLifecycleI32StringV1
            | MirLayout::OrdinaryMapOwnedGetI32StringV1
            | MirLayout::OrdinaryMapOwnedAnyV2 => {
                for operation in [
                    MirGlueOperation::MoveOut,
                    MirGlueOperation::Clone,
                    MirGlueOperation::Drop,
                ] {
                    catalog
                        .validate_glue(ty, operation)
                        .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
                }
                Ok(context.i64_type().into())
            }
            MirLayout::OrdinaryMapOwnedAnyV2Payload => {
                if ty != &crate::core::mir::types::ordinary_map_owned_any_v2_payload_type_id() {
                    return Err(NativeMirError::new(
                        ty.as_str(),
                        "opaque DynamicAny payload does not use the distinct canonical V2 TypeDesc",
                    ));
                }
                for operation in [
                    MirGlueOperation::MoveOut,
                    MirGlueOperation::Clone,
                    MirGlueOperation::Drop,
                ] {
                    catalog
                        .validate_glue(ty, operation)
                        .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
                }
                Ok(context.i64_type().into())
            }
            MirLayout::OrdinaryMapOwnedAnyV2ValuesElement => {
                let expected =
                    crate::core::mir::types::ordinary_map_owned_any_v2_values_element_type_id();
                if ty != &expected {
                    return Err(NativeMirError::new(
                        ty.as_str(),
                        "opaque Values element does not use the distinct canonical V2 TypeDesc",
                    ));
                }
                for operation in [
                    MirGlueOperation::MoveOut,
                    MirGlueOperation::Clone,
                    MirGlueOperation::Drop,
                ] {
                    catalog
                        .validate_glue(ty, operation)
                        .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
                }
                Ok(context.i64_type().into())
            }
            MirLayout::MapRoot => {
                catalog
                    .validate_glue(ty, MirGlueOperation::MoveOut)
                    .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
                catalog
                    .validate_glue(ty, MirGlueOperation::Drop)
                    .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
                if catalog.validate_glue(ty, MirGlueOperation::Clone).is_ok() {
                    return Err(NativeMirError::new(
                        ty.as_str(),
                        "canonical MapRoot must not expose generic Clone glue",
                    ));
                }
                Ok(context.i64_type().into())
            }
            MirLayout::MapAnyOwnerV1 => {
                catalog
                    .validate_glue(ty, MirGlueOperation::MoveOut)
                    .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
                catalog
                    .validate_glue(ty, MirGlueOperation::Drop)
                    .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
                if catalog.validate_glue(ty, MirGlueOperation::Clone).is_ok() {
                    return Err(NativeMirError::new(
                        ty.as_str(),
                        "canonical MapAnyOwner v1 must not expose generic Clone glue",
                    ));
                }
                Ok(context.i64_type().into())
            }
            MirLayout::Handle if desc.glue.move_out == MirGlueKind::Session => {
                catalog
                    .validate_session_channel(ty)
                    .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
                Ok(context.i64_type().into())
            }
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
        MirAbiClass::Aggregate
            if ty == &crate::core::mir::types::ordinary_map_owned_any_v2_get_result_type_id() =>
        {
            // This exact product contains one Core-owned DynamicAny token.
            // It is deliberately validated by its dedicated TypeDesc/glue
            // contract instead of the generic scalar/String tuple profile.
            for operation in [
                MirGlueOperation::MoveOut,
                MirGlueOperation::Clone,
                MirGlueOperation::Drop,
            ] {
                catalog
                    .validate_glue(ty, operation)
                    .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
            }
            let expected = MirLayout::Tuple(vec![
                crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Bool),
                crate::core::mir::types::ordinary_map_owned_any_v2_payload_type_id(),
            ]);
            if desc.kind != (MirTypeKind::Tuple { arity: 2 }) || desc.layout != expected {
                return Err(NativeMirError::new(
                    ty.as_str(),
                    "ordinary Map/Any owner-v2 GetAny result TypeDesc is not exact (bool, DynamicAny)",
                ));
            }
            let MirLayout::Tuple(elements) = &desc.layout else {
                return Err(NativeMirError::new(
                    ty.as_str(),
                    "ordinary Map/Any owner-v2 GetAny result has an inconsistent tuple layout",
                ));
            };
            let fields = elements
                .iter()
                .map(|element| native_basic_type(context, catalog, element))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(context.struct_type(&fields, false).into())
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
                            || (matches!(field_desc.layout, MirLayout::List { .. })
                                && catalog.validate_move_owned_list_payload(&field.ty).is_ok())
                            || (matches!(field_desc.layout, MirLayout::Set { .. })
                                && catalog
                                    .validate_set_glue(&field.ty, MirGlueOperation::MoveOut)
                                    .is_ok())
                            || matches!(field_desc.layout, MirLayout::Tuple(_))
                            || (matches!(field_desc.layout, MirLayout::Record { .. })
                                && validate_native_non_copy_record_type(catalog, &field.ty).is_ok())
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
                // The checker-owned flow state-id enum is a pure tag: every
                // variant is zero-payload, so the native value is the i8 tag
                // alone (R6-1040).
                if catalog.is_zero_payload_flow_state_id_enum(ty) {
                    return Ok(context
                        .struct_type(&[BasicTypeEnum::IntType(context.i8_type())], false)
                        .into());
                }
                // A materialized generic Copy Result carries independent Ok
                // and Err payload slots.  The ordinary variant helper keeps
                // the historical one-payload shape for direct variants, but
                // the generic Result receipt has already proved the two-slot
                // ABI and must be reflected in the LLVM aggregate type too.
                let allow_generic_result = matches!(desc.layout, MirLayout::Result { .. })
                    && generic_result_copy_variant(catalog, ty);
                let (variant_abi, _) = native_variant_abi_with_generic_result(
                    catalog,
                    ty,
                    desc.ownership != MirOwnership::Copy,
                    allow_generic_result,
                )?;
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
