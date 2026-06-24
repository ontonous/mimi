use std::collections::HashMap;

use serde_json::Value;

use crate::ast::{Expr, Item, Pattern, Stmt};
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

        // Pre-build param name lookup from all functions
        let mut func_params: HashMap<String, Vec<String>> = HashMap::new();
        for item in &file.items {
            if let Item::Func(f) = item {
                func_params.insert(f.name.clone(), f.params.iter().map(|p| p.name.clone()).collect());
            }
        }

        // Walk all function definitions looking for let statements and calls
        for item in &file.items {
            if let Item::Func(f) = item {
                self.collect_hints_from_block(&f.body, text, &mut hints, &func_params);
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
        func_params: &HashMap<String, Vec<String>>,
    ) {
        for stmt in stmts {
            #[allow(clippy::collapsible_match)]
            match stmt {
                Stmt::Let { pat, init, .. } => {
                    // Type hint for `let x = <literal>` — show the inferred type
                    if let Some(init_expr) = init {
                        let type_str = match init_expr {
                            Expr::Literal(lit) => match lit {
                                crate::ast::Lit::Int(_) => "i64",
                                crate::ast::Lit::Float(_) => "f64",
                                crate::ast::Lit::Bool(_) => "bool",
                                crate::ast::Lit::String(_) | crate::ast::Lit::FString(_) => "string",
                                crate::ast::Lit::Unit => "()",
                            },
                            _ => "",
                        };
                        if !type_str.is_empty() {
                            // Find the `=` position on the let line
                            let lines: Vec<&str> = text.lines().collect();
                            let pat_name = match pat {
                                Pattern::Variable(n) => n.as_str(),
                                _ => "",
                            };
                            if let Some(let_line) = lines.iter().position(|l| {
                                l.trim().starts_with("let") && !pat_name.is_empty() && l.contains(pat_name)
                            }) {
                                let line_text = lines[let_line];
                                if let Some(eq_pos) = line_text.find('=') {
                                    hints.push(serde_json::json!({
                                        "position": {
                                            "line": let_line,
                                            "character": eq_pos + 1
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
                Stmt::Expr(expr) | Stmt::Return(Some(expr)) => {
                    // Parameter name hints for function calls
                    self.collect_param_hints(expr, text, hints, func_params);
                }
                Stmt::If { cond: _, then_, else_ } => {
                    self.collect_hints_from_block(then_, text, hints, func_params);
                    if let Some(els) = else_ {
                        self.collect_hints_from_block(els, text, hints, func_params);
                    }
                }
                Stmt::While { cond: _, body } => {
                    self.collect_hints_from_block(body, text, hints, func_params);
                }
                Stmt::For { var: _, iterable: _, body } => {
                    self.collect_hints_from_block(body, text, hints, func_params);
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
        func_params: &HashMap<String, Vec<String>>,
    ) {
        #[allow(clippy::single_match)]
        match expr {
            Expr::Call(callee, args) => {
                // Extract function name from callee
                let func_name = match callee.as_ref() {
                    Expr::Ident(n) => n.as_str(),
                    _ => return,
                };
                let param_names = match func_params.get(func_name) {
                    Some(p) => p,
                    None => return,
                };
                // Find the call line
                let call_line = text.lines().position(|l| l.contains(func_name) && l.contains('('));
                let cl = match call_line {
                    Some(l) => l,
                    None => return,
                };
                let line_text: Vec<&str> = text.lines().collect();
                let line_content = match line_text.get(cl) {
                    Some(l) => l,
                    None => return,
                };
                // Find opening paren position
                let paren_pos = match line_content.find('(') {
                    Some(p) => p,
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
            _ => {}
        }
    }
}
