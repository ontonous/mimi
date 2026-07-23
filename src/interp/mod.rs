#![allow(dead_code)]

mod actor;
mod builtins;
mod call;
mod closure_utils;
pub mod error;
mod eval;
mod ffi;
mod ffi_call;
mod ops;
mod pattern;
pub(crate) mod pool;
mod quote;
mod resolved;
mod scope_env;
mod value;

pub use error::InterpError;
pub use scope_env::ScopeEnv;
pub use value::*;

/// Alias for interpreter results.
pub type InterpResult<T> = std::result::Result<T, InterpError>;

use crate::ast::*;
use crate::ffi::FfiContract;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use closure_utils::collect_free_vars;

/// Internal loop control flow signal
#[derive(Debug, Clone)]
pub(crate) enum LoopAction {
    Continue,
    Break(Option<Value>),
}

/// v0.29.14: Persistent-payload transaction state for one flow.
///
/// At transition entry we snapshot persistent field values from `self`.
/// - **Version/dirty strategy (default):** if any non-`@transactional` persistent
///   field differs from the snapshot at Fault time, `recover` degrades to `reset`.
/// - **WAL strategy (`@transactional`):** snapshot is a full shadow copy; on Fault
///   those fields are restored from the snapshot before recover.
/// - **Metadata shadow strategy (`@metadata_shadow`, v0.29.45):** only metadata
///   (length for Lists, field count for Records) is snapshotted. On restore,
///   metadata is reset but underlying data buffer is kept. C4: the dirty check
///   also stores the full value in `snapshot` for content-aware comparison,
///   even though the WAL restore is length-only.
#[derive(Debug, Clone, Default)]
pub struct FlowPersistentTx {
    /// Snapshotted values keyed by field name (turn entry).
    /// C4: for `@metadata_shadow` fields this stores the full value content
    /// (for dirty checking), while `metadata_snapshot` tracks the length
    /// (for O(1) WAL restore).
    pub snapshot: HashMap<String, Value>,
    /// v0.29.45: Metadata-only snapshots for `@metadata_shadow` fields.
    /// Stores the length (for Lists) as usize for O(1) snapshot/restore.
    pub metadata_snapshot: HashMap<String, usize>,
    /// True after a successful transition commit (snapshot cleared).
    pub committed: bool,
}

pub struct Interpreter<'a> {
    file: &'a File,
    /// Scope-level evaluation state (variable bindings, mutability, call stack).
    /// `pub(in crate::interp)` so delegate writeback can bypass the mutability
    /// check for flow state `self` (implicitly mutable in do blocks).
    pub(in crate::interp) scope_env: ScopeEnv,
    constructors: HashMap<String, usize>,
    /// Set of constructor names that are newtypes (for wrapping result in Value::Newtype)
    newtype_constructors: HashMap<String, bool>,
    /// Maps type name to its variants (for Result/Option-like types)
    type_variants: HashMap<String, Vec<String>>,
    /// Maps variant name to its parent ADT type name
    variant_parent: HashMap<String, String>,
    /// Maps variant name to field-name → position index (for named constructor patterns)
    variant_field_positions: HashMap<String, HashMap<String, usize>>,
    /// Variants that represent "failure" (Err, None, *Error, *Fail)
    failure_variants: HashMap<String, bool>,
    /// Capability definitions: cap_name -> list of component caps
    cap_defs: HashMap<String, Vec<String>>,
    /// Compensation stack for on failure blocks (LIFO) - scope-aware
    /// Each scope level contains compensation blocks registered in that scope
    /// Push a new scope when entering a block, pop when exiting
    compensation_stack: Vec<Vec<Vec<Stmt>>>,
    /// M12: count of compensation blocks that failed (not silently lost).
    pub(crate) compensation_error_count: usize,
    /// Arena memory blocks (arena_id -> Arena)
    arenas: Vec<Arena>,
    /// Current arena scope depth (track nesting for error messages)
    arena_depth: usize,
    /// Whether to verify contracts at runtime
    pub verify_contracts: bool,
    /// Whether to verify FFI contracts (requires/ensures) at runtime
    pub verify_ffi: bool,
    /// Trait definitions: trait_name -> TraitDef
    trait_defs: HashMap<String, TraitDef>,
    /// Trait implementations: type_name -> trait_name -> list of FuncDef methods
    type_impls: HashMap<String, HashMap<String, Vec<FuncDef>>>,
    /// Defensive validation for ASTs constructed without the source parser.
    /// Normal source input is rejected during parsing; this prevents the
    /// interpreter's derive expansion from becoming a second fail-open path.
    derive_expansion_error: Option<InterpError>,
    /// Extern function declarations: func_name -> ExternFunc
    extern_funcs: HashMap<String, ExternFunc>,
    /// Pre-computed FFI contracts for extern functions.
    ffi_contracts: HashMap<String, FfiContract>,
    /// Type definitions for reflection: type_name -> (fields, variants)
    type_defs: HashMap<String, TypeDef>,
    /// Pre-computed results for comptime functions (no-arg functions evaluated at startup)
    comptime_results: HashMap<String, Value>,
    /// Loaded shared libraries: (lib_path, Library handle)
    loaded_libs: Vec<(String, libloading::Library)>,
    /// Default allocator kind (set by --allocator CLI flag)
    pub default_allocator: AllocatorKind,
    /// Current loop control flow action (break/continue signal)
    loop_action: Option<LoopAction>,
    /// Early return signal for ? propagation (exception-like, preserves value)
    early_return: Option<Value>,
    /// Values of `mutate` parameters captured when the most recent user
    /// function returned. `eval_call_dispatch` writes them back to caller
    /// argument bindings, matching the codegen reference ABI.
    last_mutate_writebacks: Vec<(usize, Value)>,
    /// Exit code signal from builtin `exit()`; once set, execution stops.
    exited: Option<i32>,
    /// Recursion depth guard to prevent stack overflow
    recursion_depth: usize,
    /// O(1) function lookup index: name -> FuncDef
    func_index: HashMap<String, FuncDef>,
    /// O(1) actor lookup index: name -> ActorDef
    actor_index: HashMap<String, ActorDef>,
    /// Flow definitions: flow_name -> FlowDef
    flow_index: HashMap<String, FlowDef>,
    /// Canonical transitions from CheckedProgram: (flow, event, source) -> targets.
    /// When present, transition dispatch prefers this table over re-scanning FlowDef.
    pub(in crate::interp) resolved_transitions:
        Option<HashMap<(String, String, String), Vec<String>>>,
    /// Fallback/matrix-injected transitions from CheckedProgram.
    pub(in crate::interp) resolved_fallback_transitions:
        Option<std::collections::HashSet<(String, String, String)>>,
    /// FFI-pinned system transitions from CheckedProgram.
    pub(in crate::interp) resolved_ffi_pinned_transitions:
        Option<std::collections::HashSet<(String, String, String)>>,
    /// Transition event parameter arity from CheckedProgram.
    pub(in crate::interp) resolved_transition_param_arity:
        Option<HashMap<(String, String, String), usize>>,
    pub(in crate::interp) resolved_transition_params:
        Option<HashMap<(String, String, String), Vec<(String, String)>>>,
    /// Transitions grouped by flow: flow -> [(event, source, targets, fallback, pinned, argc)].
    pub(in crate::interp) resolved_transitions_by_flow:
        Option<HashMap<String, Vec<(String, String, String, bool, bool, usize)>>>,
    pub(in crate::interp) resolved_transitions_by_event:
        Option<HashMap<String, Vec<(String, String, String, bool, bool, usize)>>>,
    pub(in crate::interp) resolved_node_meta_spans:
        Option<HashMap<String, (usize, usize, usize, usize)>>,
    /// Function signatures from CheckedProgram: qualified_name -> (param_count, ret_fmt, effects).
    pub(in crate::interp) resolved_functions: Option<HashMap<String, (usize, String, Vec<String>)>>,
    /// Function parameter directories: name -> [(param_name, type display)].
    pub(in crate::interp) resolved_function_params: Option<HashMap<String, Vec<(String, String)>>>,
    pub(in crate::interp) resolved_comptime_functions: Option<std::collections::HashSet<String>>,
    /// Session type names materialised from CheckedProgram.
    pub(in crate::interp) resolved_sessions: Option<HashMap<String, crate::ast::SessionType>>,
    /// Session residual type display strings.
    pub(in crate::interp) resolved_session_displays: Option<HashMap<String, String>>,
    /// Protocol names materialised from CheckedProgram.
    pub(in crate::interp) resolved_protocols: Option<std::collections::HashSet<String>>,
    /// Protocol transition records: protocol -> [(event, from, to)].
    pub(in crate::interp) resolved_protocol_transitions:
        Option<HashMap<String, Vec<(String, String, String)>>>,
    /// Protocol state payloads: "Protocol.State" -> payload type display.
    pub(in crate::interp) resolved_protocol_payloads: Option<HashMap<String, String>>,
    /// Protocol state name lists: "Protocol" -> [state_name].
    pub(in crate::interp) resolved_protocol_states: Option<HashMap<String, Vec<String>>>,
    /// Protocol state payload records: "Protocol.State" -> (payload_name, payload_type).
    pub(in crate::interp) resolved_protocol_state_payloads:
        Option<HashMap<String, (String, String)>>,
    /// Actor method directories materialised from CheckedProgram: actor -> methods.
    pub(in crate::interp) resolved_actors: Option<HashMap<String, Vec<String>>>,
    /// Actor method signatures: "Actor.method" -> (arity, ret).
    pub(in crate::interp) resolved_actor_method_signatures:
        Option<HashMap<String, (usize, String)>>,
    pub(in crate::interp) resolved_actor_method_params:
        Option<HashMap<String, Vec<(String, String)>>>,
    pub(in crate::interp) resolved_actor_method_effects: Option<HashMap<String, Vec<String>>>,
    pub(in crate::interp) resolved_actor_fields:
        Option<HashMap<String, Vec<(String, String, bool)>>>,
    /// Capability names materialised from CheckedProgram.
    pub(in crate::interp) resolved_capabilities: Option<std::collections::HashSet<String>>,
    /// Capability combinations: name -> combined_with (if any).
    pub(in crate::interp) resolved_capability_combined: Option<HashMap<String, String>>,
    /// Constant names materialised from CheckedProgram.
    pub(in crate::interp) resolved_constants: Option<std::collections::HashSet<String>>,
    /// Constant directory: name -> (type display, encoded value).
    pub(in crate::interp) resolved_constant_values:
        Option<HashMap<String, (Option<String>, String)>>,
    /// Trait method directories materialised from CheckedProgram.
    pub(in crate::interp) resolved_traits: Option<HashMap<String, Vec<String>>>,
    /// Trait/impl method signatures: "Show.show" / "Show:for:Number.show" -> (arity, ret).
    pub(in crate::interp) resolved_method_signatures: Option<HashMap<String, (usize, String)>>,
    /// Trait/impl method parameter directories: "TraitName.Method" -> [(param_name, type display)].
    pub(in crate::interp) resolved_method_params: Option<HashMap<String, Vec<(String, String)>>>,
    /// Trait/impl method effect directories: "TraitName.Method" -> [effect].
    pub(in crate::interp) resolved_method_effects: Option<HashMap<String, Vec<String>>>,
    /// Impl directories materialised from CheckedProgram: "Trait:for:Type" -> methods.
    pub(in crate::interp) resolved_impls: Option<HashMap<String, Vec<String>>>,
    /// Ownership ledger owners materialised from CheckedProgram.
    pub(in crate::interp) resolved_ownership_owners: Option<std::collections::HashSet<String>>,
    /// Ownership action summaries: owner -> (intro, move, drop, return, merges, maybe_consumed).
    pub(in crate::interp) resolved_ownership_summaries:
        Option<HashMap<String, (usize, usize, usize, usize, usize, bool)>>,
    /// Ownership resources per owner: owner -> resource names.
    pub(in crate::interp) resolved_ownership_resources: Option<HashMap<String, Vec<String>>>,
    /// Ownership actions: owner -> [(kind, resource)].
    pub(in crate::interp) resolved_ownership_actions:
        Option<HashMap<String, Vec<(String, String)>>>,
    /// Ownership branch merges: owner -> [(resource, then, else, merged)].
    pub(in crate::interp) resolved_ownership_merges:
        Option<HashMap<String, Vec<(String, String, String, String)>>>,
    /// Backend capability requirements: (capability, flow).
    pub(in crate::interp) resolved_backend_requirements: Option<Vec<(String, String)>>,
    /// NodeMeta path presence count from CheckedProgram.
    pub(in crate::interp) resolved_node_meta_count: Option<usize>,
    /// NodeMeta path ids from CheckedProgram.
    pub(in crate::interp) resolved_node_meta_paths: Option<std::collections::HashSet<String>>,
    /// NodeMeta precision: path -> "exact"|"declaration_fallback".
    pub(in crate::interp) resolved_node_meta_precision: Option<HashMap<String, String>>,
    /// Type definition kinds materialised from CheckedProgram.
    pub(in crate::interp) resolved_type_kinds: Option<HashMap<String, String>>,
    pub(in crate::interp) resolved_type_fields: Option<HashMap<String, Vec<(String, String)>>>,
    pub(in crate::interp) resolved_type_variants:
        Option<HashMap<String, Vec<(String, Option<String>)>>>,
    pub(in crate::interp) resolved_type_aliases: Option<HashMap<String, String>>,
    /// Extern function names materialised from CheckedProgram.
    pub(in crate::interp) resolved_extern_funcs: Option<std::collections::HashSet<String>>,
    /// Extern function -> ABI string from CheckedProgram.
    pub(in crate::interp) resolved_extern_abis: Option<HashMap<String, String>>,
    /// Extern function signatures: name -> (arity, ret).
    pub(in crate::interp) resolved_extern_signatures: Option<HashMap<String, (usize, String)>>,
    pub(in crate::interp) resolved_extern_params: Option<HashMap<String, Vec<(String, String)>>>,
    pub(in crate::interp) resolved_extern_no_panic: Option<std::collections::HashSet<String>>,
    pub(in crate::interp) resolved_extern_unsafe: Option<std::collections::HashSet<String>>,
    /// Typed call sites from CheckedProgram: node_id -> (owner, callee, argc, kind).
    pub(in crate::interp) resolved_call_sites: Option<
        HashMap<
            String,
            (
                String,
                String,
                usize,
                Option<usize>,
                Vec<String>,
                Option<String>,
                String,
            ),
        >,
    >,
    /// Call sites grouped by owner: owner -> [(callee, argc, kind)].
    pub(in crate::interp) resolved_call_sites_by_owner:
        Option<HashMap<String, Vec<(String, usize, String)>>>,
    /// Call sites grouped by callee: callee -> [(owner, argc, kind)].
    pub(in crate::interp) resolved_call_sites_by_callee:
        Option<HashMap<String, Vec<(String, usize, String)>>>,
    /// Flow mailbox depth limits materialised from CheckedProgram: flow -> depth.
    pub(in crate::interp) resolved_mailbox_depths: Option<HashMap<String, usize>>,
    /// Flow state payloads: "Flow.State" -> [(field, type display)].
    pub(in crate::interp) resolved_flow_state_payloads:
        Option<HashMap<String, Vec<(String, String)>>>,
    /// Flow state names: flow -> [state].
    pub(in crate::interp) resolved_flow_states: Option<HashMap<String, Vec<String>>>,
    /// Flow event names: flow -> [event].
    pub(in crate::interp) resolved_flow_events: Option<HashMap<String, Vec<String>>>,
    /// Resolved item kinds: qualified_name -> kind.
    pub(in crate::interp) resolved_item_kinds: Option<HashMap<String, String>>,
    /// Persistent field sets materialised from CheckedProgram: flow -> fields.
    pub(in crate::interp) resolved_persistent_fields: Option<HashMap<String, Vec<String>>>,
    /// Transactional field sets materialised from CheckedProgram: flow -> fields.
    pub(in crate::interp) resolved_transactional_fields: Option<HashMap<String, Vec<String>>>,
    /// Metadata-shadow field sets materialised from CheckedProgram: flow -> fields.
    pub(in crate::interp) resolved_metadata_shadow_fields: Option<HashMap<String, Vec<String>>>,
    /// Flow impl Protocol names materialised from CheckedProgram.
    pub(in crate::interp) resolved_flow_protocols: Option<HashMap<String, Vec<String>>>,
    /// v0.29.24: process-wide max children (None = unlimited).
    /// Taken from first `@max_children(N)` flow annotation in the file.
    max_children: Option<usize>,
    /// v0.29.24: number of actors spawned by this interpreter process.
    spawn_count: usize,
    /// v0.29.31: per-actor-type spawn count for per-type max_children quota.
    actor_spawn_counts: std::collections::HashMap<String, usize>,
    /// v0.29.14: per-flow persistent-payload transaction state.
    /// Keyed by flow name. Snapshotted at turn entry; committed on success /
    /// used for dirty detection + WAL restore on Fault.
    pub(in crate::interp) flow_tx: HashMap<String, FlowPersistentTx>,
    /// v0.29.43: current flow state name for delayed Fault context.
    /// Set when entering `eval_flow_transition`; used by `pinned` blocks
    /// to route timeout/crash through `make_fault_value` with correct
    /// `last_state` information.
    pub(in crate::interp) current_flow_state: Option<String>,
    /// Global constants defined at top level
    globals: HashMap<String, Value>,
    /// CLI arguments forwarded to the program
    pub cli_args: Vec<String>,
    /// Registry to keep CStrings alive for FFI calls, preventing leaks.
    /// IN-C2: CString registry — stores CStrings created by str_to_c_str.
    /// Dropped when the Interpreter is dropped, freeing all CStrings.
    /// Uses ownership (not into_raw/from_raw) to guarantee no leaks.
    cstring_registry: std::cell::RefCell<Vec<std::ffi::CString>>,
    /// TC-C1: optional stdout capture for dual-backend tests.
    /// When `Some`, `print`/`println` append here instead of writing the process stdout.
    /// Actor workers and parasteps receive this buffer explicitly via `set_stdout_buf`
    /// (there is deliberately no process-wide sink — a global slot raced under
    /// parallel test scheduling, letting one test's output leak into another's buffer).
    stdout_capture: Option<std::sync::Arc<std::sync::Mutex<String>>>,
    /// v0.31.15: canonical semantic trace collector.
    /// Disabled by default; enable via `enable_trace()` for trace comparison.
    pub trace_collector: crate::trace::TraceCollector,
}

