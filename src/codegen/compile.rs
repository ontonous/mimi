use crate::ast::*;
use crate::codegen::types;
use std::collections::HashMap;

use crate::error::{CompileError, MimiResult};
use crate::span::Span;

use super::CodeGenerator;
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{InitializationConfig, Target, TargetMachine};
use inkwell::types::BasicTypeEnum;
use inkwell::OptimizationLevel;

impl<'ctx> CodeGenerator<'ctx> {
    pub(super) fn mangle_name(base: &str, type_map: &HashMap<String, crate::ast::Type>) -> String {
        if type_map.is_empty() {
            return base.to_string();
        }
        let mut parts: Vec<String> = type_map.iter()
            .map(|(k, v)| format!("{}_{}", k, crate::core::fmt_type(v)))
            .collect();
        parts.sort();
        format!("{}${}", base, parts.join("$"))
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

    /// Apply a handler to every item in `items`, recursing into modules.
    fn process_items<F>(items: &[Item], f: &mut F) -> MimiResult<()>
    where
        F: FnMut(&Item) -> MimiResult<()>,
    {
        for item in items {
            if let Item::Module(m) = item {
                for inner in &m.items {
                    f(inner)?;
                }
            } else {
                f(item)?;
            }
        }
        Ok(())
    }

    pub fn compile_file(&mut self, file: &File) -> MimiResult<()> {
        // First pass: collect type definitions, function definitions, and cap definitions
        Self::process_items(&file.items, &mut |item| {
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
                _ => {}
            }
            Ok(())
        })?;
        // Second pass: register extern functions and external types
        Self::process_items(&file.items, &mut |item| {
            match item {
                Item::ExternBlock(block) => {
                    self.register_extern_block(block)?;
                }
                Item::Type(t) if self.is_committed(&t.commitment) => {
                    self.register_type_def(t)?;
                }
                _ => {}
            }
            Ok(())
        })?;
        // Third pass: compile impl methods (needed before vtable construction)
        self.compile_impl_methods()?;
        // Fourth pass: compile vtables (needed before user function compilation)
        self.compile_vtables()?;
        // Fifth pass: compile user functions and actors
        Self::process_items(&file.items, &mut |item| {
            match item {
                Item::Func(f) if !f.is_comptime && self.is_committed(&f.commitment) => {
                    self.compile_func(f).map_err(|e| e.at(Span::from(f.pos)))?;
                }
                Item::Actor(actor) if self.is_committed(&actor.commitment) => {
                    self.compile_actor(actor)?;
                }
                _ => {}
            }
            Ok(())
        })?;
        Ok(())
    }

    /// Run LLVM optimization passes on the module (O2).
    /// Called from compile_to_object during actual builds.
    pub fn optimize_module(&self) -> MimiResult<()> {
        Target::initialize_native(&InitializationConfig::default())
            .map_err(|e| CompileError::LlvmError(format!("failed to initialize target: {}", e)))?;
        let triple = TargetMachine::get_default_triple();
        let target = Target::from_triple(&triple)
            .map_err(|e| CompileError::LlvmError(format!("failed to find target: {}", e)))?;
        let cpu = TargetMachine::get_host_cpu_name().to_string();
        let features = TargetMachine::get_host_cpu_features().to_string();
        let tm = target.create_target_machine(
            &triple, &cpu, &features,
            OptimizationLevel::Aggressive,
            inkwell::targets::RelocMode::Default,
            inkwell::targets::CodeModel::Default,
        ).ok_or_else(|| CompileError::LlvmError("failed to create target machine".to_string()))?;
        let options = PassBuilderOptions::create();
        self.module.run_passes("default<O2>", &tm, options)
            .map_err(|e| CompileError::LlvmError(format!("optimization failed: {}", e)))
    }
}
