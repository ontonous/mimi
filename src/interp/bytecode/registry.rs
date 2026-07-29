//! Builtin function registry for the bytecode VM.
//!
//! Design principles (D1/D2):
//! - Each builtin is a standalone function: `fn(vm, args) -> Result<Value>`
//! - Registration is declarative: `BuiltinDesc { name, arity, category, func }`
//! - Arity is checked automatically before dispatch
//! - No giant match statement

use super::vm::BytecodeVM;
use crate::interp::error::InterpError;
use crate::interp::value::Value;
use std::collections::HashMap;

/// Builtin function category (for organization and future type-directed dispatch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinCategory {
    Io,
    Convert,
    String,
    Math,
    List,
    HigherOrder,
    System,
}

/// A builtin function implementation.
/// Takes the VM (for stdout, closure calls, etc.) and the argument slice.
pub type BuiltinFn = fn(&mut BytecodeVM<'_>, &[Value]) -> Result<Value, InterpError>;

/// Descriptor for a builtin function.
pub struct BuiltinDesc {
    /// Builtin name (as called from Mimi code).
    pub name: &'static str,
    /// Expected number of arguments. `usize::MAX` = variadic.
    pub arity: usize,
    /// Category for organization.
    pub category: BuiltinCategory,
    /// Implementation.
    pub func: BuiltinFn,
}

/// Registry of all builtin functions.
pub struct BuiltinRegistry {
    /// Name → index mapping.
    name_to_idx: HashMap<&'static str, u32>,
    /// Descriptors in index order (index = BuiltinIdx).
    descs: Vec<BuiltinDesc>,
}

impl BuiltinRegistry {
    pub fn new() -> Self {
        BuiltinRegistry {
            name_to_idx: HashMap::new(),
            descs: Vec::new(),
        }
    }

    /// Register a builtin. Returns its index (BuiltinIdx).
    pub fn register(&mut self, desc: BuiltinDesc) -> u32 {
        let idx = self.descs.len() as u32;
        self.name_to_idx.insert(desc.name, idx);
        self.descs.push(desc);
        idx
    }

    /// Look up a builtin by name.
    pub fn lookup(&self, name: &str) -> Option<u32> {
        self.name_to_idx.get(name).copied()
    }

    /// Call a builtin by index. Performs automatic arity check.
    pub fn call(
        &self,
        vm: &mut BytecodeVM<'_>,
        idx: u32,
        args: &[Value],
    ) -> Result<Value, InterpError> {
        let desc = &self.descs[idx as usize];
        if desc.arity != usize::MAX && args.len() != desc.arity {
            return Err(InterpError::new(format!(
                "{} expects {} argument(s), got {}",
                desc.name,
                desc.arity,
                args.len()
            )));
        }
        (desc.func)(vm, args)
    }

    /// Get the function pointer and arity for a builtin (for VM dispatch without borrow conflict).
    pub fn get_func(&self, idx: u32) -> (BuiltinFn, usize, &'static str) {
        let desc = &self.descs[idx as usize];
        (desc.func, desc.arity, desc.name)
    }

    /// Get all builtin names in index order (for compiler registration).
    pub fn names(&self) -> Vec<String> {
        self.descs.iter().map(|d| d.name.to_string()).collect()
    }

    /// Number of registered builtins.
    pub fn len(&self) -> usize {
        self.descs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.descs.is_empty()
    }
}

/// Create a registry with all builtins registered.
pub fn create_registry() -> BuiltinRegistry {
    let mut reg = BuiltinRegistry::new();

    // ── IO ──────────────────────────────────────────────
    reg.register(BuiltinDesc { name: "println", arity: usize::MAX, category: BuiltinCategory::Io, func: builtin_println });
    reg.register(BuiltinDesc { name: "print", arity: usize::MAX, category: BuiltinCategory::Io, func: builtin_print });
    reg.register(BuiltinDesc { name: "print_err", arity: usize::MAX, category: BuiltinCategory::Io, func: builtin_print_err });
    reg.register(BuiltinDesc { name: "input_line", arity: 0, category: BuiltinCategory::Io, func: builtin_input_line });
    reg.register(BuiltinDesc { name: "input_int", arity: 0, category: BuiltinCategory::Io, func: builtin_input_int });

    // ── Convert ─────────────────────────────────────────
    reg.register(BuiltinDesc { name: "to_int", arity: 1, category: BuiltinCategory::Convert, func: builtin_to_int });
    reg.register(BuiltinDesc { name: "to_float", arity: 1, category: BuiltinCategory::Convert, func: builtin_to_float });
    reg.register(BuiltinDesc { name: "to_string", arity: 1, category: BuiltinCategory::Convert, func: builtin_to_string });
    reg.register(BuiltinDesc { name: "str", arity: 1, category: BuiltinCategory::Convert, func: builtin_to_string });
    reg.register(BuiltinDesc { name: "int", arity: 1, category: BuiltinCategory::Convert, func: builtin_to_int });
    reg.register(BuiltinDesc { name: "float", arity: 1, category: BuiltinCategory::Convert, func: builtin_to_float });

    // ── String ──────────────────────────────────────────
    reg.register(BuiltinDesc { name: "str_substring", arity: 3, category: BuiltinCategory::String, func: builtin_str_substring });
    reg.register(BuiltinDesc { name: "str_split", arity: 2, category: BuiltinCategory::String, func: builtin_str_split });
    reg.register(BuiltinDesc { name: "str_join", arity: 2, category: BuiltinCategory::String, func: builtin_str_join });
    reg.register(BuiltinDesc { name: "str_contains", arity: 2, category: BuiltinCategory::String, func: builtin_str_contains });
    reg.register(BuiltinDesc { name: "str_parse_int", arity: 1, category: BuiltinCategory::String, func: builtin_str_parse_int });
    reg.register(BuiltinDesc { name: "str_parse_float", arity: 1, category: BuiltinCategory::String, func: builtin_str_parse_float });

    // ── Math ────────────────────────────────────────────
    reg.register(BuiltinDesc { name: "abs", arity: 1, category: BuiltinCategory::Math, func: builtin_abs });

    // ── List ────────────────────────────────────────────
    reg.register(BuiltinDesc { name: "len", arity: 1, category: BuiltinCategory::List, func: builtin_len });
    reg.register(BuiltinDesc { name: "push", arity: 2, category: BuiltinCategory::List, func: builtin_push });
    reg.register(BuiltinDesc { name: "pop", arity: 1, category: BuiltinCategory::List, func: builtin_pop });
    reg.register(BuiltinDesc { name: "range", arity: 2, category: BuiltinCategory::List, func: builtin_range });
    reg.register(BuiltinDesc { name: "sort_list", arity: 1, category: BuiltinCategory::List, func: builtin_sort_list });
    reg.register(BuiltinDesc { name: "find", arity: 2, category: BuiltinCategory::List, func: builtin_find });
    reg.register(BuiltinDesc { name: "is_empty", arity: 1, category: BuiltinCategory::List, func: builtin_is_empty });

    // ── Higher-order ────────────────────────────────────
    reg.register(BuiltinDesc { name: "map_list", arity: 2, category: BuiltinCategory::HigherOrder, func: builtin_map_list });
    reg.register(BuiltinDesc { name: "filter_list", arity: 2, category: BuiltinCategory::HigherOrder, func: builtin_filter_list });
    reg.register(BuiltinDesc { name: "reduce_list", arity: 3, category: BuiltinCategory::HigherOrder, func: builtin_reduce_list });
    reg.register(BuiltinDesc { name: "any", arity: 2, category: BuiltinCategory::HigherOrder, func: builtin_any });
    reg.register(BuiltinDesc { name: "all", arity: 2, category: BuiltinCategory::HigherOrder, func: builtin_all });

    // ── System ──────────────────────────────────────────
    reg.register(BuiltinDesc { name: "exit", arity: usize::MAX, category: BuiltinCategory::System, func: builtin_exit });

    reg
}

// ═══════════════════════════════════════════════════════════
// Builtin implementations — one function per builtin (D2)
// ═══════════════════════════════════════════════════════════

// ── IO ──────────────────────────────────────────────────

fn builtin_println(vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let s = args
        .iter()
        .map(|a| a.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    vm.append_stdout(&s);
    vm.append_stdout("\n");
    println!("{}", s);
    Ok(Value::Unit)
}

fn builtin_print(vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let s = args
        .iter()
        .map(|a| a.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    vm.append_stdout(&s);
    print!("{}", s);
    Ok(Value::Unit)
}

fn builtin_print_err(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    for arg in args {
        eprint!("{}", arg);
    }
    eprintln!();
    Ok(Value::Unit)
}

fn builtin_input_line(_vm: &mut BytecodeVM<'_>, _args: &[Value]) -> Result<Value, InterpError> {
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .map_err(|e| InterpError::new(format!("input_line error: {}", e)))?;
    Ok(Value::String(input.trim_end().to_string()))
}

fn builtin_input_int(_vm: &mut BytecodeVM<'_>, _args: &[Value]) -> Result<Value, InterpError> {
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .map_err(|e| InterpError::new(format!("input_int error: {}", e)))?;
    match input.trim().parse::<i64>() {
        Ok(n) => Ok(Value::Int(n)),
        Err(_) => Ok(Value::Int(0)),
    }
}

// ── Convert ─────────────────────────────────────────────

fn builtin_to_int(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::Int(v) => Ok(Value::Int(*v)),
        Value::Float(v) => Ok(Value::Int(*v as i64)),
        Value::String(s) => s
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|e| InterpError::new(format!("to_int parse error: {}", e))),
        Value::Bool(b) => Ok(Value::Int(*b as i64)),
        _ => Err(InterpError::new("to_int cannot convert this type")),
    }
}

fn builtin_to_float(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::Float(v) => Ok(Value::Float(*v)),
        Value::Int(v) => Ok(Value::Float(*v as f64)),
        Value::String(s) => s
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|e| InterpError::new(format!("to_float parse error: {}", e))),
        _ => Err(InterpError::new("to_float cannot convert this type")),
    }
}

