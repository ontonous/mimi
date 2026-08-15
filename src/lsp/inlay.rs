use std::collections::HashMap;

use serde_json::Value;

use crate::ast::{Expr, Item, PatternKind, Stmt};
use crate::lsp::LspServer;

impl LspServer {
    /// Compute inlay hints for the document: type hints for let bindings
    /// and parameter name hints for function calls.
    pub fn compute_inlay_hints(&self, text: &str) -> Vec<Value> {
        let mut hints = Vec::new();
        let file = match self.parse_with_recovery(text) {
            Some(f) => f,
            None => return hints,
        };

        // Pre-build param name lookup from all functions.
        // Use (name, pos.0) as key to handle duplicate function names across modules.
        let mut func_params: HashMap<(String, usize), Vec<String>> = HashMap::new();
        for item in &file.items {
            if let Item::Func(f) = item {
                func_params.insert(
                    (f.name.clone(), f.meta.span.start_line),
                    f.params.iter().map(|p| p.name.clone()).collect(),
                );
            }
        }

        // Walk all function definitions looking for let statements and calls
        for item in &file.items {
            if let Item::Func(f) = item {
                let func_key = (f.name.clone(), f.meta.span.start_line);
                self.collect_hints_from_block(&f.body, text, &mut hints, &func_params, &func_key);
            }
        }

        hints
    }

    /// Recursively collect inlay hints from statements in a block
    fn collect_hints_from_block(
        &self,
        stmts: &[Stmt],
        text: &str,
        hints: &mut Vec<Value>,
        func_params: &HashMap<(String, usize), Vec<String>>,
        current_func: &(String, usize),
    ) {
        for stmt in stmts {
            #[allow(clippy::collapsible_match)]
            match stmt.unlocated() {
                Stmt::Let { pat, init, .. } => {
                    // Type hint for `let x = <literal>` — show the inferred type
                    if let Some(init_expr) = init {
                        let type_str = match init_expr.unlocated() {
                            Expr::Literal(lit) => match lit {
                                crate::ast::Lit::Int(_) => "i64",
                                crate::ast::Lit::Float(_) => "f64",
                                crate::ast::Lit::Bool(_) => "bool",
                                crate::ast::Lit::String(_) | crate::ast::Lit::FString(_) => {
                                    "string"
                                }
                                crate::ast::Lit::Unit => "()",
                            },
                            _ => "",
                        };
                        if !type_str.is_empty() {
                            let pat_name = match &pat.kind {
                                PatternKind::Variable(n) => n.as_str(),
                                _ => "",
                            };
                            // 0.35.15 (DX backlog #3): the pattern span
                            // anchors at the binding name — the old
                            // `let <name>` line scan could land on a
                            // different binding with the same name earlier
                            // in the file.
                            if !pat_name.is_empty() && pat.meta.span.start_line > 0 {
                                let let_line = pat.meta.span.start_line.saturating_sub(1);
                                if let Some(line_text) = text.lines().nth(let_line) {
                                    let name_byte = crate::lsp::util::char_col_to_byte(
                                        line_text,
                                        pat.meta.span.start_col.saturating_sub(1),
                                    );
                                    // Scan for `=` AFTER the binding name so a
                                    // `==` comparison inside the initializer
                                    // cannot win over the assignment operator.
                                    let tail_start =
                                        (name_byte + pat_name.len()).min(line_text.len());
                                    if let Some(eq_off) = line_text[tail_start..].find('=') {
                                        let eq_byte = tail_start + eq_off;
                                        let map =
                                            crate::lsp::position_map::PositionMap::new(line_text);
                                        hints.push(serde_json::json!({
                                            "position": {
                                                "line": let_line,
                                                "character": map.byte_to_lsp(eq_byte + 1).1
                                            },
                                            "label": format!(": {}", type_str),
                                            "kind": 1,  // Type
                                            "paddingLeft": true
                                        }));
                                    }
                                }
                            }
                        }
                    }
                }
                Stmt::Expr(expr) | Stmt::Return(Some(expr)) => {
                    // Parameter name hints for function calls
                    self.collect_param_hints(expr, text, hints, func_params, current_func);
                }
                Stmt::If {
                    cond: _,
                    then_,
                    else_,
                } => {
                    self.collect_hints_from_block(then_, text, hints, func_params, current_func);
                    if let Some(els) = else_ {
                        self.collect_hints_from_block(els, text, hints, func_params, current_func);
                    }
                }
                Stmt::While { cond: _, body } => {
                    self.collect_hints_from_block(body, text, hints, func_params, current_func);
                }
                Stmt::For {
                    var: _,
                    iterable: _,
                    body,
                } => {
                    self.collect_hints_from_block(body, text, hints, func_params, current_func);
                }
                // M3 (audit-syntax 2026-08-03): if-let / while-let / ieee_float
                // bodies were silently skipped (no arms) — param hints inside
                // them never surfaced.
                Stmt::IfLet {
                    init, then_, else_, ..
                } => {
                    self.collect_param_hints(init, text, hints, func_params, current_func);
                    self.collect_hints_from_block(then_, text, hints, func_params, current_func);
                    if let Some(els) = else_ {
                        self.collect_hints_from_block(els, text, hints, func_params, current_func);
                    }
                }
                Stmt::WhileLet { init, body, .. } => {
                    self.collect_param_hints(init, text, hints, func_params, current_func);
                    self.collect_hints_from_block(body, text, hints, func_params, current_func);
                }
                Stmt::IeeeFloat(body) => {
                    self.collect_hints_from_block(body, text, hints, func_params, current_func);
                }
                Stmt::Block(body)
                | Stmt::Loop(body)
                | Stmt::Arena(body)
                | Stmt::Unsafe(body)
                | Stmt::Defer(body)
                | Stmt::OnFailure(body)
                | Stmt::Parasteps(body) => {
                    self.collect_hints_from_block(body, text, hints, func_params, current_func);
                }
                Stmt::Pinned { expr, body, .. } => {
                    self.collect_param_hints(expr, text, hints, func_params, current_func);
                    self.collect_hints_from_block(body, text, hints, func_params, current_func);
                }
                Stmt::Func(func) => {
                    self.collect_hints_from_block(
                        &func.body,
                        text,
                        hints,
                        func_params,
                        current_func,
                    );
                }
                _ => {}
            }
        }
    }

