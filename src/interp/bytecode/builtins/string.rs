//! String builtins: formatting, searching, transforming, regex.

use crate::interp::bytecode::registry::{BuiltinCategory, BuiltinDesc, BuiltinRegistry};
use crate::interp::bytecode::vm::BytecodeVM;
use crate::interp::error::InterpError;
use crate::interp::value::Value;

pub fn register(reg: &mut BuiltinRegistry) {
    // Formatting
    reg.register(BuiltinDesc { name: "format", arity: usize::MAX, category: BuiltinCategory::String, func: builtin_format });
    // Substring / search
    reg.register(BuiltinDesc { name: "str_substring", arity: 3, category: BuiltinCategory::String, func: builtin_str_substring });
    reg.register(BuiltinDesc { name: "substring", arity: 3, category: BuiltinCategory::String, func: builtin_str_substring });
    reg.register(BuiltinDesc { name: "str_split", arity: 2, category: BuiltinCategory::String, func: builtin_str_split });
    reg.register(BuiltinDesc { name: "split", arity: 2, category: BuiltinCategory::String, func: builtin_str_split });
    reg.register(BuiltinDesc { name: "str_join", arity: 2, category: BuiltinCategory::String, func: builtin_str_join });
    reg.register(BuiltinDesc { name: "str_contains", arity: 2, category: BuiltinCategory::String, func: builtin_str_contains });
    reg.register(BuiltinDesc { name: "contains", arity: 2, category: BuiltinCategory::String, func: builtin_str_contains });
    reg.register(BuiltinDesc { name: "str_starts_with", arity: 2, category: BuiltinCategory::String, func: builtin_str_starts_with });
    reg.register(BuiltinDesc { name: "starts_with", arity: 2, category: BuiltinCategory::String, func: builtin_str_starts_with });
    reg.register(BuiltinDesc { name: "str_ends_with", arity: 2, category: BuiltinCategory::String, func: builtin_str_ends_with });
    reg.register(BuiltinDesc { name: "ends_with", arity: 2, category: BuiltinCategory::String, func: builtin_str_ends_with });
    reg.register(BuiltinDesc { name: "str_index_of", arity: 2, category: BuiltinCategory::String, func: builtin_str_index_of });
    reg.register(BuiltinDesc { name: "index_of", arity: 2, category: BuiltinCategory::String, func: builtin_str_index_of });
    reg.register(BuiltinDesc { name: "str_count_substring", arity: 2, category: BuiltinCategory::String, func: builtin_str_count_substring });
    // Transform
    reg.register(BuiltinDesc { name: "str_replace", arity: 3, category: BuiltinCategory::String, func: builtin_str_replace });
    reg.register(BuiltinDesc { name: "replace", arity: 3, category: BuiltinCategory::String, func: builtin_str_replace });
    reg.register(BuiltinDesc { name: "str_trim", arity: 1, category: BuiltinCategory::String, func: builtin_str_trim });
    reg.register(BuiltinDesc { name: "trim", arity: 1, category: BuiltinCategory::String, func: builtin_str_trim });
    reg.register(BuiltinDesc { name: "str_to_upper", arity: 1, category: BuiltinCategory::String, func: builtin_str_to_upper });
    reg.register(BuiltinDesc { name: "to_upper", arity: 1, category: BuiltinCategory::String, func: builtin_str_to_upper });
    reg.register(BuiltinDesc { name: "str_to_lower", arity: 1, category: BuiltinCategory::String, func: builtin_str_to_lower });
    reg.register(BuiltinDesc { name: "to_lower", arity: 1, category: BuiltinCategory::String, func: builtin_str_to_lower });
    reg.register(BuiltinDesc { name: "str_repeat", arity: 2, category: BuiltinCategory::String, func: builtin_str_repeat });
    reg.register(BuiltinDesc { name: "repeat", arity: 2, category: BuiltinCategory::String, func: builtin_str_repeat });
    // Char operations
    reg.register(BuiltinDesc { name: "str_char_at", arity: 2, category: BuiltinCategory::String, func: builtin_str_char_at });
    reg.register(BuiltinDesc { name: "char_at", arity: 2, category: BuiltinCategory::String, func: builtin_str_char_at });
    reg.register(BuiltinDesc { name: "char_code", arity: 1, category: BuiltinCategory::String, func: builtin_char_code });
    reg.register(BuiltinDesc { name: "chr", arity: 1, category: BuiltinCategory::String, func: builtin_chr });
    // Parse
    reg.register(BuiltinDesc { name: "str_parse_int", arity: 1, category: BuiltinCategory::String, func: builtin_str_parse_int });
    reg.register(BuiltinDesc { name: "parse_int", arity: 1, category: BuiltinCategory::String, func: builtin_str_parse_int });
    reg.register(BuiltinDesc { name: "str_parse_float", arity: 1, category: BuiltinCategory::String, func: builtin_str_parse_float });
    reg.register(BuiltinDesc { name: "parse_float", arity: 1, category: BuiltinCategory::String, func: builtin_str_parse_float });
    reg.register(BuiltinDesc { name: "string_to_int", arity: 1, category: BuiltinCategory::String, func: builtin_str_parse_int });
    // Convert
    reg.register(BuiltinDesc { name: "float_to_string", arity: 1, category: BuiltinCategory::String, func: builtin_to_string_val });
    reg.register(BuiltinDesc { name: "int_to_string", arity: 1, category: BuiltinCategory::String, func: builtin_to_string_val });
    // Regex
    reg.register(BuiltinDesc { name: "regex_match", arity: 2, category: BuiltinCategory::String, func: builtin_regex_match });
    reg.register(BuiltinDesc { name: "regex_find", arity: 2, category: BuiltinCategory::String, func: builtin_regex_find });
    reg.register(BuiltinDesc { name: "regex_replace", arity: 3, category: BuiltinCategory::String, func: builtin_regex_replace });
}

