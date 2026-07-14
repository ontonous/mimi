use super::*;

impl<'a> Interpreter<'a> {
    // === JSON ===
    pub(crate) fn builtin_to_json(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("to_json expects 1 argument"));
        }
        let json_val = self
            .value_to_json(&args[0])
            .map_err(|e| InterpError::new(e.to_string()))?;
        let json_str = serde_json::to_string(&json_val)
            .map_err(|e| InterpError::new(format!("to_json error: {}", e)))?;
        Ok(Value::String(json_str))
    }

    pub(crate) fn builtin_json_is_valid(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new(
                "json_is_valid expects 1 argument (json string)",
            ));
        }
        match &args[0] {
            Value::String(s) => {
                let valid = serde_json::from_str::<serde_json::Value>(s).is_ok();
                Ok(Value::Bool(valid))
            }
            _ => Err(InterpError::new("json_is_valid expects a string")),
        }
    }

    pub(crate) fn builtin_from_json(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new(
                "from_json expects 1 argument (json string)",
            ));
        }
        match &args[0] {
            Value::String(s) => {
                // Validate JSON and return the string as-is (matches codegen behavior)
                let _: serde_json::Value = serde_json::from_str(s)
                    .map_err(|e| InterpError::new(format!("from_json parse error: {}", e)))?;
                Ok(Value::String(s.clone()))
            }
            _ => Err(InterpError::new("from_json expects a string")),
        }
    }

    pub(crate) fn builtin_json_get_string(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("json_get_string expects 2 arguments"));
        }
        match (&args[0], &args[1]) {
            (Value::String(json), Value::String(key)) => {
                let jv: serde_json::Value = serde_json::from_str(json)
                    .map_err(|e| InterpError::new(format!("json_get_string parse error: {}", e)))?;
                match jv.get(key) {
                    Some(serde_json::Value::String(s)) => Ok(Value::String(s.clone())),
                    Some(serde_json::Value::Bool(b)) => Ok(Value::String(if *b {
                        "true".into()
                    } else {
                        "false".into()
                    })),
                    Some(serde_json::Value::Number(n)) => Ok(Value::String(n.to_string())),
                    Some(serde_json::Value::Null) => Ok(Value::String("null".into())),
                    // For arrays, objects, and other types, return JSON representation
                    Some(val) => Ok(Value::String(val.to_string())),
                    None => Ok(Value::String(String::new())),
                }
            }
            _ => Err(InterpError::new("json_get_string expects (string, string)")),
        }
    }

    pub(crate) fn builtin_json_get_int(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("json_get_int expects 2 arguments"));
        }
        match (&args[0], &args[1]) {
            (Value::String(json), Value::String(key)) => {
                let jv: serde_json::Value = serde_json::from_str(json)
                    .map_err(|e| InterpError::new(format!("json_get_int parse error: {}", e)))?;
                match jv.get(key) {
                    Some(serde_json::Value::Number(n)) => {
                        n.as_i64().map(Value::Int).ok_or_else(|| {
                            InterpError::new(format!(
                                "json_get_int: value for key '{}' is not an integer",
                                key
                            ))
                        })
                    }
                    Some(_) => Err(InterpError::new(format!(
                        "json_get_int: key '{}' is not a number",
                        key
                    ))),
                    None => Err(InterpError::new(format!(
                        "json_get_int: key '{}' not found",
                        key
                    ))),
                }
            }
            _ => Err(InterpError::new("json_get_int expects (string, string)")),
        }
    }

    pub(crate) fn builtin_json_array_length(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("json_array_length expects 1 argument"));
        }
        match &args[0] {
            Value::String(json) => {
                let jv: serde_json::Value = serde_json::from_str(json).map_err(|e| {
                    InterpError::new(format!("json_array_length parse error: {}", e))
                })?;
                match jv {
                    serde_json::Value::Array(arr) => Ok(Value::Int(arr.len() as i64)),
                    _ => Err(InterpError::new("json_array_length: value is not an array")),
                }
            }
            _ => Err(InterpError::new("json_array_length expects a string")),
        }
    }

    pub(crate) fn builtin_json_get_element(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("json_get_element expects 2 arguments"));
        }
        match (&args[0], &args[1]) {
            (Value::String(json), Value::Int(idx)) => {
                let jv: serde_json::Value = serde_json::from_str(json).map_err(|e| {
                    InterpError::new(format!("json_get_element parse error: {}", e))
                })?;
                let index = *idx as usize;
                match jv.get(index) {
                    Some(val) => Ok(Value::String(val.to_string())),
                    None => Ok(Value::String(String::new())),
                }
            }
            _ => Err(InterpError::new("json_get_element expects (string, int)")),
        }
    }

    /// CRITICAL #18 fix: json_has_key checks whether a key exists in a JSON
    /// object, returning true/false unambiguously. Previously, has_key used
    /// `json_get_string(self, key) != ""` which incorrectly returns false
    /// when the key exists but its value is an empty string "".
    pub(crate) fn builtin_json_has_key(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("json_has_key expects 2 arguments"));
        }
        match (&args[0], &args[1]) {
            (Value::String(json), Value::String(key)) => {
                let jv: serde_json::Value = serde_json::from_str(json)
                    .map_err(|e| InterpError::new(format!("json_has_key parse error: {}", e)))?;
                Ok(Value::Bool(
                    jv.as_object().is_some_and(|obj| obj.contains_key(key)),
                ))
            }
            _ => Err(InterpError::new("json_has_key expects (string, string)")),
        }
    }
}
