//! Checker-derived Actor state and dispatch contracts shared by MIR consumers.
//!
//! A language handle is a weak domain capability. The internal receiver is a
//! distinct typed, borrowed state pointer available only during one worker
//! turn. This module never reopens an AST or supplies an execution fallback.

use std::collections::{BTreeMap, BTreeSet};

use crate::core::ir::{
    Permission, PrimitiveType, ResolvedActorFlowDescriptor, ResolvedActorFlowTransitionDescriptor,
    ResolvedActorInitializerKind, ResolvedCall, ResolvedCallee, ResolvedLiteral, ResolvedLocalId,
    ResolvedScalarActorReceipt, ResolvedTypeId,
};
use crate::core::{CheckedProgram, EffectId, NodeId};

use super::types::{
    MirAbiClass, MirFieldDesc, MirGlueContract, MirGlueKind, MirLayout, MirOwnership,
    MirTypeCatalog, MirTypeDesc, MirTypeKind,
};
use super::{MirFunction, MirInstructionId, MirInstructionKind, MirValueId};

pub const MIR_SCALAR_ACTOR_SCHEMA: &str = "mimi-mir-scalar-actor-v1";

/// Explicit mailbox/state operations. A state pointer cannot be passed in the
/// handle position merely because a native representation happens to fit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirActorOperation {
    Spawn {
        result: MirValueId,
        actor: NodeId,
    },
    Call {
        result: MirValueId,
        actor: NodeId,
        method: NodeId,
        handle: MirValueId,
        args: Vec<MirValueId>,
    },
    FieldLoad {
        result: MirValueId,
        actor: NodeId,
        field: NodeId,
        state: MirValueId,
    },
    FieldStore {
        actor: NodeId,
        field: NodeId,
        state: MirValueId,
        value: MirValueId,
    },
}

impl MirActorOperation {
    pub fn result(&self) -> Option<&MirValueId> {
        match self {
            Self::Spawn { result, .. }
            | Self::Call { result, .. }
            | Self::FieldLoad { result, .. } => Some(result),
            Self::FieldStore { .. } => None,
        }
    }
    pub fn uses(&self) -> Vec<&MirValueId> {
        match self {
            Self::Spawn { .. } => vec![],
            Self::Call { handle, args, .. } => std::iter::once(handle).chain(args.iter()).collect(),
            Self::FieldLoad { state, .. } => vec![state],
            Self::FieldStore { state, value, .. } => vec![state, value],
        }
    }
    pub fn actor(&self) -> &NodeId {
        match self {
            Self::Spawn { actor, .. }
            | Self::Call { actor, .. }
            | Self::FieldLoad { actor, .. }
            | Self::FieldStore { actor, .. } => actor,
        }
    }
    pub(crate) fn canonical_text(&self) -> String {
        format!("actor.{self:?}")
    }
}

/// Exact lowering witnesses are produced while the checked typed expression
/// is visited. A later coordinated edit to a field/handle/dispatch instruction
/// and its public catalog is rejected before an executable is produced.
#[derive(Debug, Clone, Default)]
pub(crate) struct MirActorOperationReceipts {
    instructions: BTreeMap<MirInstructionId, (NodeId, MirActorOperation)>,
}

