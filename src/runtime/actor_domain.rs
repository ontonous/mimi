//! Execution-owned mailbox domain for canonical scalar Actors.
//!
//! The root alone owns thread joins. Handles and execution participants are
//! weak capabilities; neither a worker, a reply nor a language handle owns a
//! mailbox sender or keeps the root alive. Legacy Actor runtime entry points
//! remain separate. Native dispatch returns explicit status rather than
//! unwinding or aborting through the C boundary.

use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak};
use std::thread::JoinHandle;

static NEXT_DOMAIN: AtomicU64 = AtomicU64::new(1);
static NEXT_ACTOR: AtomicU64 = AtomicU64::new(1);
static NEXT_TASK: AtomicU64 = AtomicU64::new(1);

/// MAX is a permanent exhausted sentinel. No allocation can wrap to zero or
/// reuse an issued identity, including after an exhausted allocation fails.
fn allocate_identity(next: &AtomicU64) -> Result<u64, ActorDomainError> {
    next.fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
        value.checked_add(1).filter(|_| value != 0)
    })
    .map_err(|_| ActorDomainError::IdentityExhausted)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorDomainError {
    Closed,
    Cancelled,
    QuotaExceeded,
    SpawnFailed,
    UnknownActor,
    InvalidPayload,
    WorkerPanicked,
    WorkerCannotClose,
    InvalidParticipant,
    IdentityExhausted,
    Poisoned,
    Trap(i32),
}

pub type ActorDomainReply = Result<i32, ActorDomainError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorDomainCloseMode {
    Drain,
    Cancel,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(C)]
pub struct ActorDomainCounts {
    pub live_roots: u64,
    pub live_workers: u64,
    pub live_states: u64,
    pub live_controls: u64,
    pub live_tasks: u64,
    pub pending_replies: u64,
    pub live_message_buffers: u64,
    pub live_reply_buffers: u64,
    pub spawned_actors: u64,
    pub accepted_messages: u64,
    pub settled_replies: u64,
    pub cancelled_replies: u64,
    pub joined_workers: u64,
    pub joined_tasks: u64,
}

#[derive(Default)]
struct Counters {
    live_roots: AtomicU64,
    live_workers: AtomicU64,
    live_states: AtomicU64,
    live_controls: AtomicU64,
    live_tasks: AtomicU64,
    pending_replies: AtomicU64,
    live_message_buffers: AtomicU64,
    live_reply_buffers: AtomicU64,
    spawned_actors: AtomicU64,
    accepted_messages: AtomicU64,
    settled_replies: AtomicU64,
    cancelled_replies: AtomicU64,
    joined_workers: AtomicU64,
    joined_tasks: AtomicU64,
}

impl Counters {
    fn snapshot(&self) -> ActorDomainCounts {
        ActorDomainCounts {
            live_roots: self.live_roots.load(Ordering::Acquire),
            live_workers: self.live_workers.load(Ordering::Acquire),
            live_states: self.live_states.load(Ordering::Acquire),
            live_controls: self.live_controls.load(Ordering::Acquire),
            live_tasks: self.live_tasks.load(Ordering::Acquire),
            pending_replies: self.pending_replies.load(Ordering::Acquire),
            live_message_buffers: self.live_message_buffers.load(Ordering::Acquire),
            live_reply_buffers: self.live_reply_buffers.load(Ordering::Acquire),
            spawned_actors: self.spawned_actors.load(Ordering::Acquire),
            accepted_messages: self.accepted_messages.load(Ordering::Acquire),
            settled_replies: self.settled_replies.load(Ordering::Acquire),
            cancelled_replies: self.cancelled_replies.load(Ordering::Acquire),
            joined_workers: self.joined_workers.load(Ordering::Acquire),
            joined_tasks: self.joined_tasks.load(Ordering::Acquire),
        }
    }
}

/// Counter observation owns no executable state, sender, domain or join.
#[derive(Clone)]
pub struct ActorDomainMonitor(Arc<Counters>);

impl ActorDomainMonitor {
    pub fn counts(&self) -> ActorDomainCounts {
        self.0.snapshot()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Open,
    SettlingChildren,
    ClosingActors,
    Closed,
}

#[derive(Clone, Copy)]
enum ParticipantRole {
    Root,
    Child(u64),
    Worker,
}

struct ActorControl {
    arities: Vec<usize>,
    sender: Option<mpsc::Sender<Command>>,
    worker: Option<JoinHandle<()>>,
}

struct Admission {
    phase: Phase,
    actors: BTreeMap<u64, ActorControl>,
    tasks: BTreeMap<u64, JoinHandle<Result<(), ActorDomainError>>>,
    active_children: BTreeSet<u64>,
    failure: Option<ActorDomainError>,
}

struct DomainInner {
    id: u64,
    quota: Option<usize>,
    admission: Mutex<Admission>,
    cancelled: Arc<AtomicBool>,
    counters: Arc<Counters>,
    #[cfg(test)]
    fail_next_spawn: AtomicBool,
}

impl DomainInner {
    fn cancel_locked(&self, admission: &mut Admission, failure: Option<ActorDomainError>) {
        if let Some(failure) = failure {
            admission.failure.get_or_insert(failure);
        }
        self.cancelled.store(true, Ordering::Release);
        for control in admission.actors.values_mut() {
            if let Some(sender) = control.sender.take() {
                // Cleanup may encounter a receiver already dropped by a
                // failed worker. Join still observes that worker's failure;
                // there is no unaccepted execution to recover here.
                let _ = sender.send(Command::Close);
            }
        }
    }

    fn work_admission(&self) -> Result<MutexGuard<'_, Admission>, ActorDomainError> {
        match self.admission.lock() {
            Ok(guard) => Ok(guard),
            Err(error) => {
                let mut guard = error.into_inner();
                self.cancel_locked(&mut guard, Some(ActorDomainError::Poisoned));
                Err(ActorDomainError::Poisoned)
            }
        }
    }

    /// Poison recovery is exclusively for cancellation, revocation and join
    /// cleanup. It never restores Open admission or executes a turn.
    fn cleanup_admission(&self) -> MutexGuard<'_, Admission> {
        match self.admission.lock() {
            Ok(guard) => guard,
            Err(error) => {
                let mut guard = error.into_inner();
                self.cancel_locked(&mut guard, Some(ActorDomainError::Poisoned));
                guard
            }
        }
    }
}

/// Execution participants can enqueue work within the root's lifetime. They
/// cannot join, close the root, change role or manufacture a new root owner.
#[derive(Clone)]
pub struct ActorDomainParticipant {
    domain: Weak<DomainInner>,
    role: ParticipantRole,
}

/// Copy semantics do not imply ownership of a thread or mailbox sender.
#[derive(Clone)]
pub struct ActorDomainHandle {
    domain: Weak<DomainInner>,
    id: u64,
}

impl ActorDomainHandle {
    pub fn identity(&self) -> u64 {
        self.id
    }
}

pub struct ActorTurnContext {
    participant: ActorDomainParticipant,
    actor: u64,
}

impl ActorTurnContext {
    pub fn actor_identity(&self) -> u64 {
        self.actor
    }

    pub fn is_cancelled(&self) -> bool {
        self.participant.is_cancelled()
    }

    /// A worker can request cancellation, but leaves the unique join
    /// obligation with the root. This also wakes idle receivers.
    pub fn request_cancel(&self) -> Result<(), ActorDomainError> {
        self.participant.request_cancel()
    }
}

struct ReplyTicket {
    slot: Option<Arc<ReplySlot>>,
    counters: Arc<Counters>,
}

struct ReplySlotState {
    receiver_alive: bool,
    settled: bool,
    poisoned: bool,
    reply: Option<SettledReply>,
}

/// Reply waits may run in generated C main, whose exit does not run the Rust
/// main-thread TLS destructor. std::mpsc's blocking receiver installs a Thread
/// context there, so replies use an execution-owned Mutex/Condvar slot instead.
/// Only mailbox workers receive through mpsc; Rust owns and joins those threads.
/// The failure participant is weak and owns no root, mailbox sender or join.
struct ReplySlot {
    state: Mutex<ReplySlotState>,
    ready: Condvar,
    failure_participant: ActorDomainParticipant,
    #[cfg(test)]
    wait_cycles: AtomicU64,
}

impl ReplySlot {
    fn new(failure_participant: ActorDomainParticipant) -> Self {
        Self {
            state: Mutex::new(ReplySlotState {
                receiver_alive: true,
                settled: false,
                poisoned: false,
                reply: None,
            }),
            ready: Condvar::new(),
            failure_participant,
            #[cfg(test)]
            wait_cycles: AtomicU64::new(0),
        }
    }

    /// Recover poisoned storage only to terminate the receipt. Never resume
    /// a successful reply, clear poison or take admission while holding this
    /// slot lock: enqueue holds admission and may drop a rejected ticket.
    fn cleanup_state(&self) -> MutexGuard<'_, ReplySlotState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(error) => {
                let mut state = error.into_inner();
                state.poisoned = true;
                state
            }
        }
    }

    fn report_poison(&self) {
        let _ = self
            .failure_participant
            .cancel_with_failure(Some(ActorDomainError::Poisoned));
    }

    fn publish(&self, value: ActorDomainReply, counters: &Arc<Counters>) {
        let mut state = self.cleanup_state();
        let poisoned = state.poisoned;
        let value = if poisoned {
            Err(ActorDomainError::Poisoned)
        } else {
            value
        };
        // Receipt accounting precedes result publication under the same
        // mutex. A completed wait observes exactly one accepted settlement.
        counters.pending_replies.fetch_sub(1, Ordering::AcqRel);
        counters.settled_replies.fetch_add(1, Ordering::AcqRel);
        if matches!(value, Err(ActorDomainError::Cancelled)) {
            counters.cancelled_replies.fetch_add(1, Ordering::AcqRel);
        }
        counters.live_reply_buffers.fetch_add(1, Ordering::AcqRel);
        let reply = SettledReply {
            value,
            counters: Arc::clone(counters),
        };
        if state.receiver_alive {
            state.reply = Some(reply);
        } else {
            // Abandoning a waiter never cancels execution or suppresses its
            // terminal failure. Its result buffer is released immediately.
            drop(reply);
        }
        state.settled = true;
        drop(state);
        self.ready.notify_all();
        if poisoned {
            self.report_poison();
        }
    }

    fn wait(&self) -> ActorDomainReply {
        let mut state = self.cleanup_state();
        let mut reported_poison = false;
        loop {
            if state.poisoned && !reported_poison {
                drop(state);
                self.report_poison();
                reported_poison = true;
                state = self.cleanup_state();
            }
            if state.settled {
                let reply = state.reply.take();
                let value = if state.poisoned {
                    Err(ActorDomainError::Poisoned)
                } else {
                    reply
                        .as_ref()
                        .map_or(Err(ActorDomainError::Cancelled), |reply| reply.value)
                };
                state.receiver_alive = false;
                drop(state);
                drop(reply);
                return value;
            }
            // A poison error still waits for the ticket's settlement. This
            // preserves the same barrier as normal/trap/cancel replies, and
            // the predicate loop tolerates arbitrary spurious wakeups.
            #[cfg(test)]
            self.wait_cycles.fetch_add(1, Ordering::AcqRel);
            state = match self.ready.wait(state) {
                Ok(state) => state,
                Err(error) => {
                    let mut state = error.into_inner();
                    state.poisoned = true;
                    state
                }
            };
        }
    }

    fn abandon(&self) {
        let mut state = self.cleanup_state();
        state.receiver_alive = false;
        let reply = state.reply.take();
        let poisoned = state.poisoned;
        drop(state);
        drop(reply);
        if poisoned {
            self.report_poison();
        }
    }
}

struct SettledReply {
    value: ActorDomainReply,
    counters: Arc<Counters>,
}

impl Drop for SettledReply {
    fn drop(&mut self) {
        self.counters
            .live_reply_buffers
            .fetch_sub(1, Ordering::AcqRel);
    }
}

impl ReplyTicket {
    fn settle(mut self, reply: ActorDomainReply) {
        self.finish(reply);
    }

