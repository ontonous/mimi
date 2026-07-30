//! Math builtins: sqrt, sin, cos, tan, exp, log, pow, floor, ceil, round, etc.

use crate::interp::bytecode::registry::{BuiltinCategory, BuiltinDesc, BuiltinRegistry};
use crate::interp::bytecode::vm::BytecodeVM;
use crate::interp::error::InterpError;
use crate::interp::value::Value;

pub fn register(reg: &mut BuiltinRegistry) {
    // Abs
    reg.register(BuiltinDesc { name: "abs", arity: 1, category: BuiltinCategory::Math, func: builtin_abs });
    // Trig
    reg.register(BuiltinDesc { name: "sin", arity: 1, category: BuiltinCategory::Math, func: builtin_sin });
    reg.register(BuiltinDesc { name: "cos", arity: 1, category: BuiltinCategory::Math, func: builtin_cos });
    reg.register(BuiltinDesc { name: "tan", arity: 1, category: BuiltinCategory::Math, func: builtin_tan });
    reg.register(BuiltinDesc { name: "asin", arity: 1, category: BuiltinCategory::Math, func: builtin_asin });
    reg.register(BuiltinDesc { name: "acos", arity: 1, category: BuiltinCategory::Math, func: builtin_acos });
    reg.register(BuiltinDesc { name: "atan", arity: 1, category: BuiltinCategory::Math, func: builtin_atan });
    reg.register(BuiltinDesc { name: "atan2", arity: 2, category: BuiltinCategory::Math, func: builtin_atan2 });
    reg.register(BuiltinDesc { name: "sinh", arity: 1, category: BuiltinCategory::Math, func: builtin_sinh });
    reg.register(BuiltinDesc { name: "cosh", arity: 1, category: BuiltinCategory::Math, func: builtin_cosh });
    reg.register(BuiltinDesc { name: "tanh", arity: 1, category: BuiltinCategory::Math, func: builtin_tanh });
    // Exp / log
    reg.register(BuiltinDesc { name: "exp", arity: 1, category: BuiltinCategory::Math, func: builtin_exp });
    reg.register(BuiltinDesc { name: "exp2", arity: 1, category: BuiltinCategory::Math, func: builtin_exp2 });
    reg.register(BuiltinDesc { name: "ln", arity: 1, category: BuiltinCategory::Math, func: builtin_ln });
    reg.register(BuiltinDesc { name: "log", arity: usize::MAX, category: BuiltinCategory::Math, func: builtin_log });
    reg.register(BuiltinDesc { name: "log2", arity: 1, category: BuiltinCategory::Math, func: builtin_log2 });
    reg.register(BuiltinDesc { name: "log10", arity: 1, category: BuiltinCategory::Math, func: builtin_log10 });
    // Power / root
    reg.register(BuiltinDesc { name: "sqrt", arity: 1, category: BuiltinCategory::Math, func: builtin_sqrt });
    reg.register(BuiltinDesc { name: "cbrt", arity: 1, category: BuiltinCategory::Math, func: builtin_cbrt });
    reg.register(BuiltinDesc { name: "pow", arity: 2, category: BuiltinCategory::Math, func: builtin_pow });
    // Rounding
    reg.register(BuiltinDesc { name: "floor", arity: 1, category: BuiltinCategory::Math, func: builtin_floor });
    reg.register(BuiltinDesc { name: "ceil", arity: 1, category: BuiltinCategory::Math, func: builtin_ceil });
    reg.register(BuiltinDesc { name: "round", arity: 1, category: BuiltinCategory::Math, func: builtin_round });
    // Min / max
    reg.register(BuiltinDesc { name: "min", arity: 2, category: BuiltinCategory::Math, func: builtin_min });
    reg.register(BuiltinDesc { name: "max", arity: 2, category: BuiltinCategory::Math, func: builtin_max });
    // Constants
    reg.register(BuiltinDesc { name: "pi", arity: 0, category: BuiltinCategory::Math, func: builtin_pi });
    // Random
    reg.register(BuiltinDesc { name: "random", arity: 0, category: BuiltinCategory::Math, func: builtin_random });
    // Float classification
    reg.register(BuiltinDesc { name: "is_nan", arity: 1, category: BuiltinCategory::Math, func: builtin_is_nan });
    reg.register(BuiltinDesc { name: "is_finite", arity: 1, category: BuiltinCategory::Math, func: builtin_is_finite });
    reg.register(BuiltinDesc { name: "is_infinite", arity: 1, category: BuiltinCategory::Math, func: builtin_is_infinite });
    reg.register(BuiltinDesc { name: "is_close", arity: 3, category: BuiltinCategory::Math, func: builtin_is_close });
    reg.register(BuiltinDesc { name: "f64_eq_exact", arity: 2, category: BuiltinCategory::Math, func: builtin_f64_eq_exact });
    // Wrapping arithmetic
    reg.register(BuiltinDesc { name: "wrapping_add", arity: 2, category: BuiltinCategory::Math, func: builtin_wrapping_add });
    reg.register(BuiltinDesc { name: "wrapping_sub", arity: 2, category: BuiltinCategory::Math, func: builtin_wrapping_sub });
    reg.register(BuiltinDesc { name: "wrapping_mul", arity: 2, category: BuiltinCategory::Math, func: builtin_wrapping_mul });
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

// Helper: extract f64 from Int or Float.
fn to_f64(v: &Value) -> Result<f64, InterpError> {
    match v {
        Value::Float(f) => Ok(*f),
        Value::Int(i) => Ok(*i as f64),
        _ => Err(InterpError::new("expected a number")),
    }
}

fn to_i64(v: &Value) -> Result<i64, InterpError> {
    match v {
        Value::Int(i) => Ok(*i),
        Value::Float(f) => Ok(*f as i64),
        _ => Err(InterpError::new("expected a number")),
    }
}

// Macro for unary float builtins with finiteness check (SD-9).
macro_rules! unary_float {
    ($name:ident, $method:ident) => {
        fn $name(vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
            let x = to_f64(&args[0])?;
            let r = x.$method();
            vm.check_float(r, stringify!($method))?;
            Ok(Value::Float(r))
        }
    };
}

unary_float!(builtin_sin, sin);
unary_float!(builtin_cos, cos);
unary_float!(builtin_tan, tan);
unary_float!(builtin_asin, asin);
unary_float!(builtin_acos, acos);
unary_float!(builtin_atan, atan);
unary_float!(builtin_sinh, sinh);
unary_float!(builtin_cosh, cosh);
unary_float!(builtin_tanh, tanh);
unary_float!(builtin_exp, exp);
unary_float!(builtin_exp2, exp2);
unary_float!(builtin_ln, ln);
unary_float!(builtin_log2, log2);
unary_float!(builtin_log10, log10);

fn builtin_log(vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    // log(x) or log(x, base)
    if args.len() < 1 || args.len() > 2 {
        return Err(InterpError::new("log expects 1 or 2 arguments (x) or (x, base)"));
    }
    let x = to_f64(&args[0])?;
    let r = if args.len() == 2 {
        let base = to_f64(&args[1])?;
        if base <= 0.0 || base == 1.0 {
            return Err(InterpError::new("log: base must be positive and not 1"));
        }
        x.ln() / base.ln()
    } else {
        x.ln()
    };
    vm.check_float(r, "log")?;
    Ok(Value::Float(r))
}
unary_float!(builtin_sqrt, sqrt);
unary_float!(builtin_cbrt, cbrt);
fn builtin_floor(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::Int(v) => Ok(Value::Int(*v)),
        other => {
            let x = to_f64(other)?;
            Ok(Value::Float(x.floor()))
        }
    }
}

