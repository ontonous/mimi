// Interp builtins for v0.28.20 concurrency primitives (atomic / mutex / channel).
//
// All primitives are exposed via the same handle-as-Value::Int mechanism used by
// set/map, with the actual storage living in `crate::runtime` (shared between
// interp and codegen). The interpreter dispatch is a thin wrapper that simply
// calls the same runtime C ABI function the codegen uses — guaranteeing L1
// double-backend parity by construction.

use super::*;

impl<'a> Interpreter<'a> {
    pub(crate) fn builtin_atomic_i32_new(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("atomic_i32_new expects 1 argument (i32)"));
        }
        let v = match &args[0] {
            Value::Int(x) => *x as i32,
            _ => return Err(InterpError::new("atomic_i32_new expects i32")),
        };
        // SAFETY: passing a well-typed i32; runtime returns a valid i64 handle.
        let handle = crate::runtime::mimi_atomic_i32_new(v);
        Ok(Value::Int(handle))
    }

    pub(crate) fn builtin_atomic_i32_load(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("atomic_i32_load expects 1 argument"));
        }
        let h = args[0].as_i64_for_handle()?;
        // SAFETY: handle is an i64 id returned by mimi_atomic_i32_new.
        let v = crate::runtime::mimi_atomic_i32_load(h);
        Ok(Value::Int(v as i64))
    }

    pub(crate) fn builtin_atomic_i32_store(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new(
                "atomic_i32_store expects 2 arguments (handle, i32)",
            ));
        }
        let h = args[0].as_i64_for_handle()?;
        let v = match &args[1] {
            Value::Int(x) => *x as i32,
            _ => return Err(InterpError::new("atomic_i32_store expects i32 value")),
        };
        // SAFETY: handle is a valid atomic_i32 handle.
        crate::runtime::mimi_atomic_i32_store(h, v);
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_atomic_i32_fetch_add(
        &self,
        args: Vec<Value>,
    ) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new(
                "atomic_i32_fetch_add expects 2 arguments (handle, delta)",
            ));
        }
        let h = args[0].as_i64_for_handle()?;
        let d = match &args[1] {
            Value::Int(x) => *x as i32,
            _ => return Err(InterpError::new("atomic_i32_fetch_add expects i32 delta")),
        };
        // SAFETY: handle is a valid atomic_i32 handle.
        let prev = crate::runtime::mimi_atomic_i32_fetch_add(h, d);
        Ok(Value::Int(prev as i64))
    }

    pub(crate) fn builtin_atomic_i32_compare_exchange(
        &self,
        args: Vec<Value>,
    ) -> Result<Value, InterpError> {
        if args.len() != 3 {
            return Err(InterpError::new(
                "atomic_i32_compare_exchange expects 3 arguments",
            ));
        }
        let h = args[0].as_i64_for_handle()?;
        let exp = match &args[1] {
            Value::Int(x) => *x as i32,
            _ => return Err(InterpError::new("expected i32")),
        };
        let nv = match &args[2] {
            Value::Int(x) => *x as i32,
            _ => return Err(InterpError::new("expected i32")),
        };
        // SAFETY: handle is a valid atomic_i32 handle.
        let ok = crate::runtime::mimi_atomic_i32_compare_exchange(h, exp, nv);
        Ok(Value::Int(ok as i64))
    }

    pub(crate) fn builtin_atomic_i32_drop(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("atomic_i32_drop expects 1 argument"));
        }
        let h = args[0].as_i64_for_handle()?;
        // SAFETY: handle is a valid atomic_i32 handle.
        crate::runtime::mimi_atomic_i32_drop(h);
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_atomic_i64_new(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("atomic_i64_new expects 1 argument"));
        }
        let v = args[0].as_i64_for_handle()?;
        // SAFETY: passing a valid i64.
        let handle = crate::runtime::mimi_atomic_i64_new(v);
        Ok(Value::Int(handle))
    }

    pub(crate) fn builtin_atomic_i64_load(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("atomic_i64_load expects 1 argument"));
        }
        let h = args[0].as_i64_for_handle()?;
        // SAFETY: handle is a valid atomic_i64 handle.
        let v = crate::runtime::mimi_atomic_i64_load(h);
        Ok(Value::Int(v))
    }

    pub(crate) fn builtin_atomic_i64_store(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new(
                "atomic_i64_store expects 2 arguments (handle, i64)",
            ));
        }
        let h = args[0].as_i64_for_handle()?;
        let v = match &args[1] {
            Value::Int(x) => *x,
            _ => return Err(InterpError::new("atomic_i64_store expects i64")),
        };
        // SAFETY: handle is a valid atomic_i64 handle.
        crate::runtime::mimi_atomic_i64_store(h, v);
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_atomic_i64_fetch_add(
        &self,
        args: Vec<Value>,
    ) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("atomic_i64_fetch_add expects 2 arguments"));
        }
        let h = args[0].as_i64_for_handle()?;
        let d = match &args[1] {
            Value::Int(x) => *x,
            _ => return Err(InterpError::new("expected i64 delta")),
        };
        // SAFETY: handle is a valid atomic_i64 handle.
        let prev = crate::runtime::mimi_atomic_i64_fetch_add(h, d);
        Ok(Value::Int(prev))
    }

    pub(crate) fn builtin_atomic_i64_drop(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("atomic_i64_drop expects 1 argument"));
        }
        let h = args[0].as_i64_for_handle()?;
        // SAFETY: handle is a valid atomic_i64 handle.
        crate::runtime::mimi_atomic_i64_drop(h);
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_atomic_bool_new(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("atomic_bool_new expects 1 argument"));
        }
        let v = match &args[0] {
            Value::Bool(b) => {
                if *b {
                    1
                } else {
                    0
                }
            }
            _ => return Err(InterpError::new("atomic_bool_new expects bool")),
        };
        // SAFETY: passing i32 with 0/1.
        let handle = crate::runtime::mimi_atomic_bool_new(v);
        Ok(Value::Int(handle))
    }

    pub(crate) fn builtin_atomic_bool_load(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("atomic_bool_load expects 1 argument"));
        }
        let h = args[0].as_i64_for_handle()?;
        // SAFETY: handle is a valid atomic_bool handle.
        let v = crate::runtime::mimi_atomic_bool_load(h);
        Ok(Value::Bool(v != 0))
    }

    pub(crate) fn builtin_atomic_bool_store(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new(
                "atomic_bool_store expects 2 arguments (handle, bool)",
            ));
        }
        let h = args[0].as_i64_for_handle()?;
        let v = match &args[1] {
            Value::Bool(b) => {
                if *b {
                    1i32
                } else {
                    0i32
                }
            }
            _ => return Err(InterpError::new("atomic_bool_store expects bool")),
        };
        // SAFETY: handle is a valid atomic_bool handle.
        crate::runtime::mimi_atomic_bool_store(h, v);
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_atomic_bool_drop(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("atomic_bool_drop expects 1 argument"));
        }
        let h = args[0].as_i64_for_handle()?;
        // SAFETY: handle is a valid atomic_bool handle.
        crate::runtime::mimi_atomic_bool_drop(h);
        Ok(Value::Unit)
    }

    // ----- Mutex -----

    pub(crate) fn builtin_mutex_new(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("mutex_new expects 1 argument (i64 value)"));
        }
        let v = args[0].as_i64_for_handle()?;
        // SAFETY: passing a valid i64; runtime returns a valid handle.
        let h = crate::runtime::mimi_mutex_new(v);
        Ok(Value::Int(h))
    }

    pub(crate) fn builtin_mutex_lock(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("mutex_lock expects 1 argument"));
        }
        let h = args[0].as_i64_for_handle()?;
        // SAFETY: handle is a valid mutex handle.
        let token = crate::runtime::mimi_mutex_lock(h);
        Ok(Value::Int(token))
    }

    pub(crate) fn builtin_mutex_get(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new(
                "mutex_get expects 1 argument (lock token)",
            ));
        }
        let h = args[0].as_i64_for_handle()?;
        // SAFETY: handle is a valid mutex handle.
        let v = crate::runtime::mimi_mutex_get(h);
        Ok(Value::Int(v))
    }

    pub(crate) fn builtin_mutex_set(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new(
                "mutex_set expects 2 arguments (lock, value)",
            ));
        }
        let h = args[0].as_i64_for_handle()?;
        let v = match &args[1] {
            Value::Int(x) => *x,
            _ => return Err(InterpError::new("mutex_set expects i64 value")),
        };
        // SAFETY: handle is a valid mutex handle.
        crate::runtime::mimi_mutex_set(h, v);
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_mutex_unlock(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("mutex_unlock expects 1 argument"));
        }
        let h = args[0].as_i64_for_handle()?;
        // SAFETY: handle is a valid mutex handle.
        crate::runtime::mimi_mutex_unlock(h);
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_mutex_drop(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("mutex_drop expects 1 argument"));
        }
        let h = args[0].as_i64_for_handle()?;
        // SAFETY: handle is a valid mutex handle.
        crate::runtime::mimi_mutex_drop(h);
        Ok(Value::Unit)
    }

    // ----- Channel -----

    pub(crate) fn builtin_channel_new(&self, _args: Vec<Value>) -> Result<Value, InterpError> {
        // SAFETY: no parameters; runtime returns a valid i64 handle.
        let h = crate::runtime::mimi_channel_new();
        Ok(Value::Int(h))
    }

    pub(crate) fn builtin_channel_send(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new(
                "channel_send expects 2 arguments (handle, i64)",
            ));
        }
        let h = args[0].as_i64_for_handle()?;
        let v = match &args[1] {
            Value::Int(x) => *x,
            _ => return Err(InterpError::new("channel_send expects i64 value")),
        };
        // SAFETY: handle is a valid channel handle.
        crate::runtime::mimi_channel_send(h, v);
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_channel_recv(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("channel_recv expects 1 argument"));
        }
        let h = args[0].as_i64_for_handle()?;
        // SAFETY: handle is a valid channel handle.
        let v = crate::runtime::mimi_channel_recv(h);
        Ok(Value::Int(v))
    }

    pub(crate) fn builtin_channel_try_recv(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("channel_try_recv expects 1 argument"));
        }
        let h = args[0].as_i64_for_handle()?;
        // SAFETY: handle is a valid channel handle.
        let v = crate::runtime::mimi_channel_try_recv(h);
        Ok(Value::Int(v))
    }

    pub(crate) fn builtin_channel_drop(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("channel_drop expects 1 argument"));
        }
        let h = args[0].as_i64_for_handle()?;
        // SAFETY: handle is a valid channel handle.
        crate::runtime::mimi_channel_drop(h);
        Ok(Value::Unit)
    }

    // ----- Mailbox backpressure (v0.29.21) -----

    pub(crate) fn builtin_actor_mailbox_depth(
        &self,
        args: Vec<Value>,
    ) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("actor_mailbox_depth expects 1 argument"));
        }
        match &args[0] {
            Value::Actor(h) => Ok(Value::Int(h.mailbox_depth() as i64)),
            _ => Err(InterpError::new("actor_mailbox_depth expects actor handle")),
        }
    }

    pub(crate) fn builtin_actor_is_muted(
        &self,
        args: Vec<Value>,
    ) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("actor_is_muted expects 1 argument"));
        }
        match &args[0] {
            Value::Actor(h) => Ok(Value::Int(if h.is_muted() { 1 } else { 0 })),
            _ => Err(InterpError::new("actor_is_muted expects actor handle")),
        }
    }

    pub(crate) fn builtin_actor_set_mailbox_depth(
        &self,
        args: Vec<Value>,
    ) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new(
                "actor_set_mailbox_depth expects (actor, depth)",
            ));
        }
        let depth = match &args[1] {
            Value::Int(n) if *n > 0 => *n as usize,
            _ => {
                return Err(InterpError::new(
                    "actor_set_mailbox_depth: depth must be positive i64",
                ))
            }
        };
        match &args[0] {
            Value::Actor(h) => {
                h.set_mailbox_depth_limit(depth);
                Ok(Value::Unit)
            }
            _ => Err(InterpError::new(
                "actor_set_mailbox_depth expects actor handle",
            )),
        }
    }

    // ----- Spawn quota (v0.29.24) -----

    pub(crate) fn builtin_actor_set_max_children(
        &mut self,
        args: Vec<Value>,
    ) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new(
                "actor_set_max_children expects 1 argument (max, 0 = unlimited)",
            ));
        }
        let n = match &args[0] {
            Value::Int(x) if *x <= 0 => None,
            Value::Int(x) => Some(*x as usize),
            _ => {
                return Err(InterpError::new(
                    "actor_set_max_children expects i64",
                ))
            }
        };
        self.set_max_children(n);
        // Keep runtime counter in sync for dual-backend / mixed paths.
        crate::runtime::mimi_actor_set_max_children(n.map(|x| x as i64).unwrap_or(0));
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_actor_spawn_count(
        &self,
        args: Vec<Value>,
    ) -> Result<Value, InterpError> {
        if !args.is_empty() {
            return Err(InterpError::new("actor_spawn_count expects 0 arguments"));
        }
        Ok(Value::Int(self.spawn_count as i64))
    }

    pub(crate) fn builtin_actor_max_children(
        &self,
        args: Vec<Value>,
    ) -> Result<Value, InterpError> {
        if !args.is_empty() {
            return Err(InterpError::new("actor_max_children expects 0 arguments"));
        }
        Ok(Value::Int(self.max_children.map(|n| n as i64).unwrap_or(0)))
    }

    /// v0.29.37: spawn_detached(actor_name) — spawn a detached actor
    /// that survives parent SystemKill.
    pub(crate) fn builtin_spawn_detached(
        &mut self,
        args: Vec<Value>,
    ) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new(
                "spawn_detached expects 1 argument (actor type name)",
            ));
        }
        let name = match &args[0] {
            Value::String(s) => s.clone(),
            _ => return Err(InterpError::new("spawn_detached expects a string")),
        };
        self.spawn_detached_actor(&name)
    }

    /// v0.29.38: assert_state(flow_instance, state_name) — verify that a
    /// flow state record has the expected state name. Panics (returns Err)
    /// if the state doesn't match.
    pub(crate) fn builtin_assert_state(
        &self,
        args: Vec<Value>,
    ) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new(
                "assert_state expects 2 arguments (flow_instance, state_name)",
            ));
        }
        let actual_state = match &args[0] {
            Value::Record(Some(name), _) => name.clone(),
            Value::Record(None, _) => "<anonymous>".to_string(),
            other => format!("{:?}", other),
        };
        let expected_state = match &args[1] {
            Value::String(s) => s.clone(),
            _ => return Err(InterpError::new("assert_state: state_name must be a string")),
        };
        if actual_state != expected_state {
            return Err(InterpError::new(&format!(
                "assert_state failed: expected '{}', got '{}'",
                expected_state, actual_state
            )));
        }
        Ok(Value::Unit)
    }

    /// v0.29.38: inject_fault(flow_instance) — inject a Fault into a flow
    /// instance by returning a Fault record with SystemTrace.
    /// This is a test utility: it constructs a minimal Fault payload.
    pub(crate) fn builtin_inject_fault(
        &self,
        args: Vec<Value>,
    ) -> Result<Value, InterpError> {
        if args.is_empty() {
            return Err(InterpError::new(
                "inject_fault expects 1 argument (flow_instance)",
            ));
        }
        // Get the state name from the flow instance
        let state_name = match &args[0] {
            Value::Record(Some(name), _) => name.clone(),
            _ => "unknown".to_string(),
        };
        // Construct a Fault record
        let mut fault_fields = HashMap::new();
        fault_fields.insert("last_state".to_string(), Value::String(state_name.clone()));
        fault_fields.insert("unexpected_event".to_string(), Value::String("inject_fault".to_string()));
        fault_fields.insert("snapshot".to_string(), Value::String(String::new()));
        // SystemTrace sub-record
        let mut trace_fields = HashMap::new();
        trace_fields.insert("last_state_name".to_string(), Value::String(state_name));
        trace_fields.insert("unexpected_event".to_string(), Value::String("inject_fault".to_string()));
        trace_fields.insert("snapshot".to_string(), Value::String(String::new()));
        fault_fields.insert("trace".to_string(), Value::Record(Some("SystemTrace".to_string()), trace_fields));
        Ok(Value::Record(Some("Fault".to_string()), fault_fields))
    }

    /// v0.29.25: broadcast(targets, method_name) -> List of results.
    ///
    /// `targets` is a List of Actor handles (type-erased protocol set).
    /// For each target, invoke `method` with no extra args via mailbox.
    /// On success: Ok-like value (the method return).
    /// On fault/error: PeerFault-shaped record { peer_id, reason }.
    /// No 2PC — caller decides how to handle mixed results.
    pub(crate) fn builtin_broadcast(
        &mut self,
        args: Vec<Value>,
    ) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new(
                "broadcast expects 2 arguments (targets: List, method: string)",
            ));
        }
        let targets = match &args[0] {
            Value::List(items) => items.clone(),
            _ => {
                return Err(InterpError::new(
                    "broadcast: first argument must be a List of actors",
                ))
            }
        };
        let method = match &args[1] {
            Value::String(s) => s.clone(),
            _ => {
                return Err(InterpError::new(
                    "broadcast: second argument must be a method name string",
                ))
            }
        };

        let mut results = Vec::with_capacity(targets.len());
        for target in targets {
            match target {
                Value::Actor(handle) => {
                    if handle.is_faulted() {
                        // v0.29.35: PeerFault sentinel = -1
                        results.push(Value::Int(-1));
                        continue;
                    }
                    // Dispatch via existing method-call path (mailbox / self).
                    match self.call_method(
                        &Value::Actor(handle.clone()),
                        &method,
                        vec![],
                    ) {
                        Ok(v) => {
                            // v0.29.35: normalize to i64 for List<i64> result.
                            // If the method returned a non-i64, coerce to i64.
                            match v {
                                Value::Int(n) => results.push(Value::Int(n)),
                                _ => results.push(Value::Int(0)),
                            }
                        }
                        Err(_) => {
                            // v0.29.35: PeerFault sentinel = -1
                            results.push(Value::Int(-1));
                        }
                    }
                }
                other => {
                    // v0.29.35: non-actor target → PeerFault sentinel
                    results.push(Value::Int(-1));
                }
            }
        }
        Ok(Value::List(results))
    }
}

fn peer_fault_result(peer_id: &str, reason: &str) -> Value {
    use std::collections::HashMap;
    let mut fields = HashMap::new();
    fields.insert("peer_id".to_string(), Value::String(peer_id.to_string()));
    fields.insert("reason".to_string(), Value::String(reason.to_string()));
    Value::Record(Some("PeerFault".to_string()), fields)
}

/// Helper: extract the i64 payload of a Value::Int as a runtime handle id.
/// Returns InterpError on type mismatch so callers can short-circuit.
trait ValueAsI64 {
    fn as_i64_for_handle(&self) -> Result<i64, InterpError>;
}

impl ValueAsI64 for Value {
    fn as_i64_for_handle(&self) -> Result<i64, InterpError> {
        match self {
            Value::Int(x) => Ok(*x),
            _ => Err(InterpError::new(
                "expected an i64 handle (atomic / mutex / channel)",
            )),
        }
    }
}

