//! Whole-graph tagged status ABI and root Actor-domain lifetime boundary.
//!
//! Every reachable source callable returns { i32 status, i32 value } and has
//! one hidden domain argument. A nonzero status immediately propagates upward;
//! only the external root wrapper reports it after close/join/destroy. Worker
//! functions never abort or acquire the unique domain cleanup authority.

use super::*;
use crate::core::mir::{actor_trap_status, MirActorNativeCallableAbi, MIR_ACTOR_STATUS_ABI_SCHEMA};
use crate::core::NodeId;
use inkwell::types::StructType;
use inkwell::values::{IntValue, StructValue};

pub(super) fn tagged_i32_type(context: &Context) -> StructType<'_> {
    context.struct_type(
        &[context.i32_type().into(), context.i32_type().into()],
        false,
    )
}

pub(super) fn is_actor_tagged(program: &MirProgram, owner: &NodeId) -> bool {
    program
        .actor_execution_plan()
        .is_some_and(|plan| plan.tagged_functions.contains(owner))
}

pub(super) fn actor_native_symbol(program: &MirProgram, owner: &NodeId) -> Result<String, String> {
    let symbol = mir_symbol(owner)?;
    if is_actor_tagged(program, owner) {
        // Encode the full checker identity injectively. Sanitized method
        // fragments can collide for different punctuation/Unicode owners.
        let identity = owner
            .0
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Ok(format!("__mimi_actor_status_{identity}"))
    } else {
        Ok(symbol)
    }
}

pub(super) fn validate_actor_native_program(program: &MirProgram) -> Vec<NativeMirError> {
    let mut errors = Vec::new();
    if let Err(messages) = program.validate_actor_contracts() {
        errors.extend(
            messages
                .into_iter()
                .map(|message| NativeMirError::new("Actor shared contract", message)),
        );
        return errors;
    }
    let has_actor_operations = program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|instruction| matches!(instruction.kind, MirInstructionKind::Actor { .. }))
        })
    });
    let has_actor_types = program.type_catalog().iter().any(|(_, descriptor)| {
        matches!(
            descriptor.kind,
            MirTypeKind::ActorHandle { .. } | MirTypeKind::ActorState { .. }
        ) || matches!(descriptor.layout, MirLayout::ActorState { .. })
    });
    let has_actors = program.actors().iter().next().is_some();
    let Some(plan) = program.actor_execution_plan() else {
        if has_actor_operations || has_actor_types || has_actors {
            errors.push(NativeMirError::new(
                "Actor",
                "Actor types/manifest/operations require a checked execution-domain plan",
            ));
        }
        return errors;
    };
    if !has_actors || !has_actor_operations {
        errors.push(NativeMirError::new(
            "Actor",
            "Actor execution-domain plan has no selected executable manifest",
        ));
    }
    if let Err(messages) = program.actors().validate(program.type_catalog()) {
        errors.extend(
            messages
                .into_iter()
                .map(|message| NativeMirError::new("Actor catalog", message)),
        );
    }
    let declarations = program
        .functions()
        .keys()
        .map(|owner| {
            let abi = if plan.tagged_functions.contains(owner) {
                MirActorNativeCallableAbi::TaggedI32 {
                    schema: MIR_ACTOR_STATUS_ABI_SCHEMA,
                    status_bits: 32,
                }
            } else {
                MirActorNativeCallableAbi::Ordinary
            };
            (owner.clone(), abi)
        })
        .collect::<BTreeMap<_, _>>();
    if let Err(messages) = plan.validate_native_abis(program.functions(), &declarations) {
        errors.extend(
            messages
                .into_iter()
                .map(|message| NativeMirError::new("Actor status graph", message)),
        );
    }
    let Some(root) = program.functions().get(&plan.root) else {
        errors.push(NativeMirError::new("Actor root", "root function is absent"));
        return errors;
    };
    if !root.parameters.is_empty() || mir_symbol(&plan.root).as_deref() != Ok("main") {
        errors.push(NativeMirError::new(
            "Actor root",
            "external Actor execution currently requires an argument-free main",
        ));
    }
    if plan.worker_functions.contains(&plan.root) {
        errors.push(NativeMirError::new(
            "Actor root",
            "root must not be reachable from a worker",
        ));
    }
    for function in program.functions().values() {
        let tagged = plan.tagged_functions.contains(&function.owner);
        for parameter in &function.parameters {
            let Some(value) = function.values.get(parameter) else {
                continue;
            };
            let method_state =
                program
                    .actors()
                    .method(&function.owner)
                    .is_some_and(|(actor, method)| {
                        parameter == &method.self_value && value.ty == actor.state_type
                    });
            if tagged && !method_state && !is_i32(program, &value.ty) {
                errors.push(NativeMirError::new(function.owner.0.clone(), "tagged scalar parameter is outside exact i32 or the method-owned state receiver"));
            }
        }
        if tagged && !is_i32(program, &function.result) {
            errors.push(NativeMirError::new(
                function.owner.0.clone(),
                "tagged Actor result requires exact canonical i32",
            ));
        }
        for (identity, value) in &function.values {
            let Some(descriptor) = program.type_catalog().get(&value.ty) else {
                continue;
            };
            match &descriptor.kind {
                MirTypeKind::ActorState { actor } => {
                    let valid = tagged
                        && program.actors().method(&function.owner).is_some_and(
                            |(owner, method)| {
                                &owner.actor == actor
                                    && identity == &method.self_value
                                    && function.parameters.first() == Some(identity)
                                    && value.ty == owner.state_type
                            },
                        );
                    if !valid {
                        errors.push(NativeMirError::new(
                            identity.to_string(),
                            "internal Actor state receiver escapes its exact method parameter",
                        ));
                    }
                }
                MirTypeKind::ActorHandle { actor } => {
                    if !tagged
                        || function.owner != plan.root
                        || program
                            .actors()
                            .get(actor)
                            .is_none_or(|actor| value.ty != actor.handle_type)
                    {
                        errors.push(NativeMirError::new(
                            identity.to_string(),
                            "Actor handle is outside its root-owned exact manifest",
                        ));
                    }
                }
                _ if tagged => {
                    if !descriptor.has_canonical_copy_noop_metadata()
                        || !(is_i32(program, &value.ty)
                            || matches!(descriptor.abi, MirAbiClass::Bool | MirAbiClass::Unit))
                    {
                        errors.push(NativeMirError::new(
                            identity.to_string(),
                            "tagged Actor graph carries an unsupported scalar/heap value",
                        ));
                    }
                }
                _ => (),
            }
        }
        for block in function.blocks.values() {
            for instruction in &block.instructions {
                match &instruction.kind {
                    MirInstructionKind::Actor { operation } => {
                        if let Err(error) = super::actor::validate_actor_operation(
                            program,
                            function,
                            operation,
                            instruction.id.as_str(),
                        ) {
                            errors.push(error);
                        }
                    }
                    MirInstructionKind::Call {
                        callee,
                        type_arguments,
                        effect_receipts,
                        variant_call_contract,
                        result,
                        arguments,
                        ..
                    } => {
                        let ResolvedCallee::Function(target) = callee else {
                            if tagged {
                                errors.push(NativeMirError::new(
                                    instruction.id.as_str(),
                                    "tagged Actor graph only admits direct scalar functions",
                                ));
                            }
                            continue;
                        };
                        if tagged
                            && (!plan.tagged_functions.contains(target)
                                || program.actors().method(target).is_some()
                                || !type_arguments.is_empty()
                                || !effect_receipts.is_empty()
                                || variant_call_contract.is_some()
                                || result.is_none())
                        {
                            errors.push(NativeMirError::new(instruction.id.as_str(), "tagged direct call is missing its status ABI or bypasses the Actor mailbox"));
                        }
                        if !tagged && plan.tagged_functions.contains(target) {
                            errors.push(NativeMirError::new(
                                instruction.id.as_str(),
                                "ordinary ABI cannot call a tagged Actor function",
                            ));
                        }
                        if arguments.iter().any(|argument| {
                            function
                                .values
                                .get(argument)
                                .and_then(|entry| program.type_catalog().get(&entry.ty))
                                .is_some_and(|desc| {
                                    matches!(
                                        desc.kind,
                                        MirTypeKind::ActorState { .. }
                                            | MirTypeKind::ActorHandle { .. }
                                    )
                                })
                        }) {
                            errors.push(NativeMirError::new(
                                instruction.id.as_str(),
                                "Actor state/handle cannot cross an ordinary direct-call boundary",
                            ));
                        }
                    }
                    MirInstructionKind::Const { .. }
                    | MirInstructionKind::Copy { .. }
                    | MirInstructionKind::Move { .. }
                    | MirInstructionKind::Clone { .. }
                    | MirInstructionKind::Drop { .. }
                    | MirInstructionKind::Binary { .. }
                    | MirInstructionKind::Unary { .. }
                    | MirInstructionKind::Nop => (),
                    MirInstructionKind::BuiltinCall {
                        kind: MirBuiltinKind::PrintlnInt | MirBuiltinKind::PrintlnBool,
                        string_field_contract: None,
                        ..
                    } => (),
                    _ if tagged => errors.push(NativeMirError::new(
                        instruction.id.as_str(),
                        "instruction has no finite scalar tagged-status implementation",
                    )),
                    _ => (),
                }
                // State receiver is borrowed by FieldLoad/Store only; no copy,
                // move, clone, drop, direct call or boolean test can expose it.
                if tagged {
                    if let MirInstructionKind::Copy { source, .. }
                    | MirInstructionKind::Move { source, .. }
                    | MirInstructionKind::Clone { source, .. }
                    | MirInstructionKind::Drop { value: source } = &instruction.kind
                    {
                        if function
                            .values
                            .get(source)
                            .and_then(|entry| program.type_catalog().get(&entry.ty))
                            .is_some_and(|desc| matches!(desc.kind, MirTypeKind::ActorState { .. }))
                        {
                            errors.push(NativeMirError::new(
                                instruction.id.as_str(),
                                "Actor internal state cannot be copied, moved, cloned or dropped",
                            ));
                        }
                    }
                }
            }
            if tagged {
                match &block.terminator {
                    MirTerminator::Return { value: Some(_) }
                    | MirTerminator::Goto { .. }
                    | MirTerminator::Branch { .. } => (),
                    MirTerminator::Trap { code } => {
                        if let Err(message) = actor_trap_status(code) {
                            errors.push(NativeMirError::new(block.id.to_string(), message));
                        }
                    }
                    _ => errors.push(NativeMirError::new(
                        block.id.to_string(),
                        "terminator has no finite tagged-status implementation",
                    )),
                }
            }
        }
    }
    errors
}

