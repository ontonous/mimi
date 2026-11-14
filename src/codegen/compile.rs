#[cfg(test)]
use crate::ast::*;
use crate::error::{CompileError, MimiResult};
use std::collections::{HashMap, HashSet};

use super::CodeGenerator;
use inkwell::module::Linkage;
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::TargetMachine;
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

fn checked_native_data_exports(
    program: &crate::core::CheckedProgram,
) -> Vec<crate::core::native_data_export::CheckedNativeDataExport> {
    let mut exports = program
        .constants()
        .values()
        .filter_map(|constant| constant.native_data_export.clone())
        .collect::<Vec<_>>();
    exports.sort_by(|left, right| left.symbol.cmp(&right.symbol));
    exports
}

/// Receipt profile for concrete, checker-selected trait specializations that
/// share one generic receiver family.  The corresponding admission predicate
/// is intentionally limited to exact static impl owners and monomorphic impl
/// bodies; dynamic dispatch and generic impl templates stay outside it.
pub const STATIC_GENERIC_TRAIT_SPECIALIZATION_ROUTE_PROFILE: &str =
    "static-generic-trait-specialization-v1";

/// The parser injects this exact Flow around ordinary scripts that define a
/// top-level `main`.  It has no user state/transition contract: the only
/// evidence accepted here is the checker-retained RuntimeSystem provenance
/// chain plus the complete, known injected state/transition table.  A Flow
/// with the same name but any user-authored node or altered graph stays out.
fn has_only_parser_synthetic_main_flow(program: &crate::core::CheckedProgram) -> bool {
    if program.flows().is_empty() {
        return true;
    }
    if program.flows().len() != 1 || program.transitions().len() != 5 {
        return false;
    }

    let Some(flow) = program.flows().values().next() else {
        return false;
    };
    let flow_node = crate::core::NodeId("flow:Main".into());
    let main_node = crate::core::NodeId("function:main".into());
    let Some(main) = program.functions().get(&main_node) else {
        return false;
    };
    if flow.id.0 != "Main"
        || flow.node_id != flow_node
        || main.qualified_name != "main"
        || !matches!(main.origin, crate::core::Origin::User(_))
        || !matches!(
            &flow.origin,
            crate::core::Origin::RuntimeSystem { parent, rule, span }
                if parent == &main_node
                    && rule == "progressive.main"
                    && *span == main.origin.user_span()
        )
        || !flow.generic_parameters.is_empty()
        || !flow.persistent_fields.is_empty()
        || flow.max_children.is_some()
        || flow.mailbox_depth.is_some()
        || flow.states.len() != 2
    {
        return false;
    }

    let Some(single) = flow.states.get("Single") else {
        return false;
    };
    let Some(fault) = flow.states.get("Fault") else {
        return false;
    };
    if single.node_id.0 != "state:Main::Single"
        || single.id.flow.0 != "Main"
        || single.id.name != "Single"
        || !single.payload.is_empty()
        || !single.field_ids.is_empty()
        || !matches!(
            &single.origin,
            crate::core::Origin::RuntimeSystem { parent, rule, span }
                if parent == &flow_node
                    && rule == "progressive.single"
                    && *span == flow.origin.user_span()
        )
        || fault.node_id.0 != "state:Main::Fault"
        || fault.id.flow.0 != "Main"
        || fault.id.name != "Fault"
        || fault.payload.len() != 4
        || fault
            .payload
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<HashSet<_>>()
            != HashSet::from(["last_state", "unexpected_event", "snapshot", "trace"])
        || fault
            .field_ids
            .keys()
            .map(String::as_str)
            .collect::<HashSet<_>>()
            != HashSet::from(["last_state", "unexpected_event", "snapshot", "trace"])
        || fault.payload.iter().any(|(name, ty)| {
            let expected = match name.as_str() {
                "last_state" => "flow::Main::StateId",
                "unexpected_event" => "flow::Main::EventId",
                "snapshot" => "string",
                "trace" => "SystemTrace",
                _ => return true,
            };
            crate::core::fmt_type(ty) != expected
        })
        || !matches!(
            &fault.origin,
            crate::core::Origin::RuntimeSystem { parent, rule, span }
                if parent == &flow_node
                    && rule == "flow.fault_state"
                    && *span == flow.origin.user_span()
        )
    {
        return false;
    }

    let expected = [
        ("run", "Single", "Single", false, true, "progressive.run"),
        (
            "peer_fault",
            "Single",
            "Fault",
            true,
            false,
            "flow.peer_fault",
        ),
        (
            "peer_fault",
            "Fault",
            "Fault",
            true,
            true,
            "flow.peer_fault",
        ),
        ("reset", "Fault", "Single", true, false, "flow.reset"),
        ("recover", "Fault", "Single", true, false, "flow.recover"),
    ];
    if flow.transitions.len() != expected.len() {
        return false;
    }
    expected.iter().all(
        |(event, source, target, is_fallback, silent, origin_rule)| {
            let Some(transition) = program.transition("Main", event, source) else {
                return false;
            };
            transition.targets.len() == 1
                && transition.targets[0].flow == flow.id
                && transition.targets[0].name == *target
                && transition.params.is_empty()
                && transition.fails.is_none()
                && transition.is_fallback == *is_fallback
                && !transition.is_ffi_pinned
                && transition.silent_transition == *silent
                && flow.transitions.contains(&transition.id)
                && program.resolved_body(&transition.node_id).is_none()
                && matches!(
                    &transition.origin,
                    crate::core::Origin::RuntimeSystem { parent, rule, span }
                        if parent == &flow_node
                            && rule == origin_rule
                            && *span == flow.origin.user_span()
                )
        },
    )
}

fn resolved_type_contains_parser_main_state(
    type_id: &crate::core::ResolvedTypeId,
    types: &crate::core::ResolvedTypeTable,
    seen: &mut HashSet<crate::core::ResolvedTypeId>,
) -> bool {
    if !seen.insert(type_id.clone()) {
        return false;
    }
    use crate::core::ResolvedType;
    match types.get(type_id) {
        Some(ResolvedType::Nominal {
            item, arguments, ..
        }) => {
            let identity = item.as_str();
            identity.starts_with("state:Main::")
                || identity == "flow::Main::StateId"
                || identity == "flow::Main::EventId"
                || arguments
                    .iter()
                    .any(|argument| resolved_type_contains_parser_main_state(argument, types, seen))
        }
        Some(ResolvedType::FlowStateSet { flow, .. }) => flow.as_str() == "flow:Main",
        Some(ResolvedType::Reference { target, .. })
        | Some(ResolvedType::CBuffer(target))
        | Some(ResolvedType::Ownership { target, .. })
        | Some(ResolvedType::Newtype { inner: target, .. })
        | Some(ResolvedType::Array {
            element: target, ..
        })
        | Some(ResolvedType::Slice(target))
        | Some(ResolvedType::RawPointer { target, .. })
        | Some(ResolvedType::Option(target)) => {
            resolved_type_contains_parser_main_state(target, types, seen)
        }
        Some(ResolvedType::Result { ok, error }) => {
            resolved_type_contains_parser_main_state(ok, types, seen)
                || resolved_type_contains_parser_main_state(error, types, seen)
        }
        Some(ResolvedType::Tuple(elements)) => elements
            .iter()
            .any(|element| resolved_type_contains_parser_main_state(element, types, seen)),
        Some(ResolvedType::Function {
            parameters, result, ..
        }) => {
            parameters
                .iter()
                .any(|parameter| resolved_type_contains_parser_main_state(parameter, types, seen))
                || resolved_type_contains_parser_main_state(result, types, seen)
        }
        Some(
            ResolvedType::Primitive(_)
            | ResolvedType::GenericParameter(_)
            | ResolvedType::Capability(_)
            | ResolvedType::Trait { .. }
            | ResolvedType::DynamicAny { .. },
        )
        | None => false,
    }
}

/// User callable types and user-origin typed expressions may not mention the
/// injected Main states or its transition id enums. Parser-generated Flow
/// scaffolding is excluded by the provenance filter above, so this detects
/// real state use without treating the wrapper itself as user semantics.
fn has_user_main_flow_state_use(program: &crate::core::CheckedProgram) -> bool {
    let contains = |type_id: &crate::core::ResolvedTypeId| {
        resolved_type_contains_parser_main_state(
            type_id,
            program.resolved_types(),
            &mut HashSet::new(),
        )
    };
    for (node, type_id) in program.resolved_node_types() {
        if program
            .node_meta()
            .get(node)
            .is_some_and(|meta| !matches!(meta.origin, crate::core::Origin::RuntimeSystem { .. }))
            && contains(type_id)
        {
            return true;
        }
    }
    for (node, type_id) in program.resolved_type_operands() {
        if program
            .node_meta()
            .get(node)
            .is_some_and(|meta| !matches!(meta.origin, crate::core::Origin::RuntimeSystem { .. }))
            && contains(type_id)
        {
            return true;
        }
    }
    for (node, type_arguments) in program.resolved_type_arguments() {
        if program
            .node_meta()
            .get(node)
            .is_some_and(|meta| !matches!(meta.origin, crate::core::Origin::RuntimeSystem { .. }))
            && type_arguments.iter().any(&contains)
        {
            return true;
        }
    }
    for (owner, signature) in program.resolved_signatures() {
        if program.functions().contains_key(owner)
            && (signature
                .parameters
                .iter()
                .any(|parameter| contains(&parameter.ty))
                || contains(&signature.result))
        {
            return true;
        }
    }
    program
        .resolved_bodies()
        .values()
        .any(|body| body.locals.values().any(|local| contains(&local.ty)))
}

/// Whether the checked program calls two or more exact impls of the same
/// protocol method for the same supported generic receiver constructor (for
/// example, `Marker<i32> for List<i32>` and `Marker<i64> for List<i64>`).
///
/// This is a bounded route predicate, not a general trait capability claim:
/// each checked method must map to exactly one monomorphic impl body, and no
/// reachable generic free-function call may be smuggled into this route.
pub fn has_static_generic_trait_specialization_candidate(
    program: &crate::core::CheckedProgram,
) -> bool {
    if !has_only_parser_synthetic_main_flow(program)
        || has_user_main_flow_state_use(program)
        || !program.actors().is_empty()
        || !program.sessions().is_empty()
        || !program.extern_blocks().is_empty()
    {
        return false;
    }
    let declarations = program.native_declarations();
    let mut specializations = HashMap::<(String, String, String), HashSet<String>>::new();
    let mut selected_method: Option<(String, String, String)> = None;

    for site in program.call_sites().values() {
        // `has_only_parser_synthetic_main_flow` above has already checked the
        // complete generated Main graph and its RuntimeSystem provenance. Its
        // transition dispatch call sites are generated scaffolding, so they
        // do not participate in the user's static generic-trait call census.
        // User-authored Flow is rejected by that exact graph check.
        if site.owner.starts_with("transition:Main::") {
            continue;
        }
        // The MIR constructor and every consumer for this route exclude the
        // checker-known prelude source, so dormant compatibility helpers in
        // that source do not widen the executable candidate's call surface.
        if program
            .source_registry()
            .key(site.origin.user_span().source_id)
            .is_some_and(|key| key.as_str() == "stdlib:prelude.mimi")
        {
            continue;
        }
        let Some(callee) = site.resolved_callee.as_ref() else {
            return false;
        };
        let crate::core::ir::ResolvedCallee::ProtocolMethod { protocol, method } = callee else {
            if matches!(callee, crate::core::ir::ResolvedCallee::Builtin(builtin) if builtin.as_str() == "println")
            {
                continue;
            }
            return false;
        };

        let callable = crate::core::NodeId(method.as_str().to_owned());
        let mut bindings = declarations
            .impls
            .values()
            .filter(|implementation| implementation.trait_owner == *protocol)
            .flat_map(|implementation| {
                implementation
                    .methods
                    .iter()
                    .filter(|binding| binding.callable == callable)
                    .map(move |binding| (implementation, binding))
            });
        let Some((implementation, binding)) = bindings.next() else {
            return false;
        };
        if bindings.next().is_some()
            || !implementation.generics.is_empty()
            || !binding.generics.is_empty()
        {
            return false;
        }

        let Some(receiver) = binding
            .signature
            .parameters
            .first()
            .and_then(|parameter| program.resolved_types().get(&parameter.ty))
        else {
            return false;
        };
        let crate::core::ResolvedType::Nominal {
            item, arguments, ..
        } = receiver
        else {
            return false;
        };
        if item.as_str() != "builtin:type:List"
            || !matches!(arguments.as_slice(), [element]
            if matches!(
                program.resolved_types().get(element),
                Some(
                    crate::core::ResolvedType::Primitive(
                    crate::core::PrimitiveType::I32
                        | crate::core::PrimitiveType::I64
                        | crate::core::PrimitiveType::String
                    )
                )
            ))
        {
            return false;
        }
        let method_key = (
            protocol.0.clone(),
            binding.name.clone(),
            item.as_str().to_owned(),
        );
        if selected_method
            .as_ref()
            .is_some_and(|selected| selected != &method_key)
        {
            return false;
        }
        selected_method.get_or_insert_with(|| method_key.clone());
        specializations
            .entry(method_key)
            .or_default()
            .insert(implementation.owner.0.clone());
    }

    specializations.len() == 1 && specializations.values().any(|owners| owners.len() >= 2)
}