fn builtin_ceil(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::Int(v) => Ok(Value::Int(*v)),
        other => {
            let x = to_f64(other)?;
            Ok(Value::Float(x.ceil()))
        }
    }
}

fn builtin_round(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match &args[0] {
        Value::Int(v) => Ok(Value::Int(*v)),
        other => {
            let x = to_f64(other)?;
            Ok(Value::Float(x.round()))
        }
    }
}

fn builtin_atan2(vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let y = to_f64(&args[0])?;
    let x = to_f64(&args[1])?;
    let r = y.atan2(x);
    vm.check_float(r, "atan2")?;
    Ok(Value::Float(r))
}

fn builtin_pow(vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::Int(base), Value::Int(exp)) => {
            if *exp < 0 {
                return Err(InterpError::new("pow: negative exponent not allowed for integers"));
            }
            let r = base.checked_pow(*exp as u32).ok_or_else(|| {
                InterpError::new("pow: integer overflow")
            })?;
            Ok(Value::Int(r))
        }
        _ => {
            let base = to_f64(&args[0])?;
            let exp = to_f64(&args[1])?;
            let r = base.powf(exp);
            vm.check_float(r, "pow")?;
            Ok(Value::Float(r))
        }
    }
}

fn builtin_min(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int((*a).min(*b))),
        (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a.min(*b))),
        _ => Err(InterpError::new("min: arguments must have the same type (both int or both float)")),
    }
}

