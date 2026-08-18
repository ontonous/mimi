#![allow(dead_code)]

pub mod bytecode;
pub mod error;
pub(crate) mod ffi;
pub(crate) mod ffi_runtime;
mod value;

pub use error::InterpError;
pub use value::*;

/// Alias for interpreter results.
pub type InterpResult<T> = std::result::Result<T, InterpError>;

use crate::ast::*;
use std::collections::HashMap;

/// CheckedProgram directory viewer.
///
/// Holds the resolved_* data installed by `from_checked()` and provides
/// read-only accessor methods.  Execution is handled exclusively by the
/// bytecode VM (`bytecode::BytecodeVM`).
pub struct Interpreter<'a> {
    file: &'a File,
    /// Whether to verify contracts at runtime (used by tests).
    pub verify_contracts: bool,
    /// FFI execution context (shared with the bytecode VM).
    pub(crate) ffi_runtime: ffi_runtime::FfiRuntime,
    /// v0.29.24: process-wide max children (None = unlimited).
    max_children: Option<usize>,

    // ── resolved_* directory fields ──────────────────────────────
    pub(in crate::interp) resolved_transitions:
        Option<HashMap<(String, String, String), Vec<String>>>,
    pub(in crate::interp) resolved_fallback_transitions:
        Option<std::collections::HashSet<(String, String, String)>>,
    pub(in crate::interp) resolved_ffi_pinned_transitions:
        Option<std::collections::HashSet<(String, String, String)>>,
    pub(in crate::interp) resolved_transition_param_arity:
        Option<HashMap<(String, String, String), usize>>,
    pub(in crate::interp) resolved_transition_params:
        Option<HashMap<(String, String, String), Vec<(String, String)>>>,
    pub(in crate::interp) resolved_transitions_by_flow:
        Option<HashMap<String, Vec<(String, String, String, bool, bool, usize)>>>,
    pub(in crate::interp) resolved_transitions_by_event:
        Option<HashMap<String, Vec<(String, String, String, bool, bool, usize)>>>,
    pub(in crate::interp) resolved_transition_tables:
        Option<std::sync::Arc<crate::core::TransitionTables>>,
    pub(in crate::interp) resolved_node_meta_spans:
        Option<HashMap<String, (usize, usize, usize, usize)>>,
    pub(in crate::interp) resolved_functions: Option<HashMap<String, (usize, String, Vec<String>)>>,
    pub(in crate::interp) resolved_function_params: Option<HashMap<String, Vec<(String, String)>>>,
    pub(in crate::interp) resolved_comptime_functions: Option<std::collections::HashSet<String>>,
    pub(in crate::interp) resolved_sessions: Option<HashMap<String, crate::ast::SessionType>>,
    pub(in crate::interp) resolved_session_displays: Option<HashMap<String, String>>,
    pub(in crate::interp) resolved_actors: Option<HashMap<String, Vec<String>>>,
    pub(in crate::interp) resolved_actor_method_signatures:
        Option<HashMap<String, (usize, String)>>,
    pub(in crate::interp) resolved_actor_method_params:
        Option<HashMap<String, Vec<(String, String)>>>,
    pub(in crate::interp) resolved_actor_fields:
        Option<HashMap<String, Vec<(String, String, bool)>>>,
    pub(in crate::interp) resolved_capabilities: Option<std::collections::HashSet<String>>,
    pub(in crate::interp) resolved_capability_combined: Option<HashMap<String, String>>,
    pub(in crate::interp) resolved_constants: Option<std::collections::HashSet<String>>,
    pub(in crate::interp) resolved_constant_values:
        Option<HashMap<String, (Option<String>, String)>>,
    pub(in crate::interp) resolved_traits: Option<HashMap<String, Vec<String>>>,
    pub(in crate::interp) resolved_method_signatures: Option<HashMap<String, (usize, String)>>,
    pub(in crate::interp) resolved_method_params: Option<HashMap<String, Vec<(String, String)>>>,
    pub(in crate::interp) resolved_impls: Option<HashMap<String, Vec<String>>>,
    pub(in crate::interp) resolved_ownership_owners: Option<std::collections::HashSet<String>>,
    pub(in crate::interp) resolved_ownership_summaries:
        Option<HashMap<String, (usize, usize, usize, usize, usize, bool)>>,
    pub(in crate::interp) resolved_ownership_resources: Option<HashMap<String, Vec<String>>>,
    pub(in crate::interp) resolved_ownership_actions:
        Option<HashMap<String, Vec<(String, String)>>>,
    pub(in crate::interp) resolved_ownership_merges:
        Option<HashMap<String, Vec<(String, String, String, String)>>>,
    pub(in crate::interp) resolved_backend_requirements: Option<Vec<(String, String)>>,
    pub(in crate::interp) resolved_node_meta_count: Option<usize>,
    pub(in crate::interp) resolved_node_meta_paths: Option<std::collections::HashSet<String>>,
    pub(in crate::interp) resolved_node_meta_precision: Option<HashMap<String, String>>,
    pub(in crate::interp) resolved_type_kinds: Option<HashMap<String, String>>,
    pub(in crate::interp) resolved_type_fields: Option<HashMap<String, Vec<(String, String)>>>,
    pub(in crate::interp) resolved_type_variants:
        Option<HashMap<String, Vec<(String, Option<String>)>>>,
    pub(in crate::interp) resolved_type_aliases: Option<HashMap<String, String>>,
    pub(in crate::interp) resolved_extern_funcs: Option<std::collections::HashSet<String>>,
    pub(in crate::interp) resolved_extern_abis: Option<HashMap<String, String>>,
    pub(in crate::interp) resolved_extern_signatures: Option<HashMap<String, (usize, String)>>,
    pub(in crate::interp) resolved_extern_params: Option<HashMap<String, Vec<(String, String)>>>,
    pub(in crate::interp) resolved_extern_no_panic: Option<std::collections::HashSet<String>>,
    pub(in crate::interp) resolved_extern_unsafe: Option<std::collections::HashSet<String>>,
    pub(in crate::interp) resolved_mailbox_depths: Option<HashMap<String, usize>>,
    pub(in crate::interp) resolved_flow_state_payloads:
        Option<HashMap<String, Vec<(String, String)>>>,
    pub(in crate::interp) resolved_flow_states: Option<HashMap<String, Vec<String>>>,
    pub(in crate::interp) resolved_flow_events: Option<HashMap<String, Vec<String>>>,
    pub(in crate::interp) resolved_item_kinds: Option<HashMap<String, String>>,
    pub(in crate::interp) resolved_persistent_fields: Option<HashMap<String, Vec<String>>>,
}

