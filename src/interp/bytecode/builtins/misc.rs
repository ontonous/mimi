//! Miscellaneous builtins: JSON, crypto, testing, assertions, misc IO.

use crate::interp::bytecode::registry::{BuiltinCategory, BuiltinDesc, BuiltinRegistry};
use crate::interp::bytecode::vm::BytecodeVM;
use crate::interp::error::InterpError;
use crate::interp::value::Value;

pub fn register(reg: &mut BuiltinRegistry) {
    // JSON
    reg.register(BuiltinDesc { name: "to_json", arity: 1, category: BuiltinCategory::System, func: builtin_to_json });
    reg.register(BuiltinDesc { name: "from_json", arity: 1, category: BuiltinCategory::System, func: builtin_from_json });
    reg.register(BuiltinDesc { name: "from_json_typed", arity: 1, category: BuiltinCategory::System, func: builtin_from_json_typed });
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
    reg.register(BuiltinDesc { name: "inner", arity: 1, category: BuiltinCategory::System, func: builtin_inner });
    reg.register(BuiltinDesc { name: "deref", arity: 1, category: BuiltinCategory::System, func: builtin_inner });
    reg.register(BuiltinDesc { name: "fields", arity: 1, category: BuiltinCategory::System, func: builtin_fields });
    reg.register(BuiltinDesc { name: "type_fields", arity: 1, category: BuiltinCategory::System, func: builtin_fields });
    reg.register(BuiltinDesc { name: "type_variants", arity: 1, category: BuiltinCategory::System, func: builtin_type_variants });
    // C string
    reg.register(BuiltinDesc { name: "str_to_c_str", arity: 1, category: BuiltinCategory::String, func: builtin_str_to_c_str });
    reg.register(BuiltinDesc { name: "c_str_to_string", arity: 1, category: BuiltinCategory::String, func: builtin_c_str_to_string });
    // Process
    reg.register(BuiltinDesc { name: "exec", arity: 1, category: BuiltinCategory::System, func: builtin_exec });
    reg.register(BuiltinDesc { name: "exec_pipe", arity: 1, category: BuiltinCategory::System, func: builtin_exec_pipe });
    reg.register(BuiltinDesc { name: "exec_safe", arity: usize::MAX, category: BuiltinCategory::System, func: builtin_exec_safe });
    // FS extended
    reg.register(BuiltinDesc { name: "file_stat", arity: 1, category: BuiltinCategory::System, func: builtin_file_stat });
    reg.register(BuiltinDesc { name: "read_file_bytes", arity: 1, category: BuiltinCategory::System, func: builtin_read_file_bytes });
    reg.register(BuiltinDesc { name: "write_file_bytes", arity: 2, category: BuiltinCategory::System, func: builtin_write_file_bytes });
    reg.register(BuiltinDesc { name: "close_fd", arity: 1, category: BuiltinCategory::System, func: builtin_close_fd });
    reg.register(BuiltinDesc { name: "read_file_partial", arity: 3, category: BuiltinCategory::System, func: builtin_read_file_partial });
    reg.register(BuiltinDesc { name: "read_lines_each", arity: 2, category: BuiltinCategory::System, func: builtin_read_lines_each });
    reg.register(BuiltinDesc { name: "read_lines_json", arity: 1, category: BuiltinCategory::System, func: builtin_read_lines_json });
    reg.register(BuiltinDesc { name: "read_lines_json_builtin", arity: 1, category: BuiltinCategory::System, func: builtin_read_lines_json });
    // Regex extended
    reg.register(BuiltinDesc { name: "regex_capture_groups", arity: 2, category: BuiltinCategory::String, func: builtin_regex_capture_groups });
    // Tooling / meta
    reg.register(BuiltinDesc { name: "lexer", arity: 1, category: BuiltinCategory::System, func: builtin_lexer });
    reg.register(BuiltinDesc { name: "mms_parse", arity: 1, category: BuiltinCategory::System, func: builtin_mms_parse });
    reg.register(BuiltinDesc { name: "ast_eval", arity: 1, category: BuiltinCategory::System, func: builtin_ast_eval });
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
            // Without type parameter: validate and return string as-is.
            // Typed deserialization (from_json::<T>) is handled by the compiler
            // which emits a CallBuiltin to from_json_typed instead.
            let _: serde_json::Value = serde_json::from_str(s)
                .map_err(|e| InterpError::new(format!("from_json parse error: {}", e)))?;
            Ok(Value::String(s.clone()))
        }
        _ => Err(InterpError::new("from_json expects a string")),
    }
}

