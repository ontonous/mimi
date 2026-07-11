use super::*;
use std::collections::HashMap;

mod expr;
mod helpers;
mod stmt;

impl<'a> Interpreter<'a> {
    /// Cast a value to a target type
    pub(crate) fn cast_value(&self, val: Value, target_type: &Type) -> Result<Value, InterpError> {
        match target_type {
            Type::Name(name, _) => match name.as_str() {
                "i32" => match val {
                    Value::Int(v) => Ok(Value::Int(v as i32 as i64)),
                    Value::Float(v) => Ok(Value::Int(v as i64)),
                    _ => Err(InterpError::new(format!("cannot cast {:?} to i32", val))),
                },
                "i64" => match val {
                    Value::Int(v) => Ok(Value::Int(v)),
                    Value::Float(v) => Ok(Value::Int(v as i64)),
                    _ => Err(InterpError::new(format!("cannot cast {:?} to i64", val))),
                },
                "f64" => match val {
                    Value::Int(v) => Ok(Value::Float(v as f64)),
                    Value::Float(v) => Ok(Value::Float(v)),
                    _ => Err(InterpError::new(format!("cannot cast {:?} to f64", val))),
                },
                "bool" => match val {
                    Value::Int(v) => Ok(Value::Bool(v != 0)),
                    _ => Err(InterpError::new(format!("cannot cast {:?} to bool", val))),
                },
                "string" => match val {
                    Value::Int(v) => Ok(Value::String(v.to_string())),
                    Value::Float(v) => Ok(Value::String(v.to_string())),
                    Value::Bool(v) => Ok(Value::String(v.to_string())),
                    _ => Err(InterpError::new(format!("cannot cast {:?} to string", val))),
                },
                "List" => {
                    // Type annotation for lists (e.g., `[] as List<string>`).
                    // No runtime conversion needed — type checked at compile time.
                    Ok(val)
                }
                _ => Err(InterpError::new(format!(
                    "unsupported cast target type: {}",
                    name
                ))),
            },
            _ => Err(InterpError::new(format!(
                "unsupported cast target type: {:?}",
                target_type
            ))),
        }
    }

    pub(crate) fn eval_block(&mut self, block: &Block) -> Result<Option<Value>, InterpError> {
        self.push_compensation_scope();
        let result = self.eval_block_inner(block);
        self.pop_compensation_scope(
            result.is_err() || self.early_return.is_some() || self.exited.is_some(),
        );
        result
    }

