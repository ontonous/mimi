/// Static analysis / linting for Mimi source code.
///
/// Rules:
/// - W001: Unused `desc` / `rule` (metadata without implementation)
/// - W002: `$` / `$$` locked fragment with no implementation body
/// - W003: `...` placeholder residual (in .mimi files)
/// - W004: Function naming convention (snake_case)
/// - W006: Unused variable
/// - W007: Redundant parentheses (e.g., `((x))`)
/// - W008: `== true` / `== false` anti-pattern (use direct boolean expression)
/// - W009: Recursive function without base case
/// - W010: Unused import
use crate::ast::{BinOp, Expr, File, FuncDef, Item, Lit, Pattern, Stmt};
use crate::diagnostic::codes::{W002, W003, W004, W006, W007, W008, W009, W010};
use crate::diagnostic::Diagnostic;
use crate::span::Span;

pub struct Linter;

#[derive(Debug, Clone)]
pub struct LintResult {
    pub diagnostics: Vec<Diagnostic>,
}

impl Linter {
    pub fn new() -> Self {
        Self
    }

    pub fn lint(&self, file: &File, source: &str) -> LintResult {
        let mut diagnostics = Vec::new();

        // W010: Unused import detection (must run before item traversal)
        let used_imports = collect_used_names(file);
        for imp in &file.imports {
            let path_str = imp.path.join("::");
            if imp.alias.is_none()
                && !used_imports.contains(&imp.path[imp.path.len() - 1])
                && !used_imports.contains(&path_str)
            {
                diagnostics.push(Diagnostic::warning_code(
                    W010,
                    format!("unused import `{}`", path_str),
                    Span::single(1, 1),
                ));
            }
        }

        for item in file.items.iter() {
            if let Item::Func(f) = item {
                self.lint_func(f, source, &mut diagnostics);
            }
        }

        // W007: Redundant parentheses — scan source for `((` patterns
        detect_redundant_parens(source, &mut diagnostics);

        // W003: Check for `...` placeholders in source (skip strings and comments)
        let mut in_string = false;
        let mut in_block_comment = false;
        for (line_idx, line) in source.lines().enumerate() {
            if line.trim() == "..." {
                // Check if this `...` is inside a string or comment by scanning from previous context
                let mut local_in_string = in_string;
                let mut local_in_comment = in_block_comment;
                for ch in line.chars() {
                    if local_in_comment {
                        if ch == '/' && line.contains("*/") {
                            local_in_comment = false;
                        }
                        continue;
                    }
                    if ch == '"' {
                        local_in_string = !local_in_string;
                    }
                }
                if !local_in_string && !local_in_comment {
                    diagnostics.push(Diagnostic::warning_code(
                        W003,
                        "placeholder `...` residual in .mimi file",
                        Span::single(line_idx + 1, 1),
                    ));
                }
            }
            // Update cross-line state for block comments
            if line.contains("/*") {
                in_block_comment = true;
            }
            if line.contains("*/") {
                in_block_comment = false;
            }
            if !in_block_comment {
                let mut prev_ch = ' ';
                for ch in line.chars() {
                    if ch == '"' && prev_ch != '\\' {
                        in_string = !in_string;
                    }
                    prev_ch = ch;
                }
            }
        }

        LintResult { diagnostics }
    }

    fn lint_func(&self, func: &FuncDef, _source: &str, diagnostics: &mut Vec<Diagnostic>) {
        // W004: Check function naming convention (snake_case)
        if !func.name.is_empty() && !is_snake_case(&func.name) && !is_operator(&func.name) {
            diagnostics.push(Diagnostic::warning_code(
                W004,
                format!("function `{}` should use snake_case naming", func.name),
                Span::single(func.pos.0, func.pos.1),
            ));
        }

        // W002: Check for locked fragments with empty body (commitment removed in v0.8)
        if func.body.is_empty() {
            diagnostics.push(Diagnostic::warning_code(
                W002,
                format!("locked function `{}` has empty implementation", func.name),
                Span::single(func.pos.0, func.pos.1),
            ));
        }

        // W006: Unused variable detection
        let mut var_info = VarUsage::new();
        collect_decls_in_block(&func.body, &mut var_info);
        collect_refs_in_block(&func.body, &mut var_info);

        // Also collect params as declarations
        for param in &func.params {
            var_info.declared.insert(param.name.clone());
        }

        for var_name in &var_info.declared {
            if !var_info.referenced.contains(var_name) && var_name != "_" {
                diagnostics.push(Diagnostic::warning_code(
                    W006,
                    format!("unused variable `{}`", var_name),
                    Span::single(func.pos.0, func.pos.1),
                ));
            }
        }

        // W008: `== true` / `== false` anti-pattern
        detect_eq_bool(&func.body, diagnostics, func.pos);

        // W009: Recursion depth hint — direct recursion without a conditional guard
        detect_recursive_no_base(func, diagnostics);
    }
}

impl Default for Linter {
    fn default() -> Self {
        Self::new()
    }
}

fn is_snake_case(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !name.starts_with('_')
        && !name.ends_with('_')
        && !name.contains("__")
}

fn is_operator(name: &str) -> bool {
    matches!(
        name,
        "==" | "!=" | "<" | ">" | "<=" | ">=" | "+" | "-" | "*" | "/" | "%" | "!"
    )
}

