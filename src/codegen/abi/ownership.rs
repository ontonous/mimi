//! Checker-backed ownership metadata for the native ABI.
//!
//! The compiler already enforces source-level move/exactly-once rules.  This
//! module describes the runtime representation obligations that remain after
//! checking: which values carry heap data, which handles are runtime-managed,
//! and which linear values must never receive ordinary clone/drop glue.

use std::collections::{BTreeSet, HashMap};

use crate::core::{FunctionTypeAbi, PrimitiveType, ResolvedType, ResolvedTypeId, TraitTypeKind};

/// Why a nominal value is linear.  Consumers currently need only the fact that
/// it is linear; retaining the kind keeps A2 glue derivation fail-closed and
/// makes diagnostics/debug output useful without re-inspecting names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::codegen) enum LinearOwnershipKind {
    Capability,
    FlowState,
    Nominal,
}

/// Runtime category of an opaque handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::codegen) enum OpaqueHandleKind {
    /// Map/Set generation+lease handles.  They are opaque at LLVM level but
    /// remain an unsupported return-transfer shape until A2 glue adopts them.
    ManagedCollection,
    /// Other runtime handles and boxed custom enums.
    Runtime,
}

/// Canonical runtime ownership shape derived from a checker-owned type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::codegen) enum OwnershipClass {
    Scalar,
    StringBox,
    List(Box<OwnershipClass>),
    Option(Box<OwnershipClass>),
    Result {
        ok: Box<OwnershipClass>,
        error: Box<OwnershipClass>,
    },
    Tuple(Vec<OwnershipClass>),
    Record(Vec<OwnershipClass>),
    /// C-style/overlapping storage.  Field ownership is useful to the coarse
    /// legacy gates, but ordinary product glue must never visit every field:
    /// only one union member is live and the active member is not encoded in
    /// this class.
    Union(Vec<OwnershipClass>),
    Array(Box<OwnershipClass>),
    Slice(Box<OwnershipClass>),
    Shared(Box<OwnershipClass>),
    Closure,
    DynamicObject,
    OpaqueHandle(OpaqueHandleKind),
    Linear {
        kind: LinearOwnershipKind,
        payload: Option<Box<OwnershipClass>>,
    },
    Generic,
    Unknown,
}

impl OwnershipClass {
    /// Whether this value can carry a heap allocation registered in the
    /// function heap scope and therefore must transfer that scope at return.
    ///
    /// This is the typed replacement for inspecting opaque LLVM pointers.
    /// Arrays intentionally retain the pre-A1 behaviour (the old return test
    /// only considered a top-level StructType); A2 will adopt them through
    /// derived glue instead of silently changing cleanup here.
    pub(in crate::codegen) fn requires_scope_drain(&self) -> bool {
        match self {
            Self::StringBox | Self::List(_) | Self::Closure | Self::Slice(_) => true,
            Self::DynamicObject => true,
            Self::Option(inner) => inner.requires_scope_drain(),
            Self::Shared(inner) => inner.requires_scope_drain(),
            Self::Result { ok, error } => ok.requires_scope_drain() || error.requires_scope_drain(),
            Self::Tuple(fields) | Self::Record(fields) | Self::Union(fields) => {
                fields.iter().any(Self::requires_scope_drain)
            }
            Self::Linear {
                payload: Some(payload),
                ..
            } => payload.requires_scope_drain(),
            Self::Scalar
            | Self::Array(_)
            | Self::OpaqueHandle(_)
            | Self::Linear { payload: None, .. }
            | Self::Generic
            | Self::Unknown => false,
        }
    }

