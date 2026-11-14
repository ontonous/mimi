//! Declaration-name installation from checker-owned catalogs.
//! No surface declaration snapshot or body enters this boundary.

use crate::codegen::CodeGenerator;
use crate::core::{CheckedProgram, PrimitiveType};
use crate::error::{CompileError, MimiResult};
use inkwell::types::BasicTypeEnum;
use inkwell::AddressSpace;

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn install_checked_declaration_names(
        &mut self,
        program: &CheckedProgram,
    ) -> MimiResult<()> {
        let declarations = program.native_declarations();
        self.checked_native_declarations = Some(declarations.clone());
        self.checked_native_types = Some(program.resolved_types().clone());
        self.checked_generic_binder_names.clear();
        for function in program.functions().values() {
            for (name, binder) in &function.generic_binders {
                self.checked_generic_binder_names
                    .insert(binder.clone(), name.clone());
            }
        }
        let mut typed_calls = std::collections::BTreeMap::new();
        for callable in program.callables().values() {
            collect_block_calls(&callable.body.root, &mut typed_calls).map_err(|error| {
                CompileError::LlvmError(format!(
                    "checked callable '{}' has invalid typed calls: {error}",
                    callable.signature.owner.0
                ))
            })?;
            for expression in callable.body.place_inputs.values() {
                collect_expr_calls(expression, &mut typed_calls).map_err(|error| {
                    CompileError::LlvmError(format!(
                        "checked callable '{}' has invalid typed place-input calls: {error}",
                        callable.signature.owner.0
                    ))
                })?;
            }
            for expression in callable.body.default_values.values() {
                collect_expr_calls(expression, &mut typed_calls).map_err(|error| {
                    CompileError::LlvmError(format!(
                        "checked callable '{}' has invalid typed default-value calls: {error}",
                        callable.signature.owner.0
                    ))
                })?;
            }
        }
        self.checked_trait_method_dispatch.clear();
        for site in program.call_sites().values() {
            let span = site.origin.user_span();
            let checked_span = (
                span.source_id.raw(),
                span.start_line,
                span.start_col,
                span.end_line,
                span.end_col,
            );
            if let Some(callee) = site.resolved_callee.as_ref() {
                self.checked_trait_method_dispatch
                    .register_callee(checked_span, callee.clone())
                    .map_err(|error| {
                        CompileError::LlvmError(format!(
                            "checked call '{}' has an invalid typed target: {error}",
                            site.node_id.0
                        ))
                    })?;
            }
            let Some(crate::core::ir::ResolvedCallee::ProtocolMethod {
                protocol,
                method: target_method,
            }) = site.resolved_callee.as_ref()
            else {
                continue;
            };
            let callable = crate::core::NodeId(target_method.as_str().to_owned());
            let Some((typed_callee, type_arguments, receiver_type)) =
                typed_calls.get(&site.node_id)
            else {
                return Err(CompileError::LlvmError(format!(
                    "checked protocol call '{}' is absent from typed callable bodies",
                    site.node_id.0
                )));
            };
            if typed_callee
                != &(crate::core::ir::ResolvedCallee::ProtocolMethod {
                    protocol: protocol.clone(),
                    method: target_method.clone(),
                })
            {
                return Err(CompileError::LlvmError(format!(
                    "protocol call-site '{}' disagrees with its typed body target",
                    site.node_id.0
                )));
            }
            if matches!(
                program.resolved_types().get(receiver_type),
                Some(crate::core::ir::ResolvedType::Trait {
                    kind: crate::core::TraitTypeKind::Dynamic,
                    ..
                })
            ) {
                // Dynamic calls carry a slot, not a statically selected impl
                // body. Keep only the typed callee classification above; the
                // legacy vtable path remains outside this static receipt.
                continue;
            }
            let mut bindings = declarations
                .impl_order
                .iter()
                .filter_map(|owner| {
                    declarations
                        .impls
                        .get(owner)
                        .map(|implementation| (owner, implementation))
                })
                .filter(|(_, implementation)| implementation.trait_owner == *protocol)
                .flat_map(|(owner, implementation)| {
                    implementation
                        .methods
                        .iter()
                        .map(move |method| (owner, implementation, method))
                })
                .filter(|(_, _, binding)| binding.callable == callable)
                .collect::<Vec<_>>();
            if bindings.len() != 1 {
                return Err(CompileError::LlvmError(format!(
                    "checked protocol call '{}' has {} native impl bindings",
                    site.node_id.0,
                    bindings.len()
                )));
            }
            let (impl_owner, implementation, binding) = bindings.remove(0);
            let generic_binders = implementation
                .generics
                .iter()
                .chain(&binding.generics)
                .map(|generic| generic.binder.clone())
                .collect::<Vec<_>>();
            if generic_binders != binding.signature.generic_parameters
                || type_arguments.len() != generic_binders.len()
            {
                return Err(CompileError::LlvmError(format!(
                    "checked protocol call '{}' generic arguments differ from native binder order",
                    site.node_id.0
                )));
            }
            let substitutions = generic_binders
                .into_iter()
                .zip(type_arguments.iter().cloned())
                .collect::<std::collections::BTreeMap<_, _>>();
            let receiver_template = binding.signature.parameters.first().ok_or_else(|| {
                CompileError::LlvmError(format!(
                    "checked protocol method '{}' has no canonical receiver",
                    binding.callable.0
                ))
            })?;
            if receiver_template.name != "self"
                || !instantiated_type_matches(
                    &receiver_template.ty,
                    receiver_type,
                    &substitutions,
                    program.resolved_types(),
                )
            {
                return Err(CompileError::LlvmError(format!(
                    "checked protocol call '{}' receiver type does not match its checked native method signature",
                    site.node_id.0
                )));
            }
            let key = crate::codegen::generic_trait_dispatch::CheckedTraitMethodInstanceKey::new(
                (*impl_owner).clone(),
                protocol.clone(),
                binding.callable.clone(),
                implementation
                    .generics
                    .iter()
                    .chain(&binding.generics)
                    .map(|generic| generic.binder.clone())
                    .collect(),
                type_arguments.clone(),
                receiver_type.clone(),
                binding.trait_slots.clone(),
            )
            .map_err(|error| {
                CompileError::LlvmError(format!(
                    "checked protocol call '{}' has invalid exact method identity: {error}",
                    site.node_id.0
                ))
            })?;
            let target = crate::codegen::generic_trait_dispatch::CheckedTraitMethodTarget::new(
                key,
                binding.symbol.clone(),
            )
            .map_err(|error| {
                CompileError::LlvmError(format!(
                    "checked protocol call '{}' has invalid target receipt: {error}",
                    site.node_id.0
                ))
            })?;
            self.checked_trait_method_dispatch
                .register(checked_span, target)
                .map_err(|error| {
                    CompileError::LlvmError(format!(
                        "checked protocol call '{}' conflicts with another exact target: {error}",
                        site.node_id.0
                    ))
                })?;
        }
        self.checked_alias_type_targets.clear();
        self.checked_alias_llvm_types.clear();
        self.checked_alias_primitives.clear();
        for owner in &declarations.type_order {
            let Some(declaration) = declarations.types.get(owner) else {
                continue;
            };
            if let crate::core::ir::NativeTypeDeclarationKind::Alias { target } = &declaration.kind
            {
                self.checked_alias_type_targets
                    .insert(declaration.name.clone(), target.clone());
            }
        }
        for (owner, primitive) in declarations.closed_primitive_aliases(program.resolved_types()) {
            let Some(declaration) = declarations.types.get(&owner) else {
                continue;
            };
            self.checked_alias_primitives
                .insert(declaration.name.clone(), primitive);
            self.checked_alias_llvm_types.insert(
                declaration.name.clone(),
                alias_primitive_layout(self.context, primitive),
            );
        }

        self.actor_names.extend(
            declarations
                .actor_order
                .iter()
                .filter_map(|owner| declarations.actors.get(owner))
                .map(|actor| actor.qualified_name.clone()),
        );
        for capability in program.capabilities().values() {
            self.cap_type_names
                .insert(capability.qualified_name.clone());
            self.cap_components.insert(
                capability.qualified_name.clone(),
                capability.components.clone(),
            );
        }
        for owner in &declarations.trait_order {
            let Some(declaration) = declarations.traits.get(owner) else {
                continue;
            };
            self.trait_method_names.insert(
                declaration.qualified_name.clone(),
                declaration
                    .slots
                    .iter()
                    .map(|slot| slot.name.clone())
                    .collect(),
            );
        }
        Ok(())
    }
}

