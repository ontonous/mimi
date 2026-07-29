//! Concurrency builtins: atomic, mutex, channel.
//! Thin wrappers around crate::runtime (shared with codegen — L1 by construction).

use crate::interp::bytecode::registry::{BuiltinCategory, BuiltinDesc, BuiltinRegistry};
use crate::interp::bytecode::vm::BytecodeVM;
use crate::interp::error::InterpError;
use crate::interp::value::Value;

pub fn register(reg: &mut BuiltinRegistry) {
    // Atomic i32
    reg.register(BuiltinDesc { name: "atomic_i32_new", arity: 1, category: BuiltinCategory::System, func: builtin_atomic_i32_new });
    reg.register(BuiltinDesc { name: "atomic_i32_load", arity: 1, category: BuiltinCategory::System, func: builtin_atomic_i32_load });
    reg.register(BuiltinDesc { name: "atomic_i32_store", arity: 2, category: BuiltinCategory::System, func: builtin_atomic_i32_store });
    reg.register(BuiltinDesc { name: "atomic_i32_fetch_add", arity: 2, category: BuiltinCategory::System, func: builtin_atomic_i32_fetch_add });
    reg.register(BuiltinDesc { name: "atomic_i32_compare_exchange", arity: 3, category: BuiltinCategory::System, func: builtin_atomic_i32_compare_exchange });
    reg.register(BuiltinDesc { name: "atomic_i32_drop", arity: 1, category: BuiltinCategory::System, func: builtin_atomic_i32_drop });
    // Atomic i64
    reg.register(BuiltinDesc { name: "atomic_i64_new", arity: 1, category: BuiltinCategory::System, func: builtin_atomic_i64_new });
    reg.register(BuiltinDesc { name: "atomic_i64_load", arity: 1, category: BuiltinCategory::System, func: builtin_atomic_i64_load });
    reg.register(BuiltinDesc { name: "atomic_i64_store", arity: 2, category: BuiltinCategory::System, func: builtin_atomic_i64_store });
    reg.register(BuiltinDesc { name: "atomic_i64_fetch_add", arity: 2, category: BuiltinCategory::System, func: builtin_atomic_i64_fetch_add });
    reg.register(BuiltinDesc { name: "atomic_i64_drop", arity: 1, category: BuiltinCategory::System, func: builtin_atomic_i64_drop });
    // Atomic bool
    reg.register(BuiltinDesc { name: "atomic_bool_new", arity: 1, category: BuiltinCategory::System, func: builtin_atomic_bool_new });
    reg.register(BuiltinDesc { name: "atomic_bool_load", arity: 1, category: BuiltinCategory::System, func: builtin_atomic_bool_load });
    reg.register(BuiltinDesc { name: "atomic_bool_store", arity: 2, category: BuiltinCategory::System, func: builtin_atomic_bool_store });
    reg.register(BuiltinDesc { name: "atomic_bool_drop", arity: 1, category: BuiltinCategory::System, func: builtin_atomic_bool_drop });
    // Mutex
    reg.register(BuiltinDesc { name: "mutex_new", arity: 1, category: BuiltinCategory::System, func: builtin_mutex_new });
    reg.register(BuiltinDesc { name: "mutex_lock", arity: 1, category: BuiltinCategory::System, func: builtin_mutex_lock });
    reg.register(BuiltinDesc { name: "mutex_get", arity: 1, category: BuiltinCategory::System, func: builtin_mutex_get });
    reg.register(BuiltinDesc { name: "mutex_set", arity: 2, category: BuiltinCategory::System, func: builtin_mutex_set });
    reg.register(BuiltinDesc { name: "mutex_unlock", arity: 1, category: BuiltinCategory::System, func: builtin_mutex_unlock });
    reg.register(BuiltinDesc { name: "mutex_drop", arity: 1, category: BuiltinCategory::System, func: builtin_mutex_drop });
    // Channel
    reg.register(BuiltinDesc { name: "channel_new", arity: 0, category: BuiltinCategory::System, func: builtin_channel_new });
    reg.register(BuiltinDesc { name: "channel_send", arity: 2, category: BuiltinCategory::System, func: builtin_channel_send });
    reg.register(BuiltinDesc { name: "channel_recv", arity: 1, category: BuiltinCategory::System, func: builtin_channel_recv });
    reg.register(BuiltinDesc { name: "channel_try_recv", arity: 1, category: BuiltinCategory::System, func: builtin_channel_try_recv });
    reg.register(BuiltinDesc { name: "channel_drop", arity: 1, category: BuiltinCategory::System, func: builtin_channel_drop });
}

fn handle(args: &[Value], idx: usize) -> Result<i64, InterpError> {
    match &args[idx] {
        Value::Int(x) => Ok(*x),
        _ => Err(InterpError::new("expected an i64 handle")),
    }
}

// ── Atomic i32 ──────────────────────────────────────────

fn builtin_atomic_i32_new(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let v = match &args[0] { Value::Int(x) => *x as i32, _ => return Err(InterpError::new("atomic_i32_new expects i32")) };
    Ok(Value::Int(crate::runtime::mimi_atomic_i32_new(v)))
}

fn builtin_atomic_i32_load(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let h = handle(args, 0)?;
    Ok(Value::Int(crate::runtime::mimi_atomic_i32_load(h) as i64))
}

