#![allow(dead_code)]

use crate::ast::*;
use crate::interp::error::InterpError;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock, Weak as ArcWeak};

/// Poll-based future state.
/// For async fn: deferred (waiting to be evaluated by executor).
/// For actor spawn: Pending with a channel receiver (polled on await).
/// For immediately-ready: Ready with result.
pub enum PollFuture {
    Deferred {
        file: Box<crate::ast::File>,
        func: FuncDef,
        args: Vec<Value>,
    },
    Pending(std::sync::mpsc::Receiver<Result<Value, InterpError>>),
    Ready(Result<Value, InterpError>),
}

impl std::fmt::Debug for PollFuture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PollFuture::Deferred { func, .. } => {
                write!(f, "PollFuture::Deferred({})", func.name)
            }
            PollFuture::Pending(_) => write!(f, "PollFuture::Pending"),
            PollFuture::Ready(result) => match result {
                Ok(v) => write!(f, "PollFuture::Ready(Ok({:?}))", v),
                Err(e) => write!(f, "PollFuture::Ready(Err({}))", e),
            },
        }
    }
}

/// Poll a deferred future: evaluate the function body and store the result.
pub fn poll_deferred(state: &mut PollFuture) {
    if let PollFuture::Deferred { file, func, args } = state {
        let mut interp = super::Interpreter::new(&*file);
        interp.push_scope();
        let mut result = Ok(Value::Unit);
        for (p, a) in func.params.iter().zip(std::mem::take(args)) {
            if let Err(e) = interp.bind(&p.name, a) {
                result = Err(e);
                break;
            }
        }
        if result.is_ok() {
            let block_result = interp
                .eval_block(&func.body)
                .map(|v| v.unwrap_or(Value::Unit));
            result = interp.early_return.take().map_or(block_result, Ok);
        }
        interp.pop_scope();
        *state = PollFuture::Ready(result);
    }
}

/// Global executor queue for deferred futures.
fn executor_queue() -> &'static std::sync::Mutex<Vec<std::sync::Arc<std::sync::Mutex<PollFuture>>>>
{
    use std::sync::Mutex;
    static QUEUE: std::sync::OnceLock<Mutex<Vec<std::sync::Arc<Mutex<PollFuture>>>>> =
        std::sync::OnceLock::new();
    QUEUE.get_or_init(|| Mutex::new(Vec::new()))
}

/// Submit a deferred future to the global executor.
pub fn executor_submit(future: std::sync::Arc<std::sync::Mutex<PollFuture>>) {
    executor_queue()
        .lock()
        .expect("executor queue lock")
        .push(future);
}

/// Run the executor: poll all deferred futures until all are completed.
pub fn executor_run() {
    loop {
        let entry = {
            let queue = executor_queue();
            let mut guard = queue.lock().expect("executor queue lock");
            if guard.is_empty() {
                return;
            }
            // Remove all completed futures before looking for Deferred ones
            guard.retain(|fut| {
                let state = fut.lock().expect("future lock");
                !matches!(&*state, PollFuture::Ready(_))
            });
            let mut found = None;
            for i in 0..guard.len() {
                let fut = &guard[i];
                let state = fut.lock().expect("future lock");
                match &*state {
                    PollFuture::Deferred { .. } => {
                        found = Some(i);
                        break;
                    }
                    PollFuture::Pending(_) => {}
                    PollFuture::Ready(_) => {}
                }
            }
            match found {
                Some(i) => {
                    let fut = guard.swap_remove(i);
                    Some(fut)
                }
                None => return,
            }
        };
        if let Some(fut) = entry {
            let mut state = fut.lock().expect("future lock");
            poll_deferred(&mut state);
        }
    }
}

#[derive(Debug, Clone)]
pub enum QuotedAst {
    Literal(Lit),
    Ident(String),
    Binary(BinOp, Box<QuotedAst>, Box<QuotedAst>),
    Unary(UnOp, Box<QuotedAst>),
    Call(Box<QuotedAst>, Vec<QuotedAst>),
    Field(Box<QuotedAst>, String),
    Index(Box<QuotedAst>, Box<QuotedAst>),
    Tuple(Vec<QuotedAst>),
    List(Vec<QuotedAst>),
    Match(Box<QuotedAst>, Vec<MatchArmQuoted>),
    If(Box<QuotedAst>, Box<QuotedAst>, Option<Box<QuotedAst>>),
    Record {
        ty: Option<String>,
        fields: Vec<RecordFieldExprQuoted>,
    },
    Try(Box<QuotedAst>),
    Spawn(Box<QuotedAst>),
    Await(Box<QuotedAst>),
    Interpolate(Box<Value>),
    Block(Vec<QuotedAst>),
    Let {
        name: String,
        value: Box<QuotedAst>,
    },
    ExprStmt(Box<QuotedAst>),
    Return(Option<Box<QuotedAst>>),
    Break(Option<Box<QuotedAst>>),
    Continue,
    While(Box<QuotedAst>, Box<QuotedAst>),
    Loop(Box<QuotedAst>),
    For(String, Box<QuotedAst>, Box<QuotedAst>),
    Assign(Box<QuotedAst>, Box<QuotedAst>),
    Arena(Box<QuotedAst>),
    Unsafe(Box<QuotedAst>),
    Drop(Box<QuotedAst>),
    SharedLet {
        kind: SharedKind,
        name: String,
        init: Box<QuotedAst>,
    },
    OnFailure(Box<QuotedAst>),
    Parasteps(Box<QuotedAst>),
    Alloc {
        kind: AllocKind,
        body: Box<QuotedAst>,
    },
    NamedArg(String, Box<QuotedAst>),
    MapLiteral(Vec<(QuotedAst, QuotedAst)>),
    SetLiteral(Vec<QuotedAst>),
}

