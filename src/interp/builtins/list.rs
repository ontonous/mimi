use super::*;

impl<'a> Interpreter<'a> {
    // === List operations ===
    pub(crate) fn builtin_range(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("range expects 2 arguments"));
        }
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

    pub(crate) fn builtin_len(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("len expects 1 argument"));
        }
        match &args[0] {
            Value::String(s) => Ok(Value::Int(s.chars().count() as i64)),
            Value::List(l) => Ok(Value::Int(l.len() as i64)),
            Value::Array(a) => Ok(Value::Int(a.len() as i64)),
            Value::Slice { start, end, .. } => Ok(Value::Int((end - start) as i64)),
            other => Err(InterpError::new(format!("len: argument must be a string, list, array, or slice, found {}", super::value::type_name(other)))),
        }
    }

    pub(crate) fn builtin_push(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("push expects 2 arguments (list, elem)"));
        }
        match &args[0] {
            Value::List(l) => {
                let mut new_list = l.clone();
                new_list.push(args[1].clone());
                Ok(Value::List(new_list))
            }
            other => Err(InterpError::new(format!("push: first argument must be a list, found {}", super::value::type_name(other)))),
        }
    }

    pub(crate) fn builtin_pop(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("pop expects 1 argument (list)"));
        }
        match &args[0] {
            Value::List(l) => {
                if l.is_empty() {
                    return Err(InterpError::new("pop from empty list"));
                }
                let mut new_list = l.clone();
                let popped = new_list.pop().ok_or_else(|| InterpError::new("pop from empty list"))?;
                Ok(Value::Tuple(vec![popped, Value::List(new_list)]))
            }
            other => Err(InterpError::new(format!("pop: argument must be a list, found {}", super::value::type_name(other)))),
        }
    }

    pub(crate) fn builtin_contains(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("contains expects 2 arguments (container, elem)"));
        }
        match &args[0] {
            Value::List(l) => {
                let found = l.iter().any(|v| values_equal(v, &args[1]));
                Ok(Value::Bool(found))
            }
            Value::String(s) => {
                match &args[1] {
                    Value::String(sub) => Ok(Value::Bool(s.contains(sub.as_str()))),
                    other => Err(InterpError::new(format!("contains on string expects a string needle, found {}", super::value::type_name(other)))),
                }
            }
            other => Err(InterpError::new(format!("contains: first argument must be a list or string, found {}", super::value::type_name(other)))),
        }
    }

    pub(crate) fn builtin_sort(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("sort expects 1 argument (list)"));
        }
        match &args[0] {
            Value::List(l) => {
                let mut sorted = l.clone();
                sorted.sort_by(|a, b| {
                    match (a, b) {
                        (Value::Int(x), Value::Int(y)) => x.cmp(y),
                        (Value::Float(x), Value::Float(y)) => {
                            x.partial_cmp(y).unwrap_or_else(|| {
                                if x.is_nan() && !y.is_nan() { std::cmp::Ordering::Greater }
                                else if !x.is_nan() && y.is_nan() { std::cmp::Ordering::Less }
                                else { std::cmp::Ordering::Equal }
                            })
                        },
                        (Value::String(x), Value::String(y)) => x.cmp(y),
                        _ => std::cmp::Ordering::Equal,
                    }
                });
                Ok(Value::List(sorted))
            }
            _ => Err(InterpError::new("sort expects a list")),
        }
    }

    pub(crate) fn builtin_sort_f64(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        self.builtin_sort(args)
    }

    pub(crate) fn builtin_sort_str(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        self.builtin_sort(args)
    }

    pub(crate) fn builtin_reverse(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("reverse expects 1 argument (list)"));
        }
        match &args[0] {
            Value::List(l) => {
                let mut reversed = l.clone();
                reversed.reverse();
                Ok(Value::List(reversed))
            }
            _ => Err(InterpError::new("reverse expects a list")),
        }
    }

    pub(crate) fn builtin_flatten(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("flatten expects 1 argument (list of lists)"));
        }
        match &args[0] {
            Value::List(l) => {
                let mut result = Vec::new();
                for item in l {
                    match item {
                        Value::List(inner) => result.extend(inner.clone()),
                        _ => result.push(item.clone()),
                    }
                }
                Ok(Value::List(result))
            }
            _ => Err(InterpError::new("flatten expects a list")),
        }
    }

    pub(crate) fn builtin_zip(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("zip expects 2 arguments (list, list)"));
        }
        match (&args[0], &args[1]) {
            (Value::List(a), Value::List(b)) => {
                let len = a.len().min(b.len());
                let result: Vec<Value> = (0..len)
                    .map(|i| Value::Tuple(vec![a[i].clone(), b[i].clone()]))
                    .collect();
                Ok(Value::List(result))
            }
            _ => Err(InterpError::new("zip expects two lists")),
        }
    }

    pub(crate) fn builtin_enumerate(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("enumerate expects 1 argument (list)"));
        }
        match &args[0] {
            Value::List(l) => {
                let result: Vec<Value> = l.iter()
                    .enumerate()
                    .map(|(i, v)| Value::Tuple(vec![Value::Int(i as i64), v.clone()]))
                    .collect();
                Ok(Value::List(result))
            }
            _ => Err(InterpError::new("enumerate expects a list")),
        }
    }

    pub(crate) fn builtin_sum(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("sum expects 1 argument (list)"));
        }
        match &args[0] {
            Value::List(l) => {
                let mut total_int = 0i64;
                let mut total_float = 0f64;
                let mut is_float = false;
                for v in l {
                    match v {
                        Value::Int(n) => total_int += n,
                        Value::Float(n) => { total_float += n; is_float = true; }
                        _ => return Err(InterpError::new("sum expects a list of numbers")),
                    }
                }
                if is_float {
                    Ok(Value::Float(total_float + total_int as f64))
                } else {
                    Ok(Value::Int(total_int))
                }
            }
            _ => Err(InterpError::new("sum expects a list")),
        }
    }
}