/// from_json_typed: parse JSON string into a Mimi Value (Record/List/Int/etc).
/// Called by the compiler when `from_json::<T>(s)` is used with a type parameter.
fn builtin_from_json_typed(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::String(s) => {
            let jv: serde_json::Value = serde_json::from_str(s)
                .map_err(|e| InterpError::new(format!("from_json parse error: {}", e)))?;
            Ok(json_to_value(&jv))
        }
        _ => Err(InterpError::new("from_json::<T> expects a string")),
    }
}

fn builtin_json_get_string(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::String(json_str), Value::String(key)) => {
            let jv: serde_json::Value = serde_json::from_str(json_str)
                .map_err(|e| InterpError::new(format!("json_get_string parse error: {}", e)))?;
            match jv.get(key) {
                Some(serde_json::Value::String(s)) => Ok(Value::String(s.clone())),
                Some(serde_json::Value::Bool(b)) => Ok(Value::String(if *b { "true".into() } else { "false".into() })),
                Some(serde_json::Value::Number(n)) => Ok(Value::String(n.to_string())),
                Some(serde_json::Value::Null) => Ok(Value::String("null".into())),
                Some(val) => Ok(Value::String(val.to_string())),
                None => Ok(Value::String(String::new())),
            }
        }
        _ => Err(InterpError::new("json_get_string expects (string, string)")),
    }
}

fn builtin_json_get_int(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::String(json_str), Value::String(key)) => {
            let jv: serde_json::Value = serde_json::from_str(json_str)
                .map_err(|e| InterpError::new(format!("json_get_int parse error: {}", e)))?;
            match jv.get(key) {
                Some(serde_json::Value::Number(n)) => n.as_i64().map(Value::Int).ok_or_else(|| {
                    InterpError::new(format!("json_get_int: value for key '{}' is not an integer", key))
                }),
                Some(_) => Err(InterpError::new(format!("json_get_int: key '{}' is not a number", key))),
                None => Err(InterpError::new(format!("json_get_int: key '{}' not found", key))),
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
    match std::io::stdin().read_line(&mut input) {
        Ok(_) => Ok(Value::Variant("Ok".into(), vec![Value::String(input.trim_end().to_string())])),
        Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
    }
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
            let matches: Vec<String> = re.find_iter(text)
                .map(|m| m.as_str().to_string()).collect();
            let json = matches.iter().enumerate()
                .map(|(i, m)| {
                    if i > 0 { "," } else { "" }.to_string()
                        + &format!("\"{}\"", m.replace('\\', "\\\\").replace('"', "\\\""))
                })
                .collect::<String>();
            Ok(Value::String(format!("[{}]", json)))
        }
        _ => Err(InterpError::new("regex_find_all expects (string, string)")),
    }
}

// ── Misc value ops ──────────────────────────────────────

fn builtin_eq(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    Ok(Value::Bool(crate::interp::values_equal(&args[0], &args[1])))
}

fn builtin_inner(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    // inner/deref — unwrap a wrapper type.
    match &args[0] {
        Value::Variant(_, payload) => Ok(payload.first().cloned().unwrap_or(Value::Unit)),
        Value::Newtype(_, inner) => Ok(*inner.clone()),
        Value::Shared(arc) => {
            let inner = arc.read().map_err(|e| {
                InterpError::new(format!("shared read lock failed: {}", e))
            })?;
            Ok(inner.clone())
        }
        Value::LocalShared(rc) => {
            let inner = rc.lock().unwrap_or_else(|e| e.into_inner());
            Ok(inner.clone())
        }
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
    if args.len() != 1 {
        return Err(InterpError::new("exec expects 1 argument (command)"));
    }
    match &args[0] {
        Value::String(cmd) => {
            if cmd.contains('\0') {
                return Err(InterpError::new("exec: command contains null byte"));
            }
            match std::process::Command::new("sh").arg("-c").arg(cmd).output() {
                Ok(out) => {
                    const MAX_EXEC_OUTPUT: usize = 10 * 1024 * 1024;
                    let stdout_bytes = if out.stdout.len() > MAX_EXEC_OUTPUT {
                        &out.stdout[..MAX_EXEC_OUTPUT]
                    } else {
                        &out.stdout
                    };
                    let stderr_bytes = if out.stderr.len() > MAX_EXEC_OUTPUT {
                        &out.stderr[..MAX_EXEC_OUTPUT]
                    } else {
                        &out.stderr
                    };
                    let stdout = String::from_utf8_lossy(stdout_bytes).to_string();
                    let stderr = String::from_utf8_lossy(stderr_bytes).to_string();
                    let exit_code = out.status.code().unwrap_or(-1);
                    let mut fields = std::collections::HashMap::new();
                    fields.insert("exit_code".to_string(), Value::Int(exit_code as i64));
                    fields.insert("stdout".to_string(), Value::String(stdout));
                    fields.insert("stderr".to_string(), Value::String(stderr));
                    Ok(Value::Record(Some("ExecResult".to_string()), fields))
                }
                Err(e) => {
                    let mut fields = std::collections::HashMap::new();
                    fields.insert("exit_code".to_string(), Value::Int(-1));
                    fields.insert("stdout".to_string(), Value::String(String::new()));
                    fields.insert("stderr".to_string(), Value::String(format!("exec error: {}", e)));
                    Ok(Value::Record(Some("ExecResult".to_string()), fields))
                }
            }
        }
        _ => Err(InterpError::new("exec expects a string command")),
    }
}

fn builtin_exec_pipe(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    if args.len() != 1 {
        return Err(InterpError::new("exec_pipe expects 1 argument (command)"));
    }
    match &args[0] {
        Value::String(cmd) => {
            if cmd.contains('\0') {
                return Err(InterpError::new("exec_pipe: command contains null byte"));
            }
            match std::process::Command::new("sh").arg("-c").arg(cmd).output() {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                    Ok(Value::String(stdout))
                }
                Err(e) => Err(InterpError::new(format!("exec_pipe error: {}", e))),
            }
        }
        _ => Err(InterpError::new("exec_pipe expects a string command")),
    }
}

