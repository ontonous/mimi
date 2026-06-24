#![allow(dead_code)]

use crate::ast::*;
use crate::interp::error::InterpError;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak as RcWeak};
use std::sync::{Arc, RwLock, Weak as ArcWeak};

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
            PollFuture::Ready(result) => {
                match result {
                    Ok(v) => write!(f, "PollFuture::Ready(Ok({:?}))", v),
                    Err(e) => write!(f, "PollFuture::Ready(Err({}))", e),
                }
            }
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
            let block_result = interp.eval_block(&func.body).map(|v| v.unwrap_or(Value::Unit));
            result = interp.early_return.take()
                .map_or(block_result, Ok);
        }
        interp.pop_scope();
        *state = PollFuture::Ready(result);
    }
}

/// Global executor queue for deferred futures.
fn executor_queue() -> &'static std::sync::Mutex<Vec<std::sync::Arc<std::sync::Mutex<PollFuture>>>> {
    use std::sync::Mutex;
    static QUEUE: std::sync::OnceLock<Mutex<Vec<std::sync::Arc<Mutex<PollFuture>>>>> = std::sync::OnceLock::new();
    QUEUE.get_or_init(|| Mutex::new(Vec::new()))
}

/// Submit a deferred future to the global executor.
pub fn executor_submit(future: std::sync::Arc<std::sync::Mutex<PollFuture>>) {
    executor_queue().lock().expect("executor queue lock").push(future);
}

