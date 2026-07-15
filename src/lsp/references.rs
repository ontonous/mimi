use serde_json::Value;

use crate::ast::{Item, Pattern, Stmt};
use crate::lsp::util::word_range_at;
use crate::lsp::LspServer;

fn byte_col_to_utf16(line: &str, byte: usize) -> usize {
    crate::lsp::position_map::PositionMap::new(line)
        .byte_to_lsp(byte.min(line.len()))
        .1
}



impl LspServer {
    pub fn compute_definition(
        &self,
        text: &str,
        line: usize,
        character: usize,
        uri: &str,
    ) -> Option<Value> {
        // Get the word at cursor position
        let (word_start, word_end) = word_range_at(text, line, character)?;
        let current_line = text.lines().nth(line)?;
        let word = &current_line[word_start..word_end];

        if word.is_empty() {
            return None;
        }

        // Try to parse and find the symbol definition
        if let Some(file) = self.parse_with_recovery(text) {
            for item in &file.items {
                match item {
                    Item::Func(f) if f.name == word => {
                        // Use AST position (line, col) of the func keyword.
                        // f.pos is 1-indexed, LSP expects 0-indexed.
                        let def_line = f.pos.0.saturating_sub(1);
                        let func_keyword_len = "func ".len();
                        return Some(serde_json::json!({
                            "uri": uri,
                            "range": {
                                "start": { "line": def_line, "character": f.pos.1 },
                                "end": { "line": def_line, "character": f.pos.1 + func_keyword_len + f.name.len() }
                            }
                        }));
                    }
                    Item::Type(t) if t.name == word => {
                        let def_line = text
                            .lines()
                            .position(|l| l.contains(&format!("type {}", word)))
                            .unwrap_or(0);
                        let keyword_len = "type ".len();
                        return Some(serde_json::json!({
                            "uri": uri,
                            "range": {
                                "start": { "line": def_line, "character": 0 },
                                "end": { "line": def_line, "character": keyword_len + t.name.len() }
                            }
                        }));
                    }
                    Item::Module(m) if m.name == word => {
                        let def_line = text
                            .lines()
                            .position(|l| l.contains(&format!("module {}", word)))
                            .unwrap_or(0);
                        let keyword_len = "module ".len();
                        return Some(serde_json::json!({
                            "uri": uri,
                            "range": {
                                "start": { "line": def_line, "character": 0 },
                                "end": { "line": def_line, "character": keyword_len + m.name.len() }
                            }
                        }));
                    }
                    _ => {}
                }
            }
            // v0.28.11: variable goto-definition: scan function bodies
            // for `let name = ...` and function parameters.
            for item in &file.items {
                if let Item::Func(f) = item {
                    // Check function parameters
                    if f.params.iter().any(|p| p.name == word) {
                        // Parameter definition is at the function signature
                        let def_line = f.pos.0.saturating_sub(1);
                        let param_offset = f
                            .params
                            .iter()
                            .take_while(|p| p.name != word)
                            .map(|p| p.name.len() + 2) // "name, " per param
                            .sum::<usize>();
                        return Some(serde_json::json!({
                            "uri": uri,
                            "range": {
                                "start": { "line": def_line, "character": f.pos.1 + param_offset },
                                "end": { "line": def_line, "character": f.pos.1 + param_offset + word.len() }
                            }
                        }));
                    }
                    // Check let bindings: find `let name =` or `let name:` via
                    // text scan (fast, avoids deep AST traversal)
                    // L-H9: restrict text scan to the enclosing function region
                    // (not whole-file first match).
                    let func_start = f.pos.0.saturating_sub(1);
                    let text_lines: Vec<&str> = text.lines().collect();
                    let func_end = text_lines
                        .iter()
                        .enumerate()
                        .skip(func_start + 1)
                        .find(|(i, l)| {
                            *i > func_start
                                && (l.starts_with("func ")
                                    || l.starts_with("type ")
                                    || l.starts_with("flow ")
                                    || l.starts_with("actor "))
                        })
                        .map(|(i, _)| i)
                        .unwrap_or(text_lines.len());
                    for stmt in f.body.iter() {
                        if let Stmt::Let {
                            pat: Pattern::Variable(name),
                            ..
                        } = stmt
                        {
                            if name == word {
                                let text_line = text_lines
                                    .iter()
                                    .enumerate()
                                    .skip(func_start)
                                    .take(func_end.saturating_sub(func_start))
                                    .find(|(_, l)| l.contains(&format!("let {}", name)))
                                    .map(|(i, _)| i)
                                    .unwrap_or(func_start);
                                let line_text = text_lines.get(text_line).copied().unwrap_or("");
                                let byte = line_text.find(&format!("let {}", name)).unwrap_or(0);
                                let start_u = byte_col_to_utf16(line_text, byte + 4);
                                let end_u = byte_col_to_utf16(line_text, byte + 4 + name.len());
                                return Some(serde_json::json!({
                                    "uri": uri,
                                    "range": {
                                        "start": { "line": text_line, "character": start_u },
                                        "end": { "line": text_line, "character": end_u }
                                    }
                                }));
                            }
                        }
                    }
                }
            }
        }

        // Builtins don't have definitions in user code
        None
    }