    /// Whether a closure capture contains a List/Map/Set ownership boundary.
    pub(in crate::codegen) fn contains_heap_collection(&self) -> bool {
        match self {
            Self::List(_) | Self::OpaqueHandle(OpaqueHandleKind::ManagedCollection) => true,
            Self::Option(inner) | Self::Array(inner) | Self::Slice(inner) => {
                inner.contains_heap_collection()
            }
            Self::Shared(_) => false,
            Self::Result { ok, error } => {
                ok.contains_heap_collection() || error.contains_heap_collection()
            }
            Self::Tuple(fields) | Self::Record(fields) | Self::Union(fields) => {
                fields.iter().any(Self::contains_heap_collection)
            }
            Self::Linear {
                payload: Some(payload),
                ..
            } => payload.contains_heap_collection(),
            _ => false,
        }
    }

    /// A3 compatibility gate: types whose current return path cannot transfer
    /// ownership safely and therefore must fail closed with E0723 until A2.
    pub(in crate::codegen) fn has_unclaimed_return_heap(&self) -> bool {
        match self {
            Self::OpaqueHandle(OpaqueHandleKind::ManagedCollection) => true,
            Self::List(element) => element.list_element_has_unclaimed_heap(),
            Self::Option(inner) => inner.has_unclaimed_return_heap(),
            Self::Result { ok, error } => {
                ok.has_unclaimed_return_heap() || error.has_unclaimed_return_heap()
            }
            Self::Tuple(fields) | Self::Record(fields) | Self::Union(fields) => {
                fields.iter().any(Self::has_unclaimed_return_heap)
            }
            Self::Linear {
                payload: Some(payload),
                ..
            } => payload.has_unclaimed_return_heap(),
            _ => false,
        }
    }

    fn list_element_has_unclaimed_heap(&self) -> bool {
        match self {
            Self::List(inner) => inner.nested_list_inner_is_unclaimed(),
            Self::OpaqueHandle(OpaqueHandleKind::ManagedCollection) => true,
            Self::Option(inner) => inner.list_element_has_unclaimed_heap(),
            Self::Result { ok, error } => {
                ok.list_element_has_unclaimed_heap() || error.list_element_has_unclaimed_heap()
            }
            _ => false,
        }
    }

    fn nested_list_inner_is_unclaimed(&self) -> bool {
        match self {
            // Preserve the proven A3 boundary: deeper list nesting recurses,
            // while List<List<string>> and generic/user-record heads remain
            // admitted until the A2 glue matrix can decide them uniformly.
            Self::List(inner) => inner.nested_list_inner_is_unclaimed(),
            Self::StringBox | Self::Generic | Self::Record(_) | Self::Union(_) => false,
            Self::Scalar
            | Self::Tuple(_)
            | Self::OpaqueHandle(OpaqueHandleKind::ManagedCollection) => true,
            _ => false,
        }
    }
}

/// Derive ownership metadata from the checker-owned canonical type table.
pub(in crate::codegen) fn classify_resolved(
    program: &crate::core::CheckedProgram,
    id: &ResolvedTypeId,
) -> OwnershipClass {
    classify_resolved_inner(program, id, &mut BTreeSet::new())
}

/// Adapt a legacy surface type into the same ownership metadata used by the
/// resolved emitter.  Policy stays on [`OwnershipClass`]; this adapter exists
/// only for legacy sites that do not yet carry a `ResolvedTypeId`.
pub(in crate::codegen) fn classify_surface(
    ty: &crate::ast::Type,
    type_defs: &HashMap<String, crate::ast::TypeDef>,
) -> OwnershipClass {
    classify_surface_inner(ty, type_defs, &mut BTreeSet::new())
}

