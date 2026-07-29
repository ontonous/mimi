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
    // JSON extended
    reg.register(BuiltinDesc { name: "json_get_element", arity: 2, category: BuiltinCategory::System, func: builtin_json_get_element });
    reg.register(BuiltinDesc { name: "json_has_key", arity: 2, category: BuiltinCategory::System, func: builtin_json_has_key });
    reg.register(BuiltinDesc { name: "json_array_length", arity: 1, category: BuiltinCategory::System, func: builtin_json_array_length });
    // Regex extended
    reg.register(BuiltinDesc { name: "regex_find_all", arity: 2, category: BuiltinCategory::String, func: builtin_regex_find_all });
    // Misc value ops
    reg.register(BuiltinDesc { name: "eq", arity: 2, category: BuiltinCategory::System, func: builtin_eq });
    reg.register(BuiltinDesc { name: "bind", arity: 2, category: BuiltinCategory::System, func: builtin_bind });
    reg.register(BuiltinDesc { name: "inner", arity: 1, category: BuiltinCategory::System, func: builtin_inner });
    reg.register(BuiltinDesc { name: "deref", arity: 1, category: BuiltinCategory::System, func: builtin_inner });
    reg.register(BuiltinDesc { name: "fields", arity: 1, category: BuiltinCategory::System, func: builtin_fields });
    reg.register(BuiltinDesc { name: "type_fields", arity: 1, category: BuiltinCategory::System, func: builtin_fields });
    reg.register(BuiltinDesc { name: "type_variants", arity: 1, category: BuiltinCategory::System, func: builtin_type_variants });
    reg.register(BuiltinDesc { name: "assert_state", arity: 2, category: BuiltinCategory::System, func: builtin_assert_state });
    // C string
    reg.register(BuiltinDesc { name: "str_to_c_str", arity: 1, category: BuiltinCategory::String, func: builtin_str_to_c_str });
    reg.register(BuiltinDesc { name: "c_str_to_string", arity: 1, category: BuiltinCategory::String, func: builtin_c_str_to_string });
    // Process
    reg.register(BuiltinDesc { name: "exec", arity: usize::MAX, category: BuiltinCategory::System, func: builtin_exec });
    reg.register(BuiltinDesc { name: "exec_pipe", arity: usize::MAX, category: BuiltinCategory::System, func: builtin_exec });
    reg.register(BuiltinDesc { name: "exec_safe", arity: usize::MAX, category: BuiltinCategory::System, func: builtin_exec });
    // FS extended
    reg.register(BuiltinDesc { name: "file_stat", arity: 1, category: BuiltinCategory::System, func: builtin_file_stat });
    reg.register(BuiltinDesc { name: "read_file_bytes", arity: 1, category: BuiltinCategory::System, func: builtin_read_file_bytes });
    reg.register(BuiltinDesc { name: "write_file_bytes", arity: 2, category: BuiltinCategory::System, func: builtin_write_file_bytes });
    reg.register(BuiltinDesc { name: "close_fd", arity: 1, category: BuiltinCategory::System, func: builtin_close_fd });
    reg.register(BuiltinDesc { name: "read_file_partial", arity: 3, category: BuiltinCategory::System, func: builtin_read_file_partial });
    reg.register(BuiltinDesc { name: "read_lines_each", arity: 1, category: BuiltinCategory::System, func: builtin_read_lines_each });
    reg.register(BuiltinDesc { name: "read_lines_json", arity: 1, category: BuiltinCategory::System, func: builtin_read_lines_each });
    reg.register(BuiltinDesc { name: "read_lines_json_builtin", arity: 1, category: BuiltinCategory::System, func: builtin_read_lines_each });
    // Regex extended
    reg.register(BuiltinDesc { name: "regex_capture_groups", arity: 2, category: BuiltinCategory::String, func: builtin_regex_capture_groups });
    // Shadow memory
    reg.register(BuiltinDesc { name: "shadow_alloc", arity: 3, category: BuiltinCategory::System, func: builtin_shadow_alloc });
    reg.register(BuiltinDesc { name: "shadow_tag", arity: 2, category: BuiltinCategory::System, func: builtin_shadow_tag });
    reg.register(BuiltinDesc { name: "shadow_check", arity: 2, category: BuiltinCategory::System, func: builtin_shadow_check });
    reg.register(BuiltinDesc { name: "shadow_free", arity: 1, category: BuiltinCategory::System, func: builtin_shadow_free });
    // Allocator (no-op in interpreter)
    reg.register(BuiltinDesc { name: "alloc", arity: usize::MAX, category: BuiltinCategory::System, func: builtin_alloc_noop });
    reg.register(BuiltinDesc { name: "allocator_arena", arity: 0, category: BuiltinCategory::System, func: builtin_alloc_noop });
    reg.register(BuiltinDesc { name: "allocator_bump", arity: 0, category: BuiltinCategory::System, func: builtin_alloc_noop });
    reg.register(BuiltinDesc { name: "allocator_system", arity: 0, category: BuiltinCategory::System, func: builtin_alloc_noop });
    reg.register(BuiltinDesc { name: "arena_reset", arity: 1, category: BuiltinCategory::System, func: builtin_alloc_noop });
    reg.register(BuiltinDesc { name: "bump_used", arity: 1, category: BuiltinCategory::System, func: builtin_alloc_noop });
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
                Ok(json) => Ok(json_to_value(&json)),
                Err(e) => Err(InterpError::new(format!("from_json parse error: {}", e))),
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