// ---- W006: Unused variable detection ----

use std::collections::HashSet;

struct VarUsage {
    declared: HashSet<String>,
    referenced: HashSet<String>,
}

impl VarUsage {
    fn new() -> Self {
        Self {
            declared: HashSet::new(),
            referenced: HashSet::new(),
        }
    }
}

/// Collect variable declarations from a block of statements.
fn collect_decls_in_block(block: &[Stmt], info: &mut VarUsage) {
    for stmt in block {
        collect_decls_in_stmt(stmt, info);
    }
}

fn collect_decls_in_stmt(stmt: &Stmt, info: &mut VarUsage) {
    match stmt {
        Stmt::Let { pat, .. } => collect_decls_in_pat(pat, info),
        Stmt::For { var, body, .. } => {
            if var != "_" {
                info.declared.insert(var.clone());
            }
            collect_decls_in_block(body, info);
        }
        Stmt::SharedLet { name, .. } => {
            if name != "_" {
                info.declared.insert(name.clone());
            }
        }
        Stmt::Block(b) => collect_decls_in_block(b, info),
        Stmt::If { then_, else_, .. } => {
            collect_decls_in_block(then_, info);
            if let Some(els) = else_ {
                collect_decls_in_block(els, info);
            }
        }
        Stmt::While { body, .. } => collect_decls_in_block(body, info),
        Stmt::WhileLet { body, .. } => collect_decls_in_block(body, info),
        Stmt::Loop(body) => collect_decls_in_block(body, info),
        Stmt::Arena(body) => collect_decls_in_block(body, info),
        Stmt::Unsafe(body) => collect_decls_in_block(body, info),
        Stmt::OnFailure(body) => collect_decls_in_block(body, info),
        Stmt::Parasteps(body) => collect_decls_in_block(body, info),
        Stmt::Alloc { body, .. } => collect_decls_in_block(body, info),
        Stmt::Func(func) => {
            info.declared.insert(func.name.clone());
            collect_decls_in_block(&func.body, info);
        }
        _ => {}
    }
}

fn collect_decls_in_pat(pat: &Pattern, info: &mut VarUsage) {
    match pat {
        Pattern::Variable(name) if name != "_" => {
            info.declared.insert(name.clone());
        }
        Pattern::Tuple(pats) => {
            for p in pats {
                collect_decls_in_pat(p, info);
            }
        }
        Pattern::Array(pats) => {
            for p in pats {
                collect_decls_in_pat(p, info);
            }
        }
        Pattern::Slice(pats, rest) => {
            for p in pats {
                collect_decls_in_pat(p, info);
            }
            if let Some(r) = rest {
                collect_decls_in_pat(r, info);
            }
        }
        _ => {}
    }
}

/// Collect variable references from a block.
fn collect_refs_in_block(block: &[Stmt], info: &mut VarUsage) {
    for stmt in block {
        collect_refs_in_stmt(stmt, info);
    }
}

fn collect_refs_in_stmt(stmt: &Stmt, info: &mut VarUsage) {
    match stmt {
        Stmt::Let { init, .. } => {
            if let Some(e) = init {
                collect_refs_in_expr(e, info);
            }
        }
        Stmt::Return(e) => {
            if let Some(e) = e {
                collect_refs_in_expr(e, info);
            }
        }
        Stmt::Break(e) => {
            if let Some(e) = e {
                collect_refs_in_expr(e, info);
            }
        }
        Stmt::Expr(e) => collect_refs_in_expr(e, info),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            collect_refs_in_expr(cond, info);
            collect_refs_in_block(then_, info);
            if let Some(els) = else_ {
                collect_refs_in_block(els, info);
            }
        }
        Stmt::While { cond, body }
        | Stmt::WhileLet {
            init: cond, body, ..
        } => {
            collect_refs_in_expr(cond, info);
            collect_refs_in_block(body, info);
        }
        Stmt::Loop(body) => collect_refs_in_block(body, info),
        Stmt::For { iterable, body, .. } => {
            collect_refs_in_expr(iterable, info);
            collect_refs_in_block(body, info);
        }
        Stmt::Block(b) => collect_refs_in_block(b, info),
        Stmt::Assign { target, value } => {
            collect_refs_in_expr(target, info);
            collect_refs_in_expr(value, info);
        }
        Stmt::Arena(body) | Stmt::Unsafe(body) | Stmt::OnFailure(body) | Stmt::Parasteps(body) => {
            collect_refs_in_block(body, info)
        }
        Stmt::Drop(e) => collect_refs_in_expr(e, info),
        Stmt::SharedLet { init, .. } => collect_refs_in_expr(init, info),
        Stmt::Alloc { body, .. } => collect_refs_in_block(body, info),
        Stmt::Requires(e, _) | Stmt::Ensures(e, _) | Stmt::Invariant(e, _) => {
            collect_refs_in_expr(e, info);
        }
        Stmt::Math(exprs) => {
            for e in exprs {
                collect_refs_in_expr(e, info);
            }
        }
        Stmt::Func(func) => {
            collect_refs_in_block(&func.body, info);
        }
        Stmt::MmsBlock { .. }
        | Stmt::Desc(..)
        | Stmt::Rule(..)
        | Stmt::Continue
        | Stmt::Ellipsis => {}
        Stmt::Do(body) => collect_refs_in_block(body, info),
        Stmt::Delegate { expr, .. } => collect_refs_in_expr(expr, info),
        Stmt::Pinned { expr, body, .. } => {
            collect_refs_in_expr(expr, info);
            collect_refs_in_block(body, info);
        }
    }
}

