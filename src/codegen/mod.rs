mod actors;
mod block;
pub mod builtins;
mod compile;
mod expr;
mod float_chain;
mod func;
pub mod gep;
mod registry;
mod resolved;
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
    /// v0.34.10a (SD-9): `ieee_float { }` nesting depth — when > 0, float
    /// arithmetic skips the finiteness trap (NaN/Inf allowed, IEEE 754).
    ieee_depth: usize,
    type_defs: HashMap<String, crate::ast::TypeDef>,
    type_llvm: HashMap<String, BasicTypeEnum<'ctx>>,
    cap_vars: Vec<HashMap<String, (inkwell::values::PointerValue<'ctx>, bool)>>,
    cap_type_names: std::collections::HashSet<String>,
    /// Combined capability components: cap name → component names
    /// (cap FullAccess = FileReadCap + FileWriteCap → [FileReadCap, FileWriteCap]).
    /// Used by `c.split()` to materialize single-component cap handles.
    cap_components: std::collections::HashMap<String, Vec<String>>,
    type_map: HashMap<String, crate::ast::Type>,
    func_defs: HashMap<String, FuncDef>,
    /// V-11 (audit 2026-08-05): active nested-function shadows. When a
    /// nested `func name` shadows a same-named global inside its enclosing
    /// body, the nested body is emitted under a mangled symbol and this map
    /// redirects bare-name calls to it while the enclosing body compiles
    /// (mirrors the checker's bare-name directory registration, which keeps
    /// the nested signature live through the whole enclosing callable).
    /// Maps bare name -> (mangled LLVM symbol, func_defs entry displaced by
    /// the shadow, restored when the enclosing frame exits).
    nested_shadow_symbols: HashMap<String, (String, Option<FuncDef>)>,
    /// Name of the function currently compiled by compile_func_legacy (used
    /// to mangle shadowing nested symbols deterministically).
    current_legacy_fn: String,
    /// Monotonic counter disambiguating same-named shadows within one
    /// enclosing function (e.g. two `func h` declarations in sequence).
    nested_shadow_counter: usize,
    var_type_names: HashMap<String, String>,
    /// Type objects for variables (avoids string re-parsing for Arch-2).
    var_types: HashMap<String, Type>,
    /// 0.35.23 deep-eval: names bound via `let ref x = ...` (or annotated
    /// `let ref x: T = ...`). The legacy emitter stores a ref-bound value in
    /// a plain slot (surfaced type `&T`, value layout T), so `*x` must pass
    /// the value through unchanged — matching the bytecode VM's Mov/DerefValue
    /// identity semantics. Only the surface deref cares; ordinary reads of `x`
    /// also yield the value.
    ref_bound_vars: std::collections::HashSet<String>,
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
    /// 0.31.24: Defer blocks for LIFO execution on scope exit (always runs)
    defer_blocks: Vec<Vec<Stmt>>,
    defer_scope_stack: Vec<usize>,
    /// Stack of shared variable heap pointers that need release on scope exit.
    shared_release_vars: Vec<Vec<inkwell::values::PointerValue<'ctx>>>,
    /// Stack of weak reference heap pointers that need weak_release on scope exit.
    weak_release_vars: Vec<Vec<inkwell::values::PointerValue<'ctx>>>,
    /// Names of variables declared with `shared let` (for special access handling).
    shared_var_names: std::collections::HashSet<String>,
    /// Stack of heap-allocated buffer pointers from builtins that need free on scope exit.
    /// Uses RefCell for interior mutability since builtins take &self.
    heap_allocs: std::cell::RefCell<Vec<Vec<HeapEntry<'ctx>>>>,
    /// B9 (audit): env pointers of escaping closure returns. Populated by
    /// `claim_returned_closure_env` at return sites; the immediately following
    /// `free_heap_allocs` emits runtime guards so these envs survive scope
    /// exit (the caller owns them). Cleared on every `free_heap_allocs` call.
    claimed_returned_envs: std::cell::RefCell<Vec<inkwell::values::PointerValue<'ctx>>>,
    /// B9 extension: escaped `List<string>` values whose element string data
    /// pointers must survive the callee's early-return flush. Each entry is
    /// an entry-block alloca holding the list struct, plus its LLVM type.
    claimed_returned_string_lists: std::cell::RefCell<
        Vec<(
            inkwell::values::PointerValue<'ctx>,
            inkwell::types::StructType<'ctx>,
        )>,
    >,
    /// B9 extension for `List<List<string>>`: escaped outer list whose inner
    /// list boxes, inner list data arrays, and string elements must all
    /// survive an early-return flush.
    claimed_returned_string_list_lists: std::cell::RefCell<
        Vec<(
            inkwell::values::PointerValue<'ctx>,
            inkwell::types::StructType<'ctx>,
            inkwell::types::StructType<'ctx>,
        )>,
    >,
    /// 0.35.23 deep-eval: names of the current legacy-body function's
    /// view/mutate borrow params. Their list storage IS the caller's struct
    /// (pointer ABI) — `claim_returned_lists` must not null their data
    /// fields (that destroyed caller state: push on a mutate List param
    /// followed by the implicit-return claim → caller SIGSEGV on indexing).
    borrow_param_names: std::collections::HashSet<String>,
    /// Q2 (rc-quality-gate-0.34.25b): display-formatter scratch buffers
    /// (Result/List/Tuple/Record print paths) that are consumed by exactly
    /// one print-family call. Registered at malloc time, freed immediately
    /// after the consuming printf/puts via `flush_display_frees`.
    /// Kept separate from `heap_allocs` because the lifetime is
    /// "this print call", not "this scope", and the pointers may be nested
    /// (an inner formatter's buffer is snprintf'd into an outer one before
    /// the single printf consumes both).
    display_frees: std::cell::RefCell<Vec<inkwell::values::PointerValue<'ctx>>>,
    /// B9: heap-scope depth at each function-like entry (legacy function,
    /// generic instantiation, lambda body, actor method). Early-return
    /// flushes (`flush_heap_scopes_to_boundary`) pop scopes down to the
    /// innermost boundary so a callee compiled mid-caller (monomorphization,
    /// nested lambda) never frees the caller's registrations.
    heap_boundaries: std::cell::RefCell<Vec<usize>>,
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
    /// RECORD/LIN is_empty (0.1.9, Phase B): the current `is_empty(...)` arg's
    /// Map-vs-Set classification ("map" | "set" | None). Maps and sets both
    /// lower to bare i64 handles, so compile_is_empty cannot tell them apart
    /// from the value alone — the call site's inferred type disambiguates.
    pending_is_empty_kind: Option<&'static str>,
    /// Inferred Mimi type names for arguments of the current `print`/`println` call.
    /// Used to choose the correct runtime list-to-string helper (string vs i32 elements).
    pending_print_arg_types: Vec<String>,
    /// Deep-eval 2026-08-09: side channel from compile_match_expr to the
    /// Err-payload decode: the declared Result/Option type of a builtin-call
    /// scrutinee that the AST probe (expr_type_of) cannot see.
    pending_scrutinee_result_ty: Option<crate::ast::Type>,
    /// Inferred Mimi element type name for the current `push(list, elem)` call.
    /// Used so that nested lists and other struct elements are heap-copied before
    /// their pointer is stored, preventing stack-use-after-return.
    pending_push_elem_type: Option<String>,
    /// Audit wave2 (D-5a): inferred element type of the current `sum(list)`
    /// call argument. List slots are type-erased i64; `sum(List<f64>)` must
    /// reinterpret slots as f64 bit patterns instead of summing them as i64.
    /// Set at the call site (push-flag pattern), consumed by compile_sum.
    pending_sum_elem_type: Option<String>,
    /// Deep-eval 2026-08-17 audit: inferred element type of the current
    /// `pop(list)` call. List slots are type-erased i64; non-integer element
    /// types must be decoded before the popped value can be used by later IR.
    pending_pop_elem_type: Option<String>,
    /// 0.35.20 (#6): inferred product-tuple element type of the current
    /// `zip(a, b)` / `enumerate(xs)` call — e.g. `(string, i32)` / `(i32, i32)`.
    /// compile_zip/compile_enumerate use it to heap-pack each pair with the
    /// SAME LLVM layout the product-tuple formatter expects (string fields
    /// inline {ptr,len}), so zip/enumerate display matches bytecode. Without
    /// it pairs fall back to two raw i64 slots, which the formatter misreads.
    pending_zip_pair_type: Option<String>,
    /// When compiling a typed list literal (`let xs: List<T> = [...]`), the
    /// element type `T` so Result/Option constructors can be inflated to a
    /// uniform layout before heap packing.
    pending_list_elem_type: Option<Type>,
    /// Deep-eval 2026-08-09 (std/fs read_lines E0200): the Ok payload type of
    /// the enclosing Result-returning function, set while compiling the
    /// function body and consumed by the legacy Err constructor. Err builds
    /// `{i1, ok_pad, i64}` — the pad must match the Ok slot's value shape
    /// (string→struct zero, List/Map/Set→ptr zero, scalars→i64 zero) or the
    /// two arms of a `match` producing the same Result type split layouts
    /// (`{i1,ptr,i64}` vs `{i1,i64,i64}`) and the phi unification rejects them.
    pending_result_ok_ty: Option<Type>,
    pending_to_string_is_any: bool,
    /// The inferred type of the argument passed to the current `to_string`
    /// call. Used to distinguish aggregate pointers (List, etc.) from raw C
    /// string pointers so `to_string` can render the value instead of
    /// emitting a type-confused placeholder.
    pending_to_string_arg_type: Option<String>,
    /// Set when `to_int`/`to_float` receives an `Any`-typed argument (e.g. a
    /// `map_get` value). `Any` is lowered to an untyped i64 handle at LLVM
    /// level, so conversion builtins must route through the runtime
    /// `mimi_any_to_int`/`mimi_any_to_float` heuristic instead of treating the
    /// handle as a raw integer.
    pending_to_number_is_any: bool,
    /// Cached result of MIMI_OPT env var check at codegen construction time.
    /// Avoids repeated env var queries within a single compile_to_object call.
    optimize: bool,
    /// Names of variables holding first-class function pointer values.
    fn_ptr_var_names: std::collections::HashSet<String>,
    /// 0.35.14 (DX backlog #18): per tuple-literal binding, the element
    /// index -> named-function map. `let f = t.0` consults this to register
    /// `f` as a fn-pointer variable (the call dispatcher otherwise resolves
    /// `f` as a NAMED function and dies with E0700).
    tuple_fn_elems: HashMap<String, Vec<Option<String>>>,
    /// Stored extern function definitions for lazy code generation.
    extern_func_defs: HashMap<String, crate::ast::ExternFunc>,
    /// ABI per extern function name (e.g., "C", "stdcall").
    extern_block_abis: HashMap<String, String>,
    /// Generated extern wrapper functions, keyed by the original extern name.
    /// 0.34.35b (M-001): wrapper 显式命名 `{name}.extern_wrapper`（内部链接），
    /// 不再依赖 LLVM 对与 C 符号同名函数名的 mangle；调用点一律经此 map 查找。
    extern_wrapper_fns: HashMap<String, inkwell::values::FunctionValue<'ctx>>,
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
    /// Cache of signature-keyed function-pointer trampolines (N-2, 0.34.35).
    /// Key: fingerprint of the func-type field signature. Value: trampoline
    /// fn(env=callee_ptr, params...) that indirect-calls the callee held in
    /// its env slot. Used when a RUNTIME function pointer (e.g. one stored in
    /// a variable) is placed into a closure-typed slot: the callee cannot be
    /// baked statically, so it rides in the env slot.
    fnptr_trampolines: HashMap<String, inkwell::values::PointerValue<'ctx>>,
    /// Const values declared at top level (for codegen const support).
    const_values: HashMap<String, crate::ast::Expr>,

    // ====================================================================
    // v0.28.13 — Inline / GVN scaffolding
    /// Candidate set for small-function inlining (populated during
    /// `compile_func_legacy` and consulted by call-site dispatch).
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
    resolved_transitions_by_flow:
        Option<HashMap<String, Vec<(String, String, String, bool, bool, usize)>>>,
    resolved_transitions_by_event:
        Option<HashMap<String, Vec<(String, String, String, bool, bool, usize)>>>,
    resolved_node_meta_spans: Option<HashMap<String, (usize, usize, usize, usize)>>,
    /// Function directory from CheckedProgram: qualified_name -> arity.
    resolved_function_arity: Option<HashMap<String, usize>>,
    resolved_function_returns: Option<HashMap<String, String>>,
    resolved_function_params: Option<HashMap<String, Vec<(String, String)>>>,
    resolved_comptime_functions: Option<std::collections::HashSet<String>>,
    /// Session names from CheckedProgram.
    resolved_sessions: Option<std::collections::HashSet<String>>,
    resolved_session_displays: Option<HashMap<String, String>>,
    /// Actor method directory from CheckedProgram.
    resolved_actors: Option<HashMap<String, Vec<String>>>,
    resolved_actor_method_signatures: Option<HashMap<String, (usize, String)>>,
    resolved_actor_method_params: Option<HashMap<String, Vec<(String, String)>>>,
    resolved_actor_fields: Option<HashMap<String, Vec<(String, String, bool)>>>,
    resolved_method_signatures: Option<HashMap<String, (usize, String)>>,
    /// Trait/impl method parameter directories: "TraitName.Method" -> [(param_name, type display)].
    resolved_method_params: Option<HashMap<String, Vec<(String, String)>>>,
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
    resolved_ownership_summaries:
        Option<HashMap<String, (usize, usize, usize, usize, usize, bool)>>,
    resolved_ownership_resources: Option<HashMap<String, Vec<String>>>,
    resolved_ownership_actions: Option<HashMap<String, Vec<(String, String)>>>,
    resolved_ownership_merges: Option<HashMap<String, Vec<(String, String, String, String)>>>,
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
    resolved_extern_params: Option<HashMap<String, Vec<(String, String)>>>,
    resolved_extern_no_panic: Option<std::collections::HashSet<String>>,
    resolved_extern_unsafe: Option<std::collections::HashSet<String>>,
    /// Flow mailbox depths from CheckedProgram.
    resolved_mailbox_depths: Option<HashMap<String, usize>>,
    resolved_flow_state_payloads: Option<HashMap<String, Vec<(String, String)>>>,
    resolved_flow_states: Option<HashMap<String, Vec<String>>>,
    resolved_flow_events: Option<HashMap<String, Vec<String>>>,
    resolved_item_kinds: Option<HashMap<String, String>>,
    /// Persistent field sets from CheckedProgram.
    resolved_persistent_fields: Option<HashMap<String, Vec<String>>>,
    /// 0.31.30: Component IR — typed ABI surface for runtime function validation.
    /// When present, get_runtime_fn validates names against the Component IR
    /// exports, catching typos and removed functions at compiler compile time.
    component_ir: Option<crate::component::ComponentIr>,
    /// v0.29.24: process spawn quota from first @max_children(N) (None = unlimited).
    max_children: Option<usize>,
    /// FLOW-TURN-001: true when compiling a transition body that declares
    /// `fails E`. Used by `compile_try_expr` to emit a fail-closed error
    /// (Rejected codegen not yet implemented) instead of silently calling
    /// `mimi_try_exit` which would produce wrong dual-backend behavior.
    in_fails_transition: bool,
    /// 0.34.24: AST return type of the function currently being compiled.
    /// Needed by block.rs `Stmt::Return` handlers to run the same
    /// `coerce_variant_value` the func.rs emit_return path applies — without
    /// it an early `return Err("…")` from a `Result<f64, E>` function emits
    /// a ret of the wrong struct layout ({i1,i64,i64} instead of
    /// {i1,double,i64}) — invalid IR → runtime segfault (audit follow-up:
    /// Result<f64,string> display crash). Every function-body compilation
    /// entry resets this field; no restore needed because each entry that
    /// reads it sets it first.
    current_fn_ret_ty_ast: Option<crate::ast::Type>,
    /// v0.34.16 (ADR-002): true when compiling a multi-target transition.
    /// `Stmt::Return(Some(expr))` wraps the target state struct into the
    /// synthetic `{i32 tag, i64 payload}` union (tag = state ordinal in
    /// `multi_target_states`, payload = ptrtoint boxed state struct).
    in_multi_target_transition: bool,
    /// v0.34.16: target state names of the current multi-target transition,
    /// in declared order (ordinal = tag).
    multi_target_states: Vec<String>,
    /// C1 fix (audit): per-flow global state-name → tag-ordinal map for each
    /// flow's synthetic `__MultiTarget` enum. Keyed by flow name — two flows'
    /// unions are separate enums with independent ordinals, so the map must be
    /// bucketed or the later-registered flow would clobber the earlier one's
    /// ordinals mid-compilation. Built in `register_flow_multi_target_enums`
    /// from each flow's deduped union of all multi-target states, sorted by
    /// name — exactly the ordering `register_type_def` uses to assign variant
    /// ordinals. Return sites (func.rs / block.rs / fault.rs) MUST look up
    /// tags here, never in the per-transition `multi_target_states` subset: a
    /// transition targeting `A | C` would otherwise tag `C` with the subset
    /// ordinal 1, which the receiving match (dispatched on the global enum
    /// ordinal) interprets as `B` — a silent L1 violation.
    multi_target_global_ordinals:
        std::collections::HashMap<String, std::collections::HashMap<String, u64>>,
    /// 0.36.10 (裁决 6 follow-up): variables bound to a transition result that
    /// DECLARED faultability (`-> S | Fault`, incl. the 2-target case the
    /// legacy `multi_target_states`/ordinal machinery covers via the flow-wide
    /// `__MultiTarget` union). Maps variable name -> flow name. recover/reset
    /// on such a value compiles to a runtime tag dispatch (legacy leg); the
    /// union's boxed payload carries the actual state.
    multi_target_result_vars: std::collections::HashMap<String, String>,
    /// Name of the flow whose transition is currently being compiled.
    /// Selects the right bucket in `multi_target_global_ordinals`.
    current_flow_name: String,
    /// v0.34.18a: source state name of the transition currently being compiled.
    /// Used by panic→Fault absorption (expr/fault.rs) to fill `Fault.last_state`.
    current_from_state: String,
    /// H4 (audit-codegen 2026-08-03): the `self` (from-state payload) slot of
    /// the fallible transition currently being compiled, captured right after
    /// parameter binding. Panic→Fault absorption uses it to shadow persistent
    /// draft field values into the Fault record — parity with the bytecode VM's
    /// `shadow_persistent_into_fault` (interp keeps draft values; before the
    /// fix codegen defaulted them, diverging at runtime). None outside a
    /// fallible transition body.
    fault_self_entry: Option<(inkwell::values::PointerValue<'ctx>, BasicTypeEnum<'ctx>)>,
    /// H4 (audit-codegen): persistent field names of the flow currently being
    /// compiled (from FlowDef, available on both compile_checked and legacy
    /// compile_file paths — resolved_persistent_fields is CheckedProgram-only).
    current_persistent_fields: Vec<String>,
    /// Set of function qualified_names that the resolved emitter attempted to
    /// compile but failed (e.g., due to a coercion error in the body emission).
    /// These functions may have partial basic blocks (entry block without
    /// terminator) that would cause the legacy emitter's `compile_func_legacy` to
    /// incorrectly skip them. Track them here so the legacy emitter knows to
    /// recompile even when `count_basic_blocks() != 0`.
    resolved_failed_functions: std::collections::HashSet<String>,
}

