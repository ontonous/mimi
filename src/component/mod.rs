//! Component IR — the single source of truth for all component bindings.
//!
//! 0.31.30 (COMPONENT-IR-001): All stable component bindings are generated
//! from one Component IR. This module defines the in-memory representation
//! and the ABI generator that scans runtime exports to produce it.
//!
//! Pipeline (spec §7.3):
//! ```text
//! Typed Mimi IR → Component IR / .mimiabi → {C header, Rust -sys, Rust safe,
//!     Node addon, TS decls, Python/Java/Swift adapters, ABI checker, docs,
//!     conformance tests}
//! ```
//!
//! # Architecture
//!
//! The Component IR sits between the compiler's type-checked output and the
//! language-specific binding generators. It replaces the current pattern of
//! 7 bindgen backends independently consuming raw AST `ExternFunc` nodes.
//!
//! ```text
//! CheckedProgram ──→ ComponentIr ──→ c_header.rs
//!       │                  ├──→ rust_bind.rs
//!       │                  ├──→ node_bind.rs
//!       │                  ├──→ py_bind.rs
//!       │                  ├──→ go_bind.rs
//!       │                  ├──→ jni_bind.rs
//!       │                  └──→ cpp_bind.rs
//!       │
//!       └──→ runtime exports ──→ AbiGenerator ──→ ComponentIr.exports
//! ```

mod checkpoint;
mod gen;
mod handle;
mod serialize;
mod symbol;
mod types;
mod wire;

pub use checkpoint::{
    probe_layout, struct_type_count, AllocFault, AllocLedger, AllocSide, LayoutFault,
};
pub use gen::{register_core_runtime_abi, AbiGenerator};
pub use handle::{Handle, HandleError, HandleKind, HandleRegistry, RuntimeId};
pub use serialize::MimiAbi;
pub use symbol::*;
pub use types::*;
pub use wire::*;

/// Component IR: the single source of truth for all component bindings.
///
/// Generated from CheckedProgram + runtime exports, consumed by all
/// bindgen backends. Replaces the current pattern of 7 independent
/// raw-AST consumers.
#[derive(Debug, Clone)]
pub struct ComponentIr {
    /// Component identity (name, version, ABI version).
    pub identity: ComponentIdentity,
    /// Exported symbols (runtime functions available to Mimi programs).
    pub exports: Vec<AbiSymbol>,
    /// Imported symbols (extern "C" declarations from user code).
    pub imports: Vec<AbiSymbol>,
    /// Type definitions (repr(C) structs, enums, opaque handles).
    pub types: Vec<AbiTypeDef>,
}

impl ComponentIr {
    /// Look up an exported symbol by name.
    pub fn export(&self, name: &str) -> Option<&AbiSymbol> {
        self.exports.iter().find(|s| s.name == name)
    }

    /// Look up an imported symbol by name.
    pub fn import(&self, name: &str) -> Option<&AbiSymbol> {
        self.imports.iter().find(|s| s.name == name)
    }

    /// Look up a type definition by name.
    pub fn type_def(&self, name: &str) -> Option<&AbiTypeDef> {
        self.types.iter().find(|t| t.name() == name)
    }

    /// All symbol names (exports + imports), sorted.
    pub fn all_symbol_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .exports
            .iter()
            .chain(self.imports.iter())
            .map(|s| s.name.as_str())
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

/// Component identity: name, semver, ABI version.
#[derive(Debug, Clone)]
pub struct ComponentIdentity {
    /// Component name (e.g., "mimi-runtime").
    pub name: String,
    /// Semantic version (e.g., "0.1.1").
    pub version: String,
    /// ABI version (bumped on any ABI-breaking change).
    pub abi_version: u32,
}

impl Default for ComponentIdentity {
    fn default() -> Self {
        Self {
            name: "mimi-runtime".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            abi_version: 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_ir_lookup() {
        let ir = ComponentIr {
            identity: ComponentIdentity::default(),
            exports: vec![AbiSymbol {
                name: "mimi_list_push_i64".to_string(),
                kind: AbiSymbolKind::Function,
                params: vec![
                    AbiParam {
                        name: "list".to_string(),
                        ty: AbiTypeRef::Primitive(AbiPrimitive::IntPtr),
                        is_nullable: false,
                    },
                    AbiParam {
                        name: "value".to_string(),
                        ty: AbiTypeRef::Primitive(AbiPrimitive::I64),
                        is_nullable: false,
                    },
                ],
                ret: AbiTypeRef::Void,
                effects: vec![],
                is_unsafe: false,
                call_conv: AbiCallConv::C,
                callback_category: None,
            }],
            imports: vec![],
            types: vec![],
        };

        assert!(ir.export("mimi_list_push_i64").is_some());
        assert!(ir.export("nonexistent").is_none());
        assert_eq!(ir.all_symbol_names(), vec!["mimi_list_push_i64"]);
    }

    #[test]
    fn component_identity_default() {
        let id = ComponentIdentity::default();
        assert_eq!(id.name, "mimi-runtime");
        assert_eq!(id.abi_version, 1);
        assert!(!id.version.is_empty());
    }
}
