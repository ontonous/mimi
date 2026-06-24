use serde_json::Value;

use crate::ast::Item;
use crate::lsp::LspServer;

impl LspServer {
    pub fn compute_definition(
        &self,
        text: &str,
        line: usize,
        character: usize,
        uri: &str,
    ) -> Option<Value> {
        // Get the word at cursor position
        let lines: Vec<&str> = text.lines().collect();
        let current_line = lines.get(line)?;
        let before_cursor: String = current_line.chars().take(character).collect();
        let after_cursor: String = current_line.chars().skip(character).collect();

        // Find word boundaries
        let word_start = before_cursor
            .rfind(|c: char| !c.is_alphanumeric() && c != '_')
            .map(|i| i + 1)
            .unwrap_or(0);
        let word_end = after_cursor
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .map(|i| character + i)
            .unwrap_or(current_line.len());
        let word = &current_line[word_start..word_end];

        if word.is_empty() {
            return None;
        }

        // Try to parse and find the symbol definition
        if let Some(file) = self.parse_with_recovery(text) {
            for item in &file.items {
                match item {
                    Item::Func(f) if f.name == word => {
                        // Find the line where the function is defined
                        let def_line = text
                            .lines()
                            .position(|l| l.contains(&format!("func {}", word)))
                            .unwrap_or(0);
                        return Some(serde_json::json!({
                            "uri": uri,
                            "range": {
                                "start": { "line": def_line, "character": 0 },
                                "end": { "line": def_line, "character": 100 }
                            }
                        }));
                    }
                    Item::Type(t) if t.name == word => {
                        let def_line = text
                            .lines()
                            .position(|l| l.contains(&format!("type {}", word)))
                            .unwrap_or(0);
                        return Some(serde_json::json!({
                            "uri": uri,
                            "range": {
                                "start": { "line": def_line, "character": 0 },
                                "end": { "line": def_line, "character": 100 }
                            }
                        }));
                    }
                    Item::Module(m) if m.name == word => {
                        let def_line = text
                            .lines()
                            .position(|l| l.contains(&format!("module {}", word)))
                            .unwrap_or(0);
                        return Some(serde_json::json!({
                            "uri": uri,
                            "range": {
                                "start": { "line": def_line, "character": 0 },
                                "end": { "line": def_line, "character": 100 }
                            }
                        }));
                    }
                    _ => {}
                }
            }
        }

        // Builtins don't have definitions in user code
        None
    }

    /// Go to implementation for a trait name: find all `impl` blocks for this trait
    pub fn compute_go_to_implementation(
        &self,
        text: &str,
        line: usize,
        character: usize,
        uri: &str,
    ) -> Vec<Value> {
        let word = self.get_word_at(text, line, character);
        if word.is_empty() {
            return Vec::new();
        }

        let mut locations = Vec::new();
        if let Some(file) = self.parse_with_recovery(text) {
            // Check if word is a trait name
            let is_trait = file.items.iter().any(|item| {
                matches!(item, Item::Trait(t) if t.name == word)
            });
            if !is_trait {
                return locations; // Not a trait — no implementations to find
            }

            // Find all impl blocks for this trait
            for impl_def in &file.items {
                if let Item::Impl(imp) = impl_def {
                    if imp.trait_name == word {
                        let impl_line = text
                            .lines()
                            .position(|l| l.contains("impl") && l.contains(&word))
                            .unwrap_or(0);
                        locations.push(serde_json::json!({
                            "uri": uri,
                            "range": {
                                "start": { "line": impl_line, "character": 0 },
                                "end": { "line": impl_line, "character": 100 }
                            }
                        }));
                    }
                }
            }
        }
        locations
    }

