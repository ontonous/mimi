use super::*;

impl<'a> Checker<'a> {
    pub(crate) fn check_block(&mut self, block: &Block, ret: &Type, scopes: &mut Vec<HashMap<String, Type>>) {
        // Push cap scope and borrow scope for block
        self.cap_vars.push(HashMap::new());
        self.push_borrow_scope();
        let mut seen_return = false;
        for (i, stmt) in block.iter().enumerate() {
            // Unreachable code detection
            if seen_return {
                self.emit_code(crate::diagnostic::codes::E0236, "unreachable statement after return".to_string());
                break;
            }
            if let Stmt::Return(_) = stmt { seen_return = true; }
            // NLL: Release borrows whose last use was in a previous statement
            if i > 0 {
                self.release_borrows_at_last_use(block, i);
            }
            self.check_stmt(stmt, ret, scopes);
        }
        // Check for unconsumed caps before popping
        self.check_unconsumed_caps();
        self.pop_borrow_scope();
        self.cap_vars.pop();
    }

    /// Check that a statement doesn't capture local_shared variables from outer scope
    fn check_stmt_parasteps_safe(&mut self, stmt: &Stmt, scopes: &mut Vec<HashMap<String, Type>>) {
        match stmt {
            Stmt::Expr(e) | Stmt::Return(Some(e)) => {
                self.check_expr_parasteps_safe(e, scopes);
            }
            Stmt::Let { init: Some(e), .. } => {
                self.check_expr_parasteps_safe(e, scopes);
            }
            Stmt::Assign { target, value } => {
                self.check_expr_parasteps_safe(target, scopes);
                self.check_expr_parasteps_safe(value, scopes);
            }
            Stmt::If { cond, then_, else_ } => {
                self.check_expr_parasteps_safe(cond, scopes);
                for s in then_ {
                    self.check_stmt_parasteps_safe(s, scopes);
                }
                if let Some(else_) = else_ {
                    for s in else_ {
                        self.check_stmt_parasteps_safe(s, scopes);
                    }
                }
            }
            Stmt::While { cond, body } => {
                self.check_expr_parasteps_safe(cond, scopes);
                for s in body {
                    self.check_stmt_parasteps_safe(s, scopes);
                }
            }
            Stmt::For { iterable, body, .. } => {
                self.check_expr_parasteps_safe(iterable, scopes);
                for s in body {
                    self.check_stmt_parasteps_safe(s, scopes);
                }
            }
            _ => {}
        }
    }

