use crate::ast::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak as RcWeak};
use std::sync::{Arc, RwLock, Weak as ArcWeak};

/// Wrapper around Rc that implements Send/Sync.
/// Safe in our interpreter because LocalShared values are never shared across OS threads;
/// parasteps creates fresh Interpreter clones per thread.
#[derive(Debug, Clone)]
pub(crate) struct SendRc<T>(pub(crate) Rc<T>);
unsafe impl<T: Clone> Send for SendRc<T> {}
unsafe impl<T: Clone> Sync for SendRc<T> {}
impl<T> std::ops::Deref for SendRc<T> {
    type Target = Rc<T>;
    fn deref(&self) -> &Self::Target { &self.0 }
}

/// Wrapper around RcWeak that implements Send/Sync.
#[derive(Debug, Clone)]
pub(crate) struct SendWeak<T>(pub(crate) RcWeak<T>);
unsafe impl<T: Clone> Send for SendWeak<T> {}
unsafe impl<T: Clone> Sync for SendWeak<T> {}
impl<T> SendWeak<T> {
    pub(crate) fn upgrade(&self) -> Option<SendRc<T>> {
        self.0.upgrade().map(SendRc)
    }
}

/// A quoted AST value - represents syntax tree at runtime for compile-time metaprogramming
#[derive(Debug, Clone)]
pub enum QuotedAst {
    /// A literal value node
    Literal(Lit),
    /// An identifier
    Ident(String),
    /// A binary operation
    Binary(BinOp, Box<QuotedAst>, Box<QuotedAst>),
    /// A unary operation
    Unary(UnOp, Box<QuotedAst>),
    /// A function call
    Call(Box<QuotedAst>, Vec<QuotedAst>),
    /// Field access
    Field(Box<QuotedAst>, String),
    /// Index access
    Index(Box<QuotedAst>, Box<QuotedAst>),
    /// A tuple
    Tuple(Vec<QuotedAst>),
    /// A list
    List(Vec<QuotedAst>),
    /// A match expression
    Match(Box<QuotedAst>, Vec<MatchArmQuoted>),
    /// A record expression
    Record {
        ty: Option<String>,
        fields: Vec<RecordFieldExprQuoted>,
    },
    /// A try expression
    Try(Box<QuotedAst>),
    /// A spawn expression
    Spawn(Box<QuotedAst>),
    /// An await expression
    Await(Box<QuotedAst>),
    /// An interpolation splice point - contains the runtime value to splice
    Interpolate(Box<Value>),
    /// A block of quoted statements
    Block(Vec<QuotedAst>),
    /// A let statement in quote
    Let {
        name: String,
        value: Box<QuotedAst>,
    },
    /// An expression statement in quote
    ExprStmt(Box<QuotedAst>),
    /// A return statement in quote
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
    Record(HashMap<String, Value>),
    /// A future representing a spawned concurrent task
    Future(std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<Result<Value, String>>>>),
    /// An error value propagated via ? operator - triggers on failure compensation
    Error(String),
    /// Arena reference: points to a slot in an arena
    ArenaRef(usize, usize),
    /// Arena memory block containing slot-indexed values
    ArenaBlock(usize),
    /// A quoted AST - compile-time generated syntax tree
    QuoteAst(Box<QuotedAst>),
    /// A newtype-wrapped value for strong type isolation
    Newtype(Box<Value>),
    /// An actor instance - contains state and methods
    Actor(ActorHandle),
    /// A closure - captures environment and has parameters + body
    Closure {
        params: Vec<Param>,
        ret: Option<Type>,
        body: Block,
        /// Captured variables from the enclosing scope
        captured: HashMap<String, Value>,
    },
    /// Thread-safe shared ownership (Arc<RwLock<Value>>)
    Shared(Arc<RwLock<Value>>),
    /// Single-thread shared ownership (Rc<RefCell<Value>>)
    LocalShared(SendRc<RefCell<Value>>),
    /// Weak reference to a Shared value
    WeakShared(ArcWeak<RwLock<Value>>),
    /// Weak reference to a LocalShared value
    WeakLocal(SendWeak<RefCell<Value>>),
    /// A linear capability (simple or combined)
    Cap(Vec<String>),
}

/// Arena memory manager for region-based allocation
#[derive(Debug, Clone)]
pub struct Arena {
    pub id: usize,
    pub slots: Vec<Value>,
}

/// Actor instance - holds state and methods for an actor
#[derive(Debug, Clone)]
pub struct ActorInstance {
    pub actor_name: String,
    pub fields: HashMap<String, Value>,
    pub methods: Vec<FuncDef>,
}

/// Wrapper for actor that uses RwLock for interior mutability (thread-safe)
/// This allows actor state to be accessed from multiple threads
#[derive(Debug, Clone)]
pub struct ActorHandle {
    pub inner: std::sync::Arc<std::sync::RwLock<ActorInstance>>,
    pub id: usize,
}

static ACTOR_HANDLE_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl ActorHandle {
    fn new(instance: ActorInstance) -> Self {
        let id = ACTOR_HANDLE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        ActorHandle {
            inner: std::sync::Arc::new(std::sync::RwLock::new(instance)),
            id,
        }
    }
}

impl Value {
    /// Check if this value is an arena reference
    pub fn is_arena_ref(&self) -> bool {
        matches!(self, Value::ArenaRef(_, _))
    }

    /// Check if this value is an arena block
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
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, "]")
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
            Value::Record(fields) => {
                write!(f, "{{")?;
                let mut first = true;
                for (k, v) in fields.iter() {
                    if !first {
                        write!(f, ", ")?;
                    }
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
            Value::Cap(names) => {
                write!(f, "cap({})", names.join(" + "))
            }
        }
    }
}

pub struct Interpreter<'a> {
    file: &'a File,
    env: Vec<HashMap<String, Value>>,
    /// Track which variables have been moved (for move semantics)
    moved_vars: Vec<HashMap<String, bool>>,
    /// Track which variables are mutable
    mut_vars: Vec<HashMap<String, bool>>,
    constructors: HashMap<String, usize>,
    /// Set of constructor names that are newtypes (for wrapping result in Value::Newtype)
    newtype_constructors: HashMap<String, bool>,
    /// Maps type name to its variants (for Result/Option-like types)
    type_variants: HashMap<String, Vec<String>>,
    /// Variants that represent "failure" (Err, None, *Error, *Fail)
    failure_variants: HashMap<String, bool>,
    /// Capability definitions: cap_name -> list of component caps
    cap_defs: HashMap<String, Vec<String>>,
    /// Compensation stack for on failure blocks (LIFO) - scope-aware
    /// Each scope level contains compensation blocks registered in that scope
    /// Push a new scope when entering a block, pop when exiting
    compensation_stack: Vec<Vec<Vec<Stmt>>>,
    /// Arena memory blocks (arena_id -> Arena)
    arenas: Vec<Arena>,
    /// Current arena scope depth (track nesting for error messages)
    arena_depth: usize,
}

impl<'a> Interpreter<'a> {
    pub fn new(file: &'a File) -> Self {
        let mut constructors = HashMap::new();
        let mut newtype_constructors = HashMap::new();
        let mut type_variants: HashMap<String, Vec<String>> = HashMap::new();
        let mut failure_variants: HashMap<String, bool> = HashMap::new();
        let mut cap_defs: HashMap<String, Vec<String>> = HashMap::new();
        for item in &file.items {
            Self::collect_constructors(item, &mut constructors, &mut newtype_constructors, &mut type_variants, &mut failure_variants);
            Self::collect_caps(item, &mut cap_defs);
        }
        Self {
            file,
            env: vec![HashMap::new()],
            moved_vars: vec![HashMap::new()],
            mut_vars: vec![HashMap::new()],
            constructors,
            newtype_constructors,
            type_variants,
            failure_variants,
            cap_defs,
            compensation_stack: Vec::new(),
            arenas: Vec::new(),
            arena_depth: 0,
        }
    }

    fn collect_constructors(item: &Item, out: &mut HashMap<String, usize>, newtype_constructors: &mut HashMap<String, bool>, type_variants: &mut HashMap<String, Vec<String>>, failure_variants: &mut HashMap<String, bool>) {
        match item {
            Item::Type(t) => {
                match &t.kind {
                    TypeDefKind::Enum(variants) => {
                        let mut variant_names = Vec::new();
                        for v in variants {
                            let arity = match &v.payload {
                                None => 0,
                                Some(VariantPayload::Tuple(types)) => types.len(),
                                Some(VariantPayload::Record(fields)) => fields.len(),
                            };
                            out.insert(v.name.clone(), arity);
                            variant_names.push(v.name.clone());
                            // Mark failure-like variants
                            let name_lower = v.name.to_lowercase();
                            if name_lower == "err" || name_lower == "none" || name_lower.ends_with("error") || name_lower.ends_with("fail") {
                                failure_variants.insert(v.name.clone(), true);
                            }
                        }
                        type_variants.insert(t.name.clone(), variant_names);
                    }
                    TypeDefKind::Newtype(_) => {
                        out.insert(t.name.clone(), 1);
                        newtype_constructors.insert(t.name.clone(), true);
                    }
                    _ => {}
                }
            }
            Item::Module(m) => {
                for inner in &m.items {
                    Self::collect_constructors(inner, out, newtype_constructors, type_variants, failure_variants);
                }
            }
            Item::Trait(_) | Item::Impl(_) => {
                // Traits and impls don't define constructors
            }
            _ => {}
        }
    }

    fn collect_caps(item: &Item, out: &mut HashMap<String, Vec<String>>) {
        match item {
            Item::Cap(cap) => {
                let components = if let Some(ref combined) = cap.combined_with {
                    // Parse "A + B" format
                    let parts: Vec<String> = combined.split(" + ")
                        .map(|s| s.trim().to_string())
                        .collect();
                    if parts.len() > 1 {
                        parts
                    } else {
                        vec![cap.name.clone(), combined.clone()]
                    }
                } else {
                    vec![cap.name.clone()]
                };
                out.insert(cap.name.clone(), components);
            }
            Item::Module(m) => {
                for inner in &m.items {
                    Self::collect_caps(inner, out);
                }
            }
            _ => {}
        }
    }

