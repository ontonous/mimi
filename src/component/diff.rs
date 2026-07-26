//! ABI diff tool: detect breaking changes between two `.mimiabi` versions.
//!
//! 0.31.30 (COMPONENT-DIFF-001): When the ABI changes between compiler
//! versions, consumers need to know whether the change is breaking
//! (requires recompilation) or non-breaking (binary compatible).
//!
//! Breaking changes:
//! - Removed export symbol
//! - Changed parameter type (count or type)
//! - Changed return type
//! - Removed type definition
//! - Changed struct field layout
//!
//! Non-breaking changes:
//! - Added export symbol
//! - Added type definition
//! - Added optional parameter (future)

use super::serialize::MimiAbi;

/// A single ABI change between two versions.
#[derive(Debug, Clone, PartialEq)]
pub enum AbiChange {
    /// A new export was added (non-breaking).
    AddedExport(String),
    /// An export was removed (BREAKING).
    RemovedExport(String),
    /// An export's signature changed (BREAKING).
    ChangedExport { name: String, detail: String },
    /// A new type was added (non-breaking).
    AddedType(String),
    /// A type was removed (BREAKING).
    RemovedType(String),
    /// A type's definition changed (BREAKING).
    ChangedType { name: String, detail: String },
    /// ABI version was bumped (informational).
    VersionBumped { old: u32, new: u32 },
}

impl AbiChange {
    /// True if this change is breaking (requires recompilation).
    pub fn is_breaking(&self) -> bool {
        matches!(
            self,
            AbiChange::RemovedExport(_)
                | AbiChange::ChangedExport { .. }
                | AbiChange::RemovedType(_)
                | AbiChange::ChangedType { .. }
        )
    }
}

impl std::fmt::Display for AbiChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AbiChange::AddedExport(name) => write!(f, "+ export {}", name),
            AbiChange::RemovedExport(name) => write!(f, "- export {} (BREAKING)", name),
            AbiChange::ChangedExport { name, detail } => {
                write!(f, "~ export {} (BREAKING): {}", name, detail)
            }
            AbiChange::AddedType(name) => write!(f, "+ type {}", name),
            AbiChange::RemovedType(name) => write!(f, "- type {} (BREAKING)", name),
            AbiChange::ChangedType { name, detail } => {
                write!(f, "~ type {} (BREAKING): {}", name, detail)
            }
            AbiChange::VersionBumped { old, new } => {
                write!(f, "abi_version {} → {}", old, new)
            }
        }
    }
}

/// Result of comparing two ABI versions.
#[derive(Debug, Clone)]
pub struct AbiDiff {
    /// All detected changes.
    pub changes: Vec<AbiChange>,
}

impl AbiDiff {
    /// True if any change is breaking.
    pub fn has_breaking_changes(&self) -> bool {
        self.changes.iter().any(|c| c.is_breaking())
    }

    /// Number of breaking changes.
    pub fn breaking_count(&self) -> usize {
        self.changes.iter().filter(|c| c.is_breaking()).count()
    }

    /// Number of non-breaking changes.
    pub fn non_breaking_count(&self) -> usize {
        self.changes.len() - self.breaking_count()
    }

    /// Human-readable summary.
    pub fn summary(&self) -> String {
        if self.changes.is_empty() {
            return "no changes".to_string();
        }
        let breaking = self.breaking_count();
        let non_breaking = self.non_breaking_count();
        if breaking > 0 {
            format!(
                "{} breaking + {} non-breaking change(s)",
                breaking, non_breaking
            )
        } else {
            format!("{} non-breaking change(s)", non_breaking)
        }
    }
}

