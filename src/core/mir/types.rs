//! Backend-independent type descriptors consumed by canonical MIR passes.
//!
//! `ResolvedTypeId` remains the stable identity. `MirTypeDesc` is the first
//! materialized semantic view of that identity: it records ownership and an
//! abstract ABI class without importing LLVM, runtime handles, or bytecode
//! opcodes. Backends may map the ABI class to a physical layout later, but may
//! not re-derive ownership from that layout.

use std::collections::{BTreeMap, BTreeSet};

use crate::core::ir::{
    FunctionTypeAbi, OwnershipTypeKind, PrimitiveType, ResolvedProjection, ResolvedType,
    ResolvedTypeId, ResolvedTypeTable,
};
use crate::core::{CheckedProgram, NodeId, ResolvedTypeKind};

pub const MIR_TYPE_DESC_SCHEMA_VERSION: &str = "mimi-mir-type-desc-6";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirOwnership {
    Copy,
    Move,
    Linear,
    SharedBorrow,
    WeakBorrow,
}

impl MirOwnership {
    pub fn needs_drop(self) -> bool {
        matches!(self, Self::Move | Self::Linear)
    }

    pub fn needs_clone(self) -> bool {
        !matches!(self, Self::Copy)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirAbiClass {
    Unit,
    Integer { bits: u16, signed: bool },
    Float { bits: u16 },
    Bool,
    Char,
    StringHandle,
    OpaqueHandle,
    Pointer,
    Aggregate,
    FunctionPointer,
}

/// Backend-independent implementation selected for one ownership boundary.
///
/// `OwnedString` is a semantic contract, not a VM/LLVM representation: every
/// consumer must implement the same retain/release/transfer behavior for an
/// owned Mimi string. `Aggregate` is reserved for a recursively materialized
/// product contract. `Unsupported` is deliberately explicit so a backend
/// cannot turn an unmodelled aggregate into an accidental shallow copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirGlueKind {
    Noop,
    OwnedString,
    Aggregate,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirGlueOperation {
    MoveOut,
    Clone,
    Drop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MirGlueContract {
    pub move_out: MirGlueKind,
    pub clone: MirGlueKind,
    pub drop: MirGlueKind,
}

/// Canonical field-level drop schedule for an aggregate product.
///
/// The schedule is stored in destruction order (reverse declaration order),
/// while `index` remains the declaration slot used by the semantic layout.
/// Nested aggregate fields refer back to their child `MirTypeDesc`, so the
/// complete recursive schedule is carried by the TypeDesc graph rather than
/// reconstructed by a backend from a physical tuple representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirDropGluePlan {
    pub fields: Vec<MirDropGlueField>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirDropGlueField {
    pub index: usize,
    pub ty: ResolvedTypeId,
    pub glue: MirGlueKind,
}

impl MirGlueContract {
    fn for_type(kind: &MirTypeKind, ownership: MirOwnership) -> Self {
        if ownership == MirOwnership::Copy {
            return Self {
                move_out: MirGlueKind::Noop,
                clone: MirGlueKind::Noop,
                drop: MirGlueKind::Noop,
            };
        }
        if matches!(kind, MirTypeKind::Primitive(PrimitiveType::String))
            && ownership == MirOwnership::Move
        {
            return Self {
                move_out: MirGlueKind::OwnedString,
                clone: MirGlueKind::OwnedString,
                drop: MirGlueKind::OwnedString,
            };
        }
        Self {
            move_out: MirGlueKind::Unsupported,
            clone: MirGlueKind::Unsupported,
            drop: MirGlueKind::Unsupported,
        }
    }

    pub fn supports_move_out(self) -> bool {
        self.move_out != MirGlueKind::Unsupported
    }

    pub fn supports_clone(self) -> bool {
        self.clone != MirGlueKind::Unsupported
    }

    pub fn supports_drop(self) -> bool {
        self.drop != MirGlueKind::Unsupported
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirTypeKind {
    Primitive(PrimitiveType),
    GenericParameter,
    Nominal,
    FlowStateSet,
    Reference { mutable: bool },
    Option,
    Result,
    Tuple { arity: usize },
    Function { abi: FunctionTypeAbi, arity: usize },
    CBuffer,
    Capability,
    Ownership(OwnershipTypeKind),
    Newtype,
    Array { length: usize },
    Slice,
    Trait,
    RawPointer { mutable: bool },
    DynamicAny,
}

/// Backend-independent semantic layout.  This is deliberately not a byte
/// offset/size description: target ABI lowering owns those physical details,
/// while every consumer must agree on the aggregate shape and its canonical
/// field identities first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirLayout {
    Unit,
    Scalar,
    Handle,
    Pointer,
    Tuple(Vec<ResolvedTypeId>),
    Option {
        inner: ResolvedTypeId,
        variants: Vec<MirVariantDesc>,
    },
    Result {
        ok: ResolvedTypeId,
        error: ResolvedTypeId,
        variants: Vec<MirVariantDesc>,
    },
    Array {
        element: ResolvedTypeId,
        length: usize,
    },
    Newtype {
        nominal: crate::core::NominalTypeId,
        inner: ResolvedTypeId,
    },
    Record {
        nominal: crate::core::NominalTypeId,
        fields: Vec<MirFieldDesc>,
    },
    Opaque,
}

/// Canonical field contract used by aggregate lowering.  The declaration
/// order is preserved, while the field identity and type are checker-owned
/// values; no backend may recover either from a surface AST or a native struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirFieldDesc {
    pub id: NodeId,
    pub name: String,
    pub ty: ResolvedTypeId,
}

/// Canonical discriminant/payload contract for one variant. The discriminant
/// is semantic and stable; bytecode/native encodings may choose a physical
/// representation only after consuming this descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirVariantDesc {
    pub id: NodeId,
    pub name: String,
    pub discriminant: u16,
    pub fields: Vec<MirFieldDesc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirTypeDesc {
    pub id: ResolvedTypeId,
    pub kind: MirTypeKind,
    pub layout: MirLayout,
    pub ownership: MirOwnership,
    pub abi: MirAbiClass,
    pub needs_drop_glue: bool,
    pub needs_clone_glue: bool,
    pub glue: MirGlueContract,
    pub drop_plan: Option<MirDropGluePlan>,
}

impl MirTypeDesc {
    fn from_resolved(id: &ResolvedTypeId, ty: &ResolvedType, ownership: MirOwnership) -> Self {
        let (kind, abi, layout) = match ty {
            ResolvedType::Primitive(primitive) => (
                MirTypeKind::Primitive(*primitive),
                primitive_abi(*primitive),
                primitive_layout(*primitive),
            ),
            ResolvedType::GenericParameter(_) => (
                MirTypeKind::GenericParameter,
                MirAbiClass::OpaqueHandle,
                MirLayout::Opaque,
            ),
            ResolvedType::Nominal { .. } => (
                MirTypeKind::Nominal,
                MirAbiClass::OpaqueHandle,
                MirLayout::Opaque,
            ),
            ResolvedType::FlowStateSet { .. } => (
                MirTypeKind::FlowStateSet,
                MirAbiClass::OpaqueHandle,
                MirLayout::Handle,
            ),
            ResolvedType::Reference { mutable, .. } => (
                MirTypeKind::Reference { mutable: *mutable },
                MirAbiClass::Pointer,
                MirLayout::Pointer,
            ),
            ResolvedType::Option(inner) => (
                MirTypeKind::Option,
                MirAbiClass::Aggregate,
                MirLayout::Option {
                    inner: inner.clone(),
                    variants: option_variants(inner),
                },
            ),
            ResolvedType::Result { ok, error } => (
                MirTypeKind::Result,
                MirAbiClass::Aggregate,
                MirLayout::Result {
                    ok: ok.clone(),
                    error: error.clone(),
                    variants: result_variants(ok, error),
                },
            ),
            ResolvedType::Tuple(elements) => (
                MirTypeKind::Tuple {
                    arity: elements.len(),
                },
                MirAbiClass::Aggregate,
                MirLayout::Tuple(elements.clone()),
            ),
            ResolvedType::Function {
                abi, parameters, ..
            } => (
                MirTypeKind::Function {
                    abi: *abi,
                    arity: parameters.len(),
                },
                MirAbiClass::FunctionPointer,
                MirLayout::Handle,
            ),
            ResolvedType::CBuffer(_) => (
                MirTypeKind::CBuffer,
                MirAbiClass::Pointer,
                MirLayout::Pointer,
            ),
            ResolvedType::Capability(_) => (
                MirTypeKind::Capability,
                MirAbiClass::OpaqueHandle,
                MirLayout::Handle,
            ),
            ResolvedType::Ownership { kind, .. } => (
                MirTypeKind::Ownership(*kind),
                MirAbiClass::OpaqueHandle,
                MirLayout::Handle,
            ),
            ResolvedType::Newtype { item, inner } => (
                MirTypeKind::Newtype,
                MirAbiClass::Aggregate,
                MirLayout::Newtype {
                    nominal: item.clone(),
                    inner: inner.clone(),
                },
            ),
            ResolvedType::Array { element, length } => (
                MirTypeKind::Array { length: *length },
                MirAbiClass::Aggregate,
                MirLayout::Array {
                    element: element.clone(),
                    length: *length,
                },
            ),
            ResolvedType::Slice(_) => {
                (MirTypeKind::Slice, MirAbiClass::Pointer, MirLayout::Pointer)
            }
            ResolvedType::Trait { .. } => (
                MirTypeKind::Trait,
                MirAbiClass::OpaqueHandle,
                MirLayout::Opaque,
            ),
            ResolvedType::RawPointer { mutable, .. } => (
                MirTypeKind::RawPointer { mutable: *mutable },
                MirAbiClass::Pointer,
                MirLayout::Pointer,
            ),
            ResolvedType::DynamicAny { .. } => (
                MirTypeKind::DynamicAny,
                MirAbiClass::OpaqueHandle,
                MirLayout::Opaque,
            ),
        };
        let glue = MirGlueContract::for_type(&kind, ownership);
        Self {
            id: id.clone(),
            kind,
            layout,
            ownership,
            abi,
            needs_drop_glue: ownership.needs_drop(),
            needs_clone_glue: ownership.needs_clone(),
            glue,
            drop_plan: None,
        }
    }
}

fn option_variants(inner: &ResolvedTypeId) -> Vec<MirVariantDesc> {
    vec![
        MirVariantDesc {
            id: NodeId("builtin:variant:Option::None".into()),
            name: "None".into(),
            discriminant: 0,
            fields: Vec::new(),
        },
        MirVariantDesc {
            id: NodeId("builtin:variant:Option::Some".into()),
            name: "Some".into(),
            discriminant: 1,
            fields: vec![MirFieldDesc {
                id: NodeId("builtin:variant:Option::Some/payload:0".into()),
                name: "_0".into(),
                ty: inner.clone(),
            }],
        },
    ]
}

fn result_variants(ok: &ResolvedTypeId, error: &ResolvedTypeId) -> Vec<MirVariantDesc> {
    vec![
        MirVariantDesc {
            id: NodeId("builtin:variant:Result::Ok".into()),
            name: "Ok".into(),
            discriminant: 0,
            fields: vec![MirFieldDesc {
                id: NodeId("builtin:variant:Result::Ok/payload:0".into()),
                name: "_0".into(),
                ty: ok.clone(),
            }],
        },
        MirVariantDesc {
            id: NodeId("builtin:variant:Result::Err".into()),
            name: "Err".into(),
            discriminant: 1,
            fields: vec![MirFieldDesc {
                id: NodeId("builtin:variant:Result::Err/payload:0".into()),
                name: "_0".into(),
                ty: error.clone(),
            }],
        },
    ]
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MirTypeCatalog {
    entries: BTreeMap<ResolvedTypeId, MirTypeDesc>,
}

impl MirTypeCatalog {
    pub fn from_resolved_types(table: &ResolvedTypeTable) -> Result<Self, Vec<String>> {
        let mut errors = Vec::new();
        if let Err(type_errors) = table.validate() {
            errors.extend(type_errors.into_iter().map(|error| error.to_string()));
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        let mut entries = BTreeMap::new();
        for (id, ty) in table.iter() {
            let ownership = ownership_for(id, table, &mut Vec::new());
            let descriptor = MirTypeDesc::from_resolved(id, ty, ownership);
            entries.insert(id.clone(), descriptor);
        }
        let mut catalog = Self { entries };
        catalog.materialize_tuple_glue();
        Ok(catalog)
    }

    /// Build the catalog from the checker-owned program and attach record
    /// field contracts while the resolved declaration snapshot is still
    /// available.  A backend never needs to reopen `CheckedProgram` after
    /// this point.
    pub fn from_checked_program(program: &CheckedProgram) -> Result<Self, Vec<String>> {
        let mut catalog = Self::from_resolved_types(program.resolved_types())?;
        let mut errors = Vec::new();
        for (id, ty) in program.resolved_types().iter() {
            let ResolvedType::Nominal { item, .. } = ty else {
                continue;
            };
            let Some(type_def) = program.type_def(item.as_str()).or_else(|| {
                item.as_str()
                    .strip_prefix("type:")
                    .and_then(|name| program.type_def(name))
            }) else {
                continue;
            };
            if !matches!(type_def.kind, ResolvedTypeKind::Record) {
                continue;
            }
            let mut fields = Vec::with_capacity(type_def.fields.len());
            for (name, _) in &type_def.fields {
                let Some(field_id) = type_def.field_ids.get(name) else {
                    errors.push(format!(
                        "record '{}' field '{}' has no stable declaration identity",
                        type_def.qualified_name, name
                    ));
                    continue;
                };
                let Some(field_ty) = program.resolved_field_type(field_id) else {
                    errors.push(format!(
                        "record '{}' field '{}' has no resolved type",
                        type_def.qualified_name, name
                    ));
                    continue;
                };
                if catalog.get(field_ty).is_none() {
                    errors.push(format!(
                        "record '{}' field '{}' references a type absent from MIR catalog",
                        type_def.qualified_name, name
                    ));
                }
                fields.push(MirFieldDesc {
                    id: field_id.clone(),
                    name: name.clone(),
                    ty: field_ty.clone(),
                });
            }
            let ownership = fields.iter().fold(MirOwnership::Copy, |current, field| {
                let field_ownership = catalog
                    .get(&field.ty)
                    .map(|field| field.ownership)
                    .unwrap_or(MirOwnership::Move);
                combine_ownership(current, field_ownership)
            });
            if let Some(descriptor) = catalog.entries.get_mut(id) {
                descriptor.abi = MirAbiClass::Aggregate;
                descriptor.ownership = ownership;
                descriptor.needs_drop_glue = ownership.needs_drop();
                descriptor.needs_clone_glue = ownership.needs_clone();
                descriptor.glue = MirGlueContract::for_type(&descriptor.kind, ownership);
                descriptor.layout = MirLayout::Record {
                    nominal: item.clone(),
                    fields,
                };
            }
        }
        // Record ownership/layout facts are attached above from the checker.
        // Re-run tuple materialization now so a tuple containing a Copy record
        // sees the final child descriptor rather than the pre-layout nominal
        // placeholder produced by `from_resolved_types`.
        catalog.materialize_tuple_glue();
        if errors.is_empty() {
            Ok(catalog)
        } else {
            Err(errors)
        }
    }

    pub fn get(&self, id: &ResolvedTypeId) -> Option<&MirTypeDesc> {
        self.entries.get(id)
    }

    /// Validate a value boundary against the canonical glue contract.  The
    /// result/source type equality is checked by the MIR instruction validator;
    /// this method only answers whether the operation has a materialized
    /// implementation for the descriptor.
    pub fn validate_glue(
        &self,
        ty: &ResolvedTypeId,
        operation: MirGlueOperation,
    ) -> Result<(), String> {
        let descriptor = self
            .get(ty)
            .ok_or_else(|| format!("type '{}' is absent from MIR type catalog", ty.as_str()))?;
        let supported = match operation {
            MirGlueOperation::MoveOut => descriptor.glue.supports_move_out(),
            MirGlueOperation::Clone => descriptor.glue.supports_clone(),
            MirGlueOperation::Drop => descriptor.glue.supports_drop(),
        };
        if !supported {
            return Err(format!(
                "type '{}' ownership {:?} has no canonical {:?} glue",
                ty.as_str(),
                descriptor.ownership,
                operation
            ));
        }
        let operation_glue = match operation {
            MirGlueOperation::MoveOut => descriptor.glue.move_out,
            MirGlueOperation::Clone => descriptor.glue.clone,
            MirGlueOperation::Drop => descriptor.glue.drop,
        };
        if operation_glue == MirGlueKind::Aggregate {
            self.validate_aggregate_glue(ty, operation)?;
        }
        Ok(())
    }

    /// Validate the recursive product glue graph for one operation.  The
    /// caller still owns the choice of operation; this method only follows
    /// canonical child descriptors and never consults a backend ABI.
    pub fn validate_aggregate_glue(
        &self,
        ty: &ResolvedTypeId,
        operation: MirGlueOperation,
    ) -> Result<(), String> {
        let descriptor = self
            .get(ty)
            .ok_or_else(|| format!("type '{}' is absent from MIR type catalog", ty.as_str()))?;
        let MirLayout::Tuple(elements) = &descriptor.layout else {
            return Err(format!(
                "aggregate glue type '{}' has no canonical tuple layout",
                ty.as_str()
            ));
        };
        let expected_contract = MirGlueContract {
            move_out: MirGlueKind::Aggregate,
            clone: MirGlueKind::Aggregate,
            drop: MirGlueKind::Aggregate,
        };
        if descriptor.glue != expected_contract {
            return Err(format!(
                "type '{}' aggregate glue contract is not fully materialized",
                ty.as_str()
            ));
        }
        if matches!(operation, MirGlueOperation::Drop) {
            let Some(plan) = &descriptor.drop_plan else {
                return Err(format!(
                    "type '{}' aggregate drop glue has no drop plan",
                    ty.as_str()
                ));
            };
            if plan.fields.len() != elements.len() {
                return Err(format!(
                    "type '{}' drop plan has {} fields but tuple has {}",
                    ty.as_str(),
                    plan.fields.len(),
                    elements.len()
                ));
            }
            for (expected_index, field) in (0..elements.len()).rev().zip(&plan.fields) {
                if field.index != expected_index {
                    return Err(format!(
                        "type '{}' drop plan is not in reverse declaration order",
                        ty.as_str()
                    ));
                }
                if field.ty != elements[field.index] {
                    return Err(format!(
                        "type '{}' drop plan field {} type disagrees with tuple layout",
                        ty.as_str(),
                        field.index
                    ));
                }
                let child = self.get(&field.ty).ok_or_else(|| {
                    format!(
                        "type '{}' drop plan child type '{}' is absent",
                        ty.as_str(),
                        field.ty.as_str()
                    )
                })?;
                if child.glue.drop != field.glue {
                    return Err(format!(
                        "type '{}' drop plan field {} glue disagrees with child TypeDesc",
                        ty.as_str(),
                        field.index
                    ));
                }
                self.validate_glue(&field.ty, MirGlueOperation::Drop)?;
            }
        } else {
            for element in elements {
                self.validate_glue(element, operation)?;
            }
        }
        Ok(())
    }

    fn materialize_tuple_glue(&mut self) {
        let ids = self.entries.keys().cloned().collect::<Vec<_>>();
        for id in ids {
            let mut visiting = BTreeSet::new();
            let _ = self.materialize_tuple_glue_for(&id, &mut visiting);
        }
    }

    fn materialize_tuple_glue_for(
        &mut self,
        id: &ResolvedTypeId,
        visiting: &mut BTreeSet<ResolvedTypeId>,
    ) -> bool {
        if !visiting.insert(id.clone()) {
            return false;
        }
        let layout = self.get(id).map(|descriptor| descriptor.layout.clone());
        let Some(MirLayout::Tuple(elements)) = layout else {
            visiting.remove(id);
            return false;
        };
        let mut children = Vec::with_capacity(elements.len());
        for (index, child_id) in elements.iter().enumerate() {
            let child_is_tuple = self
                .get(child_id)
                .is_some_and(|child| matches!(child.layout, MirLayout::Tuple(_)));
            let child_is_copy = self
                .get(child_id)
                .is_some_and(|child| child.ownership == MirOwnership::Copy);
            if child_is_tuple
                && !child_is_copy
                && !self.materialize_tuple_glue_for(child_id, visiting)
            {
                visiting.remove(id);
                return false;
            }
            let Some(child) = self.get(child_id) else {
                visiting.remove(id);
                return false;
            };
            if !child.glue.supports_move_out()
                || !child.glue.supports_clone()
                || !child.glue.supports_drop()
            {
                visiting.remove(id);
                return false;
            }
            children.push(MirDropGlueField {
                index,
                ty: child_id.clone(),
                glue: child.glue.drop,
            });
        }
        let Some(descriptor) = self.entries.get_mut(id) else {
            visiting.remove(id);
            return false;
        };
        if descriptor.ownership != MirOwnership::Copy && !children.is_empty() {
            descriptor.glue = MirGlueContract {
                move_out: MirGlueKind::Aggregate,
                clone: MirGlueKind::Aggregate,
                drop: MirGlueKind::Aggregate,
            };
            children.reverse();
            descriptor.drop_plan = Some(MirDropGluePlan { fields: children });
        }
        visiting.remove(id);
        descriptor.glue.move_out == MirGlueKind::Aggregate
    }

    pub fn validate_copy(&self, ty: &ResolvedTypeId) -> Result<(), String> {
        let descriptor = self
            .get(ty)
            .ok_or_else(|| format!("type '{}' is absent from MIR type catalog", ty.as_str()))?;
        if descriptor.ownership == MirOwnership::Copy {
            Ok(())
        } else {
            Err(format!(
                "copy instruction is invalid for ownership {:?} type '{}'",
                descriptor.ownership,
                ty.as_str()
            ))
        }
    }

    pub fn validate_value_operation(
        &self,
        result_ty: &ResolvedTypeId,
        source_ty: &ResolvedTypeId,
        operation: MirGlueOperation,
    ) -> Result<(), String> {
        if result_ty != source_ty {
            return Err(format!(
                "{:?} result type '{}' disagrees with source type '{}'",
                operation,
                result_ty.as_str(),
                source_ty.as_str()
            ));
        }
        self.validate_glue(source_ty, operation)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ResolvedTypeId, &MirTypeDesc)> {
        self.entries.iter()
    }

    #[cfg(test)]
    pub(crate) fn replace_for_test_only(&mut self, id: ResolvedTypeId, descriptor: MirTypeDesc) {
        self.entries.insert(id, descriptor);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Validate that an aggregate construction agrees with the checker-owned
    /// layout contract.  This is intentionally structural and backend-free:
    /// native offsets, bytecode registers, and drop glue are downstream
    /// concerns.  Unknown layouts fail closed instead of being treated as an
    /// untyped product.
    pub fn validate_aggregate(
        &self,
        result_ty: &ResolvedTypeId,
        kind: &crate::core::mir::MirAggregateKind,
        field_types: &[ResolvedTypeId],
    ) -> Result<(), String> {
        let descriptor = self
            .get(result_ty)
            .ok_or_else(|| format!("aggregate result type '{}' is absent", result_ty.as_str()))?;
        match (kind, &descriptor.layout) {
            (crate::core::mir::MirAggregateKind::Tuple, MirLayout::Tuple(elements)) => {
                if elements.len() != field_types.len() {
                    return Err(format!(
                        "tuple construction has {} fields but layout expects {}",
                        field_types.len(),
                        elements.len()
                    ));
                }
                for (index, (actual, expected)) in field_types.iter().zip(elements).enumerate() {
                    if actual != expected {
                        return Err(format!(
                            "tuple field {} type '{}' disagrees with layout type '{}'",
                            index,
                            actual.as_str(),
                            expected.as_str()
                        ));
                    }
                }
                Ok(())
            }
            (
                crate::core::mir::MirAggregateKind::Record { nominal, fields },
                MirLayout::Record {
                    nominal: expected_nominal,
                    fields: expected_fields,
                },
            ) => {
                if nominal != expected_nominal {
                    return Err(format!(
                        "record nominal '{}' disagrees with layout nominal '{}'",
                        nominal.as_str(),
                        expected_nominal.as_str()
                    ));
                }
                if fields.len() != field_types.len() {
                    return Err(format!(
                        "record construction has {} fields but layout expects {}",
                        field_types.len(),
                        fields.len()
                    ));
                }
                if fields.len() != expected_fields.len() {
                    return Err(format!(
                        "record construction names {} fields but declaration has {}",
                        fields.len(),
                        expected_fields.len()
                    ));
                }
                let mut seen = std::collections::BTreeSet::new();
                for (index, (field, actual)) in fields.iter().zip(field_types).enumerate() {
                    if !seen.insert(field) {
                        return Err(format!("record field {} is repeated", field.0));
                    }
                    let Some(expected) = expected_fields
                        .iter()
                        .find(|candidate| candidate.id == *field)
                    else {
                        return Err(format!(
                            "record field '{}' is absent from declaration",
                            field.0
                        ));
                    };
                    if actual != &expected.ty {
                        return Err(format!(
                            "record field {} type '{}' disagrees with layout type '{}'",
                            index,
                            actual.as_str(),
                            expected.ty.as_str()
                        ));
                    }
                }
                Ok(())
            }
            (kind, layout) => Err(format!(
                "aggregate kind {:?} does not match result layout {:?}",
                kind, layout
            )),
        }
    }

    /// Validate one value projection against the canonical semantic layout.
    /// Field names are intentionally unavailable here: the stable field ID is
    /// the only identity that crosses the MIR/backend boundary.
    pub fn validate_projection(
        &self,
        base_ty: &ResolvedTypeId,
        result_ty: &ResolvedTypeId,
        projection: &crate::core::mir::MirProjection,
    ) -> Result<(), String> {
        let base = self
            .get(base_ty)
            .ok_or_else(|| format!("projection base type '{}' is absent", base_ty.as_str()))?;
        let result = self
            .get(result_ty)
            .ok_or_else(|| format!("projection result type '{}' is absent", result_ty.as_str()))?;
        match (&base.layout, projection) {
            (MirLayout::Tuple(elements), crate::core::mir::MirProjection::Tuple(index)) => {
                let expected = elements
                    .get(*index)
                    .ok_or_else(|| format!("tuple projection index {} is out of bounds", index))?;
                if expected != result_ty {
                    return Err(format!(
                        "tuple projection result type '{}' disagrees with layout type '{}'",
                        result_ty.as_str(),
                        expected.as_str()
                    ));
                }
                if result.ownership != MirOwnership::Copy {
                    return Err(format!(
                        "tuple projection result type '{}' is non-Copy and has no explicit move projection contract",
                        result_ty.as_str()
                    ));
                }
                Ok(())
            }
            (MirLayout::Record { fields, .. }, crate::core::mir::MirProjection::Field(field)) => {
                let expected = fields
                    .iter()
                    .find(|candidate| candidate.id == *field)
                    .ok_or_else(|| format!("record projection field '{}' is absent", field.0))?;
                if expected.ty != *result_ty {
                    return Err(format!(
                        "record projection field '{}' type '{}' disagrees with result type '{}'",
                        field.0,
                        expected.ty.as_str(),
                        result_ty.as_str()
                    ));
                }
                if result.ownership != MirOwnership::Copy {
                    return Err(format!(
                        "record projection result type '{}' is non-Copy and has no explicit move projection contract",
                        result_ty.as_str()
                    ));
                }
                Ok(())
            }
            (_, crate::core::mir::MirProjection::Index(_)) => {
                Err("indexed projection has no canonical MIR layout contract".into())
            }
            (_, crate::core::mir::MirProjection::Dereference) => {
                Err("dereference projection has no canonical MIR layout contract".into())
            }
            (layout, projection) => Err(format!(
                "projection {:?} does not match base layout {:?}",
                projection, layout
            )),
        }
    }

    /// Validate a place load one projection at a time.  This keeps lvalue
    /// projection type facts in MIR's TypeDesc contract instead of asking a
    /// backend to rediscover them from `ResolvedPlace` names.
    pub fn validate_place(
        &self,
        base_ty: &ResolvedTypeId,
        result_ty: &ResolvedTypeId,
        projections: &[ResolvedProjection],
    ) -> Result<(), String> {
        let mut current_ty = base_ty.clone();
        for projection in projections {
            let mir_projection = match projection {
                ResolvedProjection::Field { field, .. } => {
                    crate::core::mir::MirProjection::Field(field.clone())
                }
                ResolvedProjection::Tuple { index, .. } => {
                    crate::core::mir::MirProjection::Tuple(*index)
                }
                ResolvedProjection::Index { .. } => {
                    return Err(
                        "indexed place projection has no canonical MIR layout contract".into(),
                    )
                }
                ResolvedProjection::Deref { .. } => {
                    return Err(
                        "dereference place projection has no canonical MIR layout contract".into(),
                    )
                }
            };
            self.validate_projection(&current_ty, projection.ty(), &mir_projection)?;
            current_ty = projection.ty().clone();
        }
        if &current_ty != result_ty {
            return Err(format!(
                "place load result type '{}' disagrees with projected type '{}'",
                result_ty.as_str(),
                current_ty.as_str()
            ));
        }
        Ok(())
    }

    /// Validate a record update.  The base and result are the same nominal
    /// record, while the explicit field set may be a declaration-order
    /// independent subset.  The base is still an explicit MIR operand so a
    /// future ownership pass can prove its consume/clone behavior.
    pub fn validate_record_update(
        &self,
        result_ty: &ResolvedTypeId,
        base_ty: &ResolvedTypeId,
        kind: &crate::core::mir::MirAggregateKind,
        field_types: &[ResolvedTypeId],
    ) -> Result<(), String> {
        let (result_nominal, result_fields) = match self
            .get(result_ty)
            .ok_or_else(|| {
                format!(
                    "record update result type '{}' is absent",
                    result_ty.as_str()
                )
            })?
            .layout
            .clone()
        {
            MirLayout::Record { nominal, fields } => (nominal, fields),
            layout => {
                return Err(format!(
                    "record update result layout {:?} is not a record",
                    layout
                ))
            }
        };
        let (base_nominal, base_fields) = match self
            .get(base_ty)
            .ok_or_else(|| format!("record update base type '{}' is absent", base_ty.as_str()))?
            .layout
            .clone()
        {
            MirLayout::Record { nominal, fields } => (nominal, fields),
            layout => {
                return Err(format!(
                    "record update base layout {:?} is not a record",
                    layout
                ))
            }
        };
        if result_nominal != base_nominal {
            return Err(format!(
                "record update base nominal '{}' disagrees with result nominal '{}'",
                base_nominal.as_str(),
                result_nominal.as_str()
            ));
        }
        let crate::core::mir::MirAggregateKind::Record { nominal, fields } = kind else {
            return Err("record update requires a record aggregate kind".into());
        };
        if nominal != &result_nominal {
            return Err(format!(
                "record update nominal '{}' disagrees with layout nominal '{}'",
                nominal.as_str(),
                result_nominal.as_str()
            ));
        }
        if fields.len() != field_types.len() {
            return Err(format!(
                "record update names {} fields but carries {} values",
                fields.len(),
                field_types.len()
            ));
        }
        if result_fields.len() != base_fields.len()
            || result_fields
                .iter()
                .zip(&base_fields)
                .any(|(left, right)| left != right)
        {
            return Err("record update base and result layouts disagree".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for (field, actual) in fields.iter().zip(field_types) {
            if !seen.insert(field) {
                return Err(format!("record update field '{}' is repeated", field.0));
            }
            let Some(expected) = result_fields
                .iter()
                .find(|candidate| candidate.id == *field)
            else {
                return Err(format!(
                    "record update field '{}' is absent from declaration",
                    field.0
                ));
            };
            if actual != &expected.ty {
                return Err(format!(
                    "record update field '{}' type '{}' disagrees with layout type '{}'",
                    field.0,
                    actual.as_str(),
                    expected.ty.as_str()
                ));
            }
        }
        Ok(())
    }

    /// Validate one canonical variant construction.  The instruction carries
    /// stable variant/member identities; this method supplies the semantic
    /// discriminant and payload ABI from TypeDesc.
    pub fn validate_variant_construct(
        &self,
        result_ty: &ResolvedTypeId,
        nominal: &crate::core::NominalTypeId,
        variant: &NodeId,
        field_ids: &[NodeId],
        field_types: &[ResolvedTypeId],
    ) -> Result<(), String> {
        if field_ids.len() != field_types.len() {
            return Err(format!(
                "variant '{}' names {} fields but carries {} values",
                variant.0,
                field_ids.len(),
                field_types.len()
            ));
        }
        let (expected_nominal, variants) = self.variant_layout(result_ty).ok_or_else(|| {
            format!(
                "type '{}' has no canonical variant layout",
                result_ty.as_str()
            )
        })?;
        if nominal.as_str() != expected_nominal {
            return Err(format!(
                "variant nominal '{}' disagrees with canonical nominal '{}'",
                nominal.as_str(),
                expected_nominal
            ));
        }
        let expected = variants
            .iter()
            .find(|candidate| candidate.id == *variant)
            .ok_or_else(|| format!("variant '{}' is absent from TypeDesc", variant.0))?;
        validate_variant_fields(expected, field_ids, field_types)
    }

    /// Return the canonical nominal label and discriminant/payload table for
    /// the built-in Option/Result families.  User enum layouts remain
    /// fail-closed until their schema is promoted into this catalog.
    pub fn variant_layout(&self, ty: &ResolvedTypeId) -> Option<(&str, &[MirVariantDesc])> {
        let descriptor = self.get(ty)?;
        match &descriptor.layout {
            MirLayout::Option { variants, .. } => {
                Some(("builtin:type:Option", variants.as_slice()))
            }
            MirLayout::Result { variants, .. } => {
                Some(("builtin:type:Result", variants.as_slice()))
            }
            _ => None,
        }
    }

    pub fn variant(&self, ty: &ResolvedTypeId, variant: &NodeId) -> Option<&MirVariantDesc> {
        self.variant_layout(ty)?
            .1
            .iter()
            .find(|candidate| candidate.id == *variant)
    }

    /// Validate a switch over a canonical variant family.  Exhaustiveness is
    /// part of the MIR contract: either every discriminant is listed exactly
    /// once or the final arm is an explicit default.
    pub fn validate_switch(
        &self,
        scrutinee_ty: &ResolvedTypeId,
        arms: &[crate::core::mir::MirSwitchArm],
    ) -> Result<(), String> {
        let Some((_, variants)) = self.variant_layout(scrutinee_ty) else {
            if arms
                .iter()
                .any(|arm| matches!(arm.case, crate::core::mir::MirSwitchCase::Variant(_)))
            {
                return Err(format!(
                    "switch scrutinee type '{}' has no canonical variant layout",
                    scrutinee_ty.as_str()
                ));
            }
            return Ok(());
        };
        if arms.is_empty() {
            return Err("variant switch has no arms".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut has_default = false;
        for (index, arm) in arms.iter().enumerate() {
            match &arm.case {
                crate::core::mir::MirSwitchCase::Variant(variant) => {
                    if has_default {
                        return Err("variant switch has an arm after its default".into());
                    }
                    if !variants.iter().any(|candidate| candidate.id == *variant) {
                        return Err(format!(
                            "variant switch case '{}' is absent from TypeDesc",
                            variant.0
                        ));
                    }
                    if !seen.insert(variant) {
                        return Err(format!("variant switch case '{}' is repeated", variant.0));
                    }
                }
                crate::core::mir::MirSwitchCase::Default => {
                    if has_default {
                        return Err("variant switch has more than one default arm".into());
                    }
                    if index + 1 != arms.len() {
                        return Err("variant switch default arm must be last".into());
                    }
                    if !arm.bindings.is_empty() {
                        return Err("variant switch default arm cannot bind a payload".into());
                    }
                    has_default = true;
                }
                crate::core::mir::MirSwitchCase::Literal(_) => {
                    return Err("variant switch cannot use a literal case".into());
                }
            }
        }
        if !has_default && seen.len() != variants.len() {
            let missing = variants
                .iter()
                .filter(|candidate| !seen.contains(&candidate.id))
                .map(|candidate| candidate.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "variant switch is not exhaustive; missing: {missing}"
            ));
        }
        Ok(())
    }

    pub fn canonical_text(&self) -> String {
        let mut output = format!("mir.type-catalog {MIR_TYPE_DESC_SCHEMA_VERSION}\n");
        for (id, descriptor) in &self.entries {
            output.push_str(&format!(
                "{} kind={:?} layout={:?} ownership={:?} abi={:?} glue={:?} drop_plan={:?} drop={} clone={}\n",
                id.as_str(),
                descriptor.kind,
                descriptor.layout,
                descriptor.ownership,
                descriptor.abi,
                descriptor.glue,
                descriptor.drop_plan,
                descriptor.needs_drop_glue,
                descriptor.needs_clone_glue,
            ));
        }
        output
    }
}

fn validate_variant_fields(
    variant: &MirVariantDesc,
    field_ids: &[NodeId],
    field_types: &[ResolvedTypeId],
) -> Result<(), String> {
    if variant.fields.len() != field_ids.len() {
        return Err(format!(
            "variant '{}' expects {} payload fields but carries {}",
            variant.name,
            variant.fields.len(),
            field_ids.len()
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for (field_id, actual) in field_ids.iter().zip(field_types) {
        if !seen.insert(field_id) {
            return Err(format!(
                "variant payload field '{}' is repeated",
                field_id.0
            ));
        }
        let expected = variant
            .fields
            .iter()
            .find(|field| field.id == *field_id)
            .ok_or_else(|| format!("variant payload field '{}' is absent", field_id.0))?;
        if actual != &expected.ty {
            return Err(format!(
                "variant payload field '{}' type '{}' disagrees with layout type '{}'",
                field_id.0,
                actual.as_str(),
                expected.ty.as_str()
            ));
        }
    }
    if variant.fields.iter().any(|field| !seen.contains(&field.id)) {
        return Err(format!("variant '{}' payload is incomplete", variant.name));
    }
    Ok(())
}

fn primitive_layout(primitive: PrimitiveType) -> MirLayout {
    match primitive {
        PrimitiveType::Unit => MirLayout::Unit,
        PrimitiveType::String => MirLayout::Handle,
        PrimitiveType::I8
        | PrimitiveType::I16
        | PrimitiveType::I32
        | PrimitiveType::I64
        | PrimitiveType::I128
        | PrimitiveType::U8
        | PrimitiveType::U16
        | PrimitiveType::U32
        | PrimitiveType::U64
        | PrimitiveType::U128
        | PrimitiveType::Isize
        | PrimitiveType::Usize
        | PrimitiveType::F32
        | PrimitiveType::F64
        | PrimitiveType::Bool
        | PrimitiveType::Char => MirLayout::Scalar,
    }
}

fn primitive_abi(primitive: PrimitiveType) -> MirAbiClass {
    match primitive {
        PrimitiveType::I8 => MirAbiClass::Integer {
            bits: 8,
            signed: true,
        },
        PrimitiveType::I16 => MirAbiClass::Integer {
            bits: 16,
            signed: true,
        },
        PrimitiveType::I32 => MirAbiClass::Integer {
            bits: 32,
            signed: true,
        },
        PrimitiveType::I64 | PrimitiveType::Isize => MirAbiClass::Integer {
            bits: 64,
            signed: true,
        },
        PrimitiveType::I128 => MirAbiClass::Integer {
            bits: 128,
            signed: true,
        },
        PrimitiveType::U8 => MirAbiClass::Integer {
            bits: 8,
            signed: false,
        },
        PrimitiveType::U16 => MirAbiClass::Integer {
            bits: 16,
            signed: false,
        },
        PrimitiveType::U32 => MirAbiClass::Integer {
            bits: 32,
            signed: false,
        },
        PrimitiveType::U64 | PrimitiveType::Usize => MirAbiClass::Integer {
            bits: 64,
            signed: false,
        },
        PrimitiveType::U128 => MirAbiClass::Integer {
            bits: 128,
            signed: false,
        },
        PrimitiveType::F32 => MirAbiClass::Float { bits: 32 },
        PrimitiveType::F64 => MirAbiClass::Float { bits: 64 },
        PrimitiveType::Bool => MirAbiClass::Bool,
        PrimitiveType::Char => MirAbiClass::Char,
        PrimitiveType::String => MirAbiClass::StringHandle,
        PrimitiveType::Unit => MirAbiClass::Unit,
    }
}

fn ownership_for(
    id: &ResolvedTypeId,
    table: &ResolvedTypeTable,
    visiting: &mut Vec<ResolvedTypeId>,
) -> MirOwnership {
    if visiting.iter().any(|seen| seen == id) {
        return MirOwnership::Move;
    }
    let Some(ty) = table.get(id) else {
        return MirOwnership::Move;
    };
    visiting.push(id.clone());
    let ownership = match ty {
        ResolvedType::Primitive(PrimitiveType::String) => MirOwnership::Move,
        ResolvedType::Primitive(_) | ResolvedType::GenericParameter(_) => MirOwnership::Copy,
        ResolvedType::Nominal { is_linear, .. } => {
            if *is_linear {
                MirOwnership::Linear
            } else {
                MirOwnership::Move
            }
        }
        ResolvedType::Capability(_) => MirOwnership::Linear,
        ResolvedType::Reference { mutable, .. } => {
            if *mutable {
                MirOwnership::Move
            } else {
                MirOwnership::SharedBorrow
            }
        }
        ResolvedType::RawPointer { .. } => MirOwnership::Copy,
        ResolvedType::Ownership { kind, .. } => match kind {
            OwnershipTypeKind::Shared => MirOwnership::SharedBorrow,
            OwnershipTypeKind::Weak => MirOwnership::WeakBorrow,
        },
        ResolvedType::Tuple(elements) => aggregate_ownership(elements, table, visiting),
        ResolvedType::Option(inner)
        | ResolvedType::CBuffer(inner)
        | ResolvedType::Slice(inner)
        | ResolvedType::Array { element: inner, .. }
        | ResolvedType::Newtype { inner, .. } => ownership_for(inner, table, visiting),
        ResolvedType::Result { ok, error } => combine_ownership(
            ownership_for(ok, table, visiting),
            ownership_for(error, table, visiting),
        ),
        ResolvedType::Function { .. } => MirOwnership::Copy,
        ResolvedType::FlowStateSet { .. } => MirOwnership::Linear,
        ResolvedType::Trait { .. } | ResolvedType::DynamicAny { .. } => MirOwnership::Move,
    };
    visiting.pop();
    ownership
}

fn aggregate_ownership(
    elements: &[ResolvedTypeId],
    table: &ResolvedTypeTable,
    visiting: &mut Vec<ResolvedTypeId>,
) -> MirOwnership {
    elements
        .iter()
        .fold(MirOwnership::Copy, |current, element| {
            combine_ownership(current, ownership_for(element, table, visiting))
        })
}

fn combine_ownership(left: MirOwnership, right: MirOwnership) -> MirOwnership {
    use MirOwnership::{Copy, Linear, Move, SharedBorrow, WeakBorrow};
    match (left, right) {
        (Linear, _) | (_, Linear) => Linear,
        (Move, _) | (_, Move) => Move,
        (SharedBorrow, _) | (_, SharedBorrow) => SharedBorrow,
        (WeakBorrow, _) | (_, WeakBorrow) => WeakBorrow,
        (Copy, Copy) => Copy,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MirAbiClass, MirGlueKind, MirGlueOperation, MirLayout, MirOwnership, MirTypeCatalog,
    };
    use crate::core::ir::{PrimitiveType, ResolvedType, ResolvedTypeTable};
    use crate::core::mir::{MirAggregateKind, MirProjection};

    #[test]
    fn materializes_scalar_abi_and_copy_ownership() {
        let mut table = ResolvedTypeTable::new();
        let id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::I32))
            .expect("type");
        let catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");
        let descriptor = catalog.get(&id).expect("descriptor");
        assert_eq!(descriptor.ownership, MirOwnership::Copy);
        assert_eq!(
            descriptor.abi,
            MirAbiClass::Integer {
                bits: 32,
                signed: true
            }
        );
        assert!(!descriptor.needs_drop_glue);
    }

    #[test]
    fn materializes_string_drop_and_clone_contract() {
        let mut table = ResolvedTypeTable::new();
        let id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::String))
            .expect("type");
        let catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");
        let descriptor = catalog.get(&id).expect("descriptor");
        assert_eq!(descriptor.ownership, MirOwnership::Move);
        assert!(descriptor.needs_drop_glue);
        assert!(descriptor.needs_clone_glue);
        assert_eq!(descriptor.glue.move_out, MirGlueKind::OwnedString);
        assert_eq!(descriptor.glue.clone, MirGlueKind::OwnedString);
        assert_eq!(descriptor.glue.drop, MirGlueKind::OwnedString);
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            assert!(catalog.validate_glue(&id, operation).is_ok());
        }
    }

    #[test]
    fn unsupported_move_aggregate_remains_fail_closed_in_glue_contract() {
        let mut table = ResolvedTypeTable::new();
        let string_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::String))
            .expect("string");
        let option_id = table
            .intern_resolved(ResolvedType::Option(string_id))
            .expect("option");
        let catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");
        let descriptor = catalog.get(&option_id).expect("descriptor");
        assert_eq!(descriptor.ownership, MirOwnership::Move);
        assert_eq!(descriptor.glue.move_out, MirGlueKind::Unsupported);
        assert!(catalog
            .validate_glue(&option_id, MirGlueOperation::MoveOut)
            .is_err());
    }

    #[test]
    fn materializes_recursive_tuple_drop_schedule() {
        let mut table = ResolvedTypeTable::new();
        let string_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::String))
            .expect("string type");
        let i32_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::I32))
            .expect("i32 type");
        let pair_id = table
            .intern_resolved(ResolvedType::Tuple(vec![string_id.clone(), i32_id.clone()]))
            .expect("pair type");
        let nested_id = table
            .intern_resolved(ResolvedType::Tuple(vec![pair_id.clone(), i32_id.clone()]))
            .expect("nested tuple type");
        let catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");

        let pair = catalog.get(&pair_id).expect("pair descriptor");
        assert_eq!(pair.glue.move_out, MirGlueKind::Aggregate);
        assert_eq!(pair.glue.clone, MirGlueKind::Aggregate);
        assert_eq!(pair.glue.drop, MirGlueKind::Aggregate);
        let plan = pair.drop_plan.as_ref().expect("pair drop plan");
        assert_eq!(
            plan.fields
                .iter()
                .map(|field| field.index)
                .collect::<Vec<_>>(),
            vec![1, 0]
        );
        assert_eq!(plan.fields[0].ty, i32_id);
        assert_eq!(plan.fields[0].glue, MirGlueKind::Noop);
        assert_eq!(plan.fields[1].ty, string_id);
        assert_eq!(plan.fields[1].glue, MirGlueKind::OwnedString);
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            assert!(catalog.validate_glue(&pair_id, operation).is_ok());
        }
        let projection_error = catalog
            .validate_projection(&pair_id, &string_id, &MirProjection::Tuple(0))
            .expect_err("non-Copy projection needs an explicit move contract");
        assert!(projection_error.contains("explicit move projection contract"));

        let nested = catalog.get(&nested_id).expect("nested descriptor");
        assert_eq!(nested.glue.drop, MirGlueKind::Aggregate);
        assert!(catalog
            .validate_aggregate_glue(&nested_id, MirGlueOperation::Drop)
            .is_ok());
    }