    pub fn run(&mut self) -> Result<Value, String> {
        let main = self.find_function("main").ok_or("no main() function found")?;
        self.call_func(&main, vec![])
    }

    fn find_function(&self, name: &str) -> Option<FuncDef> {
        for item in &self.file.items {
            match item {
                Item::Func(f) if f.name == name => return Some(f.clone()),
                Item::Module(m) => {
                    for inner in &m.items {
                        if let Item::Func(f) = inner {
                            if f.name == name {
                                return Some(f.clone());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn find_actor(&self, name: &str) -> Option<ActorDef> {
        for item in &self.file.items {
            match item {
                Item::Actor(a) if a.name == name => return Some(a.clone()),
                Item::Module(m) => {
                    for inner in &m.items {
                        if let Item::Actor(a) = inner {
                            if a.name == name {
                                return Some(a.clone());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn push_scope(&mut self) {
        self.env.push(HashMap::new());
        self.moved_vars.push(HashMap::new());
        self.mut_vars.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.env.pop();
        self.moved_vars.pop();
        self.mut_vars.pop();
    }

    fn bind(&mut self, name: &str, value: Value) {
        self.env.last_mut().unwrap().insert(name.into(), value);
        self.moved_vars.last_mut().unwrap().insert(name.into(), false);
        // Default to immutable unless explicitly marked as mutable
        self.mut_vars.last_mut().unwrap().entry(name.into()).or_insert(false);
    }

    fn bind_mut(&mut self, name: &str, value: Value) {
        self.env.last_mut().unwrap().insert(name.into(), value);
        self.moved_vars.last_mut().unwrap().insert(name.into(), false);
        self.mut_vars.last_mut().unwrap().insert(name.into(), true);
    }

    fn lookup(&self, name: &str) -> Option<Value> {
        for (scope, moved) in self.env.iter().zip(self.moved_vars.iter()).rev() {
            if let Some(v) = scope.get(name) {
                if moved.get(name).copied().unwrap_or(false) {
                    return None; // Treat moved vars as undefined
                }
                return Some(v.clone());
            }
        }
        None
    }

    fn is_moved(&self, name: &str) -> bool {
        for moved in self.moved_vars.iter().rev() {
            if let Some(&m) = moved.get(name) {
                return m;
            }
        }
        false
    }

    fn mark_moved(&mut self, name: &str) {
        for moved in self.moved_vars.iter_mut().rev() {
            if moved.contains_key(name) {
                moved.insert(name.into(), true);
                return;
            }
        }
    }

    fn assign(&mut self, name: &str, value: Value) -> Result<(), String> {
        for (scope, moved) in self.env.iter_mut().zip(self.moved_vars.iter_mut()).rev() {
            if scope.contains_key(name) {
                // Check if variable is mutable
                for mut_scope in self.mut_vars.iter().rev() {
                    if let Some(&is_mut) = mut_scope.get(name) {
                        if !is_mut {
                            return Err(format!("cannot assign to immutable variable '{}'", name));
                        }
                        break;
                    }
                }
                scope.insert(name.into(), value);
                moved.insert(name.into(), false);
                return Ok(());
            }
        }
        Err(format!("undefined variable '{}' in assignment", name))
    }

    /// Push a new compensation scope level
    fn push_compensation_scope(&mut self) {
        self.compensation_stack.push(Vec::new());
    }

    /// Pop the current compensation scope level
    /// If run_compensations is true, execute all compensations in LIFO order before popping
    fn pop_compensation_scope(&mut self, run_compensations: bool) {
        if run_compensations {
            // Run compensation blocks in LIFO order for the current scope
            if let Some(scope) = self.compensation_stack.pop() {
                // Execute compensations in reverse order (LIFO within this scope)
                // Note: compensation_stack order is already LIFO across scopes,
                // but within a scope we want to execute in registration order (first registered = last executed)
                for block in scope.iter().rev() {
                    for stmt in block {
                        if let Err(e) = self.eval_stmt(stmt) {
                            eprintln!("compensation error: {} (ignored)", e);
                        }
                    }
                }
            }
        } else {
            // Just discard the scope (normal exit)
            self.compensation_stack.pop();
        }
    }

    /// Run all compensation blocks across all scope levels in LIFO order
    /// Used when propagation an error up through nested scopes
    fn run_all_compensations(&mut self) {
        // Run all remaining compensations in LIFO order
        while let Some(scope) = self.compensation_stack.pop() {
            for block in scope.iter().rev() {
                for stmt in block {
                    if let Err(e) = self.eval_stmt(stmt) {
                        eprintln!("compensation error: {} (ignored)", e);
                    }
                }
            }
        }
    }

    fn call_func(&mut self, func: &FuncDef, args: Vec<Value>) -> Result<Value, String> {
        if func.params.len() != args.len() {
            return Err(format!(
                "function {} expects {} arguments, got {}",
                func.name,
                func.params.len(),
                args.len()
            ));
        }
        self.push_scope();
        
        // Snapshot parameters for old() in ensures
        let mut old_snapshots: HashMap<String, Value> = HashMap::new();
        for (p, a) in func.params.iter().zip(args) {
            old_snapshots.insert(p.name.clone(), a.clone());
            self.bind(&p.name, a);
        }

        // Extract and check requires conditions
        for stmt in &func.body {
            if let Stmt::Requires(expr) = stmt {
                let cond = self.eval_expr(expr)?;
                if !is_truthy(&cond) {
                    self.pop_scope();
                    return Err(format!("requires condition failed for '{}': {}", func.name, cond));
                }
            }
        }

        let result = self.eval_block(&func.body);

        // Extract and check ensures conditions
        if let Ok(Some(ref rv)) = result {
            self.push_scope();
            self.bind("result", rv.clone());
            // Bind old snapshots for old(x) access
            for (name, val) in &old_snapshots {
                self.bind(&format!("old_{}", name), val.clone());
            }
            let ensures_ok = (|| {
                for stmt in &func.body {
                    if let Stmt::Ensures(expr) = stmt {
                        let cond = self.eval_expr(expr)?;
                        if !is_truthy(&cond) {
                            return Err(format!("ensures condition failed for '{}': {}", func.name, cond));
                        }
                    }
                }
                Ok(())
            })();
            self.pop_scope(); // always pop ensures scope
            if let Err(e) = ensures_ok {
                self.pop_scope(); // pop function scope
                return Err(e);
            }
        }

        self.pop_scope();
        result.map(|v| v.unwrap_or(Value::Unit))
    }

    fn eval_block(&mut self, block: &Block) -> Result<Option<Value>, String> {
        self.push_compensation_scope();
        let result = self.eval_block_inner(block);
        // Pop compensation scope: if error, run compensations; if ok, discard
        self.pop_compensation_scope(result.is_err());
        result
    }

    fn eval_block_inner(&mut self, block: &Block) -> Result<Option<Value>, String> {
        for (i, stmt) in block.iter().enumerate() {
            let is_last = i == block.len() - 1;
            match stmt {
                Stmt::Expr(e) if is_last => {
                    let result = self.eval_expr(e);
                    match result {
                        Ok(Value::Error(msg)) => {
                            return Err(msg);
                        }
                        Ok(v) => return Ok(Some(v)),
                        Err(e) => {
                            return Err(e);
                        }
                    }
                }
                Stmt::Expr(e) => {
                    let result = self.eval_expr(e);
                    match result {
                        Ok(Value::Error(msg)) => {
                            return Err(msg);
                        }
                        Ok(_) => {}
                        Err(e) => {
                            return Err(e);
                        }
                    }
                }
                _ => {
                    if let Some(v) = self.eval_stmt(stmt)? {
                        return Ok(Some(v));
                    }
                }
            }
        }
        Ok(None)
    }

    fn eval_stmt(&mut self, stmt: &Stmt) -> Result<Option<Value>, String> {
        match stmt {
            Stmt::Let { pat, init, mut_, ref_, ty: _ } => {
                let v = match init {
                    Some(e) => {
                        let result = self.eval_expr(e);
                        match result {
                            Ok(Value::Error(msg)) => {
                                return Err(msg);
                            }
                            Ok(v) => v,
                            Err(e) => {
                                return Err(e);
                            }
                        }
                    }
                    None => Value::Unit,
                };

                // Move semantics: if init is a simple identifier and value is non-Copy, mark source as moved
                if let Some(Expr::Ident(name)) = init {
                    if !is_copy(&v) && !self.is_moved(name) {
                        self.mark_moved(name);
                    }
                }

                // Handle `let ref` in arena: create ArenaRef instead of storing value directly
                let final_value = if *ref_ && self.arena_depth > 0 {
                    // Allocate in current arena
                    let arena_id = self.arenas.len() - 1;
                    let slot_index = self.arenas[arena_id].slots.len();
                    self.arenas[arena_id].slots.push(v.clone());
                    Value::ArenaRef(arena_id, slot_index)
                } else {
                    v.clone()
                };

                if let Some(bindings) = self.match_pattern(pat, &final_value) {
                    for (name, val) in bindings {
                        if *mut_ {
                            self.bind_mut(&name, val);
                        } else {
                            self.bind(&name, val);
                        }
                    }
                } else {
                    return Err(format!("let pattern did not match value {}", v));
                }
            }
            Stmt::Return(e) => {
                let v = match e {
                    Some(e) => self.eval_expr(e)?,
                    None => Value::Unit,
                };
                // Check if returning an ArenaRef from an active arena
                if self.arena_depth > 0 {
                    for arena in &self.arenas {
                        if contains_arena_ref(&v, arena.id) {
                            return Err(format!(
                                "arena escape: returning a reference to arena {} that is still active",
                                arena.id
                            ));
                        }
                    }
                }
                return Ok(Some(v));
            }
            Stmt::Expr(e) => {
                if let Value::Error(msg) = self.eval_expr(e)? {
                    return Err(msg);
                }
            }
            Stmt::If { cond, then_, else_ } => {
                let c = self.eval_expr(cond)?;
                if is_truthy(&c) {
                    if let Some(v) = self.eval_block(then_)? {
                        return Ok(Some(v));
                    }
                } else if let Some(else_block) = else_ {
                    if let Some(v) = self.eval_block(else_block)? {
                        return Ok(Some(v));
                    }
                }
            }
            Stmt::While { cond, body } => {
                while is_truthy(&self.eval_expr(cond)?) {
                    if let Some(v) = self.eval_block(body)? {
                        return Ok(Some(v));
                    }
                }
            }
            Stmt::For { var, iterable, body } => {
                let iter = self.eval_expr(iterable)?;
                let list = match iter {
                    Value::List(l) => l,
                    other => return Err(format!("cannot iterate over {}", other)),
                };
                for item in list {
                    self.bind(var, item);
                    if let Some(v) = self.eval_block(body)? {
                        return Ok(Some(v));
                    }
                }
            }
            Stmt::Block(block) => {
                if let Some(v) = self.eval_block(block)? {
                    return Ok(Some(v));
                }
            }
            Stmt::Arena(block) => {
                // Arena block: creates a region-based memory scope
                // All `ref T` allocations inside have lifetime equal to this block
                let arena_id = self.arenas.len();
                let arena = Arena {
                    id: arena_id,
                    slots: Vec::new(),
                };
                self.arenas.push(arena);
                self.arena_depth += 1;

                // Push a new scope for arena variables
                self.push_scope();

                // Evaluate the block
                let result = self.eval_block(block);

                // Before exiting, check for escape: scan OUTER scope variables
                // (skip the arena's own scope, which is the last one)
                // for any ArenaRefs that reference this arena
                let mut escape_var = None;
                let outer_count = self.env.len() - 1;
                for scope in self.env.iter().take(outer_count) {
                    for (name, val) in scope {
                        if contains_arena_ref(val, arena_id) {
                            escape_var = Some(name.clone());
                            break;
                        }
                    }
                    if escape_var.is_some() {
                        break;
                    }
                }
                if let Some(name) = escape_var {
                    self.arena_depth -= 1;
                    self.pop_scope();
                    self.arenas.pop();
                    return Err(format!(
                        "arena escape: variable '{}' holds a reference to arena {} that is about to be freed",
                        name, arena_id
                    ));
                }

                // Check if the result itself is an escaping ArenaRef
                if let Ok(Some(ref v)) = result {
                    if contains_arena_ref(v, arena_id) {
                        self.arena_depth -= 1;
                        self.pop_scope();
                        self.arenas.pop();
                        return Err(format!(
                            "arena escape: returning a reference to arena {} that is about to be freed",
                            arena_id
                        ));
                    }
                }

                self.arena_depth -= 1;
                self.pop_scope();

                // Arena is automatically reclaimed when block exits
                // (the Arena struct is dropped here)
                self.arenas.pop();

                return result;
            }
            Stmt::Assign { target, value } => {
                let v = self.eval_expr(value)?;
                // Move semantics: if value is a simple identifier and non-Copy, mark source as moved
                if let Expr::Ident(name) = value {
                    if !is_copy(&v) && !self.is_moved(name) {
                        self.mark_moved(name);
                    }
                }
                match target {
                    Expr::Ident(name) => self.assign(name, v)?,
                    Expr::Field(obj, field) => {
                        // Special case: if assigning to self.field, update actor directly
                        if let Expr::Ident(name) = obj.as_ref() {
                            if name == "self" {
                                // Find the actor handle in scope and update its field
                                if let Some(Value::Actor(handle)) = self.lookup("self") {
                                    handle.inner.write().map_err(|e| format!("actor lock failed: {}", e))?.fields.insert(field.clone(), v);
                                    return Ok(None);
                                }
                            }
                        }
                        let obj_val = self.eval_expr(obj)?;
                        match obj_val {
                            Value::Record(mut fields) => {
                                if fields.contains_key(field.as_str()) {
                                    if let std::collections::hash_map::Entry::Occupied(mut e) = fields.entry(field.clone()) {
                                        e.insert(v);
                                    }
                                } else {
                                    return Err(format!("field '{}' not found in record", field));
                                }
                            }
                            Value::Actor(handle) => {
                                handle.inner.write().map_err(|e| format!("actor lock failed: {}", e))?.fields.insert(field.clone(), v);
                            }
                            _ => return Err("cannot assign to non-record/non-actor value".into()),
                        }
                    }
                    _ => return Err("assignment target must be a variable".into()),
                }
            }
            Stmt::Desc(_) | Stmt::Requires(_) | Stmt::Ensures(_) | Stmt::Ellipsis | Stmt::MmsBlock(_) => {}
            Stmt::Math(exprs) => {
                // Math block: evaluate constant expressions at compile time
                for expr in exprs {
                    if let Ok(val) = self.eval_expr(expr) {
                        // Store the result if it's a constant
                        // For now, just evaluate and discard (verification conditions)
                        let _ = val;
                    }
                }
            }
            Stmt::Drop(expr) => {
                // Evaluate and discard the value (for linear capability drops)
                self.eval_expr(expr)?;
                // In a real implementation, this would track capability usage
            }
            Stmt::SharedLet { kind, name, init, .. } => {
                let v = self.eval_expr(init)?;
                let shared_val = match kind {
                    SharedKind::Shared => Value::Shared(Arc::new(RwLock::new(v))),
                    SharedKind::LocalShared => Value::LocalShared(SendRc(Rc::new(RefCell::new(v)))),
                    SharedKind::Weak => {
                        // Auto-detect: if init is Shared → WeakShared, if LocalShared → WeakLocal
                        match v {
                            Value::Shared(arc) => Value::WeakShared(Arc::downgrade(&arc)),
                            Value::LocalShared(rc) => Value::WeakLocal(SendWeak(Rc::downgrade(&rc.0))),
                            _ => return Err(format!("weak requires a shared or local_shared value, got {}", v)),
                        }
                    }
                    SharedKind::WeakLocal => {
                        match v {
                            Value::LocalShared(rc) => Value::WeakLocal(SendWeak(Rc::downgrade(&rc.0))),
                            _ => return Err(format!("weak_local requires a local_shared value, got {}", v)),
                        }
                    }
                };
                self.bind(name, shared_val);
            }
            Stmt::OnFailure(block) => {
                // Register compensation action to the current scope level
                // Will be executed in LIFO order if error propagates
                if let Some(current_scope) = self.compensation_stack.last_mut() {
                    current_scope.push(block.clone());
                }
            }
            Stmt::Parasteps(block) => {
                // Parasteps block: execute spawn statements in parallel
                // Collect spawn expressions and their results
                let mut last_value = None;
                type SpawnReceiver = std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<Result<Value, String>>>>;
                let mut futures: Vec<SpawnReceiver> = Vec::new();
                let mut spawn_bindings: HashMap<String, SpawnReceiver> = HashMap::new();

                for stmt in block {
                    match stmt {
                        Stmt::Expr(Expr::Spawn(expr)) => {
                            // Create a future for concurrent execution
                            let (tx, rx) = std::sync::mpsc::channel();
                            let expr = expr.clone();
                            let file = self.file.clone();
                            std::thread::spawn(move || {
                                let mut interp = Interpreter::new(&file);
                                let result = interp.eval_expr(&expr);
                                let _ = tx.send(result);
                            });
                            futures.push(std::sync::Arc::new(std::sync::Mutex::new(rx)));
                        }
                        Stmt::Let { pat, init, .. } => {
                            // Handle let bindings that might contain spawn
                            let v = match init {
                                Some(Expr::Spawn(expr)) => {
                                    // Create a future for concurrent execution
                                    let (tx, rx) = std::sync::mpsc::channel();
                                    let expr = expr.clone();
                                    let file = self.file.clone();
                                    std::thread::spawn(move || {
                                        let mut interp = Interpreter::new(&file);
                                        let result = interp.eval_expr(&expr);
                                        let _ = tx.send(result);
                                    });
                                    let rx_arc = std::sync::Arc::new(std::sync::Mutex::new(rx));
                                    // Store the future for later await
                                    if let Pattern::Variable(name) = pat {
                                        spawn_bindings.insert(name.clone(), rx_arc.clone());
                                    }
                                    Value::Future(rx_arc)
                                }
                                Some(e) => self.eval_expr(e)?,
                                None => Value::Unit,
                            };
                            if let Some(bindings) = self.match_pattern(pat, &v) {
                                for (name, val) in bindings {
                                    self.bind(&name, val);
                                }
                            }
                        }
                        Stmt::Expr(expr) => {
                            // Evaluate non-spawn expressions sequentially
                            last_value = Some(self.eval_expr(expr)?);
                        }
                        _ => {
                            if let Some(v) = self.eval_stmt(stmt)? {
                                last_value = Some(v);
                            }
                        }
                    }
                }

                // Wait for all futures and check for errors
                for rx in futures {
                    let rx = rx.lock().map_err(|e| format!("await failed: {}", e))?;
                    if let Ok(Err(e)) = rx.recv() {
                        return Err(e);
                    }
                }

                // If last_value is a Future, await it
                if let Some(Value::Future(rx)) = last_value {
                    let rx = rx.lock().map_err(|e| format!("await failed: {}", e))?;
                    last_value = Some(rx.recv().map_err(|e| format!("await failed: {}", e))??);
                }

                return Ok(last_value);
            }
        }
        Ok(None)
    }

    fn eval_expr(&mut self, expr: &Expr) -> Result<Value, String> {
        match expr {
            Expr::Literal(l) => Ok(match l {
                Lit::Int(v) => Value::Int(*v),
                Lit::Float(v) => Value::Float(*v),
                Lit::Bool(v) => Value::Bool(*v),
                Lit::String(v) => Value::String(v.clone()),
                Lit::FString(parts) => {
                    let mut result = String::new();
                    for part in parts {
                        match part {
                            crate::ast::FStringPart::Text(t) => result.push_str(t),
                            crate::ast::FStringPart::Interp(expr) => {
                                let val = self.eval_expr(expr)?;
                                result.push_str(&val.to_string());
                            }
                        }
                    }
                    Value::String(result)
                }
                Lit::Unit => Value::Unit,
            }),
            Expr::Ident(name) => {
                if let Some(v) = self.lookup(name) {
                    Ok(v)
                } else if self.is_moved(name) {
                    Err(format!("use of moved value '{}'", name))
                } else if let Some(components) = self.cap_defs.get(name.as_str()) {
                    // Cap definition: return as Value::Cap
                    Ok(Value::Cap(components.clone()))
                } else if let Some(func) = self.find_function(name) {
                    // First-class function: wrap as a closure with empty capture
                    Ok(Value::Closure {
                        params: func.params,
                        ret: func.ret,
                        body: func.body,
                        captured: HashMap::new(),
                    })
                } else if let Some(&arity) = self.constructors.get(name.as_str()) {
                    if arity == 0 {
                        if self.newtype_constructors.get(name.as_str()).copied().unwrap_or(false) {
                            return Err(format!("newtype '{}' requires exactly one argument", name));
                        }
                        Ok(Value::Variant(name.clone(), vec![]))
                    } else {
                        Err(format!("constructor '{}' requires {} arguments", name, arity))
                    }
                } else {
                    Err(format!("undefined variable '{}'", name))
                }
            }
            Expr::Unary(op, e) => self.eval_unary(*op, e),
            Expr::Binary(op, l, r) => self.eval_binary(*op, l, r),
            Expr::Call(callee, args) => {
                let vals: Result<Vec<_>, _> =
                    args.iter().map(|a| self.eval_expr(a)).collect();
                let vals = vals?;
                match callee.as_ref() {
                    Expr::Ident(name) => self.call_named(name, vals),
                    Expr::Field(obj, method) => {
                        // Handle Type.spawn() - actor constructor
                        if method == "spawn" {
                            if let Expr::Ident(type_name) = obj.as_ref() {
                                // Check if this is an actor type
                                if self.find_actor(type_name).is_some() {
                                    return self.spawn_actor(type_name, vals);
                                }
                            }
                        }
                        // Regular method call: evaluate the object and call method on it
                        let obj_val = self.eval_expr(obj)?;
                        self.call_method(&obj_val, method, vals)
                    }
                    _ => {
                        // Evaluate callee - could be a closure or other expression
                        let callee_val = self.eval_expr(callee)?;
                        match callee_val {
                            Value::Closure { params, ret: _, body, captured } => {
                                if params.len() != vals.len() {
                                    return Err(format!(
                                        "closure expects {} arguments, got {}",
                                        params.len(),
                                        vals.len()
                                    ));
                                }
                                self.push_scope();
                                // Restore captured environment
                                for (name, val) in &captured {
                                    self.bind(name, val.clone());
                                }
                                // Bind parameters
                                for (p, a) in params.iter().zip(vals) {
                                    self.bind(&p.name, a);
                                }
                                let result = self.eval_block(&body);
                                self.pop_scope();
                                result.map(|v| v.unwrap_or(Value::Unit))
                            }
                            _ => Err(format!("cannot call non-function value: {}", callee_val)),
                        }
                    }
                }
            }
            Expr::Tuple(elems) => {
                let mut vals = Vec::new();
                for e in elems {
                    vals.push(self.eval_expr(e)?);
                }
                Ok(Value::Tuple(vals))
            }
            Expr::List(elems) => {
                let mut vals = Vec::new();
                for e in elems {
                    vals.push(self.eval_expr(e)?);
                }
                Ok(Value::List(vals))
            }
            Expr::Comprehension { expr, var, iter, guard } => {
                let iter_val = self.eval_expr(iter)?;
                let items = match iter_val {
                    Value::List(l) => l,
                    _ => return Err("comprehension requires a list".into()),
                };
                let mut result = Vec::new();
                for item in items {
                    self.push_scope();
                    self.bind(var, item.clone());
                    let include = if let Some(g) = guard {
                        let cond = self.eval_expr(g)?;
                        is_truthy(&cond)
                    } else {
                        true
                    };
                    if include {
                        let val = self.eval_expr(expr)?;
                        result.push(val);
                    }
                    self.pop_scope();
                }
                Ok(Value::List(result))
            }
            Expr::Match(subject, arms) => {
                let val = self.eval_expr(subject)?;
                for arm in arms {
                    if let Some(bindings) = self.match_pattern(&arm.pat, &val) {
                        self.push_scope();
                        for (name, v) in bindings {
                            self.bind(&name, v);
                        }
                        if let Some(guard) = &arm.guard {
                            let g = self.eval_expr(guard)?;
                            if !is_truthy(&g) {
                                self.pop_scope();
                                continue;
                            }
                        }
                        let result = self.eval_expr(&arm.body);
                        self.pop_scope();
                        return result;
                    }
                }
                Err("non-exhaustive match".into())
            }
            Expr::Field(obj, field) => {
                // Special case: if accessing field on "self" identifier, look up field directly from actor
                if let Expr::Ident(name) = obj.as_ref() {
                    if name == "self" {
                        // Look up self from scope, then get the field from the actor
                        if let Some(Value::Actor(handle)) = self.lookup("self") {
                            let actor = handle.inner.read().map_err(|e| format!("actor lock failed: {}", e))?;
                            if let Some(value) = actor.fields.get(field.as_str()) {
                                return Ok(value.clone());
                            }
                            return Err(format!("actor field '{}' not found", field));
                        }
                        return Err("'self' is not bound to an actor".into());
                    }
                }
                let obj_val = self.eval_expr(obj)?;
                match obj_val {
                    Value::Record(fields) => {
                        fields
                            .get(field)
                            .cloned()
                            .ok_or_else(|| format!("field '{}' not found", field))
                    }
                    Value::Actor(handle) => {
                        // Actor field access using read lock
                        let actor = handle.inner.read().map_err(|e| format!("actor lock failed: {}", e))?;
                        actor.fields.get(field.as_str())
                            .cloned()
                            .ok_or_else(|| format!("actor field '{}' not found", field))
                    }
                    Value::Shared(arc) => {
                        let inner = arc.read().map_err(|e| format!("shared read lock failed: {}", e))?;
                        match &*inner {
                            Value::Record(fields) => fields.get(field.as_str()).cloned()
                                .ok_or_else(|| format!("field '{}' not found in shared record", field)),
                            _ => Err("field access on non-record shared value".into()),
                        }
                    }
                    Value::LocalShared(rc) => {
                        let inner = rc.0.borrow();
                        match &*inner {
                            Value::Record(fields) => fields.get(field.as_str()).cloned()
                                .ok_or_else(|| format!("field '{}' not found in local_shared record", field)),
                            _ => Err("field access on non-record local_shared value".into()),
                        }
                    }
                    _ => Err(format!("field access on non-record value {}", obj_val)),
                }
            }
            Expr::Record { ty: _, fields } => {
                let mut map = HashMap::new();
                for f in fields {
                    let v = self.eval_expr(&f.value)?;
                    map.insert(f.name.clone(), v);
                }
                Ok(Value::Record(map))
            }
            Expr::Index(obj, idx) => {
                let obj = self.eval_expr(obj)?;
                let idx = self.eval_expr(idx)?;
                match (obj, idx) {
                    (Value::List(list), Value::Int(i)) => {
                        let i = if i < 0 {
                            list.len() as i64 + i
                        } else {
                            i
                        } as usize;
                        list.get(i)
                            .cloned()
                            .ok_or_else(|| "index out of bounds".into())
                    }
                    (Value::String(s), Value::Int(i)) => {
                        let i = if i < 0 {
                            s.len() as i64 + i
                        } else {
                            i
                        } as usize;
                        s.chars()
                            .nth(i)
                            .map(|c| Value::String(c.to_string()))
                            .ok_or_else(|| "index out of bounds".into())
                    }
                    _ => Err("invalid index operation".into()),
                }
            }
            Expr::Try(expr) => {
                let v = self.eval_expr(expr)?;
                match v {
                    Value::Variant(name, vals) => {
                        // Check if this is a known failure variant
                        let is_failure = self.failure_variants.get(&name).copied().unwrap_or(false);
                        if is_failure {
                            // Return error value - eval_block will catch it and run compensation
                            Ok(Value::Error(format!("{} propagated via ?", name)))
                        } else {
                            // Treat as success variant - return inner value
                            Ok(vals.into_iter().next().unwrap_or(Value::Unit))
                        }
                    }
                    _ => {
                        Ok(Value::Error(format!("? operator requires Result or Option, found {}", v)))
                    }
                }
            }
            Expr::Spawn(_expr) => {
                // Spawn a concurrent task - for now just return a placeholder future
                // A full implementation would capture the expression and evaluate in a thread
                Err("spawn requires parasteps block".into())
            }
            Expr::Await(expr) => {
                // Check if this is a method call on an actor
                if let Expr::Call(callee, args) = expr.as_ref() {
                    if let Expr::Field(obj, method) = callee.as_ref() {
                        // Evaluate the object to get the actor handle
                        let obj_val = self.eval_expr(obj)?;
                        if let Value::Actor(_) = &obj_val {
                            // Spawn method call in a thread and wait for result
                            let (tx, rx) = std::sync::mpsc::channel();
                            let method = method.clone();
                            let args_clone: Vec<Value> = args.iter()
                                .map(|a| self.eval_expr(a))
                                .collect::<Result<Vec<_>, _>>()?;
                            let actor_arc = match &obj_val {
                                Value::Actor(h) => h.clone(),
                                _ => unreachable!(),
                            };
                            std::thread::spawn(move || {
                                let empty_file = File { imports: vec![], items: vec![] };
                                let mut interp = Interpreter::new(&empty_file);
                                let actor_val = Value::Actor(actor_arc);
                                let result = interp.call_method(&actor_val, &method, args_clone);
                                let _ = tx.send(result);
                            });
                            // Wait for the result
                            let result = rx.recv().map_err(|e| format!("await failed: {}", e))?;
                            return result;
                        }
                    }
                }
                // Default: evaluate and if it's a Future, wait for it
                let v = self.eval_expr(expr)?;
                match v {
                    Value::Future(rx) => {
                        let rx = rx.lock().map_err(|e| format!("await failed: {}", e))?;
                        rx.recv().map_err(|e| format!("await failed: {}", e))?
                    }
                    other => Ok(other),
                }
            }
            Expr::QuoteInterpolate(expr) => {
                let v = self.eval_expr(expr)?;
                Ok(Value::QuoteAst(Box::new(QuotedAst::Interpolate(Box::new(v)))))
            }
            Expr::Quote(block) => {
                // Convert the block to QuotedAst
                let quoted = self.quote_block(block)?;
                Ok(Value::QuoteAst(Box::new(quoted)))
            }
            Expr::Old(expr) => {
                // old(x) looks up the snapshot value from before function execution
                if let Expr::Ident(name) = expr.as_ref() {
                    let old_name = format!("old_{}", name);
                    if let Some(v) = self.lookup(&old_name) {
                        return Ok(v);
                    }
                }
                // If not found as old_ variable, evaluate the expression normally
                self.eval_expr(expr)
            }
            Expr::Lambda { params, ret, body } => {
                // Collect free variables from the lambda body
                let param_names: std::collections::HashSet<String> =
                    params.iter().map(|p| p.name.clone()).collect();
                let free_vars = collect_free_vars(body, &param_names);
                // Only capture variables that are actually used
                let mut captured = HashMap::new();
                for scope in self.env.iter().rev() {
                    for (name, val) in scope {
                        if free_vars.contains(name) && !captured.contains_key(name) {
                            captured.insert(name.clone(), val.clone());
                        }
                    }
                }
                Ok(Value::Closure {
                    params: params.clone(),
                    ret: ret.clone(),
                    body: body.clone(),
                    captured,
                })
            }
            Expr::Turbofish(name, _type_args, args) => {
                // Turbofish: func::<Type>(args) — evaluate args and call the function
                // Type arguments are ignored at runtime (monomorphization happens at compile time)
                let func = self.find_function(name)
                    .ok_or_else(|| format!("undefined function '{}'", name))?;
                let mut arg_vals = Vec::new();
                for arg in args {
                    arg_vals.push(self.eval_expr(arg)?);
                }
                self.call_func(&func, arg_vals)
            }
        }
    }

    /// Convert a block of statements into a quoted AST
    fn quote_block(&mut self, block: &Block) -> Result<QuotedAst, String> {
        let mut quoted_stmts = Vec::new();
        for stmt in block {
            if let Some(q) = self.quote_stmt(stmt)? {
                quoted_stmts.push(q);
            }
        }
        Ok(QuotedAst::Block(quoted_stmts))
    }

    /// Convert a single statement into a quoted AST (None for desc/rule/etc)
    fn quote_stmt(&mut self, stmt: &Stmt) -> Result<Option<QuotedAst>, String> {
        match stmt {
            Stmt::Let { pat, init, .. } => {
                let name = match pat {
                    Pattern::Variable(n) => n.clone(),
                    _ => return Ok(None),
                };
                let value = if let Some(e) = init {
                    Box::new(self.quote_expr(e)?)
                } else {
                    Box::new(QuotedAst::Literal(Lit::Unit))
                };
                Ok(Some(QuotedAst::Let { name, value }))
            }
            Stmt::Expr(e) => {
                Ok(Some(QuotedAst::ExprStmt(Box::new(self.quote_expr(e)?))))
            }
            Stmt::Return(e) => {
                let inner = if let Some(e) = e {
                    Some(Box::new(self.quote_expr(e)?))
                } else {
                    None
                };
                Ok(Some(QuotedAst::Return(inner)))
            }
            Stmt::Desc(_) | Stmt::Requires(_) | Stmt::Ensures(_) | Stmt::Math(_) | Stmt::Ellipsis | Stmt::MmsBlock(_) => Ok(None),
            _ => Ok(None),
        }
    }

    /// Convert an expression into a quoted AST
    fn quote_expr(&mut self, expr: &Expr) -> Result<QuotedAst, String> {
        match expr {
            Expr::Literal(l) => Ok(QuotedAst::Literal(l.clone())),
            Expr::Ident(name) => Ok(QuotedAst::Ident(name.clone())),
            Expr::Binary(op, l, r) => {
                Ok(QuotedAst::Binary(*op, Box::new(self.quote_expr(l)?), Box::new(self.quote_expr(r)?)))
            }
            Expr::Unary(op, e) => {
                Ok(QuotedAst::Unary(*op, Box::new(self.quote_expr(e)?)))
            }
            Expr::Call(callee, args) => {
                let q_callee = Box::new(self.quote_expr(callee)?);
                let q_args: Result<Vec<_>, _> = args.iter().map(|a| self.quote_expr(a)).collect();
                Ok(QuotedAst::Call(q_callee, q_args?))
            }
            Expr::Field(obj, field) => {
                Ok(QuotedAst::Field(Box::new(self.quote_expr(obj)?), field.clone()))
            }
            Expr::Index(obj, idx) => {
                Ok(QuotedAst::Index(Box::new(self.quote_expr(obj)?), Box::new(self.quote_expr(idx)?)))
            }
            Expr::Tuple(elems) => {
                let q_elems: Result<Vec<_>, _> = elems.iter().map(|e| self.quote_expr(e)).collect();
                Ok(QuotedAst::Tuple(q_elems?))
            }
            Expr::List(elems) => {
                let q_elems: Result<Vec<_>, _> = elems.iter().map(|e| self.quote_expr(e)).collect();
                Ok(QuotedAst::List(q_elems?))
            }
            Expr::Comprehension { expr, var, iter, guard } => {
                // For now, evaluate comprehension at quote time
                let iter_val = self.eval_expr(iter)?;
                let items = match iter_val {
                    Value::List(l) => l,
                    _ => return Err("comprehension requires a list".into()),
                };
                let mut result = Vec::new();
                for item in items {
                    self.push_scope();
                    self.bind(var, item.clone());
                    let include = if let Some(g) = guard {
                        let cond = self.eval_expr(g)?;
                        is_truthy(&cond)
                    } else {
                        true
                    };
                    if include {
                        let val = self.eval_expr(expr)?;
                        result.push(val);
                    }
                    self.pop_scope();
                }
                Ok(QuotedAst::List(result.into_iter().map(|v| QuotedAst::Interpolate(Box::new(v))).collect()))
            }
            Expr::Try(e) => Ok(QuotedAst::Try(Box::new(self.quote_expr(e)?))),
            Expr::Spawn(e) => Ok(QuotedAst::Spawn(Box::new(self.quote_expr(e)?))),
            Expr::Await(e) => Ok(QuotedAst::Await(Box::new(self.quote_expr(e)?))),
            Expr::Old(e) => {
                // old() in quote context - evaluate and return as interpolation
                let v = self.eval_expr(e)?;
                Ok(QuotedAst::Interpolate(Box::new(v)))
            }
            Expr::QuoteInterpolate(e) => {
                // Interpolation: evaluate the expression and embed the result
                let v = self.eval_expr(e)?;
                Ok(QuotedAst::Interpolate(Box::new(v)))
            }
            Expr::Quote(block) => {
                let quoted = self.quote_block(block)?;
                Ok(quoted)
            }
            Expr::Record { ty, fields } => {
                let q_fields: Result<Vec<RecordFieldExprQuoted>, String> = fields.iter().map(|f| {
                    Ok(RecordFieldExprQuoted {
                        name: f.name.clone(),
                        value: self.quote_expr(&f.value)?,
                    })
                }).collect();
                Ok(QuotedAst::Record { ty: ty.clone(), fields: q_fields? })
            }
            Expr::Match(subject, arms) => {
                let q_subject = Box::new(self.quote_expr(subject)?);
                let q_arms: Result<Vec<MatchArmQuoted>, String> = arms.iter().map(|arm| {
                    Ok(MatchArmQuoted {
                        pat: arm.pat.clone(),
                        guard: arm.guard.as_ref().map(|g| self.quote_expr(g)).transpose()?,
                        body: self.quote_expr(&arm.body)?,
                    })
                }).collect();
                Ok(QuotedAst::Match(q_subject, q_arms?))
            }
            Expr::Lambda { params: _, ret: _, body } => {
                // Quote the lambda body as a block
                let quoted_body = self.quote_block(body)?;
                // Represent lambda as a call to a synthetic function
                // For simplicity, just quote the body
                Ok(quoted_body)
            }
            Expr::Turbofish(name, _type_args, args) => {
                // In quote context, treat turbofish as a regular call
                let mut q_args = Vec::new();
                for arg in args {
                    q_args.push(self.quote_expr(arg)?);
                }
                Ok(QuotedAst::Call(Box::new(QuotedAst::Ident(name.clone())), q_args))
            }
        }
    }

    fn call_named(&mut self, name: &str, args: Vec<Value>) -> Result<Value, String> {
        // First check if the name is bound to a closure in the local scope
        if let Some(v) = self.lookup(name) {
            match v {
                Value::Closure { params, ret: _, body, captured } => {
                    if params.len() != args.len() {
                        return Err(format!(
                            "closure '{}' expects {} arguments, got {}",
                            name, params.len(), args.len()
                        ));
                    }
                    self.push_scope();
                    for (n, val) in &captured {
                        self.bind(n, val.clone());
                    }
                    for (p, a) in params.iter().zip(args) {
                        self.bind(&p.name, a);
                    }
                    let result = self.eval_block(&body);
                    self.pop_scope();
                    return result.map(|v| v.unwrap_or(Value::Unit));
                }
                other => {
                    // Not a closure, fall through to other lookup methods
                    drop(other);
                }
            }
        }

        // Handle Actor.spawn() calls
        if let Some(actor_name) = name.strip_suffix(".spawn") {
            return self.spawn_actor(actor_name, args);
        }

        if let Some(&arity) = self.constructors.get(name) {
            if args.len() != arity {
                return Err(format!(
                    "constructor '{}' expects {} arguments, got {}",
                    name, arity, args.len()
                ));
            }
            // Check if this is a newtype constructor - wrap in Value::Newtype
            if *self.newtype_constructors.get(name).unwrap_or(&false) && args.len() == 1 {
                return Ok(Value::Newtype(Box::new(args.into_iter().next().unwrap())));
            }
            return Ok(Value::Variant(name.into(), args));
        }
        match name {
            "println" => {
                let parts: Vec<String> = args.iter().map(|v| v.to_string()).collect();
                println!("{}", parts.join(" "));
                Ok(Value::Unit)
            }
            "assert" => {
                if args.len() != 1 {
                    return Err("assert expects 1 argument".into());
                }
                if !is_truthy(&args[0]) {
                    return Err(format!("assertion failed: {}", args[0]));
                }
                Ok(Value::Unit)
            }
            "range" => {
                if args.len() != 2 {
                    return Err("range expects 2 arguments".into());
                }
                let start = match &args[0] {
                    Value::Int(v) => *v,
                    _ => return Err("range start must be integer".into()),
                };
                let end = match &args[1] {
                    Value::Int(v) => *v,
                    _ => return Err("range end must be integer".into()),
                };
                let list: Vec<Value> = (start..end).map(Value::Int).collect();
                Ok(Value::List(list))
            }
            "sqrt" => {
                if args.len() != 1 {
                    return Err("sqrt expects 1 argument".into());
                }
                match &args[0] {
                    Value::Int(v) => Ok(Value::Float((*v as f64).sqrt())),
                    Value::Float(v) => Ok(Value::Float(v.sqrt())),
                    _ => Err("sqrt expects a number".into()),
                }
            }
            "len" => {
                if args.len() != 1 {
                    return Err("len expects 1 argument".into());
                }
                match &args[0] {
                    Value::String(s) => Ok(Value::Int(s.chars().count() as i64)),
                    Value::List(l) => Ok(Value::Int(l.len() as i64)),
                    _ => Err("len expects a string or list".into()),
                }
            }
            "to_string" => {
                if args.len() != 1 {
                    return Err("to_string expects 1 argument".into());
                }
                Ok(Value::String(args[0].to_string()))
            }
            "abs" => {
                if args.len() != 1 {
                    return Err("abs expects 1 argument".into());
                }
                match &args[0] {
                    Value::Int(v) => Ok(Value::Int(v.abs())),
                    Value::Float(v) => Ok(Value::Float(v.abs())),
                    _ => Err("abs expects a number".into()),
                }
            }
            "push" => {
                if args.len() != 2 {
                    return Err("push expects 2 arguments (list, elem)".into());
                }
                match &args[0] {
                    Value::List(l) => {
                        let mut new_list = l.clone();
                        new_list.push(args[1].clone());
                        Ok(Value::List(new_list))
                    }
                    _ => Err("push first argument must be a list".into()),
                }
            }
            "pop" => {
                if args.len() != 1 {
                    return Err("pop expects 1 argument (list)".into());
                }
                match &args[0] {
                    Value::List(l) => {
                        if l.is_empty() {
                            return Err("pop from empty list".into());
                        }
                        let mut new_list = l.clone();
                        let popped = new_list.pop().unwrap();
                        // Return (popped, new_list) as a tuple
                        Ok(Value::Tuple(vec![popped, Value::List(new_list)]))
                    }
                    _ => Err("pop expects a list".into()),
                }
            }
            "min" => {
                if args.len() != 2 {
                    return Err("min expects 2 arguments".into());
                }
                match (&args[0], &args[1]) {
                    (Value::Int(a), Value::Int(b)) => Ok(Value::Int(*a.min(b))),
                    (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a.min(*b))),
                    _ => Err("min expects two numbers of the same type".into()),
                }
            }
            "max" => {
                if args.len() != 2 {
                    return Err("max expects 2 arguments".into());
                }
                match (&args[0], &args[1]) {
                    (Value::Int(a), Value::Int(b)) => Ok(Value::Int(*a.max(b))),
                    (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a.max(*b))),
                    _ => Err("max expects two numbers of the same type".into()),
                }
            }
            "contains" => {
                if args.len() != 2 {
                    return Err("contains expects 2 arguments (container, elem)".into());
                }
                match &args[0] {
                    Value::List(l) => {
                        let found = l.iter().any(|v| values_equal(v, &args[1]));
                        Ok(Value::Bool(found))
                    }
                    Value::String(s) => {
                        match &args[1] {
                            Value::String(sub) => Ok(Value::Bool(s.contains(sub.as_str()))),
                            _ => Err("contains on string expects a string needle".into()),
                        }
                    }
                    _ => Err("contains expects a list or string".into()),
                }
            }
            "input" => {
                use std::io::{self, BufRead};
                let mut line = String::new();
                io::stdin().lock().read_line(&mut line).map_err(|e| format!("input error: {}", e))?;
                // Remove trailing newline
                if line.ends_with('\n') {
                    line.pop();
                }
                if line.ends_with('\r') {
                    line.pop();
                }
                Ok(Value::String(line))
            }
            "ast_dump" => {
                if args.len() != 1 {
                    return Err("ast_dump expects 1 argument (a quoted AST)".into());
                }
                match &args[0] {
                    Value::QuoteAst(q) => Ok(Value::String(format!("{:?}", q))),
                    other => Ok(Value::String(format!("Not a QuoteAst: {}", other))),
                }
            }
            "ast_eval" => {
                if args.len() != 1 {
                    return Err("ast_eval expects 1 argument (a quoted AST)".into());
                }
                match &args[0] {
                    Value::QuoteAst(q) => self.eval_quoted_ast(q),
                    other => Err(format!("ast_eval expects a QuoteAst, got {}", other)),
                }
            }
            _ => {
                let func = self
                    .find_function(name)
                    .ok_or_else(|| format!("undefined function '{}'", name))?;
                self.call_func(&func, args)
            }
        }
    }

    /// Evaluate a quoted AST as code (simplified version)
    fn eval_quoted_ast(&mut self, qa: &QuotedAst) -> Result<Value, String> {
        match qa {
            QuotedAst::Literal(l) => Ok(match l {
                Lit::Int(v) => Value::Int(*v),
                Lit::Float(v) => Value::Float(*v),
                Lit::Bool(v) => Value::Bool(*v),
                Lit::String(v) => Value::String(v.clone()),
                Lit::FString(_) => Value::Unit, // f-strings not supported in quoted context
                Lit::Unit => Value::Unit,
            }),
            QuotedAst::Ident(name) => {
                if let Some(v) = self.lookup(name) {
                    Ok(v)
                } else {
                    Err(format!("undefined variable '{}' in quoted AST", name))
                }
            }
            QuotedAst::Binary(op, l, r) => {
                let lv = self.eval_quoted_ast(l)?;
                let rv = self.eval_quoted_ast(r)?;
                match op {
                    BinOp::Add => {
                        match (&lv, &rv) {
                            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
                            (Value::String(a), Value::String(b)) => Ok(Value::String(format!("{}{}", a, b))),
                            _ => Err(format!("unsupported + for {} and {}", lv, rv)),
                        }
                    }
                    BinOp::Sub => {
                        match (&lv, &rv) {
                            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a - b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),
                            _ => Err(format!("unsupported - for {} and {}", lv, rv)),
                        }
                    }
                    BinOp::Mul => {
                        match (&lv, &rv) {
                            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a * b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
                            _ => Err(format!("unsupported * for {} and {}", lv, rv)),
                        }
                    }
                    BinOp::Div => {
                        match (&lv, &rv) {
                            (Value::Int(a), Value::Int(b)) => {
                                if *b == 0 { return Err("division by zero".into()); }
                                Ok(Value::Int(a / b))
                            }
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a / b)),
                            _ => Err(format!("unsupported / for {} and {}", lv, rv)),
                        }
                    }
                    BinOp::EqCmp => Ok(Value::Bool(values_equal(&lv, &rv))),
                    BinOp::NeCmp => Ok(Value::Bool(!values_equal(&lv, &rv))),
                    BinOp::Lt => {
                        match (&lv, &rv) {
                            (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a < b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a < b)),
                            _ => Err(format!("unsupported < for {} and {}", lv, rv)),
                        }
                    }
                    BinOp::Gt => {
                        match (&lv, &rv) {
                            (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a > b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a > b)),
                            _ => Err(format!("unsupported > for {} and {}", lv, rv)),
                        }
                    }
                    _ => Err("unsupported binary op in quoted AST".into()),
                }
            }
            QuotedAst::Unary(op, e) => {
                let v = self.eval_quoted_ast(e)?;
                match op {
                    UnOp::Neg => match v {
                        Value::Int(n) => Ok(Value::Int(-n)),
                        Value::Float(n) => Ok(Value::Float(-n)),
                        _ => Err(format!("unsupported neg for {}", v)),
                    },
                    UnOp::Not => match v {
                        Value::Bool(b) => Ok(Value::Bool(!b)),
                        _ => Err(format!("unsupported not for {}", v)),
                    },
                    _ => Err("unsupported unary op in quoted AST".into()),
                }
            }
            QuotedAst::Interpolate(v) => Ok(*v.clone()),
            QuotedAst::Block(stmts) => {
                self.push_scope();
                let mut result = Value::Unit;
                for stmt in stmts {
                    result = self.eval_quoted_ast(stmt)?;
                }
                self.pop_scope();
                Ok(result)
            }
            QuotedAst::Let { name, value } => {
                let v = self.eval_quoted_ast(value)?;
                self.bind(name, v.clone());
                Ok(v)
            }
            QuotedAst::ExprStmt(e) => self.eval_quoted_ast(e),
            QuotedAst::Return(e) => {
                if let Some(e) = e {
                    self.eval_quoted_ast(e)
                } else {
                    Ok(Value::Unit)
                }
            }
            QuotedAst::List(elems) => {
                let vals: Result<Vec<_>, _> = elems.iter().map(|e| self.eval_quoted_ast(e)).collect();
                Ok(Value::List(vals?))
            }
            QuotedAst::Tuple(elems) => {
                let vals: Result<Vec<_>, _> = elems.iter().map(|e| self.eval_quoted_ast(e)).collect();
                Ok(Value::Tuple(vals?))
            }
            QuotedAst::Call(callee, args) => {
                let func_val = self.eval_quoted_ast(callee)?;
                let arg_vals: Result<Vec<_>, _> = args.iter().map(|a| self.eval_quoted_ast(a)).collect();
                let arg_vals = arg_vals?;
                match func_val {
                    Value::Closure { params, ret: _, body, captured } => {
                        if params.len() != arg_vals.len() {
                            return Err(format!("closure expects {} args, got {}", params.len(), arg_vals.len()));
                        }
                        self.push_scope();
                        for (n, v) in &captured {
                            self.bind(n, v.clone());
                        }
                        for (p, a) in params.iter().zip(arg_vals) {
                            self.bind(&p.name, a);
                        }
                        let result = self.eval_block(&body);
                        self.pop_scope();
                        result.map(|v| v.unwrap_or(Value::Unit))
                    }
                    _ => Err("cannot call non-closure in quoted AST".into()),
                }
            }
            _ => Err(format!("unsupported quoted AST node: {:?}", qa)),
        }
    }

    /// Spawn a new actor instance
    fn spawn_actor(&mut self, actor_name: &str, _args: Vec<Value>) -> Result<Value, String> {
        let actor_def = self.find_actor(actor_name)
            .ok_or_else(|| format!("actor '{}' not found", actor_name))?;

        // Create actor instance with initialized fields
        let mut fields = HashMap::new();
        for field in &actor_def.fields {
            let value = field.init.as_ref()
                .map(|e| self.eval_expr(e))
                .transpose()?
                .unwrap_or_else(|| match &field.ty {
                    Type::Name(n, _) if n == "i32" => Value::Int(0),
                    Type::Name(n, _) if n == "f64" => Value::Float(0.0),
                    Type::Name(n, _) if n == "bool" => Value::Bool(false),
                    Type::Name(n, _) if n == "string" => Value::String(String::new()),
                    _ => Value::Unit,
                });
            fields.insert(field.name.clone(), value);
        }

        let instance = ActorInstance {
            actor_name: actor_name.to_string(),
            fields,
            methods: actor_def.methods.clone(),
        };

        let handle = ActorHandle::new(instance);
        Ok(Value::Actor(handle))
    }

    /// Call a method on an actor instance
    fn call_method(&mut self, obj: &Value, method: &str, args: Vec<Value>) -> Result<Value, String> {
        match obj {
            Value::Shared(arc) => {
                match method {
                    "clone" => Ok(Value::Shared(Arc::clone(arc))),
                    "deref" | "inner" => {
                        let inner = arc.read().map_err(|e| format!("shared read lock failed: {}", e))?;
                        Ok(inner.clone())
                    }
                    _ => Err(format!("shared value has no method '{}'", method)),
                }
            }
            Value::LocalShared(rc) => {
                match method {
                    "clone" => Ok(Value::LocalShared(SendRc(Rc::clone(&rc.0)))),
                    "deref" | "inner" => {
                        let inner = rc.0.borrow();
                        Ok(inner.clone())
                    }
                    _ => Err(format!("local_shared value has no method '{}'", method)),
                }
            }
            Value::WeakShared(w) => {
                match method {
                    "upgrade" => {
                        match w.upgrade() {
                            Some(arc) => Ok(Value::Shared(arc)),
                            None => Ok(Value::Variant("None".into(), vec![])),
                        }
                    }
                    _ => Err(format!("weak_shared value has no method '{}'", method)),
                }
            }
            Value::WeakLocal(w) => {
                match method {
                    "upgrade" => {
                        match w.upgrade() {
                            Some(rc) => Ok(Value::LocalShared(rc)),
                            None => Ok(Value::Variant("None".into(), vec![])),
                        }
                    }
                    _ => Err(format!("weak_local value has no method '{}'", method)),
                }
            }
            Value::Cap(names) => {
                match method {
                    "split" => {
                        if names.len() < 2 {
                            return Err("split() requires a combined capability (cap A + B)".into());
                        }
                        let tuple: Vec<Value> = names.iter()
                            .map(|n| Value::Cap(vec![n.clone()]))
                            .collect();
                        Ok(Value::Tuple(tuple))
                    }
                    _ => Err(format!("cap value has no method '{}'", method)),
                }
            }
            Value::Actor(actor_arc) => {
                // Handle special methods
                match method {
                    "spawn" => {
                        // spawn() doesn't make sense on an instance - it's a constructor
                        Err("spawn() should be called on Actor type, not instance".into())
                    }
                    _ => {
                        // First, get a clone of the actor's current state
                        let actor_name: String;
                        let actor_fields: HashMap<String, Value>;
                        let actor_methods: Vec<FuncDef>;
                        {
                            let actor = actor_arc.inner.read().map_err(|e| format!("actor lock failed: {}", e))?;
                            actor_name = actor.actor_name.clone();
                            actor_fields = actor.fields.clone();
                            actor_methods = actor.methods.clone();
                        }

                        // Find the method in the actor's methods
                        let func = actor_methods.iter()
                            .find(|f| f.name == method)
                            .ok_or_else(|| format!("actor {} has no method '{}'", actor_name, method))?;

                        // For actor methods, we need to call with self bound to this actor
                        self.push_scope();
                        // Bind 'self' to the actor handle itself (for self.field = ... access)
                        self.bind("self", obj.clone());
                        // Also bind all actor fields to scope (for direct field access)
                        for (field_name, field_value) in &actor_fields {
                            self.bind(field_name, field_value.clone());
                        }

                        let result = self.call_func(func, args);

                        self.pop_scope();

                        result
                    }
                }
            }
            _ => Err(format!("cannot call method '{}' on value {}", method, obj)),
        }
    }

    fn match_pattern(&self, pat: &Pattern, value: &Value) -> Option<Vec<(String, Value)>> {
        let mut bindings = Vec::new();
        if self.match_pattern_inner(pat, value, &mut bindings) {
            Some(bindings)
        } else {
            None
        }
    }

    fn match_pattern_inner(&self, pat: &Pattern, value: &Value, bindings: &mut Vec<(String, Value)>) -> bool {
        match pat {
            Pattern::Wildcard => true,
            Pattern::Variable(name) => {
                bindings.push((name.clone(), value.clone()));
                true
            }
            Pattern::Literal(l) => {
                let expected = match l {
                    Lit::Int(v) => Value::Int(*v),
                    Lit::Float(v) => Value::Float(*v),
                    Lit::Bool(v) => Value::Bool(*v),
                    Lit::String(v) => Value::String(v.clone()),
                    Lit::FString(_) => return false, // f-strings can't be used in patterns
                    Lit::Unit => Value::Unit,
                };
                values_equal(value, &expected)
            }
            Pattern::Constructor(name, pats) => {
                match value {
                    Value::Variant(vname, vals) if vname == name => {
                        if pats.len() != vals.len() {
                            return false;
                        }
                        for (p, v) in pats.iter().zip(vals.iter()) {
                            if !self.match_pattern_inner(p, v, bindings) {
                                return false;
                            }
                        }
                        true
                    }
                    // Handle newtype pattern matching: UserId(v) matches Newtype(v)
                    Value::Newtype(inner) if pats.len() == 1 => {
                        self.match_pattern_inner(&pats[0], inner, bindings)
                    }
                    _ => false,
                }
            }
            Pattern::Tuple(pats) => {
                match value {
                    Value::Tuple(vals) if pats.len() == vals.len() => {
                        for (p, v) in pats.iter().zip(vals.iter()) {
                            if !self.match_pattern_inner(p, v, bindings) {
                                return false;
                            }
                        }
                        true
                    }
                    _ => false,
                }
            }
        }
    }

    fn eval_unary(&mut self, op: UnOp, e: &Expr) -> Result<Value, String> {
        let v = self.eval_expr(e)?;
        match op {
            UnOp::Neg => match v {
                Value::Int(x) => Ok(Value::Int(-x)),
                Value::Float(x) => Ok(Value::Float(-x)),
                _ => Err("cannot negate non-number".into()),
            },
            UnOp::Not => Ok(Value::Bool(!is_truthy(&v))),
            UnOp::Ref | UnOp::RefMut => {
                // For now, & and &mut just return the value itself (simplified borrowing)
                // In a full implementation, this would create a reference type
                Ok(v)
            }
        }
    }

    fn eval_binary(&mut self, op: BinOp, l: &Expr, r: &Expr) -> Result<Value, String> {
        // short-circuit logic
        match op {
            BinOp::And => {
                let left = self.eval_expr(l)?;
                if !is_truthy(&left) {
                    return Ok(Value::Bool(false));
                }
                return Ok(Value::Bool(is_truthy(&self.eval_expr(r)?)));
            }
            BinOp::Or => {
                let left = self.eval_expr(l)?;
                if is_truthy(&left) {
                    return Ok(Value::Bool(true));
                }
                return Ok(Value::Bool(is_truthy(&self.eval_expr(r)?)));
            }
            _ => {}
        }
        let left = self.eval_expr(l)?;
        let right = self.eval_expr(r)?;
        match op {
            BinOp::Add => match (&left, &right) {
                (Value::String(a), Value::String(b)) => Ok(Value::String(format!("{}{}", a, b))),
                _ => numeric_op(left, right, |a, b| a + b, |a, b| a + b),
            },
            BinOp::Sub => numeric_op(left, right, |a, b| a - b, |a, b| a - b),
            BinOp::Mul => numeric_op(left, right, |a, b| a * b, |a, b| a * b),
            BinOp::Div => match (&left, &right) {
                (Value::Int(_), Value::Int(0)) => Err("division by zero".into()),
                (Value::Float(_), Value::Float(b)) if *b == 0.0 => Err("division by zero".into()),
                _ => numeric_op(left, right, |a, b| a / b, |a, b| a / b),
            },
            BinOp::Mod => match (&left, &right) {
                (Value::Int(_), Value::Int(0)) => Err("modulo by zero".into()),
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a % b)),
                _ => Err("modulo requires integers".into()),
            },
            BinOp::Pow => match (&left, &right) {
                (Value::Int(_), Value::Int(b)) if *b < 0 => Err("negative exponent not supported for integers".into()),
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.pow(*b as u32))),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a.powf(*b))),
                _ => Err("power requires numbers".into()),
            },
            BinOp::EqCmp => Ok(Value::Bool(values_equal(&left, &right))),
            BinOp::NeCmp => Ok(Value::Bool(!values_equal(&left, &right))),
            BinOp::Lt => compare_op(left, right, |o| o == std::cmp::Ordering::Less),
            BinOp::Gt => compare_op(left, right, |o| o == std::cmp::Ordering::Greater),
            BinOp::Le => compare_op(left, right, |o| o != std::cmp::Ordering::Greater),
            BinOp::Ge => compare_op(left, right, |o| o != std::cmp::Ordering::Less),
            BinOp::BitAnd => match (left, right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a & b)),
                _ => Err("bitwise and requires integers".into()),
            },
            BinOp::BitOr => match (left, right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a | b)),
                _ => Err("bitwise or requires integers".into()),
            },
            BinOp::BitXor => match (left, right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a ^ b)),
                _ => Err("bitwise xor requires integers".into()),
            },
            BinOp::Shl => match (left, right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a << b)),
                _ => Err("shift requires integers".into()),
            },
            BinOp::Shr => match (left, right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a >> b)),
                _ => Err("shift requires integers".into()),
            },
            BinOp::Assign => Err("assignment as expression not supported".into()),
            BinOp::And | BinOp::Or => unreachable!(),
        }
    }
}

