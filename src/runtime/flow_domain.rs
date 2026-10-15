//! Explicit composition domain for independent Flow instances.
//!
//! This module is deliberately smaller than the Actor mailbox domain.  A Flow
//! handle is an opaque identity (`domain`, `instance`, `generation`) and owns
//! no execution thread.  The root owner keeps the domain alive; a lease is the
//! only authority to settle one instance.  Routing is an ordered list of
//! independent handles, never an implicit product state or transaction.
//!
//! Cancellation is also split into the four observable steps required by the
//! 0.2 contract: request, acknowledgement, explicit cleanup transfer, and
//! terminal settlement.  A timeout or an unresponsive participant leaves the
//! request pending.  It cannot manufacture a cancelled terminal state.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

/// Stable schema for the event stream shared by Flow consumers.
pub const FLOW_DOMAIN_SCHEMA: &str = "mimi-flow-domain-v1";

static NEXT_DOMAIN: AtomicU64 = AtomicU64::new(1);

fn allocate(counter: &AtomicU64) -> Result<u64, FlowDomainError> {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            value.checked_add(1).filter(|next| *next != 0)
        })
        .map_err(|_| FlowDomainError::IdentityExhausted)
}

fn allocate_local(value: &mut u64) -> Result<u64, FlowDomainError> {
    let current = *value;
    *value = current
        .checked_add(1)
        .filter(|next| *next != 0)
        .ok_or(FlowDomainError::IdentityExhausted)?;
    Ok(current)
}

/// Errors are typed so stale identities cannot be confused with an ordinary
/// rejected route or an unresolved cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowDomainError {
    Closed,
    InvalidHandle,
    StaleGeneration,
    WrongDomain,
    LeaseUnavailable,
    NotLeaseOwner,
    LeaseAlreadySettled,
    InvalidRequest,
    RequestAlreadySettled,
    CancellationPending,
    CancellationNotAcknowledged,
    CancellationAlreadyAcknowledged,
    InvalidTransfer,
    RouteRejected,
    OutstandingLease,
    UnresolvedCancellation,
    AlreadyTerminal,
    IdentityExhausted,
    FaultInjected(i32),
}

pub const FLOW_STATUS_OK: i32 = 0;
pub const FLOW_STATUS_CLOSED: i32 = -1;
pub const FLOW_STATUS_STALE: i32 = -2;
pub const FLOW_STATUS_WRONG_DOMAIN: i32 = -3;
pub const FLOW_STATUS_LEASE_UNAVAILABLE: i32 = -4;
pub const FLOW_STATUS_NOT_LEASE_OWNER: i32 = -5;
pub const FLOW_STATUS_INVALID_REQUEST: i32 = -6;
pub const FLOW_STATUS_PENDING: i32 = -7;
pub const FLOW_STATUS_ALREADY_TERMINAL: i32 = -8;
pub const FLOW_STATUS_ROUTE_REJECTED: i32 = -9;
pub const FLOW_STATUS_OUTSTANDING: i32 = -10;
pub const FLOW_STATUS_UNRESOLVED: i32 = -11;
pub const FLOW_STATUS_IDENTITY_EXHAUSTED: i32 = -12;

impl FlowDomainError {
    pub const fn status(self) -> i32 {
        match self {
            Self::Closed => FLOW_STATUS_CLOSED,
            Self::InvalidHandle | Self::InvalidRequest => FLOW_STATUS_INVALID_REQUEST,
            Self::StaleGeneration => FLOW_STATUS_STALE,
            Self::WrongDomain | Self::InvalidTransfer => FLOW_STATUS_WRONG_DOMAIN,
            Self::LeaseUnavailable | Self::LeaseAlreadySettled => FLOW_STATUS_LEASE_UNAVAILABLE,
            Self::NotLeaseOwner => FLOW_STATUS_NOT_LEASE_OWNER,
            Self::CancellationPending | Self::CancellationNotAcknowledged => FLOW_STATUS_PENDING,
            Self::CancellationAlreadyAcknowledged | Self::RequestAlreadySettled => {
                FLOW_STATUS_ALREADY_TERMINAL
            }
            Self::RouteRejected => FLOW_STATUS_ROUTE_REJECTED,
            Self::OutstandingLease => FLOW_STATUS_OUTSTANDING,
            Self::UnresolvedCancellation => FLOW_STATUS_UNRESOLVED,
            Self::AlreadyTerminal | Self::FaultInjected(_) => FLOW_STATUS_ALREADY_TERMINAL,
            Self::IdentityExhausted => FLOW_STATUS_IDENTITY_EXHAUSTED,
        }
    }
}

/// The role attached to a Flow owner.  Roles are audit metadata; they do not
/// create a hidden state product or grant access without the matching lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FlowRole {
    Root,
    Task(u64),
    Actor(u64),
    Native(u64),
    Reference(u64),
    Verifier(u64),
}

/// Stable participant identity carried in event receipts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FlowParticipantId {
    domain: u64,
    id: u64,
    role: FlowRole,
}

impl FlowParticipantId {
    pub const fn domain(self) -> u64 {
        self.domain
    }

    pub const fn id(self) -> u64 {
        self.id
    }

    pub const fn role(self) -> FlowRole {
        self.role
    }
}

/// Opaque Flow instance identity.  The generation is checked on every
/// operation, so a handle from a prior recover/reopen can never alias a new
/// instance in the same slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FlowHandle {
    domain: u64,
    instance: u64,
    generation: u64,
}