pub(super) fn validate_actor_signature_type(
    program: &MirProgram,
    function: &MirFunction,
    ty: &crate::core::ResolvedTypeId,
    subject: &str,
) -> Result<(), NativeMirError> {
    let descriptor = program
        .type_catalog()
        .get(ty)
        .ok_or_else(|| NativeMirError::new(subject, "Actor signature TypeDesc is absent"))?;
    let plan = program.actor_execution_plan().ok_or_else(|| {
        NativeMirError::new(
            subject,
            "Actor signature requires its checked execution plan",
        )
    })?;
    let valid =
        match &descriptor.kind {
            MirTypeKind::ActorHandle { actor } => {
                function.owner == plan.root
                    && program
                        .actors()
                        .get(actor)
                        .is_some_and(|actor| ty == &actor.handle_type)
            }
            MirTypeKind::ActorState { actor } => program
                .actors()
                .method(&function.owner)
                .is_some_and(|(owner, method)| {
                    &owner.actor == actor
                        && ty == &owner.state_type
                        && function.parameters.first() == Some(&method.self_value)
                }),
            _ => false,
        };
    if !valid {
        return Err(NativeMirError::new(
            subject,
            "Actor signature type is outside its exact root handle or method receiver role",
        ));
    }
    Ok(())
}

fn is_i32(program: &MirProgram, ty: &crate::core::ResolvedTypeId) -> bool {
    program.type_catalog().get(ty).is_some_and(|descriptor| {
        descriptor.has_canonical_copy_noop_metadata()
            && descriptor.abi
                == (MirAbiClass::Integer {
                    bits: 32,
                    signed: true,
                })
            && descriptor.layout == MirLayout::Scalar
    })
}