fn instantiated_type_matches(
    template: &crate::core::ResolvedTypeId,
    actual: &crate::core::ResolvedTypeId,
    substitutions: &std::collections::BTreeMap<crate::core::NodeId, crate::core::ResolvedTypeId>,
    types: &crate::core::ResolvedTypeTable,
) -> bool {
    use crate::core::ir::ResolvedType;

    if let Some(ResolvedType::GenericParameter(binder)) = types.get(template) {
        return substitutions
            .get(binder)
            .map(|replacement| replacement == actual)
            .unwrap_or(template == actual);
    }
    let (Some(template), Some(actual)) = (types.get(template), types.get(actual)) else {
        return false;
    };
    let child = |left: &crate::core::ResolvedTypeId, right: &crate::core::ResolvedTypeId| {
        instantiated_type_matches(left, right, substitutions, types)
    };
    match (template, actual) {
        (ResolvedType::Primitive(left), ResolvedType::Primitive(right)) => left == right,
        (
            ResolvedType::Nominal {
                item: left_item,
                arguments: left_arguments,
                is_linear: left_linear,
            },
            ResolvedType::Nominal {
                item: right_item,
                arguments: right_arguments,
                is_linear: right_linear,
            },
        ) => {
            left_item == right_item
                && left_linear == right_linear
                && left_arguments.len() == right_arguments.len()
                && left_arguments
                    .iter()
                    .zip(right_arguments)
                    .all(|(left, right)| child(left, right))
        }
        (
            ResolvedType::FlowStateSet {
                flow: left_flow,
                states: left_states,
            },
            ResolvedType::FlowStateSet {
                flow: right_flow,
                states: right_states,
            },
        ) => left_flow == right_flow && left_states == right_states,
        (
            ResolvedType::Reference {
                lifetime: left_lifetime,
                mutable: left_mutable,
                target: left_target,
            },
            ResolvedType::Reference {
                lifetime: right_lifetime,
                mutable: right_mutable,
                target: right_target,
            },
        ) => {
            left_lifetime == right_lifetime
                && left_mutable == right_mutable
                && child(left_target, right_target)
        }
        (ResolvedType::Option(left), ResolvedType::Option(right))
        | (ResolvedType::CBuffer(left), ResolvedType::CBuffer(right))
        | (ResolvedType::Slice(left), ResolvedType::Slice(right)) => child(left, right),
        (
            ResolvedType::Result {
                ok: left_ok,
                error: left_error,
            },
            ResolvedType::Result {
                ok: right_ok,
                error: right_error,
            },
        ) => child(left_ok, right_ok) && child(left_error, right_error),
        (ResolvedType::Tuple(left), ResolvedType::Tuple(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| child(left, right))
        }
        (
            ResolvedType::Function {
                abi: left_abi,
                parameters: left_parameters,
                result: left_result,
            },
            ResolvedType::Function {
                abi: right_abi,
                parameters: right_parameters,
                result: right_result,
            },
        ) => {
            left_abi == right_abi
                && left_parameters.len() == right_parameters.len()
                && left_parameters
                    .iter()
                    .zip(right_parameters)
                    .all(|(left, right)| child(left, right))
                && child(left_result, right_result)
        }
        (ResolvedType::Capability(left), ResolvedType::Capability(right)) => left == right,
        (
            ResolvedType::Ownership {
                kind: left_kind,
                target: left_target,
            },
            ResolvedType::Ownership {
                kind: right_kind,
                target: right_target,
            },
        ) => left_kind == right_kind && child(left_target, right_target),
        (
            ResolvedType::Newtype {
                item: left_item,
                inner: left_inner,
            },
            ResolvedType::Newtype {
                item: right_item,
                inner: right_inner,
            },
        ) => left_item == right_item && child(left_inner, right_inner),
        (
            ResolvedType::Array {
                element: left_element,
                length: left_length,
            },
            ResolvedType::Array {
                element: right_element,
                length: right_length,
            },
        ) => left_length == right_length && child(left_element, right_element),
        (
            ResolvedType::Trait {
                kind: left_kind,
                traits: left_traits,
            },
            ResolvedType::Trait {
                kind: right_kind,
                traits: right_traits,
            },
        ) => left_kind == right_kind && left_traits == right_traits,
        (
            ResolvedType::RawPointer {
                mutable: left_mutable,
                target: left_target,
            },
            ResolvedType::RawPointer {
                mutable: right_mutable,
                target: right_target,
            },
        ) => left_mutable == right_mutable && child(left_target, right_target),
        (
            ResolvedType::DynamicAny {
                capability: left_capability,
            },
            ResolvedType::DynamicAny {
                capability: right_capability,
            },
        ) => left_capability == right_capability,
        _ => false,
    }
}

