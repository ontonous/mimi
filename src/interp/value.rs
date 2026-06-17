#![allow(dead_code)]

use crate::ast::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak as RcWeak};
use std::sync::{Arc, RwLock, Weak as ArcWeak};

#[derive(Debug, Clone)]
pub(crate) struct SendRc<T>(pub(crate) Rc<T>);
unsafe impl<T: Clone> Send for SendRc<T> {}
unsafe impl<T: Clone> Sync for SendRc<T> {}
impl<T> std::ops::Deref for SendRc<T> {
    type Target = Rc<T>;
    fn deref(&self) -> &Self::Target { &self.0 }
}

#[derive(Debug, Clone)]
pub(crate) struct SendWeak<T>(pub(crate) RcWeak<T>);
unsafe impl<T: Clone> Send for SendWeak<T> {}
unsafe impl<T: Clone> Sync for SendWeak<T> {}
impl<T> SendWeak<T> {
    pub(crate) fn upgrade(&self) -> Option<SendRc<T>> {
        self.0.upgrade().map(SendRc)
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
    Tuple(Vec<Value>),
    Variant(String, Vec<Value>),
    Record(Option<String>, HashMap<String, Value>),
    Future(std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<Result<Value, String>>>>),
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
    LocalShared(SendRc<RefCell<Value>>),
    WeakShared(ArcWeak<RwLock<Value>>),
    WeakLocal(SendWeak<RefCell<Value>>),
    Cap(Vec<String>),
    /// Immutable reference: &T
    Ref(SendRc<RefCell<Value>>),
    /// Mutable reference: &mut T
    RefMut(SendRc<RefCell<Value>>),
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
}

/// Kind of allocator
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocatorKind {
    System,
    Arena,
    Bump,
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

#[derive(Debug, Clone)]
pub struct ActorHandle {
    pub inner: std::sync::Arc<std::sync::RwLock<ActorInstance>>,
    pub id: usize,
}

static ACTOR_HANDLE_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl ActorHandle {
    pub(crate) fn new(instance: ActorInstance) -> Self {
        let id = ACTOR_HANDLE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        ActorHandle {
            inner: std::sync::Arc::new(std::sync::RwLock::new(instance)),
            id,
        }
    }
}

impl Value {
    pub fn is_arena_ref(&self) -> bool {
        matches!(self, Value::ArenaRef(_, _))
    }

    pub fn is_arena_block(&self) -> bool {
        matches!(self, Value::ArenaBlock(_))
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
                let v = rc.0.borrow();
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
                    let v = rc.0.borrow();
                    write!(f, "weak_local({})", v)
                }
                None => write!(f, "weak_local(None)"),
            },
            Value::Cap(names) => write!(f, "cap({})", names.join(" + ")),
            Value::Ref(rc) => {
                let v = rc.0.borrow();
                write!(f, "&{}", v)
            }
            Value::RefMut(rc) => {
                let v = rc.0.borrow();
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
        }
    }
}

pub(crate) fn contains_arena_ref(v: &Value, arena_id: usize) -> bool {
    match v {
        Value::ArenaRef(id, _) => *id == arena_id,
        Value::List(elems) => elems.iter().any(|e| contains_arena_ref(e, arena_id)),
        Value::Tuple(elems) => elems.iter().any(|e| contains_arena_ref(e, arena_id)),
        Value::Record(_, fields) => fields.values().any(|v| contains_arena_ref(v, arena_id)),
        Value::Variant(_, args) => args.iter().any(|v| contains_arena_ref(v, arena_id)),
        Value::Newtype(inner) => contains_arena_ref(inner, arena_id),
        Value::Ref(rc) | Value::RefMut(rc) => {
            let v = rc.0.borrow();
            contains_arena_ref(&v, arena_id)
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
        Value::Unit => false,
        Value::Newtype(inner) => is_truthy(inner),
        _ => true,
    }
}

pub(crate) fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => (a - b).abs() < f64::EPSILON,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Unit, Value::Unit) => true,
        (Value::List(a), Value::List(b)) => a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| values_equal(x, y)),
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
            let va = a.0.borrow();
            let vb = b.0.borrow();
            values_equal(&va, &vb)
        }
        (Value::Ref(a), _) => {
            let va = a.0.borrow();
            values_equal(&va, b)
        }
        (_, Value::Ref(b)) => {
            let vb = b.0.borrow();
            values_equal(a, &vb)
        }
        (Value::Type(a), Value::Type(b)) => a == b,
        _ => false,
    }
}

pub(crate) fn numeric_op(
    a: Value,
    b: Value,
    int_op: fn(i64, i64) -> i64,
    float_op: fn(f64, f64) -> f64,
) -> Result<Value, String> {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(int_op(a, b))),
        (Value::Float(a), Value::Float(b)) => Ok(Value::Float(float_op(a, b))),
        (Value::Int(a), Value::Float(b)) => Ok(Value::Float(float_op(a as f64, b))),
        (Value::Float(a), Value::Int(b)) => Ok(Value::Float(float_op(a, b as f64))),
        _ => Err("arithmetic requires numbers".into()),
    }
}

pub(crate) fn compare_op<F>(a: Value, b: Value, f: F) -> Result<Value, String>
where
    F: Fn(std::cmp::Ordering) -> bool,
{
    let ord = match (&a, &b) {
        (Value::Int(a), Value::Int(b)) => a.cmp(b),
        (Value::Float(a), Value::Float(b)) => a.partial_cmp(b).ok_or("cannot compare NaN with float")?,
        (Value::String(a), Value::String(b)) => a.cmp(b),
        _ => return Err(format!("cannot compare {} with {}", type_name(&a), type_name(&b))),
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
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        values_equal(self, other)
    }
}