#[derive(Debug, Clone)]
pub struct RecordFieldExprQuoted {
    pub name: String,
    pub value: QuotedAst,
}

#[derive(Debug, Clone)]
pub struct MatchArmQuoted {
    pub pat: Pattern,
    pub guard: Option<QuotedAst>,
    pub body: QuotedAst,
}

#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
    Unit,
    List(Vec<Value>),
    Set(Vec<Value>),
    Tuple(Vec<Value>),
    Variant(String, Vec<Value>),
    Record(Option<String>, HashMap<String, Value>),
    /// Poll-based future. Can be Ready (result available) or Pending (waiting on channel).
    Future(std::sync::Arc<std::sync::Mutex<crate::interp::PollFuture>>),
    Error(String),
    ArenaRef(usize, usize, u64), // (arena_id, slot_idx, generation)
    ArenaBlock(usize),
    QuoteAst(Box<QuotedAst>),
    Newtype(String, Box<Value>),
    Actor(super::ActorHandle),
    Closure {
        params: Vec<Param>,
        ret: Option<Type>,
        body: Block,
        captured: HashMap<String, Value>,
    },
    Shared(Arc<RwLock<Value>>),
    LocalShared(LocalSharedInner),
    WeakShared(ArcWeak<RwLock<Value>>),
    WeakLocal(WeakLocalInner),
    Cap(Vec<String>),
    /// Immutable reference: `&T`
    Ref(Arc<RwLock<Value>>),
    /// Mutable reference: `&mut T`
    RefMut(Arc<RwLock<Value>>),
    /// Borrowed immutable list element: `&xs[i]`
    IndexRef {
        owner: String,
        index: usize,
    },
    /// Borrowed mutable list element: `&mut xs[i]`
    IndexRefMut {
        owner: String,
        index: usize,
    },
    /// Type descriptor for comptime reflection
    Type(String),
    /// Allocator type value
    Allocator(AllocatorKind),
    /// Fixed-size array value
    Array(Vec<Value>),
    /// Slice value: a view into a list/array
    Slice {
        source: Vec<Value>,
        start: usize,
        end: usize,
    },
    /// Range value: start..end
    Range {
        start: i64,
        end: i64,
    },
    /// C buffer with automatic memory management (malloc/free)
    CBuffer(std::sync::Arc<CBufferInner>),
    /// Dynamic trait object: concrete value + trait name for vtable dispatch
    DynTrait {
        data: Box<Value>,
        concrete_type: String,
        trait_names: Vec<String>,
    },
}

/// Wrapper around `Arc<Mutex<Value>>` for LocalShared.
/// The Mutex serializes access to the wrapped Value; the Arc allows sharing
/// within a single thread (Mimi's type-checker rejects local_shared in
/// parallel blocks via E0305). The type is nevertheless Send + Sync because
/// `Arc<Mutex<Value>>` is thread-safe when Value is Send + Sync.
#[derive(Debug, Clone)]
pub struct LocalSharedInner(pub Arc<Mutex<Value>>);

// SAFETY: LocalSharedInner is a transparent wrapper around Arc<Mutex<Value>>.
// Arc<Mutex<Value>> is Send + Sync because Value is Send + Sync (all variants
// use thread-safe ownership: Arc/RwLock/Mutex/String/Vec/HashMap/etc.; raw
// pointers live behind Arc<CBufferInner>). The Mutex serializes all access to
// the wrapped Value, so sharing across threads is data-race free. Mimi's
// type-checker additionally rejects local_shared in parallel blocks (E0305),
// but the impl remains sound on its own.
unsafe impl Send for LocalSharedInner {}
unsafe impl Sync for LocalSharedInner {}

impl std::ops::Deref for LocalSharedInner {
    type Target = Mutex<Value>;
    fn deref(&self) -> &Mutex<Value> {
        &self.0
    }
}