// ── JSON extended ───────────────────────────────────────

fn builtin_json_get_element(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::String(json_str), Value::Int(idx)) => {
            match serde_json::from_str::<serde_json::Value>(json_str) {
                Ok(json) => {
                    match json.get(*idx as usize) {
                        Some(v) => Ok(Value::Variant("Some".into(), vec![json_to_value(v)])),
                        None => Ok(Value::Variant("None".into(), vec![])),
                    }
                }
                Err(_) => Ok(Value::Variant("None".into(), vec![])),
            }
        }
        _ => Err(InterpError::new("json_get_element expects (string, int)")),
    }
}

fn builtin_json_has_key(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::String(json_str), Value::String(key)) => {
            match serde_json::from_str::<serde_json::Value>(json_str) {
                Ok(json) => Ok(Value::Bool(json.get(key).is_some())),
                Err(_) => Ok(Value::Bool(false)),
            }
        }
        _ => Err(InterpError::new("json_has_key expects (string, string)")),
    }
}

fn builtin_json_array_length(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::String(json_str) => {
            match serde_json::from_str::<serde_json::Value>(json_str) {
                Ok(json) => {
                    match json.as_array() {
                        Some(arr) => Ok(Value::Int(arr.len() as i64)),
                        None => Ok(Value::Int(0)),
                    }
                }
                Err(_) => Ok(Value::Int(0)),
            }
        }
        _ => Err(InterpError::new("json_array_length expects a string")),
    }
}

// ── Regex extended ──────────────────────────────────────

fn builtin_regex_find_all(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::String(text), Value::String(pattern)) => {
            let re = regex::Regex::new(pattern)
                .map_err(|e| InterpError::new(format!("regex error: {}", e)))?;
            let matches: Vec<Value> = re.find_iter(text)
                .map(|m| Value::String(m.as_str().to_string()))
                .collect();
            Ok(Value::List(matches))
        }
        _ => Err(InterpError::new("regex_find_all expects (string, string)")),
    }
}

// ── Misc value ops ──────────────────────────────────────

fn builtin_eq(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    Ok(Value::Bool(crate::interp::values_equal(&args[0], &args[1])))
}

fn builtin_bind(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    // bind(value, fn) — apply fn to value (monadic bind).
    // For now, just return the value.
    Ok(args[0].clone())
}

fn builtin_inner(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    // inner/deref — unwrap a wrapper type.
    match &args[0] {
        Value::Variant(_, payload) => Ok(payload.first().cloned().unwrap_or(Value::Unit)),
        Value::Newtype(_, inner) => Ok(*inner.clone()),
        other => Ok(other.clone()),
    }
}

fn builtin_fields(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::Record(_, fields) => {
            let keys: Vec<Value> = fields.keys().map(|k| Value::String(k.clone())).collect();
            Ok(Value::List(keys))
        }
        _ => Ok(Value::List(vec![])),
    }
}

fn builtin_type_variants(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::Variant(name, _) => Ok(Value::List(vec![Value::String(name.clone())])),
        _ => Ok(Value::List(vec![])),
    }
}

fn builtin_assert_state(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    // assert_state(actual, expected) — assert that actual matches expected state.
    if !crate::interp::values_equal(&args[0], &args[1]) {
        return Err(InterpError::new(format!(
            "state assertion failed: expected {}, got {}",
            args[1], args[0]
        )));
    }
    Ok(Value::Unit)
}

// ── C string ────────────────────────────────────────────

fn builtin_str_to_c_str(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    // In the interpreter, C strings are just regular strings.
    Ok(args[0].clone())
}

fn builtin_c_str_to_string(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    Ok(args[0].clone())
}

// ── Process ─────────────────────────────────────────────

fn builtin_exec(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    if args.is_empty() {
        return Err(InterpError::new("exec expects at least 1 argument (command)"));
    }
    let cmd = args[0].to_string();
    let cmd_args: Vec<String> = args[1..].iter().map(|a| a.to_string()).collect();
    match std::process::Command::new(&cmd).args(&cmd_args).output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let status = output.status.code().unwrap_or(-1);
            Ok(Value::Tuple(vec![
                Value::Int(status as i64),
                Value::String(stdout),
            ]))
        }
        Err(e) => Ok(Value::Tuple(vec![
            Value::Int(-1),
            Value::String(e.to_string()),
        ])),
    }
}

// ── FS extended ─────────────────────────────────────────

fn builtin_file_stat(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::String(path) => {
            match std::fs::metadata(path) {
                Ok(meta) => {
                    let mut fields = std::collections::HashMap::new();
                    fields.insert("size".to_string(), Value::Int(meta.len() as i64));
                    fields.insert("is_dir".to_string(), Value::Bool(meta.is_dir()));
                    fields.insert("is_file".to_string(), Value::Bool(meta.is_file()));
                    Ok(Value::Record(None, fields))
                }
                Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
            }
        }
        _ => Err(InterpError::new("file_stat expects a string path")),
    }
}