fn builtin_exec_safe(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    if args.is_empty() {
        return Err(InterpError::new("exec_safe expects at least 1 argument (program)"));
    }
    let prog = match &args[0] {
        Value::String(s) => s.clone(),
        _ => return Err(InterpError::new("exec_safe: first argument must be a string (program)")),
    };
    let mut cmd = std::process::Command::new(&prog);
    for arg in args.iter().skip(1) {
        match arg {
            Value::String(s) => { cmd.arg(s); }
            _ => return Err(InterpError::new("exec_safe: all arguments must be strings")),
        }
    }
    match cmd.output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            let exit_code = out.status.code().unwrap_or(-1);
            let mut fields = std::collections::HashMap::new();
            fields.insert("exit_code".to_string(), Value::Int(exit_code as i64));
            fields.insert("stdout".to_string(), Value::String(stdout));
            fields.insert("stderr".to_string(), Value::String(stderr));
            Ok(Value::Record(Some("ExecResult".to_string()), fields))
        }
        Err(e) => {
            let mut fields = std::collections::HashMap::new();
            fields.insert("exit_code".to_string(), Value::Int(-1));
            fields.insert("stdout".to_string(), Value::String(String::new()));
            fields.insert("stderr".to_string(), Value::String(format!("exec error: {}", e)));
            Ok(Value::Record(Some("ExecResult".to_string()), fields))
        }
    }
}

// ── FS extended ─────────────────────────────────────────

fn builtin_file_stat(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::String(path) => {
            let mut fields = std::collections::HashMap::new();
            match std::fs::metadata(path) {
                Ok(meta) => {
                    fields.insert("size".to_string(), Value::Int(meta.len() as i64));
                    let modified = meta.modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    fields.insert("modified".to_string(), Value::Int(modified));
                    fields.insert("is_file".to_string(), Value::Bool(meta.is_file()));
                    fields.insert("is_dir".to_string(), Value::Bool(meta.is_dir()));
                }
                Err(_) => {
                    fields.insert("size".to_string(), Value::Int(-1));
                    fields.insert("modified".to_string(), Value::Int(0));
                    fields.insert("is_file".to_string(), Value::Bool(false));
                    fields.insert("is_dir".to_string(), Value::Bool(false));
                }
            }
            Ok(Value::Record(Some("StatResult".to_string()), fields))
        }
        _ => Err(InterpError::new("file_stat expects a string path")),
    }
}

fn builtin_read_file_bytes(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::String(path) => {
            match std::fs::read(path) {
                Ok(bytes) => {
                    let s = String::from_utf8_lossy(&bytes).to_string();
                    Ok(Value::Variant("Ok".into(), vec![Value::String(s)]))
                }
                Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
            }
        }
        _ => Err(InterpError::new("read_file_bytes expects a string path")),
    }
}

fn builtin_write_file_bytes(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::String(path), Value::String(data)) => {
            match std::fs::write(path, data.as_bytes()) {
                Ok(()) => Ok(Value::Variant("Ok".into(), vec![Value::Unit])),
                Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
            }
        }
        _ => Err(InterpError::new("write_file_bytes expects (string, string)")),
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

