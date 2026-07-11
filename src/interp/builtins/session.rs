use crate::interp::value::Value;
use crate::interp::InterpError;

impl<'a> super::Interpreter<'a> {
    /// v0.29.28: session_pair() -> List<i64> of two cross-wired channel handles.
    /// v0.29.34: Now matches runtime mimi_session_pair semantics — cross-wired.
    pub(crate) fn builtin_session_pair(
        &self,
        args: Vec<Value>,
    ) -> Result<Value, InterpError> {
        if !args.is_empty() {
            return Err(InterpError::new("session_pair expects 0 arguments"));
        }
        // Delegate to the runtime function which creates cross-wired channels.
        let packed = crate::runtime::mimi_session_pair();
        let lo = crate::runtime::mimi_session_lo(packed);
        let hi = crate::runtime::mimi_session_hi(packed);
        Ok(Value::List(vec![
            Value::Int(lo),
            Value::Int(hi),
        ]))
    }

    /// v0.29.34: session_send(ch, val) — sends val through the session channel.
    /// The session type residual is advanced at compile time; here we just
    /// perform the actual channel send (i64 values, like channel_send).
    pub(crate) fn builtin_session_send(
        &self,
        args: Vec<Value>,
    ) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("session_send expects 2 arguments (channel, value)"));
        }
        let h = match &args[0] {
            Value::Int(n) => *n,
            _ => return Err(InterpError::new("session_send: channel handle must be integer")),
        };
        let v = match &args[1] {
            Value::Int(x) => *x,
            _ => return Err(InterpError::new("session_send expects i64 value")),
        };
        crate::runtime::mimi_channel_send(h, v);
        Ok(Value::Unit)
    }

    /// v0.29.34: session_recv(ch) — receives a value from the session channel.
    pub(crate) fn builtin_session_recv(
        &self,
        args: Vec<Value>,
    ) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("session_recv expects 1 argument (channel)"));
        }
        let h = match &args[0] {
            Value::Int(n) => *n,
            _ => return Err(InterpError::new("session_recv: channel handle must be integer")),
        };
        let v = crate::runtime::mimi_channel_recv(h);
        Ok(Value::Int(v))
    }

    /// v0.29.34: session_close(ch) — drops the session channel.
    pub(crate) fn builtin_session_close(
        &self,
        args: Vec<Value>,
    ) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("session_close expects 1 argument (channel)"));
        }
        let h = match &args[0] {
            Value::Int(n) => *n,
            _ => return Err(InterpError::new("session_close: channel handle must be integer")),
        };
        crate::runtime::mimi_channel_drop(h);
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_protocol_methods(
        &self,
        args: Vec<Value>,
    ) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("protocol_methods expects 1 argument (protocol name)"));
        }
        let name = if let Value::String(s) = &args[0] {
            s.clone()
        } else {
            return Err(InterpError::new("protocol_methods expects a string"));
        };
        let methods: Vec<Value> = self
            .file
            .items
            .iter()
            .find_map(|item| match item {
                crate::ast::Item::Protocol(p) if p.name == name => {
                    Some(p.transitions.iter().map(|t| Value::String(t.name.clone())).collect())
                }
                _ => None,
            })
            .unwrap_or(Vec::new());
        Ok(Value::List(methods))
    }

}
