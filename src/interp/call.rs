use super::*;
use crate::ffi::{FfiArgContract, FfiContract, FfiRetContract, CAP_TABLE, SHARED_TABLE};

impl<'a> Interpreter<'a> {
    pub(crate) fn call_func(&mut self, func: &FuncDef, args: Vec<Value>) -> Result<Value, String> {
        if func.params.len() != args.len() {
            return Err(format!(
                "function {} expects {} arguments, got {}",
                func.name,
                func.params.len(),
                args.len()
            ));
        }
        
        // Handle async functions
        if func.is_async {
            return self.call_async_func(func, args);
        }
        
        self.push_call(&func.name);
        self.push_scope();

        // Snapshot parameters for old() in ensures
        let mut old_snapshots: HashMap<String, Value> = HashMap::new();
        for (p, a) in func.params.iter().zip(args) {
            old_snapshots.insert(p.name.clone(), a.clone());
            self.bind(&p.name, a);
        }

        // Extract and check requires conditions
        if self.verify_contracts {
            for stmt in &func.body {
                if let Stmt::Requires(expr, _) = stmt {
                    let cond = self.eval_expr(expr)?;
                    if !is_truthy(&cond) {
                        self.pop_scope();
                        self.pop_call();
                        return Err(format!("requires condition failed for '{}': {}", func.name, cond));
                    }
                }
            }
        }

        let result = self.eval_block(&func.body);

        // Extract and check ensures conditions
        if self.verify_contracts {
            if let Ok(Some(ref rv)) = result {
                self.push_scope();
                self.bind("result", rv.clone());
                // Bind old snapshots for old(x) access
                for (name, val) in &old_snapshots {
                    self.bind(&format!("old_{}", name), val.clone());
                }
                let ensures_ok = (|| {
                    for stmt in &func.body {
                        if let Stmt::Ensures(expr, _) = stmt {
                            let cond = self.eval_expr(expr)?;
                            if !is_truthy(&cond) {
                                return Err(format!("ensures condition failed for '{}': {}", func.name, cond));
                            }
                        }
                    }
                    Ok(())
                })();
                self.pop_scope(); // always pop ensures scope
                if let Err(e) = ensures_ok {
                    self.pop_scope(); // pop function scope
                    self.pop_call();
                    return Err(e);
                }
            }
        }

        self.pop_scope();
        self.pop_call();
        result.map(|v| v.unwrap_or(Value::Unit))
    }

