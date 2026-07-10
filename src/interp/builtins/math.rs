use super::*;

impl<'a> Interpreter<'a> {
    // === Arithmetic ===
    pub(crate) fn builtin_sqrt(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("sqrt expects 1 argument"));
        }
        match &args[0] {
            Value::Int(v) => Ok(Value::Float((*v as f64).sqrt())),
            Value::Float(v) => Ok(Value::Float(v.sqrt())),
            _ => Err(InterpError::new("sqrt expects a number")),
        }
    }

    pub(crate) fn builtin_abs(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("abs expects 1 argument"));
        }
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

    pub(crate) fn builtin_pow(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("pow expects 2 arguments (base, exp)"));
        }
        match (&args[0], &args[1]) {
            (Value::Int(b), Value::Int(e)) => match b.checked_pow(*e as u32) {
                Some(v) => Ok(Value::Int(v)),
                None => Err(InterpError::new(format!(
                    "integer overflow in pow({}, {})",
                    b, e
                ))),
            },
            (Value::Float(b), Value::Int(e)) => Ok(Value::Float(b.powf(*e as f64))),
            (Value::Float(b), Value::Float(e)) => Ok(Value::Float(b.powf(*e))),
            _ => Err(InterpError::new("pow expects numbers")),
        }
    }

    pub(crate) fn builtin_floor(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("floor expects 1 argument"));
        }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(v.floor())),
            Value::Int(v) => Ok(Value::Int(*v)),
            _ => Err(InterpError::new("floor expects a number")),
        }
    }

    pub(crate) fn builtin_ceil(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("ceil expects 1 argument"));
        }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(v.ceil())),
            Value::Int(v) => Ok(Value::Int(*v)),
            _ => Err(InterpError::new("ceil expects a number")),
        }
    }

    pub(crate) fn builtin_round(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("round expects 1 argument"));
        }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(v.round())),
            Value::Int(v) => Ok(Value::Int(*v)),
            _ => Err(InterpError::new("round expects a number")),
        }
    }

    pub(crate) fn builtin_min(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("min expects 2 arguments"));
        }
        match (&args[0], &args[1]) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(*a.min(b))),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a.min(*b))),
            _ => Err(InterpError::new("min expects two numbers of the same type")),
        }
    }

    pub(crate) fn builtin_max(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("max expects 2 arguments"));
        }
        match (&args[0], &args[1]) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(*a.max(b))),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a.max(*b))),
            _ => Err(InterpError::new("max expects two numbers of the same type")),
        }
    }

    pub(crate) fn builtin_random(&self, _args: Vec<Value>) -> Result<Value, InterpError> {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        let s = RandomState::new();
        let mut hasher = s.build_hasher();
        hasher.write_u64(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64,
        );
        let bits = hasher.finish();
        Ok(Value::Float((bits as f64) / (u64::MAX as f64)))
    }

    pub(crate) fn builtin_pi(&self, _args: Vec<Value>) -> Result<Value, InterpError> {
        Ok(Value::Float(std::f64::consts::PI))
    }

    // === v0.28.13 trigonometric and exponential ===

    pub(crate) fn builtin_sin(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("sin expects 1 argument"));
        }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(v.sin())),
            Value::Int(v) => Ok(Value::Float((*v as f64).sin())),
            _ => Err(InterpError::new("sin expects a number")),
        }
    }

    pub(crate) fn builtin_cos(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("cos expects 1 argument"));
        }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(v.cos())),
            Value::Int(v) => Ok(Value::Float((*v as f64).cos())),
            _ => Err(InterpError::new("cos expects a number")),
        }
    }

    pub(crate) fn builtin_tan(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("tan expects 1 argument"));
        }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(v.tan())),
            Value::Int(v) => Ok(Value::Float((*v as f64).tan())),
            _ => Err(InterpError::new("tan expects a number")),
        }
    }

    pub(crate) fn builtin_asin(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("asin expects 1 argument"));
        }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(v.asin())),
            Value::Int(v) => Ok(Value::Float((*v as f64).asin())),
            _ => Err(InterpError::new("asin expects a number")),
        }
    }

    pub(crate) fn builtin_acos(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("acos expects 1 argument"));
        }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(v.acos())),
            Value::Int(v) => Ok(Value::Float((*v as f64).acos())),
            _ => Err(InterpError::new("acos expects a number")),
        }
    }

    pub(crate) fn builtin_atan(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("atan expects 1 argument"));
        }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(v.atan())),
            Value::Int(v) => Ok(Value::Float((*v as f64).atan())),
            _ => Err(InterpError::new("atan expects a number")),
        }
    }

    pub(crate) fn builtin_atan2(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("atan2 expects 2 arguments"));
        }
        match (&args[0], &args[1]) {
            (Value::Float(y), Value::Float(x)) => Ok(Value::Float(y.atan2(*x))),
            (Value::Int(y), Value::Int(x)) => Ok(Value::Float((*y as f64).atan2(*x as f64))),
            _ => Err(InterpError::new("atan2 expects two numbers")),
        }
    }

    pub(crate) fn builtin_sinh(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("sinh expects 1 argument"));
        }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(v.sinh())),
            Value::Int(v) => Ok(Value::Float((*v as f64).sinh())),
            _ => Err(InterpError::new("sinh expects a number")),
        }
    }

    pub(crate) fn builtin_cosh(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("cosh expects 1 argument"));
        }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(v.cosh())),
            Value::Int(v) => Ok(Value::Float((*v as f64).cosh())),
            _ => Err(InterpError::new("cosh expects a number")),
        }
    }

    pub(crate) fn builtin_tanh(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("tanh expects 1 argument"));
        }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(v.tanh())),
            Value::Int(v) => Ok(Value::Float((*v as f64).tanh())),
            _ => Err(InterpError::new("tanh expects a number")),
        }
    }

    pub(crate) fn builtin_ln(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("ln expects 1 argument"));
        }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(v.ln())),
            Value::Int(v) => Ok(Value::Float((*v as f64).ln())),
            _ => Err(InterpError::new("ln expects a number")),
        }
    }

    pub(crate) fn builtin_log2(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("log2 expects 1 argument"));
        }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(v.log2())),
            Value::Int(v) => Ok(Value::Float((*v as f64).log2())),
            _ => Err(InterpError::new("log2 expects a number")),
        }
    }

    pub(crate) fn builtin_log10(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("log10 expects 1 argument"));
        }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(v.log10())),
            Value::Int(v) => Ok(Value::Float((*v as f64).log10())),
            _ => Err(InterpError::new("log10 expects a number")),
        }
    }

    pub(crate) fn builtin_log(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        // log(x) = natural log; log(x, base) = base-N logarithm
        if args.is_empty() || args.len() > 2 {
            return Err(InterpError::new("log expects 1 or 2 arguments"));
        }
        let x = match &args[0] {
            Value::Float(v) => *v,
            Value::Int(v) => *v as f64,
            _ => return Err(InterpError::new("log expects a number")),
        };
        if args.len() == 1 {
            Ok(Value::Float(x.ln()))
        } else {
            let base = match &args[1] {
                Value::Float(v) => *v,
                Value::Int(v) => *v as f64,
                _ => return Err(InterpError::new("log base must be a number")),
            };
            Ok(Value::Float(x.ln() / base.ln()))
        }
    }

    pub(crate) fn builtin_exp(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("exp expects 1 argument"));
        }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(v.exp())),
            Value::Int(v) => Ok(Value::Float((*v as f64).exp())),
            _ => Err(InterpError::new("exp expects a number")),
        }
    }

    pub(crate) fn builtin_exp2(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("exp2 expects 1 argument"));
        }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(v.exp2())),
            Value::Int(v) => Ok(Value::Float((*v as f64).exp2())),
            _ => Err(InterpError::new("exp2 expects a number")),
        }
    }

    pub(crate) fn builtin_cbrt(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("cbrt expects 1 argument"));
        }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(v.cbrt())),
            Value::Int(v) => Ok(Value::Float((*v as f64).cbrt())),
            _ => Err(InterpError::new("cbrt expects a number")),
        }
    }
}