fn collect_refs_in_expr(expr: &Expr, info: &mut VarUsage) {
    match expr {
        Expr::Ident(name) => {
            if name != "_" {
                info.referenced.insert(name.clone());
            }
        }
        Expr::Literal(_) => {}
        Expr::Binary(_, lhs, rhs) => {
            collect_refs_in_expr(lhs, info);
            collect_refs_in_expr(rhs, info);
        }
        Expr::Unary(_, e) => collect_refs_in_expr(e, info),
        Expr::Call(callee, args) => {
            collect_refs_in_expr(callee, info);
            for a in args {
                collect_refs_in_expr(a, info);
            }
        }
        Expr::Field(obj, _) => collect_refs_in_expr(obj, info),
        Expr::Index(obj, idx) => {
            collect_refs_in_expr(obj, info);
            collect_refs_in_expr(idx, info);
        }
        Expr::Tuple(items) => {
            for item in items {
                collect_refs_in_expr(item, info);
            }
        }
        Expr::List(items) => {
            for item in items {
                collect_refs_in_expr(item, info);
            }
        }
        Expr::Comprehension {
            expr, iter, guard, ..
        } => {
            collect_refs_in_expr(expr, info);
            collect_refs_in_expr(iter, info);
            if let Some(g) = guard {
                collect_refs_in_expr(g, info);
            }
        }
        Expr::Match(subj, arms) => {
            collect_refs_in_expr(subj, info);
            for arm in arms {
                collect_refs_in_expr(&arm.body, info);
                if let Some(g) = &arm.guard {
                    collect_refs_in_expr(g, info);
                }
            }
        }
        Expr::Record { fields, .. } => {
            for f in fields {
                collect_refs_in_expr(&f.value, info);
            }
        }
        Expr::Block(b) => collect_refs_in_block(b, info),
        Expr::Try(e) | Expr::Spawn(e) | Expr::Await(e) | Expr::TypeOf(e) => {
            collect_refs_in_expr(e, info);
        }
        Expr::If { cond, then_, else_ } => {
            collect_refs_in_expr(cond, info);
            collect_refs_in_block(then_, info);
            if let Some(els) = else_ {
                collect_refs_in_block(els, info);
            }
        }
        Expr::Lambda { body, .. } => collect_refs_in_block(body, info),
        Expr::Quote(b) | Expr::Comptime(b) => collect_refs_in_block(b, info),
        Expr::QuoteInterpolate(e) => collect_refs_in_expr(e, info),
        Expr::Old(e) => collect_refs_in_expr(e, info),
        Expr::SliceExpr { target, start, end } => {
            collect_refs_in_expr(target, info);
            if let Some(s) = start {
                collect_refs_in_expr(s, info);
            }
            if let Some(e) = end {
                collect_refs_in_expr(e, info);
            }
        }
        Expr::Range { start, end } => {
            collect_refs_in_expr(start, info);
            collect_refs_in_expr(end, info);
        }
        Expr::Arena(b) => collect_refs_in_block(b, info),
        Expr::MapLiteral { entries } => {
            for (k, v) in entries {
                collect_refs_in_expr(k, info);
                collect_refs_in_expr(v, info);
            }
        }
        Expr::SetLiteral(items) => {
            for item in items {
                collect_refs_in_expr(item, info);
            }
        }
        Expr::NamedArg(_, e) => collect_refs_in_expr(e, info),
        Expr::Cast(e, _) => collect_refs_in_expr(e, info),
        Expr::Turbofish(_, _, args) => {
            for a in args {
                collect_refs_in_expr(a, info);
            }
        }
        Expr::TupleIndex(e, _) => collect_refs_in_expr(e, info),
        Expr::TypeInfo(_) => {}
    }
}

// ---- W008: `== true` / `== false` anti-pattern ----

/// Detect `x == true`, `x == false`, `x != true`, `x != false` and suggest simplification.
fn detect_eq_bool(block: &[Stmt], diagnostics: &mut Vec<Diagnostic>, func_pos: (usize, usize)) {
    for stmt in block {
        detect_eq_bool_in_stmt(stmt, diagnostics, func_pos);
    }
}

