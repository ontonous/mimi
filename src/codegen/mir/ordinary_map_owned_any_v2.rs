//! Native consumer for the Checker-receipted ordinary Map/Any owner-v2
//! profile.
//!
//! The runtime root is an opaque generational handle, but payloads are typed
//! by the MIR TypeDesc contract.  The first V2 payload island admits copy
//! `i32` values and one-field concrete records whose `text` field is an owned
//! String (the canonical `Label { text: string }` shape).  Persistent updates
//! borrow their source root and must publish a distinct root handle.

use super::*;
use crate::codegen::abi::ownership::OwnershipClass;
use crate::core::mir::types::MirGlueOperation;

const V2_NEW: &str = "mimi_ordinary_map_owned_any_v2_new";
const V2_CLONE: &str = "mimi_ordinary_map_owned_any_v2_clone";
const V2_REMOVE: &str = "mimi_ordinary_map_owned_any_v2_remove";
const V2_SIZE: &str = "mimi_ordinary_map_owned_any_v2_size";
const V2_CONTAINS: &str = "mimi_ordinary_map_owned_any_v2_contains";
const V2_IS_EMPTY: &str = "mimi_ordinary_map_owned_any_v2_is_empty";
const V2_DROP: &str = "mimi_ordinary_map_owned_any_v2_drop";
const V2_PAYLOAD_NEW: &str = "mimi_any_payload_v2_new";
const V2_PAYLOAD_DROP: &str = "mimi_any_payload_v2_drop";
const V2_PAYLOAD_UNPACK: &str = "mimi_any_payload_v2_unpack";
const V2_SET: &str = "mimi_ordinary_map_owned_any_v2_set";
const V2_GET: &str = "mimi_ordinary_map_owned_any_v2_get";
const V2_VALUES_LIST: &str = "mimi_ordinary_map_owned_any_v2_values_list";

const V2_STATUS_CAPACITY: i32 = 3;
const V2_STATUS_SIZE_OVERFLOW: i32 = 17;
const V2_SIZE_FAILURE_ERROR: &str = "[E0800] ordinary Map/Any owner-v2 Size failed";
const V2_SIZE_OVERFLOW_ERROR: &str = "[E0802] ordinary Map/Any owner-v2 size overflows i32";

fn v2_size_status_error_message(status: i32) -> Option<&'static str> {
    match status {
        0 => None,
        V2_STATUS_SIZE_OVERFLOW => Some(V2_SIZE_OVERFLOW_ERROR),
        _ => Some(V2_SIZE_FAILURE_ERROR),
    }
}

const V2_TYPEDESC_HASH_DOMAIN: &[u8] = b"mimi-managed-any-payload-v2:typedesc-closure:v1\n";
const V2_SELECTION_HASH_DOMAIN: &[u8] = b"mimi-managed-any-payload-v2:admitted-descriptor-set:v1\n";

#[derive(Clone)]
struct V2PayloadAbi<'ctx> {
    ty: crate::core::ResolvedTypeId,
    tag: i32,
    descriptor_hash: [u8; 32],
    size: u64,
    align: u64,
    storage: inkwell::values::PointerValue<'ctx>,
    llvm_ty: BasicTypeEnum<'ctx>,
    class: OwnershipClass,
}

fn v2_update_framed(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn v2_layout_children(layout: &MirLayout) -> Vec<crate::core::ResolvedTypeId> {
    match layout {
        MirLayout::ActorState { fields, .. } | MirLayout::Record { fields, .. } => {
            fields.iter().map(|field| field.ty.clone()).collect()
        }
        MirLayout::Pointer { target } => target.iter().cloned().collect(),
        MirLayout::Tuple(elements) => elements.clone(),
        MirLayout::Option { inner, variants } => std::iter::once(inner.clone())
            .chain(
                variants
                    .iter()
                    .flat_map(|variant| variant.fields.iter().map(|field| field.ty.clone())),
            )
            .collect(),
        MirLayout::Result {
            ok,
            error,
            variants,
        } => std::iter::once(ok.clone())
            .chain(std::iter::once(error.clone()))
            .chain(
                variants
                    .iter()
                    .flat_map(|variant| variant.fields.iter().map(|field| field.ty.clone())),
            )
            .collect(),
        MirLayout::Enum { variants, .. } => variants
            .iter()
            .flat_map(|variant| variant.fields.iter().map(|field| field.ty.clone()))
            .collect(),
        MirLayout::Array { element, .. }
        | MirLayout::List { element }
        | MirLayout::Set { element } => vec![element.clone()],
        MirLayout::Newtype { inner, .. } => vec![inner.clone()],
        MirLayout::Unit
        | MirLayout::Scalar
        | MirLayout::Handle
        | MirLayout::MapRoot
        | MirLayout::MapAnyOwnerV1
        | MirLayout::OrdinaryMapOwnerLifecycleI32StringV1
        | MirLayout::OrdinaryMapOwnedGetI32StringV1
        | MirLayout::OrdinaryMapOwnedAnyV2
        | MirLayout::OrdinaryMapOwnedAnyV2Payload
        // Native Values descriptors use `List { element }`, which records the
        // element edge above; this dedicated profile tag has no child edge.
        | MirLayout::OrdinaryMapOwnedAnyV2Values
        | MirLayout::OrdinaryMapOwnedAnyV2ValuesElement
        | MirLayout::ManagedAnyI32StringV1
        | MirLayout::Opaque => Vec::new(),
    }
}

fn v2_descriptor_closure<'a>(
    catalog: &'a MirTypeCatalog,
    root: &crate::core::ResolvedTypeId,
) -> Result<Vec<&'a MirTypeDesc>, String> {
    let mut pending = vec![root.clone()];
    let mut seen = BTreeSet::new();
    let mut closure = Vec::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        let descriptor = catalog
            .get(&id)
            .ok_or_else(|| format!("payload TypeDesc '{}' is absent", id.as_str()))?;
        if descriptor.id != id {
            return Err(format!(
                "payload TypeDesc key '{}' differs from embedded identity",
                id.as_str()
            ));
        }
        let mut children = BTreeSet::new();
        if let Some(resolved) = catalog.resolved_type_children(&id) {
            children.extend(resolved.iter().cloned());
        } else if id.as_str().starts_with("rt:") {
            return Err(format!(
                "payload TypeDesc '{}' has no sealed recursive child inventory",
                id.as_str()
            ));
        }
        children.extend(v2_layout_children(&descriptor.layout));
        if let Some(protocol) = &descriptor.session_protocol {
            children.insert(protocol.clone());
        }
        if let Some(plan) = &descriptor.drop_plan {
            children.extend(plan.fields.iter().map(|field| field.ty.clone()));
        }
        if let Some(plans) = &descriptor.variant_drop_plan {
            children.extend(
                plans
                    .iter()
                    .flat_map(|plan| plan.fields.iter().map(|field| field.ty.clone())),
            );
        }
        pending.extend(children.into_iter().rev());
        closure.push(descriptor);
    }
    closure.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(closure)
}

fn v2_descriptor_hash(
    catalog: &MirTypeCatalog,
    root: &crate::core::ResolvedTypeId,
) -> Result<[u8; 32], String> {
    let closure = v2_descriptor_closure(catalog, root)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(V2_TYPEDESC_HASH_DOMAIN);
    for descriptor in closure {
        v2_update_framed(&mut hasher, descriptor.id.as_str().as_bytes());
        v2_update_framed(&mut hasher, descriptor.canonical_text().as_bytes());
        let checker_children = catalog
            .resolved_type_children(&descriptor.id)
            .map(|children| children.to_vec())
            .unwrap_or_default();
        v2_update_framed(&mut hasher, b"checker");
        hasher.update(&(checker_children.len() as u64).to_be_bytes());
        for child in checker_children {
            v2_update_framed(&mut hasher, child.as_str().as_bytes());
        }
        let layout_children = v2_layout_children(&descriptor.layout);
        v2_update_framed(&mut hasher, b"layout");
        hasher.update(&(layout_children.len() as u64).to_be_bytes());
        for child in layout_children {
            v2_update_framed(&mut hasher, child.as_str().as_bytes());
        }
        let protocols = descriptor
            .session_protocol
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        v2_update_framed(&mut hasher, b"session-protocol");
        hasher.update(&(protocols.len() as u64).to_be_bytes());
        for child in protocols {
            v2_update_framed(&mut hasher, child.as_str().as_bytes());
        }
        let drop_children = descriptor
            .drop_plan
            .iter()
            .flat_map(|plan| plan.fields.iter().map(|field| field.ty.clone()))
            .collect::<Vec<_>>();
        v2_update_framed(&mut hasher, b"drop-plan");
        hasher.update(&(drop_children.len() as u64).to_be_bytes());
        for child in drop_children {
            v2_update_framed(&mut hasher, child.as_str().as_bytes());
        }
        let variant_children = descriptor
            .variant_drop_plan
            .iter()
            .flat_map(|plans| {
                plans
                    .iter()
                    .flat_map(|plan| plan.fields.iter().map(|field| field.ty.clone()))
            })
            .collect::<Vec<_>>();
        v2_update_framed(&mut hasher, b"variant-drop-plans");
        hasher.update(&(variant_children.len() as u64).to_be_bytes());
        for child in variant_children {
            v2_update_framed(&mut hasher, child.as_str().as_bytes());
        }
    }
    Ok(*hasher.finalize().as_bytes())
}

pub(super) fn v2_values_is_label_type(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> bool {
    let Some(descriptor) = catalog.get(ty) else {
        return false;
    };
    let MirLayout::Record { fields, .. } = &descriptor.layout else {
        return false;
    };
    let Some(string_ty) = catalog.iter().find_map(|(candidate, descriptor)| {
        (descriptor.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::String))
            .then(|| candidate.clone())
    }) else {
        return false;
    };
    descriptor.id == *ty
        && fields.len() == 1
        && fields[0].name == "text"
        && fields[0].ty == string_ty
        && catalog.validate_owned_string(&string_ty).is_ok()
        && validate_native_non_copy_record_type(catalog, ty).is_ok()
}

fn v2_payload_tag(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<i32, String> {
    let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    if ty == &i32_type && catalog.validate_copy_scalar(ty).is_ok() {
        return Ok(1);
    }
    let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
    if ty == &string_type && catalog.validate_owned_string(ty).is_ok() {
        return Ok(2);
    }
    if v2_values_is_label_type(catalog, ty) {
        return Ok(5);
    }
    Err(format!(
        "ordinary Map/Any owner-v2 selector type '{}' is outside the admitted i32/String/Label{{text:string}} payload profile",
        ty.as_str()
    ))
}

fn values_selection_receipt_error(message: String) -> String {
    if message.contains("no Checker-authorized Set payload type")
        || message.contains("no Checker-selected Set payload TypeDescs")
        || message.contains("no AnyOwner-v2 payload selection")
    {
        "ordinary Map/Any owner-v2 Values cannot admit an empty payload selection without an exact Core empty-selection and empty-root receipt; Checker/MIR must extend that proof before native can support values(map_new())".into()
    } else {
        message
    }
}

/// Validate the native ABI contract for the dedicated Values List. Its
/// storage is a real MimiList, while the source element identity and glue are
/// kept distinct from both V1 ManagedAny and the GetAny payload token.
pub(super) fn validate_ordinary_map_owned_any_v2_values_list_type(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<(), String> {
    let expected_type = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
    let expected_element =
        crate::core::mir::types::ordinary_map_owned_any_v2_values_element_type_id();
    if ty != &expected_type {
        return Err(
            "ordinary Map/Any owner-v2 Values has a noncanonical List TypeDesc identity".into(),
        );
    }
    let descriptor = catalog
        .get(ty)
        .ok_or_else(|| "ordinary Map/Any owner-v2 Values List TypeDesc is absent".to_string())?;
    if descriptor.id != expected_type
        || descriptor.kind != MirTypeKind::OrdinaryMapOwnedAnyV2Values
        || descriptor.layout
            != (MirLayout::List {
                element: expected_element.clone(),
            })
        || descriptor.abi != MirAbiClass::OpaqueHandle
        || descriptor.ownership != MirOwnership::Move
        || !descriptor.needs_drop_glue
        || !descriptor.needs_clone_glue
        || descriptor.glue
            != (crate::core::mir::types::MirGlueContract {
                move_out: MirGlueKind::OrdinaryMapOwnedAnyV2Values,
                clone: MirGlueKind::OrdinaryMapOwnedAnyV2Values,
                drop: MirGlueKind::OrdinaryMapOwnedAnyV2Values,
            })
        || descriptor.session_protocol.is_some()
        || descriptor.drop_plan.is_some()
        || descriptor.variant_drop_plan.is_some()
    {
        return Err(
            "ordinary Map/Any owner-v2 Values List does not have the dedicated native owner contract".into(),
        );
    }
    let element = catalog
        .get(&expected_element)
        .ok_or_else(|| "ordinary Map/Any owner-v2 Values element TypeDesc is absent".to_string())?;
    if element.id != expected_element
        || element.kind != MirTypeKind::OrdinaryMapOwnedAnyV2ValuesElement
        || element.layout != MirLayout::OrdinaryMapOwnedAnyV2ValuesElement
        || element.abi != MirAbiClass::OpaqueHandle
        || element.ownership != MirOwnership::Move
        || !element.needs_drop_glue
        || !element.needs_clone_glue
        || element.glue
            != (crate::core::mir::types::MirGlueContract {
                move_out: MirGlueKind::OrdinaryMapOwnedAnyV2ValuesElement,
                clone: MirGlueKind::OrdinaryMapOwnedAnyV2ValuesElement,
                drop: MirGlueKind::OrdinaryMapOwnedAnyV2ValuesElement,
            })
        || element.session_protocol.is_some()
        || element.drop_plan.is_some()
        || element.variant_drop_plan.is_some()
    {
        return Err(
            "ordinary Map/Any owner-v2 Values element does not have its distinct V2 owner contract"
                .into(),
        );
    }
    for operation in [
        MirGlueOperation::MoveOut,
        MirGlueOperation::Clone,
        MirGlueOperation::Drop,
    ] {
        catalog.validate_glue(ty, operation)?;
        catalog.validate_glue(&expected_element, operation)?;
    }
    Ok(())
}

/// Build the exact runtime selector frame from the Core-selected payload set.
/// The Core receipt digest and runtime descriptor-registry hash are separate
/// authorities and are both checked by their respective layers.
pub(super) fn ordinary_map_owned_any_v2_values_selector_frame(
    catalog: &MirTypeCatalog,
    selection: &crate::core::mir::OrdinaryMapOwnedAnyV2PayloadSelectionReceipt,
) -> Result<(Vec<u8>, [u8; 32]), String> {
    selection.validate()?;
    if selection.selected_types.is_empty() {
        // A map may legitimately be empty at runtime after Remove, but an
        // empty selector is authority-bearing: admit it only after Core
        // provides a receipt proving this exact source root is empty.
        return Err("ordinary Map/Any owner-v2 Values cannot admit an empty selector without an exact Core empty-selection and empty-root receipt".into());
    }
    let count = u32::try_from(selection.selected_types.len())
        .map_err(|_| "ordinary Map/Any owner-v2 Values selector count exceeds u32".to_string())?;
    let mut records = Vec::with_capacity(selection.selected_types.len());
    for selected in &selection.selected_types {
        let ty = &selected.type_id;
        let descriptor = catalog.get(ty).ok_or_else(|| {
            format!(
                "ordinary Map/Any owner-v2 Values selected TypeDesc '{}' is absent",
                ty.as_str()
            )
        })?;
        let descriptor_digest = blake3::hash(descriptor.canonical_text().as_bytes())
            .to_hex()
            .to_string();
        if descriptor.id != *ty || descriptor_digest != selected.type_desc_digest {
            return Err(format!(
                "ordinary Map/Any owner-v2 Values selected TypeDesc '{}' disagrees with its Core digest",
                ty.as_str()
            ));
        }
        let type_id = ty.as_str().as_bytes();
        if type_id.len() != 35
            || !type_id.starts_with(b"rt:")
            || !type_id[3..]
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        {
            return Err(format!(
                "ordinary Map/Any owner-v2 Values selected TypeDesc '{}' has a noncanonical runtime identity",
                ty.as_str()
            ));
        }
        let tag = v2_payload_tag(catalog, ty)?;
        let descriptor_hash = v2_descriptor_hash(catalog, ty)?;
        records.push((type_id.to_vec(), tag, descriptor_hash));
    }
    records.sort_by(|left, right| left.0.cmp(&right.0));
    if records.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err("ordinary Map/Any owner-v2 Values selector repeats a TypeDesc".into());
    }
    let mut frame = Vec::with_capacity(8 + records.len() * (8 + 35 + 4 + 32));
    frame.extend_from_slice(&1_u32.to_be_bytes());
    frame.extend_from_slice(&count.to_be_bytes());
    let mut record_bytes = Vec::new();
    for (type_id, tag, descriptor_hash) in records {
        let mut record = Vec::with_capacity(8 + type_id.len() + 4 + 32);
        record.extend_from_slice(&(type_id.len() as u64).to_be_bytes());
        record.extend_from_slice(&type_id);
        record.extend_from_slice(&tag.to_be_bytes());
        record.extend_from_slice(&descriptor_hash);
        frame.extend_from_slice(&record);
        record_bytes.extend_from_slice(&record);
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(V2_SELECTION_HASH_DOMAIN);
    hasher.update(&record_bytes);
    Ok((frame, *hasher.finalize().as_bytes()))
}

pub(super) fn validate_ordinary_map_owned_any_v2_values_native_contract(
    program: &MirProgram,
) -> Result<(), Vec<String>> {
    use crate::core::mir::MirInstructionKind as Instruction;

    let values_type = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
    let values_element_type =
        crate::core::mir::types::ordinary_map_owned_any_v2_values_element_type_id();
    let instruction_count = program
        .functions()
        .values()
        .flat_map(|function| function.blocks.values())
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            matches!(
                instruction.kind,
                Instruction::OrdinaryMapOwnedAnyV2Values { .. }
            )
        })
        .count();
    let checker_action_count = program
        .functions()
        .values()
        .map(|function| {
            function
                .ownership
                .checker_ordinary_map_owned_any_v2_values_action_count()
        })
        .sum::<usize>();
    let descriptor_signal = program.type_catalog().iter().any(|(id, descriptor)| {
        id == &values_type
            || id == &values_element_type
            || descriptor.kind == MirTypeKind::OrdinaryMapOwnedAnyV2Values
            || descriptor.kind == MirTypeKind::OrdinaryMapOwnedAnyV2ValuesElement
            || descriptor.layout == MirLayout::OrdinaryMapOwnedAnyV2ValuesElement
    });
    if instruction_count == 0 && checker_action_count == 0 && !descriptor_signal {
        return Ok(());
    }
    let receipts = program
        .ordinary_map_owned_any_v2_values_result_receipts()
        .map_err(|message| vec![values_selection_receipt_error(message)])?;
    if receipts.len() != instruction_count || receipts.len() != checker_action_count {
        return Err(vec![format!(
            "Core Values receipt count {} disagrees with MIR instruction count {} and Checker action count {}",
            receipts.len(),
            instruction_count,
            checker_action_count
        )]);
    }
    let selection = program
        .ordinary_map_owned_any_v2_payload_selection_receipt()
        .map_err(|message| vec![values_selection_receipt_error(message)])?
        .ok_or_else(|| {
            vec![values_selection_receipt_error(
                "ordinary Map/Any owner-v2 Values has no Core payload-selection receipt".into(),
            )]
        })?;
    selection
        .validate_against_program(program)
        .map_err(|message| vec![message])?;
    validate_ordinary_map_owned_any_v2_values_list_type(program.type_catalog(), &values_type)
        .map_err(|message| vec![message])?;
    ordinary_map_owned_any_v2_values_selector_frame(program.type_catalog(), &selection)
        .map_err(|message| vec![message])?;

    for receipt in receipts {
        receipt
            .validate()
            .and_then(|()| receipt.validate_against_program(program))
            .map_err(|message| {
                vec![format!(
                    "pre-emission Core Values receipt validation failed: {message}"
                )]
            })?;
        if receipt.payload_selection_digest != selection.selection_digest {
            return Err(vec![format!(
                "Core Values receipt '{}' is bound to another payload-selection digest",
                receipt.instruction
            )]);
        }
        let function = program.functions().get(&receipt.owner).ok_or_else(|| {
            vec![format!(
                "Core Values receipt owner '{}' is absent from MIR",
                receipt.owner.0
            )]
        })?;
        let exact = function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| {
                instruction.id == receipt.instruction
                    && matches!(
                        &instruction.kind,
                        Instruction::OrdinaryMapOwnedAnyV2Values { result, root }
                            if result == &receipt.result && root == &receipt.root
                    )
            })
            .count();
        if exact != 1
            || function.values.get(&receipt.root).map(|value| &value.ty)
                != Some(&crate::core::mir::types::ordinary_map_owned_any_v2_type_id())
            || function.values.get(&receipt.result).map(|value| &value.ty) != Some(&values_type)
            || function.values.get(&receipt.binding).map(|value| &value.ty) != Some(&values_type)
        {
            return Err(vec![format!(
                "Core Values receipt '{}' does not bind one exact native root/result/binding TypeDesc shape",
                receipt.instruction
            )]);
        }
    }
    Ok(())
}

