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
            return Err(InterpError::new(
                "atomic_i64_fetch_add expects 2 arguments",
            ));
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
            return Err(InterpError::new("mutex_get expects 1 argument (lock token)"));
        }
        let h = args[0].as_i64_for_handle()?;
        // SAFETY: handle is a valid mutex handle.
        let v = crate::runtime::mimi_mutex_get(h);
        Ok(Value::Int(v))
    }

    pub(crate) fn builtin_mutex_set(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("mutex_set expects 2 arguments (lock, value)"));
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
