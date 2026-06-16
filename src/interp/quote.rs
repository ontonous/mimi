use super::*;

impl<'a> Interpreter<'a> {
    pub(crate) fn quote_block(&mut self, block: &Block) -> Result<QuotedAst, String> {
        let mut quoted_stmts = Vec::new();
        for stmt in block {
            if let Some(q) = self.quote_stmt(stmt)? {
                quoted_stmts.push(q);
            }
        }
        Ok(QuotedAst::Block(quoted_stmts))
    }

    /// Convert a single statement into a quoted AST (None for desc/rule/etc)
    fn quote_stmt(&mut self, stmt: &Stmt) -> Result<Option<QuotedAst>, String> {
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
            Stmt::Desc(_) | Stmt::Requires(_) | Stmt::Ensures(_) | Stmt::Math(_) | Stmt::Ellipsis | Stmt::MmsBlock { .. } => Ok(None),
            _ => Ok(None),
        }
    }

    /// Convert an expression into a quoted AST
    fn quote_expr(&mut self, expr: &Expr) -> Result<QuotedAst, String> {
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
                    _ => return Err("comprehension requires a list".into()),
                };
                let mut result = Vec::new();
                for item in items {
                    self.push_scope();
                    self.bind(var, item.clone());
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
                let q_fields: Result<Vec<RecordFieldExprQuoted>, String> = fields.iter().map(|f| {
                    Ok(RecordFieldExprQuoted {
                        name: f.name.clone(),
                        value: self.quote_expr(&f.value)?,
                    })
                }).collect();
                Ok(QuotedAst::Record { ty: ty.clone(), fields: q_fields? })
            }
            Expr::Match(subject, arms) => {
                let q_subject = Box::new(self.quote_expr(subject)?);
                let q_arms: Result<Vec<MatchArmQuoted>, String> = arms.iter().map(|arm| {
                    Ok(MatchArmQuoted {
                        pat: arm.pat.clone(),
                        guard: arm.guard.as_ref().map(|g| self.quote_expr(g)).transpose()?,
                        body: self.quote_expr(&arm.body)?,
                    })
                }).collect();
                Ok(QuotedAst::Match(q_subject, q_arms?))
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
            Expr::TypeInfo(_ty) => {
                // Quote type_info as a function call
                Ok(QuotedAst::Call(Box::new(QuotedAst::Ident("type_info".into())), vec![]))
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
        }
    }

    pub(crate) fn eval_quoted_ast(&mut self, qa: &QuotedAst) -> Result<Value, String> {
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
                } else {
                    Err(format!("undefined variable '{}' in quoted AST", name))
                }
            }
            QuotedAst::Binary(op, l, r) => {
                let lv = self.eval_quoted_ast(l)?;
                let rv = self.eval_quoted_ast(r)?;
                match op {
                    BinOp::Add => {
                        match (&lv, &rv) {
                            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
                            (Value::String(a), Value::String(b)) => Ok(Value::String(format!("{}{}", a, b))),
                            _ => Err(format!("unsupported + for {} and {}", lv, rv)),
                        }
                    }
                    BinOp::Sub => {
                        match (&lv, &rv) {
                            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a - b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),
                            _ => Err(format!("unsupported - for {} and {}", lv, rv)),
                        }
                    }
                    BinOp::Mul => {
                        match (&lv, &rv) {
                            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a * b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
                            _ => Err(format!("unsupported * for {} and {}", lv, rv)),
                        }
                    }
                    BinOp::Div => {
                        match (&lv, &rv) {
                            (Value::Int(a), Value::Int(b)) => {
                                if *b == 0 { return Err("division by zero".into()); }
                                Ok(Value::Int(a / b))
                            }
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a / b)),
                            _ => Err(format!("unsupported / for {} and {}", lv, rv)),
                        }
                    }
                    BinOp::EqCmp => Ok(Value::Bool(values_equal(&lv, &rv))),
                    BinOp::NeCmp => Ok(Value::Bool(!values_equal(&lv, &rv))),
                    BinOp::Lt => {
                        match (&lv, &rv) {
                            (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a < b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a < b)),
                            _ => Err(format!("unsupported < for {} and {}", lv, rv)),
                        }
                    }
                    BinOp::Gt => {
                        match (&lv, &rv) {
                            (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a > b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a > b)),
                            _ => Err(format!("unsupported > for {} and {}", lv, rv)),
                        }
                    }
                    _ => Err("unsupported binary op in quoted AST".into()),
                }
            }
            QuotedAst::Unary(op, e) => {
                let v = self.eval_quoted_ast(e)?;
                match op {
                    UnOp::Neg => match v {
                        Value::Int(n) => Ok(Value::Int(-n)),
                        Value::Float(n) => Ok(Value::Float(-n)),
                        _ => Err(format!("unsupported neg for {}", v)),
                    },
                    UnOp::Not => match v {
                        Value::Bool(b) => Ok(Value::Bool(!b)),
                        _ => Err(format!("unsupported not for {}", v)),
                    },
                    _ => Err("unsupported unary op in quoted AST".into()),
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
                self.bind(name, v.clone());
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
                    Value::Closure { params, ret: _, body, captured } => {
                        if params.len() != arg_vals.len() {
                            return Err(format!("closure expects {} args, got {}", params.len(), arg_vals.len()));
                        }
                        self.push_scope();
                        for (n, v) in &captured {
                            self.bind(n, v.clone());
                        }
                        for (p, a) in params.iter().zip(arg_vals) {
                            self.bind(&p.name, a);
                        }
                        let result = self.eval_block(&body);
                        self.pop_scope();
                        result.map(|v| v.unwrap_or(Value::Unit))
                    }
                    _ => Err("cannot call non-closure in quoted AST".into()),
                }
            }
            _ => Err(format!("unsupported quoted AST node: {:?}", qa)),
        }
    }
}
