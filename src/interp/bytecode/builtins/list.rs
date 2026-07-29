//! List / Map / Set builtins.

use crate::interp::bytecode::registry::{BuiltinCategory, BuiltinDesc, BuiltinRegistry};
use crate::interp::bytecode::vm::BytecodeVM;
use crate::interp::error::InterpError;
use crate::interp::value::Value;

pub fn register(reg: &mut BuiltinRegistry) {
    // Core list
    reg.register(BuiltinDesc { name: "len", arity: 1, category: BuiltinCategory::List, func: builtin_len });
    reg.register(BuiltinDesc { name: "size", arity: 1, category: BuiltinCategory::List, func: builtin_len });
    reg.register(BuiltinDesc { name: "push", arity: 2, category: BuiltinCategory::List, func: builtin_push });
    reg.register(BuiltinDesc { name: "pop", arity: 1, category: BuiltinCategory::List, func: builtin_pop });
    reg.register(BuiltinDesc { name: "range", arity: 2, category: BuiltinCategory::List, func: builtin_range });
    reg.register(BuiltinDesc { name: "is_empty", arity: 1, category: BuiltinCategory::List, func: builtin_is_empty });
    reg.register(BuiltinDesc { name: "find", arity: 2, category: BuiltinCategory::List, func: builtin_find });
    // Sort
    reg.register(BuiltinDesc { name: "sort_list", arity: 1, category: BuiltinCategory::List, func: builtin_sort_list });
    reg.register(BuiltinDesc { name: "sort", arity: 1, category: BuiltinCategory::List, func: builtin_sort_list });
    reg.register(BuiltinDesc { name: "sort_f64", arity: 1, category: BuiltinCategory::List, func: builtin_sort_list });
    reg.register(BuiltinDesc { name: "sort_str", arity: 1, category: BuiltinCategory::List, func: builtin_sort_list });
    // Transform
    reg.register(BuiltinDesc { name: "reverse", arity: 1, category: BuiltinCategory::List, func: builtin_reverse });
    reg.register(BuiltinDesc { name: "flatten", arity: 1, category: BuiltinCategory::List, func: builtin_flatten });
    reg.register(BuiltinDesc { name: "enumerate", arity: 1, category: BuiltinCategory::List, func: builtin_enumerate });
    reg.register(BuiltinDesc { name: "zip", arity: 2, category: BuiltinCategory::List, func: builtin_zip });
    reg.register(BuiltinDesc { name: "sum", arity: 1, category: BuiltinCategory::List, func: builtin_sum });
    reg.register(BuiltinDesc { name: "to_list", arity: 1, category: BuiltinCategory::List, func: builtin_to_list });
    reg.register(BuiltinDesc { name: "clone", arity: 1, category: BuiltinCategory::List, func: builtin_clone });
    // Higher-order aliases
    reg.register(BuiltinDesc { name: "map", arity: 2, category: BuiltinCategory::List, func: super::hof::builtin_map_list });
    reg.register(BuiltinDesc { name: "filter", arity: 2, category: BuiltinCategory::List, func: super::hof::builtin_filter_list });
    reg.register(BuiltinDesc { name: "reduce", arity: 3, category: BuiltinCategory::List, func: super::hof::builtin_reduce_list });
    // Map operations
    reg.register(BuiltinDesc { name: "map_new", arity: 0, category: BuiltinCategory::List, func: builtin_map_new });
    reg.register(BuiltinDesc { name: "map_get", arity: 2, category: BuiltinCategory::List, func: builtin_map_get });
    reg.register(BuiltinDesc { name: "map_set", arity: 3, category: BuiltinCategory::List, func: builtin_map_set });
    reg.register(BuiltinDesc { name: "map_remove", arity: 2, category: BuiltinCategory::List, func: builtin_map_remove });
    reg.register(BuiltinDesc { name: "map_size", arity: 1, category: BuiltinCategory::List, func: builtin_map_size });
    reg.register(BuiltinDesc { name: "map_from_list", arity: 1, category: BuiltinCategory::List, func: builtin_map_from_list });
    reg.register(BuiltinDesc { name: "has_key", arity: 2, category: BuiltinCategory::List, func: builtin_has_key });
    reg.register(BuiltinDesc { name: "keys", arity: 1, category: BuiltinCategory::List, func: builtin_keys });
    reg.register(BuiltinDesc { name: "values", arity: 1, category: BuiltinCategory::List, func: builtin_values });
    reg.register(BuiltinDesc { name: "insert", arity: 3, category: BuiltinCategory::List, func: builtin_map_set });
    reg.register(BuiltinDesc { name: "remove", arity: 2, category: BuiltinCategory::List, func: builtin_map_remove });
    // Option
    reg.register(BuiltinDesc { name: "option_value_or", arity: 2, category: BuiltinCategory::List, func: builtin_option_value_or });
    // Type reflection
    reg.register(BuiltinDesc { name: "type_name", arity: 1, category: BuiltinCategory::List, func: builtin_type_name });
}

