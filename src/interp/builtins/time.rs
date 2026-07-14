use super::*;

impl<'a> Interpreter<'a> {
    // === Time ===
    pub(crate) fn builtin_now(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if !args.is_empty() {
            return Err(InterpError::new("now/timestamp expects 0 arguments"));
        }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| InterpError::new(format!("time error: {}", e)))?
            .as_secs() as i64;
        Ok(Value::Int(ts))
    }

    pub(crate) fn builtin_now_ms(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if !args.is_empty() {
            return Err(InterpError::new("now_ms/timestamp_ms expects 0 arguments"));
        }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| InterpError::new(format!("time error: {}", e)))?
            .as_millis() as i64;
        Ok(Value::Int(ts))
    }

    pub(crate) fn builtin_sleep(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("sleep expects 1 argument (milliseconds)"));
        }
        match &args[0] {
            Value::Int(ms) => {
                // IP-H2: negative ms must not wrap to ~5.84e11 years via `as u64`.
                if *ms < 0 {
                    return Err(InterpError::new(
                        "sleep expects a non-negative integer (milliseconds)",
                    ));
                }
                // Cap absurd durations (e.g. i64::MAX ms) to 24h to avoid DoS.
                const MAX_SLEEP_MS: u64 = 24 * 60 * 60 * 1000;
                let ms_u = (*ms as u64).min(MAX_SLEEP_MS);
                std::thread::sleep(std::time::Duration::from_millis(ms_u));
                Ok(Value::Unit)
            }
            _ => Err(InterpError::new("sleep expects an integer (milliseconds)")),
        }
    }
}