// ── Formatting ──────────────────────────────────────────

fn builtin_format(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    if args.is_empty() {
        return Err(InterpError::new("format expects at least 1 argument"));
    }
    let template = match &args[0] {
        Value::String(s) => s.clone(),
        _ => return Err(InterpError::new("format expects a string template")),
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

// ── Substring / search ──────────────────────────────────

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
        (Value::String(s), Value::String(d)) => {
            let parts: Vec<Value> = s.split(d.as_str()).map(|p| Value::String(p.to_string())).collect();
            Ok(Value::List(parts))
        }
        _ => Err(InterpError::new("str_split expects (string, string)")),
    }
}

fn builtin_str_join(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::List(parts), Value::String(sep)) => {
            let strings: Vec<String> = parts.iter().map(|p| p.to_string()).collect();
            Ok(Value::String(strings.join(sep)))
        }
        _ => Err(InterpError::new("str_join expects (list, string)")),
    }
}

fn builtin_str_contains(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::String(s), Value::String(sub)) => Ok(Value::Bool(s.contains(sub.as_str()))),
        (Value::List(l), target) => Ok(Value::Bool(l.contains(target))),
        (Value::Set(s), target) => Ok(Value::Bool(s.contains(target))),
        _ => Err(InterpError::new("contains expects (string/list/set, value)")),
    }
}

fn builtin_str_starts_with(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::String(s), Value::String(prefix)) => Ok(Value::Bool(s.starts_with(prefix.as_str()))),
        _ => Err(InterpError::new("starts_with expects (string, string)")),
    }
}

fn builtin_str_ends_with(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::String(s), Value::String(suffix)) => Ok(Value::Bool(s.ends_with(suffix.as_str()))),
        _ => Err(InterpError::new("ends_with expects (string, string)")),
    }
}

fn builtin_str_index_of(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::String(s), Value::String(sub)) => {
            match s.find(sub.as_str()) {
                Some(i) => Ok(Value::Int(i as i64)),
                None => Ok(Value::Int(-1)),
            }
        }
        _ => Err(InterpError::new("index_of expects (string, string)")),
    }
}

fn builtin_str_count_substring(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::String(s), Value::String(sub)) => {
            if sub.is_empty() {
                return Ok(Value::Int(0));
            }
            Ok(Value::Int(s.matches(sub.as_str()).count() as i64))
        }
        _ => Err(InterpError::new("count_substring expects (string, string)")),
    }
}

// ── Transform ───────────────────────────────────────────