impl LocalSharedInner {
    pub fn new(v: Value) -> Self {
        LocalSharedInner(Arc::new(Mutex::new(v)))
    }
    pub fn downgrade(&self) -> WeakLocalInner {
        WeakLocalInner(Arc::downgrade(&self.0))
    }
    pub fn clone_rc(this: &Self) -> Self {
        LocalSharedInner(Arc::clone(&this.0))
    }
}

/// Wrapper around `Arc<Mutex<Value>>` weak reference for WeakLocal.
#[derive(Debug, Clone)]
pub struct WeakLocalInner(pub ArcWeak<Mutex<Value>>);

// SAFETY: WeakLocalInner wraps ArcWeak<Mutex<Value>>. ArcWeak is Send + Sync
// when its target (Mutex<Value>) is Send + Sync, which holds because Value is
// Send + Sync. Upgrading yields a LocalSharedInner that shares the same
// Mutex-protected Value. The type-checker restricts local_shared/weak_local to
// single-threaded use, but the trait impls are sound independently.
unsafe impl Send for WeakLocalInner {}
unsafe impl Sync for WeakLocalInner {}

impl WeakLocalInner {
    pub fn upgrade(&self) -> Option<LocalSharedInner> {
        self.0.upgrade().map(LocalSharedInner)
    }
}

/// Kind of allocator
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocatorKind {
    System,
    Arena,
    Bump,
}

/// C buffer wrapper that automatically frees memory on drop
pub struct CBufferInner {
    pub ptr: *mut u8,
    pub size: usize,
}

// SAFETY: CBufferInner is Send because it uniquely owns a heap-allocated buffer
// (ptr/size) obtained from libc::malloc/calloc and freed exactly once in Drop.
// Moving the value across threads does not alias or split ownership of the
// buffer; only the final Drop frees it.
unsafe impl Send for CBufferInner {}
// SAFETY: CBufferInner is Sync because its fields are a raw pointer and a usize,
// both of which are Sync. Shared references (e.g. through Arc<CBufferInner>)
// only read ptr/size or run Drop once; actual buffer access is synchronized by
// the FFI contract / runtime.
unsafe impl Sync for CBufferInner {}

impl Drop for CBufferInner {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // SAFETY: ptr is a valid non-null pointer previously allocated by libc::malloc/calloc.
            unsafe {
                libc::free(self.ptr as *mut libc::c_void);
            }
        }
    }
}

impl std::fmt::Debug for CBufferInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CBuffer({:p}, {} bytes)", self.ptr, self.size)
    }
}

#[derive(Debug, Clone)]
pub struct Arena {
    pub id: usize,
    pub slots: Vec<Value>,
    /// Generation counter: incremented on reset. ArenaRef values with stale
    /// generation are invalidated to prevent use-after-reset bugs.
    pub generation: u64,
}

#[derive(Debug, Clone)]
pub struct ActorInstance {
    pub actor_name: String,
    pub fields: HashMap<String, Value>,
    pub methods: Vec<FuncDef>,
    /// v0.29.11: set when the owning Flow entered Fault. Mailbox dispatch
    /// short-circuits (O(1)) while this is true — messages are dropped without
    /// waking business logic.
    pub faulted: bool,
    /// v0.29.20: peer actor ids linked for PeerFault propagation.
    /// When this actor faults, each peer receives a `peer_fault` notification
    /// (link-disconnect injection). Stored as actor ids (not handles) to avoid
    /// reference cycles; peers are resolved via the global actor registry.
    pub peer_links: Vec<usize>,
}

/// Message sent to an actor's mailbox for FIFO processing.
pub struct ActorMailboxMsg {
    pub method: String,
    pub args: Vec<Value>,
    pub response: std::sync::mpsc::Sender<Result<Value, InterpError>>,
}

/// Handle to a running actor with per-actor mailbox + dedicated worker thread.
#[derive(Debug, Clone)]
pub struct ActorHandle {
    pub inner: std::sync::Arc<std::sync::RwLock<ActorInstance>>,
    pub mailbox: std::sync::mpsc::Sender<ActorMailboxMsg>,
    pub id: usize,
    /// Shared program AST. v0.28.28 fix for #1: worker threads must be able
    /// to call user-defined functions / resolve user types when executing
    /// actor methods. The worker dereferences this `Arc<File>` (cheap, no
    /// full AST clone per dispatch) to construct a per-call `Interpreter`.
    pub program: std::sync::Arc<crate::ast::File>,
}

