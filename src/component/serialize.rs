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

use super::symbol::AbiParam;
use super::symbol::AbiSymbol;
use super::types::AbiField;
use super::types::AbiTypeDef;
use super::types::AbiTypeRef;
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

    /// Deserialize from JSON string.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Compute BLAKE3 hash of the serialized JSON (for tamper detection).
    pub fn hash(&self) -> Result<String, serde_json::Error> {
        let json = serde_json::to_string(self)?;
        Ok(blake3::hash(json.as_bytes()).to_hex().to_string())
    }
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
}