impl<'a> Interpreter<'a> {
    pub fn from_checked(program: &'a crate::core::CheckedProgram) -> Self {
        let mut interp = Self::new(program.legacy_body_file());
        let mut resolved = HashMap::new();
        let mut fallbacks = std::collections::HashSet::new();
        let mut pinned = std::collections::HashSet::new();
        let mut param_arity = HashMap::new();
        let mut param_lists = HashMap::new();
        for (id, transition) in program.transitions() {
            let key = (id.flow.0.clone(), id.event.clone(), id.source.name.clone());
            let targets = transition
                .targets
                .iter()
                .map(|state| state.name.clone())
                .collect();
            if transition.is_fallback {
                fallbacks.insert(key.clone());
            }
            if transition.is_ffi_pinned {
                pinned.insert(key.clone());
            }
            param_arity.insert(key.clone(), transition.params.len());
            param_lists.insert(
                key.clone(),
                transition
                    .params
                    .iter()
                    .map(|(name, ty)| (name.clone(), crate::core::fmt_type(ty)))
                    .collect(),
            );
            resolved.insert(key, targets);
        }
        interp.resolved_transitions = Some(resolved);
        interp.resolved_fallback_transitions = Some(fallbacks);
        interp.resolved_ffi_pinned_transitions = Some(pinned);
        interp.resolved_transition_param_arity = Some(param_arity);
        let mut transitions_by_flow: HashMap<
            String,
            Vec<(String, String, String, bool, bool, usize)>,
        > = HashMap::new();
        for transition in program.transitions().values() {
            let flow = transition.id.flow.0.clone();
            let event = transition.id.event.clone();
            let source = transition.id.source.name.clone();
            let targets = transition
                .targets
                .iter()
                .map(|s| s.name.clone())
                .collect::<Vec<_>>()
                .join("|");
            transitions_by_flow.entry(flow).or_default().push((
                event,
                source,
                targets,
                transition.is_fallback,
                transition.is_ffi_pinned,
                transition.params.len(),
            ));
        }
        for list in transitions_by_flow.values_mut() {
            list.sort();
        }
        let mut transitions_by_event: HashMap<
            String,
            Vec<(String, String, String, bool, bool, usize)>,
        > = HashMap::new();
        for transition in program.transitions().values() {
            let flow = transition.id.flow.0.clone();
            let event = transition.id.event.clone();
            let source = transition.id.source.name.clone();
            let targets = transition
                .targets
                .iter()
                .map(|s| s.name.clone())
                .collect::<Vec<_>>()
                .join("|");
            transitions_by_event.entry(event).or_default().push((
                flow,
                source,
                targets,
                transition.is_fallback,
                transition.is_ffi_pinned,
                transition.params.len(),
            ));
        }
        for list in transitions_by_event.values_mut() {
            list.sort();
        }
        interp.resolved_transitions_by_flow = Some(transitions_by_flow);
        interp.resolved_transitions_by_event = Some(transitions_by_event);
        interp.resolved_transition_params = Some(param_lists);
        let mut functions = HashMap::new();
        let mut function_params = HashMap::new();
        let mut comptime_functions = std::collections::HashSet::new();
        for function in program.functions().values() {
            functions.insert(
                function.qualified_name.clone(),
                (
                    function.params.len(),
                    crate::core::fmt_type(&function.ret),
                    function.effects.clone(),
                ),
            );
            function_params.insert(
                function.qualified_name.clone(),
                function
                    .params
                    .iter()
                    .map(|(name, ty)| (name.clone(), crate::core::fmt_type(ty)))
                    .collect(),
            );
            if function.is_comptime {
                comptime_functions.insert(function.qualified_name.clone());
            }
        }
        interp.resolved_functions = Some(functions);
        interp.resolved_function_params = Some(function_params);
        interp.resolved_comptime_functions = Some(comptime_functions);
        let mut sessions = HashMap::new();
        let mut session_displays = HashMap::new();
        for session in program.sessions().values() {
            sessions.insert(session.qualified_name.clone(), session.body.clone());
            session_displays.insert(session.qualified_name.clone(), session.body_display.clone());
        }
        interp.resolved_sessions = Some(sessions);
        interp.resolved_session_displays = Some(session_displays);
        let protocols = program
            .protocols()
            .values()
            .map(|protocol| protocol.qualified_name.clone())
            .collect();
        interp.resolved_protocols = Some(protocols);
        let mut protocol_transitions = HashMap::new();
        let mut protocol_payloads = HashMap::new();
        let mut protocol_states = HashMap::new();
        let mut protocol_state_payloads = HashMap::new();
        for protocol in program.protocols().values() {
            protocol_transitions.insert(
                protocol.qualified_name.clone(),
                protocol
                    .transition_records
                    .iter()
                    .map(|tr| {
                        (
                            tr.event.clone(),
                            tr.from_state.clone(),
                            tr.to_states.first().cloned().unwrap_or_default(),
                        )
                    })
                    .collect(),
            );
            let mut state_names: Vec<String> = protocol.states.clone();
            state_names.sort();
            protocol_states.insert(protocol.qualified_name.clone(), state_names);
            for state in &protocol.state_payloads {
                if let Some(ty) = &state.payload_type {
                    protocol_payloads.insert(
                        format!("{}.{}", protocol.qualified_name, state.name),
                        ty.clone(),
                    );
                    protocol_state_payloads.insert(
                        format!("{}.{}", protocol.qualified_name, state.name),
                        (state.payload_name.clone().unwrap_or_default(), ty.clone()),
                    );
                }
            }
        }
        interp.resolved_protocol_transitions = Some(protocol_transitions);
        interp.resolved_protocol_payloads = Some(protocol_payloads);
        interp.resolved_protocol_states = Some(protocol_states);
        interp.resolved_protocol_state_payloads = Some(protocol_state_payloads);
        let mut actors = HashMap::new();
        let mut actor_method_signatures = HashMap::new();
        let mut actor_method_params = HashMap::new();
        let mut actor_method_effects = HashMap::new();
        let mut actor_fields = HashMap::new();
        for actor in program.actors().values() {
            actors.insert(actor.qualified_name.clone(), actor.methods.clone());
            for method in &actor.method_signatures {
                let key = format!("{}.{}", actor.qualified_name, method.name);
                actor_method_signatures
                    .insert(key.clone(), (method.params.len(), method.ret.clone()));
                actor_method_params.insert(key.clone(), method.params.clone());
                actor_method_effects.insert(key, method.effects.clone());
            }
            if !actor.fields.is_empty() {
                actor_fields.insert(
                    actor.qualified_name.clone(),
                    actor
                        .fields
                        .iter()
                        .map(|(name, ty, mut_)| (name.clone(), crate::core::fmt_type(ty), *mut_))
                        .collect(),
                );
            }
        }
        interp.resolved_actors = Some(actors);
        interp.resolved_actor_method_signatures = Some(actor_method_signatures);
        interp.resolved_actor_method_params = Some(actor_method_params);
        interp.resolved_actor_method_effects = Some(actor_method_effects);
        interp.resolved_actor_fields = Some(actor_fields);
        let capabilities = program
            .capabilities()
            .values()
            .map(|capability| capability.qualified_name.clone())
            .collect();
        interp.resolved_capabilities = Some(capabilities);
        let mut capability_combined = HashMap::new();
        for capability in program.capabilities().values() {
            if let Some(combined) = &capability.combined_with {
                capability_combined.insert(capability.qualified_name.clone(), combined.clone());
            }
        }
        interp.resolved_capability_combined = Some(capability_combined);
        let constants = program
            .constants()
            .values()
            .map(|constant| constant.qualified_name.clone())
            .collect();
        interp.resolved_constants = Some(constants);
        let mut constant_values = HashMap::new();
        for constant in program.constants().values() {
            constant_values.insert(
                constant.qualified_name.clone(),
                (
                    constant.ty.clone(),
                    encode_resolved_const_value(&constant.value),
                ),
            );
        }
        interp.resolved_constant_values = Some(constant_values);
        let mut traits = HashMap::new();
        let mut method_signatures = HashMap::new();
        let mut method_params = HashMap::new();
        let mut method_effects = HashMap::new();
        for trait_def in program.traits().values() {
            traits.insert(trait_def.qualified_name.clone(), trait_def.methods.clone());
            for method in &trait_def.method_signatures {
                let key = format!("{}.{}", trait_def.qualified_name, method.name);
                method_signatures.insert(key.clone(), (method.params.len(), method.ret.clone()));
                method_params.insert(key.clone(), method.params.clone());
                method_effects.insert(key, method.effects.clone());
            }
        }
        interp.resolved_traits = Some(traits);
        let mut impls = HashMap::new();
        for impl_def in program.impls().values() {
            impls.insert(impl_def.qualified_name.clone(), impl_def.methods.clone());
            for method in &impl_def.method_signatures {
                let key = format!("{}.{}", impl_def.qualified_name, method.name);
                method_signatures.insert(key.clone(), (method.params.len(), method.ret.clone()));
                method_params.insert(key.clone(), method.params.clone());
                method_effects.insert(key, method.effects.clone());
            }
        }
        interp.resolved_impls = Some(impls);
        interp.resolved_method_signatures = Some(method_signatures);
        interp.resolved_method_params = Some(method_params);
        interp.resolved_method_effects = Some(method_effects);
        interp.resolved_ownership_owners = Some(
            program
                .ownership_ledgers()
                .keys()
                .map(|owner| owner.0.clone())
                .collect(),
        );
        let mut ownership_summaries = HashMap::new();
        let mut ownership_resources = HashMap::new();
        let mut ownership_actions = HashMap::new();
        let mut ownership_merges = HashMap::new();
        for (owner, ledger) in program.ownership_ledgers() {
            ownership_summaries.insert(
                owner.0.clone(),
                (
                    ledger.action_count(crate::core::ResourceActionKind::Introduce),
                    ledger.action_count(crate::core::ResourceActionKind::Move),
                    ledger.action_count(crate::core::ResourceActionKind::Drop),
                    ledger.action_count(crate::core::ResourceActionKind::Return),
                    ledger.branch_merges.len(),
                    ledger.has_maybe_consumed_merge(),
                ),
            );
            ownership_resources.insert(owner.0.clone(), ledger.resources());
            ownership_actions.insert(
                owner.0.clone(),
                ledger
                    .actions
                    .iter()
                    .map(|action| (action.kind.as_str().to_string(), action.resource.clone()))
                    .collect(),
            );
            ownership_merges.insert(
                owner.0.clone(),
                ledger
                    .branch_merges
                    .iter()
                    .map(|merge| {
                        let encode = |s: crate::core::ResourceState| match s {
                            crate::core::ResourceState::Available => "available",
                            crate::core::ResourceState::Consumed => "consumed",
                            crate::core::ResourceState::MaybeConsumed => "maybe_consumed",
                        };
                        (
                            merge.resource.clone(),
                            encode(merge.then_state).to_string(),
                            encode(merge.else_state).to_string(),
                            encode(merge.merged_state).to_string(),
                        )
                    })
                    .collect(),
            );
        }
        interp.resolved_ownership_summaries = Some(ownership_summaries);
        interp.resolved_ownership_resources = Some(ownership_resources);
        interp.resolved_ownership_actions = Some(ownership_actions);
        interp.resolved_ownership_merges = Some(ownership_merges);
        interp.resolved_backend_requirements = Some(
            program
                .backend_requirements()
                .iter()
                .map(|req| (req.capability.to_string(), req.flow.0.clone()))
                .collect(),
        );
        interp.resolved_node_meta_count = Some(program.node_meta().len());
        interp.resolved_node_meta_paths = Some(
            program
                .node_meta()
                .keys()
                .map(|node_id| node_id.0.clone())
                .collect(),
        );
        let mut node_meta_precision = HashMap::new();
        for (node_id, meta) in program.node_meta() {
            let precision = match meta.precision {
                crate::core::SpanPrecision::Exact => "exact",
                crate::core::SpanPrecision::SourceAnchor => "source_anchor",
                crate::core::SpanPrecision::DeclarationFallback => "declaration_fallback",
            };
            node_meta_precision.insert(node_id.0.clone(), precision.to_string());
        }
        interp.resolved_node_meta_precision = Some(node_meta_precision);
        let mut node_meta_spans = HashMap::new();
        for (node_id, meta) in program.node_meta() {
            let span = meta.origin.user_span();
            node_meta_spans.insert(
                node_id.0.clone(),
                (span.start_line, span.start_col, span.end_line, span.end_col),
            );
        }
        interp.resolved_node_meta_spans = Some(node_meta_spans);
        let mut type_kinds = HashMap::new();
        let mut type_fields = HashMap::new();
        let mut type_variants = HashMap::new();
        let mut type_aliases = HashMap::new();
        for type_def in program.type_defs().values() {
            let kind = match type_def.kind {
                crate::core::ResolvedTypeKind::Alias => "alias",
                crate::core::ResolvedTypeKind::Newtype => "newtype",
                crate::core::ResolvedTypeKind::Record => "record",
                crate::core::ResolvedTypeKind::Enum => "enum",
                crate::core::ResolvedTypeKind::Union => "union",
            };
            type_kinds.insert(type_def.qualified_name.clone(), kind.to_string());
            if !type_def.fields.is_empty() {
                type_fields.insert(type_def.qualified_name.clone(), type_def.fields.clone());
            }
            if !type_def.variants.is_empty() {
                type_variants.insert(type_def.qualified_name.clone(), type_def.variants.clone());
            }
            if let Some(alias) = &type_def.alias_of {
                type_aliases.insert(type_def.qualified_name.clone(), alias.clone());
            }
        }
        interp.resolved_type_kinds = Some(type_kinds);
        interp.resolved_type_fields = Some(type_fields);
        interp.resolved_type_variants = Some(type_variants);
        interp.resolved_type_aliases = Some(type_aliases);

        let mut extern_funcs = std::collections::HashSet::new();
        let mut extern_abis = HashMap::new();
        for block in program.extern_blocks().values() {
            for func in &block.funcs {
                extern_funcs.insert(func.clone());
                extern_abis.insert(func.clone(), block.abi.clone());
            }
        }
        interp.resolved_extern_funcs = Some(extern_funcs);
        interp.resolved_extern_abis = Some(extern_abis);
        let mut extern_signatures = HashMap::new();
        let mut extern_params = HashMap::new();
        for block in program.extern_blocks().values() {
            for sig in &block.signatures {
                extern_signatures.insert(sig.name.clone(), (sig.params.len(), sig.ret.clone()));
                extern_params.insert(sig.name.clone(), sig.params.clone());
            }
        }
        interp.resolved_extern_signatures = Some(extern_signatures);
        interp.resolved_extern_params = Some(extern_params);
        let mut extern_no_panic = std::collections::HashSet::new();
        let mut extern_unsafe = std::collections::HashSet::new();
        for block in program.extern_blocks().values() {
            for func in &block.funcs {
                if block.no_panic {
                    extern_no_panic.insert(func.clone());
                }
                if block.unsafe_ {
                    extern_unsafe.insert(func.clone());
                }
            }
        }
        interp.resolved_extern_no_panic = Some(extern_no_panic);
        interp.resolved_extern_unsafe = Some(extern_unsafe);
        let mut call_sites = HashMap::new();
        for (node_id, site) in program.call_sites() {
            call_sites.insert(
                node_id.0.clone(),
                (
                    site.owner.clone(),
                    site.callee.clone(),
                    site.argc,
                    site.expected_argc,
                    site.effects.clone(),
                    site.ret.clone(),
                    match site.kind {
                        crate::core::ResolvedCallKind::Function => "function".into(),
                        crate::core::ResolvedCallKind::Extern => "extern".into(),
                        crate::core::ResolvedCallKind::Builtin => "builtin".into(),
                        crate::core::ResolvedCallKind::Method => "method".into(),
                        crate::core::ResolvedCallKind::Unknown => "unknown".into(),
                    },
                ),
            );
        }
        interp.resolved_call_sites = Some(call_sites);
        let mut call_sites_by_owner: HashMap<String, Vec<(String, usize, String)>> = HashMap::new();
        if let Some(sites) = interp.resolved_call_sites.as_ref() {
            for (_path, (owner, callee, argc, _expected, _effects, _ret, kind)) in sites {
                call_sites_by_owner.entry(owner.clone()).or_default().push((
                    callee.clone(),
                    *argc,
                    kind.clone(),
                ));
            }
        }
        interp.resolved_call_sites_by_owner = Some(call_sites_by_owner);
        let mut call_sites_by_callee: HashMap<String, Vec<(String, usize, String)>> =
            HashMap::new();
        if let Some(sites) = interp.resolved_call_sites.as_ref() {
            for (_path, (owner, callee, argc, _expected, _effects, _ret, kind)) in sites {
                call_sites_by_callee
                    .entry(callee.clone())
                    .or_default()
                    .push((owner.clone(), *argc, kind.clone()));
            }
        }
        interp.resolved_call_sites_by_callee = Some(call_sites_by_callee);
        // Prefer CheckedProgram flow annotations for process spawn quota.
        let checked_max = program.flows().values().find_map(|flow| flow.max_children);
        if checked_max.is_some() {
            interp.max_children = checked_max;
        }
        let mut mailbox_depths = HashMap::new();
        for flow in program.flows().values() {
            if let Some(depth) = flow.mailbox_depth {
                mailbox_depths.insert(flow.id.0.clone(), depth);
            }
        }
        interp.resolved_mailbox_depths = Some(mailbox_depths);
        let mut flow_state_payloads = HashMap::new();
        for flow in program.flows().values() {
            for (state_name, state) in &flow.states {
                if !state.payload.is_empty() {
                    flow_state_payloads.insert(
                        format!("{}.{}", flow.id.0, state_name),
                        state
                            .payload
                            .iter()
                            .map(|(name, ty)| (name.clone(), crate::core::fmt_type(ty)))
                            .collect(),
                    );
                }
            }
        }
        interp.resolved_flow_state_payloads = Some(flow_state_payloads);
        let mut flow_states = HashMap::new();
        for flow in program.flows().values() {
            let mut names: Vec<String> = flow.states.keys().cloned().collect();
            names.sort();
            flow_states.insert(flow.id.0.clone(), names);
        }
        interp.resolved_flow_states = Some(flow_states);
        let mut flow_events = HashMap::new();
        for flow in program.flows().values() {
            let mut events: Vec<String> = flow
                .transitions
                .iter()
                .map(|tid| tid.event.clone())
                .collect();
            events.sort();
            events.dedup();
            flow_events.insert(flow.id.0.clone(), events);
        }
        interp.resolved_flow_events = Some(flow_events);
        let mut item_kinds = HashMap::new();
        for item in program.items().values() {
            let kind = match item.kind {
                crate::core::ResolvedItemKind::Function => "function",
                crate::core::ResolvedItemKind::Type => "type",
                crate::core::ResolvedItemKind::Constant => "const",
                crate::core::ResolvedItemKind::Capability => "capability",
                crate::core::ResolvedItemKind::Trait => "trait",
                crate::core::ResolvedItemKind::Impl => "impl",
                crate::core::ResolvedItemKind::ExternBlock => "extern",
                crate::core::ResolvedItemKind::Module => "module",
                crate::core::ResolvedItemKind::Actor => "actor",
                crate::core::ResolvedItemKind::Flow => "flow",
                crate::core::ResolvedItemKind::Protocol => "protocol",
                crate::core::ResolvedItemKind::Session => "session",
            };
            item_kinds.insert(item.qualified_name.clone(), kind.to_string());
        }
        interp.resolved_item_kinds = Some(item_kinds);
        let mut persistent_fields = HashMap::new();
        for flow in program.flows().values() {
            if !flow.persistent_fields.is_empty() {
                persistent_fields.insert(flow.id.0.clone(), flow.persistent_fields.clone());
            }
        }
        interp.resolved_persistent_fields = Some(persistent_fields);
        let mut transactional_fields = HashMap::new();
        let mut metadata_shadow_fields = HashMap::new();
        for flow in program.flows().values() {
            if !flow.transactional_fields.is_empty() {
                transactional_fields.insert(flow.id.0.clone(), flow.transactional_fields.clone());
            }
            if !flow.metadata_shadow_fields.is_empty() {
                metadata_shadow_fields
                    .insert(flow.id.0.clone(), flow.metadata_shadow_fields.clone());
            }
        }
        interp.resolved_transactional_fields = Some(transactional_fields);
        interp.resolved_metadata_shadow_fields = Some(metadata_shadow_fields);
        let mut flow_protocols = HashMap::new();
        for flow in program.flows().values() {
            if !flow.impl_protocols.is_empty() {
                flow_protocols.insert(flow.id.0.clone(), flow.impl_protocols.clone());
            }
        }
        interp.resolved_flow_protocols = Some(flow_protocols);
        interp
    }

