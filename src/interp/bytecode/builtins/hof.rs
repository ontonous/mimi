//! Higher-order builtins: map_list, filter_list, reduce_list, any, all.

use crate::interp::bytecode::registry::{BuiltinCategory, BuiltinDesc, BuiltinRegistry};
use crate::interp::bytecode::vm::BytecodeVM;
use crate::interp::error::InterpError;
use crate::interp::value::Value;

pub fn register(reg: &mut BuiltinRegistry) {
    reg.register(BuiltinDesc {
        name: "map_list",
        arity: 2,
        category: BuiltinCategory::HigherOrder,
        func: builtin_map_list,
    });
    reg.register(BuiltinDesc {
        name: "filter_list",
        arity: 2,
        category: BuiltinCategory::HigherOrder,
        func: builtin_filter_list,
    });
    reg.register(BuiltinDesc {
        name: "reduce_list",
        arity: 3,
        category: BuiltinCategory::HigherOrder,
        func: builtin_reduce_list,
    });
    reg.register(BuiltinDesc {
        name: "any",
        arity: 2,
        category: BuiltinCategory::HigherOrder,
        func: builtin_any,
    });
    reg.register(BuiltinDesc {
        name: "all",
        arity: 2,
        category: BuiltinCategory::HigherOrder,
        func: builtin_all,
    });
}

pub(crate) fn builtin_map_list(
    vm: &mut BytecodeVM<'_>,
    args: &[Value],
) -> Result<Value, InterpError> {
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

pub(crate) fn builtin_filter_list(
    vm: &mut BytecodeVM<'_>,
    args: &[Value],
) -> Result<Value, InterpError> {
    let list = match &args[0] {
        Value::List(l) => l.clone(),
        _ => {
            return Err(InterpError::new(
                "filter_list: first argument must be a list",
            ))
        }
    };
    let closure = &args[1];
    let mut result = Vec::new();
    for elem in list {
        let ret = vm.call_closure(closure, std::slice::from_ref(&elem))?;
        if crate::interp::is_truthy(&ret) {
            result.push(elem);
        }
    }
    Ok(Value::List(result))
}

pub(crate) fn builtin_reduce_list(
    vm: &mut BytecodeVM<'_>,
    args: &[Value],
) -> Result<Value, InterpError> {
    let list = match &args[0] {
        Value::List(l) => l.clone(),
        _ => {
            return Err(InterpError::new(
                "reduce_list: first argument must be a list",
            ))
        }
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