    /// Check that an expression doesn't reference local_shared variables
    fn check_expr_parasteps_safe(&mut self, expr: &Expr, scopes: &mut Vec<HashMap<String, Type>>) {
        match expr {
            Expr::Ident(name) => {
                // Check if this variable is local_shared from outer scope
                for scope in scopes.iter().rev() {
                    if let Some(ty) = scope.get(name) {
                        if matches!(ty, Type::LocalShared(_)) {
                            self.emit_code(crate::diagnostic::codes::E0305, format!("cannot capture 'local_shared' variable '{}' in parallel block (use 'shared' instead)", name));
                        }
                        break;
                    }
                }
            }
            Expr::Binary(_, l, r) => {
                self.check_expr_parasteps_safe(l, scopes);
                self.check_expr_parasteps_safe(r, scopes);
            }
            Expr::Unary(_, e) => {
                self.check_expr_parasteps_safe(e, scopes);
            }
            Expr::Call(callee, args) => {
                self.check_expr_parasteps_safe(callee, scopes);
                for arg in args {
                    self.check_expr_parasteps_safe(arg, scopes);
                }
            }
            Expr::Field(obj, _) => {
                self.check_expr_parasteps_safe(obj, scopes);
            }
            Expr::Index(obj, idx) => {
                self.check_expr_parasteps_safe(obj, scopes);
                self.check_expr_parasteps_safe(idx, scopes);
            }
            Expr::List(elems) => {
                for e in elems {
                    self.check_expr_parasteps_safe(e, scopes);
                }
            }
            Expr::Tuple(elems) => {
                for e in elems {
                    self.check_expr_parasteps_safe(e, scopes);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn check_stmt(
        &mut self,
        stmt: &Stmt,
        ret: &Type,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) {
        match stmt {
            Stmt::Let { pat, ty, init, mut_, ref_ } => {
                // Shadowing detection
                if let Pattern::Variable(name) = pat {
                    for scope in self.var_scopes.iter().rev() {
                        if scope.contains_key(name) {
                            self.emit_code(crate::diagnostic::codes::E0403, format!("variable '{}' shadows an outer variable", name));
                            break;
                        }
                    }
                    self.var_scopes.last_mut().expect("scope stack non-empty").insert(name.clone(), 0);
                }

                let init_ty = init
                    .as_ref()
                    .map(|e| self.infer_expr(e, scopes))
                    .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                let declared = ty.as_ref().map(|t| self.resolve_type(t));
                let final_ty = match declared {
                    Some(d) => {
                        if !same_type(&d, &init_ty) {
                            self.emit_code(crate::diagnostic::codes::E0209, format!(
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
                // Track mutability
                if let Pattern::Variable(name) = pat {
                    self.mut_vars.last_mut().expect("scope stack non-empty").insert(name.clone(), *mut_);
                }
                self.check_pattern(pat, &final_ty, scopes);
                // Track cap variables for linear type checking and introduce effects
                if let Type::Cap(cap_name) = &final_ty {
                    if let Pattern::Variable(name) = pat {
                        self.cap_vars.last_mut().expect("scope stack non-empty").insert(name.clone(), false);
                        // Introduce the cap as an effect
                        self.available_effects.last_mut().expect("scope stack non-empty").insert(cap_name.clone(), true);
                    }
                }
            }
            Stmt::Return(None) => {
                if !same_type(ret, &Type::Name("unit".into(), vec![])) {
                    self.emit_code(crate::diagnostic::codes::E0207, format!(
                        "expected return value of type {}, found unit",
                        fmt_type(ret)
                    ));
                }
            }
            Stmt::Return(Some(e)) => {
                let t = self.infer_expr(e, scopes);
                if !same_type(ret, &t) {
                    self.emit_code(crate::diagnostic::codes::E0207, format!(
                        "return type mismatch: expected {}, found {}",
                        fmt_type(ret),
                        fmt_type(&t)
                    ));
                }
            }
            Stmt::Break(_) => {
                if self.loop_depth == 0 {
                    self.emit_code(crate::diagnostic::codes::E0404, "break outside of loop".to_string());
                }
            }
            Stmt::Continue => {
                if self.loop_depth == 0 {
                    self.emit_code(crate::diagnostic::codes::E0405, "continue outside of loop".to_string());
                }
            }
            Stmt::Expr(e) => {
                self.infer_expr(e, scopes);
            }
            Stmt::If { cond, then_, else_ } => {
                let ct = self.infer_expr(cond, scopes);
                if !is_bool(&ct) {
                    self.emit_code(crate::diagnostic::codes::E0205, format!(
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
                    self.emit_code(crate::diagnostic::codes::E0206, format!(
                        "while condition must be bool, found {}",
                        fmt_type(&ct)
                    ));
                }
                self.loop_depth += 1;
                self.check_block(body, ret, scopes);
                self.loop_depth -= 1;
            }
            Stmt::For { var, iterable, body } => {
                let it = self.infer_expr(iterable, scopes);
                let elem_ty = match &it {
                    Type::Name(n, args) if n == "List" && args.len() == 1 => args[0].clone(),
                    Type::Name(n, _) if n == "Range" => Type::Name("i32".into(), vec![]),
                    _ => {
                        self.emit_code(crate::diagnostic::codes::E0212, format!(
                            "for loop requires a List or Range, found {}",
                            fmt_type(&it)
                        ));
                        Type::Name("unknown".into(), vec![])
                    }
                };
                scopes.push(HashMap::new());
                scopes.last_mut().expect("scope stack non-empty").insert(var.clone(), elem_ty);
                self.loop_depth += 1;
                self.check_block(body, ret, scopes);
                self.loop_depth -= 1;
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
            Stmt::Unsafe(block) => {
                // Unsafe block: check the body (no additional restrictions at type-check level)
                scopes.push(HashMap::new());
                self.check_block(block, ret, scopes);
                scopes.pop();
            }
            Stmt::Alloc { kind: _, body } => {
                // alloc(Kind) block: check the body with the specified allocator
                scopes.push(HashMap::new());
                self.check_block(body, ret, scopes);
                scopes.pop();
            }
            Stmt::SharedLet { kind, name, ty, init } => {
                let init_ty = self.infer_expr(init, scopes);
                let final_ty = match kind {
                    SharedKind::Shared => Type::Shared(Box::new(init_ty.clone())),
                    SharedKind::LocalShared => Type::LocalShared(Box::new(init_ty.clone())),
                    SharedKind::Weak => {
                        // Expect init to be a Shared value
                        match &init_ty {
                            Type::Shared(inner) => Type::Weak(inner.clone()),
                            _ => {
                            self.emit_code(crate::diagnostic::codes::E0411, format!(
                                "weak requires a shared value, found {}",
                                fmt_type(&init_ty)
                            ));
                                Type::Weak(Box::new(Type::Name("unknown".into(), vec![])))
                            }
                        }
                    }
                    SharedKind::WeakLocal => {
                        match &init_ty {
                            Type::LocalShared(inner) => Type::Weak(inner.clone()),
                            _ => {
                            self.emit_code(crate::diagnostic::codes::E0411, format!(
                                "weak_local requires a local_shared value, found {}",
                                fmt_type(&init_ty)
                            ));
                                Type::Weak(Box::new(Type::Name("unknown".into(), vec![])))
                            }
                        }
                    }
                };
                if let Some(declared) = ty {
                    let declared = self.resolve_type(declared);
                    if !same_type(&declared, &final_ty) {
                        self.emit(format!(
                            "shared binding declared as {} but inferred as {}",
                            fmt_type(&declared),
                            fmt_type(&final_ty)
                        ));
                    }
                }
                scopes.last_mut().expect("scope stack non-empty").insert(name.clone(), final_ty);
            }
            Stmt::Parasteps(block) => {
                // Parasteps block executes statements in parallel
                // Check that no local_shared variables are captured from outer scope
                for stmt in block {
                    self.check_stmt_parasteps_safe(stmt, scopes);
                }
                // Then type-check all statements
                scopes.push(HashMap::new());
                self.check_block(block, ret, scopes);
                scopes.pop();
            }
            Stmt::Assign { target, value } => {
                let value_ty = self.infer_expr(value, scopes);
                match target {
                    Expr::Ident(name) => {
                        // Check mutability
                        let is_mut = self.mut_vars.iter().rev().any(|scope| {
                            scope.get(name).copied().unwrap_or(false)
                        });
                        if !is_mut {
                            self.emit_code(crate::diagnostic::codes::E0208, format!("cannot assign to immutable variable '{}' (use 'let mut')", name));
                        }
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
                    Expr::Unary(UnOp::Deref, inner) => {
                        // *r = value: check that inner is &mut T
                        let inner_ty = self.infer_expr(inner, scopes);
                        match &inner_ty {
                            Type::RefMut(inner_inner) => {
                                if !same_type(&value_ty, inner_inner) {
                                    self.emit(format!(
                                        "cannot assign {} through &mut reference of type {}",
                                        fmt_type(&value_ty),
                                        fmt_type(&inner_ty)
                                    ));
                                }
                            }
                            _ => {
                                self.emit(format!(
                                    "cannot assign through non-mutable reference {}",
                                    fmt_type(&inner_ty)
                                ));
                            }
                        }
                    }
                    Expr::Field(obj, field) => {
                        let obj_ty = self.infer_expr(obj, scopes);
                        // Validate field exists on the object type
                        match &obj_ty {
                            Type::Name(name, _) => {
                                if let Some(type_def) = self.types.get(name) {
                                    match &type_def.kind {
                                        TypeDefKind::Record(fields) => {
                                            if !fields.iter().any(|f| f.name == *field) {
                                                let available: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
                                                if available.is_empty() {
                                                    self.emit(format!("field '{}' not found in record '{}' (record has no fields)", field, name));
                                                } else {
                                                    self.emit(format!("field '{}' not found in record '{}' — available fields: {}", field, name, available.join(", ")));
                                                }
                                            }
                                        }
                                        TypeDefKind::Enum(variants) => {
                                            if !variants.iter().any(|v| v.name == *field) {
                                                let available: Vec<&str> = variants.iter().map(|v| v.name.as_str()).collect();
                                                self.emit(format!("variant '{}' not found in enum '{}' — available: {}", field, name, available.join(", ")));
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => self.emit_code(crate::diagnostic::codes::E0219, "assignment target must be a variable"),
                }
            }
            Stmt::Drop(expr) => {
                // Evaluate the expression to ensure it's valid
                self.infer_expr(expr, scopes);
                // Mark the capability as consumed
                if let Expr::Ident(name) = expr {
                    if let Some(consumed) = self.cap_vars.last_mut().expect("scope stack non-empty").get_mut(name) {
                        if *consumed {
                            self.emit_code(crate::diagnostic::codes::E0304, format!(
                                "capability '{}' has already been consumed",
                                name
                            ));
                        } else {
                            *consumed = true;
                        }
                    }
                }
            }
            Stmt::Desc(_) | Stmt::Requires(_) | Stmt::Ensures(_) | Stmt::Math(_) | Stmt::Ellipsis | Stmt::OnFailure(_) | Stmt::MmsBlock { .. } => {}
        }
    }
}