/// Collect free variables from a block (variables not bound locally)
fn collect_free_vars(block: &Block, bound: &std::collections::HashSet<String>) -> std::collections::HashSet<String> {
    let mut free = std::collections::HashSet::new();
    let mut local_bound = bound.clone();
    for stmt in block {
        let current_bound = local_bound.clone();
        collect_stmt_free_vars(stmt, &current_bound, &mut free, &mut local_bound);
    }
    free
}

fn collect_stmt_free_vars(
    stmt: &Stmt,
    bound: &std::collections::HashSet<String>,
    free: &mut std::collections::HashSet<String>,
    local_bound: &mut std::collections::HashSet<String>,
) {
    match stmt {
        Stmt::Let { pat, init, .. } => {
            if let Some(e) = init {
                collect_expr_free_vars(e, bound, free);
            }
            // Add pattern variables to local bound
            collect_pattern_names(pat, local_bound);
        }
        Stmt::SharedLet { init, name, .. } => {
            collect_expr_free_vars(init, bound, free);
            local_bound.insert(name.clone());
        }
        Stmt::Expr(e) | Stmt::Return(Some(e)) => {
            collect_expr_free_vars(e, bound, free);
        }
        Stmt::If { cond, then_, else_ } => {
            collect_expr_free_vars(cond, bound, free);
            for s in then_ {
                collect_stmt_free_vars(s, bound, free, local_bound);
            }
            if let Some(else_block) = else_ {
                for s in else_block {
                    collect_stmt_free_vars(s, bound, free, local_bound);
                }
            }
        }
        Stmt::While { cond, body } => {
            collect_expr_free_vars(cond, bound, free);
            for s in body {
                collect_stmt_free_vars(s, bound, free, local_bound);
            }
        }
        Stmt::For { var, iterable, body } => {
            collect_expr_free_vars(iterable, bound, free);
            let mut inner_bound = local_bound.clone();
            inner_bound.insert(var.clone());
            for s in body {
                collect_stmt_free_vars(s, &inner_bound, free, local_bound);
            }
        }
        Stmt::Assign { target, value } => {
            collect_expr_free_vars(target, bound, free);
            collect_expr_free_vars(value, bound, free);
        }
        Stmt::Block(block) => {
            for s in block {
                collect_stmt_free_vars(s, bound, free, local_bound);
            }
        }
        Stmt::OnFailure(block) | Stmt::Parasteps(block) | Stmt::Arena(block) => {
            for s in block {
                collect_stmt_free_vars(s, bound, free, local_bound);
            }
        }
        _ => {}
    }
}