    /// Test/diagnostic access to CheckedProgram function directory.
    pub(crate) fn resolved_function_arity(&self, qualified_name: &str) -> Option<usize> {
        self.resolved_functions
            .as_ref()
            .and_then(|map| map.get(qualified_name).map(|(arity, _, _)| *arity))
    }

    /// Test/diagnostic access to CheckedProgram function effects.
    pub(crate) fn resolved_function_effects(&self, qualified_name: &str) -> Option<Vec<String>> {
        self.resolved_functions.as_ref().and_then(|map| {
            map.get(qualified_name)
                .map(|(_, _, effects)| effects.clone())
        })
    }

    pub(crate) fn resolved_function_params(
        &self,
        qualified_name: &str,
    ) -> Option<Vec<(String, String)>> {
        self.resolved_function_params
            .as_ref()
            .and_then(|map| map.get(qualified_name).cloned())
    }

    pub(crate) fn is_resolved_comptime_function(&self, qualified_name: &str) -> bool {
        self.resolved_comptime_functions
            .as_ref()
            .is_some_and(|set| set.contains(qualified_name))
    }

    pub(crate) fn has_resolved_session(&self, qualified_name: &str) -> bool {
        self.resolved_sessions
            .as_ref()
            .is_some_and(|map| map.contains_key(qualified_name))
    }

