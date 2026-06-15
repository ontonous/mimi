use crate::ast::*;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub message: String,
}

impl Diagnostic {
    fn new(msg: impl Into<String>) -> Self {
        Self { message: msg.into() }
    }
}

pub fn check(file: &File) -> Result<(), Vec<Diagnostic>> {
    let mut checker = Checker::new(file);
    checker.check()
}

struct Checker<'a> {
    file: &'a File,
    errors: Vec<Diagnostic>,
    funcs: HashMap<String, (Vec<Type>, Type)>,
    aliases: HashMap<String, Type>,
    types: HashMap<String, TypeDef>,
}

impl<'a> Checker<'a> {
    fn new(file: &'a File) -> Self {
        Self {
            file,
            errors: Vec::new(),
            funcs: HashMap::new(),
            aliases: HashMap::new(),
            types: HashMap::new(),
        }
    }

    fn check(&mut self) -> Result<(), Vec<Diagnostic>> {
        self.collect_decls();
        for item in &self.file.items {
            self.check_item(item);
        }
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(std::mem::take(&mut self.errors))
        }
    }

    fn emit(&mut self, msg: impl Into<String>) {
        self.errors.push(Diagnostic::new(msg));
    }

    fn collect_decls(&mut self) {
        for item in &self.file.items {
            self.collect_item_decls(item);
        }
    }

    fn collect_item_decls(&mut self, item: &Item) {
        match item {
            Item::Func(f) => {
                if self.funcs.contains_key(&f.name) {
                    self.emit(format!("duplicate function definition '{}'", f.name));
                    return;
                }
                let params: Vec<Type> = f.params.iter().map(|p| self.resolve_type(&p.ty)).collect();
                let ret = f
                    .ret
                    .as_ref()
                    .map(|t| self.resolve_type(t))
                    .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                self.funcs.insert(f.name.clone(), (params, ret));
            }
            Item::Type(t) => {
                if self.types.contains_key(&t.name) {
                    self.emit(format!("duplicate type definition '{}'", t.name));
                    return;
                }
                match &t.kind {
                    TypeDefKind::Alias(ty) => {
                        let resolved = self.resolve_type(ty);
                        self.aliases.insert(t.name.clone(), resolved);
                    }
                    TypeDefKind::Newtype(ty) => {
                        let inner = self.resolve_type(ty);
                        let self_ty = Type::Name(t.name.clone(), vec![]);
                        self.funcs.insert(t.name.clone(), (vec![inner], self_ty));
                    }
                    TypeDefKind::Enum(variants) => {
                        let self_ty = Type::Name(t.name.clone(), vec![]);
                        for v in variants {
                            let ret = self_ty.clone();
                            let params = match &v.payload {
                                None => vec![],
                                Some(VariantPayload::Tuple(types)) => types.iter().map(|ty| self.resolve_type(ty)).collect(),
                                Some(VariantPayload::Record(fields)) => fields.iter().map(|f| self.resolve_type(&f.ty)).collect(),
                            };
                            self.funcs.insert(v.name.clone(), (params, ret));
                        }
                    }
                    _ => {}
                }
                self.types.insert(t.name.clone(), t.clone());
            }
            Item::Module(m) => {
                for inner in &m.items {
                    self.collect_item_decls(inner);
                }
            }
            Item::Rule(_) | Item::Desc(_) | Item::Cap(_) | Item::Actor(_) => {}
        }
    }

    fn resolve_type(&self, ty: &Type) -> Type {
        match ty {
            Type::Name(name, args) => {
                if let Some(aliased) = self.aliases.get(name) {
                    // Simple aliases do not carry generic args in v0.2
                    aliased.clone()
                } else {
                    Type::Name(name.clone(), args.clone())
                }
            }
            Type::Ref(inner) => Type::Ref(Box::new(self.resolve_type(inner))),
            Type::RefMut(inner) => Type::RefMut(Box::new(self.resolve_type(inner))),
            Type::Option(inner) => Type::Option(Box::new(self.resolve_type(inner))),
            Type::Result(ok, err) => Type::Result(
                Box::new(self.resolve_type(ok)),
                Box::new(self.resolve_type(err)),
            ),
            Type::Tuple(elems) => Type::Tuple(elems.iter().map(|e| self.resolve_type(e)).collect()),
            Type::Func(args, ret) => Type::Func(
                args.iter().map(|a| self.resolve_type(a)).collect(),
                Box::new(self.resolve_type(ret)),
            ),
            Type::Cap(_) => ty.clone(),
        }
    }

    fn check_item(&mut self, item: &Item) {
        match item {
            Item::Func(f) => self.check_func(f),
            Item::Module(m) => {
                for inner in &m.items {
                    self.check_item(inner);
                }
            }
            Item::Type(_) | Item::Cap(_) | Item::Actor(_) => {}
            Item::Rule(_) | Item::Desc(_) => {}
        }
    }

    fn check_func(&mut self, func: &FuncDef) {
        let ret = func
            .ret
            .as_ref()
            .map(|t| self.resolve_type(t))
            .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
        let mut scopes: Vec<HashMap<String, Type>> = vec![HashMap::new()];
        for p in &func.params {
            let ty = self.resolve_type(&p.ty);
            scopes[0].insert(p.name.clone(), ty);
        }
        self.check_block(&func.body, &ret, &mut scopes);
    }

    fn check_block(&mut self, block: &Block, ret: &Type, scopes: &mut Vec<HashMap<String, Type>>) {
        for stmt in block {
            self.check_stmt(stmt, ret, scopes);
        }
    }

    fn check_stmt(
        &mut self,
        stmt: &Stmt,
        ret: &Type,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) {
        match stmt {
            Stmt::Let { pat, ty, init, mut_, ref_ } => {
                // If ref_ is true, the variable is an arena reference
                let init_ty = init
                    .as_ref()
                    .map(|e| self.infer_expr(e, scopes))
                    .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                let declared = ty.as_ref().map(|t| self.resolve_type(t));
                let final_ty = match declared {
                    Some(d) => {
                        if !same_type(&d, &init_ty) {
                            self.emit(format!(
                                "pattern declared as {} but initialized with {}",
                                fmt_type(&d),
                                fmt_type(&init_ty)
                            ));
                        }
                        d
                    }
                    None => {
                        if *ref_ {
                            // ref variables have reference type
                            Type::Ref(Box::new(init_ty))
                        } else {
                            init_ty
                        }
                    }
                };
                if *mut_ {
                    // For v0.2, mutability is tracked per-variable; tuple patterns ignore mut_ for simplicity.
                }
                self.check_pattern(pat, &final_ty, scopes);
            }
            Stmt::Return(None) => {
                if !same_type(ret, &Type::Name("unit".into(), vec![])) {
                    self.emit(format!(
                        "expected return value of type {}, found unit",
                        fmt_type(ret)
                    ));
                }
            }
            Stmt::Return(Some(e)) => {
                let t = self.infer_expr(e, scopes);
                if !same_type(ret, &t) {
                    self.emit(format!(
                        "return type mismatch: expected {}, found {}",
                        fmt_type(ret),
                        fmt_type(&t)
                    ));
                }
            }
            Stmt::Expr(e) => {
                self.infer_expr(e, scopes);
            }
            Stmt::If { cond, then_, else_ } => {
                let ct = self.infer_expr(cond, scopes);
                if !is_bool(&ct) {
                    self.emit(format!(
                        "if condition must be bool, found {}",
                        fmt_type(&ct)
                    ));
                }
                self.check_block(then_, ret, scopes);
                if let Some(else_) = else_ {
                    self.check_block(else_, ret, scopes);
                }
            }
            Stmt::While { cond, body } => {
                let ct = self.infer_expr(cond, scopes);
                if !is_bool(&ct) {
                    self.emit(format!(
                        "while condition must be bool, found {}",
                        fmt_type(&ct)
                    ));
                }
                self.check_block(body, ret, scopes);
            }
            Stmt::For { var, iterable, body } => {
                let it = self.infer_expr(iterable, scopes);
                let elem_ty = match it {
                    Type::Name(n, args) if n == "List" && args.len() == 1 => args[0].clone(),
                    _ => {
                        self.emit(format!(
                            "for loop requires a List, found {}",
                            fmt_type(&it)
                        ));
                        Type::Name("unknown".into(), vec![])
                    }
                };
                scopes.push(HashMap::new());
                scopes.last_mut().unwrap().insert(var.clone(), elem_ty);
                self.check_block(body, ret, scopes);
                scopes.pop();
            }
            Stmt::Block(block) => {
                scopes.push(HashMap::new());
                self.check_block(block, ret, scopes);
                scopes.pop();
            }
            Stmt::Arena(block) => {
                // Arena block is like a scope with special memory semantics
                // For now, just check the block contents
                scopes.push(HashMap::new());
                self.check_block(block, ret, scopes);
                scopes.pop();
            }
            Stmt::Assign { target, value } => {
                let value_ty = self.infer_expr(value, scopes);
                match target {
                    Expr::Ident(name) => {
                        let target_ty = self.lookup_var(name, scopes);
                        if !same_type(&target_ty, &value_ty) {
                            self.emit(format!(
                                "cannot assign {} to variable '{}' of type {}",
                                fmt_type(&value_ty),
                                name,
                                fmt_type(&target_ty)
                            ));
                        }
                    }
                    _ => self.emit("assignment target must be a variable"),
                }
            }
            Stmt::Desc(_) | Stmt::Requires(_) | Stmt::Ensures(_) | Stmt::Math(_) | Stmt::Ellipsis | Stmt::Drop(_) | Stmt::OnFailure(_) => {}
        }
    }

    fn infer_expr(&mut self, expr: &Expr, scopes: &mut Vec<HashMap<String, Type>>) -> Type {
        match expr {
            Expr::Literal(l) => match l {
                Lit::Int(_) => Type::Name("i32".into(), vec![]),
                Lit::Float(_) => Type::Name("f64".into(), vec![]),
                Lit::Bool(_) => Type::Name("bool".into(), vec![]),
                Lit::String(_) => Type::Name("string".into(), vec![]),
                Lit::Unit => Type::Name("unit".into(), vec![]),
            },
            Expr::Ident(name) => self.lookup_var(name, scopes),
            Expr::Unary(op, e) => {
                let t = self.infer_expr(e, scopes);
                match op {
                    UnOp::Neg => {
                        if is_numeric(&t) {
                            t
                        } else {
                            self.emit(format!("cannot negate {}", fmt_type(&t)));
                            Type::Name("unknown".into(), vec![])
                        }
                    }
                    UnOp::Not => {
                        if is_bool(&t) {
                            t
                        } else {
                            self.emit(format!("cannot apply ! to {}", fmt_type(&t)));
                            Type::Name("unknown".into(), vec![])
                        }
                    }
                    UnOp::Ref => Type::Ref(Box::new(t)),
                    UnOp::RefMut => Type::RefMut(Box::new(t)),
                }
            }
            Expr::Binary(op, l, r) => self.infer_binary(*op, l, r, scopes),
            Expr::Call(callee, args) => {
                if let Expr::Ident(name) = callee.as_ref() {
                    self.check_call(name, args, scopes)
                } else {
                    self.emit("callee must be a function name");
                    Type::Name("unknown".into(), vec![])
                }
            }
            Expr::Tuple(elems) => {
                Type::Tuple(elems.iter().map(|e| self.infer_expr(e, scopes)).collect())
            }
            Expr::List(elems) => {
                let mut elem_ty = Type::Name("unknown".into(), vec![]);
                for (i, e) in elems.iter().enumerate() {
                    let t = self.infer_expr(e, scopes);
                    if i == 0 {
                        elem_ty = t;
                    } else if !same_type(&elem_ty, &t) {
                        self.emit(format!(
                            "list element {} type {} does not match first element {}",
                            i + 1,
                            fmt_type(&t),
                            fmt_type(&elem_ty)
                        ));
                    }
                }
                Type::Name("List".into(), vec![elem_ty])
            }
            Expr::Match(subject, arms) => {
                let subject_ty = self.infer_expr(subject, scopes);
                if arms.is_empty() {
                    self.emit("match expression must have at least one arm");
                    return Type::Name("unknown".into(), vec![]);
                }
                let mut result_ty: Option<Type> = None;
                for arm in arms {
                    scopes.push(HashMap::new());
                    self.check_pattern(&arm.pat, &subject_ty, scopes);
                    if let Some(guard) = &arm.guard {
                        let gt = self.infer_expr(guard, scopes);
                        if !is_bool(&gt) {
                            self.emit(format!(
                                "match guard must be bool, found {}",
                                fmt_type(&gt)
                            ));
                        }
                    }
                    let body_ty = self.infer_expr(&arm.body, scopes);
                    scopes.pop();
                    match &result_ty {
                        None => result_ty = Some(body_ty),
                        Some(rt) => {
                            if !same_type(rt, &body_ty) {
                                self.emit(format!(
                                    "match arm body type {} does not match previous {}",
                                    fmt_type(&body_ty),
                                    fmt_type(rt)
                                ));
                            }
                        }
                    }
                }
                result_ty.unwrap_or_else(|| Type::Name("unknown".into(), vec![]))
            }
            Expr::Field(obj, field) => {
                let obj_ty = self.infer_expr(obj, scopes);
                match &obj_ty {
                    Type::Name(name, _) => {
                        if let Some(tdef) = self.types.get(name) {
                            match &tdef.kind {
                                TypeDefKind::Record(fields) => {
                                    if let Some(f) = fields.iter().find(|f| f.name == *field) {
                                        self.resolve_type(&f.ty)
                                    } else {
                                        self.emit(format!(
                                            "type '{}' has no field '{}'",
                                            name, field
                                        ));
                                        Type::Name("unknown".into(), vec![])
                                    }
                                }
                                _ => {
                                    self.emit(format!("'{}' is not a record type", name));
                                    Type::Name("unknown".into(), vec![])
                                }
                            }
                        } else {
                            self.emit(format!("field access on unknown type '{}'", name));
                            Type::Name("unknown".into(), vec![])
                        }
                    }
                    _ => {
                        self.emit(format!(
                            "field access requires a record type, found {}",
                            fmt_type(&obj_ty)
                        ));
                        Type::Name("unknown".into(), vec![])
                    }
                }
            }
            Expr::Record { ty, fields } => {
                let tdef = ty.as_ref().and_then(|n| self.types.get(n)).cloned();
                match tdef {
                    Some(tdef) => {
                        match &tdef.kind {
                            TypeDefKind::Record(expected_fields) => {
                                let expected: HashMap<String, Type> = expected_fields
                                    .iter()
                                    .map(|f| (f.name.clone(), self.resolve_type(&f.ty)))
                                    .collect();
                                for (name, value) in fields.iter().map(|f| (&f.name, &f.value)) {
                                    if let Some(expected_ty) = expected.get(name) {
                                        let actual_ty = self.infer_expr(value, scopes);
                                        if !same_type(expected_ty, &actual_ty) {
                                            self.emit(format!(
                                                "field '{}' expected {}, found {}",
                                                name,
                                                fmt_type(expected_ty),
                                                fmt_type(&actual_ty)
                                            ));
                                        }
                                    } else {
                                        self.emit(format!(
                                            "type '{}' has no field '{}'",
                                            tdef.name,
                                            name
                                        ));
                                    }
                                }
                                for name in expected.keys() {
                                    if !fields.iter().any(|f| &f.name == name) {
                                        self.emit(format!(
                                            "missing field '{}' in record literal",
                                            name
                                        ));
                                    }
                                }
                                Type::Name(tdef.name.clone(), vec![])
                            }
                            _ => {
                                self.emit(format!("'{}' is not a record type", tdef.name));
                                Type::Name("unknown".into(), vec![])
                            }
                        }
                    }
                    None => {
                        self.emit("cannot infer record type without explicit type name");
                        Type::Name("unknown".into(), vec![])
                    }
                }
            }
            Expr::Index(obj, idx) => {
                let obj_ty = self.infer_expr(obj, scopes);
                let idx_ty = self.infer_expr(idx, scopes);
                if !is_int(&idx_ty) {
                    self.emit(format!("index must be integer, found {}", fmt_type(&idx_ty)));
                }
                match obj_ty {
                    Type::Name(n, args) if n == "List" && args.len() == 1 => args[0].clone(),
                    Type::Name(n, _) if n == "string" => Type::Name("string".into(), vec![]),
                    _ => {
                        self.emit(format!("cannot index {}", fmt_type(&obj_ty)));
                        Type::Name("unknown".into(), vec![])
                    }
                }
            }
            Expr::Try(expr) => {
                let inner_ty = self.infer_expr(expr, scopes);
                match inner_ty {
                    Type::Name(n, args) if n == "Result" && args.len() == 2 => {
                        // Result<T, E> -> ? extracts T
                        args[0].clone()
                    }
                    Type::Name(n, args) if n == "Option" && args.len() == 1 => {
                        // Option<T> -> ? extracts T
                        args[0].clone()
                    }
                    Type::Option(inner) => {
                        // T? is syntactic sugar for Option<T>
                        (*inner).clone()
                    }
                    // Support user-defined Result/Option-like enums
                    // If it's a known type with 2 args (success, error), extract first
                    // If it's a known type with 1 arg (some value), extract it
                    Type::Name(_, args) if args.len() == 2 => {
                        args[0].clone()
                    }
                    Type::Name(_, args) if args.len() == 1 => {
                        args[0].clone()
                    }
                    // For unparameterized enum types like `Res`, look up the type definition
                    Type::Name(name, ref args) if args.is_empty() => {
                        if let Some(tdef) = self.types.get(&name) {
                            match &tdef.kind {
                                TypeDefKind::Enum(variants) if variants.len() == 2 => {
                                    // Try to find Ok/Err or Some/None pattern
                                    let first_variant = &variants[0];
                                    match &first_variant.payload {
                                        Some(VariantPayload::Tuple(types)) if !types.is_empty() => {
                                            types[0].clone()
                                        }
                                        _ => {
                                            self.emit(format!(
                                                "? operator: cannot determine success type from {}",
                                                name
                                            ));
                                            Type::Name("unknown".into(), vec![])
                                        }
                                    }
                                }
                                _ => {
                                    self.emit(format!(
                                        "? operator requires Result or Option type, found {}",
                                        name
                                    ));
                                    Type::Name("unknown".into(), vec![])
                                }
                            }
                        } else {
                            self.emit(format!(
                                "? operator requires Result or Option type, found {}",
                                name
                            ));
                            Type::Name("unknown".into(), vec![])
                        }
                    }
                    _ => {
                        self.emit(format!(
                            "? operator requires Result or Option type, found {}",
                            fmt_type(&inner_ty)
                        ));
                        Type::Name("unknown".into(), vec![])
                    }
                }
            }
            Expr::Spawn(_) => {
                // Spawn returns a future/handle type - simplified for now
                Type::Name("Future".into(), vec![])
            }
            Expr::Await(inner) => {
                // Await unwraps the future type
                let inner_ty = self.infer_expr(inner, scopes);
                // For now, just return the inner type
                match inner_ty {
                    Type::Name(n, args) if n == "Future" && !args.is_empty() => args[0].clone(),
                    other => other,
                }
            }
        }
    }

    fn check_pattern(
        &mut self,
        pat: &Pattern,
        subject: &Type,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) {
        match pat {
            Pattern::Wildcard => {}
            Pattern::Variable(name) => {
                scopes.last_mut().unwrap().insert(name.clone(), subject.clone());
            }
            Pattern::Literal(l) => {
                let lit_ty = match l {
                    Lit::Int(_) => Type::Name("i32".into(), vec![]),
                    Lit::Float(_) => Type::Name("f64".into(), vec![]),
                    Lit::Bool(_) => Type::Name("bool".into(), vec![]),
                    Lit::String(_) => Type::Name("string".into(), vec![]),
                    Lit::Unit => Type::Name("unit".into(), vec![]),
                };
                if !same_type(subject, &lit_ty) {
                    self.emit(format!(
                        "pattern literal type {} does not match subject {}",
                        fmt_type(&lit_ty),
                        fmt_type(subject)
                    ));
                }
            }
            Pattern::Constructor(name, pats) => {
                let def = self.types.values().find(|t| {
                    match &t.kind {
                        TypeDefKind::Enum(variants) => variants.iter().any(|v| v.name == *name),
                        TypeDefKind::Newtype(_) => t.name == *name,
                        _ => false,
                    }
                });
                match def {
                    Some(tdef) => {
                        match &tdef.kind {
                            TypeDefKind::Enum(variants) => {
                                if let Some(variant) = variants.iter().find(|v| v.name == *name) {
                                    match &variant.payload {
                                        None => {
                                            if !pats.is_empty() {
                                                self.emit(format!(
                                                    "variant '{}' takes no arguments",
                                                    name
                                                ));
                                            }
                                        }
                                        Some(VariantPayload::Tuple(types)) => {
                                            let types: Vec<Type> = types.clone();
                                            if pats.len() != types.len() {
                                                self.emit(format!(
                                                    "variant '{}' expects {} arguments, got {}",
                                                    name,
                                                    types.len(),
                                                    pats.len()
                                                ));
                                            } else {
                                                for (p, t) in pats.iter().zip(types.iter()) {
                                                    self.check_pattern(p, &self.resolve_type(t), scopes);
                                                }
                                            }
                                        }
                                        Some(VariantPayload::Record(_)) => {
                                            self.emit(format!(
                                                "record-style variant '{}' pattern not yet supported",
                                                name
                                            ));
                                        }
                                    }
                                } else {
                                    self.emit(format!("variant '{}' not found in type '{}'", name, tdef.name));
                                }
                            }
                            TypeDefKind::Newtype(inner) => {
                                if pats.len() != 1 {
                                    self.emit(format!(
                                        "newtype '{}' pattern expects exactly one argument",
                                        name
                                    ));
                                } else {
                                    self.check_pattern(&pats[0], &self.resolve_type(inner), scopes);
                                }
                            }
                            _ => {
                                self.emit(format!("'{}' is not an enum variant", name));
                            }
                        }
                    }
                    None => {
                        self.emit(format!("undefined constructor '{}'", name));
                    }
                }
            }
            Pattern::Tuple(pats) => {
                match subject {
                    Type::Tuple(types) => {
                        if pats.len() != types.len() {
                            self.emit(format!(
                                "tuple pattern expects {} elements, found {}",
                                types.len(),
                                pats.len()
                            ));
                        } else {
                            for (p, t) in pats.iter().zip(types.iter()) {
                                self.check_pattern(p, t, scopes);
                            }
                        }
                    }
                    _ => {
                        self.emit(format!(
                            "cannot match tuple pattern against non-tuple type {}",
                            fmt_type(subject)
                        ));
                    }
                }
            }
        }
    }

    fn infer_binary(
        &mut self,
        op: BinOp,
        l: &Expr,
        r: &Expr,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        // short-circuit logic
        if op == BinOp::And || op == BinOp::Or {
            let lt = self.infer_expr(l, scopes);
            let rt = self.infer_expr(r, scopes);
            if !is_bool(&lt) || !is_bool(&rt) {
                self.emit(format!(
                    "logical operator requires bool operands, found {} and {}",
                    fmt_type(&lt),
                    fmt_type(&rt)
                ));
            }
            return Type::Name("bool".into(), vec![]);
        }

        let lt = self.infer_expr(l, scopes);
        let rt = self.infer_expr(r, scopes);

        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Pow => {
                if !same_type(&lt, &rt) || !is_numeric(&lt) {
                    self.emit(format!(
                        "arithmetic operator requires matching numeric types, found {} and {}",
                        fmt_type(&lt),
                        fmt_type(&rt)
                    ));
                    Type::Name("unknown".into(), vec![])
                } else {
                    lt
                }
            }
            BinOp::Mod | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => {
                if !same_type(&lt, &rt) || !is_int(&lt) {
                    self.emit(format!(
                        "operator requires matching integer types, found {} and {}",
                        fmt_type(&lt),
                        fmt_type(&rt)
                    ));
                    Type::Name("unknown".into(), vec![])
                } else {
                    lt
                }
            }
            BinOp::EqCmp | BinOp::NeCmp => {
                if !same_type(&lt, &rt) {
                    self.emit(format!(
                        "equality requires matching types, found {} and {}",
                        fmt_type(&lt),
                        fmt_type(&rt)
                    ));
                }
                Type::Name("bool".into(), vec![])
            }
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                if !same_type(&lt, &rt) || !(is_numeric(&lt) || is_string(&lt)) {
                    self.emit(format!(
                        "comparison requires matching numeric or string types, found {} and {}",
                        fmt_type(&lt),
                        fmt_type(&rt)
                    ));
                }
                Type::Name("bool".into(), vec![])
            }
            BinOp::And | BinOp::Or => unreachable!("logical operators handled above"),
            BinOp::Assign => {
                self.emit("assignment is not a valid expression in v0.2");
                Type::Name("unknown".into(), vec![])
            }
        }
    }

    fn check_call(
        &mut self,
        name: &str,
        args: &[Expr],
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        // Builtins
        match name {
            "println" => {
                for a in args {
                    self.infer_expr(a, scopes);
                }
                return Type::Name("unit".into(), vec![]);
            }
            "assert" => {
                if args.len() != 1 {
                    self.emit("assert expects 1 argument");
                } else {
                    let t = self.infer_expr(&args[0], scopes);
                    if !is_bool(&t) {
                        self.emit(format!("assert expects bool, found {}", fmt_type(&t)));
                    }
                }
                return Type::Name("unit".into(), vec![]);
            }
            "range" => {
                if args.len() != 2 {
                    self.emit("range expects 2 arguments");
                } else {
                    let t1 = self.infer_expr(&args[0], scopes);
                    let t2 = self.infer_expr(&args[1], scopes);
                    if !is_int(&t1) || !is_int(&t2) {
                        self.emit("range expects integer arguments");
                    }
                }
                return Type::Name("List".into(), vec![Type::Name("i32".into(), vec![])]);
            }
            "sqrt" => {
                if args.len() != 1 {
                    self.emit("sqrt expects 1 argument");
                } else {
                    let t = self.infer_expr(&args[0], scopes);
                    if !is_numeric(&t) {
                        self.emit("sqrt expects a numeric argument");
                    }
                }
                return Type::Name("f64".into(), vec![]);
            }
            _ => {}
        }

        let (params, ret) = match self.funcs.get(name) {
            Some(sig) => sig.clone(),
            None => {
                self.emit(format!("undefined function '{}'", name));
                return Type::Name("unknown".into(), vec![]);
            }
        };

        if args.len() != params.len() {
            self.emit(format!(
                "function '{}' expects {} arguments, got {}",
                name,
                params.len(),
                args.len()
            ));
        } else {
            for (i, (arg, param)) in args.iter().zip(params.iter()).enumerate() {
                let at = self.infer_expr(arg, scopes);
                if !same_type(&at, param) {
                    self.emit(format!(
                        "argument {} of '{}' expected {}, found {}",
                        i + 1,
                        name,
                        fmt_type(param),
                        fmt_type(&at)
                    ));
                }
            }
        }
        ret
    }

    fn lookup_var(&mut self, name: &str, scopes: &mut Vec<HashMap<String, Type>>) -> Type {
        for scope in scopes.iter().rev() {
            if let Some(t) = scope.get(name) {
                return t.clone();
            }
        }
        self.emit(format!("undefined variable '{}'", name));
        Type::Name("unknown".into(), vec![])
    }
}

