//! Canonical scalar Actor state, dispatch and mailbox emission.
//!
//! State is an array of manifest-ordered i32 fields, owned by the execution
//! domain worker. Language handles are domain-local u64 identities, never state
//! pointers. All operations consume the checker-derived Actor catalog; no AST
//! or compatibility Actor emitter participates in this path.

use super::*;
use crate::core::mir::MirActorContract;
use crate::core::mir::MirActorOperation;
use crate::core::NodeId;
use inkwell::types::StructType;
use inkwell::values::{IntValue, PointerValue};

pub(super) fn actor_state_layout<'ctx>(
    context: &'ctx Context,
    actor: &MirActorContract,
) -> StructType<'ctx> {
    // All fields are i32 and the unpacked LLVM struct has the same alignment,
    // stride and declaration order as the runtime's Box<[i32]> state.
    context.struct_type(&vec![context.i32_type().into(); actor.fields.len()], false)
}

pub(super) fn actor_descriptor_native_type<'ctx>(
    context: &'ctx Context,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<Option<BasicTypeEnum<'ctx>>, NativeMirError> {
    let Some(desc) = catalog.get(ty) else {
        return Err(NativeMirError::new(ty.as_str(), "Actor TypeDesc is absent"));
    };
    match (&desc.kind, &desc.layout) {
        (MirTypeKind::ActorHandle { .. }, MirLayout::Handle)
            if desc.abi == MirAbiClass::OpaqueHandle && desc.has_canonical_copy_noop_metadata() =>
        {
            Ok(Some(context.i64_type().into()))
        }
        (
            MirTypeKind::ActorState { actor },
            MirLayout::ActorState {
                actor: layout_actor,
                fields,
            },
        ) if actor == layout_actor
            && desc.abi == MirAbiClass::Pointer
            && desc.ownership == MirOwnership::WeakBorrow
            && !desc.needs_drop_glue
            && !desc.needs_clone_glue
            && desc.drop_plan.is_none()
            && desc.variant_drop_plan.is_none()
            && desc.glue
                == (MirGlueContract {
                    move_out: MirGlueKind::Unsupported,
                    clone: MirGlueKind::Unsupported,
                    drop: MirGlueKind::Unsupported,
                })
            && fields.len() <= 64
            && fields.iter().all(|field| {
                catalog.get(&field.ty).is_some_and(|desc| {
                    desc.is_canonical_copy_scalar(false)
                        && desc.abi
                            == (MirAbiClass::Integer {
                                bits: 32,
                                signed: true,
                            })
                })
            }) =>
        {
            Ok(Some(
                context.ptr_type(inkwell::AddressSpace::default()).into(),
            ))
        }
        (MirTypeKind::ActorHandle { .. } | MirTypeKind::ActorState { .. }, _)
        | (_, MirLayout::ActorState { .. }) => Err(NativeMirError::new(
            ty.as_str(),
            "Actor handle/state kind, layout, role or ABI is inconsistent",
        )),
        _ => Ok(None),
    }
}