fn builtin_max(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    match (&args[0], &args[1]) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int((*a).max(*b))),
        (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a.max(*b))),
        _ => Err(InterpError::new("max: arguments must have the same type (both int or both float)")),
    }
}

fn builtin_pi(_vm: &mut BytecodeVM<'_>, _args: &[Value]) -> Result<Value, InterpError> {
    Ok(Value::Float(std::f64::consts::PI))
}

fn builtin_random(_vm: &mut BytecodeVM<'_>, _args: &[Value]) -> Result<Value, InterpError> {
    // Simple LCG-based pseudo-random (no external crate).
    use std::time::SystemTime;
    let seed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    let val = (seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407)) >> 33;
    Ok(Value::Float((val as f64) / (u32::MAX as f64)))
}

fn builtin_is_nan(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let x = to_f64(&args[0])?;
    Ok(Value::Bool(x.is_nan()))
}

fn builtin_is_finite(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let x = to_f64(&args[0])?;
    Ok(Value::Bool(x.is_finite()))
}

fn builtin_is_infinite(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let x = to_f64(&args[0])?;
    Ok(Value::Bool(x.is_infinite()))
}

fn builtin_is_close(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let a = to_f64(&args[0])?;
    let b = to_f64(&args[1])?;
    let epsilon = to_f64(&args[2])?;
    if epsilon < 0.0 {
        return Err(InterpError::new("is_close: epsilon must be non-negative"));
    }
    Ok(Value::Bool((a - b).abs() <= epsilon))
}

fn builtin_f64_eq_exact(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let a = to_f64(&args[0])?;
    let b = to_f64(&args[1])?;
    Ok(Value::Bool(a == b))
}

fn builtin_wrapping_add(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let (a, b) = match (&args[0], &args[1]) {
        (Value::Int(a), Value::Int(b)) => (a, b),
        _ => return Err(InterpError::new("wrapping_add expects two integers")),
    };
    Ok(Value::Int(a.wrapping_add(*b)))
}

fn builtin_wrapping_sub(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let (a, b) = match (&args[0], &args[1]) {
        (Value::Int(a), Value::Int(b)) => (a, b),
        _ => return Err(InterpError::new("wrapping_sub expects two integers")),
    };
    Ok(Value::Int(a.wrapping_sub(*b)))
}

fn builtin_wrapping_mul(_vm: &mut BytecodeVM<'_>, args: &[Value]) -> Result<Value, InterpError> {
    let (a, b) = match (&args[0], &args[1]) {
        (Value::Int(a), Value::Int(b)) => (a, b),
        _ => return Err(InterpError::new("wrapping_mul expects two integers")),
    };
    Ok(Value::Int(a.wrapping_mul(*b)))
}
