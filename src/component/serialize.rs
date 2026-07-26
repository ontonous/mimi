//! `.mimiabi` serialization: Component IR ↔ JSON.
//!
//! The `.mimiabi` format is the serialized Component IR. It serves as:
//! 1. ABI contract between compiler versions (tamper detection via hash)
//! 2. Input for all bindgen backends (C, Rust, Node, Python, Go, Java, C++)
//! 3. ABI diff tool input (breaking change detection)
//!
//! Format: JSON (human-readable, tool-friendly). Future: optional binary
//! (bincode/flatbuffers) for performance-critical paths.

use serde::{Deserialize, Serialize};

use super::symbol::{AbiCallConv, AbiCallbackCategory, AbiParam, AbiSymbol, AbiSymbolKind};
use super::types::{
    AbiAlias, AbiEnum, AbiField, AbiOpaque, AbiPrimitive, AbiStruct, AbiTypeDef, AbiTypeRef,
};
use super::{ComponentIdentity, ComponentIr};

/// Serialized `.mimiabi` format (JSON-compatible).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MimiAbi {
    /// Format version (bumped on schema changes).
    pub format_version: u32,
    /// Component identity.
    pub identity: MimiAbiIdentity,
    /// Exported symbols.
    pub exports: Vec<MimiAbiSymbol>,
    /// Imported symbols.
    pub imports: Vec<MimiAbiSymbol>,
    /// Type definitions.
    pub types: Vec<MimiAbiType>,
}

impl MimiAbi {
    /// Current format version.
    pub const FORMAT_VERSION: u32 = 1;

    /// Serialize a ComponentIr to `.mimiabi` JSON.
    pub fn from_component_ir(ir: &ComponentIr) -> Self {
        Self {
            format_version: Self::FORMAT_VERSION,
            identity: MimiAbiIdentity::from(&ir.identity),
            exports: ir.exports.iter().map(MimiAbiSymbol::from).collect(),
            imports: ir.imports.iter().map(MimiAbiSymbol::from).collect(),
            types: ir.types.iter().map(MimiAbiType::from).collect(),
        }
    }

    /// Serialize to JSON string.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Deserialize from JSON string (no validation).
    ///
    /// **Security**: Use [`from_json_validated`](Self::from_json_validated)
    /// for untrusted input. This method does not check `format_version`
    /// or validate enum field values.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Deserialize from JSON string with full validation.
    ///
    /// Checks:
    /// 1. `format_version` matches [`FORMAT_VERSION`](Self::FORMAT_VERSION)
    /// 2. All primitive type names are recognized
    /// 3. All symbol kinds are recognized
    /// 4. All calling conventions are recognized
    /// 5. All callback categories are recognized
    ///
    /// Returns `Err(MimiAbiError)` on any validation failure.
    pub fn from_json_validated(json: &str) -> Result<Self, MimiAbiError> {
        let abi: Self =
            serde_json::from_str(json).map_err(|e| MimiAbiError::Json(e.to_string()))?;
        abi.validate_deserialized()?;
        Ok(abi)
    }

    /// Validate a deserialized `.mimiabi` for semantic correctness.
    ///
    /// This catches malformed inputs that serde accepts but represent
    /// invalid ABI data (e.g., unknown primitive names, bad format version).
    pub fn validate_deserialized(&self) -> Result<(), MimiAbiError> {
        if self.format_version != Self::FORMAT_VERSION {
            return Err(MimiAbiError::BadFormatVersion {
                expected: Self::FORMAT_VERSION,
                got: self.format_version,
            });
        }
        for sym in self.exports.iter().chain(self.imports.iter()) {
            validate_symbol(sym)?;
        }
        for ty in &self.types {
            validate_type(ty)?;
        }
        Ok(())
    }

    /// Compute BLAKE3 hash of the serialized JSON (for tamper detection).
    pub fn hash(&self) -> Result<String, serde_json::Error> {
        let json = serde_json::to_string(self)?;
        Ok(blake3::hash(json.as_bytes()).to_hex().to_string())
    }