// ── Core list ───────────────────────────────────────────

fn builtin_len(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let len = match &args[0] {
        Value::List(l) => l.len(),
        Value::String(s) => s.len(),
        Value::Tuple(t) => t.len(),
        Value::Set(s) => s.len(),
        Value::Record(_, fields) => fields.len(),
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
        other => Err(InterpError::new(format!("push: expected list, found {}", other))),
    }
}

fn builtin_pop(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::List(l) => {
            let mut new_list = l.clone();
            new_list.pop().ok_or_else(|| InterpError::new("pop from empty list"))
        }
        other => Err(InterpError::new(format!("pop: expected list, found {}", other))),
    }
}

fn builtin_range(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let start = match &args[0] { Value::Int(v) => *v, _ => return Err(InterpError::new("range start must be integer")) };
    let end = match &args[1] { Value::Int(v) => *v, _ => return Err(InterpError::new("range end must be integer")) };
    Ok(Value::List((start..end).map(Value::Int).collect()))
}

fn builtin_is_empty(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::List(l) => Ok(Value::Bool(l.is_empty())),
        Value::String(s) => Ok(Value::Bool(s.is_empty())),
        Value::Record(_, f) => Ok(Value::Bool(f.is_empty())),
        _ => Err(InterpError::new("is_empty: expected list, string, or map")),
    }
}

fn builtin_find(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let list = match &args[0] { Value::List(l) => l, _ => return Err(InterpError::new("find: expected list")) };
    let target = &args[1];
    for (i, elem) in list.iter().enumerate() {
        if elem == target {
            return Ok(Value::Tuple(vec![Value::Bool(true), Value::Int(i as i64)]));
        }
    }
    Ok(Value::Tuple(vec![Value::Bool(false), Value::Int(-1)]))
}

// ── Sort ────────────────────────────────────────────────

fn builtin_sort_list(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let mut list = match &args[0] { Value::List(l) => l.clone(), _ => return Err(InterpError::new("sort: expected list")) };
    list.sort_by(|a, b| match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal),
        (Value::String(x), Value::String(y)) => x.cmp(y),
        _ => std::cmp::Ordering::Equal,
    });
    Ok(Value::List(list))
}

// ── Transform ───────────────────────────────────────────

fn builtin_reverse(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::List(l) => {
            let mut reversed = l.clone();
            reversed.reverse();
            Ok(Value::List(reversed))
        }
        Value::String(s) => Ok(Value::String(s.chars().rev().collect())),
        _ => Err(InterpError::new("reverse: expected list or string")),
    }
}

fn builtin_flatten(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::List(l) => {
            let mut result = Vec::new();
            for elem in l {
                match elem {
                    Value::List(inner) => result.extend(inner.iter().cloned()),
                    other => result.push(other.clone()),
                }
            }
            Ok(Value::List(result))
        }
        _ => Err(InterpError::new("flatten: expected list")),
    }
}

fn builtin_enumerate(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::List(l) => {
            let result: Vec<Value> = l.iter().enumerate()
                .map(|(i, v)| Value::Tuple(vec![Value::Int(i as i64), v.clone()]))
                .collect();
            Ok(Value::List(result))
        }
        _ => Err(InterpError::new("enumerate: expected list")),
    }
}

fn builtin_zip(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::List(a), Value::List(b)) => {
            let result: Vec<Value> = a.iter().zip(b.iter())
                .map(|(x, y)| Value::Tuple(vec![x.clone(), y.clone()]))
                .collect();
            Ok(Value::List(result))
        }
        _ => Err(InterpError::new("zip: expected two lists")),
    }
}