fn classify_surface_inner(
    ty: &crate::ast::Type,
    type_defs: &HashMap<String, crate::ast::TypeDef>,
    active: &mut BTreeSet<String>,
) -> OwnershipClass {
    use crate::ast::{Type, TypeDefKind};

    match ty.unlocated() {
        Type::Name(name, arguments) => match name.as_str() {
            "string" => OwnershipClass::StringBox,
            "List" => OwnershipClass::List(Box::new(
                arguments
                    .first()
                    .map(|arg| classify_surface_inner(arg, type_defs, active))
                    .unwrap_or(OwnershipClass::Unknown),
            )),
            "Option" => OwnershipClass::Option(Box::new(
                arguments
                    .first()
                    .map(|arg| classify_surface_inner(arg, type_defs, active))
                    .unwrap_or(OwnershipClass::Unknown),
            )),
            "Result" => OwnershipClass::Result {
                ok: Box::new(
                    arguments
                        .first()
                        .map(|arg| classify_surface_inner(arg, type_defs, active))
                        .unwrap_or(OwnershipClass::Unknown),
                ),
                error: Box::new(
                    arguments
                        .get(1)
                        .map(|arg| classify_surface_inner(arg, type_defs, active))
                        .unwrap_or(OwnershipClass::Unknown),
                ),
            },
            "Map" | "Set" => OwnershipClass::OpaqueHandle(OpaqueHandleKind::ManagedCollection),
            "i8" | "i16" | "i32" | "i64" | "i128" | "u8" | "u16" | "u32" | "u64" | "u128"
            | "f32" | "f64" | "bool" | "char" | "unit" | "nothing" => OwnershipClass::Scalar,
            _ => {
                let definition = type_defs.get(name).or_else(|| {
                    type_defs.values().find(|definition| {
                        definition.name == *name
                            || definition.name.ends_with(&format!("::{name}"))
                            || definition.name.ends_with(&format!(".{name}"))
                    })
                });
                let Some(definition) = definition else {
                    return OwnershipClass::Generic;
                };
                if !active.insert(definition.name.clone()) {
                    return OwnershipClass::Unknown;
                }
                let class = match &definition.kind {
                    TypeDefKind::Alias(inner) | TypeDefKind::Newtype(inner) => {
                        classify_surface_inner(inner, type_defs, active)
                    }
                    TypeDefKind::Record(fields) => OwnershipClass::Record(
                        fields
                            .iter()
                            .map(|field| classify_surface_inner(&field.ty, type_defs, active))
                            .collect(),
                    ),
                    TypeDefKind::Union(fields) => OwnershipClass::Union(
                        fields
                            .iter()
                            .map(|field| classify_surface_inner(&field.ty, type_defs, active))
                            .collect(),
                    ),
                    TypeDefKind::Enum(_) => OwnershipClass::OpaqueHandle(OpaqueHandleKind::Runtime),
                };
                active.remove(&definition.name);
                class
            }
        },
        Type::Option(inner) => {
            OwnershipClass::Option(Box::new(classify_surface_inner(inner, type_defs, active)))
        }
        Type::Result(ok, error) => OwnershipClass::Result {
            ok: Box::new(classify_surface_inner(ok, type_defs, active)),
            error: Box::new(classify_surface_inner(error, type_defs, active)),
        },
        Type::Tuple(fields) => OwnershipClass::Tuple(
            fields
                .iter()
                .map(|field| classify_surface_inner(field, type_defs, active))
                .collect(),
        ),
        Type::Func(_, _) => OwnershipClass::Closure,
        Type::ExternFunc(_, _) => OwnershipClass::Scalar,
        Type::Array(inner, _) => {
            OwnershipClass::Array(Box::new(classify_surface_inner(inner, type_defs, active)))
        }
        Type::Slice(inner) => {
            OwnershipClass::Slice(Box::new(classify_surface_inner(inner, type_defs, active)))
        }
        Type::Shared(inner) | Type::Weak(inner) => {
            OwnershipClass::Shared(Box::new(classify_surface_inner(inner, type_defs, active)))
        }
        Type::Newtype(_, inner) => classify_surface_inner(inner, type_defs, active),
        Type::Ref(_, _)
        | Type::RefMut(_, _)
        | Type::CBuffer(_)
        | Type::RawPtr(_)
        | Type::RawPtrMut(_) => OwnershipClass::Scalar,
        Type::Cap(_) | Type::CapAtom(_) => OwnershipClass::Linear {
            kind: LinearOwnershipKind::Capability,
            payload: None,
        },
        Type::DynTrait(_) => OwnershipClass::DynamicObject,
        Type::ImplTrait(_) | Type::Nothing => OwnershipClass::Scalar,
        Type::Infer | Type::TypeVar(_) | Type::ForAll(_, _) => OwnershipClass::Generic,
        Type::TyErr => OwnershipClass::Unknown,
        Type::Located { .. } => unreachable!("Type::unlocated returned Located"),
    }
}

