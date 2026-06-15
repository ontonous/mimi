use crate::ast::*;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
    Unit,
    List(Vec<Value>),
    Tuple(Vec<Value>),
    Variant(String, Vec<Value>),
    Record(HashMap<String, Value>),
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Int(v) => write!(f, "{}", v),
            Value::Float(v) => write!(f, "{}", v),
            Value::Bool(v) => write!(f, "{}", v),
            Value::String(v) => write!(f, "{}", v),
            Value::Unit => write!(f, "()"),
            Value::List(vs) => {
                write!(f, "[")?;
                for (i, v) in vs.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, "]")
            }
            Value::Tuple(vs) => {
                write!(f, "(")?;
                for (i, v) in vs.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, ")")
            }
            Value::Variant(name, vs) => {
                write!(f, "{}(", name)?;
                for (i, v) in vs.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, ")")
            }
            Value::Record(fields) => {
                write!(f, "{{")?;
                let mut first = true;
                for (k, v) in fields.iter() {
                    if !first {
                        write!(f, ", ")?;
                    }
                    first = false;
                    write!(f, "{}: {}", k, v)?;
                }
                write!(f, "}}")
            }
        }
    }
}

pub struct Interpreter<'a> {
    file: &'a File,
    env: Vec<HashMap<String, Value>>,
    constructors: HashMap<String, usize>,
}

impl<'a> Interpreter<'a> {
    pub fn new(file: &'a File) -> Self {
        let mut constructors = HashMap::new();
        for item in &file.items {
            Self::collect_constructors(item, &mut constructors);
        }
        Self {
            file,
            env: vec![HashMap::new()],
            constructors,
        }
    }

    fn collect_constructors(item: &Item, out: &mut HashMap<String, usize>) {
        match item {
            Item::Type(t) => {
                match &t.kind {
                    TypeDefKind::Enum(variants) => {
                        for v in variants {
                            let arity = match &v.payload {
                                None => 0,
                                Some(VariantPayload::Tuple(types)) => types.len(),
                                Some(VariantPayload::Record(fields)) => fields.len(),
                            };
                            out.insert(v.name.clone(), arity);
                        }
                    }
                    TypeDefKind::Newtype(_) => {
                        out.insert(t.name.clone(), 1);
                    }
                    _ => {}
                }
            }
            Item::Module(m) => {
                for inner in &m.items {
                    Self::collect_constructors(inner, out);
                }
            }
            _ => {}
        }
    }

    pub fn run(&mut self) -> Result<Value, String> {
        let main = self.find_function("main").ok_or("no main() function found")?;
        self.call_func(&main, vec![])
    }

