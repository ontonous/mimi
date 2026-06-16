use super::*;

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
                if let Stmt::Requires(expr) = stmt {
                    let cond = self.eval_expr(expr)?;
                    if !is_truthy(&cond) {
                        self.pop_scope();
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
                        if let Stmt::Ensures(expr) = stmt {
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
                    return Err(e);
                }
            }
        }

        self.pop_scope();
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

        // Handle extern function calls
        if let Some(extern_func) = self.extern_funcs.get(name).cloned() {
            return self.call_extern(&extern_func, args);
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
                    _ => Err("len expects a string, list, array, or slice".into()),
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
                    _ => Err("push first argument must be a list".into()),
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
                        // Return (popped, new_list) as a tuple
                        Ok(Value::Tuple(vec![popped, Value::List(new_list)]))
                    }
                    _ => Err("pop expects a list".into()),
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
                            _ => Err("contains on string expects a string needle".into()),
                        }
                    }
                    _ => Err("contains expects a list or string".into()),
                }
            }
            "input" => {
                use std::io::{self, BufRead};
                let mut line = String::new();
                io::stdin().lock().read_line(&mut line).map_err(|e| format!("input error: {}", e))?;
                // Remove trailing newline
                if line.ends_with('\n') {
                    line.pop();
                }
                if line.ends_with('\r') {
                    line.pop();
                }
                Ok(Value::String(line))
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
            _ => {
                // Check for pre-computed comptime function results
                if let Some(result) = self.comptime_results.get(name) {
                    return Ok(result.clone());
                }
                Err(format!("undefined function '{}'", name))
            }
        }
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
                        // First, get a clone of the actor's current state
                        let actor_name: String;
                        let actor_fields: HashMap<String, Value>;
                        let actor_methods: Vec<FuncDef>;
                        {
                            let actor = actor_arc.inner.read().map_err(|e| format!("actor lock failed: {}", e))?;
                            actor_name = actor.actor_name.clone();
                            actor_fields = actor.fields.clone();
                            actor_methods = actor.methods.clone();
                        }

                        // Find the method in the actor's methods
                        let func = actor_methods.iter()
                            .find(|f| f.name == method)
                            .ok_or_else(|| format!("actor {} has no method '{}'", actor_name, method))?;

                        // For actor methods, we need to call with self bound to this actor
                        self.push_scope();
                        // Bind 'self' to the actor handle itself (for self.field = ... access)
                        self.bind("self", obj.clone());
                        // Also bind all actor fields to scope (for direct field access)
                        for (field_name, field_value) in &actor_fields {
                            self.bind(field_name, field_value.clone());
                        }

                        let result = self.call_func(func, args);

                        self.pop_scope();

                        result
                    }
                }
            }
            Value::Record(type_name, fields) => {
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
            _ => Err(format!("cannot call method '{}' on value {}", method, obj)),
        }
    }

    /// Call an extern function via FFI
    fn call_extern(&mut self, extern_func: &ExternFunc, _args: Vec<Value>) -> Result<Value, String> {
        // Get library path from environment variable
        let lib_path = std::env::var("MIMI_FFI_LIB")
            .map_err(|_| "MIMI_FFI_LIB environment variable not set for extern function call".to_string())?;

        // For now, return a placeholder since full FFI type conversion is complex
        // In a full implementation, this would:
        // 1. Resolve the symbol: unsafe { lib.get::<CFunc>(name) }
        // 2. Convert Mimi values to C types
        // 3. Call the function via function pointer
        // 4. Convert the result back to Mimi value
        let _ = lib_path; // suppress unused warning
        Err(format!(
            "extern function '{}' call not yet fully implemented (set MIMI_FFI_LIB to load library)",
            extern_func.name
        ))
    }
}
