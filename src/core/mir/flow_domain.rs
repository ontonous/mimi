//! Canonical Flow-domain event contract shared by MIR consumers.
//!
//! Runtime owns the mutable owner/lease table.  This module is the immutable
//! semantic gate consumed by reference, bytecode, native and verifier paths:
//! each backend receives the same `FlowTrace`, validates the same lifecycle
//! grammar, and publishes a backend-labelled receipt whose semantic identity
//! excludes that label.  No backend reconstructs a Flow product or invents a
//! rollback for another independent instance.

use std::collections::BTreeMap;

use crate::runtime::flow_domain::{
    FlowCancelState, FlowEvent, FlowEventKind, FlowHandle, FlowParticipantId, FlowTrace,
    FLOW_DOMAIN_SCHEMA,
};

/// Schema for the immutable MIR-side event receipt.
pub const MIR_FLOW_EVENT_SCHEMA: &str = "mimi-mir-flow-events-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MirFlowBackend {
    Reference,
    Bytecode,
    Native,
    Verifier,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirFlowEventReceipt {
    pub schema: &'static str,
    pub backend: MirFlowBackend,
    pub domain: u64,
    pub digest: String,
    pub event_count: usize,
    pub events: Vec<FlowEvent>,
}

impl MirFlowEventReceipt {
    pub fn from_trace(backend: MirFlowBackend, trace: &FlowTrace) -> Result<Self, String> {
        validate_flow_trace(trace)?;
        let domain = trace.events.first().map_or(0, |event| event.domain);
        if trace.events.iter().any(|event| event.domain != domain) {
            return Err("Flow trace combines independent domains".into());
        }
        Ok(Self {
            schema: MIR_FLOW_EVENT_SCHEMA,
            backend,
            domain,
            digest: trace.digest(),
            event_count: trace.events.len(),
            events: trace.events.clone(),
        })
    }

    pub fn reference(trace: &FlowTrace) -> Result<Self, String> {
        Self::from_trace(MirFlowBackend::Reference, trace)
    }

    pub fn bytecode(trace: &FlowTrace) -> Result<Self, String> {
        Self::from_trace(MirFlowBackend::Bytecode, trace)
    }

    pub fn native(trace: &FlowTrace) -> Result<Self, String> {
        Self::from_trace(MirFlowBackend::Native, trace)
    }

    pub fn verifier(trace: &FlowTrace) -> Result<Self, String> {
        Self::from_trace(MirFlowBackend::Verifier, trace)
    }

    /// Compare two consumer receipts as semantic executions.  Backend labels
    /// are provenance only and intentionally do not affect equality.
    pub fn equivalent(&self, other: &Self) -> bool {
        self.schema == other.schema
            && self.domain == other.domain
            && self.digest == other.digest
            && self.event_count == other.event_count
            && self.events == other.events
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != MIR_FLOW_EVENT_SCHEMA {
            return Err("MIR Flow event receipt schema mismatch".into());
        }
        if self.event_count != self.events.len() {
            return Err("MIR Flow event count does not match event payload".into());
        }
        let trace = FlowTrace {
            schema: FLOW_DOMAIN_SCHEMA,
            events: self.events.clone(),
        };
        validate_flow_trace(&trace)?;
        if trace.digest() != self.digest {
            return Err("MIR Flow event receipt digest mismatch".into());
        }
        if self
            .events
            .first()
            .is_some_and(|event| event.domain != self.domain)
        {
            return Err("MIR Flow event receipt domain mismatch".into());
        }
        Ok(())
    }
}