fn builtin_to_string(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    Ok(Value::String(args[0].to_string()))
}

// ── String ──────────────────────────────────────────────

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
                    _ => {
                        return Err(InterpError::new(
                            "str_join: list elements must be strings",
                        ))
                    }
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

// ── Math ────────────────────────────────────────────────

fn builtin_abs(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::Int(v) => {
            let abs = v.checked_abs().ok_or_else(|| {
                InterpError::new("abs: overflow (i64::MIN has no positive equivalent)")
            })?;
            Ok(Value::Int(abs))
        }
        Value::Float(v) => Ok(Value::Float(v.abs())),
        _ => Err(InterpError::new("abs expects a number")),
    }
}

// ── List ────────────────────────────────────────────────

fn builtin_len(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let len = match &args[0] {
        Value::List(l) => l.len(),
        Value::String(s) => s.len(),
        Value::Tuple(t) => t.len(),
        Value::Set(s) => s.len(),
        other => {
            return Err(InterpError::new(format!(
                "len: unsupported type {}",
                other
            )))
        }
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

// ── Higher-order ────────────────────────────────────────

fn builtin_map_list(vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let list = match &args[0] {
        Value::List(l) => l.clone(),
        _ => return Err(InterpError::new("map_list: first argument must be a list")),
    };
    let closure = &args[1];
    let mut result = Vec::with_capacity(list.len());
    for elem in list {
        let ret = vm.call_closure(closure, &[elem])?;
        result.push(ret);
    }
    Ok(Value::List(result))
}

fn builtin_filter_list(vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let list = match &args[0] {
        Value::List(l) => l.clone(),
        _ => return Err(InterpError::new("filter_list: first argument must be a list")),
    };
    let closure = &args[1];
    let mut result = Vec::new();
    for elem in list {
        let ret = vm.call_closure(closure, &[elem.clone()])?;
        if crate::interp::is_truthy(&ret) {
            result.push(elem);
        }
    }
    Ok(Value::List(result))
}

fn builtin_reduce_list(vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let list = match &args[0] {
        Value::List(l) => l.clone(),
        _ => return Err(InterpError::new("reduce_list: first argument must be a list")),
    };
    let closure = &args[1];
    let mut acc = args[2].clone();
    for elem in list {
        acc = vm.call_closure(closure, &[acc, elem])?;
    }
    Ok(acc)
}

fn builtin_any(vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let list = match &args[0] {
        Value::List(l) => l.clone(),
        _ => return Err(InterpError::new("any: first argument must be a list")),
    };
    let closure = &args[1];
    for elem in list {
        let ret = vm.call_closure(closure, &[elem])?;
        if crate::interp::is_truthy(&ret) {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

fn builtin_all(vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let list = match &args[0] {
        Value::List(l) => l.clone(),
        _ => return Err(InterpError::new("all: first argument must be a list")),
    };
    let closure = &args[1];
    for elem in list {
        let ret = vm.call_closure(closure, &[elem])?;
        if !crate::interp::is_truthy(&ret) {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(true))
}

// ── System ──────────────────────────────────────────────

fn builtin_exit(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let code = if args.is_empty() {
        0
    } else {
        match &args[0] {
            Value::Int(n) => *n as i32,
            _ => 1,
        }
    };
    std::process::exit(code);
}
