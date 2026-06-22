use super::*;

mod helpers;
mod expr;
mod stmt;

impl<'a> Interpreter<'a> {
    pub(crate) fn eval_block(&mut self, block: &Block) -> Result<Option<Value>, InterpError> {
        self.push_compensation_scope();
        let result = self.eval_block_inner(block);
        self.pop_compensation_scope(result.is_err() || self.early_return.is_some() || self.exited.is_some());
        result
    }

    fn eval_block_inner(&mut self, block: &Block) -> Result<Option<Value>, InterpError> {
        for (i, stmt) in block.iter().enumerate() {
            let is_last = i == block.len() - 1;
            match stmt {
                Stmt::Expr(e) if is_last => {
                    let result = self.eval_expr(e);
                    // `exit()` inside the final expression must abort the block.
                    if self.exited.is_some() {
                        return Ok(None);
                    }
                    match result {
                        Ok(Value::Error(msg)) => {
                            return Err(InterpError::new(msg));
                        }
                        Ok(v) => return Ok(Some(v)),
                        Err(e) => {
                            return Err(e);
                        }
                    }
                }
                Stmt::Expr(e) => {
                    let result = self.eval_expr(e);
                    // `exit()` inside a side-effect expression must abort the block.
                    if self.exited.is_some() {
                        return Ok(None);
                    }
                    match result {
                        Ok(Value::Error(msg)) => {
                            return Err(InterpError::new(msg));
                        }
                        Ok(_) => {}
                        Err(e) => {
                            return Err(e);
                        }
                    }
                }
                _ => {
                    if let Some(v) = self.eval_stmt(stmt)? {
                        return Ok(Some(v));
                    }
                }
            }
            // Propagate break/continue, early return, and exit signals out of the block
            if self.loop_action.is_some() || self.early_return.is_some() || self.exited.is_some() {
                return Ok(None);
            }
        }
        Ok(None)
    }

    pub(crate) fn eval_stmt(&mut self, stmt: &Stmt) -> Result<Option<Value>, InterpError> {
        match stmt {
            Stmt::Let { pat, init, mut_, ref_, ty } => {
                self.eval_let(pat, init, *mut_, *ref_, ty)?;
            }
            Stmt::Return(e) => return self.eval_return(e),
            Stmt::Break(e) => return self.eval_break(e),
            Stmt::Continue => return self.eval_continue(),
            Stmt::Expr(e) => {
                if let Value::Error(msg) = self.eval_expr(e)? {
                    return Err(InterpError::new(msg));
                }
            }
            Stmt::If { cond, then_, else_ } => {
                if let Some(v) = self.eval_if_stmt(cond, then_, else_)? {
                    return Ok(Some(v));
                }
            }
            Stmt::While { cond, body } => {
                if let Some(v) = self.eval_while(cond, body)? {
                    return Ok(Some(v));
                }
            }
            Stmt::For { var, iterable, body } => {
                if let Some(v) = self.eval_for(var, iterable, body)? {
                    return Ok(Some(v));
                }
            }
            Stmt::Block(block) => {
                if let Some(v) = self.eval_block(block)? {
                    return Ok(Some(v));
                }
            }
            Stmt::Arena(block) => {
                return self.eval_arena_block(block);
            }
            Stmt::Unsafe(block) => {
                // Unsafe block: execute body with no restrictions
                // (at runtime, unsafe has no effect — it's a compile-time annotation)
                return self.eval_block(block);
            }
            Stmt::Alloc { kind, body } => {
                return self.eval_alloc(kind, body);
            }
            Stmt::Assign { target, value } => {
                return self.eval_assign(target, value);
            }
            Stmt::Desc(..) | Stmt::Rule(..) | Stmt::Requires(_, _) | Stmt::Ensures(_, _) | Stmt::Ellipsis | Stmt::MmsBlock { .. } => {}
            Stmt::Math(exprs) => {
                // Math block: evaluate constant expressions at compile time
                for expr in exprs {
                    if let Ok(val) = self.eval_expr(expr) {
                        // Store the result if it's a constant
                        // For now, just evaluate and discard (verification conditions)
                        let _ = val;
                    }
                }
            }
            Stmt::Drop(expr) => {
                // Evaluate and discard the value (for linear capability drops)
                self.eval_expr(expr)?;
                // In a real implementation, this would track capability usage
            }
            Stmt::SharedLet { kind, name, init, .. } => {
                self.eval_shared_let(kind, name, init)?;
            }
            Stmt::OnFailure(block) => {
                self.eval_on_failure(block)?;
            }
            Stmt::Parasteps(block) => {
                return self.eval_parasteps(block);
            }
        }
        Ok(None)
    }

