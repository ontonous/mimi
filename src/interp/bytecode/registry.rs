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
pub type BuiltinFn = fn(&mut BytecodeVM<'_>, &[Value]) -> Result<Value, InterpError>;

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
}

impl BuiltinRegistry {
    pub fn new() -> Self {
        BuiltinRegistry {
            name_to_idx: HashMap::new(),
            descs: Vec::new(),
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
        idx
    }

    /// Look up a builtin by name.
    pub fn lookup(&self, name: &str) -> Option<u32> {
        self.name_to_idx.get(name).copied()
    }

    /// Get the function pointer, arity, and name for a builtin.
    /// Used by VM dispatch to avoid borrow conflicts.
    pub fn get_func(&self, idx: u32) -> (BuiltinFn, usize, &'static str) {
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
}

/// Create a registry with all builtins registered.
/// Single source of truth — compiler and VM both call this.
pub fn create_registry() -> BuiltinRegistry {
    let mut reg = BuiltinRegistry::new();
    super::builtins::register_all(&mut reg);
    reg
}