    /// Go to implementation for a trait name: find all `impl` blocks for this trait
    /// across all open documents in the workspace.
    pub fn compute_go_to_implementation(
        &self,
        text: &str,
        line: usize,
        character: usize,
        _uri: &str,
    ) -> Vec<Value> {
        let word = self.get_word_at(text, line, character);
        if word.is_empty() {
            return Vec::new();
        }

        let mut locations = Vec::new();

        // Check if word is a trait name in the current file
        let is_trait = if let Some(file) = self.parse_with_recovery(text) {
            file.items
                .iter()
                .any(|item| matches!(item, Item::Trait(t) if t.name == word))
        } else {
            false
        };
        if !is_trait {
            return locations; // Not a trait — no implementations to find
        }

        // Search across all open documents for impl blocks
        for (doc_uri, doc_text) in &self.documents {
            if let Some(file) = self.parse_with_recovery(doc_text) {
                for impl_def in &file.items {
                    if let Item::Impl(imp) = impl_def {
                        if imp.trait_name == word {
                            let impl_line = doc_text
                                .lines()
                                .position(|l| l.contains("impl") && l.contains(&word))
                                .unwrap_or(0);
                            locations.push(serde_json::json!({
                                "uri": doc_uri,
                                "range": {
                                    "start": { "line": impl_line, "character": 0 },
                                    "end": { "line": impl_line, "character": 100 }
                                }
                            }));
                        }
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
            // CL-H1 (audit): prefer AST positions over text search when the
            // AST is available. The audit notes this is a broad LSP rewrite;
            // here we at least consume the parsed AST position when present
            // and fall back to text search only for legacy/unparsed sources.
            for item in &file.items {
                match item {
                    Item::Func(f) if f.name == word => {
                        // CL-H1: use the AST-recorded position (f.pos) when
                        // available to avoid substring-search false positives.
                        // Parser positions are 1-indexed; LSP is 0-indexed.
                        if f.pos.0 > 0 || f.pos.1 > 0 {
                            def_line = Some(f.pos.0.saturating_sub(1));
                            def_col = Some(f.pos.1.saturating_sub(1));
                            break;
                        }
                        def_line = text
                            .lines()
                            .position(|l| l.contains(&format!("func {}", word)));
                        def_col = def_line.and_then(|l| {
                            lines
                                .get(l)
                                .map(|line| line.find(&format!("func {}", word)).unwrap_or(0) + 5)
                        });
                        break;
                    }
                    Item::Type(t) if t.name == word => {
                        // CL-H1: TypeDef doesn't carry pos in the AST; fall
                        // back to text search. Future v0.31+ may extend
                        // TypeDef with a pos field to enable AST-based lookup.
                        def_line = text
                            .lines()
                            .position(|l| l.contains(&format!("type {}", word)));
                        def_col = def_line.and_then(|l| {
                            lines
                                .get(l)
                                .map(|line| line.find(&format!("type {}", word)).unwrap_or(0) + 5)
                        });
                        break;
                    }
                    Item::Module(m) if m.name == word => {
                        // Module doesn't carry a pos in current AST; fall back.
                        def_line = text
                            .lines()
                            .position(|l| l.contains(&format!("module {}", word)));
                        def_col = def_line.and_then(|l| {
                            lines
                                .get(l)
                                .map(|line| line.find(&format!("module {}", word)).unwrap_or(0) + 7)
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
                            "start": { "line": i, "character": byte_col_to_utf16(line_text, abs_pos) },
                            "end": { "line": i, "character": byte_col_to_utf16(line_text, abs_pos + word.len()) }
                        }
                    }));
                }
                start = abs_pos + 1;
            }
        }

        references
    }

    /// Rename all occurrences of the symbol at the given position.
    /// v0.28.11: scope-aware — only renames local variables (let bindings
    /// and function parameters), avoiding false matches on global symbols
    /// with the same name.
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

        // v0.28.11: Check whether the word at cursor is a local variable
        // (let binding or function parameter).  Only local variables get
        // renamed; global symbols (func/type/module names) are skipped to
        // avoid false positives.
        let is_local = if let Some(file) = self.parse_with_recovery(text) {
            let mut found = false;
            for item in &file.items {
                if let Item::Func(f) = item {
                    if f.params.iter().any(|p| p.name == word) {
                        found = true;
                    }
                    for stmt in &f.body {
                        if let Stmt::Let {
                            pat: Pattern::Variable(ref vname),
                            ..
                        } = stmt
                        {
                            if vname.as_str() == word {
                                found = true;
                            }
                        }
                    }
                }
            }
            found
        } else {
            false
        };

        if !is_local {
            return None;
        }

        // L-H2: only rename within the enclosing function of the cursor
        // (not whole-file text replace of every occurrence).
        let lines: Vec<&str> = text.lines().collect();
        let (range_start, range_end) = enclosing_func_line_range(text, line).unwrap_or((0, lines.len()));

        let mut changes = Vec::new();
        for (i, line_text) in lines.iter().enumerate().take(range_end).skip(range_start) {
            let mut start = 0;
            while let Some(pos) = line_text[start..].find(word.as_str()) {
                let abs_pos = start + pos;
                // Word-boundary check in char space.
                let before_ok = !line_text[..abs_pos]
                    .chars()
                    .next_back()
                    .map(|c| c.is_alphanumeric() || c == '_')
                    .unwrap_or(false);
                let after_ok = !line_text[abs_pos + word.len()..]
                    .chars()
                    .next()
                    .map(|c| c.is_alphanumeric() || c == '_')
                    .unwrap_or(false);

                if before_ok && after_ok {
                    // L-H1: LSP ranges use UTF-16 code units, not bytes.
                    let map = crate::lsp::position_map::PositionMap::new(line_text);
                    let start_utf16 = map.byte_to_lsp(abs_pos).1;
                    let end_utf16 = map.byte_to_lsp(abs_pos + word.len()).1;
                    changes.push(serde_json::json!({
                        "range": {
                            "start": { "line": i, "character": start_utf16 },
                            "end": { "line": i, "character": end_utf16 }
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
        &mut self,
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
            // Search user-defined functions
            for item in &file.items {
                if let Item::Func(f) = item {
                    if f.name == func_name {
                        let params: Vec<String> = f
                            .params
                            .iter()
                            .map(|p| {
                                let base = format!("{}: {}", p.name, crate::core::fmt_type(&p.ty));
                                if let Some(ref default_expr) = p.default_value {
                                    format!(
                                        "{} = {}",
                                        base,
                                        crate::lsp::LspServer::format_expr_simple(default_expr)
                                    )
                                } else {
                                    base
                                }
                            })
                            .collect();
                        let ret = f
                            .ret
                            .as_ref()
                            .map(|t| format!(" -> {}", crate::core::fmt_type(t)))
                            .unwrap_or_default();
                        signatures.push(serde_json::json!({
                            "label": format!("func {}({}){}", func_name, params.join(", "), ret),
                            "documentation": format!("Function {}", func_name),
                            "parameters": f.params.iter().map(|p| {
                                let label = {
                                    let base = format!("{}: {}", p.name, crate::core::fmt_type(&p.ty));
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

            // Search trait methods
            for item in &file.items {
                match item {
                    Item::Trait(t) => {
                        for m in &t.methods {
                            if m.name == func_name {
                                let params: Vec<String> = m
                                    .params
                                    .iter()
                                    .map(|p| {
                                        format!("{}: {}", p.name, crate::core::fmt_type(&p.ty))
                                    })
                                    .collect();
                                let ret = m
                                    .ret
                                    .as_ref()
                                    .map(|t| format!(" -> {}", crate::core::fmt_type(t)))
                                    .unwrap_or_default();
                                signatures.push(serde_json::json!({
                                    "label": format!("{}.{}({}){}", t.name, func_name, params.join(", "), ret),
                                    "documentation": format!("Trait method {}.{}", t.name, func_name),
                                    "parameters": m.params.iter().map(|p| {
                                        serde_json::json!({
                                            "label": format!("{}: {}", p.name, crate::core::fmt_type(&p.ty)),
                                            "documentation": format!("Parameter {}", p.name)
                                        })
                                    }).collect::<Vec<_>>()
                                }));
                            }
                        }
                    }
                    Item::Impl(imp) => {
                        for m in &imp.methods {
                            if m.name == func_name {
                                let params: Vec<String> = m
                                    .params
                                    .iter()
                                    .map(|p| {
                                        format!("{}: {}", p.name, crate::core::fmt_type(&p.ty))
                                    })
                                    .collect();
                                let ret = m
                                    .ret
                                    .as_ref()
                                    .map(|t| format!(" -> {}", crate::core::fmt_type(t)))
                                    .unwrap_or_default();
                                signatures.push(serde_json::json!({
                                    "label": format!("{}.{}({}){}", imp.type_name, func_name, params.join(", "), ret),
                                    "documentation": format!("Impl method {}.{}", imp.type_name, func_name),
                                    "parameters": m.params.iter().map(|p| {
                                        serde_json::json!({
                                            "label": format!("{}: {}", p.name, crate::core::fmt_type(&p.ty)),
                                            "documentation": format!("Parameter {}", p.name)
                                        })
                                    }).collect::<Vec<_>>()
                                }));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // Check stdlib functions (need to load first)
        self.load_stdlib_completions();
        for (module_name, funcs) in &self.stdlib_funcs {
            for (name, detail, _insert) in funcs {
                if name.as_str() == func_name {
                    signatures.push(serde_json::json!({
                        "label": format!("{}.{}", module_name, detail),
                        "documentation": format!("Stdlib function {}.{}", module_name, func_name),
                        "parameters": Vec::<serde_json::Value>::new()
                    }));
                }
            }
        }

        // Check builtins
        let builtin_sigs = vec![
            ("println", "println(msg: string)", vec!["msg: string"]),
            ("assert", "assert(condition: bool)", vec!["condition: bool"]),
            ("assert_eq", "assert_eq(a, b)", vec!["a", "b"]),
            ("len", "len(collection) -> i64", vec!["collection"]),
            (
                "range",
                "range(start: i64, end: i64) -> list",
                vec!["start: i64", "end: i64"],
            ),
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
            // CL-H1 (audit): prefer AST positions over text search when the
            // AST is available. The audit notes this is a broad LSP rewrite;
            // here we at least consume the parsed AST position when present
            // and fall back to text search only for legacy/unparsed sources.
            for item in &file.items {
                match item {
                    Item::Func(f) if f.name == word => {
                        // CL-H1: use the AST-recorded position (f.pos) when
                        // available to avoid substring-search false positives.
                        // Parser positions are 1-indexed; LSP is 0-indexed.
                        if f.pos.0 > 0 || f.pos.1 > 0 {
                            def_line = Some(f.pos.0.saturating_sub(1));
                            def_col = Some(f.pos.1.saturating_sub(1));
                            break;
                        }
                        def_line = text
                            .lines()
                            .position(|l| l.contains(&format!("func {}", word)));
                        def_col = def_line.and_then(|l| {
                            lines
                                .get(l)
                                .map(|line| line.find(&format!("func {}", word)).unwrap_or(0) + 5)
                        });
                        break;
                    }
                    Item::Type(t) if t.name == word => {
                        // CL-H1: TypeDef doesn't carry pos in the AST; fall
                        // back to text search. Future v0.31+ may extend
                        // TypeDef with a pos field to enable AST-based lookup.
                        def_line = text
                            .lines()
                            .position(|l| l.contains(&format!("type {}", word)));
                        def_col = def_line.and_then(|l| {
                            lines
                                .get(l)
                                .map(|line| line.find(&format!("type {}", word)).unwrap_or(0) + 5)
                        });
                        break;
                    }
                    Item::Module(m) if m.name == word => {
                        // Module doesn't carry a pos in current AST; fall back.
                        def_line = text
                            .lines()
                            .position(|l| l.contains(&format!("module {}", word)));
                        def_col = def_line.and_then(|l| {
                            lines
                                .get(l)
                                .map(|line| line.find(&format!("module {}", word)).unwrap_or(0) + 7)
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
                            "start": { "line": i, "character": byte_col_to_utf16(line_text, abs_pos) },
                            "end": { "line": i, "character": byte_col_to_utf16(line_text, abs_pos + word.len()) }
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


/// Approximate line range of the function containing `cursor_line` (0-based).
fn enclosing_func_line_range(text: &str, cursor_line: usize) -> Option<(usize, usize)> {
    let lines: Vec<&str> = text.lines().collect();
    // Walk upward for `func` / `fn` starter.
    let mut start = None;
    for i in (0..=cursor_line.min(lines.len().saturating_sub(1))).rev() {
        let t = lines[i].trim_start();
        if t.starts_with("func ") || t.starts_with("fn ") {
            start = Some(i);
            break;
        }
    }
    let start = start?;
    // Walk downward until next top-level-ish func or end.
    let mut end = lines.len();
    for i in (start + 1)..lines.len() {
        let t = lines[i].trim_start();
        if (t.starts_with("func ") || t.starts_with("fn ") || t.starts_with("type ") || t.starts_with("flow "))
            && !t.starts_with("func main")
            && i > cursor_line
        {
            // Only cut if this looks like a new top-level item at column 0.
            if lines[i].starts_with("func ")
                || lines[i].starts_with("fn ")
                || lines[i].starts_with("type ")
                || lines[i].starts_with("flow ")
            {
                end = i;
                break;
            }
        }
    }
    Some((start, end))
}
