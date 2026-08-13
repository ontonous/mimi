//! Builtin function registry for the bytecode VM.
//!
//! Design principles (D1/D2):
//! - Each builtin is a standalone function: `fn(vm, args) -> Result<Value>`
//! - Registration is declarative: `BuiltinDesc { name, arity, category, func }`
//! - Arity is checked automatically before dispatch
//! - No giant match statement
//!
//! Builtin implementations live in `super::builtins::*` modules.
//! This file defines the registry types and the canonical `create_registry()`.

use super::vm::BytecodeVM;
use crate::interp::error::InterpError;
use crate::interp::value::Value;
use std::collections::HashMap;

/// Builtin function category (for organization and future type-directed dispatch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinCategory {
    Io,
    Convert,
    String,
    Math,
    List,
    HigherOrder,
    System,
}

/// A builtin function implementation.
/// Takes the VM (for stdout, closure calls, etc.) and the argument slice.
pub type BuiltinFn = fn(&mut BytecodeVM, &[Value]) -> Result<Value, InterpError>;

/// Inline fast path for hot builtins (R4, 0.35.44).
///
/// `call_builtin` allocates a `Vec<Value>` for the args and dispatches through
/// an indirect function pointer per call. For pure numeric builtins that cost
/// is pure overhead; the VM can inline them in the `CallBuiltin` dispatch arm
/// (no Vec, no indirect call, no per-builtin `Value` match fallback).
/// Semantics are mirrored 1:1 from `super::builtins::*`; any case the fast
/// path does not cover (unexpected arg type / overflow) falls back to the
/// general path so the error text stays identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinFastPath {
    Abs,
    Min,
    Max,
    Floor,
    Ceil,
    Round,
}

/// Descriptor for a builtin function.
pub struct BuiltinDesc {
    /// Builtin name (as called from Mimi code).
    pub name: &'static str,
    /// Expected number of arguments. `usize::MAX` = variadic.
    pub arity: usize,
    /// Category for organization.
    pub category: BuiltinCategory,
    /// Implementation.
    pub func: BuiltinFn,
}

/// Registry of all builtin functions.
pub struct BuiltinRegistry {
    /// Name → index mapping.
    name_to_idx: HashMap<&'static str, u32>,
    /// Descriptors in index order (index = BuiltinIdx).
    descs: Vec<BuiltinDesc>,
    /// Per-index fast-path marker (R4). Populated by `mark_fast_paths`.
    fast_paths: Vec<Option<BuiltinFastPath>>,
}

impl BuiltinRegistry {
    pub fn new() -> Self {
        BuiltinRegistry {
            name_to_idx: HashMap::new(),
            descs: Vec::new(),
            fast_paths: Vec::new(),
        }
    }

    /// Register a builtin. Returns its index (BuiltinIdx).
    /// Panics if a builtin with the same name is already registered.
    pub fn register(&mut self, desc: BuiltinDesc) -> u32 {
        assert!(
            !self.name_to_idx.contains_key(desc.name),
            "duplicate builtin registration: '{}'",
            desc.name,
        );
        let idx = self.descs.len() as u32;
        self.name_to_idx.insert(desc.name, idx);
        self.descs.push(desc);
        self.fast_paths.push(None);
        idx
    }

    /// Look up a builtin by name.
    pub fn lookup(&self, name: &str) -> Option<u32> {
        self.name_to_idx.get(name).copied()
    }

    /// Get the function pointer, arity, and name for a builtin.
    /// Used by VM dispatch to avoid borrow conflicts.
    pub fn get_func(&self, idx: u32) -> (BuiltinFn, usize, &'static str) {
        mimi_debug_assert!(
            (idx as usize) < self.descs.len(),
            "builtin index {} out of bounds (len {})",
            idx,
            self.descs.len()
        );
        let desc = &self.descs[idx as usize];
        (desc.func, desc.arity, desc.name)
    }

    /// Get all builtin names in index order (for compiler registration).
    pub fn names(&self) -> Vec<String> {
        self.descs.iter().map(|d| d.name.to_string()).collect()
    }

    /// Number of registered builtins.
    pub fn len(&self) -> usize {
        self.descs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.descs.is_empty()
    }

    /// Get the inline fast-path kind for a builtin index, if any (R4).
    pub fn fast_path(&self, idx: u32) -> Option<BuiltinFastPath> {
        self.fast_paths.get(idx as usize).copied().flatten()
    }

    /// Populate the fast-path markers for hot builtins (called after all
    /// categories register). Indices are resolved by name, so registration
    /// order does not matter.
    fn mark_fast_paths(&mut self) {
        for (name, kind) in [
            ("abs", BuiltinFastPath::Abs),
            ("min", BuiltinFastPath::Min),
            ("max", BuiltinFastPath::Max),
            ("floor", BuiltinFastPath::Floor),
            ("ceil", BuiltinFastPath::Ceil),
            ("round", BuiltinFastPath::Round),
        ] {
            if let Some(idx) = self.lookup(name) {
                self.fast_paths[idx as usize] = Some(kind);
            }
        }
    }
}

impl Default for BuiltinRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Create a registry with all builtins registered.
/// Single source of truth — compiler and VM both call this.
pub fn create_registry() -> BuiltinRegistry {
    let mut reg = BuiltinRegistry::new();
    super::builtins::register_all(&mut reg);
    reg.mark_fast_paths();
    reg.validate_arities();
    reg
}

/// U1 (0.35.44): every registered builtin's arity must match the canonical
/// `core::builtins::builtin_arity` table. Fail-closed in debug builds so a
/// drift between the VM registry and the core table is caught in CI.
impl BuiltinRegistry {
    fn validate_arities(&self) {
        for desc in &self.descs {
            match crate::core::builtins::builtin_arity(desc.name) {
                Some(core_arity) => mimi_debug_assert!(
                    core_arity == desc.arity,
                    "builtin '{}' arity drift: VM registry {} vs core {}",
                    desc.name,
                    desc.arity,
                    core_arity
                ),
                None => mimi_debug_assert!(
                    false,
                    "builtin '{}' missing from core::builtins::builtin_arity table",
                    desc.name
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// U1 (0.35.44): the VM registry and the canonical core arity table must
    /// be in lockstep — every registered builtin has a matching core arity,
    /// and the hot numeric fast-path builtins are actually marked.
    #[test]
    fn arity_consistency_with_core() {
        let reg = create_registry();
        for desc in &reg.descs {
            let core = crate::core::builtins::builtin_arity(desc.name);
            assert!(
                core.is_some(),
                "builtin '{}' missing from core arity table",
                desc.name
            );
            assert_eq!(
                core.unwrap(),
                desc.arity,
                "builtin '{}' arity drift: registry {} vs core {:?}",
                desc.name,
                desc.arity,
                core
            );
        }
    }

    /// R4 (0.35.44): the fast-path markers resolve for every hot numeric
    /// builtin listed in `mark_fast_paths`.
    #[test]
    fn fast_path_markers_resolve() {
        let reg = create_registry();
        for (name, expect) in [
            ("abs", BuiltinFastPath::Abs),
            ("min", BuiltinFastPath::Min),
            ("max", BuiltinFastPath::Max),
            ("floor", BuiltinFastPath::Floor),
            ("ceil", BuiltinFastPath::Ceil),
            ("round", BuiltinFastPath::Round),
        ] {
            let idx = reg.lookup(name).expect("builtin registered");
            assert_eq!(reg.fast_path(idx), Some(expect), "fast path for '{}'", name);
        }
    }
}