    #[test]
    fn tuple_with_unmaterialized_child_glue_stays_fail_closed() {
        let mut table = ResolvedTypeTable::new();
        let string_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::String))
            .expect("string type");
        let option_id = table
            .intern_resolved(ResolvedType::Option(string_id))
            .expect("option type");
        let tuple_id = table
            .intern_resolved(ResolvedType::Tuple(vec![option_id]))
            .expect("tuple type");
        let catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");
        let tuple = catalog.get(&tuple_id).expect("tuple descriptor");
        assert_eq!(tuple.glue.move_out, MirGlueKind::Unsupported);
        assert!(tuple.drop_plan.is_none());
        assert!(catalog
            .validate_glue(&tuple_id, MirGlueOperation::Drop)
            .is_err());
    }

    #[test]
    fn materializes_product_layout_from_canonical_type_shape() {
        let mut table = ResolvedTypeTable::new();
        let i32_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::I32))
            .expect("i32");
        let bool_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::Bool))
            .expect("bool");
        let tuple_id = table
            .intern_resolved(ResolvedType::Tuple(vec![i32_id.clone(), bool_id.clone()]))
            .expect("tuple");
        let option_id = table
            .intern_resolved(ResolvedType::Option(i32_id.clone()))
            .expect("option");
        let result_id = table
            .intern_resolved(ResolvedType::Result {
                ok: i32_id.clone(),
                error: bool_id.clone(),
            })
            .expect("result");
        let catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");
        assert_eq!(
            catalog.get(&tuple_id).expect("tuple descriptor").layout,
            MirLayout::Tuple(vec![i32_id.clone(), bool_id.clone()])
        );
        assert!(matches!(
            &catalog.get(&option_id).expect("option descriptor").layout,
            MirLayout::Option { inner, variants }
                if inner == &i32_id
                    && variants.iter().map(|variant| variant.name.as_str()).collect::<Vec<_>>()
                        == ["None", "Some"]
                    && variants[0].discriminant == 0
                    && variants[1].discriminant == 1
                    && variants[1].fields[0].id.0
                        == "builtin:variant:Option::Some/payload:0"
                    && variants[1].fields[0].ty == i32_id
        ));
        assert!(matches!(
            &catalog.get(&result_id).expect("result descriptor").layout,
            MirLayout::Result { ok, error, variants }
                if ok == &i32_id
                    && error == &bool_id
                    && variants.iter().map(|variant| variant.name.as_str()).collect::<Vec<_>>()
                        == ["Ok", "Err"]
                    && variants[0].discriminant == 0
                    && variants[1].discriminant == 1
                    && variants[0].fields[0].ty == i32_id
                    && variants[1].fields[0].ty == bool_id
        ));
        assert!(catalog
            .validate_aggregate(
                &tuple_id,
                &crate::core::mir::MirAggregateKind::Tuple,
                &[i32_id.clone(), bool_id.clone()]
            )
            .is_ok());
        assert!(catalog
            .validate_aggregate(
                &tuple_id,
                &crate::core::mir::MirAggregateKind::Tuple,
                &[bool_id, i32_id]
            )
            .is_err());
    }

    #[test]
    fn materializes_checker_record_field_contract() {
        let source =
            "type Point { x: i32, y: bool }\nfunc main() -> i32 { let p = Point { x: 1, y: true }; if p.y { 0 } else { 1 } }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let catalog = MirTypeCatalog::from_checked_program(&program).expect("catalog");
        let point = catalog
            .iter()
            .find_map(|(_, descriptor)| match &descriptor.layout {
                MirLayout::Record { nominal, fields } if nominal.as_str().ends_with("Point") => {
                    Some((descriptor, fields))
                }
                _ => None,
            })
            .expect("Point record contract");
        assert_eq!(point.0.abi, MirAbiClass::Aggregate);
        assert_eq!(point.0.ownership, MirOwnership::Copy);
        assert_eq!(
            point
                .1
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            ["x", "y"]
        );
        assert!(point.1.iter().all(|field| catalog.get(&field.ty).is_some()));
    }

    #[test]
    fn record_projection_and_update_contracts_are_field_id_based() {
        let source =
            "type Point { x: i32, y: bool }\nfunc main() -> i32 { Point { x: 1, y: true }.x }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let catalog = MirTypeCatalog::from_checked_program(&program).expect("catalog");
        let (point_ty, fields) = catalog
            .iter()
            .find_map(|(id, descriptor)| match &descriptor.layout {
                MirLayout::Record { nominal, fields } if nominal.as_str().ends_with("Point") => {
                    Some((id.clone(), fields.clone()))
                }
                _ => None,
            })
            .expect("Point layout");
        let x = fields.iter().find(|field| field.name == "x").expect("x");
        let y = fields.iter().find(|field| field.name == "y").expect("y");
        assert!(catalog
            .validate_projection(&point_ty, &x.ty, &MirProjection::Field(x.id.clone()),)
            .is_ok());
        let unknown = catalog
            .validate_projection(
                &point_ty,
                &x.ty,
                &MirProjection::Field(crate::core::NodeId("field:missing".into())),
            )
            .expect_err("unknown field must fail closed");
        assert!(unknown.contains("absent"));
        let wrong_type = catalog
            .validate_projection(&point_ty, &y.ty, &MirProjection::Field(x.id.clone()))
            .expect_err("wrong projection result type must fail closed");
        assert!(wrong_type.contains("disagrees"));
        assert!(catalog
            .validate_record_update(
                &point_ty,
                &point_ty,
                &MirAggregateKind::Record {
                    nominal: match &catalog.get(&point_ty).expect("point").layout {
                        MirLayout::Record { nominal, .. } => nominal.clone(),
                        _ => unreachable!(),
                    },
                    fields: vec![y.id.clone()],
                },
                std::slice::from_ref(&y.ty),
            )
            .is_ok());
    }

    #[test]
    fn variant_layout_rejects_bad_payload_and_non_exhaustive_switches() {
        let mut table = ResolvedTypeTable::new();
        let i32_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::I32))
            .expect("i32");
        let bool_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::Bool))
            .expect("bool");
        let option_id = table
            .intern_resolved(ResolvedType::Option(i32_id.clone()))
            .expect("option");
        let catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");
        let option_nominal =
            crate::core::ir::NominalTypeId::new("builtin:type:Option").expect("Option nominal");
        let some = crate::core::NodeId("builtin:variant:Option::Some".into());
        let some_field = crate::core::NodeId("builtin:variant:Option::Some/payload:0".into());
        assert!(catalog
            .validate_variant_construct(
                &option_id,
                &option_nominal,
                &some,
                std::slice::from_ref(&some_field),
                std::slice::from_ref(&bool_id),
            )
            .is_err());
        assert!(catalog
            .validate_variant_construct(
                &option_id,
                &option_nominal,
                &crate::core::NodeId("builtin:variant:Option::Missing".into()),
                &[],
                &[],
            )
            .is_err());

        let only_some = crate::core::mir::MirSwitchArm {
            edge: crate::core::mir::MirEdgeId::new("edge:some").expect("edge"),
            target: crate::core::mir::MirBlockId::new("bb:some").expect("block"),
            arguments: Vec::new(),
            bindings: Vec::new(),
            case: crate::core::mir::MirSwitchCase::Variant(some),
        };
        let error = catalog
            .validate_switch(&option_id, &[only_some])
            .expect_err("missing None must fail closed");
        assert!(error.contains("None"));
    }

    #[test]
    fn canonical_text_is_declaration_order_independent() {
        let mut first = ResolvedTypeTable::new();
        let _ = first
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::I32))
            .expect("type");
        let _ = first
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::Bool))
            .expect("type");
        let mut second = ResolvedTypeTable::new();
        let _ = second
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::Bool))
            .expect("type");
        let _ = second
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::I32))
            .expect("type");
        let first = MirTypeCatalog::from_resolved_types(&first)
            .expect("catalog")
            .canonical_text();
        let second = MirTypeCatalog::from_resolved_types(&second)
            .expect("catalog")
            .canonical_text();
        assert_eq!(first, second);
    }
}
