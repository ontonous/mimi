#![allow(dead_code, deprecated)]

use crate::ast::*;
use crate::codegen::types;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use std::collections::HashMap;

use crate::error::{CompileError, MimiResult};

use super::CodeGenerator;
use super::VarEntry;

impl<'ctx> CodeGenerator<'ctx> {
    pub(super) fn mangle_name(base: &str, type_map: &HashMap<String, crate::ast::Type>) -> String {
        if type_map.is_empty() {
            return base.to_string();
        }
        let mut parts: Vec<String> = type_map.iter()
            .map(|(k, v)| format!("{}_{}", k, crate::core::fmt_type(v)))
            .collect();
        parts.sort();
        format!("{}__{}", base, parts.join("__"))
    }

    /// Resolve a type through the current type_map (substitute generic params)
    pub(super) fn resolve_type(&self, ty: &crate::ast::Type) -> crate::ast::Type {
        if self.type_map.is_empty() {
            return ty.clone();
        }
        let generics: Vec<crate::ast::GenericParam> = self.type_map.keys()
            .map(|k| crate::ast::GenericParam { name: k.clone(), bounds: vec![] })
            .collect();
        crate::core::subst_type_params(ty, &generics, &self.type_map)
    }

    /// Resolve a type to its LLVM representation, applying generic substitution
    pub(super) fn resolve_type_llvm(&self, ty: &crate::ast::Type) -> Option<BasicTypeEnum<'ctx>> {
        let resolved = self.resolve_type(ty);
        types::mimi_type_to_llvm(self.context, &resolved)
    }

    /// Check if an item is committed ($/$$) in strict mode.
    /// In loose mode (default), all items pass.
    /// In strict mode, only items with Locked/StrongLocked commitment compile.
    pub(super) fn is_committed(&self, c: &Commitment) -> bool {
        if !self.strict { return true; }
        c.is_locked()
    }

    /// Get the commitment of a top-level item for strict-mode filtering.
    pub(super) fn item_commitment(item: &Item) -> Commitment {
        match item {
            Item::Func(f) => f.commitment,
            Item::Type(t) => t.commitment,
            Item::Actor(a) => a.commitment,
            Item::Module(m) => m.commitment,
            _ => Commitment::None,
        }
    }

    pub fn compile_file(&mut self, file: &File) -> MimiResult<()> {
        // First pass: collect type definitions, function definitions, and cap definitions
        for item in &file.items {
            match item {
                Item::Type(t) => {
                    self.register_type_def(t)?;
                }
                Item::Actor(actor) => {
                    self.register_actor_def(actor)?;
                }
                Item::Func(f) if !f.is_comptime => {
                    self.func_defs.insert(f.name.clone(), f.clone());
                }
                Item::Cap(cap) => {
                    self.cap_type_names.insert(cap.name.clone());
                }
                Item::Trait(t) => {
                    self.trait_defs.insert(t.name.clone(), t.clone());
                }
                Item::Impl(imp) => {
                    self.type_impls
                        .entry(imp.type_name.clone())
                        .or_default()
                        .insert(imp.trait_name.clone(), imp.methods.clone());
                }
                Item::Module(m) => {
                    for inner in &m.items {
                        match inner {
                            Item::Type(t) => {
                                self.register_type_def(t)?;
                            }
                            Item::Actor(actor) => {
                                self.register_actor_def(actor)?;
                            }
                            Item::Func(f) if !f.is_comptime => {
                                self.func_defs.insert(f.name.clone(), f.clone());
                            }
                            Item::Cap(cap) => {
                                self.cap_type_names.insert(cap.name.clone());
                            }
                            Item::Trait(t) => {
                                self.trait_defs.insert(t.name.clone(), t.clone());
                            }
                            Item::Impl(imp) => {
                                self.type_impls
                                    .entry(imp.type_name.clone())
                                    .or_default()
                                    .insert(imp.trait_name.clone(), imp.methods.clone());
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        // Second pass: register extern functions and compile user functions
        for item in &file.items {
            match item {
                Item::ExternBlock(block) => {
                    self.register_extern_block(block)?;
                }
                Item::Func(f) if !f.is_comptime && self.is_committed(&f.commitment) => {
                    self.compile_func(f)?;
                }
                Item::Actor(actor) if self.is_committed(&actor.commitment) => {
                    self.compile_actor(actor)?;
                }
                Item::Module(m) => {
                    for inner in &m.items {
                        match inner {
                            Item::ExternBlock(block) => {
                                self.register_extern_block(block)?;
                            }
                            Item::Func(f) if !f.is_comptime && self.is_committed(&f.commitment) => {
                                self.compile_func(f)?;
                            }
                            Item::Actor(actor) if self.is_committed(&actor.commitment) => {
                                self.compile_actor(actor)?;
                            }
                            Item::Type(t) if self.is_committed(&t.commitment) => {
                                self.register_type_def(t)?;
                            }
                            _ => {}
                        }
                    }
                }
                Item::Type(t) if self.is_committed(&t.commitment) => {
                    self.register_type_def(t)?;
                }
                _ => {}
            }
        }
        // Second pass: compile impl methods for committed trait implementations
        self.compile_impl_methods()?;
        self.compile_vtables()?;
        Ok(())
    }
}
