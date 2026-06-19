#![allow(dead_code, deprecated)]

pub mod types;
pub mod builtins;
mod compile;
mod scope;
mod registry;
mod actors;
mod func;
mod block;
mod expr;

use crate::ast::*;
use crate::error::CompileError;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::builder::Builder;
use inkwell::targets::{CodeModel, InitializationConfig, RelocMode, Target, TargetMachine};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use inkwell::OptimizationLevel;
use std::collections::HashMap;
use std::path::Path;

/// Generated callback thunk for a closure→C function pointer conversion.
/// G1b: Each thunk reads fn_ptr and env_ptr from its globals at call time.
pub struct CallbackThunkEntry<'ctx> {
    pub thunk_fn: inkwell::values::FunctionValue<'ctx>,
    pub fn_ptr_global: inkwell::values::GlobalValue<'ctx>,
    pub env_ptr_global: inkwell::values::GlobalValue<'ctx>,
}

pub struct CodeGenerator<'ctx> {
    pub context: &'ctx Context,
    pub module: Module<'ctx>,
    pub builder: Builder<'ctx>,
    loop_break: Option<inkwell::basic_block::BasicBlock<'ctx>>,
    loop_continue: Option<inkwell::basic_block::BasicBlock<'ctx>>,
    type_defs: HashMap<String, crate::ast::TypeDef>,
    type_llvm: HashMap<String, BasicTypeEnum<'ctx>>,
    cap_vars: Vec<HashMap<String, (inkwell::values::PointerValue<'ctx>, bool)>>,
    cap_type_names: std::collections::HashSet<String>,
    type_map: HashMap<String, crate::ast::Type>,
    func_defs: HashMap<String, FuncDef>,
    var_type_names: HashMap<String, String>,
    spawn_counter: u64,
    pub strict: bool,
    pub no_std: bool,
    pub verify_contracts: bool,
    in_parasteps: bool,
    parasteps_thread_ids: Vec<inkwell::values::IntValue<'ctx>>,
    compensation_blocks: Vec<Vec<Stmt>>,
    comp_scope_stack: Vec<usize>,
    /// Stack of shared variable heap pointers that need release on scope exit.
    shared_release_vars: Vec<Vec<inkwell::values::PointerValue<'ctx>>>,
    /// Names of variables declared with `shared let` (for special access handling).
    shared_var_names: std::collections::HashSet<String>,
    /// Stack of heap-allocated buffer pointers from builtins that need free on scope exit.
    /// Uses RefCell for interior mutability since builtins take &self.
    heap_allocs: std::cell::RefCell<Vec<Vec<inkwell::values::PointerValue<'ctx>>>>,
    ensures_stmts: Vec<Box<Expr>>,
    trait_defs: HashMap<String, crate::ast::TraitDef>,
    type_impls: HashMap<String, HashMap<String, Vec<FuncDef>>>,
    vtable_globals: HashMap<String, inkwell::values::GlobalValue<'ctx>>,
    vtable_types: HashMap<String, inkwell::types::StructType<'ctx>>,
    /// G1b: Parameter types for each extern function (by wrapper name).
    extern_param_types: HashMap<String, Vec<crate::ast::Type>>,
    /// G1b: Counter for naming unique callback thunk functions.
    callback_thunk_counter: u64,
    /// G1b: Cache of generated callback thunks, keyed by signature fingerprint.
    callback_thunks: HashMap<String, CallbackThunkEntry<'ctx>>,
}

type VarEntry<'ctx> = (inkwell::values::PointerValue<'ctx>, BasicTypeEnum<'ctx>);

impl<'ctx> CodeGenerator<'ctx> {
    pub fn new(context: &'ctx Context, module_name: &str) -> Self {
        let module = context.create_module(module_name);
        let builder = context.create_builder();
        builtins::register_runtime(&module, context);
        Self { context, module, builder, loop_break: None, loop_continue: None, type_defs: HashMap::new(), type_llvm: HashMap::new(), cap_vars: vec![HashMap::new()], cap_type_names: std::collections::HashSet::new(), type_map: HashMap::new(), func_defs: HashMap::new(), var_type_names: HashMap::new(), spawn_counter: 0, strict: false, no_std: false, verify_contracts: true, compensation_blocks: Vec::new(), comp_scope_stack: Vec::new(), shared_release_vars: vec![Vec::new()], shared_var_names: std::collections::HashSet::new(), heap_allocs: std::cell::RefCell::new(vec![Vec::new()]), ensures_stmts: Vec::new(), in_parasteps: false, parasteps_thread_ids: Vec::new(), trait_defs: HashMap::new(), type_impls: HashMap::new(), vtable_globals: HashMap::new(), vtable_types: HashMap::new(), extern_param_types: HashMap::new(), callback_thunk_counter: 0, callback_thunks: HashMap::new() }
    }