impl FlowHandle {
    pub const fn domain(self) -> u64 {
        self.domain
    }

    pub const fn instance(self) -> u64 {
        self.instance
    }

    pub const fn generation(self) -> u64 {
        self.generation
    }
}

/// Per-instance terminal state.  `Pending` is a cancellation request state,
/// not a terminal state, and therefore does not appear here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FlowTerminal {
    Completed(i64),
    Failed(i32),
    Cancelled,
    Fault(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FlowState {
    Open,
    Terminal(FlowTerminal),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FlowCancelState {
    Pending,
    Acknowledged,
    Transferred {
        from: FlowParticipantId,
        to: FlowParticipantId,
    },
    Settled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FlowEventKind {
    Open {
        payload: i64,
    },
    LeaseAcquired,
    LeaseReleased,
    LeaseTransferred {
        from: FlowParticipantId,
        to: FlowParticipantId,
    },
    Route {
        ordinal: u32,
    },
    CancelRequested,
    CancelAcknowledged,
    CancelTransferred {
        from: FlowParticipantId,
        to: FlowParticipantId,
    },
    CancelPending,
    Completed {
        value: i64,
    },
    Failed {
        code: i32,
    },
    Cancelled,
    FaultInjected {
        code: i32,
    },
    Closed,
}

/// Every event carries all identities needed to replay/check it.  Consumers
/// must compare the sequence and fields, not backend-local pointer values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FlowEvent {
    pub sequence: u64,
    pub domain: u64,
    pub handle: FlowHandle,
    pub lease: Option<u64>,
    pub request: Option<u64>,
    pub actor: FlowParticipantId,
    pub kind: FlowEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowTrace {
    pub schema: &'static str,
    pub events: Vec<FlowEvent>,
}

impl FlowTrace {
    pub fn canonical_text(&self) -> String {
        let mut text = String::from(FLOW_DOMAIN_SCHEMA);
        for event in &self.events {
            write!(
                text,
                "\n{}:{}:{}:{}:{}:{}:{:?}:{:?}",
                event.sequence,
                event.domain,
                event.handle.instance,
                event.handle.generation,
                event.lease.unwrap_or(0),
                event.request.unwrap_or(0),
                event.actor,
                event.kind
            )
            .expect("String formatting cannot fail");
        }
        text
    }

    /// Stable, dependency-free FNV-1a digest.  The route receipt separately
    /// carries the stronger MIR digest; runtime/native builds must not depend
    /// on a cargo-only hashing crate.
    pub fn digest(&self) -> String {
        let mut hash = 0xcbf29ce484222325_u64;
        for byte in self.canonical_text().bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        format!("{hash:016x}")
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != FLOW_DOMAIN_SCHEMA {
            return Err("Flow trace schema mismatch".into());
        }
        let mut expected = 1_u64;
        for event in &self.events {
            if event.sequence != expected {
                return Err(format!(
                    "Flow trace sequence gap: expected {}, got {}",
                    expected, event.sequence
                ));
            }
            expected = expected
                .checked_add(1)
                .ok_or_else(|| "Flow trace sequence overflow".to_string())?;
            if event.domain != event.handle.domain || event.domain != event.actor.domain() {
                return Err("Flow trace event has mismatched domain identity".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LeaseState {
    id: u64,
    owner: FlowParticipantId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CancelRecord {
    id: u64,
    requester: FlowParticipantId,
    state: FlowCancelState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FlowInstance {
    generation: u64,
    payload: i64,
    owner: FlowParticipantId,
    state: FlowState,
    lease: Option<LeaseState>,
    cancel: Option<CancelRecord>,
}

#[derive(Debug)]
struct DomainState {
    domain: u64,
    open: bool,
    next_instance: u64,
    next_lease: u64,
    next_request: u64,
    next_participant: u64,
    next_sequence: u64,
    instances: BTreeMap<u64, FlowInstance>,
    participants: BTreeSet<FlowParticipantId>,
    events: Vec<FlowEvent>,
}

impl DomainState {
    fn new(root: FlowParticipantId) -> Self {
        let mut participants = BTreeSet::new();
        participants.insert(root);
        Self {
            domain: root.domain,
            open: true,
            next_instance: 1,
            next_lease: 1,
            next_request: 1,
            next_participant: 1,
            next_sequence: 1,
            instances: BTreeMap::new(),
            participants,
            events: Vec::new(),
        }
    }
}

struct DomainInner {
    id: u64,
    state: Mutex<DomainState>,
}

impl DomainInner {
    fn lock(&self) -> Result<MutexGuard<'_, DomainState>, FlowDomainError> {
        self.state.lock().map_err(|_| FlowDomainError::Closed)
    }
}

/// A participant has no close/join obligation.  The root owner remains the
/// only object that can close the domain.
#[derive(Clone)]
pub struct FlowParticipant {
    domain: Weak<DomainInner>,
    identity: FlowParticipantId,
}

impl FlowParticipant {
    pub const fn identity(&self) -> FlowParticipantId {
        self.identity
    }

    pub fn is_alive(&self) -> bool {
        self.domain
            .upgrade()
            .and_then(|domain| domain.lock().ok().map(|state| state.open))
            .unwrap_or(false)
    }

    pub fn open_flow(&self, payload: i64) -> Result<FlowHandle, FlowDomainError> {
        let domain = self.domain.upgrade().ok_or(FlowDomainError::Closed)?;
        let mut state = domain.lock()?;
        ensure_participant(&state, self.identity)?;
        if !state.open {
            return Err(FlowDomainError::Closed);
        }
        let instance = allocate_local(&mut state.next_instance)?;
        let handle = FlowHandle {
            domain: domain.id,
            instance,
            generation: 1,
        };
        state.instances.insert(
            instance,
            FlowInstance {
                generation: 1,
                payload,
                owner: self.identity,
                state: FlowState::Open,
                lease: None,
                cancel: None,
            },
        );
        emit(
            &mut state,
            handle,
            None,
            None,
            self.identity,
            FlowEventKind::Open { payload },
        )?;
        Ok(handle)
    }

    pub fn acquire_lease(&self, handle: FlowHandle) -> Result<FlowLease, FlowDomainError> {
        let domain = self.domain.upgrade().ok_or(FlowDomainError::Closed)?;
        let mut state = domain.lock()?;
        ensure_participant(&state, self.identity)?;
        checked_instance(&state, handle)?;
        let instance = state
            .instances
            .get(&handle.instance)
            .ok_or(FlowDomainError::InvalidHandle)?;
        if !matches!(instance.state, FlowState::Open) {
            return Err(FlowDomainError::AlreadyTerminal);
        }
        if instance.owner != self.identity {
            return Err(FlowDomainError::NotLeaseOwner);
        }
        if instance.lease.is_some() {
            return Err(FlowDomainError::LeaseUnavailable);
        }
        let lease_id = allocate_local(&mut state.next_lease)?;
        state
            .instances
            .get_mut(&handle.instance)
            .expect("checked Flow instance remains present")
            .lease = Some(LeaseState {
            id: lease_id,
            owner: self.identity,
        });
        emit(
            &mut state,
            handle,
            Some(lease_id),
            None,
            self.identity,
            FlowEventKind::LeaseAcquired,
        )?;
        Ok(FlowLease {
            domain: Arc::downgrade(&domain),
            handle,
            id: lease_id,
            owner: self.identity,
            active: true,
        })
    }

    pub fn request_cancel(&self, handle: FlowHandle) -> Result<FlowCancelTicket, FlowDomainError> {
        let domain = self.domain.upgrade().ok_or(FlowDomainError::Closed)?;
        let mut state = domain.lock()?;
        ensure_participant(&state, self.identity)?;
        let instance = checked_instance(&state, handle)?;
        if !matches!(instance.state, FlowState::Open) {
            return Err(FlowDomainError::AlreadyTerminal);
        }
        if let Some(cancel) = instance.cancel {
            if !matches!(cancel.state, FlowCancelState::Settled) {
                return Err(FlowDomainError::RequestAlreadySettled);
            }
        }
        let lease_id = instance.lease.map(|lease| lease.id);
        let _ = instance;
        let request_id = allocate_local(&mut state.next_request)?;
        state
            .instances
            .get_mut(&handle.instance)
            .expect("checked Flow instance remains present")
            .cancel = Some(CancelRecord {
            id: request_id,
            requester: self.identity,
            state: FlowCancelState::Pending,
        });
        emit(
            &mut state,
            handle,
            lease_id,
            Some(request_id),
            self.identity,
            FlowEventKind::CancelRequested,
        )?;
        emit(
            &mut state,
            handle,
            lease_id,
            Some(request_id),
            self.identity,
            FlowEventKind::CancelPending,
        )?;
        Ok(FlowCancelTicket {
            domain: Arc::downgrade(&domain),
            handle,
            id: request_id,
            requester: self.identity,
        })
    }

    /// Record an explicit routing decision.  It observes handles in the
    /// supplied order and performs no state transition or rollback.
    pub fn route(&self, route: &FlowRoute) -> Result<Vec<FlowRouteReceipt>, FlowDomainError> {
        let domain = self.domain.upgrade().ok_or(FlowDomainError::Closed)?;
        if route.domain != domain.id {
            return Err(FlowDomainError::WrongDomain);
        }
        let mut state = domain.lock()?;
        ensure_participant(&state, self.identity)?;
        if !state.open {
            return Err(FlowDomainError::Closed);
        }
        let mut receipts = Vec::with_capacity(route.handles.len());
        for (ordinal, handle) in route.handles.iter().copied().enumerate() {
            let instance = checked_instance(&state, handle)?;
            if !matches!(instance.state, FlowState::Open) {
                return Err(FlowDomainError::RouteRejected);
            }
            if route
                .handles
                .iter()
                .take(ordinal)
                .any(|previous| previous.instance == handle.instance)
            {
                return Err(FlowDomainError::RouteRejected);
            }
            emit(
                &mut state,
                handle,
                None,
                None,
                self.identity,
                FlowEventKind::Route {
                    ordinal: ordinal as u32,
                },
            )?;
            receipts.push(FlowRouteReceipt {
                handle,
                ordinal: ordinal as u32,
            });
        }
        Ok(receipts)
    }

    /// Return the current status without changing ownership or terminal state.
    pub fn inspect(&self, handle: FlowHandle) -> Result<FlowObservation, FlowDomainError> {
        let domain = self.domain.upgrade().ok_or(FlowDomainError::Closed)?;
        let state = domain.lock()?;
        ensure_participant(&state, self.identity)?;
        let instance = checked_instance(&state, handle)?;
        Ok(FlowObservation {
            handle,
            payload: instance.payload,
            owner: instance.owner,
            state: instance.state,
            lease: instance.lease.map(|lease| lease.id),
            lease_owner: instance.lease.map(|lease| lease.owner),
            cancel: instance.cancel.map(|cancel| (cancel.id, cancel.state)),
        })
    }

    /// Mark a request as still unresolved after a bounded wait.  This adds an
    /// event for the oracle while preserving the request's `Pending` state.
    pub fn observe_pending(
        &self,
        ticket: &FlowCancelTicket,
    ) -> Result<FlowCancelStatus, FlowDomainError> {
        ticket.observe_with(self)
    }

    /// Create a task/actor participant for explicit cleanup transfer.
    pub fn spawn_participant(&self, role: FlowRole) -> Result<FlowParticipant, FlowDomainError> {
        let domain = self.domain.upgrade().ok_or(FlowDomainError::Closed)?;
        let mut state = domain.lock()?;
        ensure_participant(&state, self.identity)?;
        if !state.open {
            return Err(FlowDomainError::Closed);
        }
        let id = allocate_local(&mut state.next_participant)?;
        let identity = FlowParticipantId {
            domain: domain.id,
            id,
            role,
        };
        state.participants.insert(identity);
        Ok(FlowParticipant {
            domain: Arc::downgrade(&domain),
            identity,
        })
    }

    /// Snapshot the immutable event account.  Taking a snapshot does not
    /// release leases or settle pending requests.
    pub fn trace(&self) -> Result<FlowTrace, FlowDomainError> {
        let domain = self.domain.upgrade().ok_or(FlowDomainError::Closed)?;
        let state = domain.lock()?;
        ensure_participant(&state, self.identity)?;
        Ok(FlowTrace {
            schema: FLOW_DOMAIN_SCHEMA,
            events: state.events.clone(),
        })
    }
}

fn ensure_participant(
    state: &DomainState,
    participant: FlowParticipantId,
) -> Result<(), FlowDomainError> {
    if state.participants.contains(&participant) {
        Ok(())
    } else {
        Err(FlowDomainError::NotLeaseOwner)
    }
}

fn checked_instance<'a>(
    state: &'a DomainState,
    handle: FlowHandle,
) -> Result<&'a FlowInstance, FlowDomainError> {
    if handle.domain != state.domain {
        return Err(FlowDomainError::WrongDomain);
    }
    if handle.domain != 0
        && state
            .instances
            .get(&handle.instance)
            .is_some_and(|instance| instance.generation == handle.generation)
    {
        state
            .instances
            .get(&handle.instance)
            .ok_or(FlowDomainError::InvalidHandle)
    } else if state.instances.contains_key(&handle.instance) {
        Err(FlowDomainError::StaleGeneration)
    } else {
        Err(FlowDomainError::InvalidHandle)
    }
}

fn checked_instance_mut<'a>(
    state: &'a mut DomainState,
    handle: FlowHandle,
) -> Result<&'a mut FlowInstance, FlowDomainError> {
    if handle.domain != state.domain {
        return Err(FlowDomainError::WrongDomain);
    }
    let Some(instance) = state.instances.get_mut(&handle.instance) else {
        return Err(FlowDomainError::InvalidHandle);
    };
    if instance.generation != handle.generation {
        return Err(FlowDomainError::StaleGeneration);
    }
    Ok(instance)
}

fn emit(
    state: &mut DomainState,
    handle: FlowHandle,
    lease: Option<u64>,
    request: Option<u64>,
    actor: FlowParticipantId,
    kind: FlowEventKind,
) -> Result<u64, FlowDomainError> {
    let sequence = allocate_local(&mut state.next_sequence)?;
    state.events.push(FlowEvent {
        sequence,
        domain: handle.domain,
        handle,
        lease,
        request,
        actor,
        kind,
    });
    Ok(sequence)
}

/// Move-only authority for one open Flow instance.
pub struct FlowLease {
    domain: Weak<DomainInner>,
    handle: FlowHandle,
    id: u64,
    owner: FlowParticipantId,
    active: bool,
}

impl FlowLease {
    pub const fn id(&self) -> u64 {
        self.id
    }

    pub const fn handle(&self) -> FlowHandle {
        self.handle
    }

    pub const fn owner(&self) -> FlowParticipantId {
        self.owner
    }

    pub const fn is_active(&self) -> bool {
        self.active
    }

    fn check<'a>(
        &self,
        state: &'a mut DomainState,
    ) -> Result<&'a mut FlowInstance, FlowDomainError> {
        if !self.active {
            return Err(FlowDomainError::LeaseAlreadySettled);
        }
        let instance = checked_instance_mut(state, self.handle)?;
        if instance.lease
            != Some(LeaseState {
                id: self.id,
                owner: self.owner,
            })
        {
            return Err(FlowDomainError::NotLeaseOwner);
        }
        Ok(instance)
    }

    /// Explicitly release an observation lease while leaving the Flow open.
    pub fn release(mut self) -> Result<(), FlowDomainError> {
        let domain = self.domain.upgrade().ok_or(FlowDomainError::Closed)?;
        let mut state = domain.lock()?;
        let instance = self.check(&mut state)?;
        instance.lease = None;
        let _ = instance;
        emit(
            &mut state,
            self.handle,
            Some(self.id),
            None,
            self.owner,
            FlowEventKind::LeaseReleased,
        )?;
        self.active = false;
        Ok(())
    }

    /// Transfer the current lease to an explicit participant.  The original
    /// lease is consumed; a stale alias cannot release it afterward.
    pub fn transfer(mut self, recipient: &FlowParticipant) -> Result<Self, FlowDomainError> {
        let domain = self.domain.upgrade().ok_or(FlowDomainError::Closed)?;
        if !Weak::ptr_eq(&self.domain, &recipient.domain) {
            return Err(FlowDomainError::WrongDomain);
        }
        let mut state = domain.lock()?;
        ensure_participant(&state, recipient.identity)?;
        let instance = self.check(&mut state)?;
        instance.lease = Some(LeaseState {
            id: self.id,
            owner: recipient.identity,
        });
        instance.owner = recipient.identity;
        let from = self.owner;
        let handle = self.handle;
        let lease_id = self.id;
        let _ = instance;
        emit(
            &mut state,
            handle,
            Some(lease_id),
            None,
            from,
            FlowEventKind::LeaseTransferred {
                from,
                to: recipient.identity,
            },
        )?;
        self.active = false;
        Ok(Self {
            domain: Arc::downgrade(&domain),
            handle: self.handle,
            id: self.id,
            owner: recipient.identity,
            active: true,
        })
    }

    /// Acknowledge a pending cancel request.  The request does not consume the
    /// lease; only explicit `cancel` can settle the Flow.
    pub fn acknowledge_cancel(&mut self, ticket: &FlowCancelTicket) -> Result<(), FlowDomainError> {
        let domain = self.domain.upgrade().ok_or(FlowDomainError::Closed)?;
        if !Weak::ptr_eq(&self.domain, &ticket.domain) {
            return Err(FlowDomainError::WrongDomain);
        }
        let mut state = domain.lock()?;
        let instance = self.check(&mut state)?;
        {
            let cancel = instance
                .cancel
                .as_mut()
                .ok_or(FlowDomainError::InvalidRequest)?;
            if cancel.id != ticket.id || ticket.handle != self.handle {
                return Err(FlowDomainError::InvalidRequest);
            }
            match cancel.state {
                FlowCancelState::Pending => cancel.state = FlowCancelState::Acknowledged,
                FlowCancelState::Acknowledged | FlowCancelState::Transferred { .. } => {
                    return Err(FlowDomainError::CancellationAlreadyAcknowledged)
                }
                FlowCancelState::Settled => return Err(FlowDomainError::RequestAlreadySettled),
            }
        }
        let _ = instance;
        emit(
            &mut state,
            self.handle,
            Some(self.id),
            Some(ticket.id),
            self.owner,
            FlowEventKind::CancelAcknowledged,
        )?;
        Ok(())
    }

    /// Transfer cleanup responsibility for a cancel request.  This is an
    /// explicit lease transfer, so the source cannot settle twice.  The
    /// source is borrowed while validating the ticket and remains active on
    /// error; successful transfer consumes its authority in place.
    pub fn transfer_cancel(
        &mut self,
        ticket: &FlowCancelTicket,
        recipient: &FlowParticipant,
    ) -> Result<Self, FlowDomainError> {
        if ticket.handle != self.handle {
            return Err(FlowDomainError::InvalidRequest);
        }
        // Validate the request before changing the lease owner.  The old
        // implementation called `transfer` first and only then checked the
        // ticket.  A stale/wrong ticket therefore returned an error after
        // mutating the instance owner, leaving an active lease with no
        // usable source authority.  Keep both lease and cancellation updates
        // under one lock so a failed transfer is observationally atomic.
        let domain = self.domain.upgrade().ok_or(FlowDomainError::Closed)?;
        if !Weak::ptr_eq(&self.domain, &recipient.domain) {
            return Err(FlowDomainError::WrongDomain);
        }
        let mut state = domain.lock()?;
        ensure_participant(&state, recipient.identity)?;
        let instance = self.check(&mut state)?;
        let cancel = instance.cancel.ok_or(FlowDomainError::InvalidRequest)?;
        if cancel.id != ticket.id {
            return Err(FlowDomainError::InvalidRequest);
        }
        let from = match cancel.state {
            FlowCancelState::Pending | FlowCancelState::Acknowledged => cancel.requester,
            FlowCancelState::Transferred { .. } => return Err(FlowDomainError::InvalidTransfer),
            FlowCancelState::Settled => return Err(FlowDomainError::RequestAlreadySettled),
        };
        let handle = self.handle;
        let lease_id = self.id;
        let owner = self.owner;
        instance.lease = Some(LeaseState {
            id: lease_id,
            owner: recipient.identity,
        });
        instance.owner = recipient.identity;
        instance.cancel = Some(CancelRecord {
            id: cancel.id,
            requester: cancel.requester,
            state: FlowCancelState::Transferred {
                from,
                to: recipient.identity,
            },
        });
        let _ = instance;
        emit(
            &mut state,
            handle,
            Some(lease_id),
            None,
            owner,
            FlowEventKind::LeaseTransferred {
                from: owner,
                to: recipient.identity,
            },
        )?;
        emit(
            &mut state,
            handle,
            Some(lease_id),
            Some(ticket.id),
            recipient.identity,
            FlowEventKind::CancelTransferred {
                from,
                to: recipient.identity,
            },
        )?;
        self.active = false;
        Ok(Self {
            domain: Arc::downgrade(&domain),
            handle,
            id: lease_id,
            owner: recipient.identity,
            active: true,
        })
    }

    pub fn complete(mut self, value: i64) -> Result<FlowTerminal, FlowDomainError> {
        self.finish(
            FlowTerminal::Completed(value),
            FlowEventKind::Completed { value },
        )
    }

    pub fn fail(mut self, code: i32) -> Result<FlowTerminal, FlowDomainError> {
        self.finish(FlowTerminal::Failed(code), FlowEventKind::Failed { code })
    }

    pub fn fault(mut self, code: i32) -> Result<FlowTerminal, FlowDomainError> {
        let domain = self.domain.upgrade().ok_or(FlowDomainError::Closed)?;
        let mut state = domain.lock()?;
        let instance = self.check(&mut state)?;
        if !matches!(instance.state, FlowState::Open) {
            return Err(FlowDomainError::AlreadyTerminal);
        }
        // Fault injection models a host failure.  It settles an outstanding
        // cancel request as part of the same single terminal receipt; it does
        // not claim that an unresponsive remote task was stopped.
        if let Some(cancel) = instance.cancel.as_mut() {
            cancel.state = FlowCancelState::Settled;
        }
        instance.state = FlowState::Terminal(FlowTerminal::Fault(code));
        instance.lease = None;
        let _ = instance;
        emit(
            &mut state,
            self.handle,
            Some(self.id),
            None,
            self.owner,
            FlowEventKind::FaultInjected { code },
        )?;
        self.active = false;
        Ok(FlowTerminal::Fault(code))
    }

    pub fn cancel(mut self, ticket: &FlowCancelTicket) -> Result<FlowTerminal, FlowDomainError> {
        if ticket.handle != self.handle {
            return Err(FlowDomainError::InvalidRequest);
        }
        let domain = self.domain.upgrade().ok_or(FlowDomainError::Closed)?;
        let mut state = domain.lock()?;
        let instance = self.check(&mut state)?;
        let cancel = instance
            .cancel
            .as_mut()
            .ok_or(FlowDomainError::InvalidRequest)?;
        if cancel.id != ticket.id {
            return Err(FlowDomainError::InvalidRequest);
        }
        if !matches!(
            cancel.state,
            FlowCancelState::Acknowledged | FlowCancelState::Transferred { .. }
        ) {
            return Err(FlowDomainError::CancellationNotAcknowledged);
        }
        cancel.state = FlowCancelState::Settled;
        instance.state = FlowState::Terminal(FlowTerminal::Cancelled);
        instance.lease = None;
        let _ = instance;
        emit(
            &mut state,
            self.handle,
            Some(self.id),
            Some(ticket.id),
            self.owner,
            FlowEventKind::Cancelled,
        )?;
        self.active = false;
        Ok(FlowTerminal::Cancelled)
    }

    fn finish(
        &mut self,
        terminal: FlowTerminal,
        event: FlowEventKind,
    ) -> Result<FlowTerminal, FlowDomainError> {
        let domain = self.domain.upgrade().ok_or(FlowDomainError::Closed)?;
        let mut state = domain.lock()?;
        let instance = self.check(&mut state)?;
        if !matches!(instance.state, FlowState::Open) {
            return Err(FlowDomainError::AlreadyTerminal);
        }
        if instance
            .cancel
            .is_some_and(|cancel| !matches!(cancel.state, FlowCancelState::Settled))
        {
            return Err(FlowDomainError::CancellationPending);
        }
        instance.state = FlowState::Terminal(terminal);
        instance.lease = None;
        let _ = instance;
        emit(
            &mut state,
            self.handle,
            Some(self.id),
            None,
            self.owner,
            event,
        )?;
        self.active = false;
        Ok(terminal)
    }
}

impl Drop for FlowLease {
    fn drop(&mut self) {
        // Dropping an active lease intentionally does not settle it.  The
        // domain remains observable as `OutstandingLease`, preventing a Rust
        // destructor from silently becoming a business-protocol commit.
    }
}

/// Move-only cancellation request.  Waiting or dropping a ticket never
/// consumes the Flow lease and never fabricates a terminal state.
pub struct FlowCancelTicket {
    domain: Weak<DomainInner>,
    handle: FlowHandle,
    id: u64,
    requester: FlowParticipantId,
}

impl FlowCancelTicket {
    pub const fn id(&self) -> u64 {
        self.id
    }

    pub const fn handle(&self) -> FlowHandle {
        self.handle
    }

    pub const fn requester(&self) -> FlowParticipantId {
        self.requester
    }

    pub fn observe(&self) -> Result<FlowCancelStatus, FlowDomainError> {
        let domain = self.domain.upgrade().ok_or(FlowDomainError::Closed)?;
        let state = domain.lock()?;
        let instance = checked_instance(&state, self.handle)?;
        let cancel = instance.cancel.ok_or(FlowDomainError::InvalidRequest)?;
        if cancel.id != self.id {
            return Err(FlowDomainError::RequestAlreadySettled);
        }
        Ok(match cancel.state {
            FlowCancelState::Pending => FlowCancelStatus::Pending,
            FlowCancelState::Acknowledged => FlowCancelStatus::Acknowledged,
            FlowCancelState::Transferred { to, .. } => FlowCancelStatus::Transferred(to),
            FlowCancelState::Settled => FlowCancelStatus::Settled,
        })
    }

    fn observe_with(
        &self,
        observer: &FlowParticipant,
    ) -> Result<FlowCancelStatus, FlowDomainError> {
        if !Weak::ptr_eq(&self.domain, &observer.domain) {
            return Err(FlowDomainError::WrongDomain);
        }
        let status = self.observe()?;
        if status == FlowCancelStatus::Pending {
            let domain = self.domain.upgrade().ok_or(FlowDomainError::Closed)?;
            let mut state = domain.lock()?;
            ensure_participant(&state, observer.identity)?;
            emit(
                &mut state,
                self.handle,
                None,
                Some(self.id),
                observer.identity,
                FlowEventKind::CancelPending,
            )?;
        }
        Ok(status)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FlowCancelStatus {
    Pending,
    Acknowledged,
    Transferred(FlowParticipantId),
    Settled,
}

/// Immutable observation used by reference/VM/native tests and query tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlowObservation {
    pub handle: FlowHandle,
    pub payload: i64,
    pub owner: FlowParticipantId,
    pub state: FlowState,
    pub lease: Option<u64>,
    pub lease_owner: Option<FlowParticipantId>,
    pub cancel: Option<(u64, FlowCancelState)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlowRouteReceipt {
    pub handle: FlowHandle,
    pub ordinal: u32,
}

/// Ordered explicit router. Duplicate handles are rejected during `route`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowRoute {
    domain: u64,
    handles: Vec<FlowHandle>,
}

impl FlowRoute {
    pub fn new(handles: Vec<FlowHandle>) -> Result<Self, FlowDomainError> {
        if handles.is_empty() || handles.iter().any(|handle| handle.domain == 0) {
            return Err(FlowDomainError::RouteRejected);
        }
        let domain = handles[0].domain;
        if handles.iter().any(|handle| handle.domain != domain) {
            return Err(FlowDomainError::WrongDomain);
        }
        if handles
            .windows(2)
            .any(|pair| pair[0].instance == pair[1].instance)
        {
            return Err(FlowDomainError::RouteRejected);
        }
        Ok(Self { domain, handles })
    }

    pub fn handles(&self) -> &[FlowHandle] {
        &self.handles
    }

    pub const fn domain(&self) -> u64 {
        self.domain
    }
}

/// Unique root owner.  `Rc` makes accidental cross-thread movement a compile
/// error; participants and handles are the only values intended to cross a
/// task boundary.
pub struct FlowDomainOwner {
    domain: Arc<DomainInner>,
    root: FlowParticipant,
    _root_thread: PhantomData<Rc<()>>,
}

impl FlowDomainOwner {
    pub fn new() -> Result<Self, FlowDomainError> {
        let domain_id = allocate(&NEXT_DOMAIN)?;
        let root_id = FlowParticipantId {
            domain: domain_id,
            id: 0,
            role: FlowRole::Root,
        };
        let domain = Arc::new(DomainInner {
            id: domain_id,
            state: Mutex::new(DomainState::new(root_id)),
        });
        let root = FlowParticipant {
            domain: Arc::downgrade(&domain),
            identity: root_id,
        };
        Ok(Self {
            domain,
            root,
            _root_thread: PhantomData,
        })
    }

    pub fn id(&self) -> u64 {
        self.domain.id
    }

    pub fn participant(&self) -> FlowParticipant {
        self.root.clone()
    }

    pub fn trace(&self) -> FlowTrace {
        self.root.trace().expect("root participant stays valid")
    }

    pub fn close(&mut self) -> Result<FlowTrace, FlowDomainError> {
        let mut state = self.domain.lock()?;
        if !state.open {
            return Ok(FlowTrace {
                schema: FLOW_DOMAIN_SCHEMA,
                events: state.events.clone(),
            });
        }
        if state
            .instances
            .values()
            .any(|instance| instance.lease.is_some())
        {
            return Err(FlowDomainError::OutstandingLease);
        }
        if state.instances.values().any(|instance| {
            matches!(instance.state, FlowState::Open)
                && instance
                    .cancel
                    .is_some_and(|cancel| !matches!(cancel.state, FlowCancelState::Settled))
        }) {
            return Err(FlowDomainError::UnresolvedCancellation);
        }
        let handles = state
            .instances
            .iter()
            .map(|(instance, flow)| {
                (
                    *instance,
                    FlowHandle {
                        domain: self.domain.id,
                        instance: *instance,
                        generation: flow.generation,
                    },
                    flow.state,
                )
            })
            .collect::<Vec<_>>();
        for (_instance, handle, flow_state) in handles {
            if matches!(flow_state, FlowState::Open) {
                return Err(FlowDomainError::UnresolvedCancellation);
            }
            emit(
                &mut state,
                handle,
                None,
                None,
                self.root.identity,
                FlowEventKind::Closed,
            )?;
        }
        state.open = false;
        Ok(FlowTrace {
            schema: FLOW_DOMAIN_SCHEMA,
            events: state.events.clone(),
        })
    }
}

impl Drop for FlowDomainOwner {
    fn drop(&mut self) {
        // Do not synthesize cancellation or terminal receipts in Drop.  The
        // domain is only made inaccessible; a trace captured before drop still
        // records the unresolved lease/request for diagnostics.
        if let Ok(mut state) = self.domain.lock() {
            state.open = false;
        }
    }
}

/// Small native ABI bridge.  It intentionally exposes only identity and
/// terminal status; the explicit Rust API remains the source of the full
/// lease/route contract.
#[no_mangle]
pub extern "C" fn mimi_flow_domain_open() -> u64 {
    let Ok(owner) = FlowDomainOwner::new() else {
        return 0;
    };
    let id = owner.id();
    std::mem::forget(owner);
    id
}

/// Error status for a native call where the domain is not retained by an
/// embedding owner. Native code should use the canonical actor domain bridge
/// for thread ownership; this status-only probe is kept for trace litmus tests.
#[no_mangle]
pub extern "C" fn mimi_flow_domain_status(error: i32) -> i32 {
    error
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settle_all(owner: &FlowDomainOwner, flows: &[FlowHandle]) {
        let participant = owner.participant();
        for (value, handle) in flows.iter().copied().enumerate() {
            let lease = participant.acquire_lease(handle).expect("lease");
            lease.complete(value as i64).expect("complete");
        }
    }

    #[test]
    fn independent_flow_routes_cover_two_four_and_eight_instances() {
        for count in [2_usize, 4, 8] {
            let owner = FlowDomainOwner::new().expect("domain");
            let participant = owner.participant();
            let flows = (0..count)
                .map(|payload| participant.open_flow(payload as i64).expect("flow"))
                .collect::<Vec<_>>();
            let route = FlowRoute::new(flows.clone()).expect("ordered route");
            let receipts = participant.route(&route).expect("route");
            assert_eq!(receipts.len(), count);
            assert_eq!(receipts.first().expect("first").ordinal, 0);
            assert_eq!(receipts.last().expect("last").ordinal, (count - 1) as u32);
            settle_all(&owner, &flows);
            let mut owner = owner;
            let trace = owner.close().expect("closed route");
            trace.validate().expect("valid trace");
            assert!(trace
                .events
                .iter()
                .any(|event| { matches!(event.kind, FlowEventKind::Route { ordinal: 0 }) }));
        }
    }

    #[test]
    fn lease_transfer_and_stale_generation_are_explicit() {
        let mut owner = FlowDomainOwner::new().expect("domain");
        let root = owner.participant();
        let worker = root
            .spawn_participant(FlowRole::Task(1))
            .expect("worker participant");
        let flow = root.open_flow(9).expect("flow");
        assert!(matches!(
            worker.acquire_lease(flow),
            Err(FlowDomainError::NotLeaseOwner)
        ));
        let lease = root.acquire_lease(flow).expect("root lease");
        assert!(matches!(
            root.acquire_lease(flow),
            Err(FlowDomainError::LeaseUnavailable)
        ));
        let lease = lease.transfer(&worker).expect("transfer");
        assert_eq!(lease.owner().role(), FlowRole::Task(1));
        lease.complete(10).expect("worker settles");
        let stale = FlowHandle {
            generation: flow.generation() + 1,
            ..flow
        };
        assert_eq!(root.inspect(stale), Err(FlowDomainError::StaleGeneration));
        assert!(owner.close().is_ok());
    }

    #[test]
    fn cancel_request_ack_transfer_and_settle_has_one_terminal_event() {
        let mut owner = FlowDomainOwner::new().expect("domain");
        let root = owner.participant();
        let worker = root
            .spawn_participant(FlowRole::Actor(7))
            .expect("actor participant");
        let flow = root.open_flow(3).expect("flow");
        let mut lease = root.acquire_lease(flow).expect("lease");
        let request = root.request_cancel(flow).expect("request");
        assert_eq!(request.observe(), Ok(FlowCancelStatus::Pending));
        assert_eq!(
            root.observe_pending(&request),
            Ok(FlowCancelStatus::Pending)
        );
        let mut lease = lease
            .transfer_cancel(&request, &worker)
            .expect("cleanup transfer");
        assert_eq!(
            request.observe(),
            Ok(FlowCancelStatus::Transferred(worker.identity()))
        );
        lease
            .acknowledge_cancel(&request)
            .expect_err("already transferred");
        lease.cancel(&request).expect("cancel settle");
        assert_eq!(request.observe(), Ok(FlowCancelStatus::Settled));
        assert!(matches!(
            root.acquire_lease(flow),
            Err(FlowDomainError::AlreadyTerminal)
        ));
        let trace = owner.trace();
        assert_eq!(
            trace
                .events
                .iter()
                .filter(|event| matches!(event.kind, FlowEventKind::Cancelled))
                .count(),
            1
        );
        assert!(owner.close().is_ok());
    }

    #[test]
    fn unresponsive_cancel_remains_pending_and_fault_is_single_terminal() {
        let mut owner = FlowDomainOwner::new().expect("domain");
        let root = owner.participant();
        let flow = root.open_flow(1).expect("flow");
        let ticket = root.request_cancel(flow).expect("request");
        assert_eq!(root.observe_pending(&ticket), Ok(FlowCancelStatus::Pending));
        assert_eq!(
            owner.close(),
            Err(FlowDomainError::UnresolvedCancellation),
            "timeout must not invent a cancelled terminal"
        );
        let lease = root.acquire_lease(flow).expect("lease remains owned");
        lease.fault(801).expect("fault injection");
        let trace = owner.trace();
        assert_eq!(
            trace
                .events
                .iter()
                .filter(|event| matches!(event.kind, FlowEventKind::FaultInjected { .. }))
                .count(),
            1
        );
        assert!(matches!(
            root.acquire_lease(flow),
            Err(FlowDomainError::AlreadyTerminal)
        ));
    }

    #[test]
    fn duplicate_route_and_double_settle_fail_closed() {
        let owner = FlowDomainOwner::new().expect("domain");
        let root = owner.participant();
        let flow = root.open_flow(0).expect("flow");
        assert_eq!(
            FlowRoute::new(vec![flow, flow]),
            Err(FlowDomainError::RouteRejected)
        );
        let lease = root.acquire_lease(flow).expect("lease");
        lease.complete(1).expect("first settle");
        assert!(matches!(
            root.acquire_lease(flow),
            Err(FlowDomainError::AlreadyTerminal)
        ));
        let trace = owner.trace();
        assert_eq!(
            trace
                .events
                .iter()
                .filter(|event| matches!(event.kind, FlowEventKind::Completed { .. }))
                .count(),
            1
        );
    }
}