// SAFETY: ActorHandle is Send because all fields are Send: Arc<RwLock<ActorInstance>>
// is Send when ActorInstance is Send+Sync (it holds only String/HashMap/Vec/Value);
// mpsc::Sender<ActorMailboxMsg> is Send because ActorMailboxMsg (Vec<Value>, String,
// Sender<Result<Value, InterpError>>) is Send; usize is Send.
unsafe impl Send for ActorHandle {}
// SAFETY: ActorHandle is Sync because all fields are Sync: Arc<RwLock<ActorInstance>>
// is Sync when ActorInstance is Send+Sync; mpsc::Sender<T> is Sync when T: Send;
// ActorMailboxMsg is Send because Value and InterpError are Send+Sync; usize is Sync.
unsafe impl Sync for ActorHandle {}

impl PartialEq for ActorHandle {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

static ACTOR_HANDLE_COUNTER: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Live actor handles by id for PeerFault peer resolution (v0.29.20).
/// Entries are inserted in `ActorHandle::new` and removed on short-circuit.
static ACTOR_HANDLES: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<usize, ActorHandle>>,
> = std::sync::OnceLock::new();

fn actor_handles() -> &'static std::sync::Mutex<std::collections::HashMap<usize, ActorHandle>> {
    ACTOR_HANDLES.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

// Thread-local flag set when inside an actor's worker thread.
// Used to detect self-calls and avoid mailbox deadlock.
thread_local! {
    static CURRENT_ACTOR_ID: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl ActorHandle {
    /// Creates a new actor, spawns its worker thread.
    ///
    /// `program` is the AST of the file that spawned this actor. The worker
    /// uses it to construct per-call `Interpreter`s that can resolve
    /// user-defined functions, types, and actors. Without this, actor
    /// methods cannot call any user code (see mimichat gap #1, fixed in
    /// v0.28.28).
    pub(crate) fn new(instance: ActorInstance, program: std::sync::Arc<crate::ast::File>) -> Self {
        let id = ACTOR_HANDLE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let (mailbox_tx, mailbox_rx) = std::sync::mpsc::channel::<ActorMailboxMsg>();
        let inner = std::sync::Arc::new(std::sync::RwLock::new(instance));

        let worker_inner = inner.clone();
        let mailbox_tx_clone = mailbox_tx.clone();
        let worker_program = program.clone();
        std::thread::Builder::new()
            .name(format!("actor-{}", id))
            .spawn(move || {
                CURRENT_ACTOR_ID.with(|a| a.set(id));
                while let Ok(msg) = mailbox_rx.recv() {
                    // v0.29.11: Fault absorption — drain without dispatch.
                    if worker_inner
                        .read()
                        .map(|a| a.faulted)
                        .unwrap_or(true)
                    {
                        let _ = msg.response.send(Err(InterpError::new(
                            "actor mailbox short-circuited (Fault)",
                        )));
                        continue;
                    }
                    let result = {
                        // Read method definition
                        let (func, _actor_name) = {
                            let actor = worker_inner.read().expect("actor worker lock");
                            let func = actor
                                .methods
                                .iter()
                                .find(|f| f.name == msg.method)
                                .cloned()
                                .expect("actor method not found");
                            (func, actor.actor_name.clone())
                        };
                        // v0.28.28: reuse the spawning program's AST so
                        // user-defined functions / types resolve inside
                        // the actor method body.
                        let mut interp = crate::interp::Interpreter::new(&worker_program);
                        let self_val = Value::Actor(ActorHandle {
                            inner: worker_inner.clone(),
                            mailbox: mailbox_tx_clone.clone(),
                            id,
                            program: worker_program.clone(),
                        });
                        interp.push_scope();
                        interp
                            .bind("self", self_val)
                            .expect("bind self in actor worker");
                        // Bind method parameters
                        let mut args_iter = msg.args.iter();
                        for param in &func.params {
                            if param.name == "self" {
                                continue;
                            }
                            let arg = args_iter.next().cloned().unwrap_or(Value::Unit);
                            interp
                                .bind(&param.name, arg)
                                .expect("bind param in actor worker");
                        }
                        let result = interp
                            .eval_block(&func.body)
                            .map(|opt| opt.unwrap_or(Value::Unit));
                        interp.pop_scope();
                        result
                    };
                    let _ = msg.response.send(result);
                }
                CURRENT_ACTOR_ID.with(|a| a.set(0));
            })
            .expect("failed to spawn actor worker");

        let handle = ActorHandle {
            inner,
            mailbox: mailbox_tx,
            id,
            program,
        };
        // v0.29.20: register for PeerFault peer resolution.
        if let Ok(mut map) = actor_handles().lock() {
            map.insert(id, handle.clone());
        }
        handle
    }

    /// Returns the current actor's thread-local ID (0 if not in an actor worker).
    pub(crate) fn current_worker_id() -> usize {
        CURRENT_ACTOR_ID.with(|a| a.get())
    }

    /// v0.29.20: register a bidirectional peer link for PeerFault injection.
    pub(crate) fn link_peer(&self, peer: &ActorHandle) {
        if self.id == peer.id {
            return;
        }
        if let Ok(mut actor) = self.inner.write() {
            if !actor.peer_links.contains(&peer.id) {
                actor.peer_links.push(peer.id);
            }
        }
        if let Ok(mut actor) = peer.inner.write() {
            if !actor.peer_links.contains(&self.id) {
                actor.peer_links.push(self.id);
            }
        }
    }

    /// v0.29.20: notify all linked peers that this actor has faulted.
    /// Peers receive a mailbox message `peer_fault` with a PeerFault payload
    /// description; if they are Flow-backed actors the message is drained by
    /// the short-circuit path when already faulted, otherwise the method is
    /// invoked if defined (user handlers). For Flow-level peer_fault, callers
    /// should use `propagate_peer_fault_to_value` on nested flow payloads.
    pub(crate) fn notify_peer_faults(&self, reason: &str) {
        let peers: Vec<usize> = self
            .inner
            .read()
            .map(|a| a.peer_links.clone())
            .unwrap_or_default();
        if peers.is_empty() {
            return;
        }
        let handles: Vec<ActorHandle> = {
            let Ok(map) = actor_handles().lock() else {
                return;
            };
            peers.iter().filter_map(|id| map.get(id).cloned()).collect()
        };
        for peer in handles {
            if peer.is_faulted() {
                continue;
            }
            // Best-effort: enqueue peer_fault notification via mailbox.
            // If the peer has no peer_fault method the worker returns an error
            // response which we ignore — link injection is fire-and-forget.
            let (tx, _rx) = std::sync::mpsc::channel();
            let msg = ActorMailboxMsg {
                method: "peer_fault".to_string(),
                args: vec![
                    Value::String(self.id.to_string()),
                    Value::String(reason.to_string()),
                ],
                response: tx,
            };
            let _ = peer.mailbox.send(msg);
        }
    }

    /// v0.29.11 Fault absorption: short-circuit the actor mailbox (O(1)).
    ///
    /// Sets `faulted` so every send site returns immediately without enqueueing.
    /// Clears actor fields so nested payload resources are dropped. The worker
    /// loop also checks `faulted` and drains remaining messages without dispatch.
    /// Idempotent. v0.29.20: also notifies linked peers (PeerFault injection).
    pub(crate) fn short_circuit_mailbox(&self) {
        let already = self
            .inner
            .read()
            .map(|a| a.faulted)
            .unwrap_or(true);
        if already {
            return;
        }
        // Notify peers BEFORE clearing fields / marking faulted so links remain.
        self.notify_peer_faults("peer actor entered Fault");
        if let Ok(mut actor) = self.inner.write() {
            actor.faulted = true;
            actor.fields.clear();
            actor.peer_links.clear();
        }
        // Drop from global registry.
        if let Ok(mut map) = actor_handles().lock() {
            map.remove(&self.id);
        }
    }

    /// True when this actor has entered Fault absorption (mailbox short-circuited).
    pub(crate) fn is_faulted(&self) -> bool {
        self.inner
            .read()
            .map(|a| a.faulted)
            .unwrap_or(true)
    }
}

impl Value {
    pub fn is_arena_ref(&self) -> bool {
        matches!(self, Value::ArenaRef(_, _, _))
    }

    pub fn is_arena_block(&self) -> bool {
        matches!(self, Value::ArenaBlock(_))
    }

    /// Return the numeric value as an integer if it is one.
    pub(crate) fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Return the numeric value as a float, widening integers as needed.
    pub(crate) fn as_float(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Return the value as a borrowed string if it is one.
    pub(crate) fn as_string(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Int(v) => write!(f, "{}", v),
            Value::Float(v) => write!(f, "{}", v),
            Value::Bool(v) => write!(f, "{}", v),
            Value::String(v) => write!(f, "{}", v),
            Value::Unit => write!(f, "()"),
            Value::List(vs) => {
                write!(f, "[")?;
                for (i, v) in vs.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, "]")
            }
            Value::Set(vs) => {
                write!(f, "Set{{")?;
                for (i, v) in vs.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, "}}")
            }
            Value::Tuple(vs) => {
                write!(f, "(")?;
                for (i, v) in vs.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, ")")
            }
            Value::Variant(name, vs) => {
                write!(f, "{}(", name)?;
                for (i, v) in vs.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, ")")
            }
            Value::Record(type_name, fields) => {
                let name = type_name.as_deref().unwrap_or("Record");
                if fields.is_empty() {
                    write!(f, "{} {{}}", name)
                } else {
                    write!(f, "{} {{ ", name)?;
                    let mut first = true;
                    for (k, v) in fields.iter() {
                        if !first {
                            write!(f, ", ")?;
                        }
                        first = false;
                        write!(f, "{}: {}", k, v)?;
                    }
                    write!(f, " }}")
                }
            }
            Value::Future(_) => write!(f, "Future(...)"),
            Value::Error(msg) => write!(f, "Error({})", msg),
            Value::ArenaRef(id, idx, gen) => write!(f, "ArenaRef({}, {}, gen={})", id, idx, gen),
            Value::ArenaBlock(id) => write!(f, "ArenaBlock({})", id),
            Value::QuoteAst(_) => write!(f, "QuoteAst(...)"),
            Value::Newtype(name, v) => write!(f, "{}({})", name, v),
            Value::Actor(_) => write!(f, "Actor(...)"),
            Value::Closure { .. } => write!(f, "Closure(...)"),
            Value::Shared(arc) => {
                let v = arc.read().map_err(|_| std::fmt::Error)?;
                write!(f, "shared({})", v)
            }
            Value::LocalShared(rc) => {
                let v = rc.lock().expect("local_shared lock not poisoned");
                write!(f, "local_shared({})", v)
            }
            Value::WeakShared(w) => match w.upgrade() {
                Some(arc) => {
                    let v = arc.read().map_err(|_| std::fmt::Error)?;
                    write!(f, "weak_shared({})", v)
                }
                None => write!(f, "weak_shared(None)"),
            },
            Value::WeakLocal(w) => match w.upgrade() {
                Some(rc) => {
                    let v = rc.lock().expect("local_shared lock not poisoned");
                    write!(f, "weak_local({})", v)
                }
                None => write!(f, "weak_local(None)"),
            },
            Value::Cap(names) => write!(f, "cap({})", names.join(" + ")),
            Value::Ref(rc) => {
                let v = rc.read().map_err(|_| std::fmt::Error)?;
                write!(f, "&{}", v)
            }
            Value::RefMut(rc) => {
                let v = rc.read().map_err(|_| std::fmt::Error)?;
                write!(f, "&mut {}", v)
            }
            Value::IndexRef { owner, index } => write!(f, "&{}[{}]", owner, index),
            Value::IndexRefMut { owner, index } => write!(f, "&mut {}[{}]", owner, index),
            Value::Type(name) => write!(f, "{}", name),
            Value::Allocator(kind) => write!(f, "Allocator({:?})", kind),
            Value::Array(vs) => {
                write!(f, "[")?;
                for (i, v) in vs.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, "]")
            }
            Value::Slice { source, start, end } => {
                write!(f, "[")?;
                for (i, v) in source[*start..*end].iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, "]")
            }
            Value::Range { start, end } => write!(f, "{}..{}", start, end),
            Value::CBuffer(inner) => write!(f, "CBuffer({:p}, {} bytes)", inner.ptr, inner.size),
            Value::DynTrait {
                data,
                concrete_type,
                trait_names,
            } => {
                write!(
                    f,
                    "dyn {} {{ data: {}, concrete: {} }}",
                    trait_names.join(" + "),
                    data,
                    concrete_type
                )
            }
        }
    }
}

