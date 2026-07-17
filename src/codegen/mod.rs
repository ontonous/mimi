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

/// Parse a type name string into a Type, supporting generics like List<T>
/// and product tuples `(A, B)`.
fn parse_inner_type(s: &str) -> crate::ast::Type {
    let s = s.trim();
    // Product tuple: (A, B, …) — track paren depth for nested tuples.
    if s.starts_with('(') && s.ends_with(')') && s.len() >= 2 {
        let args_str = &s[1..s.len() - 1];
        let mut args = Vec::new();
        let mut depth = 0i32;
        let mut start = 0usize;
        for (i, ch) in args_str.char_indices() {
            match ch {
                '<' | '(' => depth += 1,
                '>' | ')' => depth -= 1,
                ',' if depth == 0 => {
                    let part = args_str[start..i].trim();
                    if !part.is_empty() {
                        args.push(parse_inner_type(part));
                    }
                    start = i + 1;
                }
                _ => {}
            }
        }
        let remaining = args_str[start..].trim();
        if !remaining.is_empty() {
            args.push(parse_inner_type(remaining));
        }
        if !args.is_empty() {
            return crate::ast::Type::Tuple(args);
        }
    }
    if let Some(lt) = s.find('<') {
        if s.ends_with('>') {
            let base = s[..lt].trim();
            let args_str = s[lt + 1..s.len() - 1].trim();
            let mut args = Vec::new();
            let mut depth = 0i32;
            let mut start = 0usize;
            for (i, ch) in args_str.char_indices() {
                match ch {
                    '<' | '(' => depth += 1,
                    '>' | ')' => depth -= 1,
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
    /// v0.28.21 — Folded values for `comptime func` and `const` items.
    /// Populated by `fold_comptime_items` at the start of `compile_file`;
    /// consumed by the `Expr::Comptime` fold path so it does not have to
    /// re-evaluate the source. Maps the comptime item's declared name to
    /// the `interp::Value` returned by the interpreter.
    comptime_values: HashMap<String, crate::interp::Value>,
    /// v0.28.21 — Optional reference to the file currently being compiled.
    /// Held so `Expr::Comptime` block paths can construct a fresh
    /// interpreter per fold without re-borrowing the original argument.
    comptime_file: Option<std::rc::Rc<crate::ast::File>>,
    trait_defs: HashMap<String, crate::ast::TraitDef>,
    type_impls: HashMap<String, HashMap<String, Vec<FuncDef>>>,
    /// Generic type arguments for each type that has trait impls.
    /// For `impl<T> ListExt<T> for List<T>`, this stores `"List" → [T]`.
    impl_type_args: HashMap<String, Vec<crate::ast::Type>>,
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
    /// Maps variable names to the inner result type of `Future<T>` for async fn calls.
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
    /// Inferred Mimi type names for arguments of the current `print`/`println` call.
    /// Used to choose the correct runtime list-to-string helper (string vs i32 elements).
    pending_print_arg_types: Vec<String>,
    /// Inferred Mimi element type name for the current `push(list, elem)` call.
    /// Used so that nested lists and other struct elements are heap-copied before
    /// their pointer is stored, preventing stack-use-after-return.
    pending_push_elem_type: Option<String>,
    /// When compiling a typed list literal (`let xs: List<T> = [...]`), the
    /// element type `T` so Result/Option constructors can be inflated to a
    /// uniform layout before heap packing.
    pending_list_elem_type: Option<Type>,
    pending_to_string_is_any: bool,
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
    /// Candidate set for small-function inlining (populated during
    /// `compile_func` and consulted by call-site dispatch).
    /// Names of functions determined to be pure (no side effects).

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
    /// Flow definitions keyed by flow name — used to compile transitions
    /// as ordinary functions and dispatch `Flow::transition(...)` calls.
    flow_defs: HashMap<String, crate::ast::FlowDef>,
    /// Canonical transitions from CheckedProgram for fail-closed dispatch.
    resolved_transitions: Option<HashMap<(String, String, String), Vec<String>>>,
    resolved_fallback_transitions: Option<std::collections::HashSet<(String, String, String)>>,
    resolved_ffi_pinned_transitions: Option<std::collections::HashSet<(String, String, String)>>,
    resolved_transition_param_arity: Option<HashMap<(String, String, String), usize>>,
    resolved_transition_params: Option<HashMap<(String, String, String), Vec<(String, String)>>>,
    /// Function directory from CheckedProgram: qualified_name -> arity.
    resolved_function_arity: Option<HashMap<String, usize>>,
    resolved_function_effects: Option<HashMap<String, Vec<String>>>,
    resolved_function_returns: Option<HashMap<String, String>>,
    resolved_function_params: Option<HashMap<String, Vec<(String, String)>>>,
    resolved_comptime_functions: Option<std::collections::HashSet<String>>,
    /// Session names from CheckedProgram.
    resolved_sessions: Option<std::collections::HashSet<String>>,
    resolved_session_displays: Option<HashMap<String, String>>,
    /// Protocol names from CheckedProgram.
    resolved_protocols: Option<std::collections::HashSet<String>>,
    resolved_protocol_transitions: Option<HashMap<String, Vec<(String, String, String)>>>,
    resolved_protocol_payloads: Option<HashMap<String, String>>,
    /// Actor method directory from CheckedProgram.
    resolved_actors: Option<HashMap<String, Vec<String>>>,
    resolved_actor_method_signatures: Option<HashMap<String, (usize, String)>>,
    resolved_actor_fields: Option<HashMap<String, Vec<(String, String, bool)>>>,
    resolved_method_signatures: Option<HashMap<String, (usize, String)>>,
    /// Capability names from CheckedProgram.
    resolved_capabilities: Option<std::collections::HashSet<String>>,
    resolved_capability_combined: Option<HashMap<String, String>>,
    /// Constant names from CheckedProgram.
    resolved_constants: Option<std::collections::HashSet<String>>,
    resolved_constant_values: Option<HashMap<String, (Option<String>, String)>>,
    /// Trait method directories from CheckedProgram.
    resolved_traits: Option<HashMap<String, Vec<String>>>,
    /// Impl method directories from CheckedProgram: "Trait:for:Type" -> methods.
    resolved_impls: Option<HashMap<String, Vec<String>>>,
    /// Ownership ledger owners from CheckedProgram.
    resolved_ownership_owners: Option<std::collections::HashSet<String>>,
    resolved_ownership_summaries: Option<HashMap<String, (usize, usize, usize, usize, usize, bool)>>,
    resolved_ownership_resources: Option<HashMap<String, Vec<String>>>,
    resolved_backend_requirements: Option<Vec<(String, String)>>,
    resolved_node_meta_count: Option<usize>,
    resolved_node_meta_paths: Option<std::collections::HashSet<String>>,
    resolved_node_meta_precision: Option<HashMap<String, String>>,
    /// Type definition kinds from CheckedProgram.
    resolved_type_kinds: Option<HashMap<String, String>>,
    resolved_type_fields: Option<HashMap<String, Vec<(String, String)>>>,
    resolved_type_variants: Option<HashMap<String, Vec<(String, Option<String>)>>>,
    resolved_type_aliases: Option<HashMap<String, String>>,
    /// Extern function names from CheckedProgram.
    resolved_extern_funcs: Option<std::collections::HashSet<String>>,
    resolved_extern_abis: Option<HashMap<String, String>>,
    resolved_extern_signatures: Option<HashMap<String, (usize, String)>>,
    resolved_extern_no_panic: Option<std::collections::HashSet<String>>,
    resolved_extern_unsafe: Option<std::collections::HashSet<String>>,
    resolved_call_sites: Option<HashMap<String, (String, String, usize, Option<usize>, Vec<String>, Option<String>, String)>>,
    /// Flow mailbox depths from CheckedProgram.
    resolved_mailbox_depths: Option<HashMap<String, usize>>,
    resolved_flow_state_payloads: Option<HashMap<String, Vec<(String, String)>>>,
    resolved_flow_states: Option<HashMap<String, Vec<String>>>,
    resolved_flow_events: Option<HashMap<String, Vec<String>>>,
    resolved_item_kinds: Option<HashMap<String, String>>,
    /// Persistent field sets from CheckedProgram.
    resolved_persistent_fields: Option<HashMap<String, Vec<String>>>,
    resolved_transactional_fields: Option<HashMap<String, Vec<String>>>,
    resolved_metadata_shadow_fields: Option<HashMap<String, Vec<String>>>,
    resolved_flow_protocols: Option<HashMap<String, Vec<String>>>,
    /// v0.29.24: process spawn quota from first @max_children(N) (None = unlimited).
    max_children: Option<usize>,
}

type VarEntry<'ctx> = (inkwell::values::PointerValue<'ctx>, BasicTypeEnum<'ctx>);

/// Entries tracked for scope-exit heap cleanup.
/// `Ptr(ptr)` = a raw heap pointer to free directly.
/// `Slot(base, struct_ty, field)` = an alloca of type `struct_ty` (`base`) and
/// the field index that holds the heap pointer. At cleanup a fresh GEP is
/// emitted from `base` in the current block. `base` must dominate the cleanup
/// point; call sites therefore allocate it in the function entry block.
/// The struct's ptr field is also null-initialized at the entry block.
enum HeapEntry<'ctx> {
    Ptr(inkwell::values::PointerValue<'ctx>),
    Slot(
        inkwell::values::PointerValue<'ctx>,
        inkwell::types::StructType<'ctx>,
        u32,
    ),
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
            // v0.28.21 — cache of `comptime func` and `const` results evaluated
            // via the interpreter during `compile_file`. Used to fold
            // `comptime { ... }` blocks and `comptime func name()` call sites
            // to LLVM constants instead of erroring.
            comptime_values: HashMap::new(),
            comptime_file: None,
            in_parasteps: false,
            parasteps_future_ptrs: Vec::new(),
            trait_defs: HashMap::new(),
            type_impls: HashMap::new(),
            impl_type_args: HashMap::new(),
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
            pending_print_arg_types: Vec::new(),
            pending_push_elem_type: None,
            pending_list_elem_type: None,
            pending_to_string_is_any: false,
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
            // v0.28.19 actor concurrency
            actor_names: std::collections::HashSet::new(),
            actor_method_ids: HashMap::new(),
            actor_defs: HashMap::new(),
            // v0.29.9 flow transitions
            flow_defs: HashMap::new(),
            resolved_transitions: None,
            resolved_fallback_transitions: None,
            resolved_ffi_pinned_transitions: None,
            resolved_transition_param_arity: None,
            resolved_transition_params: None,
            resolved_function_arity: None,
            resolved_function_effects: None,
            resolved_function_returns: None,
            resolved_function_params: None,
            resolved_comptime_functions: None,
            resolved_sessions: None,
            resolved_session_displays: None,
            resolved_protocols: None,
            resolved_protocol_transitions: None,
            resolved_protocol_payloads: None,
            resolved_actors: None,
            resolved_actor_method_signatures: None,
            resolved_actor_fields: None,
            resolved_method_signatures: None,
            resolved_capabilities: None,
            resolved_capability_combined: None,
            resolved_constants: None,
            resolved_constant_values: None,
            resolved_traits: None,
            resolved_impls: None,
            resolved_ownership_owners: None,
            resolved_ownership_summaries: None,
            resolved_ownership_resources: None,
            resolved_backend_requirements: None,
            resolved_node_meta_count: None,
            resolved_node_meta_paths: None,
            resolved_node_meta_precision: None,
            resolved_type_kinds: None,
            resolved_type_fields: None,
            resolved_type_variants: None,
            resolved_type_aliases: None,
            resolved_extern_funcs: None,
            resolved_extern_abis: None,
            resolved_extern_signatures: None,
            resolved_extern_no_panic: None,
            resolved_extern_unsafe: None,
            resolved_call_sites: None,
            resolved_mailbox_depths: None,
            resolved_flow_state_payloads: None,
            resolved_flow_states: None,
            resolved_flow_events: None,
            resolved_item_kinds: None,
            resolved_persistent_fields: None,
            resolved_transactional_fields: None,
            resolved_metadata_shadow_fields: None,
            resolved_flow_protocols: None,
            max_children: None,
        }
    }

    pub(crate) fn resolved_protocol_transitions(
        &self,
        protocol: &str,
    ) -> Option<Vec<(String, String, String)>> {
        self.resolved_protocol_transitions
            .as_ref()
            .and_then(|map| map.get(protocol).cloned())
    }

    pub(crate) fn resolved_protocol_payload(
        &self,
        protocol: &str,
        state: &str,
    ) -> Option<String> {
        self.resolved_protocol_payloads
            .as_ref()
            .and_then(|map| map.get(&format!("{protocol}.{state}")).cloned())
    }

    pub(crate) fn resolved_method_signature(&self, key: &str) -> Option<(usize, String)> {
        self.resolved_method_signatures
            .as_ref()
            .and_then(|map| map.get(key).cloned())
    }

    pub(crate) fn resolved_actor_method_signature(
        &self,
        actor: &str,
        method: &str,
    ) -> Option<(usize, String)> {
        self.resolved_actor_method_signatures
            .as_ref()
            .and_then(|map| map.get(&format!("{actor}.{method}")).cloned())
    }

    pub(crate) fn resolved_extern_signature(&self, name: &str) -> Option<(usize, String)> {
        self.resolved_extern_signatures
            .as_ref()
            .and_then(|map| map.get(name).cloned())
    }

    pub(crate) fn resolved_call_return_type(&self, callee: &str) -> Option<String> {
        self.resolved_call_sites.as_ref().and_then(|map| {
            map.values()
                .find(|(_, name, _, _, _, _, _)| name == callee)
                .and_then(|(_, _, _, _, _, ret, _)| ret.clone())
        })
    }

    pub(crate) fn has_resolved_call_with_effect(&self, callee: &str, effect: &str) -> bool {
        self.resolved_call_sites.as_ref().is_some_and(|map| {
            map.values().any(|(_, name, _, _, effects, _, _)| {
                name == callee && effects.iter().any(|e| e == effect)
            })
        })
    }

    pub(crate) fn resolved_call_arity_mismatches(&self) -> usize {
        self.resolved_call_sites
            .as_ref()
            .map(|map| {
                map.values()
                    .filter(|(_, _, argc, expected, _, _, _)| {
                        expected.map(|exp| exp != *argc).unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    pub(crate) fn has_resolved_call_to(&self, callee: &str) -> bool {
        self.resolved_call_sites.as_ref().is_some_and(|map| {
            map.values().any(|(_, name, _, _, _, _, _)| name == callee)
        })
    }

    pub(crate) fn resolved_constant_value(
        &self,
        name: &str,
    ) -> Option<(Option<String>, String)> {
        self.resolved_constant_values
            .as_ref()
            .and_then(|map| map.get(name).cloned())
    }

    pub(crate) fn resolved_extern_abi(&self, name: &str) -> Option<&str> {
        self.resolved_extern_abis
            .as_ref()
            .and_then(|map| map.get(name).map(String::as_str))
    }

    pub(crate) fn resolved_function_params(
        &self,
        name: &str,
    ) -> Option<Vec<(String, String)>> {
        self.resolved_function_params
            .as_ref()
            .and_then(|map| map.get(name).cloned())
    }

    pub(crate) fn resolved_function_return_type(&self, name: &str) -> Option<&str> {
        self.resolved_function_returns
            .as_ref()
            .and_then(|map| map.get(name).map(String::as_str))
    }

    pub(crate) fn is_resolved_comptime_function(&self, name: &str) -> bool {
        self.resolved_comptime_functions
            .as_ref()
            .is_some_and(|set| set.contains(name))
    }

    pub(crate) fn resolved_function_effects(&self, name: &str) -> Option<Vec<String>> {
        self.resolved_function_effects
            .as_ref()
            .and_then(|map| map.get(name).cloned())
    }

    pub(crate) fn resolved_transition_targets(
        &self,
        flow: &str,
        event: &str,
        source: &str,
    ) -> Option<Vec<String>> {
        self.resolved_transitions.as_ref().and_then(|map| {
            map.get(&(flow.to_string(), event.to_string(), source.to_string()))
                .cloned()
        })
    }

    pub(crate) fn is_resolved_fallback_transition(
        &self,
        flow: &str,
        event: &str,
        source: &str,
    ) -> bool {
        self.resolved_fallback_transitions.as_ref().is_some_and(|set| {
            set.contains(&(flow.to_string(), event.to_string(), source.to_string()))
        })
    }

    pub(crate) fn resolved_transition_param_arity(
        &self,
        flow: &str,
        event: &str,
        source: &str,
    ) -> Option<usize> {
        self.resolved_transition_param_arity.as_ref().and_then(|map| {
            map.get(&(flow.to_string(), event.to_string(), source.to_string()))
                .copied()
        })
    }

    pub(crate) fn resolved_transition_params(
        &self,
        flow: &str,
        event: &str,
        source: &str,
    ) -> Option<Vec<(String, String)>> {
        self.resolved_transition_params.as_ref().and_then(|map| {
            map.get(&(flow.to_string(), event.to_string(), source.to_string()))
                .cloned()
        })
    }

    pub(crate) fn resolved_type_fields(
        &self,
        name: &str,
    ) -> Option<Vec<(String, String)>> {
        self.resolved_type_fields
            .as_ref()
            .and_then(|map| map.get(name).cloned())
    }

    pub(crate) fn resolved_type_variants(
        &self,
        name: &str,
    ) -> Option<Vec<(String, Option<String>)>> {
        self.resolved_type_variants
            .as_ref()
            .and_then(|map| map.get(name).cloned())
    }

    pub(crate) fn resolved_type_alias_of(&self, name: &str) -> Option<&str> {
        self.resolved_type_aliases
            .as_ref()
            .and_then(|map| map.get(name).map(String::as_str))
    }

    pub(crate) fn resolved_session_display(&self, name: &str) -> Option<&str> {
        self.resolved_session_displays
            .as_ref()
            .and_then(|map| map.get(name).map(String::as_str))
    }

    pub(crate) fn resolved_capability_combined_with(&self, name: &str) -> Option<&str> {
        self.resolved_capability_combined
            .as_ref()
            .and_then(|map| map.get(name).map(String::as_str))
    }

    pub(crate) fn resolved_flow_state_payload(
        &self,
        flow: &str,
        state: &str,
    ) -> Option<Vec<(String, String)>> {
        self.resolved_flow_state_payloads
            .as_ref()
            .and_then(|map| map.get(&format!("{flow}.{state}")).cloned())
    }

    pub(crate) fn resolved_actor_fields(
        &self,
        actor: &str,
    ) -> Option<Vec<(String, String, bool)>> {
        self.resolved_actor_fields
            .as_ref()
            .and_then(|map| map.get(actor).cloned())
    }

    pub(crate) fn resolved_backend_requirements(&self) -> Option<&[(String, String)]> {
        self.resolved_backend_requirements.as_ref().map(Vec::as_slice)
    }

    pub(crate) fn resolved_node_meta_count(&self) -> Option<usize> {
        self.resolved_node_meta_count
    }

    pub(crate) fn has_resolved_node_meta_path(&self, path: &str) -> bool {
        self.resolved_node_meta_paths
            .as_ref()
            .is_some_and(|set| set.contains(path))
    }

    pub(crate) fn resolved_node_meta_precision(&self, path: &str) -> Option<&str> {
        self.resolved_node_meta_precision
            .as_ref()
            .and_then(|map| map.get(path).map(String::as_str))
    }

    pub(crate) fn requires_resolved_capability(&self, capability: &str) -> bool {
        self.resolved_backend_requirements.as_ref().is_some_and(|reqs| {
            reqs.iter().any(|(cap, _)| cap == capability)
        })
    }

    pub(crate) fn resolved_ownership_resources(&self, owner: &str) -> Option<Vec<String>> {
        self.resolved_ownership_resources
            .as_ref()
            .and_then(|map| map.get(owner).cloned())
    }

    pub(crate) fn is_resolved_extern_no_panic(&self, name: &str) -> bool {
        self.resolved_extern_no_panic
            .as_ref()
            .is_some_and(|set| set.contains(name))
    }

    pub(crate) fn is_resolved_extern_unsafe(&self, name: &str) -> bool {
        self.resolved_extern_unsafe
            .as_ref()
            .is_some_and(|set| set.contains(name))
    }

    pub(crate) fn resolved_flow_states(&self, flow: &str) -> Option<Vec<String>> {
        self.resolved_flow_states
            .as_ref()
            .and_then(|map| map.get(flow).cloned())
    }

    pub(crate) fn resolved_flow_events(&self, flow: &str) -> Option<Vec<String>> {
        self.resolved_flow_events
            .as_ref()
            .and_then(|map| map.get(flow).cloned())
    }

    pub(crate) fn resolved_item_kind(&self, name: &str) -> Option<&str> {
        self.resolved_item_kinds
            .as_ref()
            .and_then(|map| map.get(name).map(String::as_str))
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
    /// Build an `alloca` in the function entry block so it dominates every use.
    ///
    /// Always using the entry block is required once helpers like
    /// `malloc_or_abort` split the current insert block: a mid-function
    /// alloca would not dominate `register_heap_slot`'s entry-block null
    /// init (and free paths in other blocks), producing invalid IR such as
    /// GEP of `%s` before `%s = alloca`.
    pub(super) fn build_alloca<T: inkwell::types::BasicType<'ctx>>(
        &self,
        ty: T,
        name: &str,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        self.build_entry_alloca(ty, name)
    }

    /// Build an `alloca` in the function's entry block so it dominates all uses.
    /// This is used for heap-owning struct slots that need to be freed at scope
    /// exits that may live in a different basic block.
    pub(super) fn build_entry_alloca<T: inkwell::types::BasicType<'ctx>>(
        &self,
        ty: T,
        name: &str,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        let function = self
            .current_function()
            .ok_or_else(|| CompileError::LlvmError("alloca outside function".to_string()))?;
        let entry_bb = function
            .get_first_basic_block()
            .ok_or_else(|| CompileError::LlvmError("function has no entry block".to_string()))?;
        let saved = self.builder.get_insert_block();
        // Place new allocas at the *start* of the entry block so they dominate
        // any early null-init / free that may already have been emitted later
        // in entry (e.g. heap_slot_null_init after a previous register).
        if let Some(first_inst) = entry_bb.get_first_instruction() {
            self.builder.position_before(&first_inst);
        } else {
            self.builder.position_at_end(entry_bb);
        }
        let alloca = self.builder.build_alloca(ty, name).map_err(|e| {
            CompileError::LlvmError(format!("entry alloca error ({}): {}", name, e))
        })?;
        if let Some(saved_bb) = saved {
            self.builder.position_at_end(saved_bb);
        }
        Ok(alloca)
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

    /// B4: Call `malloc` and check the return value for NULL.
    ///
    /// On NULL (OOM), calls `mimi_runtime_abort` with a message and the
    /// resulting block is marked `unreachable`.  On success, positions the
    /// builder in the `ok` block and returns the non-null pointer.
    pub(super) fn malloc_or_abort(
        &self,
        size: inkwell::values::IntValue<'ctx>,
        name: &str,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        let malloc_fn = self.get_runtime_fn("malloc")?;
        let ptr = self
            .builder
            .build_call(
                malloc_fn,
                &[BasicMetadataValueEnum::IntValue(size)],
                &format!("{}_malloc", name),
            )
            .map_err(|e| CompileError::LlvmError(format!("malloc call error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("malloc returned void".into()))?
            .into_pointer_value();

        // NULL check: if ptr == null, abort
        let is_null = self
            .builder
            .build_is_null(ptr, &format!("{}_is_null", name))
            .map_err(|e| CompileError::LlvmError(format!("is_null error: {}", e)))?;
        let current_fn = self.current_function().ok_or_else(|| {
            CompileError::LlvmError("no current function for malloc_or_abort".into())
        })?;
        let ok_bb = self
            .context
            .append_basic_block(current_fn, &format!("{}_ok", name));
        let err_bb = self
            .context
            .append_basic_block(current_fn, &format!("{}_oom", name));
        self.builder
            .build_conditional_branch(is_null, err_bb, ok_bb)
            .map_err(|e| CompileError::LlvmError(format!("cond_br error: {}", e)))?;

        // Error block: call abort
        self.builder.position_at_end(err_bb);
        let abort_fn = self.get_or_declare_abort_fn();
        let msg = self
            .builder
            .build_global_string_ptr("out of memory", &format!("{}_oom_msg", name))
            .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
        self.build_call(
            abort_fn,
            &[BasicMetadataValueEnum::PointerValue(msg.as_pointer_value())],
            &format!("{}_oom_abort", name),
        )?;
        // SAFETY: mimi_runtime_abort is noreturn; this block is unreachable.
        self.builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("unreach: {}", e)))?;

        // Continue in ok block
        self.builder.position_at_end(ok_bb);
        Ok(ptr)
    }

    /// B4 companion: Call `realloc` and abort on NULL (OOM).
    ///
    /// Same control-flow shape as [`malloc_or_abort`]: on NULL, call
    /// `mimi_runtime_abort` and mark the block unreachable; on success,
    /// continue in the ok block with a non-null pointer.
    ///
    /// SAFETY: callers must not pass `size == 0` when they still need a live
    /// allocation (use free+null instead — see list `pop` CG-H3).
    pub(super) fn realloc_or_abort(
        &self,
        old_ptr: inkwell::values::PointerValue<'ctx>,
        size: inkwell::values::IntValue<'ctx>,
        name: &str,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        let realloc_fn = self.get_runtime_fn("realloc")?;
        let ptr = self
            .builder
            .build_call(
                realloc_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(old_ptr),
                    BasicMetadataValueEnum::IntValue(size),
                ],
                &format!("{}_realloc", name),
            )
            .map_err(|e| CompileError::LlvmError(format!("realloc call error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("realloc returned void".into()))?
            .into_pointer_value();

        let is_null = self
            .builder
            .build_is_null(ptr, &format!("{}_is_null", name))
            .map_err(|e| CompileError::LlvmError(format!("is_null error: {}", e)))?;
        let current_fn = self.current_function().ok_or_else(|| {
            CompileError::LlvmError("no current function for realloc_or_abort".into())
        })?;
        let ok_bb = self
            .context
            .append_basic_block(current_fn, &format!("{}_ok", name));
        let err_bb = self
            .context
            .append_basic_block(current_fn, &format!("{}_oom", name));
        self.builder
            .build_conditional_branch(is_null, err_bb, ok_bb)
            .map_err(|e| CompileError::LlvmError(format!("cond_br error: {}", e)))?;

        self.builder.position_at_end(err_bb);
        let abort_fn = self.get_or_declare_abort_fn();
        let msg = self
            .builder
            .build_global_string_ptr("out of memory (realloc)", &format!("{}_oom_msg", name))
            .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
        self.build_call(
            abort_fn,
            &[BasicMetadataValueEnum::PointerValue(msg.as_pointer_value())],
            &format!("{}_oom_abort", name),
        )?;
        // SAFETY: mimi_runtime_abort is noreturn; this block is unreachable.
        self.builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("unreach: {}", e)))?;

        self.builder.position_at_end(ok_bb);
        Ok(ptr)
    }

    /// v0.29.32: Get or declare the `mimi_wall_clock_ms` runtime function.
    /// Returns i64 (milliseconds since UNIX epoch).
    pub(super) fn get_or_declare_wall_clock_fn(&self) -> inkwell::values::FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("mimi_wall_clock_ms") {
            return f;
        }
        let ty = self.context.i64_type().fn_type(&[], false);
        self.module.add_function(
            "mimi_wall_clock_ms",
            ty,
            Some(inkwell::module::Linkage::External),
        )
    }

    /// v0.29.32: Get or declare `mimi_runtime_abort` (returns !).
    pub(super) fn get_or_declare_abort_fn(&self) -> inkwell::values::FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("mimi_runtime_abort") {
            return f;
        }
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
    }

    /// v0.29.43: Get or declare `mimi_pinned_fault` runtime function.
    /// Sets a thread-local pending Fault flag and returns 1 (non-zero = pending).
    pub(super) fn get_or_declare_pinned_fault_fn(&self) -> inkwell::values::FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("mimi_pinned_fault") {
            return f;
        }
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let ty = self
            .context
            .i64_type()
            .fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false);
        self.module.add_function(
            "mimi_pinned_fault",
            ty,
            Some(inkwell::module::Linkage::External),
        )
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

    /// If a function returns a struct by value but the value we have is a
    /// pointer to that struct (e.g. a tuple/record alloca), load it so the
    /// return instruction sees the correct type.
    pub(super) fn load_return_value_if_needed(
        &self,
        val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        if let BasicValueEnum::PointerValue(pv) = val {
            let ret_type = self.current_fn_ret_type().unwrap_or_else(|| {
                // NOTE: fallback to `i64` when no function context (top-level
                // expressions or test harness). `i64` is a safe default for the
                // scalar-skip path — the branch below only matters when the
                // return type is a StructType (tuple/record/string alloca → by-value
                // load). With `i64` fallback we skip the load, which is correct
                // because there is no struct to load.
                BasicTypeEnum::IntType(self.context.i64_type())
            });
            if let BasicTypeEnum::StructType(sty) = ret_type {
                // Tuple/record/string allocas are emitted as pointers; a function
                // returning the corresponding struct by value needs the loaded
                // aggregate, not the alloca pointer.
                let _ = sty;
                return self.build_load(sty, pv, "ret_load");
            }
        }
        Ok(val)
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
                    // A1: use s_extend for signed integers (width > 1),
                    // z_extend for bool (i1 — sign bit would make true = -1).
                    if src_w == 1 {
                        self.builder
                            .build_int_z_extend(iv, ti, "zext")
                            .map(|v| v.into())
                            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))
                    } else {
                        self.builder
                            .build_int_s_extend(iv, ti, "sext")
                            .map(|v| v.into())
                            .map_err(|e| CompileError::LlvmError(format!("sext error: {}", e)))
                    }
                } else {
                    self.builder
                        .build_int_truncate(iv, ti, "trunc")
                        .map(|v| v.into())
                        .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))
                }
            }
            (BasicValueEnum::IntValue(iv), BasicTypeEnum::FloatType(ft)) => self
                .builder
                .build_signed_int_to_float(iv, ft, "sitofp")
                .map(|v| v.into())
                .map_err(|e| CompileError::LlvmError(format!("sitofp error: {}", e))),
            (BasicValueEnum::FloatValue(fv), BasicTypeEnum::IntType(ti)) => self
                .builder
                .build_float_to_signed_int(fv, ti, "fptosi")
                .map(|v| v.into())
                .map_err(|e| CompileError::LlvmError(format!("fptosi error: {}", e))),
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
        ty: BasicTypeEnum<'ctx>,
    ) -> Result<(), CompileError> {
        // Adjust integer width to match the alloca's declared type.
        // After A1 restoration, i32 variables have i32 allocas, but expressions
        // like `x + 1` produce i64 results that must be truncated before store.
        let val = match (val, ty) {
            (BasicValueEnum::IntValue(iv), BasicTypeEnum::IntType(slot_it)) => {
                let val_bw = iv.get_type().get_bit_width();
                let slot_bw = slot_it.get_bit_width();
                if val_bw == slot_bw {
                    val
                } else if val_bw > slot_bw {
                    self.builder
                        .build_int_truncate(iv, slot_it, &format!("{}_assign_trunc", name))
                        .map_err(|e| CompileError::LlvmError(format!("assign trunc: {}", e)))?
                        .into()
                } else {
                    self.builder
                        .build_int_s_extend(iv, slot_it, &format!("{}_assign_sext", name))
                        .map_err(|e| CompileError::LlvmError(format!("assign sext: {}", e)))?
                        .into()
                }
            }
            _ => val,
        };
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
    ///
    /// NOTE: callers that need null-initialised slot for safe `free(null)` on
    /// never-allocated paths must ensure the slot is null-initialised at the
    /// entry block BEFORE any conditional store.  The current simple
    /// implementation pushes the raw pointer to the stack; entry-block alloca
    /// + null-init is added by a future refactor (the existing Ptr→PtrSlot
    ///   transition is partially in place for the slot-load based consumers).
    pub(super) fn register_heap_alloc(&self, ptr: inkwell::values::PointerValue<'ctx>) {
        let mut guard = self.heap_allocs.borrow_mut();
        if let Some(stack) = guard.last_mut() {
            stack.push(HeapEntry::Ptr(ptr));
        } else {
            // audit (MEDIUM): no active scope — create one as a safety net
            // so the allocation does not leak silently. The caller may have
            // a codegen ordering bug (alloc before scope push), but we
            // recover gracefully by providing the scope. Log a warning so
            // the underlying bug is visible during development.
            eprintln!(
                "[mimi codegen] warning: register_heap_alloc with no active scope \
                 (codegen ordering bug); creating recovery scope"
            );
            guard.push(vec![HeapEntry::Ptr(ptr)]);
        }
    }

    /// Register an entry-alloca struct slot whose loaded value should be freed at
    /// scope exit. `field` is the index of the pointer field inside the struct.
    /// At free time, a fresh GEP is emitted from `base` in the current block,
    /// avoiding dominance issues.
    /// NOTE: null-initialisation is NOT done here — the slot must have been
    /// stored to (or covered by `register_heap_alloc`) **before** registration
    /// for all paths that reach this registration.  Scope-local cleanup runs
    /// inside the block (before merge), so the stored value is always valid.
    pub(super) fn register_heap_slot(
        &self,
        base: inkwell::values::PointerValue<'ctx>,
        struct_ty: inkwell::types::StructType<'ctx>,
        field: u32,
    ) {
        // Null-initialise the pointer field in the entry block so that
        // free_heap_allocs on a never-allocated path is a safe no-op free(null).
        self.emit_null_field_store_at_entry(base, struct_ty, field);
        let mut guard = self.heap_allocs.borrow_mut();
        if let Some(stack) = guard.last_mut() {
            stack.push(HeapEntry::Slot(base, struct_ty, field));
        } else {
            mimi_debug_assert!(false, "register_heap_slot called with no active scope");
            guard.push(vec![HeapEntry::Slot(base, struct_ty, field)]);
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

    /// Register a heap slot in the root (function-level) scope so that it
    /// survives intermediate scope exits (e.g. loop body blocks). Used for
    /// string variable assignments where the heap allocation must outlive the
    /// current block scope.
    pub(super) fn register_heap_slot_root(
        &self,
        base: inkwell::values::PointerValue<'ctx>,
        struct_ty: inkwell::types::StructType<'ctx>,
        field: u32,
    ) {
        if let Some(scopes) = self.heap_allocs.borrow_mut().first_mut() {
            scopes.push(HeapEntry::Slot(base, struct_ty, field));
        }
    }

    /// Null-initialise a struct field (pointer-typed) in the entry block,
    /// immediately after the struct's alloca instruction.  This guarantees
    /// that field loads in the matchcont cleanup block see null when the
    /// allocating arm was never taken, making free(null) a safe no-op.
    fn emit_null_field_store_at_entry(
        &self,
        base: inkwell::values::PointerValue<'ctx>,
        struct_ty: inkwell::types::StructType<'ctx>,
        field: u32,
    ) {
        let saved = self.builder.get_insert_block();
        if let Some(f) = self.current_function() {
            if let Some(entry_bb) = f.get_first_basic_block() {
                let null_ptr = self
                    .context
                    .ptr_type(inkwell::AddressSpace::default())
                    .const_null();
                // Insert IMMEDIATELY AFTER the base alloca when possible so the
                // GEP/store never precedes the alloca definition (use-before-def).
                // Falling back to "after first entry instruction" is unsafe if
                // `base` is a later alloca — prefer base's own instruction.
                if let Some(base_inst) = base.as_instruction() {
                    if let Some(next) = base_inst.get_next_instruction() {
                        self.builder.position_before(&next);
                    } else {
                        // Alloca is last in its block; append after it.
                        self.builder.position_at_end(base_inst.get_parent().unwrap_or(entry_bb));
                    }
                } else if let Some(first_inst) = entry_bb.get_first_instruction() {
                    // Non-instruction base (e.g. argument): after first entry inst.
                    if let Some(next) = first_inst.get_next_instruction() {
                        self.builder.position_before(&next);
                    } else {
                        self.builder.position_at_end(entry_bb);
                    }
                } else {
                    self.builder.position_at_end(entry_bb);
                }
                // CRITICAL #11 fix: previously errors from build_struct_gep and
                // build_store were silently swallowed by .ok() / let _ =. This
                // could leave heap slots uninitialized, causing UB in generated
                // code. Now we log a compile error diagnostic instead of
                // silently continuing.
                match self
                    .gep()
                    .build_struct_gep(struct_ty, base, field, "heap_slot_null_init")
                {
                    Ok(gep_val) => {
                        if let Err(e) = self.builder.build_store(gep_val, null_ptr) {
                            // Use mimi_assert-style: log but don't panic, as
                            // this is a best-effort null-init for safety.
                            eprintln!(
                                "[mimi codegen] WARN: build_store failed in null-init: {}",
                                e
                            );
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "[mimi codegen] WARN: build_struct_gep failed in null-init: {}",
                            e
                        );
                    }
                }
                if let Some(saved_bb) = saved {
                    self.builder.position_at_end(saved_bb);
                }
            }
        }
    }

    /// G10: Push a new scope level for heap allocations.
    /// Takes &self (not &mut self) because builtins use &self.
    pub(super) fn push_heap_scope(&self) {
        self.heap_allocs.borrow_mut().push(Vec::new());
    }

    /// G10: Pop scope level and emit `free(ptr)` for each registered heap allocation.
    ///
    /// For all entry types the heap pointer is loaded from an entry-block alloca
    /// (null-initialized at the entry block), so the slot always dominates the
    /// cleanup point. The null guarantee ensures that `free` on a never-allocated
    /// path calls free(null), which is a C-library no-op.
    pub(super) fn free_heap_allocs(&mut self) -> Result<(), CompileError> {
        if let Some(scope) = self.heap_allocs.borrow_mut().pop() {
            let free_fn = self
                .module
                .get_function("free")
                .ok_or_else(|| CompileError::LlvmError("free not declared".to_string()))?;
            for entry in scope {
                let ptr = match entry {
                    HeapEntry::Ptr(p) => p,
                    HeapEntry::Slot(base, struct_ty, field) => {
                        let gep = self
                            .gep()
                            .build_struct_gep(struct_ty, base, field, "heap_slot_gep")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("heap slot gep error: {}", e))
                            })?;
                        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                        self.builder
                            .build_load(ptr_ty, gep, "heap_slot")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("heap slot load error: {}", e))
                            })?
                            .into_pointer_value()
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
    fn current_fn_ret_type(&self) -> Option<BasicTypeEnum<'ctx>> {
        self.current_function()
            .and_then(|f| f.get_type().get_return_type())
    }

    pub(super) fn llvm_type_for(&self, ty: &crate::ast::Type) -> Option<BasicTypeEnum<'ctx>> {
        use crate::ast::Type;
        match ty {
            Type::Name(name, args) if args.is_empty() => {
                if let Some(llvm) = self.type_llvm.get(name) {
                    return Some(*llvm);
                }
                crate::codegen::types::mimi_type_to_llvm(self.context, ty)
            }
            // Option/Result of named records must use type_llvm for the payload
            // slot — mimi_type_to_llvm maps unknown names to i64.
            Type::Option(inner) => {
                // List and nested Option stay classic {i1,i64} heap-pack
                // (Option ABI split). Never embed List by-value — packing
                // Option<List> into an outer List would zero/dangle the payload.
                let force_heap = match inner.as_ref() {
                    Type::Option(_) => true,
                    Type::Name(n, _)
                        if n == "List" || n == "Option" || n == "Map" || n == "Set" =>
                    {
                        true
                    }
                    _ => false,
                };
                if force_heap {
                    return Some(BasicTypeEnum::StructType(self.context.struct_type(
                        &[
                            BasicTypeEnum::IntType(self.context.bool_type()),
                            BasicTypeEnum::IntType(self.context.i64_type()),
                        ],
                        false,
                    )));
                }
                let inner_llvm = self.llvm_type_for(inner)?;
                // Only widen scalar ints and product-tuple int fields — never
                // named records (all-i32 records must keep i32 field layout).
                let widened = match (inner.as_ref(), inner_llvm) {
                    (_, BasicTypeEnum::IntType(it)) if it.get_bit_width() < 64 => {
                        BasicTypeEnum::IntType(self.context.i64_type())
                    }
                    (Type::Tuple(_), BasicTypeEnum::StructType(sty)) => {
                        // Widen i32..i63 fields only — keep i1 bool as i1.
                        let i64_ty = BasicTypeEnum::IntType(self.context.i64_type());
                        let widened_fields: Vec<_> = sty
                            .get_field_types()
                            .iter()
                            .map(|f| match f {
                                BasicTypeEnum::IntType(it)
                                    if it.get_bit_width() > 1 && it.get_bit_width() < 64 =>
                                {
                                    i64_ty
                                }
                                other => *other,
                            })
                            .collect();
                        BasicTypeEnum::StructType(self.context.struct_type(&widened_fields, false))
                    }
                    (_, other) => other,
                };
                Some(BasicTypeEnum::StructType(self.context.struct_type(
                    &[
                        BasicTypeEnum::IntType(self.context.bool_type()),
                        widened,
                    ],
                    false,
                )))
            }
            Type::Result(ok, _) => {
                let ok_llvm = self.llvm_type_for(ok)?;
                // Widen integer Ok slots and product-tuple i32 fields to i64
                // so they match Ok((1,2)) literal ABI. Do not widen named records
                // or i1 bool fields.
                let widened = match (ok.as_ref(), ok_llvm) {
                    (_, BasicTypeEnum::IntType(it)) if it.get_bit_width() < 64 => {
                        BasicTypeEnum::IntType(self.context.i64_type())
                    }
                    (Type::Tuple(_), BasicTypeEnum::StructType(sty)) => {
                        let i64_ty = BasicTypeEnum::IntType(self.context.i64_type());
                        let widened_fields: Vec<_> = sty
                            .get_field_types()
                            .iter()
                            .map(|f| match f {
                                BasicTypeEnum::IntType(it)
                                    if it.get_bit_width() > 1 && it.get_bit_width() < 64 =>
                                {
                                    i64_ty
                                }
                                other => *other,
                            })
                            .collect();
                        BasicTypeEnum::StructType(self.context.struct_type(&widened_fields, false))
                    }
                    (_, other) => other,
                };
                Some(BasicTypeEnum::StructType(self.context.struct_type(
                    &[
                        BasicTypeEnum::IntType(self.context.bool_type()),
                        widened,
                        BasicTypeEnum::IntType(self.context.i64_type()),
                    ],
                    false,
                )))
            }
            Type::Name(n, args) if n == "Option" && args.len() == 1 => {
                self.llvm_type_for(&Type::Option(Box::new(args[0].clone())))
            }
            Type::Name(n, args) if n == "Result" && args.len() == 2 => self.llvm_type_for(
                &Type::Result(Box::new(args[0].clone()), Box::new(args[1].clone())),
            ),
            _ => crate::codegen::types::mimi_type_to_llvm(self.context, ty),
        }
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

    /// If `name` is a type alias, return its underlying type name string for
    /// Display/to_json dispatch (e.g. `Pair` → `(i32, i32)`). Non-aliases and
    /// unknown names are returned unchanged.
    pub(super) fn resolve_alias_type_name(&self, name: &str) -> String {
        if name.is_empty() {
            return String::new();
        }
        // Already a product-tuple or container form — leave as-is.
        if name.starts_with('(')
            || name.starts_with("List")
            || name.starts_with("Option")
            || name.starts_with("Result")
            || name.starts_with("Map")
            || name.starts_with("Set")
        {
            return name.to_string();
        }
        let mut cur = name.to_string();
        // Bound depth so cyclic aliases cannot loop forever.
        for _ in 0..8 {
            let Some(td) = self.type_defs.get(&cur) else {
                return cur;
            };
            match &td.kind {
                crate::ast::TypeDefKind::Alias(inner) => {
                    if let Some(full) = self.get_full_type_name(inner) {
                        cur = full;
                    } else {
                        return cur;
                    }
                }
                _ => return cur,
            }
        }
        cur
    }

    /// True when `name` is a type alias whose underlying type is a product tuple.
    pub(super) fn is_product_tuple_alias(&self, name: &str) -> bool {
        if name.is_empty() || !self.type_defs.contains_key(name) {
            return false;
        }
        let resolved = self.resolve_alias_type_name(name);
        resolved.starts_with('(')
    }

    /// Get the full type name including generics for a variable (for list element reconstruction).
    pub(super) fn get_full_type_name(&self, ty: &Type) -> Option<String> {
        match ty {
            Type::Name(tn, args) => {
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
            }
            Type::Tuple(elems) => {
                let inner: Vec<String> = elems
                    .iter()
                    .filter_map(|a| self.get_full_type_name(a))
                    .collect();
                if inner.len() == elems.len() {
                    Some(format!("({})", inner.join(", ")))
                } else {
                    None
                }
            }
            Type::Option(inner) => self
                .get_full_type_name(inner)
                .map(|s| format!("Option<{}>", s)),
            Type::Result(ok, err) => {
                let o = self.get_full_type_name(ok)?;
                let e = self.get_full_type_name(err)?;
                Some(format!("Result<{},{}>", o, e))
            }
            _ => None,
        }
    }

    /// Resolve generic type parameters (e.g., `T` → `User`) using the active
    /// `type_map` from the calling context (populated by monomorphization).
    pub(super) fn substitute_type_params(&self, ty: &crate::ast::Type) -> crate::ast::Type {
        use crate::ast::Type;
        match ty {
            Type::Name(name, args) => {
                if args.is_empty() {
                    if let Some(resolved) = self.type_map.get(name) {
                        return resolved.clone();
                    }
                    Type::Name(name.clone(), vec![])
                } else {
                    let new_args: Vec<Type> = args
                        .iter()
                        .map(|a| self.substitute_type_params(a))
                        .collect();
                    Type::Name(name.clone(), new_args)
                }
            }
            _ => ty.clone(),
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
            BasicTypeEnum::IntType(t) => {
                let bits = t.get_bit_width();
                bits.div_ceil(8) as u64
            }
            BasicTypeEnum::FloatType(_) => 8,
            BasicTypeEnum::PointerType(_) => 8,
            BasicTypeEnum::StructType(t) => {
                let field_types = t.get_field_types();
                let mut offset = 0u64;
                let mut max_align = 1u64;
                for ft in field_types.iter() {
                    let field_size = self.llvm_type_size_bytes(*ft);
                    let field_align = self.llvm_type_alignment(*ft);
                    max_align = max_align.max(field_align);
                    offset = offset.div_ceil(field_align) * field_align;
                    offset += field_size;
                }
                offset.div_ceil(max_align) * max_align
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

    /// Compute the natural alignment of an LLVM type in bytes.
    fn llvm_type_alignment(&self, ty: BasicTypeEnum<'ctx>) -> u64 {
        match ty {
            BasicTypeEnum::IntType(t) => {
                let bits = t.get_bit_width();
                let bytes = bits.div_ceil(8) as u64;
                bytes.next_power_of_two()
            }
            BasicTypeEnum::FloatType(_) => 8,
            BasicTypeEnum::PointerType(_) => 8,
            BasicTypeEnum::StructType(t) => t
                .get_field_types()
                .iter()
                .map(|ft| self.llvm_type_alignment(*ft))
                .max()
                .unwrap_or(1),
            BasicTypeEnum::ArrayType(t) => self.llvm_type_alignment(t.get_element_type()),
            BasicTypeEnum::VectorType(t) => {
                // Vector alignment: element alignment * size, clamped
                let elem_align = self.llvm_type_alignment(t.get_element_type());
                let bytes = elem_align * t.get_size() as u64;
                bytes.next_power_of_two().min(32)
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
        if !self.block_has_terminator() {
            self.build_stackrestore(saved)?;
        }
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
