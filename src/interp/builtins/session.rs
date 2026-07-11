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
}