fn builtin_sum(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::List(l) => {
            let mut int_sum: i64 = 0;
            let mut float_sum: f64 = 0.0;
            let mut has_float = false;
            for elem in l {
                match elem {
                    Value::Int(v) => int_sum += v,
                    Value::Float(v) => { float_sum += v; has_float = true; }
                    _ => return Err(InterpError::new("sum: list must contain only numbers")),
                }
            }
            if has_float {
                Ok(Value::Float(float_sum + int_sum as f64))
            } else {
                Ok(Value::Int(int_sum))
            }
        }
        _ => Err(InterpError::new("sum: expected list")),
    }
}

fn builtin_to_list(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::List(_) => Ok(args[0].clone()),
        Value::Tuple(t) => Ok(Value::List(t.clone())),
        Value::String(s) => Ok(Value::List(s.chars().map(|c| Value::String(c.to_string())).collect())),
        Value::Set(s) => Ok(Value::List(s.clone())),
        other => Ok(Value::List(vec![other.clone()])),
    }
}

fn builtin_clone(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    Ok(args[0].clone())
}

// ── Map operations ──────────────────────────────────────

fn builtin_map_new(_vm: &mut BytecodeVM<'_>, _args: &[Value]) -> Result<Value, InterpError> {
    Ok(Value::Record(None, std::collections::HashMap::new()))
}

fn builtin_map_get(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::Record(_, fields), Value::String(key)) => {
            match fields.get(key) {
                Some(v) => Ok(Value::Tuple(vec![Value::Bool(true), v.clone()])),
                None => Ok(Value::Tuple(vec![Value::Bool(false), Value::Unit])),
            }
        }
        _ => Err(InterpError::new("map_get: expected (map, string key)")),
    }
}

fn builtin_map_set(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::Record(ty, fields), Value::String(key)) => {
            let mut new_fields = fields.clone();
            new_fields.insert(key.clone(), args[2].clone());
            Ok(Value::Record(ty.clone(), new_fields))
        }
        _ => Err(InterpError::new("map_set: expected (map, string key, value)")),
    }
}

fn builtin_map_remove(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::Record(ty, fields), Value::String(key)) => {
            let mut new_fields = fields.clone();
            new_fields.remove(key);
            Ok(Value::Record(ty.clone(), new_fields))
        }
        _ => Err(InterpError::new("map_remove: expected (map, string key)")),
    }
}

fn builtin_map_size(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::Record(_, fields) => Ok(Value::Int(fields.len() as i64)),
        _ => Err(InterpError::new("map_size: expected map")),
    }
}

fn builtin_map_from_list(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::List(pairs) => {
            let mut fields = std::collections::HashMap::new();
            for pair in pairs {
                match pair {
                    Value::Tuple(kv) if kv.len() == 2 => {
                        if let Value::String(k) = &kv[0] {
                            fields.insert(k.clone(), kv[1].clone());
                        }
                    }
                    _ => return Err(InterpError::new("map_from_list: expected list of (key, value) tuples")),
                }
            }
            Ok(Value::Record(None, fields))
        }
        _ => Err(InterpError::new("map_from_list: expected list")),
    }
}

fn builtin_has_key(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::Record(_, fields), Value::String(key)) => Ok(Value::Bool(fields.contains_key(key))),
        _ => Err(InterpError::new("has_key: expected (map, string key)")),
    }
}

fn builtin_keys(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::Record(_, fields) => {
            let keys: Vec<Value> = fields.keys().map(|k| Value::String(k.clone())).collect();
            Ok(Value::List(keys))
        }
        _ => Err(InterpError::new("keys: expected map")),
    }
}

fn builtin_values(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::Record(_, fields) => {
            let values: Vec<Value> = fields.values().cloned().collect();
            Ok(Value::List(values))
        }
        _ => Err(InterpError::new("values: expected map")),
    }
}

// ── Option ──────────────────────────────────────────────

fn builtin_option_value_or(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::Variant(name, payload) if name == "Some" || name == "Ok" => {
            Ok(payload.first().cloned().unwrap_or(Value::Unit))
        }
        _ => Ok(args[1].clone()),
    }
}

// ── Type reflection ─────────────────────────────────────

fn builtin_type_name(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let name = crate::interp::type_name(&args[0]);
    Ok(Value::String(name.to_string()))
}
