//! Deterministic, root-owned Actor state for the canonical MIR oracle.
//!
//! This models synchronous turns only. It has no source AST, worker thread or
//! mailbox fallback. The production consumers retain real mailbox semantics;
//! this independent oracle retains the same instance, role and close rules.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::core::NodeId;

use super::actor::MirActorCatalog;
use super::reference::{MirExecutionError, MirRuntimeValue};

// Opaque reference identities never enter scalar stdout/results. A checked,
// monotonic root identity also rejects handles from another interpreter.
static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirActorReferenceHandle {
    actor: NodeId,
    root: u64,
    instance: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirActorReferenceState {
    actor: NodeId,
    root: u64,
    instance: u64,
    turn: u64,
}

#[derive(Debug)]
struct ActorInstance {
    actor: NodeId,
    fields: Vec<i32>,
    active: Option<(u64, NodeId)>,
}

#[derive(Debug)]
pub(crate) struct MirActorReferenceTurn {
    state: MirActorReferenceState,
    pub(crate) owner: NodeId,
    pub(crate) arguments: Vec<MirRuntimeValue>,
    flow_state_slot: Option<usize>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MirActorReferenceLifecycle {
    pub(crate) open: bool,
    pub(crate) live_instances: usize,
    pub(crate) active_turns: usize,
    pub(crate) spawned: u64,
    pub(crate) started: u64,
    pub(crate) completed: u64,
    pub(crate) failed: u64,
}

#[derive(Debug, Default)]
pub(crate) struct MirActorReferenceDomain {
    root: Option<u64>,
    next_instance: u64,
    next_turn: u64,
    instances: BTreeMap<u64, ActorInstance>,
    lifecycle: MirActorReferenceLifecycle,
}

impl MirActorReferenceDomain {
    pub(crate) fn open(&mut self) -> Result<(), String> {
        if self.root.is_some() || !self.instances.is_empty() {
            return Err("reference Actor root is already open".into());
        }
        let root = NEXT_ROOT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |root| {
                root.checked_add(1)
            })
            .map_err(|_| "reference Actor root identity space exhausted".to_string())?;
        self.root = Some(root);
        self.next_instance = 1;
        self.next_turn = 1;
        self.lifecycle = MirActorReferenceLifecycle {
            open: true,
            ..MirActorReferenceLifecycle::default()
        };
        Ok(())
    }

    /// Synchronous execution has no queued messages. Every started turn is
    /// settled by finish_call before an error propagates; the root guard also
    /// clears all state on preflight failures and unwind.
    pub(crate) fn close(&mut self) {
        // A root unwind/cancellation can bypass normal turn settlement. Mark
        // each still-active synchronous turn terminal before releasing state;
        // a repeated close observes no active turns and cannot settle twice.
        self.lifecycle.failed += self.lifecycle.active_turns as u64;
        self.root = None;
        self.instances.clear();
        self.lifecycle.open = false;
        self.lifecycle.live_instances = 0;
        self.lifecycle.active_turns = 0;
    }

    #[cfg(test)]
    pub(crate) fn lifecycle(&self) -> MirActorReferenceLifecycle {
        self.lifecycle
    }

    fn root(&self) -> Result<u64, String> {
        self.root
            .ok_or_else(|| "reference Actor root is closed".into())
    }

    fn require_outer_turn(&self) -> Result<(), String> {
        if self
            .instances
            .values()
            .any(|instance| instance.active.is_some())
        {
            return Err(
                "nested Actor spawn/mailbox call has no finite scalar turn contract".into(),
            );
        }
        Ok(())
    }

    pub(crate) fn spawn(
        &mut self,
        actors: &MirActorCatalog,
        actor: &NodeId,
    ) -> Result<MirRuntimeValue, String> {
        let root = self.root()?;
        self.require_outer_turn()?;
        let contract = actors
            .get(actor)
            .ok_or_else(|| format!("Actor '{}' has no canonical state contract", actor.0))?;
        contract.validate_snapshot()?;
        let instance = self.next_instance;
        let next = instance
            .checked_add(1)
            .ok_or_else(|| "reference Actor instance identity space exhausted".to_string())?;
        let fields = contract
            .fields
            .iter()
            .map(|field| field.initial_value)
            .collect();
        self.instances.insert(
            instance,
            ActorInstance {
                actor: actor.clone(),
                fields,
                active: None,
            },
        );
        self.next_instance = next;
        self.lifecycle.spawned += 1;
        self.lifecycle.live_instances = self.instances.len();
        Ok(MirRuntimeValue::ActorHandle(MirActorReferenceHandle {
            actor: actor.clone(),
            root,
            instance,
        }))
    }

    pub(crate) fn prepare_call(
        &mut self,
        actors: &MirActorCatalog,
        actor: &NodeId,
        method: &NodeId,
        handle: &MirRuntimeValue,
        payload: Vec<MirRuntimeValue>,
    ) -> Result<MirActorReferenceTurn, String> {
        let root = self.root()?;
        self.require_outer_turn()?;
        let MirRuntimeValue::ActorHandle(handle) = handle else {
            return Err("Actor mailbox call requires an external handle role".into());
        };
        if handle.root != root || &handle.actor != actor {
            return Err("Actor mailbox handle belongs to another root or Actor".into());
        }
        let contract = actors
            .get(actor)
            .ok_or_else(|| format!("Actor '{}' has no canonical dispatch contract", actor.0))?;
        contract.validate_snapshot()?;
        let method = contract
            .methods
            .iter()
            .find(|entry| &entry.owner == method)
            .ok_or_else(|| {
                "Actor method identity is absent from its owner dispatch contract".to_string()
            })?;
        if method.execution_parameter_types.len().checked_sub(1) != Some(payload.len()) {
            return Err("Actor mailbox payload arity disagrees with its method contract".into());
        }
        for value in &payload {
            scalar_i32(value, "Actor mailbox payload")?;
        }
        let instance = self
            .instances
            .get_mut(&handle.instance)
            .ok_or_else(|| "Actor mailbox handle names no live instance".to_string())?;
        if instance.actor != *actor || instance.active.is_some() {
            return Err("Actor mailbox instance owner/turn is inconsistent".into());
        }
        let turn = self.next_turn;
        let next = turn
            .checked_add(1)
            .ok_or_else(|| "reference Actor turn identity space exhausted".to_string())?;
        let state = MirActorReferenceState {
            actor: actor.clone(),
            root,
            instance: handle.instance,
            turn,
        };
        let flow_state_slot = if method.flow_transition.is_some() {
            let [field] = contract.fields.as_slice() else {
                return Err("Flow Actor has no exact one-field root-state slot".into());
            };
            let value = instance
                .fields
                .get(field.index)
                .copied()
                .ok_or_else(|| "Flow Actor root-state slot is absent".to_string())?;
            let mut arguments = Vec::with_capacity(payload.len() + 1);
            arguments.push(MirRuntimeValue::Int(i64::from(value)));
            arguments.extend(payload);
            instance.active = Some((turn, method.body_owner.clone()));
            self.next_turn = next;
            self.lifecycle.started += 1;
            self.lifecycle.active_turns += 1;
            return Ok(MirActorReferenceTurn {
                state,
                owner: method.body_owner.clone(),
                arguments,
                flow_state_slot: Some(field.index),
            });
        } else {
            None
        };
        let mut arguments = Vec::with_capacity(payload.len() + 1);
        arguments.push(MirRuntimeValue::ActorState(state.clone()));
        arguments.extend(payload);
        instance.active = Some((turn, method.body_owner.clone()));
        self.next_turn = next;
        self.lifecycle.started += 1;
        self.lifecycle.active_turns += 1;
        Ok(MirActorReferenceTurn {
            state,
            owner: method.body_owner.clone(),
            arguments,
            flow_state_slot,
        })
    }

    pub(crate) fn finish_call(
        &mut self,
        turn: &MirActorReferenceTurn,
        outcome: &Result<MirRuntimeValue, MirExecutionError>,
    ) -> Result<(), String> {
        self.state_instance(&turn.owner, &turn.state)?;
        let result_value = outcome
            .as_ref()
            .map(|value| scalar_i32(value, "Actor mailbox result"));
        let result_validation = result_value.as_ref().map_or(Ok(()), |value| {
            value.as_ref().map(|_| ()).map_err(Clone::clone)
        });
        let instance = self
            .instances
            .get_mut(&turn.state.instance)
            .ok_or_else(|| "Actor turn instance disappeared before settlement".to_string())?;
        if outcome.is_ok() && result_validation.is_ok() {
            if let Some(index) = turn.flow_state_slot {
                let value = result_value
                    .as_ref()
                    .ok()
                    .and_then(|value| value.as_ref().ok())
                    .copied()
                    .ok_or_else(|| "Flow Actor result disappeared before commit".to_string())?;
                let Some(slot) = instance.fields.get_mut(index) else {
                    return Err("Flow Actor root-state slot disappeared before commit".into());
                };
                *slot = value;
            }
        }
        instance.active = None;
        self.lifecycle.active_turns -= 1;
        if outcome.is_ok() && result_validation.is_ok() {
            self.lifecycle.completed += 1;
        } else {
            self.lifecycle.failed += 1;
        }
        result_validation
    }

    pub(crate) fn validate_method_entry(
        &self,
        actors: &MirActorCatalog,
        owner: &NodeId,
        arguments: &[MirRuntimeValue],
    ) -> Result<(), String> {
        let Some((actor, method)) = actors.method(owner) else {
            if arguments.iter().any(contains_actor_role) {
                return Err("ordinary direct call cannot accept Actor handle/state roles".into());
            }
            return Ok(());
        };
        if arguments.len() != method.execution_parameter_types.len() {
            return Err("Actor internal method signature has an incompatible arity".into());
        }
        if method.flow_transition.is_some() {
            let receiver = arguments.first().ok_or_else(|| {
                "Flow Actor method entry has no scalar root-state receiver".to_string()
            })?;
            let receiver = scalar_i32(receiver, "Flow Actor root-state receiver")?;
            let Some(instance) = self.instances.values().find(|instance| {
                instance.actor == actor.actor
                    && instance
                        .active
                        .as_ref()
                        .is_some_and(|(_, active_owner)| active_owner == owner)
            }) else {
                return Err(
                    "Flow Actor method entry is outside its exact active mailbox turn".into(),
                );
            };
            let [field] = actor.fields.as_slice() else {
                return Err("Flow Actor has no exact one-field root-state slot".into());
            };
            if instance.fields.get(field.index) != Some(&receiver) {
                return Err("Flow Actor receiver differs from its committed root state".into());
            }
            for value in arguments.iter().skip(1) {
                scalar_i32(value, "Actor internal method payload")?;
            }
            return Ok(());
        }
        let Some(MirRuntimeValue::ActorState(state)) = arguments.first() else {
            return Err("Actor method entry requires an internal state role".into());
        };
        if state.actor != actor.actor {
            return Err("Actor method state belongs to another Actor".into());
        }
        self.state_instance(owner, state)?;
        for value in arguments.iter().skip(1) {
            scalar_i32(value, "Actor internal method payload")?;
        }
        Ok(())
    }

    fn state_instance(
        &self,
        owner: &NodeId,
        state: &MirActorReferenceState,
    ) -> Result<&ActorInstance, String> {
        if state.root != self.root()? {
            return Err("Actor state context belongs to another root".into());
        }
        let instance = self
            .instances
            .get(&state.instance)
            .ok_or_else(|| "Actor state context names no live instance".to_string())?;
        if instance.actor != state.actor
            || instance.active.as_ref() != Some(&(state.turn, owner.clone()))
        {
            return Err("Actor state context is outside its exact method turn".into());
        }
        Ok(instance)
    }

    fn field_index(
        &self,
        actors: &MirActorCatalog,
        owner: &NodeId,
        actor: &NodeId,
        field: &NodeId,
        state: &MirRuntimeValue,
    ) -> Result<(usize, u64), String> {
        let MirRuntimeValue::ActorState(state) = state else {
            return Err("Actor field access requires an internal state role".into());
        };
        if &state.actor != actor {
            return Err("Actor field access state belongs to another Actor".into());
        }
        self.state_instance(owner, state)?;
        let (contract, _) = actors
            .method(owner)
            .ok_or_else(|| "Actor field access requires its exact method owner".to_string())?;
        if &contract.actor != actor {
            return Err("Actor field owner disagrees with the executing method".into());
        }
        contract.validate_snapshot()?;
        let field = contract
            .fields
            .iter()
            .find(|entry| &entry.field == field)
            .ok_or_else(|| {
                "Actor field identity is absent from its owner state contract".to_string()
            })?;
        // The source `mut` marker is descriptor provenance, not authority for
        // self stores: the preserved Actor fixtures have unmarked fields and
        // checker-approved stores. Exact typed operation receipts authorize
        // those stores before execution; consumers must preserve that fact.
        Ok((field.index, state.instance))
    }

    pub(crate) fn field_load(
        &self,
        actors: &MirActorCatalog,
        owner: &NodeId,
        actor: &NodeId,
        field: &NodeId,
        state: &MirRuntimeValue,
    ) -> Result<MirRuntimeValue, String> {
        let (index, instance) = self.field_index(actors, owner, actor, field, state)?;
        let value = self
            .instances
            .get(&instance)
            .and_then(|instance| instance.fields.get(index))
            .ok_or_else(|| "Actor field slot is absent from live state".to_string())?;
        Ok(MirRuntimeValue::Int(i64::from(*value)))
    }

    pub(crate) fn field_store(
        &mut self,
        actors: &MirActorCatalog,
        owner: &NodeId,
        actor: &NodeId,
        field: &NodeId,
        state: &MirRuntimeValue,
        value: &MirRuntimeValue,
    ) -> Result<(), String> {
        let (index, instance) = self.field_index(actors, owner, actor, field, state)?;
        let value = scalar_i32(value, "Actor field store")?;
        let slot = self
            .instances
            .get_mut(&instance)
            .and_then(|instance| instance.fields.get_mut(index))
            .ok_or_else(|| "Actor field slot is absent from live state".to_string())?;
        *slot = value;
        Ok(())
    }
}

fn scalar_i32(value: &MirRuntimeValue, label: &str) -> Result<i32, String> {
    match value {
        MirRuntimeValue::Int(value) => {
            i32::try_from(*value).map_err(|_| format!("{label} is outside the canonical i32 range"))
        }
        _ => Err(format!("{label} requires a canonical i32 runtime value")),
    }
}

pub(crate) fn contains_actor_role(value: &MirRuntimeValue) -> bool {
    match value {
        MirRuntimeValue::ActorHandle(_) | MirRuntimeValue::ActorState(_) => true,
        MirRuntimeValue::Tuple(values)
        | MirRuntimeValue::List(values)
        | MirRuntimeValue::Set(values)
        | MirRuntimeValue::Record { fields: values, .. }
        | MirRuntimeValue::Variant {
            payload: values, ..
        } => values.iter().any(contains_actor_role),
        _ => false,
    }
}

/// Held across every root preflight/execution Result and unwinding path.
pub(crate) struct MirActorReferenceRootGuard<'a> {
    domain: &'a std::cell::RefCell<MirActorReferenceDomain>,
}

impl<'a> MirActorReferenceRootGuard<'a> {
    pub(crate) fn open(
        domain: &'a std::cell::RefCell<MirActorReferenceDomain>,
    ) -> Result<Self, String> {
        domain.borrow_mut().open()?;
        Ok(Self { domain })
    }
}

impl Drop for MirActorReferenceRootGuard<'_> {
    fn drop(&mut self) {
        self.domain.borrow_mut().close();
    }
}