    /// Find all references to the symbol at the given position
    pub fn compute_references(
        &self,
        text: &str,
        line: usize,
        character: usize,
        uri: &str,
        include_decl: bool,
    ) -> Vec<Value> {
        let word = self.get_word_at(text, line, character);
        if word.is_empty() {
            return Vec::new();
        }

        let mut references = Vec::new();
        let mut def_line: Option<usize> = None;
        let mut def_col: Option<usize> = None;
        let lines: Vec<&str> = text.lines().collect();

        // First, find the definition location
        if let Some(file) = self.parse_with_recovery(text) {
            for item in &file.items {
                match item {
                    Item::Func(f) if f.name == word => {
                        def_line = text
                            .lines()
                            .position(|l| l.contains(&format!("func {}", word)));
                        def_col = def_line.and_then(|l| {
                            lines.get(l).map(|line| {
                                line.find(&format!("func {}", word)).unwrap_or(0) + 5
                            })
                        });
                        break;
                    }
                    Item::Type(t) if t.name == word => {
                        def_line = text
                            .lines()
                            .position(|l| l.contains(&format!("type {}", word)));
                        def_col = def_line.and_then(|l| {
                            lines.get(l).map(|line| {
                                line.find(&format!("type {}", word)).unwrap_or(0) + 5
                            })
                        });
                        break;
                    }
                    Item::Module(m) if m.name == word => {
                        def_line = text
                            .lines()
                            .position(|l| l.contains(&format!("module {}", word)));
                        def_col = def_line.and_then(|l| {
                            lines.get(l).map(|line| {
                                line.find(&format!("module {}", word)).unwrap_or(0) + 7
                            })
                        });
                        break;
                    }
                    _ => {}
                }
            }
        }

        // Add definition if requested
        if include_decl {
            if let Some(dl) = def_line {
                references.push(serde_json::json!({
                    "uri": uri,
                    "range": {
                        "start": { "line": dl, "character": def_col.unwrap_or(0) },
                        "end": { "line": dl, "character": def_col.unwrap_or(0) + word.len() }
                    }
                }));
            }
        }

        // Find all usages in text
        for (i, line_text) in lines.iter().enumerate() {
            let mut start = 0;
            while let Some(pos) = line_text[start..].find(word.as_str()) {
                let abs_pos = start + pos;
                let before = abs_pos > 0
                    && line_text
                        .chars()
                        .nth(abs_pos - 1)
                        .map(|c| c.is_alphanumeric() || c == '_')
                        .unwrap_or(false);
                let after = line_text
                    .chars()
                    .nth(abs_pos + word.len())
                    .map(|c| c.is_alphanumeric() || c == '_')
                    .unwrap_or(false);

                if !before && !after {
                    // Skip definition location if we already added it
                    if let Some(dl) = def_line {
                        if i == dl && (def_col == Some(abs_pos)) {
                            start = abs_pos + 1;
                            continue;
                        }
                    }
                    references.push(serde_json::json!({
                        "uri": uri,
                        "range": {
                            "start": { "line": i, "character": abs_pos },
                            "end": { "line": i, "character": abs_pos + word.len() }
                        }
                    }));
                }
                start = abs_pos + 1;
            }
        }

        references
    }

    /// Rename all occurrences of the symbol at the given position
    pub fn compute_rename(
        &self,
        text: &str,
        line: usize,
        character: usize,
        uri: &str,
        new_name: &str,
    ) -> Option<Value> {
        let word = self.get_word_at(text, line, character);
        if word.is_empty() || word == new_name {
            return None;
        }

        let mut changes = Vec::new();
        let lines: Vec<&str> = text.lines().collect();

        for (i, line_text) in lines.iter().enumerate() {
            let mut start = 0;
            while let Some(pos) = line_text[start..].find(word.as_str()) {
                let abs_pos = start + pos;
                let before = abs_pos > 0
                    && line_text
                        .chars()
                        .nth(abs_pos - 1)
                        .map(|c| c.is_alphanumeric() || c == '_')
                        .unwrap_or(false);
                let after = line_text
                    .chars()
                    .nth(abs_pos + word.len())
                    .map(|c| c.is_alphanumeric() || c == '_')
                    .unwrap_or(false);

                if !before && !after {
                    changes.push(serde_json::json!({
                        "range": {
                            "start": { "line": i, "character": abs_pos },
                            "end": { "line": i, "character": abs_pos + word.len() }
                        },
                        "newText": new_name
                    }));
                }
                start = abs_pos + 1;
            }
        }

        if changes.is_empty() {
            return None;
        }

        Some(serde_json::json!({
            "changes": {
                uri: changes
            }
        }))
    }

