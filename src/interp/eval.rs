use super::*;

impl<'a> Interpreter<'a> {
    pub(crate) fn eval_block(&mut self, block: &Block) -> Result<Option<Value>, InterpError> {
        self.push_compensation_scope();
        let result = self.eval_block_inner(block);
        self.pop_compensation_scope(result.is_err() || self.early_return.is_some());
        result
    }

    fn eval_block_inner(&mut self, block: &Block) -> Result<Option<Value>, InterpError> {
        for (i, stmt) in block.iter().enumerate() {
            let is_last = i == block.len() - 1;
            match stmt {
                Stmt::Expr(e) if is_last => {
                    let result = self.eval_expr(e);
                    match result {
                        Ok(Value::Error(msg)) => {
                            return Err(InterpError::new(msg));
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
                            return Err(InterpError::new(msg));
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
            // Propagate break/continue and early return signals out of the block
            if self.loop_action.is_some() || self.early_return.is_some() {
                return Ok(None);
            }
        }
        Ok(None)
    }

    pub(crate) fn eval_stmt(&mut self, stmt: &Stmt) -> Result<Option<Value>, InterpError> {
        match stmt {
            Stmt::Let { pat, init, mut_, ref_, ty } => {
                let v = match init {
                    Some(e) => {
                        let result = self.eval_expr(e);
                        match result {
                            Ok(Value::Error(msg)) => {
                                return Err(InterpError::new(msg));
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
                            return Err(InterpError::new(
                                format!("array size mismatch: expected [{}; {}], found list of length {}", size, size, list.len())
                            ));
                        }
                        Value::Array(list.clone())
                    }
                    // Coerce concrete type to dyn Trait when type annotation is dyn Trait
                    (Some(Type::DynTrait(trait_names)), Value::Record(Some(concrete_type), _)) => {
                        Value::DynTrait {
                            data: Box::new(v.clone()),
                            concrete_type: concrete_type.clone(),
                            trait_names: trait_names.clone(),
                        }
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
                            self.bind_mut(&name, val)?;
                        } else {
                            self.bind(&name, val)?;
                        }
                    }
                } else {
                    return Err(InterpError::new(format!("let pattern did not match value {}", v)));
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
                            return Err(InterpError::new(format!(
                                "arena escape: returning a reference to arena {} that is still active",
                                arena.id
                            )));
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
                    return Err(InterpError::new(msg));
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
                    if self.early_return.is_some() { break; }
                    if let Some(v) = self.eval_block(body)? {
                        return Ok(Some(v));
                    }
                    if self.early_return.is_some() { break; }
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
                    other => return Err(InterpError::new(format!("cannot iterate over {}", other))),
                };
                for item in list {
                    self.bind(var, item)?;
                    if self.early_return.is_some() { break; }
                    self.eval_block(body)?;
                    if self.early_return.is_some() { break; }
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
                return self.eval_arena_block(block);
            }
            Stmt::Unsafe(block) => {
                // Unsafe block: execute body with no restrictions
                // (at runtime, unsafe has no effect — it's a compile-time annotation)
                return self.eval_block(block);
            }
            Stmt::Alloc { kind, body } => {
                // alloc(Kind) block: uses the specified allocator
                return match kind {
                    AllocKind::Arena => self.eval_arena_block(body),
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
                        // *r = value: assign through mutable reference or shared pointer
                        let ref_val = self.eval_expr(inner)?;
                        match ref_val {
                            Value::RefMut(rc) => {
                                *rc.write().map_err(|e| InterpError::new(format!("write lock failed: {}", e)))? = v;
                            }
                            Value::Shared(arc) => {
                                *arc.write().map_err(|e| InterpError::new(format!("shared write lock failed: {}", e)))? = v;
                            }
                            Value::LocalShared(rc) => {
                                *rc.borrow_mut() = v;
                            }
                            _ => return Err(InterpError::new(format!("cannot assign through non-mutable reference (type: {})", type_name(&ref_val)))),
                        }
                    }
                    Expr::Field(obj, field) => {
                        // Special case: if assigning to self.field, update actor directly
                        if let Expr::Ident(name) = obj.as_ref() {
                            if name == "self" {
                                // Find the actor handle in scope and update its field
                                if let Some(Value::Actor(handle)) = self.lookup("self") {
                                    handle.inner.write().map_err(|e| InterpError::new(format!("actor lock failed: {}", e)))?.fields.insert(field.clone(), v);
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
                                    return Err(InterpError::new(format!("field '{}' not found in record", field)));
                                }
                            }
                            Value::Actor(handle) => {
                                handle.inner.write().map_err(|e| InterpError::new(format!("actor lock failed: {}", e)))?.fields.insert(field.clone(), v);
                            }
                            Value::Shared(arc) => {
                                let mut inner = arc.write().map_err(|e| InterpError::new(format!("shared write lock failed: {}", e)))?;
                                match &mut *inner {
                                    Value::Record(_, fields) => {
                                        if fields.contains_key(field.as_str()) {
                                            if let std::collections::hash_map::Entry::Occupied(mut e) = fields.entry(field.clone()) {
                                                e.insert(v);
                                            }
                                        } else {
                                            return Err(InterpError::new(format!("field '{}' not found in shared record", field)));
                                        }
                                    }
                                    _ => return Err(InterpError::new(format!("cannot assign to field of non-record shared value (type: {})", type_name(&inner)))),
                                }
                            }
                            Value::LocalShared(rc) => {
                                let mut inner = rc.borrow_mut();
                                match &mut *inner {
                                    Value::Record(_, fields) => {
                                        if fields.contains_key(field.as_str()) {
                                            if let std::collections::hash_map::Entry::Occupied(mut e) = fields.entry(field.clone()) {
                                                e.insert(v);
                                            }
                                        } else {
                                            return Err(InterpError::new(format!("field '{}' not found in local_shared record", field)));
                                        }
                                    }
                                    _ => return Err(InterpError::new(format!("cannot assign to field of non-record local_shared value (type: {})", type_name(&inner)))),
                                }
                            }
                            _ => return Err(InterpError::new(format!("cannot assign to field of non-record/non-actor value (type: {})", type_name(&obj_val)))),
                        }
                    }
                    Expr::Index(obj, idx) => {
                        // list[i] = val: evaluate list, index, and set element
                        let list_val = self.eval_expr(obj)?;
                        let idx_val = self.eval_expr(idx)?;
                        let index = match idx_val {
                            Value::Int(i) => i as usize,
                            _ => return Err(InterpError::new(format!("list index must be an integer, got {}", type_name(&idx_val)))),
                        };
                        match list_val {
                            Value::List(mut items) => {
                                if index >= items.len() {
                                    return Err(InterpError::new(format!("list index {} out of bounds (len {})", index, items.len())));
                                }
                                items[index] = v;
                                // Update the binding
                                if let Expr::Ident(name) = obj.as_ref() {
                                    let mut found = false;
                                    for scope in self.env.iter_mut().rev() {
                                        if scope.contains_key(name) {
                                            scope.insert(name.clone(), Value::List(items));
                                            found = true;
                                            break;
                                        }
                                    }
                                    if !found {
                                        return Err(InterpError::new(format!("variable '{}' not found", name)));
                                    }
                                }
                            }
                            _ => return Err(InterpError::new(format!("cannot index-assign to non-list value (type: {})", type_name(&list_val)))),
                        }
                    }
                    _ => return Err(InterpError::new("assignment target must be a variable")),
                }
            }
            Stmt::Desc(..) | Stmt::Requires(_, _) | Stmt::Ensures(_, _) | Stmt::Ellipsis | Stmt::MmsBlock { .. } => {}
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
                    SharedKind::LocalShared => Value::LocalShared(LocalSharedInner::new(v)),
                    SharedKind::Weak => {
                        // Auto-detect: if init is Shared → WeakShared, if LocalShared → WeakLocal
                        match v {
                            Value::Shared(arc) => Value::WeakShared(Arc::downgrade(&arc)),
                            Value::LocalShared(rc) => Value::WeakLocal(rc.downgrade()),
                            _ => return Err(InterpError::new(format!("weak requires a shared or local_shared value, got {}", v))),
                        }
                    }
                    SharedKind::WeakLocal => {
                        match v {
                            Value::LocalShared(rc) => Value::WeakLocal(rc.downgrade()),
                            _ => return Err(InterpError::new(format!("weak_local requires a local_shared value, got {}", v))),
                        }
                    }
                };
                self.bind(name, shared_val)?;
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
                // Runtime assertion: scan current scope for LocalShared values
                for scope in &self.env {
                    for val in scope.values() {
                        if crate::interp::value::contains_local_shared(val) {
                            return Err(InterpError::new("parasteps: local_shared values cannot cross thread boundary"));
                        }
                    }
                }
                let mut last_value = None;
                type SpawnReceiver = std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<Result<Value, InterpError>>>>;
                let mut futures: Vec<SpawnReceiver> = Vec::new();
                let mut spawn_bindings: HashMap<String, SpawnReceiver> = HashMap::new();

                for stmt in block {
                    match stmt {
                        Stmt::Expr(Expr::Spawn(expr)) => {
                            // Runtime assertion: no LocalShared values cross thread boundaries
                            if crate::interp::value::contains_local_shared(&self.eval_expr(expr)?) {
                                return Err(InterpError::new("parasteps: local_shared values cannot cross thread boundary"));
                            }
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
                                    self.bind(&name, val)?;
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
                    let rx = rx.lock().map_err(|e| InterpError::new(format!("await failed: {}", e)))?;
                    if let Ok(Err(e)) = rx.recv() {
                        return Err(e);
                    }
                }

                // If last_value is a Future, await it
                if let Some(Value::Future(rx)) = last_value {
                    let rx = rx.lock().map_err(|e| InterpError::new(format!("await failed: {}", e)))?;
                    last_value = Some(rx.recv().map_err(|e| InterpError::new(format!("await failed: {}", e)))??);
                }

                return Ok(last_value);
            }
        }
        Ok(None)
    }

    pub(crate) fn eval_expr(&mut self, expr: &Expr) -> Result<Value, InterpError> {
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
                    Err(InterpError::new(format!("use of moved value '{}'", name)))
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
                        return Err(InterpError::new(format!("newtype '{}' requires exactly one argument", name)));
                    }
                        Ok(Value::Variant(name.clone(), vec![]))
                    } else {
                        Err(InterpError::new(format!("constructor '{}' requires {} arguments", name, arity)))
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
                        Err(InterpError::new(format!("undefined variable '{}' — did you mean '{}'?", name, suggestion)))
                    } else {
                        Err(InterpError::new(format!("undefined variable '{}'", name)))
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
                                    return self.spawn_actor(type_name);
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
                            Value::Closure { params, ret: _, body, captured } =>
                                self.apply_closure_inner(&params, &body, &captured, vals),
                            _ => Err(InterpError::new(format!("cannot call {}: expected a function or closure", type_name(&callee_val)))),
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
            Expr::TupleIndex(obj, idx) => {
                let obj_val = self.eval_expr(obj)?;
                match obj_val {
                    Value::Tuple(items) => {
                        if *idx < items.len() {
                            Ok(items[*idx].clone())
                        } else {
                            Err(InterpError::new(format!("tuple index {} out of bounds (len {})", idx, items.len())))
                        }
                    }
                    _ => Err(InterpError::new(format!("cannot index non-tuple value with .{}", idx))),
                }
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
                    _ => return Err(InterpError::new(format!("comprehension requires a list, got {}", type_name(&iter_val)))),
                };
                let mut result = Vec::new();
                for item in items {
                    self.push_scope();
                    self.bind(var, item.clone())?;
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
                            self.bind(&name, v)?;
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
                Err(InterpError::new("non-exhaustive match"))
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
                // Special case: if accessing field on "self" identifier, look up field directly
                if let Expr::Ident(name) = obj.as_ref() {
                    if name == "self" {
                        // Look up self from scope
                        if let Some(Value::Actor(handle)) = self.lookup("self") {
                            let actor = handle.inner.read().map_err(|e| InterpError::new(format!("actor lock failed: {}", e)))?;
                            if let Some(value) = actor.fields.get(field.as_str()) {
                                return Ok(value.clone());
                            }
                            return Err(InterpError::new(format!("actor field '{}' not found", field)));
                        }
                        // For non-actor self values (records, etc.), fall through to normal field access
                    }
                }
                let obj_val = self.eval_expr(obj)?;
                match obj_val {
                    Value::Record(_, fields) => {
                        fields
                            .get(field)
                            .cloned()
                            .ok_or_else(|| InterpError::new({
                                let available: Vec<&str> = fields.keys().map(|s| s.as_str()).collect();
                                if available.is_empty() {
                                    format!("field '{}' not found in record (record is empty)", field)
                                } else {
                                    format!("field '{}' not found in record — available fields: {}", field, available.join(", "))
                                }
                            }))
                    }
                    Value::Actor(handle) => {
                        // Actor field access using read lock
                        let actor = handle.inner.read().map_err(|e| InterpError::new(format!("actor lock failed: {}", e)))?;
                        actor.fields.get(field.as_str())
                            .cloned()
                            .ok_or_else(|| InterpError::new(format!("actor field '{}' not found", field)))
                    }
                    Value::Shared(arc) => {
                        let inner = arc.read().map_err(|e| InterpError::new(format!("shared read lock failed: {}", e)))?;
                        match &*inner {
                            Value::Record(_, fields) => fields.get(field.as_str()).cloned()
                                .ok_or_else(|| InterpError::new(format!("field '{}' not found in shared record", field))),
                            _ => Err(InterpError::new("field access on non-record shared value")),
                        }
                    }
                    Value::LocalShared(rc) => {
                        let inner = rc.borrow();
                        match &*inner {
                            Value::Record(_, fields) => fields.get(field.as_str()).cloned()
                                .ok_or_else(|| InterpError::new(format!("field '{}' not found in local_shared record", field))),
                            _ => Err(InterpError::new("field access on non-record local_shared value")),
                        }
                    }
                    _ => Err(InterpError::new(format!("cannot access field '{}' on {}", field, type_name(&obj_val)))),
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
                            return Err(InterpError::new(format!("index out of bounds: index {} is not valid for list of length {}", i, len)));
                        }
                        Ok(list[i as usize].clone())
                    }
                    (Value::Array(arr), Value::Int(i)) => {
                        let len = arr.len() as i64;
                        let i = if *i < 0 { len + *i } else { *i };
                        if i < 0 || i >= len {
                            return Err(InterpError::new(format!("index out of bounds: index {} is not valid for array of length {}", i, len)));
                        }
                        Ok(arr[i as usize].clone())
                    }
                    (Value::Slice { source, start, end }, Value::Int(i)) => {
                        let slice_len = (*end - *start) as i64;
                        let i = if *i < 0 { slice_len + *i } else { *i };
                        if i < 0 || i >= slice_len {
                            return Err(InterpError::new(format!("index out of bounds: index {} is not valid for slice of length {}", i, slice_len)));
                        }
                        Ok(source[*start + i as usize].clone())
                    }
                    (Value::String(s), Value::Int(i)) => {
                        let len = s.chars().count() as i64;
                        let i = if *i < 0 { len + *i } else { *i };
                        if i < 0 || i >= len {
                            return Err(InterpError::new(format!("index out of bounds: index {} is not valid for string of length {}", i, len)));
                        }
                        let ch = s.chars().nth(i as usize).ok_or_else(|| InterpError::new(format!("index out of bounds: index {} is not valid for string of length {}", i, len)))?;
                        Ok(Value::String(ch.to_string()))
                    }
                    _ => Err(InterpError::new(format!("cannot index {} with {}", type_name(&obj), type_name(&idx)))),
                }
            }
            Expr::SliceExpr { target, start, end } => {
                let obj = self.eval_expr(target)?;
                let len = match &obj {
                    Value::List(l) => l.len(),
                    Value::Array(a) => a.len(),
                    Value::Slice { source: _, start: s, end: e } => e - s,
                    Value::String(s) => s.len(),
                    _ => return Err(InterpError::new("cannot slice non-sequence value")),
                };
                let start_idx = match start {
                    Some(e) => {
                        let v = self.eval_expr(e)?;
                        match v {
                            Value::Int(i) => {
                                let i = if i < 0 { len as i64 + i } else { i } as usize;
                                if i > len { return Err(InterpError::new("slice start out of bounds")); }
                                i
                            }
                            _ => return Err(InterpError::new("slice index must be integer")),
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
                                if i > len { return Err(InterpError::new("slice end out of bounds")); }
                                i
                            }
                            _ => return Err(InterpError::new("slice index must be integer")),
                        }
                    }
                    None => len,
                };
                if start_idx > end_idx {
                    return Err(InterpError::new("slice start > end"));
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
                    other => return Err(InterpError::new(format!("unexpected expression type in await: {}", other))),
                }
            }
            Expr::Try(expr) => {
                let v = self.eval_expr(expr)?;
                match v {
                    Value::Variant(name, vals) => {
                        // Check if this is a known failure variant
                        let is_failure = self.failure_variants.get(&name).copied().unwrap_or(false);
                        if is_failure {
                            // Set early_return so that call_func returns this value,
                            // eval_block triggers compensations, and match can catch it
                            self.early_return = Some(Value::Variant(name, vals));
                            Ok(Value::Unit)
                        } else {
                            // Treat as success variant - return inner value
                            Ok(vals.into_iter().next().unwrap_or(Value::Unit))
                        }
                    }
                    Value::Error(msg) => {
                        // ? on an already-propagated error: re-propagate
                        self.early_return = Some(Value::Error(msg));
                        Ok(Value::Unit)
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
                    let spawned_file = self.file.clone();
                    super::pool::get_pool().execute(move || {
                        let mut interp = Interpreter::new(&spawned_file);
                        let actor_val = Value::Actor(actor_handle);
                        let result = interp.call_method(&actor_val, &method, args_vals);
                        let _ = tx.send(result);
                    });
                    Ok(Value::Future(Arc::new(std::sync::Mutex::new(rx))))
                } else {
                    // Non-actor `spawn expr` — evaluate directly.
                    // Type checker marks this as Future<T>, but at runtime the
                    // value is unwrapped immediately (no threading). This is a
                    // known gap between compile-time and runtime types (#6).
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
                                other => return Err(InterpError::new(format!("unexpected expression type in await: {}", other))),
                            };
                            let spawned_file = self.file.clone();
                            super::pool::get_pool().execute(move || {
                                let mut interp = Interpreter::new(&spawned_file);
                                let actor_val = Value::Actor(actor_arc);
                                let result = interp.call_method(&actor_val, &method, args_clone);
                                let _ = tx.send(result);
                            });
                            // Wait for the result
                            let result = rx.recv().map_err(|e| InterpError::new(format!("await failed: {}", e)))?;
                            return result;
                        }
                    }
                }
                // Default: evaluate and if it's a Future, wait for it
                let v = self.eval_expr(expr)?;
                match v {
                    Value::Future(rx) => {
                    let rx = rx.lock().map_err(|e| InterpError::new(format!("await failed: {}", e)))?;
                        rx.recv().map_err(|e| InterpError::new(format!("await failed: {}", e)))?
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
                    .ok_or_else(|| InterpError::new(format!("undefined function '{}'", name)))?;
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
                    _ => Err(InterpError::new("range requires integer operands")),
                }
            }
        }
    }

    fn eval_unary(&mut self, op: UnOp, e: &Expr) -> Result<Value, InterpError> {
        let v = self.eval_expr(e)?;
        match op {
            UnOp::Neg => match v {
                Value::Int(x) => {
                    crate::safe_arith::checked_neg(x)
                        .ok_or_else(|| InterpError::new(format!("integer overflow in negation: -{}", x)))
                        .map(Value::Int)
                }
                Value::Float(x) => { let r = -x; if r.is_nan() { Err(InterpError::new(format!("NaN from negation of {}", x))) } else { Ok(Value::Float(r)) } },
                _ => Err(InterpError::new(format!("cannot negate {}", type_name(&v)))),
            },
            UnOp::Not => Ok(Value::Bool(!is_truthy(&v))),
            UnOp::Ref => Ok(Value::Ref(Arc::new(RwLock::new(v)))),
            UnOp::RefMut => Ok(Value::RefMut(Arc::new(RwLock::new(v)))),
            UnOp::Deref => match v {
                Value::Ref(rc) | Value::RefMut(rc) => Ok(rc.read().map_err(|e| InterpError::new(format!("read lock failed: {}", e)))?.clone()),
                Value::Shared(arc) => Ok(arc.read().map_err(|e| InterpError::new(format!("shared read lock failed: {}", e)))?.clone()),
                Value::LocalShared(rc) => Ok(rc.borrow().clone()),
                _ => Err(InterpError::new(format!("cannot dereference {}", type_name(&v)))),
            },
        }
    }

    fn eval_binary(&mut self, op: BinOp, l: &Expr, r: &Expr) -> Result<Value, InterpError> {
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
                        .ok_or_else(|| InterpError::new(format!("integer overflow in addition: {} + {}", a, b)))
                        .map(Value::Int)
                }
                (Value::Float(a), Value::Float(b)) => { let r = a + b; if r.is_nan() { Err(InterpError::new(format!("NaN from {} + {}", a, b))) } else { Ok(Value::Float(r)) } },
                _ => Err(InterpError::new(format!("cannot apply '+' to {} and {}", type_name(&left), type_name(&right)))),
            },
            BinOp::Sub => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => {
                    crate::safe_arith::checked_sub(*a, *b)
                        .ok_or_else(|| InterpError::new(format!("integer overflow in subtraction: {} - {}", a, b)))
                        .map(Value::Int)
                }
                (Value::Float(a), Value::Float(b)) => { let r = a - b; if r.is_nan() { Err(InterpError::new(format!("NaN from {} - {}", a, b))) } else { Ok(Value::Float(r)) } },
                _ => Err(InterpError::new(format!("cannot apply '-' to {} and {}", type_name(&left), type_name(&right)))),
            },
            BinOp::Mul => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => {
                    crate::safe_arith::checked_mul(*a, *b)
                        .ok_or_else(|| InterpError::new(format!("integer overflow in multiplication: {} * {}", a, b)))
                        .map(Value::Int)
                }
                (Value::Float(a), Value::Float(b)) => { let r = a * b; if r.is_nan() { Err(InterpError::new(format!("NaN from {} * {}", a, b))) } else { Ok(Value::Float(r)) } },
                _ => Err(InterpError::new(format!("cannot apply '*' to {} and {}", type_name(&left), type_name(&right)))),
            },
            BinOp::Div => match (&left, &right) {
                (Value::Int(_), Value::Int(0)) => Err(InterpError::new("division by zero")),
                (Value::Float(a), Value::Float(b)) => {
                    if *b == 0.0 { return Err(InterpError::new("division by zero")); }
                    let r = a / b;
                    if r.is_nan() { Err(InterpError::new(format!("NaN from {} / {}", a, b))) }
                    else if r.is_infinite() { Err(InterpError::new(format!("infinity from {} / {}", a, b))) }
                    else { Ok(Value::Float(r)) }
                }
                (Value::Int(a), Value::Int(b)) => {
                    crate::safe_arith::checked_div(*a, *b)
                        .ok_or_else(|| InterpError::new(format!("integer overflow in division: {} / {}", a, b)))
                        .map(Value::Int)
                }
                _ => Err(InterpError::new(format!("cannot apply '/' to {} and {}", type_name(&left), type_name(&right)))),
            },
            BinOp::Mod => match (&left, &right) {
                (Value::Int(_), Value::Int(0)) => Err(InterpError::new("modulo by zero")),
                (Value::Int(a), Value::Int(b)) => {
                    crate::safe_arith::checked_rem(*a, *b)
                        .ok_or_else(|| InterpError::new(format!("integer overflow in modulo: {} % {}", a, b)))
                        .map(Value::Int)
                }
                _ => Err(InterpError::new(format!("cannot apply '%' to {} and {}", type_name(&left), type_name(&right)))),
            },
            BinOp::Pow => match (&left, &right) {
                (Value::Int(_), Value::Int(b)) if *b < 0 => Err(InterpError::new("negative exponent not supported for integers")),
                (Value::Int(a), Value::Int(b)) => {
                    crate::safe_arith::checked_pow(*a, *b as u32)
                        .ok_or_else(|| InterpError::new(format!("integer overflow in power: {} ^ {}", a, b)))
                        .map(Value::Int)
                }
                (Value::Float(a), Value::Float(b)) => { let r = a.powf(*b); if r.is_nan() { Err(InterpError::new(format!("NaN from pow({}, {})", a, b))) } else { Ok(Value::Float(r)) } },
                _ => Err(InterpError::new(format!("cannot apply '^' to {} and {}", type_name(&left), type_name(&right)))),
            },
            BinOp::EqCmp => Ok(Value::Bool(values_equal(&left, &right))),
            BinOp::NeCmp => Ok(Value::Bool(!values_equal(&left, &right))),
            BinOp::Lt => compare_op(left, right, |o| o == std::cmp::Ordering::Less),
            BinOp::Gt => compare_op(left, right, |o| o == std::cmp::Ordering::Greater),
            BinOp::Le => compare_op(left, right, |o| o != std::cmp::Ordering::Greater),
            BinOp::Ge => compare_op(left, right, |o| o != std::cmp::Ordering::Less),
            BinOp::BitAnd => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a & b)),
                _ => Err(InterpError::new(format!("cannot apply '&' to {} and {}", type_name(&left), type_name(&right)))),
            },
            BinOp::BitOr => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a | b)),
                _ => Err(InterpError::new(format!("cannot apply '|' to {} and {}", type_name(&left), type_name(&right)))),
            },
            BinOp::BitXor => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a ^ b)),
                _ => Err(InterpError::new(format!("cannot apply '^' to {} and {}", type_name(&left), type_name(&right)))),
            },
            BinOp::Shl => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => crate::safe_arith::checked_shl(*a, *b as u32)
                    .ok_or_else(|| InterpError::new(format!("shift left overflow: {} << {}", a, b)))
                    .map(Value::Int),
                _ => Err(InterpError::new(format!("cannot apply '<<' to {} and {}", type_name(&left), type_name(&right)))),
            },
            BinOp::Shr => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => crate::safe_arith::checked_shr(*a, *b as u32)
                    .ok_or_else(|| InterpError::new(format!("shift right overflow: {} >> {}", a, b)))
                    .map(Value::Int),
                _ => Err(InterpError::new(format!("cannot apply '>>' to {} and {}", type_name(&left), type_name(&right)))),
            },
            BinOp::Range => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Range { start: *a, end: *b }),
                _ => Err(InterpError::new(format!("cannot apply '..' to {} and {}", type_name(&left), type_name(&right)))),
            },
            BinOp::Assign => Err(InterpError::new("assignment as expression not supported")),
            BinOp::And | BinOp::Or => Err(InterpError::new("logical and/or not supported in expression context")),
        }
    }

    /// Evaluate an arena block: create arena, push scope, eval block, check escape.
    /// Shared by Stmt::Arena and Stmt::Alloc { kind: AllocKind::Arena }.
    fn eval_arena_block(&mut self, block: &Block) -> Result<Option<Value>, InterpError> {
        let arena_id = self.arenas.len();
        self.arenas.push(Arena { id: arena_id, slots: Vec::new() });
        self.arena_depth += 1;
        self.push_scope();
        let result = self.eval_block(block);

        // Check for escape in outer scopes
        let outer_count = self.env.len() - 1;
        let mut escape_var = None;
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
            return Err(InterpError::new(
                format!("arena escape: variable '{}' holds a reference to arena {} that is about to be freed",
                    name, arena_id
                )));
        }

        // Check if the result itself contains an ArenaRef
        if let Ok(Some(ref v)) = result {
            if contains_arena_ref(v, arena_id) {
                self.arena_depth -= 1;
                self.pop_scope();
                self.arenas.pop();
                return Err(InterpError::new(
                    format!("arena escape: returning a reference to arena {} that is about to be freed",
                        arena_id
                    )));
            }
        }

        self.arena_depth -= 1;
        self.pop_scope();
        self.arenas.pop();
        result
    }
}

/// Compute Levenshtein edit distance between two strings.
#[allow(clippy::needless_range_loop)]
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