    pub fn call_named(&mut self, name: &str, args: Vec<Value>) -> Result<Value, String> {
        // First check if the name is bound to a closure in the local scope
        if let Some(v) = self.lookup(name) {
            match v {
                Value::Closure { params, ret: _, body, captured } => {
                    if params.len() != args.len() {
                        return Err(format!(
                            "closure '{}' expects {} arguments, got {}",
                            name, params.len(), args.len()
                        ));
                    }
                    self.push_scope();
                    for (n, val) in &captured {
                        self.bind(n, val.clone());
                    }
                    for (p, a) in params.iter().zip(args) {
                        self.bind(&p.name, a);
                    }
                    let result = self.eval_block(&body);
                    self.pop_scope();
                    return result.map(|v| v.unwrap_or(Value::Unit));
                }
                other => {
                    // Not a closure, fall through to other lookup methods
                    drop(other);
                }
            }
        }

        // Handle Actor.spawn() calls
        if let Some(actor_name) = name.strip_suffix(".spawn") {
            return self.spawn_actor(actor_name, args);
        }

        // Handle extern function calls via their FFI contract (wrapper layer).
        if let Some(extern_func) = self.extern_funcs.get(name).cloned() {
            let contract = self.ffi_contracts.get(name).cloned()
                .unwrap_or_else(|| FfiContract::from_extern(&extern_func));
            return self.call_extern(&extern_func, &contract, args);
        }

        if let Some(&arity) = self.constructors.get(name) {
            if args.len() != arity {
                return Err(format!(
                    "constructor '{}' expects {} arguments, got {}",
                    name, arity, args.len()
                ));
            }
            // Check if this is a newtype constructor - wrap in Value::Newtype
            if *self.newtype_constructors.get(name).unwrap_or(&false) && args.len() == 1 {
                return Ok(Value::Newtype(Box::new(args.into_iter().next().expect("args.len() == 1 guaranteed single element"))));
            }
            return Ok(Value::Variant(name.into(), args));
        }
        // Check user-defined functions before builtins
        if let Some(func) = self.find_function(name) {
            return self.call_func(&func, args);
        }
        match name {
            "println" => {
                let parts: Vec<String> = args.iter().map(|v| v.to_string()).collect();
                println!("{}", parts.join(" "));
                Ok(Value::Unit)
            }
            "assert" => {
                if args.len() != 1 {
                    return Err("assert expects 1 argument".into());
                }
                if !is_truthy(&args[0]) {
                    return Err(format!("assertion failed: {}", args[0]));
                }
                Ok(Value::Unit)
            }
            "range" => {
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
            "sqrt" => {
                if args.len() != 1 {
                    return Err("sqrt expects 1 argument".into());
                }
                match &args[0] {
                    Value::Int(v) => Ok(Value::Float((*v as f64).sqrt())),
                    Value::Float(v) => Ok(Value::Float(v.sqrt())),
                    _ => Err("sqrt expects a number".into()),
                }
            }
            "len" => {
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
            "to_string" => {
                if args.len() != 1 {
                    return Err("to_string expects 1 argument".into());
                }
                Ok(Value::String(args[0].to_string()))
            }
            "abs" => {
                if args.len() != 1 {
                    return Err("abs expects 1 argument".into());
                }
                match &args[0] {
                    Value::Int(v) => Ok(Value::Int(v.abs())),
                    Value::Float(v) => Ok(Value::Float(v.abs())),
                    _ => Err("abs expects a number".into()),
                }
            }
            "push" => {
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
            "pop" => {
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
            "min" => {
                if args.len() != 2 {
                    return Err("min expects 2 arguments".into());
                }
                match (&args[0], &args[1]) {
                    (Value::Int(a), Value::Int(b)) => Ok(Value::Int(*a.min(b))),
                    (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a.min(*b))),
                    _ => Err("min expects two numbers of the same type".into()),
                }
            }
            "max" => {
                if args.len() != 2 {
                    return Err("max expects 2 arguments".into());
                }
                match (&args[0], &args[1]) {
                    (Value::Int(a), Value::Int(b)) => Ok(Value::Int(*a.max(b))),
                    (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a.max(*b))),
                    _ => Err("max expects two numbers of the same type".into()),
                }
            }
            "contains" => {
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
            "input" => {
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
            "assert_eq" => {
                if args.len() != 2 {
                    return Err("assert_eq expects 2 arguments".into());
                }
                if !values_equal(&args[0], &args[1]) {
                    return Err(format!("assertion failed: {} != {}", args[0], args[1]));
                }
                Ok(Value::Unit)
            }
            "assert_ne" => {
                if args.len() != 2 {
                    return Err("assert_ne expects 2 arguments".into());
                }
                if values_equal(&args[0], &args[1]) {
                    return Err(format!("assertion failed: {} == {}", args[0], args[1]));
                }
                Ok(Value::Unit)
            }
            "assert_approx_eq" => {
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
            "map" => {
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
                            self.push_scope();
                            for (n, v) in captured {
                                self.bind(n, v.clone());
                            }
                            self.bind(&params[0].name, item.clone());
                            let val = self.eval_block(body)?;
                            self.pop_scope();
                            result.push(val.unwrap_or(Value::Unit));
                        }
                        Ok(Value::List(result))
                    }
                    _ => Err("map expects (list, closure)".into()),
                }
            }
            "filter" => {
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
                            self.push_scope();
                            for (n, v) in captured {
                                self.bind(n, v.clone());
                            }
                            self.bind(&params[0].name, item.clone());
                            let val = self.eval_block(body)?;
                            self.pop_scope();
                            if is_truthy(&val.unwrap_or(Value::Unit)) {
                                result.push(item.clone());
                            }
                        }
                        Ok(Value::List(result))
                    }
                    _ => Err("filter expects (list, closure)".into()),
                }
            }
            "reduce" => {
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
                            self.push_scope();
                            for (n, v) in captured {
                                self.bind(n, v.clone());
                            }
                            self.bind(&params[0].name, acc.clone());
                            self.bind(&params[1].name, item.clone());
                            let val = self.eval_block(body)?;
                            self.pop_scope();
                            acc = val.unwrap_or(Value::Unit);
                        }
                        Ok(acc)
                    }
                    _ => Err("reduce expects (list, closure, initial)".into()),
                }
            }
            "sort" => {
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
            "reverse" => {
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
            "flatten" => {
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
            "zip" => {
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
            "enumerate" => {
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
            "sum" => {
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
            "ast_dump" => {
                if args.len() != 1 {
                    return Err("ast_dump expects 1 argument (a quoted AST)".into());
                }
                match &args[0] {
                    Value::QuoteAst(q) => Ok(Value::String(format!("{:?}", q))),
                    other => Ok(Value::String(format!("Not a QuoteAst: {}", other))),
                }
            }
            "ast_eval" => {
                if args.len() != 1 {
                    return Err("ast_eval expects 1 argument (a quoted AST)".into());
                }
                match &args[0] {
                    Value::QuoteAst(q) => self.eval_quoted_ast(q),
                    other => Err(format!("ast_eval expects a QuoteAst, got {}", other)),
                }
            }
            "type_name" => {
                if args.len() != 1 {
                    return Err("type_name expects 1 argument (a value)".into());
                }
                let type_name = self.value_type_name(&args[0]);
                Ok(Value::String(type_name))
            }
            "type_fields" => {
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
            "type_variants" => {
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
            // Allocator builtins
            "allocator_system" => {
                Ok(Value::Allocator(AllocatorKind::System))
            }
            "allocator_arena" => {
                Ok(Value::Allocator(AllocatorKind::Arena))
            }
            "allocator_bump" => {
                Ok(Value::Allocator(AllocatorKind::Bump))
            }
            "alloc" => {
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
            "arena_reset" => {
                // arena_reset() - reset all arena allocations
                if !self.arenas.is_empty() {
                    let arena_id = self.arenas.len() - 1;
                    self.arenas[arena_id].slots.clear();
                }
                Ok(Value::Unit)
            }
            "bump_used" => {
                // bump_used() - return the number of bump allocations
                if self.arenas.is_empty() {
                    return Ok(Value::Int(0));
                }
                let arena_id = self.arenas.len() - 1;
                Ok(Value::Int(self.arenas[arena_id].slots.len() as i64))
            }
            // Standard library extensions
            "print" => {
                let parts: Vec<String> = args.iter().map(|v| v.to_string()).collect();
                print!("{}", parts.join(" "));
                Ok(Value::Unit)
            }
            "pow" => {
                if args.len() != 2 { return Err("pow expects 2 arguments (base, exp)".into()); }
                match (&args[0], &args[1]) {
                    (Value::Int(b), Value::Int(e)) => Ok(Value::Int(b.pow(*e as u32))),
                    (Value::Float(b), Value::Int(e)) => Ok(Value::Float(b.powf(*e as f64))),
                    (Value::Float(b), Value::Float(e)) => Ok(Value::Float(b.powf(*e))),
                    _ => Err("pow expects numbers".into()),
                }
            }
            "floor" => {
                if args.len() != 1 { return Err("floor expects 1 argument".into()); }
                match &args[0] {
                    Value::Float(v) => Ok(Value::Float(v.floor())),
                    Value::Int(v) => Ok(Value::Int(*v)),
                    _ => Err("floor expects a number".into()),
                }
            }
            "ceil" => {
                if args.len() != 1 { return Err("ceil expects 1 argument".into()); }
                match &args[0] {
                    Value::Float(v) => Ok(Value::Float(v.ceil())),
                    Value::Int(v) => Ok(Value::Int(*v)),
                    _ => Err("ceil expects a number".into()),
                }
            }
            "round" => {
                if args.len() != 1 { return Err("round expects 1 argument".into()); }
                match &args[0] {
                    Value::Float(v) => Ok(Value::Float(v.round())),
                    Value::Int(v) => Ok(Value::Int(*v)),
                    _ => Err("round expects a number".into()),
                }
            }
            "random" => {
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
            "pi" => {
                Ok(Value::Float(std::f64::consts::PI))
            }
            "read_file" => {
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
            "write_file" => {
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
            "file_exists" => {
                if args.len() != 1 { return Err("file_exists expects 1 argument".into()); }
                match &args[0] {
                    Value::String(path) => Ok(Value::Bool(std::path::Path::new(path).exists())),
                    _ => Err("file_exists expects a string path".into()),
                }
            }
            // ========== Time functions ==========
            "now" | "timestamp" => {
                if !args.is_empty() { return Err("now/timestamp expects 0 arguments".into()); }
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| format!("time error: {}", e))?
                    .as_secs() as i64;
                Ok(Value::Int(ts))
            }
            "now_ms" | "timestamp_ms" => {
                if !args.is_empty() { return Err("now_ms/timestamp_ms expects 0 arguments".into()); }
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| format!("time error: {}", e))?
                    .as_millis() as i64;
                Ok(Value::Int(ts))
            }
            "sleep" => {
                if args.len() != 1 { return Err("sleep expects 1 argument (milliseconds)".into()); }
                match &args[0] {
                    Value::Int(ms) => {
                        std::thread::sleep(std::time::Duration::from_millis(*ms as u64));
                        Ok(Value::Unit)
                    }
                    _ => Err("sleep expects an integer (milliseconds)".into()),
                }
            }
            // ========== Environment/CLI functions ==========
            "getenv" => {
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
            "args" => {
                if !args.is_empty() { return Err("args expects 0 arguments".into()); }
                let cli_args: Vec<Value> = std::env::args()
                    .skip(1) // skip program name
                    .map(|a| Value::String(a))
                    .collect();
                Ok(Value::List(cli_args))
            }
            // ========== JSON functions ==========
            "to_json" => {
                if args.len() != 1 { return Err("to_json expects 1 argument".into()); }
                let json_val = self.value_to_json(&args[0])?;
                let json_str = serde_json::to_string(&json_val)
                    .map_err(|e| format!("to_json error: {}", e))?;
                Ok(Value::String(json_str))
            }
            "from_json" => {
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
            "json_get_string" => {
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
            "json_get_int" => {
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
            "json_get_element" => {
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
            "str_char_at" => {
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
            "str_substring" => {
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
            "str_parse_int" => {
                if args.len() != 1 { return Err("str_parse_int expects 1 argument".into()); }
                match &args[0] {
                    Value::String(s) => Ok(s.trim().parse::<i64>()
                        .map(|n| Value::Tuple(vec![Value::Bool(true), Value::Int(n)]))
                        .unwrap_or_else(|_| Value::Tuple(vec![Value::Bool(false), Value::Int(0)]))),
                    _ => Err("str_parse_int expects a string".into()),
                }
            }
            "str_parse_float" => {
                if args.len() != 1 { return Err("str_parse_float expects 1 argument".into()); }
                match &args[0] {
                    Value::String(s) => Ok(s.trim().parse::<f64>()
                        .map(|n| Value::Tuple(vec![Value::Bool(true), Value::Float(n)]))
                        .unwrap_or_else(|_| Value::Tuple(vec![Value::Bool(false), Value::Float(0.0)]))),
                    _ => Err("str_parse_float expects a string".into()),
                }
            }
            // Additional string operations
            "str_split" => {
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
            "str_join" => {
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
            "str_trim" => {
                if args.len() != 1 { return Err("str_trim expects 1 argument".into()); }
                match &args[0] {
                    Value::String(s) => Ok(Value::String(s.trim().to_string())),
                    _ => Err("str_trim expects a string".into()),
                }
            }
            "str_starts_with" => {
                if args.len() != 2 { return Err("str_starts_with expects 2 arguments".into()); }
                match (&args[0], &args[1]) {
                    (Value::String(s), Value::String(prefix)) => {
                        Ok(Value::Bool(s.starts_with(prefix.as_str())))
                    }
                    _ => Err("str_starts_with expects (string, string)".into()),
                }
            }
            "str_ends_with" => {
                if args.len() != 2 { return Err("str_ends_with expects 2 arguments".into()); }
                match (&args[0], &args[1]) {
                    (Value::String(s), Value::String(suffix)) => {
                        Ok(Value::Bool(s.ends_with(suffix.as_str())))
                    }
                    _ => Err("str_ends_with expects (string, string)".into()),
                }
            }
            "str_replace" => {
                if args.len() != 3 { return Err("str_replace expects 3 arguments".into()); }
                match (&args[0], &args[1], &args[2]) {
                    (Value::String(s), Value::String(from), Value::String(to)) => {
                        Ok(Value::String(s.replace(from.as_str(), to.as_str())))
                    }
                    _ => Err("str_replace expects (string, string, string)".into()),
                }
            }
            "str_to_upper" => {
                if args.len() != 1 { return Err("str_to_upper expects 1 argument".into()); }
                match &args[0] {
                    Value::String(s) => Ok(Value::String(s.to_uppercase())),
                    _ => Err("str_to_upper expects a string".into()),
                }
            }
            "str_to_lower" => {
                if args.len() != 1 { return Err("str_to_lower expects 1 argument".into()); }
                match &args[0] {
                    Value::String(s) => Ok(Value::String(s.to_lowercase())),
                    _ => Err("str_to_lower expects a string".into()),
                }
            }
            "str_repeat" => {
                if args.len() != 2 { return Err("str_repeat expects 2 arguments".into()); }
                match (&args[0], &args[1]) {
                    (Value::String(s), Value::Int(n)) => {
                        if *n < 0 { return Err("str_repeat: count must be non-negative".into()); }
                        Ok(Value::String(s.repeat(*n as usize)))
                    }
                    _ => Err("str_repeat expects (string, int)".into()),
                }
            }
            "str_contains" => {
                if args.len() != 2 { return Err("str_contains expects 2 arguments".into()); }
                match (&args[0], &args[1]) {
                    (Value::String(s), Value::String(sub)) => {
                        Ok(Value::Bool(s.contains(sub.as_str())))
                    }
                    _ => Err("str_contains expects (string, string)".into()),
                }
            }
            "str_index_of" => {
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
            "keys" => {
                if args.len() != 1 { return Err("keys expects 1 argument (record)".into()); }
                match &args[0] {
                    Value::Record(_, fields) => {
                        let keys: Vec<Value> = fields.keys().map(|k| Value::String(k.clone())).collect();
                        Ok(Value::List(keys))
                    }
                    _ => Err("keys expects a record".into()),
                }
            }
            "values" => {
                if args.len() != 1 { return Err("values expects 1 argument (record)".into()); }
                match &args[0] {
                    Value::Record(_, fields) => {
                        Ok(Value::List(fields.values().cloned().collect()))
                    }
                    _ => Err("values expects a record".into()),
                }
            }
            "has_key" => {
                if args.len() != 2 { return Err("has_key expects 2 arguments (record, key)".into()); }
                match (&args[0], &args[1]) {
                    (Value::Record(_, fields), Value::String(key)) => {
                        Ok(Value::Bool(fields.contains_key(key.as_str())))
                    }
                    _ => Err("has_key expects (record, string)".into()),
                }
            }
            // Map operations (using Record as map)
            "map_new" => {
                Ok(Value::Record(None, std::collections::HashMap::new()))
            }
            "map_get" => {
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
            "map_set" => {
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
            "map_remove" => {
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
            "map_size" => {
                if args.len() != 1 { return Err("map_size expects 1 argument".into()); }
                match &args[0] {
                    Value::Record(_, fields) => Ok(Value::Int(fields.len() as i64)),
                    _ => Err("map_size expects a record".into()),
                }
            }
            "map_from_list" => {
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
            "to_int" => {
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
            "to_float" => {
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
            // MimiSpec runtime functions
            "lexer" => {
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
            "parse" => {
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
            "str_to_c_str" => {
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
            "c_str_to_string" => {
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
            _ => {
                // Check for pre-computed comptime function results
                if let Some(result) = self.comptime_results.get(name) {
                    return Ok(result.clone());
                }
                Err(format!("undefined function '{}'", name))
            }
        }
    }

    /// Call an async function - spawns a new thread and returns a Future
    fn call_async_func(&mut self, func: &FuncDef, args: Vec<Value>) -> Result<Value, String> {
        if func.params.len() != args.len() {
            return Err(format!(
                "function {} expects {} arguments, got {}",
                func.name,
                func.params.len(),
                args.len()
            ));
        }

        // Clone the function and arguments for the new thread
        let func_clone = func.clone();
        let args_clone = args;

        // Create a channel for the result
        let (tx, rx) = std::sync::mpsc::channel();

        // Spawn a new thread to execute the async function body directly
        super::pool::get_pool().execute(move || {
            let empty_file = crate::ast::File { imports: vec![], items: vec![] };
            let mut interp = Interpreter::new(&empty_file);
            interp.push_scope();
            for (p, a) in func_clone.params.iter().zip(args_clone) {
                interp.bind(&p.name, a);
            }
            let result = interp.eval_block(&func_clone.body).map(|v| v.unwrap_or(Value::Unit));
            interp.pop_scope();
            let _ = tx.send(result);
        });

        // Return a Future value
        Ok(Value::Future(std::sync::Arc::new(std::sync::Mutex::new(rx))))
    }

    pub(crate) fn call_method(&mut self, obj: &Value, method: &str, args: Vec<Value>) -> Result<Value, String> {
        match obj {
            Value::Shared(arc) => {
                match method {
                    "clone" => Ok(Value::Shared(Arc::clone(arc))),
                    "deref" | "inner" => {
                        let inner = arc.read().map_err(|e| format!("shared read lock failed: {}", e))?;
                        Ok(inner.clone())
                    }
                    _ => Err(format!("shared value has no method '{}'", method)),
                }
            }
            Value::LocalShared(rc) => {
                match method {
                    "clone" => Ok(Value::LocalShared(SendRc(Rc::clone(&rc.0)))),
                    "deref" | "inner" => {
                        let inner = rc.0.borrow();
                        Ok(inner.clone())
                    }
                    _ => Err(format!("local_shared value has no method '{}'", method)),
                }
            }
            Value::WeakShared(w) => {
                match method {
                    "upgrade" => {
                        match w.upgrade() {
                            Some(arc) => Ok(Value::Shared(arc)),
                            None => Ok(Value::Variant("None".into(), vec![])),
                        }
                    }
                    _ => Err(format!("weak_shared value has no method '{}'", method)),
                }
            }
            Value::WeakLocal(w) => {
                match method {
                    "upgrade" => {
                        match w.upgrade() {
                            Some(rc) => Ok(Value::LocalShared(rc)),
                            None => Ok(Value::Variant("None".into(), vec![])),
                        }
                    }
                    _ => Err(format!("weak_local value has no method '{}'", method)),
                }
            }
            Value::Cap(names) => {
                match method {
                    "split" => {
                        if names.len() < 2 {
                            return Err("split() requires a combined capability (cap A + B)".into());
                        }
                        let tuple: Vec<Value> = names.iter()
                            .map(|n| Value::Cap(vec![n.clone()]))
                            .collect();
                        Ok(Value::Tuple(tuple))
                    }
                    _ => Err(format!("cap value has no method '{}'", method)),
                }
            }
            Value::Actor(actor_arc) => {
                // Handle special methods
                match method {
                    "spawn" => {
                        // spawn() doesn't make sense on an instance - it's a constructor
                        Err("spawn() should be called on Actor type, not instance".into())
                    }
                    _ => {
                        // Get actor name and methods (clone from RwLock)
                        let actor_name: String;
                        let actor_methods: Vec<FuncDef>;
                        {
                            let actor = actor_arc.inner.read().map_err(|e| format!("actor lock failed: {}", e))?;
                            actor_name = actor.actor_name.clone();
                            actor_methods = actor.methods.clone();
                        }

                        // Find the method in the actor's methods
                        let func = actor_methods.iter()
                            .find(|f| f.name == method)
                            .ok_or_else(|| format!("actor {} has no method '{}'", actor_name, method))?;

                        // For actor methods, we need to call with self bound to this actor
                        self.push_scope();
                        // Bind 'self' to the actor handle itself (for self.field access via RwLock)
                        self.bind("self", obj.clone());

                        let result = self.call_func(func, args);

                        self.pop_scope();

                        result
                    }
                }
            }
            Value::Record(type_name, fields) => {
                // Handle built-in derive methods before trait dispatch
                match method {
                    "to_string" => {
                        let type_label = type_name.as_deref().unwrap_or("Record");
                        let field_strs: Vec<String> = fields.iter()
                            .map(|(k, v)| format!("{}: {}", k, self.value_to_debug_string(v)))
                            .collect();
                        return Ok(Value::String(format!("{} {{ {} }}", type_label, field_strs.join(", "))));
                    }
                    "clone" => {
                        return Ok(obj.clone());
                    }
                    "eq" => {
                        if let Some(other) = args.first() {
                            let equal = self.values_equal(obj, other);
                            return Ok(Value::Bool(equal));
                        }
                        return Ok(Value::Bool(false));
                    }
                    _ => {}
                }
                // Try trait method dispatch
                if let Some(type_name) = type_name {
                    if let Some(impls) = self.type_impls.get(type_name) {
                        for methods in impls.values() {
                            if let Some(func) = methods.iter().find(|f| f.name == method) {
                                let func = func.clone();
                                let fields = fields.clone();
                                // Found trait method - call it with self = the record
                                self.push_scope();
                                self.bind("self", obj.clone());
                                // Bind record fields to scope
                                for (field_name, field_value) in &fields {
                                    self.bind(field_name, field_value.clone());
                                }
                                let result = self.call_func(&func, args);
                                self.pop_scope();
                                return result;
                            }
                        }
                    }
                }
                // Try built-in methods on records
                match method {
                    "fields" => {
                        let field_list: Vec<Value> = fields.values().cloned().collect();
                        Ok(Value::List(field_list))
                    }
                    _ => Err(format!("cannot call method '{}' on record", method)),
                }
            }
            Value::String(s) => {
                match method {
                    "len" => Ok(Value::Int(s.chars().count() as i64)),
                    "trim" => Ok(Value::String(s.trim().to_string())),
                    "to_upper" => Ok(Value::String(s.to_uppercase())),
                    "to_lower" => Ok(Value::String(s.to_lowercase())),
                    "parse_int" => {
                        Ok(Value::Int(s.trim().parse::<i64>().map_err(|e| format!("parse_int: {}", e))?))
                    }
                    "parse_float" => {
                        Ok(Value::Float(s.trim().parse::<f64>().map_err(|e| format!("parse_float: {}", e))?))
                    }
                    "contains" => {
                        let substr = args.into_iter().next().ok_or("contains expects 1 argument")?;
                        match substr {
                            Value::String(p) => Ok(Value::Bool(s.contains(&p))),
                            _ => Err("contains expects a string argument".into()),
                        }
                    }
                    "starts_with" => {
                        let substr = args.into_iter().next().ok_or("starts_with expects 1 argument")?;
                        match substr {
                            Value::String(p) => Ok(Value::Bool(s.starts_with(&p))),
                            _ => Err("starts_with expects a string argument".into()),
                        }
                    }
                    "ends_with" => {
                        let substr = args.into_iter().next().ok_or("ends_with expects 1 argument")?;
                        match substr {
                            Value::String(p) => Ok(Value::Bool(s.ends_with(&p))),
                            _ => Err("ends_with expects a string argument".into()),
                        }
                    }
                    "split" => {
                        let delim = args.into_iter().next().ok_or("split expects 1 argument")?;
                        match delim {
                            Value::String(d) => {
                                let parts: Vec<Value> = s.split(&d).map(|p| Value::String(p.to_string())).collect();
                                Ok(Value::List(parts))
                            }
                            _ => Err("split expects a string argument".into()),
                        }
                    }
                    "replace" => {
                        if args.len() != 2 {
                            return Err("replace expects 2 arguments (old, new)".into());
                        }
                        let (old, new) = (args[0].clone(), args[1].clone());
                        match (old, new) {
                            (Value::String(old_s), Value::String(new_s)) => {
                                Ok(Value::String(s.replace(&old_s, &new_s)))
                            }
                            _ => Err("replace expects string arguments".into()),
                        }
                    }
                    "repeat" => {
                        let count = args.into_iter().next().ok_or("repeat expects 1 argument")?;
                        match count {
                            Value::Int(n) => {
                                if n < 0 { return Err("repeat: count must be non-negative".into()); }
                                Ok(Value::String(s.repeat(n as usize)))
                            }
                            _ => Err("repeat expects an integer argument".into()),
                        }
                    }
                    "char_at" => {
                        let idx = args.into_iter().next().ok_or("char_at expects 1 argument")?;
                        match idx {
                            Value::Int(i) => {
                                let ch = s.chars().nth(i as usize)
                                    .ok_or_else(|| format!("char_at: index {} out of bounds (len {})", i, s.chars().count()))?;
                                Ok(Value::String(ch.to_string()))
                            }
                            _ => Err("char_at expects an integer argument".into()),
                        }
                    }
                    "substring" => {
                        if args.len() != 2 {
                            return Err("substring expects 2 arguments (start, end)".into());
                        }
                        let (start, end) = (args[0].clone(), args[1].clone());
                        match (start, end) {
                            (Value::Int(si), Value::Int(ei)) => {
                                if si > ei {
                                    return Err("substring: start > end".into());
                                }
                                let chars: Vec<char> = s.chars().collect();
                                let si = si as usize;
                                let ei = ei as usize;
                                if ei > chars.len() {
                                    return Err(format!("substring: end {} out of bounds (len {})", ei, chars.len()));
                                }
                                let sub: String = chars[si..ei].iter().collect();
                                Ok(Value::String(sub))
                            }
                            _ => Err("substring expects integer arguments".into()),
                        }
                    }
                    "index_of" => {
                        let substr = args.into_iter().next().ok_or("index_of expects 1 argument")?;
                        match substr {
                            Value::String(p) => {
                                match s.find(&p) {
                                    Some(i) => Ok(Value::Int(i as i64)),
                                    None => Ok(Value::Int(-1)),
                                }
                            }
                            _ => Err("index_of expects a string argument".into()),
                        }
                    }
                    _ => Err(format!("string has no method '{}'", method)),
                }
            }
            Value::List(list) => {
                match method {
                    "len" => Ok(Value::Int(list.len() as i64)),
                    _ => Err(format!("List has no method '{}'", method)),
                }
            }
            Value::Variant(name, vals) => {
                // Option/Result combinator methods on enum variants
                match (name.as_str(), method) {
                    // ===== Option methods =====
                    ("Some" | "Ok", "unwrap") | ("Some" | "Ok", "expect") => {
                        if vals.is_empty() {
                            Err(format!("{}::{} has no inner value", name, method))
                        } else {
                            Ok(vals[0].clone())
                        }
                    }
                    ("None", "unwrap") => Err("called unwrap() on None".into()),
                    ("None", "expect") => {
                        let msg = if args.is_empty() { "called expect() on None" } else { &args[0].to_string() };
                        Err(format!("{}", msg))
                    }
                    ("Err", "unwrap") | ("Err", "expect") => {
                        let msg = if vals.is_empty() { "called unwrap() on Err".to_string() } else { format!("called unwrap() on Err({})", vals[0]) };
                        Err(msg)
                    }

                    ("Some", "unwrap_or") | ("Ok", "unwrap_or") => {
                        Ok(vals[0].clone())
                    }
                    ("None", "unwrap_or") | ("Err", "unwrap_or") => {
                        args.into_iter().next().ok_or("unwrap_or requires a default value".to_string())
                    }

                    ("Some", "is_some") | ("Ok", "is_some") | ("Some", "is_ok") | ("Ok", "is_ok") => {
                        Ok(Value::Bool(true))
                    }
                    ("None", "is_some") | ("Err", "is_some") | ("None", "is_ok") | ("Err", "is_ok") => {
                        Ok(Value::Bool(false))
                    }
                    ("None", "is_none") | ("Err", "is_none") | ("None", "is_err") | ("Err", "is_err") => {
                        Ok(Value::Bool(true))
                    }
                    ("Some", "is_none") | ("Ok", "is_none") | ("Some", "is_err") | ("Ok", "is_err") => {
                        Ok(Value::Bool(false))
                    }

                    // ok_or: Option -> Result
                    ("Some", "ok_or") => {
                        Ok(Value::Variant("Ok".into(), vec![vals[0].clone()]))
                    }
                    ("None", "ok_or") => {
                        let err = args.into_iter().next().ok_or("ok_or requires an error value")?;
                        Ok(Value::Variant("Err".into(), vec![err]))
                    }

                    // map: apply closure to inner value
                    ("Some", "map") => {
                        let closure = args.into_iter().next().ok_or("map requires a function argument")?;
                        let mapped = self.apply_closure(&closure, vec![vals[0].clone()])?;
                        Ok(Value::Variant("Some".into(), vec![mapped]))
                    }
                    ("None", "map") => Ok(Value::Variant("None".into(), vec![])),
                    ("Ok", "map") => {
                        let closure = args.into_iter().next().ok_or("map requires a function argument")?;
                        let mapped = self.apply_closure(&closure, vec![vals[0].clone()])?;
                        Ok(Value::Variant("Ok".into(), vec![mapped]))
                    }
                    ("Err", "map") => Ok(obj.clone()),

                    // and_then: apply closure returning same variant type
                    ("Some", "and_then") => {
                        let closure = args.into_iter().next().ok_or("and_then requires a function argument")?;
                        self.apply_closure(&closure, vec![vals[0].clone()])
                    }
                    ("None", "and_then") => Ok(Value::Variant("None".into(), vec![])),
                    ("Ok", "and_then") => {
                        let closure = args.into_iter().next().ok_or("and_then requires a function argument")?;
                        self.apply_closure(&closure, vec![vals[0].clone()])
                    }
                    ("Err", "and_then") => Ok(obj.clone()),

                    // map_err: apply closure to error value
                    ("Ok", "map_err") => Ok(obj.clone()),
                    ("Err", "map_err") => {
                        let closure = args.into_iter().next().ok_or("map_err requires a function argument")?;
                        let err_val = if vals.is_empty() { Value::Unit } else { vals[0].clone() };
                        let mapped = self.apply_closure(&closure, vec![err_val])?;
                        Ok(Value::Variant("Err".into(), vec![mapped]))
                    }
                    ("Some", "map_err") => Ok(obj.clone()),
                    ("None", "map_err") => Ok(Value::Variant("None".into(), vec![])),

                    _ => Err(format!("variant '{}' has no method '{}'", name, method)),
                }
            }
            _ => Err(format!("cannot call method '{}' on value {}", method, obj)),
        }
    }

    /// Apply a closure value to arguments
    fn apply_closure(&mut self, closure: &Value, args: Vec<Value>) -> Result<Value, String> {
        match closure {
            Value::Closure { params, body, captured, .. } => {
                if params.len() != args.len() {
                    return Err(format!("closure expects {} arguments, got {}", params.len(), args.len()));
                }
                self.push_scope();
                for (n, v) in captured {
                    self.bind(n, v.clone());
                }
                for (param, arg) in params.iter().zip(args) {
                    self.bind(&param.name, arg);
                }
                let result = self.eval_block(body)?;
                self.pop_scope();
                Ok(result.unwrap_or(Value::Unit))
            }
            _ => Err(format!("expected a closure, found {}", closure)),
        }
    }

    /// Call an extern function via FFI
    ///
    /// Phase 0 FFI safety: only scalar types (i32/i64/f64/bool) and borrowed
    /// strings are allowed to cross the C ABI boundary directly. Complex Mimi
    /// objects such as shared, borrowed references, records, lists, closures,
    /// etc. must be explicitly converted to passport types before being passed
    /// to extern functions.
    fn call_extern(
        &mut self,
        extern_func: &ExternFunc,
        contract: &FfiContract,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        // Stage 2 wrapper layer: validate and convert arguments according to the
        // FFI contract before loading any shared library.  This keeps the
        // interpreter FFI path aligned with the codegen wrapper path.
        if contract.args.len() != args.len() {
            return Err(format!(
                "FFI wrapper: extern function '{}' expects {} arguments, got {}",
                extern_func.name,
                contract.args.len(),
                args.len()
            ));
        }

        // Stage 4: Check precondition (requires) before the C call
        if self.verify_ffi {
            if let Some(requires_expr) = &contract.requires {
                let result = self.eval_expr(requires_expr);
                match result {
                    Ok(Value::Bool(true)) => { /* precondition holds */ }
                    Ok(Value::Bool(false)) => {
                        return Err(format!(
                            "FFI contract violation: precondition of '{}' failed",
                            extern_func.name
                        ));
                    }
                    Ok(other) => {
                        return Err(format!(
                            "FFI contract error: precondition of '{}' must evaluate to bool, got {}",
                            extern_func.name, other
                        ));
                    }
                    Err(e) => {
                        return Err(format!(
                            "FFI contract error: failed to evaluate precondition of '{}': {}",
                            extern_func.name, e
                        ));
                    }
                }
            }
        }

        let mut c_args: Vec<i64> = Vec::with_capacity(args.len());
        let mut _string_guards: Vec<std::ffi::CString> = Vec::new();
        let mut _shared_handles: Vec<std::sync::Arc<crate::ffi::runtime::SharedHandle>> = Vec::new();
        let mut _borrow_guards_read: Vec<Box<dyn std::any::Any>> = Vec::new();
        let mut _borrow_guards_write: Vec<Box<dyn std::any::Any>> = Vec::new();
        for (arg, arg_contract) in args.iter().zip(&contract.args) {
            let c_arg = self.value_to_ffi_arg(
                arg,
                arg_contract,
                &mut _string_guards,
                &mut _shared_handles,
                &mut _borrow_guards_read,
                &mut _borrow_guards_write,
            )?;
            c_args.push(c_arg);
        }

        let lib_path = std::env::var("MIMI_FFI_LIB")
            .map_err(|_| "MIMI_FFI_LIB environment variable not set for extern function call".to_string())?;

        // Load library if not already loaded
        let lib_idx = if let Some(idx) = self.loaded_libs.iter().position(|l| {
            format!("{:?}", l) == format!("Library({})", lib_path)
        }) {
            idx
        } else {
            unsafe {
                let lib = libloading::Library::new(&lib_path)
                    .map_err(|e| format!("failed to load library '{}': {}", lib_path, e))?;
                self.loaded_libs.push(lib);
                self.loaded_libs.len() - 1
            }
        };

        let func_name = extern_func.name.clone();

        // Call the function via libloading
        let result = unsafe {
            // Clear errno before call to avoid stale errno
            if contract.check_errno {
                *libc::__errno_location() = 0;
            }
            let lib = &self.loaded_libs[lib_idx];
            type CFunc = unsafe extern "C" fn(i64, i64, i64, i64, i64, i64, i64, i64) -> i64;
            let symbol: libloading::Symbol<CFunc> = lib.get(func_name.as_bytes())
                .map_err(|e| format!("failed to find symbol '{}': {}", func_name, e))?;

            // Call with up to 8 args (zeroed if fewer)
            let mut raw_args = [0i64; 8];
            for (i, &a) in c_args.iter().enumerate().take(8) {
                raw_args[i] = a;
            }
            symbol(raw_args[0], raw_args[1], raw_args[2], raw_args[3],
                   raw_args[4], raw_args[5], raw_args[6], raw_args[7])
        };

        // Priority 2: Capture errno after C call if enabled
        let errno_value = if contract.check_errno {
            Some(unsafe { *libc::__errno_location() })
        } else {
            None
        };

        let return_value = self.ffi_ret_to_value(result, &contract.ret)?;

        // Stage 4: Check postcondition (ensures) after the C call
        if self.verify_ffi {
            if let Some(ensures_expr) = &contract.ensures {
                // Bind 'result' to the return value for ensures evaluation
                // Note: The eval_expr method doesn't support scope binding directly,
                // so we use a simpler approach - just evaluate the expression
                // A more complete implementation would inject 'result' into the scope
                let eval_result = self.eval_expr(ensures_expr);
                match eval_result {
                    Ok(Value::Bool(true)) => { /* postcondition holds */ }
                    Ok(Value::Bool(false)) => {
                        return Err(format!(
                            "FFI contract violation: postcondition of '{}' failed",
                            extern_func.name
                        ));
                    }
                    Ok(other) => {
                        return Err(format!(
                            "FFI contract error: postcondition of '{}' must evaluate to bool, got {}",
                            extern_func.name, other
                        ));
                    }
                    Err(e) => {
                        return Err(format!(
                            "FFI contract error: failed to evaluate postcondition of '{}': {}",
                            extern_func.name, e
                        ));
                    }
                }
            }
        }

        // Priority 2: Map errno to Result if enabled
        if let Some(errno) = errno_value {
            if errno != 0 {
                // Create an Err result with errno information
                let errno_name = match errno {
                    1 => "EPERM",
                    2 => "ENOENT",
                    3 => "ESRCH",
                    4 => "EINTR",
                    5 => "EIO",
                    6 => "ENXIO",
                    7 => "E2BIG",
                    8 => "ENOEXEC",
                    9 => "EBADF",
                    10 => "ECHILD",
                    11 => "EAGAIN",
                    12 => "ENOMEM",
                    13 => "EACCES",
                    14 => "EFAULT",
                    16 => "EBUSY",
                    17 => "EEXIST",
                    18 => "EXDEV",
                    19 => "ENODEV",
                    20 => "ENOTDIR",
                    21 => "EISDIR",
                    22 => "EINVAL",
                    23 => "ENFILE",
                    24 => "EMFILE",
                    25 => "ENOTTY",
                    26 => "ETXTBSY",
                    27 => "EFBIG",
                    28 => "ENOSPC",
                    29 => "ESPIPE",
                    30 => "EROFS",
                    32 => "EPIPE",
                    34 => "EDOM",
                    36 => "ERANGE",
                    38 => "ENOSYS",
                    39 => "ENOTEMPTY",
                    97 => "EAFNOSUPPORT",
                    98 => "EADDRINUSE",
                    99 => "EADDRNOTAVAIL",
                    101 => "ENETUNREACH",
                    104 => "ECONNRESET",
                    110 => "ETIMEDOUT",
                    111 => "ECONNREFUSED",
                    113 => "EHOSTUNREACH",
                    _ => "UNKNOWN",
                };
                return Err(format!(
                    "FFI errno: {} (code {})",
                    errno_name, errno
                ));
            }
        }

        Ok(return_value)
    }

    /// Convert a single Mimi value into a C ABI argument according to the
    /// argument's FFI contract.
    fn value_to_ffi_arg(
        &self,
        arg: &Value,
        contract: &FfiArgContract,
        string_guards: &mut Vec<std::ffi::CString>,
        _shared_handles: &mut Vec<std::sync::Arc<crate::ffi::runtime::SharedHandle>>,
        _borrow_guards_read: &mut Vec<Box<dyn std::any::Any>>,
        _borrow_guards_write: &mut Vec<Box<dyn std::any::Any>>,
    ) -> Result<i64, String> {
        match contract {
            FfiArgContract::Int => match arg {
                Value::Int(n) => Ok(*n),
                Value::Bool(b) => Ok(*b as i64),
                other => Err(format!(
                    "FFI wrapper: expected scalar integer/bool argument, found {}",
                    other
                )),
            },
            FfiArgContract::Float => match arg {
                Value::Float(f) => Ok(f.to_bits() as i64),
                Value::Int(n) => Ok((*n as f64).to_bits() as i64),
                other => Err(format!(
                    "FFI wrapper: expected f64 argument, found {}",
                    other
                )),
            },
            FfiArgContract::StringBorrow => match arg {
                Value::String(s) => {
                    let c_str = std::ffi::CString::new(s.as_str())
                        .map_err(|e| format!("failed to convert string to C string: {}", e))?;
                    let ptr = c_str.as_ptr() as i64;
                    string_guards.push(c_str); // keep the CString alive during the C call
                    Ok(ptr)
                }
                other => Err(format!(
                    "FFI wrapper: expected string argument, found {}",
                    other
                )),
            },
            FfiArgContract::StringTransfer => match arg {
                Value::String(s) => {
                    // Transfer ownership: create a CString that C must free
                    let c_str = std::ffi::CString::new(s.as_str())
                        .map_err(|e| format!("failed to convert string to C string: {}", e))?;
                    // Convert to raw pointer - C is now responsible for freeing
                    let ptr = c_str.into_raw() as i64;
                    Ok(ptr)
                }
                other => Err(format!(
                    "FFI wrapper: expected string argument for ownership transfer, found {}",
                    other
                )),
            },
            FfiArgContract::Cap => match arg {
                Value::Cap(names) => {
                    // Register the cap in the CapTable and return its ID
                    let cap_name = names.first().unwrap_or(&String::new()).clone();
                    let cap_id = CAP_TABLE.register(&cap_name);
                    Ok(cap_id)
                }
                other => Err(format!(
                    "FFI safety: expected cap argument, found {}",
                    other
                )),
            },
            FfiArgContract::Unsupported(ty) => {
                // Runtime fallback for declarations that bypass the type checker.
                // Preserve the old Phase 0 error messages for the common unsafe
                // Mimi value categories.
                Err(self.unsupported_ffi_arg_error(arg, ty))
            }
            FfiArgContract::RawPtr(_) => match arg {
                // *T: immutable raw pointer
                Value::Shared(arc) => {
                    // Create a handle to keep the shared value alive
                    let handle_id = SHARED_TABLE.create(Arc::clone(arc));
                    // Get a pointer to the inner value
                    if let Some(handle) = SHARED_TABLE.get(handle_id) {
                        let ptr = handle.as_ptr() as *const () as i64;
                        Ok(ptr)
                    } else {
                        Err("FFI wrapper: failed to create shared handle for raw pointer".to_string())
                    }
                }
                Value::Ref(rc) => {
                    let borrow = rc.borrow();
                    let ptr = &*borrow as *const Value as *const () as i64;
                    std::mem::forget(borrow);
                    Ok(ptr)
                }
                Value::Int(n) => Ok(*n),
                other => Err(format!(
                    "FFI wrapper: raw pointer argument must be a shared value, reference, or opaque handle, found {}",
                    other
                )),
            },
            FfiArgContract::RawPtrMut(_) => match arg {
                // *mut T: mutable raw pointer
                Value::Shared(arc) => {
                    // Create a handle to keep the shared value alive
                    let handle_id = SHARED_TABLE.create(Arc::clone(arc));
                    // Get a mutable pointer to the inner value
                    if let Some(handle) = SHARED_TABLE.get(handle_id) {
                        let ptr = handle.as_mut_ptr() as *mut () as i64;
                        Ok(ptr)
                    } else {
                        Err("FFI wrapper: failed to create shared handle for mutable raw pointer".to_string())
                    }
                }
                Value::RefMut(rc) => {
                    let mut borrow = rc.borrow_mut();
                    let ptr = &mut *borrow as *mut Value as *mut () as i64;
                    std::mem::forget(borrow);
                    Ok(ptr)
                }
                Value::Int(n) => Ok(*n),
                other => Err(format!(
                    "FFI wrapper: mutable raw pointer argument must be a shared value, mutable reference, or opaque handle, found {}",
                    other
                )),
            },
            FfiArgContract::CShared(_) => match arg {
                // c_shared T: create a handle in SHARED_TABLE and return the handle ID
                Value::Shared(arc) => {
                    let handle_id = SHARED_TABLE.create(Arc::clone(arc));
                    Ok(handle_id)
                }
                Value::LocalShared(_rc) => {
                    // Convert LocalShared to Shared for handle creation
                    // Note: This is a limitation - LocalShared cannot be directly used with SharedHandleTable
                    // For now, return an error
                    Err("FFI wrapper: c_shared does not support local_shared values yet. Use shared instead.".to_string())
                }
                Value::Int(n) => {
                    // Already an opaque handle (from previous conversion)
                    Ok(*n)
                }
                other => Err(format!(
                    "FFI wrapper: c_shared argument must be a shared value or opaque handle, found {}",
                    other
                )),
            },
            FfiArgContract::CBorrow(_) => match arg {
                // c_borrow T: create a handle and return a pointer to the inner value
                Value::Shared(arc) => {
                    // Create a handle to keep the shared value alive
                    let handle_id = SHARED_TABLE.create(Arc::clone(arc));
                    // Get a pointer to the inner value
                    if let Some(handle) = SHARED_TABLE.get(handle_id) {
                        let ptr = handle.as_ptr() as *const () as i64;
                        Ok(ptr)
                    } else {
                        Err("FFI wrapper: failed to create shared handle for c_borrow".to_string())
                    }
                }
                Value::Ref(rc) => {
                    let borrow = rc.borrow();
                    let ptr = &*borrow as *const Value as *const () as i64;
                    std::mem::forget(borrow);
                    Ok(ptr)
                }
                Value::Int(n) => {
                    // Already an opaque handle
                    Ok(*n)
                }
                other => Err(format!(
                    "FFI wrapper: c_borrow argument must be a shared value, reference, or opaque handle, found {}",
                    other
                )),
            },
            FfiArgContract::CBorrowMut(_) => match arg {
                // c_borrow_mut T: create a handle and return a mutable pointer to the inner value
                Value::Shared(arc) => {
                    // Create a handle to keep the shared value alive
                    let handle_id = SHARED_TABLE.create(Arc::clone(arc));
                    // Get a mutable pointer to the inner value
                    if let Some(handle) = SHARED_TABLE.get(handle_id) {
                        let ptr = handle.as_mut_ptr() as *mut () as i64;
                        Ok(ptr)
                    } else {
                        Err("FFI wrapper: failed to create shared handle for c_borrow_mut".to_string())
                    }
                }
                Value::RefMut(rc) => {
                    let mut borrow = rc.borrow_mut();
                    let ptr = &mut *borrow as *mut Value as *mut () as i64;
                    std::mem::forget(borrow);
                    Ok(ptr)
                }
                Value::Int(n) => {
                    // Already an opaque handle
                    Ok(*n)
                }
                other => Err(format!(
                    "FFI wrapper: c_borrow_mut argument must be a shared value, mutable reference, or opaque handle, found {}",
                    other
                )),
            },
        }
    }

    /// Convert the raw i64 returned by a C function into a Mimi value according
    /// to the return-value contract.
    fn ffi_ret_to_value(&self, result: i64, contract: &FfiRetContract) -> Result<Value, String> {
        match contract {
            FfiRetContract::Unit => Ok(Value::Unit),
            FfiRetContract::Int => Ok(Value::Int(result)),
            FfiRetContract::Float => Ok(Value::Float(f64::from_bits(result as u64))),
            FfiRetContract::String => {
                if result == 0 {
                    Ok(Value::String(String::new()))
                } else {
                    let c_str = unsafe { std::ffi::CStr::from_ptr(result as *const i8) };
                    Ok(Value::String(c_str.to_string_lossy().into_owned()))
                }
            }
            FfiRetContract::RawPtr(_)
            | FfiRetContract::RawPtrMut(_)
            | FfiRetContract::CShared(_)
            | FfiRetContract::CBorrow(_)
            | FfiRetContract::CBorrowMut(_) => {
                // Passport pointers/handles are returned as opaque integers for now.
                Ok(Value::Int(result))
            }
            FfiRetContract::Unsupported(ty) => Err(format!(
                "FFI safety: extern function declared with unsupported return type '{}'",
                ty
            )),
        }
    }

    /// Produce a Phase-0-compatible error for Mimi values that cannot cross the
    /// C ABI boundary.  Used when an extern declaration bypassed the type
    /// checker (e.g. in tests that call run_source_result directly).
    fn unsupported_ffi_arg_error(&self, arg: &Value, _ty: &str) -> String {
        match arg {
            Value::Shared(_) | Value::LocalShared(_) | Value::WeakShared(_) | Value::WeakLocal(_) => {
                format!(
                    "FFI safety: cannot pass shared value '{}' directly to extern function. \
                     Use a passport type such as c_shared T or c_borrow T instead.",
                    arg
                )
            }
            Value::Ref(_) | Value::RefMut(_) => {
                format!(
                    "FFI safety: cannot pass borrowed reference '{}' directly to extern function. \
                     Use a passport type such as c_borrow T or c_borrow_mut T instead.",
                    arg
                )
            }
            Value::Cap(_) => {
                "FFI safety: cap cannot be passed directly to extern functions yet. \
                 Cap cross-boundary authentication (via a runtime CapTable) is planned for Phase 3."
                    .to_string()
            }
            Value::Record(_, _) | Value::Variant(_, _) | Value::List(_) | Value::Tuple(_) => {
                format!(
                    "FFI safety: unsupported argument type '{}' for extern function call. \
                     Only scalar types (i32/i64/f64/bool) and borrowed strings are allowed. \
                     Complex Mimi values must be converted to passport types (c_shared T, \
                     c_borrow T, c_borrow_mut T, *T, *mut T) before crossing the FFI boundary.",
                    arg
                )
            }
            other => {
                format!(
                    "FFI safety: unsupported argument type '{}' for extern function call. \
                     Only scalar types (i32/i64/f64/bool) and borrowed strings are allowed. \
                     Complex Mimi values must be converted to passport types (c_shared T, \
                     c_borrow T, c_borrow_mut T, *T, *mut T) before crossing the FFI boundary.",
                    other
                )
            }
        }
    }

    fn value_to_json(&self, v: &Value) -> Result<serde_json::Value, String> {
        match v {
            Value::Int(n) => Ok(serde_json::Value::Number((*n).into())),
            Value::Float(f) => {
                let n = serde_json::Number::from_f64(*f)
                    .ok_or_else(|| format!("float {} cannot be represented in JSON", f))?;
                Ok(serde_json::Value::Number(n))
            }
            Value::Bool(b) => Ok(serde_json::Value::Bool(*b)),
            Value::String(s) => Ok(serde_json::Value::String(s.clone())),
            Value::Unit => Ok(serde_json::Value::Null),
            Value::List(items) => {
                let arr: Result<Vec<_>, _> = items.iter().map(|i| self.value_to_json(i)).collect();
                Ok(serde_json::Value::Array(arr?))
            }
            Value::Record(_, fields) => {
                let mut map = serde_json::Map::new();
                for (k, v) in fields {
                    map.insert(k.clone(), self.value_to_json(v)?);
                }
                Ok(serde_json::Value::Object(map))
            }
            Value::Tuple(items) => {
                let arr: Result<Vec<_>, _> = items.iter().map(|i| self.value_to_json(i)).collect();
                Ok(serde_json::Value::Array(arr?))
            }
            Value::Variant(name, payload) => {
                if payload.is_empty() {
                    Ok(serde_json::Value::String(name.clone()))
                } else {
                    let arr: Result<Vec<_>, _> = payload.iter().map(|i| self.value_to_json(i)).collect();
                    let mut map = serde_json::Map::new();
                    map.insert(name.clone(), serde_json::Value::Array(arr?));
                    Ok(serde_json::Value::Object(map))
                }
            }
            _ => Ok(serde_json::Value::String(format!("{}", v))),
        }
    }

    fn json_to_value(&self, jv: &serde_json::Value) -> Value {
        match jv {
            serde_json::Value::Null => Value::Unit,
            serde_json::Value::Bool(b) => Value::Bool(*b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Value::Int(i)
                } else if let Some(f) = n.as_f64() {
                    Value::Float(f)
                } else {
                    Value::Unit
                }
            }
            serde_json::Value::String(s) => Value::String(s.clone()),
            serde_json::Value::Array(arr) => {
                Value::List(arr.iter().map(|v| self.json_to_value(v)).collect())
            }
            serde_json::Value::Object(map) => {
                let fields: HashMap<String, Value> = map.iter()
                    .map(|(k, v)| (k.clone(), self.json_to_value(v)))
                    .collect();
                Value::Record(None, fields)
            }
        }
    }

    fn value_to_debug_string(&self, v: &Value) -> String {
        match v {
            Value::Int(n) => format!("{}", n),
            Value::Float(f) => format!("{}", f),
            Value::Bool(b) => format!("{}", b),
            Value::String(s) => format!("\"{}\"", s),
            Value::Record(type_name, fields) => {
                let name = type_name.as_deref().unwrap_or("Record");
                let fs: Vec<String> = fields.iter()
                    .map(|(k, v)| format!("{}: {}", k, self.value_to_debug_string(v)))
                    .collect();
                format!("{} {{ {} }}", name, fs.join(", "))
            }
            Value::Variant(name, args) => {
                if args.is_empty() {
                    name.clone()
                } else {
                    let as_: Vec<String> = args.iter().map(|a| self.value_to_debug_string(a)).collect();
                    format!("{}({})", name, as_.join(", "))
                }
            }
            Value::List(items) => {
                let is_: Vec<String> = items.iter().map(|i| self.value_to_debug_string(i)).collect();
                format!("[{}]", is_.join(", "))
            }
            Value::Tuple(items) => {
                let ts: Vec<String> = items.iter().map(|i| self.value_to_debug_string(i)).collect();
                format!("({})", ts.join(", "))
            }
            Value::Unit => "unit".to_string(),
            _ => format!("{:?}", v),
        }
    }

    fn values_equal(&self, a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Unit, Value::Unit) => true,
            (Value::Record(n1, f1), Value::Record(n2, f2)) => {
                if n1 != n2 || f1.len() != f2.len() {
                    return false;
                }
                f1.iter().all(|(k, v)| {
                    if let Some(v2) = f2.get(k) {
                        self.values_equal(v, v2)
                    } else {
                        false
                    }
                })
            }
            (Value::Variant(n1, a1), Value::Variant(n2, a2)) => {
                n1 == n2 && a1.len() == a2.len()
                    && a1.iter().zip(a2.iter()).all(|(a, b)| self.values_equal(a, b))
            }
            (Value::List(a), Value::List(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| self.values_equal(x, y))
            }
            (Value::Tuple(a), Value::Tuple(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| self.values_equal(x, y))
            }
            _ => false,
        }
    }
}