fn collect_expr_free_vars(
    expr: &Expr,
    bound: &std::collections::HashSet<String>,
    free: &mut std::collections::HashSet<String>,
) {
    match expr {
        Expr::Ident(name) => {
            if !bound.contains(name) {
                free.insert(name.clone());
            }
        }
        Expr::Binary(_, l, r) => {
            collect_expr_free_vars(l, bound, free);
            collect_expr_free_vars(r, bound, free);
        }
        Expr::Unary(_, e) | Expr::Try(e) | Expr::Spawn(e) | Expr::Await(e) => {
            collect_expr_free_vars(e, bound, free);
        }
        Expr::Call(callee, args) => {
            collect_expr_free_vars(callee, bound, free);
            for a in args {
                collect_expr_free_vars(a, bound, free);
            }
        }
        Expr::Field(obj, _) | Expr::Index(obj, _) => {
            collect_expr_free_vars(obj, bound, free);
        }
        Expr::Tuple(elems) | Expr::List(elems) => {
            for e in elems {
                collect_expr_free_vars(e, bound, free);
            }
        }
        Expr::Match(subject, arms) => {
            collect_expr_free_vars(subject, bound, free);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_expr_free_vars(g, bound, free);
                }
                collect_expr_free_vars(&arm.body, bound, free);
            }
        }
        Expr::Record { fields, .. } => {
            for f in fields {
                collect_expr_free_vars(&f.value, bound, free);
            }
        }
        Expr::Lambda { params, body, .. } => {
            let mut inner_bound = bound.clone();
            for p in params {
                inner_bound.insert(p.name.clone());
            }
            let inner_free = collect_free_vars(body, &inner_bound);
            free.extend(inner_free);
        }
        Expr::Old(expr) => {
            collect_expr_free_vars(expr, bound, free);
        }
        _ => {}
    }
}

