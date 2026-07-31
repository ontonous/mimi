//! IO builtins: println, print, print_err, input_line, input_int.

use crate::interp::bytecode::registry::{BuiltinCategory, BuiltinDesc, BuiltinRegistry};
use crate::interp::bytecode::vm::BytecodeVM;
use crate::interp::error::InterpError;
use crate::interp::value::Value;

/// Display for print/println: auto-deref Shared/LocalShared so dual-backend
/// matches codegen (which loads the payload, not the wrapper tag).
fn print_display(v: &Value) -> String {
    match v {
        Value::Shared(arc) => match arc.read() {
            Ok(inner) => print_display(&inner),
            Err(_) => "shared(<poisoned>)".to_string(),
        },
        Value::LocalShared(rc) => {
            let inner = rc.lock().unwrap_or_else(|e| e.into_inner());
            print_display(&inner)
        }
        other => other.to_string(),
    }
}

pub fn register(reg: &mut BuiltinRegistry) {
    reg.register(BuiltinDesc {
        name: "println",
        arity: usize::MAX,
        category: BuiltinCategory::Io,
        func: builtin_println,
    });
    reg.register(BuiltinDesc {
        name: "print",
        arity: usize::MAX,
        category: BuiltinCategory::Io,
        func: builtin_print,
    });
    reg.register(BuiltinDesc {
        name: "print_err",
        arity: usize::MAX,
        category: BuiltinCategory::Io,
        func: builtin_print_err,
    });
    reg.register(BuiltinDesc {
        name: "input_line",
        arity: 0,
        category: BuiltinCategory::Io,
        func: builtin_input_line,
    });
    reg.register(BuiltinDesc {
        name: "input_int",
        arity: 0,
        category: BuiltinCategory::Io,
        func: builtin_input_int,
    });
}

fn builtin_println(vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let s = args
        .iter()
        .map(print_display)
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
        .map(print_display)
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