pub(super) fn validate_actor_operation(
    program: &MirProgram,
    function: &MirFunction,
    operation: &MirActorOperation,
    subject: &str,
) -> Result<(), NativeMirError> {
    let plan = program.actor_execution_plan().ok_or_else(|| {
        NativeMirError::new(subject, "Actor instruction has no execution-domain plan")
    })?;
    if !plan.tagged_functions.contains(&function.owner) {
        return Err(NativeMirError::new(
            subject,
            "Actor instruction owner has no tagged status ABI",
        ));
    }
    let actor_id = match operation {
        MirActorOperation::Spawn { actor, .. }
        | MirActorOperation::Call { actor, .. }
        | MirActorOperation::FieldLoad { actor, .. }
        | MirActorOperation::FieldStore { actor, .. } => actor,
    };
    let actor = program.actors().get(actor_id).ok_or_else(|| {
        NativeMirError::new(subject, "Actor instruction references an absent manifest")
    })?;
    let value_type = |value: &MirValueId| {
        function
            .values
            .get(value)
            .map(|entry| &entry.ty)
            .ok_or_else(|| {
                NativeMirError::new(
                    subject,
                    format!("Actor operand '{value}' has no TypeDesc identity"),
                )
            })
    };
    match operation {
        MirActorOperation::Spawn { result, .. } => {
            if function.owner != plan.root
                || plan.worker_functions.contains(&function.owner)
                || value_type(result)? != &actor.handle_type
            {
                return Err(NativeMirError::new(
                    subject,
                    "Actor spawn is only admitted in the root with its exact handle TypeDesc",
                ));
            }
        }
        MirActorOperation::Call {
            result,
            method,
            handle,
            args,
            ..
        } => {
            let target = actor
                .methods
                .iter()
                .find(|entry| &entry.owner == method)
                .ok_or_else(|| {
                    NativeMirError::new(
                        subject,
                        "Actor mailbox method does not belong to its manifest",
                    )
                })?;
            if function.owner != plan.root
                || plan.worker_functions.contains(&function.owner)
                || value_type(handle)? != &actor.handle_type
                || value_type(result)? != &target.result_type
                || args.len() + 1 != target.execution_parameter_types.len()
                || args.len() > 64
            {
                return Err(NativeMirError::new(subject, "Actor mailbox owner, handle, payload or result ABI disagrees with its manifest"));
            }
            for (argument, ty) in args
                .iter()
                .zip(target.execution_parameter_types.iter().skip(1))
            {
                if value_type(argument)? != ty {
                    return Err(NativeMirError::new(
                        subject,
                        "Actor mailbox payload TypeDesc differs from its exact method slot",
                    ));
                }
            }
        }
        MirActorOperation::FieldLoad {
            result,
            field,
            state,
            ..
        } => {
            validate_actor_field_receiver(program, function, actor, state, subject)?;
            let field = actor
                .fields
                .iter()
                .find(|entry| &entry.field == field)
                .ok_or_else(|| {
                    NativeMirError::new(
                        subject,
                        "Actor field load references a foreign field identity",
                    )
                })?;
            if value_type(result)? != &field.ty {
                return Err(NativeMirError::new(
                    subject,
                    "Actor field load result TypeDesc differs from its manifest",
                ));
            }
        }
        MirActorOperation::FieldStore {
            field,
            state,
            value,
            ..
        } => {
            validate_actor_field_receiver(program, function, actor, state, subject)?;
            let field = actor
                .fields
                .iter()
                .find(|entry| &entry.field == field)
                .ok_or_else(|| {
                    NativeMirError::new(
                        subject,
                        "Actor field store references a foreign field identity",
                    )
                })?;
            if value_type(value)? != &field.ty {
                return Err(NativeMirError::new(
                    subject,
                    "Actor field store value TypeDesc differs from its manifest",
                ));
            }
        }
    }
    Ok(())
}

fn validate_actor_field_receiver(
    program: &MirProgram,
    function: &MirFunction,
    actor: &MirActorContract,
    state: &MirValueId,
    subject: &str,
) -> Result<(), NativeMirError> {
    let (owner, method) = program.actors().method(&function.owner).ok_or_else(|| {
        NativeMirError::new(
            subject,
            "Actor state field access requires its exact method owner",
        )
    })?;
    if owner.actor != actor.actor
        || state != &method.self_value
        || function.parameters.first() != Some(state)
        || function
            .values
            .get(state)
            .is_none_or(|entry| entry.ty != actor.state_type)
    {
        return Err(NativeMirError::new(
            subject,
            "Actor field access does not carry its method-owned internal state receiver",
        ));
    }
    Ok(())
}

