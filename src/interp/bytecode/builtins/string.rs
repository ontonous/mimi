//! String builtins: str_substring, str_split, str_join, str_contains, str_parse_int, str_parse_float.

use crate::interp::bytecode::registry::{BuiltinCategory, BuiltinDesc, BuiltinRegistry};
use crate::interp::bytecode::vm::BytecodeVM;
use crate::interp::error::InterpError;
use crate::interp::value::Value;

pub fn register(reg: &mut BuiltinRegistry) {
    reg.register(BuiltinDesc { name: "format", arity: usize::MAX, category: BuiltinCategory::String, func: builtin_format });
    reg.register(BuiltinDesc { name: "str_substring", arity: 3, category: BuiltinCategory::String, func: builtin_str_substring });
    reg.register(BuiltinDesc { name: "str_split", arity: 2, category: BuiltinCategory::String, func: builtin_str_split });
    reg.register(BuiltinDesc { name: "str_join", arity: 2, category: BuiltinCategory::String, func: builtin_str_join });
    reg.register(BuiltinDesc { name: "str_contains", arity: 2, category: BuiltinCategory::String, func: builtin_str_contains });
    reg.register(BuiltinDesc { name: "str_parse_int", arity: 1, category: BuiltinCategory::String, func: builtin_str_parse_int });
    reg.register(BuiltinDesc { name: "str_parse_float", arity: 1, category: BuiltinCategory::String, func: builtin_str_parse_float });
}

fn builtin_format(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    if args.is_empty() {
        return Err(InterpError::new(
            "format expects at least 1 argument (template string)",
        ));
    }
    let template = match &args[0] {
        Value::String(s) => s.clone(),
        _ => {
            return Err(InterpError::new(
                "format expects a string template as first argument",
            ))
        }
    };
    let mut result = String::new();
    let mut rest = template.as_str();
    let mut arg_idx = 1;
    while let Some(pos) = rest.find("{}") {
        result.push_str(&rest[..pos]);
        if arg_idx < args.len() {
            result.push_str(&args[arg_idx].to_string());
            arg_idx += 1;
        } else {
            result.push_str("{}");
        }
        rest = &rest[pos + 2..];
    }
    result.push_str(rest);
    Ok(Value::String(result))
}

fn builtin_str_substring(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1], &args[2]) {
        (Value::String(s), Value::Int(start), Value::Int(end)) => {
            let chars: Vec<char> = s.chars().collect();
            let s_idx = (*start as usize).min(chars.len());
            let e_idx = (*end as usize).min(chars.len());
            if s_idx > e_idx {
                return Err(InterpError::new("str_substring: start > end"));
            }
            Ok(Value::String(chars[s_idx..e_idx].iter().collect()))
        }
        _ => Err(InterpError::new("str_substring expects (string, int, int)")),
    }
}

fn builtin_str_split(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::String(s), Value::String(delimiter)) => {
            let parts: Vec<Value> = s
                .split(delimiter.as_str())
                .map(|p| Value::String(p.to_string()))
                .collect();
            Ok(Value::List(parts))
        }
        _ => Err(InterpError::new("str_split expects (string, string)")),
    }
}

fn builtin_str_join(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::List(parts), Value::String(sep)) => {
            let mut strings = Vec::new();
            for p in parts {
                match p {
                    Value::String(s) => strings.push(s.clone()),
                    _ => return Err(InterpError::new("str_join: list elements must be strings")),
                }
            }
            Ok(Value::String(strings.join(sep)))
        }
        _ => Err(InterpError::new("str_join expects (list, string)")),
    }
}

fn builtin_str_contains(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::String(s), Value::String(sub)) => Ok(Value::Bool(s.contains(sub.as_str()))),
        _ => Err(InterpError::new("str_contains expects (string, string)")),
    }
}

fn builtin_str_parse_int(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::String(s) => match s.parse::<i64>() {
            Ok(n) => Ok(Value::Int(n)),
            Err(_) => Ok(Value::Int(0)),
        },
        _ => Err(InterpError::new("str_parse_int expects a string")),
    }
}

fn builtin_str_parse_float(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::String(s) => match s.parse::<f64>() {
            Ok(n) => Ok(Value::Float(n)),
            Err(_) => Ok(Value::Float(0.0)),
        },
        _ => Err(InterpError::new("str_parse_float expects a string")),
    }
}
