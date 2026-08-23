//! Filesystem builtins: read_file, write_file, file_exists, path operations, etc.

use crate::interp::bytecode::registry::{BuiltinCategory, BuiltinDesc, BuiltinRegistry};
use crate::interp::bytecode::vm::BytecodeVM;
use crate::interp::error::InterpError;
use crate::interp::value::Value;
use std::collections::HashSet;
use std::sync::Arc;

pub fn register(reg: &mut BuiltinRegistry) {
    reg.register(BuiltinDesc {
        name: "read_file",
        arity: 1,
        category: BuiltinCategory::System,
        func: builtin_read_file,
    });
    reg.register(BuiltinDesc {
        name: "write_file",
        arity: 2,
        category: BuiltinCategory::System,
        func: builtin_write_file,
    });
    // Phase D (0.39.75): 收 cap 的 fs API（SystemToken 能力门禁，运行时忽略）。
    reg.register(BuiltinDesc {
        name: "read_file_guarded",
        arity: 2,
        category: BuiltinCategory::System,
        func: builtin_read_file_guarded,
    });
    reg.register(BuiltinDesc {
        name: "append_file",
        arity: 2,
        category: BuiltinCategory::System,
        func: builtin_append_file,
    });
    reg.register(BuiltinDesc {
        name: "file_exists",
        arity: 1,
        category: BuiltinCategory::System,
        func: builtin_file_exists,
    });
    reg.register(BuiltinDesc {
        name: "remove_file",
        arity: 1,
        category: BuiltinCategory::System,
        func: builtin_remove_file,
    });
    reg.register(BuiltinDesc {
        name: "is_dir",
        arity: 1,
        category: BuiltinCategory::System,
        func: builtin_is_dir,
    });
    reg.register(BuiltinDesc {
        name: "is_file",
        arity: 1,
        category: BuiltinCategory::System,
        func: builtin_is_file,
    });
    reg.register(BuiltinDesc {
        name: "mkdir_p",
        arity: 1,
        category: BuiltinCategory::System,
        func: builtin_mkdir_p,
    });
    reg.register(BuiltinDesc {
        name: "listdir",
        arity: 1,
        category: BuiltinCategory::System,
        func: builtin_listdir,
    });
    reg.register(BuiltinDesc {
        name: "walk_dir",
        arity: 1,
        category: BuiltinCategory::System,
        func: builtin_walk_dir,
    });
    reg.register(BuiltinDesc {
        name: "path_basename",
        arity: 1,
        category: BuiltinCategory::System,
        func: builtin_path_basename,
    });
    reg.register(BuiltinDesc {
        name: "path_dirname",
        arity: 1,
        category: BuiltinCategory::System,
        func: builtin_path_dirname,
    });
    reg.register(BuiltinDesc {
        name: "path_ext",
        arity: 1,
        category: BuiltinCategory::System,
        func: builtin_path_ext,
    });
    reg.register(BuiltinDesc {
        name: "path_join",
        arity: 2,
        category: BuiltinCategory::System,
        func: builtin_path_join,
    });
    // Env
    reg.register(BuiltinDesc {
        name: "args",
        arity: 0,
        category: BuiltinCategory::System,
        func: builtin_args,
    });
    reg.register(BuiltinDesc {
        name: "getenv",
        arity: 1,
        category: BuiltinCategory::System,
        func: builtin_getenv,
    });
    // Phase D (0.39.75): 收 cap 的 env API（SystemToken 能力门禁，运行时忽略）。
    reg.register(BuiltinDesc {
        name: "get_env_guarded",
        arity: 2,
        category: BuiltinCategory::System,
        func: builtin_get_env_guarded,
    });
    reg.register(BuiltinDesc {
        name: "set_env",
        arity: 2,
        category: BuiltinCategory::System,
        func: builtin_set_env,
    });
    // Time
    reg.register(BuiltinDesc {
        name: "timestamp",
        arity: 0,
        category: BuiltinCategory::System,
        func: builtin_timestamp,
    });
    reg.register(BuiltinDesc {
        name: "timestamp_ms",
        arity: 0,
        category: BuiltinCategory::System,
        func: builtin_timestamp_ms,
    });
    reg.register(BuiltinDesc {
        name: "now",
        arity: 0,
        category: BuiltinCategory::System,
        func: builtin_timestamp,
    });
    reg.register(BuiltinDesc {
        name: "now_ms",
        arity: 0,
        category: BuiltinCategory::System,
        func: builtin_timestamp_ms,
    });
    reg.register(BuiltinDesc {
        name: "sleep",
        arity: 1,
        category: BuiltinCategory::System,
        func: builtin_sleep,
    });
}