/// Check if a Value tree contains any LocalShared or WeakLocal variants.
/// Used as a runtime assertion before sending Values across thread boundaries.
pub(crate) fn contains_local_shared(v: &Value) -> bool {
    match v {
        Value::LocalShared(_) | Value::WeakLocal(_) => true,
        Value::List(elems) => elems.iter().any(contains_local_shared),
        Value::Set(elems) => elems.iter().any(contains_local_shared),
        Value::Tuple(elems) => elems.iter().any(contains_local_shared),
        Value::Record(_, fields) => fields.values().any(contains_local_shared),
        Value::Variant(_, args) => args.iter().any(contains_local_shared),
        Value::Newtype(_, inner) => contains_local_shared(inner),
        Value::DynTrait { data, .. } => contains_local_shared(data),
        Value::Ref(rc) | Value::RefMut(rc) => {
            // RwLock::read() only returns Err on poisoning; in practice this is unreachable
            // since we don't poison locks in normal operation.
            rc.read().is_ok_and(|v| contains_local_shared(&v))
        }
        Value::Closure { captured, .. } => captured.values().any(contains_local_shared),
        Value::Shared(arc) => {
            if let Ok(v) = arc.read() {
                contains_local_shared(&v)
            } else {
                false
            }
        }
        _ => false,
    }
}