type VarEntry<'ctx> = (inkwell::values::PointerValue<'ctx>, BasicTypeEnum<'ctx>);

/// Entries tracked for scope-exit heap cleanup.
/// `Ptr(ptr)` = a raw heap pointer to free directly.
/// `Slot(base, struct_ty, field)` = an alloca of type `struct_ty` (`base`) and
/// the field index that holds the heap pointer. At cleanup a fresh GEP is
/// emitted from `base` in the current block. `base` must dominate the cleanup
/// point; call sites therefore allocate it in the function entry block.
/// The struct's ptr field is also null-initialized at the entry block.
#[derive(Clone)]
enum HeapEntry<'ctx> {
    Ptr(inkwell::values::PointerValue<'ctx>),
    Slot(
        inkwell::values::PointerValue<'ctx>,
        inkwell::types::StructType<'ctx>,
        u32,
    ),
    /// L6: a custom-enum value `{i32 tag, i64 payload}` whose payload slot holds
    /// a heap box pointer ONLY for the variants listed in `boxed_ordinals`
    /// ( PayloadKind::Packed). At scope exit, load the struct, read the tag,
    /// and free `inttoptr(payload)` iff the tag is a boxed variant — `Single`/
    /// `None` variants store inline data in the i64 slot and must NOT be freed
    /// (freeing inline bits would crash). `slot` is an entry-block alloca
    /// holding the whole `{i32, i64}` struct so the tag is readable at free time.
    EnumBox {
        slot: inkwell::values::PointerValue<'ctx>,
        struct_ty: inkwell::types::StructType<'ctx>,
        boxed_ordinals: Vec<u64>,
    },
    /// A returned `List<string>` struct whose data array owns each element's
    /// heap string. At scope exit, free every string in the array and then
    /// free the array itself.
    StringListData {
        slot: inkwell::values::PointerValue<'ctx>,
        list_ty: inkwell::types::StructType<'ctx>,
    },
    /// A returned `List<List<string>>` struct. At scope exit, free every
    /// inner string list (strings + data array) and then the outer data.
    StringListListData {
        slot: inkwell::values::PointerValue<'ctx>,
        list_ty: inkwell::types::StructType<'ctx>,
        elem_list_ty: inkwell::types::StructType<'ctx>,
    },
}