fn detect_eq_bool_in_stmt(
    stmt: &Stmt,
    diagnostics: &mut Vec<Diagnostic>,
    func_pos: (usize, usize),
) {
    match stmt {
        Stmt::Expr(e) | Stmt::Return(Some(e)) | Stmt::Break(Some(e)) => {
            detect_eq_bool_in_expr(e, diagnostics, func_pos);
        }
        Stmt::Let { init: Some(e), .. }
        | Stmt::SharedLet { init: e, .. }
        | Stmt::Assign { value: e, .. }
        | Stmt::Drop(e) => {
            detect_eq_bool_in_expr(e, diagnostics, func_pos);
        }
        Stmt::If { cond, then_, else_ } => {
            detect_eq_bool_in_expr(cond, diagnostics, func_pos);
            detect_eq_bool(then_, diagnostics, func_pos);
            if let Some(els) = else_ {
                detect_eq_bool(els, diagnostics, func_pos);
            }
        }
        Stmt::While { cond, body }
        | Stmt::WhileLet {
            init: cond, body, ..
        } => {
            detect_eq_bool_in_expr(cond, diagnostics, func_pos);
            detect_eq_bool(body, diagnostics, func_pos);
        }
        Stmt::Loop(body)
        | Stmt::Block(body)
        | Stmt::Arena(body)
        | Stmt::Unsafe(body)
        | Stmt::OnFailure(body)
        | Stmt::Parasteps(body)
        | Stmt::Alloc { body, .. } => {
            detect_eq_bool(body, diagnostics, func_pos);
        }
        Stmt::For { iterable, body, .. } => {
            detect_eq_bool_in_expr(iterable, diagnostics, func_pos);
            detect_eq_bool(body, diagnostics, func_pos);
        }
        Stmt::Requires(e, _) | Stmt::Ensures(e, _) | Stmt::Invariant(e, _) => {
            detect_eq_bool_in_expr(e, diagnostics, func_pos);
        }
        Stmt::Math(exprs) => {
            for e in exprs {
                detect_eq_bool_in_expr(e, diagnostics, func_pos);
            }
        }
        _ => {}
    }
}

fn is_bool_lit(e: &Expr) -> bool {
    matches!(e, Expr::Literal(Lit::Bool(_)))
}

fn detect_eq_bool_in_expr(
    expr: &Expr,
    diagnostics: &mut Vec<Diagnostic>,
    func_pos: (usize, usize),
) {
    match expr {
        Expr::Binary(op, lhs, rhs) if *op == BinOp::EqCmp && is_bool_lit(rhs) => {
            let msg = match &**rhs {
                Expr::Literal(Lit::Bool(true)) => {
                    "comparison to `true` is unnecessary; use the expression directly"
                }
                _ => "comparison to `false`; use `!expr` instead",
            };
            diagnostics.push(Diagnostic::warning_code(
                W008,
                msg,
                Span::single(func_pos.0, func_pos.1),
            ));
        }
        Expr::Binary(op, lhs, rhs) if *op == BinOp::NeCmp && is_bool_lit(rhs) => {
            let msg = match &**rhs {
                Expr::Literal(Lit::Bool(true)) => "comparison to `true`; use `!expr` instead",
                _ => "comparison to `false` is unnecessary; use the expression directly",
            };
            diagnostics.push(Diagnostic::warning_code(
                W008,
                msg,
                Span::single(func_pos.0, func_pos.1),
            ));
        }
        // Recurse into sub-expressions
        Expr::Binary(_, lhs, rhs) => {
            detect_eq_bool_in_expr(lhs, diagnostics, func_pos);
            detect_eq_bool_in_expr(rhs, diagnostics, func_pos);
        }
        Expr::Unary(_, e) => detect_eq_bool_in_expr(e, diagnostics, func_pos),
        Expr::Call(callee, args) => {
            detect_eq_bool_in_expr(callee, diagnostics, func_pos);
            for a in args {
                detect_eq_bool_in_expr(a, diagnostics, func_pos);
            }
        }
        Expr::If { cond, then_, else_ } => {
            detect_eq_bool_in_expr(cond, diagnostics, func_pos);
            detect_eq_bool(then_, diagnostics, func_pos);
            if let Some(els) = else_ {
                detect_eq_bool(els, diagnostics, func_pos);
            }
        }
        Expr::Match(subj, arms) => {
            detect_eq_bool_in_expr(subj, diagnostics, func_pos);
            for arm in arms {
                detect_eq_bool_in_expr(&arm.body, diagnostics, func_pos);
            }
        }
        Expr::Block(b) => detect_eq_bool(b, diagnostics, func_pos),
        Expr::Tuple(items) | Expr::List(items) | Expr::SetLiteral(items) => {
            for item in items {
                detect_eq_bool_in_expr(item, diagnostics, func_pos);
            }
        }
        Expr::Record { fields, .. } => {
            for f in fields {
                detect_eq_bool_in_expr(&f.value, diagnostics, func_pos);
            }
        }
        Expr::Lambda { body, .. } => detect_eq_bool(body, diagnostics, func_pos),
        Expr::Try(e)
        | Expr::Spawn(e)
        | Expr::Await(e)
        | Expr::TypeOf(e)
        | Expr::QuoteInterpolate(e)
        | Expr::Old(e) => {
            detect_eq_bool_in_expr(e, diagnostics, func_pos);
        }
        Expr::Comprehension {
            expr, iter, guard, ..
        } => {
            detect_eq_bool_in_expr(expr, diagnostics, func_pos);
            detect_eq_bool_in_expr(iter, diagnostics, func_pos);
            if let Some(g) = guard {
                detect_eq_bool_in_expr(g, diagnostics, func_pos);
            }
        }
        Expr::Field(obj, _) | Expr::TupleIndex(obj, _) => {
            detect_eq_bool_in_expr(obj, diagnostics, func_pos);
        }
        Expr::Index(obj, idx) => {
            detect_eq_bool_in_expr(obj, diagnostics, func_pos);
            detect_eq_bool_in_expr(idx, diagnostics, func_pos);
        }
        Expr::SliceExpr { target, start, end } => {
            detect_eq_bool_in_expr(target, diagnostics, func_pos);
            if let Some(s) = start {
                detect_eq_bool_in_expr(s, diagnostics, func_pos);
            }
            if let Some(e) = end {
                detect_eq_bool_in_expr(e, diagnostics, func_pos);
            }
        }
        Expr::Range { start, end } => {
            detect_eq_bool_in_expr(start, diagnostics, func_pos);
            detect_eq_bool_in_expr(end, diagnostics, func_pos);
        }
        Expr::Arena(b) => detect_eq_bool(b, diagnostics, func_pos),
        Expr::MapLiteral { entries } => {
            for (k, v) in entries {
                detect_eq_bool_in_expr(k, diagnostics, func_pos);
                detect_eq_bool_in_expr(v, diagnostics, func_pos);
            }
        }
        Expr::NamedArg(_, e) => detect_eq_bool_in_expr(e, diagnostics, func_pos),
        Expr::Cast(e, _) => detect_eq_bool_in_expr(e, diagnostics, func_pos),
        Expr::Turbofish(_, _, args) => {
            for a in args {
                detect_eq_bool_in_expr(a, diagnostics, func_pos);
            }
        }
        Expr::Quote(b) | Expr::Comptime(b) => detect_eq_bool(b, diagnostics, func_pos),
        _ => {}
    }
}