impl<'a> Interpreter<'a> {
    pub fn from_checked(program: &'a crate::core::CheckedProgram) -> Self {
        // C2 (permanent): the surface-AST interpreter is the reference execution
        // semantics for Flow, Actor, Session, and FFI programs. ResolvedInterpreter
        // covers pure value programs; raw_ast() provides the permanent remainder.
        let mut interp = Self::new(program.raw_ast());
        // AD-6: transition tables built once in CheckedProgram, shared by both backends.
        let tables = std::sync::Arc::new(program.build_transition_tables());
        interp.resolved_transitions = Some(tables.resolved.clone());
        interp.resolved_fallback_transitions = Some(tables.fallbacks.clone());
        interp.resolved_ffi_pinned_transitions = Some(tables.pinned.clone());
        interp.resolved_transition_param_arity = Some(tables.param_arity.clone());
        interp.resolved_transitions_by_flow = Some(tables.by_flow.clone());
        interp.resolved_transitions_by_event = Some(tables.by_event.clone());
        interp.resolved_transition_params = Some(tables.param_lists.clone());
        // P0-11: keep Arc for actor worker threads (Send+Sync).
        interp.resolved_transition_tables = Some(tables);
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
        // P1-27: install bare names for unique bare names so the arity guard
        // fires for module-nested functions (FuncDef.name is always bare).
        // Two-pass: count bare names first, only install unique ones.
        {
            let mut bare_counts: HashMap<&str, usize> = HashMap::new();
            for f in program.functions().values() {
                let bare = f
                    .qualified_name
                    .rsplit("::")
                    .next()
                    .unwrap_or(&f.qualified_name);
                *bare_counts.entry(bare).or_insert(0) += 1;
            }
            for f in program.functions().values() {
                let bare = f
                    .qualified_name
                    .rsplit("::")
                    .next()
                    .unwrap_or(&f.qualified_name);
                if bare != f.qualified_name && bare_counts.get(bare) == Some(&1) {
                    functions.entry(bare.to_string()).or_insert((
                        f.params.len(),
                        crate::core::fmt_type(&f.ret),
                        f.effects.clone(),
                    ));
                }
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
        let mut actors = HashMap::new();
        let mut actor_method_signatures = HashMap::new();
        let mut actor_method_params = HashMap::new();
        let mut actor_fields = HashMap::new();
        for actor in program.actors().values() {
            actors.insert(actor.qualified_name.clone(), actor.methods.clone());
            for method in &actor.method_signatures {
                let key = format!("{}.{}", actor.qualified_name, method.name);
                actor_method_signatures
                    .insert(key.clone(), (method.params.len(), method.ret.clone()));
                actor_method_params.insert(key.clone(), method.params.clone());
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
        for trait_def in program.traits().values() {
            traits.insert(trait_def.qualified_name.clone(), trait_def.methods.clone());
            for method in &trait_def.method_signatures {
                let key = format!("{}.{}", trait_def.qualified_name, method.name);
                method_signatures.insert(key.clone(), (method.params.len(), method.ret.clone()));
                method_params.insert(key.clone(), method.params.clone());
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
            }
        }
        interp.resolved_impls = Some(impls);
        interp.resolved_method_signatures = Some(method_signatures);
        interp.resolved_method_params = Some(method_params);
        interp.resolved_ownership_owners = Some(
            program
                .resource_analyses()
                .keys()
                .map(|owner| owner.0.clone())
                .collect(),
        );
        let mut ownership_summaries = HashMap::new();
        let mut ownership_resources = HashMap::new();
        let mut ownership_actions = HashMap::new();
        let mut ownership_merges = HashMap::new();
        for (owner, analysis) in program.resource_analyses() {
            let cfg = program.callable_cfg(owner);
            let merges = cfg
                .map(|cfg| analysis.branch_merges(cfg))
                .unwrap_or_default();
            ownership_summaries.insert(
                owner.0.clone(),
                (
                    analysis.action_count(crate::core::CanonicalActionKind::Introduce),
                    analysis.action_count(crate::core::CanonicalActionKind::Move),
                    analysis.action_count(crate::core::CanonicalActionKind::Drop),
                    analysis.action_count(crate::core::CanonicalActionKind::Return),
                    merges.len(),
                    merges
                        .iter()
                        .any(|m| m.merged_state == crate::core::Availability::MaybeConsumed),
                ),
            );
            ownership_resources.insert(owner.0.clone(), analysis.resources());
            ownership_actions.insert(
                owner.0.clone(),
                analysis
                    .actions
                    .iter()
                    .filter(|a| {
                        !matches!(
                            a.kind,
                            crate::core::CanonicalActionKind::Read
                                | crate::core::CanonicalActionKind::Write
                        )
                    })
                    .map(|action| (action.kind.as_str().to_string(), action.resource_display()))
                    .collect(),
            );
            ownership_merges.insert(
                owner.0.clone(),
                merges
                    .iter()
                    .map(|merge| {
                        let encode = |s: crate::core::Availability| match s {
                            crate::core::Availability::Available => "available",
                            crate::core::Availability::Consumed => "consumed",
                            crate::core::Availability::MaybeConsumed => "maybe_consumed",
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
        interp
    }

    /// Minimal constructor: initializes FFI runtime from the AST and sets
    /// all resolved_* directory fields to None.  `from_checked()` overwrites
    /// them from the CheckedProgram.
    pub(crate) fn new(file: &'a File) -> Self {
        let ffi_runtime = ffi_runtime::FfiRuntime::from_file(file);
        // v0.29.24: first `@max_children(N)` among flows sets process spawn quota.
        let max_children = file.items.iter().find_map(|item| {
            if let Item::Flow(flow) = item {
                flow.annotations.iter().find_map(|a| match &a.kind {
                    crate::ast::FlowAnnotationKind::MaxChildren(n) => Some(*n),
                    _ => None,
                })
            } else {
                None
            }
        });
        Self {
            file,
            verify_contracts: true,
            ffi_runtime,
            max_children,
            resolved_transitions: None,
            resolved_fallback_transitions: None,
            resolved_ffi_pinned_transitions: None,
            resolved_transition_param_arity: None,
            resolved_transition_params: None,
            resolved_transitions_by_flow: None,
            resolved_transitions_by_event: None,
            resolved_transition_tables: None,
            resolved_node_meta_spans: None,
            resolved_functions: None,
            resolved_function_params: None,
            resolved_comptime_functions: None,
            resolved_sessions: None,
            resolved_session_displays: None,
            resolved_actors: None,
            resolved_actor_method_signatures: None,
            resolved_actor_method_params: None,
            resolved_actor_fields: None,
            resolved_capabilities: None,
            resolved_capability_combined: None,
            resolved_constants: None,
            resolved_constant_values: None,
            resolved_traits: None,
            resolved_method_signatures: None,
            resolved_method_params: None,
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
        }
    }

    /// Enable/disable FFI contract verification (tests, fuzz harness).
    pub(crate) fn set_verify_ffi(&mut self, verify: bool) {
        self.ffi_runtime.verify_ffi = verify;
    }

    pub(crate) fn resolved_function_arity(&self, qualified_name: &str) -> Option<usize> {
        self.resolved_functions
            .as_ref()
            .and_then(|map| map.get(qualified_name).map(|(arity, _, _)| *arity))
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

    pub(crate) fn resolved_impl_methods(
        &self,
        trait_name: &str,
        type_name: &str,
    ) -> Option<Vec<String>> {
        let key = crate::core::resolved::impl_qualified_key(trait_name, &[], type_name);
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