    fn find_function(&self, name: &str) -> Option<FuncDef> {
        for item in &self.file.items {
            match item {
                Item::Func(f) if f.name == name => return Some(f.clone()),
                Item::Module(m) => {
                    for inner in &m.items {
                        if let Item::Func(f) = inner {
                            if f.name == name {
                                return Some(f.clone());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn push_scope(&mut self) {
        self.env.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.env.pop();
    }

    fn bind(&mut self, name: &str, value: Value) {
        self.env.last_mut().unwrap().insert(name.into(), value);
    }

    fn lookup(&self, name: &str) -> Option<Value> {
        for scope in self.env.iter().rev() {
            if let Some(v) = scope.get(name) {
                return Some(v.clone());
            }
        }
        None
    }

    fn assign(&mut self, name: &str, value: Value) -> Result<(), String> {
        for scope in self.env.iter_mut().rev() {
            if scope.contains_key(name) {
                scope.insert(name.into(), value);
                return Ok(());
            }
        }
        Err(format!("undefined variable '{}' in assignment", name))
    }

    fn call_func(&mut self, func: &FuncDef, args: Vec<Value>) -> Result<Value, String> {
        if func.params.len() != args.len() {
            return Err(format!(
                "function {} expects {} arguments, got {}",
                func.name,
                func.params.len(),
                args.len()
            ));
        }
        self.push_scope();
        for (p, a) in func.params.iter().zip(args) {
            self.bind(&p.name, a);
        }
        let result = self.eval_block(&func.body)?;
        self.pop_scope();
        Ok(result.unwrap_or(Value::Unit))
    }

    fn eval_block(&mut self, block: &Block) -> Result<Option<Value>, String> {
        for (i, stmt) in block.iter().enumerate() {
            let is_last = i == block.len() - 1;
            match stmt {
                Stmt::Expr(e) if is_last => {
                    return Ok(Some(self.eval_expr(e)?));
                }
                Stmt::Expr(e) => {
                    self.eval_expr(e)?;
                }
                _ => {
                    if let Some(v) = self.eval_stmt(stmt)? {
                        return Ok(Some(v));
                    }
                }
            }
        }
        Ok(None)
    }

    fn eval_stmt(&mut self, stmt: &Stmt) -> Result<Option<Value>, String> {
        match stmt {
            Stmt::Let { pat, init, .. } => {
                let v = match init {
                    Some(e) => self.eval_expr(e)?,
                    None => Value::Unit,
                };
                if let Some(bindings) = self.match_pattern(pat, &v) {
                    for (name, val) in bindings {
                        self.bind(&name, val);
                    }
                } else {
                    return Err(format!("let pattern did not match value {}", v));
                }
            }
            Stmt::Return(e) => {
                let v = match e {
                    Some(e) => self.eval_expr(e)?,
                    None => Value::Unit,
                };
                return Ok(Some(v));
            }
            Stmt::Expr(e) => {
                self.eval_expr(e)?;
            }
            Stmt::If { cond, then_, else_ } => {
                let c = self.eval_expr(cond)?;
                if is_truthy(&c) {
                    if let Some(v) = self.eval_block(then_)? {
                        return Ok(Some(v));
                    }
                } else if let Some(else_block) = else_ {
                    if let Some(v) = self.eval_block(else_block)? {
                        return Ok(Some(v));
                    }
                }
            }
            Stmt::While { cond, body } => {
                while is_truthy(&self.eval_expr(cond)?) {
                    if let Some(v) = self.eval_block(body)? {
                        return Ok(Some(v));
                    }
                }
            }
            Stmt::For { var, iterable, body } => {
                let iter = self.eval_expr(iterable)?;
                let list = match iter {
                    Value::List(l) => l,
                    other => return Err(format!("cannot iterate over {}", other)),
                };
                for item in list {
                    self.bind(var, item);
                    if let Some(v) = self.eval_block(body)? {
                        return Ok(Some(v));
                    }
                }
            }
            Stmt::Block(block) => {
                if let Some(v) = self.eval_block(block)? {
                    return Ok(Some(v));
                }
            }
            Stmt::Arena(block) => {
                // Arena block evaluates like a regular block
                // In a real implementation, this would manage region-based memory
                if let Some(v) = self.eval_block(block)? {
                    return Ok(Some(v));
                }
            }
            Stmt::Assign { target, value } => {
                let v = self.eval_expr(value)?;
                match target {
                    Expr::Ident(name) => self.assign(name, v)?,
                    _ => return Err("assignment target must be a variable".into()),
                }
            }
            Stmt::Desc(_) | Stmt::Requires(_) | Stmt::Ensures(_) | Stmt::Math(_) | Stmt::Ellipsis => {}
            Stmt::Drop(expr) => {
                // Evaluate and discard the value (for linear capability drops)
                self.eval_expr(expr)?;
                // In a real implementation, this would track capability usage
            }
            Stmt::OnFailure(block) => {
                // On failure block - for now just evaluate the block
                // Full implementation would register compensation actions
                self.eval_block(block)?;
            }
        }
        Ok(None)
    }

    fn eval_expr(&mut self, expr: &Expr) -> Result<Value, String> {
        match expr {
            Expr::Literal(l) => Ok(match l {
                Lit::Int(v) => Value::Int(*v),
                Lit::Float(v) => Value::Float(*v),
                Lit::Bool(v) => Value::Bool(*v),
                Lit::String(v) => Value::String(v.clone()),
                Lit::Unit => Value::Unit,
            }),
            Expr::Ident(name) => self
                .lookup(name)
                .ok_or_else(|| format!("undefined variable '{}'", name)),
            Expr::Unary(op, e) => self.eval_unary(*op, e),
            Expr::Binary(op, l, r) => self.eval_binary(*op, l, r),
            Expr::Call(callee, args) => {
                let vals: Result<Vec<_>, _> =
                    args.iter().map(|a| self.eval_expr(a)).collect();
                let vals = vals?;
                match callee.as_ref() {
                    Expr::Ident(name) => self.call_named(name, vals),
                    _ => Err("callee must be a function name".into()),
                }
            }
            Expr::Tuple(elems) => {
                let mut vals = Vec::new();
                for e in elems {
                    vals.push(self.eval_expr(e)?);
                }
                Ok(Value::Tuple(vals))
            }
            Expr::List(elems) => {
                let mut vals = Vec::new();
                for e in elems {
                    vals.push(self.eval_expr(e)?);
                }
                Ok(Value::List(vals))
            }
            Expr::Match(subject, arms) => {
                let val = self.eval_expr(subject)?;
                for arm in arms {
                    if let Some(bindings) = self.match_pattern(&arm.pat, &val) {
                        self.push_scope();
                        for (name, v) in bindings {
                            self.bind(&name, v);
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
                Err("non-exhaustive match".into())
            }
            Expr::Field(obj, field) => {
                let obj = self.eval_expr(obj)?;
                match obj {
                    Value::Record(fields) => {
                        fields
                            .get(field)
                            .cloned()
                            .ok_or_else(|| format!("field '{}' not found", field))
                    }
                    _ => Err(format!("field access on non-record value {}", obj)),
                }
            }
            Expr::Record { ty: _, fields } => {
                let mut map = HashMap::new();
                for f in fields {
                    let v = self.eval_expr(&f.value)?;
                    map.insert(f.name.clone(), v);
                }
                Ok(Value::Record(map))
            }
            Expr::Index(obj, idx) => {
                let obj = self.eval_expr(obj)?;
                let idx = self.eval_expr(idx)?;
                match (obj, idx) {
                    (Value::List(list), Value::Int(i)) => {
                        let i = if i < 0 {
                            list.len() as i64 + i
                        } else {
                            i
                        } as usize;
                        list.get(i)
                            .cloned()
                            .ok_or_else(|| "index out of bounds".into())
                    }
                    (Value::String(s), Value::Int(i)) => {
                        let i = if i < 0 {
                            s.len() as i64 + i
                        } else {
                            i
                        } as usize;
                        s.chars()
                            .nth(i)
                            .map(|c| Value::String(c.to_string()))
                            .ok_or_else(|| "index out of bounds".into())
                    }
                    _ => Err("invalid index operation".into()),
                }
            }
            Expr::Try(expr) => {
                let v = self.eval_expr(expr)?;
                match v {
                    Value::Variant(name, vals) if name == "Ok" || name == "Some" => {
                        // Return the inner value of Ok/T
                        Ok(vals.into_iter().next().unwrap_or(Value::Unit))
                    }
                    Value::Variant(name, _) if name == "Err" || name == "None" => {
                        // Propagate error as runtime error
                        Err(format!("{} propagated via ?", name))
                    }
                    _ => {
                        Err(format!("? operator requires Result or Option, found {}", v))
                    }
                }
            }
            Expr::Spawn(expr) => {
                // Spawn a future - for now, just evaluate and return the value
                // Real implementation would create a task
                self.eval_expr(expr)
            }
            Expr::Await(expr) => {
                // Await a future - for now, just return the value
                // Real implementation would wait for the task
                self.eval_expr(expr)
            }
        }
    }

    fn call_named(&mut self, name: &str, args: Vec<Value>) -> Result<Value, String> {
        if let Some(&arity) = self.constructors.get(name) {
            if args.len() != arity {
                return Err(format!(
                    "constructor '{}' expects {} arguments, got {}",
                    name, arity, args.len()
                ));
            }
            return Ok(Value::Variant(name.into(), args));
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
            _ => {
                let func = self
                    .find_function(name)
                    .ok_or_else(|| format!("undefined function '{}'", name))?;
                self.call_func(&func, args)
            }
        }
    }

    fn match_pattern(&self, pat: &Pattern, value: &Value) -> Option<Vec<(String, Value)>> {
        let mut bindings = Vec::new();
        if self.match_pattern_inner(pat, value, &mut bindings) {
            Some(bindings)
        } else {
            None
        }
    }

    fn match_pattern_inner(&self, pat: &Pattern, value: &Value, bindings: &mut Vec<(String, Value)>) -> bool {
        match pat {
            Pattern::Wildcard => true,
            Pattern::Variable(name) => {
                bindings.push((name.clone(), value.clone()));
                true
            }
            Pattern::Literal(l) => {
                let expected = match l {
                    Lit::Int(v) => Value::Int(*v),
                    Lit::Float(v) => Value::Float(*v),
                    Lit::Bool(v) => Value::Bool(*v),
                    Lit::String(v) => Value::String(v.clone()),
                    Lit::Unit => Value::Unit,
                };
                values_equal(value, &expected)
            }
            Pattern::Constructor(name, pats) => {
                match value {
                    Value::Variant(vname, vals) if vname == name => {
                        if pats.len() != vals.len() {
                            return false;
                        }
                        for (p, v) in pats.iter().zip(vals.iter()) {
                            if !self.match_pattern_inner(p, v, bindings) {
                                return false;
                            }
                        }
                        true
                    }
                    _ => false,
                }
            }
            Pattern::Tuple(pats) => {
                match value {
                    Value::Tuple(vals) if pats.len() == vals.len() => {
                        for (p, v) in pats.iter().zip(vals.iter()) {
                            if !self.match_pattern_inner(p, v, bindings) {
                                return false;
                            }
                        }
                        true
                    }
                    _ => false,
                }
            }
        }
    }

    fn eval_unary(&mut self, op: UnOp, e: &Expr) -> Result<Value, String> {
        let v = self.eval_expr(e)?;
        match op {
            UnOp::Neg => match v {
                Value::Int(x) => Ok(Value::Int(-x)),
                Value::Float(x) => Ok(Value::Float(-x)),
                _ => Err("cannot negate non-number".into()),
            },
            UnOp::Not => Ok(Value::Bool(!is_truthy(&v))),
            UnOp::Ref | UnOp::RefMut => Ok(v),
        }
    }

    fn eval_binary(&mut self, op: BinOp, l: &Expr, r: &Expr) -> Result<Value, String> {
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
        match op {
            BinOp::Add => numeric_op(left, right, |a, b| a + b, |a, b| a + b),
            BinOp::Sub => numeric_op(left, right, |a, b| a - b, |a, b| a - b),
            BinOp::Mul => numeric_op(left, right, |a, b| a * b, |a, b| a * b),
            BinOp::Div => numeric_op(left, right, |a, b| a / b, |a, b| a / b),
            BinOp::Mod => match (left, right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a % b)),
                _ => Err("modulo requires integers".into()),
            },
            BinOp::Pow => match (left, right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.pow(b as u32))),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a.powf(b))),
                _ => Err("power requires numbers".into()),
            },
            BinOp::EqCmp => Ok(Value::Bool(values_equal(&left, &right))),
            BinOp::NeCmp => Ok(Value::Bool(!values_equal(&left, &right))),
            BinOp::Lt => compare_op(left, right, |o| o == std::cmp::Ordering::Less),
            BinOp::Gt => compare_op(left, right, |o| o == std::cmp::Ordering::Greater),
            BinOp::Le => compare_op(left, right, |o| o != std::cmp::Ordering::Greater),
            BinOp::Ge => compare_op(left, right, |o| o != std::cmp::Ordering::Less),
            BinOp::BitAnd => match (left, right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a & b)),
                _ => Err("bitwise and requires integers".into()),
            },
            BinOp::BitOr => match (left, right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a | b)),
                _ => Err("bitwise or requires integers".into()),
            },
            BinOp::BitXor => match (left, right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a ^ b)),
                _ => Err("bitwise xor requires integers".into()),
            },
            BinOp::Shl => match (left, right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a << b)),
                _ => Err("shift requires integers".into()),
            },
            BinOp::Shr => match (left, right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a >> b)),
                _ => Err("shift requires integers".into()),
            },
            BinOp::Assign => Err("assignment as expression not supported".into()),
            BinOp::And | BinOp::Or => unreachable!(),
        }
    }
}

fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Int(0) => false,
        Value::Float(x) => *x != 0.0,
        Value::String(s) => !s.is_empty(),
        Value::List(l) => !l.is_empty(),
        Value::Unit => false,
        _ => true,
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => (a - b).abs() < f64::EPSILON,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Unit, Value::Unit) => true,
        (Value::List(a), Value::List(b)) => a == b,
        _ => false,
    }
}

fn numeric_op(
    a: Value,
    b: Value,
    int_op: fn(i64, i64) -> i64,
    float_op: fn(f64, f64) -> f64,
) -> Result<Value, String> {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(int_op(a, b))),
        (Value::Float(a), Value::Float(b)) => Ok(Value::Float(float_op(a, b))),
        (Value::Int(a), Value::Float(b)) => Ok(Value::Float(float_op(a as f64, b))),
        (Value::Float(a), Value::Int(b)) => Ok(Value::Float(float_op(a, b as f64))),
        _ => Err("arithmetic requires numbers".into()),
    }
}

fn compare_op<F>(a: Value, b: Value, f: F) -> Result<Value, String>
where
    F: Fn(std::cmp::Ordering) -> bool,
{
    let ord = match (a, b) {
        (Value::Int(a), Value::Int(b)) => a.cmp(&b),
        (Value::Float(a), Value::Float(b)) => a.partial_cmp(&b).ok_or("cannot compare floats")?,
        (Value::String(a), Value::String(b)) => a.cmp(&b),
        _ => return Err("comparison requires comparable types".into()),
    };
    Ok(Value::Bool(f(ord)))
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        values_equal(self, other)
    }
}