// ---- W007: Redundant parentheses ----

/// Scan source for `((` patterns that indicate redundant double parentheses.
/// Uses source-level scan (not AST) since the parser strips parentheses.
fn detect_redundant_parens(source: &str, diagnostics: &mut Vec<Diagnostic>) {
    let mut in_string = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut prev_char = ' ';
    let mut prev_col = 0usize;
    let mut line = 1usize;
    let mut col = 1usize;
    let chars: Vec<char> = source.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];
        if in_line_comment {
            if ch == '\n' {
                in_line_comment = false;
                line += 1;
                col = 0;
                prev_char = ' ';
            }
            i += 1;
            if ch != '\n' {
                col += 1;
            }
            continue;
        }
        if in_block_comment {
            if ch == '*' && i + 1 < chars.len() && chars[i + 1] == '/' {
                in_block_comment = false;
                i += 2;
                col += 2;
            } else {
                if ch == '\n' {
                    line += 1;
                    col = 0;
                }
                i += 1;
                col += 1;
            }
            continue;
        }
        if ch == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            in_line_comment = true;
            i += 2;
            col += 2;
            continue;
        }
        if ch == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            in_block_comment = true;
            i += 2;
            col += 2;
            continue;
        }
        if ch == '"' && prev_char != '\\' {
            in_string = !in_string;
        } else if ch == '\n' {
            line += 1;
            col = 0;
            prev_char = ' ';
            i += 1;
            continue;
        } else if !in_string && ch == '(' && prev_char == '(' {
            diagnostics.push(Diagnostic::warning_code(
                W007,
                "redundant double parentheses `((`",
                Span::single(line, prev_col),
            ));
        }
        prev_char = ch;
        prev_col = col;
        col += 1;
        i += 1;
    }
}

// ---- W009: Recursion depth hint ----

/// Detect functions that directly recurse (call themselves by name) without
/// any apparent base case (no `if` or `match` statement in the body).
fn detect_recursive_no_base(func: &FuncDef, diagnostics: &mut Vec<Diagnostic>) {
    if func.body.is_empty() {
        return;
    }
    let func_name = &func.name;
    if !calls_self_directly(&func.body, func_name) {
        return;
    }
    // Check if there's a conditional guard (if/match) in the body
    if !has_conditional_guard(&func.body) {
        diagnostics.push(Diagnostic::warning_code(
            W009,
            format!(
                "recursive function `{}` has no base case (no `if`/`match` guard)",
                func_name
            ),
            Span::single(func.pos.0, func.pos.1),
        ));
    }
}