    fn finish(&mut self, reply: ActorDomainReply) {
        if let Some(slot) = self.slot.take() {
            slot.publish(reply, &self.counters);
        }
    }

    fn reject(mut self) {
        if self.slot.take().is_some() {
            self.counters.pending_replies.fetch_sub(1, Ordering::AcqRel);
            self.counters
                .accepted_messages
                .fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl Drop for ReplyTicket {
    fn drop(&mut self) {
        // A worker that exits unexpectedly cannot strand an accepted waiter.
        self.finish(Err(ActorDomainError::Cancelled));
    }
}

struct MessageArguments {
    values: Vec<i32>,
    counters: Arc<Counters>,
}

impl Drop for MessageArguments {
    fn drop(&mut self) {
        self.counters
            .live_message_buffers
            .fetch_sub(1, Ordering::AcqRel);
    }
}

enum Command {
    Call {
        method: u32,
        arguments: MessageArguments,
        reply: ReplyTicket,
    },
    Close,
}

/// A pending reply owns its result slot, with no sender or domain owner.
pub struct PendingActorDomainReply(Arc<ReplySlot>);

impl PendingActorDomainReply {
    pub fn wait(self) -> ActorDomainReply {
        self.0.wait()
    }
}

impl Drop for PendingActorDomainReply {
    fn drop(&mut self) {
        self.0.abandon();
    }
}

thread_local! {
    static CURRENT_DOMAIN_EXECUTION: std::cell::Cell<Option<(u64, ParticipantRole)>> = const { std::cell::Cell::new(None) };
}

struct ExecutionMarker(Option<(u64, ParticipantRole)>);

impl ExecutionMarker {
    fn enter(domain: u64, role: ParticipantRole) -> Self {
        Self(CURRENT_DOMAIN_EXECUTION.with(|current| current.replace(Some((domain, role)))))
    }
}

impl Drop for ExecutionMarker {
    fn drop(&mut self) {
        CURRENT_DOMAIN_EXECUTION.with(|current| current.set(self.0));
    }
}

struct WorkerResources(Arc<Counters>);

impl WorkerResources {
    fn enter(counters: Arc<Counters>) -> Self {
        counters.live_workers.fetch_add(1, Ordering::AcqRel);
        counters.live_states.fetch_add(1, Ordering::AcqRel);
        Self(counters)
    }
}

impl Drop for WorkerResources {
    fn drop(&mut self) {
        self.0.live_states.fetch_sub(1, Ordering::AcqRel);
        self.0.live_workers.fetch_sub(1, Ordering::AcqRel);
    }
}

struct TaskResources(Arc<Counters>);

impl TaskResources {
    fn enter(counters: Arc<Counters>) -> Self {
        counters.live_tasks.fetch_add(1, Ordering::AcqRel);
        Self(counters)
    }
}

impl Drop for TaskResources {
    fn drop(&mut self) {
        self.0.live_tasks.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Execute one scalar Actor turn against a private candidate state. The
/// candidate becomes the worker's persistent state only when the dispatch
/// returns a successful scalar reply. This is shared by Bytecode and native
/// Actor dispatch, so a failed Flow-backed turn cannot publish a partial
/// transition.
fn execute_turn_transactionally(
    state: &mut [i32],
    candidate: &mut [i32],
    dispatch: impl FnOnce(&mut [i32]) -> ActorDomainReply,
) -> ActorDomainReply {
    if state.len() != candidate.len() {
        return Err(ActorDomainError::InvalidPayload);
    }
    candidate.copy_from_slice(state);
    let reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dispatch(candidate)))
        .unwrap_or(Err(ActorDomainError::WorkerPanicked));
    if reply.is_ok() {
        state.copy_from_slice(candidate);
    }
    reply
}

impl ActorDomainParticipant {
    fn execution_role(&self, domain: u64) -> Result<ParticipantRole, ActorDomainError> {
        match CURRENT_DOMAIN_EXECUTION.with(|current| current.get()) {
            Some((id, _)) if id != domain => Err(ActorDomainError::InvalidParticipant),
            Some((_, role)) => Ok(role),
            None => Ok(self.role),
        }
    }

    fn admit(&self, admission: &Admission, domain: u64) -> Result<(), ActorDomainError> {
        let admitted = match self.execution_role(domain)? {
            ParticipantRole::Root => admission.phase == Phase::Open,
            ParticipantRole::Child(child) => {
                admission.active_children.contains(&child)
                    && matches!(admission.phase, Phase::Open | Phase::SettlingChildren)
            }
            ParticipantRole::Worker => return Err(ActorDomainError::InvalidParticipant),
        };
        if admitted {
            Ok(())
        } else {
            Err(ActorDomainError::Closed)
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.domain
            .upgrade()
            .map_or(true, |domain| domain.cancelled.load(Ordering::Acquire))
    }

    /// Linearized with spawn and enqueue. Wake messages carry no sender.
    pub fn request_cancel(&self) -> Result<(), ActorDomainError> {
        self.cancel_with_failure(None)
    }

    fn cancel_with_failure(
        &self,
        failure: Option<ActorDomainError>,
    ) -> Result<(), ActorDomainError> {
        let Some(domain) = self.domain.upgrade() else {
            return Err(ActorDomainError::Closed);
        };
        self.execution_role(domain.id)?;
        let mut admission = domain.work_admission()?;
        domain.cancel_locked(&mut admission, failure);
        Ok(())
    }

    pub fn spawn_actor<F>(
        &self,
        initial_fields: Vec<i32>,
        parameter_arities: Vec<usize>,
        mut dispatch: F,
    ) -> Result<ActorDomainHandle, ActorDomainError>
    where
        F: FnMut(&mut [i32], u32, &[i32], &ActorTurnContext) -> ActorDomainReply + Send + 'static,
    {
        if parameter_arities.is_empty()
            || initial_fields.len() > 64
            || parameter_arities.len() > 64
            || parameter_arities.iter().any(|arity| *arity > 64)
        {
            return Err(ActorDomainError::InvalidPayload);
        }
        let domain = self.domain.upgrade().ok_or(ActorDomainError::Closed)?;
        self.execution_role(domain.id)?;
        let mut admission = domain.work_admission()?;
        if domain.cancelled.load(Ordering::Acquire) {
            return Err(ActorDomainError::Cancelled);
        }
        self.admit(&admission, domain.id)?;
        if domain
            .quota
            .is_some_and(|quota| admission.actors.len() >= quota)
        {
            return Err(ActorDomainError::QuotaExceeded);
        }
        #[cfg(test)]
        if domain.fail_next_spawn.swap(false, Ordering::AcqRel) {
            return Err(ActorDomainError::SpawnFailed);
        }
        let actor = allocate_identity(&NEXT_ACTOR)?;
        let (sender, receiver) = mpsc::channel::<Command>();
        let participant = ActorDomainParticipant {
            domain: Arc::downgrade(&domain),
            role: ParticipantRole::Worker,
        };
        let counters = Arc::clone(&domain.counters);
        let cancelled = Arc::clone(&domain.cancelled);
        let domain_id = domain.id;
        let worker = std::thread::Builder::new()
            .name(format!("mimi-canonical-actor-{domain_id}-{actor}"))
            .spawn(move || {
                let _marker = ExecutionMarker::enter(domain_id, ParticipantRole::Worker);
                let _resources = WorkerResources::enter(counters);
                let mut fields = initial_fields;
                // Allocate one candidate buffer for the actor lifetime. Each
                // turn starts from committed state, so failed mutations are
                // overwritten before any later dispatch and never published.
                let mut candidate_fields = vec![0; fields.len()];
                let context = ActorTurnContext { participant, actor };
                while let Ok(command) = receiver.recv() {
                    match command {
                        Command::Close => break,
                        Command::Call {
                            method,
                            arguments,
                            reply,
                        } => {
                            if cancelled.load(Ordering::Acquire) {
                                reply.settle(Err(ActorDomainError::Cancelled));
                                continue;
                            }
                            let result = execute_turn_transactionally(
                                &mut fields,
                                &mut candidate_fields,
                                |candidate| {
                                    dispatch(candidate, method, &arguments.values, &context)
                                },
                            );
                            if let Err(error) = result {
                                // Any poisoned admission also records a
                                // terminal domain failure and wakes siblings.
                                let _ = context.participant.cancel_with_failure(Some(error));
                            }
                            reply.settle(result);
                        }
                    }
                }
                // Receiver drop settles any residual ReplyTickets, including
                // queues abandoned by an unexpected worker failure.
            })
            .map_err(|_| ActorDomainError::SpawnFailed)?;
        admission.actors.insert(
            actor,
            ActorControl {
                arities: parameter_arities,
                sender: Some(sender),
                worker: Some(worker),
            },
        );
        domain
            .counters
            .spawned_actors
            .fetch_add(1, Ordering::AcqRel);
        domain.counters.live_controls.fetch_add(1, Ordering::AcqRel);
        Ok(ActorDomainHandle {
            domain: Arc::downgrade(&domain),
            id: actor,
        })
    }

    pub fn enqueue(
        &self,
        actor: &ActorDomainHandle,
        method: u32,
        arguments: &[i32],
    ) -> Result<PendingActorDomainReply, ActorDomainError> {
        if !Weak::ptr_eq(&self.domain, &actor.domain) {
            return Err(ActorDomainError::UnknownActor);
        }
        let domain = self.domain.upgrade().ok_or(ActorDomainError::Closed)?;
        self.execution_role(domain.id)?;
        let admission = domain.work_admission()?;
        if domain.cancelled.load(Ordering::Acquire) {
            return Err(ActorDomainError::Cancelled);
        }
        self.admit(&admission, domain.id)?;
        let control = admission
            .actors
            .get(&actor.id)
            .ok_or(ActorDomainError::UnknownActor)?;
        if control.arities.get(method as usize) != Some(&arguments.len()) {
            return Err(ActorDomainError::InvalidPayload);
        }
        let sender = control.sender.as_ref().ok_or(ActorDomainError::Closed)?;
        let reply_slot = Arc::new(ReplySlot::new(self.clone()));
        domain
            .counters
            .accepted_messages
            .fetch_add(1, Ordering::AcqRel);
        domain
            .counters
            .pending_replies
            .fetch_add(1, Ordering::AcqRel);
        domain
            .counters
            .live_message_buffers
            .fetch_add(1, Ordering::AcqRel);
        let message = Command::Call {
            method,
            arguments: MessageArguments {
                values: arguments.to_vec(),
                counters: Arc::clone(&domain.counters),
            },
            reply: ReplyTicket {
                slot: Some(Arc::clone(&reply_slot)),
                counters: Arc::clone(&domain.counters),
            },
        };
        if let Err(error) = sender.send(message) {
            if let Command::Call { reply, .. } = error.0 {
                reply.reject();
            }
            return Err(ActorDomainError::Closed);
        }
        Ok(PendingActorDomainReply(reply_slot))
    }

    pub fn call(
        &self,
        actor: &ActorDomainHandle,
        method: u32,
        arguments: &[i32],
    ) -> ActorDomainReply {
        self.enqueue(actor, method, arguments)?.wait()
    }

    /// Structured child tasks receive their own role. Normal close admits
    /// their remaining actor work until all accepted children have joined.
    pub fn spawn_task<F>(&self, task: F) -> Result<(), ActorDomainError>
    where
        F: FnOnce(ActorDomainParticipant) -> Result<(), ActorDomainError> + Send + 'static,
    {
        let domain = self.domain.upgrade().ok_or(ActorDomainError::Closed)?;
        self.execution_role(domain.id)?;
        let mut admission = domain.work_admission()?;
        if domain.cancelled.load(Ordering::Acquire) {
            return Err(ActorDomainError::Cancelled);
        }
        self.admit(&admission, domain.id)?;
        let task_id = allocate_identity(&NEXT_TASK)?;
        let participant = ActorDomainParticipant {
            domain: Arc::downgrade(&domain),
            role: ParticipantRole::Child(task_id),
        };
        let counters = Arc::clone(&domain.counters);
        let domain_id = domain.id;
        admission.active_children.insert(task_id);
        let worker = std::thread::Builder::new()
            .name(format!("mimi-canonical-child-{domain_id}-{task_id}"))
            .spawn(move || {
                let _marker = ExecutionMarker::enter(domain_id, ParticipantRole::Child(task_id));
                let _resources = TaskResources::enter(counters);
                let failure_participant = participant.clone();
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| task(participant)))
                        .unwrap_or(Err(ActorDomainError::WorkerPanicked));
                if let Err(error) = result {
                    let _ = failure_participant.cancel_with_failure(Some(error));
                }
                if let Some(domain) = failure_participant.domain.upgrade() {
                    domain.cleanup_admission().active_children.remove(&task_id);
                }
                result
            });
        let worker = match worker {
            Ok(worker) => worker,
            Err(_) => {
                admission.active_children.remove(&task_id);
                return Err(ActorDomainError::SpawnFailed);
            }
        };
        admission.tasks.insert(task_id, worker);
        Ok(())
    }
}

struct DomainOwnerCore {
    domain: Arc<DomainInner>,
}

impl DomainOwnerCore {
    fn new(quota: Option<usize>) -> Result<Self, ActorDomainError> {
        if CURRENT_DOMAIN_EXECUTION.with(|current| current.get().is_some()) {
            return Err(ActorDomainError::InvalidParticipant);
        }
        let id = allocate_identity(&NEXT_DOMAIN)?;
        let counters = Arc::new(Counters::default());
        counters.live_roots.store(1, Ordering::Release);
        Ok(Self {
            domain: Arc::new(DomainInner {
                id,
                quota,
                admission: Mutex::new(Admission {
                    phase: Phase::Open,
                    actors: BTreeMap::new(),
                    tasks: BTreeMap::new(),
                    active_children: BTreeSet::new(),
                    failure: None,
                }),
                cancelled: Arc::new(AtomicBool::new(false)),
                counters,
                #[cfg(test)]
                fail_next_spawn: AtomicBool::new(false),
            }),
        })
    }

    fn participant(&self) -> ActorDomainParticipant {
        ActorDomainParticipant {
            domain: Arc::downgrade(&self.domain),
            role: ParticipantRole::Root,
        }
    }

    fn close(&mut self, mode: ActorDomainCloseMode) -> Result<ActorDomainCounts, ActorDomainError> {
        if let Some((domain, _)) = CURRENT_DOMAIN_EXECUTION.with(|current| current.get()) {
            if domain != self.domain.id {
                return Err(ActorDomainError::InvalidParticipant);
            }
            let _ = self.participant().request_cancel();
            return Err(ActorDomainError::WorkerCannotClose);
        }
        if mode == ActorDomainCloseMode::Cancel {
            let _ = self.participant().request_cancel();
        }
        {
            let mut admission = self.domain.cleanup_admission();
            if admission.phase == Phase::Closed {
                return match admission.failure {
                    Some(error) => Err(error),
                    None => Ok(self.domain.counters.snapshot()),
                };
            }
            admission.phase = Phase::SettlingChildren;
        }
        loop {
            let tasks = {
                let mut admission = self.domain.cleanup_admission();
                if admission.tasks.is_empty() {
                    admission.phase = Phase::ClosingActors;
                    break;
                }
                std::mem::take(&mut admission.tasks)
            };
            for (_, task) in tasks {
                let result = task.join().unwrap_or(Err(ActorDomainError::WorkerPanicked));
                self.domain
                    .counters
                    .joined_tasks
                    .fetch_add(1, Ordering::AcqRel);
                if let Err(error) = result {
                    let mut admission = self.domain.cleanup_admission();
                    self.domain.cancel_locked(&mut admission, Some(error));
                }
            }
        }
        let controls = {
            let mut admission = self.domain.cleanup_admission();
            for control in admission.actors.values_mut() {
                if let Some(sender) = control.sender.take() {
                    // A receiver already closed by a cancelled/failed worker
                    // needs no wake message; join below observes completion.
                    let _ = sender.send(Command::Close);
                }
            }
            std::mem::take(&mut admission.actors)
        };
        for (_, mut control) in controls {
            if let Some(worker) = control.worker.take() {
                if worker.join().is_err() {
                    let mut admission = self.domain.cleanup_admission();
                    self.domain
                        .cancel_locked(&mut admission, Some(ActorDomainError::WorkerPanicked));
                }
                self.domain
                    .counters
                    .joined_workers
                    .fetch_add(1, Ordering::AcqRel);
            }
            self.domain
                .counters
                .live_controls
                .fetch_sub(1, Ordering::AcqRel);
        }
        let failure = {
            let mut admission = self.domain.cleanup_admission();
            admission.phase = Phase::Closed;
            // Turn/task failures are recorded when they happen, rather than
            // in join order. Unexpected join failures enter the same ledger
            // so every repeated close returns the same terminal status.
            admission.failure
        };
        match failure {
            Some(error) => Err(error),
            None => Ok(self.domain.counters.snapshot()),
        }
    }
}

impl Drop for DomainOwnerCore {
    fn drop(&mut self) {
        // Native registry deletion is guarded against child/worker ownership;
        // the public Rust root is !Send and cannot be moved into a worker.
        let _ = self.close(ActorDomainCloseMode::Cancel);
        self.domain.counters.live_roots.store(0, Ordering::Release);
    }
}

/// The unique root close/join obligation. This type deliberately has no Clone
/// implementation and cannot cross threads. Participants carry no ownership.
///
/// ```compile_fail
/// let root = mimi::runtime::actor_domain::RootDomainOwner::new(None).unwrap();
/// std::thread::spawn(move || drop(root));
/// ```
pub struct RootDomainOwner {
    core: DomainOwnerCore,
    _root_thread: PhantomData<Rc<()>>,
}

impl RootDomainOwner {
    pub fn new(quota: Option<usize>) -> Result<Self, ActorDomainError> {
        Ok(Self {
            core: DomainOwnerCore::new(quota)?,
            _root_thread: PhantomData,
        })
    }

    pub fn participant(&self) -> ActorDomainParticipant {
        self.core.participant()
    }

    pub fn monitor(&self) -> ActorDomainMonitor {
        ActorDomainMonitor(Arc::clone(&self.core.domain.counters))
    }

    pub fn close(
        &mut self,
        mode: ActorDomainCloseMode,
    ) -> Result<ActorDomainCounts, ActorDomainError> {
        self.core.close(mode)
    }

    #[cfg(test)]
    fn fail_next_spawn(&self) {
        self.core
            .domain
            .fail_next_spawn
            .store(true, Ordering::Release);
    }
}

// Native root tokens index owners, never an address supplied by the caller.
// Closed roots remain only until destroy; canonical emitters must call both
// close and destroy before reporting a tagged failure at their root boundary.
struct NativeDomainRoot {
    owner: Mutex<DomainOwnerCore>,
    participant: ActorDomainParticipant,
}

static NATIVE_ROOTS: OnceLock<Mutex<BTreeMap<u64, Arc<NativeDomainRoot>>>> = OnceLock::new();

fn native_roots() -> &'static Mutex<BTreeMap<u64, Arc<NativeDomainRoot>>> {
    NATIVE_ROOTS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn native_participant(domain: u64) -> Result<ActorDomainParticipant, ActorDomainError> {
    let execution = CURRENT_DOMAIN_EXECUTION.with(|current| current.get());
    if execution.is_some_and(|(id, _)| id != domain) {
        return Err(ActorDomainError::InvalidParticipant);
    }
    let entry = match native_roots().lock() {
        Ok(roots) => roots
            .get(&domain)
            .cloned()
            .ok_or(ActorDomainError::Closed)?,
        Err(error) => {
            // Recover only enough of a poisoned registry to revoke this
            // domain and wake accepted waiters. Never return a participant
            // or resume ordinary work through the recovered table.
            let entry = error.into_inner().get(&domain).cloned();
            if let Some(inner) = entry.and_then(|entry| entry.participant.domain.upgrade()) {
                let mut admission = inner.cleanup_admission();
                inner.cancel_locked(&mut admission, Some(ActorDomainError::Poisoned));
            }
            return Err(ActorDomainError::Poisoned);
        }
    };
    if entry.owner.is_poisoned() {
        if let Some(inner) = entry.participant.domain.upgrade() {
            let mut admission = inner.cleanup_admission();
            inner.cancel_locked(&mut admission, Some(ActorDomainError::Poisoned));
        }
        return Err(ActorDomainError::Poisoned);
    }
    let mut participant = entry.participant.clone();
    if let Some((_, role)) = execution {
        participant.role = role;
    }
    Ok(participant)
}

/// Registry lookup pins the entry before releasing the global mutex. A
/// concurrent destroy can remove the token, but cannot invalidate this Arc.
/// Poison recovery is allowed here solely to cancel and join the pinned root.
fn native_cleanup_entry(domain: u64) -> Result<Arc<NativeDomainRoot>, ActorDomainError> {
    let (roots, poisoned) = match native_roots().lock() {
        Ok(roots) => (roots, false),
        Err(error) => (error.into_inner(), true),
    };
    let entry = roots.get(&domain).cloned().ok_or(if poisoned {
        ActorDomainError::Poisoned
    } else {
        ActorDomainError::Closed
    })?;
    drop(roots);
    if poisoned {
        if let Some(inner) = entry.participant.domain.upgrade() {
            let mut admission = inner.cleanup_admission();
            inner.cancel_locked(&mut admission, Some(ActorDomainError::Poisoned));
        }
    }
    Ok(entry)
}

fn native_close_entry(
    entry: &NativeDomainRoot,
    mode: ActorDomainCloseMode,
) -> (
    Result<ActorDomainCounts, ActorDomainError>,
    ActorDomainCounts,
) {
    if mode == ActorDomainCloseMode::Cancel {
        // A concurrent Drain may hold owner while joining a child that is
        // waiting for cancellation. Revoke work before waiting for that
        // serialization lock; cancellation needs no close/join ownership.
        let _ = entry.participant.request_cancel();
    }
    let (mut owner, poisoned) = match entry.owner.lock() {
        Ok(owner) => (owner, false),
        Err(error) => (error.into_inner(), true),
    };
    if poisoned {
        let mut admission = owner.domain.cleanup_admission();
        owner
            .domain
            .cancel_locked(&mut admission, Some(ActorDomainError::Poisoned));
    }
    let result = owner.close(if poisoned {
        ActorDomainCloseMode::Cancel
    } else {
        mode
    });
    let counts = owner.domain.counters.snapshot();
    (result, counts)
}

fn native_reject_execution_close(domain: u64) -> Option<i32> {
    CURRENT_DOMAIN_EXECUTION.with(|current| match current.get() {
        Some((id, _)) if id != domain => Some(MIR_ACTOR_STATUS_INVALID_PARTICIPANT),
        Some(_) => {
            if let Ok(participant) = native_participant(domain) {
                // Poison also performs cancellation before returning error.
                let _ = participant.request_cancel();
            }
            Some(MIR_ACTOR_STATUS_WORKER_CANNOT_CLOSE)
        }
        None => None,
    })
}

pub const MIR_ACTOR_STATUS_SUCCESS: i32 = 0;
pub const MIR_ACTOR_STATUS_CANCELLED: i32 = -1;
pub const MIR_ACTOR_STATUS_CLOSED: i32 = -2;
pub const MIR_ACTOR_STATUS_QUOTA: i32 = -3;
pub const MIR_ACTOR_STATUS_SPAWN_FAILED: i32 = -4;
pub const MIR_ACTOR_STATUS_UNKNOWN_ACTOR: i32 = -5;
pub const MIR_ACTOR_STATUS_INVALID_PAYLOAD: i32 = -6;
pub const MIR_ACTOR_STATUS_WORKER_PANICKED: i32 = -7;
pub const MIR_ACTOR_STATUS_WORKER_CANNOT_CLOSE: i32 = -8;
pub const MIR_ACTOR_STATUS_INVALID_PARTICIPANT: i32 = -9;
pub const MIR_ACTOR_STATUS_IDENTITY_EXHAUSTED: i32 = -10;
pub const MIR_ACTOR_STATUS_POISONED: i32 = -11;

fn error_status(error: ActorDomainError) -> i32 {
    match error {
        ActorDomainError::Closed => MIR_ACTOR_STATUS_CLOSED,
        ActorDomainError::Cancelled => MIR_ACTOR_STATUS_CANCELLED,
        ActorDomainError::QuotaExceeded => MIR_ACTOR_STATUS_QUOTA,
        ActorDomainError::SpawnFailed => MIR_ACTOR_STATUS_SPAWN_FAILED,
        ActorDomainError::UnknownActor => MIR_ACTOR_STATUS_UNKNOWN_ACTOR,
        ActorDomainError::InvalidPayload => MIR_ACTOR_STATUS_INVALID_PAYLOAD,
        ActorDomainError::WorkerPanicked => MIR_ACTOR_STATUS_WORKER_PANICKED,
        ActorDomainError::WorkerCannotClose => MIR_ACTOR_STATUS_WORKER_CANNOT_CLOSE,
        ActorDomainError::InvalidParticipant => MIR_ACTOR_STATUS_INVALID_PARTICIPANT,
        ActorDomainError::IdentityExhausted => MIR_ACTOR_STATUS_IDENTITY_EXHAUSTED,
        ActorDomainError::Poisoned => MIR_ACTOR_STATUS_POISONED,
        ActorDomainError::Trap(code) if matches!(code, 800..=802) => code,
        ActorDomainError::Trap(_) => MIR_ACTOR_STATUS_INVALID_PAYLOAD,
    }
}

fn dispatch_status(status: i32, result: i32) -> ActorDomainReply {
    let error = match status {
        MIR_ACTOR_STATUS_SUCCESS => return Ok(result),
        MIR_ACTOR_STATUS_CANCELLED => ActorDomainError::Cancelled,
        MIR_ACTOR_STATUS_CLOSED => ActorDomainError::Closed,
        MIR_ACTOR_STATUS_QUOTA => ActorDomainError::QuotaExceeded,
        MIR_ACTOR_STATUS_SPAWN_FAILED => ActorDomainError::SpawnFailed,
        MIR_ACTOR_STATUS_UNKNOWN_ACTOR => ActorDomainError::UnknownActor,
        MIR_ACTOR_STATUS_INVALID_PAYLOAD => ActorDomainError::InvalidPayload,
        MIR_ACTOR_STATUS_WORKER_PANICKED => ActorDomainError::WorkerPanicked,
        MIR_ACTOR_STATUS_WORKER_CANNOT_CLOSE => ActorDomainError::WorkerCannotClose,
        MIR_ACTOR_STATUS_INVALID_PARTICIPANT => ActorDomainError::InvalidParticipant,
        MIR_ACTOR_STATUS_IDENTITY_EXHAUSTED => ActorDomainError::IdentityExhausted,
        MIR_ACTOR_STATUS_POISONED => ActorDomainError::Poisoned,
        800..=802 => ActorDomainError::Trap(status),
        _ => ActorDomainError::InvalidPayload,
    };
    Err(error)
}

/// Dispatch receives a turn-local candidate state. The runtime commits that
/// candidate and the result only on SUCCESS; every error discards the
/// candidate. Dispatch must return E0800/E0801/E0802 as an explicit status.
/// Known runtime failure statuses also propagate unchanged; unknown statuses
/// are invalid payloads. It must neither retain pointers nor unwind across C
/// ABI.
pub type NativeScalarActorDispatch = unsafe extern "C" fn(
    fields: *mut i32,
    field_count: u64,
    method: u32,
    arguments: *const i32,
    argument_count: u64,
    result: *mut i32,
) -> i32;

#[no_mangle]
pub extern "C" fn mimi_mir_actor_domain_open(quota: u64) -> u64 {
    let quota = if quota == 0 {
        None
    } else {
        match usize::try_from(quota) {
            Ok(value) => Some(value),
            Err(_) => return 0,
        }
    };
    let owner = match DomainOwnerCore::new(quota) {
        Ok(owner) => owner,
        Err(_) => return 0,
    };
    let id = owner.domain.id;
    let participant = owner.participant();
    let mut roots = match native_roots().lock() {
        Ok(roots) => roots,
        Err(_) => return 0,
    };
    roots.insert(
        id,
        Arc::new(NativeDomainRoot {
            owner: Mutex::new(owner),
            participant,
        }),
    );
    id
}

/// # Safety
/// Nonempty fields/arities must point to valid, aligned arrays for their
/// counts; actor_out must be writable. Dispatch follows the contract above.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_actor_domain_spawn(
    domain: u64,
    fields: *const i32,
    field_count: u64,
    arities: *const u32,
    method_count: u64,
    dispatch: Option<NativeScalarActorDispatch>,
    actor_out: *mut u64,
) -> i32 {
    if actor_out.is_null()
        || (field_count > 0 && fields.is_null())
        || arities.is_null()
        || field_count > 64
        || method_count == 0
        || method_count > 64
        || dispatch.is_none()
    {
        return MIR_ACTOR_STATUS_INVALID_PAYLOAD;
    }
    // SAFETY: output pointer validity is the caller's contract.
    unsafe {
        *actor_out = 0;
    }
    let fields = if field_count == 0 {
        Vec::new()
    } else {
        // SAFETY: caller provides the checked nonempty, bounded scalar array.
        unsafe { std::slice::from_raw_parts(fields, field_count as usize) }.to_vec()
    };
    // SAFETY: caller provides an aligned, bounded method arity array.
    let arities = unsafe { std::slice::from_raw_parts(arities, method_count as usize) }
        .iter()
        .map(|arity| *arity as usize)
        .collect::<Vec<_>>();
    let participant = match native_participant(domain) {
        Ok(value) => value,
        Err(error) => return error_status(error),
    };
    let dispatch = dispatch.expect("validated native dispatch");
    match participant.spawn_actor(fields, arities, move |fields, method, arguments, _| {
        let mut result = 0i32;
        // SAFETY: the arrays live for this turn, are i32-aligned, and the
        // caller-installed dispatch promises not to retain or unwind them.
        let status = unsafe {
            dispatch(
                fields.as_mut_ptr(),
                fields.len() as u64,
                method,
                arguments.as_ptr(),
                arguments.len() as u64,
                &mut result,
            )
        };
        dispatch_status(status, result)
    }) {
        Ok(actor) => {
            unsafe {
                *actor_out = actor.id;
            }
            MIR_ACTOR_STATUS_SUCCESS
        }
        Err(error) => error_status(error),
    }
}

/// # Safety
/// Nonempty arguments must point to a valid aligned bounded i32 array.
/// result_out must point to one writable i32. Error leaves the result unchanged.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_actor_domain_call(
    domain: u64,
    actor: u64,
    method: u32,
    arguments: *const i32,
    argument_count: u64,
    result_out: *mut i32,
) -> i32 {
    if result_out.is_null() || argument_count > 64 || (argument_count > 0 && arguments.is_null()) {
        return MIR_ACTOR_STATUS_INVALID_PAYLOAD;
    }
    let participant = match native_participant(domain) {
        Ok(value) => value,
        Err(error) => return error_status(error),
    };
    let handle = ActorDomainHandle {
        domain: participant.domain.clone(),
        id: actor,
    };
    let args = if argument_count == 0 {
        &[]
    } else {
        // SAFETY: caller promises valid aligned array; count is bounded.
        unsafe { std::slice::from_raw_parts(arguments, argument_count as usize) }
    };
    match participant.call(&handle, method, args) {
        Ok(value) => {
            unsafe {
                *result_out = value;
            }
            MIR_ACTOR_STATUS_SUCCESS
        }
        Err(error) => error_status(error),
    }
}

