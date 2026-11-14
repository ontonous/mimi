//! AST-free canonical Actor mailbox consumer and execution-owned shutdown.
//!
//! Bytecode preserves the typed catalog rather than rebuilding ActorDef. A
//! compiler-private manifest binds the complete instruction stream, constant
//! pools and dispatch table before either stdout or worker creation. The root
//! owner remains a local guard; the Send VM stores only weak participants.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use crate::core::mir::reference::MirProgram;
use crate::core::mir::{MirActorContract, MirActorOperation, MirInstructionKind};
use crate::core::NodeId;
use crate::interp::{InterpError, Value};
use crate::runtime::actor_domain::{
    ActorDomainCloseMode, ActorDomainCounts, ActorDomainError, ActorDomainHandle,
    ActorDomainMonitor, ActorDomainParticipant, ActorTurnContext, RootDomainOwner,
};

use super::instr::{BytecodeProgram, ConstValue, FuncIdx, FunctionProto, Op};
use super::BytecodeVM;

#[derive(Debug, Clone)]
struct ActorDispatch {
    contract: MirActorContract,
    functions: Vec<FuncIdx>,
}

/// In-process compiler provenance, independently sealed from compatibility
/// Actor metadata and from the FFI/Map receipts. This is deliberately private
/// to canonical bytecode materialization; public tables cannot create it.
#[derive(Debug, Clone)]
pub(crate) struct CanonicalActorManifest {
    actors: Vec<ActorDispatch>,
    worker_functions: BTreeSet<FuncIdx>,
    expected_program: String,
}

fn snapshot(entry: FuncIdx, functions: &[FunctionProto]) -> String {
    format!("mimi-canonical-actor-bytecode-v1:{entry}:{functions:?}")
}

pub(crate) fn function_has_actor_ops(function: &FunctionProto) -> bool {
    function.code.iter().any(|op| {
        matches!(
            op,
            Op::MirActorSpawn { .. }
                | Op::MirActorCall { .. }
                | Op::MirActorFieldLoad { .. }
                | Op::MirActorFieldStore { .. }
        )
    })
}