    /// Collect parameter name hints for function calls
    fn collect_param_hints(
        &self,
        expr: &Expr,
        text: &str,
        hints: &mut Vec<Value>,
        func_params: &HashMap<(String, usize), Vec<String>>,
        current_func: &(String, usize),
    ) {
        #[allow(clippy::single_match)]
        match expr.unlocated() {
            Expr::Call(callee, args) => {
                // Extract function name from callee
                let func_name = match callee.unlocated() {
                    Expr::Ident(n) => n.as_str(),
                    _ => return,
                };
                // Look up params using (func_name, current_func_line) as key.
                // This handles same-named functions in different modules correctly.
                let param_names = match func_params.get(&(func_name.to_string(), current_func.1)) {
                    Some(p) => p,
                    None => return,
                };
                // Find the call line from the callee span (0.35.15, DX
                // backlog #3) — the old text scan always landed on the
                // FIRST line mentioning the callee, corrupting hints for
                // repeated calls.
                let Some(callee_meta) = callee.meta() else {
                    return;
                };
                if callee_meta.span.start_line == 0 {
                    return;
                }
                let cl = callee_meta.span.start_line.saturating_sub(1);
                let line_content = match text.lines().nth(cl) {
                    Some(l) => l,
                    None => return,
                };
                // Opening paren: scan from the callee name end so an earlier
                // call on the same line cannot supply its paren.
                let callee_byte = crate::lsp::util::char_col_to_byte(
                    line_content,
                    callee_meta.span.start_col.saturating_sub(1),
                );
                let paren_search_start = callee_byte.min(line_content.len());
                let paren_pos = match line_content[paren_search_start..].find('(') {
                    Some(p) => paren_search_start + p,
                    None => return,
                };
                // For each argument that is non-trivial, add a param hint
                let mut depth = 0i32;
                let mut arg_start_byte = paren_pos + 1;
                let mut arg_start_char = line_content[..paren_pos + 1].chars().count();
                let mut arg_idx = 0;
                let mut byte_pos = paren_pos + 1;
                for (_, ch) in line_content[byte_pos..].char_indices() {
                    let ch_byte_len = ch.len_utf8();
                    match ch {
                        '(' | '[' | '{' => depth += 1,
                        ')' | ']' | '}' => depth -= 1,
                        ',' if depth == 0 => {
                            if arg_idx < args.len() && arg_idx < param_names.len() {
                                let arg_str = line_content[arg_start_byte..byte_pos].trim();
                                if !arg_str.is_empty()
                                    && !arg_str.chars().all(|c| c.is_alphanumeric() || c == '_')
                                {
                                    hints.push(serde_json::json!({
                                        "position": {
                                            "line": cl,
                                            "character": arg_start_char as u64
                                        },
                                        "label": format!("{}:", param_names[arg_idx]),
                                        "kind": 2,
                                        "paddingRight": true
                                    }));
                                }
                            }
                            arg_start_byte = byte_pos + ch_byte_len;
                            arg_start_char = line_content[..byte_pos + ch_byte_len].chars().count();
                            arg_idx += 1;
                        }
                        _ => {}
                    }
                    byte_pos += ch_byte_len;
                }
                // Last argument
                if arg_idx < args.len() && arg_idx < param_names.len() {
                    let end_pos = line_content.rfind(')').unwrap_or(line_content.len());
                    // X-6 (full audit 2026-08-05 §3.10): guard the range. For
                    // a multi-line call the scanned line can contain a closing
                    // paren from an INNER call before the last argument region
                    // (e.g. `add(add(1, 2),\n    3)` — the inner `)` sits left
                    // of the final-arg start), making end_pos < arg_start_byte.
                    // Slicing unguarded panicked the entire inlay-hints request;
                    // skip the hint instead of guessing at the argument text.
                    if end_pos >= arg_start_byte {
                        let arg_str = line_content[arg_start_byte..end_pos].trim();
                        if !arg_str.is_empty()
                            && !arg_str.chars().all(|c| c.is_alphanumeric() || c == '_')
                        {
                            hints.push(serde_json::json!({
                                "position": {
                                    "line": cl,
                                    "character": arg_start_char as u64
                                },
                                "label": format!("{}:", param_names[arg_idx]),
                                "kind": 2,
                                "paddingRight": true
                            }));
                        }
                    }
                }
            }
            _ => {}
        }
    }
}