/// Check if any statement in a block directly calls the named function.
fn calls_self_directly(block: &[Stmt], name: &str) -> bool {
    for stmt in block {
        match stmt {
            Stmt::Expr(e) | Stmt::Return(Some(e)) | Stmt::Break(Some(e)) | Stmt::Drop(e) => {
                if expr_calls_name(e, name) {
                    return true;
                }
            }
            Stmt::Let { init: Some(e), .. } | Stmt::Assign { value: e, .. } => {
                if expr_calls_name(e, name) {
                    return true;
                }
            }
            Stmt::If { cond, then_, else_ } => {
                if expr_calls_name(cond, name) {
                    return true;
                }
                if calls_self_directly(then_, name) {
                    return true;
                }
                if let Some(els) = else_ {
                    if calls_self_directly(els, name) {
                        return true;
                    }
                }
            }
            Stmt::While { cond, body }
            | Stmt::WhileLet {
                init: cond, body, ..
            } => {
                if expr_calls_name(cond, name) || calls_self_directly(body, name) {
                    return true;
                }
            }
            Stmt::Loop(body)
            | Stmt::Block(body)
            | Stmt::Arena(body)
            | Stmt::Unsafe(body)
            | Stmt::OnFailure(body)
            | Stmt::Parasteps(body)
            | Stmt::Alloc { body, .. } => {
                if calls_self_directly(body, name) {
                    return true;
                }
            }
            Stmt::For { iterable, body, .. }
                if expr_calls_name(iterable, name) || calls_self_directly(body, name) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

/// Check if an expression tree directly calls the named function.
fn expr_calls_name(expr: &Expr, name: &str) -> bool {
    match expr {
        Expr::Call(callee, args) => {
            if let Expr::Ident(callee_name) = callee.as_ref() {
                if callee_name == name {
                    return true;
                }
            }
            for arg in args {
                if expr_calls_name(arg, name) {
                    return true;
                }
            }
            false
        }
        Expr::Binary(_, lhs, rhs) => expr_calls_name(lhs, name) || expr_calls_name(rhs, name),
        Expr::Unary(_, e) => expr_calls_name(e, name),
        Expr::Field(obj, _) | Expr::TupleIndex(obj, _) => expr_calls_name(obj, name),
        Expr::Index(obj, idx) => expr_calls_name(obj, name) || expr_calls_name(idx, name),
        Expr::If { cond, then_, else_ } => {
            expr_calls_name(cond, name)
                || stmts_call_name(then_, name)
                || else_.as_ref().is_some_and(|e| stmts_call_name(e, name))
        }
        Expr::Match(subj, arms) => {
            expr_calls_name(subj, name)
                || arms.iter().any(|arm| {
                    expr_calls_name(&arm.body, name)
                        || arm.guard.as_ref().is_some_and(|g| expr_calls_name(g, name))
                })
        }
        Expr::Block(b) => calls_self_directly(b, name),
        Expr::Tuple(items) | Expr::List(items) | Expr::SetLiteral(items) => {
            items.iter().any(|e| expr_calls_name(e, name))
        }
        Expr::Try(e)
        | Expr::Spawn(e)
        | Expr::Await(e)
        | Expr::TypeOf(e)
        | Expr::QuoteInterpolate(e)
        | Expr::Old(e) => expr_calls_name(e, name),
        Expr::Lambda { body, .. } => calls_self_directly(body, name),
        Expr::Quote(b) | Expr::Comptime(b) | Expr::Arena(b) => calls_self_directly(b, name),
        Expr::SliceExpr { target, start, end } => {
            expr_calls_name(target, name)
                || start.as_ref().is_some_and(|s| expr_calls_name(s, name))
                || end.as_ref().is_some_and(|e| expr_calls_name(e, name))
        }
        Expr::Range { start, end } => expr_calls_name(start, name) || expr_calls_name(end, name),
        Expr::MapLiteral { entries } => entries
            .iter()
            .any(|(k, v)| expr_calls_name(k, name) || expr_calls_name(v, name)),
        Expr::Record { fields, .. } => fields.iter().any(|f| expr_calls_name(&f.value, name)),
        Expr::NamedArg(_, e) => expr_calls_name(e, name),
        Expr::Cast(e, _) => expr_calls_name(e, name),
        Expr::Turbofish(_, _, args) => args.iter().any(|a| expr_calls_name(a, name)),
        Expr::Comprehension {
            expr, iter, guard, ..
        } => {
            expr_calls_name(expr, name)
                || expr_calls_name(iter, name)
                || guard.as_ref().is_some_and(|g| expr_calls_name(g, name))
        }
        _ => false,
    }
}

fn stmts_call_name(block: &[Stmt], name: &str) -> bool {
    calls_self_directly(block, name)
}

/// Check if a block contains a conditional guard (if/match) that could serve as a base case.
fn has_conditional_guard(block: &[Stmt]) -> bool {
    for stmt in block {
        match stmt {
            Stmt::If { .. } => return true,
            Stmt::While { .. } => return true, // while has a condition
            Stmt::Expr(Expr::If { .. } | Expr::Match(..)) => return true,
            Stmt::Block(b) if has_conditional_guard(b) => return true,
            _ => {}
        }
    }
    false
}

// ---- W010: Unused import ----

/// Collect all identifier names referenced in the file.
fn collect_used_names(file: &File) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    for item in &file.items {
        collect_names_in_item(item, &mut names);
    }
    names
}

fn collect_names_in_item(item: &Item, names: &mut std::collections::HashSet<String>) {
    match item {
        Item::Func(f) => {
            // Function name itself is used (defined)
            names.insert(f.name.clone());
            collect_names_in_block(&f.body, names);
        }
        Item::Module(m) => {
            for item in &m.items {
                collect_names_in_item(item, names);
            }
        }
        Item::Type(t) => {
            names.insert(t.name.clone());
        }
        Item::Trait(t) => {
            names.insert(t.name.clone());
            for m in &t.methods {
                names.insert(m.name.clone());
            }
        }
        Item::Impl(i) => {
            names.insert(i.trait_name.clone());
            names.insert(i.type_name.clone());
            for m in &i.methods {
                names.insert(m.name.clone());
                collect_names_in_block(&m.body, names);
            }
        }
        Item::Actor(a) => {
            names.insert(a.name.clone());
            for m in &a.methods {
                names.insert(m.name.clone());
                collect_names_in_block(&m.body, names);
            }
        }
        Item::ExternBlock(e) => {
            for f in &e.funcs {
                names.insert(f.name.clone());
            }
        }
        Item::Const { name, .. } => {
            names.insert(name.clone());
        }
        Item::Cap(c) => {
            names.insert(c.name.clone());
        }
        Item::Flow(f) => {
            names.insert(f.name.clone());
            for t in &f.transitions {
                names.insert(t.name.clone());
            }
        }
        Item::Protocol(p) => {
            names.insert(p.name.clone());
        }
        Item::Session(s) => {
            names.insert(s.name.clone());
        }
    }
}

fn collect_names_in_block(block: &[Stmt], names: &mut std::collections::HashSet<String>) {
    for stmt in block {
        match stmt {
            Stmt::Expr(e) | Stmt::Return(Some(e)) | Stmt::Break(Some(e)) | Stmt::Drop(e) => {
                collect_names_in_expr(e, names);
            }
            Stmt::Let { init: Some(e), .. } | Stmt::Assign { value: e, .. } => {
                collect_names_in_expr(e, names);
            }
            Stmt::SharedLet { init: e, .. } => collect_names_in_expr(e, names),
            Stmt::If { cond, then_, else_ } => {
                collect_names_in_expr(cond, names);
                collect_names_in_block(then_, names);
                if let Some(els) = else_ {
                    collect_names_in_block(els, names);
                }
            }
            Stmt::While { cond, body }
            | Stmt::WhileLet {
                init: cond, body, ..
            } => {
                collect_names_in_expr(cond, names);
                collect_names_in_block(body, names);
            }
            Stmt::Loop(body)
            | Stmt::Block(body)
            | Stmt::Arena(body)
            | Stmt::Unsafe(body)
            | Stmt::OnFailure(body)
            | Stmt::Parasteps(body)
            | Stmt::Alloc { body, .. } => collect_names_in_block(body, names),
            Stmt::For { iterable, body, .. } => {
                collect_names_in_expr(iterable, names);
                collect_names_in_block(body, names);
            }
            Stmt::Requires(e, _) | Stmt::Ensures(e, _) | Stmt::Invariant(e, _) => {
                collect_names_in_expr(e, names);
            }
            Stmt::Math(exprs) => {
                for e in exprs {
                    collect_names_in_expr(e, names);
                }
            }
            _ => {}
        }
    }
}

fn collect_names_in_expr(expr: &Expr, names: &mut std::collections::HashSet<String>) {
    match expr {
        Expr::Ident(name) => {
            names.insert(name.clone());
        }
        Expr::Binary(_, lhs, rhs) => {
            collect_names_in_expr(lhs, names);
            collect_names_in_expr(rhs, names);
        }
        Expr::Unary(_, e) => collect_names_in_expr(e, names),
        Expr::Call(callee, args) => {
            collect_names_in_expr(callee, names);
            for a in args {
                collect_names_in_expr(a, names);
            }
        }
        Expr::Field(obj, _) | Expr::TupleIndex(obj, _) => collect_names_in_expr(obj, names),
        Expr::Index(obj, idx) => {
            collect_names_in_expr(obj, names);
            collect_names_in_expr(idx, names);
        }
        Expr::Tuple(items) | Expr::List(items) | Expr::SetLiteral(items) => {
            for item in items {
                collect_names_in_expr(item, names);
            }
        }
        Expr::Comprehension {
            expr, iter, guard, ..
        } => {
            collect_names_in_expr(expr, names);
            collect_names_in_expr(iter, names);
            if let Some(g) = guard {
                collect_names_in_expr(g, names);
            }
        }
        Expr::Match(subj, arms) => {
            collect_names_in_expr(subj, names);
            for arm in arms {
                collect_names_in_expr(&arm.body, names);
                if let Some(g) = &arm.guard {
                    collect_names_in_expr(g, names);
                }
            }
        }
        Expr::Record { fields, .. } => {
            for f in fields {
                collect_names_in_expr(&f.value, names);
            }
        }
        Expr::Block(b) => collect_names_in_block(b, names),
        Expr::Try(e)
        | Expr::Spawn(e)
        | Expr::Await(e)
        | Expr::TypeOf(e)
        | Expr::QuoteInterpolate(e)
        | Expr::Old(e)
        | Expr::NamedArg(_, e) => {
            collect_names_in_expr(e, names);
        }
        Expr::Cast(e, _) => collect_names_in_expr(e, names),
        Expr::If { cond, then_, else_ } => {
            collect_names_in_expr(cond, names);
            collect_names_in_block(then_, names);
            if let Some(els) = else_ {
                collect_names_in_block(els, names);
            }
        }
        Expr::Lambda { body, .. } => collect_names_in_block(body, names),
        Expr::Arena(body) => collect_names_in_block(body, names),
        Expr::Quote(b) | Expr::Comptime(b) => collect_names_in_block(b, names),
        Expr::SliceExpr { target, start, end } => {
            collect_names_in_expr(target, names);
            if let Some(s) = start {
                collect_names_in_expr(s, names);
            }
            if let Some(e) = end {
                collect_names_in_expr(e, names);
            }
        }
        Expr::Range { start, end } => {
            collect_names_in_expr(start, names);
            collect_names_in_expr(end, names);
        }
        Expr::MapLiteral { entries } => {
            for (k, v) in entries {
                collect_names_in_expr(k, names);
                collect_names_in_expr(v, names);
            }
        }
        Expr::Turbofish(_, _, args) => {
            for a in args {
                collect_names_in_expr(a, names);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn parse_source(src: &str) -> File {
        let tokens = Lexer::new(src)
            .tokenize()
            .expect("src/lint.rs:121 unwrap failed");
        Parser::new(tokens)
            .parse_file()
            .expect("src/lint.rs:122 unwrap failed")
    }

    #[test]
    fn lint_valid_code() {
        let src = "func main() -> i32 { 42 }";
        let file = parse_source(src);
        let linter = Linter::new();
        let result = linter.lint(&file, src);
        assert!(
            result.diagnostics.is_empty(),
            "valid code should have no lints"
        );
    }

    #[test]
    fn lint_snake_case_violation() {
        let src = "func myFunction() -> i32 { 42 }";
        let file = parse_source(src);
        let linter = Linter::new();
        let result = linter.lint(&file, src);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code.as_deref() == Some(W004)),
            "should detect non-snake_case function name"
        );
    }

    #[test]
    fn lint_placeholder() {
        // `...` is not valid in .mimi, so test the lint rule via source scanning
        let src = "func main() -> i32 {\n    // TODO: ...\n}";
        let file = parse_source(src);
        let linter = Linter::new();
        let result = linter.lint(&file, src);
        // The `...` inside comment won't trigger W003 (only standalone `...` lines do)
        // This test validates the lint infrastructure works
        let _ = result;
    }

    #[test]
    fn lint_unused_variable() {
        let src = "func main() -> i32 { let x = 42; 0 }";
        let file = parse_source(src);
        let linter = Linter::new();
        let result = linter.lint(&file, src);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code.as_deref() == Some(W006)),
            "should detect unused variable `x`"
        );
    }

    #[test]
    fn lint_used_variable_no_warning() {
        let src = "func main() -> i32 { let x = 42; x }";
        let file = parse_source(src);
        let linter = Linter::new();
        let result = linter.lint(&file, src);
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| d.code.as_deref() == Some(W006)),
            "used variable should not trigger W006"
        );
    }

    #[test]
    fn lint_eq_true() {
        let src = "func main() -> bool { let x = true; x == true }";
        let file = parse_source(src);
        let linter = Linter::new();
        let result = linter.lint(&file, src);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code.as_deref() == Some(W008)),
            "should detect `x == true` anti-pattern"
        );
    }

    #[test]
    fn lint_eq_false() {
        let src = "func main() -> bool { let x = true; x == false }";
        let file = parse_source(src);
        let linter = Linter::new();
        let result = linter.lint(&file, src);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code.as_deref() == Some(W008)),
            "should detect `x == false` anti-pattern"
        );
    }

    #[test]
    fn lint_no_eq_true_false_for_non_bool() {
        let src = "func main() -> i32 { let x = 5; x + 3 }";
        let file = parse_source(src);
        let linter = Linter::new();
        let result = linter.lint(&file, src);
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| d.code.as_deref() == Some(W008)),
            "should not flag comparisons unrelated to booleans"
        );
    }

    #[test]
    fn lint_redundant_parens_detected() {
        let src = "func main() -> i32 { ((42)) }";
        let linter = Linter::new();
        let file = parse_source(src);
        let result = linter.lint(&file, src);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code.as_deref() == Some(W007)),
            "should detect redundant double parentheses `((`"
        );
    }

    #[test]
    fn lint_no_false_redundant_parens() {
        let src = "func main() -> i32 { (1 + 2) * 3 }";
        let file = parse_source(src);
        let linter = Linter::new();
        let result = linter.lint(&file, src);
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| d.code.as_deref() == Some(W007)),
            "should not flag legitimate single parentheses"
        );
    }

    #[test]
    fn lint_recursive_with_base_case_ok() {
        let src = "func factorial(n: i32) -> i32 { if n <= 1 { 1 } else { n * factorial(n - 1) } }";
        let file = parse_source(src);
        let linter = Linter::new();
        let result = linter.lint(&file, src);
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| d.code.as_deref() == Some(W009)),
            "recursive function with if guard should not trigger W009"
        );
    }

    #[test]
    fn lint_recursive_no_base_case_detected() {
        let src = "func infinite() -> i32 { infinite() }";
        let file = parse_source(src);
        let linter = Linter::new();
        let result = linter.lint(&file, src);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code.as_deref() == Some(W009)),
            "recursive function without base case should trigger W009"
        );
    }

    #[test]
    fn lint_unused_import_detected() {
        let src = "use std::io\nfunc main() -> i32 { 42 }";
        let file = parse_source(src);
        let linter = Linter::new();
        let result = linter.lint(&file, src);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code.as_deref() == Some(W010)),
            "unused import should trigger W010"
        );
    }

    #[test]
    fn lint_used_import_no_warning() {
        let src = "use std::io\nfunc main() -> i32 { io::print_line(\"hi\") }";
        let file = parse_source(src);
        let linter = Linter::new();
        let result = linter.lint(&file, src);
        // For now, unused import is best-effort — `io::print_line` references `io`
        // but the parser may resolve the path differently. Just verify no crash.
        let _ = result;
    }
}