/// Compare two `.mimiabi` versions and detect all changes.
///
/// `old` is the baseline, `new` is the candidate.
pub fn diff_abi(old: &MimiAbi, new: &MimiAbi) -> AbiDiff {
    let mut changes = Vec::new();

    // ABI version
    if old.identity.abi_version != new.identity.abi_version {
        changes.push(AbiChange::VersionBumped {
            old: old.identity.abi_version,
            new: new.identity.abi_version,
        });
    }

    // Export changes
    let old_exports: std::collections::HashMap<&str, &super::serialize::MimiAbiSymbol> =
        old.exports.iter().map(|s| (s.name.as_str(), s)).collect();
    let new_exports: std::collections::HashMap<&str, &super::serialize::MimiAbiSymbol> =
        new.exports.iter().map(|s| (s.name.as_str(), s)).collect();

    for (name, old_sym) in &old_exports {
        match new_exports.get(name) {
            None => changes.push(AbiChange::RemovedExport(name.to_string())),
            Some(new_sym) => {
                // Check signature changes
                if old_sym.params.len() != new_sym.params.len() {
                    changes.push(AbiChange::ChangedExport {
                        name: name.to_string(),
                        detail: format!(
                            "param count {} → {}",
                            old_sym.params.len(),
                            new_sym.params.len()
                        ),
                    });
                } else {
                    for (i, (op, np)) in
                        old_sym.params.iter().zip(new_sym.params.iter()).enumerate()
                    {
                        let ot = format!("{:?}", op.ty);
                        let nt = format!("{:?}", np.ty);
                        if ot != nt {
                            changes.push(AbiChange::ChangedExport {
                                name: name.to_string(),
                                detail: format!("param {} type {} → {}", i, ot, nt),
                            });
                        }
                    }
                }
                let ort = format!("{:?}", old_sym.ret);
                let nrt = format!("{:?}", new_sym.ret);
                if ort != nrt {
                    changes.push(AbiChange::ChangedExport {
                        name: name.to_string(),
                        detail: format!("return type {} → {}", ort, nrt),
                    });
                }
            }
        }
    }
    for name in new_exports.keys() {
        if !old_exports.contains_key(name) {
            changes.push(AbiChange::AddedExport(name.to_string()));
        }
    }

    // Type changes
    let old_types: std::collections::HashMap<&str, &super::serialize::MimiAbiType> =
        old.types.iter().map(|t| (type_name(t), t)).collect();
    let new_types: std::collections::HashMap<&str, &super::serialize::MimiAbiType> =
        new.types.iter().map(|t| (type_name(t), t)).collect();

    for (name, old_ty) in &old_types {
        match new_types.get(name) {
            None => changes.push(AbiChange::RemovedType(name.to_string())),
            Some(new_ty) => {
                let os = format!("{:?}", old_ty);
                let ns = format!("{:?}", new_ty);
                if os != ns {
                    changes.push(AbiChange::ChangedType {
                        name: name.to_string(),
                        detail: "definition changed".to_string(),
                    });
                }
            }
        }
    }
    for name in new_types.keys() {
        if !old_types.contains_key(name) {
            changes.push(AbiChange::AddedType(name.to_string()));
        }
    }

    AbiDiff { changes }
}