pub(crate) fn contains_arena_ref(v: &Value, arena_id: usize) -> bool {
    match v {
        Value::ArenaRef(id, _, _) => *id == arena_id,
        Value::List(elems) => elems.iter().any(|e| contains_arena_ref(e, arena_id)),
        Value::Set(elems) => elems.iter().any(|e| contains_arena_ref(e, arena_id)),
        Value::Tuple(elems) => elems.iter().any(|e| contains_arena_ref(e, arena_id)),
        Value::Record(_, fields) => fields.values().any(|v| contains_arena_ref(v, arena_id)),
        Value::Variant(_, args) => args.iter().any(|v| contains_arena_ref(v, arena_id)),
        Value::Newtype(_, inner) => contains_arena_ref(inner, arena_id),
        Value::DynTrait { data, .. } => contains_arena_ref(data, arena_id),
        Value::Ref(rc) | Value::RefMut(rc) => {
            if let Ok(v) = rc.read() {
                contains_arena_ref(&v, arena_id)
            } else {
                false
            }
        }
        Value::Closure { captured, .. } => {
            captured.values().any(|v| contains_arena_ref(v, arena_id))
        }
        Value::Shared(arc) => {
            if let Ok(v) = arc.read() {
                contains_arena_ref(&v, arena_id)
            } else {
                false
            }
        }
        Value::WeakShared(arc) => {
            if let Some(arc) = arc.upgrade() {
                if let Ok(v) = arc.read() {
                    return contains_arena_ref(&v, arena_id);
                }
            }
            false
        }
        Value::LocalShared(inner) => contains_arena_ref(
            &inner.0.lock().expect("local_shared lock not poisoned"),
            arena_id,
        ),
        Value::WeakLocal(inner) => {
            if let Some(rc) = inner.0.upgrade() {
                contains_arena_ref(
                    &rc.lock().expect("local_shared lock not poisoned"),
                    arena_id,
                )
            } else {
                false
            }
        }
        Value::Type(_) => false,
        _ => false,
    }
}

