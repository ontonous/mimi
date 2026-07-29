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
    // Session (cross-wired channels)
    reg.register(BuiltinDesc { name: "session_pair", arity: 0, category: BuiltinCategory::System, func: builtin_session_pair });
    reg.register(BuiltinDesc { name: "session_send", arity: 2, category: BuiltinCategory::System, func: builtin_session_send });
    reg.register(BuiltinDesc { name: "session_recv", arity: 1, category: BuiltinCategory::System, func: builtin_session_recv });
    reg.register(BuiltinDesc { name: "session_close", arity: 1, category: BuiltinCategory::System, func: builtin_session_close });
    // Actor quota
    reg.register(BuiltinDesc { name: "actor_max_children", arity: 0, category: BuiltinCategory::System, func: builtin_actor_max_children });
    reg.register(BuiltinDesc { name: "actor_set_max_children", arity: 1, category: BuiltinCategory::System, func: builtin_actor_set_max_children });
    reg.register(BuiltinDesc { name: "actor_spawn_count", arity: 0, category: BuiltinCategory::System, func: builtin_actor_spawn_count });
    // Actor management
    reg.register(BuiltinDesc { name: "actor_set_mailbox_depth", arity: 2, category: BuiltinCategory::System, func: builtin_actor_set_mailbox_depth });
    reg.register(BuiltinDesc { name: "actor_mailbox_depth", arity: 1, category: BuiltinCategory::System, func: builtin_actor_mailbox_depth });
    reg.register(BuiltinDesc { name: "actor_is_faulted", arity: 1, category: BuiltinCategory::System, func: builtin_actor_is_faulted });
    reg.register(BuiltinDesc { name: "actor_is_muted", arity: 1, category: BuiltinCategory::System, func: builtin_actor_is_muted });
    reg.register(BuiltinDesc { name: "broadcast", arity: 2, category: BuiltinCategory::System, func: builtin_broadcast });
    // Flow test utilities
    reg.register(BuiltinDesc { name: "assert_state", arity: 2, category: BuiltinCategory::System, func: builtin_assert_state });
    reg.register(BuiltinDesc { name: "inject_fault", arity: 1, category: BuiltinCategory::System, func: builtin_inject_fault });
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

// ── Session (cross-wired channels) ─────────────────────

fn builtin_session_pair(_vm: &mut BytecodeVM<'_>, _args: &[Value]) -> Result<Value, InterpError> {
    let packed = crate::runtime::mimi_session_pair();
    let lo = crate::runtime::mimi_session_lo(packed);
    let hi = crate::runtime::mimi_session_hi(packed);
    Ok(Value::List(vec![Value::Int(lo), Value::Int(hi)]))
}

fn builtin_session_send(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let h = handle(args, 0)?;
    let v = handle(args, 1)?;
    crate::runtime::mimi_channel_send(h, v);
    Ok(Value::Unit)
}

fn builtin_session_recv(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let h = handle(args, 0)?;
    Ok(Value::Int(crate::runtime::mimi_channel_recv(h)))
}

fn builtin_session_close(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let h = handle(args, 0)?;
    crate::runtime::mimi_channel_drop(h);
    Ok(Value::Unit)
}

// ── Actor quota ────────────────────────────────────────

fn builtin_actor_max_children(vm: &mut BytecodeVM<'_>, _args: &[Value]) -> Result<Value, InterpError> {
    Ok(Value::Int(vm.max_children.map(|n| n as i64).unwrap_or(0)))
}

fn builtin_actor_set_max_children(vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let n = match &args[0] {
        Value::Int(x) if *x <= 0 => None,
        Value::Int(x) => Some(*x as usize),
        _ => return Err(InterpError::new("actor_set_max_children expects i64")),
    };
    vm.max_children = n;
    crate::runtime::mimi_actor_set_max_children(n.map(|x| x as i64).unwrap_or(0));
    Ok(Value::Unit)
}

fn builtin_actor_spawn_count(vm: &mut BytecodeVM<'_>, _args: &[Value]) -> Result<Value, InterpError> {
    Ok(Value::Int(vm.spawn_count as i64))
}

// ── Actor management ───────────────────────────────────

fn builtin_actor_set_mailbox_depth(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let depth = match &args[1] {
        Value::Int(n) if *n > 0 => *n as usize,
        _ => return Err(InterpError::new("actor_set_mailbox_depth: depth must be positive i64")),
    };
    match &args[0] {
        Value::Actor(h) => {
            h.set_mailbox_depth_limit(depth);
            Ok(Value::Unit)
        }
        _ => Err(InterpError::new("actor_set_mailbox_depth expects actor handle")),
    }
}

fn builtin_actor_mailbox_depth(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::Actor(h) => Ok(Value::Int(h.mailbox_depth() as i64)),
        _ => Err(InterpError::new("actor_mailbox_depth expects actor handle")),
    }
}

fn builtin_actor_is_faulted(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::Actor(h) => Ok(Value::Bool(h.is_faulted())),
        _ => Err(InterpError::new("actor_is_faulted expects actor handle")),
    }
}

fn builtin_actor_is_muted(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::Actor(h) => Ok(Value::Int(if h.is_muted() { 1 } else { 0 })),
        _ => Err(InterpError::new("actor_is_muted expects actor handle")),
    }
}

fn builtin_broadcast(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let targets = match &args[0] {
        Value::List(items) => items.clone(),
        _ => return Err(InterpError::new("broadcast: first argument must be a List of actors")),
    };
    let method = match &args[1] {
        Value::String(s) => s.clone(),
        _ => return Err(InterpError::new("broadcast: second argument must be a method name string")),
    };
    let mut results = Vec::with_capacity(targets.len());
    for target in targets {
        match target {
            Value::Actor(handle) => {
                if handle.is_faulted() {
                    results.push(Value::Int(-1));
                    continue;
                }
                match handle.try_enqueue(method.clone(), vec![]) {
                    Ok(rx) => match rx.recv() {
                        Ok(Ok(v)) => results.push(v),
                        _ => results.push(Value::Int(-1)),
                    },
                    Err(_) => results.push(Value::Int(-1)),
                }
            }
            _ => results.push(Value::Int(-1)),
        }
    }
    Ok(Value::List(results))
}

// ── Flow test utilities ────────────────────────────────

fn builtin_assert_state(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let actual_state = match &args[0] {
        Value::Record(Some(name), _) => name.clone(),
        Value::Record(None, _) => "<anonymous>".to_string(),
        other => format!("{}", other),
    };
    let expected_state = match &args[1] {
        Value::String(s) => s.clone(),
        _ => return Err(InterpError::new("assert_state: state_name must be a string")),
    };
    if actual_state != expected_state {
        return Err(InterpError::new(format!(
            "state assertion failed: expected {}, got {}",
            expected_state, actual_state
        )));
    }
    Ok(Value::Unit)
}

fn builtin_inject_fault(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let state_name = match &args[0] {
        Value::Record(Some(name), _) => name.clone(),
        _ => "unknown".to_string(),
    };
    let mut fault_fields = std::collections::HashMap::new();
    fault_fields.insert("last_state".to_string(), Value::String(state_name));
    fault_fields.insert("unexpected_event".to_string(), Value::String("inject_fault".to_string()));
    fault_fields.insert("snapshot".to_string(), args[0].clone());
    fault_fields.insert("trace".to_string(), Value::Unit);
    Ok(Value::Record(Some("Fault".to_string()), fault_fields))
}
