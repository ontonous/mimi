//! Miscellaneous builtins: JSON, crypto, testing, assertions, misc IO.

use crate::interp::bytecode::registry::{BuiltinCategory, BuiltinDesc, BuiltinRegistry};
use crate::interp::bytecode::vm::BytecodeVM;
use crate::interp::error::InterpError;
use crate::interp::value::Value;

pub fn register(reg: &mut BuiltinRegistry) {
    // JSON
    reg.register(BuiltinDesc { name: "to_json", arity: 1, category: BuiltinCategory::System, func: builtin_to_json });
    reg.register(BuiltinDesc { name: "from_json", arity: 1, category: BuiltinCategory::System, func: builtin_from_json });
    reg.register(BuiltinDesc { name: "json_get_string", arity: 2, category: BuiltinCategory::System, func: builtin_json_get_string });
    reg.register(BuiltinDesc { name: "json_get_int", arity: 2, category: BuiltinCategory::System, func: builtin_json_get_int });
    reg.register(BuiltinDesc { name: "json_is_valid", arity: 1, category: BuiltinCategory::System, func: builtin_json_is_valid });
    // Crypto
    reg.register(BuiltinDesc { name: "sha256", arity: 1, category: BuiltinCategory::System, func: builtin_sha256 });
    reg.register(BuiltinDesc { name: "base64_encode", arity: 1, category: BuiltinCategory::System, func: builtin_base64_encode });
    reg.register(BuiltinDesc { name: "base64_decode", arity: 1, category: BuiltinCategory::System, func: builtin_base64_decode });
    // Testing / assertions
    reg.register(BuiltinDesc { name: "assert", arity: 1, category: BuiltinCategory::System, func: builtin_assert });
    reg.register(BuiltinDesc { name: "assert_eq", arity: 2, category: BuiltinCategory::System, func: builtin_assert_eq });
    reg.register(BuiltinDesc { name: "assert_ne", arity: 2, category: BuiltinCategory::System, func: builtin_assert_ne });
    reg.register(BuiltinDesc { name: "assert_approx_eq", arity: 2, category: BuiltinCategory::System, func: builtin_assert_approx_eq });
    // IO misc
    reg.register(BuiltinDesc { name: "eprintln", arity: usize::MAX, category: BuiltinCategory::Io, func: builtin_eprintln });
    reg.register(BuiltinDesc { name: "input", arity: 0, category: BuiltinCategory::Io, func: builtin_input });
    reg.register(BuiltinDesc { name: "input_float", arity: 0, category: BuiltinCategory::Io, func: builtin_input_float });
    reg.register(BuiltinDesc { name: "input_bool", arity: 0, category: BuiltinCategory::Io, func: builtin_input_bool });
    // Convert misc
    reg.register(BuiltinDesc { name: "from_int", arity: 1, category: BuiltinCategory::Convert, func: builtin_from_int });
}

// ── JSON ────────────────────────────────────────────────

fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Int(i) => serde_json::Value::Number((*i).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::Unit => serde_json::Value::Null,
        Value::List(l) => serde_json::Value::Array(l.iter().map(value_to_json).collect()),
        Value::Record(_, fields) => {
            let map: serde_json::Map<String, serde_json::Value> = fields
                .iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect();
            serde_json::Value::Object(map)
        }
        Value::Tuple(t) => serde_json::Value::Array(t.iter().map(value_to_json).collect()),
        _ => serde_json::Value::String(v.to_string()),
    }
}

fn json_to_value(j: &serde_json::Value) -> Value {
    match j {
        serde_json::Value::Null => Value::Unit,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Unit
            }
        }
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Array(arr) => Value::List(arr.iter().map(json_to_value).collect()),
        serde_json::Value::Object(map) => {
            let fields: std::collections::HashMap<String, Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), json_to_value(v)))
                .collect();
            Value::Record(None, fields)
        }
    }
}

fn builtin_to_json(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let json = value_to_json(&args[0]);
    Ok(Value::String(json.to_string()))
}

fn builtin_from_json(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::String(s) => {
            match serde_json::from_str::<serde_json::Value>(s) {
                Ok(json) => Ok(Value::Variant("Ok".into(), vec![json_to_value(&json)])),
                Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
            }
        }
        _ => Err(InterpError::new("from_json expects a string")),
    }
}

fn builtin_json_get_string(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::String(json_str), Value::String(key)) => {
            match serde_json::from_str::<serde_json::Value>(json_str) {
                Ok(json) => {
                    match json.get(key).and_then(|v| v.as_str()) {
                        Some(s) => Ok(Value::Variant("Some".into(), vec![Value::String(s.to_string())])),
                        None => Ok(Value::Variant("None".into(), vec![])),
                    }
                }
                Err(_) => Ok(Value::Variant("None".into(), vec![])),
            }
        }
        _ => Err(InterpError::new("json_get_string expects (string, string)")),
    }
}