    fn current_function(&self) -> Option<inkwell::values::FunctionValue<'ctx>> {
        self.builder.get_insert_block()?.get_parent()
    }

    fn block_has_terminator(&self) -> bool {
        self.builder.get_insert_block().and_then(|b| b.get_terminator()).is_some()
    }

    fn expect_basic_value(&self, call: &inkwell::values::CallSiteValue<'ctx>, name: &str) -> Result<BasicValueEnum<'ctx>, CompileError> {
        call.try_as_basic_value().left().ok_or_else(|| CompileError::LlvmError(format!("expected basic value from {}", name)))
    }

    fn current_fn_ret_type(&self) -> BasicTypeEnum<'ctx> {
        self.current_function()
            .and_then(|f| f.get_type().get_return_type())
            .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()))
    }

    fn adjust_int_val(&self, val: BasicValueEnum<'ctx>, target: BasicTypeEnum<'ctx>) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match (val, target) {
            (BasicValueEnum::IntValue(iv), BasicTypeEnum::IntType(ti)) => {
                let src_w = iv.get_type().get_bit_width();
                let dst_w = ti.get_bit_width();
                if src_w == dst_w {
                    Ok(iv.into())
                } else if src_w < dst_w {
                    self.builder.build_int_z_extend(iv, ti, "zext")
                        .map(|v| v.into())
                        .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))
                } else {
                    self.builder.build_int_truncate(iv, ti, "trunc")
                        .map(|v| v.into())
                        .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))
                }
            }
            _ => Ok(val),
        }
    }

    fn cg_err<T>(&self, _code: &str, msg: impl Into<String>) -> Result<T, CompileError> {
        Err(CompileError::LlvmError(msg.into()))
    }

    pub fn emit_ir(&self) -> String {
        self.module.print_to_string().to_string()
    }

    /// G5: Assign a compiled value to a variable (handles shared var dereference).
    pub(super) fn assign_to_var(
        &mut self,
        name: &str,
        val: BasicValueEnum<'ctx>,
        alloca: inkwell::values::PointerValue<'ctx>,
        ty: BasicTypeEnum<'ctx>,
    ) -> Result<(), CompileError> {
        if self.shared_var_names.contains(name) {
            // Shared variable: load the heap pointer, store new value at that location
            let ptr_ty = ty.ptr_type(inkwell::AddressSpace::default());
            let heap_ptr = self.builder.build_load(ptr_ty, alloca, &format!("{}_heap_ptr", name))
                .map_err(|e| CompileError::LlvmError(format!("shared heap ptr load error: {}", e)))?
                .into_pointer_value();
            self.builder.build_store(heap_ptr, val)
                .map_err(|e| CompileError::LlvmError(format!("shared assign store error: {}", e)))?;
        } else {
            self.builder.build_store(alloca, val)
                .map_err(|e| CompileError::LlvmError(format!("assign store error: {}", e)))?;
        }
        Ok(())
    }

    /// G10: Register a heap pointer (from builtins) for scope-exit free.
    /// Takes &self (not &mut self) because builtins use &self.
    pub(super) fn register_heap_alloc(&self, ptr: inkwell::values::PointerValue<'ctx>) {
        if let Some(stack) = self.heap_allocs.borrow_mut().last_mut() {
            stack.push(ptr);
        }
    }

    /// G10: Push a new scope level for heap allocations.
    /// Takes &self (not &mut self) because builtins use &self.
    pub(super) fn push_heap_scope(&self) {
        self.heap_allocs.borrow_mut().push(Vec::new());
    }

    /// G10: Pop scope level and emit `free(ptr)` for each registered heap allocation.
    pub(super) fn free_heap_allocs(&mut self) -> Result<(), CompileError> {
        if let Some(scope) = self.heap_allocs.borrow_mut().pop() {
            let free_fn = self.module.get_function("free")
                .ok_or_else(|| CompileError::LlvmError("free not declared".to_string()))?;
            for ptr in scope {
                self.builder.build_call(free_fn, &[
                    BasicMetadataValueEnum::PointerValue(ptr),
                ], "free_heap")
                    .map_err(|e| CompileError::LlvmError(format!("free error: {}", e)))?;
            }
        }
        Ok(())
    }

    /// G2: Find the ordinal index of an enum variant name across all registered types.
    pub(super) fn find_variant_ordinal(&self, name: &str) -> u64 {
        for td in self.type_defs.values() {
            if let crate::ast::TypeDefKind::Enum(variants) = &td.kind {
                for (i, v) in variants.iter().enumerate() {
                    if v.name == name {
                        return i as u64;
                    }
                }
            }
        }
        // Fallback: preserve old hash behavior if type not found
        let fallback = name.bytes().fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
        eprintln!("[codegen] warning: variant '{}' not found in any enum type definition, using fallback hash", name);
        fallback
    }

    /// G5: Compile a `shared let` statement with heap allocation and refcounting.
    pub(super) fn compile_shared_let_stmt(
        &mut self,
        name: &String,
        init: &Expr,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> Result<(), CompileError> {
        let val = self.compile_expr(init, vars)?;
        let llvm_ty = val.get_type();
        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());

        let ty_size_bytes = llvm_ty.size_of()
            .and_then(|v: inkwell::values::IntValue<'ctx>| v.get_zero_extended_constant())
            .unwrap_or(8);
        let ty_size = self.context.i64_type().const_int(ty_size_bytes, false);
        let alloc_fn = self.module.get_function("mimi_rc_alloc")
            .ok_or_else(|| CompileError::LlvmError("mimi_rc_alloc not declared".to_string()))?;
        let heap_raw = self.builder.build_call(alloc_fn, &[
            inkwell::values::BasicMetadataValueEnum::IntValue(ty_size),
        ], &format!("{}_rc_alloc", name))
            .map_err(|e| CompileError::LlvmError(format!("rc_alloc error: {}", e)))?
            .try_as_basic_value()
            .left()
            .ok_or_else(|| CompileError::LlvmError("mimi_rc_alloc returned void".to_string()))?;

        let heap_raw_ptr = heap_raw.into_pointer_value();
        let heap_ptr = self.builder.build_pointer_cast(
            heap_raw_ptr,
            llvm_ty.ptr_type(inkwell::AddressSpace::default()),
            &format!("{}_heap", name))
            .map_err(|e| CompileError::LlvmError(format!("pointer cast error: {}", e)))?;

        self.builder.build_store(heap_ptr, val)
            .map_err(|e| CompileError::LlvmError(format!("shared store error: {}", e)))?;

        let alloca = self.builder.build_alloca(
            llvm_ty.ptr_type(inkwell::AddressSpace::default()), name)
            .map_err(|e| CompileError::LlvmError(format!("shared handle alloca error: {}", e)))?;
        self.builder.build_store(alloca, heap_ptr)
            .map_err(|e| CompileError::LlvmError(format!("shared handle store error: {}", e)))?;

        vars.insert(name.clone(), (alloca, llvm_ty));
        self.shared_var_names.insert(name.clone());

        let heap_i8 = self.builder.build_pointer_cast(
            heap_ptr, i8_ptr, &format!("{}_i8", name))
            .map_err(|e| CompileError::LlvmError(format!("pointer cast error: {}", e)))?;
        self.register_shared_var(heap_i8);

        Ok(())
    }

    pub fn compile_to_object(&self, output_path: &Path) -> Result<(), CompileError> {
        Target::initialize_native(&InitializationConfig::default())
            .map_err(|e| format!("failed to initialize target: {}", e))?;
        let triple = TargetMachine::get_default_triple();
        let triple_str = triple.as_str().to_string_lossy().to_string();
        let triple_ref = if self.no_std {
            // Use freestanding target triple
            // e.g., "x86_64-unknown-linux-gnu" → "x86_64-unknown-none"
            let parts: Vec<&str> = triple_str.split('-').collect();
            let freestanding = if parts.len() >= 3 {
                format!("{}-{}-none", parts[0], parts[1])
            } else {
                format!("{}-none", parts[0])
            };
            inkwell::targets::TargetTriple::create(&freestanding)
        } else {
            triple
        };
        let target = Target::from_triple(&triple_ref)
            .map_err(|e| format!("failed to find target for triple '{}': {}", triple_ref, e))?;
        let cpu = TargetMachine::get_host_cpu_name().to_string();
        let features = TargetMachine::get_host_cpu_features().to_string();
        let tm = target.create_target_machine(
            &triple_ref, &cpu, &features,
            OptimizationLevel::Aggressive,
            RelocMode::Default, CodeModel::Default,
        ).ok_or_else(|| format!("failed to create target machine for triple '{}'", triple_ref))?;

        tm.write_to_file(&self.module, inkwell::targets::FileType::Object, output_path)
            .map_err(|e| CompileError::Io(format!("failed to write object file: {}", e)))
    }
}