type CheckedCallFacts = (
    crate::core::ir::ResolvedCallee,
    Vec<crate::core::ResolvedTypeId>,
    crate::core::ResolvedTypeId,
);

fn collect_block_calls(
    block: &crate::core::ir::ResolvedBlock,
    calls: &mut std::collections::BTreeMap<crate::core::NodeId, CheckedCallFacts>,
) -> Result<(), String> {
    for statement in &block.statements {
        match &statement.kind {
            crate::core::ir::ResolvedStmtKind::Bind { initializer, .. } => {
                if let Some(initializer) = initializer {
                    collect_expr_calls(initializer, calls)?;
                }
            }
            crate::core::ir::ResolvedStmtKind::Assign { value, .. }
            | crate::core::ir::ResolvedStmtKind::Expr(value)
            | crate::core::ir::ResolvedStmtKind::Contract {
                condition: value, ..
            } => collect_expr_calls(value, calls)?,
            crate::core::ir::ResolvedStmtKind::Return { value, .. }
            | crate::core::ir::ResolvedStmtKind::Break(value) => {
                if let Some(value) = value {
                    collect_expr_calls(value, calls)?;
                }
            }
            crate::core::ir::ResolvedStmtKind::While { condition, body } => {
                collect_expr_calls(condition, calls)?;
                collect_block_calls(body, calls)?;
            }
            crate::core::ir::ResolvedStmtKind::WhileLet {
                initializer, body, ..
            }
            | crate::core::ir::ResolvedStmtKind::For {
                iterable: initializer,
                body,
                ..
            } => {
                collect_expr_calls(initializer, calls)?;
                collect_block_calls(body, calls)?;
            }
            crate::core::ir::ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                collect_expr_calls(initializer, calls)?;
                collect_block_calls(then_block, calls)?;
                if let Some(else_block) = else_block {
                    collect_block_calls(else_block, calls)?;
                }
            }
            crate::core::ir::ResolvedStmtKind::Loop(body)
            | crate::core::ir::ResolvedStmtKind::Scope { body, .. } => {
                collect_block_calls(body, calls)?;
            }
            crate::core::ir::ResolvedStmtKind::Math(expressions) => {
                for expression in expressions {
                    collect_expr_calls(expression, calls)?;
                }
            }
            crate::core::ir::ResolvedStmtKind::Pinned { value, body, .. } => {
                collect_expr_calls(value, calls)?;
                collect_block_calls(body, calls)?;
            }
            crate::core::ir::ResolvedStmtKind::Continue
            | crate::core::ir::ResolvedStmtKind::Drop(_)
            | crate::core::ir::ResolvedStmtKind::NestedCallable(_) => {}
        }
    }
    if let Some(result) = &block.result {
        collect_expr_calls(result, calls)?;
    }
    Ok(())
}