fn collect_pattern_names(pat: &Pattern, names: &mut std::collections::HashSet<String>) {
    match pat {
        Pattern::Variable(name) => { names.insert(name.clone()); }
        Pattern::Tuple(pats) => {
            for p in pats {
                collect_pattern_names(p, names);
            }
        }
        Pattern::Constructor(_, pats) => {
            for p in pats {
                collect_pattern_names(p, names);
            }
        }
        _ => {}
    }
}

/// Check if a value contains an ArenaRef from a specific arena
fn contains_arena_ref(v: &Value, arena_id: usize) -> bool {
    match v {
        Value::ArenaRef(id, _) => *id == arena_id,
        Value::List(elems) => elems.iter().any(|e| contains_arena_ref(e, arena_id)),
        Value::Tuple(elems) => elems.iter().any(|e| contains_arena_ref(e, arena_id)),
        Value::Record(fields) => fields.values().any(|v| contains_arena_ref(v, arena_id)),
        Value::Variant(_, args) => args.iter().any(|v| contains_arena_ref(v, arena_id)),
        Value::Newtype(inner) => contains_arena_ref(inner, arena_id),
        _ => false,
    }
}
/// Copy types: Int, Float, Bool, Unit, and Tuples of Copy types
fn is_copy(v: &Value) -> bool {
    match v {
        Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Unit => true,
        Value::Tuple(elems) => elems.iter().all(is_copy),
        Value::Newtype(inner) => is_copy(inner),
        // Shared/LocalShared are reference-counted, cloning is cheap
        Value::Shared(_) | Value::LocalShared(_) => true,
        _ => false,
    }
}