impl<'a, 'ctx> NativeMirEmitter<'a, 'ctx> {
    pub(super) fn declare_actor_runtime(&self) {
        if self.program.actor_execution_plan().is_none() {
            return;
        }
        let context = self.generator.context;
        let i32_ty = context.i32_type();
        let i64_ty = context.i64_type();
        let ptr_ty = context.ptr_type(inkwell::AddressSpace::default());
        let declarations = [
            (
                "mimi_mir_actor_domain_open",
                i64_ty.fn_type(&[i64_ty.into()], false),
            ),
            (
                "mimi_mir_actor_domain_spawn",
                i32_ty.fn_type(
                    &[
                        i64_ty.into(),
                        ptr_ty.into(),
                        i64_ty.into(),
                        ptr_ty.into(),
                        i64_ty.into(),
                        ptr_ty.into(),
                        ptr_ty.into(),
                    ],
                    false,
                ),
            ),
            (
                "mimi_mir_actor_domain_call",
                i32_ty.fn_type(
                    &[
                        i64_ty.into(),
                        i64_ty.into(),
                        i32_ty.into(),
                        ptr_ty.into(),
                        i64_ty.into(),
                        ptr_ty.into(),
                    ],
                    false,
                ),
            ),
            (
                "mimi_mir_actor_domain_close",
                i32_ty.fn_type(&[i64_ty.into(), i32_ty.into(), ptr_ty.into()], false),
            ),
            (
                "mimi_mir_actor_domain_destroy",
                i32_ty.fn_type(&[i64_ty.into()], false),
            ),
        ];
        for (name, ty) in declarations {
            if self.generator.module.get_function(name).is_none() {
                self.generator
                    .module
                    .add_function(name, ty, Some(Linkage::External));
            }
        }
    }

    pub(super) fn declare_actor_dispatchers(&mut self) -> Result<(), NativeMirError> {
        let context = self.generator.context;
        let ptr_ty = context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = context.i64_type();
        let i32_ty = context.i32_type();
        let dispatch_ty = i32_ty.fn_type(
            &[
                ptr_ty.into(),
                i64_ty.into(),
                i32_ty.into(),
                ptr_ty.into(),
                i64_ty.into(),
                ptr_ty.into(),
            ],
            false,
        );
        for (ordinal, (identity, _)) in self.program.actors().iter().enumerate() {
            // Ordinals avoid collisions between distinct canonical identities
            // whose punctuation happens to have the same symbol fragment.
            let symbol = format!(
                "__mimi_mir_actor_dispatch_{ordinal}_{}",
                native_symbol_fragment(&identity.0)
            );
            if self.generator.module.get_function(&symbol).is_some() {
                return Err(NativeMirError::new(
                    identity.0.clone(),
                    "Actor dispatch symbol already exists",
                ));
            }
            let dispatcher =
                self.generator
                    .module
                    .add_function(&symbol, dispatch_ty, Some(Linkage::Internal));
            self.actor_dispatchers.insert(identity.clone(), dispatcher);
        }
        Ok(())
    }

    pub(super) fn emit_actor_dispatchers(&mut self) -> Result<(), NativeMirError> {
        let actors = self
            .program
            .actors()
            .iter()
            .map(|(_, actor)| actor.clone())
            .collect::<Vec<_>>();
        for actor in &actors {
            self.emit_actor_dispatcher(actor)?;
        }
        Ok(())
    }

