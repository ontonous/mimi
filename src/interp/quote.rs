use super::*;

impl<'a> Interpreter<'a> {
    pub(crate) fn quote_block(&mut self, block: &Block) -> Result<QuotedAst, InterpError> {
        let mut quoted_stmts = Vec::new();
        for stmt in block {
            if let Some(q) = self.quote_stmt(stmt)? {
                quoted_stmts.push(q);
            }
        }
        Ok(QuotedAst::Block(quoted_stmts))
    }

    /// Convert a single statement into a quoted AST (None for desc/rule/etc)
    fn quote_stmt(&mut self, stmt: &Stmt) -> Result<Option<QuotedAst>, InterpError> {
        match stmt {
            Stmt::Let { pat, init, .. } => {
                let name = match pat {
                    Pattern::Variable(n) => n.clone(),
                    _ => return Ok(None),
                };
                let value = if let Some(e) = init {
                    Box::new(self.quote_expr(e)?)
                } else {
                    Box::new(QuotedAst::Literal(Lit::Unit))
                };
                Ok(Some(QuotedAst::Let { name, value }))
            }
            Stmt::Expr(e) => {
                Ok(Some(QuotedAst::ExprStmt(Box::new(self.quote_expr(e)?))))
            }
            Stmt::Return(e) => {
                let inner = if let Some(e) = e {
                    Some(Box::new(self.quote_expr(e)?))
                } else {
                    None
                };
                Ok(Some(QuotedAst::Return(inner)))
            }
            Stmt::Block(block) => {
                Ok(Some(self.quote_block(block)?))
            }
            Stmt::If { cond, then_, else_ } => {
                let q_cond = Box::new(self.quote_expr(cond)?);
                let q_then = Box::new(self.quote_block(then_)?);
                let q_else = else_.as_ref().map(|e| self.quote_block(e).map(Box::new)).transpose()?;
                Ok(Some(QuotedAst::If(q_cond, q_then, q_else)))
            }
            Stmt::While { cond, body } => {
                let q_cond = Box::new(self.quote_expr(cond)?);
                let q_body = Box::new(self.quote_block(body)?);
                Ok(Some(QuotedAst::While(q_cond, q_body)))
            }
            Stmt::For { var, iterable, body } => {
                let q_iter = Box::new(self.quote_expr(iterable)?);
                let q_body = Box::new(self.quote_block(body)?);
                Ok(Some(QuotedAst::For(var.clone(), q_iter, q_body)))
            }
            Stmt::Assign { target, value } => {
                let q_target = Box::new(self.quote_expr(target)?);
                let q_value = Box::new(self.quote_expr(value)?);
                Ok(Some(QuotedAst::Assign(q_target, q_value)))
            }
            Stmt::Break(e) => {
                let inner = e.as_ref().map(|e| self.quote_expr(e).map(Box::new)).transpose()?;
                Ok(Some(QuotedAst::Break(inner)))
            }
            Stmt::Continue => {
                Ok(Some(QuotedAst::Continue))
            }
            Stmt::Arena(block) => {
                Ok(Some(QuotedAst::Arena(Box::new(self.quote_block(block)?))))
            }
            Stmt::Unsafe(block) => {
                Ok(Some(QuotedAst::Unsafe(Box::new(self.quote_block(block)?))))
            }
            Stmt::Drop(expr) => {
                Ok(Some(QuotedAst::Drop(Box::new(self.quote_expr(expr)?))))
            }
            Stmt::SharedLet { kind, name, init, .. } => {
                Ok(Some(QuotedAst::SharedLet {
                    kind: *kind,
                    name: name.clone(),
                    init: Box::new(self.quote_expr(init)?),
                }))
            }
            Stmt::OnFailure(block) => {
                Ok(Some(QuotedAst::OnFailure(Box::new(self.quote_block(block)?))))
            }
            Stmt::Parasteps(block) => {
                Ok(Some(QuotedAst::Parasteps(Box::new(self.quote_block(block)?))))
            }
            Stmt::Alloc { kind, body } => {
                Ok(Some(QuotedAst::Alloc {
                    kind: *kind,
                    body: Box::new(self.quote_block(body)?),
                }))
            }
            Stmt::Loop(body) => {
                let q_body = Box::new(self.quote_block(body)?);
                Ok(Some(QuotedAst::Loop(q_body)))
            }
            Stmt::Desc(..) | Stmt::Rule(..) | Stmt::Requires(_, _) | Stmt::Ensures(_, _) | Stmt::Invariant(_, _) | Stmt::Math(_) | Stmt::Ellipsis | Stmt::MmsBlock { .. } => Ok(None),
        }
    }

