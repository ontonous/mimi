use super::*;
use crate::diagnostic::Diagnostic;

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
            Stmt::WhileLet { init, body, .. } => {
                self.check_expr_parasteps_safe(init, scopes);
                for s in body {
                    self.check_stmt_parasteps_safe(s, scopes);
                }
            }
            Stmt::Loop(body) => {
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
            Stmt::Requires(expr, _) | Stmt::Ensures(expr, _) | Stmt::Invariant(expr, _) => {
                self.check_expr_parasteps_safe(expr, scopes);
            }
            Stmt::Math(exprs) => {
                for e in exprs {
                    self.check_expr_parasteps_safe(e, scopes);
                }
            }
            Stmt::Block(block) | Stmt::Arena(block) | Stmt::Unsafe(block)
            | Stmt::Parasteps(block) | Stmt::OnFailure(block) => {
                for s in block {
                    self.check_stmt_parasteps_safe(s, scopes);
                }
            }
            Stmt::Alloc { body, .. } => {
                for s in body {
                    self.check_stmt_parasteps_safe(s, scopes);
                }
            }
            Stmt::Drop(e) => {
                self.check_expr_parasteps_safe(e, scopes);
            }
            Stmt::SharedLet { init, .. } => {
                self.check_expr_parasteps_safe(init, scopes);
            }
            Stmt::Break(Some(e)) => {
                self.check_expr_parasteps_safe(e, scopes);
            }
            Stmt::Return(None) | Stmt::Continue | Stmt::Break(None)
            | Stmt::Let { init: None, .. }
            | Stmt::Desc(..) | Stmt::Rule(..) | Stmt::MmsBlock { .. }
            | Stmt::Ellipsis => {}
        }
    }

    /// Collect names of shared variables written to in a parasteps statement.
    /// Recurses into sub-blocks (if, while, for, block).
    fn collect_shared_writes_in_stmt(
        &self,
        stmt: &Stmt,
        scopes: &[HashMap<String, Type>],
        writes: &mut Vec<String>,
    ) {
        match stmt {
            Stmt::Assign { target, .. } => {
                self.collect_shared_writes_in_expr_target(target, scopes, writes);
            }
            Stmt::Expr(Expr::Call(callee, args)) => {
                if let Expr::Ident(name) = callee.as_ref() {
                    if name == "push" && !args.is_empty() {
                        if let Expr::Ident(list_name) = &args[0] {
                            self.collect_shared_write(list_name, scopes, writes);
                        }
                    }
                }
            }
            Stmt::If { then_, else_, .. } => {
                for s in then_ { self.collect_shared_writes_in_stmt(s, scopes, writes); }
                if let Some(else_) = else_ {
                    for s in else_ { self.collect_shared_writes_in_stmt(s, scopes, writes); }
                }
            }
            Stmt::While { body, .. } => {
                for s in body { self.collect_shared_writes_in_stmt(s, scopes, writes); }
            }
            Stmt::WhileLet { body, .. } => {
                for s in body { self.collect_shared_writes_in_stmt(s, scopes, writes); }
            }
            Stmt::Loop(body) => {
                for s in body { self.collect_shared_writes_in_stmt(s, scopes, writes); }
            }
            Stmt::For { body, .. } => {
                for s in body { self.collect_shared_writes_in_stmt(s, scopes, writes); }
            }
            Stmt::Block(block) | Stmt::Unsafe(block) | Stmt::Alloc { body: block, .. }
            | Stmt::Arena(block) | Stmt::Parasteps(block) | Stmt::OnFailure(block) => {
                for s in block { self.collect_shared_writes_in_stmt(s, scopes, writes); }
            }
            Stmt::Let { init: Some(e), .. } => {
                self.collect_shared_writes_in_expr(e, scopes, writes);
            }
            Stmt::Return(Some(e)) | Stmt::Break(Some(e)) => {
                self.collect_shared_writes_in_expr(e, scopes, writes);
            }
            Stmt::Drop(e) => {
                self.collect_shared_writes_in_expr(e, scopes, writes);
            }
            Stmt::SharedLet { init, .. } => {
                self.collect_shared_writes_in_expr(init, scopes, writes);
            }
            Stmt::Requires(expr, _) | Stmt::Ensures(expr, _) | Stmt::Invariant(expr, _) => {
                self.collect_shared_writes_in_expr(expr, scopes, writes);
            }
            Stmt::Math(exprs) => {
                for e in exprs { self.collect_shared_writes_in_expr(e, scopes, writes); }
            }
            Stmt::Continue | Stmt::Break(None) | Stmt::Return(None)
            | Stmt::Let { init: None, .. }
            | Stmt::Expr(..)
            | Stmt::Desc(..) | Stmt::Rule(..) | Stmt::MmsBlock { .. }
            | Stmt::Ellipsis => {}
        }
    }

    /// Collect shared writes inside an expression (recursive helper).
    fn collect_shared_writes_in_expr(
        &self,
        expr: &Expr,
        scopes: &[HashMap<String, Type>],
        writes: &mut Vec<String>,
    ) {
        match expr {
            Expr::Ident(name) => {
                self.collect_shared_write(name, scopes, writes);
            }
            Expr::Binary(_, l, r) | Expr::Range { start: l, end: r } => {
                self.collect_shared_writes_in_expr(l, scopes, writes);
                self.collect_shared_writes_in_expr(r, scopes, writes);
            }
            Expr::Unary(_, e) | Expr::Spawn(e) | Expr::Await(e) | Expr::Try(e)
            | Expr::Old(e) | Expr::TypeOf(e) => {
                self.collect_shared_writes_in_expr(e, scopes, writes);
            }
            Expr::Call(callee, args) => {
                self.collect_shared_writes_in_expr(callee, scopes, writes);
                for a in args { self.collect_shared_writes_in_expr(a, scopes, writes); }
            }
            Expr::Field(obj, _) | Expr::TupleIndex(obj, _) => {
                self.collect_shared_writes_in_expr(obj, scopes, writes);
            }
            Expr::Index(obj, idx) => {
                self.collect_shared_writes_in_expr(obj, scopes, writes);
                self.collect_shared_writes_in_expr(idx, scopes, writes);
            }
            Expr::SliceExpr { target, .. } => {
                self.collect_shared_writes_in_expr(target, scopes, writes);
            }
            Expr::List(elems) | Expr::Tuple(elems) | Expr::SetLiteral(elems) => {
                for e in elems { self.collect_shared_writes_in_expr(e, scopes, writes); }
            }
            Expr::Comprehension { expr, iter, guard, .. } => {
                self.collect_shared_writes_in_expr(expr, scopes, writes);
                self.collect_shared_writes_in_expr(iter, scopes, writes);
                if let Some(g) = guard { self.collect_shared_writes_in_expr(g, scopes, writes); }
            }
            Expr::Match(matched, arms) => {
                self.collect_shared_writes_in_expr(matched, scopes, writes);
                for arm in arms { self.collect_shared_writes_in_expr(&arm.body, scopes, writes); }
            }
            Expr::Record { fields, .. } => {
                for f in fields { self.collect_shared_writes_in_expr(&f.value, scopes, writes); }
            }
            Expr::MapLiteral { entries } => {
                for (k, v) in entries {
                    self.collect_shared_writes_in_expr(k, scopes, writes);
                    self.collect_shared_writes_in_expr(v, scopes, writes);
                }
            }
            Expr::NamedArg(_, e) => {
                self.collect_shared_writes_in_expr(e, scopes, writes);
            }
            Expr::Turbofish(_, _, args) => {
                for a in args { self.collect_shared_writes_in_expr(a, scopes, writes); }
            }
            Expr::Block(block) | Expr::Arena(block) | Expr::Comptime(block)
            | Expr::Quote(block) => {
                for s in block { self.collect_shared_writes_in_stmt(s, scopes, writes); }
            }
            Expr::If { cond, then_, else_ } => {
                self.collect_shared_writes_in_expr(cond, scopes, writes);
                for s in then_ { self.collect_shared_writes_in_stmt(s, scopes, writes); }
                if let Some(eb) = else_ { for s in eb { self.collect_shared_writes_in_stmt(s, scopes, writes); } }
            }
            Expr::Lambda { body, .. } => {
                for s in body { self.collect_shared_writes_in_stmt(s, scopes, writes); }
            }
            Expr::QuoteInterpolate(inner) => {
                self.collect_shared_writes_in_expr(inner, scopes, writes);
            }
            Expr::Literal(_) | Expr::TypeInfo(_) => {}
        }
    }

    /// Extract shared variable writes from an assignment target expression.
    fn collect_shared_writes_in_expr_target(
        &self,
        expr: &Expr,
        scopes: &[HashMap<String, Type>],
        writes: &mut Vec<String>,
    ) {
        match expr {
            Expr::Ident(name) => {
                self.collect_shared_write(name, scopes, writes);
            }
            Expr::Field(obj, _) => {
                self.collect_shared_writes_in_expr_target(obj, scopes, writes);
            }
            Expr::Index(obj, _) => {
                self.collect_shared_writes_in_expr_target(obj, scopes, writes);
            }
            Expr::Unary(UnOp::Deref, inner) => {
                self.collect_shared_writes_in_expr_target(inner, scopes, writes);
            }
            _ => {}
        }
    }

    /// If `name` refers to a shared variable in the given scopes, add it to writes.
    fn collect_shared_write(
        &self,
        name: &str,
        scopes: &[HashMap<String, Type>],
        writes: &mut Vec<String>,
    ) {
        for scope in scopes.iter().rev() {
            if let Some(ty) = scope.get(name) {
                if matches!(ty, Type::Shared(_)) {
                    writes.push(name.to_string());
                }
                break;
            }
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
            Expr::TupleIndex(obj, _) => {
                self.check_expr_parasteps_safe(obj, scopes);
            }
            Expr::If { cond, then_, else_ } => {
                self.check_expr_parasteps_safe(cond, scopes);
                for s in then_ { self.check_stmt_parasteps_safe(s, scopes); }
                if let Some(eb) = else_ {
                    for s in eb { self.check_stmt_parasteps_safe(s, scopes); }
                }
            }
            Expr::Block(block) => {
                for s in block { self.check_stmt_parasteps_safe(s, scopes); }
            }
            Expr::Lambda { body, .. } => {
                for s in body { self.check_stmt_parasteps_safe(s, scopes); }
            }
            Expr::Spawn(inner) | Expr::Await(inner) => {
                self.check_expr_parasteps_safe(inner, scopes);
            }
            Expr::Comprehension { expr, iter, guard, .. } => {
                self.check_expr_parasteps_safe(expr, scopes);
                self.check_expr_parasteps_safe(iter, scopes);
                if let Some(g) = guard { self.check_expr_parasteps_safe(g, scopes); }
            }
            Expr::Match(matched, arms) => {
                self.check_expr_parasteps_safe(matched, scopes);
                for arm in arms { self.check_expr_parasteps_safe(&arm.body, scopes); }
            }
            Expr::Record { fields, .. } => {
                for f in fields { self.check_expr_parasteps_safe(&f.value, scopes); }
            }
            Expr::Try(e) | Expr::Old(e) | Expr::TypeOf(e) => {
                self.check_expr_parasteps_safe(e, scopes);
            }
            Expr::SliceExpr { target, start, end } => {
                self.check_expr_parasteps_safe(target, scopes);
                if let Some(s) = start { self.check_expr_parasteps_safe(s, scopes); }
                if let Some(e) = end { self.check_expr_parasteps_safe(e, scopes); }
            }
            Expr::Range { start, end } => {
                self.check_expr_parasteps_safe(start, scopes);
                self.check_expr_parasteps_safe(end, scopes);
            }
            Expr::Arena(block) => {
                for s in block { self.check_stmt_parasteps_safe(s, scopes); }
            }
            Expr::MapLiteral { entries } => {
                for (k, v) in entries {
                    self.check_expr_parasteps_safe(k, scopes);
                    self.check_expr_parasteps_safe(v, scopes);
                }
            }
            Expr::SetLiteral(elems) => {
                for e in elems { self.check_expr_parasteps_safe(e, scopes); }
            }
            Expr::NamedArg(_, e) => {
                self.check_expr_parasteps_safe(e, scopes);
            }
            Expr::Turbofish(_, _, args) => {
                for a in args { self.check_expr_parasteps_safe(a, scopes); }
            }
            Expr::Quote(block) | Expr::Comptime(block) => {
                for s in block { self.check_stmt_parasteps_safe(s, scopes); }
            }
            Expr::QuoteInterpolate(inner) => {
                self.check_expr_parasteps_safe(inner, scopes);
            }
            Expr::Literal(_) | Expr::TypeInfo(_) => {}
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
                            self.errors.push(
                                Diagnostic::error_code(
                                    crate::diagnostic::codes::E0403,
                                    format!("variable '{}' shadows an outer variable", name),
                                    Span::single(self.current_line, self.current_col),
                                ).with_help("rename the inner variable to avoid shadowing")
                            );
                            break;
                        }
                    }
                    if let Some(s) = self.var_scopes.last_mut() {
                        s.insert(name.clone(), 0);
                    }
                }

                let init_ty = init
                    .as_ref()
                    .map(|e| self.infer_expr(e, scopes))
                    .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                let declared = ty.as_ref().map(|t| self.resolve_type(t));
                let final_ty = match declared {
                    Some(d) => {
                        if matches!(&d, Type::Infer) {
                            // _ type: infer from init expression
                            init_ty.clone()
                        } else {
                            if !same_type(&d, &init_ty) && !is_numeric_coercion(&d, &init_ty) && !is_trait_coercion(&d, &init_ty, &self.impls) {
                                self.errors.push(
                                    Diagnostic::error_code(
                                        crate::diagnostic::codes::E0209,
                                        format!("pattern declared as {} but initialized with {}", fmt_type(&d), fmt_type(&init_ty)),
                                        Span::single(self.current_line, self.current_col),
                                    ).with_help(format!("the expression has type '{}', not '{}'", fmt_type(&init_ty), fmt_type(&d)))
                                );
                            }
                            d
                        }
                    }
                    None => {
                        if *ref_ {
                            // ref variables have reference type
                            Type::Ref(None, Box::new(init_ty))
                        } else {
                            init_ty
                        }
                    }
                };
                // Track mutability
                if let Pattern::Variable(name) = pat {
                    if let Some(s) = self.mut_vars.last_mut() {
                        s.insert(name.clone(), *mut_);
                    }
                }
                self.check_pattern(pat, &final_ty, scopes);
                // Track cap variables for linear type checking and introduce effects
                if let Type::Cap(cap_name) = &final_ty {
                    if let Pattern::Variable(name) = pat {
                        if let Some(s) = self.cap_vars.last_mut() {
                            s.insert(name.clone(), false);
                        }
                        // Introduce the cap as an effect
                        if let Some(s) = self.available_effects.last_mut() {
                            s.insert(cap_name.clone(), true);
                        }
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
                    self.errors.push(
                        Diagnostic::error_code(
                            crate::diagnostic::codes::E0207,
                            format!("return type mismatch: expected {}, found {}", fmt_type(ret), fmt_type(&t)),
                            Span::single(self.current_line, self.current_col),
                        ).with_help("check the function's declared return type and the type of the returned expression")
                    );
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
            Stmt::WhileLet { pat, init, body } => {
                let it = self.infer_expr(init, scopes);
                scopes.push(HashMap::new());
                self.check_pattern(pat, &it, scopes);
                self.loop_depth += 1;
                self.check_block(body, ret, scopes);
                self.loop_depth -= 1;
                scopes.pop();
            }
            Stmt::Loop(body) => {
                self.loop_depth += 1;
                self.check_block(body, ret, scopes);
                self.loop_depth -= 1;
            }
            Stmt::For { var, iterable, body } => {
                let it = self.infer_expr(iterable, scopes);
                let elem_ty = match &it {
                    Type::Name(n, args) if n == "List" && args.len() == 1 => args[0].clone(),
                    Type::Name(n, _) if n == "Range" => Type::Name("i32".into(), vec![]),
                    Type::Name(n, _) if n == "string" => Type::Name("string".into(), vec![]),
                    Type::Name(n, args) if n == "Set" && args.len() == 1 => args[0].clone(),
                    Type::Name(n, _) if n == "Map" || n == "Record" => {
                        Type::Tuple(vec![Type::Name("string".into(), vec![]), Type::Name("Any".into(), vec![])])
                    }
                    _ => {
                        self.emit_code(crate::diagnostic::codes::E0212, format!(
                            "for loop requires a List, Range, string, Set, or Map, found {}",
                            fmt_type(&it)
                        ));
                        Type::Name("unknown".into(), vec![])
                    }
                };
                scopes.push(HashMap::new());
                if let Some(s) = scopes.last_mut() {
                    s.insert(var.clone(), elem_ty);
                }
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
                // Arena block: track depth for escape detection
                self.arena_depth += 1;
                scopes.push(HashMap::new());
                self.check_block(block, ret, scopes);
                scopes.pop();
                self.arena_depth -= 1;
            }
            Stmt::Unsafe(block) => {
                // Unsafe block: check the body (no additional restrictions at type-check level)
                scopes.push(HashMap::new());
                self.check_block(block, ret, scopes);
                scopes.pop();
            }
            Stmt::Alloc { kind: AllocKind::Arena, body } => {
                self.arena_depth += 1;
                scopes.push(HashMap::new());
                self.check_block(body, ret, scopes);
                scopes.pop();
                self.arena_depth -= 1;
            }
            Stmt::Alloc { kind: _, body } => {
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
                            Type::LocalShared(inner) => Type::WeakLocal(inner.clone()),
                            _ => {
                            self.emit_code(crate::diagnostic::codes::E0411, format!(
                                "weak_local requires a local_shared value, found {}",
                                fmt_type(&init_ty)
                            ));
                                Type::WeakLocal(Box::new(Type::Name("unknown".into(), vec![])))
                            }
                        }
                    }
                };
                if let Some(declared) = ty {
                    let declared = self.resolve_type(declared);
                    if !same_type(&declared, &final_ty) {
                        self.emit_code(crate::diagnostic::codes::E0258, format!(
                            "shared binding declared as {} but inferred as {}",
                            fmt_type(&declared),
                            fmt_type(&final_ty)
                        ));
                    }
                }
                if let Some(s) = scopes.last_mut() {
                    s.insert(name.clone(), final_ty);
                }
            }
            Stmt::Parasteps(block) => {
                // Parasteps block executes statements in parallel
                // Check that no local_shared variables are captured from outer scope
                for stmt in block {
                    self.check_stmt_parasteps_safe(stmt, scopes);
                }
                // W005: Detect shared variable written by multiple parallel steps
                let step_writes: Vec<Vec<String>> = block.iter().map(|stmt| {
                    let mut writes = Vec::new();
                    self.collect_shared_writes_in_stmt(stmt, scopes, &mut writes);
                    writes
                }).collect();
                for i in 0..step_writes.len() {
                    for j in (i + 1)..step_writes.len() {
                        for var in &step_writes[i] {
                            if step_writes[j].contains(var) {
                                self.emit_warning_code(
                                    crate::diagnostic::codes::W005,
                                    format!("shared variable '{}' is written by multiple parallel steps in parasteps — this may cause data races", var),
                                );
                            }
                        }
                    }
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
                            self.errors.push(
                                Diagnostic::error_code(
                                    crate::diagnostic::codes::E0208,
                                    format!("cannot assign to immutable variable '{}' (use 'let mut')", name),
                                    Span::single(self.current_line, self.current_col),
                                ).with_help("use 'let mut' to make the variable mutable")
                            );
                        }
                        let target_ty = self.lookup_var(name, scopes);
                        if !same_type(&target_ty, &value_ty) {
                            self.errors.push(
                                Diagnostic::error_code(
                                    crate::diagnostic::codes::E0209,
                                    format!("cannot assign {} to variable '{}' of type {}", fmt_type(&value_ty), name, fmt_type(&target_ty)),
                                    Span::single(self.current_line, self.current_col),
                                ).with_help(format!("variable '{}' has type '{}', not '{}'", name, fmt_type(&target_ty), fmt_type(&value_ty)))
                            );
                        }
                        // E0306: Arena escape — assigning arena-scoped ref to outer-scope variable
                        if self.arena_depth > 0 {
                            if let Expr::Ident(value_name) = value {
                                let value_in_arena_scope = scopes.last()
                                    .and_then(|s| s.get(value_name))
                                    .map(|ty| matches!(ty, Type::Ref(_, _) | Type::RefMut(_, _)))
                                    .unwrap_or(false);
                                if value_in_arena_scope {
                                    let target_in_outer = scopes[..scopes.len().saturating_sub(1)].iter().rev()
                                        .any(|s| s.contains_key(name));
                                    if target_in_outer {
                                        self.emit_code(crate::diagnostic::codes::E0306, format!(
                                            "arena escape: variable '{}' from outer scope cannot hold a reference to arena memory (assigned from '{}')",
                                            name, value_name
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    Expr::Unary(UnOp::Deref, inner) => {
                        // *r = value: check that inner is &mut T
                        let inner_ty = self.infer_expr(inner, scopes);
                        match &inner_ty {
                            Type::RefMut(_, inner_inner) => {
                                if !same_type(&value_ty, inner_inner) {
                                    self.emit_code(crate::diagnostic::codes::E0233, format!(
                                        "cannot assign {} through &mut reference of type {}",
                                        fmt_type(&value_ty),
                                        fmt_type(&inner_ty)
                                    ));
                                }
                            }
                            _ => {
                                self.emit_code(crate::diagnostic::codes::E0233, format!(
                                    "cannot assign through non-mutable reference {}",
                                    fmt_type(&inner_ty)
                                ));
                            }
                        }
                    }
                    Expr::Field(obj, field) => {
                        let obj_ty = self.infer_expr(obj, scopes);
                        // Validate field exists on the object type
                        if let Type::Name(name, _) = &obj_ty {
                            if let Some(type_def) = self.types.get(name) {
                                match &type_def.kind {
                                    TypeDefKind::Record(fields) => {
                                        if !fields.iter().any(|f| f.name == *field) {
                                            let available: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
                                            if available.is_empty() {
                                                self.emit_code(crate::diagnostic::codes::E0220, format!("field '{}' not found in record '{}' (record has no fields)", field, name));
                                            } else {
                                                self.emit_code(crate::diagnostic::codes::E0220, format!("field '{}' not found in record '{}' — available fields: {}", field, name, available.join(", ")));
                                            }
                                        }
                                    }
                                    TypeDefKind::Enum(variants)
                                        if !variants.iter().any(|v| v.name == *field) =>
                                    {
                                        let available: Vec<&str> = variants.iter().map(|v| v.name.as_str()).collect();
                                        self.emit_code(crate::diagnostic::codes::E0226, format!("variant '{}' not found in enum '{}' — available: {}", field, name, available.join(", ")));
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    Expr::Index(obj, idx) => {
                        // xs[i] = val: check that xs is a mutable list and val matches element type
                        let obj_ty = self.infer_expr(obj, scopes);
                        self.infer_expr(idx, scopes);
                        match &obj_ty {
                            Type::Name(n, args) if n == "List" && args.len() == 1 => {
                                let elem_ty = &args[0];
                                if !same_type(&value_ty, elem_ty) {
                                    self.errors.push(
                                        Diagnostic::error_code(
                                            crate::diagnostic::codes::E0209,
                                            format!("cannot assign {} to list element of type {}", fmt_type(&value_ty), fmt_type(elem_ty)),
                                            Span::single(self.current_line, self.current_col),
                                        ).with_help(format!("the list contains elements of type '{}', not '{}'", fmt_type(elem_ty), fmt_type(&value_ty)))
                                    );
                                }
                            }
                            _ => {
                                self.emit_code(crate::diagnostic::codes::E0218, format!(
                                    "cannot index-assign to {}",
                                    fmt_type(&obj_ty)
                                ));
                            }
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
                    if let Some(cap_scope) = self.cap_vars.last_mut() {
                        if let Some(consumed) = cap_scope.get_mut(name) {
                            if *consumed {
                                self.errors.push(
                                    Diagnostic::error_code(
                                        crate::diagnostic::codes::E0304,
                                        format!(
                                            "capability '{}' has already been consumed",
                                            name
                                        ),
                                        Span::single(self.current_line, self.current_col),
                                    ).with_help("capabilities are linear - each can only be dropped once")
                                );
                            } else {
                                *consumed = true;
                            }
                        }
                    }
                }
            }
            Stmt::Requires(expr, _) => {
                let ty = self.infer_expr(expr, scopes);
                if !matches!(&ty, Type::Name(n, _) if n == "bool") {
                    self.emit_code(crate::diagnostic::codes::E0212, format!(
                        "requires condition must be bool, found {}",
                        fmt_type(&ty)
                    ));
                }
            }
            Stmt::Invariant(expr, _) => {
                let ty = self.infer_expr(expr, scopes);
                if !matches!(&ty, Type::Name(n, _) if n == "bool") {
                    self.emit_code(crate::diagnostic::codes::E0212, format!(
                        "invariant condition must be bool, found {}",
                        fmt_type(&ty)
                    ));
                }
            }
            Stmt::Ensures(expr, _) => {
                // Inject `result` (with the function's return type) and `old_*`
                // variable placeholders so that `ensures: result == ...` and
                // `ensures: old(x) > 0` type-check correctly.
                let mut ensure_scope = HashMap::new();
                ensure_scope.insert("result".to_string(), (*ret).clone());
                // old(x) is handled by Expr::Old which delegates to
                // infer_expr on the inner expression, so any variable in scope
                // naturally works.
                scopes.push(ensure_scope);
                let ty = self.infer_expr(expr, scopes);
                scopes.pop();
                if !matches!(&ty, Type::Name(n, _) if n == "bool") {
                    self.emit_code(crate::diagnostic::codes::E0212, format!(
                        "ensures condition must be bool, found {}",
                        fmt_type(&ty)
                    ));
                }
            }
            Stmt::Math(exprs) => {
                for expr in exprs {
                    self.infer_expr(expr, scopes);
                }
            }
            Stmt::Desc(..) | Stmt::Rule(..) | Stmt::Ellipsis | Stmt::MmsBlock { .. } => {}
            Stmt::OnFailure(body) => {
                self.check_block(body, ret, scopes);
            }
        }
    }
}