impl<'a, 'ctx> NativeMirFunctionEmitter<'a, 'ctx> {
    pub(super) fn actor_domain_value(
        &self,
        subject: &str,
    ) -> Result<IntValue<'ctx>, NativeMirError> {
        self.actor_domain.ok_or_else(|| {
            NativeMirError::new(
                subject,
                "tagged Actor function has no hidden domain argument",
            )
        })
    }

    pub(super) fn emit_actor_status_return(
        &mut self,
        status: IntValue<'ctx>,
        value: IntValue<'ctx>,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        if !is_actor_tagged(self.program, &self.function.owner)
            || status.get_type().get_bit_width() != 32
            || value.get_type().get_bit_width() != 32
        {
            return Err(NativeMirError::new(
                subject,
                "Actor status return disagrees with its mandatory i32/i32 ABI",
            ));
        }
        let aggregate = self
            .generator
            .builder
            .build_insert_value(
                tagged_i32_type(self.generator.context).const_zero(),
                status,
                0,
                "actor_tagged_status",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_struct_value();
        let aggregate = self
            .generator
            .builder
            .build_insert_value(aggregate, value, 1, "actor_tagged_value")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_struct_value();
        self.generator
            .builder
            .build_return(Some(&aggregate))
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        Ok(())
    }

    pub(super) fn emit_actor_trap_status(
        &mut self,
        status: i32,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let i32_ty = self.generator.context.i32_type();
        self.emit_actor_status_return(
            i32_ty.const_int(status as u64, true),
            i32_ty.const_zero(),
            subject,
        )
    }

    pub(super) fn emit_actor_status_guard(
        &mut self,
        status: IntValue<'ctx>,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let i32_ty = self.generator.context.i32_type();
        let success = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                i32_ty.const_zero(),
                "actor_status_success",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let pass = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "actor_status_pass");
        let fail = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "actor_status_propagate");
        self.generator
            .builder
            .build_conditional_branch(success, pass, fail)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(fail);
        self.emit_actor_status_return(status, i32_ty.const_zero(), subject)?;
        self.generator.builder.position_at_end(pass);
        Ok(())
    }

    pub(super) fn emit_actor_condition_guard(
        &mut self,
        valid: IntValue<'ctx>,
        status: i32,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let pass = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "actor_condition_pass");
        let fail = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "actor_condition_failure");
        self.generator
            .builder
            .build_conditional_branch(valid, pass, fail)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(fail);
        self.emit_actor_trap_status(status, subject)?;
        self.generator.builder.position_at_end(pass);
        Ok(())
    }

    pub(super) fn emit_actor_tagged_call(
        &mut self,
        result: Option<&MirValueId>,
        owner: &NodeId,
        arguments: &[MirValueId],
        subject: &str,
    ) -> Result<(), NativeMirError> {
        if !is_actor_tagged(self.program, &self.function.owner)
            || !is_actor_tagged(self.program, owner)
            || self.program.actors().method(owner).is_some()
        {
            return Err(NativeMirError::new(
                subject,
                "tagged direct call bypasses its graph or Actor mailbox ABI",
            ));
        }
        let result_id = result.ok_or_else(|| {
            NativeMirError::new(
                subject,
                "tagged direct call requires its i32 result identity",
            )
        })?;
        let target = *self
            .functions
            .get(owner)
            .ok_or_else(|| NativeMirError::new(subject, "tagged direct callee is undeclared"))?;
        let mut values = arguments
            .iter()
            .map(|argument| {
                self.value(argument, subject)
                    .map(BasicMetadataValueEnum::from)
            })
            .collect::<Result<Vec<_>, _>>()?;
        values.push(self.actor_domain_value(subject)?.into());
        let call = self
            .generator
            .builder
            .build_call(target, &values, "actor_helper_call")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let tagged = call_try_basic_value(&call)
            .ok_or_else(|| NativeMirError::new(subject, "tagged helper returned void"))?
            .into_struct_value();
        let status = self
            .generator
            .builder
            .build_extract_value(tagged, 0, "actor_helper_status")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        self.emit_actor_status_guard(status, subject)?;
        let value = self
            .generator
            .builder
            .build_extract_value(tagged, 1, "actor_helper_value")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.values.insert(result_id.clone(), value);
        Ok(())
    }
}