    pub(crate) fn resolved_session_display(&self, qualified_name: &str) -> Option<&str> {
        self.resolved_session_displays
            .as_ref()
            .and_then(|map| map.get(qualified_name).map(String::as_str))
    }

    pub(crate) fn has_resolved_protocol(&self, qualified_name: &str) -> bool {
        self.resolved_protocols
            .as_ref()
            .is_some_and(|set| set.contains(qualified_name))
    }

    pub(crate) fn resolved_protocol_transitions(
        &self,
        protocol: &str,
    ) -> Option<Vec<(String, String, String)>> {
        self.resolved_protocol_transitions
            .as_ref()
            .and_then(|map| map.get(protocol).cloned())
    }

    pub(crate) fn resolved_protocol_payload(&self, protocol: &str, state: &str) -> Option<String> {
        self.resolved_protocol_payloads
            .as_ref()
            .and_then(|map| map.get(&format!("{protocol}.{state}")).cloned())
    }

    pub(crate) fn resolved_protocol_states(&self, protocol: &str) -> Option<Vec<String>> {
        self.resolved_protocol_states
            .as_ref()
            .and_then(|map| map.get(protocol).cloned())
    }

    pub(crate) fn resolved_protocol_state_payload(
        &self,
        protocol: &str,
        state: &str,
    ) -> Option<(String, String)> {
        self.resolved_protocol_state_payloads
            .as_ref()
            .and_then(|map| map.get(&format!("{protocol}.{state}")).cloned())
    }