    fn emit_actor_dispatcher(&mut self, actor: &MirActorContract) -> Result<(), NativeMirError> {
        let subject = &actor.actor.0;
        let dispatcher = *self.actor_dispatchers.get(&actor.actor).ok_or_else(|| {
            NativeMirError::new(subject, "Actor dispatcher declaration is absent")
        })?;
        let context = self.generator.context;
        let i32_ty = context.i32_type();
        let i64_ty = context.i64_type();
        let param = |index| {
            dispatcher.get_nth_param(index).ok_or_else(|| {
                NativeMirError::new(subject, "Actor dispatcher runtime parameter is absent")
            })
        };
        let fields = param(0)?.into_pointer_value();
        let field_count = param(1)?.into_int_value();
        let method = param(2)?.into_int_value();
        let args = param(3)?.into_pointer_value();
        let argc = param(4)?.into_int_value();
        let result_out = param(5)?.into_pointer_value();
        let entry = context.append_basic_block(dispatcher, "entry");
        let choose = context.append_basic_block(dispatcher, "choose_method");
        let invalid = context.append_basic_block(dispatcher, "invalid_payload");
        self.generator.builder.position_at_end(invalid);
        self.generator
            .builder
            .build_return(Some(&i32_ty.const_int((-6i32) as u64, true)))
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(entry);
        let field_count_ok = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                field_count,
                i64_ty.const_int(actor.fields.len() as u64, false),
                "state_count_ok",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let output_ok = self
            .generator
            .builder
            .build_is_not_null(result_out, "result_out_ok")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let mut valid = self
            .generator
            .builder
            .build_and(field_count_ok, output_ok, "dispatch_shape_ok")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        if !actor.fields.is_empty() {
            let fields_ok = self
                .generator
                .builder
                .build_is_not_null(fields, "state_pointer_ok")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            valid = self
                .generator
                .builder
                .build_and(valid, fields_ok, "dispatch_state_ok")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        }
        self.generator
            .builder
            .build_conditional_branch(valid, choose, invalid)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(choose);
        let method_blocks = actor
            .methods
            .iter()
            .map(|method| {
                context.append_basic_block(dispatcher, &format!("method_{}", method.index))
            })
            .collect::<Vec<_>>();
        let cases = actor
            .methods
            .iter()
            .zip(&method_blocks)
            .map(|(method, block)| (i32_ty.const_int(method.index as u64, false), *block))
            .collect::<Vec<_>>();
        self.generator
            .builder
            .build_switch(method, invalid, &cases)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        for (method, block) in actor.methods.iter().zip(method_blocks) {
            self.generator.builder.position_at_end(block);
            let count = method
                .execution_parameter_types
                .len()
                .checked_sub(1)
                .ok_or_else(|| {
                    NativeMirError::new(subject, "Actor method has no internal state parameter")
                })?;
            let argc_ok = self
                .generator
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    argc,
                    i64_ty.const_int(count as u64, false),
                    "argument_count_ok",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let mut valid = argc_ok;
            if count != 0 {
                let pointer_ok = self
                    .generator
                    .builder
                    .build_is_not_null(args, "argument_pointer_ok")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                valid = self
                    .generator
                    .builder
                    .build_and(valid, pointer_ok, "method_payload_ok")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            }
            let execute = context.append_basic_block(dispatcher, "execute_turn");
            self.generator
                .builder
                .build_conditional_branch(valid, execute, invalid)
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            self.generator.builder.position_at_end(execute);
            let payload_layout = context.struct_type(&vec![i32_ty.into(); count], false);
            let mut arguments = vec![BasicMetadataValueEnum::PointerValue(fields)];
            for index in 0..count {
                let slot = self
                    .generator
                    .builder
                    .build_struct_gep(payload_layout, args, index as u32, "payload_slot")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                let value = self
                    .generator
                    .builder
                    .build_load(i32_ty, slot, "payload_value")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                arguments.push(value.into());
            }
            // Workers have no domain admission or cleanup authority. The
            // shared finite-turn plan forbids Spawn/Call throughout this graph.
            arguments.push(i64_ty.const_zero().into());
            let target = *self.functions.get(&method.owner).ok_or_else(|| {
                NativeMirError::new(subject, "Actor method tagged declaration is absent")
            })?;
            let call = self
                .generator
                .builder
                .build_call(target, &arguments, "actor_turn")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let result = call_try_basic_value(&call)
                .ok_or_else(|| NativeMirError::new(subject, "Actor tagged turn returned void"))?
                .into_struct_value();
            let status = self
                .generator
                .builder
                .build_extract_value(result, 0, "turn_status")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                .into_int_value();
            let success = self
                .generator
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    status,
                    i32_ty.const_zero(),
                    "turn_succeeded",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let store = context.append_basic_block(dispatcher, "reply_success");
            let failure = context.append_basic_block(dispatcher, "reply_failure");
            self.generator
                .builder
                .build_conditional_branch(success, store, failure)
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            self.generator.builder.position_at_end(failure);
            self.generator
                .builder
                .build_return(Some(&status))
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            self.generator.builder.position_at_end(store);
            let value = self
                .generator
                .builder
                .build_extract_value(result, 1, "turn_value")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            self.generator
                .builder
                .build_store(result_out, value)
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            self.generator
                .builder
                .build_return(Some(&i32_ty.const_zero()))
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        }
        Ok(())
    }
}