/// Require every backend to consume the exact same immutable trace.  Empty
/// input is accepted for programs with no Flow domain; callers still receive a
/// stable schema and zero digest rather than silently skipping validation.
pub fn validate_backend_equivalence(receipts: &[MirFlowEventReceipt]) -> Result<(), String> {
    if receipts.is_empty() {
        return Ok(());
    }
    for receipt in receipts {
        receipt.validate()?;
    }
    let first = &receipts[0];
    if receipts
        .iter()
        .skip(1)
        .any(|receipt| !first.equivalent(receipt))
    {
        return Err("Flow reference/bytecode/native/verifier event traces diverge".into());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReplayInstance {
    opened: bool,
    terminal: bool,
    owner: Option<FlowParticipantId>,
    lease: Option<u64>,
    lease_owner: Option<FlowParticipantId>,
    cancel: Option<(u64, FlowCancelState)>,
}

/// Replay the owner/lease/cancel grammar without consulting a backend.  This
/// is the verifier-side oracle and deliberately rejects a forged event stream
/// before a route receipt can be accepted.
pub fn validate_flow_trace(trace: &FlowTrace) -> Result<(), String> {
    trace.validate()?;
    let mut instances = BTreeMap::<FlowHandle, ReplayInstance>::new();
    // `FlowDomain::route` emits one contiguous 0..N-1 ordinal stream for
    // each explicit route.  A fresh zero starts the next route; ordinals are
    // therefore global to the trace rather than attached to one handle.
    let mut next_route: Option<u32> = None;
    for event in &trace.events {
        let entry = instances.entry(event.handle).or_insert(ReplayInstance {
            opened: false,
            terminal: false,
            owner: None,
            lease: None,
            lease_owner: None,
            cancel: None,
        });
        match event.kind {
            FlowEventKind::Open { .. } => {
                if entry.opened || event.lease.is_some() || event.request.is_some() {
                    return Err(format!(
                        "Flow {} opens more than once or with a lease/request",
                        event.handle.instance()
                    ));
                }
                entry.opened = true;
                entry.owner = Some(event.actor);
            }
            FlowEventKind::LeaseAcquired => {
                require_open(entry, event)?;
                if entry.lease.is_some()
                    || event.lease.is_none()
                    || entry.owner != Some(event.actor)
                {
                    return Err("Flow lease acquire is duplicated or missing lease id".into());
                }
                entry.lease = event.lease;
                entry.lease_owner = Some(event.actor);
            }
            FlowEventKind::LeaseReleased => {
                require_open(entry, event)?;
                if entry.lease != event.lease {
                    return Err("Flow lease release does not match the active lease".into());
                }
                entry.lease = None;
                entry.lease_owner = None;
            }
            FlowEventKind::LeaseTransferred { from, to } => {
                require_open(entry, event)?;
                if entry.lease != event.lease
                    || entry.owner != Some(from)
                    || entry.lease_owner != Some(from)
                    || event.actor != from
                {
                    return Err("Flow lease transfer has the wrong source owner".into());
                }
                entry.owner = Some(to);
                entry.lease_owner = Some(to);
            }
            FlowEventKind::Route { ordinal } => {
                require_open(entry, event)?;
                match (next_route, ordinal) {
                    (None, 0) => next_route = Some(1),
                    (Some(_), 0) => next_route = Some(1),
                    (Some(expected), actual) if expected == actual => {
                        next_route = expected.checked_add(1)
                    }
                    (expected, actual) => {
                        return Err(format!(
                            "Flow route ordinal is not deterministic: expected {:?}, got {}",
                            expected, actual
                        ));
                    }
                }
            }
            FlowEventKind::CancelRequested => {
                require_open(entry, event)?;
                let Some(request) = event.request else {
                    return Err("Flow cancellation request has no request id".into());
                };
                if entry.cancel.is_some() || event.actor.domain() != event.domain {
                    return Err("Flow cancellation request is duplicated or foreign".into());
                }
                entry.cancel = Some((request, FlowCancelState::Pending));
            }
            FlowEventKind::CancelPending => {
                require_open(entry, event)?;
                let Some(request) = event.request else {
                    return Err("Flow pending cancellation has no request id".into());
                };
                if entry.cancel != Some((request, FlowCancelState::Pending)) {
                    return Err("Flow pending cancellation has no pending request".into());
                }
            }
            FlowEventKind::CancelAcknowledged => {
                require_open(entry, event)?;
                let Some(request) = event.request else {
                    return Err("Flow cancellation acknowledgement has no request id".into());
                };
                if entry.cancel != Some((request, FlowCancelState::Pending)) {
                    return Err("Flow cancellation acknowledgement is out of order".into());
                }
                entry.cancel = Some((request, FlowCancelState::Acknowledged));
            }
            FlowEventKind::CancelTransferred { from, to } => {
                require_open(entry, event)?;
                let Some(request) = event.request else {
                    return Err("Flow cancellation transfer has no request id".into());
                };
                if entry.lease != event.lease
                    || entry.lease_owner != Some(to)
                    || entry.cancel != Some((request, FlowCancelState::Pending))
                        && entry.cancel != Some((request, FlowCancelState::Acknowledged))
                {
                    return Err("Flow cancellation transfer has the wrong owner/order".into());
                }
                entry.cancel = Some((request, FlowCancelState::Transferred { from, to }));
            }
            FlowEventKind::Completed { .. }
            | FlowEventKind::Failed { .. }
            | FlowEventKind::FaultInjected { .. } => {
                require_open(entry, event)?;
                if entry.terminal || entry.lease != event.lease || entry.lease.is_none() {
                    return Err("Flow terminal event has no unique active lease".into());
                }
                if entry
                    .cancel
                    .is_some_and(|(_, state)| !matches!(state, FlowCancelState::Settled))
                {
                    return Err("Flow terminal event races an unresolved cancellation".into());
                }
                entry.terminal = true;
                entry.lease = None;
                entry.lease_owner = None;
            }
            FlowEventKind::Cancelled => {
                require_open(entry, event)?;
                let Some(request) = event.request else {
                    return Err("Flow cancelled event has no request id".into());
                };
                if entry.terminal
                    || entry.lease != event.lease
                    || entry.lease.is_none()
                    || !matches!(
                        entry.cancel,
                        Some((
                            id,
                            FlowCancelState::Acknowledged
                                | FlowCancelState::Transferred { .. }
                        )) if id == request
                    )
                {
                    return Err("Flow cancelled event is duplicated or not acknowledged".into());
                }
                entry.cancel = Some((request, FlowCancelState::Settled));
                entry.terminal = true;
                entry.lease = None;
                entry.lease_owner = None;
            }
            FlowEventKind::Closed => {
                if !entry.opened || !entry.terminal || entry.lease.is_some() {
                    return Err("Flow close precedes a unique terminal settlement".into());
                }
            }
        }
    }
    Ok(())
}

fn require_open(entry: &ReplayInstance, event: &FlowEvent) -> Result<(), String> {
    if !entry.opened || entry.terminal {
        return Err(format!(
            "Flow event {:?} occurs before open or after terminal settlement",
            event.kind
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::flow_domain::{FlowDomainOwner, FlowRole, FlowRoute};

    fn trace_with_route_and_cancel() -> FlowTrace {
        let mut owner = FlowDomainOwner::new().expect("domain");
        let root = owner.participant();
        let worker = root
            .spawn_participant(crate::runtime::flow_domain::FlowRole::Reference(1))
            .expect("worker");
        let flow = root.open_flow(7).expect("flow");
        let second = root.open_flow(8).expect("flow");
        root.route(&FlowRoute::new(vec![flow, second]).expect("route"))
            .expect("route event");
        root.acquire_lease(flow)
            .expect("lease")
            .complete(70)
            .expect("complete");
        let mut lease = root.acquire_lease(second).expect("lease");
        let request = root.request_cancel(second).expect("cancel request");
        let lease = lease.transfer_cancel(&request, &worker).expect("transfer");
        lease.cancel(&request).expect("cancel");
        owner.close().expect("close");
        owner.trace()
    }

    #[test]
    fn all_four_consumers_accept_the_same_flow_trace() {
        let trace = trace_with_route_and_cancel();
        let receipts = [
            MirFlowEventReceipt::reference(&trace).expect("reference"),
            MirFlowEventReceipt::bytecode(&trace).expect("bytecode"),
            MirFlowEventReceipt::native(&trace).expect("native"),
            MirFlowEventReceipt::verifier(&trace).expect("verifier"),
        ];
        validate_backend_equivalence(&receipts).expect("backend parity");
        assert_eq!(receipts[0].digest, receipts[3].digest);
    }

    #[test]
    fn rejected_cancel_transfer_preserves_lease_and_backend_parity() {
        let mut owner = FlowDomainOwner::new().expect("domain");
        let root = owner.participant();
        let worker = root.spawn_participant(FlowRole::Task(9)).expect("worker");
        let flow = root.open_flow(17).expect("flow");
        let mut lease = root.acquire_lease(flow).expect("lease");
        let request = root.request_cancel(flow).expect("cancel request");

        // The request has not been transferred yet, so this succeeds and
        // leaves `lease` as the only authority held by the root.
        lease = lease
            .transfer_cancel(&request, &worker)
            .expect("first transfer");
        let before = root.inspect(flow).expect("observation before retry");
        assert_eq!(before.owner, worker.identity());
        assert_eq!(before.lease, Some(lease.id()));

        // A second transfer of the same cancellation is rejected.  Crucially
        // the failed operation must not first transfer the active lease back
        // to the root (the pre-fix implementation did exactly that).
        assert!(matches!(
            lease.transfer_cancel(&request, &root),
            Err(crate::runtime::flow_domain::FlowDomainError::InvalidTransfer)
        ));
        let after = root.inspect(flow).expect("observation after retry");
        assert_eq!(after.owner, worker.identity());
        assert_eq!(after.lease, Some(lease.id()));

        lease.cancel(&request).expect("settle cancellation");
        owner.close().expect("close");
        let trace = owner.trace();
        let receipts = [
            MirFlowEventReceipt::reference(&trace).expect("reference"),
            MirFlowEventReceipt::bytecode(&trace).expect("bytecode"),
            MirFlowEventReceipt::native(&trace).expect("native"),
            MirFlowEventReceipt::verifier(&trace).expect("verifier"),
        ];
        validate_backend_equivalence(&receipts).expect("backend parity");
        assert_eq!(
            trace
                .events
                .iter()
                .filter(|event| { matches!(event.kind, FlowEventKind::LeaseTransferred { .. }) })
                .count(),
            1
        );
    }

    #[test]
    fn verifier_rejects_forged_duplicate_terminal_and_stale_request_order() {
        let trace = trace_with_route_and_cancel();
        let mut duplicate = trace.clone();
        let terminal = duplicate
            .events
            .iter()
            .find(|event| matches!(event.kind, FlowEventKind::Completed { .. }))
            .copied()
            .expect("completed event");
        duplicate.events.push(terminal);
        for (index, event) in duplicate.events.iter_mut().enumerate() {
            event.sequence = index as u64 + 1;
        }
        assert!(validate_flow_trace(&duplicate).is_err());

        let mut forged = trace;
        forged
            .events
            .retain(|event| !matches!(event.kind, FlowEventKind::CancelRequested));
        for (index, event) in forged.events.iter_mut().enumerate() {
            event.sequence = index as u64 + 1;
        }
        assert!(validate_flow_trace(&forged).is_err());
    }
}
