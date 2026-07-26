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
#[derive(Debug, Clone, PartialEq)]
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

    /// Validate internal consistency of the Component IR.
    ///
    /// Checks:
    /// 1. No duplicate export names
    /// 2. No duplicate import names
    /// 3. No export/import name conflicts
    /// 4. No duplicate type definition names
    /// 5. All Named type references resolve to a type definition
    /// 6. All Opaque type references resolve to a type definition
    ///
    /// Returns a list of validation errors (empty = consistent).
    pub fn validate(&self) -> Vec<ComponentIrError> {
        let mut errors = Vec::new();

        // 1. Duplicate exports
        let mut seen_exports = std::collections::HashSet::new();
        for sym in &self.exports {
            if !seen_exports.insert(sym.name.as_str()) {
                errors.push(ComponentIrError::DuplicateExport(sym.name.clone()));
            }
        }

        // 2. Duplicate imports
        let mut seen_imports = std::collections::HashSet::new();
        for sym in &self.imports {
            if !seen_imports.insert(sym.name.as_str()) {
                errors.push(ComponentIrError::DuplicateImport(sym.name.clone()));
            }
        }

        // 3. Export/import conflicts
        for name in &seen_exports {
            if seen_imports.contains(name) {
                errors.push(ComponentIrError::ExportImportConflict(name.to_string()));
            }
        }

        // 4. Duplicate type definitions
        let mut seen_types = std::collections::HashSet::new();
        for ty in &self.types {
            if !seen_types.insert(ty.name()) {
                errors.push(ComponentIrError::DuplicateType(ty.name().to_string()));
            }
        }

        // 5+6. Unresolved type references
        for sym in self.exports.iter().chain(self.imports.iter()) {
            Self::check_type_refs(&sym.ret, &seen_types, &sym.name, &mut errors);
            for param in &sym.params {
                Self::check_type_refs(&param.ty, &seen_types, &sym.name, &mut errors);
            }
        }

        errors
    }

    /// Recursively check that Named/Opaque type references resolve.
    fn check_type_refs(
        ty: &AbiTypeRef,
        known_types: &std::collections::HashSet<&str>,
        context: &str,
        errors: &mut Vec<ComponentIrError>,
    ) {
        match ty {
            AbiTypeRef::Named(name) => {
                if !known_types.contains(name.as_str()) {
                    errors.push(ComponentIrError::UnresolvedTypeRef {
                        name: name.clone(),
                        context: context.to_string(),
                    });
                }
            }
            AbiTypeRef::Opaque(name) => {
                if !known_types.contains(name.as_str()) {
                    errors.push(ComponentIrError::UnresolvedTypeRef {
                        name: name.clone(),
                        context: context.to_string(),
                    });
                }
            }
            AbiTypeRef::Pointer(inner) | AbiTypeRef::Slice(inner) => {
                Self::check_type_refs(inner, known_types, context, errors);
            }
            AbiTypeRef::FatPointer { element, .. } => {
                Self::check_type_refs(element, known_types, context, errors);
            }
            _ => {}
        }
    }
}

/// Validation error for Component IR consistency checks.
#[derive(Debug, Clone, PartialEq)]
pub enum ComponentIrError {
    /// Duplicate export symbol name.
    DuplicateExport(String),
    /// Duplicate import symbol name.
    DuplicateImport(String),
    /// Name appears in both exports and imports.
    ExportImportConflict(String),
    /// Duplicate type definition name.
    DuplicateType(String),
    /// Named/Opaque type reference does not resolve to a type definition.
    UnresolvedTypeRef { name: String, context: String },
}

/// Component identity: name, semver, ABI version.
#[derive(Debug, Clone, PartialEq)]
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

    #[test]
    fn validate_clean_ir() {
        let mut gen = crate::component::AbiGenerator::new();
        crate::component::register_core_runtime_abi(&mut gen);
        let ir = gen.build();
        let errors = ir.validate();
        assert!(errors.is_empty(), "validation errors: {:?}", errors);
    }

    #[test]
    fn validate_catches_duplicate_exports() {
        let ir = ComponentIr {
            identity: ComponentIdentity::default(),
            exports: vec![
                AbiSymbol {
                    name: "mimi_dup".to_string(),
                    kind: AbiSymbolKind::Function,
                    params: vec![],
                    ret: AbiTypeRef::Void,
                    effects: vec![],
                    is_unsafe: false,
                    call_conv: AbiCallConv::C,
                    callback_category: None,
                },
                AbiSymbol {
                    name: "mimi_dup".to_string(),
                    kind: AbiSymbolKind::Function,
                    params: vec![],
                    ret: AbiTypeRef::Void,
                    effects: vec![],
                    is_unsafe: false,
                    call_conv: AbiCallConv::C,
                    callback_category: None,
                },
            ],
            imports: vec![],
            types: vec![],
        };
        let errors = ir.validate();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ComponentIrError::DuplicateExport(n) if n == "mimi_dup")));
    }

    #[test]
    fn validate_catches_export_import_conflict() {
        let sym = AbiSymbol {
            name: "mimi_conflict".to_string(),
            kind: AbiSymbolKind::Function,
            params: vec![],
            ret: AbiTypeRef::Void,
            effects: vec![],
            is_unsafe: false,
            call_conv: AbiCallConv::C,
            callback_category: None,
        };
        let ir = ComponentIr {
            identity: ComponentIdentity::default(),
            exports: vec![sym.clone()],
            imports: vec![sym],
            types: vec![],
        };
        let errors = ir.validate();
        assert!(errors.iter().any(
            |e| matches!(e, ComponentIrError::ExportImportConflict(n) if n == "mimi_conflict")
        ));
    }

    #[test]
    fn validate_catches_unresolved_type_ref() {
        let ir = ComponentIr {
            identity: ComponentIdentity::default(),
            exports: vec![AbiSymbol {
                name: "mimi_uses_missing".to_string(),
                kind: AbiSymbolKind::Function,
                params: vec![],
                ret: AbiTypeRef::Named("MissingType".to_string()),
                effects: vec![],
                is_unsafe: false,
                call_conv: AbiCallConv::C,
                callback_category: None,
            }],
            imports: vec![],
            types: vec![],
        };
        let errors = ir.validate();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ComponentIrError::UnresolvedTypeRef { name, .. } if name == "MissingType")));
    }
}
