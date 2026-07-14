use super::*;

impl<'a> Interpreter<'a> {
    pub(crate) fn builtin_keys(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("keys expects 1 argument (record)"));
        }
        match &args[0] {
            Value::Record(_, fields) => {
                let keys: Vec<Value> = fields.keys().map(|k| Value::String(k.clone())).collect();
                Ok(Value::List(keys))
            }
            _ => Err(InterpError::new("keys expects a record")),
        }
    }

    pub(crate) fn builtin_values(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("values expects 1 argument (record)"));
        }
        match &args[0] {
            Value::Record(_, fields) => Ok(Value::List(fields.values().cloned().collect())),
            _ => Err(InterpError::new("values expects a record")),
        }
    }

    pub(crate) fn builtin_has_key(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new(
                "has_key expects 2 arguments (record, key)",
            ));
        }
        match (&args[0], &args[1]) {
            (Value::Record(_, fields), Value::String(key)) => {
                Ok(Value::Bool(fields.contains_key(key.as_str())))
            }
            _ => Err(InterpError::new("has_key expects (record, string)")),
        }
    }
    // === Map/Record utilities ===
    pub(crate) fn builtin_map_new(&self, _args: Vec<Value>) -> Result<Value, InterpError> {
        Ok(Value::Record(None, std::collections::HashMap::new()))
    }

    pub(crate) fn builtin_map_get(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("map_get expects 2 arguments (map, key)"));
        }
        match (&args[0], &args[1]) {
            (Value::Record(_, fields), Value::String(key)) => match fields.get(key.as_str()) {
                Some(v) => Ok(Value::Tuple(vec![Value::Bool(true), v.clone()])),
                // Match codegen: missing key → (false, 0) ValueHandle, not Unit.
                None => Ok(Value::Tuple(vec![Value::Bool(false), Value::Int(0)])),
            },
            _ => Err(InterpError::new("map_get expects (record, string)")),
        }
    }

    pub(crate) fn builtin_map_set(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 3 {
            return Err(InterpError::new(
                "map_set expects 3 arguments (map, key, value)",
            ));
        }
        match (&args[0], &args[1]) {
            (Value::Record(type_name, fields), Value::String(key)) => {
                let mut new_fields = fields.clone();
                new_fields.insert(key.clone(), args[2].clone());
                Ok(Value::Record(type_name.clone(), new_fields))
            }
            _ => Err(InterpError::new("map_set expects (record, string, value)")),
        }
    }

    pub(crate) fn builtin_map_remove(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new(
                "map_remove expects 2 arguments (map, key)",
            ));
        }
        match (&args[0], &args[1]) {
            (Value::Record(type_name, fields), Value::String(key)) => {
                let mut new_fields = fields.clone();
                new_fields.remove(key.as_str());
                Ok(Value::Record(type_name.clone(), new_fields))
            }
            _ => Err(InterpError::new("map_remove expects (record, string)")),
        }
    }

    pub(crate) fn builtin_map_size(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("map_size expects 1 argument"));
        }
        match &args[0] {
            Value::Record(_, fields) => Ok(Value::Int(fields.len() as i64)),
            _ => Err(InterpError::new("map_size expects a record")),
        }
    }

    pub(crate) fn builtin_map_from_list(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new(
                "map_from_list expects 1 argument (list of (key, value) tuples)",
            ));
        }
        match &args[0] {
            Value::List(pairs) => {
                let mut fields = std::collections::HashMap::new();
                for pair in pairs {
                    match pair {
                        Value::Tuple(vec) if vec.len() == 2 => {
                            if let Value::String(key) = &vec[0] {
                                fields.insert(key.clone(), vec[1].clone());
                            } else {
                                return Err(InterpError::new(
                                    "map_from_list: keys must be strings",
                                ));
                            }
                        }
                        _ => {
                            return Err(InterpError::new(
                                "map_from_list: elements must be (string, value) tuples",
                            ))
                        }
                    }
                }
                Ok(Value::Record(None, fields))
            }
            _ => Err(InterpError::new("map_from_list expects a list")),
        }
    }
}