fn same_type(a: &Type, b: &Type) -> bool {
    match (a, b) {
        (Type::Name(na, aa), Type::Name(nb, ab)) => na == nb && aa.len() == ab.len() && aa.iter().zip(ab.iter()).all(|(x, y)| same_type(x, y)),
        (Type::Ref(a), Type::Ref(b)) => same_type(a, b),
        (Type::RefMut(a), Type::RefMut(b)) => same_type(a, b),
        (Type::Option(a), Type::Option(b)) => same_type(a, b),
        (Type::Result(a1, a2), Type::Result(b1, b2)) => same_type(a1, b1) && same_type(a2, b2),
        (Type::Tuple(a), Type::Tuple(b)) => a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| same_type(x, y)),
        (Type::Func(a_args, a_ret), Type::Func(b_args, b_ret)) => {
            a_args.len() == b_args.len()
                && a_args.iter().zip(b_args.iter()).all(|(x, y)| same_type(x, y))
                && same_type(a_ret, b_ret)
        }
        (Type::Cap(a), Type::Cap(b)) => a == b,
        _ => false,
    }
}

fn is_int(t: &Type) -> bool {
    matches!(t, Type::Name(n, _) if n == "i32" || n == "i64")
}

fn is_numeric(t: &Type) -> bool {
    matches!(t, Type::Name(n, _) if n == "i32" || n == "i64" || n == "f64")
}

fn is_bool(t: &Type) -> bool {
    matches!(t, Type::Name(n, _) if n == "bool")
}

fn is_string(t: &Type) -> bool {
    matches!(t, Type::Name(n, _) if n == "string")
}

fn fmt_type(t: &Type) -> String {
    match t {
        Type::Name(n, args) if args.is_empty() => n.clone(),
        Type::Name(n, args) => format!("{}<{}>", n, args.iter().map(fmt_type).collect::<Vec<_>>().join(", ")),
        Type::Ref(inner) => format!("&{}", fmt_type(inner)),
        Type::RefMut(inner) => format!("&mut {}", fmt_type(inner)),
        Type::Option(inner) => format!("{}?", fmt_type(inner)),
        Type::Result(ok, err) => format!("Result<{}, {}>", fmt_type(ok), fmt_type(err)),
        Type::Tuple(elems) => format!("({})", elems.iter().map(fmt_type).collect::<Vec<_>>().join(", ")),
        Type::Func(args, ret) => format!(
            "fn({}) -> {}",
            args.iter().map(fmt_type).collect::<Vec<_>>().join(", "),
            fmt_type(ret)
        ),
        Type::Cap(name) => format!("cap {}", name),
    }
}
