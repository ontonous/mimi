use crate::ast::*;
use std::collections::HashMap;

use crate::error::{CompileError, MimiResult};

use super::CodeGenerator;
use inkwell::module::Linkage;
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{InitializationConfig, Target, TargetMachine};
use inkwell::types::BasicTypeEnum;
use inkwell::values::BasicValueEnum;
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
        program: &crate::core::CheckedProgram,
    ) -> Result<(), Vec<crate::diagnostic::Diagnostic>> {
        program.validate_backend(crate::core::BackendProfile::Native)?;
        // S10: the exact S8 Flow island has already switched to the canonical
        // MIR native consumer. This compatibility entry point delegates that
        // island to the same validated MIR program as the CLI; it never
        // reaches the legacy AST body compiler for an admitted shape.
        if crate::core::mir::is_exact_s8_flow_transition(program) {
            let canonical = crate::core::mir::reference::MirProgram::from_checked_program(program)
                .map_err(|error| {
                    vec![crate::diagnostic::Diagnostic::error_code(
                        "MIR-LOWERING-001",
                        format!("canonical MIR build failed for the S8 Flow island: {error}"),
                        program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                    )]
                })?;
            return self.compile_mir_native(&canonical);
        }
        // S12/S15: the scalar collection and flat Copy-record production
        // islands have crossed the default route boundary.  This direct
        // native API is also an old production entry point, so an admitted
        // graph must not continue into the old AST body compiler merely
        // because a caller bypassed the CLI selector.  The helper performs
        // the same whole-program, all-consumer preflight as the selector and
        // returns only after the canonical native consumer is ready.  If
        // canonical lowering has not materialized one of these candidates,
        // this remains the compatibility path for unrelated legacy programs.
        if let Some(canonical) = self.try_compile_exact_migrated_mir_island(program)? {
            return self.compile_mir_native(&canonical);
        }
        // 0.40.1.3 (A3, `blind-spots-evaluation-2026-08-29.md` §1.3-3/4): fatal
        // gate — fail closed on native return types whose heap ownership the
        // current ownership-transfer path cannot handle. The legacy
        // `func.rs` `deep_copy_returned_value` / `type_owns_heap` path (BUG P)
        // silently passes through `Set<_>` / `Map<_,_>` returns, so the returned
        // handle aliases freed heap. This runs for EVERY function (user + stdlib)
        // before any emission, so it is enforced regardless of whether the
        // function is routed to the resolved or legacy emitter. `mimi run` (VM
        // backend) is unaffected — it does not call this native path.
        for function in program.functions().values() {
            if function.is_comptime {
                continue;
            }
            let owns_unclaimed_heap = program
                .callable(&function.node_id)
                .map(|callable| {
                    crate::codegen::resolved::native_return_owns_unclaimed_heap(
                        program,
                        &callable.signature.result,
                    )
                })
                .unwrap_or(false);
            if owns_unclaimed_heap {
                return Err(vec![crate::diagnostic::Diagnostic::error_code(
                    crate::diagnostic::codes::E0723,
                    format!(
                        "returning a value of type `{}` from a native (LLVM) function is not yet supported: its heap ownership (Set/Map, or a nested non-string `List` payload) cannot be transferred safely across the return boundary. Use `mimi run` (VM backend), or restructure to avoid returning this type. Tracked as 0.1.10 A2 ownership-glue work (E0723).",
                        crate::core::fmt_type(&function.ret)
                    ),
                    crate::span::Span::UNKNOWN,
                )]);
            }
        }
        // AD-6: transition tables built once in CheckedProgram, shared by both backends.
        let tables = program.build_transition_tables();
        self.resolved_transitions = Some(tables.resolved);
        self.resolved_fallback_transitions = Some(tables.fallbacks);
        self.resolved_ffi_pinned_transitions = Some(tables.pinned);
        self.resolved_transition_param_arity = Some(tables.param_arity);
        self.resolved_transition_params = Some(tables.param_lists);
        self.resolved_transitions_by_flow = Some(tables.by_flow);
        self.resolved_transitions_by_event = Some(tables.by_event);
        let mut arity = std::collections::HashMap::new();
        let mut returns = std::collections::HashMap::new();
        let mut params = std::collections::HashMap::new();
        let mut comptime_functions = std::collections::HashSet::new();
        for function in program.functions().values() {
            arity.insert(function.qualified_name.clone(), function.params.len());
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
        // P1-27: install bare names for unique bare names so the arity guard
        // fires for module-nested functions (call-site name is always bare).
        {
            let mut bare_counts: std::collections::HashMap<&str, usize> =
                std::collections::HashMap::new();
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
                    arity.entry(bare.to_string()).or_insert(f.params.len());
                }
            }
        }
        self.resolved_function_arity = Some(arity);
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
                .resource_analyses()
                .keys()
                .map(|owner| owner.0.clone())
                .collect(),
        );
        let mut ownership_summaries = std::collections::HashMap::new();
        let mut ownership_resources = std::collections::HashMap::new();
        let mut ownership_actions = std::collections::HashMap::new();
        let mut ownership_merges = std::collections::HashMap::new();
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
                crate::core::SpanPrecision::SourceAnchor => "source_anchor",
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
        let mut actor_method_signatures = std::collections::HashMap::new();
        let mut actor_method_params = std::collections::HashMap::new();
        for actor in program.actors().values() {
            for method in &actor.method_signatures {
                let key = format!("{}.{}", actor.qualified_name, method.name);
                actor_method_signatures
                    .insert(key.clone(), (method.params.len(), method.ret.clone()));
                actor_method_params.insert(key.clone(), method.params.clone());
            }
        }
        self.resolved_actor_method_signatures = Some(actor_method_signatures);
        self.resolved_actor_method_params = Some(actor_method_params);
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
        for trait_def in program.traits().values() {
            for method in &trait_def.method_signatures {
                let key = format!("{}.{}", trait_def.qualified_name, method.name);
                method_signatures.insert(key.clone(), (method.params.len(), method.ret.clone()));
                method_params.insert(key.clone(), method.params.clone());
            }
        }
        for impl_def in program.impls().values() {
            for method in &impl_def.method_signatures {
                let key = format!("{}.{}", impl_def.qualified_name, method.name);
                method_signatures.insert(key.clone(), (method.params.len(), method.ret.clone()));
                method_params.insert(key.clone(), method.params.clone());
            }
        }
        self.resolved_method_signatures = Some(method_signatures);
        self.resolved_method_params = Some(method_params);
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
                crate::core::ResolvedItemKind::Actor => "actor",
                crate::core::ResolvedItemKind::Flow => "flow",
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
        // 0.31.30: build Component IR for runtime function validation.
        {
            let mut gen = crate::component::AbiGenerator::new();
            crate::component::register_core_runtime_abi(&mut gen);
            // 0.31.30+: scan user extern blocks and register as imports.
            // This makes the Component IR aware of user-declared extern
            // functions, enabling bindgen backends to generate complete
            // bindings that include both runtime exports and user imports.
            for block in program.extern_blocks().values() {
                for sig in &block.signatures {
                    gen.import(&sig.name, |f| {
                        let mut builder = f;
                        for (pname, pty) in &sig.params {
                            builder = builder.param(pname, crate::component::mimi_type_to_abi(pty));
                        }
                        if !sig.ret.is_empty() && sig.ret != "void" && sig.ret != "()" {
                            builder = builder.returns(crate::component::mimi_type_to_abi(&sig.ret));
                        }
                        if block.unsafe_ {
                            builder = builder.unsafe_fn();
                        }
                        builder
                    });
                }
            }
            self.component_ir = Some(gen.build());
        }
        // CODEGEN Typed IR migration: body classes enter the resolved
        // emitter only after a pure eligibility scan. Once selected, errors
        // propagate directly; there is deliberately no typed-error → AST
        // fallback. Unmigrated body classes remain on the explicit legacy arm
        // until their slice is implemented and its oracle tests are green.
        if super::resolved::supports_resolved_native(program) {
            return self.compile_resolved_native(program);
        }
        // Per-function dispatch (S12): resolved subset compilation is deferred
        // to inside compile_file, after the setup phase (forward declarations,
        // impl methods, vtables) completes. This ensures all symbols are
        // declared before the resolved emitter compiles eligible bodies.
        // ✅ 2026-07-28: Per-function dispatch (S12) enabled by default.
        // Three ABI fixes resolved the remaining blockers:
        //   1. `coerce_to_i64` — handle PointerValue/StructValue for list
        //      element storage (string ptr → i64).
        //   2. `resolved_type_display_name` — emit composite type names
        //      (List<string>, Option<i32>, etc.) instead of "unknown", so
        //      the print formatter dispatches to mimi_list_to_string etc.
        //   3. `return_owns_heap` — drain (not free) heap scope when the
        //      return type holds pointer fields (list/string structs).
        //   4. `push`/`pop` alloca-swap — pass the original alloca pointer
        //      to mutating builtins instead of a loaded copy.
        // Set MIMI_USE_PER_FUNCTION_DISPATCH=0 or unset to disable.
        let eligible: Option<std::collections::BTreeSet<crate::core::NodeId>> =
            if std::env::var("MIMI_USE_PER_FUNCTION_DISPATCH").map_or(true, |v| v != "0") {
                super::resolved::resolved_eligible_functions(program, self.verify_contracts)
            } else {
                None
            };
        self.compile_file_with_resolved(program, eligible.as_ref())
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

    /// Probe the direct native boundary for already closed MIR islands.
    ///
    /// The probe is deliberately MIR-first: a failed canonical construction
    /// means this old API has not recognized a migrated candidate and may
    /// keep serving an unrelated compatibility program.  Once a candidate is
    /// materialized, however, an island or consumer failure is a hard error;
    /// it is never converted into a legacy compile.
    fn try_compile_exact_migrated_mir_island(
        &self,
        program: &crate::core::CheckedProgram,
    ) -> Result<Option<crate::core::mir::reference::MirProgram>, Vec<crate::diagnostic::Diagnostic>>
    {
        let Ok(canonical) = crate::core::mir::reference::MirProgram::from_checked_program(program)
        else {
            return Ok(None);
        };
        let scalar_collection_candidate =
            crate::core::mir::contains_scalar_collection_candidate(&canonical);
        let flat_copy_record_candidate =
            crate::core::mir::contains_flat_copy_record_candidate(&canonical);
        if !scalar_collection_candidate && !flat_copy_record_candidate {
            return Ok(None);
        }

        // A mixed graph containing a collection candidate and a record value
        // belongs to the narrower collection island first; its island-level
        // validator then rejects the unsupported combination.  This keeps the
        // candidate precedence identical to canonical_dispatch and prevents
        // a flat record from accidentally widening the collection envelope.
        let island = if scalar_collection_candidate {
            "scalar collection island"
        } else {
            "flat Copy record island"
        };
        if scalar_collection_candidate {
            if let Err(errors) = crate::core::mir::validate_scalar_collection_island(&canonical) {
                return Err(Self::mir_gate_diagnostics(
                    program,
                    "MIR island contract",
                    island,
                    &errors,
                ));
            }
        }
        if let Err(errors) = crate::verifier::validate_mir_capabilities(&canonical) {
            return Err(Self::mir_gate_diagnostics(
                program,
                "MIR verifier capability",
                island,
                &errors,
            ));
        }
        if let Err(errors) = crate::interp::bytecode::compile_mir_program(&canonical) {
            return Err(Self::mir_gate_diagnostics(
                program,
                "MIR bytecode",
                island,
                &errors,
            ));
        }
        if let Err(errors) = crate::codegen::mir::validate_mir_native(&canonical) {
            return Err(errors);
        }
        let results = crate::verifier::verify_mir(&canonical, String::new()).map_err(|error| {
            vec![crate::diagnostic::Diagnostic::error_code(
                "MIR-VERIFY-001",
                format!("MIR verifier contract pass failed: {error}"),
                program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
            )]
        })?;
        if results.iter().any(|result| {
            !matches!(
                result.status,
                crate::verifier::VerifStatus::Proven
                    | crate::verifier::VerifStatus::NoObligations
                    | crate::verifier::VerifStatus::Disproven
            )
        }) {
            return Err(vec![crate::diagnostic::Diagnostic::error_code(
                "MIR-VERIFY-001",
                format!(
                    "MIR verifier returned an unsupported or inconclusive result for the {island}"
                ),
                program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
            )]);
        }
        Ok(Some(canonical))
    }

    fn mir_gate_diagnostics(
        program: &crate::core::CheckedProgram,
        consumer: &str,
        island: &str,
        errors: impl std::fmt::Debug,
    ) -> Vec<crate::diagnostic::Diagnostic> {
        vec![crate::diagnostic::Diagnostic::error_code(
            "MIR-CAPABILITY-001",
            format!("{consumer} gate rejected the {island}: {errors:?}"),
            program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
        )]
    }

    pub(crate) fn mangle_name(base: &str, type_map: &HashMap<String, crate::ast::Type>) -> String {
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
                meta: crate::ast::AstNodeMeta::synthetic(crate::ast::AstOrigin::RuntimeSystem(
                    "codegen.generic_substitution",
                )),
                name: k.clone(),
                bounds: vec![],
                kind: crate::ast::GenericKind::Free,
            })
            .collect();
        crate::core::subst_type_params(ty, &generics, &self.type_map)
    }

    /// Apply a handler to every top-level item.
    fn process_items<F>(items: &[Item], f: &mut F) -> MimiResult<()>
    where
        F: FnMut(&Item) -> MimiResult<()>,
    {
        for item in items {
            f(item)?;
        }
        Ok(())
    }

    // Used directly by test code; the production dispatch path goes through
    // compile_file_with_resolved.
    #[allow(dead_code)]
    pub(crate) fn compile_file(&mut self, file: &File) -> MimiResult<()> {
        self.compile_file_inner(file, None)
    }

    /// Compile with per-function resolved dispatch (S12). The resolved subset
    /// is compiled after the setup phase (forward declarations, impl methods,
    /// vtables) but before the legacy body compilation pass. This ensures all
    /// symbols are declared before the resolved emitter compiles eligible bodies.
    ///
    /// 0.32.27+: `file` extracted from `program.raw_ast()` internally,
    /// eliminating the raw AST parameter at the caller site (C1 migration).
    pub(crate) fn compile_file_with_resolved(
        &mut self,
        program: &crate::core::CheckedProgram,
        eligible: Option<&std::collections::BTreeSet<crate::core::NodeId>>,
    ) -> MimiResult<()> {
        // C1 (permanent): the fifth pass compiles ineligible body classes
        // (capturing lambdas, generics, async, extern ABI wrappers) from the
        // surface AST. The resolved native emitter handles the eligible subset;
        // raw_ast() provides the permanent remainder to the legacy emitter.
        self.compile_file_inner(program.raw_ast(), Some((program, eligible)))
    }

    fn compile_file_inner(
        &mut self,
        file: &File,
        resolved_ctx: Option<(
            &crate::core::CheckedProgram,
            Option<&std::collections::BTreeSet<crate::core::NodeId>>,
        )>,
    ) -> MimiResult<()> {
        // Register built-in Record types used by builtins
        self.register_builtin_record_types()?;

        // v0.28.21 — Hold an owned copy of the file so `Expr::Comptime`
        // block folds can construct a fresh interpreter later, after
        // the original `&File` borrow has ended. The clone is shallow
        // w.r.t. String interning but acceptable at this scope.
        self.comptime_file = Some(std::rc::Rc::new(crate::ast::File {
            sources: file.sources.clone(),
            imports: file.imports.clone(),
            items: file.items.clone(),
            implicit_single: false,
        }));

        // v0.28.21 — Evaluate top-level `comptime func` and `const` items via the
        // interpreter and cache the results so `Expr::Comptime` blocks and
        // `comptime func name()` calls can fold to constants at codegen time.
        self.fold_comptime_items(file)?;

        // 0.36.4 Fault nominal: pre-register ALL type defs (including the
        // flow-generated StateId/EventId enums, which expand_items appends after
        // each flow) BEFORE the flow-state pass below. Otherwise the Fault
        // record's `last_state: flow::<name>::StateId` field lowers via
        // llvm_type_for before the enum is registered, falling back to i64 and
        // corrupting the Fault record LLVM layout (enum Display prints raw
        // tag/payload). register_type_def is idempotent (constructor/type_llvm
        // inserts overwrite), so the later pass is a harmless no-op.
        Self::process_items(&file.items, &mut |item| {
            if let Item::Type(t) = item {
                self.register_type_def(t)?;
            }
            Ok(())
        })?;

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
                    let components = if let Some(ref combined) = cap.combined_with {
                        let parts: Vec<String> = combined
                            .split(" + ")
                            .map(|s| s.trim().to_string())
                            .collect();
                        if parts.len() > 1 {
                            parts
                        } else {
                            vec![cap.name.clone(), combined.trim().to_string()]
                        }
                    } else {
                        vec![cap.name.clone()]
                    };
                    self.cap_components.insert(cap.name.clone(), components);
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
                Item::Const {
                    name,
                    value,
                    ty,
                    extern_abi,
                    ..
                } => {
                    // Store const for later reference (inlined at use sites)
                    self.const_values.insert(name.clone(), value.clone());
                    // M-004: `extern "C" const NAME: T = V` exports a C-visible
                    // data symbol. Emit an `External`-linkage module global with
                    // the initializer so `--shared` exposes `NAME` to dlopen/
                    // dlsym consumers (component data API / clap_entry). Plain
                    // `const` items are inlined and never emit a global.
                    if extern_abi.is_some() {
                        self.emit_exported_const(name, ty.as_ref(), value)?;
                    }
                }
                Item::Flow(f) => {
                    // Register flow state payload types so record construction
                    // (e.g. `Zero { count: 0 }`) works in function codegen.
                    let qualified = format!("flow::{}", f.name);
                    for s in &f.states {
                        let type_name = format!("{}::{}", qualified, s.name);
                        let fields = s.payload.clone().unwrap_or_default();
                        let td = TypeDef {
                            meta: AstNodeMeta::inherited(
                                s.meta.span,
                                AstOrigin::RuntimeSystem("codegen.flow_state_type"),
                            ),
                            name: type_name.clone(),
                            pub_: false,
                            kind: TypeDefKind::Record(fields),
                            generics: vec![],
                            derives: vec![],
                            attributes: vec![],
                        };
                        self.register_type_def(&td)?;
                        // Also register unqualified name (skip built-in names like "i32").
                        //
                        // 0.34.36 (audit §6.9): the QUALIFIED key above
                        // (`flow::{flow}::{state}`) is the authoritative layout
                        // source — the multi-target return wrap resolves it via
                        // `flow_state_llvm_type` (mod.rs). The bare alias below is
                        // only a construction shim for bare-name record literals
                        // (`Big { v }`). It is first-wins across flows, so it can
                        // alias a same-named state of another flow; layout-sensitive
                        // paths must therefore never read the bare alias.
                        if !Self::is_builtin_type_name(&s.name)
                            && !self.type_defs.contains_key(&s.name)
                        {
                            let td = TypeDef {
                                meta: AstNodeMeta::inherited(
                                    s.meta.span,
                                    AstOrigin::RuntimeSystem("codegen.flow_state_alias"),
                                ),
                                name: s.name.clone(),
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
                    // v0.34.16 (ADR-002): register the synthetic multi-target
                    // union enum here (first pass) — match arms in main()
                    // resolve variant ordinals before the fifth pass compiles
                    // transition bodies.
                    self.register_flow_multi_target_enums(f)?;
                    // v0.29.24: first @max_children(N) wins as process spawn quota.
                    if self.max_children.is_none() {
                        for a in &f.annotations {
                            if let crate::ast::FlowAnnotationKind::MaxChildren(n) = &a.kind {
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
                if matches!(
                    f.ret.as_ref().map(Type::unlocated),
                    Some(Type::ImplTrait(_))
                ) {
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
        // S12: Per-function resolved dispatch — compile eligible function bodies
        // through the resolved native emitter AFTER all declarations, impl methods,
        // and vtables are set up. The compile_func_legacy skip guard
        // (count_basic_blocks != 0) prevents double-emission in the fifth pass.
        if let Some((program, Some(eligible))) = resolved_ctx {
            if let Err(diagnostics) = self.compile_resolved_subset(program, eligible) {
                // 0.40.1.3 (A3, blind-spots-evaluation-2026-08-29.md §1.3-3/4): a
                // fail-closed ownership error (E0723) must NOT be silently
                // downgraded to the legacy emitter — that would re-emit broken IR
                // that aliases freed heap (the BUG P hole). Escalate it as a hard
                // compile error.
                if diagnostics
                    .iter()
                    .any(|d| d.code.as_deref() == Some("E0723"))
                {
                    return Err(CompileError::Unsupported(
                        diagnostics
                            .iter()
                            .find(|d| d.code.as_deref() == Some("E0723"))
                            .map(|d| d.message.clone())
                            .unwrap_or_else(|| "E0723: unsupported native return".into()),
                    ));
                }
                // 0.1.8 Phase 0: core-callee emit failures are hard errors
                // (function name + reason), not a quiet legacy downgrade.
                if let Some(diag) = diagnostics
                    .iter()
                    .find(|d| d.message.contains("core callee"))
                {
                    return Err(CompileError::Unsupported(diag.message.clone()));
                }
                if std::env::var("MIMI_VERBOSE").is_ok() {
                    for d in &diagnostics {
                        eprintln!("warning: resolved subset issue: {}", d.message);
                    }
                }
            }
        }
        // Fifth pass (legacy emitter): compile user functions, actors, and flow
        // transitions from the surface AST. Functions already compiled by the
        // resolved native emitter (fourth pass) are skipped via the
        // count_basic_blocks != 0 guard in compile_func_legacy.
        // Permanent ineligible body classes: capturing lambdas, generics,
        // async, extern ABI wrappers, view/mutate borrow params (non-self).
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
                        self.compile_func_legacy(f).map_err(|e| e.at(f.meta.span))?;
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

    /// v0.34.16 (ADR-002): register a synthetic `flow::{Name}::__MultiTarget`
    /// enum for each flow that has a multi-target transition. Variants are the
    /// target states (declared order = tag ordinal); payload is the state's
    /// record struct type (boxed: ptrtoint-encoded into the uniform i64 slot
    /// of the enum's `{i32 tag, i64 payload}` layout). The synthetic enum is
    /// what match arms on `Small { v }` / `Large { v }` dispatch against.
    pub(super) fn register_flow_multi_target_enums(&mut self, flow: &FlowDef) -> MimiResult<()> {
        let multi_target_states: Vec<&str> = flow
            .transitions
            .iter()
            .filter(|t| t.to_states.len() > 1)
            .flat_map(|t| t.to_states.iter())
            .map(|s| s.as_str())
            .collect();
        if multi_target_states.is_empty() {
            return Ok(());
        }
        // Dedup preserving first-seen order.
        let mut seen = std::collections::HashSet::new();
        let states: Vec<&str> = multi_target_states
            .into_iter()
            .filter(|s| seen.insert((*s).to_string()))
            .collect();
        let qualified = format!("flow::{}::__MultiTarget", flow.name);
        let meta = AstNodeMeta::inherited(
            flow.meta.span,
            AstOrigin::RuntimeSystem("codegen.multi_target_union"),
        );
        let variants: Vec<Variant> = states
            .iter()
            .map(|state_name| {
                let state_ty =
                    Type::Name(format!("flow::{}::{}", flow.name, state_name), Vec::new())
                        .deep_reorigin(meta);
                Variant {
                    meta,
                    name: state_name.to_string(),
                    payload: Some(VariantPayload::Tuple(vec![state_ty])),
                }
            })
            .collect();
        let td = TypeDef {
            meta,
            name: qualified.clone(),
            pub_: false,
            kind: TypeDefKind::Enum(variants),
            generics: vec![],
            derives: vec![],
            attributes: vec![],
        };
        self.register_type_def(&td)?;
        // C1 fix: build the flow-wide state-name → tag-ordinal map using the
        // SAME ordering register_type_def applies (variants sorted by name).
        // Return sites query this map so a transition whose target set is a
        // proper subset of the flow union still emits the global ordinal —
        // the receiving match dispatches on the global enum ordinal, and a
        // subset-relative tag would silently alias a different state.
        // Bucketed by flow name: each flow's __MultiTarget enum is its own
        // enum with independent ordinals.
        {
            let mut sorted_states = states.clone();
            sorted_states.sort_unstable();
            let mut ordinals = std::collections::HashMap::with_capacity(sorted_states.len());
            for (ord, s) in sorted_states.into_iter().enumerate() {
                ordinals.insert(s.to_string(), ord as u64);
            }
            self.multi_target_global_ordinals
                .insert(flow.name.clone(), ordinals);
        }
        // Also register the unqualified alias so `match r { Small { .. } }`
        // resolves the owning enum through find_variant_owner.
        if !self
            .type_defs
            .contains_key(&format!("__MultiTarget_{}", flow.name))
        {
            let alias = TypeDef {
                meta,
                name: format!("__MultiTarget_{}", flow.name),
                pub_: false,
                kind: TypeDefKind::Alias(
                    Type::Name(qualified.clone(), Vec::new()).deep_reorigin(meta),
                ),
                generics: vec![],
                derives: vec![],
                attributes: vec![],
            };
            self.register_type_def(&alias)?;
        }
        Ok(())
    }

    /// Convert a flow transition into a synthetic FuncDef for codegen.
    ///
    /// Parameters: `self` (from-state payload) + event params.
    /// Return type: the single declared target state's nominal LLVM layout.
    /// Multi-target transitions are rejected by `compile_flow` until codegen
    /// has a closed tagged-state-union ABI.
    /// Body: the transition body (v0.34.27: `do { }` removed — plain block).
    pub(super) fn transition_to_func(flow: &FlowDef, t: &TransitionDef) -> FuncDef {
        let origin = AstOrigin::RuntimeSystem("codegen.transition_lowering");
        let meta = AstNodeMeta::inherited(t.meta.span, origin);
        let mut params = Vec::new();
        params.push(Param {
            meta,
            name: "self".to_string(),
            ty: Type::Name(t.from_state.clone(), vec![]).deep_reorigin(meta),
            mut_: false,
            default_value: None,
            borrow: None,
        });
        params.extend(t.params.iter().map(|param| Param {
            meta,
            name: param.name.clone(),
            ty: param.ty.clone().deep_reorigin(meta),
            mut_: param.mut_,
            default_value: param.default_value.clone(),
            borrow: param.borrow,
        }));

        // v0.34.16 (ADR-002): multi-target transitions return the synthetic
        // tagged union `flow::{Flow}::__MultiTarget` ({i32 tag, i64 payload});
        // single-target transitions return the target state struct as before.
        let ret_name = if t.to_states.len() > 1 {
            format!("flow::{}::__MultiTarget", flow.name)
        } else {
            t.to_states
                .first()
                .cloned()
                .unwrap_or_else(|| "unit".to_string())
        };

        // FLOW-TURN-001: when `fails E` is declared, the transition's return type
        // becomes Result<Target, (Source, E)> so the caller can match Ok/Err.
        let ret_type = if let Some(fails_ty) = &t.fails {
            Type::Result(
                Box::new(Type::Name(ret_name, vec![]).deep_reorigin(meta)),
                Box::new(Type::Tuple(vec![
                    Type::Name(t.from_state.clone(), vec![]).deep_reorigin(meta),
                    fails_ty.clone().deep_reorigin(meta),
                ])),
            )
            .deep_reorigin(meta)
        } else {
            Type::Name(ret_name, vec![]).deep_reorigin(meta)
        };

        // v0.34.27: `do { ... }` removed — transition body is the plain block.
        // (was: unwrap single outer `do { }` so compile_block sees normal stmts;
        // the unwrap itself proved `{ do { X } }` ≡ `{ X }`).
        let body: Block = t.body.clone().unwrap_or_default();

        FuncDef {
            meta,
            name: Self::transition_fn_name(&flow.name, &t.name, &t.from_state),
            pub_: false,
            params,
            ret: Some(ret_type),
            body,
            where_clause: vec![],
            generics: vec![],
            effects: vec![],
            is_comptime: false,
            is_async: false,
            extern_abi: None,
            has_requires: false,
            has_ensures: false,
            has_mutate_params: false,
        }
    }

    /// Compile all transitions of a flow as ordinary LLVM functions.
    pub(super) fn compile_flow(&mut self, flow: &FlowDef) -> MimiResult<()> {
        self.current_flow_name = flow.name.clone();
        // H4 (audit-codegen): persistent field names for the panic→Fault
        // shadow, taken from the FlowDef so BOTH compilation entry points
        // (compile_checked + legacy compile_file) see them.
        self.current_persistent_fields = flow.persistent_fields.clone();
        for t in &flow.transitions {
            if t.body.is_none() {
                continue; // abstract / protocol-style transition — no body
            }
            if t.to_states.len() != 1 {
                // Multi-target: ret type is the synthetic union; return
                // statements are wrapped (tag + boxed payload) below.
                let func = Self::transition_to_func(flow, t);
                self.in_multi_target_transition = true;
                self.multi_target_states = t.to_states.clone();
                self.current_from_state = t.from_state.clone();
                let result = self
                    .compile_func_legacy(&func)
                    .map_err(|e| e.at(t.meta.span));
                self.in_multi_target_transition = false;
                self.multi_target_states = Vec::new();
                self.current_from_state = String::new();
                self.fault_self_entry = None;
                result?;
                continue;
            }
            let func = Self::transition_to_func(flow, t);
            // FLOW-TURN-001: flag transition bodies with `fails E` so
            // compile_try_expr can emit a fail-closed error (Rejected
            // codegen not yet implemented) instead of mimi_try_exit.
            self.in_fails_transition = t.fails.is_some();
            let result = self
                .compile_func_legacy(&func)
                .map_err(|e| e.at(t.meta.span));
            self.in_fails_transition = false;
            result?;
        }
        // 0.37.x: flow-compilation state used to leak after compiling a Flow.
        // `current_flow_name` stayed set while subsequent plain functions were
        // compiled, so a builtin named like a Flow EventId variant (e.g. the
        // network `accept(fd)` vs an `accept` transition) was miscompiled as
        // the flow's EventId enum constructor and produced a struct in integer
        // comparison. Clear the per-flow context when the Flow pass finishes.
        self.current_flow_name = String::new();
        self.current_persistent_fields = Vec::new();
        self.current_from_state = String::new();
        Ok(())
    }

    /// Register built-in Record types used by builtin functions (exec, file_stat, etc.)
    /// so that field access and struct construction work in codegen.
    fn register_builtin_record_types(&mut self) -> MimiResult<()> {
        use inkwell::types::BasicTypeEnum;
        let meta = AstNodeMeta::synthetic(AstOrigin::RuntimeSystem("codegen.builtin_type"));
        let generated_type =
            |name: &str| crate::ast::Type::Name(name.to_string(), vec![]).deep_reorigin(meta);
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
                meta,
                name: "ExecResult".to_string(),
                pub_: false,
                kind: crate::ast::TypeDefKind::Record(vec![
                    crate::ast::Field {
                        meta,
                        name: "exit_code".to_string(),
                        ty: generated_type("i32"),
                    },
                    crate::ast::Field {
                        meta,
                        name: "stdout".to_string(),
                        ty: generated_type("string"),
                    },
                    crate::ast::Field {
                        meta,
                        name: "stderr".to_string(),
                        ty: generated_type("string"),
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
                meta,
                name: "StatResult".to_string(),
                pub_: false,
                kind: crate::ast::TypeDefKind::Record(vec![
                    crate::ast::Field {
                        meta,
                        name: "size".to_string(),
                        ty: generated_type("i64"),
                    },
                    crate::ast::Field {
                        meta,
                        name: "modified".to_string(),
                        ty: generated_type("i64"),
                    },
                    crate::ast::Field {
                        meta,
                        name: "is_file".to_string(),
                        ty: generated_type("bool"),
                    },
                    crate::ast::Field {
                        meta,
                        name: "is_dir".to_string(),
                        ty: generated_type("bool"),
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
                meta,
                name: "PeerFault".to_string(),
                pub_: false,
                kind: crate::ast::TypeDefKind::Record(vec![
                    crate::ast::Field {
                        meta,
                        name: "peer_id".to_string(),
                        ty: generated_type("string"),
                    },
                    crate::ast::Field {
                        meta,
                        name: "reason".to_string(),
                        ty: generated_type("string"),
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
                meta,
                name: "SystemTrace".to_string(),
                pub_: false,
                kind: crate::ast::TypeDefKind::Record(vec![
                    crate::ast::Field {
                        meta,
                        name: "last_state_name".to_string(),
                        ty: generated_type("string"),
                    },
                    crate::ast::Field {
                        meta,
                        name: "unexpected_event".to_string(),
                        ty: generated_type("string"),
                    },
                    crate::ast::Field {
                        meta,
                        name: "snapshot".to_string(),
                        ty: generated_type("string"),
                    },
                    crate::ast::Field {
                        meta,
                        name: "memory_dump".to_string(),
                        ty: generated_type("MemoryDump"),
                    },
                    crate::ast::Field {
                        meta,
                        name: "panic_payload".to_string(),
                        ty: generated_type("PanicPayload"),
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
                meta,
                name: "PanicPayload".to_string(),
                pub_: false,
                kind: crate::ast::TypeDefKind::Record(vec![
                    crate::ast::Field {
                        meta,
                        name: "error_type".to_string(),
                        ty: generated_type("string"),
                    },
                    crate::ast::Field {
                        meta,
                        name: "file".to_string(),
                        ty: generated_type("string"),
                    },
                    crate::ast::Field {
                        meta,
                        name: "line".to_string(),
                        ty: generated_type("i32"),
                    },
                    crate::ast::Field {
                        meta,
                        name: "stack".to_string(),
                        ty: generated_type("string"),
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
                meta,
                name: "MemoryDump".to_string(),
                pub_: false,
                kind: crate::ast::TypeDefKind::Record(vec![
                    crate::ast::Field {
                        meta,
                        name: "fields".to_string(),
                        ty: generated_type("string"),
                    },
                    crate::ast::Field {
                        meta,
                        name: "count".to_string(),
                        ty: generated_type("i32"),
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
    ///
    /// ⚠️ 0.31.22 Spec 修正：O2 = experimental
    /// 架构修正案规定：
    /// - O2 优化是 experimental，不保证语义等价
    /// - 最高稳定优化 = 内联 + DCE + SROA（O1 级别）
    /// - O2/O3 可能触发 LLVM 优化器 bug（如 inttoptr provenance UB）
    /// - 生产环境建议使用 O1（MIMI_OPT=1）
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
                OptimizationLevel::Default,
                inkwell::targets::RelocMode::Default,
                inkwell::targets::CodeModel::Default,
            )
            .ok_or_else(|| {
                CompileError::LlvmError("failed to create target machine".to_string())
            })?;
        let options = PassBuilderOptions::create();
        self.module
            .run_passes("internalize,default<O2>,globaldce", &tm, options)
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
        let file_ref = match &self.comptime_file {
            Some(rc) => rc.as_ref(),
            None => return Ok(()),
        };
        // v0.33 Phase F: compile ONLY comptime functions (avoid non-comptime
        // functions that may have patterns the bytecode compiler rejects).
        let has_comptime = file_ref
            .items
            .iter()
            .any(|item| matches!(item, crate::ast::Item::Func(f) if f.is_comptime && f.params.is_empty()));
        if !has_comptime {
            return Ok(());
        }
        let bytecode_result = (|| {
            // Build a synthetic file with only comptime functions + supporting items.
            let mut synth = crate::ast::File {
                sources: file_ref.sources.clone(),
                items: Vec::new(),
                imports: Vec::new(),
                implicit_single: file_ref.implicit_single,
            };
            for item in &file_ref.items {
                match item {
                    // Include comptime functions (the ones we want to evaluate).
                    crate::ast::Item::Func(f) if f.is_comptime && f.params.is_empty() => {
                        synth.items.push(item.clone());
                    }
                    // Include types, constants, traits, impls (dependencies).
                    crate::ast::Item::Type(_)
                    | crate::ast::Item::Const { .. }
                    | crate::ast::Item::Trait(_)
                    | crate::ast::Item::Impl(_)
                    | crate::ast::Item::Cap(_) => {
                        synth.items.push(item.clone());
                    }
                    // Skip non-comptime functions, actors, flows, sessions, etc.
                    // (they may have patterns the bytecode compiler rejects).
                    _ => {}
                }
            }
            let mut compiler = crate::interp::bytecode::BytecodeCompiler::new();
            let prog = compiler
                .compile_for_comptime(&synth)
                .map_err(|e| e.to_string())?;
            let mut vm = crate::interp::bytecode::BytecodeVM::new(prog.clone());
            let mut results = std::collections::HashMap::new();
            for item in &synth.items {
                if let crate::ast::Item::Func(f) = item {
                    if f.is_comptime && f.params.is_empty() {
                        if let Some(fidx) = prog.function_index(&f.name) {
                            let value = vm.call_function(fidx, &[]).map_err(|e| e.to_string())?;
                            results.insert(f.name.clone(), value);
                        }
                    }
                }
            }
            Ok::<_, String>(results)
        })();
        match bytecode_result {
            Ok(results) => {
                for (name, value) in results {
                    self.comptime_values.insert(name, value);
                }
                Ok(())
            }
            Err(err) => {
                // If bytecode comptime folding fails, skip gracefully rather
                // than aborting the whole compilation. Report the failure so
                // it is not silently swallowed (the doc comment above promises
                // a warning; comptime values will be evaluated at runtime).
                eprintln!(
                    "warning: comptime folding failed ({}); comptime values will be evaluated at runtime",
                    err
                );
                Ok(())
            }
        }
    }

    /// M-004: emit a C-visible data symbol for `extern "C" const NAME: T = V`.
    ///
    /// Builds an `External`-linkage module global initialised with the const
    /// value so a `--shared` object exposes `NAME` to dlopen/dlsym consumers
    /// (component data API, e.g. `clap_entry`). Only scalar literals (int /
    /// float / bool) and string initializers are supported today; computed or
    /// composite consts are rejected loudly rather than emitted as a broken
    /// (zero-initialised) symbol.
    fn emit_exported_const(
        &self,
        name: &str,
        ty: Option<&crate::ast::Type>,
        value: &Expr,
    ) -> MimiResult<()> {
        // 1. Resolve the const's type (explicit annotation, else inferred from
        //    the literal).
        let const_ty: crate::ast::Type = match ty {
            Some(t) => t.clone(),
            None => match value.unlocated() {
                Expr::Literal(Lit::Int(_)) => Type::Name("i64".into(), vec![]),
                Expr::Literal(Lit::Float(_)) => Type::Name("f64".into(), vec![]),
                Expr::Literal(Lit::Bool(_)) => Type::Name("bool".into(), vec![]),
                Expr::Literal(Lit::String(_)) => Type::Name("string".into(), vec![]),
                _ => {
                    return Err(CompileError::LlvmError(format!(
                        "exported const '{}' must have an explicit type annotation \
                         (computed/untyped initializers are not supported for `extern \"C\" const`)",
                        name
                    )))
                }
            },
        };

        // 2. Fold the initializer to a concrete value (literals only — computed
        //    consts would need VM evaluation, which is not wired up for data
        //    export yet).
        let val = match value.unlocated() {
            Expr::Literal(Lit::Int(n)) => crate::interp::Value::Int(*n),
            Expr::Literal(Lit::Float(f)) => crate::interp::Value::Float(*f),
            Expr::Literal(Lit::Bool(b)) => crate::interp::Value::Bool(*b),
            Expr::Literal(Lit::String(s)) => {
                crate::interp::Value::String(std::sync::Arc::new(s.clone()))
            }
            _ => {
                return Err(CompileError::LlvmError(format!(
                    "exporting computed const '{}' via `extern \"C\" const` is not yet \
                     supported; use a literal initializer (e.g. `const {} = 42;`)",
                    name, name
                )))
            }
        };

        // 3. Determine the LLVM type and build the initializer constant.
        let llvm_ty = self.llvm_type_for(&const_ty).ok_or_else(|| {
            CompileError::LlvmError(format!(
                "cannot determine LLVM type for exported const '{}' of type {:?}",
                name, const_ty
            ))
        })?;
        let init = self.const_global_initializer(name, &const_ty, &llvm_ty, &val)?;

        // 4. Emit the External-linkage global. The `u_` namespacing pass only
        //    renames functions, so the exported data symbol keeps its clean
        //    source name.
        let gv = self.module.add_global(llvm_ty, None, name);
        gv.set_linkage(Linkage::External);
        gv.set_initializer(&init);
        Ok(())
    }

    /// Build an LLVM constant initializer for an exported const value.
    fn const_global_initializer(
        &self,
        name: &str,
        ty: &crate::ast::Type,
        llvm_ty: &BasicTypeEnum<'ctx>,
        val: &crate::interp::Value,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        match (val, llvm_ty) {
            (crate::interp::Value::Int(n), BasicTypeEnum::IntType(it)) => {
                Ok(it.const_int(*n as u64, false).into())
            }
            (crate::interp::Value::Float(f), BasicTypeEnum::FloatType(ft)) => {
                Ok(ft.const_float(*f).into())
            }
            (crate::interp::Value::Bool(b), BasicTypeEnum::IntType(it)) => {
                Ok(it.const_int(*b as u64, false).into())
            }
            (crate::interp::Value::String(s), BasicTypeEnum::StructType(_st)) => {
                // Mimi string ABI = { i8*, i64 } (ptr + len).
                let bytes = s.as_bytes();
                let arr_ty = self.context.i8_type().array_type((bytes.len() as u32) + 1);
                let gstr =
                    self.module
                        .add_global(arr_ty, None, &format!("__mimi_const_str_{}", name));
                gstr.set_linkage(Linkage::Internal);
                gstr.set_initializer(&self.context.const_string(bytes, true));
                let ptr = gstr.as_pointer_value();
                let len = self.context.i64_type().const_int(bytes.len() as u64, false);
                let struct_val = self.context.const_struct(&[ptr.into(), len.into()], false);
                Ok(struct_val.into())
            }
            _ => Err(CompileError::LlvmError(format!(
                "exporting const '{}' of value {:?} (type {:?}) is not yet supported",
                name, val, ty
            ))),
        }
    }
}