/// Extract the name from a MimiAbiType.
fn type_name(ty: &super::serialize::MimiAbiType) -> &str {
    match ty {
        super::serialize::MimiAbiType::Struct { name, .. } => name,
        super::serialize::MimiAbiType::Enum { name, .. } => name,
        super::serialize::MimiAbiType::Alias { name, .. } => name,
        super::serialize::MimiAbiType::Opaque { name, .. } => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::gen::{register_core_runtime_abi, AbiGenerator};

    fn make_abi() -> MimiAbi {
        let mut gen = AbiGenerator::new();
        register_core_runtime_abi(&mut gen);
        let ir = gen.build();
        MimiAbi::from_component_ir(&ir)
    }

    #[test]
    fn identical_abis_no_changes() {
        let abi = make_abi();
        let diff = diff_abi(&abi, &abi);
        assert!(diff.changes.is_empty());
        assert!(!diff.has_breaking_changes());
        assert_eq!(diff.summary(), "no changes");
    }

    #[test]
    fn added_export_is_non_breaking() {
        let old = make_abi();
        let mut new = make_abi();
        new.exports
            .push(crate::component::serialize::MimiAbiSymbol {
                name: "mimi_new_function".to_string(),
                kind: "Function".to_string(),
                params: vec![],
                ret: crate::component::serialize::MimiAbiTypeRef::Void,
                effects: vec![],
                is_unsafe: false,
                call_conv: "C".to_string(),
                callback_category: None,
            });

        let diff = diff_abi(&old, &new);
        assert_eq!(diff.changes.len(), 1);
        assert!(!diff.has_breaking_changes());
        assert!(matches!(&diff.changes[0], AbiChange::AddedExport(n) if n == "mimi_new_function"));
    }

    #[test]
    fn removed_export_is_breaking() {
        let old = make_abi();
        let mut new = make_abi();
        new.exports.retain(|s| s.name != "mimi_rc_alloc");

        let diff = diff_abi(&old, &new);
        assert!(diff.has_breaking_changes());
        assert!(diff
            .changes
            .iter()
            .any(|c| matches!(c, AbiChange::RemovedExport(n) if n == "mimi_rc_alloc")));
    }

    #[test]
    fn changed_return_type_is_breaking() {
        let old = make_abi();
        let mut new = make_abi();
        // Change mimi_timestamp return type from I64 to I32
        if let Some(sym) = new.exports.iter_mut().find(|s| s.name == "mimi_timestamp") {
            sym.ret = crate::component::serialize::MimiAbiTypeRef::Primitive("I32".to_string());
        }

        let diff = diff_abi(&old, &new);
        assert!(diff.has_breaking_changes());
        assert!(diff.changes.iter().any(
            |c| matches!(c, AbiChange::ChangedExport { name, .. } if name == "mimi_timestamp")
        ));
    }

    #[test]
    fn changed_param_count_is_breaking() {
        let old = make_abi();
        let mut new = make_abi();
        // Add a param to mimi_rc_retain
        if let Some(sym) = new.exports.iter_mut().find(|s| s.name == "mimi_rc_retain") {
            sym.params.push(crate::component::serialize::MimiAbiParam {
                name: "extra".to_string(),
                ty: crate::component::serialize::MimiAbiTypeRef::Primitive("I32".to_string()),
                is_nullable: false,
            });
        }

        let diff = diff_abi(&old, &new);
        assert!(diff.has_breaking_changes());
        assert!(diff
            .changes
            .iter()
            .any(|c| matches!(c, AbiChange::ChangedExport { name, detail } if name == "mimi_rc_retain" && detail.contains("param count"))));
    }

    #[test]
    fn removed_type_is_breaking() {
        let old = make_abi();
        let mut new = make_abi();
        new.types.retain(|t| type_name(t) != "MimiString");

        let diff = diff_abi(&old, &new);
        assert!(diff.has_breaking_changes());
        assert!(diff
            .changes
            .iter()
            .any(|c| matches!(c, AbiChange::RemovedType(n) if n == "MimiString")));
    }

    #[test]
    fn version_bump_detected() {
        let old = make_abi();
        let mut new = make_abi();
        new.identity.abi_version = 2;

        let diff = diff_abi(&old, &new);
        assert!(diff
            .changes
            .iter()
            .any(|c| matches!(c, AbiChange::VersionBumped { old: 1, new: 2 })));
        // Version bump alone is not breaking
        assert!(!diff.has_breaking_changes());
    }

    #[test]
    fn summary_counts() {
        let old = make_abi();
        let mut new = make_abi();
        new.exports.retain(|s| s.name != "mimi_rc_alloc"); // breaking
        new.exports
            .push(crate::component::serialize::MimiAbiSymbol {
                name: "mimi_new_fn".to_string(),
                kind: "Function".to_string(),
                params: vec![],
                ret: crate::component::serialize::MimiAbiTypeRef::Void,
                effects: vec![],
                is_unsafe: false,
                call_conv: "C".to_string(),
                callback_category: None,
            }); // non-breaking

        let diff = diff_abi(&old, &new);
        assert_eq!(diff.breaking_count(), 1);
        assert_eq!(diff.non_breaking_count(), 1);
        assert_eq!(diff.summary(), "1 breaking + 1 non-breaking change(s)");
    }

    #[test]
    fn changed_param_type_is_breaking() {
        let old = make_abi();
        let mut new = make_abi();
        // Change mimi_rc_alloc param type from UIntPtr to I32
        if let Some(sym) = new.exports.iter_mut().find(|s| s.name == "mimi_rc_alloc") {
            if let Some(param) = sym.params.first_mut() {
                param.ty =
                    crate::component::serialize::MimiAbiTypeRef::Primitive("I32".to_string());
            }
        }

        let diff = diff_abi(&old, &new);
        assert!(diff.has_breaking_changes());
        assert!(diff.changes.iter().any(
            |c| matches!(c, AbiChange::ChangedExport { name, detail } if name == "mimi_rc_alloc" && detail.contains("param 0 type"))
        ));
    }

    #[test]
    fn changed_type_definition_is_breaking() {
        let old = make_abi();
        let mut new = make_abi();
        // Change MimiString struct size
        for ty in &mut new.types {
            if let crate::component::serialize::MimiAbiType::Struct { name, size, .. } = ty {
                if name == "MimiString" {
                    *size = Some(32); // was 24
                }
            }
        }

        let diff = diff_abi(&old, &new);
        assert!(diff.has_breaking_changes());
        assert!(diff
            .changes
            .iter()
            .any(|c| matches!(c, AbiChange::ChangedType { name, .. } if name == "MimiString")));
    }
}
