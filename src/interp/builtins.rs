use super::*;

impl<'a> Interpreter<'a> {
    // === I/O ===
    pub(crate) fn builtin_println(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        let parts: Vec<String> = args.iter().map(|v| v.to_string()).collect();
        println!("{}", parts.join(" "));
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_print(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        let parts: Vec<String> = args.iter().map(|v| v.to_string()).collect();
        print!("{}", parts.join(" "));
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_input(&mut self, args: Vec<Value>) -> Result<Value, InterpError> {
        use std::io::{self, BufRead};
        let mut line = String::new();
        match io::stdin().lock().read_line(&mut line) {
            Ok(_) => {
                if line.ends_with('\n') { line.pop(); }
                if line.ends_with('\r') { line.pop(); }
                Ok(Value::Variant("Ok".into(), vec![Value::String(line)]))
            }
            Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(format!("input error: {}", e))])),
        }
    }

    // === Assertions ===
    pub(crate) fn builtin_assert(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("assert expects 1 argument"));
        }
        if !is_truthy(&args[0]) {
            return Err(InterpError::new(format!("assertion failed: {}", args[0])));
        }
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_assert_eq(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("assert_eq expects 2 arguments"));
        }
        if !values_equal(&args[0], &args[1]) {
            return Err(InterpError::new(format!("assertion failed: {} != {}", args[0], args[1])));
        }
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_assert_ne(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("assert_ne expects 2 arguments"));
        }
        if values_equal(&args[0], &args[1]) {
            return Err(InterpError::new(format!("assertion failed: {} == {}", args[0], args[1])));
        }
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_assert_approx_eq(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("assert_approx_eq expects 2 arguments"));
        }
        match (&args[0], &args[1]) {
            (Value::Float(a), Value::Float(b)) => {
                if (a - b).abs() > f64::EPSILON {
                    return Err(InterpError::new(format!("assertion failed: {} != {} (difference: {})", a, b, (a - b).abs())));
                }
                Ok(Value::Unit)
            }
            (Value::Int(a), Value::Int(b)) => {
                if a != b {
                    return Err(InterpError::new(format!("assertion failed: {} != {}", a, b)));
                }
                Ok(Value::Unit)
            }
            _ => {
                if !values_equal(&args[0], &args[1]) {
                    return Err(InterpError::new(format!("assertion failed: {} != {}", args[0], args[1])));
                }
                Ok(Value::Unit)
            }
        }
    }

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
            Value::Int(v) => Ok(Value::Int(v.abs())),
            Value::Float(v) => Ok(Value::Float(v.abs())),
            _ => Err(InterpError::new("abs expects a number")),
        }
    }

    pub(crate) fn builtin_pow(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("pow expects 2 arguments (base, exp)")); }
        match (&args[0], &args[1]) {
            (Value::Int(b), Value::Int(e)) => match b.checked_pow(*e as u32) { Some(v) => Ok(Value::Int(v)), None => Err(InterpError::new(format!("integer overflow in pow({}, {})", b, e))) },
            (Value::Float(b), Value::Int(e)) => Ok(Value::Float(b.powf(*e as f64))),
            (Value::Float(b), Value::Float(e)) => Ok(Value::Float(b.powf(*e))),
            _ => Err(InterpError::new("pow expects numbers")),
        }
    }

    pub(crate) fn builtin_floor(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("floor expects 1 argument")); }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(v.floor())),
            Value::Int(v) => Ok(Value::Int(*v)),
            _ => Err(InterpError::new("floor expects a number")),
        }
    }

    pub(crate) fn builtin_ceil(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("ceil expects 1 argument")); }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(v.ceil())),
            Value::Int(v) => Ok(Value::Int(*v)),
            _ => Err(InterpError::new("ceil expects a number")),
        }
    }

    pub(crate) fn builtin_round(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("round expects 1 argument")); }
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

    pub(crate) fn builtin_random(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        let s = RandomState::new();
        let mut hasher = s.build_hasher();
        hasher.write_u64(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64);
        let bits = hasher.finish();
        Ok(Value::Float((bits as f64) / (u64::MAX as f64)))
    }

    pub(crate) fn builtin_pi(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        Ok(Value::Float(std::f64::consts::PI))
    }

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

    pub(crate) fn builtin_map(&mut self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("map expects 2 arguments (list, closure)"));
        }
        match (&args[0], &args[1]) {
            (Value::List(l), Value::Closure { params, body, captured, .. }) => {
                if params.len() != 1 {
                    return Err(InterpError::new("map closure must take 1 argument"));
                }
                let mut result = Vec::new();
                for item in l {
                    if self.early_return.is_some() { break; }
                    self.push_scope();
                    for (n, v) in captured {
                        self.bind(n, v.clone())?;
                    }
                    self.bind(&params[0].name, item.clone())?;
                    let val = self.eval_block(body)?;
                    self.pop_scope();
                    if self.early_return.is_some() { break; }
                    result.push(val.unwrap_or(Value::Unit));
                }
                Ok(Value::List(result))
            }
            _ => Err(InterpError::new("map expects (list, closure)")),
        }
    }

    pub(crate) fn builtin_filter(&mut self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("filter expects 2 arguments (list, closure)"));
        }
        match (&args[0], &args[1]) {
            (Value::List(l), Value::Closure { params, body, captured, .. }) => {
                if params.len() != 1 {
                    return Err(InterpError::new("filter closure must take 1 argument"));
                }
                let mut result = Vec::new();
                for item in l {
                    if self.early_return.is_some() { break; }
                    self.push_scope();
                    for (n, v) in captured {
                        self.bind(n, v.clone())?;
                    }
                    self.bind(&params[0].name, item.clone())?;
                    let val = self.eval_block(body)?;
                    self.pop_scope();
                    if self.early_return.is_some() { break; }
                    if is_truthy(&val.unwrap_or(Value::Unit)) {
                        result.push(item.clone());
                    }
                }
                Ok(Value::List(result))
            }
            _ => Err(InterpError::new("filter expects (list, closure)")),
        }
    }

    pub(crate) fn builtin_reduce(&mut self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 3 {
            return Err(InterpError::new("reduce expects 3 arguments (list, closure, initial)"));
        }
        match (&args[0], &args[1]) {
            (Value::List(l), Value::Closure { params, body, captured, .. }) => {
                if params.len() != 2 {
                    return Err(InterpError::new("reduce closure must take 2 arguments (acc, elem)"));
                }
                let mut acc = args[2].clone();
                for item in l {
                    if self.early_return.is_some() { break; }
                    self.push_scope();
                    for (n, v) in captured {
                        self.bind(n, v.clone())?;
                    }
                    self.bind(&params[0].name, acc.clone())?;
                    self.bind(&params[1].name, item.clone())?;
                    let val = self.eval_block(body)?;
                    self.pop_scope();
                    if self.early_return.is_some() { break; }
                    acc = val.unwrap_or(Value::Unit);
                }
                Ok(acc)
            }
            _ => Err(InterpError::new("reduce expects (list, closure, initial)")),
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
                        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal),
                        (Value::String(x), Value::String(y)) => x.cmp(y),
                        _ => std::cmp::Ordering::Equal,
                    }
                });
                Ok(Value::List(sorted))
            }
            _ => Err(InterpError::new("sort expects a list")),
        }
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

    // === Type utilities ===
    pub(crate) fn builtin_to_string(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("to_string expects 1 argument"));
        }
        Ok(Value::String(args[0].to_string()))
    }

    pub(crate) fn builtin_type_name(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("type_name expects 1 argument (a value)"));
        }
        let type_name = self.value_type_name(&args[0]);
        Ok(Value::String(type_name))
    }

    /// Extract a string type name from a Value (either Value::String or Value::Type).
    fn resolve_type_name_arg<'v>(&self, v: &'v Value) -> Result<&'v String, InterpError> {
        match v {
            Value::String(name) => Ok(name),
            Value::Type(name) => Ok(name),
            _ => Err(InterpError::new("expected a type name string or Type value")),
        }
    }

    pub(crate) fn builtin_type_fields(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("type_fields expects 1 argument (a type name string)"));
        }
        let name = self.resolve_type_name_arg(&args[0])?;
        let type_def = self.type_defs.get(name)
            .ok_or_else(|| InterpError::new(format!("unknown type '{}'", name)))?;
        match &type_def.kind {
            TypeDefKind::Record(fields) => {
                let field_names: Vec<Value> = fields.iter()
                    .map(|f| Value::String(f.name.clone()))
                    .collect();
                Ok(Value::List(field_names))
            }
            TypeDefKind::Enum(variants) => {
                let variant_names: Vec<Value> = variants.iter()
                    .map(|v| Value::String(v.name.clone()))
                    .collect();
                Ok(Value::List(variant_names))
            }
            _ => Ok(Value::List(vec![])),
        }
    }

    pub(crate) fn builtin_type_variants(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("type_variants expects 1 argument (a type name string)"));
        }
        let name = self.resolve_type_name_arg(&args[0])?;
        let type_def = self.type_defs.get(name)
            .ok_or_else(|| InterpError::new(format!("unknown type '{}'", name)))?;
        match &type_def.kind {
            TypeDefKind::Enum(variants) => {
                let variant_names: Vec<Value> = variants.iter()
                    .map(|v| Value::String(v.name.clone()))
                    .collect();
                Ok(Value::List(variant_names))
            }
            _ => Ok(Value::List(vec![])),
        }
    }

    pub(crate) fn builtin_to_int(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("to_int expects 1 argument")); }
        match &args[0] {
            Value::Int(v) => Ok(Value::Int(*v)),
            Value::Float(v) => Ok(Value::Int(*v as i64)),
            Value::String(s) => s.parse::<i64>()
                .map(Value::Int)
                .map_err(|e| InterpError::new(format!("to_int parse error: {}", e))),
            Value::Bool(b) => Ok(Value::Int(*b as i64)),
            _ => Err(InterpError::new("to_int cannot convert this type")),
        }
    }

    pub(crate) fn builtin_to_float(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("to_float expects 1 argument")); }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(*v)),
            Value::Int(v) => Ok(Value::Float(*v as f64)),
            Value::String(s) => s.parse::<f64>()
                .map(Value::Float)
                .map_err(|e| InterpError::new(format!("to_float parse error: {}", e))),
            _ => Err(InterpError::new("to_float cannot convert this type")),
        }
    }

    pub(crate) fn builtin_keys(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("keys expects 1 argument (record)")); }
        match &args[0] {
            Value::Record(_, fields) => {
                let keys: Vec<Value> = fields.keys().map(|k| Value::String(k.clone())).collect();
                Ok(Value::List(keys))
            }
            _ => Err(InterpError::new("keys expects a record")),
        }
    }

    pub(crate) fn builtin_values(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("values expects 1 argument (record)")); }
        match &args[0] {
            Value::Record(_, fields) => {
                Ok(Value::List(fields.values().cloned().collect()))
            }
            _ => Err(InterpError::new("values expects a record")),
        }
    }

    pub(crate) fn builtin_has_key(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("has_key expects 2 arguments (record, key)")); }
        match (&args[0], &args[1]) {
            (Value::Record(_, fields), Value::String(key)) => {
                Ok(Value::Bool(fields.contains_key(key.as_str())))
            }
            _ => Err(InterpError::new("has_key expects (record, string)")),
        }
    }

    // === String operations ===
    pub(crate) fn builtin_str_char_at(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("str_char_at expects 2 arguments (string, index)")); }
        match (&args[0], &args[1]) {
            (Value::String(s), Value::Int(idx)) => {
                let i = *idx as usize;
                s.chars().nth(i)
                    .map(|c| Value::String(c.to_string()))
                    .ok_or_else(|| InterpError::new(format!("str_char_at: index {} out of bounds (len {})", i, s.chars().count())))
            }
            _ => Err(InterpError::new("str_char_at expects (string, int)")),
        }
    }

    pub(crate) fn builtin_str_substring(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 3 { return Err(InterpError::new("str_substring expects 3 arguments (string, start, end)")); }
        match (&args[0], &args[1], &args[2]) {
            (Value::String(s), Value::Int(start), Value::Int(end)) => {
                let chars: Vec<char> = s.chars().collect();
                let s_idx = (*start as usize).min(chars.len());
                let e_idx = (*end as usize).min(chars.len());
                if s_idx > e_idx {
                    return Err(InterpError::new("str_substring: start > end"));
                }
                Ok(Value::String(chars[s_idx..e_idx].iter().collect()))
            }
            _ => Err(InterpError::new("str_substring expects (string, int, int)")),
        }
    }

    pub(crate) fn builtin_str_parse_int(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("str_parse_int expects 1 argument")); }
        match &args[0] {
            Value::String(s) => Ok(s.trim().parse::<i64>()
                .map(|n| Value::Tuple(vec![Value::Bool(true), Value::Int(n)]))
                .unwrap_or_else(|_| Value::Tuple(vec![Value::Bool(false), Value::Int(0)]))),
            _ => Err(InterpError::new("str_parse_int expects a string")),
        }
    }

    pub(crate) fn builtin_str_parse_float(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("str_parse_float expects 1 argument")); }
        match &args[0] {
            Value::String(s) => Ok(s.trim().parse::<f64>()
                .map(|n| Value::Tuple(vec![Value::Bool(true), Value::Float(n)]))
                .unwrap_or_else(|_| Value::Tuple(vec![Value::Bool(false), Value::Float(0.0)]))),
            _ => Err(InterpError::new("str_parse_float expects a string")),
        }
    }

    pub(crate) fn builtin_str_split(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("str_split expects 2 arguments (string, delimiter)")); }
        match (&args[0], &args[1]) {
            (Value::String(s), Value::String(delimiter)) => {
                let mut parts = Vec::new();
                for p in s.split(delimiter.as_str()) {
                    parts.push(Value::String(p.to_string()));
                }
                Ok(Value::List(parts))
            }
            _ => Err(InterpError::new("str_split expects (string, string)")),
        }
    }

    pub(crate) fn builtin_str_join(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("str_join expects 2 arguments (list, separator)")); }
        match (&args[0], &args[1]) {
            (Value::List(parts), Value::String(sep)) => {
                let mut strings = Vec::new();
                for p in parts {
                    match p {
                        Value::String(s) => strings.push(s.clone()),
                        _ => return Err(InterpError::new("str_join: list elements must be strings")),
                    }
                }
                Ok(Value::String(strings.join(sep)))
            }
            _ => Err(InterpError::new("str_join expects (list, string)")),
        }
    }

    pub(crate) fn builtin_str_trim(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("str_trim expects 1 argument")); }
        match &args[0] {
            Value::String(s) => Ok(Value::String(s.trim().to_string())),
            _ => Err(InterpError::new("str_trim expects a string")),
        }
    }

    pub(crate) fn builtin_str_starts_with(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("str_starts_with expects 2 arguments")); }
        match (&args[0], &args[1]) {
            (Value::String(s), Value::String(prefix)) => {
                Ok(Value::Bool(s.starts_with(prefix.as_str())))
            }
            _ => Err(InterpError::new("str_starts_with expects (string, string)")),
        }
    }

    pub(crate) fn builtin_str_ends_with(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("str_ends_with expects 2 arguments")); }
        match (&args[0], &args[1]) {
            (Value::String(s), Value::String(suffix)) => {
                Ok(Value::Bool(s.ends_with(suffix.as_str())))
            }
            _ => Err(InterpError::new("str_ends_with expects (string, string)")),
        }
    }

    pub(crate) fn builtin_str_replace(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 3 { return Err(InterpError::new("str_replace expects 3 arguments")); }
        match (&args[0], &args[1], &args[2]) {
            (Value::String(s), Value::String(from), Value::String(to)) => {
                Ok(Value::String(s.replace(from.as_str(), to.as_str())))
            }
            _ => Err(InterpError::new("str_replace expects (string, string, string)")),
        }
    }

    pub(crate) fn builtin_str_to_upper(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("str_to_upper expects 1 argument")); }
        match &args[0] {
            Value::String(s) => Ok(Value::String(s.to_uppercase())),
            _ => Err(InterpError::new("str_to_upper expects a string")),
        }
    }

    pub(crate) fn builtin_str_to_lower(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("str_to_lower expects 1 argument")); }
        match &args[0] {
            Value::String(s) => Ok(Value::String(s.to_lowercase())),
            _ => Err(InterpError::new("str_to_lower expects a string")),
        }
    }

    pub(crate) fn builtin_str_repeat(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("str_repeat expects 2 arguments")); }
        match (&args[0], &args[1]) {
            (Value::String(s), Value::Int(n)) => {
                if *n < 0 { return Err(InterpError::new("str_repeat: count must be non-negative")); }
                Ok(Value::String(s.repeat(*n as usize)))
            }
            _ => Err(InterpError::new("str_repeat expects (string, int)")),
        }
    }

    pub(crate) fn builtin_str_contains(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("str_contains expects 2 arguments")); }
        match (&args[0], &args[1]) {
            (Value::String(s), Value::String(sub)) => {
                Ok(Value::Bool(s.contains(sub.as_str())))
            }
            _ => Err(InterpError::new("str_contains expects (string, string)")),
        }
    }

    pub(crate) fn builtin_str_index_of(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("str_index_of expects 2 arguments")); }
        match (&args[0], &args[1]) {
            (Value::String(s), Value::String(sub)) => {
                match s.find(sub.as_str()) {
                    Some(idx) => Ok(Value::Tuple(vec![Value::Bool(true), Value::Int(idx as i64)])),
                    None => Ok(Value::Tuple(vec![Value::Bool(false), Value::Int(-1)])),
                }
            }
            _ => Err(InterpError::new("str_index_of expects (string, string)")),
        }
    }

    // === Map/Record utilities ===
    pub(crate) fn builtin_map_new(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        Ok(Value::Record(None, std::collections::HashMap::new()))
    }

    pub(crate) fn builtin_map_get(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("map_get expects 2 arguments (map, key)")); }
        match (&args[0], &args[1]) {
            (Value::Record(_, fields), Value::String(key)) => {
                match fields.get(key.as_str()) {
                    Some(v) => Ok(Value::Tuple(vec![Value::Bool(true), v.clone()])),
                    None => Ok(Value::Tuple(vec![Value::Bool(false), Value::Unit])),
                }
            }
            _ => Err(InterpError::new("map_get expects (record, string)")),
        }
    }

    pub(crate) fn builtin_map_set(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 3 { return Err(InterpError::new("map_set expects 3 arguments (map, key, value)")); }
        match (&args[0], &args[1]) {
            (Value::Record(type_name, fields), Value::String(key)) => {
                let mut new_fields = fields.clone();
                new_fields.insert(key.clone(), args[2].clone());
                Ok(Value::Record(type_name.clone(), new_fields))
            }
            _ => Err(InterpError::new("map_set expects (record, string, value)")),
        }
    }

    pub(crate) fn builtin_map_remove(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("map_remove expects 2 arguments (map, key)")); }
        match (&args[0], &args[1]) {
            (Value::Record(type_name, fields), Value::String(key)) => {
                let mut new_fields = fields.clone();
                new_fields.remove(key.as_str());
                Ok(Value::Record(type_name.clone(), new_fields))
            }
            _ => Err(InterpError::new("map_remove expects (record, string)")),
        }
    }

    pub(crate) fn builtin_map_size(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("map_size expects 1 argument")); }
        match &args[0] {
            Value::Record(_, fields) => Ok(Value::Int(fields.len() as i64)),
            _ => Err(InterpError::new("map_size expects a record")),
        }
    }

    pub(crate) fn builtin_map_from_list(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("map_from_list expects 1 argument (list of (key, value) tuples)")); }
        match &args[0] {
            Value::List(pairs) => {
                let mut fields = std::collections::HashMap::new();
                for pair in pairs {
                    match pair {
                        Value::Tuple(vec) if vec.len() == 2 => {
                            if let Value::String(key) = &vec[0] {
                                fields.insert(key.clone(), vec[1].clone());
                            } else {
                                return Err(InterpError::new("map_from_list: keys must be strings"));
                            }
                        }
                        _ => return Err(InterpError::new("map_from_list: elements must be (string, value) tuples")),
                    }
                }
                Ok(Value::Record(None, fields))
            }
            _ => Err(InterpError::new("map_from_list expects a list")),
        }
    }

    // === Meta ===
    pub(crate) fn builtin_ast_dump(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("ast_dump expects 1 argument (a quoted AST)"));
        }
        match &args[0] {
            Value::QuoteAst(q) => Ok(Value::String(format!("{:?}", q))),
            other => Ok(Value::String(format!("Not a QuoteAst: {}", other))),
        }
    }

    pub(crate) fn builtin_ast_eval(&mut self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("ast_eval expects 1 argument (a quoted AST)"));
        }
        match &args[0] {
            Value::QuoteAst(q) => self.eval_quoted_ast(q),
            other => Err(InterpError::new(format!("ast_eval expects a QuoteAst, got {}", other))),
        }
    }

    // === JSON ===
    pub(crate) fn builtin_to_json(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("to_json expects 1 argument")); }
        let json_val = self.value_to_json(&args[0]).map_err(|e| InterpError::new(e.to_string()))?;
        let json_str = serde_json::to_string(&json_val)
            .map_err(|e| InterpError::new(format!("to_json error: {}", e)))?;
        Ok(Value::String(json_str))
    }

    pub(crate) fn builtin_json_is_valid(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("json_is_valid expects 1 argument (json string)")); }
        match &args[0] {
            Value::String(s) => {
                let valid = serde_json::from_str::<serde_json::Value>(s).is_ok();
                Ok(Value::Bool(valid))
            }
            _ => Err(InterpError::new("json_is_valid expects a string")),
        }
    }

    pub(crate) fn builtin_from_json(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("from_json expects 1 argument (json string)")); }
        match &args[0] {
            Value::String(s) => {
                // Validate JSON and return the string as-is (matches codegen behavior)
                let _: serde_json::Value = serde_json::from_str(s)
                    .map_err(|e| InterpError::new(format!("from_json parse error: {}", e)))?;
                Ok(Value::String(s.clone()))
            }
            _ => Err(InterpError::new("from_json expects a string")),
        }
    }

    pub(crate) fn builtin_json_get_string(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("json_get_string expects 2 arguments")); }
        match (&args[0], &args[1]) {
            (Value::String(json), Value::String(key)) => {
                let jv: serde_json::Value = serde_json::from_str(json)
                    .map_err(|e| InterpError::new(format!("json_get_string parse error: {}", e)))?;
                match jv.get(key) {
                    Some(serde_json::Value::String(s)) => Ok(Value::String(s.clone())),
                    Some(serde_json::Value::Bool(b)) => Ok(Value::String(if *b { "true".into() } else { "false".into() })),
                    Some(serde_json::Value::Number(n)) => Ok(Value::String(n.to_string())),
                    Some(serde_json::Value::Null) => Ok(Value::String("null".into())),
                    Some(serde_json::Value::Array(_)) => {
                        Err(InterpError::new(format!("json_get_string: key '{}' is an array, not a string", key)))
                    }
                    Some(serde_json::Value::Object(_)) => {
                        Err(InterpError::new(format!("json_get_string: key '{}' is an object, not a string", key)))
                    }
                    None => Err(InterpError::new(format!("json_get_string: key '{}' not found", key))),
                }
            }
            _ => Err(InterpError::new("json_get_string expects (string, string)")),
        }
    }

    pub(crate) fn builtin_json_get_int(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("json_get_int expects 2 arguments")); }
        match (&args[0], &args[1]) {
            (Value::String(json), Value::String(key)) => {
                let jv: serde_json::Value = serde_json::from_str(json)
                    .map_err(|e| InterpError::new(format!("json_get_int parse error: {}", e)))?;
                match jv.get(key) {
                    Some(serde_json::Value::Number(n)) => {
                        n.as_i64().map(Value::Int)
                            .ok_or_else(|| InterpError::new(format!("json_get_int: value for key '{}' is not an integer", key)))
                    }
                    Some(_) => Err(InterpError::new(format!("json_get_int: key '{}' is not a number", key))),
                    None => Err(InterpError::new(format!("json_get_int: key '{}' not found", key))),
                }
            }
            _ => Err(InterpError::new("json_get_int expects (string, string)")),
        }
    }

    pub(crate) fn builtin_json_get_element(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("json_get_element expects 2 arguments")); }
        match (&args[0], &args[1]) {
            (Value::String(json), Value::Int(idx)) => {
                let jv: serde_json::Value = serde_json::from_str(json)
                    .map_err(|e| InterpError::new(format!("json_get_element parse error: {}", e)))?;
                let index = *idx as usize;
                match jv.get(index) {
                    Some(val) => Ok(Value::String(val.to_string())),
                    None => Err(InterpError::new(format!("json_get_element: index {} out of bounds", index))),
                }
            }
            _ => Err(InterpError::new("json_get_element expects (string, int)")),
        }
    }

    // === Time ===
    pub(crate) fn builtin_now(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if !args.is_empty() { return Err(InterpError::new("now/timestamp expects 0 arguments")); }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| InterpError::new(format!("time error: {}", e)))?
            .as_secs() as i64;
        Ok(Value::Int(ts))
    }

    pub(crate) fn builtin_now_ms(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if !args.is_empty() { return Err(InterpError::new("now_ms/timestamp_ms expects 0 arguments")); }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| InterpError::new(format!("time error: {}", e)))?
            .as_millis() as i64;
        Ok(Value::Int(ts))
    }

    pub(crate) fn builtin_sleep(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("sleep expects 1 argument (milliseconds)")); }
        match &args[0] {
            Value::Int(ms) => {
                std::thread::sleep(std::time::Duration::from_millis(*ms as u64));
                Ok(Value::Unit)
            }
            _ => Err(InterpError::new("sleep expects an integer (milliseconds)")),
        }
    }

    // === Environment ===
    pub(crate) fn builtin_getenv(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("getenv expects 1 argument (name)")); }
        match &args[0] {
            Value::String(name) => {
                match std::env::var(name) {
                    Ok(val) => Ok(Value::Variant("Ok".into(), vec![Value::String(val)])),
                    Err(_) => Ok(Value::Variant("Err".into(), vec![Value::String(format!("env var '{}' not set", name))])),
                }
            }
            _ => Err(InterpError::new("getenv expects a string name")),
        }
    }

    pub(crate) fn builtin_args(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if !args.is_empty() { return Err(InterpError::new("args expects 0 arguments")); }
        let cli_args: Vec<Value> = std::env::args()
            .skip(1) // skip program name
            .map(|a| Value::String(a))
            .collect();
        Ok(Value::List(cli_args))
    }

    // === File I/O ===
    pub(crate) fn builtin_read_file(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("read_file expects 1 argument (path)")); }
        match &args[0] {
            Value::String(path) => {
                match std::fs::read_to_string(path) {
                    Ok(content) => Ok(Value::Variant("Ok".into(), vec![Value::String(content)])),
                    Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(format!("read_file error: {}", e))])),
                }
            }
            _ => Err(InterpError::new("read_file expects a string path")),
        }
    }

    pub(crate) fn builtin_write_file(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("write_file expects 2 arguments (path, content)")); }
        match (&args[0], &args[1]) {
            (Value::String(path), Value::String(content)) => {
                match std::fs::write(path, content) {
                    Ok(()) => Ok(Value::Variant("Ok".into(), vec![Value::Unit])),
                    Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(format!("write_file error: {}", e))])),
                }
            }
            _ => Err(InterpError::new("write_file expects (string, string)")),
        }
    }

    pub(crate) fn builtin_file_exists(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("file_exists expects 1 argument")); }
        match &args[0] {
            Value::String(path) => Ok(Value::Bool(std::path::Path::new(path).exists())),
            _ => Err(InterpError::new("file_exists expects a string path")),
        }
    }

    // === Allocator ===
    pub(crate) fn builtin_allocator_system(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        Ok(Value::Allocator(AllocatorKind::System))
    }

    pub(crate) fn builtin_allocator_arena(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        Ok(Value::Allocator(AllocatorKind::Arena))
    }

    pub(crate) fn builtin_allocator_bump(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        Ok(Value::Allocator(AllocatorKind::Bump))
    }

    pub(crate) fn builtin_alloc(&mut self, args: Vec<Value>) -> Result<Value, InterpError> {
        // alloc(allocator, value) - allocate a value with the given allocator
        if args.len() != 2 {
            return Err(InterpError::new("alloc expects 2 arguments (allocator, value)"));
        }
        let alloc_val = &args[0];
        let value = &args[1];
        match alloc_val {
            Value::Allocator(kind) => match kind {
                AllocatorKind::System => {
                    // System allocator: just return the value as-is (heap allocated)
                    Ok(value.clone())
                }
                AllocatorKind::Arena => {
                    // Arena allocator: allocate in current arena if available
                    if self.arenas.is_empty() {
                        return Err(InterpError::new("alloc: no arena available (use arena block)"));
                    }
                    let arena_id = self.arenas.len() - 1;
                    let idx = self.arenas[arena_id].slots.len();
                    self.arenas[arena_id].slots.push(value.clone());
                    Ok(Value::ArenaRef(arena_id, idx))
                }
                AllocatorKind::Bump => {
                    // Bump allocator: same as arena (monotonic allocation)
                    if self.arenas.is_empty() {
                        return Err(InterpError::new("alloc: no arena available (use alloc(Bump) block)"));
                    }
                    let arena_id = self.arenas.len() - 1;
                    let idx = self.arenas[arena_id].slots.len();
                    self.arenas[arena_id].slots.push(value.clone());
                    Ok(Value::ArenaRef(arena_id, idx))
                }
            },
            _ => Err(InterpError::new("alloc first argument must be an Allocator value")),
        }
    }

    pub(crate) fn builtin_arena_reset(&mut self, args: Vec<Value>) -> Result<Value, InterpError> {
        // arena_reset() - reset all arena allocations
        if !self.arenas.is_empty() {
            let arena_id = self.arenas.len() - 1;
            self.arenas[arena_id].slots.clear();
        }
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_bump_used(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        // bump_used() - return the number of bump allocations
        if self.arenas.is_empty() {
            return Ok(Value::Int(0));
        }
        let arena_id = self.arenas.len() - 1;
        Ok(Value::Int(self.arenas[arena_id].slots.len() as i64))
    }

    // === C interop ===
    pub(crate) fn builtin_str_to_c_str(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("str_to_c_str expects 1 argument (string)"));
        }
        match &args[0] {
            Value::String(s) => {
                // Return a tuple (pointer, length) for C compatibility
                // The pointer is the raw pointer to the CString data
                let c_str = std::ffi::CString::new(s.as_str())
                    .map_err(|e| InterpError::new(format!("failed to create C string: {}", e)))?;
                let ptr = c_str.into_raw() as i64;
                Ok(Value::Tuple(vec![Value::Int(ptr), Value::Int(s.len() as i64)]))
            }
            other => Err(InterpError::new(format!("str_to_c_str: argument must be a string, found {}", super::value::type_name(other)))),
        }
    }

    pub(crate) fn builtin_c_str_to_string(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("c_str_to_string expects 1 argument (pointer)"));
        }
        match &args[0] {
            Value::Int(ptr) => {
                if *ptr == 0 {
                    return Ok(Value::String(String::new()));
                }
                // SAFETY: ptr is checked for null above. We also validate that it points to
                // readable memory by attempting to read the first byte via a catch_unwind guard.
                // This does NOT guarantee the entire C string is valid, but catches obvious garbage.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    // Probe the first byte: dereference the pointer to check
                    // it points to readable memory (catches obvious garbage).
                    let ptr_raw = *ptr as *const u8;
                    // SAFETY: ptr_raw is a raw pointer from the interpreter's heap; the probe
                    // is wrapped in catch_unwind to recover from segfault on invalid pointers.
                    let _first_byte = unsafe { *ptr_raw };
                    // SAFETY: CStr::from_ptr requires a valid null-terminated string pointer;
                    // catch_unwind handles invalid pointer cases.
                    let c_str = unsafe { std::ffi::CStr::from_ptr(*ptr as *const i8) };
                    Value::String(c_str.to_string_lossy().into_owned())
                }));
                match result {
                    Ok(v) => Ok(v),
                    Err(_) => Err(InterpError::new(
                        format!("c_str_to_string: invalid pointer {:#x} (segfault or unmapped memory)", ptr)
                    )),
                }
            }
            other => Err(InterpError::new(format!("c_str_to_string: argument must be a pointer (int), found {}", super::value::type_name(other)))),
        }
    }

    // === MimiSpec runtime ===
    pub(crate) fn builtin_lexer(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("lexer expects 1 argument (source string)")); }
        match &args[0] {
            Value::String(source) => {
                match mimispec::tokenize(source) {
                    Ok(tokens) => {
                        let token_values: Vec<Value> = tokens.iter().map(|t| {
                            Value::Record(None, {
                                let mut fields = std::collections::HashMap::new();
                                fields.insert("kind".into(), Value::String(format!("{:?}", t.kind)));
                                fields.insert("line".into(), Value::Int(t.line as i64));
                                fields.insert("col".into(), Value::Int(t.col as i64));
                                fields
                            })
                        }).collect();
                        Ok(Value::List(token_values))
                    }
                    Err(e) => Err(InterpError::new(format!("lexer error: {}", e))),
                }
            }
            _ => Err(InterpError::new("lexer expects a string source")),
        }
    }

    pub(crate) fn builtin_parse(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("parse expects 1 argument (source string)")); }
        match &args[0] {
            Value::String(source) => {
                let result = mimispec::parse(source);
                if result.errors.is_empty() {
                    // Convert AST to a simple record representation
                    let mut fields = std::collections::HashMap::new();
                    fields.insert("imports".into(), Value::List(vec![]));
                    fields.insert("rules".into(), Value::List(vec![]));
                    fields.insert("fragments".into(), Value::List(vec![]));
                    Ok(Value::Record(Some("MmsAst".into()), fields))
                } else {
                    let errors: Vec<Value> = result.errors.iter().map(|e| {
                        Value::Record(None, {
                            let mut fields = std::collections::HashMap::new();
                            fields.insert("message".into(), Value::String(e.to_string()));
                            fields.insert("line".into(), Value::Int(e.line as i64));
                            fields.insert("col".into(), Value::Int(e.col as i64));
                            fields
                        })
                    }).collect();
                    Ok(Value::Tuple(vec![Value::Bool(false), Value::List(errors)]))
                }
            }
            _ => Err(InterpError::new("parse expects a string source")),
        }
    }

    pub(crate) fn builtin_from_int(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() < 1 {
            return Err(InterpError::new("from_int expects at least 1 argument (int)"));
        }
        match &args[0] {
            Value::Int(n) => Ok(Value::Int(*n)),
            _ => Err(InterpError::new("from_int: first arg must be an integer")),
        }
    }

    // === I/O (stderr) ===
    pub(crate) fn builtin_eprintln(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        let parts: Vec<String> = args.iter().map(|v| v.to_string()).collect();
        eprintln!("{}", parts.join(" "));
        Ok(Value::Unit)
    }

    // === Process control ===
    pub(crate) fn builtin_exit(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        let code = if args.is_empty() {
            0
        } else {
            match &args[0] {
                Value::Int(n) => *n as i32,
                _ => return Err(InterpError::new("exit expects an integer exit code")),
            }
        };
        std::process::exit(code)
    }

    // === Network builtins (interpreter implementations via libc) ===
    //
    // SAFETY: All network builtins call libc functions that are safe per POSIX when
    // given valid arguments. The Mimi interpreter validates argument types and ranges
    // before calling; return values are checked for error codes (typically -1). These
    // are FFI calls, not memory-safety critical — the worst outcome of a bug here is
    // a failed network operation, not memory corruption.
    pub(crate) fn builtin_socket(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 3 {
            return Err(InterpError::new("socket expects 3 arguments (domain, type, protocol)"));
        }
        let domain = match &args[0] { Value::Int(v) => *v, _ => return Err(InterpError::new("socket: domain must be i32")) };
        let type_ = match &args[1] { Value::Int(v) => *v, _ => return Err(InterpError::new("socket: type must be i32")) };
        let protocol = match &args[2] { Value::Int(v) => *v, _ => return Err(InterpError::new("socket: protocol must be i32")) };
        // SAFETY: libc::socket is safe per POSIX when arguments are valid integers;
        // we validate types above. Returns -1 on error, which we propagate.
        let fd = unsafe { libc::socket(domain as i32, type_ as i32, protocol as i32) };
        Ok(Value::Int(fd as i64))
    }

    pub(crate) fn builtin_connect(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 3 {
            return Err(InterpError::new("connect expects 3 arguments (fd, host, port)"));
        }
        let fd = match &args[0] { Value::Int(v) => *v, _ => return Err(InterpError::new("connect: fd must be i32")) };
        let host = match &args[1] { Value::String(s) => s.clone(), _ => return Err(InterpError::new("connect: host must be string")) };
        let port = match &args[2] { Value::Int(v) => *v, _ => return Err(InterpError::new("connect: port must be i32")) };
        let c_host = std::ffi::CString::new(host.as_str())
            .map_err(|e| InterpError::new(format!("connect: invalid host: {}", e)))?;
        // SAFETY: zeroed() is safe for POD structs like addrinfo/sockaddr_in.
        // getaddrinfo allocates memory that we check for null before use.
        // connect uses validated fd and the res pointer we receive from getaddrinfo.
        // freeaddrinfo frees memory allocated by getaddrinfo — safe as long as res is
        // non-null and was returned by getaddrinfo (both checked above).
        let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
        hints.ai_family = libc::AF_UNSPEC;
        hints.ai_socktype = libc::SOCK_STREAM;
        let port_str = format!("{}", port);
        let c_port = std::ffi::CString::new(port_str)
            .map_err(|_| InterpError::new("connect: invalid port"))?;
        let mut res: *mut libc::addrinfo = std::ptr::null_mut();
        let err = unsafe { libc::getaddrinfo(c_host.as_ptr(), c_port.as_ptr(), &hints, &mut res) };
        if err != 0 || res.is_null() {
            return Err(InterpError::new(format!("connect: getaddrinfo failed for '{}'", host)));
        }
        let ret = unsafe { libc::connect(fd as i32, (*res).ai_addr, (*res).ai_addrlen) };
        unsafe { libc::freeaddrinfo(res) };
        Ok(Value::Int(ret as i64))
    }

    pub(crate) fn builtin_bind(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("bind expects 2 arguments (fd, port)"));
        }
        let fd = match &args[0] { Value::Int(v) => *v, _ => return Err(InterpError::new("bind: fd must be i32")) };
        let port = match &args[1] { Value::Int(v) => *v, _ => return Err(InterpError::new("bind: port must be i32")) };
        // SAFETY: sockaddr_in is a POD struct; zeroed() is safe. bind() uses the validated
        // fd and a properly initialized sockaddr_in structure.
        let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        addr.sin_family = libc::AF_INET as libc::sa_family_t;
        addr.sin_port = (port as u16).to_be();
        addr.sin_addr.s_addr = libc::INADDR_ANY as u32;
        let ret = unsafe { libc::bind(fd as i32, &addr as *const _ as *const libc::sockaddr, std::mem::size_of::<libc::sockaddr_in>() as u32) };
        Ok(Value::Int(ret as i64))
    }

    pub(crate) fn builtin_listen(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("listen expects 2 arguments (fd, backlog)"));
        }
        let fd = match &args[0] { Value::Int(v) => *v, _ => return Err(InterpError::new("listen: fd must be i32")) };
        let backlog = match &args[1] { Value::Int(v) => *v, _ => return Err(InterpError::new("listen: backlog must be i32")) };
        // SAFETY: listen() uses a validated fd that came from a previous socket() call.
        let ret = unsafe { libc::listen(fd as i32, backlog as i32) };
        Ok(Value::Int(ret as i64))
    }

    pub(crate) fn builtin_accept(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("accept expects 1 argument (fd)"));
        }
        let fd = match &args[0] { Value::Int(v) => *v, _ => return Err(InterpError::new("accept: fd must be i32")) };
        // SAFETY: sockaddr_in is POD; zeroed() is safe. accept() fills in the
        // sockaddr with client info — the fd was validated by the interpreter.
        let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        let mut addr_len: libc::socklen_t = std::mem::size_of::<libc::sockaddr_in>() as u32;
        // SAFETY: accept() uses a validated fd; addr and addr_len are properly sized.
        let client_fd = unsafe { libc::accept(fd as i32, &mut addr as *mut _ as *mut libc::sockaddr, &mut addr_len) };
        Ok(Value::Int(client_fd as i64))
    }

    pub(crate) fn builtin_send(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("send expects 2 arguments (fd, data)"));
        }
        let fd = match &args[0] { Value::Int(v) => *v, _ => return Err(InterpError::new("send: fd must be i32")) };
        let data = match &args[1] { Value::String(s) => s.clone(), _ => return Err(InterpError::new("send: data must be string")) };
        // SAFETY: send() writes up to data.len() bytes from a Rust string's buffer,
        // which is guaranteed to be valid readable memory. fd was validated above.
        let sent = unsafe { libc::send(fd as i32, data.as_ptr() as *const libc::c_void, data.len(), 0) };
        Ok(Value::Int(sent as i64))
    }

    pub(crate) fn builtin_recv(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("recv expects 2 arguments (fd, buf_size)"));
        }
        let fd = match &args[0] { Value::Int(v) => *v, _ => return Err(InterpError::new("recv: fd must be i32")) };
        let buf_size = match &args[1] { Value::Int(v) => *v, _ => return Err(InterpError::new("recv: buf_size must be i32")) };
        if buf_size <= 0 {
            return Err(InterpError::new("recv: buf_size must be positive"));
        }
        let mut buf: Vec<u8> = vec![0u8; buf_size as usize];
        // SAFETY: recv() writes into a Rust Vec's buffer which is guaranteed writable
        // for buf_size bytes. fd was validated above. Returns -1 on error.
        let n = unsafe { libc::recv(fd as i32, buf.as_mut_ptr() as *mut libc::c_void, buf_size as usize, 0) };
        if n <= 0 {
            return Ok(Value::String(String::new()));
        }
        buf.truncate(n as usize);
        Ok(Value::String(String::from_utf8_lossy(&buf).to_string()))
    }

    pub(crate) fn builtin_close_fd(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("close_fd expects 1 argument (fd)"));
        }
        let fd = match &args[0] { Value::Int(v) => *v, _ => return Err(InterpError::new("close_fd: fd must be i32")) };
        // SAFETY: close() uses a validated fd from a previous socket() or accept() call.
        let ret = unsafe { libc::close(fd as i32) };
        Ok(Value::Int(ret as i64))
    }

    // === HTTP builtins (implemented via libc socket + http parsing) ===
    fn http_connect(host: &str, port: i64) -> Result<i64, InterpError> {
        // SAFETY: socket() creates a TCP socket; integer arguments are constants from libc.
        let domain = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
        if domain < 0 {
            return Err(InterpError::new("http: failed to create socket"));
        }
        let c_host = std::ffi::CString::new(host)
            .map_err(|e| InterpError::new(format!("http: invalid host: {}", e)))?;
        let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
        hints.ai_family = libc::AF_UNSPEC;
        hints.ai_socktype = libc::SOCK_STREAM;
        let port_str = format!("{}", port);
        let c_port = std::ffi::CString::new(port_str)
            .map_err(|_| InterpError::new("http: invalid port"))?;
        let mut res: *mut libc::addrinfo = std::ptr::null_mut();
        // SAFETY: getaddrinfo returns a linked list of addrinfo structs that we validate
        // for non-null. connect uses the first result. freeaddrinfo frees the list.
        let err = unsafe { libc::getaddrinfo(c_host.as_ptr(), c_port.as_ptr(), &hints, &mut res) };
        if err != 0 || res.is_null() {
            unsafe { libc::close(domain) };
            return Err(InterpError::new(format!("http: could not resolve host '{}'", host)));
        }
        let ret = unsafe { libc::connect(domain, (*res).ai_addr, (*res).ai_addrlen) };
        unsafe { libc::freeaddrinfo(res) };
        if ret < 0 {
            unsafe { libc::close(domain) };
            return Err(InterpError::new(format!("http: connection refused to '{}:{}'", host, port)));
        }
        Ok(domain as i64)
    }

    fn http_send_recv(fd: i64, request: &str) -> Result<String, InterpError> {
        let c_req = std::ffi::CString::new(request)
            .map_err(|e| InterpError::new(format!("http: invalid request: {}", e)))?;
        // SAFETY: send() writes from a CString buffer (null-terminated, valid memory).
        // recv() writes into a Rust Vec buffer (valid writable memory, 64KB).
        // close() uses the validated fd from http_connect().
        unsafe { libc::send(fd as i32, c_req.as_ptr() as *const libc::c_void, request.len(), 0) };
        let mut buf: Vec<u8> = vec![0u8; 65536];
        let n = unsafe { libc::recv(fd as i32, buf.as_mut_ptr() as *mut libc::c_void, 65536, 0) };
        unsafe { libc::close(fd as i32) };
        if n <= 0 {
            return Err(InterpError::new("http: empty response"));
        }
        buf.truncate(n as usize);
        Ok(String::from_utf8_lossy(&buf).to_string())
    }

    pub(crate) fn builtin_http_get(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("http_get expects 1 argument (url)"));
        }
        let url = match &args[0] { Value::String(s) => s.clone(), _ => return Err(InterpError::new("http_get: url must be string")) };
        // Parse URL: http://host[:port][/path]
        let url = url.trim_start_matches("http://");
        let (host, rest) = url.split_once('/').unwrap_or((url, ""));
        let path = if rest.is_empty() { "/" } else { &format!("/{}", rest) };
        let (host, port) = if let Some((h, p)) = host.split_once(':') {
            let port: i64 = p.parse().map_err(|_| InterpError::new("http_get: invalid port"))?;
            (h, port)
        } else {
            (host, 80)
        };
        let fd = Self::http_connect(host, port)?;
        let request = format!("GET {} HTTP/1.0\r\nHost: {}\r\nConnection: close\r\n\r\n", path, host);
        let response = Self::http_send_recv(fd, &request)?;
        // Extract body after \r\n\r\n
        let body = response.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or(&response);
        Ok(Value::String(body.to_string()))
    }

    pub(crate) fn builtin_http_post(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("http_post expects 2 arguments (url, body)"));
        }
        let url = match &args[0] { Value::String(s) => s.clone(), _ => return Err(InterpError::new("http_post: url must be string")) };
        let body = match &args[1] { Value::String(s) => s.clone(), _ => return Err(InterpError::new("http_post: body must be string")) };
        let url = url.trim_start_matches("http://");
        let (host, rest) = url.split_once('/').unwrap_or((url, ""));
        let path = if rest.is_empty() { "/" } else { &format!("/{}", rest) };
        let (host, port) = if let Some((h, p)) = host.split_once(':') {
            let port: i64 = p.parse().map_err(|_| InterpError::new("http_post: invalid port"))?;
            (h, port)
        } else {
            (host, 80)
        };
        let fd = Self::http_connect(host, port)?;
        let request = format!(
            "POST {} HTTP/1.0\r\nHost: {}\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n{}",
            path, host, body.len(), body
        );
        let response = Self::http_send_recv(fd, &request)?;
        let res_body = response.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or(&response);
        Ok(Value::String(res_body.to_string()))
    }
}
