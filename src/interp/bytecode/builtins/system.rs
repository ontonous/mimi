//! System builtins: exit.

use crate::interp::bytecode::registry::{BuiltinCategory, BuiltinDesc, BuiltinRegistry};
use crate::interp::bytecode::vm::BytecodeVM;
use crate::interp::error::InterpError;
use crate::interp::value::Value;

pub fn register(reg: &mut BuiltinRegistry) {
    reg.register(BuiltinDesc { name: "exit", arity: usize::MAX, category: BuiltinCategory::System, func: builtin_exit });
}

fn builtin_exit(vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let code = if args.is_empty() {
        0i64
    } else {
        match &args[0] {
            Value::Int(n) => *n,
            _ => 1,
        }
    };
    // Signal the VM to terminate cleanly (avoids killing the test runner).
    vm.request_exit(code);
    Ok(Value::Int(code))
}