impl MirActorOperationReceipts {
    pub(crate) fn record(
        &mut self,
        owner: &NodeId,
        id: &MirInstructionId,
        operation: &MirActorOperation,
    ) -> Result<(), String> {
        if self
            .instructions
            .insert(id.clone(), (owner.clone(), operation.clone()))
            .is_some()
        {
            Err(format!(
                "Actor operation '{}' has duplicate checked lowering identity",
                id.as_str()
            ))
        } else {
            Ok(())
        }
    }
    pub(crate) fn validate(
        &self,
        functions: &BTreeMap<NodeId, MirFunction>,
    ) -> Result<(), Vec<String>> {
        let mut observed = BTreeMap::new();
        let mut errors = Vec::new();
        for (owner, function) in functions {
            for instruction in function
                .blocks
                .values()
                .flat_map(|block| &block.instructions)
            {
                if let MirInstructionKind::Actor { operation } = &instruction.kind {
                    if observed
                        .insert(instruction.id.clone(), (owner.clone(), operation.clone()))
                        .is_some()
                    {
                        errors.push(format!(
                            "Actor instruction '{}' is duplicated",
                            instruction.id.as_str()
                        ));
                    }
                }
            }
        }
        if observed != self.instructions {
            errors
                .push("Actor operations differ from checker-owned typed lowering receipts".into());
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
    pub(crate) fn canonical_text(&self) -> String {
        format!("mir.actor-operation-receipts {:?}\n", self.instructions)
    }
}

/// This identity is not a source-level Actor handle or source raw pointer.
/// Only a checker-backed Actor contract can authorize its catalog descriptor.
pub(crate) fn actor_state_type_id(actor: &NodeId) -> ResolvedTypeId {
    ResolvedTypeId::synthetic(&format!("mimi-mir:ActorState:v1:{}", actor.0))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirActorFieldContract {
    pub field: NodeId,
    pub initializer: NodeId,
    pub name: String,
    pub index: usize,
    pub ty: ResolvedTypeId,
    pub mutable: bool,
    pub initial_value: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirActorMethodContract {
    /// Source mailbox identity used by `MirActorOperation::Call`.
    pub owner: NodeId,
    /// Checker body identity. For a `runs Flow` event this is the transition
    /// body, while `owner` remains the synthetic Actor dispatch method.
    pub body_owner: NodeId,
    pub index: usize,
    pub self_local: ResolvedLocalId,
    pub self_value: MirValueId,
    /// Exact checker signature, including the source handle receiver.
    pub source_parameter_types: Vec<ResolvedTypeId>,
    /// Exact checker self borrow. It authorizes the mailbox turn only; the
    /// private internal state receiver has a separate, non-source ABI.
    pub source_receiver_permission: Option<Permission>,
    /// Only parameter zero changes to the internal state TypeDesc.
    pub execution_parameter_types: Vec<ResolvedTypeId>,
    pub result_type: ResolvedTypeId,
    pub execution_result_type: ResolvedTypeId,
    pub flow_transition: Option<ResolvedActorFlowTransitionDescriptor>,
    /// Exact checker effect summary. Nested helper bodies may contribute the
    /// conservative `unknown` effect; it is still part of the source receipt
    /// and cannot be substituted by a caller-provided effect.
    pub source_effects: Vec<EffectId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CheckedActorSnapshot {
    declaration: ResolvedScalarActorReceipt,
    method_receivers: BTreeMap<NodeId, ResolvedLocalId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirActorContract {
    pub schema: &'static str,
    pub actor: NodeId,
    pub handle_type: ResolvedTypeId,
    pub state_type: ResolvedTypeId,
    pub flow: Option<ResolvedActorFlowDescriptor>,
    pub fields: Vec<MirActorFieldContract>,
    pub methods: Vec<MirActorMethodContract>,
    checker_snapshot: CheckedActorSnapshot,
}

#[derive(Debug, Clone, Default)]
pub struct MirActorCatalog {
    contracts: BTreeMap<NodeId, MirActorContract>,
}

impl MirActorCatalog {
    /// Callers supply the selected checker actor identities, rather than
    /// treating every declaration in an imported compatibility source as an
    /// execution candidate. Unsupported selected declarations report a bound.
    pub(crate) fn from_checked_program(
        program: &CheckedProgram,
        selected: &BTreeSet<NodeId>,
    ) -> Result<Self, Vec<String>> {
        let mut contracts = BTreeMap::new();
        let mut errors = Vec::new();
        for actor in selected {
            match checked_actor_contract(program, actor) {
                Ok(contract) => {
                    contracts.insert(actor.clone(), contract);
                }
                Err(message) => errors.push(message),
            }
        }
        if errors.is_empty() {
            Ok(Self { contracts })
        } else {
            Err(errors)
        }
    }

    pub fn get(&self, actor: &NodeId) -> Option<&MirActorContract> {
        self.contracts.get(actor)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&NodeId, &MirActorContract)> {
        self.contracts.iter()
    }

    pub fn method(&self, owner: &NodeId) -> Option<(&MirActorContract, &MirActorMethodContract)> {
        self.contracts.values().find_map(|actor| {
            actor
                .methods
                .iter()
                .find(|method| &method.owner == owner || &method.body_owner == owner)
                .map(|method| (actor, method))
        })
    }

    /// Validate source mailbox metadata before it becomes a dedicated Actor
    /// operation. General calls continue to forbid effects, sessions and
    /// borrows; only the exact checker-owned self permission is translated.
    pub(crate) fn validate_source_mailbox_call(&self, call: &ResolvedCall) -> Result<(), String> {
        let ResolvedCallee::ActorMethod { actor, method } = &call.callee else {
            return Err("source mailbox receipt requires an Actor method".into());
        };
        let contract = self
            .get(actor)
            .ok_or("source mailbox Actor owner is absent")?;
        contract.validate_snapshot()?;
        let entry = contract
            .methods
            .iter()
            .find(|entry| entry.owner.0 == method.as_str())
            .ok_or("source mailbox method owner is foreign to the Actor")?;
        if call.permission != entry.source_receiver_permission
            || call.effects != entry.source_effects
            || !call.session.is_empty()
            || !call.type_arguments.is_empty()
            || call.result != entry.result_type
            || call.arguments.len() != entry.source_parameter_types.len()
            || call
                .arguments
                .iter()
                .zip(&entry.source_parameter_types)
                .any(|(argument, ty)| {
                    argument.value.ty != *ty
                        || argument.conversion.kind
                            != crate::core::ir::CheckedConversionKind::Identity
                        || argument.conversion.from != *ty
                        || argument.conversion.to != *ty
                })
        {
            return Err("source mailbox permission/effect/session/payload differs from its checked method receipt".into());
        }
        Ok(())
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.contracts.is_empty()
    }

    pub fn validate_functions(
        &self,
        types: &MirTypeCatalog,
        functions: &BTreeMap<NodeId, MirFunction>,
    ) -> Result<(), Vec<String>> {
        self.validate(types)?;
        let mut errors = Vec::new();
        for (owner, function) in functions {
            let type_of = |value: &MirValueId| function.values.get(value).map(|entry| &entry.ty);
            let method = self.method(owner);
            for instruction in function
                .blocks
                .values()
                .flat_map(|block| &block.instructions)
            {
                let MirInstructionKind::Actor { operation } = &instruction.kind else {
                    continue;
                };
                let Some(actor) = self.get(operation.actor()) else {
                    errors.push(format!(
                        "Actor instruction '{}' has an unknown Actor owner",
                        instruction.id.as_str()
                    ));
                    continue;
                };
                let good = match operation {
                    MirActorOperation::Spawn { result, .. } => {
                        method.is_none() && type_of(result) == Some(&actor.handle_type)
                    }
                    MirActorOperation::Call {
                        result,
                        method: callee,
                        handle,
                        args,
                        ..
                    } => {
                        method.is_none()
                            && actor
                                .methods
                                .iter()
                                .find(|entry| &entry.owner == callee)
                                .is_some_and(|entry| {
                                    type_of(handle) == Some(&actor.handle_type)
                                        && type_of(result) == Some(&entry.result_type)
                                        && args.len().checked_add(1)
                                            == Some(entry.source_parameter_types.len())
                                        && args
                                            .iter()
                                            .zip(entry.source_parameter_types.iter().skip(1))
                                            .all(|(value, ty)| type_of(value) == Some(ty))
                                })
                    }
                    MirActorOperation::FieldLoad {
                        result,
                        field,
                        state,
                        ..
                    } => {
                        method.is_some_and(|(owned, entry)| {
                            owned.actor == actor.actor
                                && state == &entry.self_value
                                && type_of(state) == Some(&actor.state_type)
                        }) && actor.fields.iter().any(|entry| {
                            &entry.field == field && type_of(result) == Some(&entry.ty)
                        })
                    }
                    MirActorOperation::FieldStore {
                        field,
                        state,
                        value,
                        ..
                    } => {
                        method.is_some_and(|(owned, entry)| {
                            owned.actor == actor.actor
                                && state == &entry.self_value
                                && type_of(state) == Some(&actor.state_type)
                        }) && actor
                            .fields
                            .iter()
                            .any(|entry| &entry.field == field && type_of(value) == Some(&entry.ty))
                    }
                };
                if !good {
                    errors.push(format!(
                        "Actor instruction '{}' has a forged handle/state/field/mailbox ABI",
                        instruction.id.as_str()
                    ));
                }
            }
            for (identity, value) in &function.values {
                if self
                    .contracts
                    .values()
                    .any(|actor| value.ty == actor.state_type)
                    && !method.is_some_and(|(actor, entry)| {
                        identity == &entry.self_value && value.ty == actor.state_type
                    })
                {
                    errors.push(format!(
                        "Actor internal state '{}' escaped its exact method receiver role",
                        identity.as_str()
                    ));
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Public descriptors may be cloned and inspected. Every executable
    /// consumer must validate them against the private checker snapshot and
    /// the exact materialized TypeDesc catalog before accepting operations.
    pub fn validate(&self, types: &MirTypeCatalog) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        let mut methods = BTreeSet::new();
        let mut states = BTreeSet::new();
        for (ty, descriptor) in types.iter() {
            let actor = match &descriptor.kind {
                MirTypeKind::ActorHandle { actor } | MirTypeKind::ActorState { actor } => {
                    Some(actor)
                }
                _ => match &descriptor.layout {
                    MirLayout::ActorState { actor, .. } => Some(actor),
                    _ => None,
                },
            };
            if let Some(actor) = actor {
                if self.get(actor).is_none_or(|contract| {
                    ty != &contract.handle_type && ty != &contract.state_type
                        || descriptor != &contract.handle_descriptor()
                            && descriptor != &contract.state_descriptor()
                }) {
                    errors.push(format!(
                        "Actor TypeDesc '{}' has no exact checked catalog owner",
                        ty.as_str()
                    ));
                }
            }
        }
        for (key, contract) in &self.contracts {
            if key != &contract.actor || !states.insert(contract.state_type.clone()) {
                errors.push(format!(
                    "Actor '{}' catalog key/state identity is inconsistent",
                    key.0
                ));
            }
            if let Err(message) = contract.validate_snapshot() {
                errors.push(message);
                continue;
            }
            for method in &contract.methods {
                if !methods.insert(method.owner.clone()) {
                    errors.push(format!(
                        "Actor method '{}' has multiple owners",
                        method.owner.0
                    ));
                }
                if method.body_owner == method.owner && method.flow_transition.is_some()
                    || method.flow_transition.as_ref().is_some_and(|transition| {
                        transition.method_owner != method.owner
                            || transition.body_owner != method.body_owner
                    })
                {
                    errors.push(format!(
                        "Actor method '{}' source/body transition identities disagree",
                        method.owner.0
                    ));
                }
            }
            if types.get(&contract.handle_type) != Some(&contract.handle_descriptor())
                || types.get(&contract.state_type) != Some(&contract.state_descriptor())
            {
                errors.push(format!(
                    "Actor '{}' handle/state TypeDesc disagrees with its checker contract",
                    key.0
                ));
            }
            if let Some(flow) = &contract.flow {
                let root_nominal = crate::core::NominalTypeId::new(flow.root_state.0.clone()).ok();
                let root_state_ok = types.get(&flow.root_state_type).is_some_and(|descriptor| {
                    let MirLayout::Record { nominal, fields } = &descriptor.layout else {
                        return false;
                    };
                    matches!(&descriptor.kind, MirTypeKind::Nominal)
                        && root_nominal.as_ref() == Some(nominal)
                        && fields.len() == 1
                        && flow.payload.as_slice().first().is_some_and(|payload| {
                            fields[0].id == payload.field
                                && fields[0].name == payload.name
                                && fields[0].ty == payload.ty
                        })
                });
                let payload_ok = flow.payload.as_slice().first().is_some_and(|payload| {
                    flow.payload.len() == 1
                        && contract.fields.len() == 1
                        && contract.fields[0].index == 0
                        && contract.fields[0].field == payload.field
                        && contract.fields[0].name == payload.name
                        && contract.fields[0].ty == payload.ty
                        && types.get(&payload.ty).is_some_and(|descriptor| {
                            matches!(&descriptor.kind, MirTypeKind::Primitive(PrimitiveType::I32))
                                && descriptor.layout == MirLayout::Scalar
                                && descriptor.abi
                                    == (MirAbiClass::Integer {
                                        bits: 32,
                                        signed: true,
                                    })
                                && descriptor.has_canonical_copy_noop_metadata()
                        })
                });
                if !root_state_ok || !payload_ok {
                    errors.push(format!(
                        "Actor '{}' Flow root-state TypeDesc/field shape differs from its checked receipt",
                        key.0
                    ));
                }
            }
            for ty in contract.fields.iter().map(|field| &field.ty).chain(
                contract.methods.iter().flat_map(|method| {
                    method
                        .source_parameter_types
                        .iter()
                        .skip(1)
                        .chain(std::iter::once(&method.result_type))
                }),
            ) {
                let flow_state_result = contract
                    .flow
                    .as_ref()
                    .is_some_and(|flow| ty == &flow.root_state_type);
                if flow_state_result {
                    continue;
                }
                if let Err(message) = types.validate_copy_scalar(ty) {
                    errors.push(message);
                }
                if !types.get(ty).is_some_and(|desc| {
                    desc.abi
                        == (MirAbiClass::Integer {
                            bits: 32,
                            signed: true,
                        })
                }) {
                    errors.push(format!(
                        "Actor '{}' payload '{}' is outside i32",
                        key.0,
                        ty.as_str()
                    ));
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    pub(crate) fn canonical_text(&self) -> String {
        let mut text = format!("mir.actor-catalog {}\n", MIR_SCALAR_ACTOR_SCHEMA);
        for contract in self.contracts.values() {
            text.push_str(&contract.canonical_text());
        }
        text
    }
}

fn checked_actor_contract(
    program: &CheckedProgram,
    actor: &NodeId,
) -> Result<MirActorContract, String> {
    let descriptor = program.actor_descriptors().get(actor).ok_or_else(|| {
        format!(
            "Actor '{}' is absent from the checker descriptor catalog",
            actor.0
        )
    })?;
    let declaration = descriptor
        .scalar_i32_receipt(program)
        .map_err(|error| error.to_string())?;
    let mut method_receivers = BTreeMap::new();
    for method in declaration.dispatch_methods() {
        let flow_transition = descriptor.flow.as_ref().and_then(|flow| {
            flow.transitions
                .iter()
                .find(|transition| transition.method_owner == method.owner)
        });
        let body_owner = flow_transition
            .map(|transition| &transition.body_owner)
            .unwrap_or(&method.owner);
        let expected_signature = flow_transition
            .map(|transition| &transition.body_signature)
            .unwrap_or(&method.signature);
        let callable = program
            .callable(body_owner)
            .ok_or_else(|| format!("Actor body '{}' has no checker body", body_owner.0))?;
        if callable.body.parameters.len() != expected_signature.parameters.len()
            || !callable.body.captures.is_empty()
        {
            return Err(format!(
                "Actor body '{}' has an incomplete receiver/signature contract",
                body_owner.0
            ));
        }
        for (local, parameter) in callable
            .body
            .parameters
            .iter()
            .zip(&expected_signature.parameters)
        {
            if !callable
                .body
                .locals
                .get(local)
                .is_some_and(|entry| entry.id == *local && entry.ty == parameter.ty)
            {
                return Err(format!(
                    "Actor body '{}' parameter local identity/type disagrees",
                    body_owner.0
                ));
            }
        }
        let receiver = callable
            .body
            .parameters
            .first()
            .ok_or_else(|| format!("Actor body '{}' has no receiver local", body_owner.0))?;
        method_receivers.insert(body_owner.clone(), receiver.clone());
    }
    materialize_snapshot(CheckedActorSnapshot {
        declaration,
        method_receivers,
    })
}

fn materialize_snapshot(snapshot: CheckedActorSnapshot) -> Result<MirActorContract, String> {
    let descriptor = snapshot.declaration.descriptor();
    let dispatch_methods = snapshot.declaration.dispatch_methods();
    if descriptor.fields.len() > 64 || dispatch_methods.is_empty() || dispatch_methods.len() > 64 {
        return Err(format!(
            "Actor '{}' exceeds the bounded scalar state/dispatch contract",
            descriptor.actor.0
        ));
    }
    let state_type = actor_state_type_id(&descriptor.actor);
    let mut fields = Vec::new();
    if let Some(flow) = &descriptor.flow {
        for (index, field) in flow.payload.iter().enumerate() {
            fields.push(MirActorFieldContract {
                field: field.field.clone(),
                initializer: NodeId(format!("{}/actor-flow-root-default", field.field.0)),
                name: field.name.clone(),
                index,
                ty: field.ty.clone(),
                mutable: false,
                initial_value: 0,
            });
        }
    } else {
        for (index, field) in descriptor.fields.iter().enumerate() {
            let ResolvedActorInitializerKind::Literal(ResolvedLiteral::Int(initial)) =
                &field.initializer.kind
            else {
                return Err(format!(
                    "Actor field '{}' has no materialized i32 initializer",
                    field.field.0
                ));
            };
            let initial_value = i32::try_from(*initial).map_err(|_| {
                format!("Actor field '{}' initializer overflows i32", field.field.0)
            })?;
            fields.push(MirActorFieldContract {
                field: field.field.clone(),
                initializer: field.initializer.node_id.clone(),
                name: field.name.clone(),
                index,
                ty: field.ty.clone(),
                mutable: field.mutable,
                initial_value,
            });
        }
    }
    let mut methods = Vec::new();
    for (index, method) in dispatch_methods.iter().enumerate() {
        let flow_transition = descriptor.flow.as_ref().and_then(|flow| {
            flow.transitions
                .iter()
                .find(|transition| transition.method_owner == method.owner)
                .cloned()
        });
        let body_owner = flow_transition
            .as_ref()
            .map(|transition| transition.body_owner.clone())
            .unwrap_or_else(|| method.owner.clone());
        let self_local = snapshot
            .method_receivers
            .get(&body_owner)
            .ok_or_else(|| {
                format!(
                    "Actor body '{}' has no checker receiver identity",
                    body_owner.0
                )
            })?
            .clone();
        let source_parameter_types = method
            .signature
            .parameters
            .iter()
            .map(|parameter| parameter.ty.clone())
            .collect::<Vec<_>>();
        if source_parameter_types
            .len()
            .checked_sub(1)
            .is_none_or(|count| count > 64)
        {
            return Err(format!(
                "Actor method '{}' exceeds the bounded payload contract",
                method.owner.0
            ));
        }
        let source_receiver_permission = method
            .signature
            .parameters
            .first()
            .ok_or_else(|| format!("Actor method '{}' lacks a source receiver", method.owner.0))?
            .permission;
        if source_receiver_permission == Some(Permission::Consume) {
            return Err(format!(
                "Actor method '{}' requires a separate consuming receiver contract",
                method.owner.0
            ));
        }
        let mut execution_parameter_types = source_parameter_types.clone();
        let execution_result_type;
        if let Some(transition) = &flow_transition {
            execution_parameter_types = transition
                .body_signature
                .parameters
                .iter()
                .map(|parameter| parameter.ty.clone())
                .collect();
            let flow = descriptor
                .flow
                .as_ref()
                .ok_or_else(|| "Flow transition has no root-state descriptor".to_string())?;
            if execution_parameter_types.first() != Some(&flow.root_state_type)
                || transition.body_signature.result != flow.root_state_type
            {
                return Err(format!(
                    "Flow transition '{}' does not use the exact root state ABI",
                    transition.body_owner.0
                ));
            }
            // The closed mailbox ABI stores this one-field root state as an
            // i32 slot. ActorLowerer applies the checked one-field record
            // projection/construction law at the worker boundary.
            execution_parameter_types[0] = crate::core::ir::primitive_type_id(PrimitiveType::I32);
            execution_result_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
        } else {
            execution_result_type = method.signature.result.clone();
        }
        let Some(receiver_type) = execution_parameter_types.first_mut() else {
            return Err(format!(
                "Actor method '{}' lacks an execution receiver signature",
                method.owner.0
            ));
        };
        if flow_transition.is_none() {
            *receiver_type = state_type.clone();
        }
        let self_value = MirValueId::new(format!("local:{}", self_local.0 .0))
            .map_err(|error| error.to_string())?;
        methods.push(MirActorMethodContract {
            owner: method.owner.clone(),
            body_owner,
            index,
            self_local,
            self_value,
            source_parameter_types,
            source_receiver_permission,
            execution_parameter_types,
            result_type: method.signature.result.clone(),
            execution_result_type,
            flow_transition,
            source_effects: method.signature.effects.clone(),
        });
    }
    Ok(MirActorContract {
        schema: MIR_SCALAR_ACTOR_SCHEMA,
        actor: descriptor.actor.clone(),
        handle_type: descriptor.handle_type.clone(),
        state_type,
        flow: descriptor.flow.clone(),
        fields,
        methods,
        checker_snapshot: snapshot,
    })
}

impl MirActorContract {
    pub(crate) fn validate_snapshot(&self) -> Result<(), String> {
        let expected = materialize_snapshot(self.checker_snapshot.clone())?;
        if self != &expected {
            return Err(format!(
                "Actor '{}' state/dispatch contract differs from its checker-derived snapshot",
                self.actor.0
            ));
        }
        Ok(())
    }

    pub(crate) fn handle_descriptor(&self) -> MirTypeDesc {
        MirTypeDesc {
            id: self.handle_type.clone(),
            kind: MirTypeKind::ActorHandle {
                actor: self.actor.clone(),
            },
            layout: MirLayout::Handle,
            native_record_abi: None,
            session_protocol: None,
            ownership: MirOwnership::Copy,
            abi: MirAbiClass::OpaqueHandle,
            needs_drop_glue: false,
            needs_clone_glue: false,
            glue: MirGlueContract {
                move_out: MirGlueKind::Noop,
                clone: MirGlueKind::Noop,
                drop: MirGlueKind::Noop,
            },
            drop_plan: None,
            variant_drop_plan: None,
        }
    }

    pub(crate) fn state_descriptor(&self) -> MirTypeDesc {
        MirTypeDesc {
            id: self.state_type.clone(),
            kind: MirTypeKind::ActorState {
                actor: self.actor.clone(),
            },
            layout: MirLayout::ActorState {
                actor: self.actor.clone(),
                fields: self
                    .fields
                    .iter()
                    .map(|field| MirFieldDesc {
                        id: field.field.clone(),
                        name: field.name.clone(),
                        ty: field.ty.clone(),
                    })
                    .collect(),
            },
            native_record_abi: None,
            session_protocol: None,
            ownership: MirOwnership::WeakBorrow,
            abi: MirAbiClass::Pointer,
            needs_drop_glue: false,
            needs_clone_glue: false,
            glue: MirGlueContract {
                move_out: MirGlueKind::Unsupported,
                clone: MirGlueKind::Unsupported,
                drop: MirGlueKind::Unsupported,
            },
            drop_plan: None,
            variant_drop_plan: None,
        }
    }

    pub(crate) fn canonical_text(&self) -> String {
        let mut text = String::from("mir.actor ");
        for value in [
            self.schema,
            &self.actor.0,
            self.handle_type.as_str(),
            self.state_type.as_str(),
        ] {
            frame(&mut text, value);
        }
        text.push('\n');
        for field in &self.fields {
            text.push_str(&format!(
                "field index={} mutable={} initial={} ",
                field.index, field.mutable, field.initial_value
            ));
            for value in [
                &field.field.0,
                &field.initializer.0,
                &field.name,
                field.ty.as_str(),
            ] {
                frame(&mut text, value);
            }
            text.push('\n');
        }
        for method in &self.methods {
            text.push_str(&format!("method index={} ", method.index));
            for value in [
                &method.owner.0,
                &method.body_owner.0,
                &method.self_local.0 .0,
                method.self_value.as_str(),
                method.result_type.as_str(),
                method.execution_result_type.as_str(),
            ] {
                frame(&mut text, value);
            }
            if let Some(transition) = &method.flow_transition {
                frame(&mut text, &format!("flow-transition:{:?}", transition));
            } else {
                frame(&mut text, "actor-method");
            }
            text.push_str("source-effects ");
            for effect in &method.source_effects {
                frame(&mut text, effect.as_str());
            }
            for ty in &method.source_parameter_types {
                frame(&mut text, ty.as_str());
            }
            text.push_str(&format!(
                "source-permission={:?} execution ",
                method.source_receiver_permission
            ));
            for ty in &method.execution_parameter_types {
                frame(&mut text, ty.as_str());
            }
            text.push('\n');
        }
        if let Some(flow) = &self.flow {
            text.push_str("flow ");
            for value in [
                &flow.flow.0,
                &flow.flow_owner.0,
                &flow.root_state.0,
                flow.root_state_type.as_str(),
            ] {
                frame(&mut text, value);
            }
            text.push('\n');
        }
        text
    }
}

fn frame(text: &mut String, value: &str) {
    text.push_str(&format!("{}:{} ", value.len(), value));
}

#[cfg(test)]
mod tests {
    use super::super::types::MirGlueOperation;
    use super::*;

    fn checked(source: &str) -> CheckedProgram {
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("lex Actor fixture");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse Actor fixture");
        crate::core::check_program(&file).expect("check Actor fixture")
    }

    fn materialized(source: &str) -> (CheckedProgram, MirActorCatalog, MirTypeCatalog) {
        let program = checked(source);
        let actors = MirActorCatalog::from_checked_program(
            &program,
            &program.actor_descriptors().keys().cloned().collect(),
        )
        .expect("Actor receipts");
        let mut types = MirTypeCatalog::from_checked_program(&program).expect("MIR type catalog");
        types
            .materialize_actor_catalog(&actors)
            .expect("materialize Actor state");
        (program, actors, types)
    }

    #[test]
    fn canonical_actor_mailbox_metadata_requires_exact_checked_self_permission() {
        let source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/real_world/actor_nested_func_capture.mimi"
        ));
        let (program, actors, types) = materialized(source);
        let main = program
            .callable(&NodeId("function:main".into()))
            .expect("root body");
        let call = main
            .body
            .root
            .statements
            .iter()
            .find_map(|statement| {
                let crate::core::ir::ResolvedStmtKind::Bind {
                    initializer: Some(expression),
                    ..
                } = &statement.kind
                else {
                    return None;
                };
                let crate::core::ir::ResolvedExprKind::Call(call) = &expression.kind else {
                    return None;
                };
                matches!(call.callee, ResolvedCallee::ActorMethod { .. }).then_some(call)
            })
            .expect("checked mailbox call");
        assert_eq!(call.permission, Some(Permission::Mutate));
        let checked_method = program
            .callable(&NodeId("function:Counter::bump".into()))
            .expect("checked Actor method summary");
        assert_eq!(call.effects, checked_method.signature.effects);
        assert!(call.session.is_empty());
        actors.validate_source_mailbox_call(call).unwrap();
        for permission in [None, Some(Permission::View), Some(Permission::Consume)] {
            let mut forged = call.clone();
            forged.permission = permission;
            assert!(actors.validate_source_mailbox_call(&forged).is_err());
        }
        let mut forged = call.clone();
        forged
            .effects
            .push(crate::core::EffectId::new("forged_actor_effect").unwrap());
        assert!(actors.validate_source_mailbox_call(&forged).is_err());
        let mut session_forged = call.clone();
        let crate::core::ir::ResolvedExprKind::Load(receiver) = &call.arguments[0].value.kind
        else {
            panic!("direct checked handle");
        };
        session_forged
            .session
            .push(crate::core::ir::SessionTransition {
                endpoint: receiver.base.clone(),
                before: crate::core::SessionResidualId::new("actor-forged-before").unwrap(),
                after: crate::core::SessionResidualId::new("actor-forged-after").unwrap(),
                terminal: false,
            });
        assert!(actors
            .validate_source_mailbox_call(&session_forged)
            .is_err());
        let mut forged = actors.clone();
        forged.contracts.values_mut().next().unwrap().methods[0].source_receiver_permission =
            Some(Permission::View);
        let mut coordinated = call.clone();
        coordinated.permission = Some(Permission::View);
        assert!(
            forged.validate_source_mailbox_call(&coordinated).is_err(),
            "call and public catalog cannot coordinate a private checker permission substitution"
        );
        assert!(
            forged.validate(&types).is_err(),
            "public permission cannot replace the private checker snapshot"
        );
    }

    #[test]
    fn original_actor_programs_have_distinct_checker_bound_handle_and_state_catalogs() {
        for source in [
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/real_world/actor_nested_func_capture.mimi"
            )),
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/real_world/actor_nested_func_shadow.mimi"
            )),
        ] {
            let (program, actors, types) = materialized(source);
            assert_eq!(actors.contracts.len(), 1);
            actors.validate(&types).unwrap();
            let contract = actors.contracts.values().next().unwrap();
            assert_ne!(contract.handle_type, contract.state_type);
            assert_eq!(contract.fields[0].initial_value, 0);
            assert_eq!(contract.fields[0].index, 0);
            let method = &contract.methods[0];
            assert_eq!(method.source_parameter_types[0], contract.handle_type);
            assert_eq!(method.source_receiver_permission, Some(Permission::Mutate));
            assert_eq!(method.execution_parameter_types[0], contract.state_type);
            assert_eq!(
                method.source_parameter_types[1..],
                method.execution_parameter_types[1..]
            );
            assert!(program
                .callable(&method.owner)
                .unwrap()
                .body
                .root
                .statements
                .iter()
                .any(|statement| matches!(
                    statement.kind,
                    crate::core::ir::ResolvedStmtKind::NestedCallable(_)
                )));
            assert!(types
                .validate_glue(&contract.handle_type, MirGlueOperation::Clone)
                .is_ok());
            for operation in [
                MirGlueOperation::Clone,
                MirGlueOperation::MoveOut,
                MirGlueOperation::Drop,
            ] {
                assert!(types
                    .validate_glue(&contract.state_type, operation)
                    .is_err());
            }
            assert!(actors.canonical_text().contains("execution"));
        }
    }

    const TWO_FIELDS: &str = r#"
        actor Counter {
            first: i32 = 7;
            second: i32 = 9;
            func sum(delta: i32) -> i32 { self.first + self.second + delta }
        }
        func main() -> i32 { let c = Counter.spawn(); c.sum(1) }
    "#;

    #[test]
    fn same_type_field_slot_and_catalog_forgery_fails_private_initializer_snapshot() {
        let (_, mut actors, mut types) = materialized(TWO_FIELDS);
        let contract = actors.contracts.values_mut().next().unwrap();
        contract.fields.swap(0, 1);
        for (index, field) in contract.fields.iter_mut().enumerate() {
            field.index = index;
        }
        types.replace_for_test_only(contract.state_type.clone(), contract.state_descriptor());
        let errors = actors
            .validate(&types)
            .expect_err("same-type coordinated field forgery");
        assert!(
            errors
                .iter()
                .any(|error| error.contains("checker-derived snapshot")),
            "{errors:?}"
        );
    }

    #[test]
    fn oversized_field_dispatch_and_foreign_receiver_ids_are_not_execution_authority() {
        let (_, actors, types) = materialized(TWO_FIELDS);
        for corruption in 0..4 {
            let mut forged = actors.clone();
            let contract = forged.contracts.values_mut().next().unwrap();
            match corruption {
                0 => contract.fields[0].index = usize::MAX,
                1 => contract.methods[0].index = usize::MAX,
                2 => {
                    let method = &mut contract.methods[0];
                    method.self_local = ResolvedLocalId(NodeId("forged:same-type-self".into()));
                    method.self_value = MirValueId::new("local:forged:same-type-self").unwrap();
                }
                _ => contract.state_type = contract.handle_type.clone(),
            }
            let errors = forged
                .validate(&types)
                .expect_err("corrupted Actor contract");
            assert!(
                errors
                    .iter()
                    .any(|error| error.contains("checker-derived snapshot")),
                "{errors:?}"
            );
        }
    }

    #[test]
    fn actor_catalog_installation_is_atomic_and_rejects_a_state_type_collision() {
        let program = checked(TWO_FIELDS);
        let actors = MirActorCatalog::from_checked_program(
            &program,
            &program.actor_descriptors().keys().cloned().collect(),
        )
        .unwrap();
        let mut types = MirTypeCatalog::from_checked_program(&program).unwrap();
        let contract = actors.contracts.values().next().unwrap();
        let mut impostor = types.get(&contract.fields[0].ty).unwrap().clone();
        impostor.id = contract.state_type.clone();
        types.replace_for_test_only(contract.state_type.clone(), impostor);
        let before = types.clone();
        assert!(types.materialize_actor_catalog(&actors).is_err());
        assert_eq!(types, before);
    }

    #[test]
    fn unmodeled_actor_payloads_stay_outside_the_selected_scalar_catalog() {
        let source = r#"
            actor Wide {
                // i64 state is well-typed but remains outside the scalar i32 Actor profile.
                value: i64;
                func read() -> i32 { 42 }
            }
            func main() -> i32 { let h = Wide.spawn(); h.read() }
        "#;
        let program = checked(source);
        let selected_error = MirActorCatalog::from_checked_program(
            &program,
            &program.actor_descriptors().keys().cloned().collect(),
        )
        .expect_err("well-typed i64 Actor state must remain outside the i32 profile");
        assert!(
            selected_error.iter().any(|message| message
                .contains("scalar Actor state requires materialized i32 initializers")),
            "the selected Actor must fail at the narrow state-type gate: {selected_error:?}"
        );
        let empty = MirActorCatalog::from_checked_program(&program, &BTreeSet::new()).unwrap();
        assert!(empty.contracts.is_empty());
    }

    #[test]
    fn source_ffi_cannot_manufacture_private_actor_root_authority() {
        for symbol in [
            "mimi_mir_actor_domain_open",
            "mimi_mir_actor_domain_spawn",
            "mimi_mir_actor_domain_call",
            "mimi_mir_actor_domain_close",
            "mimi_mir_actor_domain_destroy",
        ] {
            let error = super::super::validate_ffi_symbol_manifest_safety(symbol).unwrap_err();
            assert!(
                error.contains("reserved Canonical MIR ActorExecutionDomain runtime helper"),
                "{error}"
            );
        }
        assert!(
            super::super::validate_ffi_symbol_manifest_safety("mimi_mir_map_root_new")
                .unwrap_err()
                .contains("reserved Canonical MIR MapRoot runtime helper")
        );
        assert!(
            super::super::validate_ffi_symbol_manifest_safety("user_actor_domain_callback").is_ok()
        );
    }
}
