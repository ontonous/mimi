pub mod types;
pub mod builtins;
pub mod gep;
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
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, CallSiteValue, ValueKind};
use inkwell::OptimizationLevel;
use std::collections::HashMap;
use std::path::Path;

/// Extract a BasicValueEnum from a ValueKind (inkwell 0.9+).
/// Variant names changed from 0.5: BasicValueEnum -> Basic, InstructionValue -> Instruction.
pub(crate) fn extract_basic_value<'ctx>(vk: ValueKind<'ctx>) -> Option<BasicValueEnum<'ctx>> {
    match vk {
        ValueKind::Basic(bv) => Some(bv),
        ValueKind::Instruction(_) => None,
    }
}

/// Try to get a BasicValueEnum from a CallSiteValue.
pub(crate) fn call_try_basic_value<'ctx>(call: &CallSiteValue<'ctx>) -> Option<BasicValueEnum<'ctx>> {
    extract_basic_value(call.try_as_basic_value())
}

/// Extension trait for CallSiteValue to extract BasicValueEnum.
pub(crate) trait CallSiteValueExt<'ctx> {
    fn try_as_basic_value_opt(&self) -> Option<BasicValueEnum<'ctx>>;
}

impl<'ctx> CallSiteValueExt<'ctx> for CallSiteValue<'ctx> {
    fn try_as_basic_value_opt(&self) -> Option<BasicValueEnum<'ctx>> {
        extract_basic_value(self.try_as_basic_value())
    }
}

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
    pub shared: bool,
    pub verify_contracts: bool,
    /// Optional target triple for cross-compilation (e.g. "x86_64-pc-windows-gnu").
    /// When None, defaults to the host target.
    pub target_triple: Option<String>,
    in_parasteps: bool,
    /// Pairs of (thread_id, result_type) for spawned threads inside parasteps.
    parasteps_thread_ids: Vec<(inkwell::values::IntValue<'ctx>, BasicTypeEnum<'ctx>)>,

    compensation_blocks: Vec<Vec<Stmt>>,
    comp_scope_stack: Vec<usize>,
    /// Stack of shared variable heap pointers that need release on scope exit.
    shared_release_vars: Vec<Vec<inkwell::values::PointerValue<'ctx>>>,
    /// Stack of weak reference heap pointers that need weak_release on scope exit.
    weak_release_vars: Vec<Vec<inkwell::values::PointerValue<'ctx>>>,
    /// Names of variables declared with `shared let` (for special access handling).
    shared_var_names: std::collections::HashSet<String>,
    /// Stack of heap-allocated buffer pointers from builtins that need free on scope exit.
    /// Uses RefCell for interior mutability since builtins take &self.
    heap_allocs: std::cell::RefCell<Vec<Vec<HeapEntry<'ctx>>>>,
    ensures_stmts: Vec<Box<Expr>>,
    old_snapshots: HashMap<String, VarEntry<'ctx>>,
    /// Names of comptime functions declared in the current file.
    /// Used for better error messages and unused-comptime warnings.
    comptime_func_names: std::collections::HashSet<String>,
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
    spawn_result_types: HashMap<String, BasicTypeEnum<'ctx>>,
    pending_spawn_type: Option<BasicTypeEnum<'ctx>>,
    /// Set of type names that are record types (for JSON FFI serialization).
    record_type_names: std::collections::HashSet<String>,
    /// Set of #[repr(C)] record type names (for struct-by-value FFI in codegen).
    repr_c_record_names: std::collections::HashSet<String>,
    /// Stack of tuple struct types for TupleIndex codegen.
    tuple_type_stack: Vec<inkwell::types::StructType<'ctx>>,
    /// Flag: when true, the next `compile_len("len", ...)` call should use strlen (for strings).
    /// Set in compile_call before dispatching to builtins.
    pending_len_is_string: bool,
    /// Names of variables holding first-class function pointer values.
    fn_ptr_var_names: std::collections::HashSet<String>,
    /// Stored extern function definitions for lazy code generation.
    extern_func_defs: HashMap<String, crate::ast::ExternFunc>,
    /// ABI per extern function name (e.g., "C", "stdcall").
    extern_block_abis: HashMap<String, String>,
    /// TLS callback globals that need clearing after the current extern call.
    /// Stores pointers to the fn_ptr and env_ptr TLS globals so they can be
    /// nulled out immediately after the C call returns.
    pending_callback_tls: Vec<inkwell::values::PointerValue<'ctx>>,
}

type VarEntry<'ctx> = (inkwell::values::PointerValue<'ctx>, BasicTypeEnum<'ctx>);

