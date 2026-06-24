use super::super::*;

impl<'a> Interpreter<'a> {
    pub(in crate::interp) fn eval_ident(&mut self, name: &str) -> Result<Value, InterpError> {
        if let Some(v) = self.lookup(name) {
            Ok(v)
        } else if self.is_moved(name) {
            Err(InterpError::new(format!("use of moved value '{}'", name)))
        } else if let Some(components) = self.cap_defs.get(name) {
            // Cap definition: return as Value::Cap
            Ok(Value::Cap(components.clone()))
        } else if let Some(func) = self.find_function(name) {
            // First-class function: wrap as a closure with empty capture
            Ok(Value::Closure {
                params: func.params,
                ret: func.ret,
                body: func.body,
                captured: HashMap::new(),
            })
        } else if let Some(&arity) = self.constructors.get(name) {
            if arity == 0 {
                if self.newtype_constructors.get(name).copied().unwrap_or(false) {
                    return Err(InterpError::new(format!("newtype '{}' requires exactly one argument", name)));
                }
                Ok(Value::Variant(name.to_string(), vec![]))
            } else {
                Err(InterpError::new(format!("constructor '{}' requires {} arguments", name, arity)))
            }
        } else if let Some(suggestion) = self.suggest_similar(name) {
            Err(InterpError::new(format!("undefined variable '{}' — did you mean '{}'?", name, suggestion)))
        } else {
            Err(InterpError::new(format!("undefined variable '{}'", name)))
        }
    }

    pub(in crate::interp) fn eval_unary(&mut self, op: UnOp, e: &Expr) -> Result<Value, InterpError> {
        let v = self.eval_expr(e)?;
        match op {
            UnOp::Neg => match v {
                Value::Int(x) => {
                    x.checked_neg()
                        .ok_or_else(|| InterpError::integer_overflow(format!("integer overflow in negation: -{}", x)))
                        .map(Value::Int)
                }
                Value::Float(x) => { let r = -x; if r.is_nan() { Err(InterpError::float_error(format!("NaN from negation of {}", x))) } else { Ok(Value::Float(r)) } },
                _ => Err(InterpError::new(format!("cannot negate {}", type_name(&v)))),
            },
            UnOp::Not => Ok(Value::Bool(!is_truthy(&v))),
            UnOp::Ref => Ok(Value::Ref(Arc::new(RwLock::new(v)))),
            UnOp::RefMut => Ok(Value::RefMut(Arc::new(RwLock::new(v)))),
            UnOp::Deref => match v {
                Value::Ref(rc) | Value::RefMut(rc) => Ok(rc.read().map_err(|e| InterpError::lock_error(format!("read lock failed: {}", e)))?.clone()),
                Value::Shared(arc) => Ok(arc.read().map_err(|e| InterpError::lock_error(format!("shared read lock failed: {}", e)))?.clone()),
                Value::LocalShared(rc) => Ok(rc.borrow().clone()),
                _ => Err(InterpError::new(format!("cannot dereference {}", type_name(&v)))),
            },
        }
    }