impl CanonicalActorManifest {
    pub(crate) fn materialize(
        program: &MirProgram,
        indices: &BTreeMap<NodeId, FuncIdx>,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<Option<Self>, String> {
        program
            .validate_actor_contracts()
            .map_err(|errors| errors.join("; "))?;
        let contracts: Vec<_> = program.actors().iter().collect();
        if contracts.is_empty() {
            if functions.iter().any(function_has_actor_ops) {
                return Err("canonical Actor opcodes have no typed catalog".into());
            }
            return Ok(None);
        }
        program
            .actors()
            .validate(program.type_catalog())
            .map_err(|errors| errors.join("; "))?;
        let plan = program
            .actor_execution_plan()
            .ok_or("canonical Actor graph has no finite worker execution plan")?;
        plan.validate_against(
            program.actors(),
            program.type_catalog(),
            program.functions(),
        )
        .map_err(|errors| errors.join("; "))?;
        let mut actors = Vec::with_capacity(contracts.len());
        for (_, contract) in contracts {
            let method_functions = contract
                .methods
                .iter()
                .map(|method| {
                    indices.get(&method.body_owner).copied().ok_or_else(|| {
                        format!(
                            "Actor body '{}' has no bytecode function",
                            method.body_owner.0
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            actors.push(ActorDispatch {
                contract: contract.clone(),
                functions: method_functions,
            });
        }
        let worker_functions =
            plan.worker_functions
                .iter()
                .map(|owner| {
                    indices.get(owner).copied().ok_or_else(|| {
                        format!("Actor worker '{}' has no bytecode function", owner.0)
                    })
                })
                .collect::<Result<BTreeSet<_>, _>>()?;
        let manifest = Self {
            actors,
            worker_functions,
            expected_program: snapshot(entry, functions),
        };
        manifest.validate_source_bindings(program, functions)?;
        manifest.validate_dispatch(functions)?;
        Ok(Some(manifest))
    }

    pub(crate) fn validate_program(program: &BytecodeProgram) -> Result<(), InterpError> {
        let has_ops = program.functions.iter().any(function_has_actor_ops);
        let Some(manifest) = &program.canonical_actors else {
            return if has_ops {
                Err(InterpError::new(
                    "canonical Actor bytecode has no checker-owned manifest",
                ))
            } else {
                Ok(())
            };
        };
        if program.ast.is_some()
            || !program.actor_defs.is_empty()
            || !program.actor_method_funcs.is_empty()
        {
            return Err(InterpError::new(
                "canonical Actor bytecode must have AST=None and empty compatibility Actor tables",
            ));
        }
        if snapshot(program.entry, &program.functions) != manifest.expected_program {
            return Err(InterpError::new(
                "canonical Actor bytecode disagrees with its checker-owned instruction snapshot",
            ));
        }
        manifest
            .validate_dispatch(&program.functions)
            .map_err(InterpError::new)
    }

    fn descriptor(&self, index: u32) -> Result<&ActorDispatch, InterpError> {
        self.actors.get(index as usize).ok_or_else(|| {
            InterpError::new(format!(
                "canonical Actor descriptor {index} is out of range"
            ))
        })
    }

    fn validate_source_bindings(
        &self,
        program: &MirProgram,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        let mut consumed = BTreeSet::new();
        let mut expected = BTreeSet::new();
        for (owner, function) in program.functions() {
            for instruction in function
                .blocks
                .values()
                .flat_map(|block| &block.instructions)
            {
                if matches!(&instruction.kind, MirInstructionKind::Actor { .. }) {
                    expected.insert((owner.0.clone(), instruction.id.as_str().to_owned()));
                }
            }
        }
        for function in functions {
            for op in &function.code {
                let (descriptor, instruction) = match op {
                    Op::MirActorSpawn {
                        descriptor,
                        instruction,
                        ..
                    }
                    | Op::MirActorCall {
                        descriptor,
                        instruction,
                        ..
                    }
                    | Op::MirActorFieldLoad {
                        descriptor,
                        instruction,
                        ..
                    }
                    | Op::MirActorFieldStore {
                        descriptor,
                        instruction,
                        ..
                    } => (*descriptor, *instruction),
                    _ => continue,
                };
                let Some(ConstValue::Str(instruction)) =
                    function.constants.get(instruction as usize)
                else {
                    return Err(
                        "canonical Actor opcode has no MIR instruction identity constant".into(),
                    );
                };
                if !consumed.insert((function.name.clone(), instruction.clone())) {
                    return Err(
                        "canonical Actor instruction has more than one executable binding".into(),
                    );
                }
                let source = program
                    .functions()
                    .get(&NodeId(function.name.clone()))
                    .and_then(|source| {
                        source
                            .blocks
                            .values()
                            .flat_map(|block| &block.instructions)
                            .find(|source| source.id.as_str() == instruction)
                    })
                    .ok_or("canonical Actor opcode has no source owner/instruction binding")?;
                let contract = &self
                    .descriptor(descriptor)
                    .map_err(|error| error.to_string())?
                    .contract;
                let matched = match (op, &source.kind) {
                    (
                        Op::MirActorSpawn { .. },
                        MirInstructionKind::Actor {
                            operation: MirActorOperation::Spawn { actor, .. },
                        },
                    ) => actor.0.as_str() == contract.actor.0.as_str(),
                    (
                        Op::MirActorCall { method, argc, .. },
                        MirInstructionKind::Actor {
                            operation:
                                MirActorOperation::Call {
                                    actor,
                                    method: source_method,
                                    args,
                                    ..
                                },
                        },
                    ) => {
                        actor.0.as_str() == contract.actor.0.as_str()
                            && args.len() == *argc as usize
                            && contract
                                .methods
                                .get(*method as usize)
                                .is_some_and(|method| {
                                    method.owner.0.as_str() == source_method.0.as_str()
                                })
                    }
                    (
                        Op::MirActorFieldLoad { field, .. },
                        MirInstructionKind::Actor {
                            operation:
                                MirActorOperation::FieldLoad {
                                    actor,
                                    field: source_field,
                                    ..
                                },
                        },
                    )
                    | (
                        Op::MirActorFieldStore { field, .. },
                        MirInstructionKind::Actor {
                            operation:
                                MirActorOperation::FieldStore {
                                    actor,
                                    field: source_field,
                                    ..
                                },
                        },
                    ) => {
                        actor.0.as_str() == contract.actor.0.as_str()
                            && contract.fields.get(*field as usize).is_some_and(|field| {
                                field.field.0.as_str() == source_field.0.as_str()
                            })
                    }
                    _ => false,
                };
                if !matched {
                    return Err(
                        "canonical Actor opcode disagrees with its typed MIR operation".into(),
                    );
                }
            }
        }
        if consumed != expected {
            return Err("canonical Actor source/executable bindings are incomplete".into());
        }
        Ok(())
    }

    fn validate_dispatch(&self, functions: &[FunctionProto]) -> Result<(), String> {
        let mut method_functions = BTreeSet::new();
        for actor in &self.actors {
            if actor.functions.len() != actor.contract.methods.len() {
                return Err("canonical Actor dispatch arity disagrees".into());
            }
            for (index, (method, function)) in actor
                .contract
                .methods
                .iter()
                .zip(&actor.functions)
                .enumerate()
            {
                let proto = functions
                    .get(*function as usize)
                    .ok_or("canonical Actor dispatch references a missing function")?;
                if method.index != index
                    || proto.name != method.body_owner.0
                    || proto.param_count as usize != method.execution_parameter_types.len()
                    || (method.flow_transition.is_none()
                        && method.execution_parameter_types.first()
                            != Some(&actor.contract.state_type))
                    || (method.flow_transition.is_some()
                        && method.execution_parameter_types.first()
                            != Some(&crate::core::ir::primitive_type_id(
                                crate::core::PrimitiveType::I32,
                            )))
                    || !self.worker_functions.contains(function)
                    || !method_functions.insert(*function)
                {
                    return Err(
                        "canonical Actor method owner/state/dispatch signature disagrees".into(),
                    );
                }
            }
        }
        for (function_index, function) in functions.iter().enumerate() {
            for op in &function.code {
                match op {
                    Op::ActorSpawn { .. } | Op::ActorSpawnDetached { .. } => {
                        return Err("compatibility Actor opcode in canonical Actor graph".into());
                    }
                    Op::Call { func, .. } | Op::CallMove { func, .. }
                        if method_functions.contains(func) =>
                    {
                        return Err("direct Actor method call bypasses the typed mailbox".into());
                    }
                    Op::MirActorFieldLoad {
                        descriptor, field, ..
                    }
                    | Op::MirActorFieldStore {
                        descriptor, field, ..
                    } => {
                        let actor = self
                            .descriptor(*descriptor)
                            .map_err(|error| error.to_string())?;
                        let field_contract = actor
                            .contract
                            .fields
                            .get(*field as usize)
                            .ok_or("canonical Actor field index is out of range")?;
                        if field_contract.index != *field as usize
                            || !actor.functions.contains(&(function_index as FuncIdx))
                        {
                            return Err(
                                "canonical Actor field opcode has a foreign method owner".into()
                            );
                        }
                    }
                    Op::MirActorSpawn { descriptor, .. } => {
                        self.descriptor(*descriptor)
                            .map_err(|error| error.to_string())?;
                    }
                    Op::MirActorCall {
                        descriptor,
                        method,
                        argc,
                        ..
                    } => {
                        let actor = self
                            .descriptor(*descriptor)
                            .map_err(|error| error.to_string())?;
                        let method = actor
                            .contract
                            .methods
                            .get(*method as usize)
                            .ok_or("canonical Actor method index is out of range")?;
                        if method.execution_parameter_types.len() != *argc as usize + 1 {
                            return Err("canonical Actor mailbox payload arity disagrees".into());
                        }
                    }
                    _ => {}
                }
            }
        }
        // Recheck the bytecode worker envelope independently of the MIR plan:
        // a valid manifest cannot smuggle blocking builtins, a nested Actor,
        // control-flow cycles or recursive helper calls into production.
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        for function in &self.worker_functions {
            self.validate_worker(*function, functions, &mut visiting, &mut visited)?;
        }
        Ok(())
    }

    fn validate_worker(
        &self,
        function: FuncIdx,
        functions: &[FunctionProto],
        visiting: &mut BTreeSet<FuncIdx>,
        visited: &mut BTreeSet<FuncIdx>,
    ) -> Result<(), String> {
        if visited.contains(&function) {
            return Ok(());
        }
        if !self.worker_functions.contains(&function) || !visiting.insert(function) {
            return Err("canonical Actor worker helper escapes the finite call DAG".into());
        }
        let proto = functions
            .get(function as usize)
            .ok_or("canonical Actor worker function is absent")?;
        if proto.code.is_empty()
            || proto.has_requires
            || proto.has_ensures
            || proto.is_async
            || proto.has_mut_params
            || !proto.capture_names.is_empty()
        {
            return Err("canonical Actor worker has an unmodeled execution effect".into());
        }
        for op in &proto.code {
            match op {
                Op::LoadConst { idx, .. } => {
                    if !matches!(
                        proto.constants.get(*idx as usize),
                        Some(
                            ConstValue::Int(_)
                                | ConstValue::Bool(_)
                                | ConstValue::Unit
                                | ConstValue::Str(_)
                        )
                    ) {
                        return Err(
                            "canonical Actor worker constant is outside the scalar envelope".into(),
                        );
                    }
                }
                Op::LoadUnit { .. }
                | Op::LoadTrue { .. }
                | Op::LoadFalse { .. }
                | Op::Mov { .. }
                | Op::Move { .. }
                | Op::AddInt { .. }
                | Op::SubInt { .. }
                | Op::MulInt { .. }
                | Op::DivInt { .. }
                | Op::ModInt { .. }
                | Op::PowInt { .. }
                | Op::NegInt { .. }
                | Op::EqInt { .. }
                | Op::NeInt { .. }
                | Op::LtInt { .. }
                | Op::GtInt { .. }
                | Op::LeInt { .. }
                | Op::GeInt { .. }
                | Op::BitAnd { .. }
                | Op::BitOr { .. }
                | Op::BitXor { .. }
                | Op::Shl { .. }
                | Op::Shr { .. }
                | Op::BitNot { .. }
                | Op::CheckI32 { .. }
                | Op::CheckI32DivRem { .. }
                | Op::WrapI32 { .. }
                | Op::MaskShiftAmt { .. }
                | Op::Not { .. }
                | Op::And { .. }
                | Op::Or { .. }
                | Op::Jmp { .. }
                | Op::JmpIf { .. }
                | Op::JmpIfNot { .. }
                | Op::Ret { .. }
                | Op::RetUnit
                | Op::Trap { .. }
                | Op::Nop
                | Op::MirActorFieldLoad { .. }
                | Op::MirActorFieldStore { .. } => {}
                Op::CallMove { func, .. } => {
                    self.validate_worker(*func, functions, visiting, visited)?;
                }
                _ => {
                    return Err(format!(
                        "canonical Actor worker contains unmodeled opcode {op:?}"
                    ));
                }
            }
        }
        validate_finite_control_flow(proto)?;
        visiting.remove(&function);
        visited.insert(function);
        Ok(())
    }
}

fn validate_finite_control_flow(proto: &FunctionProto) -> Result<(), String> {
    let mut colors = vec![0u8; proto.code.len()];
    let mut pending = vec![(0usize, false)];
    while let Some((pc, exiting)) = pending.pop() {
        if pc >= colors.len() {
            return Err("Actor worker control flow falls outside its function".into());
        }
        if exiting {
            colors[pc] = 2;
            continue;
        }
        if colors[pc] == 2 {
            continue;
        }
        if colors[pc] == 1 {
            return Err("Actor worker control flow is cyclic".into());
        }
        colors[pc] = 1;
        pending.push((pc, true));
        match proto.code[pc] {
            Op::Ret { .. } | Op::RetUnit | Op::Trap { .. } => {}
            Op::Jmp { offset } | Op::JmpIf { offset, .. } | Op::JmpIfNot { offset, .. } => {
                // VM advances pc before adding the relative offset.
                let target = (pc as i64 + 1)
                    .checked_add(i64::from(offset))
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or("Actor worker jump target overflows")?;
                if !matches!(proto.code[pc], Op::Jmp { .. }) {
                    pending.push((pc + 1, false));
                }
                pending.push((target, false));
            }
            _ => pending.push((pc + 1, false)),
        }
    }
    Ok(())
}

/// Copyable language handle, carrying Actor identity and a weak domain
/// capability. Its private fields cannot manufacture root or state authority.
#[derive(Clone)]
pub struct CanonicalActorHandleValue {
    actor: NodeId,
    handle: ActorDomainHandle,
}

impl fmt::Debug for CanonicalActorHandleValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CanonicalActorHandle")
            .field("actor", &self.actor)
            .field("identity", &self.handle.identity())
            .finish()
    }
}

impl PartialEq for CanonicalActorHandleValue {
    fn eq(&self, other: &Self) -> bool {
        self.actor == other.actor && self.handle.identity() == other.handle.identity()
    }
}

/// A turn-local borrowed receiver token. It is disjoint from the handle and
/// compared by unforgeable Arc identity, never by a caller-provided integer.
#[derive(Clone, Debug)]
pub struct CanonicalActorStateValue {
    actor: NodeId,
    token: Arc<()>,
}

impl PartialEq for CanonicalActorStateValue {
    fn eq(&self, other: &Self) -> bool {
        self.actor == other.actor && Arc::ptr_eq(&self.token, &other.token)
    }
}

struct WorkerState {
    receiver: CanonicalActorStateValue,
    fields: Vec<i32>,
    flow_payload: Option<i32>,
}

#[derive(Default)]
pub(crate) struct CanonicalActorRuntime {
    participant: Option<ActorDomainParticipant>,
    worker: Option<WorkerState>,
    last_monitor: Option<ActorDomainMonitor>,
}

/// This guard is deliberately local to a public execution entry. Storing it
/// in BytecodeVM would make every VM !Send, including compatibility children.
pub(crate) struct ActorExecutionDomain {
    owner: RootDomainOwner,
    monitor: ActorDomainMonitor,
}

impl CanonicalActorRuntime {
    pub(crate) fn is_worker(&self) -> bool {
        self.worker.is_some()
    }

    /// Public embedding entries cannot invoke an Actor method outside its
    /// mailbox merely because a method ignores self on one branch.
    pub(crate) fn validate_function_entry(
        &self,
        program: &BytecodeProgram,
        function: FuncIdx,
        arguments: &[Value],
    ) -> Result<(), InterpError> {
        let Some(manifest) = &program.canonical_actors else {
            return Ok(());
        };
        let Some(actor) = manifest
            .actors
            .iter()
            .find(|actor| actor.functions.contains(&function))
        else {
            return Ok(());
        };
        let Some(worker) = &self.worker else {
            return Err(InterpError::new(
                "direct canonical Actor method entry requires its mailbox worker",
            ));
        };
        let method_index = actor
            .functions
            .iter()
            .position(|entry| *entry == function)
            .ok_or_else(|| InterpError::new("canonical Actor method index is absent"))?;
        let method = actor
            .contract
            .methods
            .get(method_index)
            .ok_or_else(|| InterpError::new("canonical Actor method contract is absent"))?;
        if arguments.len() != method.execution_parameter_types.len() {
            return Err(InterpError::new(
                "canonical Actor method entry has an incompatible arity",
            ));
        }
        if method.flow_transition.is_some() {
            let Some(Value::Int(value)) = arguments.first() else {
                return Err(InterpError::new(
                    "canonical Flow Actor method entry has no scalar root-state receiver",
                ));
            };
            let Some(expected) = worker.flow_payload else {
                return Err(InterpError::new(
                    "canonical Flow Actor worker has no root-state snapshot",
                ));
            };
            if i32::try_from(*value).ok() != Some(expected) {
                return Err(InterpError::new(
                    "canonical Flow Actor receiver differs from its committed root state",
                ));
            }
        } else if !matches!(arguments.first(), Some(Value::CanonicalActorState(receiver))
            if receiver == &worker.receiver && receiver.actor == actor.contract.actor)
        {
            return Err(InterpError::new(
                "canonical Actor method entry has a foreign state receiver",
            ));
        }
        Ok(())
    }

    pub(crate) fn begin_execution(
        &mut self,
        program: &BytecodeProgram,
        quota: Option<usize>,
    ) -> Result<Option<ActorExecutionDomain>, InterpError> {
        if program.canonical_actors.is_none() || self.worker.is_some() {
            return Ok(None);
        }
        if self.participant.is_some() {
            return Err(InterpError::new(
                "canonical Actor execution root is already active",
            ));
        }
        let owner = RootDomainOwner::new(quota).map_err(domain_error)?;
        let monitor = owner.monitor();
        self.participant = Some(owner.participant());
        self.last_monitor = Some(monitor.clone());
        Ok(Some(ActorExecutionDomain { owner, monitor }))
    }

    pub(crate) fn finish_execution<T>(
        &mut self,
        guard: Option<ActorExecutionDomain>,
        result: Result<T, InterpError>,
    ) -> Result<T, InterpError> {
        let Some(mut guard) = guard else {
            return result;
        };
        let mode = if result.is_ok() {
            ActorDomainCloseMode::Drain
        } else {
            ActorDomainCloseMode::Cancel
        };
        let close = guard.owner.close(mode);
        self.participant = None;
        // Owner destruction is part of the boundary, before errors become
        // visible. The monitor owns counters alone, no executable resources.
        self.last_monitor = Some(guard.monitor.clone());
        drop(guard);
        match result {
            Err(error) => Err(error),
            Ok(value) => close.map(|_| value).map_err(domain_error),
        }
    }

    pub(crate) fn counts(&self) -> Option<ActorDomainCounts> {
        self.last_monitor.as_ref().map(ActorDomainMonitor::counts)
    }

    pub(crate) fn spawn(
        &self,
        program: Arc<BytecodeProgram>,
        descriptor: u32,
    ) -> Result<Value, InterpError> {
        if self.worker.is_some() {
            return Err(InterpError::new("nested canonical Actor spawn is disabled"));
        }
        let manifest = program
            .canonical_actors
            .as_ref()
            .ok_or_else(|| InterpError::new("canonical Actor spawn has no manifest"))?;
        let dispatch = manifest.descriptor(descriptor)?.clone();
        let initial_fields: Vec<i32> = dispatch
            .contract
            .fields
            .iter()
            .map(|field| field.initial_value)
            .collect();
        let arities = dispatch
            .contract
            .methods
            .iter()
            .map(|method| method.execution_parameter_types.len() - 1)
            .collect();
        let actor_identity = dispatch.contract.actor.clone();
        let mut worker_fields = vec![0; initial_fields.len()];
        let handle = self
            .participant
            .as_ref()
            .ok_or_else(|| InterpError::new("canonical Actor spawn has no active root domain"))?
            .spawn_actor(
                initial_fields,
                arities,
                move |fields, method, arguments, context| {
                    execute_turn(
                        Arc::clone(&program),
                        &dispatch,
                        fields,
                        &mut worker_fields,
                        method,
                        arguments,
                        context,
                    )
                },
            )
            .map_err(domain_error)?;
        Ok(Value::CanonicalActorHandle(CanonicalActorHandleValue {
            actor: actor_identity,
            handle,
        }))
    }

    pub(crate) fn call(
        &self,
        program: &BytecodeProgram,
        descriptor: u32,
        method: u32,
        handle: &Value,
        arguments: &[Value],
    ) -> Result<Value, InterpError> {
        if self.worker.is_some() {
            return Err(InterpError::new(
                "canonical worker cross-Actor call is disabled",
            ));
        }
        let manifest = program
            .canonical_actors
            .as_ref()
            .ok_or_else(|| InterpError::new("canonical Actor call has no manifest"))?;
        let dispatch = manifest.descriptor(descriptor)?;
        let Value::CanonicalActorHandle(handle) = handle else {
            return Err(InterpError::new(
                "canonical mailbox call requires an Actor handle, not state",
            ));
        };
        if handle.actor != dispatch.contract.actor {
            return Err(InterpError::new(
                "canonical mailbox handle has a foreign Actor identity",
            ));
        }
        let contract = dispatch
            .contract
            .methods
            .get(method as usize)
            .ok_or_else(|| InterpError::new("canonical mailbox method index is out of range"))?;
        if arguments.len() + 1 != contract.execution_parameter_types.len() {
            return Err(InterpError::new(
                "canonical mailbox payload arity disagrees",
            ));
        }
        let arguments = arguments
            .iter()
            .map(scalar_i32)
            .collect::<Result<Vec<_>, _>>()?;
        let result = self
            .participant
            .as_ref()
            .ok_or_else(|| InterpError::new("canonical Actor call has no active root domain"))?
            .call(&handle.handle, method, &arguments)
            .map_err(domain_error)?;
        if let Some(flow) = &contract.flow_transition {
            let state = dispatch.contract.flow.as_ref().ok_or_else(|| {
                InterpError::new("Flow Actor dispatch has no checked state receipt")
            })?;
            let [payload] = state.payload.as_slice() else {
                return Err(InterpError::new(
                    "Flow Actor dispatch has no exact single-field state payload",
                ));
            };
            if flow.method_owner != contract.owner
                || flow.body_owner != contract.body_owner
                || contract.result_type != state.root_state_type
                || payload.ty != crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32)
            {
                return Err(InterpError::new(
                    "Flow Actor result differs from its checked state receipt",
                ));
            }
            Ok(Value::Record(
                Some(state.root_state.0.clone()),
                std::collections::HashMap::from([(
                    payload.name.clone(),
                    Value::Int(i64::from(result)),
                )]),
            ))
        } else {
            Ok(Value::Int(i64::from(result)))
        }
    }

    pub(crate) fn field_load(
        &self,
        program: &BytecodeProgram,
        descriptor: u32,
        field: u32,
        state: &Value,
    ) -> Result<Value, InterpError> {
        let worker = self.validate_state(program, descriptor, field, state)?;
        Ok(Value::Int(i64::from(worker.fields[field as usize])))
    }

    pub(crate) fn field_store(
        &mut self,
        program: &BytecodeProgram,
        descriptor: u32,
        field: u32,
        state: &Value,
        value: &Value,
    ) -> Result<(), InterpError> {
        let value = scalar_i32(value)?;
        self.validate_state(program, descriptor, field, state)?;
        self.worker
            .as_mut()
            .expect("validated canonical Actor state")
            .fields[field as usize] = value;
        Ok(())
    }

    fn validate_state(
        &self,
        program: &BytecodeProgram,
        descriptor: u32,
        field: u32,
        state: &Value,
    ) -> Result<&WorkerState, InterpError> {
        let worker = self.worker.as_ref().ok_or_else(|| {
            InterpError::new("canonical Actor field access is outside a worker turn")
        })?;
        let Value::CanonicalActorState(receiver) = state else {
            return Err(InterpError::new(
                "canonical Actor field access requires state, not a handle",
            ));
        };
        let actor = program
            .canonical_actors
            .as_ref()
            .ok_or_else(|| InterpError::new("canonical Actor field access has no manifest"))?
            .descriptor(descriptor)?;
        actor
            .contract
            .fields
            .get(field as usize)
            .ok_or_else(|| InterpError::new("canonical Actor field index is out of range"))?;
        if receiver != &worker.receiver
            || receiver.actor != actor.contract.actor
            || worker.fields.len() != actor.contract.fields.len()
        {
            return Err(InterpError::new(
                "canonical Actor field state identity or layout disagrees",
            ));
        }
        Ok(worker)
    }
}

fn execute_turn(
    program: Arc<BytecodeProgram>,
    dispatch: &ActorDispatch,
    fields: &mut [i32],
    reusable_worker_fields: &mut Vec<i32>,
    method: u32,
    arguments: &[i32],
    context: &ActorTurnContext,
) -> Result<i32, ActorDomainError> {
    if context.is_cancelled() {
        return Err(ActorDomainError::Cancelled);
    }
    let function = dispatch
        .functions
        .get(method as usize)
        .copied()
        .ok_or(ActorDomainError::InvalidPayload)?;
    let contract = dispatch
        .contract
        .methods
        .get(method as usize)
        .ok_or(ActorDomainError::InvalidPayload)?;
    if arguments.len() + 1 != contract.execution_parameter_types.len()
        || fields.len() != dispatch.contract.fields.len()
        || reusable_worker_fields.len() != fields.len()
    {
        return Err(ActorDomainError::InvalidPayload);
    }
    let receiver = CanonicalActorStateValue {
        actor: dispatch.contract.actor.clone(),
        token: Arc::new(()),
    };
    reusable_worker_fields.copy_from_slice(fields);
    let mut vm = BytecodeVM::new(program);
    let flow_payload = if contract.flow_transition.is_some() {
        if dispatch.contract.flow.as_ref().is_none_or(|flow| {
            flow.payload.len() != 1
                || dispatch.contract.fields.len() != 1
                || dispatch.contract.fields[0].field != flow.payload[0].field
                || dispatch.contract.fields[0].ty != flow.payload[0].ty
                || fields.is_empty()
        }) {
            *reusable_worker_fields = fields.to_vec();
            return Err(ActorDomainError::InvalidPayload);
        }
        fields.first().copied()
    } else {
        None
    };
    vm.canonical_actor_runtime.worker = Some(WorkerState {
        receiver: receiver.clone(),
        fields: std::mem::take(reusable_worker_fields),
        flow_payload,
    });
    let mut parameters = Vec::with_capacity(arguments.len() + 1);
    if let Some(value) = flow_payload {
        parameters.push(Value::Int(i64::from(value)));
    } else {
        parameters.push(Value::CanonicalActorState(receiver));
    }
    parameters.extend(arguments.iter().map(|value| Value::Int(i64::from(*value))));
    let result = vm.call_function(function, &parameters);
    let worker = vm
        .canonical_actor_runtime
        .worker
        .take()
        .ok_or(ActorDomainError::InvalidPayload)?;
    let reply = match result {
        Ok(Value::Int(value)) => i32::try_from(value).map_err(|_| ActorDomainError::InvalidPayload),
        Ok(_) => Err(ActorDomainError::InvalidPayload),
        Err(error) => Err(ActorDomainError::Trap(match error.code() {
            "E0801" => 801,
            "E0802" => 802,
            _ => 800,
        })),
    };
    if let Ok(value) = reply {
        if contract.flow_transition.is_some() {
            if let Some(slot) = fields.first_mut() {
                *slot = value;
            } else {
                *reusable_worker_fields = worker.fields;
                return Err(ActorDomainError::InvalidPayload);
            }
        } else {
            fields.copy_from_slice(&worker.fields);
        }
        *reusable_worker_fields = worker.fields;
        Ok(value)
    } else {
        *reusable_worker_fields = worker.fields;
        reply
    }
}

fn scalar_i32(value: &Value) -> Result<i32, InterpError> {
    let Value::Int(value) = value else {
        return Err(InterpError::new("canonical Actor payload must be i32"));
    };
    i32::try_from(*value).map_err(|_| InterpError::new("canonical Actor payload overflows i32"))
}

fn domain_error(error: ActorDomainError) -> InterpError {
    match error {
        ActorDomainError::Trap(801) => InterpError::div_by_zero(),
        ActorDomainError::Trap(802) => {
            InterpError::integer_overflow("canonical Actor worker integer operation")
        }
        ActorDomainError::Trap(_) => InterpError::new("canonical Actor worker trapped"),
        other => InterpError::new(format!("canonical Actor domain: {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile(source: &str) -> Arc<BytecodeProgram> {
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("Actor lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("Actor parse");
        let checked = crate::core::check_program(&file).expect("Actor check");
        let mir = MirProgram::from_checked_program(&checked).expect("Actor typed MIR");
        super::super::compile_mir_program(&mir).expect("AST-free Actor bytecode")
    }

    fn assert_closed(counts: ActorDomainCounts, actors: u64) {
        assert_eq!(counts.live_roots, 0, "root must be destroyed before return");
        assert_eq!(counts.live_workers, 0);
        assert_eq!(counts.live_states, 0);
        assert_eq!(counts.live_controls, 0);
        assert_eq!(counts.live_tasks, 0);
        assert_eq!(counts.pending_replies, 0);
        assert_eq!(counts.live_message_buffers, 0);
        assert_eq!(counts.live_reply_buffers, 0);
        assert_eq!(counts.spawned_actors, actors);
        assert_eq!(counts.joined_workers, actors);
        assert_eq!(counts.accepted_messages, counts.settled_replies);
    }

    #[test]
    fn canonical_actor_vm_executes_original_capture_and_shadow_without_ast() {
        for (source, output) in [
            (
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/real_world/actor_nested_func_capture.mimi"
                )),
                "15\n",
            ),
            (
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/real_world/actor_nested_func_shadow.mimi"
                )),
                "42\n101\n",
            ),
        ] {
            let program = compile(source);
            assert!(program.ast.is_none());
            assert!(program.actor_defs.is_empty());
            assert!(program.actor_method_funcs.is_empty());
            assert!(program.canonical_actors.is_some());
            let mut vm = BytecodeVM::new(program);
            assert_eq!(vm.run().expect("canonical Actor production VM"), 0);
            assert_eq!(vm.stdout(), output);
            assert_closed(vm.canonical_actor_domain_counts().unwrap(), 1);
        }
    }

    const REPEAT: &str = r#"
        flow CounterFlow {
            state Active { base: i32 }
            transition seed(Active, value: i32) -> Active {
                return Active { base: value }
            }
            transition add(Active, amount: i32) -> Active {
                return Active { base: self.base + amount }
            }
        }
        actor Counter runs CounterFlow {
            func bump(delta: i32) -> i32 {
                let scale = 10
                func scaled(d: i32) -> i32 { scale + d }
                scaled(delta)
            }
        }
        func main() -> i32 {
            let first = Counter.spawn()
            let second = Counter.spawn()
            let first_turn = first.add(first.bump(5))
            let second_turn = first.add(first.bump(7))
            let other_turn = second.add(second.bump(1))
            println(first_turn.base)
            println(second_turn.base)
            println(other_turn.base)
            0
        }
    "#;

    #[test]
    fn canonical_actor_vm_repeat_calls_and_instances_keep_independent_state() {
        let mut vm = BytecodeVM::new(compile(REPEAT));
        assert_eq!(vm.run().unwrap(), 0);
        assert_eq!(vm.stdout(), "15\n32\n11\n");
        assert_closed(vm.canonical_actor_domain_counts().unwrap(), 2);
        // A reusable VM starts a fresh root and initializer state on reentry.
        assert_eq!(vm.run_value().unwrap(), Value::Int(0));
        assert_eq!(vm.stdout(), "15\n32\n11\n");
        assert_closed(vm.canonical_actor_domain_counts().unwrap(), 2);
    }

    #[test]
    fn canonical_actor_vm_nonzero_flow_seed_and_two_capture_snapshots() {
        let source = REPEAT
            .replace(
                "let first_turn = first.add(first.bump(5))",
                "let first_initial = first.seed(3)\n            let first_turn = first.add(first.bump(5) + first_initial.base)",
            )
            .replace(
                "let other_turn = second.add(second.bump(1))",
                "let other_initial = second.seed(3)\n            let other_turn = second.add(second.bump(1) + other_initial.base)",
            )
            .replace("let scale = 10", "let scale = 10\nlet offset = 2")
            .replace("scale + d", "scale + offset + d");
        let mut vm = BytecodeVM::new(compile(&source));
        assert_eq!(vm.run().unwrap(), 0);
        assert_eq!(vm.stdout(), "23\n42\n19\n");
        assert_closed(vm.canonical_actor_domain_counts().unwrap(), 2);
    }

    #[test]
    fn canonical_actor_vm_worker_helper_and_root_traps_close_before_error() {
        for (worker, expression, expected) in [
            (true, "100 / delta", "E0801"),
            (true, "2147483647 + delta", "E0802"),
            (false, "100 / delta", "E0801"),
            (false, "2147483647 + delta", "E0802"),
        ] {
            let source = if worker {
                format!(
                    r#"
                    actor Counter {{
                        base: i32 = 4;
                        func bump(delta: i32) -> i32 {{
                            func helper(delta: i32) -> i32 {{ {expression} }}
                            helper(delta)
                        }}
                    }}
                    func main() -> i32 {{
                        let c = Counter.spawn()
                        c.bump({})
                    }}
                "#,
                    if expected == "E0801" { 0 } else { 1 }
                )
            } else {
                format!(
                    r#"
                    actor Counter {{ func read() -> i32 {{ 4 }} }}
                    func helper(delta: i32) -> i32 {{ {expression} }}
                    func main() -> i32 {{
                        let c = Counter.spawn()
                        let before = c.read()
                        helper({})
                    }}
                "#,
                    if expected == "E0801" { 0 } else { 1 }
                )
            };
            let mut vm = BytecodeVM::new(compile(&source));
            assert_eq!(
                vm.run().expect_err("Actor arithmetic must trap").code(),
                expected
            );
            assert_closed(vm.canonical_actor_domain_counts().unwrap(), 1);
            assert!(vm.stdout().is_empty());
        }
    }

    #[test]
    fn canonical_actor_vm_rejects_instruction_and_metadata_forgeries_before_stdout() {
        let program = compile(REPEAT);
        let field_program = compile(
            r#"
            actor Reader {
                base: i32 = 4;
                func read(delta: i32) -> i32 { self.base + delta }
            }
            func main() -> i32 {
                let r = Reader.spawn()
                println(r.read(5))
                0
            }
        "#,
        );
        for corruption in 0..5 {
            let mut forged = match corruption {
                2 | 3 => (*field_program).clone(),
                _ => (*program).clone(),
            };
            match corruption {
                0 => forged.canonical_actors = None,
                1 => {
                    let op = forged
                        .functions
                        .iter_mut()
                        .flat_map(|function| &mut function.code)
                        .find(|op| matches!(op, Op::MirActorCall { .. }))
                        .unwrap();
                    let Op::MirActorCall { descriptor, .. } = op else {
                        unreachable!()
                    };
                    *descriptor = u32::MAX;
                }
                2 => {
                    let op = forged
                        .functions
                        .iter_mut()
                        .flat_map(|function| &mut function.code)
                        .find(|op| matches!(op, Op::MirActorFieldLoad { .. }))
                        .unwrap();
                    let Op::MirActorFieldLoad { field, .. } = op else {
                        unreachable!()
                    };
                    *field = u32::MAX;
                }
                3 => {
                    let op = forged
                        .functions
                        .iter_mut()
                        .flat_map(|function| &mut function.code)
                        .find(|op| matches!(op, Op::MirActorFieldLoad { .. }))
                        .unwrap();
                    let Op::MirActorFieldLoad {
                        rd,
                        state,
                        descriptor,
                        field,
                        instruction,
                    } = *op
                    else {
                        unreachable!()
                    };
                    *op = Op::MirActorFieldStore {
                        state: rd,
                        value: state,
                        descriptor,
                        field,
                        instruction,
                    };
                }
                _ => {
                    let tokens = crate::lexer::Lexer::new("func main() -> i32 { 0 }")
                        .tokenize()
                        .unwrap();
                    forged.ast = Some(Arc::new(
                        crate::parser::Parser::new(tokens).parse_file().unwrap(),
                    ));
                }
            }
            let mut vm = BytecodeVM::new(Arc::new(forged));
            assert!(vm.run().is_err());
            assert!(vm.stdout().is_empty());
            assert!(vm.canonical_actor_domain_counts().is_none());
        }
    }

    #[test]
    fn canonical_actor_vm_public_entries_cannot_bypass_mailbox_with_fake_self() {
        let program = compile(
            r#"
            actor Constant { value: i32 = 0; func answer() -> i32 { 42 } }
            func main() -> i32 { let c = Constant.spawn(); c.answer() }
        "#,
        );
        let method = program.canonical_actors.as_ref().unwrap().actors[0].functions[0];
        let name = program.functions[method as usize].name.clone();
        for api in 0..4 {
            let mut vm = BytecodeVM::new(Arc::clone(&program));
            let fake_self = [Value::Int(17)];
            let result = match api {
                0 => vm.call_function(method, &fake_self),
                1 => vm.call_named(&name, fake_self.to_vec()),
                2 => vm.call_function_wrap_ok(method, &fake_self, Value::Unit),
                _ => vm.call_closure(
                    &Value::BytecodeClosure {
                        proto: method,
                        captured: std::collections::HashMap::new(),
                        program: Arc::clone(&program),
                    },
                    &fake_self,
                ),
            };
            assert!(result.unwrap_err().message().contains("mailbox worker"));
            assert!(vm.stdout().is_empty());
            assert_closed(vm.canonical_actor_domain_counts().unwrap(), 0);
        }
    }

    #[test]
    fn canonical_actor_vm_accepted_queue_cancel_and_spawn_rollback_settle_all_resources() {
        let program = compile(REPEAT);
        let mut runtime = CanonicalActorRuntime::default();
        let guard = runtime.begin_execution(&program, Some(1)).unwrap();
        let value = runtime.spawn(Arc::clone(&program), 0).unwrap();
        assert!(
            runtime.spawn(Arc::clone(&program), 0).is_err(),
            "quota failure must roll back"
        );
        let Value::CanonicalActorHandle(handle) = value else {
            unreachable!()
        };
        let participant = runtime.participant.as_ref().unwrap();
        let pending = (0..256)
            .map(|_| participant.enqueue(&handle.handle, 0, &[1]).unwrap())
            .collect::<Vec<_>>();
        participant.request_cancel().unwrap();
        let result: Result<(), InterpError> =
            runtime.finish_execution(guard, Err(InterpError::new("test cancellation")));
        assert!(result.is_err());
        let mut cancelled = 0;
        for reply in pending {
            match reply.wait() {
                Ok(_) => (),
                Err(ActorDomainError::Cancelled) => cancelled += 1,
                other => panic!("unexpected accepted reply: {other:?}"),
            }
        }
        let counts = runtime.counts().unwrap();
        assert_eq!(counts.cancelled_replies, cancelled);
        assert_eq!(counts.accepted_messages, 256);
        assert_closed(counts, 1);
    }
}