/// Entries tracked for scope-exit heap cleanup.
/// `Ptr` = raw pointer to free directly.
/// `Slot` = address of an alloca/GEP holding the pointer; load it, then free the loaded value.
enum HeapEntry<'ctx> {
    Ptr(inkwell::values::PointerValue<'ctx>),
    Slot(inkwell::values::PointerValue<'ctx>),
}

impl<'ctx> CodeGenerator<'ctx> {
    pub fn new(context: &'ctx Context, module_name: &str) -> Self {
        let module = context.create_module(module_name);
        let builder = context.create_builder();
        builtins::register_runtime(&module, context);
        Self { context, module, builder, loop_break: None, loop_continue: None, type_defs: HashMap::new(), type_llvm: HashMap::new(), cap_vars: vec![HashMap::new()], cap_type_names: std::collections::HashSet::new(), type_map: HashMap::new(), func_defs: HashMap::new(), var_type_names: HashMap::new(), spawn_counter: 0, strict: false, no_std: false, shared: false, verify_contracts: true, target_triple: None, compensation_blocks: Vec::new(), comp_scope_stack: Vec::new(), shared_release_vars: vec![Vec::new()], weak_release_vars: vec![Vec::new()], shared_var_names: std::collections::HashSet::new(), heap_allocs: std::cell::RefCell::new(vec![Vec::new()]), ensures_stmts: Vec::new(), old_snapshots: HashMap::new(), comptime_func_names: std::collections::HashSet::new(), in_parasteps: false, parasteps_thread_ids: Vec::new(), trait_defs: HashMap::new(), type_impls: HashMap::new(), vtable_globals: HashMap::new(), vtable_types: HashMap::new(), extern_param_types: HashMap::new(), callback_thunk_counter: 0, callback_thunks: HashMap::new(), spawn_result_types: HashMap::new(), pending_spawn_type: None, record_type_names: std::collections::HashSet::new(), repr_c_record_names: std::collections::HashSet::new(), tuple_type_stack: Vec::new(), pending_len_is_string: false, fn_ptr_var_names: std::collections::HashSet::new(), extern_func_defs: HashMap::new(), extern_block_abis: HashMap::new(), pending_callback_tls: Vec::new() }
    }

    pub fn gep(&self) -> gep::CheckedGepBuilder<'_, 'ctx> {
        gep::CheckedGepBuilder::new(&self.builder)
    }

    fn current_function(&self) -> Option<inkwell::values::FunctionValue<'ctx>> {
        self.builder.get_insert_block()?.get_parent()
    }

    fn block_has_terminator(&self) -> bool {
        self.builder.get_insert_block().and_then(|b| b.get_terminator()).is_some()
    }

    fn expect_basic_value(&self, call: &inkwell::values::CallSiteValue<'ctx>, name: &str) -> Result<BasicValueEnum<'ctx>, CompileError> {
        call_try_basic_value(call).ok_or_else(|| CompileError::LlvmError(format!("expected basic value from {}", name)))
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
            stack.push(HeapEntry::Ptr(ptr));
        }
    }

    /// Register a GEP/slot whose loaded value should be freed at scope exit.
    /// At free time, the pointer is loaded from the slot, getting the latest
    /// value after any reallocs.
    pub(super) fn register_heap_gep(&self, gep: inkwell::values::PointerValue<'ctx>) {
        if let Some(stack) = self.heap_allocs.borrow_mut().last_mut() {
            stack.push(HeapEntry::Slot(gep));
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
            for entry in scope {
                let ptr = match entry {
                    HeapEntry::Ptr(p) => p,
                    HeapEntry::Slot(gep) => {
                        let ptr_ty = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let loaded = self.builder.build_load(ptr_ty, gep, "heap_slot")
                            .map_err(|e| CompileError::LlvmError(format!("heap slot load error: {}", e)))?;
                        loaded.into_pointer_value()
                    }
                };
                self.builder.build_call(free_fn, &[
                    BasicMetadataValueEnum::PointerValue(ptr),
                ], "free_heap")
                    .map_err(|e| CompileError::LlvmError(format!("free error: {}", e)))?;
            }
        }
        Ok(())
    }

    /// Resolve a Mimi type to its LLVM representation, preferring registered
    /// type definitions (records, enums, actors) over the built-in name mapping.
    pub(super) fn llvm_type_for(&self, ty: &crate::ast::Type) -> Option<BasicTypeEnum<'ctx>> {
        if let crate::ast::Type::Name(name, _) = ty {
            if let Some(llvm) = self.type_llvm.get(name) {
                return Some(*llvm);
            }
        }
        crate::codegen::types::mimi_type_to_llvm(self.context, ty)
    }

    /// G2: Find the ordinal index of an enum variant name across all registered types.
    pub(super) fn find_variant_ordinal(&self, name: &str) -> Result<u64, CompileError> {
        for td in self.type_defs.values() {
            if let crate::ast::TypeDefKind::Enum(variants) = &td.kind {
                let mut sorted: Vec<&crate::ast::Variant> = variants.iter().collect();
                sorted.sort_by_key(|v| &v.name);
                for (i, v) in sorted.iter().enumerate() {
                    if v.name == name {
                        return Ok(i as u64);
                    }
                }
            }
        }
        // Built-in Result/Option variants (not present in type_defs).
        match name {
            "Ok" | "Some" => return Ok(1),
            "Err" | "None" => return Ok(0),
            _ => {}
        }
        Err(CompileError::Generic(format!(
            "enum variant '{}' not found in any registered enum type definition", name
        )))
    }

    /// G2: Find the owning type name and ordinal of an enum variant name.
    /// Returns `None` if `name` is not a variant in any registered enum type.
    pub(super) fn find_variant_owner(&self, name: &str) -> Option<(String, u64)> {
        for td in self.type_defs.values() {
            if let crate::ast::TypeDefKind::Enum(variants) = &td.kind {
                let mut sorted: Vec<&crate::ast::Variant> = variants.iter().collect();
                sorted.sort_by_key(|v| &v.name);
                for (i, v) in sorted.iter().enumerate() {
                    if v.name == name {
                        return Some((td.name.clone(), i as u64));
                    }
                }
            }
        }
        None
    }

    /// Compute the size in bytes of an LLVM type using a portable layout.
    /// This does not rely on the module data layout being set.
    pub(in crate::codegen) fn llvm_type_size_bytes(&self, ty: BasicTypeEnum<'ctx>) -> u64 {
        match ty {
            BasicTypeEnum::IntType(t) => (t.get_bit_width() / 8) as u64,
            BasicTypeEnum::FloatType(_) => 8,
            BasicTypeEnum::PointerType(_) => 8,
            BasicTypeEnum::StructType(t) => {
                t.get_field_types().iter().map(|f| self.llvm_type_size_bytes(*f)).sum()
            }
            BasicTypeEnum::ArrayType(t) => {
                t.len() as u64 * self.llvm_type_size_bytes(t.get_element_type())
            }
            BasicTypeEnum::VectorType(t) => {
                t.get_size() as u64 * self.llvm_type_size_bytes(t.get_element_type())
            }
            BasicTypeEnum::ScalableVectorType(_) => 8,
        }
    }

    /// G5: Compile a `shared let` / `local_shared let` / `weak` statement.
    pub(super) fn compile_shared_let_stmt(
        &mut self,
        kind: &crate::ast::SharedKind,
        name: &String,
        ty: &Option<crate::ast::Type>,
        init: &Expr,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> Result<(), CompileError> {
        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());

        // Track type name for downstream field access / inference
        if let Some(decl_ty) = ty {
            let tn = crate::core::fmt_type(decl_ty);
            self.var_type_names.insert(name.clone(), tn);
        } else if let Expr::Record { ty: Some(tn), .. } = init {
            self.var_type_names.insert(name.clone(), tn.clone());
        } else if let Expr::Call(callee, _) = init {
            if let Expr::Ident(fname) = callee.as_ref() {
                if let Some(fdef) = self.func_defs.get(fname) {
                    if let Some(ret_ty) = &fdef.ret {
                        self.var_type_names.insert(name.clone(), crate::core::fmt_type(ret_ty));
                    }
                }
            }
        }

        match kind {
            crate::ast::SharedKind::Weak | crate::ast::SharedKind::WeakLocal => {
                // Weak reference: init must be an existing shared variable.
                if let Expr::Ident(src_name) = init {
                    let &(src_alloca, val_ty) = vars.get(src_name)
                        .ok_or_else(|| CompileError::LlvmError(
                            format!("weak source '{}' not found", src_name)))?;
                    let ptr_ty = val_ty.ptr_type(inkwell::AddressSpace::default());
                    let heap_ptr_typed = self.builder.build_load(
                        BasicTypeEnum::PointerType(ptr_ty), src_alloca,
                        &format!("{}_weak_load", name),
                    ).map_err(|e| CompileError::LlvmError(format!("weak load: {}", e)))?.into_pointer_value();

                    // Increment the weak refcount on the heap allocation.
                    let heap_i8 = self.builder.build_pointer_cast(
                        heap_ptr_typed, i8_ptr, &format!("{}_weak_i8", name))
                        .map_err(|e| CompileError::LlvmError(format!("pointer cast error: {}", e)))?;
                    let weak_retain_fn = self.module.get_function("mimi_rc_weak_retain")
                        .ok_or_else(|| CompileError::LlvmError("mimi_rc_weak_retain not declared".to_string()))?;
                    self.builder.build_call(weak_retain_fn, &[
                        inkwell::values::BasicMetadataValueEnum::PointerValue(heap_i8),
                    ], &format!("{}_weak_retain", name))
                        .map_err(|e| CompileError::LlvmError(format!("weak retain error: {}", e)))?;

                    let new_alloca = self.builder.build_alloca(ptr_ty, name)
                        .map_err(|e| CompileError::LlvmError(format!("alloca: {}", e)))?;
                    self.builder.build_store(new_alloca, heap_ptr_typed)
                        .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                    vars.insert(name.clone(), (new_alloca, val_ty));
                    self.shared_var_names.insert(name.clone());
                    // Register the weak pointer so it is released when the weak ref goes out of scope.
                    self.register_weak_var(heap_i8);
                    return Ok(());
                }
                return Err(CompileError::LlvmError(
                    "weak requires an existing shared variable as initialiser".to_string()));
            }
            _ => {}
        }

        let mut val = self.compile_expr(init, vars)?;
        // If the initialiser returns a pointer (e.g. record literal builds an
        // alloca and returns its address), load the value first so we store the
        // actual data on the heap, not a stack pointer.
        let llvm_ty = if let BasicValueEnum::PointerValue(pv) = val {
            let ty_name = self.var_type_names.get(name.as_str())
                .or_else(|| {
                    if let Expr::Record { ty: Some(tn), .. } = init { Some(tn) } else { None }
                });
            let pointee_ty = ty_name.and_then(|tn| self.type_llvm.get(tn)).cloned()
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
            let loaded = self.builder.build_load(pointee_ty, pv, &format!("{}_val", name))
                .map_err(|e| CompileError::LlvmError(format!("shared load init: {}", e)))?;
            val = loaded;
            loaded.get_type()
        } else {
            val.get_type()
        };

        let ty_size_bytes = self.llvm_type_size_bytes(llvm_ty);
        let ty_size = self.context.i64_type().const_int(ty_size_bytes, false);
        let alloc_fn = self.module.get_function("mimi_rc_alloc")
            .ok_or_else(|| CompileError::LlvmError("mimi_rc_alloc not declared".to_string()))?;
        let heap_raw = self.builder.build_call(alloc_fn, &[
            inkwell::values::BasicMetadataValueEnum::IntValue(ty_size),
        ], &format!("{}_rc_alloc", name))
            .map_err(|e| CompileError::LlvmError(format!("rc_alloc error: {}", e)))?
            .try_as_basic_value_opt()
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

    /// Compile an arena block: push arena body BB, stacksav, compile block,
    /// filter out new vars, stackrestor, branch to continuation BB.
    /// Shared by Stmt::Arena and Stmt::Alloc { kind: AllocKind::Arena }.
    pub(super) fn compile_arena_block(
        &mut self,
        block: &Block,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
        label: &str,
    ) -> Result<(), CompileError> {
        let function = self.current_function()
            .ok_or_else(|| CompileError::LlvmError("arena outside function".to_string()))?;
        let arena_body_bb = self.context.append_basic_block(function, &format!("{}_body", label));
        let arena_cont_bb = self.context.append_basic_block(function, &format!("{}_cont", label));
        if !self.block_has_terminator() {
            self.builder.build_unconditional_branch(arena_body_bb)
                .map_err(|e| CompileError::LlvmError(format!("branch to {}: {}", label, e)))?;
        }
        self.builder.position_at_end(arena_body_bb);
        let saved = self.build_stacksave()?;
        let vars_before: std::collections::HashSet<String> = vars.keys().cloned().collect();
        self.compile_block(block, vars)?;
        for k in vars.keys().cloned().collect::<Vec<_>>() {
            if !vars_before.contains(&k) {
                vars.remove(&k);
            }
        }
        self.build_stackrestore(saved)?;
        if !self.block_has_terminator() {
            self.builder.build_unconditional_branch(arena_cont_bb)
                .map_err(|e| CompileError::LlvmError(format!("branch after {}: {}", label, e)))?;
        }
        self.builder.position_at_end(arena_cont_bb);
        Ok(())
    }

    /// G5b: Clone a shared reference: retain the heap pointer and register
    /// `new_name` as a new shared variable pointing to the same allocation.
    pub(super) fn compile_shared_ref_copy(
        &mut self,
        new_name: &str,
        src_name: &str,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> Result<(), CompileError> {
        let i8_ptr_ty = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
        let &(src_alloca, val_ty) = vars.get(src_name)
            .ok_or_else(|| CompileError::LlvmError(format!("shared source '{}' not found", src_name)))?;
        let ptr_ty = val_ty.ptr_type(inkwell::AddressSpace::default());

        // 1. Load the T* heap pointer from the source's alloca
        let heap_ptr_typed = self.builder.build_load(
            BasicTypeEnum::PointerType(ptr_ty), src_alloca,
            &format!("{}_shared_load", new_name),
        ).map_err(|e| CompileError::LlvmError(format!("shared load error: {}", e)))?.into_pointer_value();

        // 2. Cast to i8* and call mimi_rc_retain
        let heap_i8 = self.builder.build_pointer_cast(
            heap_ptr_typed, i8_ptr_ty,
            &format!("{}_shared_i8", new_name),
        ).map_err(|e| CompileError::LlvmError(format!("pointer cast error: {}", e)))?;
        let retain_fn = self.module.get_function("mimi_rc_retain")
            .ok_or_else(|| CompileError::LlvmError("mimi_rc_retain not declared".to_string()))?;
        self.builder.build_call(retain_fn, &[
            inkwell::values::BasicMetadataValueEnum::PointerValue(heap_i8),
        ], &format!("{}_retain", new_name))
            .map_err(|e| CompileError::LlvmError(format!("retain error: {}", e)))?;

        // 3. Create a new alloca for the new name and store the heap pointer
        let new_alloca = self.builder.build_alloca(ptr_ty, new_name)
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        self.builder.build_store(new_alloca, heap_ptr_typed)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;

        // 4. Register the i8* pointer for release on scope exit
        self.register_shared_var(heap_i8);

        // 5. Track type name and shared status
        self.shared_var_names.insert(new_name.to_string());
        if let Some(tn) = self.var_type_names.get(src_name) {
            self.var_type_names.insert(new_name.to_string(), tn.clone());
        }
        vars.insert(new_name.to_string(), (new_alloca, val_ty));

        Ok(())
    }

    pub fn compile_to_object(&self, output_path: &Path) -> Result<(), CompileError> {
        // Initialize the appropriate LLVM target(s):
        // - Native build: initialize only the host target
        // - Cross-compilation: initialize all registered targets
        if self.target_triple.is_some() {
            Target::initialize_all(&InitializationConfig::default());
        } else {
            Target::initialize_native(&InitializationConfig::default())
                .map_err(|e| format!("failed to initialize native target: {}", e))?;
        }
        let triple_str = self.target_triple.clone()
            .unwrap_or_else(|| {
                TargetMachine::get_default_triple().as_str().to_string_lossy().to_string()
            });
        let triple_str_ref = if self.no_std {
            let parts: Vec<&str> = triple_str.split('-').collect();
            if parts.len() >= 3 {
                format!("{}-{}-none", parts[0], parts[1])
            } else {
                format!("{}-none", parts[0])
            }
        } else {
            triple_str
        };
        let triple_ref = inkwell::targets::TargetTriple::create(&triple_str_ref);
        let target = Target::from_triple(&triple_ref)
            .map_err(|e| format!("failed to find target for triple '{}': {}", triple_ref, e))?;
        // When cross-compiling, use target defaults for CPU/features.
        // For native builds, use the host CPU for best performance.
        let (cpu, features) = if self.target_triple.is_some() {
            (String::new(), String::new())
        } else {
            (TargetMachine::get_host_cpu_name().to_string(),
             TargetMachine::get_host_cpu_features().to_string())
        };
        let reloc_mode = if self.shared { RelocMode::PIC } else { RelocMode::Default };
        let tm = target.create_target_machine(
            &triple_ref, &cpu, &features,
            OptimizationLevel::Aggressive,
            reloc_mode, CodeModel::Default,
        ).ok_or_else(|| format!("failed to create target machine for triple '{}'", triple_ref))?;

        // Run LLVM optimization passes before codegen (opt-in via MIMI_OPT env var)
        if std::env::var("MIMI_OPT").map(|v| v == "1" || v == "true").unwrap_or(false) {
            let options = inkwell::passes::PassBuilderOptions::create();
            self.module.run_passes("default<O2>", &tm, options)
                .map_err(|e| CompileError::LlvmError(format!("optimization failed: {}", e)))?;
        }

        tm.write_to_file(&self.module, inkwell::targets::FileType::Object, output_path)
            .map_err(|e| CompileError::Io(format!("failed to write object file: {}", e)))
    }
}