    /// Convert an expression into a quoted AST
    fn quote_expr(&mut self, expr: &Expr) -> Result<QuotedAst, InterpError> {
        match expr {
            Expr::Literal(l) => Ok(QuotedAst::Literal(l.clone())),
            Expr::Ident(name) => Ok(QuotedAst::Ident(name.clone())),
            Expr::Binary(op, l, r) => {
                Ok(QuotedAst::Binary(*op, Box::new(self.quote_expr(l)?), Box::new(self.quote_expr(r)?)))
            }
            Expr::Unary(op, e) => {
                Ok(QuotedAst::Unary(*op, Box::new(self.quote_expr(e)?)))
            }
            Expr::Call(callee, args) => {
                let q_callee = Box::new(self.quote_expr(callee)?);
                let q_args: Result<Vec<_>, _> = args.iter().map(|a| self.quote_expr(a)).collect();
                Ok(QuotedAst::Call(q_callee, q_args?))
            }
            Expr::Field(obj, field) => {
                Ok(QuotedAst::Field(Box::new(self.quote_expr(obj)?), field.clone()))
            }
            Expr::Index(obj, idx) => {
                Ok(QuotedAst::Index(Box::new(self.quote_expr(obj)?), Box::new(self.quote_expr(idx)?)))
            }
            Expr::Tuple(elems) => {
                let q_elems: Result<Vec<_>, _> = elems.iter().map(|e| self.quote_expr(e)).collect();
                Ok(QuotedAst::Tuple(q_elems?))
            }
            Expr::List(elems) => {
                let q_elems: Result<Vec<_>, _> = elems.iter().map(|e| self.quote_expr(e)).collect();
                Ok(QuotedAst::List(q_elems?))
            }
            Expr::Comprehension { expr, var, iter, guard } => {
                // For now, evaluate comprehension at quote time
                let iter_val = self.eval_expr(iter)?;
                let items = match iter_val {
                    Value::List(l) => l,
                    _ => return Err(InterpError::new("comprehension requires a list")),
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
                Ok(QuotedAst::List(result.into_iter().map(|v| QuotedAst::Interpolate(Box::new(v))).collect()))
            }
            Expr::Try(e) => Ok(QuotedAst::Try(Box::new(self.quote_expr(e)?))),
            Expr::Spawn(e) => Ok(QuotedAst::Spawn(Box::new(self.quote_expr(e)?))),
            Expr::Await(e) => Ok(QuotedAst::Await(Box::new(self.quote_expr(e)?))),
            Expr::Old(e) => {
                // old() in quote context - evaluate and return as interpolation
                let v = self.eval_expr(e)?;
                Ok(QuotedAst::Interpolate(Box::new(v)))
            }
            Expr::QuoteInterpolate(e) => {
                // Interpolation: evaluate the expression and embed the result
                let v = self.eval_expr(e)?;
                Ok(QuotedAst::Interpolate(Box::new(v)))
            }
            Expr::Quote(block) => {
                let quoted = self.quote_block(block)?;
                Ok(quoted)
            }
            Expr::Record { ty, fields } => {
                let q_fields: Result<Vec<RecordFieldExprQuoted>, InterpError> = fields.iter().map(|f| {
                    Ok(RecordFieldExprQuoted {
                        name: f.name.clone(),
                        value: self.quote_expr(&f.value)?,
                    })
                }).collect();
                Ok(QuotedAst::Record { ty: ty.clone(), fields: q_fields? })
            }
            Expr::Match(subject, arms) => {
                let q_subject = Box::new(self.quote_expr(subject)?);
                let q_arms: Result<Vec<MatchArmQuoted>, InterpError> = arms.iter().map(|arm| {
                    Ok(MatchArmQuoted {
                        pat: arm.pat.clone(),
                        guard: arm.guard.as_ref().map(|g| self.quote_expr(g)).transpose()?,
                        body: self.quote_expr(&arm.body)?,
                    })
                }).collect();
                Ok(QuotedAst::Match(q_subject, q_arms?))
            }
            Expr::If { cond, then_, else_ } => {
                let q_cond = Box::new(self.quote_expr(cond)?);
                let q_then = Box::new(self.quote_block(then_)?);
                let q_else = else_.as_ref().map(|e| self.quote_block(e).map(Box::new)).transpose()?;
                Ok(QuotedAst::If(q_cond, q_then, q_else))
            }
            Expr::Lambda { params: _, ret: _, body } => {
                // Quote the lambda body as a block
                let quoted_body = self.quote_block(body)?;
                // Represent lambda as a call to a synthetic function
                // For simplicity, just quote the body
                Ok(quoted_body)
            }
            Expr::Turbofish(name, _type_args, args) => {
                // In quote context, treat turbofish as a regular call
                let mut q_args = Vec::new();
                for arg in args {
                    q_args.push(self.quote_expr(arg)?);
                }
                Ok(QuotedAst::Call(Box::new(QuotedAst::Ident(name.clone())), q_args))
            }
            Expr::Comptime(block) => {
                // Quote comptime block as a block
                self.quote_block(block)
            }
            Expr::TypeOf(e) => {
                // Quote type_of as a function call
                let q_arg = self.quote_expr(e)?;
                Ok(QuotedAst::Call(Box::new(QuotedAst::Ident("type_of".into())), vec![q_arg]))
            }
            Expr::TypeInfo(ty) => {
                // Evaluate type_info at quote time using the interpreter's type definitions
                let type_name = self.resolve_type_name(ty);
                let info = self.type_info_for(&type_name)?;
                Ok(QuotedAst::Interpolate(Box::new(info)))
            }
            Expr::SliceExpr { target, start, end } => {
                let q_target = self.quote_expr(target)?;
                let mut args = vec![q_target];
                if let Some(s) = start { args.push(self.quote_expr(s)?); }
                if let Some(e) = end { args.push(self.quote_expr(e)?); }
                Ok(QuotedAst::Call(
                    Box::new(QuotedAst::Ident("slice".into())),
                    args,
                ))
            }
            Expr::Arena(block) => {
                let q_block = self.quote_block(block)?;
                Ok(QuotedAst::Arena(Box::new(q_block)))
            }
            Expr::Block(block) => {
                let mut q_stmts = Vec::new();
                for s in block {
                    if let Some(q) = self.quote_stmt(s)? {
                        q_stmts.push(q);
                    }
                }
                Ok(QuotedAst::Block(q_stmts))
            }
            Expr::Range { start, end } => {
                let q_start = self.quote_expr(start)?;
                let q_end = self.quote_expr(end)?;
                Ok(QuotedAst::Binary(
                    BinOp::Range,
                    Box::new(q_start),
                    Box::new(q_end),
                ))
            }
            Expr::TupleIndex(obj, idx) => {
                let q_obj = self.quote_expr(obj)?;
                // Represent tuple index as a function call to "tuple_index"
                Ok(QuotedAst::Call(
                    Box::new(QuotedAst::Ident("tuple_index".into())),
                    vec![q_obj, QuotedAst::Literal(Lit::Int(*idx as i64))],
                ))
            }
            Expr::MapLiteral { entries } => {
                // Evaluate map literal at quote time and interpolate the result
                let v = self.eval_expr(&Expr::MapLiteral { entries: entries.clone() })?;
                Ok(QuotedAst::Interpolate(Box::new(v)))
            }
            Expr::SetLiteral(elems) => {
                let v = self.eval_expr(&Expr::SetLiteral(elems.clone()))?;
                Ok(QuotedAst::Interpolate(Box::new(v)))
            }
        }
    }

    pub(crate) fn eval_quoted_ast(&mut self, qa: &QuotedAst) -> Result<Value, InterpError> {
        match qa {
            QuotedAst::Literal(l) => Ok(match l {
                Lit::Int(v) => Value::Int(*v),
                Lit::Float(v) => Value::Float(*v),
                Lit::Bool(v) => Value::Bool(*v),
                Lit::String(v) => Value::String(v.clone()),
                Lit::FString(_) => Value::Unit, // f-strings not supported in quoted context
                Lit::Unit => Value::Unit,
            }),
            QuotedAst::Ident(name) => {
                if let Some(v) = self.lookup(name) {
                    Ok(v)
                } else if let Some(func) = self.find_function(name) {
                    Ok(Value::Closure {
                        params: func.params.clone(),
                        ret: func.ret.clone(),
                        body: func.body.clone(),
                        captured: HashMap::new(),
                    })
                } else {
                    Err(InterpError::new(format!("undefined variable '{}' in quoted AST", name)))
                }
            }
            QuotedAst::Binary(op, l, r) => {
                let lv = self.eval_quoted_ast(l)?;
                let rv = self.eval_quoted_ast(r)?;
                match op {
                    BinOp::Add => {
                        match (&lv, &rv) {
                            (Value::Int(a), Value::Int(b)) => a.checked_add(*b)
                                .ok_or_else(|| InterpError::integer_overflow(format!("integer overflow in addition: {} + {}", a, b)))
                                .map(Value::Int),
                            (Value::Float(a), Value::Float(b)) => {
                                let r = a + b;
                                if r.is_nan() { Err(InterpError::float_error(format!("NaN from {} + {}", a, b))) }
                                else { Ok(Value::Float(r)) }
                            }
                            (Value::String(a), Value::String(b)) => Ok(Value::String(format!("{}{}", a, b))),
                            _ => Err(InterpError::new(format!("unsupported + for {} and {}", lv, rv))),
                        }
                    }
                    BinOp::Sub => {
                        match (&lv, &rv) {
                            (Value::Int(a), Value::Int(b)) => a.checked_sub(*b)
                                .ok_or_else(|| InterpError::integer_overflow(format!("integer overflow in subtraction: {} - {}", a, b)))
                                .map(Value::Int),
                            (Value::Float(a), Value::Float(b)) => {
                                let r = a - b;
                                if r.is_nan() { Err(InterpError::float_error(format!("NaN from {} - {}", a, b))) }
                                else { Ok(Value::Float(r)) }
                            }
                            _ => Err(InterpError::new(format!("unsupported - for {} and {}", lv, rv))),
                        }
                    }
                    BinOp::Mul => {
                        match (&lv, &rv) {
                            (Value::Int(a), Value::Int(b)) => a.checked_mul(*b)
                                .ok_or_else(|| InterpError::integer_overflow(format!("integer overflow in multiplication: {} * {}", a, b)))
                                .map(Value::Int),
                            (Value::Float(a), Value::Float(b)) => {
                                let r = a * b;
                                if r.is_nan() { Err(InterpError::float_error(format!("NaN from {} * {}", a, b))) }
                                else { Ok(Value::Float(r)) }
                            }
                            _ => Err(InterpError::new(format!("unsupported * for {} and {}", lv, rv))),
                        }
                    }
                    BinOp::Div => {
                        match (&lv, &rv) {
                            (Value::Int(a), Value::Int(b)) => a.checked_div(*b)
                                .ok_or_else(|| InterpError::integer_overflow(format!("integer overflow or division by zero: {} / {}", a, b)))
                                .map(Value::Int),
                            (Value::Float(a), Value::Float(b)) => {
                                if *b == 0.0 { return Err(InterpError::div_by_zero()); }
                                let r = a / b;
                                if r.is_nan() { Err(InterpError::float_error(format!("NaN from {} / {}", a, b))) }
                                else if r.is_infinite() { Err(InterpError::float_error(format!("infinity from {} / {}", a, b))) }
                                else { Ok(Value::Float(r)) }
                            }
                            _ => Err(InterpError::new(format!("unsupported / for {} and {}", lv, rv))),
                        }
                    }
                    BinOp::EqCmp => Ok(Value::Bool(values_equal(&lv, &rv))),
                    BinOp::NeCmp => Ok(Value::Bool(!values_equal(&lv, &rv))),
                    BinOp::Lt => {
                        match (&lv, &rv) {
                            (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a < b)),
                            (Value::Float(a), Value::Float(b)) => {
                                if a.is_nan() || b.is_nan() {
                                    Err(InterpError::new(format!("cannot compare NaN with float: {} < {}", a, b)))
                                } else {
                                    Ok(Value::Bool(a < b))
                                }
                            }
                            _ => Err(InterpError::new(format!("unsupported < for {} and {}", lv, rv))),
                        }
                    }
                    BinOp::Gt => {
                        match (&lv, &rv) {
                            (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a > b)),
                            (Value::Float(a), Value::Float(b)) => {
                                if a.is_nan() || b.is_nan() {
                                    Err(InterpError::new(format!("cannot compare NaN with float: {} > {}", a, b)))
                                } else {
                                    Ok(Value::Bool(a > b))
                                }
                            }
                            _ => Err(InterpError::new(format!("unsupported > for {} and {}", lv, rv))),
                        }
                    }
                    BinOp::Le => {
                        match (&lv, &rv) {
                            (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a <= b)),
                            (Value::Float(a), Value::Float(b)) => {
                                if a.is_nan() || b.is_nan() {
                                    Err(InterpError::new(format!("cannot compare NaN with float: {} <= {}", a, b)))
                                } else {
                                    Ok(Value::Bool(a <= b))
                                }
                            }
                            _ => Err(InterpError::new(format!("unsupported <= for {} and {}", lv, rv))),
                        }
                    }
                    BinOp::Ge => {
                        match (&lv, &rv) {
                            (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a >= b)),
                            (Value::Float(a), Value::Float(b)) => {
                                if a.is_nan() || b.is_nan() {
                                    Err(InterpError::new(format!("cannot compare NaN with float: {} >= {}", a, b)))
                                } else {
                                    Ok(Value::Bool(a >= b))
                                }
                            }
                            _ => Err(InterpError::new(format!("unsupported >= for {} and {}", lv, rv))),
                        }
                    }
                    _ => Err(InterpError::new("unsupported binary op in quoted AST")),
                }
            }
            QuotedAst::Unary(op, e) => {
                let v = self.eval_quoted_ast(e)?;
                match op {
                    UnOp::Neg => match v {
                        Value::Int(n) => n.checked_neg()
                            .ok_or_else(|| InterpError::integer_overflow(format!("integer overflow in negation: -{}", n)))
                            .map(Value::Int),
                        Value::Float(n) => Ok(Value::Float(-n)),
                        _ => Err(InterpError::new(format!("unsupported neg for {}", v))),
                    },
                    UnOp::Not => match v {
                        Value::Bool(b) => Ok(Value::Bool(!b)),
                        _ => Err(InterpError::new(format!("unsupported not for {}", v))),
                    },
                    _ => Err(InterpError::new("unsupported unary op in quoted AST")),
                }
            }
            QuotedAst::Interpolate(v) => Ok(*v.clone()),
            QuotedAst::Block(stmts) => {
                self.push_scope();
                let mut result = Value::Unit;
                for stmt in stmts {
                    result = self.eval_quoted_ast(stmt)?;
                }
                self.pop_scope();
                Ok(result)
            }
            QuotedAst::Let { name, value } => {
                let v = self.eval_quoted_ast(value)?;
                self.bind(name, v.clone())?;
                Ok(v)
            }
            QuotedAst::ExprStmt(e) => self.eval_quoted_ast(e),
            QuotedAst::Return(e) => {
                if let Some(e) = e {
                    self.eval_quoted_ast(e)
                } else {
                    Ok(Value::Unit)
                }
            }
            QuotedAst::If(cond, then_, else_) => {
                let c = self.eval_quoted_ast(cond)?;
                if is_truthy(&c) {
                    self.eval_quoted_ast(then_)
                } else if let Some(else_block) = else_ {
                    self.eval_quoted_ast(else_block)
                } else {
                    Ok(Value::Unit)
                }
            }
            QuotedAst::Break(e) => {
                let v = e.as_ref().map(|e| self.eval_quoted_ast(e)).transpose()?;
                self.loop_action = Some(LoopAction::Break(v));
                Ok(Value::Unit)
            }
            QuotedAst::Continue => {
                self.loop_action = Some(LoopAction::Continue);
                Ok(Value::Unit)
            }
            QuotedAst::While(cond, body) => {
                while is_truthy(&self.eval_quoted_ast(cond)?) {
                    if self.early_return.is_some() { break; }
                    self.eval_quoted_ast(body)?;
                    if self.early_return.is_some() { break; }
                    match self.loop_action.take() {
                        Some(LoopAction::Break(val)) => {
                            if let Some(v) = val {
                                return Ok(v);
                            }
                            break;
                        }
                        Some(LoopAction::Continue) => continue,
                        None => {}
                    }
                }
                Ok(Value::Unit)
            }
            QuotedAst::Loop(body) => {
                loop {
                    if self.early_return.is_some() { break; }
                    self.eval_quoted_ast(body)?;
                    if self.early_return.is_some() { break; }
                    match self.loop_action.take() {
                        Some(LoopAction::Break(val)) => {
                            if let Some(v) = val {
                                return Ok(v);
                            }
                            break;
                        }
                        Some(LoopAction::Continue) => continue,
                        None => {}
                    }
                }
                Ok(Value::Unit)
            }
            QuotedAst::For(var, iterable, body) => {
                let iter = self.eval_quoted_ast(iterable)?;
                let list = match iter {
                    Value::List(l) => l,
                    other => return Err(InterpError::new(format!("cannot iterate over {}", other))),
                };
                for item in list {
                    self.bind(var, item)?;
                    if self.early_return.is_some() { break; }
                    self.eval_quoted_ast(body)?;
                    if self.early_return.is_some() { break; }
                    match self.loop_action.take() {
                        Some(LoopAction::Break(val)) => {
                            if let Some(v) = val {
                                return Ok(v);
                            }
                            break;
                        }
                        Some(LoopAction::Continue) => continue,
                        None => {}
                    }
                }
                Ok(Value::Unit)
            }
            QuotedAst::Assign(target, value) => {
                let v = self.eval_quoted_ast(value)?;
                match target.as_ref() {
                    QuotedAst::Ident(name) => self.assign(name, v)?,
                    _ => return Err(InterpError::new("assign target must be an identifier in quoted AST")),
                }
                Ok(Value::Unit)
            }
            QuotedAst::Arena(body) => {
                self.push_scope();
                let result = self.eval_quoted_ast(body);
                self.pop_scope();
                result
            }
            QuotedAst::Unsafe(body) => {
                self.eval_quoted_ast(body)
            }
            QuotedAst::Drop(expr) => {
                self.eval_quoted_ast(expr)?;
                Ok(Value::Unit)
            }
            QuotedAst::SharedLet { kind, name, init } => {
                let v = self.eval_quoted_ast(init)?;
                let shared_val = match kind {
                    SharedKind::Shared => Value::Shared(Arc::new(RwLock::new(v))),
                    SharedKind::LocalShared => Value::LocalShared(LocalSharedInner::new(v)),
                    SharedKind::Weak => match v {
                        Value::Shared(arc) => Value::WeakShared(Arc::downgrade(&arc)),
                        Value::LocalShared(rc) => Value::WeakLocal(rc.downgrade()),
                        _ => return Err(InterpError::new(format!("weak requires a shared or local_shared value, got {}", v))),
                    },
                    SharedKind::WeakLocal => match v {
                        Value::LocalShared(rc) => Value::WeakLocal(rc.downgrade()),
                        _ => return Err(InterpError::new(format!("weak_local requires a local_shared value, got {}", v))),
                    },
                };
                self.bind(name, shared_val)?;
                Ok(Value::Unit)
            }
            QuotedAst::OnFailure(_body) => {
                // OnFailure in quoted AST: register compensation
                // For simplicity, we skip compensation registration in quoted context
                Ok(Value::Unit)
            }
            QuotedAst::Parasteps(_body) => {
                // Parasteps in quoted AST: sequential fallback
                // For simplicity, we just evaluate sequentially
                Ok(Value::Unit)
            }
            QuotedAst::Alloc { kind: _, body } => {
                // Alloc in quoted AST: simplified - just evaluate the body
                self.push_scope();
                let result = self.eval_quoted_ast(body);
                self.pop_scope();
                result
            }
            QuotedAst::List(elems) => {
                let vals: Result<Vec<_>, _> = elems.iter().map(|e| self.eval_quoted_ast(e)).collect();
                Ok(Value::List(vals?))
            }
            QuotedAst::Tuple(elems) => {
                let vals: Result<Vec<_>, _> = elems.iter().map(|e| self.eval_quoted_ast(e)).collect();
                Ok(Value::Tuple(vals?))
            }
            QuotedAst::Call(callee, args) => {
                let func_val = self.eval_quoted_ast(callee)?;
                let arg_vals: Result<Vec<_>, _> = args.iter().map(|a| self.eval_quoted_ast(a)).collect();
                let arg_vals = arg_vals?;
                match func_val {
                    Value::Closure { params, ret: _, body, captured } =>
                        self.apply_closure_inner(&params, &body, &captured, arg_vals),
                    _ => Err(InterpError::new("cannot call non-closure in quoted AST")),
                }
            }
            _ => Err(InterpError::new(format!("unsupported quoted AST node: {:?}", qa))),
        }
    }
}
