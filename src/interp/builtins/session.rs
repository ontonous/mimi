use crate::interp::value::Value;
use crate::interp::InterpError;

impl<'a> super::Interpreter<'a> {
    /// v0.29.28: session_pair() -> List<i64> of two channel handles.
    pub(crate) fn builtin_session_pair(
        &self,
        args: Vec<Value>,
    ) -> Result<Value, InterpError> {
        if !args.is_empty() {
            return Err(InterpError::new("session_pair expects 0 arguments"));
        }
        Ok(Value::List(vec![
            Value::Int(crate::runtime::mimi_channel_new() as i64),
            Value::Int(crate::runtime::mimi_channel_new() as i64),
        ]))
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