fn builtin_atomic_i32_store(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let h = handle(args, 0)?;
    let v = match &args[1] { Value::Int(x) => *x as i32, _ => return Err(InterpError::new("expects i32")) };
    crate::runtime::mimi_atomic_i32_store(h, v);
    Ok(Value::Unit)
}

fn builtin_atomic_i32_fetch_add(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let h = handle(args, 0)?;
    let d = match &args[1] { Value::Int(x) => *x as i32, _ => return Err(InterpError::new("expects i32")) };
    Ok(Value::Int(crate::runtime::mimi_atomic_i32_fetch_add(h, d) as i64))
}

fn builtin_atomic_i32_compare_exchange(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let h = handle(args, 0)?;
    let exp = match &args[1] { Value::Int(x) => *x as i32, _ => return Err(InterpError::new("expects i32")) };
    let nv = match &args[2] { Value::Int(x) => *x as i32, _ => return Err(InterpError::new("expects i32")) };
    Ok(Value::Int(crate::runtime::mimi_atomic_i32_compare_exchange(h, exp, nv) as i64))
}

fn builtin_atomic_i32_drop(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let h = handle(args, 0)?;
    crate::runtime::mimi_atomic_i32_drop(h);
    Ok(Value::Unit)
}

// ── Atomic i64 ──────────────────────────────────────────

fn builtin_atomic_i64_new(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let v = handle(args, 0)?;
    Ok(Value::Int(crate::runtime::mimi_atomic_i64_new(v)))
}

fn builtin_atomic_i64_load(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let h = handle(args, 0)?;
    Ok(Value::Int(crate::runtime::mimi_atomic_i64_load(h)))
}

fn builtin_atomic_i64_store(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let h = handle(args, 0)?;
    let v = handle(args, 1)?;
    crate::runtime::mimi_atomic_i64_store(h, v);
    Ok(Value::Unit)
}

fn builtin_atomic_i64_fetch_add(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let h = handle(args, 0)?;
    let d = handle(args, 1)?;
    Ok(Value::Int(crate::runtime::mimi_atomic_i64_fetch_add(h, d)))
}

fn builtin_atomic_i64_drop(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let h = handle(args, 0)?;
    crate::runtime::mimi_atomic_i64_drop(h);
    Ok(Value::Unit)
}

// ── Atomic bool ─────────────────────────────────────────

fn builtin_atomic_bool_new(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let v = match &args[0] { Value::Bool(b) => if *b { 1 } else { 0 }, _ => return Err(InterpError::new("expects bool")) };
    Ok(Value::Int(crate::runtime::mimi_atomic_bool_new(v)))
}

fn builtin_atomic_bool_load(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let h = handle(args, 0)?;
    Ok(Value::Bool(crate::runtime::mimi_atomic_bool_load(h) != 0))
}

fn builtin_atomic_bool_store(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let h = handle(args, 0)?;
    let v = match &args[1] { Value::Bool(b) => if *b { 1i32 } else { 0i32 }, _ => return Err(InterpError::new("expects bool")) };
    crate::runtime::mimi_atomic_bool_store(h, v);
    Ok(Value::Unit)
}

fn builtin_atomic_bool_drop(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let h = handle(args, 0)?;
    crate::runtime::mimi_atomic_bool_drop(h);
    Ok(Value::Unit)
}

// ── Mutex ───────────────────────────────────────────────

fn builtin_mutex_new(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let v = handle(args, 0)?;
    Ok(Value::Int(crate::runtime::mimi_mutex_new(v)))
}

fn builtin_mutex_lock(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let h = handle(args, 0)?;
    Ok(Value::Int(crate::runtime::mimi_mutex_lock(h)))
}

fn builtin_mutex_get(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let h = handle(args, 0)?;
    Ok(Value::Int(crate::runtime::mimi_mutex_get(h)))
}

fn builtin_mutex_set(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let h = handle(args, 0)?;
    let v = handle(args, 1)?;
    crate::runtime::mimi_mutex_set(h, v);
    Ok(Value::Unit)
}

fn builtin_mutex_unlock(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let h = handle(args, 0)?;
    crate::runtime::mimi_mutex_unlock(h);
    Ok(Value::Unit)
}

fn builtin_mutex_drop(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let h = handle(args, 0)?;
    crate::runtime::mimi_mutex_drop(h);
    Ok(Value::Unit)
}

// ── Channel ─────────────────────────────────────────────

fn builtin_channel_new(_vm: &mut BytecodeVM<'_>, _args: &[Value]) -> Result<Value, InterpError> {
    Ok(Value::Int(crate::runtime::mimi_channel_new()))
}

fn builtin_channel_send(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let h = handle(args, 0)?;
    let v = handle(args, 1)?;
    crate::runtime::mimi_channel_send(h, v);
    Ok(Value::Unit)
}

fn builtin_channel_recv(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let h = handle(args, 0)?;
    Ok(Value::Int(crate::runtime::mimi_channel_recv(h)))
}

fn builtin_channel_try_recv(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let h = handle(args, 0)?;
    Ok(Value::Int(crate::runtime::mimi_channel_try_recv(h)))
}

fn builtin_channel_drop(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let h = handle(args, 0)?;
    crate::runtime::mimi_channel_drop(h);
    Ok(Value::Unit)
}