impl<'a, 'ctx> NativeMirFunctionEmitter<'a, 'ctx> {
    pub(super) fn emit_actor_operation(
        &mut self,
        operation: &MirActorOperation,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        validate_actor_operation(self.program, self.function, operation, subject)?;
        match operation {
            MirActorOperation::Spawn { result, actor } => {
                let value = self.emit_actor_spawn(actor, subject)?;
                self.values.insert(result.clone(), value.into());
            }
            MirActorOperation::Call {
                result,
                actor,
                method,
                handle,
                args,
            } => {
                let value = self.emit_actor_mailbox_call(actor, method, handle, args, subject)?;
                self.values.insert(result.clone(), value.into());
            }
            MirActorOperation::FieldLoad {
                result,
                actor,
                field,
                state,
            } => {
                let slot = self.actor_field_slot(actor, field, state, subject)?;
                let value = self
                    .generator
                    .builder
                    .build_load(self.generator.context.i32_type(), slot, "actor_field")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                self.values.insert(result.clone(), value);
            }
            MirActorOperation::FieldStore {
                actor,
                field,
                state,
                value,
            } => {
                let slot = self.actor_field_slot(actor, field, state, subject)?;
                let value = self.value(value, subject)?;
                self.generator
                    .builder
                    .build_store(slot, value)
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            }
        }
        Ok(())
    }