    /// Reconstruct a ComponentIr from the serialized `.mimiabi` format.
    ///
    /// This is the reverse of `from_component_ir()`. Used by bindgen backends
    /// that consume `.mimiabi` files directly.
    pub fn to_component_ir(&self) -> ComponentIr {
        ComponentIr {
            identity: ComponentIdentity {
                name: self.identity.name.clone(),
                version: self.identity.version.clone(),
                abi_version: self.identity.abi_version,
            },
            exports: self.exports.iter().map(AbiSymbol::from).collect(),
            imports: self.imports.iter().map(AbiSymbol::from).collect(),
            types: self.types.iter().map(AbiTypeDef::from).collect(),
        }
    }
}

/// Validation error for `.mimiabi` deserialization.
#[derive(Debug, Clone, PartialEq)]
pub enum MimiAbiError {
    /// JSON parse error (stored as string since serde_json::Error is not Clone/PartialEq).
    Json(String),
    /// format_version does not match the expected version.
    BadFormatVersion { expected: u32, got: u32 },
    /// Unknown primitive type name in a type reference.
    UnknownPrimitive(String),
    /// Unknown symbol kind.
    UnknownSymbolKind(String),
    /// Unknown calling convention.
    UnknownCallConv(String),
    /// Unknown callback category.
    UnknownCallbackCategory(String),
}

impl std::fmt::Display for MimiAbiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MimiAbiError::Json(e) => write!(f, "JSON parse error: {}", e),
            MimiAbiError::BadFormatVersion { expected, got } => {
                write!(
                    f,
                    "format_version mismatch: expected {}, got {}",
                    expected, got
                )
            }
            MimiAbiError::UnknownPrimitive(name) => {
                write!(f, "unknown primitive type: {:?}", name)
            }
            MimiAbiError::UnknownSymbolKind(name) => {
                write!(f, "unknown symbol kind: {:?}", name)
            }
            MimiAbiError::UnknownCallConv(name) => {
                write!(f, "unknown calling convention: {:?}", name)
            }
            MimiAbiError::UnknownCallbackCategory(name) => {
                write!(f, "unknown callback category: {:?}", name)
            }
        }
    }
}

impl std::error::Error for MimiAbiError {}

/// Validate a serialized symbol's enum fields.
fn validate_symbol(sym: &MimiAbiSymbol) -> Result<(), MimiAbiError> {
    if try_parse_symbol_kind(&sym.kind).is_none() {
        return Err(MimiAbiError::UnknownSymbolKind(sym.kind.clone()));
    }
    if try_parse_call_conv(&sym.call_conv).is_none() {
        return Err(MimiAbiError::UnknownCallConv(sym.call_conv.clone()));
    }
    if let Some(ref cat) = sym.callback_category {
        if try_parse_callback_category(cat).is_none() {
            return Err(MimiAbiError::UnknownCallbackCategory(cat.clone()));
        }
    }
    validate_type_ref(&sym.ret)?;
    for param in &sym.params {
        validate_type_ref(&param.ty)?;
    }
    Ok(())
}

/// Validate a serialized type definition's enum fields.
fn validate_type(ty: &MimiAbiType) -> Result<(), MimiAbiError> {
    match ty {
        MimiAbiType::Struct { fields, .. } => {
            for field in fields {
                validate_type_ref(&field.ty)?;
            }
        }
        MimiAbiType::Enum { repr, .. } => {
            if try_parse_primitive(repr).is_none() {
                return Err(MimiAbiError::UnknownPrimitive(repr.clone()));
            }
        }
        MimiAbiType::Alias { target, .. } => {
            validate_type_ref(target)?;
        }
        MimiAbiType::Opaque { .. } => {}
    }
    Ok(())
}

