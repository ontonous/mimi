use crate::ast::Type;
use crate::core::phase::ZonkedTy;
use crate::core::NodeId;
use std::collections::BTreeMap;
use std::fmt::Write as _;

pub const RESOLVED_TYPE_SCHEMA_VERSION: &str = "mimi-resolved-type-1";

/// Stable structural identity of a canonical type.
///
/// This is never a dense table index. The two-domain FNV fingerprint is
/// deterministic across processes and declaration order. The table also
/// compares canonical encodings and fails closed if a collision is observed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ResolvedTypeId(String);

impl ResolvedTypeId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn from_canonical(canonical: &str) -> Self {
        let left = stable_hash(canonical.as_bytes(), 0xcbf2_9ce4_8422_2325);
        let right = stable_hash(canonical.as_bytes(), 0x8422_2325_cbf2_9ce4);
        Self(format!("rt:{left:016x}{right:016x}"))
    }
}

/// Qualified identity of a nominal type declaration.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NominalTypeId(String);

impl NominalTypeId {
    pub fn new(identity: impl Into<String>) -> Result<Self, ResolvedTypeError> {
        let identity = identity.into();
        if identity.trim().is_empty() {
            return Err(ResolvedTypeError::InvalidIdentity {
                kind: "nominal type",
                identity,
            });
        }
        Ok(Self(identity))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PrimitiveType {
    I8,
    I16,
    I32,
    I64,
    I128,
    U8,
    U16,
    U32,
    U64,
    U128,
    Isize,
    Usize,
    F32,
    F64,
    Bool,
    Char,
    String,
    Unit,
}

impl PrimitiveType {
    pub fn from_language_name(name: &str) -> Option<Self> {
        Some(match name {
            "i8" => Self::I8,
            "i16" => Self::I16,
            "i32" => Self::I32,
            "i64" => Self::I64,
            "i128" => Self::I128,
            "u8" => Self::U8,
            "u16" => Self::U16,
            "u32" => Self::U32,
            "u64" => Self::U64,
            "u128" => Self::U128,
            "isize" => Self::Isize,
            "usize" => Self::Usize,
            "f32" => Self::F32,
            "f64" => Self::F64,
            "bool" => Self::Bool,
            "char" => Self::Char,
            "string" => Self::String,
            "unit" => Self::Unit,
            _ => return None,
        })
    }

    fn tag(self) -> &'static str {
        match self {
            Self::I8 => "i8",
            Self::I16 => "i16",
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::I128 => "i128",
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::U128 => "u128",
            Self::Isize => "isize",
            Self::Usize => "usize",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::Bool => "bool",
            Self::Char => "char",
            Self::String => "string",
            Self::Unit => "unit",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FunctionTypeAbi {
    Mimi,
    C,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OwnershipTypeKind {
    Shared,
    LocalShared,
    Weak,
    WeakLocal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TraitTypeKind {
    Opaque,
    Dynamic,
}

/// Result of checker-authoritative name resolution for `ast::Type::Name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedTypeName {
    Primitive(PrimitiveType),
    Nominal(NominalTypeId),
    /// A named binder in a generic callable/type declaration.
    GenericParameter(NodeId),
}

impl ResolvedTypeName {
    pub fn primitive(name: &str) -> Option<Self> {
        PrimitiveType::from_language_name(name).map(Self::Primitive)
    }
}

/// Capabilities which allow otherwise erased canonical types.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedTypeCapabilities {
    dynamic_any: Option<String>,
}

impl ResolvedTypeCapabilities {
    pub fn with_dynamic_any(capability: impl Into<String>) -> Result<Self, ResolvedTypeError> {
        let capability = capability.into();
        if capability.trim().is_empty() {
            return Err(ResolvedTypeError::InvalidIdentity {
                kind: "backend capability",
                identity: capability,
            });
        }
        Ok(Self {
            dynamic_any: Some(capability),
        })
    }
}

/// Canonical type shape. Every recursive edge is a stable `ResolvedTypeId`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedType {
    Primitive(PrimitiveType),
    GenericParameter(NodeId),
    Nominal {
        item: NominalTypeId,
        arguments: Vec<ResolvedTypeId>,
    },
    Reference {
        lifetime: Option<String>,
        mutable: bool,
        target: ResolvedTypeId,
    },
    Option(ResolvedTypeId),
    Result {
        ok: ResolvedTypeId,
        error: ResolvedTypeId,
    },
    Tuple(Vec<ResolvedTypeId>),
    Function {
        abi: FunctionTypeAbi,
        parameters: Vec<ResolvedTypeId>,
        result: ResolvedTypeId,
    },
    CBuffer(ResolvedTypeId),
    Capability(NominalTypeId),
    Ownership {
        kind: OwnershipTypeKind,
        target: ResolvedTypeId,
    },
    Newtype {
        item: NominalTypeId,
        inner: ResolvedTypeId,
    },
    Nothing,
    Allocator,
    Array {
        element: ResolvedTypeId,
        length: usize,
    },
    Slice(ResolvedTypeId),
    Trait {
        kind: TraitTypeKind,
        traits: Vec<NominalTypeId>,
    },
    RawPointer {
        mutable: bool,
        target: ResolvedTypeId,
    },
    CShared(ResolvedTypeId),
    CBorrow {
        mutable: bool,
        target: ResolvedTypeId,
    },
    RawString,
    DynamicAny {
        capability: String,
    },
}

impl ResolvedType {
    fn referenced_types(&self) -> Vec<&ResolvedTypeId> {
        match self {
            Self::Nominal { arguments, .. } | Self::Tuple(arguments) => arguments.iter().collect(),
            Self::Reference { target, .. }
            | Self::CBuffer(target)
            | Self::Ownership { target, .. }
            | Self::Newtype { inner: target, .. }
            | Self::Slice(target)
            | Self::RawPointer { target, .. }
            | Self::CShared(target)
            | Self::CBorrow { target, .. }
            | Self::Option(target) => vec![target],
            Self::Result { ok, error } => vec![ok, error],
            Self::Function {
                parameters, result, ..
            } => parameters.iter().chain(std::iter::once(result)).collect(),
            Self::Array { element, .. } => vec![element],
            Self::Primitive(_)
            | Self::GenericParameter(_)
            | Self::Capability(_)
            | Self::Nothing
            | Self::Allocator
            | Self::Trait { .. }
            | Self::RawString
            | Self::DynamicAny { .. } => Vec::new(),
        }
    }

    fn canonical(&self) -> String {
        let mut output = String::new();
        match self {
            Self::Primitive(primitive) => atom(&mut output, "primitive", primitive.tag()),
            Self::GenericParameter(parameter) => {
                atom(&mut output, "generic-parameter", &parameter.0);
            }
            Self::Nominal { item, arguments } => {
                atom(&mut output, "nominal", item.as_str());
                ids(&mut output, arguments);
            }
            Self::Reference {
                lifetime,
                mutable,
                target,
            } => {
                atom(
                    &mut output,
                    "reference",
                    if *mutable { "mut" } else { "shared" },
                );
                optional_atom(&mut output, lifetime.as_deref());
                id(&mut output, target);
            }
            Self::Option(inner) => {
                atom(&mut output, "option", "");
                id(&mut output, inner);
            }
            Self::Result { ok, error } => {
                atom(&mut output, "result", "");
                id(&mut output, ok);
                id(&mut output, error);
            }
            Self::Tuple(elements) => {
                atom(&mut output, "tuple", "");
                ids(&mut output, elements);
            }
            Self::Function {
                abi,
                parameters,
                result,
            } => {
                atom(
                    &mut output,
                    "function",
                    match abi {
                        FunctionTypeAbi::Mimi => "mimi",
                        FunctionTypeAbi::C => "c",
                    },
                );
                ids(&mut output, parameters);
                id(&mut output, result);
            }
            Self::CBuffer(inner) => unary(&mut output, "c-buffer", inner),
            Self::Capability(capability) => {
                atom(&mut output, "capability", capability.as_str());
            }
            Self::Ownership { kind, target } => {
                atom(
                    &mut output,
                    "ownership",
                    match kind {
                        OwnershipTypeKind::Shared => "shared",
                        OwnershipTypeKind::LocalShared => "local-shared",
                        OwnershipTypeKind::Weak => "weak",
                        OwnershipTypeKind::WeakLocal => "weak-local",
                    },
                );
                id(&mut output, target);
            }
            Self::Newtype { item, inner } => {
                atom(&mut output, "newtype", item.as_str());
                id(&mut output, inner);
            }
            Self::Nothing => atom(&mut output, "nothing", ""),
            Self::Allocator => atom(&mut output, "allocator", ""),
            Self::Array { element, length } => {
                atom(&mut output, "array", &length.to_string());
                id(&mut output, element);
            }
            Self::Slice(inner) => unary(&mut output, "slice", inner),
            Self::Trait { kind, traits } => {
                atom(
                    &mut output,
                    "trait",
                    match kind {
                        TraitTypeKind::Opaque => "opaque",
                        TraitTypeKind::Dynamic => "dynamic",
                    },
                );
                let _ = write!(output, "{};", traits.len());
                for item in traits {
                    atom(&mut output, "trait-id", item.as_str());
                }
            }
            Self::RawPointer { mutable, target } => {
                atom(
                    &mut output,
                    "raw-pointer",
                    if *mutable { "mut" } else { "const" },
                );
                id(&mut output, target);
            }
            Self::CShared(inner) => unary(&mut output, "c-shared", inner),
            Self::CBorrow { mutable, target } => {
                atom(
                    &mut output,
                    "c-borrow",
                    if *mutable { "mut" } else { "shared" },
                );
                id(&mut output, target);
            }
            Self::RawString => atom(&mut output, "raw-string", ""),
            Self::DynamicAny { capability } => atom(&mut output, "dynamic-any", capability),
        }
        output
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedTypeError {
    ResidualInference(&'static str),
    UnknownTypeName(String),
    PrimitiveHasArguments(String),
    DynamicAnyRequiresCapability,
    InvalidIdentity {
        kind: &'static str,
        identity: String,
    },
    MissingReferencedType {
        owner: ResolvedTypeId,
        missing: ResolvedTypeId,
    },
    FingerprintMismatch {
        stored: ResolvedTypeId,
        computed: ResolvedTypeId,
    },
    FingerprintCollision(ResolvedTypeId),
}

impl std::fmt::Display for ResolvedTypeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ResidualInference(kind) => {
                write!(
                    formatter,
                    "resolved type contains residual inference node {kind}"
                )
            }
            Self::UnknownTypeName(name) => {
                write!(
                    formatter,
                    "type name '{name}' has no resolved nominal identity"
                )
            }
            Self::PrimitiveHasArguments(name) => {
                write!(
                    formatter,
                    "primitive type '{name}' cannot have type arguments"
                )
            }
            Self::DynamicAnyRequiresCapability => write!(
                formatter,
                "dynamic Any requires an explicit type.dynamic_value capability"
            ),
            Self::InvalidIdentity { kind, identity } => {
                write!(
                    formatter,
                    "{kind} identity '{identity}' is empty or invalid"
                )
            }
            Self::MissingReferencedType { owner, missing } => write!(
                formatter,
                "resolved type '{}' references missing type '{}'",
                owner.as_str(),
                missing.as_str()
            ),
            Self::FingerprintMismatch { stored, computed } => write!(
                formatter,
                "resolved type fingerprint '{}' does not match canonical fingerprint '{}'",
                stored.as_str(),
                computed.as_str()
            ),
            Self::FingerprintCollision(id) => write!(
                formatter,
                "resolved type fingerprint collision for '{}'",
                id.as_str()
            ),
        }
    }
}

impl std::error::Error for ResolvedTypeError {}

/// Canonical type interner keyed only by stable structural fingerprints.
#[derive(Debug, Clone, Default)]
pub struct ResolvedTypeTable {
    entries: BTreeMap<ResolvedTypeId, ResolvedType>,
    canonical: BTreeMap<ResolvedTypeId, String>,
}

impl ResolvedTypeTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, id: &ResolvedTypeId) -> Option<&ResolvedType> {
        self.entries.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ResolvedTypeId, &ResolvedType)> {
        self.entries.iter()
    }

    /// Convert a fully-zonked AST type into canonical IR.
    ///
    /// `resolve_name` must return the checker-selected identity. Bare spelling
    /// is never accepted as nominal identity by the type table itself.
    pub fn intern_zonked<R>(
        &mut self,
        ty: &ZonkedTy,
        capabilities: &ResolvedTypeCapabilities,
        mut resolve_name: R,
    ) -> Result<ResolvedTypeId, ResolvedTypeError>
    where
        R: FnMut(&str) -> Option<ResolvedTypeName>,
    {
        self.intern_type(ty.as_type(), capabilities, &mut resolve_name)
    }

    pub fn validate(&self) -> Result<(), Vec<ResolvedTypeError>> {
        let mut errors = Vec::new();
        for (stored_id, ty) in &self.entries {
            if let ResolvedType::GenericParameter(parameter) = ty {
                if parameter.0.trim().is_empty() {
                    errors.push(ResolvedTypeError::InvalidIdentity {
                        kind: "generic parameter",
                        identity: parameter.0.clone(),
                    });
                }
            }
            let canonical = ty.canonical();
            let computed_id = ResolvedTypeId::from_canonical(&canonical);
            if &computed_id != stored_id {
                errors.push(ResolvedTypeError::FingerprintMismatch {
                    stored: stored_id.clone(),
                    computed: computed_id,
                });
            }
            if self.canonical.get(stored_id) != Some(&canonical) {
                errors.push(ResolvedTypeError::FingerprintCollision(stored_id.clone()));
            }
            for reference in ty.referenced_types() {
                if !self.entries.contains_key(reference) {
                    errors.push(ResolvedTypeError::MissingReferencedType {
                        owner: stored_id.clone(),
                        missing: reference.clone(),
                    });
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    fn intern_type<R>(
        &mut self,
        ty: &Type,
        capabilities: &ResolvedTypeCapabilities,
        resolve_name: &mut R,
    ) -> Result<ResolvedTypeId, ResolvedTypeError>
    where
        R: FnMut(&str) -> Option<ResolvedTypeName>,
    {
        let resolved = match ty.unlocated() {
            Type::Located { .. } => unreachable!("Type::unlocated returned Located"),
            Type::Infer => return Err(ResolvedTypeError::ResidualInference("Infer")),
            Type::TypeVar(_) => return Err(ResolvedTypeError::ResidualInference("TypeVar")),
            Type::ForAll(_, _) => return Err(ResolvedTypeError::ResidualInference("ForAll")),
            Type::Name(name, arguments) if name == "_" || name == "unknown" => {
                return Err(ResolvedTypeError::ResidualInference("Unknown"));
            }
            Type::Name(name, arguments) if name == "Any" => {
                if !arguments.is_empty() {
                    return Err(ResolvedTypeError::PrimitiveHasArguments(name.clone()));
                }
                let Some(capability) = capabilities.dynamic_any.clone() else {
                    return Err(ResolvedTypeError::DynamicAnyRequiresCapability);
                };
                ResolvedType::DynamicAny { capability }
            }
            Type::Name(name, arguments) if name == "Option" && arguments.len() == 1 => {
                ResolvedType::Option(self.intern_type(&arguments[0], capabilities, resolve_name)?)
            }
            Type::Name(name, arguments) if name == "Result" && arguments.len() == 2 => {
                ResolvedType::Result {
                    ok: self.intern_type(&arguments[0], capabilities, resolve_name)?,
                    error: self.intern_type(&arguments[1], capabilities, resolve_name)?,
                }
            }
            Type::Name(name, arguments) if name == "Tuple" => {
                ResolvedType::Tuple(self.intern_many(arguments, capabilities, resolve_name)?)
            }
            Type::Name(name, arguments) => {
                let Some(identity) = resolve_name(name) else {
                    return Err(ResolvedTypeError::UnknownTypeName(name.clone()));
                };
                match identity {
                    ResolvedTypeName::Primitive(primitive) => {
                        if !arguments.is_empty() {
                            return Err(ResolvedTypeError::PrimitiveHasArguments(name.clone()));
                        }
                        ResolvedType::Primitive(primitive)
                    }
                    ResolvedTypeName::Nominal(item) => ResolvedType::Nominal {
                        item,
                        arguments: self.intern_many(arguments, capabilities, resolve_name)?,
                    },
                    ResolvedTypeName::GenericParameter(parameter) => {
                        if !arguments.is_empty() {
                            return Err(ResolvedTypeError::PrimitiveHasArguments(name.clone()));
                        }
                        ResolvedType::GenericParameter(parameter)
                    }
                }
            }
            Type::Ref(lifetime, target) | Type::RefMut(lifetime, target) => {
                ResolvedType::Reference {
                    lifetime: lifetime.clone(),
                    mutable: matches!(ty.unlocated(), Type::RefMut(_, _)),
                    target: self.intern_type(target, capabilities, resolve_name)?,
                }
            }
            Type::Option(inner) => {
                ResolvedType::Option(self.intern_type(inner, capabilities, resolve_name)?)
            }
            Type::Result(ok, error) => ResolvedType::Result {
                ok: self.intern_type(ok, capabilities, resolve_name)?,
                error: self.intern_type(error, capabilities, resolve_name)?,
            },
            Type::Tuple(elements) => {
                ResolvedType::Tuple(self.intern_many(elements, capabilities, resolve_name)?)
            }
            Type::Func(parameters, result) | Type::ExternFunc(parameters, result) => {
                ResolvedType::Function {
                    abi: if matches!(ty.unlocated(), Type::ExternFunc(_, _)) {
                        FunctionTypeAbi::C
                    } else {
                        FunctionTypeAbi::Mimi
                    },
                    parameters: self.intern_many(parameters, capabilities, resolve_name)?,
                    result: self.intern_type(result, capabilities, resolve_name)?,
                }
            }
            Type::CBuffer(inner) => {
                ResolvedType::CBuffer(self.intern_type(inner, capabilities, resolve_name)?)
            }
            Type::Cap(name) => ResolvedType::Capability(resolve_nominal(name, resolve_name)?),
            Type::Shared(inner)
            | Type::LocalShared(inner)
            | Type::Weak(inner)
            | Type::WeakLocal(inner) => ResolvedType::Ownership {
                kind: match ty.unlocated() {
                    Type::Shared(_) => OwnershipTypeKind::Shared,
                    Type::LocalShared(_) => OwnershipTypeKind::LocalShared,
                    Type::Weak(_) => OwnershipTypeKind::Weak,
                    Type::WeakLocal(_) => OwnershipTypeKind::WeakLocal,
                    _ => unreachable!(),
                },
                target: self.intern_type(inner, capabilities, resolve_name)?,
            },
            Type::Newtype(name, inner) => ResolvedType::Newtype {
                item: resolve_nominal(name, resolve_name)?,
                inner: self.intern_type(inner, capabilities, resolve_name)?,
            },
            Type::Nothing => ResolvedType::Nothing,
            Type::Allocator => ResolvedType::Allocator,
            Type::Array(element, length) => ResolvedType::Array {
                element: self.intern_type(element, capabilities, resolve_name)?,
                length: *length,
            },
            Type::Slice(inner) => {
                ResolvedType::Slice(self.intern_type(inner, capabilities, resolve_name)?)
            }
            Type::ImplTrait(traits) | Type::DynTrait(traits) => {
                let kind = if matches!(ty.unlocated(), Type::ImplTrait(_)) {
                    TraitTypeKind::Opaque
                } else {
                    TraitTypeKind::Dynamic
                };
                let mut traits = traits
                    .iter()
                    .map(|name| resolve_nominal(name, resolve_name))
                    .collect::<Result<Vec<_>, _>>()?;
                traits.sort();
                traits.dedup();
                ResolvedType::Trait { kind, traits }
            }
            Type::RawPtr(inner) | Type::RawPtrMut(inner) => ResolvedType::RawPointer {
                mutable: matches!(ty.unlocated(), Type::RawPtrMut(_)),
                target: self.intern_type(inner, capabilities, resolve_name)?,
            },
            Type::CShared(inner) => {
                ResolvedType::CShared(self.intern_type(inner, capabilities, resolve_name)?)
            }
            Type::CBorrow(inner) | Type::CBorrowMut(inner) => ResolvedType::CBorrow {
                mutable: matches!(ty.unlocated(), Type::CBorrowMut(_)),
                target: self.intern_type(inner, capabilities, resolve_name)?,
            },
            Type::RawString => ResolvedType::RawString,
        };
        self.intern_resolved(resolved)
    }

    fn intern_many<R>(
        &mut self,
        types: &[Type],
        capabilities: &ResolvedTypeCapabilities,
        resolve_name: &mut R,
    ) -> Result<Vec<ResolvedTypeId>, ResolvedTypeError>
    where
        R: FnMut(&str) -> Option<ResolvedTypeName>,
    {
        types
            .iter()
            .map(|ty| self.intern_type(ty, capabilities, resolve_name))
            .collect()
    }

    fn intern_resolved(
        &mut self,
        resolved: ResolvedType,
    ) -> Result<ResolvedTypeId, ResolvedTypeError> {
        let canonical = resolved.canonical();
        let id = ResolvedTypeId::from_canonical(&canonical);
        if let Some(existing) = self.entries.get(&id) {
            if existing != &resolved || self.canonical.get(&id) != Some(&canonical) {
                return Err(ResolvedTypeError::FingerprintCollision(id));
            }
            return Ok(id);
        }
        self.canonical.insert(id.clone(), canonical);
        self.entries.insert(id.clone(), resolved);
        Ok(id)
    }
}

fn resolve_nominal<R>(name: &str, resolve_name: &mut R) -> Result<NominalTypeId, ResolvedTypeError>
where
    R: FnMut(&str) -> Option<ResolvedTypeName>,
{
    match resolve_name(name) {
        Some(ResolvedTypeName::Nominal(identity)) => Ok(identity),
        _ => Err(ResolvedTypeError::UnknownTypeName(name.to_string())),
    }
}

fn stable_hash(bytes: &[u8], offset: u64) -> u64 {
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes.iter().fold(offset, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

fn atom(output: &mut String, tag: &str, value: &str) {
    let _ = write!(output, "{}:{tag}:{}:{value};", tag.len(), value.len());
}

fn optional_atom(output: &mut String, value: Option<&str>) {
    match value {
        Some(value) => atom(output, "some", value),
        None => atom(output, "none", ""),
    }
}

fn id(output: &mut String, value: &ResolvedTypeId) {
    atom(output, "type-id", value.as_str());
}

fn ids(output: &mut String, values: &[ResolvedTypeId]) {
    let _ = write!(output, "{};", values.len());
    for value in values {
        id(output, value);
    }
}

fn unary(output: &mut String, tag: &str, inner: &ResolvedTypeId) {
    atom(output, tag, "");
    id(output, inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{AstNodeMeta, AstOrigin};

    fn nominal(name: &str) -> NominalTypeId {
        NominalTypeId::new(format!("item:type:test::{name}")).unwrap()
    }

    fn resolve(name: &str) -> Option<ResolvedTypeName> {
        ResolvedTypeName::primitive(name).or_else(|| Some(ResolvedTypeName::Nominal(nominal(name))))
    }

    fn zonked(ty: Type) -> ZonkedTy {
        ZonkedTy::from_resolved(ty).unwrap()
    }

    #[test]
    fn fingerprints_ignore_surface_metadata() {
        let plain = zonked(Type::Name("i32".into(), Vec::new()));
        let located = zonked(Type::Name("i32".into(), Vec::new()).with_meta(
            AstNodeMeta::synthetic(AstOrigin::RuntimeSystem("test.types")),
        ));
        let mut left = ResolvedTypeTable::new();
        let mut right = ResolvedTypeTable::new();
        let left_id = left
            .intern_zonked(&plain, &ResolvedTypeCapabilities::default(), resolve)
            .unwrap();
        let right_id = right
            .intern_zonked(&located, &ResolvedTypeCapabilities::default(), resolve)
            .unwrap();
        assert_eq!(left_id, right_id);
    }

    #[test]
    fn algebraic_builtin_spellings_share_one_canonical_shape() {
        let mut table = ResolvedTypeTable::new();
        let option_syntax = zonked(Type::Option(Box::new(Type::Name("i32".into(), Vec::new()))));
        let option_name = zonked(Type::Name(
            "Option".into(),
            vec![Type::Name("i32".into(), Vec::new())],
        ));
        let structural = table
            .intern_zonked(&option_syntax, &Default::default(), resolve)
            .unwrap();
        let named = table
            .intern_zonked(&option_name, &Default::default(), resolve)
            .unwrap();
        assert_eq!(structural, named);

        let result_syntax = zonked(Type::Result(
            Box::new(Type::Name("i32".into(), Vec::new())),
            Box::new(Type::Name("string".into(), Vec::new())),
        ));
        let result_name = zonked(Type::Name(
            "Result".into(),
            vec![
                Type::Name("i32".into(), Vec::new()),
                Type::Name("string".into(), Vec::new()),
            ],
        ));
        let structural = table
            .intern_zonked(&result_syntax, &Default::default(), resolve)
            .unwrap();
        let named = table
            .intern_zonked(&result_name, &Default::default(), resolve)
            .unwrap();
        assert_eq!(structural, named);
    }

    #[test]
    fn declaration_order_does_not_change_ids_or_iteration() {
        let list = zonked(Type::Name(
            "List".into(),
            vec![Type::Name("i64".into(), Vec::new())],
        ));
        let option = zonked(Type::Option(Box::new(Type::Name(
            "string".into(),
            Vec::new(),
        ))));
        let mut left = ResolvedTypeTable::new();
        let mut right = ResolvedTypeTable::new();
        let left_list = left
            .intern_zonked(&list, &Default::default(), resolve)
            .unwrap();
        let left_option = left
            .intern_zonked(&option, &Default::default(), resolve)
            .unwrap();
        let right_option = right
            .intern_zonked(&option, &Default::default(), resolve)
            .unwrap();
        let right_list = right
            .intern_zonked(&list, &Default::default(), resolve)
            .unwrap();
        assert_eq!(left_list, right_list);
        assert_eq!(left_option, right_option);
        assert_eq!(
            left.iter().collect::<Vec<_>>(),
            right.iter().collect::<Vec<_>>()
        );
    }

    #[test]
    fn canonical_table_deduplicates_recursive_shapes() {
        let ty = zonked(Type::Tuple(vec![
            Type::Name("i32".into(), Vec::new()),
            Type::Name("i32".into(), Vec::new()),
        ]));
        let mut table = ResolvedTypeTable::new();
        let tuple = table
            .intern_zonked(&ty, &Default::default(), resolve)
            .unwrap();
        assert_eq!(table.len(), 2);
        assert!(matches!(table.get(&tuple), Some(ResolvedType::Tuple(ids)) if ids[0] == ids[1]));
        assert!(table.validate().is_ok());
    }

    #[test]
    fn any_is_fail_closed_without_capability() {
        let any = zonked(Type::Name("Any".into(), Vec::new()));
        let mut table = ResolvedTypeTable::new();
        assert_eq!(
            table.intern_zonked(&any, &Default::default(), resolve),
            Err(ResolvedTypeError::DynamicAnyRequiresCapability)
        );
        let capabilities =
            ResolvedTypeCapabilities::with_dynamic_any("type.dynamic_value").unwrap();
        let id = table.intern_zonked(&any, &capabilities, resolve).unwrap();
        assert!(matches!(
            table.get(&id),
            Some(ResolvedType::DynamicAny { capability }) if capability == "type.dynamic_value"
        ));
    }

    #[test]
    fn unresolved_and_uninstantiated_types_are_rejected() {
        let mut table = ResolvedTypeTable::new();
        let capabilities = ResolvedTypeCapabilities::default();
        for (ty, kind) in [
            (Type::Infer, "Infer"),
            (Type::TypeVar(7), "TypeVar"),
            (
                Type::ForAll(
                    vec!["T".into()],
                    Box::new(Type::Name("i32".into(), Vec::new())),
                ),
                "ForAll",
            ),
            (Type::Name("unknown".into(), Vec::new()), "Unknown"),
        ] {
            assert_eq!(
                table.intern_type(&ty, &capabilities, &mut resolve),
                Err(ResolvedTypeError::ResidualInference(kind))
            );
        }
    }

    #[test]
    fn unresolved_nominal_name_is_rejected() {
        let ty = zonked(Type::Name("Missing".into(), Vec::new()));
        let mut table = ResolvedTypeTable::new();
        assert_eq!(
            table.intern_zonked(&ty, &Default::default(), |_| None),
            Err(ResolvedTypeError::UnknownTypeName("Missing".into()))
        );
    }

    #[test]
    fn trait_intersection_is_order_independent() {
        let left = zonked(Type::DynTrait(vec!["Read".into(), "Send".into()]));
        let right = zonked(Type::DynTrait(vec!["Send".into(), "Read".into()]));
        let mut table = ResolvedTypeTable::new();
        let left = table
            .intern_zonked(&left, &Default::default(), resolve)
            .unwrap();
        let right = table
            .intern_zonked(&right, &Default::default(), resolve)
            .unwrap();
        assert_eq!(left, right);
    }

    #[test]
    fn validator_rejects_dangling_child_id() {
        let mut table = ResolvedTypeTable::new();
        let missing = ResolvedTypeId("rt:missing".into());
        let resolved = ResolvedType::Option(missing.clone());
        let canonical = resolved.canonical();
        let owner = ResolvedTypeId::from_canonical(&canonical);
        table.canonical.insert(owner.clone(), canonical);
        table.entries.insert(owner.clone(), resolved);
        let errors = table.validate().unwrap_err();
        assert!(errors.contains(&ResolvedTypeError::MissingReferencedType { owner, missing }));
    }
}