pub(crate) fn is_copy(v: &Value) -> bool {
    match v {
        Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Unit => true,
        Value::Tuple(elems) => elems.iter().all(is_copy),
        Value::Newtype(_, inner) => is_copy(inner),
        Value::Shared(_) | Value::LocalShared(_) => true,
        Value::Record(_, fields) => fields.values().all(is_copy),
        Value::Variant(_, args) => args.iter().all(is_copy),
        Value::Array(elems) => elems.iter().all(is_copy),
        Value::Set(elems) => elems.iter().all(is_copy),
        _ => false,
    }
}

pub(crate) fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Int(0) => false,
        Value::Float(x) => *x != 0.0,
        Value::String(s) => !s.is_empty(),
        Value::List(l) => !l.is_empty(),
        Value::Set(s) => !s.is_empty(),
        Value::Unit => false,
        Value::Newtype(_, inner) => is_truthy(inner),
        _ => true,
    }
}

pub(crate) fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => a == b,
        // Cross numeric comparison: widen the integer side to float.
        (Value::Int(a), Value::Float(b)) | (Value::Float(b), Value::Int(a)) => {
            let a_f = *a as f64;
            let diff = (a_f - b).abs();
            if diff == 0.0 {
                return true;
            }
            let scale = a_f.abs().max(b.abs());
            diff <= f64::EPSILON * scale.max(1.0)
        }
        (Value::Float(a), Value::Float(b)) => {
            let diff = (a - b).abs();
            if diff == 0.0 {
                return true;
            }
            let scale = a.abs().max(b.abs());
            diff <= f64::EPSILON * scale.max(1.0)
        }
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Unit, Value::Unit) => true,
        (Value::List(a), Value::List(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| values_equal(x, y))
        }
        (Value::Set(a), Value::Set(b)) => {
            a.len() == b.len() && a.iter().all(|x| b.iter().any(|y| values_equal(x, y)))
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| values_equal(x, y))
        }
        (
            Value::Slice {
                source: a_src,
                start: a_s,
                end: a_e,
            },
            Value::Slice {
                source: b_src,
                start: b_s,
                end: b_e,
            },
        ) => {
            let a_slice = &a_src[*a_s..*a_e];
            let b_slice = &b_src[*b_s..*b_e];
            a_slice.len() == b_slice.len()
                && a_slice
                    .iter()
                    .zip(b_slice.iter())
                    .all(|(x, y)| values_equal(x, y))
        }
        (Value::Tuple(a), Value::Tuple(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| values_equal(x, y))
        }
        (Value::Variant(an, av), Value::Variant(bn, bv)) => {
            an == bn
                && av.len() == bv.len()
                && av.iter().zip(bv.iter()).all(|(x, y)| values_equal(x, y))
        }
        (Value::Record(_, a), Value::Record(_, b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, v)| b.get(k).map(|bv| values_equal(v, bv)).unwrap_or(false))
        }
        (Value::Newtype(_, a), Value::Newtype(_, b)) => values_equal(a, b),
        (Value::Ref(a), Value::Ref(b)) | (Value::RefMut(a), Value::RefMut(b)) => {
            if let (Ok(va), Ok(vb)) = (a.read(), b.read()) {
                values_equal(&va, &vb)
            } else {
                false
            }
        }
        (Value::Ref(a), _) => {
            if let Ok(va) = a.read() {
                values_equal(&va, b)
            } else {
                false
            }
        }
        (_, Value::Ref(b)) => {
            if let Ok(vb) = b.read() {
                values_equal(a, &vb)
            } else {
                false
            }
        }
        (Value::Shared(a), Value::Shared(b)) => {
            if let (Ok(va), Ok(vb)) = (a.read(), b.read()) {
                values_equal(&va, &vb)
            } else {
                false
            }
        }
        (Value::LocalShared(a), Value::LocalShared(b)) => values_equal(
            &a.0.lock().expect("local_shared lock not poisoned"),
            &b.0.lock().expect("local_shared lock not poisoned"),
        ),
        (Value::Cap(a), Value::Cap(b)) => a == b,
        (
            Value::Range {
                start: as_,
                end: ae,
            },
            Value::Range { start: bs, end: be },
        ) => as_ == bs && ae == be,
        (Value::Type(a), Value::Type(b)) => a == b,
        (Value::Allocator(a), Value::Allocator(b)) => a == b,
        (
            Value::DynTrait {
                data: ad,
                concrete_type: at,
                ..
            },
            Value::DynTrait {
                data: bd,
                concrete_type: bt,
                ..
            },
        ) => at == bt && values_equal(ad, bd),
        _ => false,
    }
}

