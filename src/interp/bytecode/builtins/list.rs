//! List builtins: len, push, pop, range, sort_list, find, is_empty.

use crate::interp::bytecode::registry::{BuiltinCategory, BuiltinDesc, BuiltinRegistry};
use crate::interp::bytecode::vm::BytecodeVM;
use crate::interp::error::InterpError;
use crate::interp::value::Value;

pub fn register(reg: &mut BuiltinRegistry) {
    reg.register(BuiltinDesc { name: "len", arity: 1, category: BuiltinCategory::List, func: builtin_len });
    reg.register(BuiltinDesc { name: "push", arity: 2, category: BuiltinCategory::List, func: builtin_push });
    reg.register(BuiltinDesc { name: "pop", arity: 1, category: BuiltinCategory::List, func: builtin_pop });
    reg.register(BuiltinDesc { name: "range", arity: 2, category: BuiltinCategory::List, func: builtin_range });
    reg.register(BuiltinDesc { name: "sort_list", arity: 1, category: BuiltinCategory::List, func: builtin_sort_list });
    reg.register(BuiltinDesc { name: "find", arity: 2, category: BuiltinCategory::List, func: builtin_find });
    reg.register(BuiltinDesc { name: "is_empty", arity: 1, category: BuiltinCategory::List, func: builtin_is_empty });
}

fn builtin_len(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let len = match &args[0] {
        Value::List(l) => l.len(),
        Value::String(s) => s.len(),
        Value::Tuple(t) => t.len(),
        Value::Set(s) => s.len(),
        other => return Err(InterpError::new(format!("len: unsupported type {}", other))),
    };
    Ok(Value::Int(len as i64))
}

fn builtin_push(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::List(l) => {
            let mut new_list = l.clone();
            new_list.push(args[1].clone());
            Ok(Value::List(new_list))
        }
        other => Err(InterpError::new(format!(
            "push: first argument must be a list, found {}",
            other
        ))),
    }
}

fn builtin_pop(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::List(l) => {
            let mut new_list = l.clone();
            new_list
                .pop()
                .ok_or_else(|| InterpError::new("pop from empty list"))
        }
        other => Err(InterpError::new(format!(
            "pop: argument must be a list, found {}",
            other
        ))),
    }
}

fn builtin_range(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let start = match &args[0] {
        Value::Int(v) => *v,
        _ => return Err(InterpError::new("range start must be integer")),
    };
    let end = match &args[1] {
        Value::Int(v) => *v,
        _ => return Err(InterpError::new("range end must be integer")),
    };
    let list: Vec<Value> = (start..end).map(Value::Int).collect();
    Ok(Value::List(list))
}

fn builtin_sort_list(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let mut list = match &args[0] {
        Value::List(l) => l.clone(),
        _ => return Err(InterpError::new("sort_list: argument must be a list")),
    };
    // Typed comparison (D11): Int<Int, Float<Float, String<String.
    list.sort_by(|a, b| match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => {
            x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal)
        }
        (Value::String(x), Value::String(y)) => x.cmp(y),
        _ => std::cmp::Ordering::Equal,
    });
    Ok(Value::List(list))
}

fn builtin_find(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let list = match &args[0] {
        Value::List(l) => l,
        _ => return Err(InterpError::new("find: first argument must be a list")),
    };
    let target = &args[1];
    for (i, elem) in list.iter().enumerate() {
        if elem == target {
            return Ok(Value::Tuple(vec![Value::Bool(true), Value::Int(i as i64)]));
        }
    }
    Ok(Value::Tuple(vec![Value::Bool(false), Value::Int(-1)]))
}

fn builtin_is_empty(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::List(l) => Ok(Value::Bool(l.is_empty())),
        Value::String(s) => Ok(Value::Bool(s.is_empty())),
        _ => Err(InterpError::new(
            "is_empty: argument must be a list or string",
        )),
    }
}