fn builtin_json_get_int(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::String(json_str), Value::String(key)) => {
            match serde_json::from_str::<serde_json::Value>(json_str) {
                Ok(json) => {
                    match json.get(key).and_then(|v| v.as_i64()) {
                        Some(i) => Ok(Value::Variant("Some".into(), vec![Value::Int(i)])),
                        None => Ok(Value::Variant("None".into(), vec![])),
                    }
                }
                Err(_) => Ok(Value::Variant("None".into(), vec![])),
            }
        }
        _ => Err(InterpError::new("json_get_int expects (string, string)")),
    }
}

fn builtin_json_is_valid(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::String(s) => Ok(Value::Bool(serde_json::from_str::<serde_json::Value>(s).is_ok())),
        _ => Err(InterpError::new("json_is_valid expects a string")),
    }
}

// ── Crypto ──────────────────────────────────────────────

fn builtin_sha256(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::String(data) => {
            let hash = crate::runtime::sha256_bytes(data.as_bytes());
            let hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
            Ok(Value::String(hex))
        }
        _ => Err(InterpError::new("sha256 expects a string")),
    }
}

fn builtin_base64_encode(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::String(data) => {
            let encoded = crate::runtime::base64_encode_bytes(data.as_bytes());
            Ok(Value::String(encoded))
        }
        _ => Err(InterpError::new("base64_encode expects a string")),
    }
}

fn builtin_base64_decode(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::String(data) => match crate::runtime::base64_decode_str(data) {
            Ok(decoded) => Ok(Value::Variant("Ok".into(), vec![Value::String(decoded)])),
            Err(_) => Ok(Value::Variant("Err".into(), vec![Value::String("invalid base64".to_string())])),
        },
        _ => Err(InterpError::new("base64_decode expects a string")),
    }
}

// ── Testing / assertions ────────────────────────────────

fn builtin_assert(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    if !crate::interp::is_truthy(&args[0]) {
        return Err(InterpError::new("assertion failed"));
    }
    Ok(Value::Unit)
}

fn builtin_assert_eq(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    if !crate::interp::values_equal(&args[0], &args[1]) {
        return Err(InterpError::new(format!(
            "assertion failed: {} != {}",
            args[0], args[1]
        )));
    }
    Ok(Value::Unit)
}

fn builtin_assert_ne(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    if crate::interp::values_equal(&args[0], &args[1]) {
        return Err(InterpError::new(format!(
            "assertion failed: {} == {}",
            args[0], args[1]
        )));
    }
    Ok(Value::Unit)
}

fn builtin_assert_approx_eq(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let a = match &args[0] { Value::Float(f) => *f, Value::Int(i) => *i as f64, _ => return Err(InterpError::new("assert_approx_eq expects numbers")) };
    let b = match &args[1] { Value::Float(f) => *f, Value::Int(i) => *i as f64, _ => return Err(InterpError::new("assert_approx_eq expects numbers")) };
    if (a - b).abs() > 1e-6 {
        return Err(InterpError::new(format!(
            "assertion failed: {} !≈ {}",
            a, b
        )));
    }
    Ok(Value::Unit)
}

// ── IO misc ─────────────────────────────────────────────

fn builtin_eprintln(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let parts: Vec<String> = args.iter().map(|v| v.to_string()).collect();
    eprintln!("{}", parts.join(" "));
    Ok(Value::Unit)
}

fn builtin_input(_vm: &mut BytecodeVM<'_>, _args: &[Value]) -> Result<Value, InterpError> {
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)
        .map_err(|e| InterpError::new(format!("input error: {}", e)))?;
    Ok(Value::String(input.trim_end().to_string()))
}

fn builtin_input_float(_vm: &mut BytecodeVM<'_>, _args: &[Value]) -> Result<Value, InterpError> {
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)
        .map_err(|e| InterpError::new(format!("input_float error: {}", e)))?;
    match input.trim().parse::<f64>() {
        Ok(n) => Ok(Value::Float(n)),
        Err(_) => Ok(Value::Float(0.0)),
    }
}

fn builtin_input_bool(_vm: &mut BytecodeVM<'_>, _args: &[Value]) -> Result<Value, InterpError> {
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)
        .map_err(|e| InterpError::new(format!("input_bool error: {}", e)))?;
    let trimmed = input.trim().to_lowercase();
    Ok(Value::Bool(trimmed == "true" || trimmed == "1" || trimmed == "yes"))
}

// ── Convert misc ────────────────────────────────────────

fn builtin_from_int(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::Int(i) => Ok(Value::Float(*i as f64)),
        Value::Float(f) => Ok(Value::Float(*f)),
        _ => Err(InterpError::new("from_int expects a number")),
    }
}