fn expect_str(args: &[Value], idx: usize) -> Result<String, InterpError> {
    match args.get(idx) {
        Some(Value::String(s)) => Ok(s.as_str().to_string()),
        Some(_) => Err(InterpError::new("expected a string argument")),
        None => Err(InterpError::new(format!(
            "missing argument at index {}",
            idx
        ))),
    }
}

// ── File I/O ────────────────────────────────────────────

fn builtin_read_file(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    // Guard against oversized files (matches tree-walker CL-H1).
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > crate::path_safety::MAX_SOURCE_BYTES {
            return Ok(Value::Variant(
                "Err".into(),
                vec![Value::String(Arc::new(
                    "read_file: file too large".to_string(),
                ))],
            ));
        }
    }
    match std::fs::read_to_string(&path) {
        Ok(content) => Ok(Value::Variant(
            "Ok".into(),
            vec![Value::String(Arc::new(content))],
        )),
        Err(e) => Ok(Value::Variant(
            "Err".into(),
            vec![Value::String(Arc::new(e.to_string()))],
        )),
    }
}

/// Phase D (0.39.75): 收 cap 的 fs API——path 为 args[0]，SystemToken 能力门禁
/// 在 args[1]（运行时忽略：能力由 checker/CFG 线性消费保证）。语义同 read_file。
fn builtin_read_file_guarded(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    builtin_read_file(_vm, &args[0..1])
}

fn builtin_write_file(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    let content = expect_str(args, 1)?;
    match std::fs::write(&path, &content) {
        Ok(()) => Ok(Value::Variant("Ok".into(), vec![Value::Unit])),
        Err(e) => Ok(Value::Variant(
            "Err".into(),
            vec![Value::String(Arc::new(e.to_string()))],
        )),
    }
}

fn builtin_append_file(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    let content = expect_str(args, 1)?;
    use std::io::Write;
    let ok = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| file.write_all(content.as_bytes()))
        .is_ok();
    Ok(Value::Bool(ok))
}

fn builtin_file_exists(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    Ok(Value::Bool(std::path::Path::new(&path).exists()))
}

fn builtin_remove_file(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    Ok(Value::Bool(std::fs::remove_file(&path).is_ok()))
}

fn builtin_is_dir(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    Ok(Value::Bool(std::path::Path::new(&path).is_dir()))
}

fn builtin_is_file(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    Ok(Value::Bool(std::path::Path::new(&path).is_file()))
}

fn builtin_mkdir_p(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    Ok(Value::Bool(std::fs::create_dir_all(&path).is_ok()))
}

fn builtin_listdir(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    match std::fs::read_dir(&path) {
        Ok(entries) => {
            let mut result = Vec::new();
            for entry in entries.flatten() {
                result.push(Value::String(Arc::new(
                    entry.file_name().to_string_lossy().to_string(),
                )));
            }
            Ok(Value::List(Arc::new(result)))
        }
        Err(_) => Ok(Value::List(Arc::new(vec![]))),
    }
}