    fn actor_field_slot(
        &self,
        actor: &NodeId,
        field: &NodeId,
        state: &MirValueId,
        subject: &str,
    ) -> Result<PointerValue<'ctx>, NativeMirError> {
        let actor = self
            .program
            .actors()
            .get(actor)
            .ok_or_else(|| NativeMirError::new(subject, "Actor field manifest is absent"))?;
        let field = actor
            .fields
            .iter()
            .find(|entry| &entry.field == field)
            .ok_or_else(|| NativeMirError::new(subject, "Actor field identity is absent"))?;
        let state = self.value(state, subject)?.into_pointer_value();
        self.generator
            .builder
            .build_struct_gep(
                actor_state_layout(self.generator.context, actor),
                state,
                field.index as u32,
                "actor_state_slot",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))
    }

    fn emit_actor_spawn(
        &mut self,
        actor_id: &NodeId,
        subject: &str,
    ) -> Result<IntValue<'ctx>, NativeMirError> {
        let actor = self
            .program
            .actors()
            .get(actor_id)
            .ok_or_else(|| NativeMirError::new(subject, "Actor spawn manifest is absent"))?
            .clone();
        let i32_ty = self.generator.context.i32_type();
        let i64_ty = self.generator.context.i64_type();
        let fields = actor
            .fields
            .iter()
            .map(|field| i32_ty.const_int(field.initial_value as u64, true))
            .collect::<Vec<_>>();
        let arities = actor
            .methods
            .iter()
            .map(|method| {
                i32_ty.const_int((method.execution_parameter_types.len() - 1) as u64, false)
            })
            .collect::<Vec<_>>();
        let field_array = i32_ty.const_array(&fields);
        let arity_array = i32_ty.const_array(&arities);
        let fields_slot = self
            .generator
            .builder
            .build_alloca(field_array.get_type(), "actor_initial_fields")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let arities_slot = self
            .generator
            .builder
            .build_alloca(arity_array.get_type(), "actor_method_arities")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(fields_slot, field_array)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(arities_slot, arity_array)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let output = self
            .generator
            .builder
            .build_alloca(i64_ty, "actor_handle_out")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let dispatcher = self
            .actor_dispatchers
            .get(actor_id)
            .ok_or_else(|| NativeMirError::new(subject, "Actor spawn dispatcher is absent"))?;
        let function = self.actor_runtime("mimi_mir_actor_domain_spawn", subject)?;
        let domain = self.actor_domain_value(subject)?;
        let call = self
            .generator
            .builder
            .build_call(
                function,
                &[
                    domain.into(),
                    fields_slot.into(),
                    i64_ty.const_int(fields.len() as u64, false).into(),
                    arities_slot.into(),
                    i64_ty.const_int(arities.len() as u64, false).into(),
                    dispatcher.as_global_value().as_pointer_value().into(),
                    output.into(),
                ],
                "actor_spawn_status",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let status = call_try_basic_value(&call)
            .ok_or_else(|| NativeMirError::new(subject, "Actor spawn returned void"))?
            .into_int_value();
        self.emit_actor_status_guard(status, subject)?;
        let handle = self
            .generator
            .builder
            .build_load(i64_ty, output, "actor_handle")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let valid = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                handle,
                i64_ty.const_zero(),
                "actor_handle_valid",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_actor_condition_guard(valid, -6, subject)?;
        Ok(handle)
    }

    fn emit_actor_mailbox_call(
        &mut self,
        actor_id: &NodeId,
        method_id: &NodeId,
        handle: &MirValueId,
        args: &[MirValueId],
        subject: &str,
    ) -> Result<IntValue<'ctx>, NativeMirError> {
        let actor = self
            .program
            .actors()
            .get(actor_id)
            .ok_or_else(|| NativeMirError::new(subject, "Actor call manifest is absent"))?;
        let method = actor
            .methods
            .iter()
            .find(|method| &method.owner == method_id)
            .ok_or_else(|| NativeMirError::new(subject, "Actor call method is absent"))?;
        let index = method.index;
        let i32_ty = self.generator.context.i32_type();
        let i64_ty = self.generator.context.i64_type();
        let payload_layout = self
            .generator
            .context
            .struct_type(&vec![i32_ty.into(); args.len()], false);
        let payload = self
            .generator
            .builder
            .build_alloca(payload_layout, "actor_payload")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        for (index, argument) in args.iter().enumerate() {
            let slot = self
                .generator
                .builder
                .build_struct_gep(payload_layout, payload, index as u32, "actor_payload_slot")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            self.generator
                .builder
                .build_store(slot, self.value(argument, subject)?)
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        }
        let output = self
            .generator
            .builder
            .build_alloca(i32_ty, "actor_result_out")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let function = self.actor_runtime("mimi_mir_actor_domain_call", subject)?;
        let domain = self.actor_domain_value(subject)?;
        let handle = self.value(handle, subject)?;
        let call = self
            .generator
            .builder
            .build_call(
                function,
                &[
                    domain.into(),
                    handle.into(),
                    i32_ty.const_int(index as u64, false).into(),
                    payload.into(),
                    i64_ty.const_int(args.len() as u64, false).into(),
                    output.into(),
                ],
                "actor_call_status",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let status = call_try_basic_value(&call)
            .ok_or_else(|| NativeMirError::new(subject, "Actor call returned void"))?
            .into_int_value();
        self.emit_actor_status_guard(status, subject)?;
        self.generator
            .builder
            .build_load(i32_ty, output, "actor_call_result")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))
            .map(|value| value.into_int_value())
    }

    pub(super) fn actor_runtime(
        &self,
        name: &str,
        subject: &str,
    ) -> Result<FunctionValue<'ctx>, NativeMirError> {
        self.generator.module.get_function(name).ok_or_else(|| {
            NativeMirError::new(
                subject,
                format!("canonical Actor runtime '{name}' is undeclared"),
            )
        })
    }
}
