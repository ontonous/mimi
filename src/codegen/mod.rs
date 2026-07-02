mod actors;
mod block;
pub mod builtins;
mod compile;
mod expr;
mod func;
pub mod gep;
mod registry;
mod scope;
pub mod types;

#[cfg(test)]
mod tests;

use crate::ast::*;
use crate::error::CompileError;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::targets::{CodeModel, InitializationConfig, RelocMode, Target, TargetMachine};
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
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
pub(crate) fn call_try_basic_value<'ctx>(
    call: &CallSiteValue<'ctx>,
) -> Option<BasicValueEnum<'ctx>> {
    extract_basic_value(call.try_as_basic_value())
}

/// Extension trait for CallSiteValue to extract BasicValueEnum.
pub(crate) trait CallSiteValueExt<'ctx> {
    fn try_as_basic_value_opt(&self) -> Option<BasicValueEnum<'ctx>>;
}

/// Extract the element type from a "List<T>" type name string.
pub(super) fn extract_list_elem_type(type_name: &str) -> Option<crate::ast::Type> {
    if !type_name.starts_with("List<") {
        return None;
    }
    let inner_start = 5;
    let mut depth = 0u32;
    let mut inner_end = None;
    for (i, ch) in type_name[inner_start..].char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => {
                if depth == 0 {
                    inner_end = Some(inner_start + i);
                    break;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    let inner_str = inner_end.and_then(|end| {
        let s = type_name[inner_start..end].trim();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    })?;
    // Parse the inner type, handling nested generics.
    Some(parse_inner_type(inner_str))
}

/// Parse a type name string into a Type, supporting generics like List<T>.
fn parse_inner_type(s: &str) -> crate::ast::Type {
    let s = s.trim();
    if let Some(lt) = s.find('<') {
        if s.ends_with('>') {
            let base = s[..lt].trim();
            let args_str = s[lt + 1..s.len() - 1].trim();
            let mut args = Vec::new();
            let mut depth = 0u32;
            let mut start = 0usize;
            for (i, ch) in args_str.char_indices() {
                match ch {
                    '<' => depth += 1,
                    '>' => depth = depth.saturating_sub(1),
                    ',' if depth == 0 => {
                        args.push(parse_inner_type(args_str[start..i].trim()));
                        start = i + 1;
                    }
                    _ => {}
                }
            }
            let remaining = args_str[start..].trim();
            if !remaining.is_empty() {
                args.push(parse_inner_type(remaining));
            }
            return crate::ast::Type::Name(base.to_string(), args);
        }
    }
    crate::ast::Type::Name(s.to_string(), vec![])
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
    /// Type objects for variables (avoids string re-parsing for Arch-2).
    var_types: HashMap<String, Type>,
    /// Variables whose value is the result of a `weak.upgrade()` call.
    /// These Options hold a pointer payload even when the inner type is a
    /// primitive, so `unwrap()` must load the value through the pointer.
    upgrade_option_vars: std::collections::HashSet<String>,
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
    parasteps_future_ptrs: Vec<(inkwell::values::PointerValue<'ctx>, BasicTypeEnum<'ctx>)>,

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
    ensures_stmts: Vec<Expr>,
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
    /// Counter for naming unique export callback trampolines.
    export_callback_thunk_counter: u64,
    /// Cache of export callback trampolines, keyed by signature fingerprint.
    export_callback_trampolines: HashMap<String, inkwell::values::PointerValue<'ctx>>,
    pending_spawn_type: Option<BasicTypeEnum<'ctx>>,
    /// Maps variable names to the inner result type of Future<T> for async fn calls.
    /// Set when compiling `let f = async_fn()` and used when compiling `await f`.
    async_var_inner_types: HashMap<String, BasicTypeEnum<'ctx>>,
    /// Set of type names that are record types (for JSON FFI serialization).
    record_type_names: std::collections::HashSet<String>,
    /// Set of #[repr(C)] record type names (for struct-by-value FFI in codegen).
    repr_c_record_names: std::collections::HashSet<String>,
    /// Stack of tuple struct types for TupleIndex codegen.
    tuple_type_stack: Vec<inkwell::types::StructType<'ctx>>,
    /// Counter for unique contract assertion BasicBlock naming.
    /// Prevents BB name conflicts when multiple ensures/requires exist in one function.
    contract_bb_counter: u64,
    /// Flag: when true, the next `compile_len("len", ...)` call should use strlen (for strings).
    /// Set in compile_call before dispatching to builtins.
    pending_len_is_string: bool,
    /// Cached result of MIMI_OPT env var check at codegen construction time.
    /// Avoids repeated env var queries within a single compile_to_object call.
    optimize: bool,
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
    /// Maps variable names to the LLVM type of their list elements.
    /// For `let x: List<List<i32>>`, stores "x" → LLVM struct type of `List<i32>` ({i64, i8*}).
    /// Used by compile_index_expr to reconstruct struct values from type-erased i64 storage.
    list_elem_llvm_types: HashMap<String, BasicTypeEnum<'ctx>>,
    /// Cache of closure ABI wrapper functions for named functions.
    /// Key: original function name. Value: wrapper fn(i8*, params...) -> ret.
    /// Used when passing a named function where func(T)->U is expected.
    closure_wrappers: HashMap<String, inkwell::values::PointerValue<'ctx>>,
    /// Const values declared at top level (for codegen const support).
    const_values: HashMap<String, crate::ast::Expr>,

    // ====================================================================
    // v0.28.13 — Inline / GVN scaffolding
    // ====================================================================
    /// Pure-function CSE cache: maps (func_name, arg_fingerprint) → the
    /// previously computed SSA value. Used when the optimizer is enabled
    /// (`MIMI_OPT=1`) to skip recomputation of pure calls.
    ///
    /// v0.28.13 introduces the data structure; full CSE propagation is
    /// planned for v0.28.14. The fingerprint is a stable string derived
    /// from the function name and the SSA names of its arguments.
    #[allow(dead_code)]
    pub(crate) cse_cache: HashMap<String, inkwell::values::BasicValueEnum<'ctx>>,
    /// Small-function inline candidate list: functions whose IR size
    /// (instruction count) is below `INLINE_INSTRUCTION_THRESHOLD`.
    /// Populated during `compile_func` and consulted by
    /// `should_inline_at_call_site`.
    #[allow(dead_code)]
    pub(crate) inline_candidates: std::collections::HashSet<String>,
    /// Counter incremented for each CSE cache hit (used for diagnostics
    /// and the `cse_hits` accessor in tests).
    #[allow(dead_code)]
    pub(crate) cse_hits: u64,
    /// Counter incremented for each inline decision (used for diagnostics
    /// and the `inline_count` accessor in tests).
    #[allow(dead_code)]
    pub(crate) inline_count: u64,
    /// Names of functions that have been determined to be pure (no side
    /// effects, no external calls). Pure functions are eligible for both
    /// inlining and CSE. Populated during `compile_func` analysis pass.
    #[allow(dead_code)]
    pub(crate) pure_funcs: std::collections::HashSet<String>,

    // ====================================================================
    // v0.28.19 — Actor real concurrency
    // ====================================================================
    /// Names of actor types (for method-call dispatch routing).
    actor_names: std::collections::HashSet<String>,
    /// Maps "ActorName::method_name" → method index (i32), used as method_id
    /// in the dispatch function and mimi_actor_call.
    actor_method_ids: HashMap<String, i32>,
    /// Cached actor definitions keyed by actor name. Lets the mailbox-call
    /// call-site recover the declared method return type for unpacking the
    /// packed i64 result blob back to the original LLVM type.
    actor_defs: HashMap<String, crate::ast::ActorDef>,
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
        Self {
            context,
            module,
            builder,
            loop_break: None,
            loop_continue: None,
            type_defs: HashMap::new(),
            type_llvm: HashMap::new(),
            cap_vars: vec![HashMap::new()],
            cap_type_names: std::collections::HashSet::new(),
            type_map: HashMap::new(),
            func_defs: HashMap::new(),
            var_type_names: HashMap::new(),
            var_types: HashMap::new(),
            upgrade_option_vars: std::collections::HashSet::new(),
            spawn_counter: 0,
            strict: false,
            no_std: false,
            shared: false,
            verify_contracts: true,
            target_triple: None,
            compensation_blocks: Vec::new(),
            comp_scope_stack: Vec::new(),
            shared_release_vars: vec![Vec::new()],
            weak_release_vars: vec![Vec::new()],
            shared_var_names: std::collections::HashSet::new(),
            heap_allocs: std::cell::RefCell::new(vec![Vec::new()]),
            ensures_stmts: Vec::new(),
            old_snapshots: HashMap::new(),
            comptime_func_names: std::collections::HashSet::new(),
            in_parasteps: false,
            parasteps_future_ptrs: Vec::new(),
            trait_defs: HashMap::new(),
            type_impls: HashMap::new(),
            vtable_globals: HashMap::new(),
            vtable_types: HashMap::new(),
            extern_param_types: HashMap::new(),
            callback_thunk_counter: 0,
            callback_thunks: HashMap::new(),
            export_callback_thunk_counter: 0,
            export_callback_trampolines: HashMap::new(),
            pending_spawn_type: None,
            async_var_inner_types: HashMap::new(),
            record_type_names: std::collections::HashSet::new(),
            repr_c_record_names: std::collections::HashSet::new(),
            tuple_type_stack: Vec::new(),
            pending_len_is_string: false,
            optimize: std::env::var("MIMI_OPT")
                .map(|v| v == "1" || v == "true")
                .unwrap_or(false),
            contract_bb_counter: 0,
            fn_ptr_var_names: std::collections::HashSet::new(),
            extern_func_defs: HashMap::new(),
            extern_block_abis: HashMap::new(),
            pending_callback_tls: Vec::new(),
            list_elem_llvm_types: HashMap::new(),
            closure_wrappers: HashMap::new(),
            const_values: HashMap::new(),
            // v0.28.13 inline/GVN scaffolding
            cse_cache: HashMap::new(),
            inline_candidates: std::collections::HashSet::new(),
            cse_hits: 0,
            inline_count: 0,
            pure_funcs: std::collections::HashSet::new(),
            // v0.28.19 actor concurrency
            actor_names: std::collections::HashSet::new(),
            actor_method_ids: HashMap::new(),
            actor_defs: HashMap::new(),
        }
    }

    pub fn gep(&self) -> gep::CheckedGepBuilder<'_, 'ctx> {
        gep::CheckedGepBuilder::new(&self.builder)
    }

    // -------------------------------------------------------------------------
    // Low-level LLVM builder helpers
    //
    // These thin wrappers reduce the repetitive `map_err(|e|
    // CompileError::LlvmError(format!(...)))` boilerplate that appears hundreds
    // of times across the codegen module. They intentionally keep the same
    // semantics as the underlying inkwell calls so that refactors are local and
    // low-risk.
    // -------------------------------------------------------------------------

    /// Build an `alloca` instruction, returning a typed error on failure.
    pub(super) fn build_alloca<T: inkwell::types::BasicType<'ctx>>(
        &self,
        ty: T,
        name: &str,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        self.builder
            .build_alloca(ty, name)
            .map_err(|e| CompileError::LlvmError(format!("alloca error ({}): {}", name, e)))
    }

    /// Build a `store` instruction.
    pub(super) fn build_store(
        &self,
        ptr: inkwell::values::PointerValue<'ctx>,
        val: impl inkwell::values::BasicValue<'ctx>,
    ) -> Result<(), CompileError> {
        self.builder
            .build_store(ptr, val)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        Ok(())
    }

    /// Build a typed `load` instruction.
    pub(super) fn build_load<T: inkwell::types::BasicType<'ctx>>(
        &self,
        ty: T,
        ptr: inkwell::values::PointerValue<'ctx>,
        name: &str,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        self.builder
            .build_load(ty, ptr, name)
            .map_err(|e| CompileError::LlvmError(format!("load error ({}): {}", name, e)))
    }

    /// Build an unconditional branch.
    pub(super) fn build_br(
        &self,
        dest: inkwell::basic_block::BasicBlock<'ctx>,
    ) -> Result<(), CompileError> {
        self.builder
            .build_unconditional_branch(dest)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        Ok(())
    }

    /// Build a conditional branch.
    pub(super) fn build_cond_br(
        &self,
        cond: inkwell::values::IntValue<'ctx>,
        then_bb: inkwell::basic_block::BasicBlock<'ctx>,
        else_bb: inkwell::basic_block::BasicBlock<'ctx>,
    ) -> Result<(), CompileError> {
        self.builder
            .build_conditional_branch(cond, then_bb, else_bb)
            .map_err(|e| CompileError::LlvmError(format!("conditional branch error: {}", e)))?;
        Ok(())
    }

    /// Look up a runtime/external function by name.
    pub(super) fn get_runtime_fn(
        &self,
        name: &str,
    ) -> Result<inkwell::values::FunctionValue<'ctx>, CompileError> {
        self.module
            .get_function(name)
            .ok_or_else(|| CompileError::LlvmError(format!("{} not declared", name)))
    }

    /// Build a call instruction and return the resulting `CallSiteValue`.
    pub(super) fn build_call(
        &self,
        func: inkwell::values::FunctionValue<'ctx>,
        args: &[BasicMetadataValueEnum<'ctx>],
        name: &str,
    ) -> Result<inkwell::values::CallSiteValue<'ctx>, CompileError> {
        self.builder
            .build_call(func, args, name)
            .map_err(|e| CompileError::LlvmError(format!("call error ({}): {}", name, e)))
    }

    /// Build a `return` instruction with an optional value.
    pub(super) fn build_return(
        &self,
        val: Option<&dyn inkwell::values::BasicValue<'ctx>>,
    ) -> Result<(), CompileError> {
        self.builder
            .build_return(val)
            .map_err(|e| CompileError::LlvmError(format!("return error: {}", e)))?;
        Ok(())
    }

    /// Build an `extractvalue` instruction.
    pub(super) fn build_extract_value(
        &self,
        agg: inkwell::values::AggregateValueEnum<'ctx>,
        index: u32,
        name: &str,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        self.builder
            .build_extract_value(agg, index, name)
            .map_err(|e| CompileError::LlvmError(format!("extractvalue error ({}): {}", name, e)))
    }

    /// Build a `ptrtoint` instruction.
    pub(super) fn build_ptr_to_int(
        &self,
        ptr: inkwell::values::PointerValue<'ctx>,
        int_ty: inkwell::types::IntType<'ctx>,
        name: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, CompileError> {
        self.builder
            .build_ptr_to_int(ptr, int_ty, name)
            .map_err(|e| CompileError::LlvmError(format!("ptrtoint error ({}): {}", name, e)))
    }

    /// Build a `bitcast` instruction.
    pub(super) fn build_bit_cast(
        &self,
        val: BasicValueEnum<'ctx>,
        ty: BasicTypeEnum<'ctx>,
        name: &str,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        self.builder
            .build_bit_cast(val, ty, name)
            .map_err(|e| CompileError::LlvmError(format!("bitcast error ({}): {}", name, e)))
    }

    /// Build a `pointercast` instruction.
    pub(super) fn build_pointer_cast(
        &self,
        ptr: inkwell::values::PointerValue<'ctx>,
        ty: inkwell::types::PointerType<'ctx>,
        name: &str,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        self.builder
            .build_pointer_cast(ptr, ty, name)
            .map_err(|e| CompileError::LlvmError(format!("pointercast error ({}): {}", name, e)))
    }

    /// Build an `inttoptr` instruction.
    pub(super) fn build_int_to_ptr(
        &self,
        val: inkwell::values::IntValue<'ctx>,
        ptr_ty: inkwell::types::PointerType<'ctx>,
        name: &str,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        self.builder
            .build_int_to_ptr(val, ptr_ty, name)
            .map_err(|e| CompileError::LlvmError(format!("inttoptr error ({}): {}", name, e)))
    }

    /// Build an `in_bounds_gep` instruction.
    /// Delegates to `CheckedGepBuilder` so the underlying unsafe call is absorbed.
    pub(super) fn build_in_bounds_gep<T: inkwell::types::BasicType<'ctx>>(
        &self,
        pointee_ty: T,
        ptr: inkwell::values::PointerValue<'ctx>,
        indices: &[inkwell::values::IntValue<'ctx>],
        name: &str,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        self.gep()
            .build_in_bounds_gep(pointee_ty.as_basic_type_enum(), ptr, indices, name)
            .map_err(|e| CompileError::LlvmError(format!("gep error ({}): {}", name, e)))
    }

    fn current_function(&self) -> Option<inkwell::values::FunctionValue<'ctx>> {
        self.builder.get_insert_block()?.get_parent()
    }

    /// Create an alloca at the function entry block (not at the current insertion point).
    /// This ensures allocas are in the entry block, which is required for proper
    /// stack frame management when called from inside if/else branches or loops.
    pub(super) fn entry_alloca(
        &self,
        ty: BasicTypeEnum<'ctx>,
        name: &str,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        let function = self
            .current_function()
            .ok_or_else(|| "entry_alloca: no current function".to_string())?;
        let entry = function
            .get_first_basic_block()
            .ok_or_else(|| "entry_alloca: no entry block".to_string())?;
        let saved = self.builder.get_insert_block();
        // Position at the start of the entry block
        if let Some(first_instr) = entry.get_first_instruction() {
            self.builder.position_before(&first_instr);
        } else {
            self.builder.position_at_end(entry);
        }
        let alloca = self
            .builder
            .build_alloca(ty, name)
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        // Restore original position
        if let Some(bb) = saved {
            self.builder.position_at_end(bb);
        }
        Ok(alloca)
    }

    fn block_has_terminator(&self) -> bool {
        self.builder
            .get_insert_block()
            .and_then(|b| b.get_terminator())
            .is_some()
    }

    fn expect_basic_value(
        &self,
        call: &inkwell::values::CallSiteValue<'ctx>,
        name: &str,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        call_try_basic_value(call)
            .ok_or_else(|| CompileError::LlvmError(format!("expected basic value from {}", name)))
    }

    // ========================================================================
    // v0.28.13 — Inline heuristic and GVN/CSE scaffolding
    // ========================================================================
    //
    // These helpers provide the *data structures* and *decision logic* for
    // small-function inlining and common-subexpression elimination. They are
    // wired in but the full codegen pass is planned for v0.28.14. The
    // current scope:
    //
    // - `cse_cache` is a HashMap keyed by a stable fingerprint string.
    //   `cse_lookup` returns the previously computed SSA value if present;
    //   `cse_record` inserts a new entry. Both are no-ops in this version
    //   because the call site dispatch is conservative — the scaffold is
    //   tested via the accessors `cse_hits` and the deterministic fingerprint
    //   derivation.
    //
    // - `should_inline_at_call_site` returns true when the callee is in
    //   `inline_candidates` (small functions registered during
    //   `compile_func`). The threshold and the per-function instruction
    //   count metric are exposed via `INLINE_INSTRUCTION_THRESHOLD` and
    //   `count_instructions_in_function` for diagnostic and test use.
    //
    // This is the *skeleton*: full integration with the call expression
    // dispatch is left for v0.28.14 to avoid scope creep.

    /// Maximum instruction count for a function to be inlined.
    /// Functions with more than this many instructions are emitted as
    /// real `call` instructions; smaller functions are inlined into
    /// each call site.
    pub(crate) const INLINE_INSTRUCTION_THRESHOLD: u32 = 20;

    /// Derive a stable fingerprint string for a function call based on
    /// the function name and the SSA names of its arguments. Used as
    /// the key in `cse_cache`.
    #[allow(dead_code)]
    pub(crate) fn cse_fingerprint(&self, func_name: &str, args: &[BasicValueEnum<'ctx>]) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(args.len() + 1);
        parts.push(func_name.to_string());
        for (i, a) in args.iter().enumerate() {
            // For SSA values we use the index as a proxy: the SSA
            // numbering within a function is deterministic. For a more
            // robust fingerprint, the value's instruction opcode could
            // be added, but index alone suffices for v0.28.13's
            // scaffolding.
            parts.push(format!(
                "arg{}:v{}",
                i,
                a.get_type().print_to_string().to_string_lossy()
            ));
        }
        parts.join("|")
    }

    /// Look up a previously computed value for a pure-function call.
    /// Returns the cached SSA value if present, or None.
    /// Note: v0.28.13 only implements the lookup; the call site
    /// dispatcher is left unchanged (so this always returns None in
    /// practice). The data structure is exercised by tests.
    #[allow(dead_code)]
    pub(crate) fn cse_lookup(&mut self, fingerprint: &str) -> Option<BasicValueEnum<'ctx>> {
        let hit = self.cse_cache.get(fingerprint).copied();
        if hit.is_some() {
            self.cse_hits += 1;
        }
        hit
    }

    /// Record a freshly computed pure-function result for future CSE hits.
    /// Called by the call site dispatcher (v0.28.14 will integrate this).
    #[allow(dead_code)]
    pub(crate) fn cse_record(&mut self, fingerprint: String, value: BasicValueEnum<'ctx>) {
        self.cse_cache.insert(fingerprint, value);
    }

    /// Returns true if the named function should be inlined at the call
    /// site. Decision is based on `inline_candidates` membership
    /// (populated by the analysis pass in `compile_func`).
    #[allow(dead_code)]
    pub(crate) fn should_inline_at_call_site(&mut self, func_name: &str) -> bool {
        let in_set = self.inline_candidates.contains(func_name);
        if in_set {
            self.inline_count += 1;
        }
        in_set
    }

    /// Register a function as a candidate for inlining. Called by
    /// `compile_func` after counting the function's instruction count.
    #[allow(dead_code)]
    pub(crate) fn register_inline_candidate(&mut self, func_name: String) {
        self.inline_candidates.insert(func_name);
    }

    /// Mark a function as pure (no side effects, no external calls).
    /// Pure functions are eligible for both inlining and CSE.
    #[allow(dead_code)]
    pub(crate) fn mark_pure(&mut self, func_name: String) {
        self.pure_funcs.insert(func_name);
    }

    /// Count the number of instructions in a function. Returns 0 for
    /// null or external functions. This is the metric used by the
    /// inline decision.
    pub(crate) fn count_instructions_in_function(
        &self,
        func: inkwell::values::FunctionValue<'ctx>,
    ) -> u32 {
        if func
            .get_name()
            .to_str()
            .map(|s| s.is_empty())
            .unwrap_or(true)
        {
            return 0;
        }
        // External (declaration only) functions: count = 1 (the call itself)
        if func.get_linkage() == inkwell::module::Linkage::External
            && func.count_basic_blocks() == 0
        {
            return 1;
        }
        let mut count: u32 = 0;
        for bb in func.get_basic_blocks() {
            count += bb.get_instructions().count() as u32;
        }
        count
    }

    /// Reset all inline/GVN state. Called between top-level compiles
    /// so per-function caches do not leak across compilation units.
    #[allow(dead_code)]
    pub(crate) fn reset_inline_gvn_state(&mut self) {
        self.cse_cache.clear();
        self.inline_candidates.clear();
        self.pure_funcs.clear();
        self.cse_hits = 0;
        self.inline_count = 0;
    }

    /// Diagnostic accessor: number of CSE cache hits so far.
    #[allow(dead_code)]
    pub(crate) fn cse_hits(&self) -> u64 {
        self.cse_hits
    }

    /// Diagnostic accessor: number of inline decisions made.
    #[allow(dead_code)]
    pub(crate) fn inline_count(&self) -> u64 {
        self.inline_count
    }

    fn current_fn_ret_type(&self) -> BasicTypeEnum<'ctx> {
        self.current_function()
            .and_then(|f| f.get_type().get_return_type())
            .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()))
    }

    fn adjust_int_val(
        &self,
        val: BasicValueEnum<'ctx>,
        target: BasicTypeEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match (val, target) {
            (BasicValueEnum::IntValue(iv), BasicTypeEnum::IntType(ti)) => {
                let src_w = iv.get_type().get_bit_width();
                let dst_w = ti.get_bit_width();
                if src_w == dst_w {
                    Ok(iv.into())
                } else if src_w < dst_w {
                    self.builder
                        .build_int_z_extend(iv, ti, "zext")
                        .map(|v| v.into())
                        .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))
                } else {
                    self.builder
                        .build_int_truncate(iv, ti, "trunc")
                        .map(|v| v.into())
                        .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))
                }
            }
            _ => Ok(val),
        }
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
        _ty: BasicTypeEnum<'ctx>,
    ) -> Result<(), CompileError> {
        if self.shared_var_names.contains(name) {
            // Shared variable: load the heap pointer, store new value at that location
            let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            let heap_ptr = self
                .build_load(ptr_ty, alloca, &format!("{}_heap_ptr", name))?
                .into_pointer_value();
            self.build_store(heap_ptr, val)?;
        } else {
            self.build_store(alloca, val)?;
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

    /// Remove and return the most recently registered raw heap pointer from
    /// the current scope. Used to transfer ownership of a string expression
    /// result into a local variable slot.
    pub(super) fn pop_last_heap_ptr(&self) -> Option<inkwell::values::PointerValue<'ctx>> {
        if let Some(stack) = self.heap_allocs.borrow_mut().last_mut() {
            while let Some(entry) = stack.pop() {
                if let HeapEntry::Ptr(p) = entry {
                    return Some(p);
                }
            }
        }
        None
    }

    /// Track the result type of `weak_var.upgrade()` for a `let` binding.
    /// `w.upgrade()` returns `Option<T>` where `T` is the inner type of the
    /// weak reference. Updating `var_type_names`/`var_types` lets downstream
    /// method dispatch (`is_none`, `unwrap`) find the Option implementation.
    pub(super) fn track_weak_upgrade_type(&mut self, name: &str, obj: &Expr) {
        if let Expr::Ident(obj_name) = obj {
            if let Some(ty) = self.var_types.get(obj_name).cloned() {
                let inner = match ty {
                    Type::Weak(inner) | Type::WeakLocal(inner) => inner,
                    _ => return,
                };
                let inner_name = crate::core::fmt_type(&inner);
                self.var_type_names
                    .insert(name.to_string(), format!("Option<{}>", inner_name));
                self.var_types.insert(name.to_string(), Type::Option(inner));
                self.upgrade_option_vars.insert(name.to_string());
            }
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
            let free_fn = self
                .module
                .get_function("free")
                .ok_or_else(|| CompileError::LlvmError("free not declared".to_string()))?;
            for entry in scope {
                let ptr = match entry {
                    HeapEntry::Ptr(p) => p,
                    HeapEntry::Slot(gep) => {
                        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                        let loaded =
                            self.builder
                                .build_load(ptr_ty, gep, "heap_slot")
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("heap slot load error: {}", e))
                                })?;
                        loaded.into_pointer_value()
                    }
                };
                self.builder
                    .build_call(
                        free_fn,
                        &[BasicMetadataValueEnum::PointerValue(ptr)],
                        "free_heap",
                    )
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

    /// Register the element LLVM type for a `List<T>` variable so that
    /// compile_index_expr can reconstruct struct-typed elements from type-erased storage.
    pub(super) fn register_list_elem_type(&mut self, var_name: &str, decl_ty: &Type) {
        if let Type::Name(tn, args) = decl_ty {
            if tn == "List" && args.len() == 1 {
                let elem_ty = &args[0];
                if let Some(llvm_elem) = self.llvm_type_for(elem_ty) {
                    if matches!(llvm_elem, BasicTypeEnum::StructType(_)) {
                        self.list_elem_llvm_types
                            .insert(var_name.to_string(), llvm_elem);
                    }
                }
            }
        }
    }

    /// Get the full type name including generics for a variable (for list element reconstruction).
    pub(super) fn get_full_type_name(&self, ty: &Type) -> Option<String> {
        if let Type::Name(tn, args) = ty {
            if args.is_empty() {
                Some(tn.clone())
            } else {
                let inner: Vec<String> = args
                    .iter()
                    .filter_map(|a| self.get_full_type_name(a))
                    .collect();
                if inner.len() == args.len() {
                    Some(format!("{}<{}>", tn, inner.join(", ")))
                } else {
                    Some(tn.clone())
                }
            }
        } else {
            None
        }
    }

    /// G2: Find the owning type name and ordinal of an enum variant name.
    /// Returns `None` if `name` is not a variant in any registered enum type.
    fn find_variant_info(&self, name: &str) -> Option<(String, u64)> {
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

    /// G2: Find the ordinal index of an enum variant name across all registered types.
    pub(super) fn find_variant_ordinal(&self, name: &str) -> Result<u64, CompileError> {
        if let Some((_, ordinal)) = self.find_variant_info(name) {
            return Ok(ordinal);
        }
        // Built-in Result/Option variants (not present in type_defs).
        match name {
            "Ok" | "Some" => Ok(1),
            "Err" | "None" => Ok(0),
            _ => Err(CompileError::Generic(format!(
                "enum variant '{}' not found in any registered enum type definition",
                name
            ))),
        }
    }

    /// G2: Find the owning type name and ordinal of an enum variant name.
    /// Returns `None` if `name` is not a variant in any registered enum type.
    pub(super) fn find_variant_owner(&self, name: &str) -> Option<(String, u64)> {
        self.find_variant_info(name)
    }

    /// Compute the size in bytes of an LLVM type using a portable layout.
    /// This does not rely on the module data layout being set.
    pub(in crate::codegen) fn llvm_type_size_bytes(&self, ty: BasicTypeEnum<'ctx>) -> u64 {
        match ty {
            BasicTypeEnum::IntType(t) => (t.get_bit_width() / 8) as u64,
            BasicTypeEnum::FloatType(_) => 8,
            BasicTypeEnum::PointerType(_) => 8,
            BasicTypeEnum::StructType(t) => t
                .get_field_types()
                .iter()
                .map(|f| self.llvm_type_size_bytes(*f))
                .sum(),
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
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());

        // Track type name for downstream field access / inference
        if let Some(decl_ty) = ty {
            let tn = crate::core::fmt_type(decl_ty);
            self.var_type_names.insert(name.clone(), tn);
            self.var_types.insert(name.clone(), decl_ty.clone());
        } else if let Expr::Record { ty: Some(tn), .. } = init {
            self.var_type_names.insert(name.clone(), tn.clone());
        } else if let Expr::Call(callee, _) = init {
            if let Expr::Ident(fname) = callee.as_ref() {
                if let Some(fdef) = self.func_defs.get(fname) {
                    if let Some(ret_ty) = &fdef.ret {
                        let tn = crate::core::fmt_type(ret_ty);
                        self.var_type_names.insert(name.clone(), tn);
                        self.var_types.insert(name.clone(), ret_ty.clone());
                    }
                }
                // G-41: Track return types for builtins that return List<string>
                match fname.as_str() {
                    "listdir" | "walk_dir" => {
                        self.var_type_names
                            .insert(name.clone(), "List<string>".to_string());
                        self.var_types.insert(
                            name.clone(),
                            Type::Name("List".into(), vec![Type::Name("string".into(), vec![])]),
                        );
                    }
                    "str_split" => {
                        self.var_type_names
                            .insert(name.clone(), "List<string>".to_string());
                        self.var_types.insert(
                            name.clone(),
                            Type::Name("List".into(), vec![Type::Name("string".into(), vec![])]),
                        );
                    }
                    _ => {}
                }
            }
        }

        match kind {
            crate::ast::SharedKind::Shared | crate::ast::SharedKind::LocalShared => {
                // Shared reference copy: `shared q = p` where p is already shared.
                // Share the same heap allocation and retain, rather than copying the value.
                if let Expr::Ident(src_name) = init {
                    if self.shared_var_names.contains(src_name.as_str()) {
                        return self.compile_shared_ref_copy(name, src_name, vars);
                    }
                }
            }
            crate::ast::SharedKind::Weak | crate::ast::SharedKind::WeakLocal => {
                // Weak reference: init must be an existing shared variable.
                if let Expr::Ident(src_name) = init {
                    let &(src_alloca, val_ty) = vars.get(src_name).ok_or_else(|| {
                        CompileError::LlvmError(format!("weak source '{}' not found", src_name))
                    })?;
                    let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    let heap_ptr_typed = self
                        .build_load(
                            BasicTypeEnum::PointerType(ptr_ty),
                            src_alloca,
                            &format!("{}_weak_load", name),
                        )?
                        .into_pointer_value();

                    // Increment the weak refcount on the heap allocation.
                    let heap_i8 = self
                        .builder
                        .build_pointer_cast(heap_ptr_typed, i8_ptr, &format!("{}_weak_i8", name))
                        .map_err(|e| {
                            CompileError::LlvmError(format!("pointer cast error: {}", e))
                        })?;
                    let weak_retain_fn = self.get_runtime_fn("mimi_rc_weak_retain")?;
                    self.build_call(
                        weak_retain_fn,
                        &[inkwell::values::BasicMetadataValueEnum::PointerValue(
                            heap_i8,
                        )],
                        &format!("{}_weak_retain", name),
                    )?;

                    let new_alloca = self.build_alloca(ptr_ty, name)?;
                    self.build_store(new_alloca, heap_ptr_typed)?;
                    vars.insert(name.clone(), (new_alloca, val_ty));
                    self.shared_var_names.insert(name.clone());
                    // Register the weak pointer so it is released when the weak ref goes out of scope.
                    self.register_weak_var(heap_i8);
                    return Ok(());
                }
                return Err(CompileError::LlvmError(
                    "weak requires an existing shared variable as initialiser".to_string(),
                ));
            }
        }

        let mut val = self.compile_expr(init, vars)?;
        // If the initialiser returns a pointer (e.g. record literal builds an
        // alloca and returns its address), load the value first so we store the
        // actual data on the heap, not a stack pointer.
        let llvm_ty = if let BasicValueEnum::PointerValue(pv) = val {
            let ty_name = self.var_type_names.get(name.as_str()).or({
                if let Expr::Record { ty: Some(tn), .. } = init {
                    Some(tn)
                } else {
                    None
                }
            });
            let pointee_ty = ty_name
                .and_then(|tn| self.type_llvm.get(tn))
                .cloned()
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
            let loaded = self.build_load(pointee_ty, pv, &format!("{}_val", name))?;
            val = loaded;
            loaded.get_type()
        } else {
            val.get_type()
        };

        let ty_size_bytes = self.llvm_type_size_bytes(llvm_ty);
        let ty_size = self.context.i64_type().const_int(ty_size_bytes, false);
        let alloc_fn = self.get_runtime_fn("mimi_rc_alloc")?;
        let heap_raw = self
            .build_call(
                alloc_fn,
                &[inkwell::values::BasicMetadataValueEnum::IntValue(ty_size)],
                &format!("{}_rc_alloc", name),
            )?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("mimi_rc_alloc returned void".to_string()))?;

        let heap_raw_ptr = heap_raw.into_pointer_value();

        // BUG-4: mimi_rc_alloc returns NULL on allocation failure.
        // Check for null before dereferencing to prevent UB.
        let function = self
            .current_function()
            .ok_or_else(|| CompileError::LlvmError("shared let outside function".to_string()))?;
        let alloc_ok_bb = self.context.append_basic_block(function, "alloc_ok");
        let alloc_fail_bb = self.context.append_basic_block(function, "alloc_fail");
        let is_null = self
            .builder
            .build_is_null(heap_raw_ptr, "heap_is_null")
            .map_err(|e| CompileError::LlvmError(format!("is_null error: {}", e)))?;
        self.build_cond_br(is_null, alloc_fail_bb, alloc_ok_bb)?;

        // Fail path: call abort (allocation failure is unrecoverable)
        self.builder.position_at_end(alloc_fail_bb);
        let abort_fn = if let Some(f) = self.module.get_function("mimi_runtime_abort") {
            f
        } else {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let ty = self
                .context
                .void_type()
                .fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false);
            self.module.add_function(
                "mimi_runtime_abort",
                ty,
                Some(inkwell::module::Linkage::External),
            )
        };
        let msg_ptr = self
            .builder
            .build_global_string_ptr(
                &format!("shared let '{}': allocation failed", name),
                "alloc_fail_msg",
            )
            .map_err(|e| CompileError::LlvmError(format!("string error: {}", e)))?;
        self.build_call(
            abort_fn,
            &[BasicMetadataValueEnum::PointerValue(
                msg_ptr.as_pointer_value(),
            )],
            "alloc_abort",
        )?;
        self.builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("unreachable error: {}", e)))?;

        // Ok path: proceed with the allocation
        self.builder.position_at_end(alloc_ok_bb);

        let heap_ptr = self
            .builder
            .build_pointer_cast(
                heap_raw_ptr,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                &format!("{}_heap", name),
            )
            .map_err(|e| CompileError::LlvmError(format!("pointer cast error: {}", e)))?;

        self.build_store(heap_ptr, val)?;

        let alloca = self.build_alloca(
            self.context.ptr_type(inkwell::AddressSpace::default()),
            name,
        )?;
        self.build_store(alloca, heap_ptr)?;

        vars.insert(name.clone(), (alloca, llvm_ty));
        self.shared_var_names.insert(name.clone());

        let heap_i8 = self
            .builder
            .build_pointer_cast(heap_ptr, i8_ptr, &format!("{}_i8", name))
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
        let function = self
            .current_function()
            .ok_or_else(|| CompileError::LlvmError("arena outside function".to_string()))?;
        let arena_body_bb = self
            .context
            .append_basic_block(function, &format!("{}_body", label));
        let arena_cont_bb = self
            .context
            .append_basic_block(function, &format!("{}_cont", label));
        if !self.block_has_terminator() {
            self.builder
                .build_unconditional_branch(arena_body_bb)
                .map_err(|e| CompileError::LlvmError(format!("branch to {}: {}", label, e)))?;
        }
        self.builder.position_at_end(arena_body_bb);
        let saved = self.build_stacksave()?;
        // QUAL-2 fix: isolate arena-local capability scope.
        // compile_block does NOT push/pop cap_scope, so we must do it here
        // to prevent arena-local capabilities from leaking to the outer scope.
        self.push_cap_scope();
        let vars_before: std::collections::HashSet<String> = vars.keys().cloned().collect();
        self.compile_block(block, vars)?;
        for k in vars.keys().cloned().collect::<Vec<_>>() {
            if !vars_before.contains(&k) {
                vars.remove(&k);
            }
        }
        self.pop_cap_scope();
        self.build_stackrestore(saved)?;
        if !self.block_has_terminator() {
            self.builder
                .build_unconditional_branch(arena_cont_bb)
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
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let &(src_alloca, val_ty) = vars.get(src_name).ok_or_else(|| {
            CompileError::LlvmError(format!("shared source '{}' not found", src_name))
        })?;
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());

        // 1. Load the T* heap pointer from the source's alloca
        let heap_ptr_typed = self
            .builder
            .build_load(
                BasicTypeEnum::PointerType(ptr_ty),
                src_alloca,
                &format!("{}_shared_load", new_name),
            )
            .map_err(|e| CompileError::LlvmError(format!("shared load error: {}", e)))?
            .into_pointer_value();

        // 2. Cast to i8* and call mimi_rc_retain
        let heap_i8 = self
            .builder
            .build_pointer_cast(
                heap_ptr_typed,
                i8_ptr_ty,
                &format!("{}_shared_i8", new_name),
            )
            .map_err(|e| CompileError::LlvmError(format!("pointer cast error: {}", e)))?;
        let retain_fn = self
            .module
            .get_function("mimi_rc_retain")
            .ok_or_else(|| CompileError::LlvmError("mimi_rc_retain not declared".to_string()))?;
        self.builder
            .build_call(
                retain_fn,
                &[inkwell::values::BasicMetadataValueEnum::PointerValue(
                    heap_i8,
                )],
                &format!("{}_retain", new_name),
            )
            .map_err(|e| CompileError::LlvmError(format!("retain error: {}", e)))?;

        // 3. Create a new alloca for the new name and store the heap pointer
        let new_alloca = self
            .builder
            .build_alloca(ptr_ty, new_name)
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        self.builder
            .build_store(new_alloca, heap_ptr_typed)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;

        // 4. Register the i8* pointer for release on scope exit
        self.register_shared_var(heap_i8);

        // 5. Track type name and shared status
        self.shared_var_names.insert(new_name.to_string());
        if let Some(tn) = self.var_type_names.get(src_name) {
            self.var_type_names.insert(new_name.to_string(), tn.clone());
        }
        if let Some(ty) = self.var_types.get(src_name) {
            self.var_types.insert(new_name.to_string(), ty.clone());
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
        let triple_str = self.target_triple.clone().unwrap_or_else(|| {
            TargetMachine::get_default_triple()
                .as_str()
                .to_string_lossy()
                .to_string()
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
            (
                TargetMachine::get_host_cpu_name().to_string(),
                TargetMachine::get_host_cpu_features().to_string(),
            )
        };
        let reloc_mode = if self.shared {
            RelocMode::PIC
        } else {
            RelocMode::Default
        };
        let tm = target
            .create_target_machine(
                &triple_ref,
                &cpu,
                &features,
                OptimizationLevel::None,
                reloc_mode,
                CodeModel::Default,
            )
            .ok_or_else(|| {
                format!(
                    "failed to create target machine for triple '{}'",
                    triple_ref
                )
            })?;

        // Run LLVM optimization passes before codegen (opt-in via MIMI_OPT env var)
        if self.optimize {
            let options = inkwell::passes::PassBuilderOptions::create();
            self.module
                .run_passes("default<O2>", &tm, options)
                .map_err(|e| CompileError::LlvmError(format!("optimization failed: {}", e)))?;
        }

        tm.write_to_file(
            &self.module,
            inkwell::targets::FileType::Object,
            output_path,
        )
        .map_err(|e| CompileError::Io(format!("failed to write object file: {}", e)))
    }
}
