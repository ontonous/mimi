use super::*;

impl<'a> Interpreter<'a> {
    pub(crate) fn builtin_map(&mut self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("map expects 2 arguments (list, closure)"));
        }
        match (&args[0], &args[1]) {
            (
                Value::List(l),
                Value::Closure {
                    params,
                    body,
                    captured,
                    ..
                },
            ) => {
                if params.len() != 1 {
                    return Err(InterpError::new("map closure must take 1 argument"));
                }
                let mut result = Vec::new();
                for item in l {
                    if self.early_return.is_some() {
                        break;
                    }
                    self.push_scope();
                    for (n, v) in captured {
                        if let Err(e) = self.bind(n, v.clone()) {
                            self.pop_scope();
                            return Err(e);
                        }
                    }
                    if let Err(e) = self.bind(&params[0].name, item.clone()) {
                        self.pop_scope();
                        return Err(e);
                    }
                    let val = match self.eval_block(body) {
                        Ok(v) => v,
                        Err(e) => {
                            self.pop_scope();
                            return Err(e);
                        }
                    };
                    self.pop_scope();
                    if self.early_return.is_some() {
                        break;
                    }
                    result.push(val.unwrap_or(Value::Unit));
                }
                Ok(Value::List(result))
            }
            _ => Err(InterpError::new("map expects (list, closure)")),
        }
    }

    pub(crate) fn builtin_filter(&mut self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new(
                "filter expects 2 arguments (list, closure)",
            ));
        }
        match (&args[0], &args[1]) {
            (
                Value::List(l),
                Value::Closure {
                    params,
                    body,
                    captured,
                    ..
                },
            ) => {
                if params.len() != 1 {
                    return Err(InterpError::new("filter closure must take 1 argument"));
                }
                let mut result = Vec::new();
                for item in l {
                    if self.early_return.is_some() {
                        break;
                    }
                    self.push_scope();
                    for (n, v) in captured {
                        if let Err(e) = self.bind(n, v.clone()) {
                            self.pop_scope();
                            return Err(e);
                        }
                    }
                    if let Err(e) = self.bind(&params[0].name, item.clone()) {
                        self.pop_scope();
                        return Err(e);
                    }
                    let val = match self.eval_block(body) {
                        Ok(v) => v,
                        Err(e) => {
                            self.pop_scope();
                            return Err(e);
                        }
                    };
                    self.pop_scope();
                    if self.early_return.is_some() {
                        break;
                    }
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
            return Err(InterpError::new(
                "reduce expects 3 arguments (list, closure, initial)",
            ));
        }
        match (&args[0], &args[1]) {
            (
                Value::List(l),
                Value::Closure {
                    params,
                    body,
                    captured,
                    ..
                },
            ) => {
                if params.len() != 2 {
                    return Err(InterpError::new(
                        "reduce closure must take 2 arguments (acc, elem)",
                    ));
                }
                let mut acc = args[2].clone();
                for item in l {
                    if self.early_return.is_some() {
                        break;
                    }
                    self.push_scope();
                    for (n, v) in captured {
                        if let Err(e) = self.bind(n, v.clone()) {
                            self.pop_scope();
                            return Err(e);
                        }
                    }
                    if let Err(e) = self.bind(&params[0].name, acc.clone()) {
                        self.pop_scope();
                        return Err(e);
                    }
                    if let Err(e) = self.bind(&params[1].name, item.clone()) {
                        self.pop_scope();
                        return Err(e);
                    }
                    let val = match self.eval_block(body) {
                        Ok(v) => v,
                        Err(e) => {
                            self.pop_scope();
                            return Err(e);
                        }
                    };
                    self.pop_scope();
                    if self.early_return.is_some() {
                        break;
                    }
                    acc = val.unwrap_or(Value::Unit);
                }
                Ok(acc)
            }
            _ => Err(InterpError::new("reduce expects (list, closure, initial)")),
        }
    }
}