fn collect_expr_calls(
    expression: &crate::core::ir::ResolvedExpr,
    calls: &mut std::collections::BTreeMap<crate::core::NodeId, CheckedCallFacts>,
) -> Result<(), String> {
    use crate::core::ir::{ResolvedExprKind as Expr, ResolvedFStringPart as FStringPart};
    match &expression.kind {
        Expr::Call(call) => {
            if matches!(
                &call.callee,
                crate::core::ir::ResolvedCallee::ProtocolMethod { .. }
            ) {
                let receiver_type = call
                    .arguments
                    .first()
                    .map(|argument| argument.value.ty.clone())
                    .ok_or("typed protocol method call has no canonical receiver argument")?;
                let facts = (
                    call.callee.clone(),
                    call.type_arguments.clone(),
                    receiver_type,
                );
                if calls
                    .insert(expression.node_id.clone(), facts.clone())
                    .is_some_and(|prior| prior != facts)
                {
                    return Err(format!(
                        "typed call node '{}' has conflicting canonical facts",
                        expression.node_id.0
                    ));
                }
            }
            for argument in &call.arguments {
                collect_expr_calls(&argument.value, calls)?;
            }
        }
        Expr::FString(parts) => {
            for part in parts {
                if let FStringPart::Interpolation(value) = part {
                    collect_expr_calls(value, calls)?;
                }
            }
        }
        Expr::Project { value, projection } => {
            collect_expr_calls(value, calls)?;
            if let crate::core::ir::ResolvedValueProjection::Index(index) = projection {
                collect_expr_calls(index, calls)?;
            }
        }
        Expr::Lambda(lambda) => collect_block_calls(&lambda.body, calls)?,
        Expr::Binary { left, right, .. }
        | Expr::Range {
            start: left,
            end: right,
        } => {
            collect_expr_calls(left, calls)?;
            collect_expr_calls(right, calls)?;
        }
        Expr::Unary { operand, .. }
        | Expr::TypeOf(operand)
        | Expr::Old(operand)
        | Expr::Try { value: operand, .. }
        | Expr::Spawn(operand)
        | Expr::Await(operand) => collect_expr_calls(operand, calls)?,
        Expr::Tuple(values) | Expr::List(values) | Expr::Set(values) => {
            for value in values {
                collect_expr_calls(value, calls)?;
            }
        }
        Expr::Map(entries) => {
            for (key, value) in entries {
                collect_expr_calls(key, calls)?;
                collect_expr_calls(value, calls)?;
            }
        }
        Expr::Comprehension {
            value,
            iterable,
            guard,
            ..
        } => {
            collect_expr_calls(value, calls)?;
            collect_expr_calls(iterable, calls)?;
            if let Some(guard) = guard {
                collect_expr_calls(guard, calls)?;
            }
        }
        Expr::OptionalChain { receiver, .. } => collect_expr_calls(receiver, calls)?,
        Expr::Record { fields, rest, .. } => {
            for field in fields {
                collect_expr_calls(&field.value, calls)?;
            }
            if let Some(rest) = rest {
                collect_expr_calls(rest, calls)?;
            }
        }
        Expr::Block(block)
        | Expr::Scope { body: block, .. }
        | Expr::Comptime(block)
        | Expr::Quote(block) => collect_block_calls(block, calls)?,
        Expr::If {
            condition,
            then_block,
            else_block,
        } => {
            collect_expr_calls(condition, calls)?;
            collect_block_calls(then_block, calls)?;
            collect_block_calls(else_block, calls)?;
        }
        Expr::Match { scrutinee, arms } => {
            collect_expr_calls(scrutinee, calls)?;
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_expr_calls(guard, calls)?;
                }
                collect_expr_calls(&arm.body, calls)?;
            }
        }
        Expr::Slice { target, start, end } => {
            collect_expr_calls(target, calls)?;
            if let Some(start) = start {
                collect_expr_calls(start, calls)?;
            }
            if let Some(end) = end {
                collect_expr_calls(end, calls)?;
            }
        }
        Expr::Cast { value, .. } => collect_expr_calls(value, calls)?,
        Expr::Literal(_)
        | Expr::Load(_)
        | Expr::Constant(_)
        | Expr::Callable(_)
        | Expr::DefaultArgument { .. }
        | Expr::ComptimeValue(_)
        | Expr::TypeValue(_) => {}
    }
    Ok(())
}

fn alias_primitive_layout<'ctx>(
    context: &'ctx inkwell::context::Context,
    primitive: PrimitiveType,
) -> BasicTypeEnum<'ctx> {
    let pointer = context.ptr_type(AddressSpace::default());
    match primitive {
        PrimitiveType::I8 | PrimitiveType::U8 => context.i8_type().into(),
        PrimitiveType::I16 | PrimitiveType::U16 => context.i16_type().into(),
        PrimitiveType::I32 | PrimitiveType::U32 | PrimitiveType::Char => context.i32_type().into(),
        PrimitiveType::I64
        | PrimitiveType::U64
        | PrimitiveType::Isize
        | PrimitiveType::Usize
        | PrimitiveType::Unit => context.i64_type().into(),
        PrimitiveType::I128 | PrimitiveType::U128 => context.i128_type().into(),
        PrimitiveType::F32 => context.f32_type().into(),
        PrimitiveType::F64 => context.f64_type().into(),
        PrimitiveType::Bool => context.bool_type().into(),
        PrimitiveType::String => context
            .struct_type(&[pointer.into(), context.i64_type().into()], false)
            .into(),
    }
}
