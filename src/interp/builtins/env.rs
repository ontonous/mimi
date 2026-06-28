use super::*;

impl<'a> Interpreter<'a> {
    // === Environment ===
    pub(crate) fn builtin_getenv(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("getenv expects 1 argument (name)"));
        }
        match &args[0] {
            Value::String(name) => match std::env::var(name) {
                Ok(val) => Ok(Value::Variant("Ok".into(), vec![Value::String(val)])),
                Err(_) => Ok(Value::Variant(
                    "Err".into(),
                    vec![Value::String(format!("env var '{}' not set", name))],
                )),
            },
            _ => Err(InterpError::new("getenv expects a string name")),
        }
    }

    pub(crate) fn builtin_args(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if !args.is_empty() {
            return Err(InterpError::new("args expects 0 arguments"));
        }
        let cli_args: Vec<Value> = self
            .cli_args
            .iter()
            .map(|s| Value::String(s.clone()))
            .collect();
        Ok(Value::List(cli_args))
    }
}