    pub(in crate::interp) fn eval_binary(&mut self, op: BinOp, l: &Expr, r: &Expr) -> Result<Value, InterpError> {
        // short-circuit logic
        match op {
            BinOp::And => {
                let left = self.eval_expr(l)?;
                if !is_truthy(&left) {
                    return Ok(Value::Bool(false));
                }
                return Ok(Value::Bool(is_truthy(&self.eval_expr(r)?)));
            }
            BinOp::Or => {
                let left = self.eval_expr(l)?;
                if is_truthy(&left) {
                    return Ok(Value::Bool(true));
                }
                return Ok(Value::Bool(is_truthy(&self.eval_expr(r)?)));
            }
            _ => {}
        }
        let left = self.eval_expr(l)?;
        let right = self.eval_expr(r)?;

        // Helper for mixed numeric arithmetic: any float operand promotes the
        // whole operation to float, matching the typechecker's widening rules.
        let float_binop = |a: f64, b: f64, op: &str| -> Result<Value, InterpError> {
            let r = match op {
                "+" => a + b,
                "-" => a - b,
                "*" => a * b,
                "/" => {
                    if b == 0.0 { return Err(InterpError::div_by_zero()); }
                    let v = a / b;
                    if v.is_nan() { return Err(InterpError::float_error(format!("NaN from {} / {}", a, b))); }
                    if v.is_infinite() { return Err(InterpError::float_error(format!("infinity from {} / {}", a, b))); }
                    v
                }
                "^" => {
                    let v = a.powf(b);
                    if v.is_nan() { return Err(InterpError::float_error(format!("NaN from pow({}, {})", a, b))); }
                    v
                }
                _ => unreachable!("unsupported float binop {}", op),
            };
            Ok(Value::Float(r))
        };

        match op {
            BinOp::Add => match (&left, &right) {
                (Value::String(a), Value::String(b)) => Ok(Value::String(format!("{}{}", a, b))),
                (Value::Int(a), Value::Int(b)) => {
                    a.checked_add(*b)
                        .ok_or_else(|| InterpError::integer_overflow(format!("integer overflow in addition: {} + {}", a, b)))
                        .map(Value::Int)
                }
                (Value::Float(a), Value::Float(b)) => float_binop(*a, *b, "+"),
                (Value::Int(a), Value::Float(b)) | (Value::Float(b), Value::Int(a)) => float_binop(*a as f64, *b, "+"),
                _ => Err(InterpError::new(format!("cannot apply '+' to {} and {}", type_name(&left), type_name(&right)))),
            },
            BinOp::Sub => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => {
                    a.checked_sub(*b)
                        .ok_or_else(|| InterpError::integer_overflow(format!("integer overflow in subtraction: {} - {}", a, b)))
                        .map(Value::Int)
                }
                (Value::Float(a), Value::Float(b)) => float_binop(*a, *b, "-"),
                (Value::Int(a), Value::Float(b)) => float_binop(*a as f64, *b, "-"),
                (Value::Float(a), Value::Int(b)) => float_binop(*a, *b as f64, "-"),
                _ => Err(InterpError::new(format!("cannot apply '-' to {} and {}", type_name(&left), type_name(&right)))),
            },
            BinOp::Mul => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => {
                    a.checked_mul(*b)
                        .ok_or_else(|| InterpError::integer_overflow(format!("integer overflow in multiplication: {} * {}", a, b)))
                        .map(Value::Int)
                }
                (Value::Float(a), Value::Float(b)) => float_binop(*a, *b, "*"),
                (Value::Int(a), Value::Float(b)) | (Value::Float(b), Value::Int(a)) => float_binop(*a as f64, *b, "*"),
                _ => Err(InterpError::new(format!("cannot apply '*' to {} and {}", type_name(&left), type_name(&right)))),
            },
            BinOp::Div => match (&left, &right) {
                (Value::Int(_), Value::Int(0)) => Err(InterpError::div_by_zero()),
                (Value::Int(a), Value::Int(b)) => {
                    a.checked_div(*b)
                        .ok_or_else(|| InterpError::integer_overflow(format!("integer overflow in division: {} / {}", a, b)))
                        .map(Value::Int)
                }
                (Value::Float(a), Value::Float(b)) => float_binop(*a, *b, "/"),
                (Value::Int(a), Value::Float(b)) => float_binop(*a as f64, *b, "/"),
                (Value::Float(a), Value::Int(b)) => float_binop(*a, *b as f64, "/"),
                _ => Err(InterpError::new(format!("cannot apply '/' to {} and {}", type_name(&left), type_name(&right)))),
            },
            BinOp::Mod => match (&left, &right) {
                (Value::Int(_), Value::Int(0)) => Err(InterpError::div_by_zero()),
                (Value::Int(a), Value::Int(b)) => {
                    a.checked_rem(*b)
                        .ok_or_else(|| InterpError::integer_overflow(format!("integer overflow in modulo: {} % {}", a, b)))
                        .map(Value::Int)
                }
                _ => Err(InterpError::new(format!("cannot apply '%' to {} and {}", type_name(&left), type_name(&right)))),
            },
            BinOp::Pow => match (&left, &right) {
                (Value::Int(_), Value::Int(b)) if *b < 0 => Err(InterpError::new("negative exponent not supported for integers")),
                (Value::Int(a), Value::Int(b)) => {
                    a.checked_pow(*b as u32)
                        .ok_or_else(|| InterpError::integer_overflow(format!("integer overflow in power: {} ^ {}", a, b)))
                        .map(Value::Int)
                }
                (Value::Float(a), Value::Float(b)) => float_binop(*a, *b, "^"),
                (Value::Int(a), Value::Float(b)) => float_binop(*a as f64, *b, "^"),
                (Value::Float(a), Value::Int(b)) => float_binop(*a, *b as f64, "^"),
                _ => Err(InterpError::new(format!("cannot apply '^' to {} and {}", type_name(&left), type_name(&right)))),
            },
            BinOp::EqCmp => Ok(Value::Bool(values_equal(&left, &right))),
            BinOp::NeCmp => Ok(Value::Bool(!values_equal(&left, &right))),
            BinOp::Lt => compare_op(left, right, |o| o == std::cmp::Ordering::Less),
            BinOp::Gt => compare_op(left, right, |o| o == std::cmp::Ordering::Greater),
            BinOp::Le => compare_op(left, right, |o| o != std::cmp::Ordering::Greater),
            BinOp::Ge => compare_op(left, right, |o| o != std::cmp::Ordering::Less),
            BinOp::BitAnd => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a & b)),
                _ => Err(InterpError::new(format!("cannot apply '&' to {} and {}", type_name(&left), type_name(&right)))),
            },
            BinOp::BitOr => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a | b)),
                _ => Err(InterpError::new(format!("cannot apply '|' to {} and {}", type_name(&left), type_name(&right)))),
            },
            BinOp::BitXor => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a ^ b)),
                _ => Err(InterpError::new(format!("cannot apply '^' to {} and {}", type_name(&left), type_name(&right)))),
            },
            BinOp::Shl => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => a.checked_shl(*b as u32)
                    .ok_or_else(|| InterpError::integer_overflow(format!("shift left overflow: {} << {}", a, b)))
                    .map(Value::Int),
                _ => Err(InterpError::new(format!("cannot apply '<<' to {} and {}", type_name(&left), type_name(&right)))),
            },
            BinOp::Shr => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => a.checked_shr(*b as u32)
                    .ok_or_else(|| InterpError::integer_overflow(format!("shift right overflow: {} >> {}", a, b)))
                    .map(Value::Int),
                _ => Err(InterpError::new(format!("cannot apply '>>' to {} and {}", type_name(&left), type_name(&right)))),
            },
            BinOp::Range => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Range { start: *a, end: *b }),
                _ => Err(InterpError::new(format!("cannot apply '..' to {} and {}", type_name(&left), type_name(&right)))),
            },
            BinOp::Assign => Err(InterpError::new("assignment as expression not supported")),
            BinOp::And | BinOp::Or => Err(InterpError::new("logical and/or not supported in expression context")),
        }
    }

    pub(in crate::interp) fn eval_call(&mut self, callee: &Expr, args: &[Expr]) -> Result<Value, InterpError> {
        // Handle named args by resolving them to positional order.
        // If the call has named args, reorder before evaluating.
        let has_named = args.iter().any(|a| matches!(a, Expr::NamedArg(_, _)));
        if has_named {
            if let Expr::Ident(name) = callee {
                if let Some(f) = self.find_function(name) {
                    let mut ordered_exprs: Vec<Expr> = Vec::new();
                    let mut dest_idx = 0;
                    // Process positional args
                    for arg in args {
                        match arg {
                            Expr::NamedArg(n, val) => {
                                // Find position in params
                                if let Some(pos) = f.params.iter().position(|p| p.name == *n) {
                                    while ordered_exprs.len() <= pos {
                                        ordered_exprs.push(Expr::Literal(Lit::Unit));
                                    }
                                    ordered_exprs[pos] = *val.clone();
                                } else {
                                    // Unknown named arg — push the expr itself (evaluated later)
                                    ordered_exprs.push(arg.clone());
                                    continue;
                                }
                            }
                            _ => {
                                while ordered_exprs.len() <= dest_idx {
                                    ordered_exprs.push(Expr::Literal(Lit::Unit));
                                }
                                ordered_exprs[dest_idx] = arg.clone();
                                dest_idx += 1;
                            }
                        }
                    }
                    // Fill in defaults
                    for (i, p) in f.params.iter().enumerate() {
                        if i >= ordered_exprs.len() || matches!(ordered_exprs[i], Expr::Literal(Lit::Unit)) {
                            if let Some(ref default_expr) = p.default_value {
                                if i >= ordered_exprs.len() {
                                    ordered_exprs.push(default_expr.clone());
                                } else {
                                    ordered_exprs[i] = default_expr.clone();
                                }
                            }
                        }
                    }
                    let vals: Result<Vec<_>, _> = ordered_exprs.iter().map(|a| self.eval_expr(a)).collect();
                    let vals = vals?;
                    return self.eval_call_dispatch(callee, &vals, args);
                }
            }
        }

        let vals: Result<Vec<_>, _> = args.iter().map(|a| self.eval_expr(a)).collect();
        let vals = vals?;
        self.eval_call_dispatch(callee, &vals, args)
    }

    /// Dispatch an evaluated function call — shared by both positional and named-arg paths.
    fn eval_call_dispatch(&mut self, callee: &Expr, vals: &[Value], args: &[Expr]) -> Result<Value, InterpError> {
        let vals = vals.to_vec();
        match callee {
            Expr::Ident(name) => {
                let result = self.call_named(name, vals)?;
                // push(list, elem) mutates the list variable in place in the interpreter,
                // matching the codegen semantics where the list pointer is modified.
                if name == "push" && !args.is_empty() {
                    if let Expr::Ident(var_name) = &args[0] {
                        if let Value::List(_) = &result {
                            // Only mutate the source variable if it was declared mut;
                            // otherwise push behaves as a pure function returning a new list.
                            if self.is_mutable(var_name) {
                                self.assign(var_name, result.clone())?;
                            }
                        }
                    }
                }
                Ok(result)
            }
            Expr::Field(obj, method) => {
                // Handle Type.spawn() - actor constructor
                if method == "spawn" {
                    if let Expr::Ident(type_name) = obj.as_ref() {
                        // Check if this is an actor type
                        if self.find_actor(type_name).is_some() {
                            return self.spawn_actor(type_name);
                        }
                    }
                }
                // Handle module-qualified function call: Module::func(args)
                if let Some(qualified) = Self::build_qualified_path(obj, method) {
                    if let Some(f) = self.find_function(&qualified) {
                        return self.call_func(&f, vals);
                    }
                }
                // Regular method call: evaluate the object and call method on it
                let obj_val = self.eval_expr(obj)?;
                self.call_method(&obj_val, method, vals)
            }
            _ => {
                // Evaluate callee - could be a closure or other expression
                let callee_val = self.eval_expr(callee)?;
                match callee_val {
                    Value::Closure { params, ret: _, body, captured } =>
                        self.apply_closure_inner(&params, &body, &captured, vals),
                    _ => Err(InterpError::new(format!("cannot call {}: expected a function or closure", type_name(&callee_val)))),
                }
            }
        }
    }

    pub(in crate::interp) fn eval_tuple(&mut self, elems: &[Expr]) -> Result<Value, InterpError> {
        let mut vals = Vec::new();
        for e in elems {
            vals.push(self.eval_expr(e)?);
        }
        Ok(Value::Tuple(vals))
    }

    pub(in crate::interp) fn eval_tuple_index(&mut self, obj: &Expr, idx: usize) -> Result<Value, InterpError> {
        let obj_val = self.eval_expr(obj)?;
        match obj_val {
            Value::Tuple(items) => {
                if idx < items.len() {
                    Ok(items[idx].clone())
                } else {
                    Err(InterpError::new(format!("tuple index {} out of bounds (len {})", idx, items.len())))
                }
            }
            _ => Err(InterpError::new(format!("cannot index non-tuple value with .{}", idx))),
        }
    }

    pub(in crate::interp) fn eval_list(&mut self, elems: &[Expr]) -> Result<Value, InterpError> {
        let mut vals = Vec::new();
        for e in elems {
            vals.push(self.eval_expr(e)?);
        }
        Ok(Value::List(vals))
    }

    pub(in crate::interp) fn eval_comprehension(&mut self, expr: &Expr, var: &str, iter: &Expr, guard: &Option<Box<Expr>>) -> Result<Value, InterpError> {
        let iter_val = self.eval_expr(iter)?;
        let items = match iter_val {
            Value::List(l) => l,
            _ => return Err(InterpError::new(format!("comprehension requires a list, got {}", type_name(&iter_val)))),
        };
        let mut result = Vec::new();
        for item in items {
            self.push_scope();
            self.bind(var, item.clone())?;
            let include = if let Some(g) = guard {
                let cond = self.eval_expr(g)?;
                is_truthy(&cond)
            } else {
                true
            };
            if include {
                let val = self.eval_expr(expr)?;
                result.push(val);
            }
            self.pop_scope();
        }
        Ok(Value::List(result))
    }

    pub(in crate::interp) fn eval_if_expr(&mut self, cond: &Expr, then_: &Block, else_: &Option<Block>) -> Result<Value, InterpError> {
        let c = self.eval_expr(cond)?;
        if is_truthy(&c) {
            self.push_scope();
            let r = self.eval_block(then_);
            self.pop_scope();
            r.map(|v| v.unwrap_or(Value::Unit))
        } else if let Some(eb) = else_ {
            self.push_scope();
            let r = self.eval_block(eb);
            self.pop_scope();
            r.map(|v| v.unwrap_or(Value::Unit))
        } else {
            Ok(Value::Unit)
        }
    }

    pub(in crate::interp) fn eval_match(&mut self, subject: &Expr, arms: &[MatchArm]) -> Result<Value, InterpError> {
        let val = self.eval_expr(subject)?;
        for arm in arms {
            if let Some(bindings) = self.match_pattern(&arm.pat, &val) {
                self.push_scope();
                for (name, v) in bindings {
                    self.bind(&name, v)?;
                }
                if let Some(guard) = &arm.guard {
                    let g = self.eval_expr(guard)?;
                    if !is_truthy(&g) {
                        self.pop_scope();
                        continue;
                    }
                }
                let result = self.eval_expr(&arm.body);
                self.pop_scope();
                return result;
            }
        }
        Err(InterpError::new("non-exhaustive match"))
    }

    pub(in crate::interp) fn eval_field(&mut self, obj: &Expr, field: &str) -> Result<Value, InterpError> {
        // Special case: module-qualified access (Module::func or Module::Sub::func)
        // Build qualified path by collecting nested Field(Ident(...), ...) nodes
        if let Some(qualified) = Self::build_qualified_path(obj, field) {
            if let Some(f) = self.find_function(&qualified) {
                return Ok(Value::Closure {
                    params: f.params.clone(),
                    ret: f.ret.clone(),
                    body: f.body.clone(),
                    captured: HashMap::new(),
                });
            }
            // Check if it's an enum variant constructor (e.g., Color::Red)
            if let Expr::Ident(_type_name) = obj {
                if let Some(&ctor_arity) = self.constructors.get(field) {
                    if ctor_arity == 0 {
                        // 0-arity variant: return the variant value directly
                        return Ok(Value::Variant(field.to_string(), vec![]));
                    } else {
                        // N-arity variant: return a closure that constructs it
                        let field_clone = field.to_string();
                        return Ok(Value::Closure {
                            params: (0..ctor_arity).map(|i| Param {
                                name: format!("arg{}", i),
                                ty: Type::Name("unknown".into(), vec![]),
                                mut_: false,
                                default_value: None,
                            }).collect(),
                            ret: None,
                            body: vec![Stmt::Return(Some(Expr::Call(
                                Box::new(Expr::Ident(field_clone)),
                                (0..ctor_arity).map(|i| Expr::Ident(format!("arg{}", i))).collect(),
                            )))],
                            captured: HashMap::new(),
                        });
                    }
                }
            }
        }
        // Special case: if accessing field on "self" identifier, look up field directly
        if let Expr::Ident(name) = obj {
            if name == "self" {
                // Look up self from scope
                if let Some(Value::Actor(handle)) = self.lookup("self") {
                    let actor = handle.inner.read().map_err(|e| InterpError::new(format!("actor lock failed: {}", e)))?;
                    if let Some(value) = actor.fields.get(field) {
                        return Ok(value.clone());
                    }
                    return Err(InterpError::new(format!("actor field '{}' not found", field)));
                }
                // For non-actor self values (records, etc.), fall through to normal field access
            }
        }
        let obj_val = self.eval_expr(obj)?;
        match obj_val {
            Value::Record(_, fields) => {
                fields
                    .get(field)
                    .cloned()
                    .ok_or_else(|| InterpError::new({
                        let available: Vec<&str> = fields.keys().map(|s| s.as_str()).collect();
                        if available.is_empty() {
                            format!("field '{}' not found in record (record is empty)", field)
                        } else {
                            format!("field '{}' not found in record — available fields: {}", field, available.join(", "))
                        }
                    }))
            }
            Value::Actor(handle) => {
                // Actor field access using read lock
                let actor = handle.inner.read().map_err(|e| InterpError::new(format!("actor lock failed: {}", e)))?;
                actor.fields.get(field)
                    .cloned()
                    .ok_or_else(|| InterpError::new(format!("actor field '{}' not found", field)))
            }
            Value::Shared(arc) => {
                let inner = arc.read().map_err(|e| InterpError::new(format!("shared read lock failed: {}", e)))?;
                match &*inner {
                    Value::Record(_, fields) => fields.get(field).cloned()
                        .ok_or_else(|| InterpError::new(format!("field '{}' not found in shared record", field))),
                    _ => Err(InterpError::new("field access on non-record shared value")),
                }
            }
            Value::LocalShared(rc) => {
                let inner = rc.borrow();
                match &*inner {
                    Value::Record(_, fields) => fields.get(field).cloned()
                        .ok_or_else(|| InterpError::new(format!("field '{}' not found in local_shared record", field))),
                    _ => Err(InterpError::new("field access on non-record local_shared value")),
                }
            }
            _ => Err(InterpError::new(format!("cannot access field '{}' on {}", field, type_name(&obj_val)))),
        }
    }

    pub(in crate::interp) fn eval_record(&mut self, ty: &Option<String>, fields: &[RecordFieldExpr]) -> Result<Value, InterpError> {
        let mut map = HashMap::new();
        for f in fields {
            let v = self.eval_expr(&f.value)?;
            map.insert(f.name.clone(), v);
        }
        Ok(Value::Record(ty.clone(), map))
    }

    pub(in crate::interp) fn eval_map_literal(&mut self, entries: &[(Expr, Expr)]) -> Result<Value, InterpError> {
        let mut map = HashMap::new();
        for (k, v) in entries {
            let key = self.eval_expr(k)?;
            let val = self.eval_expr(v)?;
            let key_str = key.to_string();
            map.insert(key_str, val);
        }
        Ok(Value::Record(None, map))
    }

    pub(in crate::interp) fn eval_set_literal(&mut self, elems: &[Expr]) -> Result<Value, InterpError> {
        let mut vals: Vec<Value> = Vec::new();
        for e in elems {
            let v = self.eval_expr(e)?;
            if !vals.iter().any(|existing| values_equal(existing, &v)) {
                vals.push(v);
            }
        }
        Ok(Value::Set(vals))
    }

    pub(in crate::interp) fn eval_index(&mut self, obj_expr: &Expr, idx_expr: &Expr) -> Result<Value, InterpError> {
        let obj = self.eval_expr(obj_expr)?;
        let idx = self.eval_expr(idx_expr)?;
        match (&obj, &idx) {
            (Value::List(list), Value::Int(i)) => {
                let len = list.len() as i64;
                let i = if *i < 0 { len + *i } else { *i };
                if i < 0 || i >= len {
                    return Err(InterpError::index_out_of_bounds(format!("index out of bounds: index {} is not valid for list of length {}", i, len)));
                }
                Ok(list[i as usize].clone())
            }
            (Value::Array(arr), Value::Int(i)) => {
                let len = arr.len() as i64;
                let i = if *i < 0 { len + *i } else { *i };
                if i < 0 || i >= len {
                    return Err(InterpError::index_out_of_bounds(format!("index out of bounds: index {} is not valid for array of length {}", i, len)));
                }
                Ok(arr[i as usize].clone())
            }
            (Value::Slice { source, start, end }, Value::Int(i)) => {
                let slice_len = (*end - *start) as i64;
                let i = if *i < 0 { slice_len + *i } else { *i };
                if i < 0 || i >= slice_len {
                    return Err(InterpError::index_out_of_bounds(format!("index out of bounds: index {} is not valid for slice of length {}", i, slice_len)));
                }
                Ok(source[*start + i as usize].clone())
            }
            (Value::String(s), Value::Int(i)) => {
                let len = s.chars().count() as i64;
                let i = if *i < 0 { len + *i } else { *i };
                if i < 0 || i >= len {
                    return Err(InterpError::index_out_of_bounds(format!("index out of bounds: index {} is not valid for string of length {}", i, len)));
                }
                let ch = s.chars().nth(i as usize).ok_or_else(|| InterpError::index_out_of_bounds(format!("index out of bounds: index {} is not valid for string of length {}", i, len)))?;
                Ok(Value::String(ch.to_string()))
            }
            _ => Err(InterpError::new(format!("cannot index {} with {}", type_name(&obj), type_name(&idx)))),
        }
    }

    pub(in crate::interp) fn eval_slice_expr(&mut self, target: &Expr, start: &Option<Box<Expr>>, end: &Option<Box<Expr>>) -> Result<Value, InterpError> {
        let obj = self.eval_expr(target)?;
        let len = match &obj {
            Value::List(l) => l.len(),
            Value::Array(a) => a.len(),
            Value::Slice { source: _, start: s, end: e } => e - s,
            Value::String(s) => s.len(),
            _ => return Err(InterpError::new("cannot slice non-sequence value")),
        };
        let start_idx = match start {
            Some(e) => {
                let v = self.eval_expr(e)?;
                match v {
                    Value::Int(i) => {
                        let i = if i < 0 { len as i64 + i } else { i } as usize;
                        if i > len { return Err(InterpError::slice_error("slice start out of bounds")); }
                        i
                    }
                    _ => return Err(InterpError::new("slice index must be integer")),
                }
            }
            None => 0,
        };
        let end_idx = match end {
            Some(e) => {
                let v = self.eval_expr(e)?;
                match v {
                    Value::Int(i) => {
                        let i = if i < 0 { len as i64 + i } else { i } as usize;
                        if i > len { return Err(InterpError::slice_error("slice end out of bounds")); }
                        i
                    }
                    _ => return Err(InterpError::new("slice index must be integer")),
                }
            }
            None => len,
        };
        if start_idx > end_idx {
            return Err(InterpError::slice_error("slice start > end"));
        }
        match obj {
            Value::List(l) => Ok(Value::Slice { source: l, start: start_idx, end: end_idx }),
            Value::Array(a) => Ok(Value::Slice { source: a, start: start_idx, end: end_idx }),
            Value::Slice { source, start: _, end: _ } => {
                // Re-slice: adjust indices relative to the original source
                Ok(Value::Slice { source, start: start_idx, end: end_idx })
            }
            Value::String(s) => {
                let chars: Vec<char> = s.chars().collect();
                let sliced: String = chars[start_idx..end_idx].iter().collect();
                Ok(Value::String(sliced))
            }
            other => Err(InterpError::new(format!("unexpected expression type in await: {}", other))),
        }
    }

    pub(in crate::interp) fn eval_try(&mut self, expr: &Expr) -> Result<Value, InterpError> {
        let v = self.eval_expr(expr)?;
        match v {
            Value::Variant(name, vals) => {
                // Check if this is a known failure variant
                let is_failure = self.failure_variants.get(&name).copied().unwrap_or(false);
                if is_failure {
                    // Set early_return so that call_func returns this value,
                    // eval_block triggers compensations, and match can catch it
                    self.early_return = Some(Value::Variant(name, vals));
                    Ok(Value::Unit)
                } else {
                    // Treat as success variant - return inner value
                    Ok(vals.into_iter().next().unwrap_or(Value::Unit))
                }
            }
            Value::Error(msg) => {
                // ? on an already-propagated error: re-propagate
                self.early_return = Some(Value::Error(msg));
                Ok(Value::Unit)
            }
            _ => {
                Ok(Value::Error(format!("? operator requires Result or Option, found {}", v)))
            }
        }
    }

    pub(in crate::interp) fn eval_spawn(&mut self, expr: &Expr) -> Result<Value, InterpError> {
        // Check for actor method spawn: `spawn actor.method(args)`
        if let Expr::Call(callee, args) = expr {
            if let Expr::Field(obj, method) = callee.as_ref() {
                let obj_val = self.eval_expr(obj)?;
                let method_name = method.clone();
                let args_vals: Vec<Value> = args.iter()
                    .map(|a| self.eval_expr(a))
                    .collect::<Result<Vec<_>, _>>()?;
                if let Value::Actor(handle) = obj_val {
                        // Send through mailbox, return Future<T> for awaiting later
                        let (tx, rx) = std::sync::mpsc::channel();
                        let msg = crate::interp::value::ActorMailboxMsg {
                            method: method_name,
                            args: args_vals,
                            response: tx,
                        };
                        handle.mailbox.send(msg)
                            .map_err(|_| InterpError::new("actor mailbox send failed"))?;
                        return Ok(Value::Future(std::sync::Arc::new(std::sync::Mutex::new(
                            crate::interp::value::PollFuture::Pending(rx)
                        ))));
                }
            }
        }
        // Non-actor `spawn expr` — evaluate directly.
        self.eval_expr(expr)
    }

    pub(in crate::interp) fn eval_await(&mut self, expr: &Expr) -> Result<Value, InterpError> {
        let v = self.eval_expr(expr)?;
        match v {
            Value::Future(state) => {
                // Run the executor to ensure all deferred futures are completed
                crate::interp::value::executor_run();

                let mut state = state.lock()
                    .map_err(|e| InterpError::new(format!("await lock failed: {}", e)))?;

                // After executor_run, deferred futures should be Ready.
                // If still Deferred, poll inline.
                crate::interp::value::poll_deferred(&mut state);

                match &mut *state {
                    crate::interp::value::PollFuture::Ready(result) => {
                        match std::mem::replace(result, Err(InterpError::new("future already consumed"))) {
                            Ok(v) => Ok(v),
                            Err(e) => Err(e),
                        }
                    }
                    crate::interp::value::PollFuture::Pending(rx) => {
                        rx.recv()
                            .map_err(|e| InterpError::new(format!("await recv failed: {}", e)))?
                    }
                    crate::interp::value::PollFuture::Deferred { .. } => {
                        Err(InterpError::new("future still deferred after polling"))
                    }
                }
            }
            other => Ok(other),
        }
    }

    pub(in crate::interp) fn eval_lambda(&mut self, params: &[Param], ret: &Option<Type>, body: &Block) -> Result<Value, InterpError> {
        // Collect free variables from the lambda body
        let param_names: std::collections::HashSet<String> =
            params.iter().map(|p| p.name.clone()).collect();
        let free_vars = collect_free_vars(body, &param_names);
        // Only capture variables that are actually used
        let mut captured = HashMap::new();
        for scope in self.env.iter().rev() {
            for (name, val) in scope {
                if free_vars.contains(name) && !captured.contains_key(name) {
                    captured.insert(name.clone(), val.clone());
                }
            }
        }
        Ok(Value::Closure {
            params: params.to_vec(),
            ret: ret.clone(),
            body: body.clone(),
            captured,
        })
    }

    pub(in crate::interp) fn eval_turbofish(&mut self, name: &str, type_args: &[Type], args: &[Expr]) -> Result<Value, InterpError> {
        // Special case: from_json::<T>(s) — typed JSON deserialization
        if name == "from_json" && !type_args.is_empty() {
            if args.len() != 1 {
                return Err(InterpError::new("from_json::<T> expects 1 argument (json string)"));
            }
            let s_val = self.eval_expr(&args[0])?;
            let json_str = match &s_val {
                Value::String(s) => s.clone(),
                _ => return Err(InterpError::new("from_json::<T> expects a string argument")),
            };
            let jv: serde_json::Value = serde_json::from_str(&json_str)
                .map_err(|e| InterpError::new(format!("JSON parse error: {}", e)))?;
            let val = self.json_to_value(&jv);
            return self.coerce_value_to_type(val, &type_args[0]);
        }
        // Turbofish: func::<Type>(args) — evaluate args and call the function
        // Type arguments are ignored at runtime (monomorphization happens at compile time)
        let func = self.find_function(name)
            .ok_or_else(|| InterpError::new(format!("undefined function '{}'", name)))?;
        let mut arg_vals = Vec::new();
        for arg in args {
            arg_vals.push(self.eval_expr(arg)?);
        }
        self.call_func(&func, arg_vals)
    }

    pub(in crate::interp) fn eval_comptime(&mut self, block: &Block) -> Result<Value, InterpError> {
        // Comptime block: evaluate all statements, return last expression value
        let mut result = Value::Unit;
        let len = block.len();
        for (i, stmt) in block.iter().enumerate() {
            let is_last = i == len - 1;
            match stmt {
                Stmt::Expr(e) if is_last => {
                    result = self.eval_expr(e)?;
                }
                Stmt::Expr(e) => {
                    self.eval_expr(e)?;
                }
                _ => {
                    if let Some(v) = self.eval_stmt(stmt)? {
                        result = v;
                    }
                }
            }
        }
        Ok(result)
    }

    pub(in crate::interp) fn eval_type_of(&mut self, expr: &Expr) -> Result<Value, InterpError> {
        let val = self.eval_expr(expr)?;
        let type_name = self.value_type_name(&val);
        Ok(Value::String(type_name))
    }

    pub(in crate::interp) fn eval_type_info(&mut self, ty: &Type) -> Result<Value, InterpError> {
        let type_name = self.resolve_type_name(ty);
        let info = self.type_info_for(&type_name)?;
        Ok(info)
    }

    pub(in crate::interp) fn eval_range(&mut self, start: &Expr, end: &Expr) -> Result<Value, InterpError> {
        let start_val = self.eval_expr(start)?;
        let end_val = self.eval_expr(end)?;
        match (start_val, end_val) {
            (Value::Int(s), Value::Int(e)) => Ok(Value::Range { start: s, end: e }),
            _ => Err(InterpError::new("range requires integer operands")),
        }
    }

    pub(in crate::interp) fn coerce_value_to_type(&self, val: Value, target: &Type) -> Result<Value, InterpError> {
        match target {
            Type::Name(name, type_args) => match name.as_str() {
                "i32" | "i64" | "i8" | "i16" => match val {
                    Value::Int(n) => Ok(Value::Int(n)),
                    Value::Float(f) => Ok(Value::Int(f as i64)),
                    _ => Err(InterpError::new(format!("expected integer, got {}", val))),
                },
                "f32" | "f64" => match val {
                    Value::Float(f) => Ok(Value::Float(f)),
                    Value::Int(n) => Ok(Value::Float(n as f64)),
                    _ => Err(InterpError::new(format!("expected float, got {}", val))),
                },
                "string" => match val {
                    Value::String(s) => Ok(Value::String(s)),
                    _ => Err(InterpError::new(format!("expected string, got {}", val))),
                },
                "bool" => match val {
                    Value::Bool(b) => Ok(Value::Bool(b)),
                    _ => Err(InterpError::new(format!("expected bool, got {}", val))),
                },
                "unit" => Ok(Value::Unit),
                "List" if type_args.len() == 1 => match val {
                    Value::List(items) => {
                        let converted: Result<Vec<Value>, _> = items
                            .into_iter()
                            .map(|item| self.coerce_value_to_type(item, &type_args[0]))
                            .collect();
                        Ok(Value::List(converted?))
                    }
                    _ => Err(InterpError::new(format!("expected list, got {}", val))),
                },
                "Option" if type_args.len() == 1 => match val {
                    Value::Unit => Ok(Value::Unit),
                    val => {
                        let inner_val = self.coerce_value_to_type(val, &type_args[0])?;
                        Ok(Value::Variant("Some".into(), vec![inner_val]))
                    }
                },
                "Result" if type_args.len() == 2 => match val {
                    Value::Variant(name, payload) if name == "Ok" => {
                        let converted = payload
                            .into_iter()
                            .map(|v| self.coerce_value_to_type(v, &type_args[0]))
                            .collect::<Result<Vec<_>, _>>()?;
                        Ok(Value::Variant("Ok".to_string(), converted))
                    }
                    Value::Variant(name, payload) if name == "Err" => {
                        let converted = payload
                            .into_iter()
                            .map(|v| self.coerce_value_to_type(v, &type_args[1]))
                            .collect::<Result<Vec<_>, _>>()?;
                        Ok(Value::Variant("Err".to_string(), converted))
                    }
                    _ => Err(InterpError::new(format!("expected Result, got {}", val))),
                },
                _ => {
                    if let Some(type_def) = self.type_defs.get(name) {
                        match &type_def.kind {
                            TypeDefKind::Record(fields) => match val {
                                Value::Record(_, mut existing_fields) => {
                                    let mut typed_fields = HashMap::new();
                                    for field in fields {
                                        let field_val = existing_fields.remove(&field.name).ok_or_else(|| {
                                            InterpError::new(format!(
                                                "missing field '{}' in JSON for type '{}'",
                                                field.name, name
                                            ))
                                        })?;
                                        typed_fields.insert(
                                            field.name.clone(),
                                            self.coerce_value_to_type(field_val, &field.ty)?,
                                        );
                                    }
                                    Ok(Value::Record(Some(name.clone()), typed_fields))
                                }
                                _ => Err(InterpError::new(format!(
                                    "expected object for type '{}', got {}",
                                    name, val
                                ))),
                            },
                            TypeDefKind::Alias(inner_type) => {
                                self.coerce_value_to_type(val, inner_type)
                            }
                            TypeDefKind::Newtype(inner_type) => {
                                let inner_val = self.coerce_value_to_type(val, inner_type)?;
                                let mut map = HashMap::new();
                                map.insert("value".to_string(), inner_val);
                                Ok(Value::Record(Some(name.clone()), map))
                            }
                            TypeDefKind::Enum(variants) => match val {
                                Value::String(s) => {
                                    if variants.iter().any(|v| v.name == s && v.payload.is_none()) {
                                        Ok(Value::Variant(s, vec![]))
                                    } else {
                                        Err(InterpError::new(format!(
                                            "unknown or payload-bearing variant '{}' for type '{}'",
                                            s, name
                                        )))
                                    }
                                }
                                Value::Record(_, mut fields) => {
                                    if fields.len() != 1 {
                                        Err(InterpError::new(format!(
                                            "enum '{}' expects exactly one key in JSON object",
                                            name
                                        )))
                                    } else {
                                        let (var_name, payload_val) = fields.drain().next().expect("enum variant fields should non-empty");
                                        let variant = variants
                                            .iter()
                                            .find(|v| v.name == var_name)
                                            .ok_or_else(|| {
                                                InterpError::new(format!(
                                                    "unknown variant '{}' for type '{}'",
                                                    var_name, name
                                                ))
                                            })?;
                                        match &variant.payload {
                                            None => Ok(Value::Variant(var_name, vec![])),
                                            Some(VariantPayload::Tuple(types)) => {
                                                let payload_items = match payload_val {
                                                    Value::List(items) => items,
                                                    _ => vec![payload_val],
                                                };
                                                if payload_items.len() != types.len() {
                                                    return Err(InterpError::new(format!(
                                                        "variant '{}' expects {} payload fields, got {}",
                                                        var_name,
                                                        types.len(),
                                                        payload_items.len()
                                                    )));
                                                }
                                                let converted: Result<Vec<Value>, _> =
                                                    payload_items
                                                        .into_iter()
                                                        .zip(types.iter())
                                                        .map(|(item, ty)| {
                                                            self.coerce_value_to_type(item, ty)
                                                        })
                                                        .collect();
                                                Ok(Value::Variant(var_name, converted?))
                                            }
                                            Some(VariantPayload::Record(fields_def)) => {
                                                let payload_map = match payload_val {
                                                    Value::Record(_, map) => map,
                                                    v => {
                                                        let mut map = HashMap::new();
                                                        map.insert("value".to_string(), v);
                                                        map
                                                    }
                                                };
                                                let mut typed_fields = HashMap::new();
                                                for fdef in fields_def {
                                                    let fval = payload_map
                                                        .get(&fdef.name)
                                                        .cloned()
                                                        .unwrap_or(Value::Unit);
                                                    typed_fields.insert(
                                                        fdef.name.clone(),
                                                        self.coerce_value_to_type(fval, &fdef.ty)?,
                                                    );
                                                }
                                                Ok(Value::Variant(
                                                    var_name.clone(),
                                                    vec![Value::Record(
                                                        Some(format!("{}_{}", name, var_name)),
                                                        typed_fields,
                                                    )],
                                                ))
                                            }
                                        }
                                    }
                                }
                                _ => Err(InterpError::new(format!(
                                    "expected string or object for enum '{}', got {}",
                                    name, val
                                ))),
                            },
                            _ => Err(InterpError::new(format!(
                                "cannot deserialize JSON to type '{}'",
                                name
                            ))),
                        }
                    } else {
                        // Unknown named type (e.g., generic type parameter resolved to concrete)
                        // Return the value as-is
                        Ok(val)
                    }
                }
            },
            Type::Option(inner) => match val {
                Value::Unit => Ok(Value::Unit),
                val => {
                    let inner_val = self.coerce_value_to_type(val, inner)?;
                    Ok(Value::Variant("Some".into(), vec![inner_val]))
                }
            },
            Type::Result(ok, err) => match val {
                Value::Variant(name, payload) if name == "Ok" => {
                    let converted = payload
                        .into_iter()
                        .map(|v| self.coerce_value_to_type(v, ok))
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(Value::Variant("Ok".to_string(), converted))
                }
                Value::Variant(name, payload) if name == "Err" => {
                    let converted = payload
                        .into_iter()
                        .map(|v| self.coerce_value_to_type(v, err))
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(Value::Variant("Err".to_string(), converted))
                }
                _ => Err(InterpError::new(format!("expected Result, got {}", val))),
            },
            Type::Tuple(types) => match val {
                Value::List(items) | Value::Tuple(items) => {
                    if items.len() != types.len() {
                        Err(InterpError::new(format!(
                            "expected tuple of {} elements, got {}",
                            types.len(),
                            items.len()
                        )))
                    } else {
                        let converted: Result<Vec<Value>, _> = items
                            .into_iter()
                            .zip(types.iter())
                            .map(|(item, ty)| self.coerce_value_to_type(item, ty))
                            .collect();
                        Ok(Value::Tuple(converted?))
                    }
                }
                _ => Err(InterpError::new(format!("expected tuple, got {}", val))),
            },
            _ => Err(InterpError::new(format!(
                "unsupported target type for JSON deserialization: {:?}",
                target
            ))),
        }
    }
}
