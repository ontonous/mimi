use crate::ast::*;
use std::collections::HashMap;

use crate::error::{CompileError, MimiResult};
use crate::span::Span;

use super::CodeGenerator;
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{InitializationConfig, Target, TargetMachine};
use inkwell::OptimizationLevel;

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

impl<'ctx> CodeGenerator<'ctx> {
    pub fn compile_checked(
        &mut self,
        program: &crate::core::CheckedProgram<'_>,
    ) -> Result<(), Vec<crate::diagnostic::Diagnostic>> {
        program.validate_backend(crate::core::BackendProfile::Native)?;
        let mut resolved = std::collections::HashMap::new();
        let mut fallbacks = std::collections::HashSet::new();
        let mut pinned = std::collections::HashSet::new();
        let mut param_arity = std::collections::HashMap::new();
        let mut param_lists = std::collections::HashMap::new();
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
        self.resolved_transitions = Some(resolved);
        self.resolved_fallback_transitions = Some(fallbacks);
        self.resolved_ffi_pinned_transitions = Some(pinned);
        self.resolved_transition_param_arity = Some(param_arity);
        self.resolved_transition_params = Some(param_lists);

        let mut transitions_by_flow: std::collections::HashMap<
            String,
            Vec<(String, String, String, bool, bool, usize)>,
        > = std::collections::HashMap::new();
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
        let mut transitions_by_event: std::collections::HashMap<
            String,
            Vec<(String, String, String, bool, bool, usize)>,
        > = std::collections::HashMap::new();
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
        self.resolved_transitions_by_flow = Some(transitions_by_flow);
        self.resolved_transitions_by_event = Some(transitions_by_event);
        let mut arity = std::collections::HashMap::new();
        let mut effects = std::collections::HashMap::new();
        let mut returns = std::collections::HashMap::new();
        let mut params = std::collections::HashMap::new();
        let mut comptime_functions = std::collections::HashSet::new();
        for function in program.functions().values() {
            arity.insert(function.qualified_name.clone(), function.params.len());
            effects.insert(function.qualified_name.clone(), function.effects.clone());
            returns.insert(
                function.qualified_name.clone(),
                crate::core::fmt_type(&function.ret),
            );
            params.insert(
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
        self.resolved_function_arity = Some(arity);
        self.resolved_function_effects = Some(effects);
        self.resolved_function_returns = Some(returns);
        self.resolved_function_params = Some(params);
        self.resolved_comptime_functions = Some(comptime_functions);
        self.resolved_sessions = Some(
            program
                .sessions()
                .values()
                .map(|session| session.qualified_name.clone())
                .collect(),
        );
        let mut session_displays = std::collections::HashMap::new();
        for session in program.sessions().values() {
            session_displays.insert(session.qualified_name.clone(), session.body_display.clone());
        }
        self.resolved_session_displays = Some(session_displays);
        self.resolved_protocols = Some(
            program
                .protocols()
                .values()
                .map(|protocol| protocol.qualified_name.clone())
                .collect(),
        );
        let mut protocol_transitions = std::collections::HashMap::new();
        let mut protocol_payloads = std::collections::HashMap::new();
        let mut protocol_states = std::collections::HashMap::new();
        let mut protocol_state_payloads = std::collections::HashMap::new();
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
            let mut state_names = protocol.states.clone();
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
        self.resolved_protocol_transitions = Some(protocol_transitions);
        self.resolved_protocol_payloads = Some(protocol_payloads);
        self.resolved_protocol_states = Some(protocol_states);
        self.resolved_protocol_state_payloads = Some(protocol_state_payloads);
        let mut actors = std::collections::HashMap::new();
        for actor in program.actors().values() {
            actors.insert(actor.qualified_name.clone(), actor.methods.clone());
        }
        self.resolved_actors = Some(actors);
        self.resolved_capabilities = Some(
            program
                .capabilities()
                .values()
                .map(|capability| capability.qualified_name.clone())
                .collect(),
        );
        let mut capability_combined = std::collections::HashMap::new();
        for capability in program.capabilities().values() {
            if let Some(combined) = &capability.combined_with {
                capability_combined.insert(capability.qualified_name.clone(), combined.clone());
            }
        }
        self.resolved_capability_combined = Some(capability_combined);
        self.resolved_constants = Some(
            program
                .constants()
                .values()
                .map(|constant| constant.qualified_name.clone())
                .collect(),
        );
        let mut constant_values = std::collections::HashMap::new();
        for constant in program.constants().values() {
            constant_values.insert(
                constant.qualified_name.clone(),
                (
                    constant.ty.clone(),
                    encode_resolved_const_value(&constant.value),
                ),
            );
        }
        self.resolved_constant_values = Some(constant_values);
        let mut traits = std::collections::HashMap::new();
        for trait_def in program.traits().values() {
            traits.insert(trait_def.qualified_name.clone(), trait_def.methods.clone());
        }
        self.resolved_traits = Some(traits);
        let mut impls = std::collections::HashMap::new();
        for impl_def in program.impls().values() {
            impls.insert(impl_def.qualified_name.clone(), impl_def.methods.clone());
        }
        self.resolved_impls = Some(impls);
        self.resolved_ownership_owners = Some(
            program
                .ownership_ledgers()
                .keys()
                .map(|owner| owner.0.clone())
                .collect(),
        );
        let mut ownership_summaries = std::collections::HashMap::new();
        let mut ownership_resources = std::collections::HashMap::new();
        let mut ownership_actions = std::collections::HashMap::new();
        let mut ownership_merges = std::collections::HashMap::new();
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
                    .map(|action| {
                        let kind = match action.kind {
                            crate::core::ResourceActionKind::Introduce => "introduce",
                            crate::core::ResourceActionKind::Move => "move",
                            crate::core::ResourceActionKind::Drop => "drop",
                            crate::core::ResourceActionKind::Return => "return",
                        };
                        (kind.to_string(), action.resource.clone())
                    })
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
        self.resolved_ownership_summaries = Some(ownership_summaries);
        self.resolved_ownership_resources = Some(ownership_resources);
        self.resolved_ownership_actions = Some(ownership_actions);
        self.resolved_ownership_merges = Some(ownership_merges);

        self.resolved_backend_requirements = Some(
            program
                .backend_requirements()
                .iter()
                .map(|req| (req.capability.to_string(), req.flow.0.clone()))
                .collect(),
        );
        self.resolved_node_meta_count = Some(program.node_meta().len());
        self.resolved_node_meta_paths = Some(
            program
                .node_meta()
                .keys()
                .map(|node_id| node_id.0.clone())
                .collect(),
        );
        let mut node_meta_precision = std::collections::HashMap::new();
        for (node_id, meta) in program.node_meta() {
            let precision = match meta.precision {
                crate::core::SpanPrecision::Exact => "exact",
                crate::core::SpanPrecision::DeclarationFallback => "declaration_fallback",
            };
            node_meta_precision.insert(node_id.0.clone(), precision.to_string());
        }
        self.resolved_node_meta_precision = Some(node_meta_precision);
        let mut node_meta_spans = std::collections::HashMap::new();
        for (node_id, meta) in program.node_meta() {
            let span = meta.origin.user_span();
            node_meta_spans.insert(
                node_id.0.clone(),
                (span.start_line, span.start_col, span.end_line, span.end_col),
            );
        }
        self.resolved_node_meta_spans = Some(node_meta_spans);
        let mut type_kinds = std::collections::HashMap::new();
        let mut type_fields = std::collections::HashMap::new();
        let mut type_variants = std::collections::HashMap::new();
        let mut type_aliases = std::collections::HashMap::new();
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
        self.resolved_type_kinds = Some(type_kinds);
        self.resolved_type_fields = Some(type_fields);
        self.resolved_type_variants = Some(type_variants);
        self.resolved_type_aliases = Some(type_aliases);

        let mut extern_funcs = std::collections::HashSet::new();
        let mut extern_abis = std::collections::HashMap::new();
        for block in program.extern_blocks().values() {
            for func in &block.funcs {
                extern_funcs.insert(func.clone());
                extern_abis.insert(func.clone(), block.abi.clone());
            }
        }
        self.resolved_extern_funcs = Some(extern_funcs);
        self.resolved_extern_abis = Some(extern_abis);
        let mut extern_signatures = std::collections::HashMap::new();
        let mut extern_params = std::collections::HashMap::new();
        for block in program.extern_blocks().values() {
            for sig in &block.signatures {
                extern_signatures.insert(sig.name.clone(), (sig.params.len(), sig.ret.clone()));
                extern_params.insert(sig.name.clone(), sig.params.clone());
            }
        }
        self.resolved_extern_signatures = Some(extern_signatures);
        self.resolved_extern_params = Some(extern_params);
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
        self.resolved_extern_no_panic = Some(extern_no_panic);
        self.resolved_extern_unsafe = Some(extern_unsafe);
        let mut call_sites = std::collections::HashMap::new();
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
                        crate::core::ResolvedCallKind::Method => "method".into(),
                        crate::core::ResolvedCallKind::Unknown => "unknown".into(),
                    },
                ),
            );
        }
        self.resolved_call_sites = Some(call_sites);
        let mut call_sites_by_owner: std::collections::HashMap<
            String,
            Vec<(String, usize, String)>,
        > = std::collections::HashMap::new();
        if let Some(sites) = self.resolved_call_sites.as_ref() {
            for (_path, (owner, callee, argc, _expected, _effects, _ret, kind)) in sites {
                call_sites_by_owner.entry(owner.clone()).or_default().push((
                    callee.clone(),
                    *argc,
                    kind.clone(),
                ));
            }
        }
        self.resolved_call_sites_by_owner = Some(call_sites_by_owner);
        let mut call_sites_by_callee: std::collections::HashMap<
            String,
            Vec<(String, usize, String)>,
        > = std::collections::HashMap::new();
        if let Some(sites) = self.resolved_call_sites.as_ref() {
            for (_path, (owner, callee, argc, _expected, _effects, _ret, kind)) in sites {
                call_sites_by_callee
                    .entry(callee.clone())
                    .or_default()
                    .push((owner.clone(), *argc, kind.clone()));
            }
        }
        self.resolved_call_sites_by_callee = Some(call_sites_by_callee);
        let mut actor_method_signatures = std::collections::HashMap::new();
        let mut actor_method_params = std::collections::HashMap::new();
        let mut actor_method_effects = std::collections::HashMap::new();
        for actor in program.actors().values() {
            for method in &actor.method_signatures {
                let key = format!("{}.{}", actor.qualified_name, method.name);
                actor_method_signatures
                    .insert(key.clone(), (method.params.len(), method.ret.clone()));
                actor_method_params.insert(key.clone(), method.params.clone());
                actor_method_effects.insert(key, method.effects.clone());
            }
        }
        self.resolved_actor_method_signatures = Some(actor_method_signatures);
        self.resolved_actor_method_params = Some(actor_method_params);
        self.resolved_actor_method_effects = Some(actor_method_effects);
        let mut actor_fields = std::collections::HashMap::new();
        for actor in program.actors().values() {
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
        self.resolved_actor_fields = Some(actor_fields);
        let mut method_signatures = std::collections::HashMap::new();
        let mut method_params = std::collections::HashMap::new();
        let mut method_effects = std::collections::HashMap::new();
        for trait_def in program.traits().values() {
            for method in &trait_def.method_signatures {
                let key = format!("{}.{}", trait_def.qualified_name, method.name);
                method_signatures.insert(key.clone(), (method.params.len(), method.ret.clone()));
                method_params.insert(key.clone(), method.params.clone());
                method_effects.insert(key, method.effects.clone());
            }
        }
        for impl_def in program.impls().values() {
            for method in &impl_def.method_signatures {
                let key = format!("{}.{}", impl_def.qualified_name, method.name);
                method_signatures.insert(key.clone(), (method.params.len(), method.ret.clone()));
                method_params.insert(key.clone(), method.params.clone());
                method_effects.insert(key, method.effects.clone());
            }
        }
        self.resolved_method_signatures = Some(method_signatures);
        self.resolved_method_params = Some(method_params);
        self.resolved_method_effects = Some(method_effects);
        if let Some(max_children) = program.flows().values().find_map(|flow| flow.max_children) {
            self.max_children = Some(max_children);
        }
        let mut mailbox_depths = std::collections::HashMap::new();
        for flow in program.flows().values() {
            if let Some(depth) = flow.mailbox_depth {
                mailbox_depths.insert(flow.id.0.clone(), depth);
            }
        }
        self.resolved_mailbox_depths = Some(mailbox_depths);
        let mut flow_state_payloads = std::collections::HashMap::new();
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
        self.resolved_flow_state_payloads = Some(flow_state_payloads);
        let mut flow_states = std::collections::HashMap::new();
        for flow in program.flows().values() {
            let mut names: Vec<String> = flow.states.keys().cloned().collect();
            names.sort();
            flow_states.insert(flow.id.0.clone(), names);
        }
        self.resolved_flow_states = Some(flow_states);
        let mut flow_events = std::collections::HashMap::new();
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
        self.resolved_flow_events = Some(flow_events);
        let mut item_kinds = std::collections::HashMap::new();
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
        self.resolved_item_kinds = Some(item_kinds);
        let mut persistent_fields = std::collections::HashMap::new();
        for flow in program.flows().values() {
            if !flow.persistent_fields.is_empty() {
                persistent_fields.insert(flow.id.0.clone(), flow.persistent_fields.clone());
            }
        }
        self.resolved_persistent_fields = Some(persistent_fields);
        let mut transactional_fields = std::collections::HashMap::new();
        let mut metadata_shadow_fields = std::collections::HashMap::new();
        for flow in program.flows().values() {
            if !flow.transactional_fields.is_empty() {
                transactional_fields.insert(flow.id.0.clone(), flow.transactional_fields.clone());
            }
            if !flow.metadata_shadow_fields.is_empty() {
                metadata_shadow_fields
                    .insert(flow.id.0.clone(), flow.metadata_shadow_fields.clone());
            }
        }
        self.resolved_transactional_fields = Some(transactional_fields);
        self.resolved_metadata_shadow_fields = Some(metadata_shadow_fields);
        let mut flow_protocols = std::collections::HashMap::new();
        for flow in program.flows().values() {
            if !flow.impl_protocols.is_empty() {
                flow_protocols.insert(flow.id.0.clone(), flow.impl_protocols.clone());
            }
        }
        self.resolved_flow_protocols = Some(flow_protocols);
        self.compile_file(program.legacy_body_file())
            .map_err(|error| {
                let mut diagnostic = error.to_diagnostic();
                if diagnostic.span.start_line == 0 || diagnostic.span.start_col == 0 {
                    if let Some(span) = program.entry_span() {
                        diagnostic = diagnostic.with_span(span);
                    }
                }
                vec![diagnostic]
            })
    }

    pub(super) fn mangle_name(base: &str, type_map: &HashMap<String, crate::ast::Type>) -> String {
        if type_map.is_empty() {
            return base.to_string();
        }
        let mut parts: Vec<String> = type_map
            .iter()
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
        let generics: Vec<crate::ast::GenericParam> = self
            .type_map
            .keys()
            .map(|k| crate::ast::GenericParam {
                name: k.clone(),
                bounds: vec![],
            })
            .collect();
        crate::core::subst_type_params(ty, &generics, &self.type_map)
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

    pub(crate) fn compile_file(&mut self, file: &File) -> MimiResult<()> {
        // Register built-in Record types used by builtins
        self.register_builtin_record_types()?;

        // v0.28.21 — Hold an owned copy of the file so `Expr::Comptime`
        // block folds can construct a fresh interpreter later, after
        // the original `&File` borrow has ended. The clone is shallow
        // w.r.t. String interning but acceptable at this scope.
        self.comptime_file = Some(std::rc::Rc::new(crate::ast::File {
            imports: file.imports.clone(),
            items: file.items.clone(),
            implicit_single: false,
        }));

        // v0.28.21 — Evaluate top-level `comptime func` and `const` items via the
        // interpreter and cache the results so `Expr::Comptime` blocks and
        // `comptime func name()` calls can fold to constants at codegen time.
        self.fold_comptime_items(file)?;

        // First pass: collect type definitions, function definitions, and cap definitions
        Self::process_items(&file.items, &mut |item| {
            match item {
                Item::Type(t) => {
                    self.register_type_def(t)?;
                }
                Item::Actor(actor) => {
                    self.register_actor_def(actor)?;
                }
                Item::Func(f) => {
                    self.func_defs.insert(f.name.clone(), f.clone());
                    if f.is_comptime {
                        self.comptime_func_names.insert(f.name.clone());
                    }
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
                    if !imp.type_args.is_empty() {
                        self.impl_type_args
                            .entry(imp.type_name.clone())
                            .or_insert_with(|| imp.type_args.clone());
                    }
                }
                Item::Const { name, value, .. } => {
                    // Store const for later reference
                    self.const_values.insert(name.clone(), value.clone());
                }
                Item::Flow(f) => {
                    // Register flow state payload types so record construction
                    // (e.g. `Zero { count: 0 }`) works in function codegen.
                    let qualified = format!("flow::{}", f.name);
                    for s in &f.states {
                        let type_name = format!("{}::{}", qualified, s.name);
                        let fields = s.payload.clone().unwrap_or_default();
                        let td = TypeDef {
                            name: type_name.clone(),
                            decl_pos: None,
                            pub_: false,
                            kind: TypeDefKind::Record(fields),
                            generics: vec![],
                            derives: vec![],
                            attributes: vec![],
                        };
                        self.register_type_def(&td)?;
                        // Also register unqualified name (skip built-in names like "i32")
                        if !Self::is_builtin_type_name(&s.name)
                            && !self.type_defs.contains_key(&s.name)
                        {
                            let td = TypeDef {
                                name: s.name.clone(),
                                decl_pos: None,
                                pub_: false,
                                kind: TypeDefKind::Record(s.payload.clone().unwrap_or_default()),
                                generics: vec![],
                                derives: vec![],
                                attributes: vec![],
                            };
                            self.register_type_def(&td)?;
                        }
                    }
                    // Cache the flow definition for transition compilation.
                    self.flow_defs.insert(f.name.clone(), f.clone());
                    // v0.29.24: first @max_children(N) wins as process spawn quota.
                    if self.max_children.is_none() {
                        for a in &f.annotations {
                            if let crate::ast::FlowAnnotation::MaxChildren(n) = a {
                                self.max_children = Some(*n);
                                break;
                            }
                        }
                    }
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
                Item::Type(t) => {
                    self.register_type_def(t)?;
                }
                _ => {}
            }
            Ok(())
        })?;
        // v0.28.26 — Forward-declare all non-extern, non-async, non-comptime
        // user functions before any bodies are compiled. This lets functions
        // (including those in imported modules) call later-defined functions.
        // Iterate over file.items to keep declaration order deterministic and
        // match the order used for the rest of codegen.
        for item in &file.items {
            if let Item::Func(f) = item {
                if f.is_comptime || f.is_async || f.extern_abi.is_some() {
                    continue;
                }
                if matches!(f.ret, Some(Type::ImplTrait(_))) {
                    continue;
                }
                self.declare_func(f)?;
            }
        }
        // Forward-declare flow transitions so user functions can call them.
        {
            let flow_defs: Vec<FlowDef> = self.flow_defs.values().cloned().collect();
            for flow in &flow_defs {
                for t in &flow.transitions {
                    let func = Self::transition_to_func(flow, t);
                    self.func_defs.insert(func.name.clone(), func.clone());
                    self.declare_func(&func)?;
                }
            }
        }

        // Third pass: compile impl methods (needed before vtable construction)
        self.compile_impl_methods()?;
        // Fourth pass: compile vtables (needed before user function compilation)
        self.compile_vtables()?;
        // Fifth pass: compile user functions, actors, and flow transitions.
        // v0.28.21 — `comptime func` items are folded at codegen-start by
        // `fold_comptime_items` and intentionally NOT compiled to LLVM IR
        // (the caller resolves them via the cached `comptime_values` map,
        // so no runtime symbol is required for the function body).
        Self::process_items(&file.items, &mut |item| {
            match item {
                Item::Func(f) => {
                    if f.is_comptime {
                        // Skip — folded value lives in self.comptime_values.
                    } else {
                        self.compile_func(f).map_err(|e| e.at(Span::from(f.pos)))?;
                    }
                }
                Item::Actor(actor) => {
                    self.compile_actor(actor)?;
                }
                Item::Flow(f) => {
                    self.compile_flow(f)?;
                }
                _ => {}
            }
            Ok(())
        })?;
        // Warn about comptime functions that could not be compiled
        // (from external modules that were excluded)
        for item in &file.items {
            if let Item::Func(f) = item {
                if f.is_comptime {
                    eprintln!("warning: comptime function '{}' was not compiled", f.name);
                }
            }
        }
        Ok(())
    }

    /// Check if a name is a built-in Mimi type (should not be registered as a flow state type).
    fn is_builtin_type_name(name: &str) -> bool {
        matches!(
            name,
            "i32"
                | "i64"
                | "f32"
                | "f64"
                | "bool"
                | "string"
                | "unit"
                | "char"
                | "Int"
                | "Float"
                | "Bool"
                | "String"
                | "List"
                | "Option"
                | "Result"
                | "Set"
                | "Map"
        )
    }

    /// Mangle a flow transition into an ordinary function name.
    /// Format: `{FlowName}__{transition}__from_{FromState}`
    pub(super) fn transition_fn_name(flow: &str, transition: &str, from: &str) -> String {
        format!("{}__{}__from_{}", flow, transition, from)
    }

    /// Convert a flow transition into a synthetic FuncDef for codegen.
    ///
    /// Parameters: `self` (from-state payload) + event params.
    /// Return type: the single declared target state's nominal LLVM layout.
    /// Multi-target transitions are rejected by `compile_flow` until codegen
    /// has a closed tagged-state-union ABI.
    /// Body: the transition body with outer `do { }` unwrapped (if present).
    pub(super) fn transition_to_func(flow: &FlowDef, t: &TransitionDef) -> FuncDef {
        let mut params = Vec::new();
        params.push(Param {
            name: "self".to_string(),
            ty: Type::Name(t.from_state.clone(), vec![]),
            mut_: false,
            default_value: None,
            borrow: None,
        });
        params.extend(t.params.iter().cloned());

        // H2: recover bodies already keep persistent shadows when
        // `flow.persistent_fields` is non-empty (inject_system_verbs keep=true).
        let _ = &flow.persistent_fields;
        let ret_name = t
            .to_states
            .first()
            .cloned()
            .unwrap_or_else(|| "unit".to_string());

        // Unwrap a single outer `do { ... }` so compile_block sees normal stmts.
        let body: Block = match &t.body {
            Some(block) => {
                if block.len() == 1 {
                    if let Stmt::Do(inner) = &block[0] {
                        inner.clone()
                    } else {
                        block.clone()
                    }
                } else {
                    // Multiple top-level stmts: unwrap each Do, keep rest.
                    let mut out = Vec::new();
                    for stmt in block {
                        match stmt {
                            Stmt::Do(inner) => out.extend(inner.iter().cloned()),
                            other => out.push(other.clone()),
                        }
                    }
                    out
                }
            }
            None => Vec::new(),
        };

        FuncDef {
            name: Self::transition_fn_name(&flow.name, &t.name, &t.from_state),
            pub_: false,
            params,
            ret: Some(Type::Name(ret_name, vec![])),
            body,
            where_clause: vec![],
            generics: vec![],
            effects: vec![],
            is_comptime: false,
            is_async: false,
            extern_abi: None,
            pos: t.pos,
        }
    }

    /// Compile all transitions of a flow as ordinary LLVM functions.
    pub(super) fn compile_flow(&mut self, flow: &FlowDef) -> MimiResult<()> {
        if !flow.transactional_fields.is_empty() {
            return Err(CompileError::Unsupported(format!(
                "transactional recovery for flow '{}' requires native WAL codegen",
                flow.name
            )));
        }
        for t in &flow.transitions {
            if t.body.is_none() {
                continue; // abstract / protocol-style transition — no body
            }
            if t.to_states.len() != 1 {
                return Err(CompileError::Unsupported(format!(
                    "multi-target transition '{}::{}({})' requires a tagged-state-union ABI",
                    flow.name, t.name, t.from_state
                ))
                .at(Span::from(t.pos)));
            }
            let func = Self::transition_to_func(flow, t);
            self.compile_func(&func)
                .map_err(|e| e.at(Span::from(t.pos)))?;
        }
        Ok(())
    }

    /// Register built-in Record types used by builtin functions (exec, file_stat, etc.)
    /// so that field access and struct construction work in codegen.
    fn register_builtin_record_types(&mut self) -> MimiResult<()> {
        use inkwell::types::BasicTypeEnum;
        let i32_ty = BasicTypeEnum::IntType(self.context.i32_type());
        let i64_ty = BasicTypeEnum::IntType(self.context.i64_type());
        let bool_ty = BasicTypeEnum::IntType(self.context.bool_type());
        let string_ty = {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            BasicTypeEnum::StructType(
                self.context
                    .struct_type(&[BasicTypeEnum::PointerType(i8_ptr), i64_ty], false),
            )
        };
        // ExecResult { exit_code: i32, stdout: string, stderr: string }
        if !self.type_defs.contains_key("ExecResult") {
            let exec_ty = crate::ast::TypeDef {
                name: "ExecResult".to_string(),
                decl_pos: None,
                pub_: false,
                kind: crate::ast::TypeDefKind::Record(vec![
                    crate::ast::Field {
                        name: "exit_code".to_string(),
                        ty: crate::ast::Type::Name("i32".to_string(), vec![]),
                    },
                    crate::ast::Field {
                        name: "stdout".to_string(),
                        ty: crate::ast::Type::Name("string".to_string(), vec![]),
                    },
                    crate::ast::Field {
                        name: "stderr".to_string(),
                        ty: crate::ast::Type::Name("string".to_string(), vec![]),
                    },
                ]),
                generics: vec![],
                derives: vec![],
                attributes: vec![],
            };
            let llvm_ty = BasicTypeEnum::StructType(
                self.context
                    .struct_type(&[i32_ty, string_ty, string_ty], false),
            );
            self.type_llvm.insert("ExecResult".to_string(), llvm_ty);
            self.type_defs.insert("ExecResult".to_string(), exec_ty);
        }
        // StatResult { size: i64, modified: i64, is_file: bool, is_dir: bool }
        if !self.type_defs.contains_key("StatResult") {
            let stat_ty = crate::ast::TypeDef {
                name: "StatResult".to_string(),
                decl_pos: None,
                pub_: false,
                kind: crate::ast::TypeDefKind::Record(vec![
                    crate::ast::Field {
                        name: "size".to_string(),
                        ty: crate::ast::Type::Name("i64".to_string(), vec![]),
                    },
                    crate::ast::Field {
                        name: "modified".to_string(),
                        ty: crate::ast::Type::Name("i64".to_string(), vec![]),
                    },
                    crate::ast::Field {
                        name: "is_file".to_string(),
                        ty: crate::ast::Type::Name("bool".to_string(), vec![]),
                    },
                    crate::ast::Field {
                        name: "is_dir".to_string(),
                        ty: crate::ast::Type::Name("bool".to_string(), vec![]),
                    },
                ]),
                generics: vec![],
                derives: vec![],
                attributes: vec![],
            };
            let llvm_ty = BasicTypeEnum::StructType(
                self.context
                    .struct_type(&[i64_ty, i64_ty, bool_ty, bool_ty], false),
            );
            self.type_llvm.insert("StatResult".to_string(), llvm_ty);
            self.type_defs.insert("StatResult".to_string(), stat_ty);
        }
        // v0.29.20 PeerFault { peer_id, reason }
        if !self.type_defs.contains_key("PeerFault") {
            let pf_ty = crate::ast::TypeDef {
                name: "PeerFault".to_string(),
                decl_pos: None,
                pub_: false,
                kind: crate::ast::TypeDefKind::Record(vec![
                    crate::ast::Field {
                        name: "peer_id".to_string(),
                        ty: crate::ast::Type::Name("string".to_string(), vec![]),
                    },
                    crate::ast::Field {
                        name: "reason".to_string(),
                        ty: crate::ast::Type::Name("string".to_string(), vec![]),
                    },
                ]),
                generics: vec![],
                derives: vec![],
                attributes: vec![],
            };
            let llvm_ty =
                BasicTypeEnum::StructType(self.context.struct_type(&[string_ty, string_ty], false));
            self.type_llvm.insert("PeerFault".to_string(), llvm_ty);
            self.type_defs.insert("PeerFault".to_string(), pf_ty);
        }
        // v0.29.12 SystemTrace { last_state_name, unexpected_event, snapshot, memory_dump, panic_payload }
        // v0.29.39: added memory_dump + panic_payload structured sub-records
        if !self.type_defs.contains_key("SystemTrace") {
            let st_ty = crate::ast::TypeDef {
                name: "SystemTrace".to_string(),
                decl_pos: None,
                pub_: false,
                kind: crate::ast::TypeDefKind::Record(vec![
                    crate::ast::Field {
                        name: "last_state_name".to_string(),
                        ty: crate::ast::Type::Name("string".to_string(), vec![]),
                    },
                    crate::ast::Field {
                        name: "unexpected_event".to_string(),
                        ty: crate::ast::Type::Name("string".to_string(), vec![]),
                    },
                    crate::ast::Field {
                        name: "snapshot".to_string(),
                        ty: crate::ast::Type::Name("string".to_string(), vec![]),
                    },
                    crate::ast::Field {
                        name: "memory_dump".to_string(),
                        ty: crate::ast::Type::Name("MemoryDump".to_string(), vec![]),
                    },
                    crate::ast::Field {
                        name: "panic_payload".to_string(),
                        ty: crate::ast::Type::Name("PanicPayload".to_string(), vec![]),
                    },
                ]),
                generics: vec![],
                derives: vec![],
                attributes: vec![],
            };
            // SystemTrace LLVM struct: { string, string, string, MemoryDump, PanicPayload }
            let memory_dump_ty =
                self.type_llvm
                    .get("MemoryDump")
                    .copied()
                    .unwrap_or(BasicTypeEnum::StructType(
                        self.context.struct_type(&[string_ty, i32_ty], false),
                    ));
            let panic_payload_ty =
                self.type_llvm
                    .get("PanicPayload")
                    .copied()
                    .unwrap_or(BasicTypeEnum::StructType(
                        self.context
                            .struct_type(&[string_ty, string_ty, i32_ty, string_ty], false),
                    ));
            let llvm_ty = BasicTypeEnum::StructType(self.context.struct_type(
                &[
                    string_ty,
                    string_ty,
                    string_ty,
                    memory_dump_ty,
                    panic_payload_ty,
                ],
                false,
            ));
            self.type_llvm.insert("SystemTrace".to_string(), llvm_ty);
            self.type_defs.insert("SystemTrace".to_string(), st_ty);
        }
        // v0.29.39: PanicPayload { error_type: string, file: string, line: i32, stack: string }
        if !self.type_defs.contains_key("PanicPayload") {
            let pp_ty = crate::ast::TypeDef {
                name: "PanicPayload".to_string(),
                decl_pos: None,
                pub_: false,
                kind: crate::ast::TypeDefKind::Record(vec![
                    crate::ast::Field {
                        name: "error_type".to_string(),
                        ty: crate::ast::Type::Name("string".to_string(), vec![]),
                    },
                    crate::ast::Field {
                        name: "file".to_string(),
                        ty: crate::ast::Type::Name("string".to_string(), vec![]),
                    },
                    crate::ast::Field {
                        name: "line".to_string(),
                        ty: crate::ast::Type::Name("i32".to_string(), vec![]),
                    },
                    crate::ast::Field {
                        name: "stack".to_string(),
                        ty: crate::ast::Type::Name("string".to_string(), vec![]),
                    },
                ]),
                generics: vec![],
                derives: vec![],
                attributes: vec![],
            };
            let llvm_ty = BasicTypeEnum::StructType(
                self.context
                    .struct_type(&[string_ty, string_ty, i32_ty, string_ty], false),
            );
            self.type_llvm.insert("PanicPayload".to_string(), llvm_ty);
            self.type_defs.insert("PanicPayload".to_string(), pp_ty);
        }
        // v0.29.39: MemoryDump { fields: string, count: i32 }
        if !self.type_defs.contains_key("MemoryDump") {
            let md_ty = crate::ast::TypeDef {
                name: "MemoryDump".to_string(),
                decl_pos: None,
                pub_: false,
                kind: crate::ast::TypeDefKind::Record(vec![
                    crate::ast::Field {
                        name: "fields".to_string(),
                        ty: crate::ast::Type::Name("string".to_string(), vec![]),
                    },
                    crate::ast::Field {
                        name: "count".to_string(),
                        ty: crate::ast::Type::Name("i32".to_string(), vec![]),
                    },
                ]),
                generics: vec![],
                derives: vec![],
                attributes: vec![],
            };
            let llvm_ty =
                BasicTypeEnum::StructType(self.context.struct_type(&[string_ty, i32_ty], false));
            self.type_llvm.insert("MemoryDump".to_string(), llvm_ty);
            self.type_defs.insert("MemoryDump".to_string(), md_ty);
        }
        Ok(())
    }

    /// Run LLVM optimization passes on the module (O2).
    /// Called from compile_to_object during actual builds.
    pub fn optimize_module(&self) -> MimiResult<()> {
        if self.target_triple.is_some() {
            Target::initialize_all(&InitializationConfig::default());
        } else {
            Target::initialize_native(&InitializationConfig::default()).map_err(|e| {
                CompileError::LlvmError(format!("failed to initialize target: {}", e))
            })?;
        }
        let triple_str = self.target_triple.clone().unwrap_or_else(|| {
            TargetMachine::get_default_triple()
                .as_str()
                .to_string_lossy()
                .to_string()
        });
        let triple = inkwell::targets::TargetTriple::create(&triple_str);
        let target = Target::from_triple(&triple)
            .map_err(|e| CompileError::LlvmError(format!("failed to find target: {}", e)))?;
        let (cpu, features) = if self.target_triple.is_some() {
            (String::new(), String::new())
        } else {
            (
                TargetMachine::get_host_cpu_name().to_string(),
                TargetMachine::get_host_cpu_features().to_string(),
            )
        };
        let tm = target
            .create_target_machine(
                &triple,
                &cpu,
                &features,
                OptimizationLevel::Aggressive,
                inkwell::targets::RelocMode::Default,
                inkwell::targets::CodeModel::Default,
            )
            .ok_or_else(|| {
                CompileError::LlvmError("failed to create target machine".to_string())
            })?;
        let options = PassBuilderOptions::create();
        self.module
            .run_passes("default<O2>", &tm, options)
            .map_err(|e| CompileError::LlvmError(format!("optimization failed: {}", e)))
    }

    /// v0.28.21 — Walk top-level items and fold any `comptime func` or
    /// `const` declaration into `self.comptime_values` by running the
    /// interpreter. This is what allows `comptime { ... }` blocks and
    /// `comptime func name()` call sites in subsequent compilation to
    /// resolve to a constant value without re-evaluating the AST at
    /// codegen time.
    ///
    /// Errors from individual items are downgraded to `eprintln!`
    /// warnings so a single broken `comptime` declaration does not
    /// prevent the rest of the file from compiling. (This matches
    /// the v0.28.19 behaviour of warning-on-uncompilable-comptime.)
    fn fold_comptime_items(&mut self, _file: &File) -> MimiResult<()> {
        // Use the cloned file stored in self.comptime_file so the
        // interpreter can be created without re-borrowing the caller's
        // argument after `compile_file` has moved on.
        let file_ref = match &self.comptime_file {
            Some(rc) => rc.as_ref(),
            None => return Ok(()),
        };
        let mut interp = crate::interp::Interpreter::new(file_ref);
        // Drive the same `eval_comptime_funcs` step `Interpreter::run`
        // uses so we get a `comptime_results` map populated before any
        // user-level `Expr::Comptime` block is asked to fold.
        // H15-fix: comptime errors should be compile errors, not silent warnings.
        // Previously downgraded to eprintln warning, which could cause generated
        // code to use wrong constants. Now propagated as a CompileError.
        if let Err(e) = interp.eval_comptime_block(&Vec::new()) {
            return Err(CompileError::Generic(format!(
                "comptime evaluation error: {}",
                e
            )));
        }
        // Drain every pre-computed comptime result into the codegen cache.
        for (name, value) in interp.drain_comptime_results() {
            self.comptime_values.insert(name, value);
        }
        Ok(())
    }
}
