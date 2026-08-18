//! IO builtins: println, print, print_err, input_line, input_int.

use crate::interp::bytecode::registry::{BuiltinCategory, BuiltinDesc, BuiltinRegistry};
use crate::interp::bytecode::vm::BytecodeVM;
use crate::interp::error::InterpError;
use crate::interp::value::Value;
use std::sync::Arc;

/// Display for print/println: auto-deref Shared/LocalShared so dual-backend
/// matches codegen (which loads the payload, not the wrapper tag).
pub(crate) fn print_display(v: &Value) -> String {
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
        name: "try_input_line",
        arity: 0,
        category: BuiltinCategory::Io,
        func: builtin_try_input_line,
    });
    reg.register(BuiltinDesc {
        name: "input_int",
        arity: 0,
        category: BuiltinCategory::Io,
        func: builtin_input_int,
    });
}

fn builtin_println(vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let s = args.iter().map(print_display).collect::<Vec<_>>().join(" ");
    vm.append_stdout(&s);
    vm.append_stdout("\n");
    flush_c_stdio();
    println!("{}", s);
    Ok(Value::Unit)
}

fn builtin_print(vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let s = args.iter().map(print_display).collect::<Vec<_>>().join(" ");
    vm.append_stdout(&s);
    flush_c_stdio();
    print!("{}", s);
    Ok(Value::Unit)
}

fn builtin_print_err(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    // B-7 (audit 2026-08-05): auto-deref Shared/LocalShared for dual-backend
    // parity with print/println (codegen loads the payload, not the wrapper).
    let s = args.iter().map(print_display).collect::<Vec<_>>().join(" ");
    flush_c_stdio();
    eprintln!("{}", s);
    Ok(Value::Unit)
}

/// 0.35.14 (DX backlog #16): C stdio and Rust stdout are SEPARATE buffers
/// over the same fd. Under the VM, mimi prints go through Rust's stdout
/// (flushed per line) while FFI callees' puts/printf sit in libc's
/// block-buffered stdout — the C output then surfaces only at process
/// exit, landing AFTER every mimi line (M-007 stream reordering). Flush
/// the C buffers before every Rust-side write so program order holds at
/// each interleaving point. A flush of an empty C buffer is a cheap no-op
/// for the common FFI-free case.
fn flush_c_stdio() {
    // SAFETY: libc::stdout/stderr are process-lifetime FILE* globals;
    // fflush(nullptr) drains every open output stream — the stronger
    // form guards against C callees writing to streams we cannot name.
    unsafe {
        libc::fflush(std::ptr::null_mut());
    }
}

fn builtin_input_line(_vm: &mut BytecodeVM, _args: &[Value]) -> Result<Value, InterpError> {
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .map_err(|e| InterpError::new(format!("input_line error: {}", e)))?;
    Ok(Value::String(Arc::new(input.trim_end().to_string())))
}

fn builtin_try_input_line(_vm: &mut BytecodeVM, _args: &[Value]) -> Result<Value, InterpError> {
    let mut input = String::new();
    match std::io::stdin().read_line(&mut input) {
        Ok(0) => Ok(Value::Variant(
            "Err".into(),
            vec![Value::String(Arc::new(
                "input: EOF or read error".to_string(),
            ))],
        )),
        Ok(_) => Ok(Value::Variant(
            "Ok".into(),
            vec![Value::String(Arc::new(input.trim_end().to_string()))],
        )),
        Err(e) => Ok(Value::Variant(
            "Err".into(),
            vec![Value::String(Arc::new(format!("input: {}", e)))],
        )),
    }
}

fn builtin_input_int(_vm: &mut BytecodeVM, _args: &[Value]) -> Result<Value, InterpError> {
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .map_err(|e| InterpError::new(format!("input_int error: {}", e)))?;
    match input.trim().parse::<i64>() {
        Ok(n) => Ok(Value::Int(n)),
        Err(_) => Ok(Value::Int(0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// B-7 (audit 2026-08-05): the print family (print/println/print_err and
    /// the misc-registered `eprintln`) must display the Shared PAYLOAD, not
    /// the wrapper tag — codegen loads the payload before formatting, so
    /// `shared(42)` on the VM vs `42` natively was an L1 divergence.
    #[test]
    fn print_display_auto_derefs_shared() {
        let shared = Value::Shared(Arc::new(std::sync::RwLock::new(Value::Int(42))));
        assert_eq!(print_display(&shared), "42");
        // Nested shared chains deref recursively.
        let nested = Value::Shared(Arc::new(std::sync::RwLock::new(shared.clone())));
        assert_eq!(print_display(&nested), "42");
        // Non-shared values keep their normal display.
        assert_eq!(print_display(&Value::Int(7)), "7");
    }
}