fn builtin_read_lines_each(vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    // read_lines_each(path, callback) — iterate lines calling closure, return count.
    let path = match &args[0] {
        Value::String(s) => s.clone(),
        _ => return Err(InterpError::new("read_lines_each expects (string, closure)")),
    };
    let callback = &args[1];
    use std::io::BufRead;
    let file = std::fs::File::open(path)
        .map_err(|e| InterpError::new(format!("read_lines_each: {}", e)))?;
    let reader = std::io::BufReader::new(file);
    let mut count: i64 = 0;
    for line_result in reader.lines() {
        let line = line_result.map_err(|e| {
            InterpError::new(format!("read_lines_each: failed to read line: {}", e))
        })?;
        vm.call_closure(callback, &[Value::String(line)])?;
        count += 1;
    }
    Ok(Value::Int(count))
}

fn builtin_read_lines_json(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let path = match &args[0] {
        Value::String(s) => s.clone(),
        _ => return Err(InterpError::new("read_lines_json expects a string path")),
    };
    use std::io::BufRead;
    let file = std::fs::File::open(path)
        .map_err(|e| InterpError::new(format!("read_lines_json: {}", e)))?;
    let reader = std::io::BufReader::new(file);
    let mut result = String::from("[");
    let mut first = true;
    for line in reader.lines().map_while(Result::ok) {
        if !first { result.push(','); }
        first = false;
        result.push('"');
        for ch in line.chars() {
            match ch {
                '"' => result.push_str("\\\""),
                '\\' => result.push_str("\\\\"),
                '\n' => result.push_str("\\n"),
                '\r' => result.push_str("\\r"),
                '\t' => result.push_str("\\t"),
                c if c < '\x20' => result.push_str(&format!("\\u{:04x}", c as u32)),
                c => result.push(c),
            }
        }
        result.push('"');
    }
    result.push(']');
    Ok(Value::String(result))
}

fn builtin_regex_capture_groups(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::String(text), Value::String(pattern)) => {
            let re = regex::Regex::new(pattern)
                .map_err(|e| InterpError::new(format!("regex error: {}", e)))?;
            match re.captures(text) {
                Some(caps) => {
                    // Skip group 0 (full match), start from index 1 (tree-walker semantics).
                    let groups: Vec<String> = caps.iter().skip(1)
                        .map(|m| m.map(|m| m.as_str().to_string()).unwrap_or_default())
                        .collect();
                    let json = groups.iter().enumerate()
                        .map(|(i, g)| {
                            if i > 0 { "," } else { "" }.to_string()
                                + &format!("\"{}\"", g.replace('\\', "\\\\").replace('"', "\\\""))
                        })
                        .collect::<String>();
                    Ok(Value::String(format!("[{}]", json)))
                }
                None => Ok(Value::String("[]".to_string())),
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

// === Tooling / meta builtins ===

fn builtin_lexer(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let source = args[0].as_string().ok_or_else(|| InterpError::new("lexer expects a string source"))?;
    let c_source = std::ffi::CString::new(source)
        .map_err(|_| InterpError::new("lexer: source contains null bytes"))?;
    let result_ptr = crate::runtime::mimi_lexer_tokenize(c_source.as_ptr());
    if result_ptr.is_null() {
        return Ok(Value::String("[]".to_string()));
    }
    let result = unsafe { std::ffi::CStr::from_ptr(result_ptr) }
        .to_string_lossy()
        .into_owned();
    unsafe { libc::free(result_ptr as *mut libc::c_void) };
    Ok(Value::String(result))
}

fn builtin_mms_parse(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let source = args[0].as_string().ok_or_else(|| InterpError::new("parse expects a string source"))?;
    let c_source = std::ffi::CString::new(source)
        .map_err(|_| InterpError::new("parse: source contains null bytes"))?;
    let result_ptr = crate::runtime::mimi_parse_source(c_source.as_ptr());
    if result_ptr.is_null() {
        return Ok(Value::String(
            r#"{"functions":[],"types":[],"imports":[],"has_main":false}"#.to_string(),
        ));
    }
    let result = unsafe { std::ffi::CStr::from_ptr(result_ptr) }
        .to_string_lossy()
        .into_owned();
    unsafe { libc::free(result_ptr as *mut libc::c_void) };
    Ok(Value::String(result))
}

fn builtin_ast_eval(_vm: &mut BytecodeVM<'_>, _args: &[Value]) -> Result<Value, InterpError> {
    Err(InterpError::new("ast_eval is not available in bytecode VM (use tree-walker interpreter)"))
}