impl<'ctx> CodeGenerator<'ctx> {
    pub fn compile_checked(
        &mut self,
        program: &crate::core::CheckedProgram,
    ) -> Result<(), Vec<crate::diagnostic::Diagnostic>> {
        // A serializer may only rely on a completed shared-mode scan for the
        // module currently in this generator. Clear stale provenance before
        // every attempt; compilation can fail after a partial emission.
        self.shared_dynamic_any_preflight_passed = false;
        self.preflight_checked_native_data_exports(program)?;
        self.compile_checked_inner(program)?;
        self.emit_checked_native_data_exports(program)?;
        if self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        Ok(())
    }

    /// Validate every checked scalar data export before a native body route
    /// starts emitting LLVM. This also rejects collisions with declarations
    /// already installed in the module, including runtime function symbols.
    pub fn preflight_checked_native_data_exports(
        &self,
        program: &crate::core::CheckedProgram,
    ) -> Result<(), Vec<crate::diagnostic::Diagnostic>> {
        crate::core::native_data_export::validate_checked_native_data_exports(program)?;
        let exports = checked_native_data_exports(program);
        let mut errors = Vec::new();
        for export in exports {
            if self.module.get_function(&export.symbol).is_some()
                || self.module.get_global(&export.symbol).is_some()
            {
                errors.push(crate::diagnostic::Diagnostic::error_code(
                    crate::core::native_data_export::NATIVE_DATA_EXPORT_ERROR_CODE,
                    format!(
                        "exported data symbol '{}' collides with an existing native symbol",
                        export.symbol
                    ),
                    export.span,
                ));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Emit the Checker-owned scalar data exports after the selected body route
    /// succeeds. Callers must run the preflight before route compilation.
    pub fn emit_checked_native_data_exports(
        &self,
        program: &crate::core::CheckedProgram,
    ) -> Result<(), Vec<crate::diagnostic::Diagnostic>> {
        crate::core::native_data_export::validate_checked_native_data_exports(program)?;
        let exports = checked_native_data_exports(program);
        let mut errors = Vec::new();
        for export in &exports {
            if self.module.get_function(&export.symbol).is_some()
                || self.module.get_global(&export.symbol).is_some()
            {
                errors.push(crate::diagnostic::Diagnostic::error_code(
                    crate::core::native_data_export::NATIVE_DATA_EXPORT_ERROR_CODE,
                    format!(
                        "exported data symbol '{}' collides with an existing native symbol",
                        export.symbol
                    ),
                    export.span,
                ));
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }

        for export in exports {
            use crate::core::native_data_export::{
                NativeDataExportType as ExportType, NativeDataExportValue as ExportValue,
            };
            let (ty, initializer): (BasicTypeEnum<'ctx>, BasicValueEnum<'ctx>) = match (
                export.ty,
                export.value,
            ) {
                (ExportType::I32, ExportValue::I32(value)) => {
                    let ty = self.context.i32_type();
                    let initializer = ty.const_int(value as u32 as u64, false);
                    (ty.into(), initializer.into())
                }
                (ExportType::I64, ExportValue::I64(value)) => {
                    let ty = self.context.i64_type();
                    let initializer = ty.const_int(value as u64, false);
                    (ty.into(), initializer.into())
                }
                (ExportType::F32, ExportValue::F32(value)) => {
                    let ty = self.context.f32_type();
                    let initializer = ty.const_float(f64::from(value));
                    (ty.into(), initializer.into())
                }
                (ExportType::F64, ExportValue::F64(value)) => {
                    let ty = self.context.f64_type();
                    let initializer = ty.const_float(value);
                    (ty.into(), initializer.into())
                }
                (ExportType::Bool, ExportValue::Bool(value)) => {
                    let ty = self.context.bool_type();
                    let initializer = ty.const_int(if value { 1 } else { 0 }, false);
                    (ty.into(), initializer.into())
                }
                _ => {
                    return Err(vec![crate::diagnostic::Diagnostic::error_code(
                            crate::core::native_data_export::NATIVE_DATA_EXPORT_ERROR_CODE,
                            format!(
                                "checked descriptor for exported data symbol '{}' has a mismatched scalar type and value",
                                export.symbol
                            ),
                            export.span,
                        )]);
                }
            };
            let global = self.module.add_global(ty, None, &export.symbol);
            global.set_linkage(Linkage::External);
            global.set_constant(true);
            global.set_initializer(&initializer);
        }
        Ok(())
    }

    /// Check the native entry roots that can be invoked from outside the
    /// current Mimi call graph. Shared-library builds root every non-comptime
    /// callable; executable builds root `main` plus C exports.
    pub(crate) fn preflight_native_dynamic_any_unpack(
        &self,
        program: &crate::core::CheckedProgram,
    ) -> Result<(), Vec<crate::diagnostic::Diagnostic>> {
        let dynamic_any_roots = self.native_dynamic_any_preflight_roots(program);
        let dynamic_any_unpack = super::resolved::first_reachable_dynamic_any_unpack_from_roots(
            program,
            &dynamic_any_roots,
        );
        if let Some((owner, node)) = dynamic_any_unpack {
            return Err(vec![super::resolved::dynamic_any_unpack_diagnostic(
                program, &owner, &node,
            )]);
        }
        Ok(())
    }

    /// Return the Checker-owned entry roots for native DynamicAny preflight.
    /// Reachability from these roots is resolved through `CheckedProgram`'s
    /// call-site directory by the shared scanner; dormant imported helpers do
    /// not become executable roots merely because their module was loaded.
    fn native_dynamic_any_preflight_roots(
        &self,
        program: &crate::core::CheckedProgram,
    ) -> std::collections::BTreeSet<crate::core::NodeId> {
        let mut roots = std::collections::BTreeSet::new();
        if self.shared {
            roots.extend(
                program
                    .callables()
                    .keys()
                    .filter(|owner| {
                        !program
                            .functions()
                            .get(*owner)
                            .is_some_and(|function| function.is_comptime)
                    })
                    .cloned(),
            );
            return roots;
        }

        roots.extend(
            program
                .functions()
                .values()
                .filter(|function| {
                    !function.is_comptime
                        && (function.qualified_name == "main"
                            || function.qualified_name.ends_with("::main")
                            || function.qualified_name.ends_with(":main"))
                        && program.callable(&function.node_id).is_some()
                })
                .map(|function| function.node_id.clone()),
        );
        roots.extend(
            program
                .native_declarations()
                .exports
                .keys()
                .filter(|owner| program.callable(owner).is_some())
                .cloned(),
        );
        roots
    }

    /// Admit the narrow dormant-import exception only when every Map owner
    /// signal belongs to an unreachable callable from the exact checker-owned
    /// stdlib maps source. Dormant user helpers and unknown dispatch must keep
    /// their ordinary fail-closed admission result.
    fn has_only_dormant_std_maps_owner_signal(
        &self,
        program: &crate::core::CheckedProgram,
    ) -> bool {
        fn analysis_has_map_owner_signal(analysis: &crate::core::ResourceAnalysis) -> bool {
            !analysis.map_root_actions.is_empty()
                || !analysis.map_root_new_attempts.is_empty()
                || !analysis.map_any_owner_actions.is_empty()
                || !analysis.map_any_owner_new_attempts.is_empty()
                || !analysis.ordinary_map_owner_lifecycle_v1_actions.is_empty()
                || !analysis
                    .ordinary_map_owner_lifecycle_v1_new_attempts
                    .is_empty()
                || !analysis
                    .ordinary_map_owner_lifecycle_v1_operation_attempts
                    .is_empty()
                || !analysis.ordinary_map_owner_observer_v1_attempts.is_empty()
                || !analysis.ordinary_map_owner_observer_v1_receipts.is_empty()
                || !analysis
                    .ordinary_map_owned_get_i32_string_v1_actions
                    .is_empty()
                || !analysis
                    .ordinary_map_owned_get_i32_string_v1_new_attempts
                    .is_empty()
                || !analysis
                    .ordinary_map_owned_get_i32_string_v1_operation_attempts
                    .is_empty()
                || analysis
                    .ordinary_map_owned_get_i32_string_v1_cfg_receipt
                    .is_some()
                || !analysis.ordinary_map_owned_any_v2_actions.is_empty()
                || !analysis
                    .ordinary_map_owned_any_v2_operation_attempts
                    .is_empty()
                || analysis.ordinary_map_owned_any_v2_cfg_receipt.is_some()
        }

        let roots = self.native_dynamic_any_preflight_roots(program);
        let reachable = super::resolved::reachable_callable_ids_from_roots(program, &roots);
        let mut found_dormant_maps_signal = false;
        for (owner, analysis) in program.resource_analyses() {
            if !analysis_has_map_owner_signal(analysis) {
                continue;
            }
            if reachable.contains(owner) || !super::resolved::is_std_maps_callable(program, owner) {
                return false;
            }
            found_dormant_maps_signal = true;
        }
        found_dormant_maps_signal
    }

    fn install_checked_actor_metadata(&mut self, program: &crate::core::CheckedProgram) {
        let mut actors = std::collections::HashMap::new();
        for actor in program.actors().values() {
            actors.insert(actor.qualified_name.clone(), actor.methods.clone());
        }
        self.resolved_actors = Some(actors);

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
    }

    fn compile_checked_inner(
        &mut self,
        program: &crate::core::CheckedProgram,
    ) -> Result<(), Vec<crate::diagnostic::Diagnostic>> {
        program.validate_backend(crate::core::BackendProfile::Native)?;
        if program.dynamic_protocol_method_boundary_reason().is_some() {
            return Err(vec![crate::diagnostic::Diagnostic::error_code(
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                "dynamic Protocol method dispatch through a `dyn Trait` receiver is removed by language-spec §3.9; checked native consumers cannot use a compatibility fallback",
                program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
            )]);
        }
        // Keep the public CheckedProgram API on the same FFI boundary as CLI
        // dispatch. `Compatibility` from canonical MIR means this declaration
        // is outside the migrated scalar C ABI; treating that as permission to
        // enter the resolved/legacy emitters would silently broaden the FFI
        // profile and bypass its checker-owned receipt.
        if let Some(reason) = crate::core::mir::scalar_ffi_boundary_reason(program) {
            return Err(vec![crate::diagnostic::Diagnostic::error_code(
                crate::core::mir::MIR_FFI_DECLARATION_BOUNDARY_ERROR_CODE,
                format!("canonical scalar FFI declaration boundary: {reason}"),
                program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
            )]);
        }
        // DynamicAnyUnpack currently carries only an erased i64 ABI: neither
        // the Resolved emitter nor the legacy remainder checks that the
        // stored runtime value actually matches the concrete generic type.
        // Treat every reachable instance as a hard native boundary before
        // MIR selection or per-function fallback can choose either emitter.
        let checked_v2_unpack_candidate = match self.preflight_native_dynamic_any_unpack(program) {
            Ok(()) => false,
            Err(original_error) => {
                let admission = crate::core::mir::classify_canonical_mir_route_admission(program);
                if !admission.ordinary_map_owned_any_v2_complete()
                    && !admission.ordinary_map_owned_any_v2_zero_miss_i32()
                    && !admission.ordinary_map_owned_any_v2_get_or_default_string()
                    && !admission.ordinary_map_owned_any_v2_set_get_any_result_i32()
                    && !admission
                        .ordinary_map_owned_any_v2_values_result_crosscall_string_caller_v3()
                    && !admission
                        .ordinary_map_owned_any_v2_values_result_crosscall_string_caller_v4()
                    && !admission.ordinary_map_owned_any_v2_persistent_root_alias_replacement()
                {
                    // The reachable unpack itself is the precise safety
                    // boundary. An unrelated incomplete owner-v2 operation
                    // must not replace its E0722 with a broader route-coverage
                    // diagnostic.
                    return Err(original_error);
                }
                // Defer the exception until canonical MIR materialization
                // below. A complete source profile alone cannot authorize an
                // erased Any cast or a compatibility emitter.
                true
            }
        };
        // S12/S15/S25/S30/R6-15/R6-1163g: the scalar FFI, S8 Flow, scalar
        // collection, flat Copy-record, MapRoot, exact non-Copy Option<string>,
        // exact nested Option-tuple, generic Option/Result projections and
        // predicates, ManagedResultCall, and SessionChannel have crossed the
        // default route boundary. This public API is also an old production
        // entry point, so an admitted graph must not continue into the old AST
        // body compiler merely because a caller bypassed the CLI selector.
        // The helper performs the same whole-program, all-consumer preflight as the selector and
        // returns only after the canonical native consumer is ready.  If
        // canonical lowering has not materialized one of these candidates,
        // this remains the compatibility path for unrelated legacy programs.
        // The shared default selector gives unsupported exported C wrapper
        // bodies an explicit compatibility disposition before considering
        // other closed islands. Preserve that precedence for direct
        // `compile_checked` callers too: an unrelated MapRoot/record candidate
        // must not make MIR materialization fail on the export body after the
        // caller has entered the compatibility API.
        // An ordinary Map operation attempt is a whole-program ownership
        // candidate.  If the checker could not seal its operation ledger,
        // do not let the resolved emitter (or the retired legacy remainder)
        // reinterpret the same erased Map/Any payload.  In particular this
        // covers record-valued `map_set`: the current canonical profile owns
        // only i32/String entries, so this shape must be an explicit,
        // checker-owned coverage result until a record descriptor/owner
        // contract exists.
        let admission = crate::core::mir::classify_canonical_mir_route_admission(program);
        if let Some(error) =
            crate::core::mir::scalar_export_map_owner_composition_failure(admission)
        {
            return Err(vec![error.to_diagnostic()]);
        }
        // The coarse admission summary includes imported maps helpers. Only
        // exact stdlib:maps.mimi bodies proven unreachable from native roots
        // may contribute the dormant signal ignored here. A dormant user Map
        // helper or an uncertain dispatch retains the normal coverage error.
        let incomplete_owned_get_is_dormant_only = admission.ordinary_map_owned_get_i32_string_v1
            == crate::core::mir::OrdinaryMapOwnedGetI32StringV1Admission::IncompleteCoverage
            && self.has_only_dormant_std_maps_owner_signal(program);
        if admission.ordinary_map_owned_any_v2_get_or_default_string() {
            let excluded_sources = program
                .source_registry()
                .records()
                .iter()
                .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
                .map(|record| record.id)
                .collect::<HashSet<_>>();
            let excluded_sources = (!excluded_sources.is_empty()).then_some(&excluded_sources);
            let route =
                crate::core::mir::materialize_canonical_mir_route(program, excluded_sources)
                    .map_err(|error| vec![error.to_diagnostic()])?;
            let receipt =
                crate::core::mir::validate_ordinary_map_owned_any_v2_get_or_default_string_route(
                    program,
                    &route.program,
                )
                .map_err(|message| {
                    vec![crate::diagnostic::Diagnostic::error_code(
                        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                        format!("GetOrDefault<String> native route rejected: {message}"),
                        program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                    )]
                })?;
            if !route
                .admission
                .ordinary_map_owned_any_v2_get_or_default_string()
                || receipt.profile
                    != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_STRING_PROFILE
                || receipt
                    != route.program.route_receipt(
                        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_STRING_PROFILE,
                    )
            {
                return Err(vec![crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    "MIR-COVERAGE-001: GetOrDefault<String> native route lacks its exact profile receipt for this MIR digest",
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )]);
            }
            crate::core::mir::validate_ordinary_map_owned_any_v2_get_or_default_string_mir_route(
                &route.program,
            )
            .map_err(|message| {
                vec![crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    format!("GetOrDefault<String> native MIR route rejected: {message}"),
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )]
            })?;
            self.install_checked_actor_metadata(program);
            return self
                .compile_checked_mir_native_with_ordinary_map_owned_any_v2_get_or_default_string_route(
                    program,
                    &route.program,
                    &receipt,
                );
        }
        if admission.ordinary_map_owned_any_v2_values_result_crosscall_string_caller_v4() {
            let excluded_sources = program
                .source_registry()
                .records()
                .iter()
                .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
                .map(|record| record.id)
                .collect::<HashSet<_>>();
            let excluded_sources = (!excluded_sources.is_empty()).then_some(&excluded_sources);
            let route =
                crate::core::mir::materialize_canonical_mir_route(program, excluded_sources)
                    .map_err(|error| vec![error.to_diagnostic()])?;
            let receipt = crate::core::mir::validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route(
                program,
                &route.program,
            )
            .map_err(|message| {
                vec![crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    format!(
                        "ValuesResultCrosscallStringCallerIndex1 V4 native route rejected: {message}"
                    ),
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )]
            })?;
            let profile = crate::core::mir::CanonicalMirRouteProfile::
                OrdinaryMapOwnedAnyV2ValuesResultCrosscallStringCallerIndex1
                .as_str();
            if !route
                .admission
                .ordinary_map_owned_any_v2_values_result_crosscall_string_caller_v4()
                || receipt.profile != profile
                || route
                    .ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route_receipt
                    .as_ref()
                    != Some(&receipt)
                || receipt != route.program.route_receipt(profile)
            {
                return Err(vec![crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    "MIR-COVERAGE-001: ValuesResultCrosscallStringCallerIndex1 V4 native route lacks its exact receipt for this MIR digest",
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )]);
            }
            crate::core::mir::validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_index1_mir_route(
                &route.program,
            )
            .map_err(|message| {
                vec![crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    format!(
                        "ValuesResultCrosscallStringCallerIndex1 V4 native MIR route rejected: {message}"
                    ),
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )]
            })?;
            crate::interp::bytecode::compile_mir_program_with_values_result_crosscall_string_caller_index1_route(
                program,
                &route.program,
            )
            .map_err(|errors| {
                vec![crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    format!("ValuesResultCrosscallStringCallerIndex1 V4 Bytecode route rejected: {errors:?}"),
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )]
            })?;
            crate::codegen::mir::validate_checked_mir_native_values_result_crosscall_string_caller_index1_route(
                program,
                &route.program,
                &receipt,
            )
            .map_err(|errors| {
                vec![crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    format!("ValuesResultCrosscallStringCallerIndex1 V4 native capability rejected: {errors:?}"),
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )]
            })?;
            crate::verifier::validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_index1_route(
                program,
                &route.program,
                &receipt,
            )
            .map_err(|errors| {
                vec![crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    format!("ValuesResultCrosscallStringCallerIndex1 V4 verifier capability rejected: {errors:?}"),
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )]
            })?;
            let results = crate::verifier::verify_mir_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_index1_route(
                program,
                &route.program,
                &receipt,
                String::new(),
            )
            .map_err(|message| {
                vec![crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    format!("ValuesResultCrosscallStringCallerIndex1 V4 verifier rejected: {message}"),
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )]
            })?;
            if !crate::verifier::canonical_execution_route_verifier_ready(&results, false, false) {
                return Err(vec![crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    format!("ValuesResultCrosscallStringCallerIndex1 V4 verifier did not prove the route: {results:?}"),
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )]);
            }
            self.install_checked_actor_metadata(program);
            return self
                .compile_checked_mir_native_with_values_result_crosscall_string_caller_index1_route(
                    program,
                    &route.program,
                    &receipt,
                );
        }
        if admission.ordinary_map_owned_any_v2_values_result_crosscall_string_caller_v3() {
            let excluded_sources = program
                .source_registry()
                .records()
                .iter()
                .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
                .map(|record| record.id)
                .collect::<HashSet<_>>();
            let excluded_sources = (!excluded_sources.is_empty()).then_some(&excluded_sources);
            let route =
                crate::core::mir::materialize_canonical_mir_route(program, excluded_sources)
                    .map_err(|error| vec![error.to_diagnostic()])?;
            let receipt = crate::core::mir::validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route(
                program,
                &route.program,
            )
            .map_err(|message| {
                vec![crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    format!("ValuesResultCrosscallStringCaller V3 native route rejected: {message}"),
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )]
            })?;
            let profile = crate::core::mir::CanonicalMirRouteProfile::
                OrdinaryMapOwnedAnyV2ValuesResultCrosscallStringCaller
                .as_str();
            if !route
                .admission
                .ordinary_map_owned_any_v2_values_result_crosscall_string_caller_v3()
                || receipt.profile != profile
                || route
                    .ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route_receipt
                    .as_ref()
                    != Some(&receipt)
                || receipt != route.program.route_receipt(profile)
            {
                return Err(vec![crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    "MIR-COVERAGE-001: ValuesResultCrosscallStringCaller V3 native route lacks its exact receipt for this MIR digest",
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )]);
            }
            crate::core::mir::validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_mir_route(
                &route.program,
            )
            .map_err(|message| {
                vec![crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    format!("ValuesResultCrosscallStringCaller V3 native MIR route rejected: {message}"),
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )]
            })?;
            crate::interp::bytecode::compile_mir_program_with_values_result_crosscall_string_caller_route(
                program,
                &route.program,
            )
            .map_err(|errors| {
                vec![crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    format!("ValuesResultCrosscallStringCaller V3 Bytecode route rejected: {errors:?}"),
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )]
            })?;
            crate::codegen::mir::validate_checked_mir_native_values_result_crosscall_string_caller_route(
                program,
                &route.program,
                &receipt,
            )
            .map_err(|errors| {
                vec![crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    format!("ValuesResultCrosscallStringCaller V3 native capability rejected: {errors:?}"),
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )]
            })?;
            crate::verifier::validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route(
                program,
                &route.program,
                &receipt,
            )
            .map_err(|errors| {
                vec![crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    format!("ValuesResultCrosscallStringCaller V3 verifier capability rejected: {errors:?}"),
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )]
            })?;
            let results = crate::verifier::verify_mir_with_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route(
                program,
                &route.program,
                &receipt,
                String::new(),
            )
            .map_err(|message| {
                vec![crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    format!("ValuesResultCrosscallStringCaller V3 verifier rejected: {message}"),
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )]
            })?;
            if !crate::verifier::canonical_execution_route_verifier_ready(&results, false, false) {
                return Err(vec![crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    format!("ValuesResultCrosscallStringCaller V3 verifier did not prove the route: {results:?}"),
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )]);
            }
            self.install_checked_actor_metadata(program);
            return self
                .compile_checked_mir_native_with_values_result_crosscall_string_caller_route(
                    program,
                    &route.program,
                    &receipt,
                );
        }
        if admission.ordinary_map_owned_any_v2_zero_miss_i32() {
            let excluded_sources = program
                .source_registry()
                .records()
                .iter()
                .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
                .map(|record| record.id)
                .collect::<HashSet<_>>();
            let excluded_sources = (!excluded_sources.is_empty()).then_some(&excluded_sources);
            let route =
                crate::core::mir::materialize_canonical_mir_route(program, excluded_sources)
                    .map_err(|error| vec![error.to_diagnostic()])?;
            let expected_receipt = route
                .ordinary_map_owned_any_v2_zero_miss_i32_route_receipt
                .as_ref()
                .ok_or_else(|| {
                    vec![crate::diagnostic::Diagnostic::error_code(
                        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                        "MIR-COVERAGE-001: missing exact ZeroMissI32 native route receipt",
                        program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                    )]
                })?;
            if !route.admission.ordinary_map_owned_any_v2_zero_miss_i32()
                || !route.materialized_ordinary_map_owned_any_v2_zero_miss_i32_candidate
                || expected_receipt.profile
                    != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE
                || expected_receipt
                    != &route.program.route_receipt(
                        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE,
                    )
            {
                return Err(vec![crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    "MIR-COVERAGE-001: ZeroMissI32 native route has no exact candidate/receipt for this MIR digest",
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )]);
            }
            crate::core::mir::validate_ordinary_map_owned_any_v2_zero_miss_i32_route(
                program,
                &route.program,
            )
            .and_then(|()| {
                crate::core::mir::validate_ordinary_map_owned_any_v2_zero_miss_i32_mir_route(
                    &route.program,
                )
            })
            .map_err(|message| {
                vec![crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    format!("MIR-COVERAGE-001: ZeroMissI32 native route rejected: {message}"),
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )]
            })?;
            self.install_checked_actor_metadata(program);
            return self
                .compile_checked_mir_native_with_ordinary_map_owned_any_v2_zero_miss_i32_route(
                    program,
                    &route.program,
                    expected_receipt,
                );
        }
        if admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::MapEmptyRootRootQueriesV1
        {
            let route = crate::core::mir::materialize_canonical_mir_route(program, None)
                .map_err(|e| vec![e.to_diagnostic()])?;
            let receipt = route
                .map_empty_root_root_queries_v1_route_receipt
                .as_ref()
                .ok_or_else(|| {
                    vec![crate::diagnostic::Diagnostic::error_code(
                        "MIR-COVERAGE-001",
                        "missing exact empty-root production receipt",
                        crate::span::Span::UNKNOWN,
                    )]
                })?;
            crate::interp::bytecode::compile_mir_program_with_empty_root_root_queries_v1_route(
                program,
                &route.program,
            )
            .map_err(|e| {
                Self::mir_gate_diagnostics(
                    program,
                    "MIR bytecode",
                    crate::core::mir::MAP_EMPTY_ROOT_ROOT_QUERIES_V1_PROFILE,
                    e,
                )
            })?;
            let results = crate::verifier::verify_mir_with_map_empty_root_root_queries_v1_route(
                program,
                &route.program,
                receipt,
                String::new(),
            )
            .map_err(|e| {
                Self::mir_gate_diagnostics(
                    program,
                    "MIR verifier",
                    crate::core::mir::MAP_EMPTY_ROOT_ROOT_QUERIES_V1_PROFILE,
                    e,
                )
            })?;
            if !crate::verifier::canonical_execution_route_verifier_ready(&results, false, false) {
                return Err(Self::mir_gate_diagnostics(
                    program,
                    "MIR verifier",
                    crate::core::mir::MAP_EMPTY_ROOT_ROOT_QUERIES_V1_PROFILE,
                    format!("{results:?}"),
                ));
            }
            return self.compile_checked_mir_native_with_empty_root_root_queries_v1_route(
                program,
                &route.program,
                receipt,
            );
        }

        // A generic declaration is a template, not an executable native body.
        // The canonical route must own every reachable generic call with a
        // concrete instance receipt.  If no generic island admitted this
        // checked program, stop here before `supports_resolved_native` can
        // enter the resolved emitter and report the older E0722 ABI failure.
        // DynamicAny calls are checked above first so their established E0722
        // tag-safety diagnostic remains the more precise boundary.
        let has_generic_function_call = program.call_sites_sorted().into_iter().any(|site| {
            let Some(crate::core::ir::ResolvedCallee::Function(target)) =
                site.resolved_callee.as_ref()
            else {
                return false;
            };
            program
                .functions()
                .get(target)
                .is_some_and(|function| !function.generics.is_empty())
        });
        let generic_route_complete = admission.scalar_generic_identity_i32_complete()
            || admission.scalar_generic_identity_i64_complete()
            || admission.generic_variant_complete()
            || admission.generic_option_projection_complete()
            || admission.generic_option_projection_fallback_complete()
            || admission.generic_result_projection_complete()
            || admission.generic_result_projection_fallback_complete()
            || admission.managed_result_call_complete()
            || admission.map_any_owner_complete();
        if has_generic_function_call && !generic_route_complete {
            return Err(vec![crate::diagnostic::Diagnostic::error_code(
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                "MIR-COVERAGE-001/NITS: reachable generic call has no complete canonical instance receipt; legacy and erased resolved generic bodies are retired",
                program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
            )]);
        }
        if crate::core::mir::has_scalar_actor_nested_callable_candidate(program)
            && !admission.scalar_actor
        {
            return Err(vec![crate::diagnostic::Diagnostic::error_code(
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                "nested Actor callable candidate has no complete canonical lexical-environment receipt; legacy Actor fallback is disabled",
                program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
            )]);
        }
        if let Some(reason) =
            crate::core::mir::unsupported_nested_callable_boundary_reason(program, None)
        {
            return Err(vec![crate::diagnostic::Diagnostic::error_code(
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                reason,
                program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
            )]);
        }
        if admission
            .ordinary_map_owned_get_i32_string_v1
            .has_profile_signal()
            && !admission.ordinary_map_owned_get_i32_string_v1_complete()
            && !incomplete_owned_get_is_dormant_only
        {
            return Err(vec![crate::diagnostic::Diagnostic::error_code(
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                "Checker recognized an ordinary Map owned-get attempt without complete consumer coverage",
                program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
            )]);
        }
        let ordinary_map_compatibility = admission
            .ordinary_map_owner_lifecycle_i32_string_v1
            .has_profile_signal()
            && !admission.ordinary_map_owner_lifecycle_i32_string_v1_complete()
            && !admission.map_any_owner.is_candidate()
            && !admission.ordinary_map_owned_get_i32_string_v1_complete();
        if ordinary_map_compatibility {
            return Err(vec![crate::diagnostic::Diagnostic::error_code(
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                "MIR-COVERAGE-001/NITS: ordinary Map owner receipt is incomplete; record-valued map_set and other unsealed Map/Any operations have no canonical ownership consumer",
                program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
            )]);
        }
        if let Some(reason) = crate::core::mir::exported_abi_body_boundary_reason(program) {
            return Err(vec![crate::diagnostic::Diagnostic::error_code(
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                format!(
                    "{reason}; aggregate, generic, variadic, errno, parameter-mode, async, and contract-bearing exports remain outside the trusted canonical scalar subset"
                ),
                program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
            )]);
        }
        if crate::core::mir::exported_abi_body_boundary_reason(program).is_none()
            && crate::core::mir::scalar_actor_contract_boundary_reason(program).is_none()
            && !ordinary_map_compatibility
            && !incomplete_owned_get_is_dormant_only
        {
            if let Some(canonical) = self.try_compile_exact_migrated_mir_island(program)? {
                let persistent_root_alias_replacement =
                    admission.ordinary_map_owned_any_v2_persistent_root_alias_replacement();
                if checked_v2_unpack_candidate && !persistent_root_alias_replacement {
                    if let Err(message) =
                        super::mir::checked_managed_any_unpack_coverage(program, &canonical)
                    {
                        return Err(vec![crate::diagnostic::Diagnostic::error_code(
                            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                            format!(
                                "ordinary Map/Any owner-v2 unpack lacks complete checked MIR coverage: {message}"
                            ),
                            program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                        )]);
                    }
                }
                self.install_checked_actor_metadata(program);
                if canonical.functions().values().any(|function| {
                    function
                        .ownership
                        .ordinary_map_owned_get_result_branch_v3_receipt()
                        .is_some()
                }) {
                    let receipt = canonical.route_receipt(
                        crate::core::mir::ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND,
                    );
                    return self
                        .compile_checked_mir_native_with_ordinary_map_owned_get_result_branch_v3_route(
                            program,
                            &canonical,
                            &receipt,
                        );
                }
                if persistent_root_alias_replacement {
                    let receipt = canonical.route_receipt(
                        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE,
                    );
                    return self
                        .compile_checked_mir_native_with_persistent_root_alias_replacement_route(
                            program, &canonical, &receipt,
                        );
                }
                let receipt_profile = if admission.ordinary_map_owned_any_v2.is_direct_print() {
                    // The single direct-print profile has a distinct native
                    // consumer receipt. Never label a merely materialized V2
                    // graph as DirectPrint: recheck the exact MIR shape before
                    // passing that receipt to the production emitter.
                    crate::core::mir::validate_ordinary_map_owned_any_v2_direct_print_mir_route(
                        &canonical,
                    )
                    .map_err(|message| {
                        vec![crate::diagnostic::Diagnostic::error_code(
                            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                            format!(
                                "ordinary Map/Any owner-v2 DirectPrint native route rejected: {message}"
                            ),
                            program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                        )]
                    })?;
                    crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE
                } else {
                    crate::core::mir::MIR_NATIVE_DIRECT_ROUTE_PROFILE
                };
                let receipt = canonical.route_receipt(receipt_profile);
                return self.compile_mir_native_with_route_receipt(&canonical, &receipt);
            }
            if has_static_generic_trait_specialization_candidate(program) {
                return self.compile_static_generic_trait_specializations(program);
            }
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
        let tables = program
            .native_declarations()
            .build_transition_tables(program.resolved_types());
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
        self.install_checked_actor_metadata(program);
        self.resolved_capabilities = Some(
            program
                .capabilities()
                .values()
                .map(|capability| capability.qualified_name.clone())
                .collect(),
        );
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
        for block in program.extern_blocks_sorted() {
            for func in &block.funcs {
                extern_funcs.insert(func.clone());
                extern_abis
                    .entry(func.clone())
                    .or_insert_with(|| block.abi.clone());
            }
        }
        self.resolved_extern_funcs = Some(extern_funcs);
        self.resolved_extern_abis = Some(extern_abis);
        let mut extern_signatures = std::collections::HashMap::new();
        let mut extern_params = std::collections::HashMap::new();
        for block in program.extern_blocks_sorted() {
            for sig in crate::core::CheckedProgram::extern_signatures_sorted(block) {
                extern_signatures
                    .entry(sig.name.clone())
                    .or_insert_with(|| (sig.params.len(), sig.ret.clone()));
                extern_params
                    .entry(sig.name.clone())
                    .or_insert_with(|| sig.params.clone());
            }
        }
        self.resolved_extern_signatures = Some(extern_signatures);
        self.resolved_extern_params = Some(extern_params);
        let mut extern_no_panic = std::collections::HashSet::new();
        let mut extern_unsafe = std::collections::HashSet::new();
        for block in program.extern_blocks_sorted() {
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
            let mut imported_externs = std::collections::HashSet::new();
            // 0.31.30+: scan user extern blocks and register as imports.
            // This makes the Component IR aware of user-declared extern
            // functions, enabling bindgen backends to generate complete
            // bindings that include both runtime exports and user imports.
            for block in program.extern_blocks_sorted() {
                for sig in crate::core::CheckedProgram::extern_signatures_sorted(block) {
                    if !imported_externs.insert(sig.name.clone()) {
                        return Err(vec![crate::diagnostic::Diagnostic::error_code(
                            crate::diagnostic::codes::E0402,
                            format!(
                                "duplicate resolved extern symbol '{}' while building component ABI",
                                sig.name
                            ),
                            sig.span,
                        )]);
                    }
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
        // fallback. A body class without a complete canonical slice is
        // rejected below with MIR-COVERAGE-001; production never reopens the
        // retained surface AST.
        // Generic checked ProtocolMethod calls currently have exact checker
        // receipts but no materialized concrete native instance ABI. The
        // resolved emitter parses the declaration symbol and uses erased Any
        // handles, while the legacy body fallback may miss the AST call-span
        // receipt. Stop before either compatibility emitter can produce a
        // mismatched call. A complete canonical MIR route returned above is
        // allowed to own this shape once it materializes the exact instance.
        self.install_checked_declaration_names(program).map_err(|error| {
            vec![crate::diagnostic::Diagnostic::error_code(
                "NATIVE-CHECKED-METHOD-RECEIPT-001",
                format!("native generic-method preflight could not install exact checked receipts: {error}"),
                program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
            )]
        })?;
        for site in program.call_sites_sorted() {
            if !matches!(
                site.resolved_callee.as_ref(),
                Some(crate::core::ir::ResolvedCallee::ProtocolMethod { .. })
            ) {
                continue;
            }
            let span = site.origin.user_span();
            let checked_span = (
                span.source_id.raw(),
                span.start_line,
                span.start_col,
                span.end_line,
                span.end_col,
            );
            let Some(target) = self
                .checked_trait_method_dispatch
                .call_targets
                .get(&checked_span)
                .and_then(|targets| {
                    targets
                        .iter()
                        .find(|target| !target.key.type_arguments.is_empty())
                })
            else {
                continue;
            };
            return Err(vec![crate::diagnostic::Diagnostic::error_code(
                "NATIVE-GENERIC-PROTOCOL-ABI-001",
                format!(
                    "native codegen cannot emit checked generic ProtocolMethod '{}' until a concrete MIR instance owns its ABI; stopped before resolved or legacy compatibility emission",
                    target.key.callable.0
                ),
                span,
            )]);
        }
        if incomplete_owned_get_is_dormant_only {
            // Select and validate one complete native cohort before declaring
            // any body. This production path skips only dormant stdlib maps
            // validation; every other helper keeps the strict native boundary.
            let native_roots = self.native_dynamic_any_preflight_roots(program);
            return self.compile_resolved_native_reachable(program, &native_roots);
        }
        if super::resolved::supports_resolved_native(program) {
            return self.compile_resolved_native(program);
        }
        // No canonical route admitted this checked program. The former
        // per-function dispatch retained a surface AST snapshot here and sent
        // the ineligible remainder to the old AST function emitter. That
        // native surface-AST emitter has been physically removed. Return a
        // checker-owned coverage result before any body accessor can run.
        Err(vec![crate::diagnostic::Diagnostic::error_code(
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            "MIR-COVERAGE-001/NITS: native surface-AST emission has been removed; this checked program has no complete canonical native route",
            program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
        )])
    }

    fn compile_static_generic_trait_specializations(
        &mut self,
        program: &crate::core::CheckedProgram,
    ) -> Result<(), Vec<crate::diagnostic::Diagnostic>> {
        let span = program.entry_span().unwrap_or(crate::span::Span::UNKNOWN);
        if let Some(reason) = crate::core::mir::unknown_effect_boundary_reason(program) {
            return Err(vec![crate::diagnostic::Diagnostic::error_code(
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                reason,
                span,
            )]);
        }
        let excluded_sources = program
            .source_registry()
            .records()
            .iter()
            .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
            .map(|record| record.id)
            .collect::<HashSet<_>>();
        let canonical =
            crate::core::mir::reference::MirProgram::from_checked_program_excluding_sources(
                program,
                &excluded_sources,
            )
            .map_err(|error| error.to_diagnostics())?;
        crate::verifier::validate_mir_capabilities(&canonical).map_err(|errors| {
            Self::mir_gate_diagnostics(
                program,
                "MIR verifier capability",
                STATIC_GENERIC_TRAIT_SPECIALIZATION_ROUTE_PROFILE,
                errors,
            )
        })?;
        let receipt = canonical.route_receipt(STATIC_GENERIC_TRAIT_SPECIALIZATION_ROUTE_PROFILE);
        crate::interp::bytecode::compile_mir_program_with_route_receipt(&canonical, &receipt)
            .map_err(|errors| {
                Self::mir_gate_diagnostics(
                    program,
                    "MIR bytecode",
                    STATIC_GENERIC_TRAIT_SPECIALIZATION_ROUTE_PROFILE,
                    errors,
                )
            })?;
        crate::codegen::mir::validate_mir_native(&canonical)?;
        let results =
            crate::verifier::verify_mir_with_route_receipt(&canonical, &receipt, String::new())
                .map_err(|error| {
                    vec![crate::diagnostic::Diagnostic::error_code(
                        "MIR-VERIFY-001",
                        format!("MIR generic trait specialization verifier pass failed: {error}"),
                        span,
                    )]
                })?;
        if !crate::verifier::canonical_execution_route_verifier_ready(
            &results,
            crate::core::mir::is_exact_cross_state_f64_failure_receipt(program),
            crate::core::mir::contains_multi_target_flow_union_candidate(&canonical),
        ) {
            return Err(vec![crate::diagnostic::Diagnostic::error_code(
                "MIR-VERIFY-001",
                "MIR verifier returned an unsupported or inconclusive result for static generic trait specialization",
                span,
            )]);
        }
        self.compile_mir_native_with_route_receipt(&canonical, &receipt)
    }

    /// Probe the direct native boundary for already closed MIR islands.
    ///
    /// The probe is deliberately checker-admission-first: an unrelated or
    /// mixed compatibility program may remain on the old API, but complete
    /// flat Copy-record admission crosses a hard MIR materialization boundary.
    /// Once a candidate is materialized, an island or consumer failure is also
    /// a hard error; it is never converted into a legacy compile.
    fn try_compile_exact_migrated_mir_island(
        &self,
        program: &crate::core::CheckedProgram,
    ) -> Result<Option<crate::core::mir::reference::MirProgram>, Vec<crate::diagnostic::Diagnostic>>
    {
        // The shared core-MIR envelope performs checker-owned admission,
        // canonical construction, and operation materialization before this
        // direct native entry can approach LLVM.  Complete failures are hard;
        // Mixed/Outside remains an explicit compatibility boundary only when
        // no typed candidate was recognized; a recognized candidate that fails
        // before materialization is still fail-closed.
        // The checker keeps stdlib prelude callables in the same typed
        // program, but the production MIR route intentionally excludes that
        // compatibility source.  Otherwise admitting a user-facing Float
        // island would force unrelated legacy prelude arithmetic through the
        // narrow native Float contract and poison the graph before emission.
        let admission = crate::core::mir::classify_canonical_mir_route_admission(program);
        let excluded_sources = if admission.scalar_export
            || admission.scalar_actor
            || admission.scalar_nested
            || admission.copy_option_f64_complete()
            || admission.map_root_complete()
            // R6-1091 parity: a complete copy-Option (i32/bool/i64) admission
            // must see the same prelude-free graph the CLI dispatch wrapper
            // builds. Its recognized match face (R6-1089) otherwise dies on
            // preloaded prelude bodies MIR Phase 0 cannot lower before the
            // island validator ever runs.
            // R6-1133s/t: both complete scalar identity admissions also use
            // the production prelude-free canonical graph.
            || admission.scalar_generic_identity_i32_complete()
            || admission.scalar_generic_identity_i64_complete()
            || admission.copy_option_i32_complete()
            || admission.copy_option_bool_complete()
            || admission.copy_option_i64_complete()
            || admission.copy_result_i32_complete()
            || admission.generic_variant_complete()
            || admission.generic_option_projection_complete()
            || admission.generic_option_projection_fallback_complete()
            || admission.generic_result_projection_complete()
            || admission.generic_result_projection_fallback_complete()
            || admission.managed_result_call_complete()
            || admission.session_complete()
            || admission.flow_failure_retry
            || admission.flow_complete()
            || admission.ordinary_map_owned_any_v2.is_direct_print()
            || admission.ordinary_map_owned_any_v2_persistent_root_alias_replacement()
            || admission.ordinary_map_owned_any_v2_branch_local_set_diamond()
            || admission.map_any_owner_complete()
            || admission.ordinary_map_owned_any_v2_complete()
            || admission.ordinary_map_owned_get_i32_string_v1_complete()
            || admission.ordinary_map_owner_lifecycle_i32_string_v1_complete()
            // R6-1048 parity: a complete flat Copy-record admission must see
            // the same prelude-free graph the CLI dispatch wrapper builds.
            // Without the exclusion, preloaded prelude bodies carrying Assign
            // statements (repeat_action/times) hard-fail construction even
            // though dispatch never lowers them.
            || admission.record_complete()
        {
            program
                .source_registry()
                .records()
                .iter()
                .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
                .map(|record| record.id)
                .collect::<HashSet<_>>()
        } else {
            HashSet::new()
        };
        let excluded_sources = (!excluded_sources.is_empty()).then_some(&excluded_sources);
        let route = match crate::core::mir::materialize_canonical_mir_route(
            program,
            excluded_sources,
        ) {
            Ok(route) => route,
            Err(crate::core::mir::CanonicalMirRouteMaterializationError::Complete {
                profile,
                stage,
                message,
            }) => {
                let (code, detail) = match (profile, stage) {
                    (crate::core::mir::CanonicalMirRouteProfile::MapEmptyRootRootQueriesV1,
                     crate::core::mir::CanonicalMirRouteFailureStage::Construction) => ("MIR-LOWERING-001", format!("MapEmptyRootRootQueriesV1 construction failed: {message}")),
                    (crate::core::mir::CanonicalMirRouteProfile::MapEmptyRootRootQueriesV1,
                     crate::core::mir::CanonicalMirRouteFailureStage::Coverage) => ("MIR-COVERAGE-001", format!("MapEmptyRootRootQueriesV1 exact receipt failed: {message}")),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2SetGetAnyI32,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("Map/Any Set/GetAny-i32 MIR construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2SetGetAnyI32,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("Map/Any Set/GetAny-i32 exact receipt failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2SetGetAnyResultI32,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("Map/Any Set/GetAny-Result-i32 MIR construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2SetGetAnyResultI32,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("Map/Any Set/GetAny-Result-i32 exact receipt failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DynamicKeysGet,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("Map/Any DynamicKeysGet MIR construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DynamicKeysGet,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("Map/Any DynamicKeysGet exact receipt failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2CloneIsolation,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("Map/Any CloneIsolation MIR construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2CloneIsolation,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("Map/Any CloneIsolation exact receipt failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2PersistentRootAliasReplacement,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("Map/Any PersistentRootAliasReplacement MIR construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2PersistentRootAliasReplacement,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("Map/Any PersistentRootAliasReplacement exact receipt failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::ScalarActor,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => ("MIR-LOWERING-001", format!("complete scalar Actor construction failed: {message}")),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::ScalarActor,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => ("MIR-COVERAGE-001", format!("complete scalar Actor receipt failed: {message}")),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::MapRoot,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("complete MapRoot MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::MapRoot,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("complete MapRoot MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::MapAnyOwnerV1,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("complete MapAnyOwner v1 MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::MapAnyOwnerV1,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("complete MapAnyOwner v1 MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleI32StringV1,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map owner lifecycle MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleI32StringV1,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map owner lifecycle MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleObserverV1,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map owner lifecycle observer MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleObserverV1,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map owner lifecycle observer MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map owned-get MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map owned-get MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedGetAnyTupleL2Verifier,
                        stage,
                    ) => (
                        "MIR-CAPABILITY-001",
                        format!("VerifierOnly legacy tuple GetAny L2 profile cannot enter the build route ({stage:?}): {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2RecordUnit,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any RecordUnit MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2RecordUnit,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any RecordUnit exact receipt failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DirectPrint,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 DirectPrint MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DirectPrint,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 DirectPrint MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DirectPrintResult,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 DirectPrintResult MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DirectPrintResult,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 DirectPrintResult exact receipt failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2GetOrDefaultI32,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 GetOrDefaultI32 MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2GetOrDefaultI32,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 GetOrDefaultI32 MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2GetOrDefaultString,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 GetOrDefaultString MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2GetOrDefaultString,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 GetOrDefaultString MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2CheckedGet,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 CheckedGet MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2CheckedGet,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 CheckedGet MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ComputedKeyGet,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 ComputedKeyGet MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ComputedKeyGet,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 ComputedKeyGet MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChain,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 ComputedStringKeyOwnerChain MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChain,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 ComputedStringKeyOwnerChain MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ZeroMissI32,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 ZeroMissI32 MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ZeroMissI32,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 ZeroMissI32 MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2RemovedKeyCheckedMiss,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 RemovedKeyCheckedMiss MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2RemovedKeyCheckedMiss,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 RemovedKeyCheckedMiss MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesFieldMove,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 Values field-move MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesFieldMove,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 Values field-move MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesLocalIndexI32,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 Values local-index i32 MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesLocalIndexI32,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 Values local-index i32 MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultBranch,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 Values local-index i32 Result-branch MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultBranch,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 Values local-index i32 Result-branch MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesHeterogeneousStringIndex1,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 heterogeneous Values String index-one MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesHeterogeneousStringIndex1,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 heterogeneous Values String index-one MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2KeysSnapshot,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 Keys snapshot MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2KeysSnapshot,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 Keys snapshot MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesSnapshotReplacement,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 Values snapshot-replacement MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesSnapshotReplacement,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 Values snapshot-replacement MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesMixedSnapshot,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 Values mixed-snapshot MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesMixedSnapshot,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 Values mixed-snapshot MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesMixedSnapshotRemove,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 Values mixed-snapshot Remove MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesMixedSnapshotRemove,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 Values mixed-snapshot Remove MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesProducerCaller,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 Values producer-caller MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesProducerCaller,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 Values producer-caller MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 Values cross-call string-caller MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 Values cross-call string-caller MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 Values cross-call string-caller Index1 MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 Values cross-call string-caller Index1 MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootProducerCaller,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 MapRoot producer-caller MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootProducerCaller,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 MapRoot producer-caller MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootGetAnyCaller,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 MapRootGetAnyCaller MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootGetAnyCaller,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 MapRootGetAnyCaller MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2IsEmpty,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 IsEmpty MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2IsEmpty,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 IsEmpty MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapSetDropControl,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 MapSetDropControl MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapSetDropControl,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 MapSetDropControl MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapSetDropControlResult,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 MapSetDropControlResult MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapSetDropControlResult,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 MapSetDropControlResult MIR island materialization failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2BranchLocalSetDiamond,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("ordinary Map/Any owner v2 BranchLocalSetDiamond MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2BranchLocalSetDiamond,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("ordinary Map/Any owner v2 BranchLocalSetDiamond MIR island materialization failed: {message}"),
                    ),
                    (crate::core::mir::CanonicalMirRouteProfile::ScalarFfi, stage) => (
                        "MIR-FFI-COVERAGE-001",
                        format!("scalar FFI canonical MIR {stage:?} failed: {message}"),
                    ),
                    (crate::core::mir::CanonicalMirRouteProfile::ScalarNested, stage) => (
                        "MIR-NESTED-COVERAGE-001",
                        format!("scalar nested canonical MIR {stage:?} failed: {message}"),
                    ),
                    (crate::core::mir::CanonicalMirRouteProfile::ScalarExport, stage) => (
                        "MIR-EXPORT-COVERAGE-001",
                        format!("scalar export canonical MIR {stage:?} failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::ScalarGenericIdentityI32,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("generic scalar identity canonical MIR construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::ScalarGenericIdentityI32,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("generic scalar identity canonical MIR coverage failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::ScalarGenericIdentityI64,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("generic scalar identity i64 canonical MIR construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::ScalarGenericIdentityI64,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!("generic scalar identity i64 canonical MIR coverage failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::ScalarCollection,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!(
                            "complete scalar collection MIR island construction failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::ScalarCollection,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!(
                            "complete scalar collection MIR island materialization failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::FlatCopyRecord,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!(
                            "complete flat Copy-record MIR island construction failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::FlatCopyRecord,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!(
                            "complete flat Copy-record MIR island materialization failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::S8FlowTransition,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!("complete S8 Flow MIR island construction failed: {message}"),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::S8FlowTransition,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!(
                            "complete S8 Flow MIR island materialization failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::NonCopyOptionStringVariant,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!(
                            "complete Option<string> variant MIR island construction failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::NonCopyOptionStringVariant,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!(
                            "complete Option<string> variant MIR island materialization failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::NonCopyOptionNestedTupleVariant,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!(
                            "complete nested Option tuple variant MIR island construction failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::NonCopyOptionNestedTupleVariant,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!(
                            "complete nested Option tuple variant MIR island materialization failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::GenericOptionPredicate,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!(
                            "complete generic variant predicate MIR island construction failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::GenericOptionPredicate,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!(
                            "complete generic variant predicate MIR island materialization failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::GenericOptionProjection,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!(
                            "complete generic Option projection MIR island construction failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::GenericOptionProjection,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!(
                            "complete generic Option projection MIR island materialization failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::GenericOptionProjectionFallback,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!(
                            "complete generic Option fallback projection MIR island construction failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::GenericOptionProjectionFallback,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!(
                            "complete generic Option fallback projection MIR island materialization failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::GenericResultProjection,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!(
                            "complete generic Result projection MIR island construction failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::GenericResultProjection,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!(
                            "complete generic Result projection MIR island materialization failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::GenericResultProjectionFallback,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!(
                            "complete generic Result fallback projection MIR island construction failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::GenericResultProjectionFallback,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!(
                            "complete generic Result fallback projection MIR island materialization failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::ManagedResultCall,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!(
                            "complete managed Result direct-call MIR island construction failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::ManagedResultCall,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!(
                            "complete managed Result direct-call MIR island materialization failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::CopyOptionI32Variant,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!(
                            "complete Copy Option<i32> variant MIR island construction failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::CopyOptionI32Variant,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!(
                            "complete Copy Option<i32> variant MIR island materialization failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::CopyOptionBoolVariant,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!(
                            "complete Copy Option<bool> variant MIR island construction failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::CopyOptionBoolVariant,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!(
                            "complete Copy Option<bool> variant MIR island materialization failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::CopyOptionI64Variant,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!(
                            "complete Copy Option<i64> variant MIR island construction failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::CopyOptionI64Variant,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!(
                            "complete Copy Option<i64> variant MIR island materialization failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::CopyOptionF64Variant,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!(
                            "complete Copy Option<f64> variant MIR island construction failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::CopyOptionF64Variant,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!(
                            "complete Copy Option<f64> variant MIR island materialization failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::CopyResultI32Variant,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!(
                            "complete Copy Result<i32, i32> variant MIR island construction failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::CopyResultI32Variant,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!(
                            "complete Copy Result<i32, i32> variant MIR island materialization failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::SessionChannel,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!(
                            "complete session-channel MIR island construction failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::SessionChannel,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!(
                            "complete session-channel MIR island materialization failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::FlowFailureRetry,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!(
                            "complete recoverable Flow MIR island construction failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::FlowFailureRetry,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!(
                            "complete recoverable Flow MIR island materialization failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapDirectTupleGetUnpackL2Verifier,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!(
                            "direct tuple Get L2 VerifierOnly semantic projection cannot enter native code generation: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapDirectTupleGetUnpackL2Verifier,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!(
                            "direct tuple Get L2 VerifierOnly semantic projection has no native emission route: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesResultCrosscallStringCaller,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!(
                            "Values Result cross-call String caller V3 MIR construction failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesResultCrosscallStringCaller,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!(
                            "Values Result cross-call String caller V3 exact receipt failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesResultCrosscallStringCallerIndex1,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!(
                            "Values Result cross-call String caller V4 MIR construction failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesResultCrosscallStringCallerIndex1,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!(
                            "Values Result cross-call String caller V4 exact receipt failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesResultProducerCaller,
                        crate::core::mir::CanonicalMirRouteFailureStage::Construction,
                    ) => (
                        "MIR-LOWERING-001",
                        format!(
                            "Values Result producer-caller MIR construction failed: {message}"
                        ),
                    ),
                    (
                        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesResultProducerCaller,
                        crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
                    ) => (
                        "MIR-COVERAGE-001",
                        format!(
                            "Values Result producer-caller exact receipt failed: {message}"
                        ),
                    ),
                };
                return Err(vec![crate::diagnostic::Diagnostic::error_code(
                    code,
                    detail,
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )]);
            }
            Err(crate::core::mir::CanonicalMirRouteMaterializationError::Compatibility {
                admission,
                message,
            }) => {
                // Mixed/Outside construction is the explicit compatibility
                // boundary.  A complete admission can never arrive here:
                // `materialize_canonical_mir_route` classifies that failure as
                // `Complete` above.  A mixed candidate that did not
                // materialize therefore remains on the old API, while any
                // materialized candidate is rejected by the gates below.
                if !matches!(
                    admission.option_string,
                    crate::core::mir::OptionStringVariantAdmission::OutsideProfile
                ) {
                    return Err(vec![crate::diagnostic::Diagnostic::error_code(
                        "MIR-COVERAGE-001",
                        format!(
                            "recognized Option<string> variant candidate could not materialize canonical MIR: {message}"
                        ),
                        program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                    )]);
                }
                if !matches!(
                    admission.option_nested_tuple,
                    crate::core::mir::OptionNestedTupleVariantAdmission::OutsideProfile
                ) {
                    return Err(vec![crate::diagnostic::Diagnostic::error_code(
                        "MIR-COVERAGE-001",
                        format!(
                            "recognized nested Option tuple variant candidate could not materialize canonical MIR: {message}"
                        ),
                        program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                    )]);
                }
                if !matches!(
                    admission.copy_option_i32,
                    crate::core::mir::CopyOptionI32VariantAdmission::OutsideProfile
                ) {
                    return Err(vec![crate::diagnostic::Diagnostic::error_code(
                        "MIR-COVERAGE-001",
                        format!(
                            "recognized Copy Option<i32> variant candidate could not materialize canonical MIR: {message}"
                        ),
                        program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                    )]);
                }
                if !matches!(
                    admission.copy_option_bool,
                    crate::core::mir::CopyOptionI32VariantAdmission::OutsideProfile
                ) {
                    return Err(vec![crate::diagnostic::Diagnostic::error_code(
                        "MIR-COVERAGE-001",
                        format!(
                            "recognized Copy Option<bool> variant candidate could not materialize canonical MIR: {message}"
                        ),
                        program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                    )]);
                }
                if !matches!(
                    admission.copy_option_i64,
                    crate::core::mir::CopyOptionI32VariantAdmission::OutsideProfile
                ) {
                    return Err(vec![crate::diagnostic::Diagnostic::error_code(
                        "MIR-COVERAGE-001",
                        format!(
                            "recognized Copy Option<i64> variant candidate could not materialize canonical MIR: {message}"
                        ),
                        program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                    )]);
                }
                if !matches!(
                    admission.copy_option_f64,
                    crate::core::mir::CopyOptionI32VariantAdmission::OutsideProfile
                ) {
                    return Err(vec![crate::diagnostic::Diagnostic::error_code(
                        "MIR-COVERAGE-001",
                        format!(
                            "recognized Copy Option<f64> variant candidate could not materialize canonical MIR: {message}"
                        ),
                        program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                    )]);
                }
                if !matches!(
                    admission.copy_result_i32,
                    crate::core::mir::CopyResultI32VariantAdmission::OutsideProfile
                ) {
                    return Err(vec![crate::diagnostic::Diagnostic::error_code(
                        "MIR-COVERAGE-001",
                        format!(
                            "recognized Copy Result<i32, i32> variant candidate could not materialize canonical MIR: {message}"
                        ),
                        program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                    )]);
                }
                if !matches!(
                    admission.generic_variant,
                    crate::core::mir::GenericVariantPredicateAdmission::OutsideProfile
                ) || crate::core::mir::has_generic_variant_predicate_operation_candidate(program)
                {
                    return Err(vec![crate::diagnostic::Diagnostic::error_code(
                        "MIR-COVERAGE-001",
                        format!(
                            "recognized generic Option predicate candidate could not materialize canonical MIR: {message}"
                        ),
                        program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                    )]);
                }
                if !matches!(
                    admission.generic_option_projection,
                    crate::core::mir::GenericOptionProjectionAdmission::OutsideProfile
                ) || crate::core::mir::has_generic_option_projection_operation_candidate(program)
                {
                    return Err(vec![crate::diagnostic::Diagnostic::error_code(
                        "MIR-COVERAGE-001",
                        format!(
                            "recognized generic Option projection candidate could not materialize canonical MIR: {message}"
                        ),
                        program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                    )]);
                }
                if !matches!(
                    admission.generic_option_projection_fallback,
                    crate::core::mir::GenericOptionProjectionFallbackAdmission::OutsideProfile
                ) || crate::core::mir::has_generic_option_projection_fallback_operation_candidate(
                    program,
                ) {
                    return Err(vec![crate::diagnostic::Diagnostic::error_code(
                        "MIR-COVERAGE-001",
                        format!(
                            "recognized generic Option fallback candidate could not materialize canonical MIR: {message}"
                        ),
                        program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                    )]);
                }
                if !matches!(
                    admission.generic_result_projection,
                    crate::core::mir::GenericResultProjectionAdmission::OutsideProfile
                ) || crate::core::mir::has_generic_result_projection_operation_candidate(program)
                {
                    return Err(vec![crate::diagnostic::Diagnostic::error_code(
                        "MIR-COVERAGE-001",
                        format!(
                            "recognized generic Result projection candidate could not materialize canonical MIR: {message}"
                        ),
                        program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                    )]);
                }
                if !matches!(
                    admission.generic_result_projection_fallback,
                    crate::core::mir::GenericResultProjectionFallbackAdmission::OutsideProfile
                ) || crate::core::mir::has_generic_result_projection_fallback_operation_candidate(
                    program,
                ) {
                    return Err(vec![crate::diagnostic::Diagnostic::error_code(
                        "MIR-COVERAGE-001",
                        format!(
                            "recognized generic Result fallback candidate could not materialize canonical MIR: {message}"
                        ),
                        program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                    )]);
                }
                if !matches!(
                    admission.managed_result_call,
                    crate::core::mir::ManagedResultCallAdmission::OutsideProfile
                ) {
                    return Err(vec![crate::diagnostic::Diagnostic::error_code(
                        "MIR-COVERAGE-001",
                        format!(
                            "recognized managed Result direct-call candidate could not materialize canonical MIR: {message}"
                        ),
                        program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                    )]);
                }
                return Ok(None);
            }
        };
        let canonical = &route.program;
        if route
            .admission
            .ordinary_map_owned_any_v2_persistent_root_alias_replacement()
        {
            let profile = crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2PersistentRootAliasReplacement;
            let profile_name =
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE;
            let materialized_receipt = route
                .ordinary_map_owned_any_v2_persistent_root_alias_replacement_route_receipt
                .as_ref()
                .ok_or_else(|| {
                    vec![crate::diagnostic::Diagnostic::error_code(
                        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                        "MIR-COVERAGE-001: PersistentRootAliasReplacement admission has no exact materialized route receipt",
                        program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                    )]
                })?;
            let receipt = canonical.route_receipt(profile_name);
            if !profile.is_admitted(route.admission)
                || !profile.is_materialized(&route)
                || !route.materialized_ordinary_map_owned_any_v2_persistent_root_alias_replacement_candidate
                || materialized_receipt.profile != profile_name
                || materialized_receipt != &receipt
            {
                return Err(vec![crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE,
                    "MIR-RECEIPT-001: PersistentRootAliasReplacement Checker admission, materialization, and canonical route receipt disagree",
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )]);
            }
            crate::core::mir::validate_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route(
                program, canonical,
            )
            .map_err(|message| {
                vec![crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    format!("PersistentRootAliasReplacement native route rejected: {message}"),
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )]
            })?;
            receipt
                .validate_against_program(canonical)
                .map_err(|message| {
                    vec![crate::diagnostic::Diagnostic::error_code(
                        crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE,
                        format!("PersistentRootAliasReplacement route receipt rejected: {message}"),
                        program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                    )]
                })?;
            crate::verifier::validate_mir_capabilities(canonical).map_err(|errors| {
                Self::mir_gate_diagnostics(program, "MIR verifier capability", profile_name, errors)
            })?;
            crate::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route(
                program, canonical,
            )
            .map_err(|errors| {
                Self::mir_gate_diagnostics(program, "MIR bytecode", profile_name, errors)
            })?;
            crate::codegen::mir::validate_mir_native(canonical)?;
            let results = crate::verifier::verify_mir_with_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route(
                program,
                canonical,
                &receipt,
                String::new(),
            )
            .map_err(|error| {
                Self::mir_gate_diagnostics(program, "MIR verifier", profile_name, error)
            })?;
            if !crate::verifier::canonical_execution_route_verifier_ready(&results, false, false) {
                return Err(Self::mir_gate_diagnostics(
                    program,
                    "MIR verifier",
                    profile_name,
                    format!("unsupported or inconclusive result: {results:?}"),
                ));
            }
            return Ok(Some(route.program));
        }
        let scalar_export_candidate =
            route.admission.scalar_export && route.materialized_scalar_export_candidate;
        if scalar_export_candidate {
            let island = crate::core::mir::SCALAR_EXPORT_ISLAND;
            if canonical.export_declarations().is_empty() {
                return Err(Self::mir_gate_diagnostics(
                    program,
                    "MIR export receipt",
                    island,
                    "scalar export route has no checker-owned declarations",
                ));
            }
            crate::verifier::validate_mir_capabilities(canonical).map_err(|errors| {
                Self::mir_gate_diagnostics(program, "MIR verifier capability", island, errors)
            })?;
            let receipt =
                canonical.route_receipt(crate::core::mir::MIR_NATIVE_DIRECT_ROUTE_PROFILE);
            crate::interp::bytecode::compile_mir_program_with_route_receipt(canonical, &receipt)
                .map_err(|errors| {
                    Self::mir_gate_diagnostics(program, "MIR bytecode", island, errors)
                })?;
            crate::codegen::mir::validate_mir_native(canonical)?;
            let results =
                crate::verifier::verify_mir_with_route_receipt(canonical, &receipt, String::new())
                    .map_err(|error| {
                        Self::mir_gate_diagnostics(program, "MIR verifier", island, error)
                    })?;
            if !results.is_empty() {
                return Err(Self::mir_gate_diagnostics(
                    program,
                    "MIR export verifier",
                    island,
                    results,
                ));
            }
            return Ok(Some(route.program));
        }
        let scalar_actor_candidate =
            route.admission.scalar_actor && route.materialized_scalar_actor_candidate;
        if scalar_actor_candidate {
            let island = crate::core::mir::SCALAR_ACTOR_ISLAND;
            crate::core::mir::validate_scalar_actor_island(canonical).map_err(|errors| {
                Self::mir_gate_diagnostics(program, "MIR Actor receipt", island, errors)
            })?;
            crate::verifier::validate_mir_capabilities(canonical).map_err(|errors| {
                Self::mir_gate_diagnostics(program, "MIR verifier capability", island, errors)
            })?;
            let receipt =
                canonical.route_receipt(crate::core::mir::MIR_NATIVE_DIRECT_ROUTE_PROFILE);
            crate::interp::bytecode::compile_mir_program_with_route_receipt(canonical, &receipt)
                .map_err(|errors| {
                    Self::mir_gate_diagnostics(program, "MIR bytecode", island, errors)
                })?;
            crate::codegen::mir::validate_mir_native(canonical)?;
            let results =
                crate::verifier::verify_mir_with_route_receipt(canonical, &receipt, String::new())
                    .map_err(|error| {
                        Self::mir_gate_diagnostics(program, "MIR verifier", island, error)
                    })?;
            if !results.is_empty() {
                return Err(Self::mir_gate_diagnostics(
                    program,
                    "MIR neutral NoContracts",
                    island,
                    results,
                ));
            }
            return Ok(Some(route.program));
        }
        let scalar_nested_candidate =
            route.admission.scalar_nested && route.materialized_scalar_nested_candidate;
        if scalar_nested_candidate {
            let island = crate::core::mir::SCALAR_NESTED_ISLAND;
            if canonical.nested_callable_scopes().is_empty() {
                return Err(Self::mir_gate_diagnostics(
                    program,
                    "MIR nested-callable receipt",
                    island,
                    "canonical route has no nested-callable scope receipt",
                ));
            }
            crate::verifier::validate_mir_capabilities(canonical).map_err(|errors| {
                Self::mir_gate_diagnostics(program, "MIR verifier capability", island, errors)
            })?;
            let receipt =
                canonical.route_receipt(crate::core::mir::MIR_NATIVE_DIRECT_ROUTE_PROFILE);
            crate::interp::bytecode::compile_mir_program_with_route_receipt(canonical, &receipt)
                .map_err(|errors| {
                    Self::mir_gate_diagnostics(program, "MIR bytecode", island, errors)
                })?;
            crate::codegen::mir::validate_mir_native(canonical)?;
            let results =
                crate::verifier::verify_mir_with_route_receipt(canonical, &receipt, String::new())
                    .map_err(|error| {
                        vec![crate::diagnostic::Diagnostic::error_code(
                            "MIR-VERIFY-001",
                            format!("MIR nested-callable verifier contract pass failed: {error}"),
                            program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                        )]
                    })?;
            if !crate::verifier::canonical_execution_route_verifier_ready(&results, false, false) {
                return Err(vec![crate::diagnostic::Diagnostic::error_code(
                    "MIR-VERIFY-001",
                    format!(
                        "MIR verifier returned an unsupported or inconclusive result for the {island}"
                    ),
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )]);
            }
            return Ok(Some(route.program));
        }
        let scalar_ffi_candidate =
            route.admission.scalar_ffi && route.materialized_scalar_ffi_candidate;
        let map_root_candidate = route.materialized_map_root_candidate;
        let map_any_owner_v1_candidate = route.materialized_map_any_owner_candidate;
        let ordinary_map_owned_get_i32_string_v1_candidate =
            route.materialized_ordinary_map_owned_get_i32_string_v1_candidate;
        let ordinary_map_owner_lifecycle_v1_candidate =
            route.materialized_ordinary_map_owner_lifecycle_i32_string_v1_candidate;
        let scalar_generic_identity_i32_candidate =
            route.admission.scalar_generic_identity_i32_complete()
                && route.materialized_scalar_generic_identity_i32_candidate;
        let scalar_generic_identity_i64_candidate =
            route.admission.scalar_generic_identity_i64_complete()
                && route.materialized_scalar_generic_identity_i64_candidate;
        // A MapRoot graph may use the already modeled scalar stdout face.
        // Its dedicated Checker receipt and all-consumer preflight own that
        // graph; the older collection-only validator rejects MapRoot by
        // design and must not steal this narrower route.
        let scalar_collection_candidate = route.materialized_collection_candidate
            && !map_root_candidate
            && !map_any_owner_v1_candidate
            && !ordinary_map_owned_get_i32_string_v1_candidate
            && !ordinary_map_owner_lifecycle_v1_candidate;
        let flat_copy_record_candidate = route.materialized_record_candidate;
        let flow_transition_candidate = route.materialized_flow_candidate;
        let flow_failure_retry_candidate = route.materialized_flow_failure_retry_candidate;
        let option_string_candidate = route.materialized_option_string_candidate;
        let option_nested_tuple_candidate = route.materialized_option_nested_tuple_candidate;
        let generic_option_predicate_candidate = route.admission.generic_variant_complete()
            && route.materialized_generic_variant_candidate;
        let generic_option_projection_candidate =
            route.admission.generic_option_projection_complete()
                && route.materialized_generic_option_projection_candidate;
        let generic_option_projection_fallback_candidate = route
            .admission
            .generic_option_projection_fallback_complete()
            && route.materialized_generic_option_projection_fallback_candidate;
        let generic_result_projection_candidate =
            route.admission.generic_result_projection_complete()
                && route.materialized_generic_result_projection_candidate;
        let generic_result_projection_fallback_candidate = route
            .admission
            .generic_result_projection_fallback_complete()
            && route.materialized_generic_result_projection_fallback_candidate;
        let managed_result_call_candidate = route.admission.managed_result_call_complete()
            && route.materialized_managed_result_call_candidate;
        let copy_option_i32_candidate = route.materialized_copy_option_i32_candidate;
        let copy_option_bool_candidate = route.materialized_copy_option_bool_candidate;
        let copy_option_i64_candidate = route.materialized_copy_option_i64_candidate;
        let copy_option_f64_candidate = route.materialized_copy_option_f64_candidate;
        let copy_result_i32_candidate = route.materialized_copy_result_i32_candidate;
        let session_candidate =
            route.admission.session_complete() && route.materialized_session_candidate;
        if !map_root_candidate
            && !map_any_owner_v1_candidate
            && !ordinary_map_owned_get_i32_string_v1_candidate
            && !ordinary_map_owner_lifecycle_v1_candidate
            && !scalar_ffi_candidate
            && !scalar_generic_identity_i32_candidate
            && !scalar_generic_identity_i64_candidate
            && !scalar_collection_candidate
            && !flat_copy_record_candidate
            && !flow_transition_candidate
            && !flow_failure_retry_candidate
            && !generic_option_predicate_candidate
            && !generic_option_projection_candidate
            && !generic_option_projection_fallback_candidate
            && !generic_result_projection_candidate
            && !generic_result_projection_fallback_candidate
            && !managed_result_call_candidate
            && !option_string_candidate
            && !option_nested_tuple_candidate
            && !copy_option_i32_candidate
            && !copy_option_bool_candidate
            && !copy_option_i64_candidate
            && !copy_option_f64_candidate
            && !copy_result_i32_candidate
            && !session_candidate
        {
            return Ok(None);
        }

        // A mixed graph containing a collection candidate and a record value
        // belongs to the narrower collection island first; its island-level
        // validator then rejects the unsupported combination.  This keeps the
        // candidate precedence identical to canonical_dispatch and prevents
        // a flat record from accidentally widening the collection envelope.
        let island = if map_any_owner_v1_candidate {
            crate::core::mir::MAP_ANY_OWNER_V1_ISLAND
        } else if ordinary_map_owned_get_i32_string_v1_candidate {
            crate::core::mir::ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND
        } else if ordinary_map_owner_lifecycle_v1_candidate {
            crate::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_I32_STRING_V1_ISLAND
        } else if map_root_candidate {
            crate::core::mir::MAP_ROOT_ISLAND
        } else if scalar_ffi_candidate {
            "scalar FFI island"
        } else if scalar_generic_identity_i32_candidate {
            crate::core::mir::SCALAR_GENERIC_IDENTITY_I32_ISLAND
        } else if scalar_generic_identity_i64_candidate {
            crate::core::mir::SCALAR_GENERIC_IDENTITY_I64_ISLAND
        } else if flow_failure_retry_candidate {
            "recoverable Flow failure island"
        } else if scalar_collection_candidate {
            "scalar collection island"
        } else if generic_option_predicate_candidate {
            crate::core::mir::GENERIC_VARIANT_PREDICATE_ISLAND
        } else if generic_option_projection_candidate {
            crate::core::mir::GENERIC_OPTION_PROJECTION_ISLAND
        } else if generic_option_projection_fallback_candidate {
            crate::core::mir::GENERIC_OPTION_PROJECTION_FALLBACK_ISLAND
        } else if generic_result_projection_candidate {
            crate::core::mir::GENERIC_RESULT_PROJECTION_ISLAND
        } else if generic_result_projection_fallback_candidate {
            crate::core::mir::GENERIC_RESULT_PROJECTION_FALLBACK_ISLAND
        } else if managed_result_call_candidate {
            crate::core::mir::MANAGED_RESULT_CALL_ISLAND
        } else if flat_copy_record_candidate {
            "flat Copy record island"
        } else if option_string_candidate {
            "non-Copy Option<string> variant island"
        } else if option_nested_tuple_candidate {
            "non-Copy nested Option tuple variant island"
        } else if copy_option_i32_candidate {
            "Copy Option<i32> variant island"
        } else if copy_option_bool_candidate {
            "Copy Option<bool> variant island"
        } else if copy_option_i64_candidate {
            "Copy Option<i64> variant island"
        } else if copy_option_f64_candidate {
            "Copy Option<f64> variant island"
        } else if copy_result_i32_candidate {
            "Copy Result<i32, i32> variant island"
        } else if session_candidate {
            crate::core::mir::SESSION_CHANNEL_ISLAND
        } else {
            "S8 Flow transition island"
        };
        if scalar_collection_candidate {
            if let Err(errors) = crate::core::mir::validate_scalar_collection_island(canonical) {
                // R6-1052: an stdout-only plain-scalar candidate that fails
                // the island preflight is an explicit compatibility input
                // (the route it already ran on), not a hard rejection of a
                // previously working program.  A materialized List/Set
                // operation keeps its cannot-re-enter-legacy tripwire.
                if !route.materialized_collection_operation_candidate && !flow_transition_candidate
                {
                    return Ok(None);
                }
                return Err(Self::mir_gate_diagnostics(
                    program,
                    "MIR island contract",
                    island,
                    &errors,
                ));
            }
        }
        if scalar_generic_identity_i32_candidate {
            if let Err(errors) =
                crate::core::mir::validate_scalar_generic_identity_island(canonical)
            {
                return Err(Self::mir_gate_diagnostics(
                    program,
                    "MIR island contract",
                    island,
                    &errors,
                ));
            }
        }
        if scalar_generic_identity_i64_candidate {
            if let Err(errors) =
                crate::core::mir::validate_scalar_generic_identity_i64_island(canonical)
            {
                return Err(Self::mir_gate_diagnostics(
                    program,
                    "MIR island contract",
                    island,
                    &errors,
                ));
            }
        }
        if option_string_candidate {
            if let Err(errors) = crate::core::mir::validate_option_string_variant_island(canonical)
            {
                return Err(Self::mir_gate_diagnostics(
                    program,
                    "MIR island contract",
                    island,
                    &errors,
                ));
            }
        }
        if option_nested_tuple_candidate {
            if let Err(errors) =
                crate::core::mir::validate_option_nested_tuple_variant_island(canonical)
            {
                return Err(Self::mir_gate_diagnostics(
                    program,
                    "MIR island contract",
                    island,
                    &errors,
                ));
            }
        }
        if copy_option_i32_candidate {
            if let Err(errors) =
                crate::core::mir::validate_copy_option_i32_variant_island(canonical)
            {
                return Err(Self::mir_gate_diagnostics(
                    program,
                    "MIR island contract",
                    island,
                    &errors,
                ));
            }
        }
        if copy_option_bool_candidate {
            if let Err(errors) = crate::core::mir::validate_copy_option_variant_island(
                canonical,
                crate::core::PrimitiveType::Bool,
                crate::core::mir::COPY_OPTION_BOOL_VARIANT_ISLAND,
            ) {
                return Err(Self::mir_gate_diagnostics(
                    program,
                    "MIR island contract",
                    island,
                    &errors,
                ));
            }
        }
        if copy_option_i64_candidate {
            if let Err(errors) =
                crate::core::mir::validate_copy_option_i64_variant_island(canonical)
            {
                return Err(Self::mir_gate_diagnostics(
                    program,
                    "MIR island contract",
                    island,
                    &errors,
                ));
            }
        }
        if copy_option_f64_candidate {
            if let Err(errors) =
                crate::core::mir::validate_copy_option_f64_variant_island(canonical)
            {
                return Err(Self::mir_gate_diagnostics(
                    program,
                    "MIR island contract",
                    island,
                    &errors,
                ));
            }
        }
        if copy_result_i32_candidate {
            if let Err(errors) =
                crate::core::mir::validate_copy_result_i32_variant_island(canonical)
            {
                return Err(Self::mir_gate_diagnostics(
                    program,
                    "MIR island contract",
                    island,
                    &errors,
                ));
            }
        }
        if managed_result_call_candidate {
            if let Err(errors) = crate::core::mir::validate_managed_result_call_island(canonical) {
                return Err(Self::mir_gate_diagnostics(
                    program,
                    "MIR island contract",
                    island,
                    &errors,
                ));
            }
        }
        if session_candidate {
            if let Err(errors) = crate::core::mir::validate_session_channel_island(canonical) {
                return Err(Self::mir_gate_diagnostics(
                    program,
                    "MIR island contract",
                    island,
                    &errors,
                ));
            }
        }
        if let Err(errors) = crate::verifier::validate_mir_capabilities(canonical) {
            return Err(Self::mir_gate_diagnostics(
                program,
                "MIR verifier capability",
                island,
                &errors,
            ));
        }
        let receipt = canonical.route_receipt(crate::core::mir::MIR_NATIVE_DIRECT_ROUTE_PROFILE);
        if let Err(errors) =
            crate::interp::bytecode::compile_mir_program_with_route_receipt(canonical, &receipt)
        {
            return Err(Self::mir_gate_diagnostics(
                program,
                "MIR bytecode",
                island,
                &errors,
            ));
        }
        if let Err(errors) = crate::codegen::mir::validate_mir_native(canonical) {
            return Err(errors);
        }
        let results =
            crate::verifier::verify_mir_with_route_receipt(canonical, &receipt, String::new())
                .map_err(|error| {
                    vec![crate::diagnostic::Diagnostic::error_code(
                        "MIR-VERIFY-001",
                        format!("MIR verifier contract pass failed: {error}"),
                        program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                    )]
                })?;
        if !crate::verifier::canonical_execution_route_verifier_ready(
            &results,
            crate::core::mir::is_exact_cross_state_f64_failure_receipt(program),
            crate::core::mir::contains_multi_target_flow_union_candidate(canonical),
        ) {
            return Err(vec![crate::diagnostic::Diagnostic::error_code(
                "MIR-VERIFY-001",
                format!(
                    "MIR verifier returned an unsupported or inconclusive result for the {island}"
                ),
                program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
            )]);
        }
        Ok(Some(route.program))
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

    /// Mangle a flow transition into an ordinary function name.
    /// Format: `{FlowName}__{transition}__from_{FromState}`
    pub(super) fn transition_fn_name(flow: &str, transition: &str, from: &str) -> String {
        format!("{}__{}__from_{}", flow, transition, from)
    }

    /// Convert a flow transition into a synthetic FuncDef for test-only
    /// compatibility helpers and declaration metadata.
    ///
    /// Production transition bodies are lowered by the canonical MIR route.
    /// This descriptor remains useful for symbol declarations and for the
    /// explicit fail-closed test adapter; `compile_flow` never emits it into
    /// the deleted legacy production owner.
    #[cfg(test)]
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
        super::initialize_codegen_target(self.target_triple.as_deref())?;
        let triple_str = self.target_triple.clone().unwrap_or_else(|| {
            TargetMachine::get_default_triple()
                .as_str()
                .to_string_lossy()
                .to_string()
        });
        let triple = inkwell::targets::TargetTriple::create(&triple_str);
        let target = inkwell::targets::Target::from_triple(&triple)
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
}