impl<'a, 'ctx> NativeMirEmitter<'a, 'ctx> {
    pub(super) fn emit_actor_root_wrapper(&mut self) -> Result<(), NativeMirError> {
        let Some(plan) = self.program.actor_execution_plan() else {
            return Ok(());
        };
        let subject = &plan.root.0;
        let target = *self
            .functions
            .get(&plan.root)
            .ok_or_else(|| NativeMirError::new(subject, "tagged Actor root is undeclared"))?;
        let symbol =
            mir_symbol(&plan.root).map_err(|message| NativeMirError::new(subject, message))?;
        if self.generator.module.get_function(&symbol).is_some() {
            return Err(NativeMirError::new(
                subject,
                "Actor root public symbol already exists",
            ));
        }
        let context = self.generator.context;
        let i32_ty = context.i32_type();
        let i64_ty = context.i64_type();
        let ptr_ty = context.ptr_type(inkwell::AddressSpace::default());
        let wrapper = self.generator.module.add_function(
            &symbol,
            i32_ty.fn_type(&[], false),
            Some(Linkage::External),
        );
        let entry = context.append_basic_block(wrapper, "actor_root_open");
        let execute = context.append_basic_block(wrapper, "actor_root_execute");
        let failed_open = context.append_basic_block(wrapper, "actor_root_open_failed");
        let done = context.append_basic_block(wrapper, "actor_root_success");
        let report = context.append_basic_block(wrapper, "actor_root_report_after_join");
        let runtime = |name: &str| {
            self.generator.module.get_function(name).ok_or_else(|| {
                NativeMirError::new(subject, format!("Actor runtime '{name}' is undeclared"))
            })
        };
        let open_fn = runtime("mimi_mir_actor_domain_open")?;
        let close_fn = runtime("mimi_mir_actor_domain_close")?;
        let destroy_fn = runtime("mimi_mir_actor_domain_destroy")?;
        self.generator.builder.position_at_end(entry);
        let call = self
            .generator
            .builder
            .build_call(open_fn, &[i64_ty.const_zero().into()], "actor_root_domain")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let domain = call_try_basic_value(&call)
            .ok_or_else(|| NativeMirError::new(subject, "Actor domain open returned void"))?
            .into_int_value();
        let opened = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                domain,
                i64_ty.const_zero(),
                "actor_root_opened",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_conditional_branch(opened, execute, failed_open)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(failed_open);
        self.emit_actor_external_abort(
            "[E0800] canonical Actor execution domain could not be created",
            subject,
        )?;
        self.generator.builder.position_at_end(execute);
        let call = self
            .generator
            .builder
            .build_call(target, &[domain.into()], "actor_root_tagged")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let result: StructValue<'ctx> = call_try_basic_value(&call)
            .ok_or_else(|| NativeMirError::new(subject, "Actor tagged root returned void"))?
            .into_struct_value();
        let status = self
            .generator
            .builder
            .build_extract_value(result, 0, "actor_root_status")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let source_value = self
            .generator
            .builder
            .build_extract_value(result, 1, "actor_root_value")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let failed = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                status,
                i32_ty.const_zero(),
                "actor_root_failed",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let cancel = self
            .generator
            .builder
            .build_int_z_extend(failed, i32_ty, "actor_root_cancel")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        // close is the join fence. destroy always runs, including close failure,
        // and independently joins before unregistering the unique root owner.
        let close_call = self
            .generator
            .builder
            .build_call(
                close_fn,
                &[domain.into(), cancel.into(), ptr_ty.const_null().into()],
                "actor_root_close_join",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let close_status = call_try_basic_value(&close_call)
            .ok_or_else(|| NativeMirError::new(subject, "Actor close returned void"))?
            .into_int_value();
        let destroy_call = self
            .generator
            .builder
            .build_call(destroy_fn, &[domain.into()], "actor_root_destroy")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let destroy_status = call_try_basic_value(&destroy_call)
            .ok_or_else(|| NativeMirError::new(subject, "Actor destroy returned void"))?
            .into_int_value();
        let close_failed = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                close_status,
                i32_ty.const_zero(),
                "actor_root_close_failed",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let cleanup_status = self
            .generator
            .builder
            .build_select(
                close_failed,
                close_status,
                destroy_status,
                "actor_root_cleanup_status",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let final_status = self
            .generator
            .builder
            .build_select(failed, status, cleanup_status, "actor_root_final_status")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let succeeded = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                final_status,
                i32_ty.const_zero(),
                "actor_root_all_settled",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_conditional_branch(succeeded, done, report)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(done);
        self.generator
            .builder
            .build_return(Some(&source_value))
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(report);
        let div_zero = context.append_basic_block(wrapper, "actor_report_div_zero");
        let overflow = context.append_basic_block(wrapper, "actor_report_overflow");
        let runtime_failure = context.append_basic_block(wrapper, "actor_report_runtime_failure");
        self.generator
            .builder
            .build_switch(
                final_status,
                runtime_failure,
                &[
                    (i32_ty.const_int(801, false), div_zero),
                    (i32_ty.const_int(802, false), overflow),
                ],
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(div_zero);
        let trap = self
            .generator
            .get_runtime_fn("mimi_trap_div_by_zero")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(trap, &[], "actor_report_e0801")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_unreachable()
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(overflow);
        let trap = self
            .generator
            .get_runtime_fn("mimi_trap_overflow")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let operation = self
            .generator
            .builder
            .build_global_string_ptr(
                "canonical Actor integer arithmetic",
                "actor_overflow_operation",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(
                trap,
                &[operation.as_pointer_value().into()],
                "actor_report_e0802",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_unreachable()
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(runtime_failure);
        self.emit_actor_external_abort(
            "[E0800] canonical Actor execution failed after domain close/join/destroy",
            subject,
        )?;
        Ok(())
    }

    fn emit_actor_external_abort(
        &mut self,
        message: &str,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let function = self.generator.get_or_declare_abort_fn();
        let message = self
            .generator
            .builder
            .build_global_string_ptr(message, "actor_root_failure_message")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(
                function,
                &[message.as_pointer_value().into()],
                "actor_root_report_failure",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_unreachable()
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::actor_domain::{self as domain, ActorDomainCounts};
    use inkwell::execution_engine::ExecutionEngine;
    use inkwell::values::AnyValue;
    use inkwell::OptimizationLevel;
    use std::cell::RefCell;

    const CAPTURE: &str = include_str!("../../../tests/real_world/actor_nested_func_capture.mimi");
    const SHADOW: &str = include_str!("../../../tests/real_world/actor_nested_func_shadow.mimi");
    const REPEATED: &str = r#"
        actor Counter {
            base: i32 = 0;
            func bump(delta: i32) -> i32 {
                let scale = 10
                func scaled(d: i32) -> i32 { scale + d }
                self.base = self.base + scaled(delta)
                self.base
            }
        }
        func main() -> i32 {
            let c = Counter.spawn()
            println(c.bump(5))
            println(c.bump(7))
            0
        }
    "#;
    const TWO_INSTANCES: &str = r#"
        actor Counter {
            base: i32 = 3;
            func bump(delta: i32) -> i32 {
                let scale = 8
                let bias = 2
                func scaled(d: i32) -> i32 { scale + bias + d }
                self.base = self.base + scaled(delta)
                self.base
            }
        }
        func main() -> i32 {
            let c = Counter.spawn()
            let d = Counter.spawn()
            let first = c.bump(5)
            let second = c.bump(7)
            let other = d.bump(1)
            first + second + other
        }
    "#;

    fn checked_source(source: &str) -> crate::core::CheckedProgram {
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("Actor source lexing");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("Actor source parsing");
        crate::core::check_program(&file).expect("Actor source checking")
    }

    fn program(source: &str) -> MirProgram {
        MirProgram::from_checked_program(&checked_source(source))
            .expect("Actor canonical materialization")
    }

    #[derive(Default)]
    struct CloseTrace {
        closes: Vec<(u64, i32, i32, ActorDomainCounts)>,
        destroys: Vec<(u64, i32, i32)>,
    }
    thread_local! { static TRACE: RefCell<CloseTrace> = RefCell::new(CloseTrace::default()); }

    // SAFETY: this observer uses the exact verified runtime close C ABI;
    // non-null output pointers must satisfy its writable/aligned contract.
    unsafe extern "C" fn observed_close(
        domain_id: u64,
        cancel: i32,
        output: *mut ActorDomainCounts,
    ) -> i32 {
        let mut counts = ActorDomainCounts::default();
        // SAFETY: counts is writable and correctly aligned for the real C ABI.
        let status = unsafe { domain::mimi_mir_actor_domain_close(domain_id, cancel, &mut counts) };
        if !output.is_null() {
            // SAFETY: JIT-generated root passes null; any other caller must
            // obey the runtime's writable/aligned counts-out C contract.
            unsafe {
                *output = counts;
            }
        }
        TRACE.with(|trace| {
            trace
                .borrow_mut()
                .closes
                .push((domain_id, cancel, status, counts))
        });
        status
    }

    extern "C" fn observed_destroy(domain_id: u64) -> i32 {
        let status = domain::mimi_mir_actor_domain_destroy(domain_id);
        // SAFETY: the registry-observation call uses no output pointer.
        let after = unsafe {
            // SAFETY: null output requests registry observation only.
            domain::mimi_mir_actor_domain_close(domain_id, 1, std::ptr::null_mut())
        };
        TRACE.with(|trace| trace.borrow_mut().destroys.push((domain_id, status, after)));
        status
    }

    fn map_jit_runtime<'ctx>(engine: &ExecutionEngine<'ctx>, generator: &CodeGenerator<'ctx>) {
        let symbols = [
            (
                "mimi_mir_actor_domain_open",
                domain::mimi_mir_actor_domain_open as *const () as usize,
            ),
            (
                "mimi_mir_actor_domain_spawn",
                domain::mimi_mir_actor_domain_spawn as *const () as usize,
            ),
            (
                "mimi_mir_actor_domain_call",
                domain::mimi_mir_actor_domain_call as *const () as usize,
            ),
            (
                "mimi_mir_actor_domain_close",
                observed_close as *const () as usize,
            ),
            (
                "mimi_mir_actor_domain_destroy",
                observed_destroy as *const () as usize,
            ),
            ("printf", libc::printf as *const () as usize),
            ("puts", libc::puts as *const () as usize),
            (
                "mimi_runtime_abort",
                crate::runtime::mimi_runtime_abort as *const () as usize,
            ),
            (
                "mimi_trap_div_by_zero",
                crate::runtime::mimi_trap_div_by_zero as *const () as usize,
            ),
            (
                "mimi_trap_overflow",
                crate::runtime::mimi_trap_overflow as *const () as usize,
            ),
            (
                "__mimi_pow_i64",
                crate::runtime::__mimi_pow_i64 as *const () as usize,
            ),
            (
                "__mimi_pow_i64_status",
                crate::runtime::__mimi_pow_i64_status as *const () as usize,
            ),
            (
                "mimi_trap_div_overflow",
                crate::runtime::mimi_trap_div_overflow as *const () as usize,
            ),
        ];
        for (name, address) in symbols {
            if let Some(function) = generator.module.get_function(name) {
                engine.add_global_mapping(&function, address);
            }
        }
    }

    fn assert_quiescent(counts: ActorDomainCounts, spawned: u64, accepted: u64) {
        assert_eq!(
            counts.live_roots, 1,
            "root remains until destroy: {counts:?}"
        );
        assert_eq!(counts.live_workers, 0, "{counts:?}");
        assert_eq!(counts.live_states, 0, "{counts:?}");
        assert_eq!(counts.live_controls, 0, "{counts:?}");
        assert_eq!(counts.live_tasks, 0, "{counts:?}");
        assert_eq!(counts.pending_replies, 0, "{counts:?}");
        assert_eq!(counts.live_message_buffers, 0, "{counts:?}");
        assert_eq!(counts.live_reply_buffers, 0, "{counts:?}");
        assert_eq!(counts.spawned_actors, spawned, "{counts:?}");
        assert_eq!(counts.joined_workers, spawned, "{counts:?}");
        assert_eq!(counts.accepted_messages, accepted, "{counts:?}");
        assert_eq!(counts.settled_replies, accepted, "{counts:?}");
    }

    #[test]
    fn canonical_actor_native_jit_preserves_fixtures_state_and_domain_join() {
        for (source, expected, spawned, accepted) in [
            (CAPTURE, 0, 1, 1),
            (SHADOW, 0, 1, 1),
            (REPEATED, 0, 1, 2),
            (TWO_INSTANCES, 67, 2, 3),
        ] {
            TRACE.with(|trace| *trace.borrow_mut() = CloseTrace::default());
            let program = program(source);
            let context = Context::create();
            let mut generator = CodeGenerator::new(&context, "canonical_actor_jit");
            generator
                .compile_mir_native(&program)
                .expect("Actor native emission");
            generator
                .module
                .verify()
                .expect("Actor native LLVM verification");
            let plan = program
                .actor_execution_plan()
                .expect("Actor execution plan");
            for owner in &plan.tagged_functions {
                let function = generator
                    .module
                    .get_function(&actor_native_symbol(&program, owner).expect("tagged identity"))
                    .expect("all reachable declarations are tagged");
                assert_eq!(function.get_linkage(), Linkage::Internal);
                assert_eq!(
                    function.get_type().get_return_type(),
                    Some(tagged_i32_type(&context).into())
                );
                for block in function.get_basic_blocks() {
                    for instruction in block.get_instructions() {
                        let text = instruction.print_to_string().to_string();
                        assert!(
                            !text.contains("@mimi_runtime_abort"),
                            "worker/root helper abort: {text}"
                        );
                        assert!(
                            !text.contains("@mimi_trap_"),
                            "worker/root helper direct trap: {text}"
                        );
                    }
                }
            }
            let engine = generator
                .module
                .create_jit_execution_engine(OptimizationLevel::None)
                .expect("Actor JIT engine");
            map_jit_runtime(&engine, &generator);
            // SAFETY: public main has the verified argument-free signed-i32
            // C ABI. This test invokes only successful programs; errors are
            // exercised through status-only shims and real child processes.
            let actual = unsafe {
                engine
                    .get_function::<unsafe extern "C" fn() -> i32>("main") // SAFETY: verified public main has the exact no-argument i32 C ABI.
                    .expect("public Actor main")
                    .call()
            };
            assert_eq!(actual, expected);
            TRACE.with(|trace| {
                let trace = trace.borrow();
                assert_eq!(trace.closes.len(), 1);
                assert_eq!(trace.destroys.len(), 1);
                let (identity, cancel, status, counts) = trace.closes[0];
                assert_eq!((cancel, status), (0, 0));
                assert_quiescent(counts, spawned, accepted);
                assert_eq!(
                    trace.destroys[0],
                    (identity, 0, domain::MIR_ACTOR_STATUS_CLOSED)
                );
            });
        }
    }

    fn trap_source(location: &str, arithmetic: &str) -> String {
        let arithmetic = match arithmetic {
            "7 / 0" => "7 / test_delta",
            "2147483647 + 1" => "2147483647 + (test_delta + 1)",
            "(-2147483647 - 1) / -1" => "(-2147483647 - 1) / (test_delta - 1)",
            "negative power exponent" => {
                "let pow_base = test_delta + 2; let pow_exponent = test_delta - 1; pow_base ** pow_exponent"
            }
            "power intermediate overflow" => {
                "let pow_base = test_delta + 4; let pow_exponent = test_delta + 32; pow_base ** pow_exponent"
            }
            _ => panic!("unknown runtime arithmetic test"),
        };
        match location {
            "worker" => format!(
                r#"actor Counter {{ n: i32 = 5; func step(x: i32) -> i32 {{ let test_delta = x - x; {arithmetic} }} }} func main() -> i32 {{ let c = Counter.spawn(); c.step(0) }}"#
            ),
            "worker_helper" => format!(
                r#"actor Counter {{ n: i32 = 5; func step(x: i32) -> i32 {{ func crash(y: i32) -> i32 {{ let test_delta = y - y; {arithmetic} }} crash(x) }} }} func main() -> i32 {{ let c = Counter.spawn(); c.step(0) }}"#
            ),
            "root_helper" => format!(
                r#"func crash(x: i32) -> i32 {{ let test_delta = x - x; {arithmetic} }} actor Counter {{ n: i32 = 5; func step(x: i32) -> i32 {{ self.n + x }} }} func main() -> i32 {{ let c = Counter.spawn(); let r = c.step(0); crash(r) }}"#
            ),
            "root" => format!(
                r#"actor Counter {{ n: i32 = 5; func step(x: i32) -> i32 {{ self.n + x }} }} func main() -> i32 {{ let c = Counter.spawn(); let r = c.step(0); let test_delta = r - r; {arithmetic} }}"#
            ),
            _ => panic!("unknown Actor trap test location"),
        }
    }

    #[test]
    fn canonical_actor_native_jit_tagged_traps_propagate_801_802_and_join() {
        for location in ["root", "root_helper", "worker", "worker_helper"] {
            for (arithmetic, expected) in [
                ("7 / 0", 801),
                ("2147483647 + 1", 802),
                ("(-2147483647 - 1) / -1", 802),
                ("negative power exponent", 800),
                ("power intermediate overflow", 802),
            ] {
                let source = trap_source(location, arithmetic);
                let program = program(&source);
                let context = Context::create();
                let mut generator = CodeGenerator::new(&context, "canonical_actor_status_jit");
                generator
                    .compile_mir_native(&program)
                    .expect("Actor trapping native emission");
                let plan = program.actor_execution_plan().expect("Actor status plan");
                let root = generator
                    .module
                    .get_function(
                        &actor_native_symbol(&program, &plan.root).expect("root identity"),
                    )
                    .expect("tagged root");
                // Keep the private aggregate ABI inside LLVM. The JIT host
                // observes only a plain i32 status, avoiding assumptions about
                // a platform's host-language small-struct return convention.
                let shim = generator.module.add_function(
                    "__actor_test_status",
                    context
                        .i32_type()
                        .fn_type(&[context.i64_type().into()], false),
                    Some(Linkage::External),
                );
                generator
                    .builder
                    .position_at_end(context.append_basic_block(shim, "entry"));
                let domain_argument = shim.get_first_param().expect("shim domain argument");
                let call = generator
                    .builder
                    .build_call(root, &[domain_argument.into()], "tagged_root")
                    .expect("shim root call");
                let value = call_try_basic_value(&call)
                    .expect("tagged root aggregate")
                    .into_struct_value();
                let status = generator
                    .builder
                    .build_extract_value(value, 0, "status")
                    .expect("shim status");
                generator
                    .builder
                    .build_return(Some(&status))
                    .expect("shim status return");
                generator
                    .module
                    .verify()
                    .expect("status shim LLVM verification");
                let engine = generator
                    .module
                    .create_jit_execution_engine(OptimizationLevel::None)
                    .expect("Actor status JIT");
                map_jit_runtime(&engine, &generator);
                let identity = domain::mimi_mir_actor_domain_open(0);
                assert_ne!(identity, 0);
                // SAFETY: the verified shim has the exact u64 -> i32 ABI and
                // executes only status-returning source functions.
                let status = unsafe {
                    engine
                        .get_function::<unsafe extern "C" fn(u64) -> i32>("__actor_test_status") // SAFETY: verified shim takes u64 and returns i32.
                        .expect("status-only JIT shim")
                        .call(identity)
                };
                let mut counts = ActorDomainCounts::default();
                // SAFETY: counts is a writable, aligned runtime C result.
                let close = unsafe {
                    // SAFETY: counts is writable and aligned for the runtime C ABI.
                    domain::mimi_mir_actor_domain_close(identity, 1, &mut counts)
                };
                let destroy = domain::mimi_mir_actor_domain_destroy(identity);
                assert_eq!(status, expected, "{location}: {arithmetic}");
                assert_quiescent(counts, 1, 1);
                if location.starts_with("worker") {
                    assert_eq!(close, expected);
                    assert_eq!(destroy, expected);
                }
                // SAFETY: post-destroy observation has no output pointer.
                assert_eq!(
                    unsafe {
                        // SAFETY: null output requests observation of the destroyed registry entry.
                        domain::mimi_mir_actor_domain_close(identity, 1, std::ptr::null_mut())
                    },
                    domain::MIR_ACTOR_STATUS_CLOSED
                );
            }
        }
    }

    #[test]
    fn canonical_actor_native_jit_power_wrap_and_traps_join_before_reporting() {
        for (expression, expected_status, expected_value) in [
            (
                "let delta = x - x; let base = delta + 2; let exponent = delta + 31; base ** exponent",
                0,
                i32::MIN,
            ),
            (
                "let delta = x - x; let base = delta + 2; let exponent = delta - 1; base ** exponent",
                800,
                0,
            ),
            (
                "let delta = x - x; let base = delta + 4; let exponent = delta + 32; base ** exponent",
                802,
                0,
            ),
        ] {
            let source = format!(
                r#"actor Counter {{ n: i32 = 5; func step(x: i32) -> i32 {{ {expression} }} }} func main() -> i32 {{ let c = Counter.spawn(); c.step(0) }}"#
            );
            let program = program(&source);
            let context = Context::create();
            let mut generator = CodeGenerator::new(&context, "canonical_actor_power_status_jit");
            generator
                .compile_mir_native(&program)
                .expect("Actor checked-Power native emission");
            let plan = program.actor_execution_plan().expect("Actor status plan");
            let root = generator
                .module
                .get_function(&actor_native_symbol(&program, &plan.root).expect("root identity"))
                .expect("tagged root");

            // Keep the private Actor aggregate ABI inside LLVM. The host sees
            // only the status plus an explicit i32 result slot.
            let i32_ty = context.i32_type();
            let ptr_ty = context.ptr_type(inkwell::AddressSpace::default());
            let shim = generator.module.add_function(
                "__actor_test_power_status_value",
                i32_ty.fn_type(&[context.i64_type().into(), ptr_ty.into()], false),
                Some(Linkage::External),
            );
            generator
                .builder
                .position_at_end(context.append_basic_block(shim, "entry"));
            let domain_argument = shim.get_first_param().expect("shim domain argument");
            let output_pointer = shim.get_nth_param(1).expect("shim output pointer");
            let call = generator
                .builder
                .build_call(root, &[domain_argument.into()], "tagged_root")
                .expect("shim root call");
            let actor_result = call_try_basic_value(&call)
                .expect("tagged root aggregate")
                .into_struct_value();
            let status = generator
                .builder
                .build_extract_value(actor_result, 0, "status")
                .expect("shim status");
            let value = generator
                .builder
                .build_extract_value(actor_result, 1, "value")
                .expect("shim value");
            generator
                .builder
                .build_store(output_pointer.into_pointer_value(), value)
                .expect("shim result store");
            generator
                .builder
                .build_return(Some(&status))
                .expect("shim status return");
            generator
                .module
                .verify()
                .expect("Power status shim LLVM verification");
            let engine = generator
                .module
                .create_jit_execution_engine(OptimizationLevel::None)
                .expect("Actor Power status JIT");
            map_jit_runtime(&engine, &generator);

            let identity = domain::mimi_mir_actor_domain_open(0);
            assert_ne!(identity, 0);
            let mut value = 0_i32;
            // SAFETY: the verified shim takes a domain id and writable i32
            // output slot, then returns only the tagged root status.
            let status = unsafe {
                // SAFETY: the verified LLVM shim has exactly this C ABI.
                engine
                    .get_function::<unsafe extern "C" fn(u64, *mut i32) -> i32>(
                        "__actor_test_power_status_value",
                    )
                    .expect("Power status/value shim")
                    .call(identity, &mut value)
            };
            let mut counts = ActorDomainCounts::default();
            // SAFETY: counts is writable and aligned for the runtime C ABI.
            let close = unsafe {
                domain::mimi_mir_actor_domain_close(identity, 1, &mut counts)
            };
            let destroy = domain::mimi_mir_actor_domain_destroy(identity);

            assert_eq!(status, expected_status, "{expression}");
            if expected_status == 0 {
                assert_eq!(value, expected_value, "{expression}");
            }
            // The worker reply is settled and joined before status is
            // observed at the root; both error cases keep the process alive.
            assert_quiescent(counts, 1, 1);
            assert_eq!(close, expected_status, "{expression}");
            assert_eq!(destroy, expected_status, "{expression}");
            // SAFETY: null output requests observation of the destroyed entry.
            assert_eq!(
                // SAFETY: a null output is accepted for this observation-only query.
                unsafe {
                    domain::mimi_mir_actor_domain_close(
                        identity,
                        1,
                        std::ptr::null_mut(),
                    )
                },
                domain::MIR_ACTOR_STATUS_CLOSED,
                "{expression}"
            );
        }
    }

    const NATIVE_C_OBSERVER: &str = r#"
        #include <stdint.h>
        #include <stdio.h>
        typedef struct {
            uint64_t live_roots, live_workers, live_states, live_controls;
            uint64_t live_tasks, pending_replies, live_message_buffers, live_reply_buffers;
            uint64_t spawned_actors, accepted_messages, settled_replies, cancelled_replies;
            uint64_t joined_workers, joined_tasks;
        } ActorCounts;
        extern int32_t mimi_mir_actor_domain_close(uint64_t, int32_t, ActorCounts *);
        extern int32_t mimi_mir_actor_domain_destroy(uint64_t);
        int32_t __mimi_actor_test_close(uint64_t domain, int32_t cancel, ActorCounts *ignored) {
            ActorCounts c = {0};
            int32_t status = mimi_mir_actor_domain_close(domain, cancel, &c);
            int valid = c.live_roots == 1 && c.live_workers == 0 && c.live_states == 0
                && c.live_controls == 0 && c.live_tasks == 0 && c.pending_replies == 0
                && c.live_message_buffers == 0 && c.live_reply_buffers == 0
                && c.spawned_actors > 0 && c.spawned_actors == c.joined_workers
                && c.accepted_messages == c.settled_replies;
            fprintf(stderr, "[actor-test-close cleanup=%s cancel=%d status=%d spawned=%llu joined=%llu]\n",
                valid ? "ok" : "failed", cancel, status,
                (unsigned long long)c.spawned_actors, (unsigned long long)c.joined_workers);
            if (ignored) *ignored = c;
            return status;
        }
        int32_t __mimi_actor_test_destroy(uint64_t domain) {
            int32_t status = mimi_mir_actor_domain_destroy(domain);
            int32_t after = mimi_mir_actor_domain_close(domain, 1, NULL);
            fprintf(stderr, "[actor-test-destroy closed=%d status=%d]\n", after, status);
            return status;
        }
    "#;

    fn observe_real_native(source: &str, valgrind: bool) -> crate::tests::NativeRunObservation {
        observe_real_native_route(source, valgrind, false)
    }

    fn observe_real_native_route(
        source: &str,
        valgrind: bool,
        direct: bool,
    ) -> crate::tests::NativeRunObservation {
        let checked = checked_source(source);
        let program = MirProgram::from_checked_program(&checked).expect("same Actor source graph");
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "canonical_actor_real_process");
        if direct {
            crate::core::CheckedProgram::reset_test_legacy_body_access();
            generator
                .compile_checked(&checked)
                .expect("production direct Actor native route");
            assert!(
                crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
                "direct Actor emitter must not consume legacy bodies"
            );
            assert_eq!(
                generator.test_mir_native_route_receipt(),
                Some(&program.route_receipt(crate::core::mir::MIR_NATIVE_DIRECT_ROUTE_PROFILE))
            );
        } else {
            generator
                .compile_mir_native(&program)
                .expect("production Actor MIR emission");
        }
        // Only add runtime observations to the already-emitted module. Both
        // C observers invoke the real standalone runtime and preserve status;
        // no execution behavior or source fixture is replaced.
        generator
            .module
            .get_function("mimi_mir_actor_domain_close")
            .expect("real close ABI")
            .as_global_value()
            .set_name("__mimi_actor_test_close");
        generator
            .module
            .get_function("mimi_mir_actor_domain_destroy")
            .expect("real destroy ABI")
            .as_global_value()
            .set_name("__mimi_actor_test_destroy");
        generator
            .module
            .verify()
            .expect("observed native Actor LLVM module");
        crate::tests::link_and_observe_canonical_mir_with_config(
            &generator,
            &crate::tests::E2EConfig {
                extra_c_src: Some(NATIVE_C_OBSERVER.into()),
                use_valgrind: valgrind,
                valgrind_args: vec![
                    "--tool=memcheck".into(),
                    "--leak-check=full".into(),
                    "--show-leak-kinds=all".into(),
                    "--errors-for-leak-kinds=definite,indirect,possible".into(),
                    "--error-exitcode=99".into(),
                ],
                run_timeout: Some(std::time::Duration::from_secs(30)),
                retain_tmp_if_requested: true,
                ..crate::tests::E2EConfig::default()
            },
        )
        .expect("real native Actor process linked against production runtime")
    }

    fn assert_real_native_closed(observation: &crate::tests::NativeRunObservation, cancel: i32) {
        assert!(
            observation
                .stderr
                .contains(&format!("[actor-test-close cleanup=ok cancel={cancel}")),
            "{}",
            observation.stderr
        );
        assert!(
            observation.stderr.contains("[actor-test-destroy closed=-2"),
            "{}",
            observation.stderr
        );
        assert_eq!(observation.stderr.matches("[actor-test-close").count(), 1);
        assert_eq!(observation.stderr.matches("[actor-test-destroy").count(), 1);
        assert!(
            observation.stderr.find("[actor-test-close").unwrap()
                < observation.stderr.find("[actor-test-destroy").unwrap()
        );
    }

    #[test]
    fn canonical_actor_direct_native_does_not_trim_unused_user_contracts() {
        for predicate in ["result == x", "result != x"] {
            let source = format!("{CAPTURE}\nfunc guarded(x: i32) -> i32 {{\nrequires: x > 0\nensures: {predicate}\nx\n}}");
            let checked = checked_source(&source);
            let context = Context::create();
            let mut generator = CodeGenerator::new(&context, "actor_user_proof_compatibility");
            crate::core::CheckedProgram::reset_test_legacy_body_access();
            // This source is outside the execution-only Actor profile. The
            // retained native owner may reject its unsupported capture, but it
            // must never report a canonical receipt for a trimmed proof graph.
            let _compatibility_result = generator.compile_checked(&checked);
            assert!(generator.test_mir_native_route_receipt().is_none());
            assert!(!crate::core::CheckedProgram::test_legacy_body_access().is_empty());
        }
    }

    #[test]
    fn canonical_actor_route_direct_native_originals_and_traps_close_join_destroy() {
        for (source, stdout) in [(CAPTURE, "15\n"), (SHADOW, "42\n101\n")] {
            let observed = observe_real_native_route(source, false, true);
            assert_eq!(observed.exit_code, Some(0), "{}", observed.stderr);
            assert_eq!(observed.stdout, stdout);
            assert_real_native_closed(&observed, 0);
            eprintln!(
                "Actor direct native normal close report:\n{}",
                observed.stderr
            );
        }
        for location in ["root", "root_helper", "worker", "worker_helper"] {
            for (arithmetic, code) in [
                ("7 / 0", "E0801"),
                ("2147483647 + 1", "E0802"),
                ("(-2147483647 - 1) / -1", "E0802"),
                ("negative power exponent", "E0800"),
                ("power intermediate overflow", "E0802"),
            ] {
                let observed =
                    observe_real_native_route(&trap_source(location, arithmetic), false, true);
                assert_eq!(observed.exit_code, Some(1), "{}", observed.stderr);
                assert_eq!(observed.stdout, "");
                assert_real_native_closed(&observed, 1);
                assert!(
                    observed.stderr.find("[actor-test-destroy").unwrap()
                        < observed
                            .stderr
                            .find(code)
                            .expect("root reports trap after destruction")
                );
                eprintln!(
                    "Actor direct native trap close report:\n{}",
                    observed.stderr
                );
            }
        }
    }

    #[test]
    fn canonical_actor_native_real_fixtures_and_instances_close_join_destroy() {
        for (source, stdout, exit) in [
            (CAPTURE, "15\n", 0),
            (SHADOW, "42\n101\n", 0),
            (REPEATED, "15\n32\n", 0),
            (TWO_INSTANCES, "", 67),
            (
                "actor Counter { n: i32 = 5; func step(x: i32) -> i32 { let delta = x - x; let base = delta + 2; let exponent = delta + 31; base ** exponent } } func main() -> i32 { let c = Counter.spawn(); println(c.step(0)); 0 }",
                "-2147483648\n",
                0,
            ),
        ] {
            let observed = observe_real_native(source, false);
            assert_eq!(observed.exit_code, Some(exit), "{}", observed.stderr);
            assert_eq!(observed.stdout, stdout);
            assert_real_native_closed(&observed, 0);
        }
    }

    #[test]
    fn canonical_actor_native_real_root_worker_helper_traps_close_before_report() {
        for location in ["root", "root_helper", "worker", "worker_helper"] {
            for (arithmetic, code) in [
                ("7 / 0", "E0801"),
                ("2147483647 + 1", "E0802"),
                ("(-2147483647 - 1) / -1", "E0802"),
                ("negative power exponent", "E0800"),
                ("power intermediate overflow", "E0802"),
            ] {
                let observed = observe_real_native(&trap_source(location, arithmetic), false);
                assert_eq!(
                    observed.exit_code,
                    Some(1),
                    "{location}: {}",
                    observed.stderr
                );
                assert_eq!(observed.stdout, "");
                assert_real_native_closed(&observed, 1);
                let trap = observed
                    .stderr
                    .find(code)
                    .expect("native registered arithmetic trap code");
                assert!(
                    observed.stderr.find("[actor-test-destroy").unwrap() < trap,
                    "trap reported before join/destroy: {}",
                    observed.stderr
                );
            }
        }
    }

    #[test]
    fn canonical_actor_native_real_normal_and_trap_memcheck_has_no_losses() {
        if !std::process::Command::new("valgrind")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            eprintln!(
                "Valgrind unavailable; real native success/trap/counter tests remain mandatory"
            );
            return;
        }
        let mut fixtures = vec![
            (CAPTURE.to_owned(), 0, 0, "15\n", None),
            (SHADOW.to_owned(), 0, 0, "42\n101\n", None),
            (trap_source("root", "7 / 0"), 1, 1, "", Some("E0801")),
            (
                trap_source("worker_helper", "2147483647 + 1"),
                1,
                1,
                "",
                Some("E0802"),
            ),
        ];
        // The Actor Power path has its own status-returning runtime ABI. Keep
        // real child-process Memcheck coverage for success and for both trap
        // classes at root, worker, and worker-helper sites. Each failure must
        // join/close/destroy its Actor before the root reports the E-code.
        fixtures.push((
            "actor Counter { n: i32 = 5; func step(x: i32) -> i32 { let delta = x - x; let base = delta + 2; let exponent = delta + 31; base ** exponent } } func main() -> i32 { let c = Counter.spawn(); println(c.step(0)); 0 }".to_owned(),
            0,
            0,
            "-2147483648\n",
            None,
        ));
        for location in ["root", "root_helper", "worker", "worker_helper"] {
            fixtures.push((
                trap_source(location, "negative power exponent"),
                1,
                1,
                "",
                Some("E0800"),
            ));
            fixtures.push((
                trap_source(location, "power intermediate overflow"),
                1,
                1,
                "",
                Some("E0802"),
            ));
        }
        for (source, expected, cancel, stdout, trap_code) in fixtures {
            let observed = observe_real_native(&source, true);
            eprintln!("Actor native Memcheck raw report:\n{}", observed.stderr);
            assert_eq!(observed.exit_code, Some(expected), "{}", observed.stderr);
            assert_eq!(observed.stdout, stdout, "{}", observed.stderr);
            assert_real_native_closed(&observed, cancel);
            if let Some(code) = trap_code {
                let report = observed
                    .stderr
                    .find(code)
                    .expect("root reports the typed trap code");
                assert!(
                    observed.stderr.find("[actor-test-destroy").unwrap() < report,
                    "trap reported before Actor destroy: {}",
                    observed.stderr
                );
            }
            assert!(
                observed.stderr.contains("ERROR SUMMARY: 0 errors"),
                "{}",
                observed.stderr
            );
            for summary in [
                "definitely lost: 0 bytes in 0 blocks",
                "indirectly lost: 0 bytes in 0 blocks",
                "possibly lost: 0 bytes in 0 blocks",
            ] {
                assert!(
                    observed.stderr.contains(summary)
                        || observed.stderr.contains("All heap blocks were freed"),
                    "{summary}: {}",
                    observed.stderr
                );
            }
        }
    }

    #[test]
    fn canonical_actor_native_rejects_forged_field_before_any_runtime_declaration() {
        let mut program = program(CAPTURE);
        let owner = program.actors().iter().next().expect("Actor").1.methods[0]
            .owner
            .clone();
        let mut function = program.functions().get(&owner).expect("method").clone();
        let operation = function
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find_map(|instruction| {
                if let MirInstructionKind::Actor {
                    operation: crate::core::mir::MirActorOperation::FieldStore { field, .. },
                } = &mut instruction.kind
                {
                    Some(field)
                } else {
                    None
                }
            })
            .expect("actual checked Actor field store");
        *operation = NodeId("forged:foreign-field".into());
        program.replace_function_for_test_only(function);
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "forged_actor_native");
        assert!(generator.compile_mir_native(&program).is_err());
        assert!(generator
            .module
            .get_function("mimi_mir_actor_domain_open")
            .is_none());
        assert!(generator.module.get_function("main").is_none());
    }
}
