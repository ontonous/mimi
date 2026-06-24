use crate::ast::*;

pub(crate) fn collect_free_vars(block: &Block, bound: &std::collections::HashSet<String>) -> std::collections::HashSet<String> {
    let mut free = std::collections::HashSet::new();
    let mut local_bound = bound.clone();
    for stmt in block {
        let current_bound = local_bound.clone();
        collect_stmt_free_vars(stmt, &current_bound, &mut free, &mut local_bound);
    }
    free
}

pub(crate) fn collect_stmt_free_vars(
    stmt: &Stmt,
    bound: &std::collections::HashSet<String>,
    free: &mut std::collections::HashSet<String>,
    local_bound: &mut std::collections::HashSet<String>,
) {
    match stmt {
        Stmt::Let { pat, init, .. } => {
            if let Some(e) = init {
                collect_expr_free_vars(e, bound, free);
            }
            collect_pattern_names(pat, local_bound);
        }
        Stmt::SharedLet { init, name, .. } => {
            collect_expr_free_vars(init, bound, free);
            local_bound.insert(name.clone());
        }
        Stmt::Expr(e) | Stmt::Return(Some(e)) => {
            collect_expr_free_vars(e, bound, free);
        }
        Stmt::If { cond, then_, else_ } => {
            collect_expr_free_vars(cond, bound, free);
            for s in then_ {
                collect_stmt_free_vars(s, bound, free, local_bound);
            }
            if let Some(else_block) = else_ {
                for s in else_block {
                    collect_stmt_free_vars(s, bound, free, local_bound);
                }
            }
        }
        Stmt::While { cond, body } => {
            collect_expr_free_vars(cond, bound, free);
            for s in body {
                collect_stmt_free_vars(s, bound, free, local_bound);
            }
        }
        Stmt::Loop(body) => {
            for s in body {
                collect_stmt_free_vars(s, bound, free, local_bound);
            }
        }
        Stmt::For { var, iterable, body } => {
            collect_expr_free_vars(iterable, bound, free);
            let mut inner_bound = local_bound.clone();
            inner_bound.insert(var.clone());
            for s in body {
                collect_stmt_free_vars(s, &inner_bound, free, local_bound);
            }
        }
        Stmt::Assign { target, value } => {
            collect_expr_free_vars(target, bound, free);
            collect_expr_free_vars(value, bound, free);
        }
        Stmt::Block(block) => {
            for s in block {
                collect_stmt_free_vars(s, bound, free, local_bound);
            }
        }
        Stmt::OnFailure(block) | Stmt::Parasteps(block) | Stmt::Arena(block) => {
            for s in block {
                collect_stmt_free_vars(s, bound, free, local_bound);
            }
        }
        _ => {}
    }
}

pub(crate) fn collect_expr_free_vars(
    expr: &Expr,
    bound: &std::collections::HashSet<String>,
    free: &mut std::collections::HashSet<String>,
) {
    match expr {
        Expr::Ident(name) => {
            if !bound.contains(name) {
                free.insert(name.clone());
            }
        }
        Expr::Binary(_, l, r) => {
            collect_expr_free_vars(l, bound, free);
            collect_expr_free_vars(r, bound, free);
        }
        Expr::Unary(_, e) | Expr::Try(e) | Expr::Spawn(e) | Expr::Await(e) => {
            collect_expr_free_vars(e, bound, free);
        }
        Expr::Call(callee, args) => {
            collect_expr_free_vars(callee, bound, free);
            for a in args {
                collect_expr_free_vars(a, bound, free);
            }
        }
        Expr::Field(obj, _) | Expr::Index(obj, _) => {
            collect_expr_free_vars(obj, bound, free);
        }
        Expr::Tuple(elems) | Expr::List(elems) => {
            for e in elems {
                collect_expr_free_vars(e, bound, free);
            }
        }
        Expr::SetLiteral(elems) => {
            for e in elems {
                collect_expr_free_vars(e, bound, free);
            }
        }
        Expr::Match(subject, arms) => {
            collect_expr_free_vars(subject, bound, free);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_expr_free_vars(g, bound, free);
                }
                collect_expr_free_vars(&arm.body, bound, free);
            }
        }
        Expr::Record { fields, .. } => {
            for f in fields {
                collect_expr_free_vars(&f.value, bound, free);
            }
        }
        Expr::Lambda { params, body, .. } => {
            let mut inner_bound = bound.clone();
            for p in params {
                inner_bound.insert(p.name.clone());
            }
            let inner_free = collect_free_vars(body, &inner_bound);
            free.extend(inner_free);
        }
        Expr::Old(expr) => {
            collect_expr_free_vars(expr, bound, free);
        }
        // Newly handled expression types for correct closure capture
        Expr::Comprehension { expr: ce, iter, guard, .. } => {
            collect_expr_free_vars(ce, bound, free);
            collect_expr_free_vars(iter, bound, free);
            if let Some(g) = guard {
                collect_expr_free_vars(g, bound, free);
            }
        }
        Expr::If { cond, then_, else_ } => {
            collect_expr_free_vars(cond, bound, free);
            let mut inner_bound = bound.clone();
            for s in then_ {
                collect_stmt_free_vars(s, bound, free, &mut inner_bound);
            }
            if let Some(eb) = else_ {
                let mut inner_bound = bound.clone();
                for s in eb {
                    collect_stmt_free_vars(s, bound, free, &mut inner_bound);
                }
            }
        }
        Expr::SliceExpr { target, start, end } => {
            collect_expr_free_vars(target, bound, free);
            if let Some(s) = start { collect_expr_free_vars(s, bound, free); }
            if let Some(e) = end { collect_expr_free_vars(e, bound, free); }
        }
        Expr::Range { start, end } => {
            collect_expr_free_vars(start, bound, free);
            collect_expr_free_vars(end, bound, free);
        }
        Expr::Turbofish(_, _, args) => {
            for a in args {
                collect_expr_free_vars(a, bound, free);
            }
        }
        Expr::QuoteInterpolate(inner) => {
            collect_expr_free_vars(inner, bound, free);
        }
        Expr::TupleIndex(obj, _) => {
            collect_expr_free_vars(obj, bound, free);
        }
        Expr::TypeOf(inner) => {
            collect_expr_free_vars(inner, bound, free);
        }
        Expr::Arena(block) => {
            for s in block {
                collect_stmt_free_vars(s, bound, free, &mut bound.clone());
            }
        }
        Expr::Block(block) => {
            for s in block {
                collect_stmt_free_vars(s, bound, free, &mut bound.clone());
            }
        }
        // Expr::Literal, Expr::Quote, Expr::Comptime, Expr::TypeInfo have no free vars
        // or their content is compile-time only
        _ => {}
    }
}

pub(crate) fn collect_pattern_names(pat: &Pattern, names: &mut std::collections::HashSet<String>) {
    match pat {
        Pattern::Variable(name) => { names.insert(name.clone()); }
        Pattern::Tuple(pats) => {
            for p in pats {
                collect_pattern_names(p, names);
            }
        }
        Pattern::Constructor(_, pats) => {
            for p in pats {
                collect_pattern_names(p, names);
            }
        }
        _ => {}
    }
}
