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
        let v = match (ty.as_ref().map(Type::unlocated), &v) {
            (Some(Type::Array(_, size)), Value::List(list)) => {
                if list.len() != *size {
                    return Err(InterpError::new(format!(
                        "array size mismatch: expected [{}; {}], found list of length {}",
                        size,
                        size,
                        list.len()
                    )));
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
        if let Some(init) = init {
            if let Expr::Ident(name) = init.unlocated() {
                if !is_copy(&v) && !self.is_moved(name) {
                    self.mark_moved(name);
                }
            }
        }

        // Handle `let ref` in arena: create ArenaRef for arena-allocated values.
        // The value is always stored in the arena; ArenaRef is needed so that
        // the arena escape check can detect when a reference to arena data
        // escapes the arena scope (via return or assignment to outer scope).
        let final_value = if ref_ && self.arena_depth > 0 {
            let arena_id = self.arenas.len() - 1;
            let slot_index = self.arenas[arena_id].slots.len();
            self.arenas[arena_id].slots.push(v.clone());
            let gen = self.arenas[arena_id].generation;
            Value::ArenaRef(arena_id, slot_index, gen)
        } else {
            v.clone()
        };

        if let Some(bindings) = self.match_pattern_bind(pat, &final_value) {
            for (name, val) in bindings {
                if mut_ {
                    self.bind_mut(&name, val)?;
                } else {
                    self.bind(&name, val)?;
                }
            }
        } else {
            return Err(InterpError::new(format!(
                "let pattern did not match value {}",
                v
            )));
        }
        Ok(())
    }

    pub(in crate::interp) fn eval_return(
        &mut self,
        e: &Option<Expr>,
    ) -> Result<Option<Value>, InterpError> {
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

    pub(in crate::interp) fn eval_break(
        &mut self,
        e: &Option<Expr>,
    ) -> Result<Option<Value>, InterpError> {
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

    pub(in crate::interp) fn eval_if_stmt(
        &mut self,
        cond: &Expr,
        then_: &Block,
        else_: &Option<Block>,
    ) -> Result<Option<Value>, InterpError> {
        let c = self.eval_expr(cond)?;
        if is_truthy(&c) {
            if let Some(v) = self.eval_block(then_)? {
                // Trailing Unit from a then-block must not turn the if-stmt
                // into a value-returning one.
                if v != Value::Unit {
                    return Ok(Some(v));
                }
            }
        } else if let Some(else_block) = else_ {
            if let Some(v) = self.eval_block(else_block)? {
                if v != Value::Unit {
                    return Ok(Some(v));
                }
            }
        }
        Ok(None)
    }

    pub(in crate::interp) fn eval_while(
        &mut self,
        cond: &Expr,
        body: &Block,
    ) -> Result<Option<Value>, InterpError> {
        while is_truthy(&self.eval_expr(cond)?) {
            if self.early_return.is_some() {
                break;
            }
            // Check invariants at each iteration start
            self.check_invariants(body)?;
            // The trailing expression of a loop body is NOT the loop's return
            // value (that role belongs to explicit `return`/`break val`). Ignore
            // `Value::Unit` from trailing statements so loops with e.g. a final
            // `println(...)` don't terminate after one iteration.
            if let Some(v) = self.eval_block(body)? {
                if v != Value::Unit {
                    return Ok(Some(v));
                }
            }
            if self.early_return.is_some() {
                break;
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
        Ok(None)
    }

    pub(in crate::interp) fn eval_while_let(
        &mut self,
        pat: &Pattern,
        init: &Expr,
        body: &Block,
    ) -> Result<Option<Value>, InterpError> {
        loop {
            let val = self.eval_expr(init)?;
            let bindings = self.match_pattern(pat, &val);
            if let Some(bindings) = bindings {
                self.push_scope();
                for (name, v) in &bindings {
                    if let Err(e) = self.bind(name, v.clone()) {
                        self.pop_scope();
                        return Err(e);
                    }
                }
                if self.early_return.is_some() {
                    self.pop_scope();
                    break;
                }
                if let Err(e) = self.check_invariants(body) {
                    self.pop_scope();
                    return Err(e);
                }
                match self.eval_block(body) {
                    Ok(Some(v)) => {
                        // See eval_while: trailing Unit must not terminate the loop.
                        if v != Value::Unit {
                            self.pop_scope();
                            return Ok(Some(v));
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        self.pop_scope();
                        return Err(e);
                    }
                }
                if self.early_return.is_some() {
                    self.pop_scope();
                    break;
                }
                match self.loop_action.take() {
                    Some(LoopAction::Break(val)) => {
                        self.pop_scope();
                        if let Some(v) = val {
                            return Ok(Some(v));
                        }
                        break;
                    }
                    Some(LoopAction::Continue) => {
                        self.pop_scope();
                        continue;
                    }
                    None => {}
                }
                self.pop_scope();
            } else {
                break;
            }
        }
        Ok(None)
    }

    fn check_invariants(&mut self, block: &Block) -> Result<(), InterpError> {
        for stmt in block {
            if let Stmt::Invariant(expr, _) = stmt.unlocated() {
                let val = self.eval_expr(expr)?;
                if !is_truthy(&val) {
                    return Err(InterpError::new(format!("invariant violated: {:?}", expr)));
                }
            }
            // Recursively check nested structures that contain blocks
            match stmt.unlocated() {
                Stmt::If { then_, else_, .. } => {
                    self.check_invariants(then_)?;
                    if let Some(eb) = else_ {
                        self.check_invariants(eb)?;
                    }
                }
                Stmt::While { body, .. } => {
                    self.check_invariants(body)?;
                }
                Stmt::Loop(body) => {
                    self.check_invariants(body)?;
                }
                Stmt::For { body, .. } => {
                    self.check_invariants(body)?;
                }
                Stmt::Block(b) => {
                    self.check_invariants(b)?;
                }
                Stmt::Arena(b) | Stmt::Parasteps(b) | Stmt::OnFailure(b) => {
                    self.check_invariants(b)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub(in crate::interp) fn eval_loop(
        &mut self,
        body: &Block,
    ) -> Result<Option<Value>, InterpError> {
        loop {
            if self.early_return.is_some() {
                break;
            }
            self.check_invariants(body)?;
            if let Some(v) = self.eval_block(body)? {
                // See eval_while: trailing Unit must not terminate the loop.
                if v != Value::Unit {
                    return Ok(Some(v));
                }
            }
            if self.early_return.is_some() {
                break;
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
        Ok(None)
    }

    pub(in crate::interp) fn eval_for(
        &mut self,
        var: &str,
        iterable: &Expr,
        body: &Block,
    ) -> Result<Option<Value>, InterpError> {
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
            Value::String(s) => s
                .chars()
                .map(|c| Value::String(c.to_string()))
                .collect::<Vec<_>>(),
            Value::Set(elems) => elems,
            Value::Record(_, fields) => fields
                .iter()
                .map(|(k, v)| Value::Tuple(vec![Value::String(k.clone()), v.clone()]))
                .collect(),
            other => return Err(InterpError::new(format!("cannot iterate over {}", other))),
        };
        for item in list {
            // IN-H5 (deep audit): push a scope per iteration so the loop variable
            // doesn't leak into the enclosing scope (matching while_let behavior).
            self.scope_env.push_scope();
            self.bind(var, item)?;
            if self.early_return.is_some() {
                self.scope_env.pop_scope();
                break;
            }
            if let Some(v) = self.eval_block(body)? {
                // Match while/loop semantics: an explicit non-Unit return from
                // the body must propagate out of the loop immediately.
                if v != Value::Unit {
                    self.scope_env.pop_scope();
                    return Ok(Some(v));
                }
            }
            self.scope_env.pop_scope();
            if self.early_return.is_some() {
                break;
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
        Ok(None)
    }

    pub(in crate::interp) fn eval_arena_block(
        &mut self,
        block: &Block,
    ) -> Result<Option<Value>, InterpError> {
        let arena_id = self.arenas.len();
        self.arenas.push(Arena {
            id: arena_id,
            slots: Vec::new(),
            generation: 0,
        });
        self.arena_depth += 1;
        self.push_scope();
        let result = self.eval_block(block);

        // Check for escape in outer scopes
        let outer_count = self.scope_env.env.len() - 1;
        let mut escape_var = None;
        for scope in self.scope_env.env.iter().take(outer_count) {
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
            return Err(InterpError::arena_escape(
                format!("arena escape: variable '{}' holds a reference to arena {} that is about to be freed",
                    name, arena_id
                )));
        }

        // If the result is a direct ArenaRef into the same arena, extract the value
        if let Ok(Some(Value::ArenaRef(id, slot, gen))) = &result {
            if *id == arena_id {
                if let Some(Arena {
                    slots, generation, ..
                }) = self.arenas.get(*id)
                {
                    if *gen != *generation {
                        // Stale ArenaRef from before a reset — treat as invalid
                        self.arena_depth -= 1;
                        self.pop_scope();
                        self.arenas.pop();
                        return Err(InterpError::new("arena: use-after-reset detected"));
                    }
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
                return Err(InterpError::arena_escape(format!(
                    "arena escape: returning a reference to arena {} that is about to be freed",
                    arena_id
                )));
            }
        }

        self.arena_depth -= 1;
        self.pop_scope();
        self.arenas.pop();
        result
    }

    pub(in crate::interp) fn eval_alloc(
        &mut self,
        kind: &AllocKind,
        body: &Block,
    ) -> Result<Option<Value>, InterpError> {
        match kind {
            AllocKind::Arena => self.eval_arena_block(body),
            AllocKind::Bump => {
                let arena_id = self.arenas.len();
                let arena = Arena {
                    id: arena_id,
                    slots: Vec::new(),
                    generation: 0,
                };
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

    pub(in crate::interp) fn eval_assign(
        &mut self,
        target: &Expr,
        value: &Expr,
    ) -> Result<Option<Value>, InterpError> {
        let v = self.eval_expr(value)?;
        // Move semantics: if value is a simple identifier and non-Copy, mark source as moved
        if let Expr::Ident(name) = value.unlocated() {
            if !is_copy(&v) && !self.is_moved(name) {
                self.mark_moved(name);
            }
        }
        match target.unlocated() {
            Expr::Ident(name) => self.assign(name, v)?,
            Expr::Unary(UnOp::Deref, inner) => {
                // *r = value: assign through mutable reference or shared pointer
                let ref_val = self.eval_expr(inner)?;
                match ref_val {
                    Value::RefMut(rc) => {
                        *rc.write().map_err(|e| {
                            InterpError::lock_error(format!("write lock failed: {}", e))
                        })? = v;
                    }
                    Value::Shared(arc) => {
                        *arc.write().map_err(|e| {
                            InterpError::lock_error(format!("shared write lock failed: {}", e))
                        })? = v;
                    }
                    Value::LocalShared(rc) => {
                        *rc.lock().unwrap_or_else(|e| e.into_inner()) = v;
                    }
                    Value::IndexRefMut { owner, index } => {
                        // Ensure the owner variable is mutable
                        let is_mut = self.is_mutable(&owner);
                        if !is_mut {
                            return Err(InterpError::new(format!(
                                "cannot assign through borrowed index into immutable variable '{}'",
                                owner
                            )));
                        }
                        let owner_val = self.lookup(&owner).ok_or_else(|| {
                            InterpError::new(format!(
                                "borrowed variable '{}' is no longer available",
                                owner
                            ))
                        })?;
                        match owner_val {
                            Value::List(mut list) => {
                                if index >= list.len() {
                                    return Err(InterpError::index_out_of_bounds(format!(
                                        "borrowed index {} out of bounds for list of length {}",
                                        index,
                                        list.len()
                                    )));
                                }
                                list[index] = v;
                                self.assign(&owner, Value::List(list))?;
                            }
                            _ => {
                                return Err(InterpError::new(format!(
                                    "cannot assign through borrowed index into {}",
                                    type_name(&owner_val)
                                )))
                            }
                        }
                    }
                    Value::PlaceRefMut { owner, projections } => {
                        if !self.is_mutable(&owner) {
                            return Err(InterpError::new(format!(
                                "cannot assign through borrowed projection of immutable variable '{}'",
                                owner
                            )));
                        }
                        let mut owner_value = self.lookup(&owner).ok_or_else(|| {
                            InterpError::new(format!(
                                "borrowed variable '{}' is no longer available",
                                owner
                            ))
                        })?;
                        write_runtime_place(&mut owner_value, &projections, v)?;
                        self.assign(&owner, owner_value)?;
                    }
                    _ => {
                        return Err(InterpError::new(format!(
                            "cannot assign through non-mutable reference (type: {})",
                            type_name(&ref_val)
                        )))
                    }
                }
            }
            Expr::Field(obj, field) => {
                // Special case: if assigning to self.field, update actor directly
                if let Expr::Ident(name) = obj.unlocated() {
                    if name == "self" {
                        // Find the actor handle in scope and update its field
                        if let Some(Value::Actor(handle)) = self.lookup("self") {
                            handle
                                .inner
                                .write()
                                .map_err(|e| {
                                    InterpError::lock_error(format!("actor lock failed: {}", e))
                                })?
                                .fields
                                .insert(field.clone(), v);
                            return Ok(None);
                        }
                    }
                }
                let obj_val = self.eval_expr(obj)?;
                match obj_val {
                    Value::Record(type_name, mut fields) => {
                        if fields.contains_key(field.as_str()) {
                            if let std::collections::hash_map::Entry::Occupied(mut e) =
                                fields.entry(field.clone())
                            {
                                e.insert(v);
                            }
                            // DAT-C4 / I-H7: write the modified record back through
                            // the place. Nested `o.inner.x = 42` must update the
                            // outer record, not only a discarded clone.
                            let updated = Value::Record(type_name, fields);
                            self.write_place_value(obj, updated)?;
                        } else {
                            return Err(InterpError::field_not_found(format!(
                                "field '{}' not found in record",
                                field
                            )));
                        }
                    }
                    Value::Actor(handle) => {
                        handle
                            .inner
                            .write()
                            .map_err(|e| {
                                InterpError::lock_error(format!("actor lock failed: {}", e))
                            })?
                            .fields
                            .insert(field.clone(), v);
                    }
                    Value::Shared(arc) => {
                        let mut inner = arc.write().map_err(|e| {
                            InterpError::lock_error(format!("shared write lock failed: {}", e))
                        })?;
                        match &mut *inner {
                            Value::Record(_, fields) => {
                                if fields.contains_key(field.as_str()) {
                                    if let std::collections::hash_map::Entry::Occupied(mut e) =
                                        fields.entry(field.clone())
                                    {
                                        e.insert(v);
                                    }
                                } else {
                                    return Err(InterpError::field_not_found(format!(
                                        "field '{}' not found in shared record",
                                        field
                                    )));
                                }
                            }
                            _ => {
                                return Err(InterpError::new(format!(
                                    "cannot assign to field of non-record shared value (type: {})",
                                    type_name(&inner)
                                )))
                            }
                        }
                    }
                    Value::LocalShared(rc) => {
                        let mut inner = rc.lock().unwrap_or_else(|e| e.into_inner());
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
                    _ => {
                        return Err(InterpError::new(format!(
                            "cannot assign to field of non-record/non-actor value (type: {})",
                            type_name(&obj_val)
                        )))
                    }
                }
            }
            Expr::Index(obj, idx) => {
                // list[i] = val: evaluate list, index, and set element
                let list_val = self.eval_expr(obj)?;
                let idx_val = self.eval_expr(idx)?;
                let index = match idx_val {
                    Value::Int(i) => i as usize,
                    _ => {
                        return Err(InterpError::new(format!(
                            "list index must be an integer, got {}",
                            type_name(&idx_val)
                        )))
                    }
                };
                fn assign_list_index(
                    items: &mut [Value],
                    index: usize,
                    v: Value,
                ) -> Result<(), InterpError> {
                    if index >= items.len() {
                        return Err(InterpError::new(format!(
                            "list index {} out of bounds (len {})",
                            index,
                            items.len()
                        )));
                    }
                    items[index] = v;
                    Ok(())
                }

                match list_val {
                    Value::List(mut items) => {
                        assign_list_index(&mut items, index, v)?;
                        // Update the binding
                        if let Expr::Ident(name) = obj.unlocated() {
                            let mut found = false;
                            for scope in self.scope_env.env.iter_mut().rev() {
                                if scope.contains_key(name) {
                                    scope.insert(name.clone(), Value::List(items));
                                    found = true;
                                    break;
                                }
                            }
                            if !found {
                                return Err(InterpError::new(format!(
                                    "variable '{}' not found",
                                    name
                                )));
                            }
                        }
                    }
                    Value::RefMut(rc) => {
                        let mut inner = rc.write().map_err(|e| {
                            InterpError::lock_error(format!("write lock failed: {}", e))
                        })?;
                        match &mut *inner {
                            Value::List(items) => assign_list_index(items, index, v)?,
                            _ => {
                                return Err(InterpError::new(format!(
                                    "cannot index-assign through &mut reference to {}",
                                    type_name(&inner)
                                )))
                            }
                        }
                    }
                    Value::Shared(rc) => {
                        let mut inner = rc.write().map_err(|e| {
                            InterpError::lock_error(format!("shared write lock failed: {}", e))
                        })?;
                        match &mut *inner {
                            Value::List(items) => assign_list_index(items, index, v)?,
                            _ => {
                                return Err(InterpError::new(format!(
                                    "cannot index-assign through shared reference to {}",
                                    type_name(&inner)
                                )))
                            }
                        }
                    }
                    _ => {
                        return Err(InterpError::new(format!(
                            "cannot index-assign to non-list value (type: {})",
                            type_name(&list_val)
                        )))
                    }
                }
            }
            _ => return Err(InterpError::new("assignment target must be a variable")),
        }
        Ok(None)
    }

    pub(in crate::interp) fn eval_shared_let(
        &mut self,
        kind: &SharedKind,
        name: &str,
        init: &Expr,
    ) -> Result<(), InterpError> {
        let v = self.eval_expr(init)?;
        let shared_val = match kind {
            // TC-C1 dual: do not double-wrap. `shared y = x` / `shared y = if { x }`
            // where x is already Shared must alias the same cell (codegen loads once).
            SharedKind::Shared => match v {
                Value::Shared(arc) => Value::Shared(Arc::clone(&arc)),
                other => Value::Shared(Arc::new(RwLock::new(other))),
            },
            SharedKind::LocalShared => match v {
                Value::LocalShared(rc) => Value::LocalShared(LocalSharedInner::clone_rc(&rc)),
                other => Value::LocalShared(LocalSharedInner::new(other)),
            },
            SharedKind::Weak => {
                // Auto-detect: if init is Shared → WeakShared, if LocalShared → WeakLocal
                match v {
                    Value::Shared(arc) => Value::WeakShared(Arc::downgrade(&arc)),
                    Value::LocalShared(rc) => Value::WeakLocal(rc.downgrade()),
                    _ => {
                        return Err(InterpError::new(format!(
                            "weak requires a shared or local_shared value, got {}",
                            v
                        )))
                    }
                }
            }
            SharedKind::WeakLocal => match v {
                Value::LocalShared(rc) => Value::WeakLocal(rc.downgrade()),
                _ => {
                    return Err(InterpError::new(format!(
                        "weak_local requires a local_shared value, got {}",
                        v
                    )))
                }
            },
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

    pub(in crate::interp) fn eval_parasteps(
        &mut self,
        block: &Block,
    ) -> Result<Option<Value>, InterpError> {
        // Parasteps block: execute spawn statements in parallel
        // Collect spawn expressions and their results
        // Runtime assertion: scan current scope for LocalShared values
        for scope in &self.scope_env.env {
            for val in scope.values() {
                if crate::interp::value::contains_local_shared(val) {
                    return Err(InterpError::new(
                        "parasteps: local_shared values cannot cross thread boundary",
                    ));
                }
            }
        }
        let mut last_value = None;
        type SpawnFuture = std::sync::Arc<std::sync::Mutex<crate::interp::value::PollFuture>>;
        let mut futures: Vec<SpawnFuture> = Vec::new();
        let mut spawn_bindings: HashMap<String, SpawnFuture> = HashMap::new();

        for stmt in block {
            match stmt.unlocated() {
                Stmt::Expr(spawn_expr) if matches!(spawn_expr.unlocated(), Expr::Spawn(_)) => {
                    let Expr::Spawn(expr) = spawn_expr.unlocated() else {
                        unreachable!("spawn guard accepted a non-spawn expression")
                    };
                    // Run the spawn expression only once — in the worker thread.
                    // The previous implementation evaluated `expr` on the main
                    // thread (to scan for LocalShared) and again in the worker,
                    // which doubled any side effects (IN-H10 / parasteps double
                    // evaluation). We now evaluate exactly once and perform the
                    // LocalShared boundary check in the worker instead.
                    let (tx, rx) = std::sync::mpsc::channel();
                    let expr = expr.clone();
                    let file = self.file.clone();
                    // I-H6: capture free variables from the lexical environment.
                    let mut free = std::collections::HashSet::new();
                    crate::interp::closure_utils::collect_expr_free_vars(
                        expr.as_ref(),
                        &std::collections::HashSet::new(),
                        &mut free,
                    );
                    let mut captures = Vec::new();
                    for name in free {
                        if let Some(v) = self.lookup(&name) {
                            captures.push((name, v));
                        }
                    }
                    let stdout_buf = self.stdout_capture.clone();
                    super::super::pool::get_pool().execute(move || {
                        let mut interp = Interpreter::new(&file);
                        if let Some(buf) = stdout_buf {
                            interp.set_stdout_buf(buf);
                        }
                        interp.push_scope();
                        for (n, v) in captures {
                            let _ = interp.bind(&n, v);
                        }
                        let result = interp.eval_expr(&expr);
                        let checked = match &result {
                            Ok(v) if crate::interp::value::contains_local_shared(v) => {
                                Err(InterpError::new(
                                    "parasteps: local_shared values cannot cross thread boundary",
                                ))
                            }
                            other => (*other).clone(),
                        };
                        interp.pop_scope();
                        let _ = tx.send(checked);
                    });
                    futures.push(std::sync::Arc::new(std::sync::Mutex::new(
                        crate::interp::value::PollFuture::Pending(rx),
                    )));
                }
                Stmt::Let { pat, init, .. } => {
                    // Handle let bindings that might contain spawn
                    let v = match init {
                        Some(spawn_expr) if matches!(spawn_expr.unlocated(), Expr::Spawn(_)) => {
                            let Expr::Spawn(expr) = spawn_expr.unlocated() else {
                                unreachable!("spawn guard accepted a non-spawn expression")
                            };
                            // Create a future for concurrent execution
                            let (tx, rx) = std::sync::mpsc::channel();
                            let expr = expr.clone();
                            let file = self.file.clone();
                            // I-H6: capture free variables from the lexical environment.
                            let mut free = std::collections::HashSet::new();
                            crate::interp::closure_utils::collect_expr_free_vars(
                                expr.as_ref(),
                                &std::collections::HashSet::new(),
                                &mut free,
                            );
                            let mut captures = Vec::new();
                            for name in free {
                                if let Some(v) = self.lookup(&name) {
                                    captures.push((name, v));
                                }
                            }
                            let stdout_buf = self.stdout_capture.clone();
                            super::super::pool::get_pool().execute(move || {
                                let mut interp = Interpreter::new(&file);
                                if let Some(buf) = stdout_buf {
                                    interp.set_stdout_buf(buf);
                                }
                                interp.push_scope();
                                for (n, v) in captures {
                                    let _ = interp.bind(&n, v);
                                }
                                let result = interp.eval_expr(&expr);
                                interp.pop_scope();
                                let _ = tx.send(result);
                            });
                            let fut_arc = std::sync::Arc::new(std::sync::Mutex::new(
                                crate::interp::value::PollFuture::Pending(rx),
                            ));
                            // Add to futures for proper await at block end
                            futures.push(fut_arc.clone());
                            // Store in spawn_bindings for name->future lookup
                            if let PatternKind::Variable(name) = &pat.kind {
                                spawn_bindings.insert(name.clone(), fut_arc.clone());
                            }
                            Value::Future(fut_arc)
                        }
                        Some(e) => self.eval_expr(e)?,
                        None => Value::Unit,
                    };
                    if let Some(bindings) = self.match_pattern_bind(pat, &v) {
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
            let mut fut = fut
                .lock()
                .map_err(|e| InterpError::new(format!("await lock failed: {}", e)))?;
            crate::interp::value::poll_deferred(&mut fut);
            match &mut *fut {
                crate::interp::value::PollFuture::Pending(rx) => {
                    match rx.recv() {
                        Ok(Err(e)) => return Err(e),
                        Ok(Ok(val)) => {
                            // P2-24: check if returned value is a failure
                            // variant and propagate via early_return so
                            // on_failure compensations trigger.
                            if let Value::Variant(name, _) = &val {
                                if self.failure_variants.get(name).copied().unwrap_or(false) {
                                    self.early_return = Some(val);
                                    return Ok(None);
                                }
                            }
                        }
                        Err(_) => {
                            // Channel closed: the future was consumed by
                            // an explicit `await` elsewhere. Silently skip.
                        }
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
            let mut fut = fut
                .lock()
                .map_err(|e| InterpError::new(format!("await lock failed: {}", e)))?;
            crate::interp::value::poll_deferred(&mut fut);
            match &mut *fut {
                crate::interp::value::PollFuture::Pending(rx) => {
                    last_value = Some(
                        rx.recv()
                            .map_err(|e| InterpError::new(format!("await failed: {}", e)))??,
                    );
                }
                crate::interp::value::PollFuture::Ready(result) => {
                    last_value = Some(std::mem::replace(
                        result,
                        Err(InterpError::new("future already consumed")),
                    )?);
                }
                crate::interp::value::PollFuture::Deferred { .. } => {
                    return Err(InterpError::new("future still deferred in parasteps"));
                }
            }
        }

        Ok(last_value)
    }
}

fn write_runtime_place(
    value: &mut Value,
    projections: &[RuntimeProjection],
    replacement: Value,
) -> Result<(), InterpError> {
    let Some((projection, rest)) = projections.split_first() else {
        *value = replacement;
        return Ok(());
    };
    let child = match (value, projection) {
        (Value::Record(_, fields), RuntimeProjection::Field(field)) => fields
            .get_mut(field)
            .ok_or_else(|| InterpError::new(format!("record has no field '{}'", field)))?,
        (Value::Tuple(values), RuntimeProjection::Tuple(index)) => {
            values.get_mut(*index).ok_or_else(|| {
                InterpError::index_out_of_bounds(format!("tuple index {} is out of bounds", index))
            })?
        }
        _ => {
            return Err(InterpError::new(
                "borrowed mutable projection does not match its runtime value",
            ))
        }
    };
    write_runtime_place(child, rest, replacement)
}
