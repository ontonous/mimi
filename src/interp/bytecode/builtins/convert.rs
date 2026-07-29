//! Convert builtins: to_int, to_float, to_string, str, int, float.

use crate::interp::bytecode::registry::{BuiltinCategory, BuiltinDesc, BuiltinRegistry};
use crate::interp::bytecode::vm::BytecodeVM;
use crate::interp::error::InterpError;
use crate::interp::value::Value;

pub fn register(reg: &mut BuiltinRegistry) {
    reg.register(BuiltinDesc { name: "to_int", arity: 1, category: BuiltinCategory::Convert, func: builtin_to_int });
    reg.register(BuiltinDesc { name: "to_float", arity: 1, category: BuiltinCategory::Convert, func: builtin_to_float });
    reg.register(BuiltinDesc { name: "to_string", arity: 1, category: BuiltinCategory::Convert, func: builtin_to_string });
    reg.register(BuiltinDesc { name: "str", arity: 1, category: BuiltinCategory::Convert, func: builtin_to_string });
    reg.register(BuiltinDesc { name: "int", arity: 1, category: BuiltinCategory::Convert, func: builtin_to_int });
    reg.register(BuiltinDesc { name: "float", arity: 1, category: BuiltinCategory::Convert, func: builtin_to_float });
}

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