pub(crate) fn numeric_op(
    a: Value,
    b: Value,
    int_op: fn(i64, i64) -> i64,
    float_op: fn(f64, f64) -> f64,
) -> Result<Value, InterpError> {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(int_op(a, b))),
        (Value::Float(a), Value::Float(b)) => Ok(Value::Float(float_op(a, b))),
        (Value::Int(a), Value::Float(b)) => Ok(Value::Float(float_op(a as f64, b))),
        (Value::Float(a), Value::Int(b)) => Ok(Value::Float(float_op(a, b as f64))),
        _ => Err(InterpError::new("arithmetic requires numbers")),
    }
}

pub(crate) fn compare_op<F>(a: Value, b: Value, f: F) -> Result<Value, InterpError>
where
    F: Fn(std::cmp::Ordering) -> bool,
{
    let ord = match (&a, &b) {
        (Value::Int(a), Value::Int(b)) => a.cmp(b),
        // Mixed numeric comparison: widen the integer side to float.
        (Value::Int(i), Value::Float(fl)) => (*i as f64)
            .partial_cmp(fl)
            .ok_or_else(|| InterpError::new("cannot compare NaN with float"))?,
        (Value::Float(fl), Value::Int(i)) => fl
            .partial_cmp(&(*i as f64))
            .ok_or_else(|| InterpError::new("cannot compare NaN with float"))?,
        (Value::Float(a), Value::Float(b)) => a
            .partial_cmp(b)
            .ok_or_else(|| InterpError::new("cannot compare NaN with float"))?,
        (Value::String(a), Value::String(b)) => a.cmp(b),
        _ => {
            return Err(InterpError::new(format!(
                "cannot compare {} with {}",
                type_name(&a),
                type_name(&b)
            )))
        }
    };
    Ok(Value::Bool(f(ord)))
}

/// Get a human-readable type name for a value.
pub(crate) fn type_name(val: &Value) -> &'static str {
    match val {
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Bool(_) => "bool",
        Value::String(_) => "string",
        Value::Unit => "unit",
        Value::List(_) => "list",
        Value::Set(_) => "set",
        Value::Array(_) => "array",
        Value::Tuple(_) => "tuple",
        Value::Variant(_, _) => "variant",
        Value::Record(Some(_), _) => "record",
        Value::Record(None, _) => "record",
        Value::Error(_) => "error",
        Value::Newtype(_, _) => "newtype",
        Value::Type(_) => "type",
        Value::Closure { .. } => "closure",
        Value::QuoteAst(_) => "AST",
        Value::Shared(_) => "shared",
        Value::LocalShared(_) => "local_shared",
        Value::Ref(_) => "ref",
        Value::RefMut(_) => "ref_mut",
        Value::IndexRef { .. } => "borrowed_index",
        Value::IndexRefMut { .. } => "borrowed_index_mut",
        Value::Cap(_) => "cap",
        Value::Actor(_) => "actor",
        Value::Future(_) => "future",
        Value::ArenaRef(_, _, _) => "arena_ref",
        Value::ArenaBlock(_) => "arena_block",
        Value::WeakShared(_) | Value::WeakLocal(_) => "weak",
        Value::Allocator(_) => "allocator",
        Value::Slice { .. } => "slice",
        Value::Range { .. } => "range",
        Value::CBuffer(_) => "c_buffer",
        Value::DynTrait { .. } => "dyn_trait",
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        values_equal(self, other)
    }
}
