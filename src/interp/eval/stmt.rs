use super::super::*;

impl<'a> Interpreter<'a> {
    pub(in crate::interp) fn eval_let(
        &mut self,
        pat: &Pattern,
        init: &Option<Expr>,
        mut_: bool,
        ref_: bool,
        ty: &Option<Type>,
    ) -> Result<(), InterpError> {
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
        let final_value = if ref_ && self.arena_depth > 0 {
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
                if mut_ {
                    self.bind_mut(&name, val)?;
                } else {
                    self.bind(&name, val)?;
                }
            }
        } else {
            return Err(InterpError::new(format!("let pattern did not match value {}", v)));
        }
        Ok(())
    }

    pub(in crate::interp) fn eval_return(&mut self, e: &Option<Expr>) -> Result<Option<Value>, InterpError> {
        let v = match e {
            Some(e) => self.eval_expr(e)?,
            None => Value::Unit,
        };
        // Check if returning an ArenaRef from an active arena
        if self.arena_depth > 0 {
            for arena in &self.arenas {
                if contains_arena_ref(&v, arena.id) {
                    return Err(InterpError::arena_escape(format!(
                        "arena escape: returning a reference to arena {} that is still active",
                        arena.id
                    )));
                }
            }
        }
        Ok(Some(v))
    }

    pub(in crate::interp) fn eval_break(&mut self, e: &Option<Expr>) -> Result<Option<Value>, InterpError> {
        let v = match e {
            Some(e) => Some(self.eval_expr(e)?),
            None => None,
        };
        self.loop_action = Some(LoopAction::Break(v));
        Ok(None)
    }

    pub(in crate::interp) fn eval_continue(&mut self) -> Result<Option<Value>, InterpError> {
        self.loop_action = Some(LoopAction::Continue);
        Ok(None)
    }

    pub(in crate::interp) fn eval_if_stmt(&mut self, cond: &Expr, then_: &Block, else_: &Option<Block>) -> Result<Option<Value>, InterpError> {
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
        Ok(None)
    }

    pub(in crate::interp) fn eval_while(&mut self, cond: &Expr, body: &Block) -> Result<Option<Value>, InterpError> {
        while is_truthy(&self.eval_expr(cond)?) {
            if self.early_return.is_some() { break; }
            // Check invariants at each iteration start
            self.check_invariants(body)?;
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
        Ok(None)
    }

    fn check_invariants(&mut self, block: &Block) -> Result<(), InterpError> {
        for stmt in block {
            if let Stmt::Invariant(expr, _) = stmt {
                let val = self.eval_expr(expr)?;
                if !is_truthy(&val) {
                    return Err(InterpError::new(
                        format!("invariant violated: {:?}", expr)
                    ));
                }
            }
        }
        Ok(())
    }

    pub(in crate::interp) fn eval_loop(&mut self, body: &Block) -> Result<Option<Value>, InterpError> {
        loop {
            if self.early_return.is_some() { break; }
            self.check_invariants(body)?;
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
        Ok(None)
    }

    pub(in crate::interp) fn eval_for(&mut self, var: &str, iterable: &Expr, body: &Block) -> Result<Option<Value>, InterpError> {
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
        Ok(None)
    }

    pub(in crate::interp) fn eval_arena_block(&mut self, block: &Block) -> Result<Option<Value>, InterpError> {
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
            return Err(InterpError::arena_escape(
                format!("arena escape: variable '{}' holds a reference to arena {} that is about to be freed",
                    name, arena_id
                )));
        }

        // If the result is a direct ArenaRef into the same arena, extract the value
        if let Ok(Some(Value::ArenaRef(id, slot))) = &result {
            if *id == arena_id {
                if let Some(Arena { slots, .. }) = self.arenas.get(*id) {
                    if let Some(val) = slots.get(*slot) {
                        let extracted = val.clone();
                        self.arena_depth -= 1;
                        self.pop_scope();
                        self.arenas.pop();
                        return Ok(Some(extracted));
                    }
                }
            }
        }

        // Check if the result itself contains an ArenaRef in a deeper structure
        if let Ok(Some(ref v)) = result {
            if contains_arena_ref(v, arena_id) {
                self.arena_depth -= 1;
                self.pop_scope();
                self.arenas.pop();
                return Err(InterpError::arena_escape(
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

    pub(in crate::interp) fn eval_alloc(&mut self, kind: &AllocKind, body: &Block) -> Result<Option<Value>, InterpError> {
        match kind {
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
        }
    }

    pub(in crate::interp) fn eval_assign(&mut self, target: &Expr, value: &Expr) -> Result<Option<Value>, InterpError> {
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
                        *rc.write().map_err(|e| InterpError::lock_error(format!("write lock failed: {}", e)))? = v;
                    }
                    Value::Shared(arc) => {
                        *arc.write().map_err(|e| InterpError::lock_error(format!("shared write lock failed: {}", e)))? = v;
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
                            handle.inner.write().map_err(|e| InterpError::lock_error(format!("actor lock failed: {}", e)))?.fields.insert(field.clone(), v);
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
                            return Err(InterpError::field_not_found(format!("field '{}' not found in record", field)));
                        }
                    }
                    Value::Actor(handle) => {
                        handle.inner.write().map_err(|e| InterpError::lock_error(format!("actor lock failed: {}", e)))?.fields.insert(field.clone(), v);
                    }
                    Value::Shared(arc) => {
                        let mut inner = arc.write().map_err(|e| InterpError::lock_error(format!("shared write lock failed: {}", e)))?;
                        match &mut *inner {
                            Value::Record(_, fields) => {
                                if fields.contains_key(field.as_str()) {
                                    if let std::collections::hash_map::Entry::Occupied(mut e) = fields.entry(field.clone()) {
                                        e.insert(v);
                                    }
                                } else {
                                    return Err(InterpError::field_not_found(format!("field '{}' not found in shared record", field)));
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
                                    return Err(InterpError::field_not_found(format!("field '{}' not found in local_shared record", field)));
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
        Ok(None)
    }

    pub(in crate::interp) fn eval_shared_let(&mut self, kind: &SharedKind, name: &str, init: &Expr) -> Result<(), InterpError> {
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
        Ok(())
    }

    pub(in crate::interp) fn eval_on_failure(&mut self, block: &Block) -> Result<(), InterpError> {
        // Register compensation action to the current scope level
        // Will be executed in LIFO order if error propagates
        if let Some(current_scope) = self.compensation_stack.last_mut() {
            current_scope.push(block.clone());
        }
        Ok(())
    }

    pub(in crate::interp) fn eval_parasteps(&mut self, block: &Block) -> Result<Option<Value>, InterpError> {
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
        type SpawnFuture = std::sync::Arc<std::sync::Mutex<crate::interp::value::PollFuture>>;
        let mut futures: Vec<SpawnFuture> = Vec::new();
        let mut spawn_bindings: HashMap<String, SpawnFuture> = HashMap::new();

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
                    super::super::pool::get_pool().execute(move || {
                        let mut interp = Interpreter::new(&file);
                        let result = interp.eval_expr(&expr);
                        let _ = tx.send(result);
                    });
                    futures.push(std::sync::Arc::new(std::sync::Mutex::new(
                        crate::interp::value::PollFuture::Pending(rx)
                    )));
                }
                Stmt::Let { pat, init, .. } => {
                    // Handle let bindings that might contain spawn
                    let v = match init {
                        Some(Expr::Spawn(expr)) => {
                            // Create a future for concurrent execution
                            let (tx, rx) = std::sync::mpsc::channel();
                            let expr = expr.clone();
                            let file = self.file.clone();
                            super::super::pool::get_pool().execute(move || {
                                let mut interp = Interpreter::new(&file);
                                let result = interp.eval_expr(&expr);
                                let _ = tx.send(result);
                            });
                            let fut_arc = std::sync::Arc::new(std::sync::Mutex::new(
                                crate::interp::value::PollFuture::Pending(rx)
                            ));
                            // Store the future for later await
                            if let Pattern::Variable(name) = pat {
                                spawn_bindings.insert(name.clone(), fut_arc.clone());
                            }
                            Value::Future(fut_arc)
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
        for fut in futures {
            let mut fut = fut.lock()
                .map_err(|e| InterpError::new(format!("await lock failed: {}", e)))?;
            crate::interp::value::poll_deferred(&mut fut);
            match &mut *fut {
                crate::interp::value::PollFuture::Pending(rx) => {
                    if let Ok(Err(e)) = rx.recv() {
                        return Err(e);
                    }
                }
                crate::interp::value::PollFuture::Ready(result) => {
                    if let Err(e) = result {
                        return Err(InterpError::new(format!("parasteps error: {}", e)));
                    }
                }
                crate::interp::value::PollFuture::Deferred { .. } => {
                    return Err(InterpError::new("future still deferred in parasteps"));
                }
            }
        }

        // If last_value is a Future, await it
        if let Some(Value::Future(fut)) = last_value {
            let mut fut = fut.lock()
                .map_err(|e| InterpError::new(format!("await lock failed: {}", e)))?;
            crate::interp::value::poll_deferred(&mut fut);
            match &mut *fut {
                crate::interp::value::PollFuture::Pending(rx) => {
                    last_value = Some(rx.recv()
                        .map_err(|e| InterpError::new(format!("await failed: {}", e)))??);
                }
                crate::interp::value::PollFuture::Ready(result) => {
                    last_value = Some(std::mem::replace(result,
                        Err(InterpError::new("future already consumed")))?);
                }
                crate::interp::value::PollFuture::Deferred { .. } => {
                    return Err(InterpError::new("future still deferred in parasteps"));
                }
            }
        }

        Ok(last_value)
    }
}
