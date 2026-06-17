use super::*;

impl<'a> Interpreter<'a> {
    pub(crate) fn eval_block(&mut self, block: &Block) -> Result<Option<Value>, String> {
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
            // Propagate break/continue signals out of the block
            if self.loop_action.is_some() {
                return Ok(None);
            }
        }
        Ok(None)
    }

    pub(crate) fn eval_stmt(&mut self, stmt: &Stmt) -> Result<Option<Value>, String> {
        match stmt {
            Stmt::Let { pat, init, mut_, ref_, ty } => {
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

                // Convert List to Array when type annotation is [T; n]
                let v = match (ty, &v) {
                    (Some(Type::Array(_, size)), Value::List(list)) => {
                        if list.len() != *size {
                            return Err(format!(
                                "array size mismatch: expected [{}; {}], found list of length {}",
                                size, size, list.len()
                            ));
                        }
                        Value::Array(list.clone())
                    }
                    _ => v,
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
            Stmt::Break(e) => {
                let v = match e {
                    Some(e) => Some(self.eval_expr(e)?),
                    None => None,
                };
                self.loop_action = Some(LoopAction::Break(v));
                return Ok(None);
            }
            Stmt::Continue => {
                self.loop_action = Some(LoopAction::Continue);
                return Ok(None);
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
                    match self.loop_action.take() {
                        Some(LoopAction::Break(val)) => {
                            if let Some(v) = val {
                                return Ok(Some(v));
                            }
                            break;
                        }
                        Some(LoopAction::Continue) => {
                            continue;
                        }
                        None => {}
                    }
                }
            }
            Stmt::For { var, iterable, body } => {
                let iter = self.eval_expr(iterable)?;
                let list = match iter {
                    Value::List(l) => l,
                    Value::Range { start, end } => {
                        let mut items = Vec::new();
                        if start <= end {
                            for i in start..end {
                                items.push(Value::Int(i));
                            }
                        }
                        items
                    }
                    other => return Err(format!("cannot iterate over {}", other)),
                };
                for item in list {
                    self.bind(var, item);
                    if let Some(v) = self.eval_block(body)? {
                        return Ok(Some(v));
                    }
                    match self.loop_action.take() {
                        Some(LoopAction::Break(val)) => {
                            if let Some(v) = val {
                                return Ok(Some(v));
                            }
                            break;
                        }
                        Some(LoopAction::Continue) => {
                            continue;
                        }
                        None => {}
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
            Stmt::Unsafe(block) => {
                // Unsafe block: execute body with no restrictions
                // (at runtime, unsafe has no effect — it's a compile-time annotation)
                return self.eval_block(block);
            }
            Stmt::Alloc { kind, body } => {
                // alloc(Kind) block: uses the specified allocator
                return match kind {
                    AllocKind::Arena => {
                        // Same as arena block
                        let arena_id = self.arenas.len();
                        let arena = Arena { id: arena_id, slots: Vec::new() };
                        self.arenas.push(arena);
                        self.arena_depth += 1;
                        self.push_scope();
                        let result = self.eval_block(body);
                        // Check for escape
                        let mut escape_var = None;
                        let outer_count = self.env.len() - 1;
                        for scope in self.env.iter().take(outer_count) {
                            for (name, val) in scope {
                                if contains_arena_ref(val, arena_id) {
                                    escape_var = Some(name.clone());
                                    break;
                                }
                            }
                            if escape_var.is_some() { break; }
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
                        self.arenas.pop();
                        result
                    }
                    AllocKind::Bump => {
                        let arena_id = self.arenas.len();
                        let arena = Arena { id: arena_id, slots: Vec::new() };
                        self.arenas.push(arena);
                        self.arena_depth += 1;
                        self.push_scope();
                        let result = self.eval_block(body);
                        self.arena_depth -= 1;
                        self.pop_scope();
                        self.arenas.pop();
                        result
                    }
                    AllocKind::System => {
                        self.push_scope();
                        let result = self.eval_block(body);
                        self.pop_scope();
                        result
                    }
                };
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
                    Expr::Unary(UnOp::Deref, inner) => {
                        // *r = value: assign through mutable reference
                        let ref_val = self.eval_expr(inner)?;
                        match ref_val {
                            Value::RefMut(rc) => {
                                *rc.0.borrow_mut() = v;
                            }
                            _ => return Err("cannot assign through non-mutable reference".into()),
                        }
                    }
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
                            Value::Record(_, mut fields) => {
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
            Stmt::Desc(_) | Stmt::Requires(_) | Stmt::Ensures(_) | Stmt::Ellipsis | Stmt::MmsBlock { .. } => {}
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
                            super::pool::get_pool().execute(move || {
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
                                    super::pool::get_pool().execute(move || {
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

    pub(crate) fn eval_expr(&mut self, expr: &Expr) -> Result<Value, String> {
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
                    // Try to suggest a similar name
                    let mut candidates: Vec<String> = Vec::new();
                    for scope in self.env.iter().rev() {
                        for var_name in scope.keys() {
                            if levenshtein_distance(name, var_name) <= 2 && name != var_name {
                                candidates.push(var_name.clone());
                            }
                        }
                    }
                    for func_name in self.file.items.iter().filter_map(|item| {
                        if let Item::Func(f) = item { Some(&f.name) } else { None }
                    }) {
                        if levenshtein_distance(name, func_name) <= 2 && name != func_name {
                            candidates.push(func_name.clone());
                        }
                    }
                    candidates.sort();
                    candidates.dedup();
                    if let Some(suggestion) = candidates.first() {
                        Err(format!("undefined variable '{}' — did you mean '{}'?", name, suggestion))
                    } else {
                        Err(format!("undefined variable '{}'", name))
                    }
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
                        // Handle module-qualified function call: Module::func(args)
                        if let Some(qualified) = Self::build_qualified_path(obj, method) {
                            if let Some(f) = self.find_function(&qualified) {
                                return self.call_func(&f, vals);
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
                            _ => Err(format!("cannot call {}: expected a function or closure", Self::type_name(&callee_val))),
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
            Expr::If { cond, then_, else_ } => {
                let c = self.eval_expr(cond)?;
                if is_truthy(&c) {
                    self.push_scope();
                    let r = self.eval_block(then_);
                    self.pop_scope();
                    r.map(|v| v.unwrap_or(Value::Unit))
                } else if let Some(eb) = else_ {
                    self.push_scope();
                    let r = self.eval_block(eb);
                    self.pop_scope();
                    r.map(|v| v.unwrap_or(Value::Unit))
                } else {
                    Ok(Value::Unit)
                }
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
                // Special case: module-qualified access (Module::func or Module::Sub::func)
                // Build qualified path by collecting nested Field(Ident(...), ...) nodes
                if let Some(qualified) = Self::build_qualified_path(obj, field) {
                    if let Some(f) = self.find_function(&qualified) {
                        return Ok(Value::Closure {
                            params: f.params.clone(),
                            ret: f.ret.clone(),
                            body: f.body.clone(),
                            captured: HashMap::new(),
                        });
                    }
                    // Check if it's an enum variant constructor (e.g., Color::Red)
                    if let Expr::Ident(_type_name) = obj.as_ref() {
                        if let Some(&ctor_arity) = self.constructors.get(field.as_str()) {
                            if ctor_arity == 0 {
                                // 0-arity variant: return the variant value directly
                                return Ok(Value::Variant(field.clone(), vec![]));
                            } else {
                                // N-arity variant: return a closure that constructs it
                                let field_clone = field.clone();
                                return Ok(Value::Closure {
                                    params: (0..ctor_arity).map(|i| Param {
                                        name: format!("arg{}", i),
                                        ty: Type::Name("unknown".into(), vec![]),
                                        mut_: false,
                                    }).collect(),
                                    ret: None,
                                    body: vec![Stmt::Return(Some(Expr::Call(
                                        Box::new(Expr::Ident(field_clone)),
                                        (0..ctor_arity).map(|i| Expr::Ident(format!("arg{}", i))).collect(),
                                    )))],
                                    captured: HashMap::new(),
                                });
                            }
                        }
                    }
                }
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
                    Value::Record(_, fields) => {
                        fields
                            .get(field)
                            .cloned()
                            .ok_or_else(|| {
                                let available: Vec<&str> = fields.keys().map(|s| s.as_str()).collect();
                                if available.is_empty() {
                                    format!("field '{}' not found in record (record is empty)", field)
                                } else {
                                    format!("field '{}' not found in record — available fields: {}", field, available.join(", "))
                                }
                            })
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
                            Value::Record(_, fields) => fields.get(field.as_str()).cloned()
                                .ok_or_else(|| format!("field '{}' not found in shared record", field)),
                            _ => Err("field access on non-record shared value".into()),
                        }
                    }
                    Value::LocalShared(rc) => {
                        let inner = rc.0.borrow();
                        match &*inner {
                            Value::Record(_, fields) => fields.get(field.as_str()).cloned()
                                .ok_or_else(|| format!("field '{}' not found in local_shared record", field)),
                            _ => Err("field access on non-record local_shared value".into()),
                        }
                    }
                    _ => Err(format!("cannot access field '{}' on {}", field, Self::type_name(&obj_val))),
                }
            }
            Expr::Record { ty, fields } => {
                let mut map = HashMap::new();
                for f in fields {
                    let v = self.eval_expr(&f.value)?;
                    map.insert(f.name.clone(), v);
                }
                Ok(Value::Record(ty.clone(), map))
            }
            Expr::Index(obj_expr, idx_expr) => {
                let obj = self.eval_expr(obj_expr)?;
                let idx = self.eval_expr(idx_expr)?;
                match (&obj, &idx) {
                    (Value::List(list), Value::Int(i)) => {
                        let len = list.len() as i64;
                        let i = if *i < 0 { len + *i } else { *i };
                        if i < 0 || i >= len {
                            return Err(format!("index out of bounds: index {} is not valid for list of length {}", i, len));
                        }
                        Ok(list[i as usize].clone())
                    }
                    (Value::Array(arr), Value::Int(i)) => {
                        let len = arr.len() as i64;
                        let i = if *i < 0 { len + *i } else { *i };
                        if i < 0 || i >= len {
                            return Err(format!("index out of bounds: index {} is not valid for array of length {}", i, len));
                        }
                        Ok(arr[i as usize].clone())
                    }
                    (Value::Slice { source, start, end }, Value::Int(i)) => {
                        let slice_len = (*end - *start) as i64;
                        let i = if *i < 0 { slice_len + *i } else { *i };
                        if i < 0 || i >= slice_len {
                            return Err(format!("index out of bounds: index {} is not valid for slice of length {}", i, slice_len));
                        }
                        Ok(source[*start + i as usize].clone())
                    }
                    (Value::String(s), Value::Int(i)) => {
                        let len = s.chars().count() as i64;
                        let i = if *i < 0 { len + *i } else { *i };
                        if i < 0 || i >= len {
                            return Err(format!("index out of bounds: index {} is not valid for string of length {}", i, len));
                        }
                        Ok(Value::String(s.chars().nth(i as usize).unwrap().to_string()))
                    }
                    _ => Err(format!("cannot index {} with {}", Self::type_name(&obj), Self::type_name(&idx))),
                }
            }
            Expr::SliceExpr { target, start, end } => {
                let obj = self.eval_expr(target)?;
                let len = match &obj {
                    Value::List(l) => l.len(),
                    Value::Array(a) => a.len(),
                    Value::Slice { source: _, start: s, end: e } => e - s,
                    Value::String(s) => s.len(),
                    _ => return Err("cannot slice non-sequence value".into()),
                };
                let start_idx = match start {
                    Some(e) => {
                        let v = self.eval_expr(e)?;
                        match v {
                            Value::Int(i) => {
                                let i = if i < 0 { len as i64 + i } else { i } as usize;
                                if i > len { return Err("slice start out of bounds".into()); }
                                i
                            }
                            _ => return Err("slice index must be integer".into()),
                        }
                    }
                    None => 0,
                };
                let end_idx = match end {
                    Some(e) => {
                        let v = self.eval_expr(e)?;
                        match v {
                            Value::Int(i) => {
                                let i = if i < 0 { len as i64 + i } else { i } as usize;
                                if i > len { return Err("slice end out of bounds".into()); }
                                i
                            }
                            _ => return Err("slice index must be integer".into()),
                        }
                    }
                    None => len,
                };
                if start_idx > end_idx {
                    return Err("slice start > end".into());
                }
                match obj {
                    Value::List(l) => Ok(Value::Slice { source: l, start: start_idx, end: end_idx }),
                    Value::Array(a) => Ok(Value::Slice { source: a, start: start_idx, end: end_idx }),
                    Value::Slice { source, start: _, end: _ } => {
                        // Re-slice: adjust indices relative to the original source
                        Ok(Value::Slice { source, start: start_idx, end: end_idx })
                    }
                    Value::String(s) => {
                        let chars: Vec<char> = s.chars().collect();
                        let sliced: String = chars[start_idx..end_idx].iter().collect();
                        Ok(Value::String(sliced))
                    }
                    _ => unreachable!(),
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
            Expr::Spawn(expr) => {
                // Spawn evaluates the expression in a new thread and returns a Future
                let (tx, rx) = std::sync::mpsc::channel();
                // Evaluate args and actor reference in current thread
                let spawned = {
                    if let Expr::Call(callee, args) = expr.as_ref() {
                        if let Expr::Field(obj, method) = callee.as_ref() {
                            let obj_val = self.eval_expr(obj)?;
                            let method_name = method.clone();
                            let args_vals: Vec<Value> = args.iter()
                                .map(|a| self.eval_expr(a))
                                .collect::<Result<Vec<_>, _>>()?;
                            match obj_val {
                                Value::Actor(handle) => {
                                    Some((handle, method_name, args_vals))
                                }
                                _ => None,
                            }
                        } else { None }
                    } else { None }
                };

                if let Some((actor_handle, method, args_vals)) = spawned {
                    super::pool::get_pool().execute(move || {
                        let empty_file = File { imports: vec![], items: vec![] };
                        let mut interp = Interpreter::new(&empty_file);
                        let actor_val = Value::Actor(actor_handle);
                        let result = interp.call_method(&actor_val, &method, args_vals);
                        let _ = tx.send(result);
                    });
                    Ok(Value::Future(Arc::new(std::sync::Mutex::new(rx))))
                } else {
                    // For non-actor spawns, evaluate directly
                    self.eval_expr(expr)
                }
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
                            super::pool::get_pool().execute(move || {
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
            Expr::Comptime(block) => {
                // Comptime block: evaluate all statements, return last expression value
                let mut result = Value::Unit;
                let len = block.len();
                for (i, stmt) in block.iter().enumerate() {
                    let is_last = i == len - 1;
                    match stmt {
                        Stmt::Expr(e) if is_last => {
                            result = self.eval_expr(e)?;
                        }
                        Stmt::Expr(e) => {
                            self.eval_expr(e)?;
                        }
                        _ => {
                            if let Some(v) = self.eval_stmt(stmt)? {
                                result = v;
                            }
                        }
                    }
                }
                Ok(result)
            }
            Expr::TypeOf(expr) => {
                let val = self.eval_expr(expr)?;
                let type_name = self.value_type_name(&val);
                Ok(Value::String(type_name))
            }
            Expr::TypeInfo(ty) => {
                let type_name = self.resolve_type_name(ty);
                let info = self.type_info_for(&type_name)?;
                Ok(info)
            }
            Expr::Range { start, end } => {
                let start_val = self.eval_expr(start)?;
                let end_val = self.eval_expr(end)?;
                match (start_val, end_val) {
                    (Value::Int(s), Value::Int(e)) => Ok(Value::Range { start: s, end: e }),
                    _ => Err("range requires integer operands".into()),
                }
            }
        }
    }

    fn eval_unary(&mut self, op: UnOp, e: &Expr) -> Result<Value, String> {
        let v = self.eval_expr(e)?;
        match op {
            UnOp::Neg => match v {
                Value::Int(x) => {
                    crate::safe_arith::checked_neg(x)
                        .ok_or_else(|| format!("integer overflow in negation: -{}", x))
                        .map(Value::Int)
                }
                Value::Float(x) => Ok(Value::Float(-x)),
                _ => Err(format!("cannot negate {}", Self::type_name(&v))),
            },
            UnOp::Not => Ok(Value::Bool(!is_truthy(&v))),
            UnOp::Ref => Ok(Value::Ref(SendRc(Rc::new(RefCell::new(v))))),
            UnOp::RefMut => Ok(Value::RefMut(SendRc(Rc::new(RefCell::new(v))))),
            UnOp::Deref => match v {
                Value::Ref(rc) | Value::RefMut(rc) => Ok(rc.0.borrow().clone()),
                _ => Err(format!("cannot dereference {}", Self::type_name(&v))),
            },
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
                (Value::Int(a), Value::Int(b)) => {
                    crate::safe_arith::checked_add(*a, *b)
                        .ok_or_else(|| format!("integer overflow in addition: {} + {}", a, b))
                        .map(Value::Int)
                }
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
                _ => Err(format!("cannot apply '+' to {} and {}", Self::type_name(&left), Self::type_name(&right))),
            },
            BinOp::Sub => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => {
                    crate::safe_arith::checked_sub(*a, *b)
                        .ok_or_else(|| format!("integer overflow in subtraction: {} - {}", a, b))
                        .map(Value::Int)
                }
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),
                _ => Err(format!("cannot apply '-' to {} and {}", Self::type_name(&left), Self::type_name(&right))),
            },
            BinOp::Mul => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => {
                    crate::safe_arith::checked_mul(*a, *b)
                        .ok_or_else(|| format!("integer overflow in multiplication: {} * {}", a, b))
                        .map(Value::Int)
                }
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
                _ => Err(format!("cannot apply '*' to {} and {}", Self::type_name(&left), Self::type_name(&right))),
            },
            BinOp::Div => match (&left, &right) {
                (Value::Int(_), Value::Int(0)) => Err("division by zero".into()),
                (Value::Float(_), Value::Float(b)) if *b == 0.0 => Err("division by zero".into()),
                (Value::Int(a), Value::Int(b)) => {
                    crate::safe_arith::checked_div(*a, *b)
                        .ok_or_else(|| format!("integer overflow in division: {} / {}", a, b))
                        .map(Value::Int)
                }
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a / b)),
                _ => Err(format!("cannot apply '/' to {} and {}", Self::type_name(&left), Self::type_name(&right))),
            },
            BinOp::Mod => match (&left, &right) {
                (Value::Int(_), Value::Int(0)) => Err("modulo by zero".into()),
                (Value::Int(a), Value::Int(b)) => {
                    crate::safe_arith::checked_rem(*a, *b)
                        .ok_or_else(|| format!("integer overflow in modulo: {} % {}", a, b))
                        .map(Value::Int)
                }
                _ => Err(format!("cannot apply '%' to {} and {}", Self::type_name(&left), Self::type_name(&right))),
            },
            BinOp::Pow => match (&left, &right) {
                (Value::Int(_), Value::Int(b)) if *b < 0 => Err("negative exponent not supported for integers".into()),
                (Value::Int(a), Value::Int(b)) => {
                    crate::safe_arith::checked_pow(*a, *b as u32)
                        .ok_or_else(|| format!("integer overflow in power: {} ^ {}", a, b))
                        .map(Value::Int)
                }
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a.powf(*b))),
                _ => Err(format!("cannot apply '^' to {} and {}", Self::type_name(&left), Self::type_name(&right))),
            },
            BinOp::EqCmp => Ok(Value::Bool(values_equal(&left, &right))),
            BinOp::NeCmp => Ok(Value::Bool(!values_equal(&left, &right))),
            BinOp::Lt => compare_op(left, right, |o| o == std::cmp::Ordering::Less),
            BinOp::Gt => compare_op(left, right, |o| o == std::cmp::Ordering::Greater),
            BinOp::Le => compare_op(left, right, |o| o != std::cmp::Ordering::Greater),
            BinOp::Ge => compare_op(left, right, |o| o != std::cmp::Ordering::Less),
            BinOp::BitAnd => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a & b)),
                _ => Err(format!("cannot apply '&' to {} and {}", Self::type_name(&left), Self::type_name(&right))),
            },
            BinOp::BitOr => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a | b)),
                _ => Err(format!("cannot apply '|' to {} and {}", Self::type_name(&left), Self::type_name(&right))),
            },
            BinOp::BitXor => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a ^ b)),
                _ => Err(format!("cannot apply '^' to {} and {}", Self::type_name(&left), Self::type_name(&right))),
            },
            BinOp::Shl => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a << b)),
                _ => Err(format!("cannot apply '<<' to {} and {}", Self::type_name(&left), Self::type_name(&right))),
            },
            BinOp::Shr => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a >> b)),
                _ => Err(format!("cannot apply '>>' to {} and {}", Self::type_name(&left), Self::type_name(&right))),
            },
            BinOp::Range => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Range { start: *a, end: *b }),
                _ => Err(format!("cannot apply '..' to {} and {}", Self::type_name(&left), Self::type_name(&right))),
            },
            BinOp::Assign => Err("assignment as expression not supported".into()),
            BinOp::And | BinOp::Or => unreachable!(),
        }
    }

    /// Get a human-readable type name for a value (standalone, no interpreter state needed).
    fn type_name(val: &Value) -> &'static str {
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
            Value::Record(_, _) => "record",
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
}

/// Compute Levenshtein edit distance between two strings.
fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a_len = a.len();
    let b_len = b.len();
    if a_len == 0 { return b_len; }
    if b_len == 0 { return a_len; }

    let mut prev = vec![0usize; b_len + 1];
    let mut curr = vec![0usize; b_len + 1];

    for j in 0..=b_len {
        prev[j] = j;
    }

    for (i, ca) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.chars().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1)
                .min(curr[j] + 1)
                .min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[b_len]
}