fn builtin_str_replace(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1], &args[2]) {
        (Value::String(s), Value::String(from), Value::String(to)) => {
            Ok(Value::String(s.replace(from.as_str(), to.as_str())))
        }
        _ => Err(InterpError::new("replace expects (string, string, string)")),
    }
}

fn builtin_str_trim(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::String(s) => Ok(Value::String(s.trim().to_string())),
        _ => Err(InterpError::new("trim expects a string")),
    }
}

fn builtin_str_to_upper(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::String(s) => Ok(Value::String(s.to_uppercase())),
        _ => Err(InterpError::new("to_upper expects a string")),
    }
}

fn builtin_str_to_lower(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::String(s) => Ok(Value::String(s.to_lowercase())),
        _ => Err(InterpError::new("to_lower expects a string")),
    }
}

fn builtin_str_repeat(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::String(s), Value::Int(n)) => {
            if *n < 0 {
                return Err(InterpError::new("repeat count must be non-negative"));
            }
            Ok(Value::String(s.repeat(*n as usize)))
        }
        _ => Err(InterpError::new("repeat expects (string, int)")),
    }
}

// ── Char operations ─────────────────────────────────────

fn builtin_str_char_at(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::String(s), Value::Int(idx)) => {
            let i = *idx as usize;
            s.chars().nth(i)
                .map(|c| Value::String(c.to_string()))
                .ok_or_else(|| InterpError::new(format!("char_at: index {} out of bounds", i)))
        }
        _ => Err(InterpError::new("char_at expects (string, int)")),
    }
}

fn builtin_char_code(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::String(s) => {
            s.chars().next()
                .map(|c| Value::Int(c as i64))
                .ok_or_else(|| InterpError::new("char_code: empty string"))
        }
        _ => Err(InterpError::new("char_code expects a string")),
    }
}

fn builtin_chr(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::Int(code) => {
            char::from_u32(*code as u32)
                .map(|c| Value::String(c.to_string()))
                .ok_or_else(|| InterpError::new(format!("chr: invalid code point {}", code)))
        }
        _ => Err(InterpError::new("chr expects an integer")),
    }
}

// ── Parse ───────────────────────────────────────────────

fn builtin_str_parse_int(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::String(s) => match s.trim().parse::<i64>() {
            Ok(n) => Ok(Value::Tuple(vec![Value::Bool(true), Value::Int(n)])),
            Err(_) => Ok(Value::Tuple(vec![Value::Bool(false), Value::Int(0)])),
        },
        _ => Err(InterpError::new("parse_int expects a string")),
    }
}

fn builtin_str_parse_float(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::String(s) => match s.trim().parse::<f64>() {
            Ok(n) => Ok(Value::Tuple(vec![Value::Bool(true), Value::Float(n)])),
            Err(_) => Ok(Value::Tuple(vec![Value::Bool(false), Value::Float(0.0)])),
        },
        _ => Err(InterpError::new("parse_float expects a string")),
    }
}

fn builtin_to_string_val(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    Ok(Value::String(args[0].to_string()))
}

// ── Regex ───────────────────────────────────────────────

fn builtin_regex_match(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::String(text), Value::String(pattern)) => {
            let re = regex::Regex::new(pattern)
                .map_err(|e| InterpError::new(format!("regex error: {}", e)))?;
            Ok(Value::Bool(re.is_match(text)))
        }
        _ => Err(InterpError::new("regex_match expects (string, string)")),
    }
}

fn builtin_regex_find(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::String(text), Value::String(pattern)) => {
            let re = regex::Regex::new(pattern)
                .map_err(|e| InterpError::new(format!("regex error: {}", e)))?;
            match re.find(text) {
                Some(m) => Ok(Value::String(m.as_str().to_string())),
                None => Ok(Value::String(String::new())),
            }
        }
        _ => Err(InterpError::new("regex_find expects (string, string)")),
    }
}

fn builtin_regex_replace(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1], &args[2]) {
        (Value::String(text), Value::String(pattern), Value::String(replacement)) => {
            let re = regex::Regex::new(pattern)
                .map_err(|e| InterpError::new(format!("regex error: {}", e)))?;
            Ok(Value::String(re.replace_all(text, replacement.as_str()).to_string()))
        }
        _ => Err(InterpError::new("regex_replace expects (string, string, string)")),
    }
}