    fn eval_block_inner(&mut self, block: &Block) -> Result<Option<Value>, InterpError> {
        for (i, stmt) in block.iter().enumerate() {
            let is_last = i == block.len() - 1;
            match stmt {
                Stmt::Expr(e) if is_last => {
                    let result = self.eval_expr(e);
                    // `exit()` inside the final expression must abort the block.
                    if self.exited.is_some() {
                        return Ok(None);
                    }
                    match result {
                        Ok(Value::Error(msg)) => {
                            return Err(InterpError::new(msg));
                        }
                        Ok(v) => {
                            // Only a non-Unit trailing expression is a meaningful
                            // return value for callers (e.g. function bodies).
                            // Unit must fall through so loop/if-else bodies with
                            // a trailing `println(...)` keep iterating.
                            if v == Value::Unit {
                                return Ok(None);
                            }
                            return Ok(Some(v));
                        }
                        Err(e) => {
                            return Err(e);
                        }
                    }
                }
                Stmt::Expr(e) => {
                    let result = self.eval_expr(e);
                    // `exit()` inside a side-effect expression must abort the block.
                    if self.exited.is_some() {
                        return Ok(None);
                    }
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
                _ if is_last => {
                    if let Some(v) = self.eval_stmt(stmt)? {
                        return Ok(Some(v));
                    }
                }
                _ => {
                    // Propagate control-flow signals (return/break values) but not
                    // meaningless Unit values (e.g. push() as trailing expression in a block).
                    if let Some(v) = self.eval_stmt(stmt)? {
                        if v != Value::Unit {
                            return Ok(Some(v));
                        }
                    }
                }
            }
            // Propagate break/continue, early return, and exit signals out of the block
            if self.loop_action.is_some() || self.early_return.is_some() || self.exited.is_some() {
                return Ok(None);
            }
        }
        Ok(None)
    }

    pub(crate) fn eval_stmt(&mut self, stmt: &Stmt) -> Result<Option<Value>, InterpError> {
        match stmt {
            Stmt::Let {
                pat,
                init,
                mut_,
                ref_,
                ty,
                ..
            } => {
                self.eval_let(pat, init, *mut_, *ref_, ty)?;
            }
            Stmt::Return(e) => return self.eval_return(e),
            Stmt::Break(e) => return self.eval_break(e),
            Stmt::Continue => return self.eval_continue(),
            Stmt::Expr(e) => {
                if let Value::Error(msg) = self.eval_expr(e)? {
                    return Err(InterpError::new(msg));
                }
            }
            Stmt::If { cond, then_, else_ } => {
                if let Some(v) = self.eval_if_stmt(cond, then_, else_)? {
                    return Ok(Some(v));
                }
            }
            Stmt::While { cond, body } => {
                if let Some(v) = self.eval_while(cond, body)? {
                    return Ok(Some(v));
                }
            }
            Stmt::WhileLet { pat, init, body } => {
                if let Some(v) = self.eval_while_let(pat, init, body)? {
                    return Ok(Some(v));
                }
            }
            Stmt::Loop(body) => {
                if let Some(v) = self.eval_loop(body)? {
                    return Ok(Some(v));
                }
            }
            Stmt::For {
                var,
                iterable,
                body,
            } => {
                if let Some(v) = self.eval_for(var, iterable, body)? {
                    return Ok(Some(v));
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
                return self.eval_alloc(kind, body);
            }
            Stmt::Assign { target, value } => {
                return self.eval_assign(target, value);
            }
            Stmt::Desc(..)
            | Stmt::Rule(..)
            | Stmt::Requires(_, _)
            | Stmt::Ensures(_, _)
            | Stmt::Invariant(_, _)
            | Stmt::Ellipsis
            | Stmt::MmsBlock { .. } => {}
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
            Stmt::SharedLet {
                kind, name, init, ..
            } => {
                self.eval_shared_let(kind, name, init)?;
            }
            Stmt::OnFailure(block) => {
                self.eval_on_failure(block)?;
            }
            Stmt::Parasteps(block) => {
                return self.eval_parasteps(block);
            }
            Stmt::Func(f) => {
                // Bind nested function as a closure in the current scope
                let closure = Value::Closure {
                    params: f.params.clone(),
                    ret: f.ret.clone(),
                    body: f.body.clone(),
                    captured: HashMap::new(),
                };
                self.bind(&f.name, closure)?;
            }
            Stmt::Do(body) => {
                if let Some(v) = self.eval_block(body)? {
                    return Ok(Some(v));
                }
            }
            Stmt::Delegate { kind, expr, target } => {
                let val = self.eval_expr(expr)?;
                // Look up the target subflow in scope (validates it exists)
                let _target_val = self.scope_env.lookup(target).ok_or_else(|| {
                    InterpError::new(format!("delegate target '{}' not found in scope", target))
                })?;
                match kind {
                    DelegateKind::Consume => {
                        // Consume: ownership transferred to subflow — explicitly drop
                        // In a full implementation this would route to the subflow's
                        // transition. For now, consuming means the resource moves.
                        drop(val);
                    }
                    DelegateKind::View | DelegateKind::Mutate => {
                        // View/Mutate: resource stays in the parent flow.
                        // Evaluate the expression (side effects if any) but keep
                        // the value alive by storing in a temp that lives for the
                        // scope. In practice, self.field access just reads — the
                        // value remains bound in the flow state.
                        let _kept = val;
                    }
                }
            }
            Stmt::Pinned { expr, var, body, .. } => {
                let val = self.eval_expr(expr)?;
                // Bind the pinned variable in a nested scope for the body
                self.scope_env.push_scope();
                if let Some(var_name) = var {
                    self.scope_env.bind(var_name, val)?;
                }
                self.eval_block(body)?;
                self.scope_env.pop_scope();
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
            Expr::Ident(name) => self.eval_ident(name),
            Expr::Unary(op, e) => self.eval_unary(*op, e),
            Expr::Binary(op, l, r) => self.eval_binary(*op, l, r),
            Expr::Call(callee, args) => self.eval_call(callee, args),
            Expr::Tuple(elems) => self.eval_tuple(elems),
            Expr::TupleIndex(obj, idx) => self.eval_tuple_index(obj, *idx),
            Expr::List(elems) => self.eval_list(elems),
            Expr::Comprehension {
                expr,
                var,
                iter,
                guard,
            } => self.eval_comprehension(expr, var, iter, guard),
            Expr::If { cond, then_, else_ } => self.eval_if_expr(cond, then_, else_),
            Expr::Arena(block) => self
                .eval_arena_block(block)
                .map(|v| v.unwrap_or(Value::Unit)),
            Expr::Block(block) => Ok(self.eval_block(block)?.unwrap_or(Value::Unit)),
            Expr::Match(subject, arms) => self.eval_match(subject, arms),
            Expr::Field(obj, field) => self.eval_field(obj, field),
            Expr::Record { ty, fields } => self.eval_record(ty, fields),
            Expr::Index(obj_expr, idx_expr) => self.eval_index(obj_expr, idx_expr),
            Expr::SliceExpr { target, start, end } => self.eval_slice_expr(target, start, end),
            Expr::Try(expr) => self.eval_try(expr),
            Expr::Spawn(expr) => self.eval_spawn(expr),
            Expr::Await(expr) => self.eval_await(expr),
            Expr::QuoteInterpolate(expr) => {
                let v = self.eval_expr(expr)?;
                Ok(Value::QuoteAst(Box::new(QuotedAst::Interpolate(Box::new(
                    v,
                )))))
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
            Expr::Lambda { params, ret, body } => self.eval_lambda(params, ret, body),
            Expr::Turbofish(name, type_args, args) => self.eval_turbofish(name, type_args, args),
            Expr::Comptime(block) => self.eval_comptime(block),
            Expr::TypeOf(expr) => self.eval_type_of(expr),
            Expr::TypeInfo(ty) => self.eval_type_info(ty),
            Expr::Range { start, end } => self.eval_range(start, end),
            Expr::MapLiteral { entries } => self.eval_map_literal(entries),
            Expr::SetLiteral(elems) => self.eval_set_literal(elems),
            Expr::NamedArg(_, value) => self.eval_expr(value),
            Expr::Cast(inner, target_type) => {
                let val = self.eval_expr(inner)?;
                self.cast_value(val, target_type)
            }
        }
    }

    /// Execute a flow transition call: FlowName::transition(self_payload, params...)
    /// The first argument is the from-state payload (bound to `self`),
    /// remaining args are the transition's event parameters.
    ///
    /// v0.29.11: when the transition is a transfer-matrix fallback (`is_fallback`)
    /// or the result is a Fault record, the from-state payload is dropped after the
    /// body runs (auto-destruct of the abandoned state's resources). Actors nested
    /// in that payload have their mailboxes short-circuited so subsequent messages
    /// are discarded without waking the worker.
    ///
    /// v0.29.12: runtime Panic inside a transition body is absorbed into Fault
    /// with a full SystemTrace (last_state / unexpected_event=`panic:<code>` /
    /// snapshot with message + call stack). Already-Fault sources re-raise.
    pub(in crate::interp) fn eval_flow_transition(
        &mut self,
        flow: &FlowDef,
        t: &TransitionDef,
        vals: &[Value],
    ) -> Result<Value, InterpError> {
        let body = t.body.as_ref().ok_or_else(|| {
            InterpError::new(format!("transition '{}' has no body", t.name))
        })?;

        self.push_scope();

        // Bind self to the first argument (from-state payload)
        let self_val = vals.first().cloned().unwrap_or(Value::Unit);
        self.bind("self", self_val)?;

        // Bind transition params from remaining args
        for (i, param) in t.params.iter().enumerate() {
            let arg = vals.get(i + 1).cloned().unwrap_or(Value::Unit);
            if param.mut_ {
                self.bind_mut(&param.name, arg)?;
            } else {
                self.bind(&param.name, arg)?;
            }
        }

        // Execute the transition body
        let result = self.eval_block(body);
        let call_stack = self.scope_env.call_stack.clone();

        self.pop_scope();

        let out = match result {
            Ok(v) => v.unwrap_or(Value::Unit),
            Err(e) => {
                // Already in Fault: do not re-wrap (avoid infinite absorption).
                if t.from_state == "Fault" {
                    return Err(e);
                }
                // v0.29.12: only absorb true runtime panics (div0/overflow/OOB/…),
                // not programming errors like undefined names or type mismatches.
                if !is_runtime_panic(&e) {
                    return Err(e);
                }
                let event = format!("panic:{}", e.code());
                let snapshot = format_panic_snapshot(&e, &call_stack);
                let mut fault =
                    crate::flow_matrix::make_fault_value(&t.from_state, &event, &snapshot);
                // Shadow persistent fields for recover.
                if let Some(from_payload) = vals.first() {
                    shadow_persistent_into_fault(&mut fault, from_payload, &flow.persistent_fields);
                    drop_fault_payload_except(from_payload, &flow.persistent_fields);
                }
                return Ok(fault);
            }
        };

        // Fault absorption: drop abandoned from-state payload (+ mailbox short-circuit).
        // v0.29.13: skip Drop on persistent fields (they live on as Fault shadows).
        let enters_fault = t.is_fallback
            || t.to_states.iter().any(|s| s == "Fault")
            || matches!(&out, Value::Record(Some(n), _) if n == "Fault");
        if enters_fault {
            if let Some(from_payload) = vals.first() {
                drop_fault_payload_except(from_payload, &flow.persistent_fields);
            }
        }

        // v0.29.13: reset / recover clear actor faulted flags so the flow can
        // accept messages again after leaving Fault.
        if (t.name == "reset" || t.name == "recover") && t.from_state == "Fault" {
            if let Some(from_payload) = vals.first() {
                clear_faulted_actors(from_payload);
            }
            // Also clear on the newly built root if it carries actors.
            clear_faulted_actors(&out);
        }

        Ok(out)
    }
}

/// Copy persistent fields from the abandoned state into the Fault record.
fn shadow_persistent_into_fault(
    fault: &mut Value,
    from: &Value,
    persistent: &[String],
) {
    if persistent.is_empty() {
        return;
    }
    let (Value::Record(_, from_fields), Value::Record(_, fault_fields)) = (from, fault) else {
        return;
    };
    for name in persistent {
        if let Some(v) = from_fields.get(name) {
            fault_fields.insert(name.clone(), v.clone());
        }
    }
}

/// Like `drop_fault_payload` but skips fields listed in `persistent`.
fn drop_fault_payload_except(val: &Value, persistent: &[String]) {
    match val {
        Value::Actor(handle) => {
            handle.short_circuit_mailbox();
        }
        Value::Record(_, fields) => {
            for (name, f) in fields {
                if persistent.iter().any(|p| p == name) {
                    continue;
                }
                drop_fault_payload_except(f, persistent);
            }
        }
        Value::List(items)
        | Value::Tuple(items)
        | Value::Set(items)
        | Value::Array(items)
        | Value::Variant(_, items) => {
            for item in items {
                drop_fault_payload_except(item, persistent);
            }
        }
        Value::Newtype(_, inner) | Value::DynTrait { data: inner, .. } => {
            drop_fault_payload_except(inner, persistent);
        }
        Value::Shared(arc) | Value::Ref(arc) | Value::RefMut(arc) => {
            if let Ok(guard) = arc.read() {
                drop_fault_payload_except(&guard, persistent);
            }
        }
        Value::LocalShared(inner) => {
            if let Ok(guard) = inner.0.lock() {
                drop_fault_payload_except(&guard, persistent);
            }
        }
        Value::Slice { source, .. } => {
            for item in source {
                drop_fault_payload_except(item, persistent);
            }
        }
        _ => {}
    }
}

/// Clear `faulted` on nested actors so they accept messages after reset/recover.
fn clear_faulted_actors(val: &Value) {
    match val {
        Value::Actor(handle) => {
            if let Ok(mut actor) = handle.inner.write() {
                actor.faulted = false;
            }
        }
        Value::Record(_, fields) => {
            for f in fields.values() {
                clear_faulted_actors(f);
            }
        }
        Value::List(items)
        | Value::Tuple(items)
        | Value::Set(items)
        | Value::Array(items)
        | Value::Variant(_, items) => {
            for item in items {
                clear_faulted_actors(item);
            }
        }
        Value::Newtype(_, inner) | Value::DynTrait { data: inner, .. } => {
            clear_faulted_actors(inner);
        }
        Value::Shared(arc) | Value::Ref(arc) | Value::RefMut(arc) => {
            if let Ok(guard) = arc.read() {
                clear_faulted_actors(&guard);
            }
        }
        Value::LocalShared(inner) => {
            if let Ok(guard) = inner.0.lock() {
                clear_faulted_actors(&guard);
            }
        }
        _ => {}
    }
}

fn format_panic_snapshot(e: &InterpError, call_stack: &[String]) -> String {
    let stack = if call_stack.is_empty() {
        String::from("<empty>")
    } else {
        call_stack.join(" <- ")
    };
    format!("{} [{}] stack: {}", e.message(), e.code(), stack)
}

/// True for runtime panics that map to Fault (white-paper §6.4).
/// Excludes Generic / FieldNotFound / WrongArgCount etc. so legitimate
/// programming errors still surface to the caller.
fn is_runtime_panic(e: &InterpError) -> bool {
    matches!(
        e,
        InterpError::DivisionByZero(_)
            | InterpError::IntegerOverflow(_)
            | InterpError::IndexOutOfBounds(_)
            | InterpError::NonExhaustiveMatch(_)
            | InterpError::FloatError(_)
            | InterpError::SliceError(_)
            | InterpError::ContractViolation(_)
    )
}

/// Recursively release resources held by a from-state payload when entering Fault.
fn drop_fault_payload(val: &Value) {
    drop_fault_payload_except(val, &[]);
}