/// Run the executor: poll all deferred futures until all are completed.
pub fn executor_run() {
    loop {
        let entry = {
            let queue = executor_queue();
            let mut guard = queue.lock().expect("executor queue lock");
            if guard.is_empty() { return; }
            let mut found = None;
            for i in 0..guard.len() {
                let fut = &guard[i];
                let state = fut.lock().expect("future lock");
                match &*state {
                    PollFuture::Deferred { .. } => {
                        found = Some(i);
                        break;
                    }
                    PollFuture::Ready(_) | PollFuture::Pending(_) => {}
                }
            }
            match found {
                Some(i) => {
                    let fut = guard.swap_remove(i);
                    Some(fut)
                }
                None => {
                    guard.clear();
                    return;
                }
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
    ArenaRef(usize, usize),
    ArenaBlock(usize),
    QuoteAst(Box<QuotedAst>),
    Newtype(Box<Value>),
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
    /// Immutable reference: &T
    Ref(Arc<RwLock<Value>>),
    /// Mutable reference: &mut T
    RefMut(Arc<RwLock<Value>>),
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

/// Wrapper around Rc<RefCell<Value>> for LocalShared.
///
/// SAFETY: Rc<RefCell<Value>> is !Send due to non-atomic reference counting
/// and !Sync due to RefCell's runtime borrow tracking. This wrapper is safe
/// because:
/// 1. The typechecker rejects local_shared captures in parasteps/spawn (E0305).
/// 2. `contains_local_shared()` provides a defense-in-depth runtime check
///    before any thread boundary crossing in the interpreter.
/// 3. The codegen relies entirely on (1); LLVM codegen lacks a runtime check
///    but cannot reach here without passing (1).
/// 4. `check_expr_parasteps_safe` also descends into Expr::Lambda bodies.
#[derive(Debug, Clone)]
pub struct LocalSharedInner(pub Rc<RefCell<Value>>);

// SAFETY: LocalSharedInner wraps Rc<RefCell<Value>> which is !Send/!Sync by default.
// Safety relies on the type-level E0305 rejection of local_shared in parallel blocks,
// plus the defense-in-depth runtime check in contains_local_shared().
// See the struct-level doc on LocalSharedInner for full details.
unsafe impl Send for LocalSharedInner {}
// SAFETY: Same reasoning as Send — single-threaded access only, guaranteed by the
// typechecker (E0305) and runtime check (contains_local_shared).
unsafe impl Sync for LocalSharedInner {}

impl std::ops::Deref for LocalSharedInner {
    type Target = RefCell<Value>;
    fn deref(&self) -> &RefCell<Value> {
        &self.0
    }
}

impl LocalSharedInner {
    pub fn new(v: Value) -> Self {
        LocalSharedInner(Rc::new(RefCell::new(v)))
    }
    pub fn downgrade(&self) -> WeakLocalInner {
        WeakLocalInner(Rc::downgrade(&self.0))
    }
    pub fn clone_rc(this: &Self) -> Self {
        LocalSharedInner(Rc::clone(&this.0))
    }
}

/// Wrapper around RcWeak<RefCell<Value>> for WeakLocal.
#[derive(Debug, Clone)]
pub struct WeakLocalInner(pub RcWeak<RefCell<Value>>);

// SAFETY: WeakLocalInner wraps RcWeak<RefCell<Value>> which is !Send/!Sync by default,
// but all accesses are single-threaded (only used within the interpreter which is
// single-threaded per instance). WeakLocal is always paired with LocalShared, which
// is already restricted to single-threaded use.
unsafe impl Send for WeakLocalInner {}
// SAFETY: Same reasoning as Send — single-threaded access only.
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

// SAFETY: CBufferInner owns a heap-allocated buffer via raw pointer; ownership is exclusive
// (implemented via Arc, so the buffer is only accessed through safe methods that validate
// the pointer before reading/writing). The underlying memory is always properly aligned
// and sized according to the CBuffer creation path.
unsafe impl Send for CBufferInner {}
// SAFETY: Same reasoning as Send — exclusive ownership per Arc instance guarantees that
// concurrent reads do not race with writes. Arc<RwLock<CBufferInner>> is used externally.
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
}

#[derive(Debug, Clone)]
pub struct ActorInstance {
    pub actor_name: String,
    pub fields: HashMap<String, Value>,
    pub methods: Vec<FuncDef>,
}

/// Message sent to an actor's mailbox for FIFO processing.
pub struct ActorMailboxMsg {
    pub method: String,
    pub args: Vec<Value>,
    pub response: std::sync::mpsc::Sender<Result<Value, InterpError>>,
}

/// Handle to a running actor with per-actor mailbox + dedicated worker thread.
#[derive(Debug)]
pub struct ActorHandle {
    pub inner: std::sync::Arc<std::sync::RwLock<ActorInstance>>,
    pub mailbox: std::sync::mpsc::Sender<ActorMailboxMsg>,
    pub id: usize,
}

unsafe impl Send for ActorHandle {}
unsafe impl Sync for ActorHandle {}

impl Clone for ActorHandle {
    fn clone(&self) -> Self {
        ActorHandle {
            inner: self.inner.clone(),
            mailbox: self.mailbox.clone(),
            id: self.id,
        }
    }
}

impl PartialEq for ActorHandle {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

static ACTOR_HANDLE_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

// Thread-local flag set when inside an actor's worker thread.
// Used to detect self-calls and avoid mailbox deadlock.
thread_local! {
    static CURRENT_ACTOR_ID: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl ActorHandle {
    /// Creates a new actor, spawns its worker thread.
    pub(crate) fn new(instance: ActorInstance) -> Self {
        let id = ACTOR_HANDLE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let (mailbox_tx, mailbox_rx) = std::sync::mpsc::channel::<ActorMailboxMsg>();
        let inner = std::sync::Arc::new(std::sync::RwLock::new(instance));

        let worker_inner = inner.clone();
        let mailbox_tx_clone = mailbox_tx.clone();
        std::thread::Builder::new()
            .name(format!("actor-{}", id))
            .spawn(move || {
                CURRENT_ACTOR_ID.with(|a| a.set(id));
                let empty_file = crate::ast::File { imports: vec![], items: vec![] };
                while let Ok(msg) = mailbox_rx.recv() {
                    let result = {
                        // Read method definition
                        let (func, _actor_name) = {
                            let actor = worker_inner.read()
                                .expect("actor worker lock");
                            let func = actor.methods.iter()
                                .find(|f| f.name == msg.method)
                                .cloned()
                                .expect("actor method not found");
                            (func, actor.actor_name.clone())
                        };
                        // Create minimal interpreter to run the method body
                        let mut interp = crate::interp::Interpreter::new(&empty_file);
                        let self_val = Value::Actor(ActorHandle {
                            inner: worker_inner.clone(),
                            mailbox: mailbox_tx_clone.clone(),
                            id,
                        });
                        interp.push_scope();
                        interp.bind("self", self_val)
                            .expect("bind self in actor worker");
                        // Bind method parameters
                        let mut args_iter = msg.args.iter();
                        for param in &func.params {
                            if param.name == "self" { continue; }
                            let arg = args_iter.next()
                                .cloned()
                                .unwrap_or(Value::Unit);
                            interp.bind(&param.name, arg)
                                .expect("bind param in actor worker");
                        }
                        let result = interp.eval_block(&func.body)
                            .map(|opt| opt.unwrap_or(Value::Unit));
                        interp.pop_scope();
                        result
                    };
                    let _ = msg.response.send(result);
                }
                CURRENT_ACTOR_ID.with(|a| a.set(0));
            })
            .expect("failed to spawn actor worker");

        ActorHandle { inner, mailbox: mailbox_tx, id }
    }

    /// Returns the current actor's thread-local ID (0 if not in an actor worker).
    pub(crate) fn current_worker_id() -> usize {
        CURRENT_ACTOR_ID.with(|a| a.get())
    }
}

impl Value {
    pub fn is_arena_ref(&self) -> bool {
        matches!(self, Value::ArenaRef(_, _))
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
                    if i > 0 { write!(f, ", ")?; }
                    write!(f, "{}", v)?;
                }
                write!(f, "]")
            }
            Value::Set(vs) => {
                write!(f, "Set{{")?;
                for (i, v) in vs.iter().enumerate() {
                    if i > 0 { write!(f, ", ")?; }
                    write!(f, "{}", v)?;
                }
                write!(f, "}}")
            }
            Value::Tuple(vs) => {
                write!(f, "(")?;
                for (i, v) in vs.iter().enumerate() {
                    if i > 0 { write!(f, ", ")?; }
                    write!(f, "{}", v)?;
                }
                write!(f, ")")
            }
            Value::Variant(name, vs) => {
                write!(f, "{}(", name)?;
                for (i, v) in vs.iter().enumerate() {
                    if i > 0 { write!(f, ", ")?; }
                    write!(f, "{}", v)?;
                }
                write!(f, ")")
            }
            Value::Record(_, fields) => {
                write!(f, "{{")?;
                let mut first = true;
                for (k, v) in fields.iter() {
                    if !first { write!(f, ", ")?; }
                    first = false;
                    write!(f, "{}: {}", k, v)?;
                }
                write!(f, "}}")
            }
            Value::Future(_) => write!(f, "Future(...)"),
            Value::Error(msg) => write!(f, "Error({})", msg),
            Value::ArenaRef(id, idx) => write!(f, "ArenaRef({}, {})", id, idx),
            Value::ArenaBlock(id) => write!(f, "ArenaBlock({})", id),
            Value::QuoteAst(_) => write!(f, "QuoteAst(...)"),
            Value::Newtype(v) => write!(f, "Newtype({})", v),
            Value::Actor(_) => write!(f, "Actor(...)"),
            Value::Closure { .. } => write!(f, "Closure(...)"),
            Value::Shared(arc) => {
                let v = arc.read().map_err(|_| std::fmt::Error)?;
                write!(f, "shared({})", v)
            }
            Value::LocalShared(rc) => {
                let v = rc.borrow();
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
                    let v = rc.borrow();
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
            Value::Type(name) => write!(f, "{}", name),
            Value::Allocator(kind) => write!(f, "Allocator({:?})", kind),
            Value::Array(vs) => {
                write!(f, "[")?;
                for (i, v) in vs.iter().enumerate() {
                    if i > 0 { write!(f, ", ")?; }
                    write!(f, "{}", v)?;
                }
                write!(f, "]")
            }
            Value::Slice { source, start, end } => {
                write!(f, "[")?;
                for (i, v) in source[*start..*end].iter().enumerate() {
                    if i > 0 { write!(f, ", ")?; }
                    write!(f, "{}", v)?;
                }
                write!(f, "]")
            }
            Value::Range { start, end } => write!(f, "{}..{}", start, end),
            Value::CBuffer(inner) => write!(f, "CBuffer({:p}, {} bytes)", inner.ptr, inner.size),
            Value::DynTrait { data, concrete_type, trait_names } => {
                write!(f, "dyn {} {{ data: {}, concrete: {} }}", trait_names.join(" + "), data, concrete_type)
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
        Value::Newtype(inner) => contains_local_shared(inner),
        Value::DynTrait { data, .. } => contains_local_shared(data),
        Value::Ref(rc) | Value::RefMut(rc) => {
            if let Ok(v) = rc.read() {
                contains_local_shared(&v)
            } else {
                false
            }
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
        Value::ArenaRef(id, _) => *id == arena_id,
        Value::List(elems) => elems.iter().any(|e| contains_arena_ref(e, arena_id)),
        Value::Set(elems) => elems.iter().any(|e| contains_arena_ref(e, arena_id)),
        Value::Tuple(elems) => elems.iter().any(|e| contains_arena_ref(e, arena_id)),
        Value::Record(_, fields) => fields.values().any(|v| contains_arena_ref(v, arena_id)),
        Value::Variant(_, args) => args.iter().any(|v| contains_arena_ref(v, arena_id)),
        Value::Newtype(inner) => contains_arena_ref(inner, arena_id),
        Value::DynTrait { data, .. } => contains_arena_ref(data, arena_id),
        Value::Ref(rc) | Value::RefMut(rc) => {
            if let Ok(v) = rc.read() {
                contains_arena_ref(&v, arena_id)
            } else {
                false
            }
        }
        Value::Closure { captured, .. } => captured.values().any(|v| contains_arena_ref(v, arena_id)),
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
        Value::LocalShared(inner) => {
            contains_arena_ref(&inner.0.borrow(), arena_id)
        }
        Value::WeakLocal(inner) => {
            if let Some(rc) = inner.0.upgrade() {
                contains_arena_ref(&rc.borrow(), arena_id)
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
        Value::Newtype(inner) => is_copy(inner),
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
        Value::Newtype(inner) => is_truthy(inner),
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
            if diff == 0.0 { return true; }
            let scale = a_f.abs().max(b.abs());
            diff <= f64::EPSILON * scale.max(1.0)
        }
        (Value::Float(a), Value::Float(b)) => {
            let diff = (a - b).abs();
            if diff == 0.0 { return true; }
            let scale = a.abs().max(b.abs());
            diff <= f64::EPSILON * scale.max(1.0)
        }
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Unit, Value::Unit) => true,
        (Value::List(a), Value::List(b)) => a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| values_equal(x, y)),
        (Value::Set(a), Value::Set(b)) => a.len() == b.len() && a.iter().all(|x| b.iter().any(|y| values_equal(x, y))),
        (Value::Array(a), Value::Array(b)) => a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| values_equal(x, y)),
        (Value::Slice { source: a_src, start: a_s, end: a_e }, Value::Slice { source: b_src, start: b_s, end: b_e }) => {
            let a_slice = &a_src[*a_s..*a_e];
            let b_slice = &b_src[*b_s..*b_e];
            a_slice.len() == b_slice.len() && a_slice.iter().zip(b_slice.iter()).all(|(x, y)| values_equal(x, y))
        }
        (Value::Tuple(a), Value::Tuple(b)) => a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| values_equal(x, y)),
        (Value::Variant(an, av), Value::Variant(bn, bv)) => {
            an == bn && av.len() == bv.len() && av.iter().zip(bv.iter()).all(|(x, y)| values_equal(x, y))
        }
        (Value::Record(_, a), Value::Record(_, b)) => {
            a.len() == b.len() && a.iter().all(|(k, v)| b.get(k).map(|bv| values_equal(v, bv)).unwrap_or(false))
        }
        (Value::Newtype(a), Value::Newtype(b)) => values_equal(a, b),
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
            if let (Ok(va), Ok(vb)) = (a.read(), b.read()) { values_equal(&va, &vb) } else { false }
        }
        (Value::LocalShared(a), Value::LocalShared(b)) => {
            values_equal(&a.0.borrow(), &b.0.borrow())
        }
        (Value::Cap(a), Value::Cap(b)) => a == b,
        (Value::Range { start: as_, end: ae }, Value::Range { start: bs, end: be }) => as_ == bs && ae == be,
        (Value::Type(a), Value::Type(b)) => a == b,
        (Value::Allocator(a), Value::Allocator(b)) => a == b,
        (Value::DynTrait { data: ad, concrete_type: at, .. }, Value::DynTrait { data: bd, concrete_type: bt, .. }) => {
            at == bt && values_equal(ad, bd)
        }
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
        (Value::Int(i), Value::Float(fl)) => (*i as f64).partial_cmp(fl).ok_or_else(|| InterpError::new("cannot compare NaN with float"))?,
        (Value::Float(fl), Value::Int(i)) => fl.partial_cmp(&(*i as f64)).ok_or_else(|| InterpError::new("cannot compare NaN with float"))?,
        (Value::Float(a), Value::Float(b)) => a.partial_cmp(b).ok_or_else(|| InterpError::new("cannot compare NaN with float"))?,
        (Value::String(a), Value::String(b)) => a.cmp(b),
        _ => return Err(InterpError::new(format!("cannot compare {} with {}", type_name(&a), type_name(&b)))),
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
        Value::Newtype(_) => "newtype",
        Value::Type(_) => "type",
        Value::Closure { .. } => "closure",
        Value::QuoteAst(_) => "AST",
        Value::Shared(_) => "shared",
        Value::LocalShared(_) => "local_shared",
        Value::Ref(_) => "ref",
        Value::RefMut(_) => "ref_mut",
        Value::Cap(_) => "cap",
        Value::Actor(_) => "actor",
        Value::Future(_) => "future",
        Value::ArenaRef(_, _) => "arena_ref",
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
