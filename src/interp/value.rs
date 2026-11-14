#![allow(dead_code)]
#![allow(clippy::unwrap_used)]

use crate::ast::*;
use crate::interp::error::{ErrorContext, InterpError};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::{Arc, Mutex, RwLock, Weak as ArcWeak};

/// Shared VM string storage. Clones share the allocation; mutation uses COW.
/// Unlike the stable standard Arc, this storage also supports fallible publication.
pub type SharedString = triomphe::Arc<String>;

/// Poll-based future state.
/// For async fn: deferred (waiting to be evaluated by executor).
/// For actor spawn: Pending with a channel receiver (polled on await).
/// For immediately-ready: Ready with result.
///
/// CRITICAL #5 analysis: `Deferred` owns its `Box<File>` (heap-allocated,
/// independent copy), NOT a reference to an external Interpreter's data.
/// When the original Interpreter is dropped, the `Box<File>` in the global
/// executor queue remains valid. `poll_deferred` creates a fresh Interpreter
/// from `&*file`, which lives only within the poll call. No dangling
/// references are possible.
pub enum PollFuture {
    Deferred {
        file: Box<crate::ast::File>,
        func: FuncDef,
        args: Vec<Value>,
        /// I-H4: snapshot of parent globals/const/cli_args for async body.
        globals: std::collections::HashMap<String, Value>,
        cli_args: Vec<String>,
        verify_contracts: bool,
        /// TC-C1: parent stdout capture buffer so deferred/async spawn output
        /// is captured (explicit Arc; the process-wide global slot was removed).
        stdout_capture: Option<std::sync::Arc<std::sync::Mutex<String>>>,
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

/// Poll a deferred future: evaluate the function body via bytecode VM and store the result.
/// 0.33 Phase F: tree-walker removed; uses eval_comptime_block_bytecode.
pub fn poll_deferred(state: &mut PollFuture) {
    if let PollFuture::Deferred {
        file,
        func,
        args,
        globals,
        cli_args: _,
        verify_contracts: _,
        stdout_capture: _,
    } = state
    {
        // Build comptime values from globals + args (params bound as constants).
        let mut comptime_values: std::collections::HashMap<String, Value> = std::mem::take(globals);
        let taken_args = std::mem::take(args);
        for (p, a) in func.params.iter().zip(taken_args) {
            comptime_values.insert(p.name.clone(), a);
        }
        let result = crate::interp::bytecode::compiler::eval_comptime_block_bytecode(
            &*file,
            &func.body,
            &comptime_values,
        )
        .map_err(InterpError::new);
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
    // Recover from poison so a panicked poller cannot brick the queue.
    let mut guard = executor_queue().lock().unwrap_or_else(|e| e.into_inner());
    guard.push(future);
}

/// Run the executor: poll all deferred futures until all are completed.
pub fn executor_run() {
    loop {
        let entry = {
            let queue = executor_queue();
            let mut guard = queue.lock().unwrap_or_else(|e| e.into_inner());
            if guard.is_empty() {
                return;
            }
            // Remove all completed futures before looking for Deferred ones
            guard.retain(|fut| {
                let state = fut.lock().unwrap_or_else(|e| e.into_inner());
                !matches!(&*state, PollFuture::Ready(_))
            });
            let mut found = None;
            for i in 0..guard.len() {
                let fut = &guard[i];
                let state = fut.lock().unwrap_or_else(|e| e.into_inner());
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
            let mut state = fut.lock().unwrap_or_else(|e| e.into_inner());
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
    /// PA-H3 (audit): optional chain `x?.y`
    OptionalChain(Box<QuotedAst>, String),
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
    WhileLet {
        pat: Pattern,
        init: Box<QuotedAst>,
        body: Box<QuotedAst>,
    },
    Loop(Box<QuotedAst>),
    For(String, Box<QuotedAst>, Box<QuotedAst>),
    Assign(Box<QuotedAst>, Box<QuotedAst>),
    Arena(Box<QuotedAst>),
    Unsafe(Box<QuotedAst>),
    /// v0.34.10a (SD-9): `ieee_float { }` escape hatch.
    IeeeFloat(Box<QuotedAst>),
    Drop(Box<QuotedAst>),
    Defer(Box<QuotedAst>),
    SharedLet {
        kind: SharedKind,
        name: String,
        init: Box<QuotedAst>,
    },
    OnFailure(Box<QuotedAst>),
    Parasteps(Box<QuotedAst>),
    /// C1: preserve lambda params and return type during quote.
    Lambda {
        params: Vec<Param>,
        ret: Option<Type>,
        body: Block,
        captured: HashMap<String, Value>,
    },
    Cast(Box<QuotedAst>, Type),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeProjection {
    Field(String),
    Tuple(usize),
}

/// 0.31.23: Value clone elimination.
///
/// Blind review fix: `#[derive(Clone)]` on Value allows accidental deep copying
/// of heap-allocated data (List, Record, Closure, etc.), which is expensive and
/// can lead to unexpected behavior.
///
/// This manual Clone implementation documents the cost and provides alternatives:
/// - `Value::share()`: Returns `Arc<Value>` for shared ownership (cheap)
/// - `Value::deep_clone()`: Explicit deep copy (expensive, use with care)
///
/// The derived Clone is kept for backward compatibility but should be avoided
/// in new code. Prefer `share()` for sharing and `deep_clone()` when a true
/// copy is needed.
/// Values admitted inside the checker-receipted Canonical MIR MapRoot.
/// This intentionally stays separate from ordinary dynamic `Any` maps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalMapRootValue {
    I32(i32),
    String(SharedString),
}

/// Values admitted by the separately versioned Checker-receipted
/// MapAnyOwner v1 profile. Even though its first payload slice matches
/// MapRoot, the distinct enum prevents a raw MapRoot value from crossing the
/// typed Any-owner bytecode boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalMapAnyOwnerV1Value {
    I32(i32),
    String(SharedString),
}

/// Values admitted by the independent source-Map owner lifecycle profile.
/// This has its own value identity even though the initial payload cases
/// overlap MapRoot and MapAnyOwner v1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalOrdinaryMapOwnerLifecycleI32StringV1Value {
    I32(i32),
    String(SharedString),
}

/// Values admitted by the independent ordinary Map owned-Get profile. Its
/// runtime identity is separate from both the lifecycle profile and legacy
/// dynamic Maps even though its initial stored payload set is the same.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalOrdinaryMapOwnedGetI32StringV1Value {
    I32(i32),
    String(SharedString),
}

/// Values admitted by the independent ordinary Map/Any owner-v2 profile.
///
/// Payloads preserve their selected TypeDesc identity. Direct Strings and
/// concrete `Label { text: string }` records each receive owner-private
/// storage when a Set operation stages a new root; neither may alias a
/// source String allocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalOrdinaryMapOwnedAnyV2Value {
    I32 {
        type_id: crate::core::ResolvedTypeId,
        value: i32,
    },
    Bool {
        type_id: crate::core::ResolvedTypeId,
        value: bool,
    },
    String {
        type_id: crate::core::ResolvedTypeId,
        text: SharedString,
    },
    /// A one-String-field nominal payload keeps its concrete nominal identity
    /// in the VM snapshot. Shape alone is not an Any descriptor: two nominal
    /// records with the same fields must remain distinct values.
    Record {
        type_id: crate::core::ResolvedTypeId,
        nominal: String,
        text: SharedString,
    },
}

/// Independently owned dynamic payload returned by the ordinary Map/Any V2
/// `GetAny` operation.  Its opaque TypeId is preserved from the selected Set
/// site, and its payload storage is detached from the source Map entry.
///
/// This is deliberately not the V1 managed Any representation: V2 records
/// retain their nominal identity while direct Strings retain the exact
/// selected String TypeDesc.
#[derive(Debug, PartialEq, Eq)]
pub struct CanonicalManagedAnyV2 {
    type_id: crate::core::ResolvedTypeId,
    payload: CanonicalManagedAnyV2Payload,
}

#[derive(Debug, PartialEq, Eq)]
enum CanonicalManagedAnyV2Payload {
    I32(i32),
    /// The payload bytes stay in the production ManagedAny-v2 runtime token.
    /// The Bytecode VM's Core site sidecar owns that token until the
    /// Checker-receipted pair Drop.
    RuntimeI32,
    Bool(bool),
    String(String),
    Record {
        nominal: String,
        text: String,
    },
}

impl CanonicalManagedAnyV2 {
    pub(crate) fn from_i32(type_id: crate::core::ResolvedTypeId, value: i32) -> Self {
        Self {
            type_id,
            payload: CanonicalManagedAnyV2Payload::I32(value),
        }
    }

    pub(crate) fn from_runtime_i32(type_id: crate::core::ResolvedTypeId) -> Self {
        Self {
            type_id,
            payload: CanonicalManagedAnyV2Payload::RuntimeI32,
        }
    }

    pub(crate) fn from_bool(type_id: crate::core::ResolvedTypeId, value: bool) -> Self {
        Self {
            type_id,
            payload: CanonicalManagedAnyV2Payload::Bool(value),
        }
    }

    pub(crate) fn from_string(type_id: crate::core::ResolvedTypeId, text: String) -> Self {
        Self {
            type_id,
            payload: CanonicalManagedAnyV2Payload::String(text),
        }
    }

    pub(crate) fn from_record(
        type_id: crate::core::ResolvedTypeId,
        nominal: String,
        text: String,
    ) -> Self {
        Self {
            type_id,
            payload: CanonicalManagedAnyV2Payload::Record { nominal, text },
        }
    }

    pub(crate) fn type_id(&self) -> &crate::core::ResolvedTypeId {
        &self.type_id
    }

    pub(crate) fn i32_value_for_type_id(
        &self,
        expected_type_id: &crate::core::ResolvedTypeId,
    ) -> Option<i32> {
        if &self.type_id != expected_type_id {
            return None;
        }
        match &self.payload {
            CanonicalManagedAnyV2Payload::I32(value) => Some(*value),
            _ => None,
        }
    }

    pub(crate) fn is_runtime_i32_for_type_id(
        &self,
        expected_type_id: &crate::core::ResolvedTypeId,
    ) -> bool {
        &self.type_id == expected_type_id
            && matches!(&self.payload, CanonicalManagedAnyV2Payload::RuntimeI32)
    }

    pub(crate) fn bool_value_for_type_id(
        &self,
        expected_type_id: &crate::core::ResolvedTypeId,
    ) -> Option<bool> {
        if &self.type_id != expected_type_id {
            return None;
        }
        match &self.payload {
            CanonicalManagedAnyV2Payload::Bool(value) => Some(*value),
            _ => None,
        }
    }

    pub(crate) fn try_deep_clone(&self) -> Result<Self, std::collections::TryReserveError> {
        let payload = match &self.payload {
            CanonicalManagedAnyV2Payload::I32(value) => CanonicalManagedAnyV2Payload::I32(*value),
            CanonicalManagedAnyV2Payload::RuntimeI32 => CanonicalManagedAnyV2Payload::RuntimeI32,
            CanonicalManagedAnyV2Payload::Bool(value) => CanonicalManagedAnyV2Payload::Bool(*value),
            CanonicalManagedAnyV2Payload::String(text) => {
                let mut copy = String::new();
                copy.try_reserve_exact(text.len())?;
                copy.push_str(text);
                CanonicalManagedAnyV2Payload::String(copy)
            }
            CanonicalManagedAnyV2Payload::Record { nominal, text } => {
                let mut nominal_copy = String::new();
                nominal_copy.try_reserve_exact(nominal.len())?;
                nominal_copy.push_str(nominal);
                let mut text_copy = String::new();
                text_copy.try_reserve_exact(text.len())?;
                text_copy.push_str(text);
                CanonicalManagedAnyV2Payload::Record {
                    nominal: nominal_copy,
                    text: text_copy,
                }
            }
        };
        Ok(Self {
            type_id: self.type_id.clone(),
            payload,
        })
    }

    /// Borrow a direct String payload only when the complete selected
    /// TypeDesc identity matches. The bytecode VM performs its own fallible
    /// copy into the result owner before the GetAny pair can be dropped.
    pub(crate) fn string_for_type_id(
        &self,
        expected_type_id: &crate::core::ResolvedTypeId,
    ) -> Option<&str> {
        if &self.type_id != expected_type_id {
            return None;
        }
        match &self.payload {
            CanonicalManagedAnyV2Payload::String(text) => Some(text),
            _ => None,
        }
    }

    /// Borrow the single String field of a concrete record payload only when
    /// both its complete TypeDesc identity and nominal record identity match.
    /// The bytecode VM must copy this text into a fresh `Value::Record` before
    /// the GetAny pair is settled.
    pub(crate) fn record_text_for_type_id(
        &self,
        expected_type_id: &crate::core::ResolvedTypeId,
        expected_nominal: &str,
    ) -> Option<&str> {
        if &self.type_id != expected_type_id {
            return None;
        }
        match &self.payload {
            CanonicalManagedAnyV2Payload::Record { nominal, text }
                if nominal == expected_nominal =>
            {
                Some(text)
            }
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn matches_i32_value(
        &self,
        expected_type_id: &crate::core::ResolvedTypeId,
        expected_value: i32,
    ) -> bool {
        self.type_id == *expected_type_id
            && matches!(self.payload, CanonicalManagedAnyV2Payload::I32(value) if value == expected_value)
    }

    #[cfg(test)]
    pub(crate) fn matches_bool_value(
        &self,
        expected_type_id: &crate::core::ResolvedTypeId,
        expected_value: bool,
    ) -> bool {
        self.type_id == *expected_type_id
            && matches!(self.payload, CanonicalManagedAnyV2Payload::Bool(value) if value == expected_value)
    }

    #[cfg(test)]
    pub(crate) fn matches_string(
        &self,
        expected_type_id: &crate::core::ResolvedTypeId,
        expected_text: &str,
    ) -> bool {
        self.type_id == *expected_type_id
            && matches!(&self.payload, CanonicalManagedAnyV2Payload::String(text) if text == expected_text)
    }

    #[cfg(test)]
    pub(crate) fn matches_record(
        &self,
        expected_type_id: &crate::core::ResolvedTypeId,
        expected_nominal: &str,
    ) -> bool {
        self.type_id == *expected_type_id
            && matches!(
                &self.payload,
                CanonicalManagedAnyV2Payload::Record { nominal, .. }
                    if nominal == expected_nominal
            )
    }

    #[cfg(test)]
    pub(crate) fn matches_record_text(
        &self,
        expected_type_id: &crate::core::ResolvedTypeId,
        expected_nominal: &str,
        expected_text: &str,
    ) -> bool {
        self.type_id == *expected_type_id
            && matches!(
                &self.payload,
                CanonicalManagedAnyV2Payload::Record { nominal, text }
                    if nominal == expected_nominal && text == expected_text
            )
    }
}

/// Private-payload owned Any for the checker-receipted i32/String v1 slice.
/// This VM oracle contains no native handle, raw token, or shared Map entry.
/// Every clone recursively creates its own String owner.
#[derive(Debug, PartialEq, Eq)]
pub struct CanonicalManagedAnyI32StringV1 {
    descriptor_version: u32,
    payload: CanonicalManagedAnyI32StringV1Payload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CanonicalManagedAnyI32StringV1Payload {
    I32(i32),
    String(String),
}

impl CanonicalManagedAnyI32StringV1 {
    pub(crate) fn from_i32(value: i32) -> Self {
        Self {
            descriptor_version: 1,
            payload: CanonicalManagedAnyI32StringV1Payload::I32(value),
        }
    }

    pub(crate) fn from_string(value: String) -> Self {
        Self {
            descriptor_version: 1,
            payload: CanonicalManagedAnyI32StringV1Payload::String(value),
        }
    }

    pub(crate) fn descriptor_version(&self) -> u32 {
        self.descriptor_version
    }

    #[cfg(test)]
    pub(crate) fn with_descriptor_version_for_test(mut self, version: u32) -> Self {
        self.descriptor_version = version;
        self
    }

    pub(crate) fn kind(&self) -> u32 {
        match &self.payload {
            CanonicalManagedAnyI32StringV1Payload::I32(_) => 1,
            CanonicalManagedAnyI32StringV1Payload::String(_) => 2,
        }
    }

    pub(crate) fn read_i32(&self) -> Option<i32> {
        match &self.payload {
            CanonicalManagedAnyI32StringV1Payload::I32(value) => Some(*value),
            CanonicalManagedAnyI32StringV1Payload::String(_) => None,
        }
    }

    pub(crate) fn string_payload(&self) -> Option<&str> {
        match &self.payload {
            CanonicalManagedAnyI32StringV1Payload::I32(_) => None,
            CanonicalManagedAnyI32StringV1Payload::String(value) => Some(value),
        }
    }
}

impl Clone for CanonicalManagedAnyI32StringV1 {
    fn clone(&self) -> Self {
        Self {
            descriptor_version: self.descriptor_version,
            payload: self.payload.clone(),
        }
    }
}

/// Runtime handle for the real ordinary Map/Any owner-v2 NewResult profile.
/// Cloning a `Value` may clone this Arc cell, but it never clones the unique
/// runtime owner. The synchronized alias count lets owner operations reject a
/// copied value; unwind cleanup can retire the exact handle through any
/// remaining alias without touching unrelated roots.
pub struct CanonicalOrdinaryMapOwnedAnyV2RuntimeRoot(
    Arc<Mutex<CanonicalOrdinaryMapOwnedAnyV2RuntimeRootCell>>,
);

struct CanonicalOrdinaryMapOwnedAnyV2RuntimeRootCell {
    aliases: usize,
    owner: Option<crate::runtime::OwnedAnyV2Root>,
}

impl CanonicalOrdinaryMapOwnedAnyV2RuntimeRoot {
    pub(crate) fn new(owner: crate::runtime::OwnedAnyV2Root) -> Self {
        Self(Arc::new(Mutex::new(
            CanonicalOrdinaryMapOwnedAnyV2RuntimeRootCell {
                aliases: 1,
                owner: Some(owner),
            },
        )))
    }

    pub(crate) fn is_unique(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .aliases
            == 1
    }

    pub(crate) fn with_owner<R>(
        &self,
        operation: impl FnOnce(&crate::runtime::OwnedAnyV2Root) -> R,
    ) -> Option<R> {
        let owner = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        owner.owner.as_ref().map(operation)
    }

    pub(crate) fn take_unique(self) -> Result<crate::runtime::OwnedAnyV2Root, Self> {
        let mut cell = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cell.aliases != 1 {
            drop(cell);
            return Err(self);
        }
        let owner = cell.owner.take();
        drop(cell);
        match owner {
            Some(owner) => Ok(owner),
            None => Err(self),
        }
    }

    /// Retire the exact owner if this cell still contains one. This is
    /// idempotent across Value aliases and deliberately does not drain any
    /// execution-domain or process-wide registry.
    pub(crate) fn cleanup(&self) {
        let owner = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .owner
            .take();
        drop(owner);
    }

    pub(crate) fn is_live(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .owner
            .is_some()
    }

    fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Clone for CanonicalOrdinaryMapOwnedAnyV2RuntimeRoot {
    fn clone(&self) -> Self {
        let mut cell = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cell.aliases = cell
            .aliases
            .checked_add(1)
            .unwrap_or_else(|| std::process::abort());
        let clone = Self(Arc::clone(&self.0));
        drop(cell);
        clone
    }
}

impl Drop for CanonicalOrdinaryMapOwnedAnyV2RuntimeRoot {
    fn drop(&mut self) {
        let mut cell = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cell.aliases == 0 {
            std::process::abort();
        }
        cell.aliases -= 1;
    }
}

impl std::fmt::Debug for CanonicalOrdinaryMapOwnedAnyV2RuntimeRoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.is_live() {
            "<ordinary Map/Any owner-v2 runtime root>"
        } else {
            "<retired ordinary Map/Any owner-v2 runtime root>"
        })
    }
}

#[derive(Debug)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(SharedString),
    Unit,
    List(Arc<Vec<Value>>),
    Set(Vec<Value>),
    Tuple(Vec<Value>),
    Variant(String, Vec<Value>),
    /// Variant value produced by the canonical MIR bytecode path.  The
    /// legacy `Variant(String, Vec<Value>)` remains available to the old
    /// compiler and builtin surface, but it deliberately carries no
    /// semantic identity.  Canonical consumers must preserve both IDs.
    CanonicalVariant {
        nominal: crate::core::ir::NominalTypeId,
        variant: crate::core::NodeId,
        tag: String,
        payload: Vec<Value>,
    },
    /// Opaque runtime value for the checker-owned Canonical MIR MapRoot
    /// island. It is separate from legacy `Record(None, fields)` Maps and is
    /// only created/consumed by dedicated MIR bytecode operations.
    CanonicalMapRoot(Arc<BTreeMap<String, CanonicalMapRootValue>>),
    /// Opaque root for the independently receipted MapAnyOwner v1 profile.
    /// This deliberately has a different runtime identity from CanonicalMapRoot.
    CanonicalMapAnyOwnerV1(Arc<BTreeMap<String, CanonicalMapAnyOwnerV1Value>>),
    /// Opaque root for source-compatible persistent ordinary Map updates.
    /// This runtime identity is independent from MapRoot and MapAnyOwner v1.
    CanonicalOrdinaryMapOwnerLifecycleI32StringV1(
        Arc<BTreeMap<String, CanonicalOrdinaryMapOwnerLifecycleI32StringV1Value>>,
    ),
    /// Opaque root for the separately receipted typed owned-Get ordinary Map
    /// profile. Persistent updates produce a new root and preserve the
    /// source root value.
    CanonicalOrdinaryMapOwnedGetI32StringV1(
        Arc<BTreeMap<String, CanonicalOrdinaryMapOwnedGetI32StringV1Value>>,
    ),
    /// Opaque root for the independent concrete-record ordinary Map/Any
    /// owner-v2 profile.  This runtime identity is separate from all v1 and
    /// legacy Map values; its payloads are deep-copied snapshots.
    CanonicalOrdinaryMapOwnedAnyV2(Arc<BTreeMap<String, CanonicalOrdinaryMapOwnedAnyV2Value>>),
    /// Unique real-runtime owner-v2 root created by the narrow recoverable
    /// NewResult opcode. It is distinct from snapshot-backed V2 roots, which
    /// remain limited to the existing owner-v2 routes.
    CanonicalOrdinaryMapOwnedAnyV2RuntimeRoot(CanonicalOrdinaryMapOwnedAnyV2RuntimeRoot),
    /// Dedicated owned Any; construction and extraction require canonical MIR receipts.
    CanonicalManagedAnyI32StringV1(CanonicalManagedAnyI32StringV1),
    /// Dedicated owned dynamic Any for the independently receipted
    /// ordinary Map/Any owner-v2 profile.
    CanonicalManagedAnyV2(Arc<CanonicalManagedAnyV2>),
    Record(Option<String>, HashMap<String, Value>),
    /// Poll-based future. Can be Ready (result available) or Pending (waiting on channel).
    Future(std::sync::Arc<std::sync::Mutex<crate::interp::PollFuture>>),
    Error(String),
    ArenaRef(usize, usize, u64), // (arena_id, slot_idx, generation)
    ArenaBlock(usize),
    QuoteAst(Box<QuotedAst>),
    Newtype(String, Box<Value>),
    Actor(super::ActorHandle),
    CanonicalActorHandle(super::bytecode::actor::CanonicalActorHandleValue),
    CanonicalActorState(super::bytecode::actor::CanonicalActorStateValue),
    Closure {
        params: Vec<Param>,
        ret: Option<Type>,
        body: Block,
        captured: HashMap<String, Value>,
    },
    /// Bytecode closure: stores function proto index + captured environment.
    /// 0.35.27 (C3): carries the owning program's Arc — the closure is
    /// self-contained. Cross-thread evaluation (FFI callbacks, actor workers)
    /// always uses THIS program, so the proto index can never be resolved
    /// against a freed or mismatched program, and the program outlives the
    /// VM that created the closure (no UAF).
    BytecodeClosure {
        proto: u32,
        captured: HashMap<String, Value>,
        program: std::sync::Arc<crate::interp::bytecode::instr::BytecodeProgram>,
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
    /// Borrowed record/tuple projection that aliases its root variable.
    PlaceRef {
        owner: String,
        projections: Vec<RuntimeProjection>,
    },
    /// Mutable record/tuple projection that writes back to its root variable.
    PlaceRefMut {
        owner: String,
        projections: Vec<RuntimeProjection>,
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

/// 0.31.23: Manual Clone implementation for Value.
///
/// This implementation documents the cost of cloning each variant:
/// - Scalar types (Int, Float, Bool, Unit): cheap (stack copy)
/// - String: moderate (heap allocation + copy)
/// - List/Set/Tuple/Array: expensive (deep copy of all elements)
/// - Record: expensive (deep copy of all fields)
/// - Closure: expensive (deep copy of captured variables)
/// - Shared/Ref/RefMut: cheap (Arc clone)
/// - Future: cheap (Arc clone)
///
/// For sharing without copying, use `Value::share()` instead.
impl Clone for Value {
    fn clone(&self) -> Self {
        match self {
            Value::Int(v) => Value::Int(*v),
            Value::Float(v) => Value::Float(*v),
            Value::Bool(v) => Value::Bool(*v),
            Value::String(v) => Value::String(SharedString::clone(v)),
            Value::Unit => Value::Unit,
            Value::List(v) => Value::List(Arc::clone(v)),
            Value::Set(v) => Value::Set(v.clone()),
            Value::Tuple(v) => Value::Tuple(v.clone()),
            Value::Variant(name, v) => Value::Variant(name.clone(), v.clone()),
            Value::CanonicalVariant {
                nominal,
                variant,
                tag,
                payload,
            } => Value::CanonicalVariant {
                nominal: nominal.clone(),
                variant: variant.clone(),
                tag: tag.clone(),
                payload: payload.clone(),
            },
            Value::CanonicalMapRoot(root) => Value::CanonicalMapRoot(Arc::clone(root)),
            Value::CanonicalMapAnyOwnerV1(root) => Value::CanonicalMapAnyOwnerV1(Arc::clone(root)),
            Value::CanonicalOrdinaryMapOwnerLifecycleI32StringV1(root) => {
                Value::CanonicalOrdinaryMapOwnerLifecycleI32StringV1(Arc::clone(root))
            }
            Value::CanonicalOrdinaryMapOwnedGetI32StringV1(root) => {
                Value::CanonicalOrdinaryMapOwnedGetI32StringV1(Arc::clone(root))
            }
            Value::CanonicalOrdinaryMapOwnedAnyV2(root) => {
                Value::CanonicalOrdinaryMapOwnedAnyV2(Arc::clone(root))
            }
            Value::CanonicalOrdinaryMapOwnedAnyV2RuntimeRoot(root) => {
                Value::CanonicalOrdinaryMapOwnedAnyV2RuntimeRoot(root.clone())
            }
            Value::CanonicalManagedAnyI32StringV1(value) => {
                Value::CanonicalManagedAnyI32StringV1(value.clone())
            }
            Value::CanonicalManagedAnyV2(value) => Value::CanonicalManagedAnyV2(Arc::clone(value)),
            Value::Record(name, fields) => Value::Record(name.clone(), fields.clone()),
            Value::Future(v) => Value::Future(Arc::clone(v)),
            Value::Error(v) => Value::Error(v.clone()),
            Value::ArenaRef(a, b, c) => Value::ArenaRef(*a, *b, *c),
            Value::ArenaBlock(v) => Value::ArenaBlock(*v),
            Value::QuoteAst(v) => Value::QuoteAst(v.clone()),
            Value::Newtype(name, v) => Value::Newtype(name.clone(), v.clone()),
            Value::Actor(v) => Value::Actor(v.clone()),
            Value::CanonicalActorHandle(v) => Value::CanonicalActorHandle(v.clone()),
            Value::CanonicalActorState(v) => Value::CanonicalActorState(v.clone()),
            Value::Closure {
                params,
                ret,
                body,
                captured,
            } => Value::Closure {
                params: params.clone(),
                ret: ret.clone(),
                body: body.clone(),
                captured: captured.clone(),
            },
            Value::BytecodeClosure {
                proto,
                captured,
                program,
            } => Value::BytecodeClosure {
                proto: *proto,
                captured: captured.clone(),
                program: std::sync::Arc::clone(program),
            },
            Value::Shared(v) => Value::Shared(Arc::clone(v)),
            Value::LocalShared(v) => Value::LocalShared(v.clone()),
            Value::WeakShared(v) => Value::WeakShared(v.clone()),
            Value::WeakLocal(v) => Value::WeakLocal(v.clone()),
            Value::Cap(v) => Value::Cap(v.clone()),
            Value::Ref(v) => Value::Ref(Arc::clone(v)),
            Value::RefMut(v) => Value::RefMut(Arc::clone(v)),
            Value::IndexRef { owner, index } => Value::IndexRef {
                owner: owner.clone(),
                index: *index,
            },
            Value::IndexRefMut { owner, index } => Value::IndexRefMut {
                owner: owner.clone(),
                index: *index,
            },
            Value::PlaceRef { owner, projections } => Value::PlaceRef {
                owner: owner.clone(),
                projections: projections.clone(),
            },
            Value::PlaceRefMut { owner, projections } => Value::PlaceRefMut {
                owner: owner.clone(),
                projections: projections.clone(),
            },
            Value::Type(v) => Value::Type(v.clone()),
            Value::Allocator(v) => Value::Allocator(*v),
            Value::Array(v) => Value::Array(v.clone()),
            Value::Slice { source, start, end } => Value::Slice {
                source: source.clone(),
                start: *start,
                end: *end,
            },
            Value::Range { start, end } => Value::Range {
                start: *start,
                end: *end,
            },
            Value::CBuffer(v) => Value::CBuffer(Arc::clone(v)),
            Value::DynTrait {
                data,
                concrete_type,
                trait_names,
            } => Value::DynTrait {
                data: data.clone(),
                concrete_type: concrete_type.clone(),
                trait_names: trait_names.clone(),
            },
        }
    }
}

impl Value {
    /// 0.31.23: Share this value without copying.
    ///
    /// Returns an `Arc<Value>` that can be shared across threads.
    /// This is cheap (just increments the reference count) and should be
    /// preferred over `clone()` when sharing is the goal.
    pub fn share(self) -> Arc<Self> {
        Arc::new(self)
    }

    /// 0.31.23: Explicit deep clone.
    ///
    /// This is equivalent to `clone()` but makes the cost explicit.
    /// Use this when you truly need a separate copy of the value.
    /// For sharing, prefer `share()` instead.
    pub fn deep_clone(&self) -> Self {
        self.clone()
    }
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

// ---------------------------------------------------------------------------
// 0.31.23: Shared mutable collections (Rc<RefCell> wrapping)
// ---------------------------------------------------------------------------
// Blind review fix: List/Map/Set cloning was expensive (deep copy).
// These wrapper types enable shared mutability: cloning only copies the
// smart pointer (cheap), and mutations are visible through all references.
//
// NOTE: These types are infrastructure for future integration into the Value
// enum. They are not currently used but provide the foundation for eliminating
// expensive deep copies of collections.

/// 0.31.23: Shared mutable list.
///
/// Wraps `Vec<Value>` in `Rc<RefCell<...>>` for shared mutability.
/// Cloning is cheap (just increments the reference count).
/// Mutations are visible through all clones.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct SharedList(pub Rc<RefCell<Vec<Value>>>);

impl SharedList {
    /// Create a new shared list from a vector.
    pub fn new(items: Vec<Value>) -> Self {
        SharedList(Rc::new(RefCell::new(items)))
    }

    /// Create an empty shared list.
    pub fn empty() -> Self {
        SharedList(Rc::new(RefCell::new(Vec::new())))
    }

    /// Get the length of the list.
    pub fn len(&self) -> usize {
        self.0.borrow().len()
    }

    /// Check if the list is empty.
    pub fn is_empty(&self) -> bool {
        self.0.borrow().is_empty()
    }

    /// Push an element to the list.
    pub fn push(&self, value: Value) {
        self.0.borrow_mut().push(value);
    }

    /// Get a clone of the element at the given index.
    pub fn get(&self, index: usize) -> Option<Value> {
        self.0.borrow().get(index).cloned()
    }

    /// Set the element at the given index.
    pub fn set(&self, index: usize, value: Value) -> Result<(), InterpError> {
        let mut list = self.0.borrow_mut();
        if index >= list.len() {
            return Err(InterpError::IndexOutOfBounds(Box::new(ErrorContext {
                msg: format!(
                    "index {} out of bounds for list of length {}",
                    index,
                    list.len()
                ),
                function: None,
                operation: Some("list_set".to_string()),
                help: None,
                call_stack: vec![],
                line: None,
            })));
        }
        list[index] = value;
        Ok(())
    }

    /// Get a clone of the underlying vector.
    pub fn to_vec(&self) -> Vec<Value> {
        self.0.borrow().clone()
    }

    /// Iterate over cloned elements.
    pub fn iter_cloned(&self) -> impl Iterator<Item = Value> + '_ {
        self.0
            .borrow()
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
    }
}

/// 0.31.23: Shared mutable set.
///
/// Wraps `Vec<Value>` in `Rc<RefCell<...>>` for shared mutability.
/// Note: This is a set represented as a vector (no duplicate checking).
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct SharedSet(pub Rc<RefCell<Vec<Value>>>);

impl SharedSet {
    /// Create a new shared set from a vector.
    pub fn new(items: Vec<Value>) -> Self {
        SharedSet(Rc::new(RefCell::new(items)))
    }

    /// Create an empty shared set.
    pub fn empty() -> Self {
        SharedSet(Rc::new(RefCell::new(Vec::new())))
    }

    /// Get the length of the set.
    pub fn len(&self) -> usize {
        self.0.borrow().len()
    }

    /// Check if the set is empty.
    pub fn is_empty(&self) -> bool {
        self.0.borrow().is_empty()
    }

    /// Insert an element into the set (no duplicate checking).
    pub fn insert(&self, value: Value) {
        self.0.borrow_mut().push(value);
    }

    /// Get a clone of the underlying vector.
    pub fn to_vec(&self) -> Vec<Value> {
        self.0.borrow().clone()
    }
}

/// 0.31.23: Shared mutable record.
///
/// Wraps `HashMap<String, Value>` in `Rc<RefCell<...>>` for shared mutability.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct SharedRecord {
    pub name: Option<String>,
    pub fields: Rc<RefCell<HashMap<String, Value>>>,
}

impl SharedRecord {
    /// Create a new shared record.
    pub fn new(name: Option<String>, fields: HashMap<String, Value>) -> Self {
        SharedRecord {
            name,
            fields: Rc::new(RefCell::new(fields)),
        }
    }

    /// Create an empty shared record.
    pub fn empty(name: Option<String>) -> Self {
        SharedRecord {
            name,
            fields: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    /// Get a clone of a field value.
    pub fn get(&self, key: &str) -> Option<Value> {
        self.fields.borrow().get(key).cloned()
    }

    /// Set a field value.
    pub fn set(&self, key: String, value: Value) {
        self.fields.borrow_mut().insert(key, value);
    }

    /// Get a clone of the underlying hashmap.
    pub fn to_map(&self) -> HashMap<String, Value> {
        self.fields.borrow().clone()
    }
}

/// Kind of allocator
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocatorKind {
    System,
    Arena,
    Bump,
}

/// C buffer wrapper that automatically frees memory on drop.
///
/// H9 (0.35.37): the fields are **private** on purpose. `unsafe impl Sync`
/// is only sound because shared references (`&CBufferInner`) expose read-only
/// accessors and *no* `&self` path can mutate the buffer; mutation requires
/// `&mut CBufferInner` or ownership, which is exactly once (Arc + Drop).
/// Public raw-pointer fields would let any holder of `&CBufferInner` write
/// through the pointer from another thread — a data race the type system
/// could not catch. Privacy is the structural enforcement of the FFI
/// contract that previously existed only as a comment.
pub struct CBufferInner {
    ptr: *mut u8,
    size: usize,
}

impl CBufferInner {
    /// Create a buffer wrapper taking ownership of `ptr` (from
    /// libc::malloc/calloc) of `size` bytes. Freed exactly once in Drop.
    ///
    /// # Safety
    /// `ptr` must be non-null unless `size` is zero, must be the sole owner
    /// of a libc allocation (malloc/calloc), and must not be wrapped by
    /// another `CBufferInner` or otherwise freed before this value drops.
    pub unsafe fn new(ptr: *mut u8, size: usize) -> Self {
        CBufferInner { ptr, size }
    }

    /// Read-only view of the buffer pointer. Safe to call on `&self`:
    /// reading the pointer does not dereference it. Shared references must
    /// not obtain a mutable pointer (batch2 P3-1); use `as_mut_ptr` on a
    /// `&mut` value when mutation is required.
    pub fn as_ptr(&self) -> *const u8 {
        self.ptr
    }

    /// Mutable view of the buffer pointer. Requires `&mut self` so shared
    /// references cannot race on the buffer contents.
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr
    }

    /// Read-only view of the buffer size.
    pub fn len(&self) -> usize {
        self.size
    }

    /// Whether the buffer is empty (null pointer or zero size).
    pub fn is_empty(&self) -> bool {
        self.ptr.is_null() || self.size == 0
    }
}

// SAFETY: CBufferInner is Send because it uniquely owns a heap-allocated buffer
// (ptr/size) obtained from libc::malloc/calloc and freed exactly once in Drop.
// Moving the value across threads does not alias or split ownership of the
// buffer; only the final Drop frees it.
unsafe impl Send for CBufferInner {}
// SAFETY: CBufferInner is Sync because its fields are private and no `&self`
// method mutates the buffer: `as_ptr`/`len`/`is_empty` only read the fields,
// and the buffer contents are only reachable through a mutable reference or
// ownership. Shared references (e.g. through Arc<CBufferInner>) therefore
// cannot race on buffer contents; Drop runs exactly once (Arc ownership).
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
        write!(f, "CBuffer({:p}, {} bytes)", self.as_ptr(), self.len())
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
    /// v0.31.11: `actor Name runs FlowName` — the actor owns a Flow instance.
    /// When set, messages decode as Flow events and dispatch through transitions.
    pub runs_flow: Option<String>,
    /// v0.31.11: current Flow state (the actor's business state).
    /// Initialized to the flow's root state on spawn; updated on each transition.
    pub flow_state: Option<Value>,
    /// v0.29.11: set when the owning Flow entered Fault. Mailbox dispatch
    /// short-circuits (O(1)) while this is true — messages are dropped without
    /// waking business logic.
    pub faulted: bool,
    /// v0.29.20: peer actor ids linked for PeerFault propagation.
    /// When this actor faults, each peer receives a `peer_fault` notification
    /// (link-disconnect injection). Stored as actor ids (not handles) to avoid
    /// reference cycles; peers are resolved via the global actor registry.
    pub peer_links: Vec<usize>,
    /// v0.29.37: parent actor id (None for top-level actors).
    /// Used for SystemKill cascade: when parent faults/drops, non-detached
    /// children are killed.
    pub parent_id: Option<usize>,
    /// v0.29.37: detached actors survive parent SystemKill.
    /// Set by `spawn_detached` builtin.
    pub is_detached: bool,
    /// v0.29.46: actor IDs that have sent messages to this actor.
    /// When this actor enters mute (backpressure), all producers are
    /// push-muted to cascade backpressure upstream (white-paper §6.7,
    /// Pony-style full-actor muting).
    pub producers: Vec<usize>,
}

/// Message sent to an actor's mailbox for FIFO processing.
pub struct ActorMailboxMsg {
    pub method: String,
    pub args: Vec<Value>,
    pub response: std::sync::mpsc::Sender<Result<Value, InterpError>>,
    /// v0.29.21: absolute deadline (Instant) for TTL deadlock break.
    /// `None` means no deadline (default path).
    pub deadline: Option<std::time::Instant>,
}

/// Default mailbox high-water depth when no `@mailbox(depth=N)` is configured.
pub const DEFAULT_MAILBOX_DEPTH: usize = 2048;
/// Default mute cooldown ticks before an overloaded actor can unmute.
pub const DEFAULT_MUTE_COOLDOWN_TICKS: u64 = 3;
/// Default message TTL for backpressure wait (milliseconds).
pub const DEFAULT_MAILBOX_TTL_MS: u64 = 50;

/// Shared mailbox backpressure state (v0.29.21).
///
/// Tracked separately from `ActorInstance` so send sites can update depth
/// without taking the instance write lock for every message.
#[derive(Debug)]
pub struct MailboxBpState {
    /// High-water depth from `@mailbox(depth=N)` (or default).
    pub depth_limit: std::sync::atomic::AtomicUsize,
    /// Approximate number of in-flight messages (enqueued, not yet completed).
    pub depth: std::sync::atomic::AtomicUsize,
    /// True while this actor is muted (full-actor mute strategy).
    pub muted: std::sync::atomic::AtomicBool,
    /// Monotonic mute generation — bumped on each mute transition.
    pub mute_gen: std::sync::atomic::AtomicU64,
    /// Earliest Instant (as millis since UNIX_EPOCH) when unmute is allowed.
    /// Stored as millis for atomic updates; 0 means not muted / no cooldown.
    pub unmute_after_ms: std::sync::atomic::AtomicU64,
    /// TTL for blocking send while muted (milliseconds).
    pub ttl_ms: u64,
    /// Cooldown duration in milliseconds (N ticks × tick_ms).
    pub cooldown_ms: u64,
}

impl MailboxBpState {
    pub fn new(depth_limit: usize) -> Self {
        Self {
            depth_limit: std::sync::atomic::AtomicUsize::new(depth_limit.max(1)),
            depth: std::sync::atomic::AtomicUsize::new(0),
            muted: std::sync::atomic::AtomicBool::new(false),
            mute_gen: std::sync::atomic::AtomicU64::new(0),
            unmute_after_ms: std::sync::atomic::AtomicU64::new(0),
            ttl_ms: DEFAULT_MAILBOX_TTL_MS,
            cooldown_ms: DEFAULT_MUTE_COOLDOWN_TICKS * 10, // 10ms/tick default
        }
    }

    fn now_ms() -> u64 {
        // H13-fix: log system clock failure instead of silently returning 0.
        // A return of 0 means "epoch (1970)" which breaks mailbox TTL/deadline
        // logic. Logging makes the failure visible.
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or_else(|e| {
                eprintln!("[mimi runtime] system clock error: {}", e);
                0
            })
    }

    /// Current approximate depth.
    pub fn current_depth(&self) -> usize {
        self.depth.load(std::sync::atomic::Ordering::Acquire)
    }

    /// True if muted and still inside the cooldown window.
    pub fn is_muted(&self) -> bool {
        if !self.muted.load(std::sync::atomic::Ordering::Acquire) {
            return false;
        }
        let until = self
            .unmute_after_ms
            .load(std::sync::atomic::Ordering::Acquire);
        let now = Self::now_ms();
        if until == 0 || now >= until {
            // DAT-C5 (deep audit): cooldown elapsed — check if depth is still over limit.
            // If depth is under the limit, we can unmute now. Otherwise stay muted.
            let d = self.depth.load(std::sync::atomic::Ordering::Acquire);
            let limit = self.depth_limit.load(std::sync::atomic::Ordering::Acquire);
            if d <= limit {
                // Depth is under limit — clear the mute flag ourselves (don't wait for try_unmute).
                self.muted
                    .store(false, std::sync::atomic::Ordering::Release);
                return false;
            }
            return true;
        }
        true
    }

    /// Enter mute with cooldown debounce.
    pub fn enter_mute(&self) {
        let until = Self::now_ms().saturating_add(self.cooldown_ms);
        self.unmute_after_ms
            .store(until, std::sync::atomic::Ordering::Release);
        self.muted.store(true, std::sync::atomic::Ordering::Release);
        self.mute_gen
            .fetch_add(1, std::sync::atomic::Ordering::Release);
    }

    /// Leave mute if depth is under the high-water mark and cooldown elapsed.
    pub fn try_unmute(&self) {
        if !self.muted.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let until = self
            .unmute_after_ms
            .load(std::sync::atomic::Ordering::Acquire);
        let now = Self::now_ms();
        if until != 0 && now < until {
            return; // still in cooldown
        }
        let d = self.depth.load(std::sync::atomic::Ordering::Acquire);
        // Unmute when depth falls to ≤ 50% of limit (hysteresis).
        let low = self.depth_limit.load(std::sync::atomic::Ordering::Acquire) / 2;
        if d <= low {
            self.muted
                .store(false, std::sync::atomic::Ordering::Release);
            self.unmute_after_ms
                .store(0, std::sync::atomic::Ordering::Release);
        }
    }

    /// Increment depth; enter mute if over high-water mark. Returns new depth.
    pub fn on_enqueue(&self) -> usize {
        let d = self.depth.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
        if d > self.depth_limit.load(std::sync::atomic::Ordering::Acquire) {
            self.enter_mute();
        }
        d
    }

    /// Decrement depth (message completed or rejected); try unmute.
    pub fn on_dequeue(&self) {
        let _ = self.depth.fetch_update(
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
            |v| Some(v.saturating_sub(1)),
        );
        self.try_unmute();
    }
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
    /// v0.29.21: mailbox backpressure state (depth / mute / cooldown / TTL).
    pub bp: std::sync::Arc<MailboxBpState>,
    /// v0.33: compiled bytecode program for actor worker threads.
    /// When set, the worker thread uses BytecodeVM instead of tree-walker.
    pub bytecode_program: Option<std::sync::Arc<crate::interp::bytecode::BytecodeProgram>>,
    /// v0.33: shared stdout capture buffer for actor worker threads.
    pub stdout_buf: Option<std::sync::Arc<std::sync::Mutex<String>>>,
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

/// Live actor registry entry for PeerFault peer resolution (v0.29.20).
///
/// R-C9: store *weak* references so the registry does not keep actors and
/// worker threads alive after all user handles are dropped. Upgrade fails
/// once the last strong `ActorHandle` is gone.
#[derive(Clone)]
pub(crate) struct WeakActorEntry {
    pub id: usize,
    inner: std::sync::Weak<std::sync::RwLock<ActorInstance>>,
    mailbox: std::sync::mpsc::Sender<ActorMailboxMsg>,
    program: std::sync::Weak<crate::ast::File>,
    bp: std::sync::Weak<MailboxBpState>,
}

impl WeakActorEntry {
    fn from_handle(h: &ActorHandle) -> Self {
        Self {
            id: h.id,
            inner: std::sync::Arc::downgrade(&h.inner),
            mailbox: h.mailbox.clone(),
            program: std::sync::Arc::downgrade(&h.program),
            bp: std::sync::Arc::downgrade(&h.bp),
        }
    }

    pub(crate) fn upgrade(&self) -> Option<ActorHandle> {
        Some(ActorHandle {
            inner: self.inner.upgrade()?,
            mailbox: self.mailbox.clone(),
            id: self.id,
            program: self.program.upgrade()?,
            bp: self.bp.upgrade()?,
            bytecode_program: None,
            stdout_buf: None,
        })
    }
}

/// Live actor handles by id for PeerFault peer resolution (v0.29.20).
/// Entries are inserted in `ActorHandle::new` and removed on short-circuit.
/// v0.29.37: pub(crate) for SystemKill cascade in actor.rs.
/// R-C9: weak entries only — no strong ownership.
static ACTOR_HANDLES: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<usize, WeakActorEntry>>,
> = std::sync::OnceLock::new();

pub(crate) fn actor_handles(
) -> &'static std::sync::Mutex<std::collections::HashMap<usize, WeakActorEntry>> {
    ACTOR_HANDLES.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

// Thread-local flag set when inside an actor's worker thread.
// Used to detect self-calls and avoid mailbox deadlock.
// v0.29.37: pub(crate) for SystemKill parent tracking in actor.rs.
thread_local! {
    pub(crate) static CURRENT_ACTOR_ID: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl ActorHandle {
    /// 0.33 Phase F: tree-walker actor paths removed.
    /// Use new_bytecode() for all actor creation.

    /// v0.33: Create actor with bytecode VM worker thread.
    /// The worker thread uses BytecodeVM to execute actor methods instead of
    /// the tree-walker Interpreter. This is the path used by the bytecode VM's
    /// spawn_actor; the tree-walker path uses new_with_depth.
    pub(crate) fn new_bytecode(
        instance: ActorInstance,
        program: std::sync::Arc<crate::ast::File>,
        bytecode_prog: std::sync::Arc<crate::interp::bytecode::BytecodeProgram>,
        stdout_buf: Option<std::sync::Arc<std::sync::Mutex<String>>>,
        verify_contracts: bool,
        verify_ffi: bool,
        ffi_library_path: Option<String>,
    ) -> Self {
        Self::new_bytecode_inner(
            instance,
            program,
            bytecode_prog,
            stdout_buf,
            verify_contracts,
            verify_ffi,
            ffi_library_path,
            None,
        )
    }

    pub(crate) fn new_bytecode_with_binding_snapshot(
        instance: ActorInstance,
        program: std::sync::Arc<crate::ast::File>,
        bytecode_prog: std::sync::Arc<crate::interp::bytecode::BytecodeProgram>,
        stdout_buf: Option<std::sync::Arc<std::sync::Mutex<String>>>,
        verify_contracts: bool,
        verify_ffi: bool,
        ffi_binding_snapshot: Option<
            std::sync::Arc<crate::interp::bytecode::mir_ffi::CanonicalMirFfiBindingSnapshot>,
        >,
    ) -> Self {
        Self::new_bytecode_inner(
            instance,
            program,
            bytecode_prog,
            stdout_buf,
            verify_contracts,
            verify_ffi,
            None,
            ffi_binding_snapshot,
        )
    }

    fn new_bytecode_inner(
        instance: ActorInstance,
        program: std::sync::Arc<crate::ast::File>,
        bytecode_prog: std::sync::Arc<crate::interp::bytecode::BytecodeProgram>,
        stdout_buf: Option<std::sync::Arc<std::sync::Mutex<String>>>,
        verify_contracts: bool,
        verify_ffi: bool,
        ffi_library_path: Option<String>,
        ffi_binding_snapshot: Option<
            std::sync::Arc<crate::interp::bytecode::mir_ffi::CanonicalMirFfiBindingSnapshot>,
        >,
    ) -> Self {
        let id = ACTOR_HANDLE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let (mailbox_tx, mailbox_rx) = std::sync::mpsc::channel::<ActorMailboxMsg>();
        let inner = std::sync::Arc::new(std::sync::RwLock::new(instance));
        let bp = std::sync::Arc::new(MailboxBpState::new(DEFAULT_MAILBOX_DEPTH));

        let worker_inner = inner.clone();
        let worker_bp = bp.clone();
        let worker_bc = bytecode_prog.clone();
        let mailbox_tx_clone = mailbox_tx.clone();
        let worker_program = program.clone();
        let worker_stdout = stdout_buf.clone();
        let worker_verify_contracts = verify_contracts;
        let worker_verify_ffi = verify_ffi;
        let worker_ffi_library_path = ffi_library_path;
        let worker_ffi_binding_snapshot = ffi_binding_snapshot;

        let _ = std::thread::Builder::new()
            .name(format!("actor-{}", id))
            .spawn(move || {
                CURRENT_ACTOR_ID.with(|a| a.set(id));
                while let Ok(msg) = mailbox_rx.recv() {
                    worker_bp.on_dequeue();
                    if let Some(deadline) = msg.deadline {
                        if std::time::Instant::now() >= deadline {
                            let _ = msg.response.send(Err(InterpError::new(
                                "SendErrorNotWriteable: mailbox message TTL expired",
                            )));
                            continue;
                        }
                    }
                    // Fault check: drain without dispatch.
                    if worker_inner.read().map(|a| a.faulted).unwrap_or(false) {
                        let _ = msg.response.send(Err(InterpError::new(
                            "actor mailbox short-circuited (Fault)",
                        )));
                        continue;
                    }

                    // Determine if this is a flow actor or regular actor.
                    let (runs_flow, flow_state, actor_name_for_dispatch) = {
                        let actor = worker_inner.read().unwrap_or_else(|e| e.into_inner());
                        (
                            actor.runs_flow.clone(),
                            actor.flow_state.clone(),
                            actor.actor_name.clone(),
                        )
                    };

                    // 0.39.135 (L1 parity): a runs-flow actor may ALSO declare
                    // plain methods (`func ping()`); codegen resolves those to
                    // direct method calls at compile time. The mailbox worker
                    // previously routed EVERY message through transition
                    // dispatch for flow actors, so `w.ping()` died with
                    // "no transition ping from state …" while native returned
                    // the method result. Methods take precedence; unmatched
                    // names fall through to transition dispatch.
                    let has_matching_method = worker_bc
                        .actor_method_funcs
                        .contains_key(&(actor_name_for_dispatch, msg.method.clone()));

                    let result = if let Some(flow_name) =
                        runs_flow.filter(|_| !has_matching_method)
                    {
                        // Flow actor: dispatch through transition functions.
                        let from_state = match &flow_state {
                            Some(Value::Record(Some(name), _)) => name.clone(),
                            _ => {
                                let _ = msg.response.send(Err(InterpError::new(
                                    "flow actor: no valid flow_state",
                                )));
                                continue;
                            }
                        };
                        let key = (flow_name, msg.method.clone(), from_state);
                        match worker_bc.flow_transition_funcs.get(&key) {
                            Some(&func_idx) => {
                                let current_state = flow_state.unwrap_or(Value::Unit);
                                // Arity check: transition func takes (self/state + params).
                                let expected = worker_bc.functions[func_idx as usize].param_count as usize;
                                if 1 + msg.args.len() != expected {
                                    let err = InterpError::new(format!(
                                        "wrong arg count for transition '{}' from state '{}': expected {}, got {}",
                                        msg.method, key.2, expected - 1, msg.args.len()
                                    ));
                                    let _ = msg.response.send(Err(err));
                                    continue;
                                }
                                let mut args = vec![current_state.clone()];
                                args.extend(msg.args);
                                let mut vm = crate::interp::bytecode::BytecodeVM::new(worker_bc.clone());
                                if let Some(ref buf) = worker_stdout {
                                    vm.set_stdout_buf(buf.clone());
                                }
                                vm.verify_contracts = worker_verify_contracts;
                                vm.set_verify_ffi(worker_verify_ffi);
                                if let Some(snapshot) = &worker_ffi_binding_snapshot {
                                    vm.inherit_canonical_ffi_binding_snapshot(
                                        std::sync::Arc::clone(snapshot),
                                    );
                                } else if let Some(path) = worker_ffi_library_path.clone() {
                                    vm.set_canonical_ffi_library_path(path);
                                }
                                // Use wrap_ok for fails transitions so Op::Ret
                                // wraps the body value in Ok/Err Variant matching
                                // tree-walker's eval_flow_transition convention.
                                let r = if worker_bc.flow_fails_transitions.contains(&key) {
                                    vm.call_function_wrap_ok(func_idx, &args, current_state)
                                } else {
                                    vm.call_function(func_idx, &args)
                                };
                                // Update flow_state on success.
                                if let Ok(ref new_state) = r {
                                    if let Ok(mut actor) = worker_inner.write() {
                                        // For fails transitions, unwrap the Ok/Err
                                        // wrapper to get the actual state value
                                        // (mirrors tree-walker value.rs:1164-1184).
                                        let actual_state = if worker_bc.flow_fails_transitions.contains(&key) {
                                            match new_state {
                                                Value::Variant(tag, inner) if tag == "Ok" => {
                                                    inner.first().cloned().unwrap_or(Value::Unit)
                                                }
                                                Value::Variant(tag, inner) if tag == "Err" => {
                                                    match inner.first() {
                                                        Some(Value::Tuple(elems)) => {
                                                            elems.first().cloned().unwrap_or(Value::Unit)
                                                        }
                                                        other => other.cloned().unwrap_or(Value::Unit),
                                                    }
                                                }
                                                other => other.clone(),
                                            }
                                        } else {
                                            new_state.clone()
                                        };
                                        actor.flow_state = Some(actual_state);
                                    }
                                }
                                r
                            }
                            None => Err(InterpError::new(format!(
                                "no transition {} from state {}",
                                msg.method, key.2
                            ))),
                        }
                    } else {
                        // Regular actor: call method function.
                        let actor_name = {
                            let actor = worker_inner.read().unwrap_or_else(|e| e.into_inner());
                            actor.actor_name.clone()
                        };
                        let key = (actor_name, msg.method.clone());
                        match worker_bc.actor_method_funcs.get(&key) {
                            Some(&func_idx) => {
                                // Arity check: method func takes (self + params).
                                let expected = worker_bc.functions[func_idx as usize].param_count as usize;
                                if 1 + msg.args.len() != expected {
                                    let err = InterpError::new(format!(
                                        "wrong arg count for method '{}': expected {}, got {}",
                                        msg.method, expected - 1, msg.args.len()
                                    ));
                                    let _ = msg.response.send(Err(err));
                                    continue;
                                }
                                // Build self handle for the method.
                                let self_val = Value::Actor(ActorHandle {
                                    inner: worker_inner.clone(),
                                    mailbox: mailbox_tx_clone.clone(),
                                    id,
                                    program: worker_program.clone(),
                                    bp: worker_bp.clone(),
                                    bytecode_program: Some(worker_bc.clone()),
                                    stdout_buf: worker_stdout.clone(),
                                });
                                let mut args = vec![self_val];
                                args.extend(msg.args);
                                let mut vm = crate::interp::bytecode::BytecodeVM::new(worker_bc.clone());
                                if let Some(ref buf) = worker_stdout {
                                    vm.set_stdout_buf(buf.clone());
                                }
                                vm.verify_contracts = worker_verify_contracts;
                                vm.set_verify_ffi(worker_verify_ffi);
                                if let Some(snapshot) = &worker_ffi_binding_snapshot {
                                    vm.inherit_canonical_ffi_binding_snapshot(
                                        std::sync::Arc::clone(snapshot),
                                    );
                                } else if let Some(path) = worker_ffi_library_path.clone() {
                                    vm.set_canonical_ffi_library_path(path);
                                }
                                vm.call_function(func_idx, &args)
                            }
                            None => Err(InterpError::new(format!(
                                "actor method '{}' not found", msg.method
                            ))),
                        }
                    };
                    let _ = msg.response.send(result);
                }
                CURRENT_ACTOR_ID.with(|a| a.set(0));
            });

        let handle = ActorHandle {
            inner,
            mailbox: mailbox_tx,
            id,
            program,
            bp,
            bytecode_program: Some(bytecode_prog),
            stdout_buf,
        };
        if let Ok(mut map) = actor_handles().lock() {
            map.retain(|_, e| e.upgrade().is_some());
            map.insert(id, WeakActorEntry::from_handle(&handle));
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
            let Ok(mut map) = actor_handles().lock() else {
                return;
            };
            let mut out = Vec::new();
            for id in &peers {
                match map.get(id).and_then(|e| e.upgrade()) {
                    Some(h) => out.push(h),
                    None => {
                        map.remove(id);
                    }
                }
            }
            out
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
                    Value::String(SharedString::new(self.id.to_string())),
                    Value::String(SharedString::new(reason.to_string())),
                ],
                response: tx,
                deadline: None,
            };
            // Best-effort peer notify — still account depth so worker on_dequeue balances.
            peer.bp.on_enqueue();
            if peer.mailbox.send(msg).is_err() {
                peer.bp.on_dequeue();
            }
        }
    }

    /// v0.29.11 Fault absorption: short-circuit the actor mailbox (O(1)).
    ///
    /// Sets `faulted` so every send site returns immediately without enqueueing.
    /// Clears actor fields so nested payload resources are dropped. The worker
    /// loop also checks `faulted` and drains remaining messages without dispatch.
    /// Idempotent. v0.29.20: also notifies linked peers (PeerFault injection).
    /// C3: use scoped locks — never hold inner lock across peer notification or
    /// registry access. Each lock is acquired and dropped within its own scope.
    pub(crate) fn short_circuit_mailbox(&self) {
        // Check faulted flag (inner read lock, scoped).
        let already = self.inner.read().map(|a| a.faulted).unwrap_or(true);
        if already {
            return;
        }
        // Notify peers BEFORE marking faulted so peer_links are still available.
        // (inner read lock acquired and released inside notify_peer_faults, not held here).
        self.notify_peer_faults("peer actor entered Fault");
        // Mark faulted + clear fields (inner write lock, scoped — no other lock held).
        if let Ok(mut actor) = self.inner.write() {
            actor.faulted = true;
            actor.fields.clear();
            actor.peer_links.clear();
        }
        // Remove from global registry (registry lock only, no inner lock held).
        if let Ok(mut map) = actor_handles().lock() {
            map.remove(&self.id);
        }
        self.system_kill_children();
    }

    /// True when this actor has entered Fault absorption (mailbox short-circuited).
    pub(crate) fn is_faulted(&self) -> bool {
        self.inner.read().map(|a| a.faulted).unwrap_or(true)
    }

    /// v0.29.21: enqueue a mailbox message with backpressure governance.
    ///
    /// - If faulted → immediate error.
    /// - If muted / over HWM → wait up to TTL; on timeout return
    ///   `SendErrorNotWriteable` (deadlock break).
    /// - On success, depth is incremented and the message is sent.
    pub(crate) fn try_enqueue(
        &self,
        method: String,
        args: Vec<Value>,
    ) -> Result<std::sync::mpsc::Receiver<Result<Value, InterpError>>, InterpError> {
        if self.is_faulted() {
            return Err(InterpError::new("actor mailbox short-circuited (Fault)"));
        }
        let start = std::time::Instant::now();
        let ttl = std::time::Duration::from_millis(self.bp.ttl_ms);
        let deadline = start + ttl;

        // Wait while muted / over depth, with TTL.
        loop {
            self.bp.try_unmute();
            let depth = self.bp.current_depth();
            let muted = self.bp.is_muted();
            let over = depth
                >= self
                    .bp
                    .depth_limit
                    .load(std::sync::atomic::Ordering::Acquire);
            if !muted && !over {
                break;
            }
            if std::time::Instant::now() >= deadline {
                return Err(InterpError::new(
                    "SendErrorNotWriteable: mailbox backpressure TTL expired",
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        let (tx, rx) = std::sync::mpsc::channel();
        let msg = ActorMailboxMsg {
            method,
            args,
            response: tx,
            deadline: Some(deadline),
        };
        // Reserve depth slot before send so concurrent producers see HWM.
        self.bp.on_enqueue();
        // v0.29.46: register current actor as a producer of this target.
        // When this target enters mute, producers will be push-muted.
        let caller_id = CURRENT_ACTOR_ID.with(|id| id.get());
        if caller_id != 0 && caller_id != self.id {
            // Recover from poison (worker panic) instead of crashing the sender.
            let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
            if !inner.producers.contains(&caller_id) {
                inner.producers.push(caller_id);
            }
        }
        // v0.29.46: if this enqueue pushed us into mute, cascade to producers.
        if self.bp.is_muted() {
            self.cascade_mute_to_producers();
        }
        if self.mailbox.send(msg).is_err() {
            self.bp.on_dequeue(); // roll back reservation
            return Err(InterpError::lock_error(
                "actor mailbox send failed".to_string(),
            ));
        }
        Ok(rx)
    }

    /// Current mailbox depth (approx).
    pub(crate) fn mailbox_depth(&self) -> usize {
        self.bp.current_depth()
    }

    /// True if this actor is currently muted under backpressure.
    pub(crate) fn is_muted(&self) -> bool {
        self.bp.is_muted()
    }

    /// v0.29.46: Push-mute all producers of this actor.
    /// Called after this actor enters mute. Iterates the producer list
    /// and mutes each producer's MailboxBpState (white-paper §6.7:
    /// "生产者 Actor 整体被挂起").
    pub(crate) fn cascade_mute_to_producers(&self) {
        let producers: Vec<usize> = {
            let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
            inner.producers.clone()
        };
        if producers.is_empty() {
            return;
        }
        let handles: Vec<ActorHandle> = {
            let registry = actor_handles();
            let Ok(map) = registry.lock() else {
                return;
            };
            producers
                .iter()
                .filter_map(|pid| map.get(pid).and_then(|entry| entry.upgrade()))
                .collect()
        };
        for handle in handles {
            handle.bp.enter_mute();
        }
    }

    /// Remove the complete owned subtree from the registry before faulting it.
    /// This snapshot-then-act protocol never holds the topology lock while
    /// acquiring actor locks and is safe for cyclic peer/producer graphs.
    fn system_kill_children(&self) {
        let handles: Vec<ActorHandle> = {
            let map = actor_handles().lock().unwrap_or_else(|e| e.into_inner());
            map.values().filter_map(WeakActorEntry::upgrade).collect()
        };
        let mut parent_ids = vec![self.id];
        let mut victims = Vec::new();
        let mut cursor = 0;
        while cursor < parent_ids.len() {
            let parent_id = parent_ids[cursor];
            for handle in &handles {
                if parent_ids.contains(&handle.id) {
                    continue;
                }
                let owned = handle.inner.read().map_or(false, |actor| {
                    actor.parent_id == Some(parent_id) && !actor.is_detached
                });
                if owned {
                    parent_ids.push(handle.id);
                    victims.push(handle.clone());
                }
            }
            cursor += 1;
        }
        {
            let mut map = actor_handles().lock().unwrap_or_else(|e| e.into_inner());
            for child in &victims {
                map.remove(&child.id);
            }
        };
        for child in victims {
            if let Ok(mut actor) = child.inner.write() {
                actor.faulted = true;
                actor.fields.clear();
                actor.peer_links.clear();
            }
        }
    }

    /// Configured high-water depth limit.
    pub(crate) fn mailbox_depth_limit(&self) -> usize {
        self.bp
            .depth_limit
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Reconfigure high-water depth (v0.29.21).
    pub(crate) fn set_mailbox_depth_limit(&self, depth: usize) {
        self.bp
            .depth_limit
            .store(depth.max(1), std::sync::atomic::Ordering::Release);
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
                // Sorted for dual with codegen set Display.
                write!(f, "Set{{")?;
                let mut ints: Vec<i64> = Vec::new();
                let mut bools: Vec<bool> = Vec::new();
                let mut floats: Vec<f64> = Vec::new();
                let mut other: Vec<&Value> = Vec::new();
                for v in vs {
                    match v {
                        Value::Int(n) => ints.push(*n),
                        Value::Bool(b) => bools.push(*b),
                        Value::Float(f) => floats.push(*f),
                        o => other.push(o),
                    }
                }
                ints.sort_unstable();
                bools.sort_unstable(); // false < true
                floats.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let mut first = true;
                for n in ints {
                    if !first {
                        write!(f, ", ")?;
                    }
                    first = false;
                    write!(f, "{}", n)?;
                }
                for b in bools {
                    if !first {
                        write!(f, ", ")?;
                    }
                    first = false;
                    write!(f, "{}", b)?;
                }
                for fv in floats {
                    if !first {
                        write!(f, ", ")?;
                    }
                    first = false;
                    if fv.fract() == 0.0 && fv.is_finite() {
                        write!(f, "{}", fv as i64)?;
                    } else {
                        write!(f, "{}", fv)?;
                    }
                }
                // Sort non-scalar elements by Display for dual-stable Set order.
                let mut other_s: Vec<String> = other.iter().map(|v| format!("{}", v)).collect();
                other_s.sort();
                for s in other_s {
                    if !first {
                        write!(f, ", ")?;
                    }
                    first = false;
                    write!(f, "{}", s)?;
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
            Value::CanonicalVariant { tag, payload, .. } => {
                write!(f, "{}(", tag)?;
                for (i, v) in payload.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, ")")
            }
            Value::CanonicalMapRoot(_) => write!(f, "<canonical MapRoot>"),
            Value::CanonicalMapAnyOwnerV1(_) => write!(f, "<canonical MapAnyOwner v1>"),
            Value::CanonicalOrdinaryMapOwnerLifecycleI32StringV1(_) => {
                write!(f, "<canonical ordinary Map owner lifecycle v1>")
            }
            Value::CanonicalOrdinaryMapOwnedGetI32StringV1(_) => {
                write!(f, "<canonical ordinary Map owned-Get v1>")
            }
            Value::CanonicalOrdinaryMapOwnedAnyV2(_) => {
                write!(f, "<canonical ordinary Map/Any owner-v2>")
            }
            Value::CanonicalOrdinaryMapOwnedAnyV2RuntimeRoot(root) => write!(f, "{root:?}"),
            Value::CanonicalManagedAnyI32StringV1(_) => {
                write!(f, "<canonical managed Any i32/String v1>")
            }
            Value::CanonicalManagedAnyV2(_) => {
                write!(f, "<canonical managed Any v2>")
            }
            Value::Record(type_name, fields) => {
                // Untyped records (JSON/Map) print as JSON objects for dual
                // with codegen Map handles (mimi_map_to_json_i64).
                if type_name.is_none() {
                    write!(f, "{{")?;
                    let mut keys: Vec<_> = fields.keys().collect();
                    keys.sort();
                    for (i, k) in keys.iter().enumerate() {
                        if i > 0 {
                            write!(f, ",")?;
                        }
                        write!(f, "\"{}\":", k)?;
                        match &fields[*k] {
                            Value::Int(n) => write!(f, "{}", n)?,
                            Value::String(s) => {
                                // Escape for dual with codegen Map JSON Display.
                                write!(f, "\"")?;
                                for ch in s.chars() {
                                    match ch {
                                        '"' => write!(f, "\\\"")?,
                                        '\\' => write!(f, "\\\\")?,
                                        '\n' => write!(f, "\\n")?,
                                        '\r' => write!(f, "\\r")?,
                                        '\t' => write!(f, "\\t")?,
                                        c => write!(f, "{}", c)?,
                                    }
                                }
                                write!(f, "\"")?
                            }
                            Value::Bool(b) => write!(f, "{}", b)?,
                            Value::Float(x) => write!(f, "{}", x)?,
                            // Product tuples in Map Display: match codegen `(1, 2)`.
                            Value::Tuple(items) => {
                                write!(f, "(")?;
                                for (j, item) in items.iter().enumerate() {
                                    if j > 0 {
                                        write!(f, ", ")?;
                                    }
                                    write!(f, "{}", item)?;
                                }
                                write!(f, ")")?;
                            }
                            other => write!(f, "{}", other)?,
                        }
                    }
                    return write!(f, "}}");
                }
                let name = type_name.as_deref().unwrap();
                if fields.is_empty() {
                    write!(f, "{} {{}}", name)
                } else {
                    write!(f, "{} {{ ", name)?;
                    // Sorted keys for dual-backend stable Display.
                    let mut keys: Vec<_> = fields.keys().collect();
                    keys.sort();
                    let mut first = true;
                    for k in keys {
                        let v = &fields[k];
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
            Value::CanonicalActorHandle(_) => write!(f, "<canonical Actor handle>"),
            Value::CanonicalActorState(_) => write!(f, "<canonical Actor state>"),
            Value::Closure { .. } => write!(f, "Closure(...)"),
            Value::BytecodeClosure { proto, .. } => write!(f, "BytecodeClosure(proto={})", proto),
            Value::Shared(arc) => {
                let v = arc.read().map_err(|_| std::fmt::Error)?;
                write!(f, "shared({})", v)
            }
            Value::LocalShared(rc) => {
                let v = rc.lock().unwrap_or_else(|e| e.into_inner());
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
                    let v = rc.lock().unwrap_or_else(|e| e.into_inner());
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
            Value::PlaceRef { owner, projections } => {
                write!(f, "&{}{}", owner, display_projections(projections))
            }
            Value::PlaceRefMut { owner, projections } => {
                write!(f, "&mut {}{}", owner, display_projections(projections))
            }
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
            Value::CBuffer(inner) => {
                write!(f, "CBuffer({:p}, {} bytes)", inner.as_ptr(), inner.len())
            }
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
        Value::CanonicalVariant { payload, .. } => payload.iter().any(contains_local_shared),
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
        Value::CanonicalVariant { payload, .. } => {
            payload.iter().any(|v| contains_arena_ref(v, arena_id))
        }
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
        Value::LocalShared(inner) => {
            contains_arena_ref(&inner.0.lock().unwrap_or_else(|e| e.into_inner()), arena_id)
        }
        Value::WeakLocal(inner) => {
            if let Some(rc) = inner.0.upgrade() {
                contains_arena_ref(&rc.lock().unwrap_or_else(|e| e.into_inner()), arena_id)
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
        Value::Int(_)
        | Value::Float(_)
        | Value::Bool(_)
        | Value::Unit
        | Value::CanonicalActorHandle(_) => true,
        Value::Tuple(elems) => elems.iter().all(is_copy),
        Value::Newtype(_, inner) => is_copy(inner),
        Value::Shared(_) | Value::LocalShared(_) => true,
        Value::Record(_, fields) => fields.values().all(is_copy),
        Value::Variant(_, args) => args.iter().all(is_copy),
        Value::CanonicalVariant { payload, .. } => payload.iter().all(is_copy),
        Value::Array(elems) => elems.iter().all(is_copy),
        Value::Set(elems) => elems.iter().all(is_copy),
        _ => false,
    }
}

#[inline(always)]
pub(crate) fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Int(0) => false,
        // audit (LOW): f64::NAN was previously truthy because NAN != 0.0
        // is true. Standard semantics: NAN should be falsy.
        Value::Float(x) => *x != 0.0 && !x.is_nan(),
        Value::String(s) => !s.is_empty(),
        Value::List(l) => !l.is_empty(),
        Value::Set(s) => !s.is_empty(),
        Value::Unit => false,
        Value::Newtype(_, inner) => is_truthy(inner),
        _ => true,
    }
}

pub(crate) fn values_equal(a: &Value, b: &Value) -> bool {
    values_equal_depth(a, b, 0)
}

const VALUES_EQUAL_MAX_DEPTH: u32 = 256;

fn values_equal_depth(a: &Value, b: &Value, depth: u32) -> bool {
    // IN-H3 (deep audit): prevent infinite recursion on cyclic Shared/Ref values.
    if depth > VALUES_EQUAL_MAX_DEPTH {
        return false; // assume not equal on cycle/overflow
    }
    match (a, b) {
        (Value::CanonicalActorHandle(a), Value::CanonicalActorHandle(b)) => a == b,
        (Value::CanonicalActorState(a), Value::CanonicalActorState(b)) => a == b,
        (Value::Int(a), Value::Int(b)) => a == b,
        // Cross numeric comparison: widen the integer side to float.
        // SD-10 (0.31.51b): exact comparison (matches codegen OEQ).
        // Epsilon comparison was an L1 divergence — use is_close() for approximate.
        (Value::Int(a), Value::Float(b)) | (Value::Float(b), Value::Int(a)) => (*a as f64) == *b,
        (Value::Float(a), Value::Float(b)) => a == b,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Unit, Value::Unit) => true,
        (Value::List(a), Value::List(b)) => {
            // B8: use values_equal_depth to propagate depth tracking.
            a.len() == b.len()
                && a.iter()
                    .zip(b.iter())
                    .all(|(x, y)| values_equal_depth(x, y, depth + 1))
        }
        (Value::Set(a), Value::Set(b)) => {
            // B8: use values_equal_depth to propagate depth tracking.
            a.len() == b.len()
                && a.iter()
                    .all(|x| b.iter().any(|y| values_equal_depth(x, y, depth + 1)))
        }
        (Value::Array(a), Value::Array(b)) => {
            // B8: use values_equal_depth to propagate depth tracking.
            a.len() == b.len()
                && a.iter()
                    .zip(b.iter())
                    .all(|(x, y)| values_equal_depth(x, y, depth + 1))
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
                    .all(|(x, y)| values_equal_depth(x, y, depth + 1))
        }
        (Value::Tuple(a), Value::Tuple(b)) => {
            a.len() == b.len()
                && a.iter()
                    .zip(b.iter())
                    .all(|(x, y)| values_equal_depth(x, y, depth + 1))
        }
        (Value::Variant(an, av), Value::Variant(bn, bv)) => {
            an == bn
                && av.len() == bv.len()
                && av
                    .iter()
                    .zip(bv.iter())
                    .all(|(x, y)| values_equal_depth(x, y, depth + 1))
        }
        (
            Value::CanonicalVariant {
                nominal: an,
                variant: ai,
                tag: at,
                payload: av,
            },
            Value::CanonicalVariant {
                nominal: bn,
                variant: bi,
                tag: bt,
                payload: bv,
            },
        ) => {
            an == bn
                && ai == bi
                && at == bt
                && av.len() == bv.len()
                && av
                    .iter()
                    .zip(bv.iter())
                    .all(|(x, y)| values_equal_depth(x, y, depth + 1))
        }
        (Value::CanonicalMapRoot(a), Value::CanonicalMapRoot(b)) => a == b,
        (Value::CanonicalMapAnyOwnerV1(a), Value::CanonicalMapAnyOwnerV1(b)) => a == b,
        (
            Value::CanonicalOrdinaryMapOwnerLifecycleI32StringV1(a),
            Value::CanonicalOrdinaryMapOwnerLifecycleI32StringV1(b),
        ) => a == b,
        (
            Value::CanonicalOrdinaryMapOwnedGetI32StringV1(a),
            Value::CanonicalOrdinaryMapOwnedGetI32StringV1(b),
        ) => a == b,
        (Value::CanonicalOrdinaryMapOwnedAnyV2(a), Value::CanonicalOrdinaryMapOwnedAnyV2(b)) => {
            a == b
        }
        (
            Value::CanonicalOrdinaryMapOwnedAnyV2RuntimeRoot(a),
            Value::CanonicalOrdinaryMapOwnedAnyV2RuntimeRoot(b),
        ) => a.ptr_eq(b),
        (Value::CanonicalManagedAnyI32StringV1(a), Value::CanonicalManagedAnyI32StringV1(b)) => {
            a == b
        }
        (Value::CanonicalManagedAnyV2(a), Value::CanonicalManagedAnyV2(b)) => a == b,
        (Value::Record(_, a), Value::Record(_, b)) => {
            a.len() == b.len()
                && a.iter().all(|(k, v)| {
                    b.get(k)
                        .map(|bv| values_equal_depth(v, bv, depth + 1))
                        .unwrap_or(false)
                })
        }
        (Value::Newtype(_, a), Value::Newtype(_, b)) => values_equal_depth(a, b, depth + 1),
        (Value::Ref(a), Value::Ref(b)) | (Value::RefMut(a), Value::RefMut(b)) => {
            if let (Ok(va), Ok(vb)) = (a.read(), b.read()) {
                values_equal_depth(&va, &vb, depth + 1)
            } else {
                false
            }
        }
        (Value::Ref(a), _) => {
            if let Ok(va) = a.read() {
                values_equal_depth(&va, b, depth + 1)
            } else {
                false
            }
        }
        (_, Value::Ref(b)) => {
            if let Ok(vb) = b.read() {
                values_equal_depth(a, &vb, depth + 1)
            } else {
                false
            }
        }
        (Value::Shared(a), Value::Shared(b)) => {
            if let (Ok(va), Ok(vb)) = (a.read(), b.read()) {
                values_equal_depth(&va, &vb, depth + 1)
            } else {
                false
            }
        }
        (Value::LocalShared(a), Value::LocalShared(b)) => values_equal_depth(
            &a.0.lock().unwrap_or_else(|e| e.into_inner()),
            &b.0.lock().unwrap_or_else(|e| e.into_inner()),
            depth + 1,
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
        ) => at == bt && values_equal_depth(ad, bd, depth + 1),
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
        Value::CanonicalVariant { .. } => "variant",
        Value::CanonicalMapRoot(_) => "map-root",
        Value::CanonicalMapAnyOwnerV1(_) => "map-any-owner-v1",
        Value::CanonicalOrdinaryMapOwnerLifecycleI32StringV1(_) => {
            "ordinary-map-owner-lifecycle-i32-string-v1"
        }
        Value::CanonicalOrdinaryMapOwnedGetI32StringV1(_) => "ordinary-map-owned-get-i32-string-v1",
        Value::CanonicalOrdinaryMapOwnedAnyV2(_) => "ordinary-map-owned-any-v2",
        Value::CanonicalOrdinaryMapOwnedAnyV2RuntimeRoot(_) => {
            "ordinary-map-owned-any-v2-runtime-root"
        }
        Value::CanonicalManagedAnyI32StringV1(_) => "managed-any-i32-string-v1",
        Value::CanonicalManagedAnyV2(_) => "managed-any-v2",
        Value::Record(Some(_), _) => "record",
        Value::Record(None, _) => "record",
        Value::Error(_) => "error",
        Value::Newtype(_, _) => "newtype",
        Value::Type(_) => "type",
        Value::Closure { .. } => "closure",
        Value::BytecodeClosure { .. } => "closure",
        Value::QuoteAst(_) => "AST",
        Value::Shared(_) => "shared",
        Value::LocalShared(_) => "local_shared",
        Value::Ref(_) => "ref",
        Value::RefMut(_) => "ref_mut",
        Value::IndexRef { .. } => "borrowed_index",
        Value::IndexRefMut { .. } => "borrowed_index_mut",
        Value::PlaceRef { .. } => "borrowed_place",
        Value::PlaceRefMut { .. } => "borrowed_place_mut",
        Value::Cap(_) => "cap",
        Value::Actor(_) => "actor",
        Value::CanonicalActorHandle(_) => "canonical-actor-handle",
        Value::CanonicalActorState(_) => "canonical-actor-state",
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

fn display_projections(projections: &[RuntimeProjection]) -> String {
    let mut value = String::new();
    for projection in projections {
        match projection {
            RuntimeProjection::Field(field) => {
                value.push('.');
                value.push_str(field);
            }
            RuntimeProjection::Tuple(index) => {
                value.push('.');
                value.push_str(&index.to_string());
            }
        }
    }
    value
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        values_equal(self, other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_managed_any_recursive_clone_has_independent_string_owners() {
        let value = Value::Tuple(vec![Value::Tuple(vec![
            Value::CanonicalManagedAnyI32StringV1(CanonicalManagedAnyI32StringV1::from_string(
                "owned snapshot".into(),
            )),
        ])]);
        let copy = value.clone();
        let payload = |value: &Value| {
            let Value::Tuple(outer) = value else {
                panic!("outer tuple")
            };
            let Value::Tuple(inner) = &outer[0] else {
                panic!("inner tuple")
            };
            let Value::CanonicalManagedAnyI32StringV1(any) = &inner[0] else {
                panic!("managed Any")
            };
            any.string_payload().expect("String payload").as_ptr()
        };
        assert_ne!(payload(&value), payload(&copy));
        drop(value);
        let Value::Tuple(outer) = copy else {
            panic!("copied outer tuple")
        };
        let Value::Tuple(inner) = &outer[0] else {
            panic!("copied inner tuple")
        };
        let Value::CanonicalManagedAnyI32StringV1(any) = &inner[0] else {
            panic!("copied Any")
        };
        assert_eq!(any.descriptor_version(), 1);
        assert_eq!(any.kind(), 2);
        assert_eq!(any.string_payload(), Some("owned snapshot"));
        assert_eq!(any.read_i32(), None);
    }

    #[test]
    fn canonical_managed_any_i32_zero_is_owned_and_descriptor_checked() {
        let zero = CanonicalManagedAnyI32StringV1::from_i32(0);
        assert_eq!(zero.descriptor_version(), 1);
        assert_eq!(zero.kind(), 1);
        assert_eq!(zero.read_i32(), Some(0));
        assert_eq!(zero.string_payload(), None);
    }

    /// H9 (0.35.37): compile-time enforcement that CBufferInner's unsafe
    /// Send + Sync impls remain in place. If a future refactor removes the
    /// impls (or adds a field that breaks the manual impl), this function
    /// fails to compile — the structural contract is pinned.
    #[test]
    fn c_buffer_inner_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CBufferInner>();
    }

    /// H9: the read-only accessors must not expose mutation. A `&CBufferInner`
    /// held across threads can only read ptr/size; buffer writes need `&mut`
    /// or ownership. This test pins the accessor surface: no field access.
    #[test]
    fn c_buffer_inner_read_only_accessors() {
        // SAFETY: fresh null buffer, never dereferenced.
        // SAFETY: null pointer is a valid libc free no-op and size is zero.
        let buf = unsafe { CBufferInner::new(std::ptr::null_mut(), 0) };
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
        assert!(buf.as_ptr().is_null());
    }
}