fn builtin_read_file_bytes(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::String(path) => {
            match std::fs::read(path) {
                Ok(bytes) => {
                    let list: Vec<Value> = bytes.iter().map(|b| Value::Int(*b as i64)).collect();
                    Ok(Value::Variant("Ok".into(), vec![Value::List(list)]))
                }
                Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
            }
        }
        _ => Err(InterpError::new("read_file_bytes expects a string path")),
    }
}

fn builtin_write_file_bytes(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::String(path), Value::List(bytes)) => {
            let data: Vec<u8> = bytes.iter().filter_map(|b| match b {
                Value::Int(i) => Some(*i as u8),
                _ => None,
            }).collect();
            match std::fs::write(path, &data) {
                Ok(()) => Ok(Value::Variant("Ok".into(), vec![Value::Unit])),
                Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
            }
        }
        _ => Err(InterpError::new("write_file_bytes expects (string, list of ints)")),
    }
}

fn builtin_close_fd(_vm: &mut BytecodeVM<'_>, _args: &[Value]) -> Result<Value, InterpError> {
    // No-op in the interpreter.
    Ok(Value::Unit)
}

fn builtin_read_file_partial(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1], &args[2]) {
        (Value::String(path), Value::Int(offset), Value::Int(len)) => {
            match std::fs::read(path) {
                Ok(data) => {
                    let start = (*offset as usize).min(data.len());
                    let end = (start + *len as usize).min(data.len());
                    let slice = &data[start..end];
                    let content = String::from_utf8_lossy(slice).to_string();
                    Ok(Value::Variant("Ok".into(), vec![Value::String(content)]))
                }
                Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
            }
        }
        _ => Err(InterpError::new("read_file_partial expects (string, int, int)")),
    }
}

fn builtin_read_lines_each(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::String(path) => {
            match std::fs::read_to_string(path) {
                Ok(content) => {
                    let lines: Vec<Value> = content.lines().map(|l| Value::String(l.to_string())).collect();
                    Ok(Value::List(lines))
                }
                Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
            }
        }
        _ => Err(InterpError::new("read_lines_each expects a string path")),
    }
}

fn builtin_regex_capture_groups(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::String(text), Value::String(pattern)) => {
            let re = regex::Regex::new(pattern)
                .map_err(|e| InterpError::new(format!("regex error: {}", e)))?;
            match re.captures(text) {
                Some(caps) => {
                    let groups: Vec<Value> = caps.iter()
                        .map(|m| Value::String(m.map(|m| m.as_str().to_string()).unwrap_or_default()))
                        .collect();
                    Ok(Value::List(groups))
                }
                None => Ok(Value::List(vec![])),
            }
        }
        _ => Err(InterpError::new("regex_capture_groups expects (string, string)")),
    }
}

// ── Shadow memory ───────────────────────────────────────

fn builtin_shadow_alloc(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let size = match &args[0] { Value::Int(n) => *n as usize, _ => return Err(InterpError::new("shadow_alloc: size must be int")) };
    let tag = match &args[1] { Value::Int(n) => *n as u8, _ => return Err(InterpError::new("shadow_alloc: tag must be int")) };
    let label = match &args[2] { Value::String(s) => s.clone(), _ => return Err(InterpError::new("shadow_alloc: label must be string")) };
    let c_label = std::ffi::CString::new(label).unwrap_or_default();
    let ptr = crate::runtime::mimi_shadow_alloc(size, tag, c_label.as_ptr());
    Ok(Value::Int(ptr as i64))
}

fn builtin_shadow_tag(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let ptr = match &args[0] { Value::Int(n) => *n as *const u8, _ => return Err(InterpError::new("shadow_tag: ptr must be int")) };
    let tag = match &args[1] { Value::Int(n) => *n as u8, _ => return Err(InterpError::new("shadow_tag: tag must be int")) };
    Ok(Value::Int(crate::runtime::mimi_shadow_tag(ptr, tag) as i64))
}

fn builtin_shadow_check(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let ptr = match &args[0] { Value::Int(n) => *n as *const u8, _ => return Err(InterpError::new("shadow_check: ptr must be int")) };
    let tag = match &args[1] { Value::Int(n) => *n as u8, _ => return Err(InterpError::new("shadow_check: tag must be int")) };
    Ok(Value::Bool(crate::runtime::mimi_shadow_check(ptr, tag) == 1))
}

fn builtin_shadow_free(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let ptr = match &args[0] { Value::Int(n) => *n as *mut u8, _ => return Err(InterpError::new("shadow_free: ptr must be int")) };
    crate::runtime::mimi_shadow_free(ptr);
    Ok(Value::Unit)
}

fn builtin_alloc_noop(_vm: &mut BytecodeVM<'_>, _args: &[Value]) -> Result<Value, InterpError> {
    // Allocator builtins are no-ops in the interpreter (memory is GC'd).
    Ok(Value::Unit)
}