pub(super) fn validate_ordinary_map_owned_any_v2_values_unpack_native_contract(
    program: &MirProgram,
) -> Result<(), Vec<String>> {
    use crate::core::mir::MirInstructionKind as Instruction;

    let instruction_count = program
        .functions()
        .values()
        .flat_map(|function| function.blocks.values())
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            matches!(
                instruction.kind,
                Instruction::OrdinaryMapOwnedAnyV2ValuesUnpack { .. }
            )
        })
        .count();
    let receipts = program
        .ordinary_map_owned_any_v2_values_unpack_result_receipts()
        .map_err(|message| vec![message])?;
    if receipts.len() != instruction_count {
        return Err(vec![format!(
            "Core AnyOwner-v2 Values unpack receipt count {} disagrees with MIR instruction count {}",
            receipts.len(),
            instruction_count
        )]);
    }
    if receipts.is_empty() {
        return Ok(());
    }

    let values_type = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
    let selection = program
        .ordinary_map_owned_any_v2_payload_selection_receipt()
        .map_err(|message| vec![message])?
        .ok_or_else(|| vec!["AnyOwner-v2 Values unpack has no payload-selection receipt".into()])?;
    selection
        .validate_against_program(program)
        .map_err(|message| vec![message])?;
    validate_ordinary_map_owned_any_v2_values_list_type(program.type_catalog(), &values_type)
        .map_err(|message| vec![message])?;
    ordinary_map_owned_any_v2_values_selector_frame(program.type_catalog(), &selection)
        .map_err(|message| vec![message])?;

    let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    let i64_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I64);
    let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
    let catalog = program.type_catalog();
    let snapshot_receipts = program
        .ordinary_map_owned_any_v2_values_result_receipts()
        .map_err(|message| vec![message])?;

    for receipt in receipts {
        receipt
            .validate()
            .and_then(|()| receipt.validate_against_program(program))
            .map_err(|message| {
                vec![format!(
                    "pre-emission Core AnyOwner-v2 Values unpack receipt validation failed: {message}"
                )]
            })?;
        if receipt.payload_selection_digest != selection.selection_digest {
            return Err(vec![format!(
                "AnyOwner-v2 Values unpack '{}' has a stale payload-selection or conversion receipt",
                receipt.instruction
            )]);
        }
        if !matches!(
            catalog.get(&receipt.conversion_from),
            Some(descriptor) if descriptor.kind == MirTypeKind::DynamicAny
        ) {
            return Err(vec![format!(
                "AnyOwner-v2 Values unpack '{}' conversion source is not the exact DynamicAny element TypeDesc",
                receipt.instruction
            )]);
        }
        if receipt.index_type != i32_type && receipt.index_type != i64_type {
            return Err(vec![format!(
                "AnyOwner-v2 Values unpack '{}' index TypeDesc is not signed i32 or i64",
                receipt.instruction
            )]);
        }
        if catalog.get(&receipt.index_type).is_none_or(|descriptor| {
            descriptor.kind
                != MirTypeKind::Primitive(if receipt.index_type == i32_type {
                    crate::core::PrimitiveType::I32
                } else {
                    crate::core::PrimitiveType::I64
                })
                || descriptor.layout != MirLayout::Scalar
        }) || catalog.validate_copy_scalar(&receipt.index_type).is_err()
        {
            return Err(vec![format!(
                "AnyOwner-v2 Values unpack '{}' index is not a canonical signed copy scalar",
                receipt.instruction
            )]);
        }
        let target = catalog.get(&receipt.result_type).ok_or_else(|| {
            vec![format!(
                "AnyOwner-v2 Values unpack '{}' target TypeDesc is absent",
                receipt.instruction
            )]
        })?;
        let target_is_i32 = receipt.result_type == i32_type
            && target.id == i32_type
            && target.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::I32)
            && target.layout == MirLayout::Scalar
            && catalog.validate_copy_scalar(&i32_type).is_ok();
        let target_is_string = receipt.result_type == string_type
            && target.id == string_type
            && target.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::String)
            && catalog.validate_owned_string(&string_type).is_ok();
        let target_is_label = v2_values_is_label_type(catalog, &receipt.result_type);
        if !target_is_i32 && !target_is_string && !target_is_label {
            return Err(vec![format!(
                "AnyOwner-v2 Values unpack '{}' target is not canonical i32, owned String, or Label {{ text: string }}",
                receipt.instruction
            )]);
        }
        for operation in [MirGlueOperation::Clone, MirGlueOperation::Drop] {
            catalog
                .validate_glue(&receipt.result_type, operation)
                .map_err(|message| {
                    vec![format!(
                        "AnyOwner-v2 Values unpack '{}' target glue is invalid: {message}",
                        receipt.instruction
                    )]
                })?;
        }
        let target_digest = blake3::hash(target.canonical_text().as_bytes())
            .to_hex()
            .to_string();
        if receipt.payload_type_desc_digest != target_digest
            || !selection.selected_types.iter().any(|selected| {
                selected.type_id == receipt.result_type
                    && selected.type_desc_digest == target_digest
            })
        {
            return Err(vec![format!(
                "AnyOwner-v2 Values unpack '{}' target is not the exact selected Set payload TypeDesc",
                receipt.instruction
            )]);
        }

        let function = program.functions().get(&receipt.owner).ok_or_else(|| {
            vec![format!(
                "AnyOwner-v2 Values unpack '{}' owner is absent from MIR",
                receipt.instruction
            )]
        })?;
        let exact_instruction_count = function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| {
                instruction.id == receipt.instruction
                    && matches!(
                        &instruction.kind,
                        Instruction::OrdinaryMapOwnedAnyV2ValuesUnpack {
                            result,
                            values,
                            index,
                            projection,
                        } if result == &receipt.result
                            && values == &receipt.values
                            && index == &receipt.index
                            && projection == &receipt.projection
                    )
            })
            .count();
        let values_type_matches =
            function.values.get(&receipt.values).map(|value| &value.ty) == Some(&values_type);
        let index_type_matches =
            function.values.get(&receipt.index).map(|value| &value.ty) == Some(&receipt.index_type);
        let result_type_matches = function.values.get(&receipt.result).map(|value| &value.ty)
            == Some(&receipt.result_type);
        let direct_moves = function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| {
                matches!(
                    &instruction.kind,
                    Instruction::Move { result, source }
                        if result == &receipt.result_binding && source == &receipt.result
                )
            })
            .count();
        let result_binding_type = function
            .values
            .get(&receipt.result_binding)
            .map(|value| &value.ty);
        let settlement_drops = function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| {
                instruction.id == receipt.settlement_instruction
                    && matches!(
                        &instruction.kind,
                        Instruction::Drop { value } if value == &receipt.values
                    )
            })
            .count();
        let matching_snapshots = snapshot_receipts
            .iter()
            .filter(|snapshot| {
                snapshot.owner == receipt.owner
                    && snapshot.operation == receipt.operation
                    && snapshot.binding_local == receipt.values_binding_local
                    && snapshot.binding == receipt.values
                    && snapshot.binding_point == receipt.values_binding_point
                    && snapshot.settlement_point == receipt.settlement_point
                    && snapshot.settlement_instruction == receipt.settlement_instruction
                    && snapshot.settlement
                        == crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
            })
            .count();
        if exact_instruction_count != 1
            || !values_type_matches
            || !index_type_matches
            || !result_type_matches
            || direct_moves != 1
            || result_binding_type != Some(&receipt.result_type)
            || settlement_drops != 1
            || matching_snapshots != 1
        {
            return Err(vec![format!(
                "AnyOwner-v2 Values unpack '{}' does not bind one exact native snapshot/index/result, direct result Move, and receipted snapshot Drop",
                receipt.instruction
            )]);
        }
    }
    Ok(())
}

pub(super) fn validate_ordinary_map_owned_any_v2_unpack_native_contract(
    program: &MirProgram,
) -> Result<(), Vec<String>> {
    use crate::core::mir::MirInstructionKind as Instruction;

    let instruction_count = program
        .functions()
        .values()
        .flat_map(|function| function.blocks.values())
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            matches!(
                instruction.kind,
                Instruction::OrdinaryMapOwnedAnyV2Unpack { .. }
            )
        })
        .count();
    let receipts = program
        .ordinary_map_owned_any_v2_unpack_result_receipts()
        .map_err(|message| vec![message])?;
    if receipts.len() != instruction_count {
        return Err(vec![format!(
            "Core AnyOwner-v2 unpack receipt count {} disagrees with MIR instruction count {}",
            receipts.len(),
            instruction_count
        )]);
    }
    if receipts.is_empty() {
        return Ok(());
    }

    let selection = program
        .ordinary_map_owned_any_v2_payload_selection_receipt()
        .map_err(|message| vec![message])?
        .ok_or_else(|| vec!["AnyOwner-v2 unpack has no payload-selection receipt".into()])?;
    selection
        .validate_against_program(program)
        .map_err(|message| vec![message])?;
    let get_any_receipts = program
        .ordinary_map_owned_any_v2_get_any_result_receipts()
        .map_err(|message| vec![message])?;
    let catalog = program.type_catalog();
    let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
    let pair_type = crate::core::mir::types::ordinary_map_owned_any_v2_get_result_type_id();

    for receipt in receipts {
        receipt
            .validate()
            .and_then(|()| receipt.validate_against_program(program))
            .map_err(|message| {
                vec![format!(
                    "pre-emission Core AnyOwner-v2 unpack receipt validation failed: {message}"
                )]
            })?;
        let target_type = &receipt.result_type;
        if receipt.result_type != receipt.conversion_to
            || receipt.settlement
                != crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
            || receipt.payload_selection_digest != selection.selection_digest
        {
            return Err(vec![format!(
                "AnyOwner-v2 unpack '{}' is outside the native i32/explicit-Drop receipt profile",
                receipt.instruction
            )]);
        }
        let target = catalog.get(target_type).ok_or_else(|| {
            vec![format!(
                "AnyOwner-v2 unpack '{}' has no target TypeDesc",
                receipt.instruction
            )]
        })?;
        let target_is_i32 = target_type == &i32_type
            && target.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::I32)
            && target.layout == MirLayout::Scalar
            && catalog.validate_copy_scalar(target_type).is_ok();
        let target_is_string = target_type == &string_type
            && target.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::String)
            && catalog.validate_owned_string(target_type).is_ok();
        let target_is_label = v2_values_is_label_type(catalog, target_type);
        if !target_is_i32 && !target_is_string && !target_is_label {
            return Err(vec![format!(
                "AnyOwner-v2 unpack '{}' target is not canonical i32, owned String, or Label {{ text: string }}",
                receipt.instruction
            )]);
        }
        for operation in [MirGlueOperation::Clone, MirGlueOperation::Drop] {
            catalog
                .validate_glue(target_type, operation)
                .map_err(|message| {
                    vec![format!(
                        "AnyOwner-v2 unpack '{}' target glue is invalid: {message}",
                        receipt.instruction
                    )]
                })?;
        }
        let target_digest = blake3::hash(target.canonical_text().as_bytes())
            .to_hex()
            .to_string();
        let selected = selection.selected_types.iter().any(|selected| {
            selected.type_id == *target_type && selected.type_desc_digest == target_digest
        });
        let canonical_miss = target_is_i32
            && get_any_receipts.iter().any(|get_any| {
                get_any.owner == receipt.owner
                    && get_any.operation == receipt.operation
                    && get_any.miss_payload_type == i32_type
                    && get_any.miss_payload_type_desc_digest == target_digest
            });
        let receipt_source_matches = match &receipt.payload_source {
            crate::core::mir::OrdinaryMapOwnedAnyV2UnpackPayloadSource::SelectedSetPayload => {
                selected
            }
            crate::core::mir::OrdinaryMapOwnedAnyV2UnpackPayloadSource::CanonicalI32Miss => {
                canonical_miss
            }
        };
        if !receipt_source_matches {
            return Err(vec![format!(
                "AnyOwner-v2 unpack '{}' payload-source receipt does not authorize the exact selected payload or canonical i32 miss TypeDesc",
                receipt.instruction
            )]);
        }
        let function = program.functions().get(&receipt.owner).ok_or_else(|| {
            vec![format!(
                "AnyOwner-v2 unpack '{}' owner is absent from MIR",
                receipt.owner.0
            )]
        })?;
        let exact_instruction_count = function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| {
                instruction.id == receipt.instruction
                    && matches!(
                        &instruction.kind,
                        Instruction::OrdinaryMapOwnedAnyV2Unpack { result, pair, projection }
                            if result == &receipt.result
                                && pair == &receipt.pair
                                && projection == &receipt.projection
                    )
            })
            .count();
        if exact_instruction_count != 1
            || function.values.get(&receipt.result).map(|value| &value.ty) != Some(target_type)
            || function.values.get(&receipt.pair).map(|value| &value.ty) != Some(&pair_type)
        {
            return Err(vec![format!(
                "AnyOwner-v2 unpack '{}' does not bind one exact native pair/result TypeDesc shape",
                receipt.instruction
            )]);
        }
    }
    Ok(())
}

fn v2_native_alignment(ty: BasicTypeEnum<'_>) -> u64 {
    match ty {
        BasicTypeEnum::IntType(int) => {
            u64::from(int.get_bit_width().div_ceil(8)).next_power_of_two()
        }
        BasicTypeEnum::FloatType(float) => {
            u64::from(float.get_bit_width().div_ceil(8)).next_power_of_two()
        }
        // Mimi's native StringBox and Label ABI use the host pointer-sized
        // slot; this is the same alignment contract used by existing native
        // aggregate glue.
        BasicTypeEnum::PointerType(_) => 8,
        BasicTypeEnum::StructType(structure) => structure
            .get_field_types()
            .iter()
            .map(|field| v2_native_alignment(*field))
            .max()
            .unwrap_or(1),
        BasicTypeEnum::ArrayType(array) => v2_native_alignment(array.get_element_type()),
        BasicTypeEnum::VectorType(vector) => (v2_native_alignment(vector.get_element_type())
            * u64::from(vector.get_size()))
        .next_power_of_two()
        .min(32),
        BasicTypeEnum::ScalableVectorType(_) => 8,
    }
}