/// Recursively validate a serialized type reference.
fn validate_type_ref(ty: &MimiAbiTypeRef) -> Result<(), MimiAbiError> {
    match ty {
        MimiAbiTypeRef::Primitive(name) => {
            if try_parse_primitive(name).is_none() {
                return Err(MimiAbiError::UnknownPrimitive(name.clone()));
            }
        }
        MimiAbiTypeRef::Pointer(inner) | MimiAbiTypeRef::Slice(inner) => {
            validate_type_ref(inner)?;
        }
        MimiAbiTypeRef::FatPointer { element, .. } => {
            validate_type_ref(element)?;
        }
        MimiAbiTypeRef::Named(_) | MimiAbiTypeRef::Opaque(_) | MimiAbiTypeRef::Void => {}
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MimiAbiIdentity {
    pub name: String,
    pub version: String,
    pub abi_version: u32,
}

impl From<&ComponentIdentity> for MimiAbiIdentity {
    fn from(id: &ComponentIdentity) -> Self {
        Self {
            name: id.name.clone(),
            version: id.version.clone(),
            abi_version: id.abi_version,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MimiAbiSymbol {
    pub name: String,
    pub kind: String,
    pub params: Vec<MimiAbiParam>,
    pub ret: MimiAbiTypeRef,
    pub effects: Vec<String>,
    pub is_unsafe: bool,
    pub call_conv: String,
    /// 0.31.33: Callback category (null for non-callbacks).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_category: Option<String>,
}

impl From<&AbiSymbol> for MimiAbiSymbol {
    fn from(sym: &AbiSymbol) -> Self {
        Self {
            name: sym.name.clone(),
            kind: format!("{:?}", sym.kind),
            params: sym.params.iter().map(MimiAbiParam::from).collect(),
            ret: MimiAbiTypeRef::from(&sym.ret),
            effects: sym.effects.clone(),
            is_unsafe: sym.is_unsafe,
            call_conv: format!("{:?}", sym.call_conv),
            callback_category: sym.callback_category.map(|c| format!("{:?}", c)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MimiAbiParam {
    pub name: String,
    pub ty: MimiAbiTypeRef,
    pub is_nullable: bool,
}

impl From<&AbiParam> for MimiAbiParam {
    fn from(p: &AbiParam) -> Self {
        Self {
            name: p.name.clone(),
            ty: MimiAbiTypeRef::from(&p.ty),
            is_nullable: p.is_nullable,
        }
    }
}

/// Serialized type reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
pub enum MimiAbiTypeRef {
    Primitive(String),
    Named(String),
    Pointer(Box<MimiAbiTypeRef>),
    Slice(Box<MimiAbiTypeRef>),
    Opaque(String),
    FatPointer {
        element: Box<MimiAbiTypeRef>,
        has_capacity: bool,
    },
    Void,
}

impl From<&AbiTypeRef> for MimiAbiTypeRef {
    fn from(ty: &AbiTypeRef) -> Self {
        match ty {
            AbiTypeRef::Primitive(p) => MimiAbiTypeRef::Primitive(format!("{:?}", p)),
            AbiTypeRef::Named(name) => MimiAbiTypeRef::Named(name.clone()),
            AbiTypeRef::Pointer(inner) => {
                MimiAbiTypeRef::Pointer(Box::new(MimiAbiTypeRef::from(inner.as_ref())))
            }
            AbiTypeRef::Slice(inner) => {
                MimiAbiTypeRef::Slice(Box::new(MimiAbiTypeRef::from(inner.as_ref())))
            }
            AbiTypeRef::Opaque(name) => MimiAbiTypeRef::Opaque(name.clone()),
            AbiTypeRef::FatPointer {
                element,
                has_capacity,
            } => MimiAbiTypeRef::FatPointer {
                element: Box::new(MimiAbiTypeRef::from(element.as_ref())),
                has_capacity: *has_capacity,
            },
            AbiTypeRef::Void => MimiAbiTypeRef::Void,
        }
    }
}

/// Serialized type definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum MimiAbiType {
    Struct {
        name: String,
        fields: Vec<MimiAbiField>,
        size: Option<usize>,
        align: Option<usize>,
    },
    Enum {
        name: String,
        variants: Vec<(String, i64)>,
        repr: String,
    },
    Alias {
        name: String,
        target: MimiAbiTypeRef,
    },
    Opaque {
        name: String,
        description: String,
    },
}

impl From<&AbiTypeDef> for MimiAbiType {
    fn from(def: &AbiTypeDef) -> Self {
        match def {
            AbiTypeDef::Struct(s) => MimiAbiType::Struct {
                name: s.name.clone(),
                fields: s.fields.iter().map(MimiAbiField::from).collect(),
                size: s.size,
                align: s.align,
            },
            AbiTypeDef::Enum(e) => MimiAbiType::Enum {
                name: e.name.clone(),
                variants: e.variants.clone(),
                repr: format!("{:?}", e.repr),
            },
            AbiTypeDef::Alias(a) => MimiAbiType::Alias {
                name: a.name.clone(),
                target: MimiAbiTypeRef::from(&a.target),
            },
            AbiTypeDef::Opaque(o) => MimiAbiType::Opaque {
                name: o.name.clone(),
                description: o.description.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MimiAbiField {
    pub name: String,
    pub ty: MimiAbiTypeRef,
    pub offset: Option<usize>,
}

impl From<&AbiField> for MimiAbiField {
    fn from(f: &AbiField) -> Self {
        Self {
            name: f.name.clone(),
            ty: MimiAbiTypeRef::from(&f.ty),
            offset: f.offset,
        }
    }
}

// ── Reverse conversions (MimiAbi → Component IR) ──────────────────────────

impl From<&MimiAbiSymbol> for AbiSymbol {
    fn from(sym: &MimiAbiSymbol) -> Self {
        Self {
            name: sym.name.clone(),
            kind: parse_symbol_kind(&sym.kind),
            params: sym.params.iter().map(AbiParam::from).collect(),
            ret: AbiTypeRef::from(&sym.ret),
            effects: sym.effects.clone(),
            is_unsafe: sym.is_unsafe,
            call_conv: parse_call_conv(&sym.call_conv),
            callback_category: sym
                .callback_category
                .as_deref()
                .map(parse_callback_category),
        }
    }
}

impl From<&MimiAbiParam> for AbiParam {
    fn from(p: &MimiAbiParam) -> Self {
        Self {
            name: p.name.clone(),
            ty: AbiTypeRef::from(&p.ty),
            is_nullable: p.is_nullable,
        }
    }
}

impl From<&MimiAbiTypeRef> for AbiTypeRef {
    fn from(ty: &MimiAbiTypeRef) -> Self {
        match ty {
            MimiAbiTypeRef::Primitive(name) => AbiTypeRef::Primitive(parse_primitive(name)),
            MimiAbiTypeRef::Named(name) => AbiTypeRef::Named(name.clone()),
            MimiAbiTypeRef::Pointer(inner) => {
                AbiTypeRef::Pointer(Box::new(AbiTypeRef::from(inner.as_ref())))
            }
            MimiAbiTypeRef::Slice(inner) => {
                AbiTypeRef::Slice(Box::new(AbiTypeRef::from(inner.as_ref())))
            }
            MimiAbiTypeRef::Opaque(name) => AbiTypeRef::Opaque(name.clone()),
            MimiAbiTypeRef::FatPointer {
                element,
                has_capacity,
            } => AbiTypeRef::FatPointer {
                element: Box::new(AbiTypeRef::from(element.as_ref())),
                has_capacity: *has_capacity,
            },
            MimiAbiTypeRef::Void => AbiTypeRef::Void,
        }
    }
}

impl From<&MimiAbiType> for AbiTypeDef {
    fn from(ty: &MimiAbiType) -> Self {
        match ty {
            MimiAbiType::Struct {
                name,
                fields,
                size,
                align,
            } => AbiTypeDef::Struct(AbiStruct {
                name: name.clone(),
                fields: fields.iter().map(AbiField::from).collect(),
                is_repr_c: true,
                size: *size,
                align: *align,
            }),
            MimiAbiType::Enum {
                name,
                variants,
                repr,
            } => AbiTypeDef::Enum(AbiEnum {
                name: name.clone(),
                variants: variants.clone(),
                repr: parse_primitive(repr),
            }),
            MimiAbiType::Alias { name, target } => AbiTypeDef::Alias(AbiAlias {
                name: name.clone(),
                target: AbiTypeRef::from(target),
            }),
            MimiAbiType::Opaque { name, description } => AbiTypeDef::Opaque(AbiOpaque {
                name: name.clone(),
                description: description.clone(),
            }),
        }
    }
}

impl From<&MimiAbiField> for AbiField {
    fn from(f: &MimiAbiField) -> Self {
        Self {
            name: f.name.clone(),
            ty: AbiTypeRef::from(&f.ty),
            offset: f.offset,
        }
    }
}

// ── Parse helpers (Debug format → enum) ────────────────────────────────────

/// Try to parse a primitive type name. Returns `None` for unknown names.
fn try_parse_primitive(name: &str) -> Option<AbiPrimitive> {
    match name {
        "I8" => Some(AbiPrimitive::I8),
        "I16" => Some(AbiPrimitive::I16),
        "I32" => Some(AbiPrimitive::I32),
        "I64" => Some(AbiPrimitive::I64),
        "U8" => Some(AbiPrimitive::U8),
        "U16" => Some(AbiPrimitive::U16),
        "U32" => Some(AbiPrimitive::U32),
        "U64" => Some(AbiPrimitive::U64),
        "F32" => Some(AbiPrimitive::F32),
        "F64" => Some(AbiPrimitive::F64),
        "Bool" => Some(AbiPrimitive::Bool),
        "IntPtr" => Some(AbiPrimitive::IntPtr),
        "UIntPtr" => Some(AbiPrimitive::UIntPtr),
        _ => None,
    }
}

/// Try to parse a symbol kind. Returns `None` for unknown kinds.
fn try_parse_symbol_kind(name: &str) -> Option<AbiSymbolKind> {
    match name {
        "Function" => Some(AbiSymbolKind::Function),
        "ExternFunction" => Some(AbiSymbolKind::ExternFunction),
        "Method" => Some(AbiSymbolKind::Method),
        "Constructor" => Some(AbiSymbolKind::Constructor),
        "Destructor" => Some(AbiSymbolKind::Destructor),
        "Callback" => Some(AbiSymbolKind::Callback),
        _ => None,
    }
}

/// Try to parse a calling convention. Returns `None` for unknown conventions.
fn try_parse_call_conv(name: &str) -> Option<AbiCallConv> {
    match name {
        "C" => Some(AbiCallConv::C),
        "SystemV" => Some(AbiCallConv::SystemV),
        "Win64" => Some(AbiCallConv::Win64),
        "Fast" => Some(AbiCallConv::Fast),
        "MimiInternal" => Some(AbiCallConv::MimiInternal),
        _ => None,
    }
}

/// Try to parse a callback category. Returns `None` for unknown categories.
fn try_parse_callback_category(name: &str) -> Option<AbiCallbackCategory> {
    match name {
        "SyncSameThread" => Some(AbiCallbackCategory::SyncSameThread),
        "SyncCrossThread" => Some(AbiCallbackCategory::SyncCrossThread),
        "AsyncOneShot" => Some(AbiCallbackCategory::AsyncOneShot),
        "AsyncMultiShot" => Some(AbiCallbackCategory::AsyncMultiShot),
        "AsyncSubscription" => Some(AbiCallbackCategory::AsyncSubscription),
        _ => None,
    }
}

/// Parse a primitive type name with fallback to I64.
///
/// **Security**: For untrusted input, use [`MimiAbi::from_json_validated`]
/// which rejects unknown primitives instead of silently falling back.
fn parse_primitive(name: &str) -> AbiPrimitive {
    try_parse_primitive(name).unwrap_or(AbiPrimitive::I64)
}

fn parse_symbol_kind(name: &str) -> AbiSymbolKind {
    try_parse_symbol_kind(name).unwrap_or(AbiSymbolKind::Function)
}

fn parse_call_conv(name: &str) -> AbiCallConv {
    try_parse_call_conv(name).unwrap_or(AbiCallConv::C)
}

fn parse_callback_category(name: &str) -> AbiCallbackCategory {
    try_parse_callback_category(name).unwrap_or(AbiCallbackCategory::SyncSameThread)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::gen::{register_core_runtime_abi, AbiGenerator};
    use crate::component::types::AbiPrimitive;

    #[test]
    fn serialize_deserialize_roundtrip() {
        let mut gen = AbiGenerator::new();
        register_core_runtime_abi(&mut gen);
        let ir = gen.build();

        let abi = MimiAbi::from_component_ir(&ir);
        let json = abi.to_json().expect("serialize");
        let abi2 = MimiAbi::from_json(&json).expect("deserialize");

        assert_eq!(abi.format_version, abi2.format_version);
        assert_eq!(abi.identity.name, abi2.identity.name);
        assert_eq!(abi.exports.len(), abi2.exports.len());
        assert_eq!(abi.exports[0].name, abi2.exports[0].name);
    }

    #[test]
    fn hash_is_deterministic() {
        let mut gen = AbiGenerator::new();
        register_core_runtime_abi(&mut gen);
        let ir = gen.build();

        let abi = MimiAbi::from_component_ir(&ir);
        let h1 = abi.hash().expect("hash");
        let h2 = abi.hash().expect("hash");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // BLAKE3 hex
    }

    #[test]
    fn json_is_human_readable() {
        let mut gen = AbiGenerator::new();
        gen.export("mimi_test_fn", |f| {
            f.param("x", crate::component::gen::prim(AbiPrimitive::I32))
                .returns(crate::component::gen::prim(AbiPrimitive::I64))
        });
        let ir = gen.build();
        let abi = MimiAbi::from_component_ir(&ir);
        let json = abi.to_json().expect("serialize");

        // Should contain the function name
        assert!(json.contains("mimi_test_fn"));
        // Should be valid JSON
        let _: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    }

    #[test]
    fn component_ir_roundtrip_via_mimiabi() {
        let mut gen = AbiGenerator::new();
        register_core_runtime_abi(&mut gen);
        let ir1 = gen.build();

        // ComponentIr → MimiAbi → JSON → MimiAbi → ComponentIr
        let abi = MimiAbi::from_component_ir(&ir1);
        let json = abi.to_json().expect("serialize");
        let abi2 = MimiAbi::from_json(&json).expect("deserialize");
        let ir2 = abi2.to_component_ir();

        // Structural equality
        assert_eq!(ir1.identity, ir2.identity);
        assert_eq!(ir1.exports.len(), ir2.exports.len());
        assert_eq!(ir1.imports.len(), ir2.imports.len());
        assert_eq!(ir1.types.len(), ir2.types.len());

        // Spot-check a symbol
        let sym1 = ir1.export("mimi_list_push_i64").expect("should exist");
        let sym2 = ir2.export("mimi_list_push_i64").expect("should exist");
        assert_eq!(sym1, sym2);

        // Spot-check a type
        let ty1 = ir1.type_def("MimiString").expect("should exist");
        let ty2 = ir2.type_def("MimiString").expect("should exist");
        assert_eq!(ty1, ty2);
    }

    #[test]
    fn reverse_conversion_preserves_callback_category() {
        let mut gen = AbiGenerator::new();
        gen.export("mimi_on_event", |f| {
            f.param("data", crate::component::gen::prim(AbiPrimitive::I64))
                .callback(crate::component::AbiCallbackCategory::AsyncMultiShot)
        });
        let ir1 = gen.build();

        let abi = MimiAbi::from_component_ir(&ir1);
        let json = abi.to_json().expect("serialize");
        let abi2 = MimiAbi::from_json(&json).expect("deserialize");
        let ir2 = abi2.to_component_ir();

        let sym = ir2.export("mimi_on_event").expect("should exist");
        assert_eq!(sym.kind, crate::component::AbiSymbolKind::Callback);
        assert_eq!(
            sym.callback_category,
            Some(crate::component::AbiCallbackCategory::AsyncMultiShot)
        );
    }

    // ── Attack tests (0.31.37) ──

    #[test]
    fn validated_rejects_bad_format_version() {
        let json = r#"{
            "format_version": 999,
            "identity": { "name": "t", "version": "0", "abi_version": 1 },
            "exports": [], "imports": [], "types": []
        }"#;
        let err = MimiAbi::from_json_validated(json).unwrap_err();
        assert!(matches!(
            err,
            MimiAbiError::BadFormatVersion {
                expected: 1,
                got: 999
            }
        ));
    }

    #[test]
    fn validated_rejects_unknown_primitive() {
        let json = r#"{
            "format_version": 1,
            "identity": { "name": "t", "version": "0", "abi_version": 1 },
            "exports": [{
                "name": "mimi_evil",
                "kind": "Function",
                "params": [],
                "ret": {"kind": "Primitive", "value": "NotAType"},
                "effects": [],
                "is_unsafe": false,
                "call_conv": "C"
            }],
            "imports": [], "types": []
        }"#;
        let err = MimiAbi::from_json_validated(json).unwrap_err();
        assert!(matches!(err, MimiAbiError::UnknownPrimitive(n) if n == "NotAType"));
    }

    #[test]
    fn validated_rejects_unknown_symbol_kind() {
        let json = r#"{
            "format_version": 1,
            "identity": { "name": "t", "version": "0", "abi_version": 1 },
            "exports": [{
                "name": "mimi_evil",
                "kind": "NotAKind",
                "params": [],
                "ret": {"kind": "Void"},
                "effects": [],
                "is_unsafe": false,
                "call_conv": "C"
            }],
            "imports": [], "types": []
        }"#;
        let err = MimiAbi::from_json_validated(json).unwrap_err();
        assert!(matches!(err, MimiAbiError::UnknownSymbolKind(n) if n == "NotAKind"));
    }

    #[test]
    fn validated_rejects_unknown_call_conv() {
        let json = r#"{
            "format_version": 1,
            "identity": { "name": "t", "version": "0", "abi_version": 1 },
            "exports": [{
                "name": "mimi_evil",
                "kind": "Function",
                "params": [],
                "ret": {"kind": "Void"},
                "effects": [],
                "is_unsafe": false,
                "call_conv": "StdCall"
            }],
            "imports": [], "types": []
        }"#;
        let err = MimiAbi::from_json_validated(json).unwrap_err();
        assert!(matches!(err, MimiAbiError::UnknownCallConv(n) if n == "StdCall"));
    }

    #[test]
    fn validated_rejects_unknown_callback_category() {
        let json = r#"{
            "format_version": 1,
            "identity": { "name": "t", "version": "0", "abi_version": 1 },
            "exports": [{
                "name": "mimi_evil",
                "kind": "Callback",
                "params": [],
                "ret": {"kind": "Void"},
                "effects": [],
                "is_unsafe": false,
                "call_conv": "C",
                "callback_category": "AsyncForever"
            }],
            "imports": [], "types": []
        }"#;
        let err = MimiAbi::from_json_validated(json).unwrap_err();
        assert!(matches!(err, MimiAbiError::UnknownCallbackCategory(n) if n == "AsyncForever"));
    }

    #[test]
    fn validated_rejects_unknown_primitive_in_type_def() {
        let json = r#"{
            "format_version": 1,
            "identity": { "name": "t", "version": "0", "abi_version": 1 },
            "exports": [], "imports": [],
            "types": [{
                "kind": "Enum",
                "name": "BadEnum",
                "variants": [["A", 0]],
                "repr": "NotAPrimitive"
            }]
        }"#;
        let err = MimiAbi::from_json_validated(json).unwrap_err();
        assert!(matches!(err, MimiAbiError::UnknownPrimitive(n) if n == "NotAPrimitive"));
    }

    #[test]
    fn validated_accepts_clean_abi() {
        let mut gen = AbiGenerator::new();
        register_core_runtime_abi(&mut gen);
        let ir = gen.build();
        let abi = MimiAbi::from_component_ir(&ir);
        let json = abi.to_json().expect("serialize");
        // from_json_validated should accept our own output
        let abi2 = MimiAbi::from_json_validated(&json).expect("validated deserialize");
        assert_eq!(abi.exports.len(), abi2.exports.len());
    }

    #[test]
    fn unvalidated_from_json_still_works_for_backward_compat() {
        // from_json (unvalidated) should still accept unknown primitives
        // with fallback behavior (backward compat)
        let json = r#"{
            "format_version": 1,
            "identity": { "name": "t", "version": "0", "abi_version": 1 },
            "exports": [{
                "name": "mimi_legacy",
                "kind": "Function",
                "params": [],
                "ret": {"kind": "Primitive", "value": "UnknownPrim"},
                "effects": [],
                "is_unsafe": false,
                "call_conv": "C"
            }],
            "imports": [], "types": []
        }"#;
        // Unvalidated: accepts with fallback
        let abi = MimiAbi::from_json(json).expect("unvalidated deserialize");
        let ir = abi.to_component_ir();
        let sym = ir.export("mimi_legacy").expect("should exist");
        // Fallback: unknown primitive → I64
        assert_eq!(
            sym.ret,
            crate::component::AbiTypeRef::Primitive(AbiPrimitive::I64)
        );
    }
}