fn classify_resolved_inner(
    program: &crate::core::CheckedProgram,
    id: &ResolvedTypeId,
    active: &mut BTreeSet<ResolvedTypeId>,
) -> OwnershipClass {
    if !active.insert(id.clone()) {
        return OwnershipClass::Unknown;
    }
    let class = match program.resolved_types().get(id) {
        Some(ResolvedType::Primitive(PrimitiveType::String)) => OwnershipClass::StringBox,
        Some(ResolvedType::Primitive(_)) => OwnershipClass::Scalar,
        Some(ResolvedType::GenericParameter(_)) => OwnershipClass::Generic,
        Some(ResolvedType::Option(inner)) => {
            OwnershipClass::Option(Box::new(classify_resolved_inner(program, inner, active)))
        }
        Some(ResolvedType::Result { ok, error }) => OwnershipClass::Result {
            ok: Box::new(classify_resolved_inner(program, ok, active)),
            error: Box::new(classify_resolved_inner(program, error, active)),
        },
        Some(ResolvedType::Tuple(fields)) => OwnershipClass::Tuple(
            fields
                .iter()
                .map(|field| classify_resolved_inner(program, field, active))
                .collect(),
        ),
        Some(ResolvedType::Array { element, .. }) => {
            OwnershipClass::Array(Box::new(classify_resolved_inner(program, element, active)))
        }
        Some(ResolvedType::Slice(inner)) => {
            OwnershipClass::Slice(Box::new(classify_resolved_inner(program, inner, active)))
        }
        Some(ResolvedType::Newtype { inner, .. }) => {
            classify_resolved_inner(program, inner, active)
        }
        Some(ResolvedType::Function { abi, .. }) => match abi {
            FunctionTypeAbi::Mimi => OwnershipClass::Closure,
            FunctionTypeAbi::C => OwnershipClass::Scalar,
        },
        Some(ResolvedType::Trait {
            kind: TraitTypeKind::Dynamic,
            ..
        }) => OwnershipClass::DynamicObject,
        Some(ResolvedType::Ownership { target, .. }) => {
            OwnershipClass::Shared(Box::new(classify_resolved_inner(program, target, active)))
        }
        Some(ResolvedType::Trait { .. })
        | Some(ResolvedType::Reference { .. })
        | Some(ResolvedType::CBuffer(_))
        | Some(ResolvedType::RawPointer { .. })
        | Some(ResolvedType::DynamicAny { .. }) => OwnershipClass::Scalar,
        Some(ResolvedType::Capability(_)) => OwnershipClass::Linear {
            kind: LinearOwnershipKind::Capability,
            payload: None,
        },
        Some(ResolvedType::FlowStateSet { .. }) => OwnershipClass::Linear {
            kind: LinearOwnershipKind::FlowState,
            payload: None,
        },
        Some(ResolvedType::Nominal {
            item,
            arguments,
            is_linear,
        }) => {
            let name = item
                .as_str()
                .strip_prefix("builtin:type:")
                .unwrap_or(item.as_str());
            match name {
                "List" => OwnershipClass::List(Box::new(
                    arguments
                        .first()
                        .map(|arg| classify_resolved_inner(program, arg, active))
                        .unwrap_or(OwnershipClass::Unknown),
                )),
                "Option" => OwnershipClass::Option(Box::new(
                    arguments
                        .first()
                        .map(|arg| classify_resolved_inner(program, arg, active))
                        .unwrap_or(OwnershipClass::Unknown),
                )),
                "Result" => OwnershipClass::Result {
                    ok: Box::new(
                        arguments
                            .first()
                            .map(|arg| classify_resolved_inner(program, arg, active))
                            .unwrap_or(OwnershipClass::Unknown),
                    ),
                    error: Box::new(
                        arguments
                            .get(1)
                            .map(|arg| classify_resolved_inner(program, arg, active))
                            .unwrap_or(OwnershipClass::Unknown),
                    ),
                },
                "Map" | "Set" => OwnershipClass::OpaqueHandle(OpaqueHandleKind::ManagedCollection),
                "string" => OwnershipClass::StringBox,
                _ => {
                    let payload = classify_nominal_fields(program, item.as_str(), active)
                        .or_else(|| builtin_record_class(item.as_str()));
                    if *is_linear {
                        OwnershipClass::Linear {
                            kind: if item.nominal_is_flow_state() {
                                LinearOwnershipKind::FlowState
                            } else {
                                LinearOwnershipKind::Nominal
                            },
                            payload: payload.map(Box::new),
                        }
                    } else {
                        payload.unwrap_or(OwnershipClass::OpaqueHandle(OpaqueHandleKind::Runtime))
                    }
                }
            }
        }
        None => OwnershipClass::Unknown,
    };
    active.remove(id);
    class
}

