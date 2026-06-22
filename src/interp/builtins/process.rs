use super::*;

impl<'a> Interpreter<'a> {
    // === Process control ===
    pub(crate) fn builtin_exit(&mut self, args: Vec<Value>) -> Result<Value, InterpError> {
        let code = if args.is_empty() {
            0
        } else {
            match &args[0] {
                Value::Int(n) => *n as i32,
                _ => return Err(InterpError::new("exit expects an integer exit code")),
            }
        };
        // Set the exit signal so the interpreter stops gracefully instead of
        // killing the host process (important for tests and embedded use).
        self.exited = Some(code);
        Ok(Value::Unit)
    }
}