    pub(crate) fn resolved_actor_methods(&self, qualified_name: &str) -> Option<Vec<String>> {
        self.resolved_actors
            .as_ref()
            .and_then(|map| map.get(qualified_name).cloned())
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

    pub(crate) fn resolved_actor_method_effects(
        &self,
        actor: &str,
        method: &str,
    ) -> Option<Vec<String>> {
        self.resolved_actor_method_effects
            .as_ref()
            .and_then(|map| map.get(&format!("{actor}.{method}")).cloned())
    }

    pub(crate) fn resolved_actor_fields(&self, actor: &str) -> Option<Vec<(String, String, bool)>> {
        self.resolved_actor_fields
            .as_ref()
            .and_then(|map| map.get(actor).cloned())
    }

    pub(crate) fn has_resolved_capability(&self, qualified_name: &str) -> bool {
        self.resolved_capabilities
            .as_ref()
            .is_some_and(|set| set.contains(qualified_name))
    }

    pub(crate) fn resolved_capability_combined_with(&self, qualified_name: &str) -> Option<&str> {
        self.resolved_capability_combined
            .as_ref()
            .and_then(|map| map.get(qualified_name).map(String::as_str))
    }

    pub(crate) fn has_resolved_constant(&self, qualified_name: &str) -> bool {
        self.resolved_constants
            .as_ref()
            .is_some_and(|set| set.contains(qualified_name))
    }

    pub(crate) fn resolved_constant_value(
        &self,
        qualified_name: &str,
    ) -> Option<(Option<String>, String)> {
        self.resolved_constant_values
            .as_ref()
            .and_then(|map| map.get(qualified_name).cloned())
    }

    pub(crate) fn resolved_trait_methods(&self, qualified_name: &str) -> Option<Vec<String>> {
        self.resolved_traits
            .as_ref()
            .and_then(|map| map.get(qualified_name).cloned())
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

    pub(crate) fn resolved_method_effects(&self, key: &str) -> Option<Vec<String>> {
        self.resolved_method_effects
            .as_ref()
            .and_then(|map| map.get(key).cloned())
    }

    pub(crate) fn resolved_impl_methods(
        &self,
        trait_name: &str,
        type_name: &str,
    ) -> Option<Vec<String>> {
        let key = format!("{}:for:{}", trait_name, type_name);
        self.resolved_impls
            .as_ref()
            .and_then(|map| map.get(&key).cloned())
    }

    pub(crate) fn has_resolved_ownership_owner(&self, owner: &str) -> bool {
        self.resolved_ownership_owners
            .as_ref()
            .is_some_and(|set| set.contains(owner))
    }

    pub(crate) fn resolved_ownership_summary(
        &self,
        owner: &str,
    ) -> Option<(usize, usize, usize, usize, usize, bool)> {
        self.resolved_ownership_summaries
            .as_ref()
            .and_then(|map| map.get(owner).copied())
    }

    pub(crate) fn resolved_ownership_resources(&self, owner: &str) -> Option<Vec<String>> {
        self.resolved_ownership_resources
            .as_ref()
            .and_then(|map| map.get(owner).cloned())
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

    pub(crate) fn resolved_node_meta_span(
        &self,
        path: &str,
    ) -> Option<(usize, usize, usize, usize)> {
        self.resolved_node_meta_spans
            .as_ref()
            .and_then(|map| map.get(path).copied())
    }

    pub(crate) fn requires_resolved_capability(&self, capability: &str) -> bool {
        self.resolved_backend_requirements
            .as_ref()
            .is_some_and(|reqs| reqs.iter().any(|(cap, _)| cap == capability))
    }

    pub(crate) fn resolved_type_kind(&self, qualified_name: &str) -> Option<&str> {
        self.resolved_type_kinds
            .as_ref()
            .and_then(|map| map.get(qualified_name).map(String::as_str))
    }

    pub(crate) fn resolved_type_fields(
        &self,
        qualified_name: &str,
    ) -> Option<Vec<(String, String)>> {
        self.resolved_type_fields
            .as_ref()
            .and_then(|map| map.get(qualified_name).cloned())
    }

    pub(crate) fn resolved_type_variants(
        &self,
        qualified_name: &str,
    ) -> Option<Vec<(String, Option<String>)>> {
        self.resolved_type_variants
            .as_ref()
            .and_then(|map| map.get(qualified_name).cloned())
    }

    pub(crate) fn resolved_type_alias_of(&self, qualified_name: &str) -> Option<&str> {
        self.resolved_type_aliases
            .as_ref()
            .and_then(|map| map.get(qualified_name).map(String::as_str))
    }

    pub(crate) fn has_resolved_extern_func(&self, name: &str) -> bool {
        self.resolved_extern_funcs
            .as_ref()
            .is_some_and(|set| set.contains(name))
    }

    pub(crate) fn resolved_extern_abi(&self, name: &str) -> Option<&str> {
        self.resolved_extern_abis
            .as_ref()
            .and_then(|map| map.get(name).map(String::as_str))
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

    pub(crate) fn resolved_call_sites(
        &self,
    ) -> Option<
        &HashMap<
            String,
            (
                String,
                String,
                usize,
                Option<usize>,
                Vec<String>,
                Option<String>,
                String,
            ),
        >,
    > {
        self.resolved_call_sites.as_ref()
    }

    pub(crate) fn resolved_call_sites_for_owner(
        &self,
        owner: &str,
    ) -> Option<Vec<(String, usize, String)>> {
        self.resolved_call_sites_by_owner
            .as_ref()
            .and_then(|map| map.get(owner).cloned())
    }

    pub(crate) fn resolved_call_sites_for_callee(
        &self,
        callee: &str,
    ) -> Option<Vec<(String, usize, String)>> {
        self.resolved_call_sites_by_callee
            .as_ref()
            .and_then(|map| map.get(callee).cloned())
    }

    pub(crate) fn has_resolved_call_to(&self, callee: &str) -> bool {
        self.resolved_call_sites
            .as_ref()
            .is_some_and(|map| map.values().any(|(_, name, _, _, _, _, _)| name == callee))
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

    pub(crate) fn is_resolved_ffi_pinned_transition(
        &self,
        flow: &str,
        event: &str,
        source: &str,
    ) -> bool {
        self.resolved_ffi_pinned_transitions
            .as_ref()
            .is_some_and(|set| {
                set.contains(&(flow.to_string(), event.to_string(), source.to_string()))
            })
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

    pub(crate) fn resolved_max_children(&self) -> Option<usize> {
        self.max_children
    }

    pub(crate) fn resolved_persistent_fields(&self, flow_name: &str) -> Option<Vec<String>> {
        let Some(map) = self.resolved_persistent_fields.as_ref() else {
            return None;
        };
        if let Some(fields) = map.get(flow_name) {
            return Some(fields.clone());
        }
        map.iter().find_map(|(qualified, fields)| {
            qualified
                .rsplit("::")
                .next()
                .filter(|bare| *bare == flow_name)
                .map(|_| fields.clone())
        })
    }

    pub(in crate::interp) fn effective_persistent_fields(&self, flow: &FlowDef) -> Vec<String> {
        self.resolved_persistent_fields(&flow.name)
            .unwrap_or_else(|| flow.persistent_fields.clone())
    }

    fn resolved_field_set(
        map: &Option<HashMap<String, Vec<String>>>,
        flow_name: &str,
    ) -> Option<Vec<String>> {
        let Some(map) = map.as_ref() else {
            return None;
        };
        if let Some(fields) = map.get(flow_name) {
            return Some(fields.clone());
        }
        map.iter().find_map(|(qualified, fields)| {
            qualified
                .rsplit("::")
                .next()
                .filter(|bare| *bare == flow_name)
                .map(|_| fields.clone())
        })
    }

    pub(in crate::interp) fn effective_transactional_fields(&self, flow: &FlowDef) -> Vec<String> {
        Self::resolved_field_set(&self.resolved_transactional_fields, &flow.name)
            .unwrap_or_else(|| flow.transactional_fields.clone())
    }

    pub(in crate::interp) fn effective_metadata_shadow_fields(
        &self,
        flow: &FlowDef,
    ) -> Vec<String> {
        Self::resolved_field_set(&self.resolved_metadata_shadow_fields, &flow.name)
            .unwrap_or_else(|| flow.metadata_shadow_fields.clone())
    }

    pub(crate) fn resolved_flow_protocols(&self, flow_name: &str) -> Option<Vec<String>> {
        Self::resolved_field_set(&self.resolved_flow_protocols, flow_name)
    }

    pub(crate) fn resolved_mailbox_depth(&self, flow_name: &str) -> Option<usize> {
        let Some(map) = self.resolved_mailbox_depths.as_ref() else {
            return None;
        };
        if let Some(depth) = map.get(flow_name) {
            return Some(*depth);
        }
        // Module-qualified flows: "pkg::Worker" should match actor/flow name "Worker".
        map.iter().find_map(|(qualified, depth)| {
            qualified
                .rsplit("::")
                .next()
                .filter(|bare| *bare == flow_name)
                .map(|_| *depth)
        })
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

    pub(crate) fn resolved_item_kind(&self, qualified_name: &str) -> Option<&str> {
        self.resolved_item_kinds
            .as_ref()
            .and_then(|map| map.get(qualified_name).map(String::as_str))
    }

    pub(crate) fn new(file: &'a File) -> Self {
        let mut constructors = HashMap::new();
        let mut newtype_constructors = HashMap::new();
        let mut type_variants: HashMap<String, Vec<String>> = HashMap::new();
        let mut variant_parent: HashMap<String, String> = HashMap::new();
        let mut variant_field_positions: HashMap<String, HashMap<String, usize>> = HashMap::new();
        let mut failure_variants: HashMap<String, bool> = HashMap::new();
        let mut cap_defs: HashMap<String, Vec<String>> = HashMap::new();
        for item in &file.items {
            Self::collect_constructors(
                item,
                &mut constructors,
                &mut newtype_constructors,
                &mut type_variants,
                &mut variant_parent,
                &mut variant_field_positions,
                &mut failure_variants,
            );
            Self::collect_caps(item, &mut cap_defs);
        }
        // Register built-in Result/Option constructors
        constructors.insert("Ok".to_string(), 1);
        constructors.insert("Err".to_string(), 1);
        constructors.insert("Some".to_string(), 1);
        constructors.insert("None".to_string(), 0);
        // Also mark Err and None as failure variants for the ? operator
        failure_variants.insert("Err".to_string(), true);
        failure_variants.insert("None".to_string(), true);
        let mut trait_defs = HashMap::new();
        let mut type_impls: HashMap<String, HashMap<String, Vec<FuncDef>>> = HashMap::new();
        let mut extern_funcs: HashMap<String, ExternFunc> = HashMap::new();
        let mut ffi_contracts: HashMap<String, FfiContract> = HashMap::new();
        let mut type_defs: HashMap<String, TypeDef> = HashMap::new();
        for item in &file.items {
            Self::collect_traits(item, &mut trait_defs, &mut type_impls);
            Self::collect_type_defs(item, &mut type_defs);
        }
        // Build contracts after type_defs are populated so record type names are known
        for item in &file.items {
            Self::collect_extern_funcs(
                item,
                &mut extern_funcs,
                &mut ffi_contracts,
                &cap_defs,
                &type_defs,
            );
        }
        // Expand built-in derive macros. Source-parsed files have already been
        // validated, but programmatically constructed ASTs must also fail closed.
        let derive_expansion_error =
            Self::expand_derives(&type_defs, &mut trait_defs, &mut type_impls).err();
        // Build O(1) function, actor, and flow lookup indices
        let mut func_index = HashMap::new();
        let mut actor_index = HashMap::new();
        let mut flow_index = HashMap::new();
        Self::build_func_index(&file.items, &mut func_index);
        Self::build_actor_index(&file.items, &mut actor_index);
        Self::build_flow_index(&file.items, &mut flow_index);
        // v0.29.24: first `@max_children(N)` among flows sets process spawn quota.
        let max_children = flow_index
            .values()
            .flat_map(|f| f.annotations.iter())
            .find_map(|a| match &a.kind {
                crate::ast::FlowAnnotationKind::MaxChildren(n) => Some(*n),
                _ => None,
            });
        Self {
            file,
            scope_env: ScopeEnv::new(),
            constructors,
            newtype_constructors,
            type_variants,
            variant_parent,
            variant_field_positions,
            failure_variants,
            cap_defs,
            compensation_stack: Vec::new(),
            compensation_error_count: 0,
            arenas: Vec::new(),
            arena_depth: 0,
            verify_contracts: true,
            verify_ffi: true,
            trait_defs,
            type_impls,
            derive_expansion_error,
            extern_funcs,
            ffi_contracts,
            type_defs,
            comptime_results: HashMap::new(),
            loaded_libs: Vec::new(),
            default_allocator: AllocatorKind::System,
            loop_action: None,
            early_return: None,
            last_mutate_writebacks: Vec::new(),
            exited: None,
            recursion_depth: 0,
            func_index,
            actor_index,
            flow_index,
            resolved_transitions: None,
            resolved_fallback_transitions: None,
            resolved_ffi_pinned_transitions: None,
            resolved_transition_param_arity: None,
            resolved_transition_params: None,
            resolved_transitions_by_flow: None,
            resolved_transitions_by_event: None,
            resolved_node_meta_spans: None,
            resolved_functions: None,
            resolved_function_params: None,
            resolved_comptime_functions: None,
            resolved_sessions: None,
            resolved_session_displays: None,
            resolved_protocols: None,
            resolved_protocol_transitions: None,
            resolved_protocol_payloads: None,
            resolved_protocol_states: None,
            resolved_protocol_state_payloads: None,
            resolved_actors: None,
            resolved_actor_method_signatures: None,
            resolved_actor_method_params: None,
            resolved_actor_method_effects: None,
            resolved_actor_fields: None,
            resolved_capabilities: None,
            resolved_capability_combined: None,
            resolved_constants: None,
            resolved_constant_values: None,
            resolved_traits: None,
            resolved_method_signatures: None,
            resolved_method_params: None,
            resolved_method_effects: None,
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
            resolved_call_sites: None,
            resolved_call_sites_by_owner: None,
            resolved_call_sites_by_callee: None,
            resolved_mailbox_depths: None,
            resolved_flow_state_payloads: None,
            resolved_flow_states: None,
            resolved_flow_events: None,
            resolved_item_kinds: None,
            resolved_persistent_fields: None,
            resolved_transactional_fields: None,
            resolved_metadata_shadow_fields: None,
            resolved_flow_protocols: None,
            max_children,
            spawn_count: 0,
            actor_spawn_counts: std::collections::HashMap::new(),
            flow_tx: HashMap::new(),
            current_flow_state: None,
            globals: HashMap::new(),
            cli_args: Vec::new(),
            cstring_registry: std::cell::RefCell::new(Vec::new()),
            stdout_capture: None,
            trace_collector: crate::trace::TraceCollector::new(),
        }
    }

    /// TC-C1: redirect `print`/`println` into an in-memory buffer (no process stdout).
    /// Actor workers share the buffer via `set_stdout_buf` (explicit `Arc`, no global sink).
    pub fn enable_stdout_capture(&mut self) {
        let buf = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        self.stdout_capture = Some(buf);
    }

    /// Set the stdout capture buffer directly. Actor workers and parasteps
    /// receive the spawning interpreter's buffer this way — the worker owns
    /// its own reference to the same `Arc` (there is no process-wide sink).
    pub fn set_stdout_buf(&mut self, buf: std::sync::Arc<std::sync::Mutex<String>>) {
        self.stdout_capture = Some(buf);
    }

    /// Take captured stdout and disable further capture.
    pub fn take_stdout(&mut self) -> String {
        self.stdout_capture
            .take()
            .map(|b| b.lock().map(|g| g.clone()).unwrap_or_default())
            .unwrap_or_default()
    }

    /// Borrow a snapshot of captured stdout without disabling capture.
    pub fn captured_stdout(&self) -> Option<String> {
        self.stdout_capture
            .as_ref()
            .and_then(|b| b.lock().ok().map(|g| g.clone()))
    }

    pub(in crate::interp) fn emit_stdout(&self, text: &str) {
        if let Some(buf) = self.resolve_stdout_buf() {
            if let Ok(mut g) = buf.lock() {
                g.push_str(text);
                return;
            }
        }
        print!("{}", text);
    }

    pub(in crate::interp) fn emit_stdout_line(&self, text: &str) {
        if let Some(buf) = self.resolve_stdout_buf() {
            if let Ok(mut g) = buf.lock() {
                g.push_str(text);
                g.push('\n');
                return;
            }
        }
        println!("{}", text);
    }

    fn resolve_stdout_buf(&self) -> Option<std::sync::Arc<std::sync::Mutex<String>>> {
        self.stdout_capture.clone()
    }

    // Default Rust thread stack is 2MB; each interpreter frame is ~2KB.
    // 768 frames × 2KB = 1.5MB, leaving ~0.5MB headroom.
    const MAX_RECURSION_DEPTH: usize = 768;

    /// Apply a closure value: push scope, bind captured vars and params,
    /// eval body, handle early return, pop scope.
    fn apply_closure_inner(
        &mut self,
        params: &[Param],
        body: &Block,
        captured: &HashMap<String, Value>,
        args: Vec<Value>,
    ) -> Result<Value, InterpError> {
        if params.len() != args.len() {
            return Err(InterpError::new(format!(
                "closure expects {} arguments, got {}",
                params.len(),
                args.len()
            )));
        }
        let result = self.with_scope(|this| {
            for (n, v) in captured {
                this.bind(n, v.clone())?;
            }
            for (param, arg) in params.iter().zip(args) {
                this.bind(&param.name, arg)?;
            }
            this.eval_block(body)
        })?;
        if self.exited.is_some() {
            return Ok(result.unwrap_or(Value::Unit));
        }
        if let Some(val) = self.early_return.take() {
            return Ok(val);
        }
        Ok(result.unwrap_or(Value::Unit))
    }

    fn build_func_index(items: &[Item], index: &mut HashMap<String, FuncDef>) {
        Self::build_func_index_rec(items, "", index);
    }

    fn build_func_index_rec(items: &[Item], prefix: &str, index: &mut HashMap<String, FuncDef>) {
        for item in items {
            match item {
                Item::Func(f) => {
                    // Store by unqualified name (first wins)
                    index.entry(f.name.clone()).or_insert_with(|| f.clone());
                    // Store by qualified name
                    if !prefix.is_empty() {
                        let qualified = format!("{}::{}", prefix, f.name);
                        index.entry(qualified).or_insert_with(|| f.clone());
                    }
                }
                Item::Module(m) => {
                    let new_prefix = if prefix.is_empty() {
                        m.name.clone()
                    } else {
                        format!("{}::{}", prefix, m.name)
                    };
                    Self::build_func_index_rec(&m.items, &new_prefix, index);
                }
                // M13-fix: explicitly list non-indexed variants so new
                // Item variants trigger a compile warning (uncovered pattern).
                Item::Type(_)
                | Item::Actor(_)
                | Item::Cap(_)
                | Item::Trait(_)
                | Item::Impl(_)
                | Item::ExternBlock(_)
                | Item::Const { .. }
                | Item::Flow(_)
                | Item::Protocol(_)
                | Item::Session(_) => {}
                #[allow(unreachable_patterns)]
                _ => {}
            }
        }
    }

    fn build_actor_index(items: &[Item], index: &mut HashMap<String, ActorDef>) {
        for item in items {
            match item {
                Item::Actor(a) => {
                    index.insert(a.name.clone(), a.clone());
                }
                Item::Module(m) => Self::build_actor_index(&m.items, index),
                // M13-fix: explicitly list non-indexed variants
                Item::Func(_)
                | Item::Type(_)
                | Item::Cap(_)
                | Item::Trait(_)
                | Item::Impl(_)
                | Item::ExternBlock(_)
                | Item::Const { .. }
                | Item::Flow(_)
                | Item::Protocol(_)
                | Item::Session(_) => {}
                #[allow(unreachable_patterns)]
                _ => {}
            }
        }
    }

    fn build_flow_index(items: &[Item], index: &mut HashMap<String, FlowDef>) {
        for item in items {
            match item {
                Item::Flow(f) => {
                    index.insert(f.name.clone(), f.clone());
                }
                Item::Module(m) => Self::build_flow_index(&m.items, index),
                // M13-fix: explicitly list non-indexed variants
                Item::Func(_)
                | Item::Type(_)
                | Item::Actor(_)
                | Item::Cap(_)
                | Item::Trait(_)
                | Item::Impl(_)
                | Item::ExternBlock(_)
                | Item::Const { .. }
                | Item::Protocol(_)
                | Item::Session(_) => {}
                #[allow(unreachable_patterns)]
                _ => {}
            }
        }
    }

    fn collect_constructors(
        item: &Item,
        out: &mut HashMap<String, usize>,
        newtype_constructors: &mut HashMap<String, bool>,
        type_variants: &mut HashMap<String, Vec<String>>,
        variant_parent: &mut HashMap<String, String>,
        variant_field_positions: &mut HashMap<String, HashMap<String, usize>>,
        failure_variants: &mut HashMap<String, bool>,
    ) {
        match item {
            Item::Type(t) => {
                match &t.kind {
                    TypeDefKind::Enum(variants) => {
                        let mut variant_names = Vec::new();
                        for v in variants {
                            let arity = match &v.payload {
                                None => 0,
                                Some(VariantPayload::Tuple(types)) => types.len(),
                                Some(VariantPayload::Record(fields)) => {
                                    // Store field name → position for named constructor patterns
                                    let mut positions = HashMap::new();
                                    for (i, f) in fields.iter().enumerate() {
                                        positions.insert(f.name.clone(), i);
                                    }
                                    variant_field_positions.insert(v.name.clone(), positions);
                                    fields.len()
                                }
                            };
                            out.insert(v.name.clone(), arity);
                            variant_names.push(v.name.clone());
                            variant_parent.insert(v.name.clone(), t.name.clone());
                            // Mark failure-like variants
                            let name_lower = v.name.to_lowercase();
                            if name_lower == "err"
                                || name_lower == "none"
                                || name_lower.ends_with("error")
                                || name_lower.ends_with("fail")
                            {
                                failure_variants.insert(v.name.clone(), true);
                            }
                        }
                        type_variants.insert(t.name.clone(), variant_names);
                    }
                    TypeDefKind::Newtype(_) => {
                        out.insert(t.name.clone(), 1);
                        newtype_constructors.insert(t.name.clone(), true);
                    }
                    _ => {}
                }
            }
            Item::Module(m) => {
                for inner in &m.items {
                    Self::collect_constructors(
                        inner,
                        out,
                        newtype_constructors,
                        type_variants,
                        variant_parent,
                        variant_field_positions,
                        failure_variants,
                    );
                }
            }
            Item::Trait(_) | Item::Impl(_) => {
                // Traits and impls don't define constructors
            }
            _ => {}
        }
    }

    fn collect_extern_funcs(
        item: &Item,
        out: &mut HashMap<String, ExternFunc>,
        contracts: &mut HashMap<String, FfiContract>,
        cap_defs: &HashMap<String, Vec<String>>,
        type_defs: &HashMap<String, TypeDef>,
    ) {
        let cap_names: std::collections::HashSet<String> = cap_defs.keys().cloned().collect();
        let record_type_names: std::collections::HashSet<String> = type_defs
            .iter()
            .filter(|(_, td)| matches!(td.kind, TypeDefKind::Record(_)))
            .map(|(name, _)| name.clone())
            .collect();
        let repr_c_record_names: std::collections::HashSet<String> = type_defs
            .iter()
            .filter(|(_, td)| td.attributes.contains(&TypeAttribute::ReprC))
            .map(|(name, _)| name.clone())
            .collect();
        match item {
            Item::ExternBlock(block) => {
                let no_panic = block.no_panic;
                for func in &block.funcs {
                    let mut f = func.clone();
                    // Propagate block-level no_panic to each function
                    if no_panic {
                        f.no_panic = true;
                    }
                    out.insert(f.name.clone(), f);
                    contracts.insert(
                        func.name.clone(),
                        FfiContract::from_extern_with_caps_repr(
                            func,
                            &cap_names,
                            &record_type_names,
                            &repr_c_record_names,
                        ),
                    );
                }
            }
            Item::Module(m) => {
                for inner in &m.items {
                    Self::collect_extern_funcs(inner, out, contracts, cap_defs, type_defs);
                }
            }
            _ => {}
        }
    }

    fn collect_type_defs(item: &Item, out: &mut HashMap<String, TypeDef>) {
        match item {
            Item::Type(t) => {
                out.insert(t.name.clone(), t.clone());
            }
            Item::Actor(actor) => {
                let actor_type_def = TypeDef {
                    meta: AstNodeMeta::inherited(
                        actor.meta.span,
                        AstOrigin::RuntimeSystem("interp.actor_type"),
                    ),
                    name: actor.name.clone(),
                    pub_: actor.pub_,
                    kind: TypeDefKind::Record(
                        actor
                            .fields
                            .iter()
                            .map(|f| Field {
                                meta: f.meta,
                                name: f.name.clone(),
                                ty: f.ty.clone(),
                            })
                            .collect(),
                    ),
                    generics: Vec::new(),
                    derives: Vec::new(),
                    attributes: Vec::new(),
                };
                out.insert(actor.name.clone(), actor_type_def);
            }
            Item::Module(m) => {
                for inner in &m.items {
                    Self::collect_type_defs(inner, out);
                }
            }
            _ => {}
        }
    }

    /// Expand built-in derive macros for types
    fn expand_derives(
        type_defs: &HashMap<String, TypeDef>,
        _trait_defs: &mut HashMap<String, TraitDef>,
        type_impls: &mut HashMap<String, HashMap<String, Vec<FuncDef>>>,
    ) -> InterpResult<()> {
        // Validate the complete input before mutating the implementation table,
        // so an unsupported derive cannot leave a partially expanded program.
        for (type_name, type_def) in type_defs {
            if let Some(derive_name) = type_def
                .derives
                .iter()
                .find(|name| !matches!(name.as_str(), "Debug" | "Clone" | "Eq"))
            {
                return Err(InterpError::with_op(
                    format!(
                        "unsupported derive `{}` on type `{}`; supported derives: Debug, Clone, Eq",
                        derive_name, type_name
                    ),
                    "derive expansion",
                ));
            }
        }

        for (type_name, type_def) in type_defs {
            for derive_name in &type_def.derives {
                let derive_meta = AstNodeMeta::inherited(
                    type_def.meta.span,
                    AstOrigin::Desugared("interp.derive_method"),
                );
                match derive_name.as_str() {
                    "Debug" => {
                        // Generate to_string method for Debug
                        let to_string_func = FuncDef {
                            meta: derive_meta,
                            name: "to_string".to_string(),
                            pub_: false,
                            params: vec![],
                            ret: Some(
                                Type::Name("string".into(), vec![]).deep_reorigin(derive_meta),
                            ),
                            body: vec![],
                            where_clause: Vec::new(),
                            generics: vec![],
                            effects: vec![],
                            is_comptime: false,
                            is_async: false,
                            extern_abi: None,
                        };
                        type_impls
                            .entry(type_name.clone())
                            .or_default()
                            .entry("Debug".to_string())
                            .or_default()
                            .push(to_string_func);
                    }
                    "Clone" => {
                        // Generate clone method for Clone
                        let clone_func = FuncDef {
                            meta: derive_meta,
                            name: "clone".to_string(),
                            pub_: false,
                            params: vec![],
                            ret: Some(
                                Type::Name(type_name.clone(), vec![]).deep_reorigin(derive_meta),
                            ),
                            body: vec![],
                            where_clause: Vec::new(),
                            generics: vec![],
                            effects: vec![],
                            is_comptime: false,
                            is_async: false,
                            extern_abi: None,
                        };
                        type_impls
                            .entry(type_name.clone())
                            .or_default()
                            .entry("Clone".to_string())
                            .or_default()
                            .push(clone_func);
                    }
                    "Eq" => {
                        // Generate eq method for Eq
                        let eq_func = FuncDef {
                            meta: derive_meta,
                            name: "eq".to_string(),
                            pub_: false,
                            params: vec![Param {
                                meta: derive_meta,
                                name: "other".to_string(),
                                ty: Type::Name(type_name.clone(), vec![])
                                    .deep_reorigin(derive_meta),
                                mut_: false,
                                default_value: None,
                                borrow: None,
                            }],
                            ret: Some(Type::Name("bool".into(), vec![]).deep_reorigin(derive_meta)),
                            body: vec![],
                            where_clause: Vec::new(),
                            generics: vec![],
                            effects: vec![],
                            is_comptime: false,
                            is_async: false,
                            extern_abi: None,
                        };
                        type_impls
                            .entry(type_name.clone())
                            .or_default()
                            .entry("Eq".to_string())
                            .or_default()
                            .push(eq_func);
                    }
                    _ => unreachable!("derive names were validated before expansion"),
                }
            }
        }
        Ok(())
    }

    fn collect_traits(
        item: &Item,
        trait_defs: &mut HashMap<String, TraitDef>,
        type_impls: &mut HashMap<String, HashMap<String, Vec<FuncDef>>>,
    ) {
        match item {
            Item::Trait(trait_def) => {
                trait_defs.insert(trait_def.name.clone(), trait_def.clone());
            }
            Item::Impl(impl_def) => {
                type_impls
                    .entry(impl_def.type_name.clone())
                    .or_default()
                    .insert(impl_def.trait_name.clone(), impl_def.methods.clone());
            }
            Item::Module(m) => {
                for inner in &m.items {
                    Self::collect_traits(inner, trait_defs, type_impls);
                }
            }
            _ => {}
        }
    }

    fn collect_caps(item: &Item, out: &mut HashMap<String, Vec<String>>) {
        match item {
            Item::Cap(cap) => {
                let components = if let Some(ref combined) = cap.combined_with {
                    // Parse "A + B" format
                    let parts: Vec<String> = combined
                        .split(" + ")
                        .map(|s| s.trim().to_string())
                        .collect();
                    if parts.len() > 1 {
                        parts
                    } else {
                        vec![cap.name.clone(), combined.clone()]
                    }
                } else {
                    vec![cap.name.clone()]
                };
                out.insert(cap.name.clone(), components);
            }
            Item::Module(m) => {
                for inner in &m.items {
                    Self::collect_caps(inner, out);
                }
            }
            _ => {}
        }
    }

    /// Get the type name of a runtime value
    fn value_type_name(&self, val: &Value) -> String {
        match val {
            Value::Int(_) => "i32".into(),
            Value::Float(_) => "f64".into(),
            Value::Bool(_) => "bool".into(),
            Value::String(_) => "string".into(),
            Value::Unit => "unit".into(),
            Value::List(_) => "list".into(),
            Value::Set(_) => "set".into(),
            Value::Array(_) => "array".into(),
            Value::Tuple(_) => "tuple".into(),
            Value::Variant(name, _) => name.clone(),
            Value::Record(Some(name), _) => name.clone(),
            Value::Record(None, _) => "record".into(),
            Value::Error(_) => "error".into(),
            Value::Newtype(name, _v) => name.clone(),
            Value::Type(name) => name.clone(),
            Value::Closure { .. } => "closure".into(),
            Value::QuoteAst(_) => "AST".into(),
            Value::Shared(_) => "shared".into(),
            Value::LocalShared(_) => "local_shared".into(),
            Value::Ref(_) => "ref".into(),
            Value::RefMut(_) => "ref_mut".into(),
            Value::IndexRef { .. } => "borrowed_index".into(),
            Value::IndexRefMut { .. } => "borrowed_index_mut".into(),
            Value::PlaceRef { .. } => "borrowed_place".into(),
            Value::PlaceRefMut { .. } => "borrowed_place_mut".into(),
            Value::Cap(_) => "cap".into(),
            Value::Actor(_) => "actor".into(),
            Value::Future(_) => "future".into(),
            Value::ArenaRef(_, _, _) => "arena_ref".into(),
            Value::ArenaBlock(_) => "arena_block".into(),
            Value::WeakShared(_) | Value::WeakLocal(_) => "weak".into(),
            Value::Allocator(_) => "Allocator".into(),
            Value::Slice { .. } => "slice".into(),
            Value::Range { .. } => "range".into(),
            Value::CBuffer(_) => "CBuffer".into(),
            Value::DynTrait { trait_names, .. } => format!("dyn {}", trait_names.join(" + ")),
        }
    }

    /// Resolve a Type AST node to a type name string
    fn resolve_type_name(&self, ty: &Type) -> String {
        match ty {
            Type::Located { ty, .. } => self.resolve_type_name(ty),
            Type::Name(name, _) => name.clone(),
            Type::Ref(lt, inner) => {
                if let Some(l) = lt {
                    format!("&'{} {}", l, self.resolve_type_name(inner))
                } else {
                    format!("&{}", self.resolve_type_name(inner))
                }
            }
            Type::RefMut(lt, inner) => {
                if let Some(l) = lt {
                    format!("&'{} mut {}", l, self.resolve_type_name(inner))
                } else {
                    format!("&mut {}", self.resolve_type_name(inner))
                }
            }
            Type::Option(inner) => format!("Option<{}>", self.resolve_type_name(inner)),
            Type::Result(ok, err) => format!(
                "Result<{}, {}>",
                self.resolve_type_name(ok),
                self.resolve_type_name(err)
            ),
            Type::Tuple(elems) => {
                let names: Vec<String> = elems.iter().map(|e| self.resolve_type_name(e)).collect();
                format!("({})", names.join(", "))
            }
            Type::Func(args, ret) => {
                let arg_names: Vec<String> =
                    args.iter().map(|a| self.resolve_type_name(a)).collect();
                format!(
                    "({}) -> {}",
                    arg_names.join(", "),
                    self.resolve_type_name(ret)
                )
            }
            Type::Cap(name) => format!("cap {}", name),
            Type::Shared(inner) => format!("shared {}", self.resolve_type_name(inner)),
            Type::LocalShared(inner) => format!("local_shared {}", self.resolve_type_name(inner)),
            Type::Weak(inner) => format!("weak {}", self.resolve_type_name(inner)),
            Type::WeakLocal(inner) => format!("weak_local {}", self.resolve_type_name(inner)),
            Type::RawPtr(inner) => format!("*{}", self.resolve_type_name(inner)),
            Type::RawPtrMut(inner) => format!("*mut {}", self.resolve_type_name(inner)),
            Type::CShared(inner) => format!("c_shared {}", self.resolve_type_name(inner)),
            Type::CBorrow(inner) => format!("c_borrow {}", self.resolve_type_name(inner)),
            Type::CBorrowMut(inner) => format!("c_borrow_mut {}", self.resolve_type_name(inner)),
            Type::RawString => "raw_string".into(),
            Type::Infer => "_".into(),
            Type::ExternFunc(args, ret) => {
                let args_str: Vec<String> =
                    args.iter().map(|a| self.resolve_type_name(a)).collect();
                format!(
                    "extern \"C\" fn({}) -> {}",
                    args_str.join(", "),
                    self.resolve_type_name(ret)
                )
            }
            Type::Newtype(name, _) => name.clone(),
            Type::Nothing => "nothing".into(),
            Type::Allocator => "Allocator".into(),
            Type::Array(inner, size) => format!("[{}; {}]", self.resolve_type_name(inner), size),
            Type::Slice(inner) => format!("[{}]", self.resolve_type_name(inner)),
            Type::ImplTrait(traits) => format!("impl {}", traits.join(" + ")),
            Type::DynTrait(traits) => format!("dyn {}", traits.join(" + ")),
            Type::CBuffer(inner) => format!("CBuffer<{}>", self.resolve_type_name(inner)),
            Type::TypeVar(id) => format!("?T{}", id),
            Type::ForAll(params, body) => {
                format!(
                    "forall {}. {}",
                    params.join(", "),
                    self.resolve_type_name(body)
                )
            }
        }
    }

    /// Get type info for a type name
    fn type_info_for(&self, type_name: &str) -> Result<Value, InterpError> {
        if let Some(type_def) = self.type_defs.get(type_name) {
            let mut fields_map = HashMap::new();
            match &type_def.kind {
                TypeDefKind::Record(fields) => {
                    for f in fields {
                        let field_info = vec![
                            (Value::String("name".into()), Value::String(f.name.clone())),
                            (
                                Value::String("type".into()),
                                Value::String(self.resolve_type_name(&f.ty)),
                            ),
                        ];
                        fields_map.insert(
                            f.name.clone(),
                            Value::Tuple(field_info.into_iter().map(|(_, v)| v).collect()),
                        );
                    }
                }
                TypeDefKind::Enum(variants) => {
                    for v in variants {
                        let variant_info = vec![
                            Value::String(v.name.clone()),
                            Value::Bool(v.payload.is_some()),
                        ];
                        fields_map.insert(v.name.clone(), Value::Tuple(variant_info));
                    }
                }
                TypeDefKind::Alias(ty) => {
                    fields_map.insert("alias_of".into(), Value::String(self.resolve_type_name(ty)));
                }
                TypeDefKind::Newtype(ty) => {
                    fields_map.insert("inner".into(), Value::String(self.resolve_type_name(ty)));
                }
                TypeDefKind::Union(fields) => {
                    for f in fields {
                        let field_info = vec![
                            (Value::String("name".into()), Value::String(f.name.clone())),
                            (
                                Value::String("type".into()),
                                Value::String(self.resolve_type_name(&f.ty)),
                            ),
                        ];
                        fields_map.insert(
                            f.name.clone(),
                            Value::Tuple(field_info.into_iter().map(|(_, v)| v).collect()),
                        );
                    }
                }
            }
            let mut info = HashMap::new();
            info.insert("name".into(), Value::String(type_name.into()));
            info.insert(
                "fields".into(),
                Value::List(fields_map.into_values().collect()),
            );
            Ok(Value::Record(None, info))
        } else {
            Err(InterpError::new(format!("unknown type '{}'", type_name)))
        }
    }

    pub fn run(&mut self) -> Result<Value, InterpError> {
        if let Some(error) = &self.derive_expansion_error {
            return Err(error.clone());
        }
        self.eval_comptime_funcs()?;
        self.eval_consts()?;
        let main = self
            .find_function("main")
            .ok_or_else(|| InterpError::new("no main() function found"))?;
        self.call_func(&main, vec![])
    }

    /// Evaluate a `comptime { ... }` block as a standalone expression.
    /// Runs `eval_comptime_funcs` first so the block can call top-level
    /// `comptime func` results; this is the canonical fold-time entry
    /// used by codegen when it encounters an `Expr::Comptime` node.
    pub fn eval_comptime_block(&mut self, block: &crate::ast::Block) -> Result<Value, InterpError> {
        if let Some(error) = &self.derive_expansion_error {
            return Err(error.clone());
        }
        self.eval_comptime_funcs()?;
        self.eval_comptime(block)
    }

    /// Move all cached `comptime func` results out of the interpreter.
    /// Used by codegen to seed its `comptime_values` cache after
    /// `eval_comptime_block` has been called once for bootstrap.
    pub fn drain_comptime_results(&mut self) -> std::collections::HashMap<String, Value> {
        std::mem::take(&mut self.comptime_results)
    }

    /// v0.28.21 — Insert a single pre-folded `comptime func` result so
    /// the next `eval_comptime_block` can see it without re-evaluating
    /// the function. Used by codegen to seed a fresh interpreter with
    /// results it already has.
    pub fn inject_comptime_result(&mut self, name: String, value: Value) {
        self.comptime_results.insert(name, value);
    }

    /// Evaluate comptime functions with no arguments at startup
    fn eval_comptime_funcs(&mut self) -> Result<(), InterpError> {
        let funcs: Vec<FuncDef> = self
            .file
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Func(f) if f.is_comptime && f.params.is_empty() => Some(f.clone()),
                _ => None,
            })
            .collect();
        for func in funcs {
            // I-H9: skip if already cached (e.g. was called as a dependency
            // from another comptime func's body during its pre-evaluation).
            if self.comptime_results.contains_key(&func.name) {
                continue;
            }
            let result = self.call_func(&func, vec![])?;
            self.comptime_results.insert(func.name.clone(), result);
        }
        Ok(())
    }

    /// Evaluate top-level const declarations at startup
    fn eval_consts(&mut self) -> Result<(), InterpError> {
        let consts: Vec<(String, Expr)> = self
            .file
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Const { name, value, .. } => Some((name.clone(), value.clone())),
                _ => None,
            })
            .collect();
        for (name, expr) in consts {
            let val = self.eval_expr(&expr)?;
            self.globals.insert(name, val);
        }
        Ok(())
    }

    fn find_function(&self, name: &str) -> Option<FuncDef> {
        // O(1) lookup via pre-built index — try both qualified and unqualified
        self.func_index
            .get(name)
            .cloned()
            .or_else(|| self.func_index.values().find(|f| f.name == name).cloned())
    }

    /// Build a qualified path from nested Field(Ident(...), ...) expressions
    fn build_qualified_path(obj: &Expr, field: &str) -> Option<String> {
        match obj.unlocated() {
            Expr::Ident(name) => Some(format!("{}::{}", name, field)),
            Expr::Field(inner_obj, inner_field) => {
                Self::build_qualified_path(inner_obj, inner_field)
                    .map(|base| format!("{}::{}", base, field))
            }
            _ => None,
        }
    }

    fn find_function_in_module(module: &ModuleDef, prefix: &str, name: &str) -> Option<FuncDef> {
        let current_prefix = if prefix.is_empty() {
            module.name.clone()
        } else {
            format!("{}::{}", prefix, module.name)
        };
        for inner in &module.items {
            match inner {
                Item::Func(f) => {
                    let qualified = format!("{}::{}", current_prefix, f.name);
                    if qualified == name || f.name == name {
                        return Some(f.clone());
                    }
                }
                Item::Module(m) => {
                    if let Some(f) = Self::find_function_in_module(m, &current_prefix, name) {
                        return Some(f);
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn find_actor(&self, name: &str) -> Option<ActorDef> {
        // O(1) lookup via pre-built index
        self.actor_index.get(name).cloned()
    }

    pub(in crate::interp) fn find_flow(&self, name: &str) -> Option<FlowDef> {
        self.flow_index.get(name).cloned()
    }

    fn push_scope(&mut self) {
        self.scope_env.push_scope();
    }

    fn pop_scope(&mut self) {
        self.scope_env.pop_scope();
    }

    /// Run a closure inside a freshly pushed scope, guaranteeing that the
    /// scope is popped even if the closure returns an error.
    fn with_scope<F, T>(&mut self, f: F) -> Result<T, InterpError>
    where
        F: FnOnce(&mut Self) -> Result<T, InterpError>,
    {
        self.scope_env.push_scope();
        let result = f(self);
        self.scope_env.pop_scope();
        result
    }

    fn push_call(&mut self, func_name: &str) {
        self.scope_env.push_call(func_name);
    }

    fn pop_call(&mut self) {
        self.scope_env.pop_call();
    }

    /// Convert a string error into an InterpError with current call stack context.
    fn interp_err(&self, msg: String) -> InterpError {
        self.scope_env.interp_err(msg)
    }

    /// Convert a string error with operation context into an InterpError.
    fn interp_err_op(&self, msg: String, op: &str) -> InterpError {
        self.scope_env.interp_err_op(msg, op)
    }

    fn bind(&mut self, name: &str, value: Value) -> Result<(), InterpError> {
        self.scope_env.bind(name, value)
    }

    fn bind_mut(&mut self, name: &str, value: Value) -> Result<(), InterpError> {
        self.scope_env.bind_mut(name, value)
    }

    fn lookup(&self, name: &str) -> Option<Value> {
        let v = self.scope_env.lookup(name);
        if v.is_some() {
            return v;
        }
        // Check globals
        self.globals.get(name).cloned()
    }

    fn is_mutable(&self, name: &str) -> bool {
        self.scope_env.is_mutable(name)
    }

    fn is_moved(&self, name: &str) -> bool {
        self.scope_env.is_moved(name)
    }

    fn mark_moved(&mut self, name: &str) {
        self.scope_env.mark_moved(name)
    }

    fn assign(&mut self, name: &str, value: Value) -> Result<(), InterpError> {
        self.scope_env.assign(name, value)
    }

    /// Write `value` into a place expression (I-H7 nested record assignment).
    /// Supports `x`, `x.f`, `x.f.g`, and shared/local_shared record fields.
    pub(crate) fn write_place_value(
        &mut self,
        place: &Expr,
        value: Value,
    ) -> Result<(), InterpError> {
        match place.unlocated() {
            Expr::Ident(name) => self.assign(name, value),
            Expr::Field(obj, field) => {
                if let Expr::Ident(name) = obj.unlocated() {
                    if name == "self" {
                        if let Some(Value::Actor(handle)) = self.lookup("self") {
                            handle
                                .inner
                                .write()
                                .map_err(|e| {
                                    InterpError::lock_error(format!("actor lock failed: {}", e))
                                })?
                                .fields
                                .insert(field.clone(), value);
                            return Ok(());
                        }
                    }
                }
                let obj_val = self.eval_expr(obj)?;
                match obj_val {
                    Value::Record(type_name, mut fields) => {
                        if !fields.contains_key(field.as_str()) {
                            return Err(InterpError::field_not_found(format!(
                                "field '{}' not found in record",
                                field
                            )));
                        }
                        fields.insert(field.clone(), value);
                        self.write_place_value(obj, Value::Record(type_name, fields))
                    }
                    Value::Actor(handle) => {
                        handle
                            .inner
                            .write()
                            .map_err(|e| {
                                InterpError::lock_error(format!("actor lock failed: {}", e))
                            })?
                            .fields
                            .insert(field.clone(), value);
                        Ok(())
                    }
                    Value::Shared(arc) => {
                        let mut inner = arc.write().map_err(|e| {
                            InterpError::lock_error(format!("shared write lock failed: {}", e))
                        })?;
                        match &mut *inner {
                            Value::Record(_, fields) => {
                                if !fields.contains_key(field.as_str()) {
                                    return Err(InterpError::field_not_found(format!(
                                        "field '{}' not found in shared record",
                                        field
                                    )));
                                }
                                fields.insert(field.clone(), value);
                                Ok(())
                            }
                            _ => Err(InterpError::new(format!(
                                "cannot assign to field of non-record shared value (type: {})",
                                type_name(&inner)
                            ))),
                        }
                    }
                    Value::LocalShared(rc) => {
                        let mut inner = rc.lock().unwrap_or_else(|e| e.into_inner());
                        match &mut *inner {
                            Value::Record(_, fields) => {
                                if !fields.contains_key(field.as_str()) {
                                    return Err(InterpError::field_not_found(format!(
                                        "field '{}' not found in local_shared record",
                                        field
                                    )));
                                }
                                fields.insert(field.clone(), value);
                                Ok(())
                            }
                            _ => Err(InterpError::new(format!(
                                "cannot assign to field of non-record local_shared value (type: {})",
                                type_name(&inner)
                            ))),
                        }
                    }
                    other => Err(InterpError::new(format!(
                        "cannot assign to field of non-record/non-actor value (type: {})",
                        type_name(&other)
                    ))),
                }
            }
            _ => Err(InterpError::new(
                "assignment target is not a writable place",
            )),
        }
    }

    /// Force-update a variable's value, bypassing the mutability check.
    /// Used by `push()` write-back — push mutates in place in codegen
    /// regardless of `mut`, so the interpreter must match.
    fn force_update(&mut self, name: &str, value: Value) {
        self.scope_env.force_update(name, value);
    }

    /// Push a new compensation scope level
    fn push_compensation_scope(&mut self) {
        self.compensation_stack.push(Vec::new());
    }

    /// Pop the current compensation scope level
    /// If run_compensations is true, execute all compensations in LIFO order before popping
    fn pop_compensation_scope(&mut self, run_compensations: bool) {
        if run_compensations {
            // Run compensation blocks in LIFO order for the current scope
            if let Some(scope) = self.compensation_stack.pop() {
                // Execute compensations in reverse order (LIFO within this scope)
                // Note: compensation_stack order is already LIFO across scopes,
                // but within a scope we want to execute in registration order (first registered = last executed)
                for block in scope.iter().rev() {
                    for stmt in block {
                        if let Err(e) = self.eval_stmt(stmt) {
                            // M12: surface compensation failures (count + log).
                            self.compensation_error_count =
                                self.compensation_error_count.saturating_add(1);
                            eprintln!(
                                "compensation error #{}: {} (continuing remaining compensations)",
                                self.compensation_error_count, e
                            );
                        }
                    }
                }
            }
        } else {
            // Just discard the scope (normal exit)
            self.compensation_stack.pop();
        }
    }

    /// Run all compensation blocks across all scope levels in LIFO order
    /// Used when propagation an error up through nested scopes
    fn run_all_compensations(&mut self) {
        // Run all remaining compensations in LIFO order
        while let Some(scope) = self.compensation_stack.pop() {
            for block in scope.iter().rev() {
                for stmt in block {
                    if let Err(e) = self.eval_stmt(stmt) {
                        // M12: surface compensation failures (count + log).
                        self.compensation_error_count =
                            self.compensation_error_count.saturating_add(1);
                        eprintln!(
                            "compensation error #{}: {} (continuing remaining compensations)",
                            self.compensation_error_count, e
                        );
                    }
                }
            }
        }
    }

    /// Find the numeric discriminant (ordinal) of an enum variant by name.
    /// Returns 0-based index based on alphabetical ordering of variants.
    fn find_variant_ordinal(&self, variant_name: &str) -> usize {
        // Check built-in Result/Option variants
        match variant_name {
            "Ok" | "Some" => return 1,
            "Err" | "None" => return 0,
            _ => {}
        }
        // Scan type definitions for matching enum
        for td in self.type_defs.values() {
            if let crate::ast::TypeDefKind::Enum(variants) = &td.kind {
                let mut sorted: Vec<&crate::ast::Variant> = variants.iter().collect();
                sorted.sort_by_key(|v| &v.name);
                for (i, v) in sorted.iter().enumerate() {
                    if v.name == variant_name {
                        return i;
                    }
                }
            }
        }
        0
    }
}
fn encode_resolved_const_value(value: &crate::core::ResolvedConstValue) -> String {
    match value {
        crate::core::ResolvedConstValue::Int(v) => format!("int:{}", v),
        crate::core::ResolvedConstValue::Float(v) => format!("float:{}", v),
        crate::core::ResolvedConstValue::Bool(v) => format!("bool:{}", v),
        crate::core::ResolvedConstValue::String(v) => format!("string:{}", v),
        crate::core::ResolvedConstValue::Unit => "unit".into(),
        crate::core::ResolvedConstValue::Complex => "complex".into(),
    }
}

#[cfg(test)]
mod derive_validation_tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    #[test]
    fn programmatic_ast_with_unknown_derive_fails_before_execution() {
        let source = "type Packet { value: i32 }\nfunc main() -> i32 { 1 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let mut file = Parser::new(tokens).parse_file().expect("parse");
        let type_def = file
            .items
            .iter_mut()
            .find_map(|item| match item {
                Item::Type(type_def) if type_def.name == "Packet" => Some(type_def),
                _ => None,
            })
            .expect("Packet type");
        type_def.derives.push("Serialize".to_string());

        let mut interpreter = Interpreter::new(&file);
        let error = interpreter
            .run()
            .expect_err("unsupported derive must not be ignored");
        assert_eq!(error.code(), crate::diagnostic::codes::E0800);
        assert_eq!(error.ctx().operation.as_deref(), Some("derive expansion"));
        assert!(error.message().contains("unsupported derive `Serialize`"));
        assert!(error.message().contains("type `Packet`"));

        let mut direct = Interpreter::new(&file);
        let direct_error = direct
            .call_named("main", vec![])
            .expect_err("direct calls must also fail closed");
        assert_eq!(
            direct_error.ctx().operation.as_deref(),
            Some("derive expansion")
        );
    }
}
