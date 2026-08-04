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

mod c_header;
mod checkpoint;
mod conformance;
mod diff;
mod gen;
mod handle;
mod rust_bind;
mod serialize;
mod symbol;
mod types;
mod wire;

pub use c_header::generate_c_header;
pub use checkpoint::{
    probe_layout, struct_type_count, AllocFault, AllocLedger, AllocSide, LayoutFault,
};
pub use diff::{diff_abi, AbiChange, AbiDiff};
pub use gen::{mimi_type_to_abi, register_core_runtime_abi, AbiGenerator};
pub use handle::{Handle, HandleError, HandleKind, HandleRegistry, RuntimeId};
pub use rust_bind::generate_rust_bindings;
pub use serialize::{MimiAbi, MimiAbiError};
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

    // ── End-to-end pipeline integration test ──

    #[test]
    fn full_pipeline_integration() {
        // Step 1: Build ComponentIr from registry
        let mut gen = crate::component::AbiGenerator::new();
        crate::component::register_core_runtime_abi(&mut gen);
        let ir = gen.build();
        assert!(ir.exports.len() >= 140, "registry too small");

        // Step 2: Validate internal consistency
        let errors = ir.validate();
        assert!(errors.is_empty(), "validation errors: {:?}", errors);

        // Step 3: Serialize to .mimiabi JSON
        let abi = crate::component::MimiAbi::from_component_ir(&ir);
        let json = abi.to_json().expect("serialize");
        assert!(json.contains("mimi_rc_alloc"));
        assert!(json.contains("ListHandle"));

        // Step 4: Deserialize back
        let abi2 = crate::component::MimiAbi::from_json(&json).expect("deserialize");
        assert_eq!(abi.exports.len(), abi2.exports.len());
        assert_eq!(abi.types.len(), abi2.types.len());

        // Step 5: Reverse conversion to ComponentIr
        let ir2 = abi2.to_component_ir();
        assert_eq!(ir.exports.len(), ir2.exports.len());
        assert_eq!(ir.identity, ir2.identity);

        // Step 6: Hash is deterministic
        let h1 = abi.hash().expect("hash");
        let h2 = abi2.hash().expect("hash");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // BLAKE3 hex

        // Step 7: Layout probe (checkpoint)
        let faults = crate::component::probe_layout(&abi);
        assert!(faults.is_empty(), "layout faults: {:?}", faults);
        // Phantom fat-pointer structs removed (audit 2026-08-05): the core
        // registry now only carries opaque handle typedefs.
        assert!(abi.types.len() >= 4);
        assert_eq!(crate::component::struct_type_count(&abi), 0);

        // Step 8: ABI diff (identical → no changes)
        let diff = crate::component::diff_abi(&abi, &abi2);
        assert!(!diff.has_breaking_changes());
        assert_eq!(diff.summary(), "no changes");

        // Step 9: Generate C header
        let c_header = crate::component::generate_c_header(&ir);
        assert!(c_header.contains("#ifndef MIMI_RUNTIME_ABI_H"));
        assert!(c_header.contains("typedef uintptr_t MimiHandle;"));
        assert!(c_header.contains("extern \"C\" {"));
        assert!(c_header.contains("mimi_rc_alloc"));

        // Step 10: Generate Rust bindings
        let rust_bind = crate::component::generate_rust_bindings(&ir);
        // Struct typedefs no longer exist (phantom fat-pointer surface removed,
        // audit 2026-08-05) — opaque handles render as type aliases.
        assert!(rust_bind.contains("pub fn mimi_list_free"));
        assert!(rust_bind.contains("pub type ListHandle = usize;"));
        assert!(rust_bind.contains("extern \"C\" {"));
        assert!(rust_bind.contains("pub fn mimi_rc_alloc"));

        // Step 11: Roundtrip fixpoint (serialize → deserialize → re-serialize)
        let json2 = abi2.to_json().expect("re-serialize");
        assert_eq!(json, json2, "roundtrip is not a fixpoint");
    }

    #[test]
    fn mimi_type_to_abi_conversions() {
        use crate::component::mimi_type_to_abi;
        use crate::component::AbiPrimitive;

        // Primitives
        assert_eq!(
            mimi_type_to_abi("i32"),
            AbiTypeRef::Primitive(AbiPrimitive::I32)
        );
        assert_eq!(
            mimi_type_to_abi("f64"),
            AbiTypeRef::Primitive(AbiPrimitive::F64)
        );
        assert_eq!(
            mimi_type_to_abi("bool"),
            AbiTypeRef::Primitive(AbiPrimitive::Bool)
        );
        assert_eq!(
            mimi_type_to_abi("int"),
            AbiTypeRef::Primitive(AbiPrimitive::I32)
        );

        // String → *mut u8
        assert_eq!(
            mimi_type_to_abi("string"),
            AbiTypeRef::Pointer(Box::new(AbiTypeRef::Primitive(AbiPrimitive::U8)))
        );
        assert_eq!(
            mimi_type_to_abi("String"),
            AbiTypeRef::Pointer(Box::new(AbiTypeRef::Primitive(AbiPrimitive::U8)))
        );

        // Void
        assert_eq!(mimi_type_to_abi("void"), AbiTypeRef::Void);
        assert_eq!(mimi_type_to_abi("()"), AbiTypeRef::Void);
        assert_eq!(mimi_type_to_abi(""), AbiTypeRef::Void);

        // User-defined → Named
        assert_eq!(
            mimi_type_to_abi("MyStruct"),
            AbiTypeRef::Named("MyStruct".to_string())
        );

        // Pointer types
        assert_eq!(
            mimi_type_to_abi("*mut i32"),
            AbiTypeRef::Pointer(Box::new(AbiTypeRef::Primitive(AbiPrimitive::I32)))
        );
        assert_eq!(
            mimi_type_to_abi("*const u8"),
            AbiTypeRef::Pointer(Box::new(AbiTypeRef::Primitive(AbiPrimitive::U8)))
        );

        // Reference types (ABI-equivalent to pointers)
        assert_eq!(
            mimi_type_to_abi("&i64"),
            AbiTypeRef::Pointer(Box::new(AbiTypeRef::Primitive(AbiPrimitive::I64)))
        );
        assert_eq!(
            mimi_type_to_abi("&mut bool"),
            AbiTypeRef::Pointer(Box::new(AbiTypeRef::Primitive(AbiPrimitive::Bool)))
        );

        // Slice types
        assert_eq!(
            mimi_type_to_abi("[i32]"),
            AbiTypeRef::Slice(Box::new(AbiTypeRef::Primitive(AbiPrimitive::I32)))
        );
        assert_eq!(
            mimi_type_to_abi("Vec<u8>"),
            AbiTypeRef::Slice(Box::new(AbiTypeRef::Primitive(AbiPrimitive::U8)))
        );

        // Nested pointer
        assert_eq!(
            mimi_type_to_abi("*mut *mut u8"),
            AbiTypeRef::Pointer(Box::new(AbiTypeRef::Pointer(Box::new(
                AbiTypeRef::Primitive(AbiPrimitive::U8)
            ))))
        );

        // Whitespace trimming
        assert_eq!(
            mimi_type_to_abi("  i32  "),
            AbiTypeRef::Primitive(AbiPrimitive::I32)
        );
    }
}