// Resolved-directory query methods are production instrumentation consumed by
// external tooling and selectively by tests; a crate target need not call all.
#[allow(dead_code)]
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
            ieee_depth: 0,
            type_defs: HashMap::new(),
            type_llvm: HashMap::new(),
            cap_vars: vec![HashMap::new()],
            cap_type_names: std::collections::HashSet::new(),
            cap_components: std::collections::HashMap::new(),
            type_map: HashMap::new(),
            func_defs: HashMap::new(),
            nested_shadow_symbols: HashMap::new(),
            current_legacy_fn: String::new(),
            nested_shadow_counter: 0,
            var_type_names: HashMap::new(),
            var_types: HashMap::new(),
            ref_bound_vars: std::collections::HashSet::new(),
            upgrade_option_vars: std::collections::HashSet::new(),
            spawn_counter: 0,
            strict: false,
            no_std: false,
            shared: false,
            verify_contracts: true,
            target_triple: None,
            compensation_blocks: Vec::new(),
            comp_scope_stack: Vec::new(),
            defer_blocks: Vec::new(),
            defer_scope_stack: Vec::new(),
            shared_release_vars: vec![Vec::new()],
            weak_release_vars: vec![Vec::new()],
            shared_var_names: std::collections::HashSet::new(),
            heap_allocs: std::cell::RefCell::new(vec![Vec::new()]),
            claimed_returned_envs: std::cell::RefCell::new(Vec::new()),
            claimed_returned_string_lists: std::cell::RefCell::new(Vec::new()),
            claimed_returned_string_list_lists: std::cell::RefCell::new(Vec::new()),
            borrow_param_names: std::collections::HashSet::new(),
            heap_boundaries: std::cell::RefCell::new(Vec::new()),
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
            pending_is_empty_kind: None,
            pending_print_arg_types: Vec::new(),
            pending_scrutinee_result_ty: None,
            display_frees: std::cell::RefCell::new(Vec::new()),
            pending_push_elem_type: None,
            pending_sum_elem_type: None,
            pending_pop_elem_type: None,
            pending_zip_pair_type: None,
            pending_list_elem_type: None,
            pending_result_ok_ty: None,
            pending_to_string_is_any: false,
            pending_to_string_arg_type: None,
            pending_to_number_is_any: false,
            // 0.34.34: O1 is the default. 0.31.21 fixed the O1 codegen bugs
            // (try_expr i32-vs-i1 mismatch, extern wrapper name collision);
            // the previous opt-in default was deferred "pending fuzz testing".
            // MIMI_OPT is now opt-OUT: MIMI_OPT=0 / MIMI_OPT=false disables
            // optimization (debug fallback); unset or 1/true keeps O1.
            optimize: std::env::var("MIMI_OPT")
                .map(|v| !(v == "0" || v == "false"))
                .unwrap_or(true),
            contract_bb_counter: 0,
            fn_ptr_var_names: std::collections::HashSet::new(),
            tuple_fn_elems: HashMap::new(),
            extern_func_defs: HashMap::new(),
            extern_block_abis: HashMap::new(),
            extern_wrapper_fns: HashMap::new(),
            pending_callback_tls: Vec::new(),
            list_elem_llvm_types: HashMap::new(),
            closure_wrappers: HashMap::new(),
            fnptr_trampolines: HashMap::new(),
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
            resolved_transitions_by_flow: None,
            resolved_transitions_by_event: None,
            resolved_node_meta_spans: None,
            resolved_function_arity: None,
            resolved_function_returns: None,
            resolved_function_params: None,
            resolved_comptime_functions: None,
            resolved_sessions: None,
            resolved_session_displays: None,
            resolved_actors: None,
            resolved_actor_method_signatures: None,
            resolved_actor_method_params: None,
            resolved_actor_fields: None,
            resolved_method_signatures: None,
            resolved_method_params: None,
            resolved_capabilities: None,
            resolved_capability_combined: None,
            resolved_constants: None,
            resolved_constant_values: None,
            resolved_traits: None,
            resolved_impls: None,
            resolved_ownership_owners: None,
            resolved_ownership_summaries: None,
            resolved_ownership_resources: None,
            resolved_ownership_actions: None,
            resolved_ownership_merges: None,
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
            resolved_extern_params: None,
            resolved_extern_no_panic: None,
            resolved_extern_unsafe: None,
            resolved_mailbox_depths: None,
            resolved_flow_state_payloads: None,
            resolved_flow_states: None,
            resolved_flow_events: None,
            resolved_item_kinds: None,
            resolved_persistent_fields: None,
            component_ir: None,
            max_children: None,
            in_fails_transition: false,
            current_fn_ret_ty_ast: None,
            in_multi_target_transition: false,
            multi_target_states: Vec::new(),
            multi_target_global_ordinals: std::collections::HashMap::new(),
            multi_target_result_vars: std::collections::HashMap::new(),
            current_flow_name: String::new(),
            current_from_state: String::new(),
            fault_self_entry: None,
            current_persistent_fields: Vec::new(),
            resolved_failed_functions: std::collections::HashSet::new(),
        }
    }

    /// Function names the resolved emitter attempted and then handed to
    /// legacy. Phase 0 core-callee policy treats a non-empty set on
    /// Flow/Session/spawn/linear functions as a hard compile error; tests
    /// still inspect this to prove a core Flow program stayed resolved.
    pub fn resolved_failed_functions(&self) -> &std::collections::HashSet<String> {
        &self.resolved_failed_functions
    }

    pub(crate) fn resolved_method_signature(&self, key: &str) -> Option<(usize, String)> {
        self.resolved_method_signatures
            .as_ref()
            .and_then(|map| map.get(key).cloned())
    }

    pub(crate) fn resolved_method_params(&self, key: &str) -> Option<Vec<(String, String)>> {
        self.resolved_method_params
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

    pub(crate) fn resolved_actor_method_params(
        &self,
        actor: &str,
        method: &str,
    ) -> Option<Vec<(String, String)>> {
        self.resolved_actor_method_params
            .as_ref()
            .and_then(|map| map.get(&format!("{actor}.{method}")).cloned())
    }

    pub(crate) fn resolved_extern_signature(&self, name: &str) -> Option<(usize, String)> {
        self.resolved_extern_signatures
            .as_ref()
            .and_then(|map| map.get(name).cloned())
    }

    pub(crate) fn resolved_extern_params(&self, name: &str) -> Option<Vec<(String, String)>> {
        self.resolved_extern_params
            .as_ref()
            .and_then(|map| map.get(name).cloned())
    }

    pub(crate) fn resolved_constant_value(&self, name: &str) -> Option<(Option<String>, String)> {
        self.resolved_constant_values
            .as_ref()
            .and_then(|map| map.get(name).cloned())
    }

    pub(crate) fn resolved_extern_abi(&self, name: &str) -> Option<&str> {
        self.resolved_extern_abis
            .as_ref()
            .and_then(|map| map.get(name).map(String::as_str))
    }

    /// 0.34.35b (M-001): 按声明名查找已生成的 extern wrapper 函数。
    /// wrapper 显式命名 `{name}.extern_wrapper`（可能与声明名不同），
    /// 调用点（legacy emit_named_call / resolved ResolvedCallee::Extern）
    /// 必须经此 map 而非 `module.get_function(name)`——后者会命中 extern
    /// 原符号（跳过 wrapper 的 ABI 参数转换）。
    pub(crate) fn extern_wrapper_fn(
        &self,
        name: &str,
    ) -> Option<inkwell::values::FunctionValue<'ctx>> {
        self.extern_wrapper_fns.get(name).copied()
    }

    pub(crate) fn resolved_function_params(&self, name: &str) -> Option<Vec<(String, String)>> {
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
        self.resolved_fallback_transitions
            .as_ref()
            .is_some_and(|set| {
                set.contains(&(flow.to_string(), event.to_string(), source.to_string()))
            })
    }

    pub(crate) fn resolved_transition_param_arity(
        &self,
        flow: &str,
        event: &str,
        source: &str,
    ) -> Option<usize> {
        self.resolved_transition_param_arity
            .as_ref()
            .and_then(|map| {
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

    pub(crate) fn resolved_type_fields(&self, name: &str) -> Option<Vec<(String, String)>> {
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

    pub(crate) fn resolved_actor_fields(&self, actor: &str) -> Option<Vec<(String, String, bool)>> {
        self.resolved_actor_fields
            .as_ref()
            .and_then(|map| map.get(actor).cloned())
    }

    pub(crate) fn resolved_backend_requirements(&self) -> Option<&[(String, String)]> {
        self.resolved_backend_requirements
            .as_ref()
            .map(Vec::as_slice)
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
        self.resolved_backend_requirements
            .as_ref()
            .is_some_and(|reqs| reqs.iter().any(|(cap, _)| cap == capability))
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

    pub(crate) fn resolved_ownership_actions(&self, owner: &str) -> Option<Vec<(String, String)>> {
        self.resolved_ownership_actions
            .as_ref()
            .and_then(|map| map.get(owner).cloned())
    }

    pub(crate) fn resolved_ownership_merges(
        &self,
        owner: &str,
    ) -> Option<Vec<(String, String, String, String)>> {
        self.resolved_ownership_merges
            .as_ref()
            .and_then(|map| map.get(owner).cloned())
    }

    pub(crate) fn resolved_transitions_for_flow(
        &self,
        flow: &str,
    ) -> Option<Vec<(String, String, String, bool, bool, usize)>> {
        self.resolved_transitions_by_flow
            .as_ref()
            .and_then(|map| map.get(flow).cloned())
    }

    pub(crate) fn resolved_transitions_for_event(
        &self,
        event: &str,
    ) -> Option<Vec<(String, String, String, bool, bool, usize)>> {
        self.resolved_transitions_by_event
            .as_ref()
            .and_then(|map| map.get(event).cloned())
    }

    pub(crate) fn resolved_node_meta_span(
        &self,
        path: &str,
    ) -> Option<(usize, usize, usize, usize)> {
        self.resolved_node_meta_spans
            .as_ref()
            .and_then(|map| map.get(path).copied())
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

    /// Attach `llvm.loop.unroll.count` metadata to the current block's
    /// terminator (the loop latch back-edge), capping LLVM's LoopUnroll pass at
    /// a moderate factor.
    ///
    /// dsp-style hot loops (0.35.30 audit) have a serial floating-point chain:
    /// LLVM's default aggressive unrolling (80×) bloats the I-cache and drops
    /// IPC to ~0.72 vs the C baseline's 1.00 at identical instruction count.
    /// The serial chain exposes no ILP, so a small unroll (amortize the branch
    /// + let the convergence pass hoist the per-op finiteness checks) is enough;
    /// 80× only inflates I-cache pressure. Disabling unroll entirely regressed
    /// to ~2× (the per-op trap branches are no longer hoisted), so the cap is
    /// the balance point. Only the O1 path is affected — O0 runs no loop passes.
    ///
    /// LLVM requires the loop metadata to be a self-referential distinct node:
    /// `!N = distinct !{!N, !M}` with `!M = !{!"llvm.loop.unroll.count", i32 K}`.
    /// The C API has no `MDNode::getDistinct`; we build the node with a
    /// placeholder first operand and then make it self-referential via
    /// `LLVMReplaceMDNodeOperandWith`.
    const LOOP_UNROLL_CAP: u64 = 4;

    pub(super) fn cap_loop_unroll(&self) -> Result<(), CompileError> {
        // Opt-in tuning knob: default OFF. The 0.35.30 audit found LLVM 18's
        // aggressive 80× unroll of the dsp loop was a net win (455M instrs vs
        // 800M without unroll) — capping it regresses wall time. Keep the
        // mechanism behind MIMI_LOOP_UNROLL_CAP for experimentation only.
        let Ok(cap) = std::env::var("MIMI_LOOP_UNROLL_CAP") else {
            return Ok(());
        };
        if !self.optimize {
            return Ok(());
        }
        let cap: u64 = cap.parse().unwrap_or(Self::LOOP_UNROLL_CAP);
        let Some(block) = self.builder.get_insert_block() else {
            return Ok(());
        };
        let Some(terminator) = block.get_terminator() else {
            return Ok(());
        };

        // !M = !{!"llvm.loop.unroll.count", i32 K}
        let md_string = self.context.metadata_string("llvm.loop.unroll.count");
        let count = self.context.i32_type().const_int(cap, false);
        let prop_node = self
            .context
            .metadata_node(&[md_string.into(), count.into()]);

        // !N = !{placeholder, !M}, then replace operand 0 with !N itself so the
        // node becomes self-referential (and therefore distinct).
        let placeholder = self.context.metadata_node(&[]);
        let loop_id = self
            .context
            .metadata_node(&[placeholder.into(), prop_node.into()]);
        unsafe {
            use inkwell::llvm_sys::core::{LLVMReplaceMDNodeOperandWith, LLVMValueAsMetadata};
            let loop_id_val = inkwell::values::AsValueRef::as_value_ref(&loop_id);
            let loop_id_md = LLVMValueAsMetadata(loop_id_val);
            LLVMReplaceMDNodeOperandWith(loop_id_val, 0, loop_id_md);
        }

        let kind_id = self.context.get_kind_id("llvm.loop");
        terminator
            .set_metadata(loop_id, kind_id)
            .map_err(|e| CompileError::LlvmError(format!("loop metadata: {}", e)))?;
        Ok(())
    }

    /// Look up a runtime/external function by name.
    ///
    /// 0.31.30: when Component IR is available (debug builds), validates
    /// that the name is a known export. Catches typos and removed functions
    /// at compiler development time.
    pub(super) fn get_runtime_fn(
        &self,
        name: &str,
    ) -> Result<inkwell::values::FunctionValue<'ctx>, CompileError> {
        // 0.31.30: debug-time validation against Component IR.
        // Only checks mimi_* names (runtime exports); libc/LLVM intrinsics
        // (malloc, strcpy, etc.) are not in the Component IR.
        #[cfg(debug_assertions)]
        if name.starts_with("mimi_") {
            if let Some(ref ir) = self.component_ir {
                if ir.export(name).is_none() {
                    // Not a hard error yet — the Component IR registry is
                    // incomplete (~180/388 functions). Log for development.
                    eprintln!(
                        "[component-ir] warning: get_runtime_fn(\"{}\") not in Component IR registry",
                        name
                    );
                }
            }
        }
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
        let f = self.module.add_function(
            "mimi_runtime_abort",
            ty,
            Some(inkwell::module::Linkage::External),
        );
        // Mark noreturn so the optimizer knows control flow never falls through.
        let kind = inkwell::attributes::Attribute::get_named_enum_kind_id("noreturn");
        let attr = self.context.create_enum_attribute(kind, 0);
        f.add_attribute(inkwell::attributes::AttributeLoc::Function, attr);
        f
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

    /// Zero/unit value for a return type. A bare `return` in a unit function
    /// must `ret i64 0` — the unit signature is i64 (compile_func), so the
    /// old `ret void` produced invalid IR (mismatched terminator) that O0
    /// tolerated but O1's CalledValuePropagationPass SIGSEGV'd on
    /// ("func f() { if true { return } }" crash, 0.35.23 deep-eval).
    pub(super) fn zero_value_for(&self, ty: BasicTypeEnum<'ctx>) -> BasicValueEnum<'ctx> {
        match ty {
            BasicTypeEnum::IntType(t) => t.const_zero().into(),
            BasicTypeEnum::FloatType(t) => t.const_float(0.0).into(),
            BasicTypeEnum::PointerType(t) => t.const_null().into(),
            BasicTypeEnum::StructType(t) => t.const_zero().into(),
            BasicTypeEnum::ArrayType(t) => t.const_zero().into(),
            _ => self.context.i64_type().const_zero().into(),
        }
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

    /// v0.34.16 (ADR-002): wrap a multi-target transition's target state
    /// struct into the synthetic tagged union `{i32 tag, i64 payload}`.
    /// The struct is boxed (malloc + store), and the box pointer is
    /// ptrtoint-encoded into the uniform i64 payload slot so targets with
    /// differing layouts share one return type.
    pub(super) fn wrap_multi_target_value(
        &self,
        state_val: BasicValueEnum<'ctx>,
        tag: u64,
        state_ty: Option<BasicTypeEnum<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i32_ty = BasicTypeEnum::IntType(self.context.i32_type());
        let i64_ty = BasicTypeEnum::IntType(self.context.i64_type());
        // Dereference pointer-valued states (compile_record returns the state
        // in an alloca; the callee's `return` path hands us the pointer). The
        // element type is passed in by the caller (opaque pointers cannot
        // recover it).
        let state_val = match state_val {
            BasicValueEnum::PointerValue(pv) => {
                let ty = state_ty.unwrap_or(state_val.get_type());
                self.build_load(ty, pv, "mt_state_load")?
            }
            other => other,
        };
        // Box the state struct. C2 (MEM-C8): size the box from the actual
        // LLVM type size, NOT field_count × 8. A nested record field
        // (state Foo { inner: Bar } with Bar = { i32, i32, i32 }) lowers to
        // { { i32, i32, i32 } }: count_fields() == 1 but the store writes
        // 12 bytes. v0.34.18a: use llvm_type_size_bytes (recursive, alignment
        // aware) instead of size_of().get_zero_extended_constant() — the latter
        // returns None for the deeply nested Fault record (3 strings + nested
        // SystemTrace/MemoryDump/PanicPayload, ~176 bytes), and the old
        // .unwrap_or(64) fallback undersized the box → heap overflow on the
        // panic→Fault absorption store.
        let state_ty = state_val.get_type();
        let box_bytes: u64 = self.llvm_type_size_bytes(state_ty).max(8);
        let size = self.context.i64_type().const_int(box_bytes, false);
        // L1 (audit-codegen 2026-08-03): former TODO(L6) "this payload box is
        // never freed" is STALE — 0714504c + method.rs (L6 multi-target fix,
        // the `Register the box in heap_allocs` block after the transition
        // call) implemented single-owner call-site registration: the box is
        // freed exactly once at the owning call's scope exit via
        // free_heap_allocs. The match decode copies fields out without
        // freeing; copies of the result are Ident reads of the same box, not
        // new owners, so no double-free.
        let box_ptr = self.malloc_or_abort(size, "mt_box")?;
        self.build_store(box_ptr, state_val)
            .map_err(|e| CompileError::LlvmError(format!("mt box store: {}", e)))?;
        let payload_i64 = self.build_ptr_to_int(box_ptr, self.context.i64_type(), "mt_box_ptr")?;
        // Pack {i32 tag, i64 payload}.
        let struct_ty = self.context.struct_type(&[i32_ty, i64_ty], false);
        let alloca = self.build_alloca(struct_ty, "mt_union")?;
        let tag_gep = self
            .gep()
            .build_struct_gep(struct_ty, alloca, 0, "mt_tag")
            .map_err(|e| CompileError::LlvmError(format!("mt tag gep: {}", e)))?;
        self.build_store(tag_gep, self.context.i32_type().const_int(tag, false))?;
        let payload_gep = self
            .gep()
            .build_struct_gep(struct_ty, alloca, 1, "mt_payload")
            .map_err(|e| CompileError::LlvmError(format!("mt payload gep: {}", e)))?;
        self.build_store(payload_gep, payload_i64)?;
        self.build_load(BasicTypeEnum::StructType(struct_ty), alloca, "mt_union_val")
            .map_err(|e| CompileError::LlvmError(format!("mt union load: {}", e)))
    }

    /// 0.34.36 (audit §6.9): resolve a flow STATE's LLVM layout for the
    /// multi-target return wrap. Looks up the QUALIFIED key
    /// `flow::{current_flow_name}::{state}` first — the key register_type_def
    /// received in the first pass — and only falls back to the bare state name
    /// (the legacy first-wins alias) when the qualified key is missing.
    ///
    /// The bare-name lookup was unsound across flows: two flows declaring
    /// same-named states with different payloads share one alias slot
    /// (first-wins), so the loser flow's return wrap loaded/dereferenced the
    /// state pointer with the WRONG struct type (mis-sized load → garbage
    /// payload bytes in the tagged union).
    pub(super) fn flow_state_llvm_type(&self, state_name: &str) -> Option<BasicTypeEnum<'ctx>> {
        if !self.current_flow_name.is_empty() {
            let qualified = format!("flow::{}::{}", self.current_flow_name, state_name);
            if let Some(&ty) = self.type_llvm.get(&qualified) {
                return Some(ty);
            }
        }
        self.type_llvm.get(state_name).copied()
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
                    // SD-7 (0.34.34): narrowing store into an i32 slot is E0802
                    // overflow when out of range — not a silent wrap. The VM
                    // assign-guard traps identically. Casts already arrive at
                    // target width and never reach this path.
                    if slot_bw == 32 {
                        self.emit_i32_range_guard(iv, "assign")?;
                    }
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
            // C2 (audit 2026-08-03): the 0.34.6 one-way widening {i32→i64,
            // i32→f64, i64→f64} must also materialize on assignment in the
            // legacy emitter (`z = 3` where z: f64). The resolved emitter
            // covers this via CheckedConversion::NumericWiden, but the legacy
            // arm (generic / lambda / async / extern-ABI bodies) previously
            // stored the raw int into the float alloca — silently keeping the
            // old value or storing a garbage bit pattern (L1 divergence with
            // the bytecode VM, which materializes IntToFloat in compile_assign).
            (BasicValueEnum::IntValue(iv), BasicTypeEnum::FloatType(slot_ft)) => self
                .builder
                .build_signed_int_to_float(iv, slot_ft, &format!("{}_assign_sitofp", name))
                .map_err(|e| CompileError::LlvmError(format!("assign sitofp: {}", e)))?
                .into(),
            (BasicValueEnum::PointerValue(pv), BasicTypeEnum::StructType(_)) => {
                self.build_load(ty, pv, &format!("{}_assign_load", name))?
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
        } else if matches!(val, BasicValueEnum::StructValue(_))
            && matches!(ty, BasicTypeEnum::PointerType(_))
        {
            let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            let dest_ptr = self
                .build_load(ptr_ty, alloca, &format!("{}_assign_dest", name))?
                .into_pointer_value();
            self.build_store(dest_ptr, val)?;
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
    /// Q2 (rc-quality-gate-0.34.25b): register a display-formatter scratch
    /// buffer for release at the consuming print call. These buffers are
    /// produced by the io display emitters (Result/List/Tuple/Record/Enum/
    /// Option formatting), consumed by exactly one printf/puts, and were
    /// previously never freed (256B per printed Result, 4096B per printed
    /// List, ...). Freed by `flush_display_frees` immediately after the
    /// print call, so a loop printing a Result leaks zero bytes.
    pub(super) fn register_display_alloc(&self, ptr: inkwell::values::PointerValue<'ctx>) {
        self.display_frees.borrow_mut().push(ptr);
    }

    /// Q2: free every display-formatter buffer consumed by the print call
    /// just emitted. Must run *after* the printf/puts build_call that uses
    /// the pointers. Safe: display buffers are referenced only by that
    /// call's arguments and are dead afterwards. Nested formatter buffers
    /// (inner snprintf'd into an outer one) are all released here in one
    /// pass, so no intermediate buffer leaks either.
    pub(super) fn flush_display_frees(&self) -> Result<(), CompileError> {
        // Take the buffer list first: build_call below may re-enter codegen
        // paths (runtime fn resolution), and holding the RefCell borrow
        // across it risked a re-entrant borrow panicking mid-emit.
        let frees = std::mem::take(&mut *self.display_frees.borrow_mut());
        if frees.is_empty() {
            return Ok(());
        }
        if let Ok(free_fn) = self.get_runtime_fn("free") {
            for ptr in frees.iter() {
                self.build_call(
                    free_fn,
                    &[BasicMetadataValueEnum::PointerValue(*ptr)],
                    "display_free",
                )?;
            }
        }
        Ok(())
    }

    /// Q2 (0.34.25b): snapshot the display-frees list length. Sub-emitters
    /// called from *inside* a runtime branch/loop (Result Ok arm, list element
    /// formatter) register their buffers here, but the top-level
    /// `flush_display_frees` runs after printf — for an unexecuted arm those
    /// pointers are undef (free(garbage) → segfault) and for an N>1 loop all
    /// but the last iteration leak. Callers in branched display emitters take
    /// a marker after their own unconditional buffer, then call
    /// `flush_display_since(marker)` at the end of each arm/iteration body:
    /// the frees are emitted *inside* the same runtime block as the mallocs,
    /// so they execute exactly as often as the allocations.
    pub(super) fn display_marker(&self) -> usize {
        self.display_frees.borrow().len()
    }

    /// Q2: free every display buffer registered since `marker` and drop them
    /// from the pending list (so the end-of-print flush does not double-free).
    /// Must be emitted where `marker`'s allocations are guaranteed live — a
    /// branch arm (after the last use of that arm's sub-buffers) or a loop
    /// body (after the element formatter was consumed).
    pub(super) fn flush_display_since(&self, marker: usize) -> Result<(), CompileError> {
        let tail: Vec<_> = {
            let mut frees = self.display_frees.borrow_mut();
            if frees.len() <= marker {
                return Ok(());
            }
            frees.split_off(marker)
        };
        if let Ok(free_fn) = self.get_runtime_fn("free") {
            for ptr in tail.iter() {
                self.build_call(
                    free_fn,
                    &[BasicMetadataValueEnum::PointerValue(*ptr)],
                    "disp_arm_free",
                )?;
            }
        }
        Ok(())
    }

    /// 0.35.14 (DX backlog #18): record which elements of a tuple-literal
    /// binding are named functions so a later `let f = t.N` can register `f`
    /// as a fn-pointer variable.
    pub(super) fn record_tuple_fn_elems(&mut self, name: &str, init: &crate::ast::Expr) {
        if let crate::ast::Expr::Tuple(elems) = init.unlocated() {
            let recorded: Vec<Option<String>> = elems
                .iter()
                .map(|e| match e.unlocated() {
                    crate::ast::Expr::Ident(fn_name)
                        if self.module.get_function(fn_name.as_str()).is_some() =>
                    {
                        Some(fn_name.clone())
                    }
                    _ => None,
                })
                .collect();
            if recorded.iter().any(|e| e.is_some()) {
                self.tuple_fn_elems.insert(name.to_string(), recorded);
            }
        }
    }

    /// 0.35.14 (DX backlog #18): `let f = t.N` where `t` was bound to a
    /// tuple literal whose Nth element is a named function — register `f`
    /// as a fn-pointer variable plus its declared Func signature (the
    /// indirect-call path recovers the real return type from var_types;
    /// without it an i64 signature reads garbage for f64/struct returns).
    pub(super) fn register_tuple_index_fn_binding(&mut self, name: &str, init: &crate::ast::Expr) {
        if let crate::ast::Expr::TupleIndex(base, idx) = init.unlocated() {
            if let crate::ast::Expr::Ident(base_name) = base.unlocated() {
                let fn_name = self
                    .tuple_fn_elems
                    .get(base_name.as_str())
                    .and_then(|elems| elems.get(*idx))
                    .and_then(|e| e.clone());
                if let Some(fn_name) = fn_name {
                    self.fn_ptr_var_names.insert(name.to_string());
                    if let Some(fdef) = self.func_defs.get(fn_name.as_str()) {
                        let params: Vec<crate::ast::Type> =
                            fdef.params.iter().map(|p| p.ty.clone()).collect();
                        let ret = fdef.ret.clone().unwrap_or(crate::ast::Type::Infer);
                        self.var_types.insert(
                            name.to_string(),
                            crate::ast::Type::Func(params, Box::new(ret)),
                        );
                    }
                }
            }
        }
    }

    pub(super) fn register_heap_alloc(&self, ptr: inkwell::values::PointerValue<'ctx>) {
        // Materialize the pointer into an entry-block alloca: cleanup may be
        // emitted in a later basic block (merge block, function return), and
        // free(ptr) on a branch-local SSA value would violate SSA dominance
        // (LLVM crashes under O1). The slot dominates all uses; frees load it.
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let slot = self
            .build_entry_alloca(ptr_ty, "heap_alloc_slot")
            .unwrap_or(ptr);
        if slot != ptr {
            // Deep-eval 2026-08-09 (demos/04 describe_point segv): null-init
            // the slot at the entry block, mirroring register_heap_box. The
            // registration is often emitted inside a conditional branch (the
            // concat arm of an if/match returning string); a sibling path
            // that never executes the allocation would otherwise load stack
            // garbage at cleanup and free(garbage) → munmap_chunk abort /
            // segfault.
            let saved = self.builder.get_insert_block();
            if let Some(slot_inst) = slot.as_instruction() {
                if let Some(next) = slot_inst.get_next_instruction() {
                    self.builder.position_before(&next);
                } else if let Some(parent) = slot_inst.get_parent() {
                    self.builder.position_at_end(parent);
                }
                if let Err(e) = self.build_store(slot, ptr_ty.const_null()) {
                    eprintln!("[mimi codegen] warning: heap-slot null-init failed: {}", e);
                }
            }
            if let Some(saved) = saved {
                self.builder.position_at_end(saved);
            }
            if let Err(e) = self.build_store(slot, ptr) {
                // L2 (audit-codegen 2026-08-03): do not silently swallow —
                // a failed store leaves the slot undef → free(undef) at
                // scope exit. Registration contexts cannot propagate
                // (fn returns ()), so surface the failure loudly.
                eprintln!("[mimi codegen] warning: heap-slot store failed: {}", e);
            }
        }
        let mut guard = self.heap_allocs.borrow_mut();
        if let Some(stack) = guard.last_mut() {
            stack.push(HeapEntry::Ptr(slot));
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
            guard.push(vec![HeapEntry::Ptr(slot)]);
        }
    }

    /// L6: register a heap box pointer for scope-exit free, with an entry-block
    /// null-init of the slot. Unlike `register_heap_alloc`, the slot is
    /// null-initialised at the entry block so a registration emitted in a
    /// conditional branch that is NOT taken at runtime frees `null` (a no-op)
    /// rather than garbage — e.g. an enum constructor inside `if cond { Rect(..)
    /// }` whose sibling branch registers a different box. Without null-init the
    /// untaken branch's slot is undef → `free(undef)` crashes.
    pub(super) fn register_heap_box(&self, box_ptr: inkwell::values::PointerValue<'ctx>) {
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let slot = self
            .build_entry_alloca(ptr_ty, "enum_box_ptr_slot")
            .unwrap_or(box_ptr);
        if slot != box_ptr {
            // Null-init at the entry block (after the alloca) so untaken
            // conditional paths load null.
            let saved = self.builder.get_insert_block();
            if let Some(slot_inst) = slot.as_instruction() {
                if let Some(next) = slot_inst.get_next_instruction() {
                    self.builder.position_before(&next);
                } else if let Some(parent) = slot_inst.get_parent() {
                    self.builder.position_at_end(parent);
                }
                if let Err(e) = self.build_store(slot, ptr_ty.const_null()) {
                    // L2 (audit-codegen 2026-08-03): do not silently swallow —
                    // a failed store leaves the slot undef → free(undef) at
                    // scope exit. Registration contexts cannot propagate
                    // (fn returns ()), so surface the failure loudly.
                    eprintln!("[mimi codegen] warning: heap-slot store failed: {}", e);
                }
            }
            if let Some(saved) = saved {
                self.builder.position_at_end(saved);
            }
            // Store the actual box pointer at the current (ctor call) position.
            if let Err(e) = self.build_store(slot, box_ptr) {
                // L2 (audit-codegen 2026-08-03): do not silently swallow —
                // a failed store leaves the slot undef → free(undef) at
                // scope exit. Registration contexts cannot propagate
                // (fn returns ()), so surface the failure loudly.
                eprintln!("[mimi codegen] warning: heap-slot store failed: {}", e);
            }
        }
        let mut guard = self.heap_allocs.borrow_mut();
        if let Some(stack) = guard.last_mut() {
            stack.push(HeapEntry::Ptr(slot));
        }
    }

    /// L6: register a custom-enum value `{i32 tag, i64 payload}` for a
    /// tag-conditional box free at scope exit (`HeapEntry::EnumBox`). `slot` is
    /// an entry-block alloca holding the whole struct (so the tag is readable at
    /// free time). The slot is null-initialised at the entry block so a
    /// never-stored path (e.g. the untaken branch of a conditional call) loads
    /// tag=0/payload=0 → either the tag check skips, or `free(inttoptr(0))` is a
    /// `free(null)` no-op — never a garbage free.
    pub(super) fn register_enum_box(
        &self,
        slot: inkwell::values::PointerValue<'ctx>,
        struct_ty: inkwell::types::StructType<'ctx>,
        boxed_ordinals: Vec<u64>,
    ) {
        let saved = self.builder.get_insert_block();
        if let Some(f) = self.current_function() {
            if let Some(entry_bb) = f.get_first_basic_block() {
                if let Some(slot_inst) = slot.as_instruction() {
                    if let Some(next) = slot_inst.get_next_instruction() {
                        self.builder.position_before(&next);
                    } else {
                        self.builder
                            .position_at_end(slot_inst.get_parent().unwrap_or(entry_bb));
                    }
                } else {
                    self.builder.position_at_end(entry_bb);
                }
                if let Err(e) = self.build_store(slot, struct_ty.const_zero()) {
                    // L2 (audit-codegen 2026-08-03): do not silently swallow —
                    // a failed store leaves the slot undef → free(undef) at
                    // scope exit. Registration contexts cannot propagate
                    // (fn returns ()), so surface the failure loudly.
                    eprintln!("[mimi codegen] warning: heap-slot store failed: {}", e);
                }
            }
        }
        if let Some(saved) = saved {
            self.builder.position_at_end(saved);
        }
        let mut guard = self.heap_allocs.borrow_mut();
        if let Some(stack) = guard.last_mut() {
            stack.push(HeapEntry::EnumBox {
                slot,
                struct_ty,
                boxed_ordinals,
            });
        }
    }

    /// Register a returned `List<string>` value with the caller's heap scope.
    /// The list struct is stored in an entry-block alloca; at scope exit the
    /// cleanup loop frees each element string data and then the data array.
    pub(super) fn register_returned_string_list(
        &self,
        list_sv: inkwell::values::StructValue<'ctx>,
        list_ty: inkwell::types::StructType<'ctx>,
    ) -> Result<(), CompileError> {
        let slot =
            self.build_entry_alloca(BasicTypeEnum::StructType(list_ty), "call_string_list_slot")?;
        self.build_store(slot, list_sv)?;
        let mut guard = self.heap_allocs.borrow_mut();
        if let Some(stack) = guard.last_mut() {
            stack.push(HeapEntry::StringListData { slot, list_ty });
        } else {
            mimi_debug_assert!(false, "register_returned_string_list with no active scope");
            guard.push(vec![HeapEntry::StringListData { slot, list_ty }]);
        }
        Ok(())
    }

    /// Register a returned `List<List<string>>` value with the caller's heap
    /// scope. At scope exit, free every inner `List<string>` (each string and
    /// its data array) and then the outer data array.
    pub(super) fn register_returned_string_list_list(
        &self,
        list_sv: inkwell::values::StructValue<'ctx>,
        list_ty: inkwell::types::StructType<'ctx>,
        elem_list_ty: inkwell::types::StructType<'ctx>,
    ) -> Result<(), CompileError> {
        let slot = self.build_entry_alloca(
            BasicTypeEnum::StructType(list_ty),
            "call_string_list_list_slot",
        )?;
        self.build_store(slot, list_sv)?;
        let mut guard = self.heap_allocs.borrow_mut();
        if let Some(stack) = guard.last_mut() {
            stack.push(HeapEntry::StringListListData {
                slot,
                list_ty,
                elem_list_ty,
            });
        } else {
            mimi_debug_assert!(
                false,
                "register_returned_string_list_list with no active scope"
            );
            guard.push(vec![HeapEntry::StringListListData {
                slot,
                list_ty,
                elem_list_ty,
            }]);
        }
        Ok(())
    }

    /// Register an entry-alloca struct slot whose loaded value should be freed at
    /// scope exit. `field` is the index of the pointer field inside the struct.
    /// At free time, a fresh GEP is emitted from `base` in the current block,
    /// avoiding dominance issues.
    /// NOTE: null-initialisation is NOT done here — the slot must have been
    /// stored to (or covered by `register_heap_alloc`) **before** registration
    /// for all paths that reach this registration.  Scope-local cleanup runs
    /// inside the block (before merge), so the stored value is always valid.
    /// 0.39.x matrix sweep (LOOP-REBIND-HEAP-001): register a raw heap
    /// pointer in the FUNCTION ROOT heap scope (the constructor's seed scope),
    /// not the currently-innermost one. Loop bodies push a per-iteration heap
    /// scope (0.37.30) whose pop frees everything registered inside the body —
    /// correct for temporaries, catastrophic for a value rebound to an
    /// outer-declared variable (`sh_rest = random_remove_ith(sh_rest, i)` in
    /// std/random.mimi: each iteration freed the buffer the variable still
    /// referenced; the next iteration then read freed memory). Transferring
    /// such rebindings to the root scope gives them the variable's lifetime.
    pub(super) fn register_heap_ptr_root(&self, ptr: inkwell::values::PointerValue<'ctx>) {
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let slot = self
            .build_entry_alloca(ptr_ty, "root_heap_slot")
            .unwrap_or(ptr);
        if slot != ptr {
            // Null-init at entry so untaken paths free(null) safely.
            let saved = self.builder.get_insert_block();
            if let Some(slot_inst) = slot.as_instruction() {
                if let Some(next) = slot_inst.get_next_instruction() {
                    self.builder.position_before(&next);
                } else if let Some(parent) = slot_inst.get_parent() {
                    self.builder.position_at_end(parent);
                }
                let _ = self.build_store(slot, ptr_ty.const_null());
            }
            if let Some(saved) = saved {
                self.builder.position_at_end(saved);
            }
            let _ = self.build_store(slot, ptr);
        }
        let boundary = self.heap_boundaries.borrow().last().copied().unwrap_or(0);
        let mut guard = self.heap_allocs.borrow_mut();
        let idx = boundary.min(guard.len().saturating_sub(1));
        if let Some(scope) = guard.get_mut(idx) {
            scope.push(HeapEntry::Ptr(slot));
        }
    }

    /// True iff some heap scope already owns a SLOT entry for this alloca.
    /// LOOP-REBIND-HEAP-001: a rebound variable's owner slot is registered at
    /// first binding; the rebinding must transfer (pop) only the temporary
    /// registration, not add a second entry for the same storage — two entries
    /// loading one data field freed the same buffer twice at function exit.
    pub(super) fn has_heap_slot(&self, base: inkwell::values::PointerValue<'ctx>) -> bool {
        let guard = self.heap_allocs.borrow();
        guard
            .iter()
            .flatten()
            .any(|e| matches!(e, HeapEntry::Slot(b, _, _) if *b == base))
    }

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
    /// 0.39.x (L1 parity fix): remove the heap-slot registration for `base`
    /// (a nested list literal's header alloca) from the current scope. When
    /// an inner list literal becomes an ELEMENT of an outer list literal,
    /// ownership of its data array transfers to the outer container: the
    /// inner registration must be dropped or the enclosing function's
    /// scope-exit frees an array the outer list still references
    /// (use-after-free once the value escapes the frame).
    pub(super) fn claim_nested_list_slot(&self, base: inkwell::values::PointerValue<'ctx>) {
        let mut guard = self.heap_allocs.borrow_mut();
        if let Some(stack) = guard.last_mut() {
            if let Some(pos) = stack
                .iter()
                .rposition(|e| matches!(e, HeapEntry::Slot(b, _, _) if *b == base))
            {
                stack.remove(pos);
            }
        }
    }

    /// 0.1.9 (L1 parity): remove the MOST RECENT HeapEntry::Slot registration
    /// from the current scope. Used when a freshly-built list literal is
    /// stored into a PERSISTENT location (actor/record field): ownership of
    /// its data array transfers to the container, so the enclosing scope must
    /// not free it (LIFO assumption mirrors pop_last_heap_ptr).
    pub(super) fn claim_last_heap_slot(&self) {
        let mut guard = self.heap_allocs.borrow_mut();
        if let Some(stack) = guard.last_mut() {
            if let Some(pos) = stack.iter().rposition(|e| matches!(e, HeapEntry::Slot(..))) {
                stack.remove(pos);
            }
        }
    }

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

    /// Deep-eval 2026-08-09: collect the runtime pointer values of every
    /// live heap registration in the current function scope as i64, for the
    /// resolved string-return ownership probe (`claim_resolved_string_return`,
    /// func.rs). `Ptr` entries materialize their allocation pointer in an
    /// entry-block alloca (register_heap_alloc); the RUNTIME ownership value
    /// is the slot's contents, so probe by loading the slot (entry allocas
    /// dominate every return point, and the new null-init makes untaken
    /// branch slots load null — a safe no-match). `Slot` entries are included
    /// too: local string/list variables transfer their heap temp registration
    /// into a struct field slot, and skipping them made returned nested
    /// heap-owning values heap-copy the strings without freeing the original
    /// slots (the callee drains the scope on return) — a per-call leak in
    /// high-pressure soak tests.
    pub(super) fn heap_probe_candidates(&self) -> Vec<inkwell::values::IntValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let mut out = Vec::new();
        let boundary = self.heap_boundaries.borrow().last().copied().unwrap_or(0);
        let entries: Vec<HeapEntry<'ctx>> = {
            let scopes = self.heap_allocs.borrow();
            scopes
                .iter()
                .skip(boundary)
                .flat_map(|scope| scope.iter().cloned())
                .collect()
        };
        for entry in entries {
            let slot = match entry {
                HeapEntry::Ptr(slot) => slot,
                HeapEntry::Slot(base, struct_ty, field) => {
                    let Ok(gep) =
                        self.gep()
                            .build_struct_gep(struct_ty, base, field, "res_ret_probe_gep")
                    else {
                        continue;
                    };
                    gep
                }
                _ => continue,
            };
            let Ok(loaded) = self.build_load(ptr_ty, slot, "res_ret_probe_ld") else {
                continue;
            };
            let BasicValueEnum::PointerValue(pv) = loaded else {
                continue;
            };
            if let Ok(iv) = self.build_ptr_to_int(pv, i64_ty, "res_ret_cand_i") {
                out.push(iv);
            }
        }
        out
    }

    /// B9 (audit): record an escaping closure env pointer so the next
    /// `free_heap_allocs` skips it at scope exit. Ownership of the env
    /// transfers to the caller, which registers it at its own call site
    /// (see `track_closure_return_lifetime`).
    pub(super) fn claim_closure_env(&self, env_ptr: inkwell::values::PointerValue<'ctx>) {
        self.claimed_returned_envs.borrow_mut().push(env_ptr);
    }

    /// Claim a returned `List<string>` so its element string pointers survive
    /// an early-return flush. The list struct is stored in an entry-block
    /// alloca; `flush_heap_scopes_to_boundary` can then inspect the elements
    /// at runtime and skip freeing any matching string data pointers.
    pub(super) fn claim_returned_string_list(
        &self,
        list_sv: inkwell::values::StructValue<'ctx>,
        list_ty: inkwell::types::StructType<'ctx>,
    ) -> Result<(), CompileError> {
        let slot = self.build_entry_alloca(
            BasicTypeEnum::StructType(list_ty),
            "claimed_string_list_slot",
        )?;
        self.build_store(slot, list_sv)?;
        self.claimed_returned_string_lists
            .borrow_mut()
            .push((slot, list_ty));
        Ok(())
    }

    /// Claim a returned `List<List<string>>` value. The outer list struct is
    /// stored in an entry-block alloca; at flush time the membership check can
    /// inspect every inner list box, inner data array, and string element.
    pub(super) fn claim_returned_string_list_list(
        &self,
        list_sv: inkwell::values::StructValue<'ctx>,
        list_ty: inkwell::types::StructType<'ctx>,
        elem_list_ty: inkwell::types::StructType<'ctx>,
    ) -> Result<(), CompileError> {
        let slot = self.build_entry_alloca(
            BasicTypeEnum::StructType(list_ty),
            "claimed_string_list_list_slot",
        )?;
        self.build_store(slot, list_sv)?;
        self.claimed_returned_string_list_lists
            .borrow_mut()
            .push((slot, list_ty, elem_list_ty));
        Ok(())
    }

    /// Track the result type of `weak_var.upgrade()` for a `let` binding.
    /// `w.upgrade()` returns `Option<T>` where `T` is the inner type of the
    /// weak reference. Updating `var_type_names`/`var_types` lets downstream
    /// method dispatch (`is_none`, `unwrap`) find the Option implementation.
    pub(super) fn track_weak_upgrade_type(&mut self, name: &str, obj: &Expr) {
        if let Expr::Ident(obj_name) = obj.unlocated() {
            if let Some(ty) = self.var_types.get(obj_name).cloned() {
                let inner = match ty.unlocated() {
                    Type::Weak(inner) => inner.clone(),
                    _ => return,
                };
                let inner_name = crate::core::fmt_type(&inner);
                self.var_type_names
                    .insert(name.to_string(), format!("Option<{}>", inner_name));
                self.var_types
                    .insert(name.to_string(), Type::Option(inner.clone()));
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
                        self.builder
                            .position_at_end(base_inst.get_parent().unwrap_or(entry_bb));
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

    /// B9: begin a function-level heap scope. Records the current depth as a
    /// boundary so early-return flushes (`flush_heap_scopes_to_boundary`)
    /// stop here, preserving caller scopes when a callee is compiled
    /// mid-caller (generic monomorphization, nested lambda bodies, actor
    /// method calls). The matching `end_function_heap_scope` runs at the
    /// function compile's exit.
    pub(super) fn begin_function_heap_scope(&self) {
        let depth = self.heap_allocs.borrow().len();
        self.heap_boundaries.borrow_mut().push(depth);
        self.push_heap_scope();
    }

    /// B9: pop the function-boundary marker and discard (without emitting)
    /// every heap scope above it. The frees for each runtime path were
    /// already emitted by the path's own `flush_heap_scopes_to_boundary`
    /// call; this only balances the compile-time bookkeeping stack.
    pub(super) fn end_function_heap_scope(&self) {
        let boundary = match self.heap_boundaries.borrow_mut().pop() {
            Some(b) => b,
            None => {
                mimi_debug_assert!(false, "end_function_heap_scope without begin");
                return;
            }
        };
        let mut scopes = self.heap_allocs.borrow_mut();
        while scopes.len() > boundary {
            scopes.pop();
        }
    }

    /// B9: emit frees for every registered allocation in the scopes from the
    /// top down to (but not including) the innermost function boundary,
    /// skipping escaping closure envs claimed by the current return
    /// (one-shot: claims are drained here, all guards are emitted in the
    /// current block, so no cross-block SSA references are possible).
    ///
    /// The scopes are NOT popped and their entries are NOT removed — the
    /// compile-time stack is shared across all control-flow paths, and each
    /// return site must emit its own path-specific frees (e.g. the
    /// if-fallthrough path frees the env that is dead on that path, while
    /// the then-path's claim skips it there). Scope bookkeeping is balanced
    /// by `end_function_heap_scope` at the end of the function compile.
    ///
    /// Early-return paths call this instead of `free_heap_allocs` so that
    /// function-level registrations (e.g. closure env slots tracked at call
    /// sites) are released on every exit path. Without an active boundary
    /// (untracked path, e.g. async poll), falls back to today's single-scope
    /// free emission.
    pub(super) fn flush_heap_scopes_to_boundary(&mut self) -> Result<(), CompileError> {
        let claimed = std::mem::take(&mut *self.claimed_returned_envs.borrow_mut());
        let claimed_string_lists =
            std::mem::take(&mut *self.claimed_returned_string_lists.borrow_mut());
        let claimed_string_list_lists =
            std::mem::take(&mut *self.claimed_returned_string_list_lists.borrow_mut());
        let boundary = self
            .heap_boundaries
            .borrow()
            .last()
            .copied()
            .unwrap_or_else(|| self.heap_allocs.borrow().len().saturating_sub(1));
        let free_fn = self
            .module
            .get_function("free")
            .ok_or_else(|| CompileError::LlvmError("free not declared".to_string()))?;
        // Snapshot the entries in every scope at or above the boundary.
        // The scopes themselves stay intact for the remaining paths.
        let entries: Vec<HeapEntry<'ctx>> = {
            let scopes = self.heap_allocs.borrow();
            scopes
                .iter()
                .skip(boundary)
                .flat_map(|scope| scope.iter().cloned())
                .collect()
        };
        for entry in entries {
            // L6: EnumBox needs a tag-conditional free; handle separately.
            if let HeapEntry::EnumBox {
                slot,
                struct_ty,
                boxed_ordinals,
            } = entry
            {
                self.emit_enum_box_free(free_fn, slot, struct_ty, &boxed_ordinals, &claimed)?;
                continue;
            }
            if let HeapEntry::StringListData { slot, list_ty } = entry {
                self.emit_string_list_data_free(slot, list_ty)?;
                self.builder
                    .build_store(slot, list_ty.const_zero())
                    .map_err(|e| CompileError::LlvmError(format!("string-list reset: {e}")))?;
                continue;
            }
            if let HeapEntry::StringListListData {
                slot,
                list_ty,
                elem_list_ty,
            } = entry
            {
                self.emit_string_list_list_data_free(slot, list_ty, elem_list_ty)?;
                self.builder
                    .build_store(slot, list_ty.const_zero())
                    .map_err(|e| CompileError::LlvmError(format!("string-list-list reset: {e}")))?;
                continue;
            }
            let ptr = match entry {
                HeapEntry::Ptr(slot) => {
                    // register_heap_alloc stores the pointer into an
                    // entry-block alloca; load it so the free uses a value
                    // that dominates this block (SSA dominance).
                    let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    self.builder
                        .build_load(ptr_ty, slot, "heap_slot")
                        .map_err(|e| {
                            CompileError::LlvmError(format!("heap slot load error: {}", e))
                        })?
                        .into_pointer_value()
                }
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
                HeapEntry::EnumBox { .. } => unreachable!("handled above"),
                HeapEntry::StringListData { .. } => unreachable!("string-list handled above"),
                HeapEntry::StringListListData { .. } => {
                    unreachable!("string-list-list handled above")
                }
            };
            if claimed.is_empty()
                && claimed_string_lists.is_empty()
                && claimed_string_list_lists.is_empty()
            {
                self.builder
                    .build_call(
                        free_fn,
                        &[BasicMetadataValueEnum::PointerValue(ptr)],
                        "free_heap",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("free error: {}", e)))?;
            } else {
                // Value-exact runtime comparison: skip the free when the
                // pointer is a claimed escaping closure env — ownership
                // transferred to the caller.
                self.emit_guarded_scope_free(
                    free_fn,
                    ptr,
                    &claimed,
                    &claimed_string_lists,
                    &claimed_string_list_lists,
                )?;
            }
        }
        Ok(())
    }

    /// Pop the current heap scope WITHOUT freeing any allocations.
    /// Used by the resolved emitter when returning string values:
    /// the caller takes ownership of the heap data.
    pub(super) fn drain_heap_scope(&self) {
        self.heap_allocs.borrow_mut().pop();
    }

    /// G10: Pop scope level and emit `free(ptr)` for each registered heap allocation.
    ///
    /// For all entry types the heap pointer is loaded from an entry-block alloca
    /// (null-initialized at the entry block), so the slot always dominates the
    /// cleanup point. The null guarantee ensures that `free` on a never-allocated
    /// path calls free(null), which is a C-library no-op.
    pub(super) fn free_heap_allocs(&mut self) -> Result<(), CompileError> {
        // B9 (audit): claims persist across nested scope pops so an escaping
        // closure env registered in an outer (function-level) scope stays
        // guarded until that scope itself is popped. The seed scope pushed by
        // the constructor marks the function boundary — when a pop returns the
        // stack to it, the function's registrations are all gone and claims
        // are no longer needed. Stale claims are harmless beyond that: they
        // only suppress frees of pointers that can never equal them.
        let claimed = std::mem::take(&mut *self.claimed_returned_envs.borrow_mut());
        let claimed_string_lists =
            std::mem::take(&mut *self.claimed_returned_string_lists.borrow_mut());
        let claimed_string_list_lists =
            std::mem::take(&mut *self.claimed_returned_string_list_lists.borrow_mut());
        let scope = self.heap_allocs.borrow_mut().pop();
        if self.heap_allocs.borrow().len() > 1 {
            // codegen_mod F1: claims persist across nested scope pops so an
            // escaping closure env OR `List<string>` registered in an outer
            // (function-level) scope stays guarded until that scope itself is
            // popped. The seed scope pushed by the constructor marks the
            // function boundary — when a pop returns the stack to it, the
            // function's registrations are all gone and claims are no longer
            // needed. Without this, an escaped `List<string>` loses its guard
            // when an inner scope pops and is freed → UAF / double-free.
            *self.claimed_returned_envs.borrow_mut() = claimed.clone();
            *self.claimed_returned_string_lists.borrow_mut() = claimed_string_lists.clone();
            *self.claimed_returned_string_list_lists.borrow_mut() =
                claimed_string_list_lists.clone();
        }
        if let Some(scope) = scope {
            let free_fn = self
                .module
                .get_function("free")
                .ok_or_else(|| CompileError::LlvmError("free not declared".to_string()))?;
            // 0.39.x matrix sweep (LOOP-REBIND-HEAP-001): enforce release
            // uniqueness for this flush session — multiple ownership sources
            // (construction registration + call-result tracking + binding
            // transfer) can legitimately point at one allocation, e.g. an impl
            // method that returns its receiver verbatim. Duplicate entries now
            // degrade to a leak instead of a double free.
            match self.get_runtime_fn("mimi_heap_guard_reset") {
                Ok(reset_fn) => {
                    self.builder
                        .build_call(reset_fn, &[], "heap_guard_reset")
                        .map_err(|e| CompileError::LlvmError(format!("guard reset: {e}")))?;
                }
                Err(_) => {
                    eprintln!(
                        "[mimi codegen] warning: mimi_heap_guard_reset unavailable; \
                         free-uniqueness guard inactive"
                    );
                }
            }
            for entry in scope {
                // L6: EnumBox needs a tag-conditional free (only Packed variants
                // carry a box); handle it separately from the plain-pointer entries.
                if let HeapEntry::EnumBox {
                    slot,
                    struct_ty,
                    boxed_ordinals,
                } = entry
                {
                    self.emit_enum_box_free(free_fn, slot, struct_ty, &boxed_ordinals, &claimed)?;
                    // L6b (D-4, 2026-08-06): after a tag-conditional free, reset
                    // the slot to zero so a subsequent scope pop for a branch
                    // that never re-stored the slot (e.g. the untaken arm of an
                    // `if` inside a loop) frees tag=0/payload=0 — a no-op —
                    // instead of the previous iteration's already-freed box.
                    // Without the reset, the stale tag could re-trigger the free.
                    self.builder
                        .build_store(slot, struct_ty.const_zero())
                        .map_err(|e| CompileError::LlvmError(format!("enum-box reset: {}", e)))?;
                    continue;
                }
                if let HeapEntry::StringListData { slot, list_ty } = entry {
                    self.emit_string_list_data_free(slot, list_ty)?;
                    self.builder
                        .build_store(slot, list_ty.const_zero())
                        .map_err(|e| CompileError::LlvmError(format!("string-list reset: {e}")))?;
                    continue;
                }
                if let HeapEntry::StringListListData {
                    slot,
                    list_ty,
                    elem_list_ty,
                } = entry
                {
                    self.emit_string_list_list_data_free(slot, list_ty, elem_list_ty)?;
                    self.builder
                        .build_store(slot, list_ty.const_zero())
                        .map_err(|e| {
                            CompileError::LlvmError(format!("string-list-list reset: {e}"))
                        })?;
                    continue;
                }
                let (ptr, reset_target) = match entry {
                    HeapEntry::Ptr(slot) => {
                        // Load from the entry-block alloca (see register_heap_alloc).
                        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                        let ptr = self
                            .builder
                            .build_load(ptr_ty, slot, "heap_slot")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("heap slot load error: {}", e))
                            })?
                            .into_pointer_value();
                        (ptr, Some(slot))
                    }
                    HeapEntry::Slot(base, struct_ty, field) => {
                        let gep = self
                            .gep()
                            .build_struct_gep(struct_ty, base, field, "heap_slot_gep")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("heap slot gep error: {}", e))
                            })?;
                        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                        let ptr = self
                            .builder
                            .build_load(ptr_ty, gep, "heap_slot")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("heap slot load error: {}", e))
                            })?
                            .into_pointer_value();
                        (ptr, Some(gep))
                    }
                    HeapEntry::EnumBox { .. } => unreachable!("handled above"),
                    HeapEntry::StringListData { .. } => unreachable!("string-list handled above"),
                    HeapEntry::StringListListData { .. } => {
                        unreachable!("string-list-list handled above")
                    }
                };
                // LOOP-REBIND-HEAP-001: route through the uniqueness guard.
                // mimi_heap_free_claim returns null for pointers already freed
                // in this session; only fresh pointers reach free.
                let guarded = if claimed.is_empty() {
                    true
                } else {
                    self.emit_guarded_scope_free(
                        free_fn,
                        ptr,
                        &claimed,
                        &claimed_string_lists,
                        &claimed_string_list_lists,
                    )?;
                    false
                };
                if guarded {
                    let claim_fn = self.get_runtime_fn("mimi_heap_free_claim").ok();
                    match claim_fn {
                        Some(claim_fn) => {
                            let claim = self
                                .builder
                                .build_call(
                                    claim_fn,
                                    &[BasicMetadataValueEnum::PointerValue(ptr)],
                                    "heap_claim",
                                )
                                .map_err(|e| CompileError::LlvmError(format!("heap claim: {e}")))?
                                .try_as_basic_value();
                            let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                            let claim_ptr = match claim {
                                inkwell::values::ValueKind::Basic(bv) => bv.into_pointer_value(),
                                _ => ptr_ty.const_null(),
                            };
                            let is_fresh = self
                                .builder
                                .build_is_not_null(claim_ptr, "heap_claim_fresh")
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("claim check: {e}"))
                                })?;
                            let function = self.current_function().ok_or_else(|| {
                                CompileError::LlvmError("no current function for guard".into())
                            })?;
                            let do_free_bb = self
                                .context
                                .append_basic_block(function, "heap_guard_do_free");
                            let skip_bb =
                                self.context.append_basic_block(function, "heap_guard_skip");
                            self.build_cond_br(is_fresh, do_free_bb, skip_bb)?;
                            self.builder.position_at_end(do_free_bb);
                            self.builder
                                .build_call(
                                    free_fn,
                                    &[BasicMetadataValueEnum::PointerValue(claim_ptr)],
                                    "free_unique",
                                )
                                .map_err(|e| CompileError::LlvmError(format!("free error: {e}")))?;
                            self.build_br(skip_bb)?;
                            self.builder.position_at_end(skip_bb);
                        }
                        None => {
                            self.builder
                                .build_call(
                                    free_fn,
                                    &[BasicMetadataValueEnum::PointerValue(ptr)],
                                    "free_heap",
                                )
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("free error: {}", e))
                                })?;
                        }
                    }
                }
                // L6c (D-4, 2026-08-06): reset the heap slot to null right after
                // the free. When a conditional (e.g. `if` with a `Some(string)`
                // arm) registers the allocation only on one branch, the slot is
                // entry-block state that survives into the next iteration of a
                // loop. The untaken branch never re-stores it, so the next
                // scope pop would free the previous iteration's already-freed
                // pointer — a double free under glibc tcache. Resetting here
                // turns the stale free into `free(null)`, a no-op.
                if let Some(target) = reset_target {
                    let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    self.builder
                        .build_store(target, ptr_ty.const_null())
                        .map_err(|e| CompileError::LlvmError(format!("heap slot reset: {}", e)))?;
                }
            }
        }
        Ok(())
    }

    /// Emit frees for the current innermost heap scope WITHOUT popping it.
    ///
    /// Used by loop `break`/`continue` paths: those branches must release the
    /// current iteration's loop-local heap allocations immediately, while the
    /// compile-time scope stack must remain balanced for other branches that
    /// are still being emitted. A later normal-path `free_heap_allocs()` will
    /// emit frees again, but those frees execute on a different runtime path
    /// where the slots are null (or belong to that path's own allocations).
    pub(super) fn emit_frees_for_top_scope(&mut self) -> Result<(), CompileError> {
        let claimed = std::mem::take(&mut *self.claimed_returned_envs.borrow_mut());
        let claimed_string_lists =
            std::mem::take(&mut *self.claimed_returned_string_lists.borrow_mut());
        let claimed_string_list_lists =
            std::mem::take(&mut *self.claimed_returned_string_list_lists.borrow_mut());
        let scope = self.heap_allocs.borrow().last().cloned();
        if let Some(scope) = scope {
            let free_fn = self
                .module
                .get_function("free")
                .ok_or_else(|| CompileError::LlvmError("free not declared".to_string()))?;
            // 0.39.x matrix sweep (LOOP-REBIND-HEAP-001): enforce release
            // uniqueness for this flush session — multiple ownership sources
            // (construction registration + call-result tracking + binding
            // transfer) can legitimately point at one allocation, e.g. an impl
            // method that returns its receiver verbatim. Duplicate entries now
            // degrade to a leak instead of a double free.
            match self.get_runtime_fn("mimi_heap_guard_reset") {
                Ok(reset_fn) => {
                    self.builder
                        .build_call(reset_fn, &[], "heap_guard_reset")
                        .map_err(|e| CompileError::LlvmError(format!("guard reset: {e}")))?;
                }
                Err(_) => {
                    eprintln!(
                        "[mimi codegen] warning: mimi_heap_guard_reset unavailable; \
                         free-uniqueness guard inactive"
                    );
                }
            }
            for entry in scope {
                if let HeapEntry::EnumBox {
                    slot,
                    struct_ty,
                    boxed_ordinals,
                } = entry
                {
                    self.emit_enum_box_free(free_fn, slot, struct_ty, &boxed_ordinals, &claimed)?;
                    self.builder
                        .build_store(slot, struct_ty.const_zero())
                        .map_err(|e| CompileError::LlvmError(format!("enum-box reset: {}", e)))?;
                    continue;
                }
                if let HeapEntry::StringListData { slot, list_ty } = entry {
                    self.emit_string_list_data_free(slot, list_ty)?;
                    self.builder
                        .build_store(slot, list_ty.const_zero())
                        .map_err(|e| CompileError::LlvmError(format!("string-list reset: {e}")))?;
                    continue;
                }
                if let HeapEntry::StringListListData {
                    slot,
                    list_ty,
                    elem_list_ty,
                } = entry
                {
                    self.emit_string_list_list_data_free(slot, list_ty, elem_list_ty)?;
                    self.builder
                        .build_store(slot, list_ty.const_zero())
                        .map_err(|e| {
                            CompileError::LlvmError(format!("string-list-list reset: {e}"))
                        })?;
                    continue;
                }
                let (ptr, reset_target) = match entry {
                    HeapEntry::Ptr(slot) => {
                        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                        let ptr = self
                            .builder
                            .build_load(ptr_ty, slot, "heap_slot")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("heap slot load error: {}", e))
                            })?
                            .into_pointer_value();
                        (ptr, Some(slot))
                    }
                    HeapEntry::Slot(base, struct_ty, field) => {
                        let gep = self
                            .gep()
                            .build_struct_gep(struct_ty, base, field, "heap_slot_gep")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("heap slot gep error: {}", e))
                            })?;
                        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                        let ptr = self
                            .builder
                            .build_load(ptr_ty, gep, "heap_slot")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("heap slot load error: {}", e))
                            })?
                            .into_pointer_value();
                        (ptr, Some(gep))
                    }
                    HeapEntry::EnumBox { .. } => unreachable!("handled above"),
                    HeapEntry::StringListData { .. } => unreachable!("string-list handled above"),
                    HeapEntry::StringListListData { .. } => {
                        unreachable!("string-list-list handled above")
                    }
                };
                // LOOP-REBIND-HEAP-001: route through the uniqueness guard.
                // mimi_heap_free_claim returns null for pointers already freed
                // in this session; only fresh pointers reach free.
                let guarded = if claimed.is_empty() {
                    true
                } else {
                    self.emit_guarded_scope_free(
                        free_fn,
                        ptr,
                        &claimed,
                        &claimed_string_lists,
                        &claimed_string_list_lists,
                    )?;
                    false
                };
                if guarded {
                    let claim_fn = self.get_runtime_fn("mimi_heap_free_claim").ok();
                    match claim_fn {
                        Some(claim_fn) => {
                            let claim = self
                                .builder
                                .build_call(
                                    claim_fn,
                                    &[BasicMetadataValueEnum::PointerValue(ptr)],
                                    "heap_claim",
                                )
                                .map_err(|e| CompileError::LlvmError(format!("heap claim: {e}")))?
                                .try_as_basic_value();
                            let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                            let claim_ptr = match claim {
                                inkwell::values::ValueKind::Basic(bv) => bv.into_pointer_value(),
                                _ => ptr_ty.const_null(),
                            };
                            let is_fresh = self
                                .builder
                                .build_is_not_null(claim_ptr, "heap_claim_fresh")
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("claim check: {e}"))
                                })?;
                            let function = self.current_function().ok_or_else(|| {
                                CompileError::LlvmError("no current function for guard".into())
                            })?;
                            let do_free_bb = self
                                .context
                                .append_basic_block(function, "heap_guard_do_free");
                            let skip_bb =
                                self.context.append_basic_block(function, "heap_guard_skip");
                            self.build_cond_br(is_fresh, do_free_bb, skip_bb)?;
                            self.builder.position_at_end(do_free_bb);
                            self.builder
                                .build_call(
                                    free_fn,
                                    &[BasicMetadataValueEnum::PointerValue(claim_ptr)],
                                    "free_unique",
                                )
                                .map_err(|e| CompileError::LlvmError(format!("free error: {e}")))?;
                            self.build_br(skip_bb)?;
                            self.builder.position_at_end(skip_bb);
                        }
                        None => {
                            self.builder
                                .build_call(
                                    free_fn,
                                    &[BasicMetadataValueEnum::PointerValue(ptr)],
                                    "free_heap",
                                )
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("free error: {}", e))
                                })?;
                        }
                    }
                }
                if let Some(target) = reset_target {
                    let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    self.builder
                        .build_store(target, ptr_ty.const_null())
                        .map_err(|e| CompileError::LlvmError(format!("heap slot reset: {}", e)))?;
                }
            }
        }
        // codegen_mod F1: restore string-list claims alongside the env claim so
        // an escaping `List<string>` stays guarded across this non-destructive
        // top-scope free (loop break/continue path) until the function's own
        // scope is popped.
        *self.claimed_returned_envs.borrow_mut() = claimed;
        *self.claimed_returned_string_lists.borrow_mut() = claimed_string_lists;
        *self.claimed_returned_string_list_lists.borrow_mut() = claimed_string_list_lists;
        Ok(())
    }

    /// Emit a runtime traversal checking whether `ptr` is owned by a claimed
    /// `List<List<string>>`: an inner list box, an inner data array, or any
    /// string element inside an inner list.
    fn emit_string_list_list_contains(
        &mut self,
        ptr: inkwell::values::PointerValue<'ctx>,
        slot: inkwell::values::PointerValue<'ctx>,
        list_ty: inkwell::types::StructType<'ctx>,
        elem_list_ty: inkwell::types::StructType<'ctx>,
    ) -> Result<inkwell::values::IntValue<'ctx>, CompileError> {
        let i64_ty = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_sv = self
            .builder
            .build_load(BasicTypeEnum::StructType(list_ty), slot, "claimed_sll_load")
            .map_err(|e| CompileError::LlvmError(format!("claimed sll load: {e}")))?
            .into_struct_value();
        let outer_len = self
            .builder
            .build_extract_value(list_sv, 0, "claimed_sll_outer_len")
            .map_err(|e| CompileError::LlvmError(format!("claimed sll outer len: {e}")))?
            .into_int_value();
        let outer_data = self
            .builder
            .build_extract_value(list_sv, 1, "claimed_sll_outer_data")
            .map_err(|e| CompileError::LlvmError(format!("claimed sll outer data: {e}")))?
            .into_pointer_value();
        let function = self
            .current_function()
            .ok_or_else(|| CompileError::LlvmError("claimed sll outside function".into()))?;
        let outer_header = self
            .context
            .append_basic_block(function, "claimed_sll_outer_header");
        let outer_body = self
            .context
            .append_basic_block(function, "claimed_sll_outer_body");
        let outer_done = self
            .context
            .append_basic_block(function, "claimed_sll_outer_done");
        let outer_idx =
            self.build_alloca(BasicTypeEnum::IntType(i64_ty), "claimed_sll_outer_idx")?;
        let found = self.build_alloca(
            BasicTypeEnum::IntType(self.context.bool_type()),
            "claimed_sll_found",
        )?;
        self.build_store(outer_idx, i64_ty.const_int(0, false))?;
        self.build_store(found, self.context.bool_type().const_int(0, false))?;
        self.build_br(outer_header)?;

        self.builder.position_at_end(outer_header);
        let oi = self
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                outer_idx,
                "claimed_sll_outer_idx_val",
            )?
            .into_int_value();
        let outer_cond = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                oi,
                outer_len,
                "claimed_sll_outer_cond",
            )
            .map_err(|e| CompileError::LlvmError(format!("claimed sll outer cmp: {e}")))?;
        self.build_cond_br(outer_cond, outer_body, outer_done)?;

        self.builder.position_at_end(outer_body);
        let elem_slot =
            self.build_in_bounds_gep(i64_ty, outer_data, &[oi], "claimed_sll_elem_slot")?;
        let inner_handle = self
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                elem_slot,
                "claimed_sll_inner_handle",
            )?
            .into_int_value();
        let inner_ptr = self.build_int_to_ptr(inner_handle, ptr_ty, "claimed_sll_inner_ptr")?;
        let box_eq = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                inner_ptr,
                ptr,
                "claimed_sll_box_eq",
            )
            .map_err(|e| CompileError::LlvmError(format!("claimed sll box eq: {e}")))?;
        let found_box = self
            .build_load(
                BasicTypeEnum::IntType(self.context.bool_type()),
                found,
                "claimed_sll_found_box",
            )?
            .into_int_value();
        let found_after_box = self
            .builder
            .build_or(found_box, box_eq, "claimed_sll_found_after_box")
            .map_err(|e| CompileError::LlvmError(format!("claimed sll box or: {e}")))?;
        self.build_store(found, found_after_box)?;

        let inner_sv = self
            .build_load(
                BasicTypeEnum::StructType(elem_list_ty),
                inner_ptr,
                "claimed_sll_inner_sv",
            )?
            .into_struct_value();
        let inner_len = self
            .builder
            .build_extract_value(inner_sv, 0, "claimed_sll_inner_len")
            .map_err(|e| CompileError::LlvmError(format!("claimed sll inner len: {e}")))?
            .into_int_value();
        let inner_data = self
            .builder
            .build_extract_value(inner_sv, 1, "claimed_sll_inner_data")
            .map_err(|e| CompileError::LlvmError(format!("claimed sll inner data: {e}")))?
            .into_pointer_value();
        let data_eq = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                inner_data,
                ptr,
                "claimed_sll_data_eq",
            )
            .map_err(|e| CompileError::LlvmError(format!("claimed sll data eq: {e}")))?;
        let found_data = self
            .build_load(
                BasicTypeEnum::IntType(self.context.bool_type()),
                found,
                "claimed_sll_found_data",
            )?
            .into_int_value();
        let found_after_data = self
            .builder
            .build_or(found_data, data_eq, "claimed_sll_found_after_data")
            .map_err(|e| CompileError::LlvmError(format!("claimed sll data or: {e}")))?;
        self.build_store(found, found_after_data)?;

        let inner_header = self
            .context
            .append_basic_block(function, "claimed_sll_inner_header");
        let inner_body = self
            .context
            .append_basic_block(function, "claimed_sll_inner_body");
        let inner_done = self
            .context
            .append_basic_block(function, "claimed_sll_inner_done");
        let ii_storage =
            self.build_alloca(BasicTypeEnum::IntType(i64_ty), "claimed_sll_inner_idx")?;
        self.build_store(ii_storage, i64_ty.const_int(0, false))?;
        self.build_br(inner_header)?;

        self.builder.position_at_end(inner_header);
        let ii = self
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                ii_storage,
                "claimed_sll_inner_idx_val",
            )?
            .into_int_value();
        let inner_cond = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                ii,
                inner_len,
                "claimed_sll_inner_cond",
            )
            .map_err(|e| CompileError::LlvmError(format!("claimed sll inner cmp: {e}")))?;
        self.build_cond_br(inner_cond, inner_body, inner_done)?;

        self.builder.position_at_end(inner_body);
        let inner_elem_slot =
            self.build_in_bounds_gep(i64_ty, inner_data, &[ii], "claimed_sll_inner_elem_slot")?;
        let inner_elem_i64 = self
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                inner_elem_slot,
                "claimed_sll_inner_elem_i64",
            )?
            .into_int_value();
        let inner_elem_ptr =
            self.build_int_to_ptr(inner_elem_i64, ptr_ty, "claimed_sll_inner_elem_ptr")?;
        let elem_eq = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                inner_elem_ptr,
                ptr,
                "claimed_sll_elem_eq",
            )
            .map_err(|e| CompileError::LlvmError(format!("claimed sll elem eq: {e}")))?;
        let f = self
            .build_load(
                BasicTypeEnum::IntType(self.context.bool_type()),
                found,
                "claimed_sll_found_elem",
            )?
            .into_int_value();
        let f2 = self
            .builder
            .build_or(f, elem_eq, "claimed_sll_found_elem_new")
            .map_err(|e| CompileError::LlvmError(format!("claimed sll elem or: {e}")))?;
        self.build_store(found, f2)?;
        let ii_next = self
            .builder
            .build_int_add(ii, i64_ty.const_int(1, false), "claimed_sll_inner_idx_next")
            .map_err(|e| CompileError::LlvmError(format!("claimed sll inner inc: {e}")))?;
        self.build_store(ii_storage, ii_next)?;
        self.build_br(inner_header)?;

        self.builder.position_at_end(inner_done);
        let oi_next = self
            .builder
            .build_int_add(oi, i64_ty.const_int(1, false), "claimed_sll_outer_idx_next")
            .map_err(|e| CompileError::LlvmError(format!("claimed sll outer inc: {e}")))?;
        self.build_store(outer_idx, oi_next)?;
        self.build_br(outer_header)?;

        self.builder.position_at_end(outer_done);
        Ok(self
            .build_load(
                BasicTypeEnum::IntType(self.context.bool_type()),
                found,
                "claimed_sll_result",
            )?
            .into_int_value())
    }

    /// Emit a runtime loop checking whether `ptr` is one of the string data
    /// pointers owned by a claimed `List<string>`.
    fn emit_string_list_contains(
        &mut self,
        ptr: inkwell::values::PointerValue<'ctx>,
        slot: inkwell::values::PointerValue<'ctx>,
        list_ty: inkwell::types::StructType<'ctx>,
    ) -> Result<inkwell::values::IntValue<'ctx>, CompileError> {
        let i64_ty = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_sv = self
            .builder
            .build_load(
                BasicTypeEnum::StructType(list_ty),
                slot,
                "claimed_str_list_load",
            )
            .map_err(|e| CompileError::LlvmError(format!("claimed str list load: {e}")))?
            .into_struct_value();
        let len = self
            .builder
            .build_extract_value(list_sv, 0, "claimed_str_list_len")
            .map_err(|e| CompileError::LlvmError(format!("claimed str list len: {e}")))?
            .into_int_value();
        let data = self
            .builder
            .build_extract_value(list_sv, 1, "claimed_str_list_data")
            .map_err(|e| CompileError::LlvmError(format!("claimed str list data: {e}")))?
            .into_pointer_value();
        let function = self
            .current_function()
            .ok_or_else(|| CompileError::LlvmError("claimed str list outside function".into()))?;
        let header = self
            .context
            .append_basic_block(function, "claimed_str_list_header");
        let body = self
            .context
            .append_basic_block(function, "claimed_str_list_body");
        let done = self
            .context
            .append_basic_block(function, "claimed_str_list_done");
        let idx_storage =
            self.build_alloca(BasicTypeEnum::IntType(i64_ty), "claimed_str_list_idx")?;
        let found_storage = self.build_alloca(
            BasicTypeEnum::IntType(self.context.bool_type()),
            "claimed_str_list_found",
        )?;
        self.build_store(idx_storage, i64_ty.const_int(0, false))?;
        self.build_store(found_storage, self.context.bool_type().const_int(0, false))?;
        self.build_br(header)?;

        self.builder.position_at_end(header);
        let idx = self
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                idx_storage,
                "claimed_str_list_idx_val",
            )?
            .into_int_value();
        let cond = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                idx,
                len,
                "claimed_str_list_cond",
            )
            .map_err(|e| CompileError::LlvmError(format!("claimed str list cmp: {e}")))?;
        self.build_cond_br(cond, body, done)?;

        self.builder.position_at_end(body);
        let elem_slot =
            self.build_in_bounds_gep(i64_ty, data, &[idx], "claimed_str_list_elem_slot")?;
        let elem_i64 = self
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                elem_slot,
                "claimed_str_list_elem_i64",
            )?
            .into_int_value();
        let elem_ptr = self.build_int_to_ptr(elem_i64, ptr_ty, "claimed_str_list_elem_ptr")?;
        let eq = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                elem_ptr,
                ptr,
                "claimed_str_list_elem_eq",
            )
            .map_err(|e| CompileError::LlvmError(format!("claimed str list eq: {e}")))?;
        let found = self
            .build_load(
                BasicTypeEnum::IntType(self.context.bool_type()),
                found_storage,
                "claimed_str_list_found_val",
            )?
            .into_int_value();
        let found_new = self
            .builder
            .build_or(found, eq, "claimed_str_list_found_new")
            .map_err(|e| CompileError::LlvmError(format!("claimed str list or: {e}")))?;
        self.build_store(found_storage, found_new)?;
        let next = self
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "claimed_str_list_idx_next")
            .map_err(|e| CompileError::LlvmError(format!("claimed str list inc: {e}")))?;
        self.build_store(idx_storage, next)?;
        self.build_br(header)?;

        self.builder.position_at_end(done);
        Ok(self
            .build_load(
                BasicTypeEnum::IntType(self.context.bool_type()),
                found_storage,
                "claimed_str_list_result",
            )?
            .into_int_value())
    }

    /// Emit `if (ptr != claimed_0 && ptr != claimed_1 && ...) { free(ptr); }`.
    /// Also skips when `ptr` is one of the string elements in a claimed
    /// `List<string>`. Splits the current block; the insertion point is left
    /// at the merge block so subsequent emission flows normally.
    fn emit_guarded_scope_free(
        &mut self,
        free_fn: inkwell::values::FunctionValue<'ctx>,
        ptr: inkwell::values::PointerValue<'ctx>,
        claimed: &[inkwell::values::PointerValue<'ctx>],
        claimed_string_lists: &[(
            inkwell::values::PointerValue<'ctx>,
            inkwell::types::StructType<'ctx>,
        )],
        claimed_string_list_lists: &[(
            inkwell::values::PointerValue<'ctx>,
            inkwell::types::StructType<'ctx>,
            inkwell::types::StructType<'ctx>,
        )],
    ) -> Result<(), CompileError> {
        let i1_ty = self.context.bool_type();
        let mut matched = i1_ty.const_int(0, false);
        for env in claimed {
            let eq = self
                .builder
                .build_int_compare(inkwell::IntPredicate::EQ, ptr, *env, "b9_env_eq")
                .map_err(|e| CompileError::LlvmError(format!("b9 env compare error: {}", e)))?;
            matched = self
                .builder
                .build_or(matched, eq, "b9_env_matched")
                .map_err(|e| CompileError::LlvmError(format!("b9 env or error: {}", e)))?;
        }
        for (slot, list_ty) in claimed_string_lists {
            let in_list = self.emit_string_list_contains(ptr, *slot, *list_ty)?;
            matched = self
                .builder
                .build_or(matched, in_list, "b9_string_list_matched")
                .map_err(|e| CompileError::LlvmError(format!("b9 string list or: {e}")))?;
        }
        for (slot, list_ty, elem_list_ty) in claimed_string_list_lists {
            let in_list =
                self.emit_string_list_list_contains(ptr, *slot, *list_ty, *elem_list_ty)?;
            matched = self
                .builder
                .build_or(matched, in_list, "b9_string_list_list_matched")
                .map_err(|e| CompileError::LlvmError(format!("b9 string list list or: {e}")))?;
        }
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("guarded free outside function".into()))?;
        let free_bb = self.context.append_basic_block(parent, "b9_free_env");
        let skip_bb = self.context.append_basic_block(parent, "b9_skip_free");
        self.build_cond_br(matched, skip_bb, free_bb)?;
        self.builder.position_at_end(free_bb);
        self.builder
            .build_call(
                free_fn,
                &[BasicMetadataValueEnum::PointerValue(ptr)],
                "free_heap",
            )
            .map_err(|e| CompileError::LlvmError(format!("free error: {}", e)))?;
        self.builder
            .build_unconditional_branch(skip_bb)
            .map_err(|e| CompileError::LlvmError(format!("b9 free merge error: {}", e)))?;
        self.builder.position_at_end(skip_bb);
        Ok(())
    }

    /// L6: emit the conditional free for a `HeapEntry::EnumBox`. Loads the
    /// `{i32 tag, i64 payload}` struct from `slot` and frees `inttoptr(payload)`
    /// iff `tag` is one of `boxed_ordinals` (the `PayloadKind::Packed` variants
    /// that actually carry a heap box). `Single`/`None` variants store inline
    /// data in the i64 slot and are skipped — freeing inline bits would crash.
    /// When `claimed` is non-empty (the enum value is being returned), the free
    /// is further guarded so a claimed box (ownership transferred to the caller)
    /// is not released here.
    fn emit_enum_box_free(
        &mut self,
        free_fn: inkwell::values::FunctionValue<'ctx>,
        slot: inkwell::values::PointerValue<'ctx>,
        struct_ty: inkwell::types::StructType<'ctx>,
        boxed_ordinals: &[u64],
        claimed: &[inkwell::values::PointerValue<'ctx>],
    ) -> Result<(), CompileError> {
        if boxed_ordinals.is_empty() {
            return Ok(());
        }
        let struct_val = self
            .builder
            .build_load(
                BasicTypeEnum::StructType(struct_ty),
                slot,
                "enum_box_struct",
            )
            .map_err(|e| CompileError::LlvmError(format!("enum box load: {e}")))?
            .into_struct_value();
        let tag = self
            .builder
            .build_extract_value(struct_val, 0, "enum_box_tag")
            .map_err(|e| CompileError::LlvmError(format!("enum box tag: {e}")))?
            .into_int_value();
        let i32_ty = self.context.i32_type();
        let mut is_boxed = self.context.bool_type().const_int(0, false);
        for ord in boxed_ordinals {
            let eq = self
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::EQ,
                    tag,
                    i32_ty.const_int(*ord, false),
                    "enum_box_ord_eq",
                )
                .map_err(|e| CompileError::LlvmError(format!("enum box cmp: {e}")))?;
            is_boxed = self
                .builder
                .build_or(is_boxed, eq, "enum_box_matched")
                .map_err(|e| CompileError::LlvmError(format!("enum box or: {e}")))?;
        }
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("enum box free outside function".into()))?;
        let free_bb = self.context.append_basic_block(parent, "enum_box_free");
        let done_bb = self.context.append_basic_block(parent, "enum_box_done");
        self.build_cond_br(is_boxed, free_bb, done_bb)?;
        self.builder.position_at_end(free_bb);
        // struct_val dominates free_bb (computed before the branch), so the
        // payload extraction here is SSA-valid.
        let payload_i64 = self
            .builder
            .build_extract_value(struct_val, 1, "enum_box_payload")
            .map_err(|e| CompileError::LlvmError(format!("enum box payload: {e}")))?
            .into_int_value();
        let box_ptr = self.build_int_to_ptr(
            payload_i64,
            self.context.ptr_type(inkwell::AddressSpace::default()),
            "enum_box_ptr",
        )?;
        if claimed.is_empty() {
            self.builder
                .build_call(
                    free_fn,
                    &[BasicMetadataValueEnum::PointerValue(box_ptr)],
                    "free_enum_box",
                )
                .map_err(|e| CompileError::LlvmError(format!("enum box free: {e}")))?;
            self.build_br(done_bb)?;
        } else {
            // Guarded: skip if the box is claimed (returned to the caller).
            // emit_guarded_scope_free leaves insertion at its merge block.
            self.emit_guarded_scope_free(free_fn, box_ptr, claimed, &[], &[])?;
            self.build_br(done_bb)?;
        }
        self.builder.position_at_end(done_bb);
        Ok(())
    }

    /// Emit the runtime cleanup for a returned `List<string>` value. The
    /// data array is loaded from an entry-block alloca; every element is a
    /// heap `char*`, so each is passed to `mimi_string_free`, then the array
    /// itself is freed.
    /// Free one in-register `List<string>` value: every string data pointer
    /// in the array, then the array itself. Used by returned `List<string>`
    /// cleanup and by each inner list of a returned `List<List<string>>`.
    fn emit_string_list_struct_free(
        &mut self,
        list_sv: inkwell::values::StructValue<'ctx>,
        tag: &str,
    ) -> Result<(), CompileError> {
        let i64_ty = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let len = self
            .builder
            .build_extract_value(list_sv, 0, &format!("{tag}_len"))
            .map_err(|e| CompileError::LlvmError(format!("{tag}: len {e}")))?
            .into_int_value();
        let data = self
            .builder
            .build_extract_value(list_sv, 1, &format!("{tag}_data"))
            .map_err(|e| CompileError::LlvmError(format!("{tag}: data {e}")))?
            .into_pointer_value();
        let function = self
            .current_function()
            .ok_or_else(|| CompileError::LlvmError(format!("{tag}: outside function")))?;
        let header = self
            .context
            .append_basic_block(function, &format!("{tag}_header"));
        let body = self
            .context
            .append_basic_block(function, &format!("{tag}_body"));
        let exit = self
            .context
            .append_basic_block(function, &format!("{tag}_exit"));
        let idx_storage =
            self.build_alloca(BasicTypeEnum::IntType(i64_ty), &format!("{tag}_idx"))?;
        self.build_store(idx_storage, i64_ty.const_int(0, false))?;
        self.build_br(header)?;

        self.builder.position_at_end(header);
        let idx = self
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                idx_storage,
                &format!("{tag}_idx_val"),
            )?
            .into_int_value();
        let cond = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, idx, len, &format!("{tag}_cond"))
            .map_err(|e| CompileError::LlvmError(format!("{tag}: cmp {e}")))?;
        self.build_cond_br(cond, body, exit)?;

        self.builder.position_at_end(body);
        let elem_slot =
            self.build_in_bounds_gep(i64_ty, data, &[idx], &format!("{tag}_elem_slot"))?;
        let elem_i64 = self
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                elem_slot,
                &format!("{tag}_elem_i64"),
            )?
            .into_int_value();
        let elem_ptr = self.build_int_to_ptr(elem_i64, ptr_ty, &format!("{tag}_elem_ptr"))?;
        // 0.1.8 Phase B fat ABI: list<string> slots contain MimiStr box
        // handles. Free the box (and its owned bytes) with the dedicated
        // runtime helper instead of treating the slot as a raw C string.
        let free_str = self
            .module
            .get_function("mimi_str_free_box")
            .ok_or_else(|| CompileError::LlvmError("mimi_str_free_box not declared".into()))?;
        let box_i64 = self
            .build_ptr_to_int(elem_ptr, i64_ty, &format!("{tag}_elem_box_i64"))
            .map_err(|e| CompileError::LlvmError(format!("{tag}: box i64 {e}")))?;
        self.builder
            .build_call(
                free_str,
                &[BasicMetadataValueEnum::IntValue(box_i64)],
                &format!("{tag}_elem_free"),
            )
            .map_err(|e| CompileError::LlvmError(format!("{tag}: elem free {e}")))?;
        let next = self
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), &format!("{tag}_idx_next"))
            .map_err(|e| CompileError::LlvmError(format!("{tag}: inc {e}")))?;
        self.build_store(idx_storage, next)?;
        self.build_br(header)?;

        self.builder.position_at_end(exit);
        let free_fn = self
            .module
            .get_function("free")
            .ok_or_else(|| CompileError::LlvmError("free not declared".into()))?;
        self.builder
            .build_call(
                free_fn,
                &[BasicMetadataValueEnum::PointerValue(data)],
                &format!("{tag}_free_data"),
            )
            .map_err(|e| CompileError::LlvmError(format!("{tag}: data free {e}")))?;
        Ok(())
    }

    fn emit_string_list_data_free(
        &mut self,
        slot: inkwell::values::PointerValue<'ctx>,
        list_ty: inkwell::types::StructType<'ctx>,
    ) -> Result<(), CompileError> {
        let list_sv = self
            .builder
            .build_load(
                BasicTypeEnum::StructType(list_ty),
                slot,
                "string_list_ret_load",
            )
            .map_err(|e| CompileError::LlvmError(format!("string list ret load: {e}")))?
            .into_struct_value();
        self.emit_string_list_struct_free(list_sv, "string_list_ret")
    }

    /// Free a returned `List<List<string>>`: loop the outer list, free each
    /// inner `List<string>` via `emit_string_list_struct_free`, then free the
    /// outer data array.
    fn emit_string_list_list_data_free(
        &mut self,
        slot: inkwell::values::PointerValue<'ctx>,
        list_ty: inkwell::types::StructType<'ctx>,
        elem_list_ty: inkwell::types::StructType<'ctx>,
    ) -> Result<(), CompileError> {
        let i64_ty = self.context.i64_type();
        let list_sv = self
            .builder
            .build_load(
                BasicTypeEnum::StructType(list_ty),
                slot,
                "string_list_list_ret_load",
            )
            .map_err(|e| CompileError::LlvmError(format!("string list list ret load: {e}")))?
            .into_struct_value();
        let len = self
            .builder
            .build_extract_value(list_sv, 0, "string_list_list_ret_len")
            .map_err(|e| CompileError::LlvmError(format!("string list list ret len: {e}")))?
            .into_int_value();
        let data = self
            .builder
            .build_extract_value(list_sv, 1, "string_list_list_ret_data")
            .map_err(|e| CompileError::LlvmError(format!("string list list ret data: {e}")))?
            .into_pointer_value();
        let function = self.current_function().ok_or_else(|| {
            CompileError::LlvmError("string list list free outside function".into())
        })?;
        let header = self
            .context
            .append_basic_block(function, "string_list_list_ret_header");
        let body = self
            .context
            .append_basic_block(function, "string_list_list_ret_body");
        let exit = self
            .context
            .append_basic_block(function, "string_list_list_ret_exit");
        let idx_storage =
            self.build_alloca(BasicTypeEnum::IntType(i64_ty), "string_list_list_ret_idx")?;
        self.build_store(idx_storage, i64_ty.const_int(0, false))?;
        self.build_br(header)?;

        self.builder.position_at_end(header);
        let idx = self
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                idx_storage,
                "string_list_list_ret_idx_val",
            )?
            .into_int_value();
        let cond = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                idx,
                len,
                "string_list_list_ret_cond",
            )
            .map_err(|e| CompileError::LlvmError(format!("string list list ret cmp: {e}")))?;
        self.build_cond_br(cond, body, exit)?;

        self.builder.position_at_end(body);
        let elem_slot =
            self.build_in_bounds_gep(i64_ty, data, &[idx], "string_list_list_ret_elem_slot")?;
        let inner_handle = self
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                elem_slot,
                "string_list_list_ret_inner_handle",
            )?
            .into_int_value();
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let inner_ptr =
            self.build_int_to_ptr(inner_handle, ptr_ty, "string_list_list_ret_inner_ptr")?;
        let inner_list_sv = self
            .build_load(
                BasicTypeEnum::StructType(elem_list_ty),
                inner_ptr,
                "string_list_list_ret_inner",
            )?
            .into_struct_value();
        self.emit_string_list_struct_free(inner_list_sv, "nested_str_list_elem")?;
        let free_fn = self
            .module
            .get_function("free")
            .ok_or_else(|| CompileError::LlvmError("free not declared".into()))?;
        self.builder
            .build_call(
                free_fn,
                &[BasicMetadataValueEnum::PointerValue(inner_ptr)],
                "string_list_list_ret_free_inner_box",
            )
            .map_err(|e| CompileError::LlvmError(format!("string list list ret box free: {e}")))?;
        let next = self
            .builder
            .build_int_add(
                idx,
                i64_ty.const_int(1, false),
                "string_list_list_ret_idx_next",
            )
            .map_err(|e| CompileError::LlvmError(format!("string list list ret inc: {e}")))?;
        self.build_store(idx_storage, next)?;
        self.build_br(header)?;

        self.builder.position_at_end(exit);
        let free_fn = self
            .module
            .get_function("free")
            .ok_or_else(|| CompileError::LlvmError("free not declared".into()))?;
        self.builder
            .build_call(
                free_fn,
                &[BasicMetadataValueEnum::PointerValue(data)],
                "string_list_list_ret_free_data",
            )
            .map_err(|e| CompileError::LlvmError(format!("string list list ret data free: {e}")))?;
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
        match ty.unlocated() {
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
                let force_heap = match inner.as_ref().unlocated() {
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
                // 0.39.136: unit payload → i64 sentinel slot (resolved-emitter
                // ABI parity); a None here poisoned the whole Option<()>
                // lowering and forced an i64 signature fallback.
                // Priority: real lowering first (records/lists); unit
                // sentinel only as fallback (llvm_type_for(unit) is None).
                let inner_llvm = self.llvm_type_for(inner).or_else(|| {
                    crate::codegen::types::container_payload_llvm(self.context, inner)
                })?;
                // Only widen scalar ints and product-tuple int fields — never
                // named records (all-i32 records must keep i32 field layout).
                let widened = match (inner.unlocated(), inner_llvm) {
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
                    &[BasicTypeEnum::IntType(self.context.bool_type()), widened],
                    false,
                )))
            }
            Type::Result(ok, _) => {
                // 0.39.136: unit payload → i64 sentinel slot (same poisoning
                // as Option<()> above — Result<(), E> previously failed the
                // whole lowering and fell back to an i64 function signature
                // while bodies emitted struct returns).
                // Priority: real lowering first (records/tuples); unit
                // sentinel only as fallback.
                let ok_llvm = self
                    .llvm_type_for(ok)
                    .or_else(|| crate::codegen::types::container_payload_llvm(self.context, ok))?;
                // Widen integer Ok slots and product-tuple i32 fields to i64
                // so they match Ok((1,2)) literal ABI. Do not widen named records
                // or i1 bool fields.
                let widened = match (ok.unlocated(), ok_llvm) {
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
            // Generic instantiation of a user-defined type (e.g. `Box<i32>`,
            // `Pair<i32>`). The LLVM struct layout is registered under the base
            // name without type parameters. Strip args and look up the base.
            Type::Name(name, args) if !args.is_empty() => {
                if let Some(llvm) = self.type_llvm.get(name) {
                    return Some(*llvm);
                }
                crate::codegen::types::mimi_type_to_llvm(self.context, ty)
            }
            _ => crate::codegen::types::mimi_type_to_llvm(self.context, ty),
        }
    }

    /// Register the element LLVM type for a `List<T>` variable so that
    /// compile_index_expr can reconstruct struct-typed elements from type-erased storage.
    pub(super) fn register_list_elem_type(&mut self, var_name: &str, decl_ty: &Type) {
        if let Type::Name(tn, args) = decl_ty.unlocated() {
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

    /// 0.39.136: whether a `map_set`/`map_remove` value's static type is
    /// decodable by the runtime's heuristic Any renderer
    /// (`mimi_map_to_json_any`: heap C string → JSON string, else decimal
    /// integer). Only ints and strings are — everything else (floats store
    /// bit patterns, bools would render 0/1, product tuples and containers
    /// need structural decoding) requires a NARROWED `Map<string, T>`
    /// var_type_names hint so the typed serializers keep handling it.
    /// Narrowing a hint that heterogeneous chains share would make the last
    /// insertion's type misrender every other entry, so scalar int/string
    /// values intentionally fall back to the bare `Map` hint.
    pub(super) fn map_value_decodable_by_any(vt: &str) -> bool {
        vt.is_empty() || matches!(vt, "i32" | "i64" | "int" | "string")
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
    /// v0.34.16 (ADR-002): field names of a Record-typed AST type (declaration
    /// order), used to map named constructor-pattern fields to struct indices.
    pub(super) fn record_fields_of(&self, ty: &Type) -> Option<Vec<String>> {
        let type_name = self.get_full_type_name(ty)?;
        let def = self.type_defs.get(&type_name).or_else(|| {
            self.type_defs.values().find(|td| {
                td.name == type_name || td.name.rsplit("::").next() == Some(type_name.as_str())
            })
        })?;
        match &def.kind {
            crate::ast::TypeDefKind::Record(fields) => {
                Some(fields.iter().map(|f| f.name.clone()).collect())
            }
            _ => None,
        }
    }

    /// Full field definitions (name + type) of a record type, for registering a
    /// match-bound record field's AST type so downstream field access resolves
    /// (v0.34.18b: fixes E0707 on `Fault { trace, .. }` → `trace.subfield`).
    pub(super) fn record_field_defs_of(&self, ty: &Type) -> Option<Vec<crate::ast::Field>> {
        let type_name = self.get_full_type_name(ty)?;
        let def = self.type_defs.get(&type_name).or_else(|| {
            self.type_defs.values().find(|td| {
                td.name == type_name || td.name.rsplit("::").next() == Some(type_name.as_str())
            })
        })?;
        match &def.kind {
            crate::ast::TypeDefKind::Record(fields) => Some(fields.clone()),
            _ => None,
        }
    }

    pub(super) fn get_full_type_name(&self, ty: &Type) -> Option<String> {
        // Depth-capped wrapper: the type_map substitution below must tolerate
        // pathological maps (a generic parameter mapping to a type that still
        // mentions it — observed as a stack overflow while compiling
        // stdlib random shuffle/sample instances).
        fn go(this: &CodeGenerator<'_>, ty: &Type, depth: u32) -> Option<String> {
            if depth > 16 {
                return None;
            }
            match ty.unlocated() {
                Type::Name(tn, args) => {
                    if args.is_empty() {
                        // 0.39.x matrix sweep (RESULT-MAPERR-ABI-001): inside a
                        // monomorphized instance, a GENERIC PARAMETER annotation
                        // (`let result: List<T> = []`) must resolve through the
                        // active type_map — otherwise this registration overwrote
                        // the instantiated parameter registration with the bare
                        // "List<T>", and downstream push/string-ABI decisions saw
                        // an unknown element type (crash class fixed alongside).
                        if let Some(substituted) = this.type_map.get(tn.as_str()) {
                            // Guard against self-referential maps: only follow
                            // the substitution when it actually differs from
                            // the name we are expanding (depth cap is the
                            // backstop for longer cycles).
                            let is_self = match substituted.unlocated() {
                                Type::Name(sn, sa) => sn == tn && sa.is_empty(),
                                _ => false,
                            };
                            if !is_self {
                                return go(this, substituted, depth + 1);
                            }
                        }
                        Some(tn.clone())
                    } else {
                        let inner: Vec<String> =
                            args.iter().filter_map(|a| go(this, a, depth + 1)).collect();
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
                        .filter_map(|a| go(this, a, depth + 1))
                        .collect();
                    if inner.len() == elems.len() {
                        Some(format!("({})", inner.join(", ")))
                    } else {
                        None
                    }
                }
                Type::Option(inner) => go(this, inner, depth + 1).map(|s| format!("Option<{}>", s)),
                Type::Result(ok, err) => {
                    let o = go(this, ok, depth + 1)?;
                    let e = go(this, err, depth + 1)?;
                    Some(format!("Result<{},{}>", o, e))
                }
                _ => None,
            }
        }
        go(self, ty, 0)
    }

    /// Register the full Result<T, (Source, E)> return type of a flow

    /// Register the full Result<T, (Source, E)> return type of a flow
    /// transition call in `var_types`, enabling pattern matching code
    /// (e.g., `Err((src, e))`) to recover the correct struct types for
    /// the source state and error payload.
    pub(super) fn track_flow_result_type(
        &mut self,
        var_name: &str,
        from_state: &str,
        to_state: &str,
        fails: Option<crate::ast::Type>,
    ) {
        use crate::ast::Type;
        let to_ty = Type::Name(to_state.to_string(), vec![]);
        let from_ty = Type::Name(from_state.to_string(), vec![]);
        let err_ty = match fails {
            Some(f) => Type::Tuple(vec![from_ty, f]),
            None => Type::Tuple(vec![from_ty, Type::Name("string".to_string(), vec![])]),
        };
        self.var_types.insert(
            var_name.to_string(),
            Type::Result(Box::new(to_ty), Box::new(err_ty)),
        );
    }

    /// Resolve generic type parameters (e.g., `T` → `User`) using the active
    /// `type_map` from the calling context (populated by monomorphization).
    pub(super) fn substitute_type_params(&self, ty: &crate::ast::Type) -> crate::ast::Type {
        use crate::ast::Type;
        match ty.unlocated() {
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
                // 0.36.4 Fault nominal: the flow-scoped StateId/EventId enums
                // are never resolved globally — their variants resolve scoped
                // (construction via build_nominal_variant, match via the direct
                // enum name in owner_enum_of_scrutinee). Skipping them keeps the
                // global lookup unambiguous for the __MultiTarget union whose
                // variant names (state names / Fault) they otherwise shadow.
                if td.name.ends_with("::StateId") || td.name.ends_with("::EventId") {
                    continue;
                }
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

    /// 0.36.4 Fault nominal: find the flow-scoped StateId/EventId enum whose
    /// variants include `name`. Unlike `find_variant_info` (which skips these
    /// enums for __MultiTarget disambiguation), this looks them up explicitly.
    ///
    /// 0.36.7 (跨 flow 补全, latent L1): the injected system verbs — peer_fault,
    /// recover, reset, Panic, ffi_crash — are variants of EVERY flow's
    /// StateId/EventId enum, so an unscoped first-match across type_defs is
    /// nondeterministic across processes (HashMap RandomState) and mis-tags the
    /// ordinal (enum Display then prints another flow's variant at that slot →
    /// wrong state/event name in native output, flapping run-to-run). Scope to
    /// the CURRENT flow's enums first (the transition body being compiled
    /// belongs to it — same scoped-first discipline as `flow_state_llvm_type`,
    /// 0.34.36 audit §6.9); fall back to the unscoped scan only for contexts
    /// without an active flow (a scope miss can never trap — the fallback
    /// keeps all previously-valid resolutions working).
    pub(in crate::codegen) fn nominal_variant_enum(&self, name: &str) -> Option<String> {
        if !self.current_flow_name.is_empty() {
            for suffix in ["StateId", "EventId"] {
                let qualified = format!("flow::{}::{}", self.current_flow_name, suffix);
                if let Some(td) = self.type_defs.get(&qualified) {
                    if let crate::ast::TypeDefKind::Enum(variants) = &td.kind {
                        if variants.iter().any(|v| v.name == name) {
                            return Some(qualified);
                        }
                    }
                }
            }
        }
        for td in self.type_defs.values() {
            if !(td.name.ends_with("::StateId") || td.name.ends_with("::EventId")) {
                continue;
            }
            if let crate::ast::TypeDefKind::Enum(variants) = &td.kind {
                if variants.iter().any(|v| v.name == name) {
                    return Some(td.name.clone());
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

    /// v0.34.18a: Resolve a variant name to its ordinal *scoped to the
    /// scrutinee's enum type* when known, falling back to the global lookup.
    ///
    /// The global `find_variant_ordinal` searches every registered enum and
    /// returns the first hit, which is ambiguous when a variant name appears in
    /// multiple enums. This bites the synthetic per-flow `__MultiTarget` unions:
    /// every flow that can fault has a `Fault` variant, so a program with two
    /// fallible flows (`Calc::__MultiTarget` and `FCalc::__MultiTarget`) makes a
    /// bare `find_variant_ordinal("Fault")` resolve to whichever enum the
    /// `type_defs` HashMap yields first — nondeterministically mis-tagging the
    /// match arms. Scoping to the scrutinee's enum makes the dispatch agree with
    /// the tag the transition return / panic-absorption path produces.
    pub(super) fn find_variant_ordinal_scoped(
        &self,
        name: &str,
        scrutinee_type: Option<&crate::ast::Type>,
    ) -> Result<u64, CompileError> {
        if let Some(owner) = scrutinee_type.and_then(|ty| self.owner_enum_of_scrutinee(ty)) {
            if let Some(td) = self.type_defs.get(&owner) {
                if let crate::ast::TypeDefKind::Enum(variants) = &td.kind {
                    let mut sorted: Vec<&crate::ast::Variant> = variants.iter().collect();
                    sorted.sort_by_key(|v| &v.name);
                    if let Some(i) = sorted.iter().position(|v| v.name == name) {
                        return Ok(i as u64);
                    }
                }
            }
        }
        self.find_variant_ordinal(name)
    }

    /// 0.36.4 Fault nominal: like `find_variant_ordinal_scoped` but returns the
    /// (owner enum name, ordinal) pair. Used by match-arm field binding so the
    /// shared `Fault` variant resolves to THIS flow's `__MultiTarget` enum —
    /// otherwise the Fault record payload (and its flow-specific StateId/EventId
    /// field types) resolves to a different flow's Fault, corrupting the enum
    /// Display for multi-flow programs.
    pub(super) fn find_variant_owner_scoped(
        &self,
        name: &str,
        scrutinee_type: Option<&crate::ast::Type>,
    ) -> Option<(String, u64)> {
        if let Some(owner) = scrutinee_type.and_then(|ty| self.owner_enum_of_scrutinee(ty)) {
            if let Some(td) = self.type_defs.get(&owner) {
                if let crate::ast::TypeDefKind::Enum(variants) = &td.kind {
                    let mut sorted: Vec<&crate::ast::Variant> = variants.iter().collect();
                    sorted.sort_by_key(|v| &v.name);
                    if let Some(i) = sorted.iter().position(|v| v.name == name) {
                        return Some((owner, i as u64));
                    }
                }
            }
        }
        self.find_variant_owner(name)
    }

    /// v0.34.18a: Determine the owning enum type name for a match scrutinee's
    /// type, so variant-ordinal resolution can be scoped to it.
    ///
    /// A multi-target transition result is typed (at the AST level) as a
    /// `Result(Ok_state, ...)` rather than the synthetic `__MultiTarget` enum
    /// name, so we cannot key off the type name directly. Instead we extract an
    /// *anchor* variant name from the scrutinee type (the `Ok` state name, e.g.
    /// `S`/`F`) and look up which registered enum owns it. State names are
    /// flow-specific, so the anchor uniquely identifies the flow's
    /// `__MultiTarget` enum — disambiguating the shared `Fault` variant that
    /// appears in every fallible flow's union.
    pub(super) fn owner_enum_of_scrutinee(&self, ty: &crate::ast::Type) -> Option<String> {
        let anchor = self.extract_anchor_variant(ty)?;
        // The anchor may itself be an enum type name (ordinary enum scrutinee).
        if let Some(td) = self.type_defs.get(&anchor) {
            if matches!(td.kind, crate::ast::TypeDefKind::Enum(_)) {
                return Some(self.resolve_enum_alias(&anchor));
            }
        }
        // Otherwise the anchor is a variant name; resolve its owning enum.
        self.find_variant_info(&anchor).map(|(owner, _)| owner)
    }

    /// Extract a variant/type name usable as an enum anchor from a scrutinee
    /// type: unwrap Result/Option to the payload `Name`.
    fn extract_anchor_variant(&self, ty: &crate::ast::Type) -> Option<String> {
        match ty.unlocated() {
            crate::ast::Type::Name(n, _) => Some(n.clone()),
            crate::ast::Type::Result(ok, _) => self.extract_anchor_variant(ok),
            crate::ast::Type::Option(inner) => self.extract_anchor_variant(inner),
            crate::ast::Type::Located { ty: inner, .. } => self.extract_anchor_variant(inner),
            _ => None,
        }
    }

    /// Follow `Alias` type_defs (e.g. `__MultiTarget_Calc` →
    /// `flow::Calc::__MultiTarget`) to the underlying enum type name. Returns the
    /// input unchanged if it is not an alias.
    fn resolve_enum_alias(&self, name: &str) -> String {
        let mut current = name.to_string();
        // Bound the chain so a cyclic alias cannot loop forever.
        for _ in 0..8 {
            let Some(td) = self.type_defs.get(&current) else {
                break;
            };
            if let crate::ast::TypeDefKind::Alias(crate::ast::Type::Name(target, _)) = &td.kind {
                if *target == current {
                    break;
                }
                current = target.clone();
            } else {
                break;
            }
        }
        current
    }

    /// G2: Find the owning type name and ordinal of an enum variant name.
    /// Returns `None` if `name` is not a variant in any registered enum type.
    pub(super) fn find_variant_owner(&self, name: &str) -> Option<(String, u64)> {
        self.find_variant_info(name)
    }

    /// Convert a Mimi AST Type to the type-name string used in
    /// `var_type_names` and `infer_object_type` lookups.
    fn mimi_type_to_type_name(ty: &crate::ast::Type) -> Option<String> {
        match ty.unlocated() {
            crate::ast::Type::Name(n, _) => Some(n.clone()),
            crate::ast::Type::Infer => Some("i64".to_string()),
            _ => None,
        }
    }

    /// Reverse-lookup: given an LLVM type, find the best matching registered
    /// Mimi type name. This is used to recover the Mimi type of a payload
    /// extracted from a built-in Result/Option constructor pattern (where the
    /// AST type info is not available in type_defs).
    ///
    /// Prefers unqualified names (shorter, no `::`) over qualified ones
    /// (e.g. `Paid` over `flow::Order::Paid`) so that field access and
    /// transition dispatch resolve correctly.
    pub(super) fn find_type_name_by_llvm_type(
        &self,
        llvm_ty: BasicTypeEnum<'ctx>,
    ) -> Option<String> {
        let mut best: Option<&str> = None;
        for (name, registered_ty) in &self.type_llvm {
            if llvm_ty == *registered_ty {
                match best {
                    None => best = Some(name),
                    Some(current) => {
                        // Prefer shorter (unqualified) names.
                        if name.len() < current.len() {
                            best = Some(name);
                        }
                    }
                }
            }
        }
        best.map(|s| s.to_string())
    }

    /// Compute the size in bytes of an LLVM type using a portable layout.
    /// This does not rely on the module data layout being set.
    pub(in crate::codegen) fn llvm_type_size_bytes(&self, ty: BasicTypeEnum<'ctx>) -> u64 {
        match ty {
            BasicTypeEnum::IntType(t) => {
                let bits = t.get_bit_width();
                bits.div_ceil(8) as u64
            }
            // 0.34.36 (audit §6.14): size by actual width — f32 is 4 bytes, not 8.
            // The old constant-8 undersized/oversized blob and box computations
            // for f32 payloads (actor mailbox result pack, async future slots).
            BasicTypeEnum::FloatType(t) => t.get_bit_width().div_ceil(8) as u64,
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
            // 0.34.36 (audit §6.14): f32 aligns to 4, not 8 — mirrors the
            // width-based size computation above.
            BasicTypeEnum::FloatType(t) => {
                let bytes = t.get_bit_width().div_ceil(8) as u64;
                bytes.next_power_of_two()
            }
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
        } else if let Expr::Record { ty: Some(tn), .. } = init.unlocated() {
            self.var_type_names.insert(name.clone(), tn.clone());
        } else if let Expr::Call(callee, _) = init.unlocated() {
            if let Expr::Ident(fname) = callee.unlocated() {
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
            crate::ast::SharedKind::Shared => {
                // Shared reference copy: `shared q = p` where p is already shared.
                // Share the same heap allocation and retain, rather than copying the value.
                if let Expr::Ident(src_name) = init.unlocated() {
                    if self.shared_var_names.contains(src_name.as_str()) {
                        return self.compile_shared_ref_copy(name, src_name, vars);
                    }
                }
            }
            crate::ast::SharedKind::Weak => {
                // Weak reference: init must be an existing shared variable.
                if let Expr::Ident(src_name) = init.unlocated() {
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
                if let Expr::Record { ty: Some(tn), .. } = init.unlocated() {
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
        let abort_fn = self.get_or_declare_abort_fn();
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

    /// Create a TargetMachine for the current configuration.
    ///
    /// Extracted from `compile_to_object` so callers (e.g. the test
    /// harness) can cache the TargetMachine across many compilations
    /// and avoid repeated CPU-feature detection + MC-layer setup.
    pub fn create_target_machine(&self) -> Result<TargetMachine, CompileError> {
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
        target
            .create_target_machine(
                &triple_ref,
                &cpu,
                &features,
                OptimizationLevel::None,
                reloc_mode,
                CodeModel::Default,
            )
            .ok_or_else(|| {
                CompileError::LlvmError(format!(
                    "failed to create target machine for triple '{}'",
                    triple_ref
                ))
            })
    }

    pub fn compile_to_object(&self, output_path: &Path) -> Result<(), CompileError> {
        let tm = self.create_target_machine()?;
        self.emit_object(&tm, output_path)
    }

    /// 0.39.x matrix sweep: opt out of the `u_` symbol-namespacing pass for
    /// shared-library outputs whose exported function names are part of the
    /// host contract (`dlsym("mul_sse16")` must keep resolving). Executable
    /// links (the default path) keep the pass enabled.
    pub fn compile_to_object_shared(&self, output_path: &Path) -> Result<(), CompileError> {
        let tm = self.create_target_machine()?;
        self.emit_object_with_namespacing(&tm, output_path, false)
    }

    /// Emit an object file using a pre-created TargetMachine.
    ///
    /// Allows callers to amortise TargetMachine construction across
    /// many compilations (the test harness creates one per thread).
    pub fn emit_object(&self, tm: &TargetMachine, output_path: &Path) -> Result<(), CompileError> {
        self.emit_object_with_namespacing(tm, output_path, true)
    }

    fn emit_object_with_namespacing(
        &self,
        tm: &TargetMachine,
        output_path: &Path,
        namespace_symbols: bool,
    ) -> Result<(), CompileError> {
        // Run LLVM optimization passes before codegen. 0.34.34: O1 is the
        // DEFAULT (opt-out via MIMI_OPT=0/false). 0.31.21 fixed the O1 bugs
        // (try_expr i32-vs-i1 type mismatch; extern wrapper name collision
        // strlen → strlen.11). Confidence baseline: 0.34.34 full-suite +
        // differential fuzz re-run with O1 default.
        // MIMI_DUMP_MODULE=<path>: dump the module IR right before the
        // optimization pipeline (diagnostics; mirrors the test-side
        // MIMI_DUMP_IR hook but for the CLI build path). 0.35.11: hoisted
        // above the optimize gate so O0 (MIMI_OPT=0) builds can be inspected
        // too — the O1-only placement left the default debug opt-out builds
        // invisible.
        if let Ok(path) = std::env::var("MIMI_DUMP_MODULE") {
            let _ = self.module.print_to_file(&path);
        }
        // 0.39.x matrix sweep (SYMBOL-NAMESPACE-001): user functions compile
        // to global C-ABI symbols with their bare source names. A stdlib
        // `pub func write`/`read`/`stat` then EXPORTED those names in the
        // dynamic symbol table, shadowing libc's write/read for every static
        // std call site — Rust's stdout path resolved into mimi's
        // fs::write(filename=fd) and recursed until the stack died. Renaming
        // DEFINED functions at object-emission time is purely a linker-level
        // change: all in-module references were already resolved by name
        // during codegen, so nothing else needs updating. main keeps its
        // entrypoint name; runtime (mimi_*) and libc externs have no body and
        // are skipped by the has-body check.
        if namespace_symbols {
            const RESERVED_PREFIXES: [&str; 2] = ["mimi_", "u_"];
            for func in self.module.get_functions() {
                let name = func.get_name().to_string_lossy().into_owned();
                if name == "main"
                    || name.starts_with(RESERVED_PREFIXES[0])
                    || name.starts_with(RESERVED_PREFIXES[1])
                    || func.count_basic_blocks() == 0
                {
                    continue;
                }
                unsafe {
                    // inkwell 0.9 has no FunctionValue::set_name; go through
                    // the LLVM core API directly (same value-ref rename).
                    use inkwell::llvm_sys::core::LLVMSetValueName2;
                    use inkwell::values::AsValueRef;
                    let new_name = format!("u_{}", name);
                    LLVMSetValueName2(
                        func.as_value_ref(),
                        new_name.as_ptr() as *const std::os::raw::c_char,
                        new_name.len(),
                    );
                }
            }
        }
        if self.optimize {
            // 0.35.3 L1 (SD-9 chain convergence): fold per-op finiteness
            // checks to chain-end points before the optimizer so the check
            // branches no longer block vectorization. O0 keeps per-point
            // checks (behavior unchanged).
            crate::codegen::float_chain::converge_float_finiteness(&self.module);
            // MIMI_DUMP_MODULE_CONVERGED=<path>: dump IR right after the
            // chain-convergence pass (diagnostics for 0.35.3).
            if let Ok(path) = std::env::var("MIMI_DUMP_MODULE_CONVERGED") {
                let _ = self.module.print_to_file(&path);
            }
            let options = inkwell::passes::PassBuilderOptions::create();
            // 0.35.4: branch_weights cold metadata 由 emitter/收敛 pass 直接
            // 附加（见 float_chain::mark_cold_trap_branch），优化管线保持
            // default<O1>（CVP 实测无收益，不引入风险面）。
            self.module
                .run_passes("default<O1>", tm, options)
                .map_err(|e| CompileError::LlvmError(format!("optimization failed: {}", e)))?;
        }

        if std::env::var("MIMI_DUMP_MODULE_OPT").is_ok() {
            if let Ok(path) = std::env::var("MIMI_DUMP_MODULE_OPT") {
                let _ = self.module.print_to_file(&path);
            }
        }
        tm.write_to_file(
            &self.module,
            inkwell::targets::FileType::Object,
            output_path,
        )
        .map_err(|e| CompileError::Io(format!("failed to write object file: {}", e)))
    }
}
