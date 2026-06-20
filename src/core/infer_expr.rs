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
            Expr::Call(callee, args) => self.infer_call_expr(callee, args, scopes),
            Expr::Field(obj, field) => self.infer_field_access(obj, field, scopes),
            Expr::Record { ty, fields } => self.infer_record_expr(ty, fields, scopes),
            Expr::Match(target, arms) => self.infer_match_expr(target, arms, scopes),
            Expr::Unary(op, e) => {
                let t = self.infer_expr(e, scopes);
                match op {
                    UnOp::Neg => {
                        if is_numeric(&t) {
                            t
                        } else {
                            self.errors.push(
                                Diagnostic::error_code(
                                    crate::diagnostic::codes::E0201,
                                    format!("cannot negate {}", fmt_type(&t)),
                                    Span::single(self.current_line, self.current_col),
                                ).with_help("negation only works on numeric types (i32, i64, f64)")
                            );
                            Type::Name("unknown".into(), vec![])
                        }
                    }
                    UnOp::Not => {
                        if is_bool(&t) {
                            t
                        } else {
                            self.emit_code(crate::diagnostic::codes::E0203, format!("cannot apply ! to {}", fmt_type(&t)));
                            Type::Name("unknown".into(), vec![])
                        }
                    }
                    UnOp::Ref => {
                        // Check borrow rules: cannot borrow if already mutably borrowed
                        if let Expr::Ident(name) = e.as_ref() {
                            if let Some(BorrowState::BorrowedMut { span }) = self.lookup_borrow(name) {
                                let borrow_span = *span;
                                self.errors.push(
                                    Diagnostic::error_code(
                                        crate::diagnostic::codes::E0302,
                                        format!("cannot borrow '{}' as immutable because it is already mutably borrowed", name),
                                        Span::single(self.current_line, self.current_col),
                                    ).with_note("mutable borrow occurs here", borrow_span)
                                );
                            }
                            self.set_borrow(name, BorrowState::BorrowedImm { span: Span::single(self.current_line, self.current_col) });
                        }
                        Type::Ref(None, Box::new(t))
                    }
                    UnOp::RefMut => {
                        // Check borrow rules: cannot &mut if already borrowed (imm or mut)
                        if let Expr::Ident(name) = e.as_ref() {
                            if let Some(state) = self.lookup_borrow(name) {
                                match state {
                                    BorrowState::Unborrowed => {}
                                    BorrowState::BorrowedImm { span } => {
                                        let borrow_span = *span;
                                        self.errors.push(
                                            Diagnostic::error_code(
                                                crate::diagnostic::codes::E0300,
                                                format!("cannot borrow '{}' as mutable because it is already immutably borrowed", name),
                                                Span::single(self.current_line, self.current_col),
                                            ).with_note("immutable borrow occurs here", borrow_span)
                                        );
                                    }
                                    BorrowState::BorrowedMut { span } => {
                                        let borrow_span = *span;
                                        self.errors.push(
                                            Diagnostic::error_code(
                                                crate::diagnostic::codes::E0301,
                                                format!("cannot borrow '{}' as mutable because it is already mutably borrowed", name),
                                                Span::single(self.current_line, self.current_col),
                                            ).with_note("mutable borrow occurs here", borrow_span)
                                        );
                                    }
                                }
                            }
                            self.set_borrow(name, BorrowState::BorrowedMut { span: Span::single(self.current_line, self.current_col) });
                        }
                        Type::RefMut(None, Box::new(t))
                    }
                    UnOp::Deref => {
                        match &t {
                            Type::Ref(_, inner) | Type::RefMut(_, inner) => (**inner).clone(),
                            _ => {
                                self.emit_code(crate::diagnostic::codes::E0204, format!("cannot dereference {}", fmt_type(&t)));
                                Type::Name("unknown".into(), vec![])
                            }
                        }
                    }
                }
            }
            Expr::Binary(op, l, r) => self.infer_binary(*op, l, r, scopes),
            Expr::Tuple(elems) => {
                Type::Tuple(elems.iter().map(|e| self.infer_expr(e, scopes)).collect())
            }
            Expr::TupleIndex(obj, idx) => {
                let obj_ty = self.infer_expr(obj, scopes);
                match &obj_ty {
                    Type::Tuple(elems) => {
                        if *idx < elems.len() {
                            elems[*idx].clone()
                        } else {
                            self.emit_code(crate::diagnostic::codes::E0243, format!("tuple index {} out of bounds (len {})", idx, elems.len()));
                            Type::Name("unknown".into(), vec![])
                        }
                    }
                    _ => {
                        self.emit_code(crate::diagnostic::codes::E0244, format!("cannot index non-tuple type {} with .{}", fmt_type(&obj_ty), idx));
                        Type::Name("unknown".into(), vec![])
                    }
                }
            }
            Expr::List(elems) => {
                let mut elem_ty = Type::Name("unknown".into(), vec![]);
                for (i, e) in elems.iter().enumerate() {
                    let t = self.infer_expr(e, scopes);
                    if i == 0 {
                        elem_ty = t;
                    } else if !same_type(&elem_ty, &t) {
                        self.emit_code(crate::diagnostic::codes::E0242, format!(
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
                        self.emit_code(crate::diagnostic::codes::E0250, format!("comprehension requires a list, found {}", fmt_type(&iter_ty)));
                    }
                }
                // Infer element type from iter
                let elem_ty = if let Type::Name(_, args) = &iter_ty {
                    if args.len() == 1 { args[0].clone() } else { Type::Name("unknown".into(), vec![]) }
                } else {
                    Type::Name("unknown".into(), vec![])
                };
                // Add var to scope
                if let Some(s) = scopes.last_mut() {
                    s.insert(var.clone(), elem_ty);
                }
                // Infer expression type
                let expr_ty = self.infer_expr(expr, scopes);
                // Check guard if present
                if let Some(g) = guard {
                    let guard_ty = self.infer_expr(g, scopes);
                    if !matches!(&guard_ty, Type::Name(n, _) if n == "bool") {
                        self.emit_code(crate::diagnostic::codes::E0230, format!("comprehension guard must be bool, found {}", fmt_type(&guard_ty)));
                    }
                }
                Type::Name("List".into(), vec![expr_ty])
            }
            Expr::If { cond, then_, else_ } => {
                let else_ref = else_.as_ref().map(|b| { let v: &Block = b; v });
                self.infer_if_expr(cond, then_, else_ref, scopes)
            }
            Expr::Index(obj, idx) => {
                let obj_ty = self.infer_expr(obj, scopes);
                let idx_ty = self.infer_expr(idx, scopes);
                if !is_int(&idx_ty) {
                        self.emit_code(crate::diagnostic::codes::E0217, format!("index must be integer, found {}", fmt_type(&idx_ty)));
                }
                match obj_ty {
                    Type::Name(n, args) if n == "List" && args.len() == 1 => args[0].clone(),
                    Type::Name(n, _) if n == "string" => Type::Name("string".into(), vec![]),
                    _ => {
                        self.emit_code(crate::diagnostic::codes::E0218, format!("cannot index {}", fmt_type(&obj_ty)));
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
                                            self.emit_code(crate::diagnostic::codes::E0224, format!(
                                                "? operator: cannot determine success type from enum '{}'",
                                                name
                                            ));
                                            Type::Name("unknown".into(), vec![])
                                        }
                                    }
                                }
                                _ => {
                                    self.emit_code(crate::diagnostic::codes::E0224, format!(
                                        "? operator requires Result or Option type, found '{}'",
                                        name
                                    ));
                                    Type::Name("unknown".into(), vec![])
                                }
                            }
                        } else {
                            self.emit_code(crate::diagnostic::codes::E0224, format!(
                                "? operator requires Result or Option type, found '{}'",
                                name
                            ));
                            Type::Name("unknown".into(), vec![])
                        }
                    }
                    Type::Infer => {
                        // _ type in let binding: infer from init expression
                        Type::Name("unknown".into(), vec![])
                    }
                    _ => {
                        self.emit_code(crate::diagnostic::codes::E0224, format!(
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
                let inner_ty = self.infer_expr(inner, scopes);
                match inner_ty {
                    Type::Name(n, args) if n == "Future" && !args.is_empty() => args[0].clone(),
                    other => {
                        self.emit_code(crate::diagnostic::codes::E0245, format!("await requires Future type, found {}", fmt_type(&other)));
                        Type::Name("unknown".into(), vec![])
                    }
                }
            }
            Expr::Quote(_) => Type::Name("AST".into(), vec![]),
            Expr::QuoteInterpolate(inner) => self.infer_expr(inner, scopes),
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
            Expr::Range { start, end } => {
                let _ = self.infer_expr(start, scopes);
                let _ = self.infer_expr(end, scopes);
                Type::Name("Range".into(), vec![])
            }
            Expr::TypeInfo(_) => {
                // type_info returns a record with type metadata
                Type::Name("TypeInfo".into(), vec![])
            }
            Expr::Old(expr) => {
                // old(x) returns the same type as x
                self.infer_expr(expr, scopes)
            }
            Expr::Lambda { params, ret, body } => {
                let param_types: Vec<Type> = params.iter().map(|p| self.resolve_type(&p.ty)).collect();
                scopes.push(HashMap::new());
                for p in params {
                    if let Some(s) = scopes.last_mut() {
                        s.insert(p.name.clone(), self.resolve_type(&p.ty));
                    }
                }
                let mut body_type = Type::Name("unit".into(), vec![]);
                for stmt in body {
                    match stmt {
                        Stmt::Expr(e) => body_type = self.infer_expr(e, scopes),
                        Stmt::Return(Some(e)) => { body_type = self.infer_expr(e, scopes); break; }
                        _ => {}
                    }
                }
                scopes.pop();
                let return_type = ret.clone().unwrap_or(body_type);
                Type::Func(param_types, Box::new(return_type))
            }
            Expr::Turbofish(name, type_args, args) => {
                // Turbofish: func::<Type>(args) — explicit type instantiation
                let (params, ret) = match self.funcs.get(name) {
                    Some(sig) => sig.clone(),
                    None => {
                        self.emit_code(crate::diagnostic::codes::E0401, format!("undefined function '{}'", name));
                        return Type::Name("unknown".into(), vec![]);
                    }
                };
                let generics = self.func_generics.get(name).cloned().unwrap_or_default();

                // Build type param map from turbofish type args
                let mut type_map: HashMap<String, Type> = HashMap::new();
                if !generics.is_empty() && !type_args.is_empty() {
                    if type_args.len() != generics.len() {
                        self.emit_code(crate::diagnostic::codes::E0239, format!(
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
                    self.emit_code(crate::diagnostic::codes::E0257, format!(
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
                                        self.emit_code(crate::diagnostic::codes::E0253, format!(
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
                            self.emit_code(crate::diagnostic::codes::E0211, format!(
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

    fn infer_call_expr(&mut self, callee: &Expr, args: &[Expr], scopes: &mut Vec<HashMap<String, Type>>) -> Type {
        match callee {
            Expr::Ident(name) => self.check_call(name, args, scopes),
            Expr::Field(obj, method_name) => self.infer_method_call(obj, method_name, args, scopes),
            _ => {
                self.emit_code(crate::diagnostic::codes::E0223, "callee must be a function name");
                Type::Name("unknown".into(), vec![])
            }
        }
    }

    fn infer_method_call(&mut self, obj: &Expr, method_name: &str, args: &[Expr], scopes: &mut Vec<HashMap<String, Type>>) -> Type {
        let obj_ty = self.infer_expr(obj, scopes);
        if let Type::Name(type_name, type_args) = &obj_ty {
            // Check built-in Option/Result methods; fall through to trait dispatch for unknown methods
            if type_name == "Option" && type_args.len() == 1 {
                let known = ["unwrap", "expect", "unwrap_or", "is_some", "is_none", "ok_or", "map", "and_then", "map_err"];
                if known.contains(&method_name) {
                    return self.check_option_method(method_name, &type_args[0], args, scopes);
                }
            } else if type_name == "Result" && type_args.len() == 2 {
                let known = ["unwrap", "expect", "unwrap_or", "is_ok", "is_err", "map", "and_then", "map_err", "ok_or"];
                if known.contains(&method_name) {
                    return self.check_result_method(method_name, &type_args[0], &type_args[1], args, scopes);
                }
            }
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
                    if let Some(f) = fields.iter().find(|f| f.name == method_name) {
                        return self.resolve_type(&f.ty);
                    }
                }
                if let TypeDefKind::Enum(variants) = &tdef.kind {
                    if variants.iter().any(|v| v.name == method_name) {
                        return Type::Name(type_name.clone(), vec![]);
                    }
                }
            }
            // Check trait methods on this type
            if let Some(methods) = self.type_methods.get(type_name) {
                if let Some((trait_name, _)) = methods.iter().find(|(_, m)| m == method_name) {
                    let trait_name = trait_name.clone();
                    if let Some((params, ret)) = self.trait_method_sigs.get(&(trait_name.clone(), method_name.to_string())).cloned() {
                        let (method_params, method_ret) = if let Some(trait_generic_names) = self.trait_generics.get(&trait_name) {
                            if !trait_generic_names.is_empty() && trait_generic_names.len() == type_args.len() {
                                let type_map: HashMap<String, Type> = trait_generic_names.iter()
                                    .zip(type_args.iter())
                                    .map(|(g, a)| (g.clone(), a.clone()))
                                    .collect();
                                let gen_slice: Vec<GenericParam> = trait_generic_names.iter()
                                    .map(|g| GenericParam { name: g.clone(), bounds: vec![] })
                                    .collect();
                                let subst_params: Vec<Type> = params.iter()
                                    .map(|p| subst_type_params(p, &gen_slice, &type_map))
                                    .collect();
                                let subst_ret = subst_type_params(&ret, &gen_slice, &type_map);
                                (subst_params, subst_ret)
                            } else {
                                (params, ret)
                            }
                        } else {
                            (params, ret)
                        };
                        let user_args = &args;
                        if user_args.len() != method_params.len() {
                            self.emit_code(crate::diagnostic::codes::E0257, format!(
                                "method '{}' of trait '{}' expects {} arguments, got {}",
                                method_name, trait_name, method_params.len(), user_args.len()
                            ));
                        } else {
                            for (i, (arg, param)) in user_args.iter().zip(method_params.iter()).enumerate() {
                                let at = self.infer_expr(arg, scopes);
                                if !same_type(&at, param) {
                                    self.emit_code(crate::diagnostic::codes::E0211, format!(
                                        "argument {} of method '{}' expected {}, found {}",
                                        i + 1, method_name, fmt_type(param), fmt_type(&at)
                                    ));
                                }
                            }
                        }
                        return method_ret;
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
            // Check string methods
            if type_name == "string" {
                return self.check_string_method(method_name, args, scopes);
            }
            // Check list methods
            if type_name == "List" && method_name == "len" {
                return self.check_list_method(method_name, args, scopes);
            }
            let mut method_candidates: Vec<String> = self.type_methods.get(type_name)
                .map(|methods| methods.iter().map(|(_, m)| m.clone()).collect())
                .unwrap_or_default();
            if let Some(actor_def) = self.file.items.iter().find_map(|item| {
                if let Item::Actor(a) = item { if a.name == *type_name { Some(a) } else { None } } else { None }
            }) {
                method_candidates.extend(actor_def.methods.iter().map(|m| m.name.clone()));
            }
            let suggestion = super::suggest_name(method_name, &method_candidates, 3);
            let help = if let Some(s) = suggestion {
                format!("did you mean '{}'?", s)
            } else {
                "check the method name spelling or available methods for this type".to_string()
            };
            self.errors.push(
                Diagnostic::error_code(
                    crate::diagnostic::codes::E0221,
                    format!("type '{}' has no method '{}'", type_name, method_name),
                    Span::single(self.current_line, self.current_col),
                ).with_help(&help)
            );
            Type::Name("unknown".into(), vec![])
        } else if let Type::DynTrait(traits) = &obj_ty {
            self.resolve_trait_method(traits, method_name, args, scopes)
        } else if let Type::ImplTrait(traits) = &obj_ty {
            self.resolve_trait_method(traits, method_name, args, scopes)
        } else if let Type::Option(inner) = &obj_ty {
            self.check_option_method(method_name, inner, args, scopes)
        } else if let Type::Result(ok_ty, err_ty) = &obj_ty {
            self.check_result_method(method_name, ok_ty, err_ty, args, scopes)
        } else {
            self.errors.push(
                Diagnostic::error_code(
                    crate::diagnostic::codes::E0222,
                    format!("method call requires a named type, found {}", fmt_type(&obj_ty)),
                    Span::single(self.current_line, self.current_col),
                ).with_help("only named types (record, enum, actor) have methods")
            );
            Type::Name("unknown".into(), vec![])
        }
    }

    fn resolve_trait_method(&mut self, traits: &[String], method_name: &str, args: &[Expr], scopes: &mut Vec<HashMap<String, Type>>) -> Type {
        for trait_name in traits {
            if let Some((params, ret)) = self.trait_method_sigs.get(&(trait_name.clone(), method_name.to_string())).cloned() {
                let user_args = &args;
                let method_params = &params;
                if user_args.len() != method_params.len() {
                    self.emit_code(crate::diagnostic::codes::E0257, format!(
                        "method '{}' of trait '{}' expects {} arguments, got {}",
                        method_name, trait_name, method_params.len(), user_args.len()
                    ));
                } else {
                    for (i, (arg, param)) in user_args.iter().zip(method_params.iter()).enumerate() {
                        let at = self.infer_expr(arg, scopes);
                        if !same_type(&at, param) {
                            self.emit_code(crate::diagnostic::codes::E0211, format!(
                                "argument {} of method '{}' expected {}, found {}",
                                i + 1, method_name, fmt_type(param), fmt_type(&at)
                            ));
                        }
                    }
                }
                return ret;
            }
        }
        self.errors.push(
            Diagnostic::error_code(
                crate::diagnostic::codes::E0221,
                format!("trait object does not have method '{}'", method_name),
                Span::single(self.current_line, self.current_col),
            ).with_help("check the method name spelling or available methods for this type")
        );
        Type::Name("unknown".into(), vec![])
    }

    fn infer_field_access(&mut self, obj: &Expr, field: &str, scopes: &mut Vec<HashMap<String, Type>>) -> Type {
        let obj_ty = self.infer_expr(obj, scopes);
        self.infer_field_access_on_type(&obj_ty, field, scopes)
    }

    fn infer_field_access_on_type(&mut self, obj_ty: &Type, field: &str, scopes: &mut Vec<HashMap<String, Type>>) -> Type {
        match obj_ty {
            Type::Name(name, _) => {
                if let Some(actor_def) = self.file.items.iter().find_map(|item| {
                    if let Item::Actor(a) = item {
                        if a.name == *name { Some(a) } else { None }
                    } else { None }
                }) {
                    if let Some(f) = actor_def.fields.iter().find(|f| f.name == field) {
                        return self.resolve_type(&f.ty);
                    }
                    let field_names: Vec<String> = actor_def.fields.iter().map(|f| f.name.clone()).collect();
                    let suggestion = super::suggest_name(field, &field_names, 3);
                    let help = if let Some(s) = suggestion {
                        format!("did you mean '{}'?", s)
                    } else {
                        format!("available fields: {}", field_names.join(", "))
                    };
                    self.errors.push(Diagnostic::error_code(
                        crate::diagnostic::codes::E0220,
                        format!("actor '{}' has no field '{}'", name, field),
                        Span::single(self.current_line, self.current_col),
                    ).with_help(&help));
                    return Type::Name("unknown".into(), vec![]);
                }
                if let Some(tdef) = self.types.get(name) {
                    match &tdef.kind {
                        TypeDefKind::Record(fields) => {
                            if let Some(f) = fields.iter().find(|f| f.name == field) {
                                return self.resolve_type(&f.ty);
                            }
                            if let Some(methods) = self.type_methods.get(name) {
                                if let Some((trait_name, _)) = methods.iter().find(|(_, m)| m == field) {
                                    let tn = trait_name.clone();
                                    if let Some((params, ret)) = self.trait_method_sigs.get(&(tn, field.to_string())).cloned() {
                                        return Type::Func(params, Box::new(ret));
                                    }
                                }
                            }
                            let field_names: Vec<String> = fields.iter().map(|f| f.name.clone()).collect();
                            let suggestion = super::suggest_name(field, &field_names, 3);
                            self.errors.push(Diagnostic::error_code(
                                crate::diagnostic::codes::E0220,
                                format!("type '{}' has no field '{}'", name, field),
                                Span::single(self.current_line, self.current_col),
                            ).with_help(&suggestion.map(|s| format!("did you mean '{}'?", s)).unwrap_or_default()));
                            Type::Name("unknown".into(), vec![])
                        }
                        TypeDefKind::Enum(variants) => {
                            if variants.iter().any(|v| v.name == field) {
                                let variant_func = format!("{}::{}", name, field);
                                if let Some((params, ret)) = self.funcs.get(&variant_func) {
                                    Type::Func(params.clone(), Box::new(ret.clone()))
                                } else {
                                    Type::Name(name.into(), vec![])
                                }
                            } else {
                                let variant_names: Vec<String> = variants.iter().map(|v| v.name.clone()).collect();
                                let suggestion = super::suggest_name(field, &variant_names, 3);
                                self.errors.push(Diagnostic::error_code(
                                    crate::diagnostic::codes::E0246,
                                    if let Some(s) = suggestion {
                                        format!("type '{}' has no variant '{}' — did you mean '{}'?", name, field, s)
                                    } else {
                                        format!("type '{}' has no variant '{}' — available variants: {}", name, field, variant_names.join(", "))
                                    },
                                    Span::single(self.current_line, self.current_col),
                                ).with_help("check the variant name spelling"));
                                Type::Name("unknown".into(), vec![])
                            }
                        }
                        _ => {
                            if let Some(methods) = self.type_methods.get(name) {
                                if let Some((trait_name, _)) = methods.iter().find(|(_, m)| m == field) {
                                    let tn = trait_name.clone();
                                    if let Some((params, ret)) = self.trait_method_sigs.get(&(tn, field.to_string())).cloned() {
                                        return Type::Func(params, Box::new(ret));
                                    }
                                }
                            }
                            self.emit_code(crate::diagnostic::codes::E0249, format!("'{}' is not a record type", name));
                            Type::Name("unknown".into(), vec![])
                        }
                    }
                } else {
                    self.emit_code(crate::diagnostic::codes::E0220, format!("field access on unknown type '{}'", name));
                    Type::Name("unknown".into(), vec![])
                }
            }
            Type::Tuple(elems) => match field.parse::<usize>() {
                Ok(idx) if idx < elems.len() => elems[idx].clone(),
                _ => {
                    self.emit_code(crate::diagnostic::codes::E0223,
                        format!("tuple of {} elements has no field '{}'", elems.len(), field));
                    Type::Name("unknown".into(), vec![])
                }
            },
            Type::Ref(_, inner) | Type::RefMut(_, inner) =>
                self.infer_field_deref(inner, field, scopes),
            Type::Shared(inner) | Type::LocalShared(inner) =>
                self.infer_field_deref(inner, field, scopes),
            Type::Newtype(_, inner) =>
                self.infer_field_deref(inner, field, scopes),
            Type::Infer => Type::Infer,
            _ => {
                self.errors.push(Diagnostic::error_code(
                    crate::diagnostic::codes::E0219,
                    format!("field access requires record type, found {}", fmt_type(&obj_ty)),
                    Span::single(self.current_line, self.current_col),
                ).with_help("only record types support field access with '.'"));
                Type::Name("unknown".into(), vec![])
            }
        }
    }

    fn infer_field_deref(&mut self, inner: &Type, field: &str, scopes: &mut Vec<HashMap<String, Type>>) -> Type {
        self.infer_field_access_on_type(inner, field, scopes)
    }

    fn infer_record_expr(&mut self, ty: &Option<String>, fields: &[RecordFieldExpr], scopes: &mut Vec<HashMap<String, Type>>) -> Type {
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
                                    self.emit_code(crate::diagnostic::codes::E0247, format!(
                                        "field '{}' expected {}, found {}",
                                        name, fmt_type(expected_ty), fmt_type(&actual_ty)
                                    ));
                                }
                            } else {
                                self.emit_code(crate::diagnostic::codes::E0247, format!(
                                    "type '{}' has no field '{}'", tdef.name, name
                                ));
                            }
                        }
                        for name in expected.keys() {
                            if !fields.iter().any(|f| &f.name == name) {
                                self.emit_code(crate::diagnostic::codes::E0248, format!(
                                    "missing field '{}' in record literal", name
                                ));
                            }
                        }
                        Type::Name(tdef.name.clone(), vec![])
                    }
                    _ => {
                        self.emit_code(crate::diagnostic::codes::E0249, format!("'{}' is not a record type", tdef.name));
                        Type::Name("unknown".into(), vec![])
                    }
                }
            }
            None => {
                self.emit_code(crate::diagnostic::codes::E0410, "cannot infer record type without explicit type name");
                Type::Name("unknown".into(), vec![])
            }
        }
    }

    fn infer_match_expr(&mut self, subject: &Expr, arms: &[MatchArm], scopes: &mut Vec<HashMap<String, Type>>) -> Type {
        let subject_ty = self.infer_expr(subject, scopes);
        if arms.is_empty() {
            self.emit_code(crate::diagnostic::codes::E0213, "match expression must have at least one arm");
            return Type::Name("unknown".into(), vec![]);
        }

        let all_variants = self.get_enum_variants(&subject_ty);
        let mut covered_variants: Vec<String> = Vec::new();
        let mut has_catchall = false;
        let mut has_guard = false;
        let mut result_ty: Option<Type> = None;

        for arm in arms {
            let (pattern_covered, is_catchall) = self.pattern_covers_variants(&arm.pat, &subject_ty);
            if is_catchall { has_catchall = true; }
            for variant in pattern_covered {
                if !covered_variants.contains(&variant) { covered_variants.push(variant); }
            }

            scopes.push(HashMap::new());
            self.check_pattern(&arm.pat, &subject_ty, scopes);
            if let Some(guard) = &arm.guard {
                has_guard = true;
                let gt = self.infer_expr(guard, scopes);
                if !is_bool(&gt) {
                    self.emit_code(crate::diagnostic::codes::E0216, format!(
                        "match guard must be bool, found {}", fmt_type(&gt)
                    ));
                }
            }
            let body_ty = self.infer_expr(&arm.body, scopes);
            scopes.pop();

            match &result_ty {
                None => result_ty = Some(body_ty),
                Some(rt) => if !same_type(rt, &body_ty) {
                    self.emit_code(crate::diagnostic::codes::E0214, format!(
                        "match arm body type {} does not match previous {}",
                        fmt_type(&body_ty), fmt_type(rt)
                    ));
                }
            }
        }

        if !all_variants.is_empty() && !has_catchall && !has_guard {
            for variant in &all_variants {
                if !covered_variants.contains(variant) {
                    self.errors.push(Diagnostic::error_code(
                        crate::diagnostic::codes::E0215,
                        format!("match expression is not exhaustive: missing variant '{}' of '{}'", variant, fmt_type(&subject_ty)),
                        Span::single(self.current_line, self.current_col),
                    ).with_help(format!("add an arm for '{}' or a wildcard '_ => ...' arm", variant)));
                }
            }
        }

        result_ty.unwrap_or_else(|| Type::Name("unknown".into(), vec![]))
    }

    fn infer_if_expr(&mut self, cond: &Expr, then_: &Block, else_: Option<&Block>, scopes: &mut Vec<HashMap<String, Type>>) -> Type {
        self.infer_expr(cond, scopes);
        let then_ty = self.infer_block_expr(then_, scopes);
        if let Some(eb) = else_ {
            let else_ty = self.infer_block_expr(eb, scopes);
            if same_type(&then_ty, &else_ty) { then_ty } else { Type::Name("unknown".into(), vec![]) }
        } else {
            then_ty
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
                self.emit_code(crate::diagnostic::codes::E0202, format!(
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
                    self.emit_code(crate::diagnostic::codes::E0202, format!(
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
                    self.emit_code(crate::diagnostic::codes::E0202, format!(
                        "arithmetic operator requires matching numeric types, found {} and {}",
                        fmt_type(&lt),
                        fmt_type(&rt)
                    ));
                    Type::Name("unknown".into(), vec![])
                } else {
                    // Static divide-by-zero detection
                    if op == BinOp::Div || op == BinOp::Mod {
                        if let Expr::Literal(Lit::Int(0)) = r {
                            self.emit_code(crate::diagnostic::codes::E0237, format!("{} by zero literal", if op == BinOp::Div { "division" } else { "modulo" }));
                        }
                    }
                    lt
                }
            }
            BinOp::Mod | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => {
                if !same_type(&lt, &rt) || !is_int(&lt) {
                    self.emit_code(crate::diagnostic::codes::E0202, format!(
                        "operator requires matching integer types, found {} and {}",
                        fmt_type(&lt),
                        fmt_type(&rt)
                    ));
                    Type::Name("unknown".into(), vec![])
                } else {
                    // Static modulo-by-zero detection
                    if op == BinOp::Mod {
                        if let Expr::Literal(Lit::Int(0)) = r {
                            self.emit_code(crate::diagnostic::codes::E0238, "modulo by zero literal".to_string());
                        }
                    }
                    lt
                }
            }
            BinOp::EqCmp | BinOp::NeCmp => {
                if !same_type(&lt, &rt) {
                    self.emit_code(crate::diagnostic::codes::E0202, format!(
                        "equality requires matching types, found {} and {}",
                        fmt_type(&lt),
                        fmt_type(&rt)
                    ));
                }
                Type::Name("bool".into(), vec![])
            }
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                if !same_type(&lt, &rt) || !(is_numeric(&lt) || is_string(&lt)) {
                    self.emit_code(crate::diagnostic::codes::E0202, format!(
                        "comparison requires matching numeric or string types, found {} and {}",
                        fmt_type(&lt),
                        fmt_type(&rt)
                    ));
                }
                Type::Name("bool".into(), vec![])
            }
            BinOp::Range => {
                if !same_type(&lt, &rt) || !is_int(&lt) {
                    self.emit_code(crate::diagnostic::codes::E0202, format!(
                        "range requires matching integer types, found {} and {}",
                        fmt_type(&lt),
                        fmt_type(&rt)
                    ));
                    Type::Name("unknown".into(), vec![])
                } else {
                    Type::Name("Range".into(), vec![])
                }
            }
            BinOp::And | BinOp::Or => panic!("logical operators should be handled above"),
            BinOp::Assign => {
                self.emit_code(crate::diagnostic::codes::E0224, "assignment is not a valid expression in v0.2");
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
                    self.emit_code(crate::diagnostic::codes::E0242, "assert expects 1 argument");
                } else {
                    let t = self.infer_expr(&args[0], scopes);
                    if !is_bool(&t) {
                        self.emit_code(crate::diagnostic::codes::E0242, format!("assert expects bool, found {}", fmt_type(&t)));
                    }
                }
                return Type::Name("unit".into(), vec![]);
            }
            "range" => {
                if args.len() != 2 {
                    self.emit_code(crate::diagnostic::codes::E0242, "range expects 2 arguments");
                } else {
                    let t1 = self.infer_expr(&args[0], scopes);
                    let t2 = self.infer_expr(&args[1], scopes);
                    if !is_int(&t1) || !is_int(&t2) {
                        self.emit_code(crate::diagnostic::codes::E0242, "range expects integer arguments");
                    }
                }
                return Type::Name("List".into(), vec![Type::Name("i32".into(), vec![])]);
            }
            "sqrt" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "sqrt expects 1 argument");
                } else {
                    let t = self.infer_expr(&args[0], scopes);
                    if !is_numeric(&t) {
                        self.emit_code(crate::diagnostic::codes::E0242, "sqrt expects a numeric argument");
                    }
                }
                return Type::Name("f64".into(), vec![]);
            }
            "len" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "len expects 1 argument");
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                return Type::Name("i32".into(), vec![]);
            }
            "to_string" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "to_string expects 1 argument");
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                return Type::Name("string".into(), vec![]);
            }
            "to_int" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "to_int expects 1 argument");
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                return Type::Name("i32".into(), vec![]);
            }
            "to_float" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "to_float expects 1 argument");
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                return Type::Name("f64".into(), vec![]);
            }
            "abs" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "abs expects 1 argument");
                } else {
                    let t = self.infer_expr(&args[0], scopes);
                    if !is_numeric(&t) {
                        self.emit_code(crate::diagnostic::codes::E0242, "abs expects a numeric argument");
                    }
                }
                return Type::Name("unknown".into(), vec![]);
            }
            "push" => {
                if args.len() != 2 {
                    self.emit_code(crate::diagnostic::codes::E0242, "push expects 2 arguments");
                } else {
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
                }
                return Type::Name("unit".into(), vec![]);
            }
            "pop" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "pop expects 1 argument");
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                return Type::Name("unknown".into(), vec![]);
            }
            "min" | "max" => {
                if args.len() != 2 {
                    self.emit_code(crate::diagnostic::codes::E0242, format!("{} expects 2 arguments", name));
                } else {
                    let t1 = self.infer_expr(&args[0], scopes);
                    let t2 = self.infer_expr(&args[1], scopes);
                    if !same_type(&t1, &t2) {
                        self.emit_code(crate::diagnostic::codes::E0242, format!("{} expects matching types, found {} and {}", name, fmt_type(&t1), fmt_type(&t2)));
                    }
                }
                return Type::Name("unknown".into(), vec![]);
            }
            "contains" => {
                if args.len() != 2 {
                    self.emit_code(crate::diagnostic::codes::E0242, "contains expects 2 arguments");
                } else {
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
                }
                return Type::Name("bool".into(), vec![]);
            }
            "assert_eq" | "assert_ne" => {
                if args.len() != 2 {
                    self.emit_code(crate::diagnostic::codes::E0242, format!("{} expects 2 arguments", name));
                } else {
                    let t1 = self.infer_expr(&args[0], scopes);
                    let t2 = self.infer_expr(&args[1], scopes);
                    if !same_type(&t1, &t2) {
                        self.emit_code(crate::diagnostic::codes::E0242, format!("{} expects matching types, found {} and {}", name, fmt_type(&t1), fmt_type(&t2)));
                    }
                }
                return Type::Name("unit".into(), vec![]);
            }
            "input" => {
                return Type::Result(
                    Box::new(Type::Name("string".into(), vec![])),
                    Box::new(Type::Name("string".into(), vec![])),
                );
            }
            "map" => {
                if args.len() != 2 {
                    self.emit_code(crate::diagnostic::codes::E0242, "map expects 2 arguments (list, closure)");
                } else {
                    let list_ty = self.infer_expr(&args[0], scopes);
                    let elem_ty = match &list_ty {
                        Type::Name(_, args) if args.len() == 1 => args[0].clone(),
                        _ => Type::Name("unknown".into(), vec![]),
                    };
                    let closure_ty = self.infer_expr(&args[1], scopes);
                    let ret_ty = match &closure_ty {
                        Type::Func(_, ret) => ret.as_ref().clone(),
                        _ => elem_ty.clone(),
                    };
                    return Type::Name("List".into(), vec![ret_ty]);
                }
                return Type::Name("unknown".into(), vec![]);
            }
            "filter" => {
                if args.len() != 2 {
                    self.emit_code(crate::diagnostic::codes::E0242, "filter expects 2 arguments (list, closure)");
                } else {
                    let list_ty = self.infer_expr(&args[0], scopes);
                    let elem_ty = match &list_ty {
                        Type::Name(_, args) if args.len() == 1 => args[0].clone(),
                        _ => Type::Name("unknown".into(), vec![]),
                    };
                    self.infer_expr(&args[1], scopes);
                    return Type::Name("List".into(), vec![elem_ty]);
                }
                return Type::Name("unknown".into(), vec![]);
            }
            "reduce" => {
                if args.len() != 3 {
                    self.emit_code(crate::diagnostic::codes::E0242, "reduce expects 3 arguments");
                } else {
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
                    self.infer_expr(&args[2], scopes);
                }
                return Type::Name("unknown".into(), vec![]);
            }
            "sort" | "reverse" | "flatten" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, format!("{} expects 1 argument", name));
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                return Type::Name("List".into(), vec![Type::Name("unknown".into(), vec![])]);
            }
            "zip" => {
                if args.len() != 2 {
                    self.emit_code(crate::diagnostic::codes::E0242, "zip expects 2 arguments (list, list)");
                } else {
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
                }
                return Type::Name("List".into(), vec![Type::Name("unknown".into(), vec![])]);
            }
            "sum" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "sum expects 1 argument");
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                return Type::Name("i32".into(), vec![]);
            }
            "pow" | "floor" | "ceil" | "round" => {
                if args.len() != 2 {
                    self.emit_code(crate::diagnostic::codes::E0242, format!("{} expects 2 arguments", name));
                } else {
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
                }
                return Type::Name("f64".into(), vec![]);
            }
            "random" => {
                return Type::Name("f64".into(), vec![]);
            }
            "pi" => {
                return Type::Name("f64".into(), vec![]);
            }
            "now" | "timestamp" | "now_ms" | "timestamp_ms" => {
                return Type::Name("i64".into(), vec![]);
            }
            "sleep" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "sleep expects 1 argument (milliseconds)");
                } else {
                    let t = self.infer_expr(&args[0], scopes);
                    if !is_int(&t) {
                        self.emit_code(crate::diagnostic::codes::E0242, "sleep expects an integer argument");
                    }
                }
                return Type::Name("unit".into(), vec![]);
            }
            "type_name" | "type_fields" | "type_variants" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, format!("{} expects 1 argument", name));
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                return Type::Name("string".into(), vec![]);
            }
            "keys" | "values" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, format!("{} expects 1 argument", name));
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                return Type::Name("List".into(), vec![Type::Name("unknown".into(), vec![])]);
            }
            "has_key" => {
                if args.len() != 2 {
                    self.emit_code(crate::diagnostic::codes::E0242, "has_key expects 2 arguments");
                } else {
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
                }
                return Type::Name("bool".into(), vec![]);
            }
            "print" => {
                for a in args {
                    self.infer_expr(a, scopes);
                }
                return Type::Name("unit".into(), vec![]);
            }
            "ast_dump" | "ast_eval" => {
                for a in args {
                    self.infer_expr(a, scopes);
                }
                return Type::Name("unknown".into(), vec![]);
            }
            "allocator_system" | "allocator_arena" | "allocator_bump" => {
                return Type::Name("unknown".into(), vec![]);
            }
            "alloc" => {
                for a in args {
                    self.infer_expr(a, scopes);
                }
                return Type::Name("unknown".into(), vec![]);
            }
            "arena_reset" | "bump_used" => {
                for a in args {
                    self.infer_expr(a, scopes);
                }
                return Type::Name("unit".into(), vec![]);
            }
            "read_file" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "read_file expects 1 argument");
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                return Type::Result(
                    Box::new(Type::Name("string".into(), vec![])),
                    Box::new(Type::Name("string".into(), vec![])),
                );
            }
            "write_file" => {
                if args.len() != 2 {
                    self.emit_code(crate::diagnostic::codes::E0242, "write_file expects 2 arguments");
                } else {
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
                }
                return Type::Result(
                    Box::new(Type::Name("unit".into(), vec![])),
                    Box::new(Type::Name("string".into(), vec![])),
                );
            }
            "file_exists" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "file_exists expects 1 argument");
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                return Type::Name("bool".into(), vec![]);
            }
            "str_split" => {
                if args.len() != 2 {
                    self.emit_code(crate::diagnostic::codes::E0242, "str_split expects 2 arguments (string, delimiter)");
                } else {
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
                }
                return Type::Name("List".into(), vec![Type::Name("string".into(), vec![])]);
            }
            "str_join" => {
                if args.len() != 2 {
                    self.emit_code(crate::diagnostic::codes::E0242, "str_join expects 2 arguments (list, separator)");
                } else {
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
                }
                return Type::Name("string".into(), vec![]);
            }
            "str_trim" | "str_to_upper" | "str_to_lower" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, format!("{} expects 1 argument", name));
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                return Type::Name("string".into(), vec![]);
            }
            "str_starts_with" | "str_ends_with" | "str_contains" => {
                if args.len() != 2 {
                    self.emit_code(crate::diagnostic::codes::E0242, format!("{} expects 2 arguments", name));
                } else {
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
                }
                return Type::Name("bool".into(), vec![]);
            }
            "str_replace" => {
                if args.len() != 3 {
                    self.emit_code(crate::diagnostic::codes::E0242, "str_replace expects 3 arguments");
                } else {
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
                    self.infer_expr(&args[2], scopes);
                }
                return Type::Name("string".into(), vec![]);
            }
            "str_repeat" => {
                if args.len() != 2 {
                    self.emit_code(crate::diagnostic::codes::E0242, "str_repeat expects 2 arguments");
                } else {
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
                }
                return Type::Name("string".into(), vec![]);
            }
            "str_char_at" => {
                if args.len() != 2 {
                    self.emit_code(crate::diagnostic::codes::E0242, "str_char_at expects 2 arguments");
                } else {
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
                }
                return Type::Name("string".into(), vec![]);
            }
            "str_substring" => {
                if args.len() != 3 {
                    self.emit_code(crate::diagnostic::codes::E0242, "str_substring expects 3 arguments");
                } else {
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
                    self.infer_expr(&args[2], scopes);
                }
                return Type::Name("string".into(), vec![]);
            }
            "str_index_of" => {
                if args.len() != 2 {
                    self.emit_code(crate::diagnostic::codes::E0242, "str_index_of expects 2 arguments");
                } else {
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
                }
                return Type::Result(
                    Box::new(Type::Name("i32".into(), vec![])),
                    Box::new(Type::Name("i32".into(), vec![])),
                );
            }
            "str_parse_int" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "str_parse_int expects 1 argument");
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                return Type::Result(
                    Box::new(Type::Name("i32".into(), vec![])),
                    Box::new(Type::Name("string".into(), vec![])),
                );
            }
            "str_parse_float" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "str_parse_float expects 1 argument");
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                return Type::Result(
                    Box::new(Type::Name("f64".into(), vec![])),
                    Box::new(Type::Name("string".into(), vec![])),
                );
            }
            "eprintln" => {
                for a in args {
                    self.infer_expr(a, scopes);
                }
                return Type::Name("unit".into(), vec![]);
            }
            "str_to_c_str" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "str_to_c_str expects 1 argument");
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                return Type::Tuple(vec![
                    Type::Name("i64".into(), vec![]),
                    Type::Name("i64".into(), vec![]),
                ]);
            }
            "c_str_to_string" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "c_str_to_string expects 1 argument");
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                return Type::Name("string".into(), vec![]);
            }
            "from_json" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "from_json expects 1 argument");
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                return Type::Name("string".into(), vec![]);
            }
            "json_is_valid" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "json_is_valid expects 1 argument");
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                return Type::Name("bool".into(), vec![]);
            }
            "json_get_string" => {
                if args.len() != 2 {
                    self.emit_code(crate::diagnostic::codes::E0242, "json_get_string expects 2 arguments");
                } else {
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
                }
                return Type::Name("string".into(), vec![]);
            }
            "json_get_int" => {
                if args.len() != 2 {
                    self.emit_code(crate::diagnostic::codes::E0242, "json_get_int expects 2 arguments");
                } else {
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
                }
                return Type::Name("i32".into(), vec![]);
            }
            "json_get_element" => {
                if args.len() != 2 {
                    self.emit_code(crate::diagnostic::codes::E0242, "json_get_element expects 2 arguments");
                } else {
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
                }
                return Type::Name("string".into(), vec![]);
            }
            _ => {}
        }

        let (params, mut ret) = match self.funcs.get(name) {
            Some(sig) => sig.clone(),
            None => {
                // Try closure/lambda variable lookup: check if the name is a local
                // variable with a function type (let f = fn(x) { ... }; f(42))
                let closure_sig: Option<(Vec<Type>, Type)> = scopes.iter().rev()
                    .find_map(|scope| scope.get(name).cloned())
                    .and_then(|ty| match ty {
                        Type::Func(params, ret) => Some((params, *ret)),
                        _ => None,
                    });
                if let Some((param_types, ret_ty)) = closure_sig {
                    if args.len() != param_types.len() {
                        self.emit_code(crate::diagnostic::codes::E0257, format!("closure '{}' expects {} arguments, got {}", name, param_types.len(), args.len()));
                    } else {
                        for (i, (arg, param_ty)) in args.iter().zip(param_types.iter()).enumerate() {
                            let arg_ty = self.infer_expr(arg, scopes);
                            if !same_type(&arg_ty, param_ty) {
                                self.emit_code(crate::diagnostic::codes::E0211, format!("argument {} of closure '{}' expected {}, found {}", i + 1, name, fmt_type(param_ty), fmt_type(&arg_ty)));
                            }
                        }
                    }
                    return ret_ty;
                }
                // Try built-in Option/Result constructors as fallback
                match name {
                    "Some" => {
                        if args.len() != 1 {
                            self.emit_code(crate::diagnostic::codes::E0242, "Some expects 1 argument");
                        } else {
                            let inner = self.infer_expr(&args[0], scopes);
                            return Type::Option(Box::new(inner));
                        }
                        return Type::Option(Box::new(Type::Name("unknown".into(), vec![])));
                    }
                    "None" => {
                        if args.len() != 0 {
                            self.emit_code(crate::diagnostic::codes::E0242, "None expects 0 arguments");
                        }
                        return Type::Option(Box::new(Type::Name("unknown".into(), vec![])));
                    }
                    "Ok" => {
                        if args.len() != 1 {
                            self.emit_code(crate::diagnostic::codes::E0242, "Ok expects 1 argument");
                        } else {
                            let inner = self.infer_expr(&args[0], scopes);
                            return Type::Result(Box::new(inner), Box::new(Type::Name("unknown".into(), vec![])));
                        }
                        return Type::Result(
                            Box::new(Type::Name("unknown".into(), vec![])),
                            Box::new(Type::Name("unknown".into(), vec![])),
                        );
                    }
                    "Err" => {
                        if args.len() != 1 {
                            self.emit_code(crate::diagnostic::codes::E0242, "Err expects 1 argument");
                        } else {
                            let inner = self.infer_expr(&args[0], scopes);
                            return Type::Result(Box::new(Type::Name("unknown".into(), vec![])), Box::new(inner));
                        }
                        return Type::Result(
                            Box::new(Type::Name("unknown".into(), vec![])),
                            Box::new(Type::Name("unknown".into(), vec![])),
                        );
                    }
                    _ => {}
                }
                // Try module-qualified lookup via use imports
                for module in self.use_imports.clone() {
                    let qualified = format!("{}::{}", module, name);
                    if self.funcs.contains_key(&qualified) {
                        // Recursively check with qualified name
                        return self.check_call(&qualified, args, scopes);
                    }
                }
                // Collect all known function names for "did you mean?" suggestions
                let candidates: Vec<String> = self.funcs.keys().cloned().collect();
                let suggestion = super::suggest_name(name, &candidates, 3);
                if let Some(suggested) = suggestion {
                    self.errors.push(
                        Diagnostic::error_code(
                            crate::diagnostic::codes::E0401,
                            format!("undefined function '{}'", name),
                            Span::single(self.current_line, self.current_col),
                        ).with_help(format!("did you mean '{}'?", suggested))
                    );
                } else {
                    self.emit_code(crate::diagnostic::codes::E0401, format!("undefined function '{}'", name));
                }
                return Type::Name("unknown".into(), vec![]);
            }
        };

        if args.len() != params.len() {
            self.emit_code(crate::diagnostic::codes::E0257, format!(
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
                                    self.emit_code(crate::diagnostic::codes::E0253, format!(
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
                        self.emit_code(crate::diagnostic::codes::E0211, format!(
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
                        self.emit_code(crate::diagnostic::codes::E0211, format!(
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
                                    self.emit_code(crate::diagnostic::codes::E0253, format!(
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
                        self.emit_code(crate::diagnostic::codes::E0254, format!(
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
            Pattern::Variable(name) => {
                // Variable pattern: if the name matches an enum variant of the
                // subject type, treat it as a constructor reference rather than
                // a catch-all binding.  This makes `match c { Red => … }` on
                // an enum type `Color { Red, Green, Blue }` count as covering
                // only the `Red` variant instead of all variants.
                let all = self.get_enum_variants(subject_ty);
                if all.contains(name) {
                    (vec![name.clone()], false)
                } else {
                    (all, true)
                }
            }
            Pattern::Literal(lit) => {
                // For bool literals, cover the specific variant (true/false)
                let covered = match lit {
                    Lit::Bool(true) => vec!["true".into()],
                    Lit::Bool(false) => vec!["false".into()],
                    _ => Vec::new(),
                };
                (covered, false)
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

    fn infer_block_expr(&mut self, block: &Block, scopes: &mut Vec<HashMap<String, Type>>) -> Type {
        scopes.push(HashMap::new());
        let mut result_type = Type::Name("unit".into(), vec![]);
        for stmt in block {
            match stmt {
                Stmt::Expr(e) => { result_type = self.infer_expr(e, scopes); }
                Stmt::Return(Some(e)) => { result_type = self.infer_expr(e, scopes); break; }
                Stmt::Let { init: Some(e), .. } => { result_type = self.infer_expr(e, scopes); }
                _ => {}
            }
        }
        scopes.pop();
        result_type
    }

    /// Type-check a method call on Option<T>
    fn check_option_method(&mut self, method: &str, inner: &Type, args: &[Expr], scopes: &mut Vec<HashMap<String, Type>>) -> Type {
        match method {
            "unwrap" | "expect" => {
                if method == "expect" && !args.is_empty() {
                    self.infer_expr(&args[0], scopes);
                }
                (*inner).clone()
            }
            "unwrap_or" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "unwrap_or expects 1 argument");
                } else {
                    let default = self.infer_expr(&args[0], scopes);
                    if !same_type(&default, inner) {
                        self.emit_code(crate::diagnostic::codes::E0242, format!("unwrap_or expected {}, found {}", fmt_type(inner), fmt_type(&default)));
                    }
                }
                (*inner).clone()
            }
            "is_some" | "is_none" => Type::Name("bool".into(), vec![]),
            "ok_or" => Type::Result(Box::new((*inner).clone()), Box::new(Type::Name("unknown".into(), vec![]))),
            "map" => Type::Option(Box::new(Type::Name("unknown".into(), vec![]))),
            "and_then" => Type::Name("unknown".into(), vec![]),
            "map_err" => Type::Option(Box::new((*inner).clone())),
            _ => {
                // Unknown methods are handled by the caller via trait dispatch; this is a fallback
                self.emit_code(crate::diagnostic::codes::E0242, format!("Option<{}> has no method '{}'", fmt_type(inner), method));
                Type::Name("unknown".into(), vec![])
            }
        }
    }

    /// Type-check a method call on Result<T, E>
    fn check_result_method(&mut self, method: &str, ok_ty: &Type, err_ty: &Type, args: &[Expr], scopes: &mut Vec<HashMap<String, Type>>) -> Type {
        match method {
            "unwrap" | "expect" => {
                if method == "expect" && !args.is_empty() {
                    self.infer_expr(&args[0], scopes);
                }
                (*ok_ty).clone()
            }
            "unwrap_or" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "unwrap_or expects 1 argument");
                } else {
                    let default = self.infer_expr(&args[0], scopes);
                    if !same_type(&default, ok_ty) {
                        self.emit_code(crate::diagnostic::codes::E0242, format!("unwrap_or expected {}, found {}", fmt_type(ok_ty), fmt_type(&default)));
                    }
                }
                (*ok_ty).clone()
            }
            "is_ok" | "is_err" => Type::Name("bool".into(), vec![]),
            "map" => Type::Result(Box::new(Type::Name("unknown".into(), vec![])), Box::new((*err_ty).clone())),
            "and_then" => Type::Name("unknown".into(), vec![]),
            "map_err" => Type::Result(Box::new((*ok_ty).clone()), Box::new(Type::Name("unknown".into(), vec![]))),
            _ => {
                // Unknown methods are handled by the caller via trait dispatch; this is a fallback
                self.emit_code(crate::diagnostic::codes::E0242, format!("Result<{}, {}> has no method '{}'", fmt_type(ok_ty), fmt_type(err_ty), method));
                Type::Name("unknown".into(), vec![])
            }
        }
    }

    /// Type-check a method call on string
    fn check_string_method(&mut self, method: &str, args: &[Expr], scopes: &mut Vec<HashMap<String, Type>>) -> Type {
        match method {
            "len" | "trim" | "to_upper" | "to_lower" => {
                if !args.is_empty() {
                    self.emit_code(crate::diagnostic::codes::E0242, format!("{} takes no arguments", method));
                }
                match method {
                    "len" => Type::Name("i32".into(), vec![]),
                    _ => Type::Name("string".into(), vec![]),
                }
            }
            "parse_int" => {
                if args.len() != 0 { self.emit_code(crate::diagnostic::codes::E0242, "parse_int takes no arguments"); }
                Type::Result(
                    Box::new(Type::Name("i32".into(), vec![])),
                    Box::new(Type::Name("string".into(), vec![])),
                )
            }
            "parse_float" => {
                if args.len() != 0 { self.emit_code(crate::diagnostic::codes::E0242, "parse_float takes no arguments"); }
                Type::Result(
                    Box::new(Type::Name("f64".into(), vec![])),
                    Box::new(Type::Name("string".into(), vec![])),
                )
            }
            "contains" | "starts_with" | "ends_with" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, format!("{} expects 1 argument", method));
                } else {
                    let t = self.infer_expr(&args[0], scopes);
                    if !same_type(&t, &Type::Name("string".into(), vec![])) {
                        self.emit_code(crate::diagnostic::codes::E0242, format!("{} expects a string argument", method));
                    }
                }
                Type::Name("bool".into(), vec![])
            }
            "split" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "split expects 1 argument");
                } else {
                    let t = self.infer_expr(&args[0], scopes);
                    if !same_type(&t, &Type::Name("string".into(), vec![])) {
                        self.emit_code(crate::diagnostic::codes::E0242, "split expects a string argument");
                    }
                }
                Type::Name("List".into(), vec![Type::Name("string".into(), vec![])])
            }
            "replace" => {
                if args.len() != 2 {
                    self.emit_code(crate::diagnostic::codes::E0242, "replace expects 2 arguments");
                } else {
                    for a in args {
                        let t = self.infer_expr(a, scopes);
                        if !same_type(&t, &Type::Name("string".into(), vec![])) {
                            self.emit_code(crate::diagnostic::codes::E0242, "replace expects string arguments");
                        }
                    }
                }
                Type::Name("string".into(), vec![])
            }
            "repeat" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "repeat expects 1 argument");
                } else {
                    let t = self.infer_expr(&args[0], scopes);
                    if !is_int(&t) {
                        self.emit_code(crate::diagnostic::codes::E0242, "repeat expects an integer argument");
                    }
                }
                Type::Name("string".into(), vec![])
            }
            "char_at" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "char_at expects 1 argument");
                } else {
                    let t = self.infer_expr(&args[0], scopes);
                    if !is_int(&t) {
                        self.emit_code(crate::diagnostic::codes::E0242, "char_at expects an integer argument");
                    }
                }
                Type::Name("string".into(), vec![])
            }
            "substring" => {
                if args.len() != 2 {
                    self.emit_code(crate::diagnostic::codes::E0242, "substring expects 2 arguments");
                } else {
                    for a in args {
                        let t = self.infer_expr(a, scopes);
                        if !is_int(&t) {
                            self.emit_code(crate::diagnostic::codes::E0242, "substring expects integer arguments");
                        }
                    }
                }
                Type::Name("string".into(), vec![])
            }
            "index_of" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "index_of expects 1 argument");
                } else {
                    let t = self.infer_expr(&args[0], scopes);
                    if !same_type(&t, &Type::Name("string".into(), vec![])) {
                        self.emit_code(crate::diagnostic::codes::E0242, "index_of expects a string argument");
                    }
                }
                Type::Name("i32".into(), vec![])
            }
            _ => {
                self.emit_code(crate::diagnostic::codes::E0242, format!("string has no method '{}'", method));
                Type::Name("unknown".into(), vec![])
            }
        }
    }

    /// Type-check a method call on List<T>
    fn check_list_method(&mut self, method: &str, args: &[Expr], scopes: &mut Vec<HashMap<String, Type>>) -> Type {
        match method {
            "len" => Type::Name("i32".into(), vec![]),
            _ => {
                self.emit_code(crate::diagnostic::codes::E0242, format!("List has no method '{}'", method));
                Type::Name("unknown".into(), vec![])
            }
        }
    }
}