    /// Compute signature help at the given position
    pub fn compute_signature_help(
        &self,
        text: &str,
        line: usize,
        character: usize,
    ) -> Option<Value> {
        let lines: Vec<&str> = text.lines().collect();
        let current_line = lines.get(line)?;

        // Find the function call: look backward for '(' and the function name
        let before_cursor: String = current_line.chars().take(character).collect();
        let paren_pos = before_cursor.rfind('(')?;
        let before_paren = before_cursor[..paren_pos].trim_end();

        // Extract function name
        let func_name = before_paren
            .rsplit(|c: char| !c.is_alphanumeric() && c != '_')
            .next()
            .unwrap_or("");

        if func_name.is_empty() {
            return None;
        }

        // Count current argument index
        let args_in_call = before_cursor[paren_pos + 1..]
            .chars()
            .filter(|&c| c == ',')
            .count();

        // Find function signature
        let mut signatures = Vec::new();

        if let Some(file) = self.parse_with_recovery(text) {
            for item in &file.items {
                if let Item::Func(f) = item {
                    if f.name == func_name {
                        let params: Vec<String> = f
                            .params
                            .iter()
                            .map(|p| {
                                let base = format!("{}: {:?}", p.name, p.ty);
                                if let Some(ref default_expr) = p.default_value {
                                    format!("{} = {}", base, crate::lsp::LspServer::format_expr_simple(default_expr))
                                } else {
                                    base
                                }
                            })
                            .collect();
                        let ret = f
                            .ret
                            .as_ref()
                            .map(|t| format!(" -> {:?}", t))
                            .unwrap_or_default();
                        signatures.push(serde_json::json!({
                            "label": format!("func {}({}){}", func_name, params.join(", "), ret),
                            "documentation": format!("Function {}", func_name),
                            "parameters": f.params.iter().map(|p| {
                                let label = {
                                    let base = format!("{}: {:?}", p.name, p.ty);
                                    if let Some(ref default_expr) = p.default_value {
                                        format!("{} = {}", base, crate::lsp::LspServer::format_expr_simple(default_expr))
                                    } else {
                                        base
                                    }
                                };
                                serde_json::json!({
                                    "label": label,
                                    "documentation": format!("Parameter {}", p.name)
                                })
                            }).collect::<Vec<_>>()
                        }));
                    }
                }
            }
        }

        // Check builtins
        let builtin_sigs = vec![
            ("println", "println(msg: string)", vec!["msg: string"]),
            ("assert", "assert(condition: bool)", vec!["condition: bool"]),
            ("assert_eq", "assert_eq(a, b)", vec!["a", "b"]),
            ("len", "len(collection) -> i64", vec!["collection"]),
            ("range", "range(start: i64, end: i64) -> list", vec!["start: i64", "end: i64"]),
            ("push", "push(list, item) -> list", vec!["list", "item"]),
            ("pop", "pop(list) -> item", vec!["list"]),
            ("min", "min(a, b) -> a", vec!["a", "b"]),
            ("max", "max(a, b) -> a", vec!["a", "b"]),
        ];

        for (name, label, params) in builtin_sigs {
            if func_name == name {
                signatures.push(serde_json::json!({
                    "label": label,
                    "documentation": format!("Built-in function {}", name),
                    "parameters": params.iter().map(|p| serde_json::json!({
                        "label": p,
                        "documentation": ""
                    })).collect::<Vec<_>>()
                }));
            }
        }

        if signatures.is_empty() {
            return None;
        }

        Some(serde_json::json!({
            "signatures": signatures,
            "activeParameter": args_in_call
        }))
    }

    /// Compute document highlights for the symbol at the given position
    pub fn compute_document_highlight(
        &self,
        text: &str,
        line: usize,
        character: usize,
    ) -> Vec<Value> {
        let word = self.get_word_at(text, line, character);
        if word.is_empty() {
            return Vec::new();
        }

        let mut highlights = Vec::new();
        let mut def_line: Option<usize> = None;
        let mut def_col: Option<usize> = None;
        let lines: Vec<&str> = text.lines().collect();

        // Find definition location
        if let Some(file) = self.parse_with_recovery(text) {
            for item in &file.items {
                match item {
                    Item::Func(f) if f.name == word => {
                        def_line = text
                            .lines()
                            .position(|l| l.contains(&format!("func {}", word)));
                        def_col = def_line.and_then(|l| {
                            lines.get(l).map(|line| {
                                line.find(&format!("func {}", word)).unwrap_or(0) + 5
                            })
                        });
                        break;
                    }
                    Item::Type(t) if t.name == word => {
                        def_line = text
                            .lines()
                            .position(|l| l.contains(&format!("type {}", word)));
                        def_col = def_line.and_then(|l| {
                            lines.get(l).map(|line| {
                                line.find(&format!("type {}", word)).unwrap_or(0) + 5
                            })
                        });
                        break;
                    }
                    Item::Module(m) if m.name == word => {
                        def_line = text
                            .lines()
                            .position(|l| l.contains(&format!("module {}", word)));
                        def_col = def_line.and_then(|l| {
                            lines.get(l).map(|line| {
                                line.find(&format!("module {}", word)).unwrap_or(0) + 7
                            })
                        });
                        break;
                    }
                    _ => {}
                }
            }
        }

        // Add definition as Write highlight
        if let (Some(dl), Some(dc)) = (def_line, def_col) {
            highlights.push(serde_json::json!({
                "range": {
                    "start": { "line": dl, "character": dc },
                    "end": { "line": dl, "character": dc + word.len() }
                },
                "kind": 3 // Write
            }));
        }

        // Find all usages as Text highlights
        for (i, line_text) in lines.iter().enumerate() {
            let mut start = 0;
            while let Some(pos) = line_text[start..].find(word.as_str()) {
                let abs_pos = start + pos;
                let before = abs_pos > 0
                    && line_text
                        .chars()
                        .nth(abs_pos - 1)
                        .map(|c| c.is_alphanumeric() || c == '_')
                        .unwrap_or(false);
                let after = line_text
                    .chars()
                    .nth(abs_pos + word.len())
                    .map(|c| c.is_alphanumeric() || c == '_')
                    .unwrap_or(false);

                if !before && !after {
                    // Skip definition location
                    if let (Some(dl), Some(dc)) = (def_line, def_col) {
                        if i == dl && dc == abs_pos {
                            start = abs_pos + 1;
                            continue;
                        }
                    }
                    highlights.push(serde_json::json!({
                        "range": {
                            "start": { "line": i, "character": abs_pos },
                            "end": { "line": i, "character": abs_pos + word.len() }
                        },
                        "kind": 1 // Text
                    }));
                }
                start = abs_pos + 1;
            }
        }

        highlights
    }
}