fn is_truthy(v: &Value) -> bool {
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

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => (a - b).abs() < f64::EPSILON,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Unit, Value::Unit) => true,
        (Value::List(a), Value::List(b)) => a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| values_equal(x, y)),
        (Value::Tuple(a), Value::Tuple(b)) => a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| values_equal(x, y)),
        (Value::Variant(an, av), Value::Variant(bn, bv)) => {
            an == bn && av.len() == bv.len() && av.iter().zip(bv.iter()).all(|(x, y)| values_equal(x, y))
        }
        (Value::Record(a), Value::Record(b)) => {
            a.len() == b.len() && a.iter().all(|(k, v)| b.get(k).map(|bv| values_equal(v, bv)).unwrap_or(false))
        }
        (Value::Newtype(a), Value::Newtype(b)) => values_equal(a, b),
        _ => false,
    }
}

fn numeric_op(
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

fn compare_op<F>(a: Value, b: Value, f: F) -> Result<Value, String>
where
    F: Fn(std::cmp::Ordering) -> bool,
{
    let ord = match (a, b) {
        (Value::Int(a), Value::Int(b)) => a.cmp(&b),
        (Value::Float(a), Value::Float(b)) => a.partial_cmp(&b).ok_or("cannot compare floats")?,
        (Value::String(a), Value::String(b)) => a.cmp(&b),
        _ => return Err("comparison requires comparable types".into()),
    };
    Ok(Value::Bool(f(ord)))
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        values_equal(self, other)
    }
}
