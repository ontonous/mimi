//! Filesystem builtins: read_file, write_file, file_exists, path operations, etc.

use crate::interp::bytecode::registry::{BuiltinCategory, BuiltinDesc, BuiltinRegistry};
use crate::interp::bytecode::vm::BytecodeVM;
use crate::interp::error::InterpError;
use crate::interp::value::Value;

pub fn register(reg: &mut BuiltinRegistry) {
    reg.register(BuiltinDesc { name: "read_file", arity: 1, category: BuiltinCategory::System, func: builtin_read_file });
    reg.register(BuiltinDesc { name: "write_file", arity: 2, category: BuiltinCategory::System, func: builtin_write_file });
    reg.register(BuiltinDesc { name: "append_file", arity: 2, category: BuiltinCategory::System, func: builtin_append_file });
    reg.register(BuiltinDesc { name: "file_exists", arity: 1, category: BuiltinCategory::System, func: builtin_file_exists });
    reg.register(BuiltinDesc { name: "remove_file", arity: 1, category: BuiltinCategory::System, func: builtin_remove_file });
    reg.register(BuiltinDesc { name: "is_dir", arity: 1, category: BuiltinCategory::System, func: builtin_is_dir });
    reg.register(BuiltinDesc { name: "is_file", arity: 1, category: BuiltinCategory::System, func: builtin_is_file });
    reg.register(BuiltinDesc { name: "mkdir_p", arity: 1, category: BuiltinCategory::System, func: builtin_mkdir_p });
    reg.register(BuiltinDesc { name: "listdir", arity: 1, category: BuiltinCategory::System, func: builtin_listdir });
    reg.register(BuiltinDesc { name: "walk_dir", arity: 1, category: BuiltinCategory::System, func: builtin_listdir });
    reg.register(BuiltinDesc { name: "path_basename", arity: 1, category: BuiltinCategory::System, func: builtin_path_basename });
    reg.register(BuiltinDesc { name: "path_dirname", arity: 1, category: BuiltinCategory::System, func: builtin_path_dirname });
    reg.register(BuiltinDesc { name: "path_ext", arity: 1, category: BuiltinCategory::System, func: builtin_path_ext });
    reg.register(BuiltinDesc { name: "path_join", arity: 2, category: BuiltinCategory::System, func: builtin_path_join });
    // Env
    reg.register(BuiltinDesc { name: "args", arity: 0, category: BuiltinCategory::System, func: builtin_args });
    reg.register(BuiltinDesc { name: "getenv", arity: 1, category: BuiltinCategory::System, func: builtin_getenv });
    reg.register(BuiltinDesc { name: "set_env", arity: 2, category: BuiltinCategory::System, func: builtin_set_env });
    // Time
    reg.register(BuiltinDesc { name: "timestamp", arity: 0, category: BuiltinCategory::System, func: builtin_timestamp });
    reg.register(BuiltinDesc { name: "timestamp_ms", arity: 0, category: BuiltinCategory::System, func: builtin_timestamp_ms });
    reg.register(BuiltinDesc { name: "now", arity: 0, category: BuiltinCategory::System, func: builtin_timestamp });
    reg.register(BuiltinDesc { name: "now_ms", arity: 0, category: BuiltinCategory::System, func: builtin_timestamp_ms });
    reg.register(BuiltinDesc { name: "sleep", arity: 1, category: BuiltinCategory::System, func: builtin_sleep });
}

fn expect_str(args: &[Value], idx: usize) -> Result<String, InterpError> {
    match &args[idx] {
        Value::String(s) => Ok(s.clone()),
        _ => Err(InterpError::new("expected a string argument")),
    }
}

// ── File I/O ────────────────────────────────────────────

fn builtin_read_file(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    match std::fs::read_to_string(&path) {
        Ok(content) => Ok(Value::Variant("Ok".into(), vec![Value::String(content)])),
        Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
    }
}

fn builtin_write_file(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    let content = expect_str(args, 1)?;
    match std::fs::write(&path, &content) {
        Ok(()) => Ok(Value::Variant("Ok".into(), vec![Value::Unit])),
        Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
    }
}

fn builtin_append_file(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    let content = expect_str(args, 1)?;
    use std::io::Write;
    match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut file) => {
            match file.write_all(content.as_bytes()) {
                Ok(()) => Ok(Value::Variant("Ok".into(), vec![Value::Unit])),
                Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
            }
        }
        Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
    }
}

fn builtin_file_exists(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    Ok(Value::Bool(std::path::Path::new(&path).exists()))
}

fn builtin_remove_file(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(Value::Variant("Ok".into(), vec![Value::Unit])),
        Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
    }
}

fn builtin_is_dir(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    Ok(Value::Bool(std::path::Path::new(&path).is_dir()))
}

fn builtin_is_file(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    Ok(Value::Bool(std::path::Path::new(&path).is_file()))
}

fn builtin_mkdir_p(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    match std::fs::create_dir_all(&path) {
        Ok(()) => Ok(Value::Variant("Ok".into(), vec![Value::Unit])),
        Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
    }
}

fn builtin_listdir(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    match std::fs::read_dir(&path) {
        Ok(entries) => {
            let mut result = Vec::new();
            for entry in entries.flatten() {
                result.push(Value::String(entry.file_name().to_string_lossy().to_string()));
            }
            Ok(Value::List(result))
        }
        Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
    }
}

// ── Path operations ─────────────────────────────────────

fn builtin_path_basename(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    let basename = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok(Value::String(basename))
}

fn builtin_path_dirname(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    let dirname = std::path::Path::new(&path)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());
    Ok(Value::String(dirname))
}

fn builtin_path_ext(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    let ext = std::path::Path::new(&path)
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok(Value::String(ext))
}

fn builtin_path_join(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let base = expect_str(args, 0)?;
    let other = expect_str(args, 1)?;
    let joined = std::path::Path::new(&base).join(&other);
    Ok(Value::String(joined.to_string_lossy().to_string()))
}

// ── Env ─────────────────────────────────────────────────

fn builtin_args(_vm: &mut BytecodeVM<'_>, _args: &[Value]) -> Result<Value, InterpError> {
    let args: Vec<Value> = std::env::args().map(Value::String).collect();
    Ok(Value::List(args))
}

fn builtin_getenv(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let key = expect_str(args, 0)?;
    match std::env::var(&key) {
        Ok(val) => Ok(Value::Variant("Ok".into(), vec![Value::String(val)])),
        Err(_) => Ok(Value::Variant("Err".into(), vec![Value::String(format!("env var '{}' not set", key))])),
    }
}

fn builtin_set_env(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let key = expect_str(args, 0)?;
    let val = expect_str(args, 1)?;
    std::env::set_var(&key, &val);
    Ok(Value::Unit)
}

// ── Time ────────────────────────────────────────────────

fn builtin_timestamp(_vm: &mut BytecodeVM<'_>, _args: &[Value]) -> Result<Value, InterpError> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Ok(Value::Int(secs as i64))
}

fn builtin_timestamp_ms(_vm: &mut BytecodeVM<'_>, _args: &[Value]) -> Result<Value, InterpError> {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    Ok(Value::Int(ms as i64))
}

fn builtin_sleep(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let ms = match &args[0] {
        Value::Int(v) => *v as u64,
        Value::Float(v) => *v as u64,
        _ => return Err(InterpError::new("sleep expects a number (milliseconds)")),
    };
    std::thread::sleep(std::time::Duration::from_millis(ms));
    Ok(Value::Unit)
}