impl<'a, 'ctx> NativeMirFunctionEmitter<'a, 'ctx> {
    fn validate_v2_values_receipt(
        &self,
        result: &MirValueId,
        root: &MirValueId,
        subject: &str,
    ) -> Result<
        (
            crate::core::mir::OrdinaryMapOwnedAnyV2ValuesResultReceipt,
            crate::core::mir::OrdinaryMapOwnedAnyV2PayloadSelectionReceipt,
        ),
        NativeMirError,
    > {
        let selection = self
            .program
            .ordinary_map_owned_any_v2_payload_selection_receipt()
            .map_err(|message| {
                NativeMirError::new(subject, values_selection_receipt_error(message))
            })?
            .ok_or_else(|| {
                NativeMirError::new(
                    subject,
                    values_selection_receipt_error(
                        "ordinary Map/Any owner-v2 Values has no Core payload-selection receipt"
                            .into(),
                    ),
                )
            })?;
        selection
            .validate_against_program(self.program)
            .map_err(|message| NativeMirError::new(subject, message))?;

        let receipts = self
            .program
            .ordinary_map_owned_any_v2_values_result_receipts()
            .map_err(|message| {
                NativeMirError::new(subject, values_selection_receipt_error(message))
            })?;
        let matching = receipts
            .iter()
            .filter(|receipt| {
                receipt.owner == self.function.owner && receipt.instruction.as_str() == subject
            })
            .collect::<Vec<_>>();
        let [receipt] = matching.as_slice() else {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Values has no unique Core result receipt for this instruction",
            ));
        };
        receipt
            .validate()
            .and_then(|()| receipt.validate_against_program(self.program))
            .map_err(|message| NativeMirError::new(subject, message))?;

        let expected_root = format!("local:{}", receipt.source_local.0 .0);
        let expected_binding = format!("local:{}", receipt.binding_local.0 .0);
        let expected_instruction = format!(
            "inst:ordinary_map_owned_any_v2_values:{}:{}",
            expected_root, receipt.operation.0
        );
        let values_type = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
        if receipt.owner != self.function.owner
            || receipt.instruction.as_str() != subject
            || receipt.instruction.as_str() != expected_instruction
            || receipt.root != *root
            || root.as_str() != expected_root
            || receipt.result != *result
            || receipt.binding.as_str() != expected_binding
            || receipt.mir_result_type != values_type
            || receipt.payload_selection_digest != selection.selection_digest
            || receipt.settlement
                != crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
        {
            return Err(NativeMirError::new(
                subject,
                format!(
                    "ordinary Map/Any owner-v2 Values receipt identity mismatch: owner={} expected_owner={}; instruction={} expected_instruction={}; root={} expected_root={}; result={} expected_result={}; binding={} expected_binding={}; result_type_matches={}; selection_digest_matches={}; explicit_drop={}",
                    receipt.owner.0,
                    self.function.owner.0,
                    receipt.instruction,
                    expected_instruction,
                    receipt.root,
                    root,
                    receipt.result,
                    result,
                    receipt.binding,
                    expected_binding,
                    receipt.mir_result_type == values_type,
                    receipt.payload_selection_digest == selection.selection_digest,
                    receipt.settlement
                        == crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop,
                ),
            ));
        }
        let catalog = self.program.type_catalog();
        validate_ordinary_map_owned_any_v2_values_list_type(catalog, &values_type)
            .map_err(|message| NativeMirError::new(subject, message))?;
        if self.value_type(root, subject)?
            != crate::core::mir::types::ordinary_map_owned_any_v2_type_id()
            || self.value_type(result, subject)? != values_type
            || self
                .function
                .values
                .get(&receipt.binding)
                .map(|value| &value.ty)
                != Some(&values_type)
            || catalog.get(&receipt.checker_source_type).is_none()
        {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Values receipt source/result/binding TypeDesc identities disagree with the native MIR operands",
            ));
        }
        let checker_result = catalog.get(&receipt.checker_result_type).ok_or_else(|| {
            NativeMirError::new(subject, "Checker List<DynamicAny> TypeDesc is absent")
        })?;
        let MirLayout::List { element } = &checker_result.layout else {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Values Checker result is not List<DynamicAny>",
            ));
        };
        if checker_result.kind != MirTypeKind::List
            || catalog
                .get(element)
                .is_none_or(|descriptor| descriptor.kind != MirTypeKind::DynamicAny)
        {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Values Checker result is not exact List<DynamicAny>",
            ));
        }
        let exact_instruction_count = self
            .function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| {
                instruction.id.as_str() == subject
                    && matches!(
                        &instruction.kind,
                        MirInstructionKind::OrdinaryMapOwnedAnyV2Values {
                            result: observed_result,
                            root: observed_root,
                        } if observed_result == result && observed_root == root
                    )
            })
            .count();
        let direct_moves = self
            .function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Move {
                        result: moved,
                        source,
                    } if moved == &receipt.binding && source == result
                )
            })
            .count();
        let settlement_drops = self
            .function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| {
                instruction.id == receipt.settlement_instruction
                    && matches!(
                        &instruction.kind,
                        MirInstructionKind::Drop { value } if value == &receipt.binding
                    )
            })
            .count();
        if exact_instruction_count != 1 || direct_moves != 1 || settlement_drops != 1 {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Values is missing its exact MIR operation, direct binding Move, or receipted explicit Drop",
            ));
        }
        ordinary_map_owned_any_v2_values_selector_frame(catalog, &selection)
            .map_err(|message| NativeMirError::new(subject, message))?;
        Ok(((**receipt).clone(), selection))
    }

    fn validate_v2_get_any_receipt(
        &self,
        result: &MirValueId,
        root: &MirValueId,
        key: &MirValueId,
        subject: &str,
    ) -> Result<crate::core::mir::OrdinaryMapOwnedAnyV2GetAnyResultReceipt, NativeMirError> {
        let selection = self
            .program
            .ordinary_map_owned_any_v2_payload_selection_receipt()
            .map_err(|message| NativeMirError::new(subject, message))?
            .ok_or_else(|| {
                NativeMirError::new(
                    subject,
                    "ordinary Map/Any owner-v2 GetAny has no Checker payload selection receipt",
                )
            })?;
        selection
            .validate_against_program(self.program)
            .map_err(|message| NativeMirError::new(subject, message))?;

        let receipts = self
            .program
            .ordinary_map_owned_any_v2_get_any_result_receipts()
            .map_err(|message| NativeMirError::new(subject, message))?;
        let matching = receipts
            .iter()
            .filter(|receipt| {
                receipt.owner == self.function.owner && receipt.instruction.as_str() == subject
            })
            .collect::<Vec<_>>();
        let [receipt] = matching.as_slice() else {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 GetAny has no unique Core result receipt for this instruction",
            ));
        };
        receipt
            .validate()
            .and_then(|()| receipt.validate_against_program(self.program))
            .map_err(|message| NativeMirError::new(subject, message))?;

        let expected_root = format!("local:{}", receipt.source_local.0 .0);
        let expected_binding = format!("local:{}", receipt.binding_local.0 .0);
        let expected_instruction = format!(
            "inst:ordinary_map_owned_any_v2_get_any:{}:{}",
            expected_root, receipt.operation.0
        );
        if receipt.owner != self.function.owner
            || receipt.instruction.as_str() != subject
            || receipt.instruction.as_str() != expected_instruction.as_str()
            || receipt.root != *root
            || root.as_str() != expected_root.as_str()
            || receipt.result != *result
            || receipt.key != *key
            || receipt.binding.as_str() != expected_binding.as_str()
            || receipt.settlement
                != crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
        {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 GetAny receipt does not bind the exact owner, source local, instruction, MIR operands, result binding, and ExplicitDrop settlement",
            ));
        }

        let root_type = self.value_type(root, subject)?;
        let key_type = self.value_type(key, subject)?;
        let result_type = self.value_type(result, subject)?;
        let expected_key_type =
            crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        let expected_result_type =
            crate::core::mir::types::ordinary_map_owned_any_v2_get_result_type_id();
        if root_type != crate::core::mir::types::ordinary_map_owned_any_v2_type_id()
            || self
                .program
                .type_catalog()
                .get(&receipt.checker_source_type)
                .is_none()
            || key_type != expected_key_type
            || receipt.checker_key_type != key_type
            || result_type != expected_result_type
            || receipt.mir_result_type != result_type
        {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 GetAny receipt source/key/result TypeDesc identities disagree with the native MIR operands",
            ));
        }

        let catalog = self.program.type_catalog();
        let checker_result = catalog.get(&receipt.checker_result_type).ok_or_else(|| {
            NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 GetAny Checker result TypeDesc is absent",
            )
        })?;
        let bool_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Bool);
        let checker_dynamic_any = match &checker_result.layout {
            MirLayout::Tuple(fields) if fields.len() == 2 && fields[0] == bool_type => &fields[1],
            _ => {
                return Err(NativeMirError::new(
                    subject,
                    "ordinary Map/Any owner-v2 GetAny Checker result is not exact (bool, DynamicAny)",
                ));
            }
        };
        if checker_result.id != receipt.checker_result_type
            || checker_result.kind != (MirTypeKind::Tuple { arity: 2 })
            || catalog
                .get(checker_dynamic_any)
                .is_none_or(|descriptor| descriptor.kind != MirTypeKind::DynamicAny)
        {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 GetAny Checker result is not exact (bool, DynamicAny)",
            ));
        }

        let exact_instructions = self
            .function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| {
                instruction.id.as_str() == subject
                    && matches!(
                        &instruction.kind,
                        MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny {
                            result: observed_result,
                            root: observed_root,
                            key: observed_key,
                        } if observed_result == result
                            && observed_root == root
                            && observed_key == key
                    )
            })
            .count();
        if exact_instructions != 1 {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 GetAny receipt does not identify one exact MIR instruction",
            ));
        }

        let direct_moves = self
            .function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Move {
                        result: moved,
                        source,
                    } if moved == &receipt.binding && source == result
                )
            })
            .count();
        let binding_type = self
            .function
            .values
            .get(&receipt.binding)
            .map(|value| &value.ty);
        let settlement_drops = self
            .function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| {
                instruction.id == receipt.settlement_instruction
                    && matches!(
                        &instruction.kind,
                        MirInstructionKind::Drop { value } if value == &receipt.binding
                    )
            })
            .count();
        if direct_moves != 1 || binding_type != Some(&expected_result_type) || settlement_drops != 1
        {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 GetAny result is not moved to its receipted binding and explicitly dropped at the receipted settlement point",
            ));
        }

        let canonical_i32 = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        let miss_descriptor = catalog.get(&receipt.miss_payload_type).ok_or_else(|| {
            NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 GetAny miss TypeDesc from Core receipt is absent",
            )
        })?;
        let miss_descriptor_digest = blake3::hash(miss_descriptor.canonical_text().as_bytes())
            .to_hex()
            .to_string();
        if receipt.miss_payload_type != canonical_i32
            || miss_descriptor.id != receipt.miss_payload_type
            || miss_descriptor.kind != MirTypeKind::Primitive(crate::core::PrimitiveType::I32)
            || miss_descriptor.layout != MirLayout::Scalar
            || miss_descriptor.ownership != MirOwnership::Copy
            || miss_descriptor_digest != receipt.miss_payload_type_desc_digest
        {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 GetAny miss TypeDesc is not the canonical Core-receipted i32 scalar",
            ));
        }
        catalog
            .validate_copy_scalar(&receipt.miss_payload_type)
            .map_err(|message| NativeMirError::new(subject, message))?;

        Ok((**receipt).clone())
    }

    fn validate_v2_root(&self, value: &MirValueId, subject: &str) -> Result<(), NativeMirError> {
        let ty = self.value_type(value, subject)?;
        if ty != crate::core::mir::types::ordinary_map_owned_any_v2_type_id() {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 operand does not use its canonical TypeDesc",
            ));
        }
        self.program
            .type_catalog()
            .validate_glue(&ty, MirGlueOperation::MoveOut)
            .map_err(|message| NativeMirError::new(subject, message))
    }

    fn emit_v2_root_guard(
        &mut self,
        condition: inkwell::values::IntValue<'ctx>,
        message: &str,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        self.emit_v2_root_guard_with_key_cleanup(condition, message, subject, None)
    }

    fn emit_v2_root_guard_with_key_cleanup(
        &mut self,
        condition: inkwell::values::IntValue<'ctx>,
        message: &str,
        subject: &str,
        key_data: Option<inkwell::values::PointerValue<'ctx>>,
    ) -> Result<(), NativeMirError> {
        let valid = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_map_any_v2_valid");
        let invalid = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_map_any_v2_invalid");
        self.generator
            .builder
            .build_conditional_branch(condition, valid, invalid)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(invalid);
        if let Some(key_data) = key_data {
            self.emit_v2_string_free(key_data, subject)?;
        }
        self.emit_abort_with_message(message, subject)?;
        self.generator.builder.position_at_end(valid);
        Ok(())
    }

    /// Trap after releasing a malformed or failing Values-index temporary
    /// owner when one was actually published by the list getter.
    fn emit_v2_temporary_owner_abort(
        &mut self,
        owner: inkwell::values::IntValue<'ctx>,
        message: &str,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let owner_is_zero = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                owner,
                self.generator.context.i64_type().const_zero(),
                "mir_map_any_v2_values_unpack_abort_owner_zero",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let no_owner = self.generator.context.append_basic_block(
            self.llvm_function,
            "mir_map_any_v2_values_unpack_abort_no_owner",
        );
        let release_owner = self.generator.context.append_basic_block(
            self.llvm_function,
            "mir_map_any_v2_values_unpack_abort_release_owner",
        );
        self.generator
            .builder
            .build_conditional_branch(owner_is_zero, no_owner, release_owner)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(release_owner);
        let drop_fn = self
            .generator
            .get_runtime_fn(V2_PAYLOAD_DROP)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(
                drop_fn,
                &[BasicMetadataValueEnum::from(owner)],
                "mir_map_any_v2_values_unpack_abort_owner_drop",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_abort_with_message(message, subject)?;
        self.generator.builder.position_at_end(no_owner);
        self.emit_abort_with_message(message, subject)
    }

    fn emit_v2_string_free(
        &mut self,
        key_data: inkwell::values::PointerValue<'ctx>,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let free_fn = self
            .generator
            .get_runtime_fn("mimi_string_free")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(
                free_fn,
                &[BasicMetadataValueEnum::from(key_data)],
                "mir_map_any_v2_key_drop",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        Ok(())
    }

    /// The private Checker action distinguishes an existing local borrowed
    /// for the byte-span call from a compiler-owned temporary. Do not infer
    /// this from later Drops: a borrowed local may itself be dropped at scope
    /// exit.
    fn v2_key_is_owned_temporary(
        &self,
        key: &MirValueId,
        subject: &str,
    ) -> Result<bool, NativeMirError> {
        let instruction = crate::core::mir::MirInstructionId::new(subject.to_owned())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let Some((kind, key_node, key_local, key_type, checked_key)) = self
            .function
            .ownership
            .ordinary_map_owned_any_v2_key_receipt(&instruction)
        else {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 key has no unique private Checker action",
            ));
        };
        let key_kind = matches!(
            kind,
            crate::core::OrdinaryMapOwnedAnyV2ActionKind::Set
                | crate::core::OrdinaryMapOwnedAnyV2ActionKind::Remove
                | crate::core::OrdinaryMapOwnedAnyV2ActionKind::HasKey
                | crate::core::OrdinaryMapOwnedAnyV2ActionKind::GetAny
        );
        let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        if !key_kind || &checked_key != key || key_type != string_type {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 key does not match its exact Checker source/type receipt",
            ));
        }
        if let Some(local) = key_local.as_ref() {
            let expected = MirValueId::new(format!("local:{}", local.0 .0))
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            if *key != expected || !self.v2_key_is_borrowed_local(key) {
                return Err(NativeMirError::new(
                    subject,
                    "ordinary Map/Any owner-v2 Checker-borrowed key is not its exact local String",
                ));
            }
            return Ok(false);
        }
        let source_temporary = MirValueId::new(format!("expr:{}", key_node.0)).ok();
        if source_temporary.as_ref() != Some(key) {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 unborrowed key is not the exact checked String expression temporary",
            ));
        }
        Ok(true)
    }

    fn v2_key_has_normal_drop_after(&self, key: &MirValueId, subject: &str) -> bool {
        let drops = self
            .function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| {
                matches!(&instruction.kind, MirInstructionKind::Drop { value } if value == key)
            })
            .count();
        if drops != 1 {
            return false;
        }
        self.function.blocks.values().any(|block| {
            let call_index = block.instructions.iter().position(|instruction| {
                instruction.id.as_str() == subject
                    && matches!(
                        &instruction.kind,
                        MirInstructionKind::OrdinaryMapOwnedAnyV2Set { key: observed, .. }
                            | MirInstructionKind::OrdinaryMapOwnedAnyV2Remove { key: observed, .. }
                            | MirInstructionKind::OrdinaryMapOwnedAnyV2Contains { key: observed, .. }
                            | MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { key: observed, .. }
                            if observed == key
                    )
            });
            let drop_index = block.instructions.iter().position(|instruction| {
                matches!(&instruction.kind, MirInstructionKind::Drop { value } if value == key)
            });
            matches!((call_index, drop_index), (Some(call), Some(drop)) if drop == call + 1)
        })
    }

    fn v2_key_is_borrowed_local(&self, key: &MirValueId) -> bool {
        key.as_str().starts_with("local:")
            && self.function.values.get(key).is_some_and(|value| {
                self.program
                    .type_catalog()
                    .get(&value.ty)
                    .is_some_and(|descriptor| {
                        descriptor.kind
                            == MirTypeKind::Primitive(crate::core::PrimitiveType::String)
                    })
            })
    }

    fn emit_live_v2_root(
        &mut self,
        value: &MirValueId,
        subject: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, NativeMirError> {
        self.emit_live_v2_root_with_key_cleanup(value, subject, None)
    }

    fn emit_live_v2_root_with_key_cleanup(
        &mut self,
        value: &MirValueId,
        subject: &str,
        key_data: Option<inkwell::values::PointerValue<'ctx>>,
    ) -> Result<inkwell::values::IntValue<'ctx>, NativeMirError> {
        self.validate_v2_root(value, subject)?;
        let handle = self.value(value, subject)?.into_int_value();
        let nonzero = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                handle,
                self.generator.context.i64_type().const_zero(),
                "mir_map_any_v2_root_nonzero",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_v2_root_guard_with_key_cleanup(
            nonzero,
            "[E0800] ordinary Map/Any owner-v2 root is invalid",
            subject,
            key_data,
        )?;
        Ok(handle)
    }

    fn v2_string_parts(
        &mut self,
        value: &MirValueId,
        role: &str,
        subject: &str,
    ) -> Result<
        (
            inkwell::values::PointerValue<'ctx>,
            inkwell::values::IntValue<'ctx>,
        ),
        NativeMirError,
    > {
        let ty = self.value_type(value, subject)?;
        let descriptor = self.program.type_catalog().get(&ty).ok_or_else(|| {
            NativeMirError::new(
                subject,
                format!("ordinary Map/Any owner-v2 {role} TypeDesc is absent"),
            )
        })?;
        if descriptor.kind != MirTypeKind::Primitive(crate::core::PrimitiveType::String) {
            return Err(NativeMirError::new(
                subject,
                format!("ordinary Map/Any owner-v2 {role} is not canonical String"),
            ));
        }
        self.program
            .type_catalog()
            .validate_owned_string(&ty)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let string = self.value(value, subject)?.into_struct_value();
        let data = self
            .generator
            .builder
            .build_extract_value(string, 0, "mir_map_any_v2_string_data")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_pointer_value();
        let len = self
            .generator
            .builder
            .build_extract_value(string, 1, "mir_map_any_v2_string_len")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        Ok((data, len))
    }

    fn v2_primitive_type(
        &self,
        primitive: crate::core::PrimitiveType,
        subject: &str,
    ) -> Result<crate::core::ResolvedTypeId, NativeMirError> {
        self.program
            .type_catalog()
            .iter()
            .find_map(|(ty, descriptor)| {
                (descriptor.kind == MirTypeKind::Primitive(primitive)).then(|| ty.clone())
            })
            .ok_or_else(|| {
                NativeMirError::new(
                    subject,
                    format!("canonical {primitive:?} TypeDesc is absent"),
                )
            })
    }

    /// Extract the one supported concrete record payload as a borrowed label
    /// descriptor.  The alloca has the MIR record LLVM type, and the runtime's
    /// `repr(C)` label view has the same two fields (`ptr`, `i64`).
    fn v2_label_pointer(
        &mut self,
        value: &MirValueId,
        subject: &str,
    ) -> Result<inkwell::values::PointerValue<'ctx>, NativeMirError> {
        let ty = self.value_type(value, subject)?;
        let descriptor = self.program.type_catalog().get(&ty).ok_or_else(|| {
            NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 record TypeDesc is absent",
            )
        })?;
        let MirLayout::Record { fields, .. } = &descriptor.layout else {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 payload is not a concrete record",
            ));
        };
        let string_ty = self.v2_primitive_type(crate::core::PrimitiveType::String, subject)?;
        if fields.len() != 1 || fields[0].name != "text" || fields[0].ty != string_ty {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 record payload must be Label { text: string }",
            ));
        }
        validate_native_non_copy_record_type(self.program.type_catalog(), &ty)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let record_ty =
            native_basic_type(self.generator.context, self.program.type_catalog(), &ty)?;
        let slot = self
            .generator
            .builder
            .build_alloca(record_ty, "mir_map_any_v2_label_slot")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(slot, self.value(value, subject)?)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        Ok(slot)
    }

    fn emit_v2_result_root(
        &mut self,
        handle: inkwell::values::IntValue<'ctx>,
        source: &MirValueId,
        subject: &str,
        operation: &str,
        key_cleanup: Option<inkwell::values::PointerValue<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let nonzero = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                handle,
                self.generator.context.i64_type().const_zero(),
                "mir_map_any_v2_result_nonzero",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_v2_root_guard_with_key_cleanup(
            nonzero,
            "[E0800] ordinary Map/Any owner-v2 allocation failed",
            subject,
            key_cleanup,
        )?;
        let source_handle = self.value(source, subject)?.into_int_value();
        let distinct = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                handle,
                source_handle,
                "mir_map_any_v2_result_distinct",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_v2_root_guard_with_key_cleanup(
            distinct,
            &format!(
                "[E0800] ordinary Map/Any owner-v2 persistent {operation} aliased its source root"
            ),
            subject,
            key_cleanup,
        )?;
        Ok(handle.into())
    }

    pub(super) fn emit_ordinary_map_owned_any_v2_new(
        &mut self,
        result: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        self.validate_v2_root(result, subject)?;
        let new_fn = self
            .generator
            .get_runtime_fn(V2_NEW)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let out_slot = self
            .generator
            .builder
            .build_alloca(self.generator.context.i64_type(), "mir_map_any_v2_new_out")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(out_slot, self.generator.context.i64_type().const_zero())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        // The exact V2 ABI reports status separately from the output root.  A
        // nonzero status must trap before the out slot is observed.
        let status = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    new_fn,
                    &[BasicMetadataValueEnum::from(out_slot)],
                    "mir_map_any_v2_new_status",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "ordinary Map/Any owner-v2 New returned void"))?
        .into_int_value();
        let status_ok = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.generator.context.i32_type().const_zero(),
                "mir_map_any_v2_new_status_ok",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_v2_root_guard(
            status_ok,
            "[E0800] ordinary Map/Any owner-v2 New failed",
            subject,
        )?;
        let handle = self
            .generator
            .builder
            .build_load(
                self.generator.context.i64_type(),
                out_slot,
                "mir_map_any_v2_new_value",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let nonzero = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                handle,
                self.generator.context.i64_type().const_zero(),
                "mir_map_any_v2_new_nonzero",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_v2_root_guard(
            nonzero,
            "[E0800] ordinary Map/Any owner-v2 allocation failed",
            subject,
        )?;
        Ok(handle.into())
    }

    pub(super) fn emit_ordinary_map_owned_any_v2_clone(
        &mut self,
        result: &MirValueId,
        source: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        self.validate_v2_root(result, subject)?;
        let source_handle = self.emit_live_v2_root(source, subject)?;
        let clone_fn = self
            .generator
            .get_runtime_fn(V2_CLONE)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let out_slot = self
            .generator
            .builder
            .build_alloca(
                self.generator.context.i64_type(),
                "mir_map_any_v2_clone_out",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(out_slot, self.generator.context.i64_type().const_zero())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let status = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    clone_fn,
                    &[
                        BasicMetadataValueEnum::from(source_handle),
                        BasicMetadataValueEnum::from(out_slot),
                    ],
                    "mir_map_any_v2_clone_status",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| {
            NativeMirError::new(subject, "ordinary Map/Any owner-v2 Clone returned void")
        })?
        .into_int_value();
        let status_ok = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.generator.context.i32_type().const_zero(),
                "mir_map_any_v2_clone_status_ok",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_v2_root_guard(
            status_ok,
            "[E0800] ordinary Map/Any owner-v2 Clone failed",
            subject,
        )?;
        let handle = self
            .generator
            .builder
            .build_load(
                self.generator.context.i64_type(),
                out_slot,
                "mir_map_any_v2_clone_value",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        self.emit_v2_result_root(handle, source, subject, "Clone", None)
    }

    fn emit_v2_bytes_global(
        &mut self,
        bytes: &[u8],
        name: &str,
        subject: &str,
    ) -> Result<inkwell::values::PointerValue<'ctx>, NativeMirError> {
        let byte_count = u32::try_from(bytes.len()).map_err(|_| {
            NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 receipt exceeds LLVM array limit",
            )
        })?;
        let i8 = self.generator.context.i8_type();
        let array_ty = i8.array_type(byte_count);
        if let Some(global) = self.generator.module.get_global(name) {
            if global.get_value_type() != array_ty.into()
                || global.get_linkage() != inkwell::module::Linkage::Private
                || !global.is_constant()
            {
                return Err(NativeMirError::new(
                    subject,
                    format!(
                        "reserved AnyOwner V2 receipt global `{name}` has an incompatible shape"
                    ),
                ));
            }
            return Ok(global.as_pointer_value());
        }
        let values = bytes
            .iter()
            .map(|byte| i8.const_int(u64::from(*byte), false))
            .collect::<Vec<_>>();
        let global = self.generator.module.add_global(array_ty, None, name);
        global.set_initializer(&i8.const_array(&values));
        global.set_constant(true);
        global.set_alignment(1);
        global.set_linkage(inkwell::module::Linkage::Private);
        Ok(global.as_pointer_value())
    }

    fn v2_is_label_type(&self, ty: &crate::core::ResolvedTypeId) -> bool {
        v2_values_is_label_type(self.program.type_catalog(), ty)
    }

    fn emit_v2_payload_abi(
        &mut self,
        value: &MirValueId,
        subject: &str,
    ) -> Result<V2PayloadAbi<'ctx>, NativeMirError> {
        let ty = self.value_type(value, subject)?;
        let catalog = self.program.type_catalog();
        let i32_ty = self.v2_primitive_type(crate::core::PrimitiveType::I32, subject)?;
        let string_ty = self.v2_primitive_type(crate::core::PrimitiveType::String, subject)?;
        let (tag, class, llvm_ty, storage) = if ty == i32_ty {
            catalog
                .validate_copy_scalar(&ty)
                .map_err(|message| NativeMirError::new(subject, message))?;
            let llvm_ty = BasicTypeEnum::IntType(self.generator.context.i32_type());
            let storage = self
                .generator
                .builder
                .build_alloca(
                    self.generator.context.i32_type(),
                    "mir_map_any_v2_i32_payload",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            self.generator
                .builder
                .build_store(storage, self.value(value, subject)?)
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            (1, OwnershipClass::Scalar, llvm_ty, storage)
        } else if ty == string_ty {
            catalog
                .validate_owned_string(&ty)
                .map_err(|message| NativeMirError::new(subject, message))?;
            let llvm_ty = native_basic_type(self.generator.context, catalog, &ty)?;
            let storage = self
                .generator
                .builder
                .build_alloca(llvm_ty, "mir_map_any_v2_string_payload")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            // This is a borrowed ABI view. The AnyOwner V2 callback clones it
            // into its private allocation, while the source MIR value keeps
            // its ordinary ownership and later Drop.
            self.generator
                .builder
                .build_store(storage, self.value(value, subject)?)
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            (2, OwnershipClass::StringBox, llvm_ty, storage)
        } else if self.v2_is_label_type(&ty) {
            let llvm_ty = native_basic_type(self.generator.context, catalog, &ty)?;
            let storage = self.v2_label_pointer(value, subject)?;
            (
                5,
                OwnershipClass::Record(vec![OwnershipClass::StringBox]),
                llvm_ty,
                storage,
            )
        } else {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Set payload is outside the admitted i32/String/Label{text:string} profile",
            ));
        };
        let size = self.generator.llvm_type_size_bytes(llvm_ty);
        let align = v2_native_alignment(llvm_ty);
        if size == 0 || align == 0 {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 payload has zero native size or alignment",
            ));
        }
        let descriptor_hash = v2_descriptor_hash(catalog, &ty)
            .map_err(|message| NativeMirError::new(subject, message))?;
        Ok(V2PayloadAbi {
            ty,
            tag,
            descriptor_hash,
            size,
            align,
            storage,
            llvm_ty,
            class,
        })
    }

    fn emit_v2_selection_receipt(
        &mut self,
        payload_ty: &crate::core::ResolvedTypeId,
        subject: &str,
    ) -> Result<
        (
            inkwell::values::PointerValue<'ctx>,
            inkwell::values::IntValue<'ctx>,
            inkwell::values::PointerValue<'ctx>,
        ),
        NativeMirError,
    > {
        let catalog = self.program.type_catalog();
        // Core owns the closed set of payload types admitted by Checker Set
        // receipts.  The native consumer validates that proof against the
        // same MIR program before using it as the runtime selection table.
        let selection = self
            .program
            .ordinary_map_owned_any_v2_payload_selection_receipt()
            .map_err(|message| NativeMirError::new(subject, message))?
            .ok_or_else(|| {
                NativeMirError::new(
                    subject,
                    "ordinary Map/Any owner-v2 Set has no Checker payload selection receipt",
                )
            })?;
        selection
            .validate_against_program(self.program)
            .map_err(|message| NativeMirError::new(subject, message))?;

        let selected_types = selection
            .selected_types
            .iter()
            .map(|selected| selected.type_id.clone())
            .collect::<BTreeSet<_>>();
        let mut saw_payload = false;
        for instruction in self
            .function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
        {
            let MirInstructionKind::OrdinaryMapOwnedAnyV2Set { value, .. } = &instruction.kind
            else {
                continue;
            };
            saw_payload = true;
            let value_ty = self
                .function
                .values
                .get(value)
                .map(|mir_value| mir_value.ty.clone())
                .ok_or_else(|| {
                    NativeMirError::new(
                        subject,
                        format!(
                            "ordinary Map/Any owner-v2 Set selection is missing payload value `{value}`"
                        ),
                    )
                })?;
            if !selected_types.contains(&value_ty) {
                return Err(NativeMirError::new(
                    subject,
                    "ordinary Map/Any owner-v2 Set payload TypeDesc is absent from the Checker selection receipt",
                ));
            }
        }
        if !saw_payload || !selected_types.contains(payload_ty) {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Set selection is missing its Checker-admitted payload TypeDesc",
            ));
        }
        let mut records = Vec::with_capacity(selection.selected_types.len());
        for selected in &selection.selected_types {
            let ty = &selected.type_id;
            let tag = v2_payload_tag(catalog, ty)
                .map_err(|message| NativeMirError::new(subject, message))?;
            // The Checker receipt chooses the exact type set; runtime ABI
            // metadata remains derived from each canonical MIR TypeDesc.
            let descriptor_hash = v2_descriptor_hash(catalog, ty)
                .map_err(|message| NativeMirError::new(subject, message))?;
            records.push((ty.as_str().as_bytes().to_vec(), tag, descriptor_hash));
        }
        records.sort_by(|left, right| left.0.cmp(&right.0));
        let mut bytes = Vec::with_capacity(8 + records.len() * (8 + 35 + 4 + 32));
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.extend_from_slice(&(records.len() as u32).to_be_bytes());
        let mut record_bytes = Vec::new();
        for (type_id, tag, hash) in &records {
            if type_id.len() != 35
                || !type_id.starts_with(b"rt:")
                || !type_id[3..]
                    .iter()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
            {
                return Err(NativeMirError::new(
                    subject,
                    "ordinary Map/Any owner-v2 selection contains a non-canonical TypeDesc identity",
                ));
            }
            let mut record = Vec::with_capacity(8 + type_id.len() + 4 + 32);
            record.extend_from_slice(&(type_id.len() as u64).to_be_bytes());
            record.extend_from_slice(type_id);
            record.extend_from_slice(&tag.to_be_bytes());
            record.extend_from_slice(hash);
            bytes.extend_from_slice(&record);
            record_bytes.extend_from_slice(&record);
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(V2_SELECTION_HASH_DOMAIN);
        hasher.update(&record_bytes);
        let registry_hash = *hasher.finalize().as_bytes();
        let hash_hex = registry_hash
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let bytes_ptr = self.emit_v2_bytes_global(
            &bytes,
            &format!("__mimi_any_payload_v2_selection_{hash_hex}"),
            subject,
        )?;
        let hash_ptr = self.emit_v2_bytes_global(
            &registry_hash,
            &format!("__mimi_any_payload_v2_hash_{hash_hex}"),
            subject,
        )?;
        Ok((
            bytes_ptr,
            self.generator
                .context
                .i64_type()
                .const_int(bytes.len() as u64, false),
            hash_ptr,
        ))
    }

    fn emit_v2_payload_token(
        &mut self,
        payload: &V2PayloadAbi<'ctx>,
        subject: &str,
        key_data: Option<inkwell::values::PointerValue<'ctx>>,
    ) -> Result<inkwell::values::IntValue<'ctx>, NativeMirError> {
        let type_id_bytes = payload.ty.as_str().as_bytes();
        if type_id_bytes.len() != 35
            || !type_id_bytes.starts_with(b"rt:")
            || !type_id_bytes[3..]
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 payload TypeDesc identity is not canonical",
            ));
        }
        let type_id_name = format!(
            "__mimi_any_payload_v2_type_id_{}",
            native_symbol_fragment(payload.ty.as_str())
        );
        let type_id_ptr = self.emit_v2_bytes_global(type_id_bytes, &type_id_name, subject)?;
        let hash_hex = payload
            .descriptor_hash
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let hash_ptr = self.emit_v2_bytes_global(
            &payload.descriptor_hash,
            &format!("__mimi_any_payload_v2_descriptor_hash_{hash_hex}"),
            subject,
        )?;
        let (clone_fn, drop_fn) = self
            .generator
            .ensure_any_payload_v2_glue(&payload.class, payload.llvm_ty)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let out = self
            .generator
            .builder
            .build_alloca(
                self.generator.context.i64_type(),
                "mir_map_any_v2_payload_owner_out",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(out, self.generator.context.i64_type().const_zero())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let new_fn = self
            .generator
            .get_runtime_fn(V2_PAYLOAD_NEW)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let status = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    new_fn,
                    &[
                        BasicMetadataValueEnum::from(
                            self.generator
                                .context
                                .i32_type()
                                .const_int(payload.tag as u64, false),
                        ),
                        BasicMetadataValueEnum::from(type_id_ptr),
                        BasicMetadataValueEnum::from(
                            self.generator
                                .context
                                .i64_type()
                                .const_int(type_id_bytes.len() as u64, false),
                        ),
                        BasicMetadataValueEnum::from(hash_ptr),
                        BasicMetadataValueEnum::from(payload.storage),
                        BasicMetadataValueEnum::from(
                            self.generator
                                .context
                                .i64_type()
                                .const_int(payload.size, false),
                        ),
                        BasicMetadataValueEnum::from(
                            self.generator
                                .context
                                .i64_type()
                                .const_int(payload.align, false),
                        ),
                        BasicMetadataValueEnum::from(clone_fn.as_global_value().as_pointer_value()),
                        BasicMetadataValueEnum::from(drop_fn.as_global_value().as_pointer_value()),
                        BasicMetadataValueEnum::from(out),
                    ],
                    "mir_map_any_v2_payload_new",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| {
            NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 payload New returned void",
            )
        })?
        .into_int_value();

        // `payload.storage` is a borrowed ABI view over the MIR source value
        // (for Label, a shallow record copy whose String field still points
        // at the source allocation).  The runtime clones it into the private
        // owner; dropping this view would free the source and leave its
        // ordinary String ownership receipt stale.  Keep the MIR source live
        // and let its checked ownership actions perform the one real Drop.
        let status_ok = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.generator.context.i32_type().const_zero(),
                "mir_map_any_v2_payload_new_status_ok",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_v2_root_guard_with_key_cleanup(
            status_ok,
            "[E0800] ordinary Map/Any owner-v2 payload creation failed",
            subject,
            key_data,
        )?;
        let owner = self
            .generator
            .builder
            .build_load(
                self.generator.context.i64_type(),
                out,
                "mir_map_any_v2_payload_owner",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let owner_live = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                owner,
                self.generator.context.i64_type().const_zero(),
                "mir_map_any_v2_payload_owner_nonzero",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_v2_root_guard_with_key_cleanup(
            owner_live,
            "[E0800] ordinary Map/Any owner-v2 payload allocation failed",
            subject,
            key_data,
        )?;
        Ok(owner)
    }

    pub(super) fn emit_ordinary_map_owned_any_v2_set(
        &mut self,
        result: &MirValueId,
        source: &MirValueId,
        key: &MirValueId,
        value: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        self.validate_v2_root(result, subject)?;
        let (key_data, key_len) = self.v2_string_parts(key, "Set key", subject)?;
        let key_is_temporary = self.v2_key_is_owned_temporary(key, subject)?;
        if key_is_temporary && !self.v2_key_has_normal_drop_after(key, subject) {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Set temporary key has no unique immediate MIR Drop",
            ));
        }
        let trap_key_cleanup = key_is_temporary.then_some(key_data);
        let source_handle =
            self.emit_live_v2_root_with_key_cleanup(source, subject, trap_key_cleanup)?;
        let payload = self.emit_v2_payload_abi(value, subject)?;
        let (selection_bytes, selection_len, selection_hash) =
            self.emit_v2_selection_receipt(&payload.ty, subject)?;
        let owner = self.emit_v2_payload_token(&payload, subject, trap_key_cleanup)?;
        let out_slot = self
            .generator
            .builder
            .build_alloca(self.generator.context.i64_type(), "mir_map_any_v2_set_out")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(out_slot, self.generator.context.i64_type().const_zero())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let set_fn = self
            .generator
            .get_runtime_fn(V2_SET)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let set_status = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    set_fn,
                    &[
                        BasicMetadataValueEnum::from(source_handle),
                        BasicMetadataValueEnum::from(key_data),
                        BasicMetadataValueEnum::from(key_len),
                        BasicMetadataValueEnum::from(owner),
                        BasicMetadataValueEnum::from(selection_bytes),
                        BasicMetadataValueEnum::from(selection_len),
                        BasicMetadataValueEnum::from(selection_hash),
                        BasicMetadataValueEnum::from(out_slot),
                    ],
                    "mir_map_any_v2_set_status",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "ordinary Map/Any owner-v2 Set returned void"))?
        .into_int_value();

        // Release this temporary creator-domain token exactly once after the
        // call, regardless of Set's status, then trap on either ABI failure.
        let drop_fn = self
            .generator
            .get_runtime_fn(V2_PAYLOAD_DROP)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let drop_status = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    drop_fn,
                    &[BasicMetadataValueEnum::from(owner)],
                    "mir_map_any_v2_payload_drop_status",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| {
            NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 payload Drop returned void",
            )
        })?
        .into_int_value();
        let drop_ok = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                drop_status,
                self.generator.context.i32_type().const_zero(),
                "mir_map_any_v2_payload_drop_status_ok",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_v2_root_guard_with_key_cleanup(
            drop_ok,
            "[E0800] ordinary Map/Any owner-v2 temporary payload release failed",
            subject,
            trap_key_cleanup,
        )?;
        let set_ok = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                set_status,
                self.generator.context.i32_type().const_zero(),
                "mir_map_any_v2_set_status_ok",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_v2_root_guard_with_key_cleanup(
            set_ok,
            "[E0800] ordinary Map/Any owner-v2 Set failed",
            subject,
            trap_key_cleanup,
        )?;
        let handle = self
            .generator
            .builder
            .build_load(
                self.generator.context.i64_type(),
                out_slot,
                "mir_map_any_v2_set_value",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        self.emit_v2_result_root(handle, source, subject, "Set", trap_key_cleanup)
    }

    pub(super) fn emit_ordinary_map_owned_any_v2_remove(
        &mut self,
        result: &MirValueId,
        source: &MirValueId,
        key: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        self.validate_v2_root(result, subject)?;
        let (key_data, key_len) = self.v2_string_parts(key, "Remove key", subject)?;
        let key_is_temporary = self.v2_key_is_owned_temporary(key, subject)?;
        if key_is_temporary && !self.v2_key_has_normal_drop_after(key, subject) {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Remove temporary key has no unique immediate MIR Drop",
            ));
        }
        let trap_key_cleanup = key_is_temporary.then_some(key_data);
        let source_handle =
            self.emit_live_v2_root_with_key_cleanup(source, subject, trap_key_cleanup)?;
        let remove_fn = self
            .generator
            .get_runtime_fn(V2_REMOVE)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let out_slot = self
            .generator
            .builder
            .build_alloca(
                self.generator.context.i64_type(),
                "mir_map_any_v2_remove_out",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(out_slot, self.generator.context.i64_type().const_zero())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let removed = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    remove_fn,
                    &[
                        BasicMetadataValueEnum::from(source_handle),
                        BasicMetadataValueEnum::from(key_data),
                        BasicMetadataValueEnum::from(key_len),
                        BasicMetadataValueEnum::from(out_slot),
                    ],
                    "mir_map_any_v2_remove_status",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| {
            NativeMirError::new(subject, "ordinary Map/Any owner-v2 Remove returned void")
        })?
        .into_int_value();
        let status_ok = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                removed,
                self.generator.context.i32_type().const_zero(),
                "mir_map_any_v2_remove_status_ok",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_v2_root_guard_with_key_cleanup(
            status_ok,
            "[E0800] ordinary Map/Any owner-v2 Remove failed",
            subject,
            trap_key_cleanup,
        )?;
        let removed = self
            .generator
            .builder
            .build_load(
                self.generator.context.i64_type(),
                out_slot,
                "mir_map_any_v2_remove_value",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        self.emit_v2_result_root(removed, source, subject, "Remove", trap_key_cleanup)
    }

    pub(super) fn emit_ordinary_map_owned_any_v2_size(
        &mut self,
        result: &MirValueId,
        root: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let handle = self.emit_live_v2_root(root, subject)?;
        let i32_ty = self.v2_primitive_type(crate::core::PrimitiveType::I32, subject)?;
        if self.value_type(result, subject)? != i32_ty {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Size result is not canonical i32",
            ));
        }
        self.program
            .type_catalog()
            .validate_copy_scalar(&i32_ty)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let size_fn = self
            .generator
            .get_runtime_fn(V2_SIZE)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let out_slot = self
            .generator
            .builder
            .build_alloca(self.generator.context.i32_type(), "mir_map_any_v2_size_out")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(out_slot, self.generator.context.i32_type().const_zero())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let status = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    size_fn,
                    &[
                        BasicMetadataValueEnum::from(handle),
                        BasicMetadataValueEnum::from(out_slot),
                    ],
                    "mir_map_any_v2_size_status",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| {
            NativeMirError::new(subject, "ordinary Map/Any owner-v2 Size returned void")
        })?
        .into_int_value();
        let not_size_overflow = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                status,
                self.generator
                    .context
                    .i32_type()
                    .const_int(V2_STATUS_SIZE_OVERFLOW as u64, false),
                "mir_map_any_v2_size_status_not_overflow",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_v2_root_guard(
            not_size_overflow,
            v2_size_status_error_message(V2_STATUS_SIZE_OVERFLOW)
                .expect("size overflow is an error status"),
            subject,
        )?;
        let status_ok = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.generator.context.i32_type().const_zero(),
                "mir_map_any_v2_size_status_ok",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_v2_root_guard(
            status_ok,
            v2_size_status_error_message(V2_STATUS_CAPACITY).expect("capacity is an error status"),
            subject,
        )?;
        let size = self
            .generator
            .builder
            .build_load(
                self.generator.context.i32_type(),
                out_slot,
                "mir_map_any_v2_size_value",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let valid = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::SGE,
                size,
                self.generator.context.i32_type().const_zero(),
                "mir_map_any_v2_size_valid",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_v2_root_guard(
            valid,
            "[E0800] ordinary Map/Any owner-v2 Size failed",
            subject,
        )?;
        Ok(size.into())
    }

    pub(super) fn emit_ordinary_map_owned_any_v2_contains(
        &mut self,
        result: &MirValueId,
        root: &MirValueId,
        key: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let (key_data, key_len) = self.v2_string_parts(key, "Contains key", subject)?;
        let key_is_temporary = self.v2_key_is_owned_temporary(key, subject)?;
        if key_is_temporary && !self.v2_key_has_normal_drop_after(key, subject) {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Contains temporary key has no unique immediate MIR Drop",
            ));
        }
        let trap_key_cleanup = key_is_temporary.then_some(key_data);
        let handle = self.emit_live_v2_root_with_key_cleanup(root, subject, trap_key_cleanup)?;
        let bool_ty = self.v2_primitive_type(crate::core::PrimitiveType::Bool, subject)?;
        if self.value_type(result, subject)? != bool_ty {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Contains result is not canonical bool",
            ));
        }
        self.program
            .type_catalog()
            .validate_copy_scalar(&bool_ty)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let contains_fn = self
            .generator
            .get_runtime_fn(V2_CONTAINS)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let out_slot = self
            .generator
            .builder
            .build_alloca(
                self.generator.context.i32_type(),
                "mir_map_any_v2_contains_out",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(out_slot, self.generator.context.i32_type().const_zero())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let contains = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    contains_fn,
                    &[
                        BasicMetadataValueEnum::from(handle),
                        BasicMetadataValueEnum::from(key_data),
                        BasicMetadataValueEnum::from(key_len),
                        BasicMetadataValueEnum::from(out_slot),
                    ],
                    "mir_map_any_v2_contains_status",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| {
            NativeMirError::new(subject, "ordinary Map/Any owner-v2 Contains returned void")
        })?
        .into_int_value();
        let status_ok = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                contains,
                self.generator.context.i32_type().const_zero(),
                "mir_map_any_v2_contains_status_ok",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_v2_root_guard_with_key_cleanup(
            status_ok,
            "[E0800] ordinary Map/Any owner-v2 Contains failed",
            subject,
            trap_key_cleanup,
        )?;
        let contains = self
            .generator
            .builder
            .build_load(
                self.generator.context.i32_type(),
                out_slot,
                "mir_map_any_v2_contains_value",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let valid = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::ULE,
                contains,
                self.generator.context.i32_type().const_int(1, false),
                "mir_map_any_v2_contains_valid",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_v2_root_guard_with_key_cleanup(
            valid,
            "[E0800] ordinary Map/Any owner-v2 Contains failed",
            subject,
            trap_key_cleanup,
        )?;
        let boolean = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                contains,
                self.generator.context.i32_type().const_zero(),
                "mir_map_any_v2_contains_bool",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        Ok(boolean.into())
    }

    /// Produce the Checker-owned `(found, DynamicAny)` result.  The miss
    /// payload descriptor is always the canonical Core `i32` TypeDesc; the
    /// runtime turns it into a fresh zero owner when the key is absent.
    pub(super) fn emit_ordinary_map_owned_any_v2_get_any(
        &mut self,
        result: &MirValueId,
        root: &MirValueId,
        key: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let receipt = self.validate_v2_get_any_receipt(result, root, key, subject)?;

        let (key_data, key_len) = self.v2_string_parts(key, "GetAny key", subject)?;
        let key_is_temporary = self.v2_key_is_owned_temporary(key, subject)?;
        if key_is_temporary && !self.v2_key_has_normal_drop_after(key, subject) {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 GetAny temporary key has no unique immediate MIR Drop",
            ));
        }
        let trap_key_cleanup = key_is_temporary.then_some(key_data);
        let root_handle =
            self.emit_live_v2_root_with_key_cleanup(root, subject, trap_key_cleanup)?;

        let catalog = self.program.type_catalog();
        let miss_type = receipt.miss_payload_type.clone();
        catalog
            .validate_copy_scalar(&miss_type)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let miss_desc = catalog
            .get(&miss_type)
            .ok_or_else(|| NativeMirError::new(subject, "canonical i32 miss TypeDesc is absent"))?;
        if miss_desc.id != miss_type
            || miss_desc.kind != MirTypeKind::Primitive(crate::core::PrimitiveType::I32)
            || miss_desc.layout != MirLayout::Scalar
        {
            return Err(NativeMirError::new(
                subject,
                "GetAny miss descriptor is not the canonical Core i32 TypeDesc",
            ));
        }
        let miss_type_id = miss_type.as_str().as_bytes();
        let miss_hash = v2_descriptor_hash(catalog, &miss_type)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let miss_hash_hex = miss_hash
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let miss_type_id_ptr = self.emit_v2_bytes_global(
            miss_type_id,
            &format!(
                "__mimi_any_payload_v2_type_id_{}",
                native_symbol_fragment(miss_type.as_str())
            ),
            subject,
        )?;
        let miss_hash_ptr = self.emit_v2_bytes_global(
            &miss_hash,
            &format!("__mimi_any_payload_v2_descriptor_hash_{miss_hash_hex}"),
            subject,
        )?;
        let i32_llvm_ty = native_basic_type(self.generator.context, catalog, &miss_type)?;
        let miss_size = self.generator.llvm_type_size_bytes(i32_llvm_ty);
        let miss_align = v2_native_alignment(i32_llvm_ty);
        if miss_size != 4 || miss_align != 4 {
            return Err(NativeMirError::new(
                subject,
                "Core-receipted i32 GetAny miss descriptor has a non-canonical native size or alignment",
            ));
        }
        let (miss_clone, miss_drop) = self
            .generator
            .ensure_any_payload_v2_glue(&OwnershipClass::Scalar, i32_llvm_ty)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        let found_out = self
            .generator
            .builder
            .build_alloca(
                self.generator.context.i32_type(),
                "mir_map_any_v2_get_found_out",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let token_out = self
            .generator
            .builder
            .build_alloca(
                self.generator.context.i64_type(),
                "mir_map_any_v2_get_token_out",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(found_out, self.generator.context.i32_type().const_zero())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(token_out, self.generator.context.i64_type().const_zero())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let get_fn = self
            .generator
            .get_runtime_fn(V2_GET)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let status = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    get_fn,
                    &[
                        BasicMetadataValueEnum::from(root_handle),
                        BasicMetadataValueEnum::from(key_data),
                        BasicMetadataValueEnum::from(key_len),
                        BasicMetadataValueEnum::from(found_out),
                        BasicMetadataValueEnum::from(token_out),
                        BasicMetadataValueEnum::from(
                            self.generator.context.i32_type().const_int(1, false),
                        ),
                        BasicMetadataValueEnum::from(miss_type_id_ptr),
                        BasicMetadataValueEnum::from(
                            self.generator
                                .context
                                .i64_type()
                                .const_int(miss_type_id.len() as u64, false),
                        ),
                        BasicMetadataValueEnum::from(miss_hash_ptr),
                        BasicMetadataValueEnum::from(
                            self.generator
                                .context
                                .i64_type()
                                .const_int(miss_size, false),
                        ),
                        BasicMetadataValueEnum::from(
                            self.generator
                                .context
                                .i64_type()
                                .const_int(miss_align, false),
                        ),
                        BasicMetadataValueEnum::from(
                            miss_clone.as_global_value().as_pointer_value(),
                        ),
                        BasicMetadataValueEnum::from(
                            miss_drop.as_global_value().as_pointer_value(),
                        ),
                    ],
                    "mir_map_any_v2_get_status",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| {
            NativeMirError::new(subject, "ordinary Map/Any owner-v2 GetAny returned void")
        })?
        .into_int_value();
        let status_ok = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.generator.context.i32_type().const_zero(),
                "mir_map_any_v2_get_status_ok",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_v2_root_guard_with_key_cleanup(
            status_ok,
            "[E0800] ordinary Map/Any owner-v2 GetAny failed",
            subject,
            trap_key_cleanup,
        )?;

        // The runtime's status=OK contract guarantees both outputs, but the
        // adapter rechecks that ABI invariant before publishing any MIR value.
        let found = self
            .generator
            .builder
            .build_load(
                self.generator.context.i32_type(),
                found_out,
                "mir_map_any_v2_get_found",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let token = self
            .generator
            .builder
            .build_load(
                self.generator.context.i64_type(),
                token_out,
                "mir_map_any_v2_get_token",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let found_valid = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::ULE,
                found,
                self.generator.context.i32_type().const_int(1, false),
                "mir_map_any_v2_get_found_valid",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let token_nonzero = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                token,
                self.generator.context.i64_type().const_zero(),
                "mir_map_any_v2_get_token_nonzero",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let outputs_valid = self
            .generator
            .builder
            .build_and(
                found_valid,
                token_nonzero,
                "mir_map_any_v2_get_outputs_valid",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let outputs_ok = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_map_any_v2_get_outputs_ok");
        let outputs_invalid = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_map_any_v2_get_outputs_invalid");
        self.generator
            .builder
            .build_conditional_branch(outputs_valid, outputs_ok, outputs_invalid)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(outputs_invalid);
        if let Some(key_data) = trap_key_cleanup {
            self.emit_v2_string_free(key_data, subject)?;
        }
        let payload_drop_fn = self
            .generator
            .get_runtime_fn(V2_PAYLOAD_DROP)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(
                payload_drop_fn,
                &[BasicMetadataValueEnum::from(token)],
                "mir_map_any_v2_get_invalid_token_cleanup",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_abort_with_message(
            "[E0800] ordinary Map/Any owner-v2 GetAny returned invalid outputs",
            subject,
        )?;
        self.generator.builder.position_at_end(outputs_ok);

        let found_bool = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                found,
                self.generator.context.i32_type().const_zero(),
                "mir_map_any_v2_get_found_bool",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let result_ty = self.value_type(result, subject)?;
        let expected_result_ty =
            crate::core::mir::types::ordinary_map_owned_any_v2_get_result_type_id();
        if result_ty != expected_result_ty {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 GetAny result is not its exact Core tuple TypeDesc",
            ));
        }
        self.program
            .type_catalog()
            .validate_glue(&result_ty, MirGlueOperation::Drop)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let llvm_result_ty = native_basic_type(self.generator.context, catalog, &result_ty)?;
        let BasicTypeEnum::StructType(result_struct_ty) = llvm_result_ty else {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 GetAny result ABI is not a struct aggregate",
            ));
        };
        if result_struct_ty.count_fields() != 2 {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 GetAny result aggregate does not have two fields",
            ));
        }
        let mut aggregate = self
            .generator
            .builder
            .build_insert_value(
                result_struct_ty.get_undef(),
                found_bool,
                0,
                "mir_map_any_v2_get_result_found",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_struct_value();
        aggregate = self
            .generator
            .builder
            .build_insert_value(aggregate, token, 1, "mir_map_any_v2_get_result_owner")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_struct_value();
        Ok(inkwell::values::BasicValueEnum::StructValue(aggregate))
    }

    /// IsEmpty uses the exact status/out runtime ABI.  The older V2 record
    /// helpers return sentinel values and are intentionally not used here:
    /// status must be checked before the out slot can be observed.
    pub(super) fn emit_ordinary_map_owned_any_v2_is_empty(
        &mut self,
        result: &MirValueId,
        root: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let handle = self.emit_live_v2_root(root, subject)?;
        let bool_ty = self.v2_primitive_type(crate::core::PrimitiveType::Bool, subject)?;
        if self.value_type(result, subject)? != bool_ty {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 IsEmpty result is not canonical bool",
            ));
        }
        self.program
            .type_catalog()
            .validate_copy_scalar(&bool_ty)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let out_slot = self
            .generator
            .builder
            .build_alloca(
                self.generator.context.i32_type(),
                "mir_map_any_v2_is_empty_out",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(out_slot, self.generator.context.i32_type().const_zero())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let is_empty_fn = self
            .generator
            .get_runtime_fn("mimi_ordinary_map_owned_any_v2_is_empty")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let status = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    is_empty_fn,
                    &[
                        BasicMetadataValueEnum::from(handle),
                        BasicMetadataValueEnum::from(out_slot),
                    ],
                    "mir_map_any_v2_is_empty_status",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| {
            NativeMirError::new(subject, "ordinary Map/Any owner-v2 IsEmpty returned void")
        })?
        .into_int_value();
        let status_ok = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.generator.context.i32_type().const_zero(),
                "mir_map_any_v2_is_empty_status_ok",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_v2_root_guard(
            status_ok,
            "[E0800] ordinary Map/Any owner-v2 IsEmpty failed",
            subject,
        )?;
        let value = self
            .generator
            .builder
            .build_load(
                self.generator.context.i32_type(),
                out_slot,
                "mir_map_any_v2_is_empty_value",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let valid = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::ULE,
                value,
                self.generator.context.i32_type().const_int(1, false),
                "mir_map_any_v2_is_empty_valid",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_v2_root_guard(
            valid,
            "[E0800] ordinary Map/Any owner-v2 IsEmpty returned invalid output",
            subject,
        )?;
        let boolean = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                value,
                self.generator.context.i32_type().const_zero(),
                "mir_map_any_v2_is_empty_bool",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        Ok(boolean.into())
    }

    /// Exact-check and clone the token in GetAny tuple field 1 into the
    /// Checker-selected payload TypeDesc. The pair remains the owner; its
    /// later explicit Drop is still responsible for releasing the token.
    pub(super) fn emit_ordinary_map_owned_any_v2_unpack(
        &mut self,
        result: &MirValueId,
        pair: &MirValueId,
        projection: &crate::core::NodeId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let receipts = self
            .program
            .ordinary_map_owned_any_v2_unpack_result_receipts()
            .map_err(|message| NativeMirError::new(subject, message))?;
        let matching = receipts
            .iter()
            .filter(|receipt| {
                receipt.owner == self.function.owner && receipt.instruction.as_str() == subject
            })
            .collect::<Vec<_>>();
        let [receipt] = matching.as_slice() else {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 unpack has no unique Core result receipt for this instruction",
            ));
        };
        receipt
            .validate()
            .and_then(|()| receipt.validate_against_program(self.program))
            .map_err(|message| NativeMirError::new(subject, message))?;

        let i32_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        let string_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        let target_ty = receipt.result_type.clone();
        let expected_pair_ty =
            crate::core::mir::types::ordinary_map_owned_any_v2_get_result_type_id();
        if receipt.owner != self.function.owner
            || receipt.instruction.as_str() != subject
            || receipt.result != *result
            || receipt.pair != *pair
            || receipt.projection != *projection
            || receipt.settlement
                != crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
            || receipt.conversion_kind != crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
            || self
                .program
                .type_catalog()
                .get(&receipt.conversion_from)
                .is_none_or(|ty| ty.kind != MirTypeKind::DynamicAny)
            || receipt.conversion_to != target_ty
            || self.value_type(result, subject)? != target_ty
            || self.value_type(pair, subject)? != expected_pair_ty
        {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 unpack receipt does not bind the exact i32/String result, pair, projection, and explicit pair settlement",
            ));
        }

        let selection = self
            .program
            .ordinary_map_owned_any_v2_payload_selection_receipt()
            .map_err(|message| NativeMirError::new(subject, message))?
            .ok_or_else(|| {
                NativeMirError::new(
                    subject,
                    "ordinary Map/Any owner-v2 unpack has no Core payload-selection receipt",
                )
            })?;
        selection
            .validate_against_program(self.program)
            .map_err(|message| NativeMirError::new(subject, message))?;
        if selection.selection_digest != receipt.payload_selection_digest {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 unpack receipt is bound to another payload-selection digest",
            ));
        }

        let get_any_receipts = self
            .program
            .ordinary_map_owned_any_v2_get_any_result_receipts()
            .map_err(|message| NativeMirError::new(subject, message))?;
        let matching_get_any = get_any_receipts
            .iter()
            .filter(|get_any| {
                get_any.owner == receipt.owner
                    && get_any.operation == receipt.operation
                    && get_any.binding_local == receipt.pair_binding_local
                    && get_any.binding == receipt.pair
            })
            .collect::<Vec<_>>();
        let [get_any] = matching_get_any.as_slice() else {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 unpack does not join one exact GetAny pair receipt",
            ));
        };
        get_any
            .validate()
            .and_then(|()| get_any.validate_against_program(self.program))
            .map_err(|message| NativeMirError::new(subject, message))?;
        if get_any.binding_point != receipt.pair_binding_point
            || get_any.settlement
                != crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
            || get_any.settlement_point != receipt.settlement_point
            || get_any.settlement_instruction != receipt.settlement_instruction
        {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 unpack does not preserve the receipted GetAny pair Drop",
            ));
        }

        let catalog = self.program.type_catalog();
        let target_desc = catalog.get(&target_ty).ok_or_else(|| {
            NativeMirError::new(subject, "canonical unpack target TypeDesc is absent")
        })?;
        let target_is_i32 = target_ty == i32_ty
            && target_desc.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::I32)
            && target_desc.layout == MirLayout::Scalar
            && catalog.validate_copy_scalar(&target_ty).is_ok();
        let target_is_string = target_ty == string_ty
            && target_desc.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::String)
            && catalog.validate_owned_string(&target_ty).is_ok();
        let target_is_label = v2_values_is_label_type(catalog, &target_ty);
        if target_desc.id != target_ty || (!target_is_i32 && !target_is_string && !target_is_label)
        {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 unpack target is not canonical i32, owned String, or Label { text: string }",
            ));
        }
        for operation in [MirGlueOperation::Clone, MirGlueOperation::Drop] {
            catalog
                .validate_glue(&target_ty, operation)
                .map_err(|message| NativeMirError::new(subject, message))?;
        }
        let target_type_desc_digest = blake3::hash(target_desc.canonical_text().as_bytes())
            .to_hex()
            .to_string();
        let selected_target = selection
            .selected_types
            .iter()
            .find(|selected| selected.type_id == target_ty);
        let selected_target_matches = selected_target
            .is_some_and(|selected| selected.type_desc_digest == receipt.payload_type_desc_digest);
        let canonical_miss_matches = target_is_i32
            && get_any.miss_payload_type == i32_ty
            && get_any.miss_payload_type_desc_digest == receipt.payload_type_desc_digest;
        let receipt_source_matches = match &receipt.payload_source {
            crate::core::mir::OrdinaryMapOwnedAnyV2UnpackPayloadSource::SelectedSetPayload => {
                selected_target_matches
            }
            crate::core::mir::OrdinaryMapOwnedAnyV2UnpackPayloadSource::CanonicalI32Miss => {
                canonical_miss_matches
            }
        };
        if target_type_desc_digest != receipt.payload_type_desc_digest || !receipt_source_matches {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 unpack payload-source receipt does not authorize the exact selected payload or canonical i32 miss TypeDesc",
            ));
        }

        let pair_desc = catalog.get(&expected_pair_ty).ok_or_else(|| {
            NativeMirError::new(subject, "canonical GetAny pair TypeDesc is absent")
        })?;
        let payload_token_ty = crate::core::mir::types::ordinary_map_owned_any_v2_payload_type_id();
        let bool_ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Bool);
        if pair_desc.id != expected_pair_ty
            || pair_desc.kind != (MirTypeKind::Tuple { arity: 2 })
            || pair_desc.layout != MirLayout::Tuple(vec![bool_ty, payload_token_ty.clone()])
        {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 unpack pair is not the exact (bool, DynamicAny token) tuple TypeDesc",
            ));
        }
        catalog
            .validate_glue(&payload_token_ty, MirGlueOperation::MoveOut)
            .map_err(|message| NativeMirError::new(subject, message))?;
        catalog
            .validate_glue(&payload_token_ty, MirGlueOperation::Drop)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let BasicTypeEnum::IntType(token_type) =
            native_basic_type(self.generator.context, catalog, &payload_token_ty)?
        else {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 GetAny tuple field 1 is not an integer token",
            ));
        };
        if token_type.get_bit_width() != 64 {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 GetAny tuple field 1 is not the canonical i64 payload token",
            ));
        }

        let payload_type_id = target_ty.as_str().as_bytes();
        if payload_type_id.len() != 35
            || !payload_type_id.starts_with(b"rt:")
            || !payload_type_id[3..]
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 unpack TypeDesc identity is not canonical",
            ));
        }
        let descriptor_hash = v2_descriptor_hash(catalog, &target_ty)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let descriptor_hash_hex = descriptor_hash
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let type_id_ptr = self.emit_v2_bytes_global(
            payload_type_id,
            &format!(
                "__mimi_any_payload_v2_type_id_{}",
                native_symbol_fragment(target_ty.as_str())
            ),
            subject,
        )?;
        let descriptor_hash_ptr = self.emit_v2_bytes_global(
            &descriptor_hash,
            &format!("__mimi_any_payload_v2_descriptor_hash_{descriptor_hash_hex}"),
            subject,
        )?;

        let payload_llvm_ty = native_basic_type(self.generator.context, catalog, &target_ty)?;
        let payload_size = self.generator.llvm_type_size_bytes(payload_llvm_ty);
        let payload_align = v2_native_alignment(payload_llvm_ty);
        let expected_size = if target_is_i32 { 4 } else { 16 };
        let expected_align = if target_is_i32 { 4 } else { 8 };
        if payload_size != expected_size || payload_align != expected_align {
            return Err(NativeMirError::new(
                subject,
                "Core-receipted i32/String/Label unpack target has a noncanonical native size or alignment",
            ));
        }
        // V2 owners retain generated callbacks with their exact TypeDesc;
        // ensure this compilation emits the matching glue before asking the
        // runtime to clone the payload into the fresh output slot.
        let payload_class = if target_is_i32 {
            OwnershipClass::Scalar
        } else if target_is_label {
            // The only aggregate currently admitted by the V2 payload ABI is
            // the exact one-field Label record validated above.
            OwnershipClass::Record(vec![OwnershipClass::StringBox])
        } else {
            OwnershipClass::StringBox
        };
        let (_clone_glue, payload_drop_glue) = self
            .generator
            .ensure_any_payload_v2_glue(&payload_class, payload_llvm_ty)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        let pair_value = self.value(pair, subject)?.into_struct_value();
        if pair_value.get_type().count_fields() != 2 {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 unpack pair LLVM aggregate does not have two fields",
            ));
        }
        // The payload token is the exact tuple field 1 from the receipted
        // GetAny pair. Extracting this integer does not move or consume pair.
        let token = self
            .generator
            .builder
            .build_extract_value(pair_value, 1, "mir_map_any_v2_unpack_token")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let output = self
            .generator
            .builder
            .build_alloca(payload_llvm_ty, "mir_map_any_v2_unpack_output")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let unpack_fn = self
            .generator
            .get_runtime_fn(V2_PAYLOAD_UNPACK)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let status = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    unpack_fn,
                    &[
                        BasicMetadataValueEnum::from(token),
                        BasicMetadataValueEnum::from(
                            self.generator.context.i32_type().const_int(
                                v2_payload_tag(catalog, &target_ty)
                                    .map_err(|message| NativeMirError::new(subject, message))?
                                    as u64,
                                false,
                            ),
                        ),
                        BasicMetadataValueEnum::from(type_id_ptr),
                        BasicMetadataValueEnum::from(
                            self.generator
                                .context
                                .i64_type()
                                .const_int(payload_type_id.len() as u64, false),
                        ),
                        BasicMetadataValueEnum::from(descriptor_hash_ptr),
                        BasicMetadataValueEnum::from(output),
                        BasicMetadataValueEnum::from(
                            self.generator
                                .context
                                .i64_type()
                                .const_int(payload_size, false),
                        ),
                        BasicMetadataValueEnum::from(
                            self.generator
                                .context
                                .i64_type()
                                .const_int(payload_align, false),
                        ),
                    ],
                    "mir_map_any_v2_unpack_status",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| {
            NativeMirError::new(subject, "ordinary Map/Any owner-v2 unpack returned void")
        })?
        .into_int_value();
        let status_ok = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.generator.context.i32_type().const_zero(),
                "mir_map_any_v2_unpack_status_ok",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_v2_root_guard(
            status_ok,
            "[E0800] ordinary Map/Any owner-v2 payload unpack failed",
            subject,
        )?;
        let unpacked = self
            .generator
            .builder
            .build_load(payload_llvm_ty, output, "mir_map_any_v2_unpack_value")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        if target_is_string {
            let string = unpacked.into_struct_value();
            let data = self
                .generator
                .builder
                .build_extract_value(string, 0, "mir_map_any_v2_unpack_string_data")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                .into_pointer_value();
            let len = self
                .generator
                .builder
                .build_extract_value(string, 1, "mir_map_any_v2_unpack_string_len")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                .into_int_value();
            // V2 callback glue uses malloc/free for its private payload
            // copies. A language String uses mimi_alloc/mimi_free, so bridge
            // the callback result into the ordinary String allocator before
            // exposing it and release the temporary through its exact glue.
            let clone_fn = self
                .generator
                .get_runtime_fn("mimi_str_clone")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let handle = call_try_basic_value(
                &self
                    .generator
                    .builder
                    .build_call(
                        clone_fn,
                        &[
                            BasicMetadataValueEnum::from(data),
                            BasicMetadataValueEnum::from(len),
                        ],
                        "mir_map_any_v2_unpack_string_clone",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
            )
            .ok_or_else(|| {
                NativeMirError::new(subject, "ordinary Map/Any V2 String clone returned void")
            })?
            .into_int_value();
            self.generator
                .builder
                .build_call(
                    payload_drop_glue,
                    &[BasicMetadataValueEnum::from(output)],
                    "mir_map_any_v2_unpack_string_temporary_drop",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            return self.emit_owned_string_from_handle(handle, len, subject);
        }
        if target_is_label {
            let label = unpacked.into_struct_value();
            if label.get_type().count_fields() != 1 {
                return Err(NativeMirError::new(
                    subject,
                    "Core-receipted Label unpack LLVM aggregate does not have one String field",
                ));
            }
            let text = self
                .generator
                .builder
                .build_extract_value(label, 0, "mir_map_any_v2_unpack_label_text")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                .into_struct_value();
            let data = self
                .generator
                .builder
                .build_extract_value(text, 0, "mir_map_any_v2_unpack_label_text_data")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                .into_pointer_value();
            let len = self
                .generator
                .builder
                .build_extract_value(text, 1, "mir_map_any_v2_unpack_label_text_len")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                .into_int_value();
            // The payload callback cloned this Label through native value glue,
            // whose temporary String leaf uses malloc/free. Clone that leaf
            // into Mimi's ordinary String allocator before releasing the
            // callback-owned aggregate, so the returned Label has the exact
            // ownership ABI expected by native aggregate Drop glue.
            let clone_fn = self
                .generator
                .get_runtime_fn("mimi_str_clone")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let handle = call_try_basic_value(
                &self
                    .generator
                    .builder
                    .build_call(
                        clone_fn,
                        &[
                            BasicMetadataValueEnum::from(data),
                            BasicMetadataValueEnum::from(len),
                        ],
                        "mir_map_any_v2_unpack_label_text_clone",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
            )
            .ok_or_else(|| {
                NativeMirError::new(
                    subject,
                    "ordinary Map/Any V2 Label String clone returned void",
                )
            })?
            .into_int_value();
            self.generator
                .builder
                .build_call(
                    payload_drop_glue,
                    &[BasicMetadataValueEnum::from(output)],
                    "mir_map_any_v2_unpack_label_temporary_drop",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let owned_text = self.emit_owned_string_from_handle(handle, len, subject)?;
            let label_type = payload_llvm_ty.into_struct_type();
            let owned_label = self
                .generator
                .builder
                .build_insert_value(
                    label_type.const_zero(),
                    owned_text,
                    0,
                    "mir_map_any_v2_unpack_label_owner",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            return Ok(owned_label.into_struct_value().into());
        }
        Ok(unpacked.into())
    }

    /// Read one selected payload from the immutable Values snapshot, exact-
    /// check its runtime TypeDesc, and clone it into the ordinary Mimi owner
    /// domain. The list snapshot retains its own token; the per-index token
    /// is always released on both successful and failing unpack paths.
    fn emit_v2_values_unpack_copy_string(
        &mut self,
        source: inkwell::values::PointerValue<'ctx>,
        length: inkwell::values::IntValue<'ctx>,
        temporary_payload: inkwell::values::PointerValue<'ctx>,
        payload_drop_glue: inkwell::values::FunctionValue<'ctx>,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let pointer_type = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default());
        let output_pointer = self
            .generator
            .builder
            .build_alloca(
                pointer_type,
                "mir_map_any_v2_values_unpack_string_output_ptr",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let output_length = self
            .generator
            .builder
            .build_alloca(
                self.generator.context.i64_type(),
                "mir_map_any_v2_values_unpack_string_output_len",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(output_pointer, pointer_type.const_null())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(
                output_length,
                self.generator.context.i64_type().const_zero(),
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        let copy = self
            .generator
            .get_runtime_fn("mimi_mir_ordinary_map_owned_any_v2_values_unpack_copy_string")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let status = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    copy,
                    &[
                        BasicMetadataValueEnum::from(source),
                        BasicMetadataValueEnum::from(length),
                        BasicMetadataValueEnum::from(output_pointer),
                        BasicMetadataValueEnum::from(output_length),
                    ],
                    "mir_map_any_v2_values_unpack_string_copy_status",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "Values String copy helper returned void"))?
        .into_int_value();

        // The typed temporary is no longer needed regardless of whether the
        // final String allocation or domain publication succeeds.
        self.generator
            .builder
            .build_call(
                payload_drop_glue,
                &[BasicMetadataValueEnum::from(temporary_payload)],
                "mir_map_any_v2_values_unpack_string_temporary_drop",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        let ok = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.generator.context.i32_type().const_zero(),
                "mir_map_any_v2_values_unpack_string_copy_ok",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let success = self.generator.context.append_basic_block(
            self.llvm_function,
            "mir_map_any_v2_values_unpack_string_copy_success",
        );
        let failure = self.generator.context.append_basic_block(
            self.llvm_function,
            "mir_map_any_v2_values_unpack_string_copy_failure",
        );
        self.generator
            .builder
            .build_conditional_branch(ok, success, failure)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(failure);
        self.emit_abort_with_message(
            "[E0800] ordinary Map/Any owner-v2 Values String allocation or publication failed",
            subject,
        )?;

        self.generator.builder.position_at_end(success);
        let data = self
            .generator
            .builder
            .build_load(
                pointer_type,
                output_pointer,
                "mir_map_any_v2_values_unpack_string_data",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_pointer_value();
        let output_length = self
            .generator
            .builder
            .build_load(
                self.generator.context.i64_type(),
                output_length,
                "mir_map_any_v2_values_unpack_string_length",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        self.generator
            .build_string_struct(data, output_length)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))
    }

    pub(super) fn emit_ordinary_map_owned_any_v2_values_unpack(
        &mut self,
        result: &MirValueId,
        values: &MirValueId,
        index: &MirValueId,
        projection: &crate::core::NodeId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        validate_ordinary_map_owned_any_v2_values_unpack_native_contract(self.program)
            .map_err(|messages| NativeMirError::new(subject, messages.join("; ")))?;

        let receipts = self
            .program
            .ordinary_map_owned_any_v2_values_unpack_result_receipts()
            .map_err(|message| NativeMirError::new(subject, message))?;
        let matching = receipts
            .iter()
            .filter(|receipt| {
                receipt.owner == self.function.owner && receipt.instruction.as_str() == subject
            })
            .collect::<Vec<_>>();
        let [receipt] = matching.as_slice() else {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Values unpack has no unique Core result receipt for this instruction",
            ));
        };
        receipt
            .validate()
            .and_then(|()| receipt.validate_against_program(self.program))
            .map_err(|message| NativeMirError::new(subject, message))?;

        let catalog = self.program.type_catalog();
        let values_type = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
        let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        let i64_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I64);
        let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        let target_type = receipt.result_type.clone();
        let expected_index_type = receipt.index_type.clone();
        if receipt.owner != self.function.owner
            || receipt.instruction.as_str() != subject
            || receipt.result != *result
            || receipt.values != *values
            || receipt.index != *index
            || receipt.projection != *projection
            || receipt.conversion_kind != crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
            || receipt.conversion_to != target_type
            || receipt.result_type != target_type
            || receipt.payload_selection_digest.is_empty()
            || receipt.settlement
                != crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
            || self.value_type(result, subject)? != target_type
            || self.value_type(values, subject)? != values_type
            || self.value_type(index, subject)? != expected_index_type
        {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Values unpack receipt does not bind the exact snapshot/index/projection/result and explicit snapshot settlement",
            ));
        }
        if expected_index_type != i32_type && expected_index_type != i64_type {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Values unpack index is not signed i32 or i64",
            ));
        }
        validate_ordinary_map_owned_any_v2_values_list_type(catalog, &values_type)
            .map_err(|message| NativeMirError::new(subject, message))?;

        let selection = self
            .program
            .ordinary_map_owned_any_v2_payload_selection_receipt()
            .map_err(|message| NativeMirError::new(subject, message))?
            .ok_or_else(|| {
                NativeMirError::new(
                    subject,
                    "ordinary Map/Any owner-v2 Values unpack has no Core payload-selection receipt",
                )
            })?;
        selection
            .validate_against_program(self.program)
            .map_err(|message| NativeMirError::new(subject, message))?;
        if selection.selection_digest != receipt.payload_selection_digest {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Values unpack receipt is bound to another payload-selection digest",
            ));
        }

        let snapshot_receipts = self
            .program
            .ordinary_map_owned_any_v2_values_result_receipts()
            .map_err(|message| NativeMirError::new(subject, message))?;
        let matching_snapshots = snapshot_receipts
            .iter()
            .filter(|snapshot| {
                snapshot.owner == receipt.owner
                    && snapshot.operation == receipt.operation
                    && snapshot.binding_local == receipt.values_binding_local
                    && snapshot.binding == receipt.values
                    && snapshot.binding_point == receipt.values_binding_point
                    && snapshot.settlement_point == receipt.settlement_point
                    && snapshot.settlement_instruction == receipt.settlement_instruction
                    && snapshot.settlement
                        == crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
            })
            .collect::<Vec<_>>();
        let [snapshot] = matching_snapshots.as_slice() else {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Values unpack does not join one exact Values snapshot receipt",
            ));
        };
        let checker_snapshot = catalog.get(&snapshot.checker_result_type).ok_or_else(|| {
            NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Values Checker snapshot TypeDesc is absent",
            )
        })?;
        let checker_element = match &checker_snapshot.layout {
            MirLayout::List { element } if checker_snapshot.kind == MirTypeKind::List => element,
            _ => {
                return Err(NativeMirError::new(
                    subject,
                    "ordinary Map/Any owner-v2 Values Checker snapshot is not List<DynamicAny>",
                ));
            }
        };
        if &receipt.conversion_from != checker_element
            || catalog
                .get(checker_element)
                .is_none_or(|descriptor| descriptor.kind != MirTypeKind::DynamicAny)
        {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Values unpack source type is not the exact Checker DynamicAny element",
            ));
        }

        let exact_instruction_count = self
            .function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| {
                instruction.id.as_str() == subject
                    && matches!(
                        &instruction.kind,
                        MirInstructionKind::OrdinaryMapOwnedAnyV2ValuesUnpack {
                            result: observed_result,
                            values: observed_values,
                            index: observed_index,
                            projection: observed_projection,
                        } if observed_result == result
                            && observed_values == values
                            && observed_index == index
                            && observed_projection == projection
                    )
            })
            .count();
        let binding_moves = self
            .function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Move { result: moved, source }
                        if moved == &receipt.result_binding && source == result
                )
            })
            .count();
        let snapshot_drops = self
            .function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| {
                instruction.id == receipt.settlement_instruction
                    && matches!(
                        &instruction.kind,
                        MirInstructionKind::Drop { value } if value == values
                    )
            })
            .count();
        if exact_instruction_count != 1 || binding_moves != 1 || snapshot_drops != 1 {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Values unpack is missing its exact MIR operation, direct result Move, or receipted snapshot Drop",
            ));
        }

        let target_desc = catalog.get(&target_type).ok_or_else(|| {
            NativeMirError::new(subject, "canonical Values unpack target TypeDesc is absent")
        })?;
        let target_is_i32 = target_type == i32_type
            && target_desc.id == i32_type
            && target_desc.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::I32)
            && target_desc.layout == MirLayout::Scalar
            && catalog.validate_copy_scalar(&target_type).is_ok();
        let target_is_string = target_type == string_type
            && target_desc.id == string_type
            && target_desc.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::String)
            && catalog.validate_owned_string(&target_type).is_ok();
        let target_is_label = v2_values_is_label_type(catalog, &target_type);
        if !target_is_i32 && !target_is_string && !target_is_label {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Values unpack target is not canonical i32, owned String, or Label { text: string }",
            ));
        }
        for operation in [MirGlueOperation::Clone, MirGlueOperation::Drop] {
            catalog
                .validate_glue(&target_type, operation)
                .map_err(|message| NativeMirError::new(subject, message))?;
        }
        let target_digest = blake3::hash(target_desc.canonical_text().as_bytes())
            .to_hex()
            .to_string();
        if target_digest != receipt.payload_type_desc_digest
            || !selection.selected_types.iter().any(|selected| {
                selected.type_id == target_type
                    && selected.type_desc_digest == receipt.payload_type_desc_digest
            })
        {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Values unpack target is not the exact Checker-selected Set payload TypeDesc",
            ));
        }

        let payload_type_id = target_type.as_str().as_bytes();
        if payload_type_id.len() != 35
            || !payload_type_id.starts_with(b"rt:")
            || !payload_type_id[3..]
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Values unpack TypeDesc identity is not canonical",
            ));
        }
        let expected_tag = v2_payload_tag(catalog, &target_type)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let descriptor_hash = v2_descriptor_hash(catalog, &target_type)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let descriptor_hash_hex = descriptor_hash
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let payload_llvm_ty = native_basic_type(self.generator.context, catalog, &target_type)?;
        let payload_size = self.generator.llvm_type_size_bytes(payload_llvm_ty);
        let payload_align = v2_native_alignment(payload_llvm_ty);
        let expected_size = if target_is_i32 { 4 } else { 16 };
        let expected_align = if target_is_i32 { 4 } else { 8 };
        if payload_size != expected_size || payload_align != expected_align {
            return Err(NativeMirError::new(
                subject,
                "Core-receipted i32/String/Label Values unpack target has a noncanonical native size or alignment",
            ));
        }

        // All receipt, TypeDesc, index, lineage, and ABI-shape checks above
        // precede global creation and payload glue emission.
        let type_id_ptr = self.emit_v2_bytes_global(
            payload_type_id,
            &format!(
                "__mimi_any_payload_v2_type_id_{}",
                native_symbol_fragment(target_type.as_str())
            ),
            subject,
        )?;
        let descriptor_hash_ptr = self.emit_v2_bytes_global(
            &descriptor_hash,
            &format!("__mimi_any_payload_v2_descriptor_hash_{descriptor_hash_hex}"),
            subject,
        )?;
        let payload_class = if target_is_i32 {
            OwnershipClass::Scalar
        } else if target_is_label {
            OwnershipClass::Record(vec![OwnershipClass::StringBox])
        } else {
            OwnershipClass::StringBox
        };
        let (_clone_glue, payload_drop_glue) = self
            .generator
            .ensure_any_payload_v2_glue(&payload_class, payload_llvm_ty)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        let list = self.value(values, subject)?.into_pointer_value();
        let raw_index = self.value(index, subject)?.into_int_value();
        let index64 = if expected_index_type == i32_type {
            self.generator
                .builder
                .build_int_s_extend(
                    raw_index,
                    self.generator.context.i64_type(),
                    "mir_map_any_v2_values_unpack_index64",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?
        } else {
            raw_index
        };
        let found_slot = self
            .generator
            .builder
            .build_alloca(
                self.generator.context.i32_type(),
                "mir_map_any_v2_values_unpack_found",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let owner_slot = self
            .generator
            .builder
            .build_alloca(
                self.generator.context.i64_type(),
                "mir_map_any_v2_values_unpack_owner",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(found_slot, self.generator.context.i32_type().const_zero())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(owner_slot, self.generator.context.i64_type().const_zero())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let list_get = self
            .generator
            .get_runtime_fn("mimi_mir_list_get_managed_any_v2")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let list_get_status = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    list_get,
                    &[
                        BasicMetadataValueEnum::from(list),
                        BasicMetadataValueEnum::from(index64),
                        BasicMetadataValueEnum::from(found_slot),
                        BasicMetadataValueEnum::from(owner_slot),
                    ],
                    "mir_map_any_v2_values_unpack_list_status",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| {
            NativeMirError::new(subject, "ordinary Map/Any V2 Values list get returned void")
        })?
        .into_int_value();
        let list_get_ok = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                list_get_status,
                self.generator.context.i32_type().const_zero(),
                "mir_map_any_v2_values_unpack_list_status_ok",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let list_get_success = self.generator.context.append_basic_block(
            self.llvm_function,
            "mir_map_any_v2_values_unpack_list_get_success",
        );
        let list_get_failure = self.generator.context.append_basic_block(
            self.llvm_function,
            "mir_map_any_v2_values_unpack_list_get_failure",
        );
        self.generator
            .builder
            .build_conditional_branch(list_get_ok, list_get_success, list_get_failure)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(list_get_failure);
        let failed_owner = self
            .generator
            .builder
            .build_load(
                self.generator.context.i64_type(),
                owner_slot,
                "mir_map_any_v2_values_unpack_failed_owner_value",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        self.emit_v2_temporary_owner_abort(
            failed_owner,
            "[E0800] ordinary Map/Any owner-v2 Values list get failed",
            subject,
        )?;
        self.generator.builder.position_at_end(list_get_success);
        let found = self
            .generator
            .builder
            .build_load(
                self.generator.context.i32_type(),
                found_slot,
                "mir_map_any_v2_values_unpack_found_value",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let owner = self
            .generator
            .builder
            .build_load(
                self.generator.context.i64_type(),
                owner_slot,
                "mir_map_any_v2_values_unpack_owner_value",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let found_one = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                found,
                self.generator.context.i32_type().const_int(1, false),
                "mir_map_any_v2_values_unpack_found_one",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let found_block = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_map_any_v2_values_unpack_present");
        let absent_block = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_map_any_v2_values_unpack_absent");
        self.generator
            .builder
            .build_conditional_branch(found_one, found_block, absent_block)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(absent_block);
        let found_zero = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                found,
                self.generator.context.i32_type().const_zero(),
                "mir_map_any_v2_values_unpack_found_zero",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let absent_empty = self.generator.context.append_basic_block(
            self.llvm_function,
            "mir_map_any_v2_values_unpack_absent_empty",
        );
        let malformed_found = self.generator.context.append_basic_block(
            self.llvm_function,
            "mir_map_any_v2_values_unpack_malformed_found",
        );
        self.generator
            .builder
            .build_conditional_branch(found_zero, absent_empty, malformed_found)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(malformed_found);
        self.emit_v2_temporary_owner_abort(
            owner,
            "[E0800] ordinary Map/Any owner-v2 Values list get returned invalid found output",
            subject,
        )?;
        self.generator.builder.position_at_end(absent_empty);
        let owner_zero = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                owner,
                self.generator.context.i64_type().const_zero(),
                "mir_map_any_v2_values_unpack_absent_owner_zero",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let out_of_bounds = self.generator.context.append_basic_block(
            self.llvm_function,
            "mir_map_any_v2_values_unpack_out_of_bounds",
        );
        let malformed_absent = self.generator.context.append_basic_block(
            self.llvm_function,
            "mir_map_any_v2_values_unpack_absent_with_owner",
        );
        self.generator
            .builder
            .build_conditional_branch(owner_zero, out_of_bounds, malformed_absent)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(malformed_absent);
        self.emit_v2_temporary_owner_abort(
            owner,
            "[E0800] ordinary Map/Any owner-v2 Values list get returned malformed found/owner outputs",
            subject,
        )?;
        self.generator.builder.position_at_end(out_of_bounds);
        self.emit_abort_with_message("[E0803] list index out of bounds", subject)?;
        self.generator.builder.position_at_end(found_block);
        let owner_nonzero = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                owner,
                self.generator.context.i64_type().const_zero(),
                "mir_map_any_v2_values_unpack_owner_nonzero",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_v2_root_guard(
            owner_nonzero,
            "[E0800] ordinary Map/Any owner-v2 Values list get returned a null owner token",
            subject,
        )?;

        let output = self
            .generator
            .builder
            .build_alloca(payload_llvm_ty, "mir_map_any_v2_values_unpack_output")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let unpack_fn = self
            .generator
            .get_runtime_fn(V2_PAYLOAD_UNPACK)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let unpack_status = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    unpack_fn,
                    &[
                        BasicMetadataValueEnum::from(owner),
                        BasicMetadataValueEnum::from(
                            self.generator
                                .context
                                .i32_type()
                                .const_int(expected_tag as u64, false),
                        ),
                        BasicMetadataValueEnum::from(type_id_ptr),
                        BasicMetadataValueEnum::from(
                            self.generator
                                .context
                                .i64_type()
                                .const_int(payload_type_id.len() as u64, false),
                        ),
                        BasicMetadataValueEnum::from(descriptor_hash_ptr),
                        BasicMetadataValueEnum::from(output),
                        BasicMetadataValueEnum::from(
                            self.generator
                                .context
                                .i64_type()
                                .const_int(payload_size, false),
                        ),
                        BasicMetadataValueEnum::from(
                            self.generator
                                .context
                                .i64_type()
                                .const_int(payload_align, false),
                        ),
                    ],
                    "mir_map_any_v2_values_unpack_status",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| {
            NativeMirError::new(
                subject,
                "ordinary Map/Any V2 Values payload unpack returned void",
            )
        })?
        .into_int_value();
        let unpack_ok = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                unpack_status,
                self.generator.context.i32_type().const_zero(),
                "mir_map_any_v2_values_unpack_status_ok",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let unpack_success = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_map_any_v2_values_unpack_success");
        let unpack_failure = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_map_any_v2_values_unpack_failure");
        self.generator
            .builder
            .build_conditional_branch(unpack_ok, unpack_success, unpack_failure)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(unpack_failure);
        self.emit_v2_temporary_owner_abort(
            owner,
            "[E0800] ordinary Map/Any owner-v2 Values payload tag or unpack status is invalid",
            subject,
        )?;

        self.generator.builder.position_at_end(unpack_success);
        let token_drop_fn = self
            .generator
            .get_runtime_fn(V2_PAYLOAD_DROP)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let token_drop_status = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    token_drop_fn,
                    &[BasicMetadataValueEnum::from(owner)],
                    "mir_map_any_v2_values_unpack_token_drop",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| {
            NativeMirError::new(
                subject,
                "ordinary Map/Any V2 Values token Drop returned void",
            )
        })?
        .into_int_value();
        let token_drop_ok = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                token_drop_status,
                self.generator.context.i32_type().const_zero(),
                "mir_map_any_v2_values_unpack_token_drop_ok",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let token_released = self.generator.context.append_basic_block(
            self.llvm_function,
            "mir_map_any_v2_values_unpack_token_released",
        );
        let token_release_failed = self.generator.context.append_basic_block(
            self.llvm_function,
            "mir_map_any_v2_values_unpack_token_release_failed",
        );
        self.generator
            .builder
            .build_conditional_branch(token_drop_ok, token_released, token_release_failed)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(token_release_failed);
        self.generator
            .builder
            .build_call(
                payload_drop_glue,
                &[BasicMetadataValueEnum::from(output)],
                "mir_map_any_v2_values_unpack_failed_output_drop",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_abort_with_message(
            "[E0800] ordinary Map/Any owner-v2 Values temporary token Drop failed",
            subject,
        )?;
        self.generator.builder.position_at_end(token_released);

        let unpacked = self
            .generator
            .builder
            .build_load(
                payload_llvm_ty,
                output,
                "mir_map_any_v2_values_unpack_value",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        if target_is_string {
            let string = unpacked.into_struct_value();
            let data = self
                .generator
                .builder
                .build_extract_value(string, 0, "mir_map_any_v2_values_unpack_string_data")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                .into_pointer_value();
            let len = self
                .generator
                .builder
                .build_extract_value(string, 1, "mir_map_any_v2_values_unpack_string_len")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                .into_int_value();
            return self.emit_v2_values_unpack_copy_string(
                data,
                len,
                output,
                payload_drop_glue,
                subject,
            );
        }
        if target_is_label {
            let label = unpacked.into_struct_value();
            if label.get_type().count_fields() != 1 {
                return Err(NativeMirError::new(
                    subject,
                    "Core-receipted Label Values unpack LLVM aggregate does not have one String field",
                ));
            }
            let text = self
                .generator
                .builder
                .build_extract_value(label, 0, "mir_map_any_v2_values_unpack_label_text")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                .into_struct_value();
            let data = self
                .generator
                .builder
                .build_extract_value(text, 0, "mir_map_any_v2_values_unpack_label_text_data")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                .into_pointer_value();
            let len = self
                .generator
                .builder
                .build_extract_value(text, 1, "mir_map_any_v2_values_unpack_label_text_len")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                .into_int_value();
            let owned_text = self.emit_v2_values_unpack_copy_string(
                data,
                len,
                output,
                payload_drop_glue,
                subject,
            )?;
            let owned_label = self
                .generator
                .builder
                .build_insert_value(
                    payload_llvm_ty.into_struct_type().const_zero(),
                    owned_text,
                    0,
                    "mir_map_any_v2_values_unpack_label_owner",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            return Ok(owned_label.into_struct_value().into());
        }
        self.generator
            .builder
            .build_call(
                payload_drop_glue,
                &[BasicMetadataValueEnum::from(output)],
                "mir_map_any_v2_values_unpack_scalar_temporary_drop",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        Ok(unpacked.into())
    }

    fn emit_v2_values_selector_globals(
        &mut self,
        selection: &crate::core::mir::OrdinaryMapOwnedAnyV2PayloadSelectionReceipt,
        subject: &str,
    ) -> Result<
        (
            inkwell::values::PointerValue<'ctx>,
            inkwell::values::IntValue<'ctx>,
            inkwell::values::PointerValue<'ctx>,
        ),
        NativeMirError,
    > {
        let (frame, registry_hash) =
            ordinary_map_owned_any_v2_values_selector_frame(self.program.type_catalog(), selection)
                .map_err(|message| NativeMirError::new(subject, message))?;
        let hash_hex = registry_hash
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let frame_ptr = self.emit_v2_bytes_global(
            &frame,
            &format!("__mimi_any_payload_v2_values_selection_{hash_hex}"),
            subject,
        )?;
        let hash_ptr = self.emit_v2_bytes_global(
            &registry_hash,
            &format!("__mimi_any_payload_v2_values_hash_{hash_hex}"),
            subject,
        )?;
        Ok((
            frame_ptr,
            self.generator
                .context
                .i64_type()
                .const_int(frame.len() as u64, false),
            hash_ptr,
        ))
    }

    pub(super) fn emit_ordinary_map_owned_any_v2_values(
        &mut self,
        result: &MirValueId,
        root: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let (_receipt, selection) = self.validate_v2_values_receipt(result, root, subject)?;
        let (selection_bytes, selection_len, selection_hash) =
            self.emit_v2_values_selector_globals(&selection, subject)?;
        let root_handle = self.emit_live_v2_root(root, subject)?;
        let values_type = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
        validate_ordinary_map_owned_any_v2_values_list_type(
            self.program.type_catalog(),
            &values_type,
        )
        .map_err(|message| NativeMirError::new(subject, message))?;
        let out_slot = self
            .generator
            .builder
            .build_alloca(
                self.generator
                    .context
                    .ptr_type(inkwell::AddressSpace::default()),
                "mir_map_any_v2_values_list_out",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(
                out_slot,
                self.generator
                    .context
                    .ptr_type(inkwell::AddressSpace::default())
                    .const_null(),
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let values_fn = self
            .generator
            .get_runtime_fn(V2_VALUES_LIST)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let status = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    values_fn,
                    &[
                        BasicMetadataValueEnum::from(root_handle),
                        BasicMetadataValueEnum::from(selection_bytes),
                        BasicMetadataValueEnum::from(selection_len),
                        BasicMetadataValueEnum::from(selection_hash),
                        BasicMetadataValueEnum::from(out_slot),
                    ],
                    "mir_map_any_v2_values_status",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| {
            NativeMirError::new(subject, "ordinary Map/Any owner-v2 Values returned void")
        })?
        .into_int_value();
        let status_ok = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.generator.context.i32_type().const_zero(),
                "mir_map_any_v2_values_status_ok",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_v2_root_guard(
            status_ok,
            "[E0800] ordinary Map/Any owner-v2 Values failed",
            subject,
        )?;
        let list = self
            .generator
            .builder
            .build_load(
                self.generator
                    .context
                    .ptr_type(inkwell::AddressSpace::default()),
                out_slot,
                "mir_map_any_v2_values_list",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_pointer_value();
        let nonnull = self
            .generator
            .builder
            .build_is_not_null(list, "mir_map_any_v2_values_list_nonnull")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_v2_root_guard(
            nonnull,
            "[E0800] ordinary Map/Any owner-v2 Values returned a null List",
            subject,
        )?;
        Ok(list.into())
    }

    pub(super) fn emit_ordinary_map_owned_any_v2_values_len(
        &mut self,
        result: &MirValueId,
        list: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let values_type = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
        if self.value_type(list, subject)? != values_type {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Values Len receiver has another TypeDesc",
            ));
        }
        let receipts = self
            .program
            .ordinary_map_owned_any_v2_values_result_receipts()
            .map_err(|message| NativeMirError::new(subject, message))?;
        let matching = receipts
            .iter()
            .filter(|receipt| receipt.owner == self.function.owner && receipt.binding == *list)
            .collect::<Vec<_>>();
        let [receipt] = matching.as_slice() else {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Values Len is not bound to one receipted snapshot owner",
            ));
        };
        receipt
            .validate()
            .and_then(|()| receipt.validate_against_program(self.program))
            .map_err(|message| NativeMirError::new(subject, message))?;
        validate_ordinary_map_owned_any_v2_values_list_type(
            self.program.type_catalog(),
            &values_type,
        )
        .map_err(|message| NativeMirError::new(subject, message))?;
        let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        if self.value_type(result, subject)? != i32_type {
            return Err(NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Values Len result is not canonical i32",
            ));
        }
        self.program
            .type_catalog()
            .validate_copy_scalar(&i32_type)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let list_handle = self.value(list, subject)?.into_pointer_value();
        let len_fn = self
            .generator
            .get_runtime_fn("mimi_mir_list_len_managed_any_v2")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    len_fn,
                    &[BasicMetadataValueEnum::from(list_handle)],
                    "mir_map_any_v2_values_len",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .map(Into::into)
        .ok_or_else(|| {
            NativeMirError::new(
                subject,
                "ordinary Map/Any owner-v2 Values Len returned void",
            )
        })
    }

    pub(super) fn emit_ordinary_map_owned_any_v2_drop(
        &mut self,
        root: &MirValueId,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let handle = self.emit_live_v2_root(root, subject)?;
        let ty = self.value_type(root, subject)?;
        self.program
            .type_catalog()
            .validate_glue(&ty, MirGlueOperation::Drop)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let drop_fn = self
            .generator
            .get_runtime_fn(V2_DROP)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let status = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(drop_fn, &[handle.into()], "mir_map_any_v2_drop")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| {
            NativeMirError::new(subject, "ordinary Map/Any owner-v2 Drop returned void")
        })?
        .into_int_value();
        let ok = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.generator.context.i32_type().const_zero(),
                "mir_map_any_v2_drop_ok",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_v2_root_guard(ok, "[E0800] ordinary Map/Any owner-v2 Drop failed", subject)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn run_generated_callback_child(
        test: &str,
        environment: &str,
        mode: &str,
    ) -> std::process::Output {
        let mut command =
            std::process::Command::new(std::env::current_exe().expect("test executable"));
        command
            .args([
                "--exact",
                test,
                "--ignored",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(environment, mode);
        run_generated_callback_command(&mut command, &format!("{test} ({mode})"))
    }

    fn run_generated_callback_command(
        command: &mut std::process::Command,
        label: &str,
    ) -> std::process::Output {
        use std::io::Read;
        use std::process::Stdio;

        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn isolated generated-callback child");
        let mut stdout = child.stdout.take().expect("child stdout");
        let mut stderr = child.stderr.take().expect("child stderr");
        let stdout_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).expect("read child stdout");
            bytes
        });
        let stderr_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).expect("read child stderr");
            bytes
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut timed_out = false;
        let status = loop {
            if let Some(status) = child.try_wait().expect("poll callback child") {
                break status;
            }
            if std::time::Instant::now() >= deadline {
                timed_out = true;
                child.kill().expect("terminate stuck callback child");
                break child.wait().expect("reap timed-out callback child");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let output = std::process::Output {
            status,
            stdout: stdout_reader.join().expect("stdout reader"),
            stderr: stderr_reader.join().expect("stderr reader"),
        };
        assert!(
            !timed_out,
            "generated-callback child {label} exceeded 30s:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    static GENERATED_CALLBACK_MALLOC_FAILED: AtomicBool = AtomicBool::new(false);
    static GENERATED_CALLBACK_FAILURE_MAPS: AtomicUsize = AtomicUsize::new(0);

    /// Only generated LLVM calls reach this mapping. The host Rust allocator
    /// and the real runtime retain their normal allocation behavior.
    unsafe extern "C" fn fail_generated_clone_malloc_with_live_map_lease(
        size: usize,
    ) -> *mut libc::c_void {
        let maps = crate::runtime::ordinary_map_domain_live_counts_for_test()
            .map(|counts| counts.runtime_map_handles)
            .unwrap_or(usize::MAX);
        // New and Set have just one root. The second root is published before
        // Clone starts, so this fails the actual payload callback while Clone
        // holds that root's MapLease, rather than an earlier creator callback.
        if maps == 2 && !GENERATED_CALLBACK_MALLOC_FAILED.swap(true, Ordering::SeqCst) {
            GENERATED_CALLBACK_FAILURE_MAPS.store(maps, Ordering::SeqCst);
            return std::ptr::null_mut();
        }
        // SAFETY: forward the unchanged allocation request to the real libc;
        // this symbol mapping affects generated LLVM imports only.
        unsafe { libc::malloc(size) }
    }

    unsafe extern "C" fn generated_callback_oom_error_handler(message: *const libc::c_char) {
        let injected = GENERATED_CALLBACK_MALLOC_FAILED.load(Ordering::SeqCst)
            && GENERATED_CALLBACK_FAILURE_MAPS.load(Ordering::SeqCst) == 2;
        let counts = crate::runtime::ordinary_map_domain_live_counts_for_test();
        let settled = counts
            .as_ref()
            .is_ok_and(|counts| *counts == crate::runtime::OrdinaryMapDomainLiveCounts::default());
        let typed_diagnostic = !message.is_null()
            // SAFETY: runtime_abort passes a live NUL-terminated diagnostic
            // snapshot and keeps it readable throughout this host callback.
            && unsafe { std::ffi::CStr::from_ptr(message) }
                .to_bytes()
                .windows(b"[E0800]".len())
                .any(|window| window == b"[E0800]");
        eprintln!(
            "generated-v2-callback-oom injected={injected} settled={settled} typed={typed_diagnostic} counts={counts:?}"
        );
        let code = if !injected {
            96
        } else if !settled {
            97
        } else if !typed_diagnostic {
            98
        } else {
            0
        };
        // SAFETY: this purpose-built child must observe cleanup before it
        // terminates. It never unwinds through the generated callback ABI.
        unsafe { libc::_exit(code) }
    }

    #[test]
    fn ordinary_map_owned_any_v2_generated_callback_oom_settles_before_abort_handler() {
        let output = run_generated_callback_child(
            "codegen::mir::ordinary_map_owned_any_v2::tests::ordinary_map_owned_any_v2_generated_callback_oom_child",
            "MIMI_V2_GENERATED_CALLBACK_OOM_CHILD",
            "1",
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stdout.contains("running 1 test"), "{stdout}\n{stderr}");
        assert_eq!(
            output.status.code(),
            Some(0),
            "generated clone must return through the real runtime, release its MapLease, and settle every root/Any/String before the real abort handler (97 means live owners survived):\n{stdout}\n{stderr}"
        );
        assert!(
            stderr.contains("generated-v2-callback-oom injected=true settled=true typed=true"),
            "child did not observe the intended allocation failure and cleanup: {stderr}"
        );
    }

    #[test]
    #[ignore = "isolated generated-callback OOM subprocess helper"]
    fn ordinary_map_owned_any_v2_generated_callback_oom_child() {
        if std::env::var_os("MIMI_V2_GENERATED_CALLBACK_OOM_CHILD").is_none() {
            return;
        }
        assert_eq!(
            crate::runtime::ordinary_map_domain_live_counts_for_test(),
            Ok(crate::runtime::OrdinaryMapDomainLiveCounts::default())
        );
        let source = r#"
            type Label { text: string }
            func main() -> i32 {
                let label = Label { text: "generated callback payload" }
                let empty = map_new()
                let root = map_set(empty, "label", label)
                let copied = root
                drop(copied)
                drop(root)
                drop(empty)
                drop(label)
                0
            }
        "#;
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "generated_v2_callback_oom");
        generator
            .compile_mir_native_for_component(&program)
            .expect("emit the real staged V2 native adapter");
        generator.module.verify().expect("valid generated LLVM");
        let native_ir = generator.module.print_to_string().to_string();
        assert_eq!(
            native_ir
                .matches("call i32 @mimi_ordinary_map_owned_any_v2_clone(")
                .count(),
            1
        );
        assert!(native_ir.contains("mimi_any_payload_v2_clone_glue__record"));
        let engine = generator
            .module
            .create_jit_execution_engine(inkwell::OptimizationLevel::None)
            .expect("generated callback execution engine");
        for (symbol, address) in [
            (
                "malloc",
                fail_generated_clone_malloc_with_live_map_lease as *const () as usize,
            ),
            (
                "mimi_ordinary_map_owned_any_v2_new",
                crate::runtime::mimi_ordinary_map_owned_any_v2_new as *const () as usize,
            ),
            (
                "mimi_ordinary_map_owned_any_v2_set",
                crate::runtime::mimi_ordinary_map_owned_any_v2_set as *const () as usize,
            ),
            (
                "mimi_ordinary_map_owned_any_v2_clone",
                crate::runtime::mimi_ordinary_map_owned_any_v2_clone as *const () as usize,
            ),
            (
                "mimi_ordinary_map_owned_any_v2_drop",
                crate::runtime::mimi_ordinary_map_owned_any_v2_drop as *const () as usize,
            ),
            (
                "mimi_any_payload_v2_new",
                crate::runtime::mimi_any_payload_v2_new as *const () as usize,
            ),
            (
                "mimi_any_payload_v2_drop",
                crate::runtime::mimi_any_payload_v2_drop as *const () as usize,
            ),
            (
                "mimi_mir_ordinary_map_domain_own_string",
                crate::runtime::mimi_mir_ordinary_map_domain_own_string as *const () as usize,
            ),
            (
                "mimi_str_clone",
                crate::runtime::mimi_str_clone as *const () as usize,
            ),
            (
                "mimi_string_free",
                crate::runtime::mimi_string_free as *const () as usize,
            ),
            (
                "mimi_runtime_abort",
                crate::runtime::mimi_runtime_abort as *const () as usize,
            ),
        ] {
            let declaration = generator
                .module
                .get_function(symbol)
                .unwrap_or_else(|| panic!("missing native import {symbol}"));
            assert_eq!(
                declaration.count_basic_blocks(),
                0,
                "{symbol} must remain an import"
            );
            engine.add_global_mapping(&declaration, address);
        }
        let main = generator.module.get_function("main").expect("native main");
        assert_eq!(main.count_params(), 0);
        assert_eq!(
            main.get_type().get_return_type(),
            Some(context.i32_type().into())
        );
        crate::runtime::mimi_runtime_set_error_handler(Some(generated_callback_oom_error_handler));
        // SAFETY: the assertions establish main's exact C ABI, every runtime
        // import is real, and the engine remains live throughout execution.
        let native = unsafe { engine.get_function::<unsafe extern "C" fn() -> i32>("main") }
            .expect("native main entry");
        // SAFETY: this is the just-validated zero-argument native main. The
        // intended path exits from the genuine runtime error handler above.
        let value = unsafe { native.call() };
        panic!("the generated Map clone unexpectedly survived its injected OOM: {value}");
    }

    static DIRECT_CALLBACK_MALLOC_CALLS: AtomicUsize = AtomicUsize::new(0);
    static DIRECT_CALLBACK_MALLOC_FAIL_AT: AtomicUsize = AtomicUsize::new(usize::MAX);
    static DIRECT_CALLBACK_MALLOC_FAILURES: AtomicUsize = AtomicUsize::new(0);
    static DIRECT_CALLBACK_LIVE_ALLOCATIONS: std::sync::atomic::AtomicIsize =
        std::sync::atomic::AtomicIsize::new(0);

    unsafe extern "C" fn direct_callback_malloc(size: usize) -> *mut libc::c_void {
        let attempt = DIRECT_CALLBACK_MALLOC_CALLS.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt == DIRECT_CALLBACK_MALLOC_FAIL_AT.load(Ordering::SeqCst) {
            DIRECT_CALLBACK_MALLOC_FAILURES.fetch_add(1, Ordering::SeqCst);
            return std::ptr::null_mut();
        }
        // SAFETY: forward the unchanged request to libc. The host allocator
        // does not pass through this generated-module-only mapping.
        let allocation = unsafe { libc::malloc(size) };
        if !allocation.is_null() {
            DIRECT_CALLBACK_LIVE_ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        }
        allocation
    }

    unsafe extern "C" fn direct_callback_free(allocation: *mut libc::c_void) {
        if !allocation.is_null() {
            DIRECT_CALLBACK_LIVE_ALLOCATIONS.fetch_sub(1, Ordering::SeqCst);
        }
        // SAFETY: this observer changes no allocator behavior. Valid generated
        // Drop glue supplies the same malloc base at most once, or null.
        unsafe { libc::free(allocation) };
    }

    unsafe extern "C" fn direct_callback_unexpected_abort(_message: *const libc::c_char) {
        eprintln!("generated-v2-direct unexpected abort instead of a clone status");
        // SAFETY: abort is a regression here. Terminate only this dedicated
        // subprocess, without unwinding through the callback ABI.
        unsafe { libc::_exit(99) }
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct DirectString {
        data: *const u8,
        len: i64,
    }

    #[repr(C)]
    struct DirectStringPair {
        first: DirectString,
        second: DirectString,
    }

    #[repr(C)]
    struct DirectOption {
        active: u8,
        payload: DirectString,
    }

    #[repr(C)]
    struct DirectResult {
        active: u8,
        ok: DirectString,
        error: i64,
    }

    #[repr(C, align(8))]
    struct DirectStorage([u8; 64]);

    fn assert_direct_string(value: DirectString, source: DirectString) {
        assert_eq!(value.len, source.len);
        assert!(!value.data.is_null());
        assert_ne!(value.data, source.data, "clone must own a private leaf");
        // SAFETY: successful generated Clone initialized exactly `len` bytes;
        // the expected source span is the static payload passed below.
        assert_eq!(
            unsafe { std::slice::from_raw_parts(value.data, value.len as usize) },
            unsafe { std::slice::from_raw_parts(source.data, source.len as usize) }
        );
    }

    fn generated_callback_shape<'ctx>(
        context: &'ctx Context,
        mode: &str,
    ) -> (OwnershipClass, BasicTypeEnum<'ctx>) {
        let pointer = context.ptr_type(inkwell::AddressSpace::default());
        let string = context.struct_type(&[pointer.into(), context.i64_type().into()], false);
        let option = context.struct_type(&[context.bool_type().into(), string.into()], false);
        let result = context.struct_type(
            &[
                context.bool_type().into(),
                string.into(),
                context.i64_type().into(),
            ],
            false,
        );
        match mode {
            "record-second" => (
                OwnershipClass::Record(vec![OwnershipClass::StringBox, OwnershipClass::StringBox]),
                context
                    .struct_type(&[string.into(), string.into()], false)
                    .into(),
            ),
            "option-some" | "option-none-poison" => (
                OwnershipClass::Record(vec![OwnershipClass::Option(Box::new(
                    OwnershipClass::StringBox,
                ))]),
                context.struct_type(&[option.into()], false).into(),
            ),
            "result-ok-poison" | "result-err-poison" | "result-err-header" | "result-err-empty" => {
                (
                    OwnershipClass::Record(vec![OwnershipClass::Result {
                        ok: Box::new(OwnershipClass::StringBox),
                        error: Box::new(OwnershipClass::StringBox),
                    }]),
                    context.struct_type(&[result.into()], false).into(),
                )
            }
            _ => (OwnershipClass::StringBox, string.into()),
        }
    }

    const GENERATED_CALLBACK_MODES: [&str; 12] = [
        "string-first",
        "string-null-empty",
        "string-negative",
        "string-max-length",
        "string-near-max-length",
        "record-second",
        "option-some",
        "option-none-poison",
        "result-ok-poison",
        "result-err-poison",
        "result-err-header",
        "result-err-empty",
    ];

    #[test]
    #[ignore = "0.1.11 native callback L3 requires Valgrind and the host C compiler"]
    fn ordinary_map_owned_any_v2_generated_callbacks_strict_native_memcheck() {
        let output_root = std::env::var_os("MIMI_V2_CALLBACK_L3_EVIDENCE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::env::temp_dir().join(format!(
                    "mimi-v2-callback-l3-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .expect("system clock")
                        .as_nanos()
                ))
            });
        std::fs::create_dir_all(&output_root).expect("native callback evidence directory");
        let harness = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/v2_callback_oom_harness.c");
        for mode in GENERATED_CALLBACK_MODES {
            let directory = output_root.join(mode);
            std::fs::create_dir_all(&directory).expect("native callback mode directory");
            let context = Context::create();
            let generator = CodeGenerator::new(&context, "v2_callback_native_l3");
            let target = generator.create_target_machine().expect("host target");
            generator.module.set_triple(&target.get_triple());
            generator
                .module
                .set_data_layout(&target.get_target_data().get_data_layout());
            let (class, llvm_type) = generated_callback_shape(&context, mode);
            let (clone, drop) = generator
                .ensure_any_payload_v2_glue(&class, llvm_type)
                .expect("real generated callback pair");
            clone.set_linkage(inkwell::module::Linkage::External);
            clone.as_global_value().set_name("mimi_v2_test_clone");
            drop.set_linkage(inkwell::module::Linkage::External);
            drop.as_global_value().set_name("mimi_v2_test_drop");
            generator.module.verify().expect("native callback LLVM");
            generator
                .module
                .print_to_file(directory.join("callbacks.ll"))
                .expect("preserve generated callback IR");
            let object = directory.join("callbacks.o");
            target
                .write_to_file(
                    &generator.module,
                    inkwell::targets::FileType::Object,
                    &object,
                )
                .expect("emit native callback object");
            let binary = directory.join("callback-test");
            let mut compiler = std::process::Command::new("cc");
            compiler
                .args([
                    "-std=c11",
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                    "-fno-strict-aliasing",
                    "-no-pie",
                    "-Wl,--wrap=malloc",
                    "-Wl,--wrap=free",
                ])
                .arg(&harness)
                .arg(&object)
                .arg("-o")
                .arg(&binary);
            std::fs::write(directory.join("cc-command.txt"), format!("{compiler:?}\n"))
                .expect("preserve native compile command");
            let compiled = run_generated_callback_command(&mut compiler, mode);
            assert!(
                compiled.status.success(),
                "{mode}: native callback link failed:\n{}",
                String::from_utf8_lossy(&compiled.stderr)
            );
            let log_path = directory.join("memcheck.log");
            let mut valgrind = std::process::Command::new("valgrind");
            valgrind
                .args([
                    "--tool=memcheck",
                    "--leak-check=full",
                    "--show-leak-kinds=all",
                    "--errors-for-leak-kinds=definite,indirect,possible",
                    "--error-exitcode=91",
                    "--track-origins=yes",
                ])
                .arg(format!("--log-file={}", log_path.display()))
                .arg(&binary)
                .arg(mode);
            std::fs::write(
                directory.join("valgrind-command.txt"),
                format!("{valgrind:?}\n"),
            )
            .expect("preserve Memcheck command");
            let observed = run_generated_callback_command(&mut valgrind, mode);
            std::fs::write(directory.join("stdout.log"), &observed.stdout).expect("native stdout");
            std::fs::write(directory.join("stderr.log"), &observed.stderr).expect("native stderr");
            let log = std::fs::read_to_string(&log_path).expect("native Memcheck report");
            assert!(
                observed.status.success()
                    && String::from_utf8_lossy(&observed.stdout)
                        .contains(&format!("v2-callback-native-ok mode={mode}")),
                "{mode}: native Memcheck failed: {:?}\n{}\n{log}",
                observed.status,
                String::from_utf8_lossy(&observed.stderr)
            );
            let no_loss = log.contains("All heap blocks were freed -- no leaks are possible")
                || [
                    "definitely lost: 0 bytes",
                    "indirectly lost: 0 bytes",
                    "possibly lost: 0 bytes",
                ]
                .iter()
                .all(|entry| log.contains(entry));
            assert!(
                no_loss && log.contains("ERROR SUMMARY: 0 errors"),
                "{mode}: strict native callback memory gate failed:\n{log}"
            );
            eprintln!("native-v2-callback-memcheck mode={mode} lost=0 errors=0");
        }
    }

    #[test]
    fn ordinary_map_owned_any_v2_generated_callbacks_are_fallible_and_rollback() {
        for mode in GENERATED_CALLBACK_MODES {
            let output = run_generated_callback_child(
                "codegen::mir::ordinary_map_owned_any_v2::tests::ordinary_map_owned_any_v2_generated_direct_callback_child",
                "MIMI_V2_GENERATED_DIRECT_CALLBACK_CHILD",
                mode,
            );
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stdout.contains("running 1 test"),
                "{mode}: {stdout}\n{stderr}"
            );
            assert_eq!(output.status.code(), Some(0), "{mode}: {stdout}\n{stderr}");
            assert!(
                stderr.contains("generated-v2-direct completed"),
                "{mode}: {stderr}"
            );
        }
    }

    #[test]
    #[ignore = "isolated generated-callback status/rollback subprocess helper"]
    fn ordinary_map_owned_any_v2_generated_direct_callback_child() {
        let Ok(mode) = std::env::var("MIMI_V2_GENERATED_DIRECT_CALLBACK_CHILD") else {
            return;
        };
        let context = Context::create();
        let generator = CodeGenerator::new(&context, "generated_v2_direct_callback");
        let (class, llvm_type) = generated_callback_shape(&context, &mode);
        let (clone, drop) = generator
            .ensure_any_payload_v2_glue(&class, llvm_type)
            .expect("the real generated V2 callback pair");
        let clone_name = clone.get_name().to_str().expect("clone symbol").to_owned();
        let drop_name = drop.get_name().to_str().expect("drop symbol").to_owned();
        generator.module.verify().expect("valid callback LLVM");
        let engine = generator
            .module
            .create_jit_execution_engine(inkwell::OptimizationLevel::None)
            .expect("direct generated callback execution engine");
        for (symbol, address) in [
            ("malloc", direct_callback_malloc as *const () as usize),
            ("free", direct_callback_free as *const () as usize),
            (
                "mimi_runtime_abort",
                crate::runtime::mimi_runtime_abort as *const () as usize,
            ),
        ] {
            if let Some(import) = generator.module.get_function(symbol) {
                assert_eq!(import.count_basic_blocks(), 0);
                engine.add_global_mapping(&import, address);
            }
        }
        // SAFETY: ensure_any_payload_v2_glue emits exactly these pointer-based
        // C callback ABIs, and the engine stays live until both calls finish.
        let native_clone = unsafe {
            engine.get_function::<unsafe extern "C" fn(*mut u8, *const u8) -> i32>(&clone_name)
        }
        .expect("generated clone entry");
        // SAFETY: the same checked callback pair declares void Drop(ptr).
        let native_drop =
            unsafe { engine.get_function::<unsafe extern "C" fn(*mut u8)>(&drop_name) }
                .expect("generated drop entry");
        let text = DirectString {
            data: b"payload\0".as_ptr(),
            len: 7,
        };
        let poison = DirectString {
            data: std::ptr::without_provenance(1),
            len: -1,
        };
        let error_header = text;
        let mut source = DirectStorage([0; 64]);
        // SAFETY: the aligned source storage fits every repr(C) shape. Their
        // fields match the LLVM types above; poison leaves are inactive only.
        unsafe {
            match mode.as_str() {
                "record-second" => std::ptr::write(
                    source.0.as_mut_ptr().cast::<DirectStringPair>(),
                    DirectStringPair {
                        first: text,
                        second: text,
                    },
                ),
                "option-some" | "option-none-poison" => std::ptr::write(
                    source.0.as_mut_ptr().cast::<DirectOption>(),
                    DirectOption {
                        active: u8::from(mode == "option-some"),
                        payload: if mode == "option-some" { text } else { poison },
                    },
                ),
                "result-ok-poison" | "result-err-poison" | "result-err-header"
                | "result-err-empty" => std::ptr::write(
                    source.0.as_mut_ptr().cast::<DirectResult>(),
                    DirectResult {
                        active: u8::from(mode == "result-ok-poison"),
                        ok: if mode == "result-ok-poison" {
                            text
                        } else {
                            poison
                        },
                        error: if mode == "result-ok-poison" {
                            1
                        } else if mode == "result-err-empty" {
                            0
                        } else {
                            (&error_header as *const DirectString) as usize as i64
                        },
                    },
                ),
                _ => std::ptr::write(
                    source.0.as_mut_ptr().cast::<DirectString>(),
                    match mode.as_str() {
                        "string-null-empty" => DirectString {
                            data: std::ptr::null(),
                            len: 0,
                        },
                        "string-negative" => DirectString { len: -1, ..text },
                        "string-max-length" => DirectString {
                            len: i64::MAX,
                            ..text
                        },
                        "string-near-max-length" => DirectString {
                            len: i64::MAX - 7,
                            ..text
                        },
                        _ => text,
                    },
                ),
            }
        }
        let expected_failure = matches!(
            mode.as_str(),
            "string-first"
                | "string-negative"
                | "string-max-length"
                | "string-near-max-length"
                | "record-second"
                | "result-err-header"
        );
        let expected_attempts = match mode.as_str() {
            "string-first" | "option-some" | "result-ok-poison" => 1,
            "record-second" | "result-err-poison" | "result-err-header" => 2,
            _ => 0,
        };
        let fail_at = match mode.as_str() {
            "string-first" | "string-negative" | "string-max-length" | "string-near-max-length" => {
                1
            }
            "record-second" | "result-err-header" => 2,
            _ => usize::MAX,
        };
        DIRECT_CALLBACK_MALLOC_FAIL_AT.store(fail_at, Ordering::SeqCst);
        let mut destination = DirectStorage([0xa5; 64]);
        crate::runtime::mimi_runtime_set_error_handler(Some(direct_callback_unexpected_abort));
        // SAFETY: both storage addresses are aligned and large enough for the
        // selected exact source/destination shape; destination starts unowned.
        let status = unsafe { native_clone.call(destination.0.as_mut_ptr(), source.0.as_ptr()) };
        assert_eq!(
            status != 0,
            expected_failure,
            "{mode}: callback status {status}"
        );
        assert_eq!(
            DIRECT_CALLBACK_MALLOC_CALLS.load(Ordering::SeqCst),
            expected_attempts,
            "{mode}: unexpected allocation before length/tag validation"
        );
        let expected_injected = usize::from(matches!(
            mode.as_str(),
            "string-first" | "record-second" | "result-err-header"
        ));
        assert_eq!(
            DIRECT_CALLBACK_MALLOC_FAILURES.load(Ordering::SeqCst),
            expected_injected
        );
        if expected_failure {
            assert_eq!(
                destination.0, [0xa5; 64],
                "{mode}: failure published a partial destination"
            );
        } else {
            // SAFETY: successful Clone initialized the exact selected shape;
            // only active payloads are read below.
            unsafe {
                match mode.as_str() {
                    "string-null-empty" => {
                        let output = &*destination.0.as_ptr().cast::<DirectString>();
                        assert!(output.data.is_null());
                        assert_eq!(output.len, 0);
                    }
                    "option-some" | "option-none-poison" => {
                        let output = &*destination.0.as_ptr().cast::<DirectOption>();
                        assert_eq!(output.active, u8::from(mode == "option-some"));
                        if mode == "option-some" {
                            assert_direct_string(output.payload, text);
                        } else {
                            assert!(output.payload.data.is_null());
                            assert_eq!(output.payload.len, 0);
                        }
                    }
                    "result-ok-poison" | "result-err-poison" | "result-err-empty" => {
                        let output = &*destination.0.as_ptr().cast::<DirectResult>();
                        assert_eq!(output.active, u8::from(mode == "result-ok-poison"));
                        if mode == "result-ok-poison" {
                            assert_direct_string(output.ok, text);
                            assert_eq!(output.error, 0);
                        } else {
                            assert!(output.ok.data.is_null());
                            assert_eq!(output.ok.len, 0);
                            if mode == "result-err-empty" {
                                assert_eq!(output.error, 0);
                            } else {
                                assert_ne!(output.error, 0);
                                assert_direct_string(
                                    *(output.error as usize as *const DirectString),
                                    text,
                                );
                            }
                        }
                    }
                    _ => unreachable!("unexpected success mode"),
                }
                native_drop.call(destination.0.as_mut_ptr());
            }
        }
        assert_eq!(
            DIRECT_CALLBACK_LIVE_ALLOCATIONS.load(Ordering::SeqCst),
            0,
            "{mode}: prefix/data allocation survived failure or Drop"
        );
        if matches!(
            mode.as_str(),
            "string-first" | "record-second" | "result-err-header"
        ) {
            // A rejected destination remains reusable. The same generated
            // callback must subsequently succeed and its exact Drop must free
            // every data/header allocation once after fault injection ends.
            DIRECT_CALLBACK_MALLOC_CALLS.store(0, Ordering::SeqCst);
            DIRECT_CALLBACK_MALLOC_FAIL_AT.store(usize::MAX, Ordering::SeqCst);
            // SAFETY: failure left this aligned destination wholly unowned;
            // the unchanged source still contains the original borrowed data.
            assert_eq!(
                unsafe { native_clone.call(destination.0.as_mut_ptr(), source.0.as_ptr()) },
                0
            );
            // SAFETY: the successful retry initialized the exact selected ABI.
            unsafe {
                match mode.as_str() {
                    "string-first" => {
                        assert_direct_string(*destination.0.as_ptr().cast::<DirectString>(), text)
                    }
                    "record-second" => {
                        let output = &*destination.0.as_ptr().cast::<DirectStringPair>();
                        assert_direct_string(output.first, text);
                        assert_direct_string(output.second, text);
                        assert_ne!(output.first.data, output.second.data);
                    }
                    "result-err-header" => {
                        let output = &*destination.0.as_ptr().cast::<DirectResult>();
                        assert_eq!(output.active, 0);
                        assert!(output.ok.data.is_null());
                        assert_eq!(output.ok.len, 0);
                        assert_ne!(output.error, 0);
                        assert_direct_string(*(output.error as usize as *const DirectString), text);
                    }
                    _ => unreachable!(),
                }
                native_drop.call(destination.0.as_mut_ptr());
            }
            assert_eq!(
                DIRECT_CALLBACK_MALLOC_CALLS.load(Ordering::SeqCst),
                if mode == "string-first" { 1 } else { 2 }
            );
            assert_eq!(
                DIRECT_CALLBACK_LIVE_ALLOCATIONS.load(Ordering::SeqCst),
                0,
                "{mode}: successful retry Drop retained data/header storage"
            );
        }
        assert_eq!(
            crate::runtime::ordinary_map_domain_live_counts_for_test(),
            Ok(crate::runtime::OrdinaryMapDomainLiveCounts::default())
        );
        crate::runtime::mimi_runtime_set_error_handler(None);
        eprintln!("generated-v2-direct completed mode={mode} status={status} allocations={expected_attempts} live=0");
        // Return normally: LLVM/JIT resources are dropped and this exact child
        // can run under Valgrind without an _exit-based leak-check shortcut.
    }

    #[test]
    fn size_status_maps_overflow_and_fails_closed() {
        assert_eq!(V2_STATUS_CAPACITY, 3);
        assert_eq!(V2_STATUS_SIZE_OVERFLOW, 17);
        assert_eq!(v2_size_status_error_message(0), None);
        assert_eq!(
            v2_size_status_error_message(V2_STATUS_CAPACITY),
            Some(V2_SIZE_FAILURE_ERROR)
        );
        assert_eq!(
            v2_size_status_error_message(V2_STATUS_SIZE_OVERFLOW),
            Some(V2_SIZE_OVERFLOW_ERROR)
        );
        assert_eq!(
            v2_size_status_error_message(99),
            Some(V2_SIZE_FAILURE_ERROR)
        );
    }
}
