use super::*;

impl<'a> Interpreter<'a> {
    // === I/O ===
    pub(crate) fn builtin_println(&self, args: Vec<Value>) -> Result<Value, String> {
        let parts: Vec<String> = args.iter().map(|v| v.to_string()).collect();
        println!("{}", parts.join(" "));
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_print(&self, args: Vec<Value>) -> Result<Value, String> {
        let parts: Vec<String> = args.iter().map(|v| v.to_string()).collect();
        print!("{}", parts.join(" "));
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_input(&mut self, args: Vec<Value>) -> Result<Value, String> {
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
    pub(crate) fn builtin_assert(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("assert expects 1 argument".into());
        }
        if !is_truthy(&args[0]) {
            return Err(format!("assertion failed: {}", args[0]));
        }
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_assert_eq(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 {
            return Err("assert_eq expects 2 arguments".into());
        }
        if !values_equal(&args[0], &args[1]) {
            return Err(format!("assertion failed: {} != {}", args[0], args[1]));
        }
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_assert_ne(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 {
            return Err("assert_ne expects 2 arguments".into());
        }
        if values_equal(&args[0], &args[1]) {
            return Err(format!("assertion failed: {} == {}", args[0], args[1]));
        }
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_assert_approx_eq(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 {
            return Err("assert_approx_eq expects 2 arguments".into());
        }
        match (&args[0], &args[1]) {
            (Value::Float(a), Value::Float(b)) => {
                if (a - b).abs() > f64::EPSILON {
                    return Err(format!("assertion failed: {} != {} (difference: {})", a, b, (a - b).abs()));
                }
                Ok(Value::Unit)
            }
            (Value::Int(a), Value::Int(b)) => {
                if a != b {
                    return Err(format!("assertion failed: {} != {}", a, b));
                }
                Ok(Value::Unit)
            }
            _ => {
                if !values_equal(&args[0], &args[1]) {
                    return Err(format!("assertion failed: {} != {}", args[0], args[1]));
                }
                Ok(Value::Unit)
            }
        }
    }

    // === Arithmetic ===
    pub(crate) fn builtin_sqrt(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("sqrt expects 1 argument".into());
        }
        match &args[0] {
            Value::Int(v) => Ok(Value::Float((*v as f64).sqrt())),
            Value::Float(v) => Ok(Value::Float(v.sqrt())),
            _ => Err("sqrt expects a number".into()),
        }
    }

    pub(crate) fn builtin_abs(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("abs expects 1 argument".into());
        }
        match &args[0] {
            Value::Int(v) => Ok(Value::Int(v.abs())),
            Value::Float(v) => Ok(Value::Float(v.abs())),
            _ => Err("abs expects a number".into()),
        }
    }

    pub(crate) fn builtin_pow(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 { return Err("pow expects 2 arguments (base, exp)".into()); }
        match (&args[0], &args[1]) {
            (Value::Int(b), Value::Int(e)) => Ok(Value::Int(b.pow(*e as u32))),
            (Value::Float(b), Value::Int(e)) => Ok(Value::Float(b.powf(*e as f64))),
            (Value::Float(b), Value::Float(e)) => Ok(Value::Float(b.powf(*e))),
            _ => Err("pow expects numbers".into()),
        }
    }

    pub(crate) fn builtin_floor(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 { return Err("floor expects 1 argument".into()); }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(v.floor())),
            Value::Int(v) => Ok(Value::Int(*v)),
            _ => Err("floor expects a number".into()),
        }
    }

    pub(crate) fn builtin_ceil(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 { return Err("ceil expects 1 argument".into()); }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(v.ceil())),
            Value::Int(v) => Ok(Value::Int(*v)),
            _ => Err("ceil expects a number".into()),
        }
    }

    pub(crate) fn builtin_round(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 { return Err("round expects 1 argument".into()); }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(v.round())),
            Value::Int(v) => Ok(Value::Int(*v)),
            _ => Err("round expects a number".into()),
        }
    }

    pub(crate) fn builtin_min(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 {
            return Err("min expects 2 arguments".into());
        }
        match (&args[0], &args[1]) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(*a.min(b))),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a.min(*b))),
            _ => Err("min expects two numbers of the same type".into()),
        }
    }

    pub(crate) fn builtin_max(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 {
            return Err("max expects 2 arguments".into());
        }
        match (&args[0], &args[1]) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(*a.max(b))),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a.max(*b))),
            _ => Err("max expects two numbers of the same type".into()),
        }
    }

    pub(crate) fn builtin_random(&self, args: Vec<Value>) -> Result<Value, String> {
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

    pub(crate) fn builtin_pi(&self, args: Vec<Value>) -> Result<Value, String> {
        Ok(Value::Float(std::f64::consts::PI))
    }

    // === List operations ===
    pub(crate) fn builtin_range(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 {
            return Err("range expects 2 arguments".into());
        }
        let start = match &args[0] {
            Value::Int(v) => *v,
            _ => return Err("range start must be integer".into()),
        };
        let end = match &args[1] {
            Value::Int(v) => *v,
            _ => return Err("range end must be integer".into()),
        };
        let list: Vec<Value> = (start..end).map(Value::Int).collect();
        Ok(Value::List(list))
    }

    pub(crate) fn builtin_len(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("len expects 1 argument".into());
        }
        match &args[0] {
            Value::String(s) => Ok(Value::Int(s.chars().count() as i64)),
            Value::List(l) => Ok(Value::Int(l.len() as i64)),
            Value::Array(a) => Ok(Value::Int(a.len() as i64)),
            Value::Slice { start, end, .. } => Ok(Value::Int((end - start) as i64)),
            other => Err(format!("len: argument must be a string, list, array, or slice, found {}", super::value::type_name(other))),
        }
    }

    pub(crate) fn builtin_push(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 {
            return Err("push expects 2 arguments (list, elem)".into());
        }
        match &args[0] {
            Value::List(l) => {
                let mut new_list = l.clone();
                new_list.push(args[1].clone());
                Ok(Value::List(new_list))
            }
            other => Err(format!("push: first argument must be a list, found {}", super::value::type_name(other))),
        }
    }

    pub(crate) fn builtin_pop(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("pop expects 1 argument (list)".into());
        }
        match &args[0] {
            Value::List(l) => {
                if l.is_empty() {
                    return Err("pop from empty list".into());
                }
                let mut new_list = l.clone();
                let popped = new_list.pop().expect("checked non-empty above");
                Ok(Value::Tuple(vec![popped, Value::List(new_list)]))
            }
            other => Err(format!("pop: argument must be a list, found {}", super::value::type_name(other))),
        }
    }

    pub(crate) fn builtin_contains(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 {
            return Err("contains expects 2 arguments (container, elem)".into());
        }
        match &args[0] {
            Value::List(l) => {
                let found = l.iter().any(|v| values_equal(v, &args[1]));
                Ok(Value::Bool(found))
            }
            Value::String(s) => {
                match &args[1] {
                    Value::String(sub) => Ok(Value::Bool(s.contains(sub.as_str()))),
                    other => Err(format!("contains on string expects a string needle, found {}", super::value::type_name(other))),
                }
            }
            other => Err(format!("contains: first argument must be a list or string, found {}", super::value::type_name(other))),
        }
    }

    pub(crate) fn builtin_map(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 {
            return Err("map expects 2 arguments (list, closure)".into());
        }
        match (&args[0], &args[1]) {
            (Value::List(l), Value::Closure { params, body, captured, .. }) => {
                if params.len() != 1 {
                    return Err("map closure must take 1 argument".into());
                }
                let mut result = Vec::new();
                for item in l {
                    if self.early_return.is_some() { break; }
                    self.push_scope();
                    for (n, v) in captured {
                        self.bind(n, v.clone());
                    }
                    self.bind(&params[0].name, item.clone());
                    let val = self.eval_block(body)?;
                    self.pop_scope();
                    if self.early_return.is_some() { break; }
                    result.push(val.unwrap_or(Value::Unit));
                }
                Ok(Value::List(result))
            }
            _ => Err("map expects (list, closure)".into()),
        }
    }

    pub(crate) fn builtin_filter(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 {
            return Err("filter expects 2 arguments (list, closure)".into());
        }
        match (&args[0], &args[1]) {
            (Value::List(l), Value::Closure { params, body, captured, .. }) => {
                if params.len() != 1 {
                    return Err("filter closure must take 1 argument".into());
                }
                let mut result = Vec::new();
                for item in l {
                    if self.early_return.is_some() { break; }
                    self.push_scope();
                    for (n, v) in captured {
                        self.bind(n, v.clone());
                    }
                    self.bind(&params[0].name, item.clone());
                    let val = self.eval_block(body)?;
                    self.pop_scope();
                    if self.early_return.is_some() { break; }
                    if is_truthy(&val.unwrap_or(Value::Unit)) {
                        result.push(item.clone());
                    }
                }
                Ok(Value::List(result))
            }
            _ => Err("filter expects (list, closure)".into()),
        }
    }

    pub(crate) fn builtin_reduce(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 3 {
            return Err("reduce expects 3 arguments (list, closure, initial)".into());
        }
        match (&args[0], &args[1]) {
            (Value::List(l), Value::Closure { params, body, captured, .. }) => {
                if params.len() != 2 {
                    return Err("reduce closure must take 2 arguments (acc, elem)".into());
                }
                let mut acc = args[2].clone();
                for item in l {
                    if self.early_return.is_some() { break; }
                    self.push_scope();
                    for (n, v) in captured {
                        self.bind(n, v.clone());
                    }
                    self.bind(&params[0].name, acc.clone());
                    self.bind(&params[1].name, item.clone());
                    let val = self.eval_block(body)?;
                    self.pop_scope();
                    if self.early_return.is_some() { break; }
                    acc = val.unwrap_or(Value::Unit);
                }
                Ok(acc)
            }
            _ => Err("reduce expects (list, closure, initial)".into()),
        }
    }

    pub(crate) fn builtin_sort(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("sort expects 1 argument (list)".into());
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
            _ => Err("sort expects a list".into()),
        }
    }

    pub(crate) fn builtin_reverse(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("reverse expects 1 argument (list)".into());
        }
        match &args[0] {
            Value::List(l) => {
                let mut reversed = l.clone();
                reversed.reverse();
                Ok(Value::List(reversed))
            }
            _ => Err("reverse expects a list".into()),
        }
    }

    pub(crate) fn builtin_flatten(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("flatten expects 1 argument (list of lists)".into());
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
            _ => Err("flatten expects a list".into()),
        }
    }

    pub(crate) fn builtin_zip(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 {
            return Err("zip expects 2 arguments (list, list)".into());
        }
        match (&args[0], &args[1]) {
            (Value::List(a), Value::List(b)) => {
                let len = a.len().min(b.len());
                let result: Vec<Value> = (0..len)
                    .map(|i| Value::Tuple(vec![a[i].clone(), b[i].clone()]))
                    .collect();
                Ok(Value::List(result))
            }
            _ => Err("zip expects two lists".into()),
        }
    }

    pub(crate) fn builtin_enumerate(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("enumerate expects 1 argument (list)".into());
        }
        match &args[0] {
            Value::List(l) => {
                let result: Vec<Value> = l.iter()
                    .enumerate()
                    .map(|(i, v)| Value::Tuple(vec![Value::Int(i as i64), v.clone()]))
                    .collect();
                Ok(Value::List(result))
            }
            _ => Err("enumerate expects a list".into()),
        }
    }

    pub(crate) fn builtin_sum(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("sum expects 1 argument (list)".into());
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
                        _ => return Err("sum expects a list of numbers".into()),
                    }
                }
                if is_float {
                    Ok(Value::Float(total_float + total_int as f64))
                } else {
                    Ok(Value::Int(total_int))
                }
            }
            _ => Err("sum expects a list".into()),
        }
    }

    // === Type utilities ===
    pub(crate) fn builtin_to_string(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("to_string expects 1 argument".into());
        }
        Ok(Value::String(args[0].to_string()))
    }

    pub(crate) fn builtin_type_name(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("type_name expects 1 argument (a value)".into());
        }
        let type_name = self.value_type_name(&args[0]);
        Ok(Value::String(type_name))
    }

    pub(crate) fn builtin_type_fields(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("type_fields expects 1 argument (a type name string)".into());
        }
        match &args[0] {
            Value::String(name) => {
                if let Some(type_def) = self.type_defs.get(name) {
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
                } else {
                    Err(format!("unknown type '{}'", name))
                }
            }
            Value::Type(name) => {
                if let Some(type_def) = self.type_defs.get(name) {
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
                } else {
                    Err(format!("unknown type '{}'", name))
                }
            }
            _ => Err("type_fields expects a type name string or Type value".into()),
        }
    }

    pub(crate) fn builtin_type_variants(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("type_variants expects 1 argument (a type name string)".into());
        }
        match &args[0] {
            Value::String(name) => {
                if let Some(type_def) = self.type_defs.get(name) {
                    match &type_def.kind {
                        TypeDefKind::Enum(variants) => {
                            let variant_names: Vec<Value> = variants.iter()
                                .map(|v| Value::String(v.name.clone()))
                                .collect();
                            Ok(Value::List(variant_names))
                        }
                        _ => Ok(Value::List(vec![])),
                    }
                } else {
                    Err(format!("unknown type '{}'", name))
                }
            }
            Value::Type(name) => {
                if let Some(type_def) = self.type_defs.get(name) {
                    match &type_def.kind {
                        TypeDefKind::Enum(variants) => {
                            let variant_names: Vec<Value> = variants.iter()
                                .map(|v| Value::String(v.name.clone()))
                                .collect();
                            Ok(Value::List(variant_names))
                        }
                        _ => Ok(Value::List(vec![])),
                    }
                } else {
                    Err(format!("unknown type '{}'", name))
                }
            }
            _ => Err("type_variants expects a type name string or Type value".into()),
        }
    }

    pub(crate) fn builtin_to_int(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 { return Err("to_int expects 1 argument".into()); }
        match &args[0] {
            Value::Int(v) => Ok(Value::Int(*v)),
            Value::Float(v) => Ok(Value::Int(*v as i64)),
            Value::String(s) => s.parse::<i64>()
                .map(Value::Int)
                .map_err(|e| format!("to_int parse error: {}", e)),
            Value::Bool(b) => Ok(Value::Int(*b as i64)),
            _ => Err("to_int cannot convert this type".into()),
        }
    }

    pub(crate) fn builtin_to_float(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 { return Err("to_float expects 1 argument".into()); }
        match &args[0] {
            Value::Float(v) => Ok(Value::Float(*v)),
            Value::Int(v) => Ok(Value::Float(*v as f64)),
            Value::String(s) => s.parse::<f64>()
                .map(Value::Float)
                .map_err(|e| format!("to_float parse error: {}", e)),
            _ => Err("to_float cannot convert this type".into()),
        }
    }

    pub(crate) fn builtin_keys(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 { return Err("keys expects 1 argument (record)".into()); }
        match &args[0] {
            Value::Record(_, fields) => {
                let keys: Vec<Value> = fields.keys().map(|k| Value::String(k.clone())).collect();
                Ok(Value::List(keys))
            }
            _ => Err("keys expects a record".into()),
        }
    }

    pub(crate) fn builtin_values(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 { return Err("values expects 1 argument (record)".into()); }
        match &args[0] {
            Value::Record(_, fields) => {
                Ok(Value::List(fields.values().cloned().collect()))
            }
            _ => Err("values expects a record".into()),
        }
    }

    pub(crate) fn builtin_has_key(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 { return Err("has_key expects 2 arguments (record, key)".into()); }
        match (&args[0], &args[1]) {
            (Value::Record(_, fields), Value::String(key)) => {
                Ok(Value::Bool(fields.contains_key(key.as_str())))
            }
            _ => Err("has_key expects (record, string)".into()),
        }
    }

    // === String operations ===
    pub(crate) fn builtin_str_char_at(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 { return Err("str_char_at expects 2 arguments (string, index)".into()); }
        match (&args[0], &args[1]) {
            (Value::String(s), Value::Int(idx)) => {
                let i = *idx as usize;
                s.chars().nth(i)
                    .map(|c| Value::String(c.to_string()))
                    .ok_or_else(|| format!("str_char_at: index {} out of bounds (len {})", i, s.chars().count()))
            }
            _ => Err("str_char_at expects (string, int)".into()),
        }
    }

    pub(crate) fn builtin_str_substring(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 3 { return Err("str_substring expects 3 arguments (string, start, end)".into()); }
        match (&args[0], &args[1], &args[2]) {
            (Value::String(s), Value::Int(start), Value::Int(end)) => {
                let chars: Vec<char> = s.chars().collect();
                let s_idx = (*start as usize).min(chars.len());
                let e_idx = (*end as usize).min(chars.len());
                if s_idx > e_idx {
                    return Err("str_substring: start > end".into());
                }
                Ok(Value::String(chars[s_idx..e_idx].iter().collect()))
            }
            _ => Err("str_substring expects (string, int, int)".into()),
        }
    }

    pub(crate) fn builtin_str_parse_int(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 { return Err("str_parse_int expects 1 argument".into()); }
        match &args[0] {
            Value::String(s) => Ok(s.trim().parse::<i64>()
                .map(|n| Value::Tuple(vec![Value::Bool(true), Value::Int(n)]))
                .unwrap_or_else(|_| Value::Tuple(vec![Value::Bool(false), Value::Int(0)]))),
            _ => Err("str_parse_int expects a string".into()),
        }
    }

    pub(crate) fn builtin_str_parse_float(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 { return Err("str_parse_float expects 1 argument".into()); }
        match &args[0] {
            Value::String(s) => Ok(s.trim().parse::<f64>()
                .map(|n| Value::Tuple(vec![Value::Bool(true), Value::Float(n)]))
                .unwrap_or_else(|_| Value::Tuple(vec![Value::Bool(false), Value::Float(0.0)]))),
            _ => Err("str_parse_float expects a string".into()),
        }
    }

    pub(crate) fn builtin_str_split(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 { return Err("str_split expects 2 arguments (string, delimiter)".into()); }
        match (&args[0], &args[1]) {
            (Value::String(s), Value::String(delimiter)) => {
                let mut parts = Vec::new();
                for p in s.split(delimiter.as_str()) {
                    parts.push(Value::String(p.to_string()));
                }
                Ok(Value::List(parts))
            }
            _ => Err("str_split expects (string, string)".into()),
        }
    }

    pub(crate) fn builtin_str_join(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 { return Err("str_join expects 2 arguments (list, separator)".into()); }
        match (&args[0], &args[1]) {
            (Value::List(parts), Value::String(sep)) => {
                let mut strings = Vec::new();
                for p in parts {
                    match p {
                        Value::String(s) => strings.push(s.clone()),
                        _ => return Err("str_join: list elements must be strings".into()),
                    }
                }
                Ok(Value::String(strings.join(sep)))
            }
            _ => Err("str_join expects (list, string)".into()),
        }
    }

    pub(crate) fn builtin_str_trim(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 { return Err("str_trim expects 1 argument".into()); }
        match &args[0] {
            Value::String(s) => Ok(Value::String(s.trim().to_string())),
            _ => Err("str_trim expects a string".into()),
        }
    }

    pub(crate) fn builtin_str_starts_with(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 { return Err("str_starts_with expects 2 arguments".into()); }
        match (&args[0], &args[1]) {
            (Value::String(s), Value::String(prefix)) => {
                Ok(Value::Bool(s.starts_with(prefix.as_str())))
            }
            _ => Err("str_starts_with expects (string, string)".into()),
        }
    }

    pub(crate) fn builtin_str_ends_with(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 { return Err("str_ends_with expects 2 arguments".into()); }
        match (&args[0], &args[1]) {
            (Value::String(s), Value::String(suffix)) => {
                Ok(Value::Bool(s.ends_with(suffix.as_str())))
            }
            _ => Err("str_ends_with expects (string, string)".into()),
        }
    }

    pub(crate) fn builtin_str_replace(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 3 { return Err("str_replace expects 3 arguments".into()); }
        match (&args[0], &args[1], &args[2]) {
            (Value::String(s), Value::String(from), Value::String(to)) => {
                Ok(Value::String(s.replace(from.as_str(), to.as_str())))
            }
            _ => Err("str_replace expects (string, string, string)".into()),
        }
    }

    pub(crate) fn builtin_str_to_upper(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 { return Err("str_to_upper expects 1 argument".into()); }
        match &args[0] {
            Value::String(s) => Ok(Value::String(s.to_uppercase())),
            _ => Err("str_to_upper expects a string".into()),
        }
    }

    pub(crate) fn builtin_str_to_lower(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 { return Err("str_to_lower expects 1 argument".into()); }
        match &args[0] {
            Value::String(s) => Ok(Value::String(s.to_lowercase())),
            _ => Err("str_to_lower expects a string".into()),
        }
    }

    pub(crate) fn builtin_str_repeat(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 { return Err("str_repeat expects 2 arguments".into()); }
        match (&args[0], &args[1]) {
            (Value::String(s), Value::Int(n)) => {
                if *n < 0 { return Err("str_repeat: count must be non-negative".into()); }
                Ok(Value::String(s.repeat(*n as usize)))
            }
            _ => Err("str_repeat expects (string, int)".into()),
        }
    }

    pub(crate) fn builtin_str_contains(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 { return Err("str_contains expects 2 arguments".into()); }
        match (&args[0], &args[1]) {
            (Value::String(s), Value::String(sub)) => {
                Ok(Value::Bool(s.contains(sub.as_str())))
            }
            _ => Err("str_contains expects (string, string)".into()),
        }
    }

    pub(crate) fn builtin_str_index_of(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 { return Err("str_index_of expects 2 arguments".into()); }
        match (&args[0], &args[1]) {
            (Value::String(s), Value::String(sub)) => {
                match s.find(sub.as_str()) {
                    Some(idx) => Ok(Value::Tuple(vec![Value::Bool(true), Value::Int(idx as i64)])),
                    None => Ok(Value::Tuple(vec![Value::Bool(false), Value::Int(-1)])),
                }
            }
            _ => Err("str_index_of expects (string, string)".into()),
        }
    }

    // === Map/Record utilities ===
    pub(crate) fn builtin_map_new(&self, args: Vec<Value>) -> Result<Value, String> {
        Ok(Value::Record(None, std::collections::HashMap::new()))
    }

    pub(crate) fn builtin_map_get(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 { return Err("map_get expects 2 arguments (map, key)".into()); }
        match (&args[0], &args[1]) {
            (Value::Record(_, fields), Value::String(key)) => {
                match fields.get(key.as_str()) {
                    Some(v) => Ok(Value::Tuple(vec![Value::Bool(true), v.clone()])),
                    None => Ok(Value::Tuple(vec![Value::Bool(false), Value::Unit])),
                }
            }
            _ => Err("map_get expects (record, string)".into()),
        }
    }

    pub(crate) fn builtin_map_set(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 3 { return Err("map_set expects 3 arguments (map, key, value)".into()); }
        match (&args[0], &args[1]) {
            (Value::Record(type_name, fields), Value::String(key)) => {
                let mut new_fields = fields.clone();
                new_fields.insert(key.clone(), args[2].clone());
                Ok(Value::Record(type_name.clone(), new_fields))
            }
            _ => Err("map_set expects (record, string, value)".into()),
        }
    }

    pub(crate) fn builtin_map_remove(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 { return Err("map_remove expects 2 arguments (map, key)".into()); }
        match (&args[0], &args[1]) {
            (Value::Record(type_name, fields), Value::String(key)) => {
                let mut new_fields = fields.clone();
                new_fields.remove(key.as_str());
                Ok(Value::Record(type_name.clone(), new_fields))
            }
            _ => Err("map_remove expects (record, string)".into()),
        }
    }

    pub(crate) fn builtin_map_size(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 { return Err("map_size expects 1 argument".into()); }
        match &args[0] {
            Value::Record(_, fields) => Ok(Value::Int(fields.len() as i64)),
            _ => Err("map_size expects a record".into()),
        }
    }

    pub(crate) fn builtin_map_from_list(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 { return Err("map_from_list expects 1 argument (list of (key, value) tuples)".into()); }
        match &args[0] {
            Value::List(pairs) => {
                let mut fields = std::collections::HashMap::new();
                for pair in pairs {
                    match pair {
                        Value::Tuple(vec) if vec.len() == 2 => {
                            if let Value::String(key) = &vec[0] {
                                fields.insert(key.clone(), vec[1].clone());
                            } else {
                                return Err("map_from_list: keys must be strings".into());
                            }
                        }
                        _ => return Err("map_from_list: elements must be (string, value) tuples".into()),
                    }
                }
                Ok(Value::Record(None, fields))
            }
            _ => Err("map_from_list expects a list".into()),
        }
    }

    // === Meta ===
    pub(crate) fn builtin_ast_dump(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("ast_dump expects 1 argument (a quoted AST)".into());
        }
        match &args[0] {
            Value::QuoteAst(q) => Ok(Value::String(format!("{:?}", q))),
            other => Ok(Value::String(format!("Not a QuoteAst: {}", other))),
        }
    }

    pub(crate) fn builtin_ast_eval(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("ast_eval expects 1 argument (a quoted AST)".into());
        }
        match &args[0] {
            Value::QuoteAst(q) => self.eval_quoted_ast(q),
            other => Err(format!("ast_eval expects a QuoteAst, got {}", other)),
        }
    }

    // === JSON ===
    pub(crate) fn builtin_to_json(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 { return Err("to_json expects 1 argument".into()); }
        let json_val = self.value_to_json(&args[0])?;
        let json_str = serde_json::to_string(&json_val)
            .map_err(|e| format!("to_json error: {}", e))?;
        Ok(Value::String(json_str))
    }

    pub(crate) fn builtin_from_json(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 { return Err("from_json expects 1 argument (json string)".into()); }
        match &args[0] {
            Value::String(s) => {
                // Validate JSON and return the string as-is (matches codegen behavior)
                let _: serde_json::Value = serde_json::from_str(s)
                    .map_err(|e| format!("from_json parse error: {}", e))?;
                Ok(Value::String(s.clone()))
            }
            _ => Err("from_json expects a string".into()),
        }
    }

    pub(crate) fn builtin_json_get_string(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 { return Err("json_get_string expects 2 arguments".into()); }
        match (&args[0], &args[1]) {
            (Value::String(json), Value::String(key)) => {
                let jv: serde_json::Value = serde_json::from_str(json)
                    .map_err(|e| format!("json_get_string parse error: {}", e))?;
                match jv.get(key) {
                    Some(serde_json::Value::String(s)) => Ok(Value::String(s.clone())),
                    _ => Ok(Value::String("".into())),
                }
            }
            _ => Err("json_get_string expects (string, string)".into()),
        }
    }

    pub(crate) fn builtin_json_get_int(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 { return Err("json_get_int expects 2 arguments".into()); }
        match (&args[0], &args[1]) {
            (Value::String(json), Value::String(key)) => {
                let jv: serde_json::Value = serde_json::from_str(json)
                    .map_err(|e| format!("json_get_int parse error: {}", e))?;
                match jv.get(key) {
                    Some(serde_json::Value::Number(n)) => {
                        if let Some(i) = n.as_i64() { Ok(Value::Int(i)) }
                        else { Ok(Value::Int(0)) }
                    }
                    _ => Ok(Value::Int(0)),
                }
            }
            _ => Err("json_get_int expects (string, string)".into()),
        }
    }

    pub(crate) fn builtin_json_get_element(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 { return Err("json_get_element expects 2 arguments".into()); }
        match (&args[0], &args[1]) {
            (Value::String(json), Value::Int(idx)) => {
                let jv: serde_json::Value = serde_json::from_str(json)
                    .map_err(|e| format!("json_get_element parse error: {}", e))?;
                match jv.get(*idx as usize) {
                    Some(val) => Ok(Value::String(val.to_string())),
                    None => Ok(Value::String("".into())),
                }
            }
            _ => Err("json_get_element expects (string, int)".into()),
        }
    }

    // === Time ===
    pub(crate) fn builtin_now(&self, args: Vec<Value>) -> Result<Value, String> {
        if !args.is_empty() { return Err("now/timestamp expects 0 arguments".into()); }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| format!("time error: {}", e))?
            .as_secs() as i64;
        Ok(Value::Int(ts))
    }

    pub(crate) fn builtin_now_ms(&self, args: Vec<Value>) -> Result<Value, String> {
        if !args.is_empty() { return Err("now_ms/timestamp_ms expects 0 arguments".into()); }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| format!("time error: {}", e))?
            .as_millis() as i64;
        Ok(Value::Int(ts))
    }

    pub(crate) fn builtin_sleep(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 { return Err("sleep expects 1 argument (milliseconds)".into()); }
        match &args[0] {
            Value::Int(ms) => {
                std::thread::sleep(std::time::Duration::from_millis(*ms as u64));
                Ok(Value::Unit)
            }
            _ => Err("sleep expects an integer (milliseconds)".into()),
        }
    }

    // === Environment ===
    pub(crate) fn builtin_getenv(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 { return Err("getenv expects 1 argument (name)".into()); }
        match &args[0] {
            Value::String(name) => {
                match std::env::var(name) {
                    Ok(val) => Ok(Value::Variant("Ok".into(), vec![Value::String(val)])),
                    Err(_) => Ok(Value::Variant("Err".into(), vec![Value::String(format!("env var '{}' not set", name))])),
                }
            }
            _ => Err("getenv expects a string name".into()),
        }
    }

    pub(crate) fn builtin_args(&self, args: Vec<Value>) -> Result<Value, String> {
        if !args.is_empty() { return Err("args expects 0 arguments".into()); }
        let cli_args: Vec<Value> = std::env::args()
            .skip(1) // skip program name
            .map(|a| Value::String(a))
            .collect();
        Ok(Value::List(cli_args))
    }

    // === File I/O ===
    pub(crate) fn builtin_read_file(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 { return Err("read_file expects 1 argument (path)".into()); }
        match &args[0] {
            Value::String(path) => {
                match std::fs::read_to_string(path) {
                    Ok(content) => Ok(Value::Variant("Ok".into(), vec![Value::String(content)])),
                    Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(format!("read_file error: {}", e))])),
                }
            }
            _ => Err("read_file expects a string path".into()),
        }
    }

    pub(crate) fn builtin_write_file(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 { return Err("write_file expects 2 arguments (path, content)".into()); }
        match (&args[0], &args[1]) {
            (Value::String(path), Value::String(content)) => {
                match std::fs::write(path, content) {
                    Ok(()) => Ok(Value::Variant("Ok".into(), vec![Value::Unit])),
                    Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(format!("write_file error: {}", e))])),
                }
            }
            _ => Err("write_file expects (string, string)".into()),
        }
    }

    pub(crate) fn builtin_file_exists(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 { return Err("file_exists expects 1 argument".into()); }
        match &args[0] {
            Value::String(path) => Ok(Value::Bool(std::path::Path::new(path).exists())),
            _ => Err("file_exists expects a string path".into()),
        }
    }

    // === Allocator ===
    pub(crate) fn builtin_allocator_system(&self, args: Vec<Value>) -> Result<Value, String> {
        Ok(Value::Allocator(AllocatorKind::System))
    }

    pub(crate) fn builtin_allocator_arena(&self, args: Vec<Value>) -> Result<Value, String> {
        Ok(Value::Allocator(AllocatorKind::Arena))
    }

    pub(crate) fn builtin_allocator_bump(&self, args: Vec<Value>) -> Result<Value, String> {
        Ok(Value::Allocator(AllocatorKind::Bump))
    }

    pub(crate) fn builtin_alloc(&mut self, args: Vec<Value>) -> Result<Value, String> {
        // alloc(allocator, value) - allocate a value with the given allocator
        if args.len() != 2 {
            return Err("alloc expects 2 arguments (allocator, value)".into());
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
                        return Err("alloc: no arena available (use arena block)".into());
                    }
                    let arena_id = self.arenas.len() - 1;
                    let idx = self.arenas[arena_id].slots.len();
                    self.arenas[arena_id].slots.push(value.clone());
                    Ok(Value::ArenaRef(arena_id, idx))
                }
                AllocatorKind::Bump => {
                    // Bump allocator: same as arena (monotonic allocation)
                    if self.arenas.is_empty() {
                        return Err("alloc: no arena available (use alloc(Bump) block)".into());
                    }
                    let arena_id = self.arenas.len() - 1;
                    let idx = self.arenas[arena_id].slots.len();
                    self.arenas[arena_id].slots.push(value.clone());
                    Ok(Value::ArenaRef(arena_id, idx))
                }
            },
            _ => Err("alloc first argument must be an Allocator value".into()),
        }
    }

    pub(crate) fn builtin_arena_reset(&mut self, args: Vec<Value>) -> Result<Value, String> {
        // arena_reset() - reset all arena allocations
        if !self.arenas.is_empty() {
            let arena_id = self.arenas.len() - 1;
            self.arenas[arena_id].slots.clear();
        }
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_bump_used(&self, args: Vec<Value>) -> Result<Value, String> {
        // bump_used() - return the number of bump allocations
        if self.arenas.is_empty() {
            return Ok(Value::Int(0));
        }
        let arena_id = self.arenas.len() - 1;
        Ok(Value::Int(self.arenas[arena_id].slots.len() as i64))
    }

    // === C interop ===
    pub(crate) fn builtin_str_to_c_str(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("str_to_c_str expects 1 argument (string)".into());
        }
        match &args[0] {
            Value::String(s) => {
                // Return a tuple (pointer, length) for C compatibility
                // The pointer is the raw pointer to the CString data
                let c_str = std::ffi::CString::new(s.as_str())
                    .map_err(|e| format!("failed to create C string: {}", e))?;
                let ptr = c_str.into_raw() as i64;
                Ok(Value::Tuple(vec![Value::Int(ptr), Value::Int(s.len() as i64)]))
            }
            other => Err(format!("str_to_c_str: argument must be a string, found {}", super::value::type_name(other))),
        }
    }

    pub(crate) fn builtin_c_str_to_string(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("c_str_to_string expects 1 argument (pointer)".into());
        }
        match &args[0] {
            Value::Int(ptr) => {
                if *ptr == 0 {
                    return Ok(Value::String(String::new()));
                }
                let c_str = unsafe { std::ffi::CStr::from_ptr(*ptr as *const i8) };
                Ok(Value::String(c_str.to_string_lossy().into_owned()))
            }
            other => Err(format!("c_str_to_string: argument must be a pointer (int), found {}", super::value::type_name(other))),
        }
    }

    // === MimiSpec runtime ===
    pub(crate) fn builtin_lexer(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 { return Err("lexer expects 1 argument (source string)".into()); }
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
                    Err(e) => Err(format!("lexer error: {}", e)),
                }
            }
            _ => Err("lexer expects a string source".into()),
        }
    }

    pub(crate) fn builtin_parse(&self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 1 { return Err("parse expects 1 argument (source string)".into()); }
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
                            fields.insert("line".into(), Value::Int(e.line() as i64));
                            fields.insert("col".into(), Value::Int(e.col() as i64));
                            fields
                        })
                    }).collect();
                    Ok(Value::Tuple(vec![Value::Bool(false), Value::List(errors)]))
                }
            }
            _ => Err("parse expects a string source".into()),
        }
    }
}
