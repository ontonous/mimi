//! Math builtins: abs.

use crate::interp::bytecode::registry::{BuiltinCategory, BuiltinDesc, BuiltinRegistry};
use crate::interp::bytecode::vm::BytecodeVM;
use crate::interp::error::InterpError;
use crate::interp::value::Value;

pub fn register(reg: &mut BuiltinRegistry) {
    reg.register(BuiltinDesc { name: "abs", arity: 1, category: BuiltinCategory::Math, func: builtin_abs });
}

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