    pub(crate) fn eval_expr(&mut self, expr: &Expr) -> Result<Value, InterpError> {
        match expr {
            Expr::Literal(l) => Ok(match l {
                Lit::Int(v) => Value::Int(*v),
                Lit::Float(v) => Value::Float(*v),
                Lit::Bool(v) => Value::Bool(*v),
                Lit::String(v) => Value::String(v.clone()),
                Lit::FString(parts) => {
                    let mut result = String::new();
                    for part in parts {
                        match part {
                            crate::ast::FStringPart::Text(t) => result.push_str(t),
                            crate::ast::FStringPart::Interp(expr) => {
                                let val = self.eval_expr(expr)?;
                                result.push_str(&val.to_string());
                            }
                        }
                    }
                    Value::String(result)
                }
                Lit::Unit => Value::Unit,
            }),
            Expr::Ident(name) => self.eval_ident(name),
            Expr::Unary(op, e) => self.eval_unary(*op, e),
            Expr::Binary(op, l, r) => self.eval_binary(*op, l, r),
            Expr::Call(callee, args) => self.eval_call(callee, args),
            Expr::Tuple(elems) => self.eval_tuple(elems),
            Expr::TupleIndex(obj, idx) => self.eval_tuple_index(obj, *idx),
            Expr::List(elems) => self.eval_list(elems),
            Expr::Comprehension { expr, var, iter, guard } => self.eval_comprehension(expr, var, iter, guard),
            Expr::If { cond, then_, else_ } => self.eval_if_expr(cond, then_, else_),
            Expr::Arena(block) => self.eval_arena_block(block).map(|v| v.unwrap_or(Value::Unit)),
            Expr::Block(block) => Ok(self.eval_block(block)?.unwrap_or(Value::Unit)),
            Expr::Match(subject, arms) => self.eval_match(subject, arms),
            Expr::Field(obj, field) => self.eval_field(obj, field),
            Expr::Record { ty, fields } => self.eval_record(ty, fields),
            Expr::Index(obj_expr, idx_expr) => self.eval_index(obj_expr, idx_expr),
            Expr::SliceExpr { target, start, end } => self.eval_slice_expr(target, start, end),
            Expr::Try(expr) => self.eval_try(expr),
            Expr::Spawn(expr) => self.eval_spawn(expr),
            Expr::Await(expr) => self.eval_await(expr),
            Expr::QuoteInterpolate(expr) => {
                let v = self.eval_expr(expr)?;
                Ok(Value::QuoteAst(Box::new(QuotedAst::Interpolate(Box::new(v)))))
            }
            Expr::Quote(block) => {
                // Convert the block to QuotedAst
                let quoted = self.quote_block(block)?;
                Ok(Value::QuoteAst(Box::new(quoted)))
            }
            Expr::Old(expr) => {
                // old(x) looks up the snapshot value from before function execution
                if let Expr::Ident(name) = expr.as_ref() {
                    let old_name = format!("old_{}", name);
                    if let Some(v) = self.lookup(&old_name) {
                        return Ok(v);
                    }
                }
                // If not found as old_ variable, evaluate the expression normally
                self.eval_expr(expr)
            }
            Expr::Lambda { params, ret, body } => self.eval_lambda(params, ret, body),
            Expr::Turbofish(name, type_args, args) => self.eval_turbofish(name, type_args, args),
            Expr::Comptime(block) => self.eval_comptime(block),
            Expr::TypeOf(expr) => self.eval_type_of(expr),
            Expr::TypeInfo(ty) => self.eval_type_info(ty),
            Expr::Range { start, end } => self.eval_range(start, end),
        }
    }
}