/// Close is idempotent until destroy. A native worker may request cancellation
/// through this entry, but can never take/drop the unique owner or self-join.
/// # Safety
/// counts_out is null or points to one writable ActorDomainCounts.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_actor_domain_close(
    domain: u64,
    cancel: i32,
    counts_out: *mut ActorDomainCounts,
) -> i32 {
    if let Some(status) = native_reject_execution_close(domain) {
        return status;
    }
    // The private owner mutex serializes repeated close. Global registry
    // access is released before joins; other domains and cancellation remain
    // available, including a worker's request to close its own domain.
    let entry = match native_cleanup_entry(domain) {
        Ok(entry) => entry,
        Err(error) => return error_status(error),
    };
    let mode = if cancel == 0 {
        ActorDomainCloseMode::Drain
    } else {
        ActorDomainCloseMode::Cancel
    };
    let (result, counts) = native_close_entry(&entry, mode);
    if !counts_out.is_null() {
        unsafe {
            *counts_out = counts;
        }
    }
    match result {
        Ok(_) => MIR_ACTOR_STATUS_SUCCESS,
        Err(error) => error_status(error),
    }
}

#[no_mangle]
pub extern "C" fn mimi_mir_actor_domain_destroy(domain: u64) -> i32 {
    if let Some(status) = native_reject_execution_close(domain) {
        return status;
    }
    let entry = match native_cleanup_entry(domain) {
        Ok(entry) => entry,
        Err(error) => return error_status(error),
    };
    // Join before removing the owner even if another native close is pinned.
    let (result, _) = native_close_entry(&entry, ActorDomainCloseMode::Cancel);
    // Removal is cleanup only. The registry remains poisoned and all future
    // ordinary lookup/open attempts continue to report failure.
    match native_roots().lock() {
        Ok(mut roots) => {
            roots.remove(&domain);
        }
        Err(error) => {
            error.into_inner().remove(&domain);
        }
    }
    match result {
        Ok(_) => MIR_ACTOR_STATUS_SUCCESS,
        Err(error) => error_status(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn quiescent(counts: ActorDomainCounts, root_alive: bool) {
        assert_eq!(counts.live_roots, u64::from(root_alive), "{counts:?}");
        assert_eq!(counts.live_workers, 0, "{counts:?}");
        assert_eq!(counts.live_states, 0, "{counts:?}");
        assert_eq!(counts.live_controls, 0, "{counts:?}");
        assert_eq!(counts.live_tasks, 0, "{counts:?}");
        assert_eq!(counts.pending_replies, 0, "{counts:?}");
        assert_eq!(counts.live_message_buffers, 0, "{counts:?}");
        assert_eq!(counts.live_reply_buffers, 0, "{counts:?}");
        assert_eq!(
            counts.accepted_messages, counts.settled_replies,
            "{counts:?}"
        );
        assert_eq!(counts.spawned_actors, counts.joined_workers, "{counts:?}");
    }

    fn wait_until(mut predicate: impl FnMut() -> bool) {
        let limit = Instant::now() + Duration::from_secs(3);
        while !predicate() {
            assert!(
                Instant::now() < limit,
                "domain test synchronization timed out"
            );
            std::thread::yield_now();
        }
    }

    fn counter(participant: &ActorDomainParticipant, initial: i32) -> ActorDomainHandle {
        participant
            .spawn_actor(vec![initial], vec![1], |fields, _, args, _| {
                let scaled = 10i32
                    .checked_add(args[0])
                    .ok_or(ActorDomainError::Trap(802))?;
                let next = fields[0]
                    .checked_add(scaled)
                    .ok_or(ActorDomainError::Trap(802))?;
                fields[0] = next;
                Ok(next)
            })
            .unwrap()
    }

    #[test]
    fn actor_turn_candidate_commits_only_after_successful_reply() {
        let mut state = [15];
        let mut candidate = [0];
        assert_eq!(
            execute_turn_transactionally(&mut state, &mut candidate, |candidate| {
                candidate[0] = 32;
                Ok(candidate[0])
            }),
            Ok(32)
        );
        assert_eq!(state, [32]);

        let result = execute_turn_transactionally(&mut state, &mut candidate, |candidate| {
            candidate[0] = 99;
            Err(ActorDomainError::Trap(801))
        });
        assert_eq!(result, Err(ActorDomainError::Trap(801)));
        assert_eq!(state, [32], "failed turn must not publish candidate state");

        let result = execute_turn_transactionally(&mut state, &mut candidate, |candidate| {
            candidate[0] = 101;
            panic!("actor dispatch panic")
        });
        assert_eq!(result, Err(ActorDomainError::WorkerPanicked));
        assert_eq!(
            state,
            [32],
            "panicked turn must not publish candidate state"
        );

        let result = execute_turn_transactionally(&mut state, &mut candidate, |candidate| {
            candidate[0] = 88;
            Err(ActorDomainError::InvalidPayload)
        });
        assert_eq!(result, Err(ActorDomainError::InvalidPayload));
        assert_eq!(
            state,
            [32],
            "invalid reply must not publish candidate state"
        );

        assert_eq!(
            execute_turn_transactionally(&mut state, &mut candidate, |candidate| {
                candidate[0] += 5;
                Ok(candidate[0])
            }),
            Ok(37),
            "a later successful turn starts from the last committed state"
        );
        assert_eq!(state, [37]);
    }

    #[test]
    fn domain_keeps_actor_state_and_drains_every_accepted_reply() {
        let mut root = RootDomainOwner::new(None).unwrap();
        let participant = root.participant();
        let actor = counter(&participant, 0);
        assert_eq!(participant.call(&actor, 0, &[5]), Ok(15));
        assert_eq!(participant.call(&actor, 0, &[7]), Ok(32));
        let queued = (0..8)
            .map(|_| participant.enqueue(&actor, 0, &[0]).unwrap())
            .collect::<Vec<_>>();
        let counts = root.close(ActorDomainCloseMode::Drain).unwrap();
        assert_eq!(counts.live_reply_buffers, 8);
        for (index, reply) in queued.into_iter().enumerate() {
            assert_eq!(reply.wait(), Ok(42 + 10 * index as i32));
        }
        let counts = root.monitor().counts();
        quiescent(counts, true);
        assert_eq!(root.close(ActorDomainCloseMode::Drain), Ok(counts));
        assert!(matches!(
            participant.enqueue(&actor, 0, &[1]),
            Err(ActorDomainError::Closed)
        ));
    }

    #[test]
    fn dropping_root_joins_even_when_copy_handles_and_participants_survive() {
        let (monitor, participant, actor) = {
            let root = RootDomainOwner::new(None).unwrap();
            let participant = root.participant();
            let actor = counter(&participant, 0);
            assert_eq!(participant.call(&actor, 0, &[5]), Ok(15));
            (root.monitor(), participant, actor)
        };
        quiescent(monitor.counts(), false);
        assert_eq!(
            participant.call(&actor, 0, &[1]),
            Err(ActorDomainError::Closed)
        );
    }

    #[test]
    fn cancellation_settles_queued_replies_and_finishes_the_active_turn() {
        let mut root = RootDomainOwner::new(None).unwrap();
        let participant = root.participant();
        let (entered, running) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let actor = participant
            .spawn_actor(vec![0], vec![1], move |fields, _, args, _| {
                entered.send(()).unwrap();
                resume.recv_timeout(Duration::from_secs(3)).unwrap();
                fields[0] += args[0];
                Ok(fields[0])
            })
            .unwrap();
        let first = participant.enqueue(&actor, 0, &[15]).unwrap();
        running.recv_timeout(Duration::from_secs(3)).unwrap();
        let queued = participant.enqueue(&actor, 0, &[100]).unwrap();
        participant.request_cancel().unwrap();
        assert!(matches!(
            participant.enqueue(&actor, 0, &[1]),
            Err(ActorDomainError::Cancelled)
        ));
        release.send(()).unwrap();
        assert_eq!(first.wait(), Ok(15));
        assert_eq!(queued.wait(), Err(ActorDomainError::Cancelled));
        let counts = root.close(ActorDomainCloseMode::Cancel).unwrap();
        quiescent(counts, true);
        assert_eq!(counts.cancelled_replies, 1);
    }

    #[test]
    fn normal_close_settles_children_and_nested_children_before_actor_drain() {
        let mut root = RootDomainOwner::new(None).unwrap();
        let participant = root.participant();
        let actor = counter(&participant, 0);
        let inner = participant.domain.clone();
        participant
            .spawn_task(move |child| {
                wait_until(|| {
                    inner.upgrade().unwrap().admission.lock().unwrap().phase
                        == Phase::SettlingChildren
                });
                assert_eq!(child.call(&actor, 0, &[5]), Ok(15));
                child.spawn_task(move |grandchild| {
                    assert_eq!(grandchild.call(&actor, 0, &[7]), Ok(32));
                    Ok(())
                })?;
                Ok(())
            })
            .unwrap();
        let counts = root.close(ActorDomainCloseMode::Drain).unwrap();
        quiescent(counts, true);
        assert_eq!(counts.joined_tasks, 2);
        assert_eq!(counts.accepted_messages, 2);
    }

    #[test]
    fn stale_child_participant_cannot_keep_close_admission_open() {
        let mut root = RootDomainOwner::new(None).unwrap();
        let participant = root.participant();
        let actor = counter(&participant, 0);
        let (send, receive) = mpsc::channel();
        participant
            .spawn_task(move |child| {
                send.send(child).unwrap();
                Ok(())
            })
            .unwrap();
        let stale = receive.recv_timeout(Duration::from_secs(3)).unwrap();
        wait_until(|| {
            participant
                .domain
                .upgrade()
                .unwrap()
                .admission
                .lock()
                .unwrap()
                .active_children
                .is_empty()
        });
        assert_eq!(stale.call(&actor, 0, &[5]), Err(ActorDomainError::Closed));
        quiescent(root.close(ActorDomainCloseMode::Drain).unwrap(), true);
    }

    #[test]
    fn independent_domains_do_not_share_actor_registry_quota_or_close() {
        let mut first = RootDomainOwner::new(Some(1)).unwrap();
        let mut second = RootDomainOwner::new(Some(1)).unwrap();
        let a = first.participant();
        let b = second.participant();
        let actor_a = counter(&a, 0);
        let actor_b = counter(&b, 7);
        assert!(matches!(
            a.spawn_actor(vec![0], vec![0], |_, _, _, _| Ok(0)),
            Err(ActorDomainError::QuotaExceeded)
        ));
        assert_eq!(
            b.call(&actor_a, 0, &[1]),
            Err(ActorDomainError::UnknownActor)
        );
        assert_eq!(a.call(&actor_a, 0, &[5]), Ok(15));
        quiescent(first.close(ActorDomainCloseMode::Drain).unwrap(), true);
        assert_eq!(b.call(&actor_b, 0, &[5]), Ok(22));
        assert_eq!(second.monitor().counts().live_workers, 1);
        quiescent(second.close(ActorDomainCloseMode::Drain).unwrap(), true);
    }

    #[test]
    fn tagged_turn_trap_cancels_idle_sibling_and_closes_without_residual_buffers() {
        let mut root = RootDomainOwner::new(None).unwrap();
        let participant = root.participant();
        let trap = counter(&participant, i32::MAX);
        let _idle = counter(&participant, 0);
        assert_eq!(
            participant.call(&trap, 0, &[1]),
            Err(ActorDomainError::Trap(802))
        );
        assert_eq!(
            root.close(ActorDomainCloseMode::Drain),
            Err(ActorDomainError::Trap(802))
        );
        quiescent(root.monitor().counts(), true);
        assert_eq!(
            root.close(ActorDomainCloseMode::Cancel),
            Err(ActorDomainError::Trap(802))
        );
    }

    #[test]
    fn close_preserves_first_recorded_task_failure_instead_of_join_order() {
        let mut root = RootDomainOwner::new(None).unwrap();
        let participant = root.participant();
        let (release, resume) = mpsc::channel();
        // This child has the earlier join position, but fails only after the
        // later child has already recorded the domain's terminal error.
        participant
            .spawn_task(move |_| {
                resume.recv_timeout(Duration::from_secs(3)).unwrap();
                Err(ActorDomainError::Trap(802))
            })
            .unwrap();
        participant
            .spawn_task(|_| Err(ActorDomainError::Trap(800)))
            .unwrap();
        wait_until(|| participant.is_cancelled());
        release.send(()).unwrap();
        for mode in [ActorDomainCloseMode::Drain, ActorDomainCloseMode::Cancel] {
            assert_eq!(root.close(mode), Err(ActorDomainError::Trap(800)));
        }
        quiescent(root.monitor().counts(), true);
        assert_eq!(root.monitor().counts().joined_tasks, 2);
    }

    #[test]
    fn close_persists_worker_teardown_panic_for_every_repeated_close() {
        struct PanicOnDrop;
        impl Drop for PanicOnDrop {
            fn drop(&mut self) {
                panic!("injected dispatch destructor panic");
            }
        }
        let mut root = RootDomainOwner::new(None).unwrap();
        let participant = root.participant();
        let destructor = PanicOnDrop;
        participant
            .spawn_actor(vec![], vec![0], move |_, _, _, _| {
                let _keep_capture = &destructor;
                Ok(42)
            })
            .unwrap();
        for mode in [ActorDomainCloseMode::Drain, ActorDomainCloseMode::Cancel] {
            assert_eq!(root.close(mode), Err(ActorDomainError::WorkerPanicked));
        }
        quiescent(root.monitor().counts(), true);
    }

    #[test]
    fn actor_spawn_failure_rolls_back_every_domain_resource() {
        let mut root = RootDomainOwner::new(Some(1)).unwrap();
        let participant = root.participant();
        root.fail_next_spawn();
        assert!(matches!(
            participant.spawn_actor(vec![0], vec![1], |_, _, _, _| Ok(0)),
            Err(ActorDomainError::SpawnFailed)
        ));
        let before = root.monitor().counts();
        assert_eq!(before.spawned_actors, 0);
        assert_eq!(before.live_workers, 0);
        assert_eq!(before.live_states, 0);
        assert_eq!(before.live_controls, 0);
        let actor = counter(&participant, 0);
        assert_eq!(participant.call(&actor, 0, &[5]), Ok(15));
        quiescent(root.close(ActorDomainCloseMode::Drain).unwrap(), true);
    }

    #[test]
    fn cancellation_and_concurrent_enqueue_linearize_every_accepted_message() {
        let mut root = RootDomainOwner::new(None).unwrap();
        let participant = root.participant();
        let actor = counter(&participant, 0);
        let racing = participant.clone();
        let thread = std::thread::spawn(move || {
            let mut replies = Vec::new();
            for _ in 0..128 {
                match racing.enqueue(&actor, 0, &[0]) {
                    Ok(reply) => replies.push(reply),
                    Err(ActorDomainError::Cancelled | ActorDomainError::Closed) => break,
                    Err(error) => panic!("unexpected admission failure {error:?}"),
                }
            }
            for reply in replies {
                assert!(matches!(
                    reply.wait(),
                    Ok(_) | Err(ActorDomainError::Cancelled)
                ));
            }
        });
        root.close(ActorDomainCloseMode::Cancel).unwrap();
        thread.join().unwrap();
        quiescent(root.monitor().counts(), true);
    }

    #[test]
    fn worker_cancellation_request_never_takes_its_join_obligation() {
        let mut root = RootDomainOwner::new(None).unwrap();
        let participant = root.participant();
        let actor = participant
            .spawn_actor(vec![0], vec![0], |_, _, _, context| {
                context.request_cancel().unwrap();
                Ok(42)
            })
            .unwrap();
        assert_eq!(participant.call(&actor, 0, &[]), Ok(42));
        quiescent(root.close(ActorDomainCloseMode::Cancel).unwrap(), true);
    }

    unsafe extern "C" fn native_counter(
        fields: *mut i32,
        count: u64,
        method: u32,
        args: *const i32,
        argc: u64,
        result: *mut i32,
    ) -> i32 {
        if count != 1 || argc != 1 || method != 0 {
            return MIR_ACTOR_STATUS_INVALID_PAYLOAD;
        }
        let Some(scaled) = 10i32.checked_add(unsafe { *args }) else {
            return 802;
        };
        let Some(value) = unsafe { *fields }.checked_add(scaled) else {
            return 802;
        };
        unsafe {
            *fields = value;
            *result = value;
        }
        MIR_ACTOR_STATUS_SUCCESS
    }

    unsafe extern "C" fn native_divide(
        fields: *mut i32,
        count: u64,
        method: u32,
        args: *const i32,
        argc: u64,
        result: *mut i32,
    ) -> i32 {
        if count != 1 || argc != 1 || method != 0 {
            return MIR_ACTOR_STATUS_INVALID_PAYLOAD;
        }
        let (left, right) = unsafe { (*fields, *args) };
        if right == 0 {
            return 801;
        }
        let Some(value) = left.checked_div(right) else {
            return 802;
        };
        unsafe {
            *result = value;
        }
        MIR_ACTOR_STATUS_SUCCESS
    }

    #[test]
    fn native_definedness_status_distinguishes_zero_division_and_integer_overflow() {
        for (left, right, status) in [(15, 0, 801), (i32::MIN, -1, 802)] {
            let domain = mimi_mir_actor_domain_open(1);
            let monitor = native_monitor(domain);
            let mut actor = 0;
            unsafe {
                assert_eq!(
                    mimi_mir_actor_domain_spawn(
                        domain,
                        [left].as_ptr(),
                        1,
                        [1].as_ptr(),
                        1,
                        Some(native_divide),
                        &mut actor
                    ),
                    0
                );
                let mut output = 77;
                assert_eq!(
                    mimi_mir_actor_domain_call(domain, actor, 0, [right].as_ptr(), 1, &mut output),
                    status
                );
                assert_eq!(output, 77);
                assert_eq!(
                    mimi_mir_actor_domain_close(domain, 1, std::ptr::null_mut()),
                    status
                );
            }
            assert_eq!(mimi_mir_actor_domain_destroy(domain), status);
            quiescent(monitor.counts(), false);
        }
    }

    unsafe extern "C" fn native_return_status(
        fields: *mut i32,
        _: u64,
        _: u32,
        _: *const i32,
        _: u64,
        _: *mut i32,
    ) -> i32 {
        unsafe { *fields }
    }

    #[test]
    fn native_dispatch_preserves_known_failure_statuses_and_rejects_unknown_statuses() {
        let statuses = [
            MIR_ACTOR_STATUS_CANCELLED,
            MIR_ACTOR_STATUS_CLOSED,
            MIR_ACTOR_STATUS_QUOTA,
            MIR_ACTOR_STATUS_SPAWN_FAILED,
            MIR_ACTOR_STATUS_UNKNOWN_ACTOR,
            MIR_ACTOR_STATUS_INVALID_PAYLOAD,
            MIR_ACTOR_STATUS_WORKER_PANICKED,
            MIR_ACTOR_STATUS_WORKER_CANNOT_CLOSE,
            MIR_ACTOR_STATUS_INVALID_PARTICIPANT,
            MIR_ACTOR_STATUS_IDENTITY_EXHAUSTED,
            MIR_ACTOR_STATUS_POISONED,
            800,
            801,
            802,
            -999,
            999,
        ];
        for status in statuses {
            let expected = if status == -999 || status == 999 {
                MIR_ACTOR_STATUS_INVALID_PAYLOAD
            } else {
                status
            };
            let domain = mimi_mir_actor_domain_open(1);
            let monitor = native_monitor(domain);
            let mut actor = 0;
            let mut output = 77;
            unsafe {
                assert_eq!(
                    mimi_mir_actor_domain_spawn(
                        domain,
                        [status].as_ptr(),
                        1,
                        [0].as_ptr(),
                        1,
                        Some(native_return_status),
                        &mut actor,
                    ),
                    MIR_ACTOR_STATUS_SUCCESS
                );
                assert_eq!(
                    mimi_mir_actor_domain_call(domain, actor, 0, std::ptr::null(), 0, &mut output,),
                    expected
                );
                assert_eq!(output, 77);
                for cancel in [0, 1] {
                    assert_eq!(
                        mimi_mir_actor_domain_close(domain, cancel, std::ptr::null_mut()),
                        expected
                    );
                }
            }
            assert_eq!(mimi_mir_actor_domain_destroy(domain), expected);
            quiescent(monitor.counts(), false);
        }
    }

    fn native_monitor(domain: u64) -> ActorDomainMonitor {
        let entry = native_roots().lock().unwrap().get(&domain).unwrap().clone();
        let owner = entry.owner.lock().unwrap();
        ActorDomainMonitor(Arc::clone(&owner.domain.counters))
    }

    #[test]
    fn native_domain_abi_preserves_state_and_ignores_other_roots_on_close() {
        let a = mimi_mir_actor_domain_open(1);
        let b = mimi_mir_actor_domain_open(1);
        let a_monitor = native_monitor(a);
        let b_monitor = native_monitor(b);
        let field = [0i32];
        let arity = [1u32];
        let mut actor_a = 0;
        let mut actor_b = 0;
        unsafe {
            assert_eq!(
                mimi_mir_actor_domain_spawn(
                    a,
                    field.as_ptr(),
                    1,
                    arity.as_ptr(),
                    1,
                    Some(native_counter),
                    &mut actor_a
                ),
                0
            );
            assert_eq!(
                mimi_mir_actor_domain_spawn(
                    b,
                    field.as_ptr(),
                    1,
                    arity.as_ptr(),
                    1,
                    Some(native_counter),
                    &mut actor_b
                ),
                0
            );
            let mut result = -999;
            assert_eq!(
                mimi_mir_actor_domain_call(a, actor_a, 0, [5].as_ptr(), 1, &mut result),
                0
            );
            assert_eq!(result, 15);
            assert_eq!(
                mimi_mir_actor_domain_call(a, actor_a, 0, [7].as_ptr(), 1, &mut result),
                0
            );
            assert_eq!(result, 32);
            assert_eq!(
                mimi_mir_actor_domain_call(b, actor_a, 0, [5].as_ptr(), 1, &mut result),
                MIR_ACTOR_STATUS_UNKNOWN_ACTOR
            );
            let mut counts = ActorDomainCounts::default();
            assert_eq!(mimi_mir_actor_domain_close(a, 0, &mut counts), 0);
            quiescent(counts, true);
            assert_eq!(mimi_mir_actor_domain_close(a, 0, &mut counts), 0);
            assert_eq!(
                mimi_mir_actor_domain_call(b, actor_b, 0, [5].as_ptr(), 1, &mut result),
                0
            );
            assert_eq!(result, 15);
            assert_eq!(mimi_mir_actor_domain_close(b, 0, &mut counts), 0);
            quiescent(counts, true);
        }
        assert_eq!(mimi_mir_actor_domain_destroy(a), 0);
        assert_eq!(mimi_mir_actor_domain_destroy(b), 0);
        quiescent(a_monitor.counts(), false);
        quiescent(b_monitor.counts(), false);
    }

    #[test]
    fn native_tagged_trap_leaves_result_unchanged_and_closes_all_owned_resources() {
        let domain = mimi_mir_actor_domain_open(0);
        let monitor = native_monitor(domain);
        let mut actor = 0;
        let field = [i32::MAX];
        let arity = [1u32];
        unsafe {
            assert_eq!(
                mimi_mir_actor_domain_spawn(
                    domain,
                    field.as_ptr(),
                    1,
                    arity.as_ptr(),
                    1,
                    Some(native_counter),
                    &mut actor
                ),
                0
            );
            let mut result = 123;
            assert_eq!(
                mimi_mir_actor_domain_call(domain, actor, 0, [1].as_ptr(), 1, &mut result),
                802
            );
            assert_eq!(result, 123);
            let mut counts = ActorDomainCounts::default();
            assert_eq!(mimi_mir_actor_domain_close(domain, 1, &mut counts), 802);
            quiescent(counts, true);
        }
        assert_eq!(mimi_mir_actor_domain_destroy(domain), 802);
        quiescent(monitor.counts(), false);
    }

    static WORKER_NATIVE_DOMAIN: AtomicU64 = AtomicU64::new(0);

    unsafe extern "C" fn native_self_close(
        _: *mut i32,
        _: u64,
        _: u32,
        _: *const i32,
        _: u64,
        result: *mut i32,
    ) -> i32 {
        let domain = WORKER_NATIVE_DOMAIN.load(Ordering::Acquire);
        let close = unsafe { mimi_mir_actor_domain_close(domain, 1, std::ptr::null_mut()) };
        let destroy = mimi_mir_actor_domain_destroy(domain);
        unsafe {
            *result = if close == MIR_ACTOR_STATUS_WORKER_CANNOT_CLOSE && destroy == close {
                42
            } else {
                0
            };
        }
        MIR_ACTOR_STATUS_SUCCESS
    }

    #[test]
    fn native_worker_close_and_destroy_request_cancel_without_self_join() {
        let domain = mimi_mir_actor_domain_open(0);
        WORKER_NATIVE_DOMAIN.store(domain, Ordering::Release);
        let monitor = native_monitor(domain);
        let mut actor = 0;
        let arity = [0u32];
        unsafe {
            assert_eq!(
                mimi_mir_actor_domain_spawn(
                    domain,
                    std::ptr::null(),
                    0,
                    arity.as_ptr(),
                    1,
                    Some(native_self_close),
                    &mut actor
                ),
                0
            );
            let mut result = 0;
            assert_eq!(
                mimi_mir_actor_domain_call(domain, actor, 0, std::ptr::null(), 0, &mut result),
                0
            );
            assert_eq!(result, 42);
            assert_eq!(
                mimi_mir_actor_domain_close(domain, 1, std::ptr::null_mut()),
                0
            );
        }
        assert_eq!(mimi_mir_actor_domain_destroy(domain), 0);
        quiescent(monitor.counts(), false);
    }
    #[test]
    fn checked_identity_exhaustion_never_wraps_reuses_or_issues_zero() {
        for counter in [AtomicU64::new(u64::MAX - 1), AtomicU64::new(0)] {
            if counter.load(Ordering::Acquire) != 0 {
                assert_eq!(allocate_identity(&counter), Ok(u64::MAX - 1));
            }
            let sentinel = counter.load(Ordering::Acquire);
            for _ in 0..3 {
                assert_eq!(
                    allocate_identity(&counter),
                    Err(ActorDomainError::IdentityExhausted)
                );
                assert_eq!(counter.load(Ordering::Acquire), sentinel);
            }
        }
        let counter = Arc::new(AtomicU64::new(u64::MAX - 1));
        let threads = (0..8)
            .map(|_| {
                let counter = Arc::clone(&counter);
                std::thread::spawn(move || allocate_identity(&counter))
            })
            .collect::<Vec<_>>();
        let results = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(counter.load(Ordering::Acquire), u64::MAX);
    }

    #[test]
    fn worker_cannot_upgrade_a_captured_rust_participant_or_access_another_root() {
        let mut first = RootDomainOwner::new(None).unwrap();
        let mut second = RootDomainOwner::new(None).unwrap();
        let a = first.participant();
        let b = second.participant();
        let other = counter(&b, 0);
        let captured_root = a.clone();
        let actor = a
            .spawn_actor(vec![], vec![0], move |_, _, _, _| {
                assert!(matches!(
                    RootDomainOwner::new(None),
                    Err(ActorDomainError::InvalidParticipant)
                ));
                assert_eq!(
                    b.call(&other, 0, &[5]),
                    Err(ActorDomainError::InvalidParticipant)
                );
                assert_eq!(
                    b.request_cancel(),
                    Err(ActorDomainError::InvalidParticipant)
                );
                assert!(matches!(
                    captured_root.spawn_actor(vec![], vec![0], |_, _, _, _| Ok(0)),
                    Err(ActorDomainError::InvalidParticipant)
                ));
                Ok(42)
            })
            .unwrap();
        assert_eq!(a.call(&actor, 0, &[]), Ok(42));
        quiescent(first.close(ActorDomainCloseMode::Drain).unwrap(), true);
        assert!(!second.participant().is_cancelled());
        quiescent(second.close(ActorDomainCloseMode::Drain).unwrap(), true);
    }

    #[test]
    fn poisoned_admission_rejects_work_but_cleanup_settles_and_joins() {
        let mut root = RootDomainOwner::new(None).unwrap();
        let participant = root.participant();
        let actor = counter(&participant, 0);
        let inner = participant.domain.upgrade().unwrap();
        let poison = Arc::clone(&inner);
        assert!(std::thread::spawn(move || {
            let _guard = poison.admission.lock().unwrap();
            panic!("injected admission poison");
        })
        .join()
        .is_err());
        assert!(matches!(
            participant.enqueue(&actor, 0, &[5]),
            Err(ActorDomainError::Poisoned)
        ));
        assert!(matches!(
            participant.spawn_actor(vec![], vec![0], |_, _, _, _| Ok(0)),
            Err(ActorDomainError::Poisoned)
        ));
        assert!(matches!(
            participant.spawn_task(|_| Ok(())),
            Err(ActorDomainError::Poisoned)
        ));
        assert_eq!(
            root.close(ActorDomainCloseMode::Drain),
            Err(ActorDomainError::Poisoned)
        );
        assert_eq!(
            root.close(ActorDomainCloseMode::Cancel),
            Err(ActorDomainError::Poisoned)
        );
        quiescent(root.monitor().counts(), true);
        assert!(inner.admission.is_poisoned());
    }

    #[test]
    fn concurrent_spawn_and_close_cannot_register_an_unjoined_worker() {
        let mut root = RootDomainOwner::new(None).unwrap();
        let participant = root.participant();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let ready = Arc::clone(&barrier);
        let thread = std::thread::spawn(move || {
            ready.wait();
            for _ in 0..128 {
                match participant.spawn_actor(vec![0], vec![0], |_, _, _, _| Ok(0)) {
                    Ok(_) => (),
                    Err(ActorDomainError::Cancelled | ActorDomainError::Closed) => break,
                    Err(error) => panic!("unexpected spawn failure {error:?}"),
                }
            }
        });
        barrier.wait();
        root.close(ActorDomainCloseMode::Cancel).unwrap();
        thread.join().unwrap();
        quiescent(root.monitor().counts(), true);
    }

    #[test]
    fn abandoned_reply_receiver_releases_settled_result_buffer() {
        let mut root = RootDomainOwner::new(None).unwrap();
        let participant = root.participant();
        let actor = counter(&participant, 0);
        let reply = participant.enqueue(&actor, 0, &[5]).unwrap();
        root.close(ActorDomainCloseMode::Drain).unwrap();
        assert_eq!(root.monitor().counts().live_reply_buffers, 1);
        drop(reply);
        quiescent(root.monitor().counts(), true);
    }

    #[test]
    fn owned_reply_slots_support_concurrent_root_and_structured_child_waits() {
        let mut root = RootDomainOwner::new(None).unwrap();
        let participant = root.participant();
        let actor = participant
            .spawn_actor(vec![0], vec![1], |fields, _, args, _| {
                fields[0] += args[0];
                Ok(fields[0])
            })
            .unwrap();
        let results = Arc::new(Mutex::new(Vec::new()));
        for _ in 0..4 {
            let actor = actor.clone();
            let results = Arc::clone(&results);
            participant
                .spawn_task(move |child| {
                    for _ in 0..32 {
                        let value = child.call(&actor, 0, &[1])?;
                        results.lock().unwrap().push(value);
                    }
                    Ok(())
                })
                .unwrap();
        }
        for _ in 0..32 {
            let value = participant.call(&actor, 0, &[1]).unwrap();
            results.lock().unwrap().push(value);
        }
        let counts = root.close(ActorDomainCloseMode::Drain).unwrap();
        quiescent(counts, true);
        assert_eq!(counts.accepted_messages, 160);
        assert_eq!(counts.joined_tasks, 4);
        let mut results = results.lock().unwrap().clone();
        results.sort_unstable();
        assert_eq!(results, (1..=160).collect::<Vec<_>>());
    }

    #[test]
    fn owned_reply_wait_rechecks_predicate_after_spurious_condvar_wakeup() {
        let mut root = RootDomainOwner::new(None).unwrap();
        let participant = root.participant();
        let (entered, running) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let actor = participant
            .spawn_actor(vec![], vec![0], move |_, _, _, _| {
                entered.send(()).unwrap();
                resume.recv_timeout(Duration::from_secs(3)).unwrap();
                Ok(42)
            })
            .unwrap();
        let pending = participant.enqueue(&actor, 0, &[]).unwrap();
        let slot = Arc::clone(&pending.0);
        running.recv_timeout(Duration::from_secs(3)).unwrap();
        let finished = Arc::new(AtomicBool::new(false));
        let waiter_finished = Arc::clone(&finished);
        let waiter = std::thread::spawn(move || {
            let result = pending.wait();
            waiter_finished.store(true, Ordering::Release);
            result
        });
        wait_until(|| slot.wait_cycles.load(Ordering::Acquire) >= 1);
        {
            let state = slot.state.lock().unwrap();
            assert!(!state.settled);
            slot.ready.notify_all();
        }
        // Observe the waiter processing this wake and entering another wait,
        // rather than merely notifying before the first wait was registered.
        wait_until(|| slot.wait_cycles.load(Ordering::Acquire) >= 2);
        assert!(!finished.load(Ordering::Acquire));
        assert_eq!(root.monitor().counts().pending_replies, 1);
        release.send(()).unwrap();
        assert_eq!(waiter.join().unwrap(), Ok(42));
        drop(slot);
        quiescent(root.close(ActorDomainCloseMode::Drain).unwrap(), true);
    }

    #[test]
    fn abandoning_unsettled_owned_reply_releases_slot_after_worker_completion() {
        let mut root = RootDomainOwner::new(None).unwrap();
        let participant = root.participant();
        let (entered, running) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let actor = participant
            .spawn_actor(vec![], vec![0], move |_, _, _, _| {
                entered.send(()).unwrap();
                resume.recv_timeout(Duration::from_secs(3)).unwrap();
                Ok(42)
            })
            .unwrap();
        let pending = participant.enqueue(&actor, 0, &[]).unwrap();
        running.recv_timeout(Duration::from_secs(3)).unwrap();
        let slot = Arc::downgrade(&pending.0);
        drop(pending);
        assert!(
            slot.upgrade().is_some(),
            "active ticket owns its unsettled slot"
        );
        release.send(()).unwrap();
        quiescent(root.close(ActorDomainCloseMode::Drain).unwrap(), true);
        assert!(
            slot.upgrade().is_none(),
            "worker completion releases the last slot owner"
        );
        assert_eq!(root.monitor().counts().settled_replies, 1);
    }

    #[test]
    fn poisoned_unsettled_owned_reply_cancels_domain_and_waits_for_receipt_settlement() {
        let mut root = RootDomainOwner::new(None).unwrap();
        let participant = root.participant();
        let (entered, running) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let actor = participant
            .spawn_actor(vec![], vec![0], move |_, _, _, _| {
                entered.send(()).unwrap();
                resume.recv_timeout(Duration::from_secs(3)).unwrap();
                Ok(42)
            })
            .unwrap();
        let pending = participant.enqueue(&actor, 0, &[]).unwrap();
        running.recv_timeout(Duration::from_secs(3)).unwrap();
        let poison = Arc::clone(&pending.0);
        assert!(std::thread::spawn(move || {
            let _state = poison.state.lock().unwrap();
            panic!("injected unsettled reply slot poison");
        })
        .join()
        .is_err());
        let cancelling = participant.clone();
        let releaser = std::thread::spawn(move || {
            wait_until(|| cancelling.is_cancelled());
            release.send(()).unwrap();
        });
        assert_eq!(pending.wait(), Err(ActorDomainError::Poisoned));
        assert_eq!(root.monitor().counts().pending_replies, 0);
        releaser.join().unwrap();
        assert_eq!(
            root.close(ActorDomainCloseMode::Drain),
            Err(ActorDomainError::Poisoned)
        );
        assert_eq!(
            root.close(ActorDomainCloseMode::Cancel),
            Err(ActorDomainError::Poisoned)
        );
        quiescent(root.monitor().counts(), true);
    }

    #[test]
    fn poisoned_settled_owned_reply_rejects_success_and_releases_its_buffer() {
        let mut root = RootDomainOwner::new(None).unwrap();
        let participant = root.participant();
        let actor = counter(&participant, 0);
        let pending = participant.enqueue(&actor, 0, &[5]).unwrap();
        root.close(ActorDomainCloseMode::Drain).unwrap();
        assert_eq!(root.monitor().counts().live_reply_buffers, 1);
        let poison = Arc::clone(&pending.0);
        assert!(std::thread::spawn(move || {
            let _state = poison.state.lock().unwrap();
            panic!("injected settled reply slot poison");
        })
        .join()
        .is_err());
        assert_eq!(pending.wait(), Err(ActorDomainError::Poisoned));
        assert_eq!(
            root.close(ActorDomainCloseMode::Cancel),
            Err(ActorDomainError::Poisoned)
        );
        quiescent(root.monitor().counts(), true);
        let monitor = root.monitor();
        drop(root);
        quiescent(monitor.counts(), false);
    }

    #[test]
    fn panicked_turn_cancels_idle_workers_and_records_failure() {
        let mut root = RootDomainOwner::new(None).unwrap();
        let participant = root.participant();
        let bad = participant
            .spawn_actor(vec![], vec![0], |_, _, _, _| panic!("injected turn panic"))
            .unwrap();
        let _idle = counter(&participant, 0);
        assert_eq!(
            participant.call(&bad, 0, &[]),
            Err(ActorDomainError::WorkerPanicked)
        );
        assert_eq!(
            root.close(ActorDomainCloseMode::Drain),
            Err(ActorDomainError::WorkerPanicked)
        );
        quiescent(root.monitor().counts(), true);
    }

    static FOREIGN_NATIVE_DOMAIN: AtomicU64 = AtomicU64::new(0);
    static FOREIGN_NATIVE_ACTOR: AtomicU64 = AtomicU64::new(0);

    unsafe extern "C" fn native_foreign_access(
        _: *mut i32,
        _: u64,
        _: u32,
        _: *const i32,
        _: u64,
        result: *mut i32,
    ) -> i32 {
        let foreign = FOREIGN_NATIVE_DOMAIN.load(Ordering::Acquire);
        let other = FOREIGN_NATIVE_ACTOR.load(Ordering::Acquire);
        let mut output = -999;
        let call =
            unsafe { mimi_mir_actor_domain_call(foreign, other, 0, [5].as_ptr(), 1, &mut output) };
        let mut spawned = 99;
        let spawn = unsafe {
            mimi_mir_actor_domain_spawn(
                foreign,
                [0].as_ptr(),
                1,
                [1].as_ptr(),
                1,
                Some(native_counter),
                &mut spawned,
            )
        };
        let close = unsafe { mimi_mir_actor_domain_close(foreign, 1, std::ptr::null_mut()) };
        let destroy = mimi_mir_actor_domain_destroy(foreign);
        let open = mimi_mir_actor_domain_open(1);
        let own = CURRENT_DOMAIN_EXECUTION.with(|current| current.get().unwrap().0);
        let own_spawn = unsafe {
            mimi_mir_actor_domain_spawn(
                own,
                [0].as_ptr(),
                1,
                [1].as_ptr(),
                1,
                Some(native_counter),
                &mut spawned,
            )
        };
        unsafe {
            *result = if [call, spawn, close, destroy, own_spawn]
                .iter()
                .all(|status| *status == MIR_ACTOR_STATUS_INVALID_PARTICIPANT)
                && output == -999
                && open == 0
            {
                42
            } else {
                0
            };
        }
        0
    }

    #[test]
    fn native_worker_cannot_borrow_foreign_token_as_root_or_close_another_domain() {
        let first = mimi_mir_actor_domain_open(1);
        let second = mimi_mir_actor_domain_open(1);
        let first_monitor = native_monitor(first);
        let second_monitor = native_monitor(second);
        let mut other = 0;
        let mut worker = 0;
        unsafe {
            assert_eq!(
                mimi_mir_actor_domain_spawn(
                    second,
                    [0].as_ptr(),
                    1,
                    [1].as_ptr(),
                    1,
                    Some(native_counter),
                    &mut other
                ),
                0
            );
            FOREIGN_NATIVE_DOMAIN.store(second, Ordering::Release);
            FOREIGN_NATIVE_ACTOR.store(other, Ordering::Release);
            assert_eq!(
                mimi_mir_actor_domain_spawn(
                    first,
                    std::ptr::null(),
                    0,
                    [0].as_ptr(),
                    1,
                    Some(native_foreign_access),
                    &mut worker
                ),
                0
            );
            let mut result = 0;
            assert_eq!(
                mimi_mir_actor_domain_call(first, worker, 0, std::ptr::null(), 0, &mut result),
                0
            );
            assert_eq!(result, 42);
            assert_eq!(
                mimi_mir_actor_domain_call(second, other, 0, [5].as_ptr(), 1, &mut result),
                0
            );
            assert_eq!(result, 15);
            assert_eq!(
                mimi_mir_actor_domain_close(first, 0, std::ptr::null_mut()),
                0
            );
            assert_eq!(
                mimi_mir_actor_domain_close(second, 0, std::ptr::null_mut()),
                0
            );
        }
        assert_eq!(mimi_mir_actor_domain_destroy(first), 0);
        assert_eq!(mimi_mir_actor_domain_destroy(second), 0);
        quiescent(first_monitor.counts(), false);
        quiescent(second_monitor.counts(), false);
    }

    #[test]
    fn poisoned_native_owner_never_admits_successful_work_and_destroy_still_joins() {
        let domain = mimi_mir_actor_domain_open(1);
        let monitor = native_monitor(domain);
        let mut actor = 0;
        unsafe {
            assert_eq!(
                mimi_mir_actor_domain_spawn(
                    domain,
                    [0].as_ptr(),
                    1,
                    [1].as_ptr(),
                    1,
                    Some(native_counter),
                    &mut actor
                ),
                0
            );
        }
        let entry = native_roots().lock().unwrap().get(&domain).unwrap().clone();
        assert!(std::thread::spawn(move || {
            let _guard = entry.owner.lock().unwrap();
            panic!("injected native owner poison");
        })
        .join()
        .is_err());
        let mut output = 77;
        unsafe {
            assert_eq!(
                mimi_mir_actor_domain_call(domain, actor, 0, [5].as_ptr(), 1, &mut output),
                MIR_ACTOR_STATUS_POISONED
            );
            assert_eq!(output, 77);
            assert_eq!(
                mimi_mir_actor_domain_close(domain, 0, std::ptr::null_mut()),
                MIR_ACTOR_STATUS_POISONED
            );
        }
        assert_eq!(
            mimi_mir_actor_domain_destroy(domain),
            MIR_ACTOR_STATUS_POISONED
        );
        quiescent(monitor.counts(), false);
    }

    #[test]
    fn native_lookup_close_destroy_races_pin_entries_until_join() {
        let domain = mimi_mir_actor_domain_open(1);
        let monitor = native_monitor(domain);
        let mut actor = 0;
        unsafe {
            assert_eq!(
                mimi_mir_actor_domain_spawn(
                    domain,
                    [0].as_ptr(),
                    1,
                    [1].as_ptr(),
                    1,
                    Some(native_counter),
                    &mut actor
                ),
                0
            );
        }
        let barrier = Arc::new(std::sync::Barrier::new(9));
        let threads = (0..8)
            .map(|index| {
                let ready = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    ready.wait();
                    if index == 0 {
                        let status = mimi_mir_actor_domain_destroy(domain);
                        assert!(matches!(status, 0 | MIR_ACTOR_STATUS_CLOSED));
                    } else if index % 2 == 0 {
                        let status =
                            unsafe { mimi_mir_actor_domain_close(domain, 1, std::ptr::null_mut()) };
                        assert!(matches!(status, 0 | MIR_ACTOR_STATUS_CLOSED));
                    } else {
                        let mut output = -99;
                        let status = unsafe {
                            mimi_mir_actor_domain_call(
                                domain,
                                actor,
                                0,
                                [5].as_ptr(),
                                1,
                                &mut output,
                            )
                        };
                        assert!(matches!(
                            status,
                            0 | MIR_ACTOR_STATUS_CLOSED | MIR_ACTOR_STATUS_CANCELLED
                        ));
                    }
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        for thread in threads {
            thread.join().unwrap();
        }
        assert!(matches!(
            mimi_mir_actor_domain_destroy(domain),
            0 | MIR_ACTOR_STATUS_CLOSED
        ));
        quiescent(monitor.counts(), false);
        assert_eq!(
            mimi_mir_actor_domain_destroy(domain),
            MIR_ACTOR_STATUS_CLOSED
        );
    }

    #[test]
    fn native_cancel_close_or_destroy_wakes_children_while_drain_holds_join_lock() {
        for destroy in [false, true] {
            let domain = mimi_mir_actor_domain_open(0);
            let monitor = native_monitor(domain);
            let participant = native_participant(domain).unwrap();
            let inner = participant.domain.upgrade().unwrap();
            let (entered, running) = mpsc::channel();
            participant
                .spawn_task(move |child| {
                    entered.send(()).unwrap();
                    wait_until(|| child.is_cancelled());
                    Ok(())
                })
                .unwrap();
            running.recv_timeout(Duration::from_secs(3)).unwrap();
            let drain = std::thread::spawn(move || unsafe {
                mimi_mir_actor_domain_close(domain, 0, std::ptr::null_mut())
            });
            wait_until(|| inner.admission.lock().unwrap().phase == Phase::SettlingChildren);
            let status = if destroy {
                mimi_mir_actor_domain_destroy(domain)
            } else {
                unsafe { mimi_mir_actor_domain_close(domain, 1, std::ptr::null_mut()) }
            };
            assert_eq!(status, MIR_ACTOR_STATUS_SUCCESS);
            assert_eq!(drain.join().unwrap(), MIR_ACTOR_STATUS_SUCCESS);
            quiescent(monitor.counts(), !destroy);
            assert_eq!(monitor.counts().joined_tasks, 1);
            if !destroy {
                assert_eq!(
                    mimi_mir_actor_domain_destroy(domain),
                    MIR_ACTOR_STATUS_SUCCESS
                );
                quiescent(monitor.counts(), false);
            }
        }
    }

    #[test]
    #[ignore = "global poison must run in an isolated child process"]
    fn native_registry_poison_subprocess_only() {
        let domain = mimi_mir_actor_domain_open(1);
        let monitor = native_monitor(domain);
        let participant = native_participant(domain).unwrap();
        let mut actor = 0;
        unsafe {
            assert_eq!(
                mimi_mir_actor_domain_spawn(
                    domain,
                    [0].as_ptr(),
                    1,
                    [1].as_ptr(),
                    1,
                    Some(native_counter),
                    &mut actor
                ),
                0
            );
        }
        assert!(std::thread::spawn(|| {
            let _guard = native_roots().lock().unwrap();
            panic!("injected native registry poison");
        })
        .join()
        .is_err());
        assert_eq!(mimi_mir_actor_domain_open(0), 0);
        let mut result = 77;
        unsafe {
            assert_eq!(
                mimi_mir_actor_domain_call(domain, actor, 0, [5].as_ptr(), 1, &mut result),
                MIR_ACTOR_STATUS_POISONED
            );
            assert_eq!(result, 77);
            assert!(participant.is_cancelled());
            assert_eq!(
                mimi_mir_actor_domain_close(domain, 0, std::ptr::null_mut()),
                MIR_ACTOR_STATUS_POISONED
            );
        }
        assert_eq!(
            mimi_mir_actor_domain_destroy(domain),
            MIR_ACTOR_STATUS_POISONED
        );
        quiescent(monitor.counts(), false);
        assert!(native_roots().is_poisoned());
    }

    #[test]
    fn native_registry_poison_is_explicit_and_cleanup_runs_in_isolated_process() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "runtime::actor_domain::tests::native_registry_poison_subprocess_only",
                "--ignored",
                "--test-threads=1",
            ])
            .output()
            .unwrap();
        let transcript = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.status.success(), "{transcript}");
        assert!(
            transcript
                .lines()
                .any(|line| line.trim() == "running 1 test"),
            "expected child transcript to report exactly one test executed; got:\n{transcript}"
        );
    }
    #[test]
    fn structured_child_error_wakes_pending_reply_and_joins_idle_siblings() {
        let mut root = RootDomainOwner::new(None).unwrap();
        let participant = root.participant();
        let (entered, running) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let active = participant
            .spawn_actor(vec![], vec![0], move |_, _, _, _| {
                entered.send(()).unwrap();
                resume.recv_timeout(Duration::from_secs(3)).unwrap();
                Ok(42)
            })
            .unwrap();
        let _idle = counter(&participant, 0);
        let first = participant.enqueue(&active, 0, &[]).unwrap();
        running.recv_timeout(Duration::from_secs(3)).unwrap();
        let queued = participant.enqueue(&active, 0, &[]).unwrap();
        participant
            .spawn_task(|_| Err(ActorDomainError::Trap(800)))
            .unwrap();
        wait_until(|| participant.is_cancelled());
        release.send(()).unwrap();
        assert_eq!(first.wait(), Ok(42));
        assert_eq!(queued.wait(), Err(ActorDomainError::Cancelled));
        assert_eq!(
            root.close(ActorDomainCloseMode::Drain),
            Err(ActorDomainError::Trap(800))
        );
        quiescent(root.monitor().counts(), true);
        assert_eq!(root.monitor().counts().joined_tasks, 1);
    }

    #[test]
    fn native_child_role_allows_only_its_own_active_domain_work() {
        let first = mimi_mir_actor_domain_open(1);
        let second = mimi_mir_actor_domain_open(1);
        let first_monitor = native_monitor(first);
        let second_monitor = native_monitor(second);
        let mut own = 0;
        let mut other = 0;
        unsafe {
            assert_eq!(
                mimi_mir_actor_domain_spawn(
                    first,
                    [0].as_ptr(),
                    1,
                    [1].as_ptr(),
                    1,
                    Some(native_counter),
                    &mut own
                ),
                0
            );
            assert_eq!(
                mimi_mir_actor_domain_spawn(
                    second,
                    [0].as_ptr(),
                    1,
                    [1].as_ptr(),
                    1,
                    Some(native_counter),
                    &mut other
                ),
                0
            );
        }
        native_participant(first)
            .unwrap()
            .spawn_task(move |_| {
                let mut result = -99;
                unsafe {
                    assert_eq!(
                        mimi_mir_actor_domain_call(first, own, 0, [5].as_ptr(), 1, &mut result),
                        0
                    );
                    assert_eq!(result, 15);
                    assert_eq!(
                        mimi_mir_actor_domain_call(second, other, 0, [5].as_ptr(), 1, &mut result),
                        MIR_ACTOR_STATUS_INVALID_PARTICIPANT
                    );
                    assert_eq!(
                        mimi_mir_actor_domain_close(second, 1, std::ptr::null_mut()),
                        MIR_ACTOR_STATUS_INVALID_PARTICIPANT
                    );
                }
                assert_eq!(
                    mimi_mir_actor_domain_destroy(second),
                    MIR_ACTOR_STATUS_INVALID_PARTICIPANT
                );
                Ok(())
            })
            .unwrap();
        unsafe {
            assert_eq!(
                mimi_mir_actor_domain_close(first, 0, std::ptr::null_mut()),
                0
            );
            let mut result = -99;
            assert_eq!(
                mimi_mir_actor_domain_call(second, other, 0, [5].as_ptr(), 1, &mut result),
                0
            );
            assert_eq!(result, 15);
            assert_eq!(
                mimi_mir_actor_domain_close(second, 0, std::ptr::null_mut()),
                0
            );
        }
        assert_eq!(mimi_mir_actor_domain_destroy(first), 0);
        assert_eq!(mimi_mir_actor_domain_destroy(second), 0);
        quiescent(first_monitor.counts(), false);
        quiescent(second_monitor.counts(), false);
    }
}