fn builtin_record_class(identity: &str) -> Option<OwnershipClass> {
    let name = identity.strip_prefix("builtin:type:").unwrap_or(identity);
    let string = || OwnershipClass::StringBox;
    let scalar = || OwnershipClass::Scalar;
    let fields = match name {
        "MemoryDump" => vec![string(), scalar()],
        "PanicPayload" => vec![string(), string(), scalar(), string()],
        "PeerFault" => vec![string(), string()],
        "ExecResult" => vec![scalar(), string(), string()],
        "SystemTrace" => vec![
            string(),
            string(),
            string(),
            OwnershipClass::Record(vec![string(), scalar()]),
            OwnershipClass::Record(vec![string(), string(), scalar(), string()]),
        ],
        "StatResult" => vec![scalar(), scalar(), scalar(), scalar()],
        _ => return None,
    };
    Some(OwnershipClass::Record(fields))
}

fn classify_nominal_fields(
    program: &crate::core::CheckedProgram,
    identity: &str,
    active: &mut BTreeSet<ResolvedTypeId>,
) -> Option<OwnershipClass> {
    let short = identity.strip_prefix("type:").unwrap_or(identity);
    let definition = program
        .type_def(identity)
        .or_else(|| program.type_def(short))
        .or_else(|| {
            program.type_defs().values().find(|definition| {
                definition.qualified_name == identity
                    || definition.qualified_name == short
                    || definition.qualified_name.ends_with(&format!("::{short}"))
                    || definition.qualified_name.ends_with(&format!(".{short}"))
            })
        })?;
    let is_union = match definition.kind {
        crate::core::resolved::ResolvedTypeKind::Record => false,
        crate::core::resolved::ResolvedTypeKind::Union => true,
        _ => return None,
    };
    let fields = definition
        .fields
        .iter()
        .filter_map(|(name, _)| definition.field_ids.get(name))
        .filter_map(|field| program.resolved_field_types().get(field))
        .map(|field| classify_resolved_inner(program, field, active))
        .collect();
    Some(if is_union {
        OwnershipClass::Union(fields)
    } else {
        OwnershipClass::Record(fields)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked_program(source: &str) -> crate::core::CheckedProgram {
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let file = crate::parser::Parser::new(tokens).parse_file().unwrap();
        crate::core::check_program(&file).unwrap()
    }

    fn result_class(program: &crate::core::CheckedProgram, function_name: &str) -> OwnershipClass {
        let function = program
            .functions()
            .values()
            .find(|function| function.qualified_name == function_name)
            .unwrap();
        let callable = program.callable(&function.node_id).unwrap();
        classify_resolved(program, &callable.signature.result)
    }

    #[test]
    fn a3_unclaimed_heap_policy_is_shape_driven() {
        let scalar = OwnershipClass::Scalar;
        let string = OwnershipClass::StringBox;
        assert!(
            OwnershipClass::List(Box::new(OwnershipClass::List(Box::new(scalar.clone()))))
                .has_unclaimed_return_heap()
        );
        assert!(
            !OwnershipClass::List(Box::new(OwnershipClass::List(Box::new(string))))
                .has_unclaimed_return_heap()
        );
        assert!(
            !OwnershipClass::Record(vec![OwnershipClass::List(Box::new(scalar))])
                .has_unclaimed_return_heap()
        );
        assert!(
            OwnershipClass::OpaqueHandle(OpaqueHandleKind::ManagedCollection)
                .has_unclaimed_return_heap()
        );
    }

    #[test]
    fn collection_and_scope_queries_share_the_same_shape() {
        let class = OwnershipClass::Result {
            ok: Box::new(OwnershipClass::Tuple(vec![OwnershipClass::List(Box::new(
                OwnershipClass::Scalar,
            ))])),
            error: Box::new(OwnershipClass::StringBox),
        };
        assert!(class.contains_heap_collection());
        assert!(class.requires_scope_drain());
    }

    #[test]
    fn checked_record_fields_are_classified_from_canonical_ids() {
        let program = checked_program(
            r#"
            type Wrap { mats: List<List<i32>> }
            func make() -> Wrap { Wrap { mats: [[1, 2], [3, 4]] } }
            func main() -> i32 { let _w = make(); 0 }
            "#,
        );
        let class = result_class(&program, "make");
        assert!(matches!(class, OwnershipClass::Record(_)));
        assert!(class.has_unclaimed_return_heap());
    }

    #[test]
    fn checked_linear_nominal_never_collapses_to_scalar() {
        let program = checked_program(
            r#"
            func pass(t: SystemToken) -> SystemToken { t }
            func main() -> i32 {
                let t = make_token()
                let _id = token_id(pass(t))
                0
            }
            "#,
        );
        assert!(matches!(
            result_class(&program, "pass"),
            OwnershipClass::Linear { .. }
        ));
    }

    #[test]
    fn surface_union_remains_distinct_from_record() {
        let tokens = crate::lexer::Lexer::new(
            r#"
            #[repr(C)]
            type Cell = union { narrow: i32, wide: i64 }
            func main() -> i32 { 0 }
            "#,
        )
        .tokenize()
        .unwrap();
        let file = crate::parser::Parser::new(tokens).parse_file().unwrap();
        let type_defs = file
            .items
            .iter()
            .filter_map(|item| match item {
                crate::ast::Item::Type(definition) => {
                    Some((definition.name.clone(), definition.clone()))
                }
                _ => None,
            })
            .collect::<HashMap<_, _>>();

        assert_eq!(
            classify_surface(
                &crate::ast::Type::Name("Cell".into(), Vec::new()),
                &type_defs,
            ),
            OwnershipClass::Union(vec![OwnershipClass::Scalar, OwnershipClass::Scalar])
        );
    }

    #[test]
    fn checked_union_remains_distinct_from_record() {
        let program = checked_program(
            r#"
            #[repr(C)]
            type Cell = union { narrow: i32, wide: i64 }
            func pass(value: Cell) -> Cell { value }
            func main() -> i32 { 0 }
            "#,
        );

        assert_eq!(
            result_class(&program, "pass"),
            OwnershipClass::Union(vec![OwnershipClass::Scalar, OwnershipClass::Scalar])
        );
    }
}
