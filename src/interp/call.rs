use super::*;
use crate::ffi::FfiContract;

impl<'a> Interpreter<'a> {
    pub(crate) fn call_func(&mut self, func: &FuncDef, args: Vec<Value>) -> Result<Value, InterpError> {
        if func.params.len() != args.len() {
            let expected_types: Vec<String> = func.params.iter().map(|p| crate::core::fmt_type(&p.ty)).collect();
            let actual_types: Vec<String> = args.iter().map(|a| crate::interp::value::type_name(a).to_string()).collect();
            return Err(InterpError::new(
                format!("function '{}' expects {} arguments [{}], got {} [{}]",
                    func.name, func.params.len(), expected_types.join(", "), args.len(), actual_types.join(", "))
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
            self.bind(&p.name, a)?;
        }

        // Extract and check requires conditions
        if self.verify_contracts {
            for stmt in &func.body {
                if let Stmt::Requires(expr, _) = stmt {
                    let cond = self.eval_expr(expr)?;
                    if !is_truthy(&cond) {
                        self.pop_scope();
                        self.pop_call();
                        return Err(InterpError::new(format!("requires condition failed for '{}': {}", func.name, cond)));
                    }
                }
            }
        }

        // Isolate early_return per function call — save outer, clear for this function's body
        let saved_early_return = self.early_return.take();

        let result = self.eval_block(&func.body);

        // If `exit()` was called inside the function body, propagate the exit code
        // and bypass the remaining post-conditions.
        if let Some(code) = self.exited.take() {
            self.pop_scope();
            self.pop_call();
            self.early_return = saved_early_return;
            return Ok(Value::Int(code as i64));
        }

        // Extract and check ensures conditions
        if self.verify_contracts {
            if let Ok(Some(ref rv)) = result {
                self.push_scope();
                self.bind("result", rv.clone())?;
                // Bind old snapshots for old(x) access
                for (name, val) in &old_snapshots {
                    self.bind(&format!("old_{}", name), val.clone())?;
                }
                let ensures_ok = (|| {
                    for stmt in &func.body {
                        if let Stmt::Ensures(expr, _) = stmt {
                            let cond = self.eval_expr(expr)?;
                            if !is_truthy(&cond) {
                                return Err(InterpError::new(format!("ensures condition failed for '{}': {}", func.name, cond)));
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
        // Check early_return set by this function's execution
        if let Some(val) = self.early_return.take() {
            self.early_return = saved_early_return; // restore outer early_return
            return Ok(val);
        }
        self.early_return = saved_early_return; // restore (no early_return from this call)
        result.map(|v| v.unwrap_or(Value::Unit))
    }

    pub fn call_named(&mut self, name: &str, args: Vec<Value>) -> Result<Value, InterpError> {
        // First check if the name is bound to a closure in the local scope
        if let Some(v) = self.lookup(name) {
            match v {
                Value::Closure { params, ret: _, body, captured } => {
                    if params.len() != args.len() {
                        return Err(InterpError::new(
                            format!("closure '{}' expects {} arguments, got {}", name, params.len(), args.len())
                        ));
                    }
                    self.push_scope();
                    for (n, val) in &captured {
                        self.bind(n, val.clone())?;
                    }
                    for (p, a) in params.iter().zip(args) {
                        self.bind(&p.name, a)?;
                    }
                    let result = self.eval_block(&body);
                    self.pop_scope();
                    if self.exited.is_some() {
                        return result.map(|v| v.unwrap_or(Value::Unit));
                    }
                    if let Some(val) = self.early_return.take() {
                        return Ok(val);
                    }
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
            return self.spawn_actor(actor_name);
        }

        // Handle extern function calls via their FFI contract (wrapper layer).
        if let Some(extern_func) = self.extern_funcs.get(name).cloned() {
            let contract = self.ffi_contracts.get(name).cloned()
                .unwrap_or_else(|| FfiContract::from_extern(&extern_func));
            return self.call_extern(&extern_func, &contract, args)
                .map_err(|e| InterpError::new(e.to_string()));
        }

        if let Some(&arity) = self.constructors.get(name) {
            if args.len() != arity {
                return Err(InterpError::new(format!(
                    "constructor '{}' expects {} arguments, got {}",
                    name, arity, args.len()
                )));
            }
            // Check if this is a newtype constructor - wrap in Value::Newtype
            if *self.newtype_constructors.get(name).unwrap_or(&false) && args.len() == 1 {
                let val = args.into_iter().next().ok_or_else(|| InterpError::new("newtype constructor: expected one argument"))?;
                return Ok(Value::Newtype(Box::new(val)));
            }
            return Ok(Value::Variant(name.into(), args));
        }
        // Check user-defined functions before builtins
        if let Some(func) = self.find_function(name) {
            return self.call_func(&func, args);
        }
        match name {
            "println" => self.builtin_println(args),
            "assert" => self.builtin_assert(args),
            "range" => self.builtin_range(args),
            "sqrt" => self.builtin_sqrt(args),
            "len" => self.builtin_len(args),
            "to_string" => self.builtin_to_string(args),
            "abs" => self.builtin_abs(args),
            "push" => self.builtin_push(args),
            "pop" => self.builtin_pop(args),
            "min" => self.builtin_min(args),
            "max" => self.builtin_max(args),
            "contains" => self.builtin_contains(args),
            "input" => self.builtin_input(args),
            "assert_eq" => self.builtin_assert_eq(args),
            "assert_ne" => self.builtin_assert_ne(args),
            "assert_approx_eq" => self.builtin_assert_approx_eq(args),
            "map" => self.builtin_map(args),
            "filter" => self.builtin_filter(args),
            "reduce" => self.builtin_reduce(args),
            "sort" => self.builtin_sort(args),
            "reverse" => self.builtin_reverse(args),
            "flatten" => self.builtin_flatten(args),
            "zip" => self.builtin_zip(args),
            "enumerate" => self.builtin_enumerate(args),
            "sum" => self.builtin_sum(args),
            "ast_dump" => self.builtin_ast_dump(args),
            "ast_eval" => self.builtin_ast_eval(args),
            "type_name" => self.builtin_type_name(args),
            "type_fields" => self.builtin_type_fields(args),
            "type_variants" => self.builtin_type_variants(args),
            "allocator_system" => self.builtin_allocator_system(args),
            "allocator_arena" => self.builtin_allocator_arena(args),
            "allocator_bump" => self.builtin_allocator_bump(args),
            "alloc" => self.builtin_alloc(args),
            "arena_reset" => self.builtin_arena_reset(args),
            "bump_used" => self.builtin_bump_used(args),
            "print" => self.builtin_print(args),
            "pow" => self.builtin_pow(args),
            "floor" => self.builtin_floor(args),
            "ceil" => self.builtin_ceil(args),
            "round" => self.builtin_round(args),
            "random" => self.builtin_random(args),
            "pi" => self.builtin_pi(args),
            "read_file" => self.builtin_read_file(args),
            "write_file" => self.builtin_write_file(args),
            "file_exists" => self.builtin_file_exists(args),
            "now" | "timestamp" => self.builtin_now(args),
            "now_ms" | "timestamp_ms" => self.builtin_now_ms(args),
            "sleep" => self.builtin_sleep(args),
            "getenv" => self.builtin_getenv(args),
            "args" => self.builtin_args(args),
            "to_json" => self.builtin_to_json(args),
            "from_json" => self.builtin_from_json(args),
            "json_is_valid" => self.builtin_json_is_valid(args),
            "json_get_string" => self.builtin_json_get_string(args),
            "json_get_int" => self.builtin_json_get_int(args),
            "json_get_element" => self.builtin_json_get_element(args),
            "str_char_at" => self.builtin_str_char_at(args),
            "str_substring" => self.builtin_str_substring(args),
            "str_parse_int" => self.builtin_str_parse_int(args),
            "str_parse_float" => self.builtin_str_parse_float(args),
            "str_split" => self.builtin_str_split(args),
            "str_join" => self.builtin_str_join(args),
            "str_trim" => self.builtin_str_trim(args),
            "str_starts_with" => self.builtin_str_starts_with(args),
            "str_ends_with" => self.builtin_str_ends_with(args),
            "str_replace" => self.builtin_str_replace(args),
            "str_to_upper" => self.builtin_str_to_upper(args),
            "str_to_lower" => self.builtin_str_to_lower(args),
            "str_repeat" => self.builtin_str_repeat(args),
            "str_contains" => self.builtin_str_contains(args),
            "str_index_of" => self.builtin_str_index_of(args),
            "keys" => self.builtin_keys(args),
            "values" => self.builtin_values(args),
            "has_key" => self.builtin_has_key(args),
            "map_new" => self.builtin_map_new(args),
            "map_get" => self.builtin_map_get(args),
            "map_set" => self.builtin_map_set(args),
            "map_remove" => self.builtin_map_remove(args),
            "map_size" => self.builtin_map_size(args),
            "map_from_list" => self.builtin_map_from_list(args),
            "to_int" => self.builtin_to_int(args),
            "to_float" => self.builtin_to_float(args),
            "from_int" => self.builtin_from_int(args),
            "lexer" => self.builtin_lexer(args),
            "parse" => self.builtin_parse(args),
            "str_to_c_str" => self.builtin_str_to_c_str(args),
            "c_str_to_string" => self.builtin_c_str_to_string(args),
            "eprintln" => self.builtin_eprintln(args),
            "exit" => self.builtin_exit(args),
            "socket" => self.builtin_socket(args),
            "connect" => self.builtin_connect(args),
            "bind" => self.builtin_bind(args),
            "listen" => self.builtin_listen(args),
            "accept" => self.builtin_accept(args),
            "send" => self.builtin_send(args),
            "recv" => self.builtin_recv(args),
            "close_fd" => self.builtin_close_fd(args),
            "http_get" => self.builtin_http_get(args),
            "http_post" => self.builtin_http_post(args),
            _ => {
                // Check for pre-computed comptime function results
                if let Some(result) = self.comptime_results.get(name) {
                    return Ok(result.clone());
                }
                Err(InterpError::new(format!("undefined function '{}'", name)))
            }
        }
    }

    /// Call an async function - spawns a new thread and returns a Future
    fn call_async_func(&mut self, func: &FuncDef, args: Vec<Value>) -> Result<Value, InterpError> {
        if func.params.len() != args.len() {
            return Err(InterpError::new(
                format!("function {} expects {} arguments, got {}", func.name, func.params.len(), args.len())
            ));
        }

        // Clone the function and arguments for the new thread
        let func_clone = func.clone();
        let args_clone = args;

        // Create a channel for the result
        let (tx, rx) = std::sync::mpsc::channel();

        // Spawn a new thread to execute the async function body directly
        let file_clone = self.file.clone();
        super::pool::get_pool().execute(move || {
            let mut interp = Interpreter::new(&file_clone);
            interp.push_scope();
            for (p, a) in func_clone.params.iter().zip(args_clone) {
                if let Err(e) = interp.bind(&p.name, a) {
                    let _ = tx.send(Err(e));
                    return;
                }
            }
            let block_result = interp.eval_block(&func_clone.body).map(|v| v.unwrap_or(Value::Unit));
            let result = interp.early_return.take()
                .map_or(block_result, Ok);
            interp.pop_scope();
            let _ = tx.send(result);
        });

        // Return a Future value
        Ok(Value::Future(std::sync::Arc::new(std::sync::Mutex::new(rx))))
    }

    pub(crate) fn call_method(&mut self, obj: &Value, method: &str, args: Vec<Value>) -> Result<Value, InterpError> {
        match obj {
            Value::Shared(arc) => {
                match method {
                    "clone" => Ok(Value::Shared(Arc::clone(arc))),
                    "deref" | "inner" => {
                        let inner = arc.read().map_err(|e| InterpError::new(format!("shared read lock failed: {}", e)))?;
                        Ok(inner.clone())
                    }
                    _ => Err(InterpError::new(format!("shared value has no method '{}' (type: {})", method, crate::interp::value::type_name(obj)))),
                }
            }
            Value::LocalShared(rc) => {
                match method {
                    "clone" => Ok(Value::LocalShared(LocalSharedInner::clone_rc(&rc))),
                    "deref" | "inner" => {
                        let inner = rc.borrow();
                        Ok(inner.clone())
                    }
                    _ => Err(InterpError::new(format!("local_shared value has no method '{}' (type: {})", method, crate::interp::value::type_name(obj)))),
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
                    _ => Err(InterpError::new(format!("weak_shared value has no method '{}' (type: {})", method, crate::interp::value::type_name(obj)))),
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
                    _ => Err(InterpError::new(format!("weak_local value has no method '{}' (type: {})", method, crate::interp::value::type_name(obj)))),
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
                    _ => Err(InterpError::new(format!("cap value has no method '{}' — available: split, consume, is_consumed", method))),
                }
            }
            Value::Actor(actor_arc) => {
                match method {
                    "spawn" => {
                        Err("spawn() should be called on Actor type, not instance".into())
                    }
                    _ => {
                        // Check if we're inside this actor's own worker thread
                        // If so, execute directly to avoid mailbox deadlock.
                        // Cross-actor calls from a worker always go through mailbox.
                        let is_self_call = crate::interp::value::ActorHandle::current_worker_id() == actor_arc.id;

                        if is_self_call {
                            // Direct execution (same thread, no mailbox)
                            let actor_name: String;
                            let actor_methods: Vec<FuncDef>;
                            {
                                let actor = actor_arc.inner.read()
                                    .map_err(|e| format!("actor lock failed: {}", e))?;
                                actor_name = actor.actor_name.clone();
                                actor_methods = actor.methods.clone();
                            }
                            let func = actor_methods.iter()
                                .find(|f| f.name == method)
                                .ok_or_else(|| format!("actor {} has no method '{}'", actor_name, method))?;
                            self.push_scope();
                            self.bind("self", obj.clone())?;
                            let result = self.call_func(func, args);
                            self.pop_scope();
                            result
                        } else {
                            // Mailbox dispatch: send message, wait for response
                            let (tx, rx) = std::sync::mpsc::channel();
                            let msg = crate::interp::value::ActorMailboxMsg {
                                method: method.to_string(),
                                args: args.to_vec(),
                                response: tx,
                            };
                            actor_arc.mailbox.send(msg)
                                .map_err(|_| InterpError::new("actor mailbox send failed".to_string()))?;
                            match rx.recv() {
                                Ok(result) => result,
                                Err(_) => Err(InterpError::new("actor worker terminated".to_string())),
                            }
                        }
                    }
                }
            }
            Value::DynTrait { data, concrete_type, trait_names } => {
                // Look up the concrete type's trait implementations
                if let Some(impls) = self.type_impls.get(concrete_type) {
                    for trait_name in trait_names {
                        if let Some(methods) = impls.get(trait_name) {
                            if let Some(func) = methods.iter().find(|f| f.name == method) {
                                let func = func.clone();
                                // Call the trait method with self = the concrete value
                                self.push_scope();
                                self.bind("self", *data.clone())?;
                                // If the concrete value is a record, bind its fields too
                                if let Value::Record(_, fields) = data.as_ref() {
                                    for (field_name, field_value) in fields {
                                        self.bind(field_name, field_value.clone())?;
                                    }
                                }
                                let result = self.call_func(&func, args);
                                self.pop_scope();
                                return result;
                            }
                        }
                    }
                }
                Err(InterpError::new(format!("cannot call method '{}' on dyn {} (concrete type: {})",
                    method, trait_names.join(" + "), concrete_type)))
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
                                self.bind("self", obj.clone())?;
                                // Bind record fields to scope
                                for (field_name, field_value) in &fields {
                                    self.bind(field_name, field_value.clone())?;
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
                    _ => {
                        let type_label = type_name.as_deref().unwrap_or("Record");
                        Err(InterpError::new(format!("record '{}' has no method '{}'", type_label, method)))
                    }
                }
            }
            Value::String(s) => {
                // Try trait method dispatch via type_impls first
                if let Some(impls) = self.type_impls.get("string") {
                    for methods in impls.values() {
                        if let Some(func) = methods.iter().find(|f| f.name == method) {
                            let func = func.clone();
                            self.push_scope();
                            self.bind("self", obj.clone())?;
                            let result = self.call_func(&func, args);
                            self.pop_scope();
                            return result;
                        }
                    }
                }
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
                                    return Err(InterpError::new(format!("substring: end {} out of bounds (len {})", ei, chars.len())));
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
                    _ => Err(InterpError::new(format!("string has no method '{}'", method))),
                }
            }
            Value::List(list) => {
                // Try trait method dispatch via type_impls first
                if let Some(impls) = self.type_impls.get("List") {
                    for methods in impls.values() {
                        if let Some(func) = methods.iter().find(|f| f.name == method) {
                            let func = func.clone();
                            self.push_scope();
                            self.bind("self", obj.clone())?;
                            let result = self.call_func(&func, args);
                            self.pop_scope();
                            return result;
                        }
                    }
                }
                match method {
                    "len" => Ok(Value::Int(list.len() as i64)),
                    _ => Err(InterpError::new(format!("List has no method '{}'", method))),
                }
            }
            Value::Variant(name, vals) => {
                // Option/Result combinator methods on enum variants
                match (name.as_str(), method) {
                    // ===== Option methods =====
                    ("Some" | "Ok", "unwrap") | ("Some" | "Ok", "expect") => {
                        if vals.is_empty() {
                            Err(InterpError::new(format!("{}::{} has no inner value", name, method)))
                        } else {
                            Ok(vals[0].clone())
                        }
                    }
                    ("None", "unwrap") => Err("called unwrap() on None".into()),
                    ("None", "expect") => {
                        let msg = if args.is_empty() { "called expect() on None" } else { &args[0].to_string() };
                        Err(InterpError::new(format!("{}", msg)))
                    }
                    ("Err", "unwrap") | ("Err", "expect") => {
                        let msg = if vals.is_empty() { "called unwrap() on Err".to_string() } else { format!("called unwrap() on Err({})", vals[0]) };
                        Err(InterpError::new(msg))
                    }

                    ("Some", "unwrap_or") | ("Ok", "unwrap_or") => {
                        Ok(vals[0].clone())
                    }
                    ("None", "unwrap_or") | ("Err", "unwrap_or") => {
                        args.into_iter().next().ok_or_else(|| InterpError::new("unwrap_or requires a default value".to_string()))
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

                    _ => Err(InterpError::new(format!("variant '{}' has no method '{}'", name, method))),
                }
            }
            _ => Err(InterpError::new(format!("cannot call method '{}' on value {}", method, obj))),
        }
    }

    /// Apply a closure value to arguments
    fn apply_closure(&mut self, closure: &Value, args: Vec<Value>) -> Result<Value, InterpError> {
        match closure {
            Value::Closure { params, body, captured, .. } =>
                self.apply_closure_inner(params, body, captured, args),
            _ => Err(InterpError::new(format!("expected a closure, found {}", closure))),
        }
    }
}
