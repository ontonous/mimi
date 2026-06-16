use super::*;

impl<'a> Checker<'a> {
    pub(crate) fn infer_expr(&mut self, expr: &Expr, scopes: &mut Vec<HashMap<String, Type>>) -> Type {
        match expr {
            Expr::Literal(l) => match l {
                Lit::Int(_) => Type::Name("i32".into(), vec![]),
                Lit::Float(_) => Type::Name("f64".into(), vec![]),
                Lit::Bool(_) => Type::Name("bool".into(), vec![]),
                Lit::String(_) => Type::Name("string".into(), vec![]),
                Lit::FString(_) => Type::Name("string".into(), vec![]),
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
                    UnOp::Ref => {
                        // Check borrow rules: cannot borrow if already mutably borrowed
                        if let Expr::Ident(name) = e.as_ref() {
                            if let Some(BorrowState::BorrowedMut) = self.lookup_borrow(name) {
                                self.emit(format!("cannot borrow '{}' as immutable because it is already mutably borrowed", name));
                            }
                            self.set_borrow(name, BorrowState::BorrowedImm);
                        }
                        Type::Ref(Box::new(t))
                    }
                    UnOp::RefMut => {
                        // Check borrow rules: cannot &mut if already borrowed (imm or mut)
                        if let Expr::Ident(name) = e.as_ref() {
                            if let Some(state) = self.lookup_borrow(name) {
                                match state {
                                    BorrowState::Unborrowed => {}
                                    BorrowState::BorrowedImm => {
                                        self.emit(format!("cannot borrow '{}' as mutable because it is already immutably borrowed", name));
                                    }
                                    BorrowState::BorrowedMut => {
                                        self.emit(format!("cannot borrow '{}' as mutable because it is already mutably borrowed", name));
                                    }
                                }
                            }
                            self.set_borrow(name, BorrowState::BorrowedMut);
                        }
                        Type::RefMut(Box::new(t))
                    }
                    UnOp::Deref => {
                        match &t {
                            Type::Ref(inner) | Type::RefMut(inner) => (**inner).clone(),
                            _ => {
                                self.emit(format!("cannot dereference {}", fmt_type(&t)));
                                Type::Name("unknown".into(), vec![])
                            }
                        }
                    }
                }
            }
            Expr::Binary(op, l, r) => self.infer_binary(*op, l, r, scopes),
            Expr::Call(callee, args) => {
                match callee.as_ref() {
                    Expr::Ident(name) => self.check_call(name, args, scopes),
                    Expr::Field(obj, method_name) => {
                        // Method call: obj.method(args) or Type.spawn(args)
                        let obj_ty = self.infer_expr(obj, scopes);
                        if let Type::Name(type_name, _) = &obj_ty {
                            // Check if it's an actor spawn call (Type.spawn)
                            if method_name == "spawn" {
                                return Type::Name(type_name.clone(), vec![]);
                            }
                            // Check module-qualified function call: Module::func(args)
                            let qualified_func = format!("{}::{}", type_name, method_name);
                            if self.funcs.contains_key(&qualified_func) {
                                return self.check_call(&qualified_func, args, scopes);
                            }
                            // Check record field access (field is a closure/function)
                            if let Some(tdef) = self.types.get(type_name) {
                                if let TypeDefKind::Record(fields) = &tdef.kind {
                                    if let Some(f) = fields.iter().find(|f| f.name == *method_name) {
                                        // Field access that returns a callable — just return the field type
                                        return self.resolve_type(&f.ty);
                                    }
                                }
                            }
                            // Check trait methods on this type
                            if let Some(methods) = self.type_methods.get(type_name) {
                                if let Some((trait_name, _)) = methods.iter().find(|(_, m)| m == method_name) {
                                    let trait_name = trait_name.clone();
                                    if let Some((params, ret)) = self.trait_method_sigs.get(&(trait_name.clone(), method_name.clone())).cloned() {
                                        // Validate arguments (skip first param which is self)
                                        let user_args = &args;
                                        let method_params = if !params.is_empty() { &params[1..] } else { &params };
                                        if user_args.len() != method_params.len() {
                                            self.emit(format!(
                                                "method '{}' of trait '{}' expects {} arguments, got {}",
                                                method_name, trait_name, method_params.len(), user_args.len()
                                            ));
                                        } else {
                                            for (i, (arg, param)) in user_args.iter().zip(method_params.iter()).enumerate() {
                                                let at = self.infer_expr(arg, scopes);
                                                if !same_type(&at, param) {
                                                    self.emit(format!(
                                                        "argument {} of method '{}' expected {}, found {}",
                                                        i + 1, method_name, fmt_type(param), fmt_type(&at)
                                                    ));
                                                }
                                            }
                                        }
                                        return ret;
                                    }
                                }
                            }
                            // Check if the type has this as a direct method (actor methods)
                            if let Some(actor_def) = self.file.items.iter().find_map(|item| {
                                if let Item::Actor(a) = item { if a.name == *type_name { Some(a) } else { None } } else { None }
                            }) {
                                if let Some(method) = actor_def.methods.iter().find(|m| m.name == *method_name) {
                                    let ret = method.ret.as_ref()
                                        .map(|t| self.resolve_type(t))
                                        .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                                    return ret;
                                }
                            }
                            self.emit(format!("type '{}' has no method '{}'", type_name, method_name));
                            Type::Name("unknown".into(), vec![])
                        } else {
                            self.emit(format!("method call requires a named type, found {}", fmt_type(&obj_ty)));
                            Type::Name("unknown".into(), vec![])
                        }
                    }
                    _ => {
                        self.emit("callee must be a function name");
                        Type::Name("unknown".into(), vec![])
                    }
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
            Expr::Comprehension { expr, var, iter, guard } => {
                let iter_ty = self.infer_expr(iter, scopes);
                // Check iter is a list
                if let Type::Name(n, args) = &iter_ty {
                    if n != "List" || args.len() != 1 {
                        self.emit(format!("comprehension requires a list, found {}", fmt_type(&iter_ty)));
                    }
                }
                // Infer element type from iter
                let elem_ty = if let Type::Name(_, args) = &iter_ty {
                    if args.len() == 1 { args[0].clone() } else { Type::Name("unknown".into(), vec![]) }
                } else {
                    Type::Name("unknown".into(), vec![])
                };
                // Add var to scope
                scopes.last_mut().expect("scope stack non-empty").insert(var.clone(), elem_ty);
                // Infer expression type
                let expr_ty = self.infer_expr(expr, scopes);
                // Check guard if present
                if let Some(g) = guard {
                    let guard_ty = self.infer_expr(g, scopes);
                    if !matches!(&guard_ty, Type::Name(n, _) if n == "bool") {
                        self.emit(format!("comprehension guard must be bool, found {}", fmt_type(&guard_ty)));
                    }
                }
                Type::Name("List".into(), vec![expr_ty])
            }
            Expr::Match(subject, arms) => {
                let subject_ty = self.infer_expr(subject, scopes);
                if arms.is_empty() {
                    self.emit("match expression must have at least one arm");
                    return Type::Name("unknown".into(), vec![]);
                }

                // Get all variants of the subject type for exhaustiveness checking
                let all_variants = self.get_enum_variants(&subject_ty);

                // Track which variants are covered by match arms
                let mut covered_variants: Vec<String> = Vec::new();
                let mut has_catchall = false;
                // Track if any arm has a guard - guards make exhaustiveness checking unreliable
                let mut has_guard = false;

                let mut result_ty: Option<Type> = None;
                for arm in arms {
                    // Check pattern coverage
                    let (pattern_covered, is_catchall) = self.pattern_covers_variants(&arm.pat, &subject_ty);
                    if is_catchall {
                        has_catchall = true;
                    }
                    for variant in pattern_covered {
                        if !covered_variants.contains(&variant) {
                            covered_variants.push(variant);
                        }
                    }

                    scopes.push(HashMap::new());
                    self.check_pattern(&arm.pat, &subject_ty, scopes);
                    if let Some(guard) = &arm.guard {
                        has_guard = true;
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

                // Check exhaustiveness: all variants must be covered
                // Skip if: no enum variants, has catchall, or any arm has a guard (undecidable)
                if !all_variants.is_empty() && !has_catchall && !has_guard {
                    for variant in &all_variants {
                        if !covered_variants.contains(variant) {
                            self.emit(format!(
                                "match expression is not exhaustive: missing variant '{}' of '{}'",
                                variant,
                                fmt_type(&subject_ty)
                            ));
                        }
                    }
                }

                result_ty.unwrap_or_else(|| Type::Name("unknown".into(), vec![]))
            }
            Expr::Field(obj, field) => {
                let obj_ty = self.infer_expr(obj, scopes);
                match &obj_ty {
                    Type::Name(name, _) => {
                        // Check if it's an actor type
                        if let Some(actor_def) = self.file.items.iter().find_map(|item| {
                            if let Item::Actor(a) = item {
                                if a.name == *name { Some(a) } else { None }
                            } else { None }
                        }) {
                            // Actor field access
                            if let Some(f) = actor_def.fields.iter().find(|f| f.name == *field) {
                                self.resolve_type(&f.ty)
                            } else {
                                self.emit(format!(
                                    "actor '{}' has no field '{}'",
                                    name, field
                                ));
                                Type::Name("unknown".into(), vec![])
                            }
                        } else if let Some(tdef) = self.types.get(name) {
                            match &tdef.kind {
                                TypeDefKind::Record(fields) => {
                                    if let Some(f) = fields.iter().find(|f| f.name == *field) {
                                        self.resolve_type(&f.ty)
                                    } else if let Some(methods) = self.type_methods.get(name) {
                                        if let Some((trait_name, _)) = methods.iter().find(|(_, m)| m == field) {
                                            let trait_name = trait_name.clone();
                                            if let Some((params, ret)) = self.trait_method_sigs.get(&(trait_name, field.clone())).cloned() {
                                                Type::Func(params, Box::new(ret))
                                            } else {
                                                Type::Name("unknown".into(), vec![])
                                            }
                                        } else {
                                            self.emit(format!(
                                                "type '{}' has no field '{}'",
                                                name, field
                                            ));
                                            Type::Name("unknown".into(), vec![])
                                        }
                                    } else {
                                        self.emit(format!(
                                            "type '{}' has no field '{}'",
                                            name, field
                                        ));
                                        Type::Name("unknown".into(), vec![])
                                    }
                                }
                                _ => {
                                    // Check trait methods for non-record types
                                    if let Some(methods) = self.type_methods.get(name) {
                                        if let Some((trait_name, _)) = methods.iter().find(|(_, m)| m == field) {
                                            let trait_name = trait_name.clone();
                                            if let Some((params, ret)) = self.trait_method_sigs.get(&(trait_name, field.clone())).cloned() {
                                                Type::Func(params, Box::new(ret))
                                            } else {
                                                Type::Name("unknown".into(), vec![])
                                            }
                                        } else {
                                            self.emit(format!("'{}' is not a record type", name));
                                            Type::Name("unknown".into(), vec![])
                                        }
                                    } else {
                                        self.emit(format!("'{}' is not a record type", name));
                                        Type::Name("unknown".into(), vec![])
                                    }
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
                    // Built-in Result<T, E> -> ? extracts T
                    Type::Name(n, args) if n == "Result" && args.len() == 2 => {
                        args[0].clone()
                    }
                    // Built-in Option<T> -> ? extracts T
                    Type::Name(n, args) if n == "Option" && args.len() == 1 => {
                        args[0].clone()
                    }
                    // T? syntactic sugar for Option<T>
                    Type::Option(inner) => (*inner).clone(),
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
                                                "? operator: cannot determine success type from enum '{}'",
                                                name
                                            ));
                                            Type::Name("unknown".into(), vec![])
                                        }
                                    }
                                }
                                _ => {
                                    self.emit(format!(
                                        "? operator requires Result or Option type, found '{}'",
                                        name
                                    ));
                                    Type::Name("unknown".into(), vec![])
                                }
                            }
                        } else {
                            self.emit(format!(
                                "? operator requires Result or Option type, found '{}'",
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
            Expr::Quote(_) | Expr::QuoteInterpolate(_) => {
                // quote! returns an AST value
                Type::Name("AST".into(), vec![])
            }
            Expr::Comptime(block) => {
                // Comptime block: infer type from last expression
                let mut result_type = Type::Name("unit".into(), vec![]);
                for stmt in block {
                    match stmt {
                        Stmt::Expr(e) => result_type = self.infer_expr(e, scopes),
                        Stmt::Return(Some(e)) => { result_type = self.infer_expr(e, scopes); break; }
                        _ => {}
                    }
                }
                result_type
            }
            Expr::TypeOf(_) => {
                // type_of returns a Type descriptor
                Type::Name("Type".into(), vec![])
            }
            Expr::SliceExpr { target, start, end } => {
                let target_ty = self.infer_expr(target, scopes);
                if let Some(s) = start { let _ = self.infer_expr(s, scopes); }
                if let Some(e) = end { let _ = self.infer_expr(e, scopes); }
                Type::Slice(Box::new(target_ty))
            }
            Expr::TypeInfo(_) => {
                // type_info returns a record with type metadata
                Type::Name("TypeInfo".into(), vec![])
            }
            Expr::Old(expr) => {
                // old(x) returns the same type as x
                self.infer_expr(expr, scopes)
            }
            Expr::Lambda { params, ret, .. } => {
                let param_types: Vec<Type> = params.iter().map(|p| p.ty.clone()).collect();
                let return_type = ret.clone().unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                Type::Func(param_types, Box::new(return_type))
            }
            Expr::Turbofish(name, type_args, args) => {
                // Turbofish: func::<Type>(args) — explicit type instantiation
                let (params, ret) = match self.funcs.get(name) {
                    Some(sig) => sig.clone(),
                    None => {
                        self.emit(format!("undefined function '{}'", name));
                        return Type::Name("unknown".into(), vec![]);
                    }
                };
                let generics = self.func_generics.get(name).cloned().unwrap_or_default();

                // Build type param map from turbofish type args
                let mut type_map: HashMap<String, Type> = HashMap::new();
                if !generics.is_empty() && !type_args.is_empty() {
                    if type_args.len() != generics.len() {
                        self.emit(format!(
                            "function '{}' expects {} type arguments, got {}",
                            name,
                            generics.len(),
                            type_args.len()
                        ));
                    } else {
                        for (gp, ta) in generics.iter().zip(type_args.iter()) {
                            type_map.insert(gp.name.clone(), ta.clone());
                        }
                    }
                }

                if args.len() != params.len() {
                    self.emit(format!(
                        "function '{}' expects {} arguments, got {}",
                        name,
                        params.len(),
                        args.len()
                    ));
                } else {
                    // Check where constraints (before substitution)
                    if let Some((type_param, bounds)) = self.where_clauses.get(name).cloned() {
                        for (arg, param) in args.iter().zip(params.iter()) {
                            let at = self.infer_expr(arg, scopes);
                            if self.type_uses_type_param(param, &type_param) {
                                for bound in &bounds {
                                    if !self.type_implements_trait(&at, bound) {
                                        self.emit(format!(
                                            "where constraint violated: type '{}' does not implement trait '{}' (required by function '{}')",
                                            fmt_type(&at),
                                            bound,
                                            name
                                        ));
                                    }
                                }
                            }
                        }
                    }

                    // Check arguments with substituted types
                    for (i, (arg, param)) in args.iter().zip(params.iter()).enumerate() {
                        let at = self.infer_expr(arg, scopes);
                        let subst_param = if !type_map.is_empty() {
                            subst_type_params(param, &generics, &type_map)
                        } else {
                            param.clone()
                        };
                        if !same_type(&at, &subst_param) {
                            self.emit(format!(
                                "argument {} of '{}' expected {}, found {}",
                                i + 1,
                                name,
                                fmt_type(&subst_param),
                                fmt_type(&at)
                            ));
                        }
                    }
                }
                // Substitute type args into return type
                if !type_map.is_empty() {
                    subst_type_params(&ret, &generics, &type_map)
                } else {
                    ret
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
            BinOp::Add => {
                // String concatenation: string + string -> string
                if is_string(&lt) && is_string(&rt) {
                    Type::Name("string".into(), vec![])
                } else if !same_type(&lt, &rt) || !is_numeric(&lt) {
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
            BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Pow => {
                if !same_type(&lt, &rt) || !is_numeric(&lt) {
                    self.emit(format!(
                        "arithmetic operator requires matching numeric types, found {} and {}",
                        fmt_type(&lt),
                        fmt_type(&rt)
                    ));
                    Type::Name("unknown".into(), vec![])
                } else {
                    // Static divide-by-zero detection
                    if op == BinOp::Div || op == BinOp::Mod {
                        if let Expr::Literal(Lit::Int(0)) = r {
                            self.emit(format!("{} by zero literal", if op == BinOp::Div { "division" } else { "modulo" }));
                        }
                    }
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
                    // Static modulo-by-zero detection
                    if op == BinOp::Mod {
                        if let Expr::Literal(Lit::Int(0)) = r {
                            self.emit("modulo by zero literal".to_string());
                        }
                    }
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

        let (params, mut ret) = match self.funcs.get(name) {
            Some(sig) => sig.clone(),
            None => {
                // Try module-qualified lookup via use imports
                for module in self.use_imports.clone() {
                    let qualified = format!("{}::{}", module, name);
                    if self.funcs.contains_key(&qualified) {
                        // Recursively check with qualified name
                        return self.check_call(&qualified, args, scopes);
                    }
                }
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
            // Check if this is a generic function and build type param map
            let generics = self.func_generics.get(name).cloned().unwrap_or_default();
            let mut type_map: HashMap<String, Type> = HashMap::new();

            if !generics.is_empty() {
                // Infer type parameters from argument types
                for (arg, param) in args.iter().zip(params.iter()) {
                    let at = self.infer_expr(arg, scopes);
                    self.infer_type_params(param, &at, &generics, &mut type_map);
                }

                // Check where constraints (before substitution)
                if let Some((type_param, bounds)) = self.where_clauses.get(name).cloned() {
                    for (arg, param) in args.iter().zip(params.iter()) {
                        let at = self.infer_expr(arg, scopes);
                        if self.type_uses_type_param(param, &type_param) {
                            for bound in &bounds {
                                if !self.type_implements_trait(&at, bound) {
                                    self.emit(format!(
                                        "where constraint violated: type '{}' does not implement trait '{}' (required by function '{}')",
                                        fmt_type(&at),
                                        bound,
                                        name
                                    ));
                                }
                            }
                        }
                    }
                }

                // Check arguments with substituted types
                for (i, (arg, param)) in args.iter().zip(params.iter()).enumerate() {
                    let at = self.infer_expr(arg, scopes);
                    let subst_param = subst_type_params(param, &generics, &type_map);
                    if !same_type(&at, &subst_param) {
                        self.emit(format!(
                            "argument {} of '{}' expected {}, found {}",
                            i + 1,
                            name,
                            fmt_type(&subst_param),
                            fmt_type(&at)
                        ));
                    }
                }

                ret = subst_type_params(&ret, &generics, &type_map);
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
                // Check where constraints for non-generic functions
                if let Some((type_param, bounds)) = self.where_clauses.get(name).cloned() {
                    for (arg, param) in args.iter().zip(params.iter()) {
                        let at = self.infer_expr(arg, scopes);
                        if self.type_uses_type_param(param, &type_param) {
                            for bound in &bounds {
                                if !self.type_implements_trait(&at, bound) {
                                    self.emit(format!(
                                        "where constraint violated: type '{}' does not implement trait '{}' (required by function '{}')",
                                        fmt_type(&at),
                                        bound,
                                        name
                                    ));
                                }
                            }
                        }
                    }
                }
            }

            // Check effects
            if let Some(required_effects) = self.func_effects.get(name).cloned() {
                for effect in &required_effects {
                    if !self.has_effect(effect) {
                        self.emit(format!(
                            "effect '{}' required by function '{}' is not available in current scope",
                            effect, name
                        ));
                    }
                }
            }
        }
        ret
    }

    /// Determine which variants a pattern covers.
    /// Returns (list of covered variant names, whether this is a catch-all pattern)
    pub(crate) fn pattern_covers_variants(&self, pat: &Pattern, subject_ty: &Type) -> (Vec<String>, bool) {
        match pat {
            Pattern::Wildcard => {
                // Wildcard covers all variants
                let all = self.get_enum_variants(subject_ty);
                (all, true)
            }
            Pattern::Variable(_) => {
                // Variable pattern covers all variants
                let all = self.get_enum_variants(subject_ty);
                (all, true)
            }
            Pattern::Literal(_) => {
                // Literal patterns don't cover enum variants
                (Vec::new(), false)
            }
            Pattern::Constructor(name, _) => {
                // Constructor pattern covers only that specific variant
                (vec![name.clone()], false)
            }
            Pattern::Tuple(pats) => {
                // Tuple pattern - for enum matching, this doesn't directly cover variants
                // but we need to handle nested tuple patterns that might contain constructors
                let mut covered = Vec::new();
                // For tuple patterns matching against enum types, we need the tuple element types
                if let Type::Tuple(elem_types) = subject_ty {
                    for (i, p) in pats.iter().enumerate() {
                        if i < elem_types.len() {
                            let (vars, _) = self.pattern_covers_variants(p, &elem_types[i]);
                            for v in vars {
                                if !covered.contains(&v) {
                                    covered.push(v);
                                }
                            }
                        }
                    }
                }
                (covered, false)
            }
            Pattern::Array(_) | Pattern::Slice(_, _) => {
                (Vec::new(), false)
            }
        }
    }
}
