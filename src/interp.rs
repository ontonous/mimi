use crate::ast::*;
use std::collections::HashMap;

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
        }
    }
}

pub struct Interpreter<'a> {
    file: &'a File,
    env: Vec<HashMap<String, Value>>,
    constructors: HashMap<String, usize>,
    /// Set of constructor names that are newtypes (for wrapping result in Value::Newtype)
    newtype_constructors: HashMap<String, bool>,
    /// Maps type name to its variants (for Result/Option-like types)
    type_variants: HashMap<String, Vec<String>>,
    /// Variants that represent "failure" (Err, None, *Error, *Fail)
    failure_variants: HashMap<String, bool>,
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
        for item in &file.items {
            Self::collect_constructors(item, &mut constructors, &mut newtype_constructors, &mut type_variants, &mut failure_variants);
        }
        Self {
            file,
            env: vec![HashMap::new()],
            constructors,
            newtype_constructors,
            type_variants,
            failure_variants,
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
    }

    fn pop_scope(&mut self) {
        self.env.pop();
    }

    fn bind(&mut self, name: &str, value: Value) {
        self.env.last_mut().unwrap().insert(name.into(), value);
    }

    fn lookup(&self, name: &str) -> Option<Value> {
        for scope in self.env.iter().rev() {
            if let Some(v) = scope.get(name) {
                return Some(v.clone());
            }
        }
        None
    }

    fn assign(&mut self, name: &str, value: Value) -> Result<(), String> {
        for scope in self.env.iter_mut().rev() {
            if scope.contains_key(name) {
                scope.insert(name.into(), value);
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
        for (p, a) in func.params.iter().zip(args) {
            self.bind(&p.name, a);
        }
        let result = self.eval_block(&func.body);
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
            Stmt::Let { pat, init, mut_: _, ref_, ty: _ } => {
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
                        self.bind(&name, val);
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
                return Ok(Some(v));
            }
            Stmt::Expr(e) => {
                self.eval_expr(e)?;
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

                self.arena_depth -= 1;
                self.pop_scope();

                // Arena is automatically reclaimed when block exits
                // (the Arena struct is dropped here)
                self.arenas.pop();

                return result;
            }
            Stmt::Assign { target, value } => {
                let v = self.eval_expr(value)?;
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
                            _ => return Err(format!("cannot assign to non-record/non-actor value")),
                        }
                    }
                    _ => return Err("assignment target must be a variable".into()),
                }
            }
            Stmt::Desc(_) | Stmt::Requires(_) | Stmt::Ensures(_) | Stmt::Math(_) | Stmt::Ellipsis => {}
            Stmt::Drop(expr) => {
                // Evaluate and discard the value (for linear capability drops)
                self.eval_expr(expr)?;
                // In a real implementation, this would track capability usage
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
                let mut futures = Vec::new();
                let mut spawn_bindings: HashMap<String, std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<Result<Value, String>>>>> = HashMap::new();

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
                Lit::Unit => Value::Unit,
            }),
            Expr::Ident(name) => {
                if let Some(v) = self.lookup(name) {
                    Ok(v)
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
                    _ => Err("callee must be a function name".into()),
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
                        return Err(format!("'self' is not bound to an actor"));
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
                // Await a future - receive the result from the channel
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
            Expr::Quote(_) => {
                // quote! without interpolation produces a literal AST
                // For v1.0, quote! returns the AST as a value that can be spliced
                Err("quote! must be used inside a comptime context or with $(...) interpolation".into())
            }
        }
    }

    fn call_named(&mut self, name: &str, args: Vec<Value>) -> Result<Value, String> {
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
            if self.newtype_constructors.get(name).copied().unwrap_or(false) {
                if args.len() == 1 {
                    return Ok(Value::Newtype(Box::new(args.into_iter().next().unwrap())));
                }
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
            _ => {
                let func = self
                    .find_function(name)
                    .ok_or_else(|| format!("undefined function '{}'", name))?;
                self.call_func(&func, args)
            }
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
            _ => Err(format!("cannot call method '{}' on non-actor value", method)),
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
            BinOp::Add => numeric_op(left, right, |a, b| a + b, |a, b| a + b),
            BinOp::Sub => numeric_op(left, right, |a, b| a - b, |a, b| a - b),
            BinOp::Mul => numeric_op(left, right, |a, b| a * b, |a, b| a * b),
            BinOp::Div => numeric_op(left, right, |a, b| a / b, |a, b| a / b),
            BinOp::Mod => match (left, right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a % b)),
                _ => Err("modulo requires integers".into()),
            },
            BinOp::Pow => match (left, right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.pow(b as u32))),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a.powf(b))),
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
