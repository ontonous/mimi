//! System builtins: exit.

use crate::interp::bytecode::registry::{BuiltinCategory, BuiltinDesc, BuiltinRegistry};
use crate::interp::bytecode::vm::BytecodeVM;
use crate::interp::error::InterpError;
use crate::interp::value::Value;

pub fn register(reg: &mut BuiltinRegistry) {
    reg.register(BuiltinDesc { name: "exit", arity: usize::MAX, category: BuiltinCategory::System, func: builtin_exit });
}

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