/// walk_dir: recursively list file paths under a directory.
/// Depth-first, collects files only (directories are descended into, not
/// listed) — matches the runtime `mimi_walk_dir` semantics so the dual
/// backends agree.
fn builtin_walk_dir(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    // Iterative DFS with depth/result caps and symlink-cycle detection,
    // mirroring the runtime walk_dir implementation (batch2 P2-3).
    const MAX_DEPTH: usize = 64;
    const MAX_RESULTS: usize = 1_000_000;
    let mut result: Vec<String> = Vec::new();
    let mut visited: HashSet<std::path::PathBuf> = HashSet::new();
    let mut stack = vec![(std::path::PathBuf::from(&path), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            continue;
        }
        let canonical = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if !visited.insert(canonical) {
            continue;
        }
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in rd.flatten() {
            let entry_path = entry.path();
            if entry_path.is_dir() {
                stack.push((entry_path, depth + 1));
            } else {
                result.push(entry_path.to_string_lossy().to_string());
                if result.len() >= MAX_RESULTS {
                    break;
                }
            }
        }
        if result.len() >= MAX_RESULTS {
            break;
        }
    }
    Ok(Value::List(Arc::new(
        result
            .into_iter()
            .map(|s| Value::String(Arc::new(s)))
            .collect(),
    )))
}

// ── Path operations ─────────────────────────────────────

fn builtin_path_basename(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    let basename = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok(Value::String(Arc::new(basename)))
}

fn builtin_path_dirname(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    let dirname = std::path::Path::new(&path)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("")
        .to_string();
    Ok(Value::String(Arc::new(dirname)))
}

fn builtin_path_ext(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let path = expect_str(args, 0)?;
    let ext = std::path::Path::new(&path)
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok(Value::String(Arc::new(ext)))
}

fn builtin_path_join(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let base = expect_str(args, 0)?;
    let other = expect_str(args, 1)?;
    let joined = std::path::Path::new(&base).join(&other);
    Ok(Value::String(Arc::new(
        joined.to_string_lossy().to_string(),
    )))
}

// ── Env ─────────────────────────────────────────────────

fn builtin_args(vm: &mut BytecodeVM, _args: &[Value]) -> Result<Value, InterpError> {
    let args: Vec<Value> = vm
        .cli_args
        .iter()
        .map(|s| Value::String(Arc::new(s.clone())))
        .collect();
    Ok(Value::List(Arc::new(args)))
}

/// Phase D (0.39.75): 收 cap 的 env API——name 为 args[0]，SystemToken 能力
/// 门禁在 args[1]（运行时忽略）。语义同 getenv。
fn builtin_get_env_guarded(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    builtin_getenv(_vm, &args[0..1])
}

fn builtin_getenv(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let key = expect_str(args, 0)?;
    match std::env::var(&key) {
        Ok(val) => Ok(Value::Variant(
            "Ok".into(),
            vec![Value::String(Arc::new(val))],
        )),
        Err(_) => Ok(Value::Variant(
            "Err".into(),
            vec![Value::String(Arc::new(format!(
                "env var '{}' not set",
                key
            )))],
        )),
    }
}

fn builtin_set_env(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let key = expect_str(args, 0)?;
    let val = expect_str(args, 1)?;
    std::env::set_var(&key, &val);
    Ok(Value::Bool(true))
}

// ── Time ────────────────────────────────────────────────

fn builtin_timestamp(_vm: &mut BytecodeVM, _args: &[Value]) -> Result<Value, InterpError> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Ok(Value::Int(secs as i64))
}

fn builtin_timestamp_ms(_vm: &mut BytecodeVM, _args: &[Value]) -> Result<Value, InterpError> {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    Ok(Value::Int(ms as i64))
}

fn builtin_sleep(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let ms = match &args[0] {
        Value::Int(v) => {
            if *v < 0 {
                return Err(InterpError::new("sleep: duration must be non-negative"));
            }
            *v as u64
        }
        Value::Float(v) => {
            if *v < 0.0 {
                return Err(InterpError::new("sleep: duration must be non-negative"));
            }
            *v as u64
        }
        _ => return Err(InterpError::new("sleep expects a number (milliseconds)")),
    };
    std::thread::sleep(std::time::Duration::from_millis(ms));
    Ok(Value::Unit)
}
