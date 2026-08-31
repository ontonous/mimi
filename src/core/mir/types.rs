//! Backend-independent type descriptors consumed by canonical MIR passes.
//!
//! `ResolvedTypeId` remains the stable identity. `MirTypeDesc` is the first
//! materialized semantic view of that identity: it records ownership and an
//! abstract ABI class without importing LLVM, runtime handles, or bytecode
//! opcodes. Backends may map the ABI class to a physical layout later, but may
//! not re-derive ownership from that layout.

use std::collections::BTreeMap;

use crate::core::ir::{
    FunctionTypeAbi, OwnershipTypeKind, PrimitiveType, ResolvedType, ResolvedTypeId,
    ResolvedTypeTable,
};
use crate::core::{CheckedProgram, NodeId, ResolvedTypeKind};

pub const MIR_TYPE_DESC_SCHEMA_VERSION: &str = "mimi-mir-type-desc-2";

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
    },
    Result {
        ok: ResolvedTypeId,
        error: ResolvedTypeId,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirTypeDesc {
    pub id: ResolvedTypeId,
    pub kind: MirTypeKind,
    pub layout: MirLayout,
    pub ownership: MirOwnership,
    pub abi: MirAbiClass,
    pub needs_drop_glue: bool,
    pub needs_clone_glue: bool,
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
                },
            ),
            ResolvedType::Result { ok, error } => (
                MirTypeKind::Result,
                MirAbiClass::Aggregate,
                MirLayout::Result {
                    ok: ok.clone(),
                    error: error.clone(),
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
        Self {
            id: id.clone(),
            kind,
            layout,
            ownership,
            abi,
            needs_drop_glue: ownership.needs_drop(),
            needs_clone_glue: ownership.needs_clone(),
        }
    }
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
        Ok(Self { entries })
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
            if let Some(descriptor) = catalog.entries.get_mut(id) {
                descriptor.abi = MirAbiClass::Aggregate;
                descriptor.layout = MirLayout::Record {
                    nominal: item.clone(),
                    fields,
                };
            }
        }
        if errors.is_empty() {
            Ok(catalog)
        } else {
            Err(errors)
        }
    }

    pub fn get(&self, id: &ResolvedTypeId) -> Option<&MirTypeDesc> {
        self.entries.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ResolvedTypeId, &MirTypeDesc)> {
        self.entries.iter()
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

    pub fn canonical_text(&self) -> String {
        let mut output = format!("mir.type-catalog {MIR_TYPE_DESC_SCHEMA_VERSION}\n");
        for (id, descriptor) in &self.entries {
            output.push_str(&format!(
                "{} kind={:?} layout={:?} ownership={:?} abi={:?} drop={} clone={}\n",
                id.as_str(),
                descriptor.kind,
                descriptor.layout,
                descriptor.ownership,
                descriptor.abi,
                descriptor.needs_drop_glue,
                descriptor.needs_clone_glue,
            ));
        }
        output
    }
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
    use super::{MirAbiClass, MirLayout, MirOwnership, MirTypeCatalog};
    use crate::core::ir::{PrimitiveType, ResolvedType, ResolvedTypeTable};

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
        assert_eq!(
            catalog.get(&option_id).expect("option descriptor").layout,
            MirLayout::Option {
                inner: i32_id.clone()
            }
        );
        assert_eq!(
            catalog.get(&result_id).expect("result descriptor").layout,
            MirLayout::Result {
                ok: i32_id.clone(),
                error: bool_id.clone(),
            }
        );
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
