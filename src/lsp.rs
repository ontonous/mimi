use crate::{core, lexer, parser, fmt};
use crate::ast::{Item, Expr, Stmt, FuncDef, TypeDef, TypeDefKind, Type};
use std::collections::HashMap;
use std::io::{self, BufRead, Read, Write};

const MAX_CONTENT_LENGTH: usize = 16 * 1024 * 1024; // 16MB

/// LSP server for Mimi language
pub struct LspServer {
    documents: HashMap<String, String>,
}

impl LspServer {
    pub fn new() -> Self {
        LspServer {
            documents: HashMap::new(),
        }
    }

    /// Run the LSP server (stdin/stdout JSON-RPC)
    pub fn run(&mut self) -> Result<(), String> {
        let stdin = io::stdin();
        let mut reader = stdin.lock();
        let mut buffer = String::new();

        loop {
            buffer.clear();
            // Read Content-Length header
            let mut header = String::new();
            loop {
                header.clear();
                if reader.read_line(&mut header).is_err() || header.is_empty() {
                    return Ok(());
                }
                if header.starts_with("Content-Length:") {
                    break;
                }
            }

            let len: usize = header.trim()
                .strip_prefix("Content-Length: ")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);

            if len == 0 || len > MAX_CONTENT_LENGTH {
                continue;
            }

            // Read JSON body
            let mut body = vec![0u8; len];
            reader.read_exact(&mut body).map_err(|e| format!("read error: {}", e))?;
            let body = String::from_utf8(body).map_err(|e| format!("utf8 error: {}", e))?;

            // Skip empty line after body
            let mut newline = [0u8; 1];
            let _ = io::stdin().read(&mut newline);

            // Parse and handle
            if let Ok(msg) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(response) = self.handle_message(&msg) {
                    let resp_str = serde_json::to_string(&response).unwrap_or_default();
                    print!("Content-Length: {}\r\n\r\n{}", resp_str.len(), resp_str);
                    io::stdout().flush().ok();
                }
            }
        }
    }

    pub(crate) fn handle_message(&mut self, msg: &serde_json::Value) -> Option<serde_json::Value> {
        let method = msg.get("method")?.as_str()?;
        let id = msg.get("id");

        match method {
            "initialize" => {
                let result = serde_json::json!({
                    "capabilities": {
                        "textDocumentSync": {
                            "openClose": true,
                            "change": 1,
                            "save": {
                                "includeText": false
                            }
                        },
                        "completionProvider": {
                            "triggerCharacters": [".", ":"]
                        },
                        "hoverProvider": true,
                        "definitionProvider": true,
                        "implementationProvider": true,
                        "referencesProvider": true,
                        "renameProvider": {
                            "prepareProvider": true
                        },
                        "signatureHelpProvider": {
                            "triggerCharacters": ["("]
                        },
                        "semanticTokensProvider": {
                            "legend": {
                                "tokenTypes": ["keyword", "function", "type", "variable", "number", "string", "comment", "operator"],
                                "tokenModifiers": ["declaration", "definition"]
                            },
                            "full": true
                        },
                        "diagnosticProvider": {
                            "interFileDependencies": false,
                            "workspaceDiagnostics": false
                        },
                        "foldingRangeProvider": true,
                        "documentFormattingProvider": true,
                        "documentHighlightProvider": true,
                        "inlayHintProvider": true
                    }
                });
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result
                }))
            }
            "initialized" => None,
            "textDocument/didOpen" => {
                let uri = msg.get("params")?
                    .get("textDocument")?
                    .get("uri")?
                    .as_str()?;
                let text = msg.get("params")?
                    .get("textDocument")?
                    .get("text")?
                    .as_str()?;
                self.documents.insert(uri.to_string(), text.to_string());
                if self.documents.len() > 256 {
                    if let Some(oldest_key) = self.documents.keys().next().cloned() {
                        self.documents.remove(&oldest_key);
                    }
                }
                // Publish diagnostics
                let diagnostics = self.compute_diagnostics(text);
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/publishDiagnostics",
                    "params": {
                        "uri": uri,
                        "diagnostics": diagnostics
                    }
                }))
            }
            "textDocument/didChange" => {
                let uri = msg.get("params")?
                    .get("textDocument")?
                    .get("uri")?
                    .as_str()?;
                let text = msg.get("params")?
                    .get("contentChanges")?
                    .as_array()?
                    .first()?
                    .get("text")?
                    .as_str()?;
                self.documents.insert(uri.to_string(), text.to_string());
                if self.documents.len() > 256 {
                    if let Some(oldest_key) = self.documents.keys().next().cloned() {
                        self.documents.remove(&oldest_key);
                    }
                }
                let diagnostics = self.compute_diagnostics(text);
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/publishDiagnostics",
                    "params": {
                        "uri": uri,
                        "diagnostics": diagnostics
                    }
                }))
            }
            "textDocument/didClose" => {
                let uri = msg.get("params")?
                    .get("textDocument")?
                    .get("uri")?
                    .as_str()?;
                self.documents.remove(uri);
                None
            }
            "textDocument/didSave" => {
                let uri = msg.get("params")?
                    .get("textDocument")?
                    .get("uri")?
                    .as_str()?;
                if let Some(text) = self.documents.get(uri) {
                    let diagnostics = self.compute_diagnostics(text);
                    Some(serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "textDocument/publishDiagnostics",
                        "params": {
                            "uri": uri,
                            "diagnostics": diagnostics
                        }
                    }))
                } else {
                    None
                }
            }
            "textDocument/completion" => {
                let uri = msg.get("params")?
                    .get("textDocument")?
                    .get("uri")?
                    .as_str()?;
                let text = self.documents.get(uri)?;
                let items = self.compute_completion(text);
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "isIncomplete": false,
                        "items": items
                    }
                }))
            }
            "textDocument/hover" => {
                let uri = msg.get("params")?
                    .get("textDocument")?
                    .get("uri")?
                    .as_str()?;
                let position = msg.get("params")?
                    .get("position")?;
                let line = position.get("line")?.as_u64()? as usize;
                let character = position.get("character")?.as_u64()? as usize;
                let text = self.documents.get(uri)?;
                let hover = self.compute_hover(text, line, character);
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": hover
                }))
            }
            "textDocument/definition" => {
                let uri = msg.get("params")?
                    .get("textDocument")?
                    .get("uri")?
                    .as_str()?;
                let position = msg.get("params")?
                    .get("position")?;
                let line = position.get("line")?.as_u64()? as usize;
                let character = position.get("character")?.as_u64()? as usize;
                let text = self.documents.get(uri)?;
                let definition = self.compute_definition(text, line, character, uri);
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": definition
                }))
            }
            "textDocument/implementation" => {
                let uri = msg.get("params")?
                    .get("textDocument")?
                    .get("uri")?
                    .as_str()?;
                let position = msg.get("params")?
                    .get("position")?;
                let line = position.get("line")?.as_u64()? as usize;
                let character = position.get("character")?.as_u64()? as usize;
                let text = self.documents.get(uri)?;
                let impls = self.compute_go_to_implementation(text, line, character, uri);
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": impls
                }))
            }
            "textDocument/documentSymbol" => {
                let uri = msg.get("params")?
                    .get("textDocument")?
                    .get("uri")?
                    .as_str()?;
                let text = self.documents.get(uri)?;
                let symbols = self.compute_document_symbols(text);
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": symbols
                }))
            }
            "textDocument/references" => {
                let uri = msg.get("params")?
                    .get("textDocument")?
                    .get("uri")?
                    .as_str()?;
                let position = msg.get("params")?
                    .get("position")?;
                let line = position.get("line")?.as_u64()? as usize;
                let character = position.get("character")?.as_u64()? as usize;
                let text = self.documents.get(uri)?;
                let include_decl = msg.get("params")?
                    .get("context")?
                    .get("includeDeclaration")?
                    .as_bool()
                    .unwrap_or(true);
                let references = self.compute_references(text, line, character, uri, include_decl);
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": references
                }))
            }
            "textDocument/prepareRename" => {
                let uri = msg.get("params")?
                    .get("textDocument")?
                    .get("uri")?
                    .as_str()?;
                let position = msg.get("params")?
                    .get("position")?;
                let line = position.get("line")?.as_u64()? as usize;
                let character = position.get("character")?.as_u64()? as usize;
                let text = self.documents.get(uri)?;
                let word = self.get_word_at(text, line, character);
                if word.is_empty() {
                    return None;
                }
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "start": { "line": line, "character": self.word_start_col(text, line, character) },
                        "end": { "line": line, "character": character + self.word_end_offset(text, line, character) }
                    }
                }))
            }
            "textDocument/rename" => {
                let uri = msg.get("params")?
                    .get("textDocument")?
                    .get("uri")?
                    .as_str()?;
                let position = msg.get("params")?
                    .get("position")?;
                let line = position.get("line")?.as_u64()? as usize;
                let character = position.get("character")?.as_u64()? as usize;
                let new_name = msg.get("params")?
                    .get("newName")?
                    .as_str()?;
                let text = self.documents.get(uri)?;
                let workspace_edit = self.compute_rename(text, line, character, uri, new_name);
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": workspace_edit
                }))
            }
            "textDocument/signatureHelp" => {
                let uri = msg.get("params")?
                    .get("textDocument")?
                    .get("uri")?
                    .as_str()?;
                let position = msg.get("params")?
                    .get("position")?;
                let line = position.get("line")?.as_u64()? as usize;
                let character = position.get("character")?.as_u64()? as usize;
                let text = self.documents.get(uri)?;
                let sig_help = self.compute_signature_help(text, line, character);
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": sig_help
                }))
            }
            "textDocument/semanticTokens/full" => {
                let uri = msg.get("params")?
                    .get("textDocument")?
                    .get("uri")?
                    .as_str()?;
                let text = self.documents.get(uri)?;
                let tokens = self.compute_semantic_tokens(text);
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "data": tokens
                    }
                }))
            }
            "textDocument/foldingRange" => {
                let uri = msg.get("params")?
                    .get("textDocument")?
                    .get("uri")?
                    .as_str()?;
                let text = self.documents.get(uri)?;
                let ranges = self.compute_folding_ranges(text);
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": ranges
                }))
            }
            "textDocument/formatting" => {
                let uri = msg.get("params")?
                    .get("textDocument")?
                    .get("uri")?
                    .as_str()?;
                let text = self.documents.get(uri)?;
                let formatted = fmt::Formatter::new().format(text);
                let line_count = text.lines().count();
                let last_line_len = text.lines().last().map(|l| l.len()).unwrap_or(0);
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": [{
                        "range": {
                            "start": { "line": 0, "character": 0 },
                            "end": { "line": (line_count - 1).max(0), "character": last_line_len }
                        },
                        "newText": formatted
                    }]
                }))
            }
            "textDocument/documentHighlight" => {
                let uri = msg.get("params")?
                    .get("textDocument")?
                    .get("uri")?
                    .as_str()?;
                let position = msg.get("params")?
                    .get("position")?;
                let line = position.get("line")?.as_u64()? as usize;
                let character = position.get("character")?.as_u64()? as usize;
                let text = self.documents.get(uri)?;
                let highlights = self.compute_document_highlight(text, line, character);
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": highlights
                }))
            }
            "textDocument/inlayHint" => {
                let uri = msg.get("params")?
                    .get("textDocument")?
                    .get("uri")?
                    .as_str()?;
                let text = self.documents.get(uri)?;
                let hints = self.compute_inlay_hints(text);
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": hints
                }))
            }
            "shutdown" => {
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": null
                }))
            }
            "exit" => {
                // LSP spec: exit notification means server should terminate.
                // Flush buffers before exiting to ensure log output is written.
                let _ = std::io::stdout().flush();
                let _ = std::io::stderr().flush();
                std::process::exit(0)
            }
            _ => None,
        }
    }

    pub fn compute_diagnostics(&self, text: &str) -> Vec<serde_json::Value> {
        let mut diagnostics = Vec::new();

        // Parse
        let tokens = match lexer::Lexer::new(text).tokenize() {
            Ok(t) => t,
            Err(e) => {
                diagnostics.push(serde_json::json!({
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 0, "character": 100 }
                    },
                    "severity": 1,
                    "message": e
                }));
                return diagnostics;
            }
        };

        // Use recovery parser to get partial AST + all parse errors
        let (file, parse_errors) = parser::Parser::new(tokens).parse_file_with_recovery();

        // Report all parse errors
        for e in &parse_errors {
            diagnostics.push(serde_json::json!({
                "range": {
                    "start": { "line": e.line.saturating_sub(1), "character": e.col.saturating_sub(1) },
                    "end": { "line": e.line.saturating_sub(1), "character": e.col }
                },
                "severity": 1,
                "source": "mimi",
                "message": e.message
            }));
        }

        // Type check the partial AST (even if parse had errors)
        if let Err(errs) = core::check(&file) {
            for err in errs {
                let severity = match err.severity {
                    crate::diagnostic::Severity::Error => 1,
                    crate::diagnostic::Severity::Warning => 2,
                    crate::diagnostic::Severity::Note => 3,
                    crate::diagnostic::Severity::Help => 4,
                };
                diagnostics.push(serde_json::json!({
                    "range": {
                        "start": { "line": err.span.start_line.saturating_sub(1), "character": err.span.start_col.saturating_sub(1) },
                        "end": { "line": err.span.end_line.saturating_sub(1), "character": err.span.end_col.saturating_sub(1) }
                    },
                    "severity": severity,
                    "source": "mimi",
                    "message": err.message
                }));
            }
        }

        diagnostics
    }

    /// Parse text with error recovery, returning partial AST even on errors
    fn parse_with_recovery(&self, text: &str) -> Option<crate::ast::File> {
        let tokens = lexer::Lexer::new(text).tokenize().ok()?;
        let (file, _errors) = parser::Parser::new(tokens).parse_file_with_recovery();
        Some(file)
    }

    /// Compute folding ranges based on brace matching and indentation
    pub fn compute_folding_ranges(&self, text: &str) -> Vec<serde_json::Value> {
        let mut ranges = Vec::new();
        let mut brace_stack: Vec<usize> = Vec::new();

        for (line_idx, line) in text.lines().enumerate() {
            for (_ch_idx, ch) in line.char_indices() {
                match ch {
                    '{' | '(' | '[' => {
                        brace_stack.push(line_idx);
                    }
                    '}' | ')' | ']' => {
                        if let Some(start_line) = brace_stack.pop() {
                            if start_line < line_idx {
                                ranges.push(serde_json::json!({
                                    "startLine": start_line,
                                    "endLine": line_idx
                                }));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        ranges
    }

    pub fn compute_document_symbols(&self, text: &str) -> Vec<serde_json::Value> {
        let mut symbols = Vec::new();

        if let Some(file) = self.parse_with_recovery(text) {
                for item in &file.items {
                    match item {
                        Item::Func(f) => {
                            // Find the line where the function is defined
                            let def_line = text.lines().position(|l| l.contains(&format!("func {}", f.name))).unwrap_or(0);
                            symbols.push(serde_json::json!({
                                "name": f.name,
                                "kind": 12, // Function
                                "range": {
                                    "start": { "line": def_line, "character": 0 },
                                    "end": { "line": def_line, "character": 100 }
                                },
                                "selectionRange": {
                                    "start": { "line": def_line, "character": 5 },
                                    "end": { "line": def_line, "character": 5 + f.name.len() }
                                }
                            }));
                        }
                        Item::Type(t) => {
                            let def_line = text.lines().position(|l| l.contains(&format!("type {}", t.name))).unwrap_or(0);
                            symbols.push(serde_json::json!({
                                "name": t.name,
                                "kind": 26, // Enum
                                "range": {
                                    "start": { "line": def_line, "character": 0 },
                                    "end": { "line": def_line, "character": 100 }
                                },
                                "selectionRange": {
                                    "start": { "line": def_line, "character": 5 },
                                    "end": { "line": def_line, "character": 5 + t.name.len() }
                                }
                            }));
                        }
                        Item::Module(m) => {
                            let def_line = text.lines().position(|l| l.contains(&format!("module {}", m.name))).unwrap_or(0);
                            symbols.push(serde_json::json!({
                                "name": m.name,
                                "kind": 1, // Module
                                "range": {
                                    "start": { "line": def_line, "character": 0 },
                                    "end": { "line": def_line, "character": 100 }
                                },
                                "selectionRange": {
                                    "start": { "line": def_line, "character": 7 },
                                    "end": { "line": def_line, "character": 7 + m.name.len() }
                                }
                            }));
                        }
                        _ => {}
                    }
                }
        }

        symbols
    }

    pub fn compute_definition(&self, text: &str, line: usize, character: usize, uri: &str) -> Option<serde_json::Value> {
        // Get the word at cursor position
        let lines: Vec<&str> = text.lines().collect();
        let current_line = lines.get(line)?;
        let before_cursor: String = current_line.chars().take(character).collect();
        let after_cursor: String = current_line.chars().skip(character).collect();

        // Find word boundaries
        let word_start = before_cursor.rfind(|c: char| !c.is_alphanumeric() && c != '_').map(|i| i + 1).unwrap_or(0);
        let word_end = after_cursor.find(|c: char| !c.is_alphanumeric() && c != '_').map(|i| character + i).unwrap_or(current_line.len());
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
                            let def_line = text.lines().position(|l| l.contains(&format!("func {}", word))).unwrap_or(0);
                            return Some(serde_json::json!({
                                "uri": uri,
                                "range": {
                                    "start": { "line": def_line, "character": 0 },
                                    "end": { "line": def_line, "character": 100 }
                                }
                            }));
                        }
                        Item::Type(t) if t.name == word => {
                            let def_line = text.lines().position(|l| l.contains(&format!("type {}", word))).unwrap_or(0);
                            return Some(serde_json::json!({
                                "uri": uri,
                                "range": {
                                    "start": { "line": def_line, "character": 0 },
                                    "end": { "line": def_line, "character": 100 }
                                }
                            }));
                        }
                        Item::Module(m) if m.name == word => {
                            let def_line = text.lines().position(|l| l.contains(&format!("module {}", word))).unwrap_or(0);
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
    pub fn compute_go_to_implementation(&self, text: &str, line: usize, character: usize, uri: &str) -> Vec<serde_json::Value> {
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
                        let impl_line = text.lines().position(|l| {
                            l.contains("impl") && l.contains(&word)
                        }).unwrap_or(0);
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

    pub fn compute_hover(&self, text: &str, line: usize, character: usize) -> Option<serde_json::Value> {
        // Get the word at cursor position
        let lines: Vec<&str> = text.lines().collect();
        let current_line = lines.get(line)?;
        let before_cursor: String = current_line.chars().take(character).collect();
        let after_cursor: String = current_line.chars().skip(character).collect();

        // Find word boundaries
        let word_start = before_cursor.rfind(|c: char| !c.is_alphanumeric() && c != '_').map(|i| i + 1).unwrap_or(0);
        let word_end = after_cursor.find(|c: char| !c.is_alphanumeric() && c != '_').map(|i| character + i).unwrap_or(current_line.len());
        let word = &current_line[word_start..word_end];

        if word.is_empty() {
            return None;
        }

        // Try to parse and find the symbol
        if let Some(file) = self.parse_with_recovery(text) {
                for item in &file.items {
                    match item {
                        Item::Func(f) if f.name == word => {
                            let params: Vec<String> = f.params.iter()
                                .map(|p| format!("{}: {}", p.name, Self::type_display(&p.ty)))
                                .collect();
                            let ret = f.ret.as_ref().map(|t| format!(" -> {}", Self::type_display(t))).unwrap_or_default();
                            let generics = if f.generics.is_empty() {
                                String::new()
                            } else {
                                let g: Vec<&str> = f.generics.iter().map(|g| g.name.as_str()).collect();
                                format!("[{}]", g.join(", "))
                            };
                            return Some(serde_json::json!({
                                "contents": {
                                    "kind": "markdown",
                                    "value": format!("**func** `{}{}({}){}`", word, generics, params.join(", "), ret)
                                }
                            }));
                        }
                        Item::Type(t) if t.name == word => {
                            let mut detail = format!("**type** `{}`", word);
                            match &t.kind {
                                TypeDefKind::Record(fields) => {
                                    if !fields.is_empty() {
                                        let field_strs: Vec<String> = fields.iter()
                                            .map(|f| format!("  `{}: {}`", f.name, Self::type_display(&f.ty)))
                                            .collect();
                                        detail.push_str("\n\nFields:\n");
                                        detail.push_str(&field_strs.join("\n"));
                                    }
                                }
                                TypeDefKind::Enum(variants) => {
                                    if !variants.is_empty() {
                                        let var_strs: Vec<String> = variants.iter()
                                            .map(|v| format!("  `{}`", v.name))
                                            .collect();
                                        detail.push_str("\n\nVariants:\n");
                                        detail.push_str(&var_strs.join("\n"));
                                    }
                                }
                                TypeDefKind::Alias(inner) => {
                                    detail.push_str(&format!(" = {}", Self::type_display(inner)));
                                }
                                TypeDefKind::Newtype(inner) => {
                                    detail.push_str(&format!(" (newtype over {})", Self::type_display(inner)));
                                }
                                TypeDefKind::Union(fields) => {
                                    if !fields.is_empty() {
                                        let field_strs: Vec<String> = fields.iter()
                                            .map(|f| format!("  `{}: {}`", f.name, Self::type_display(&f.ty)))
                                            .collect();
                                        detail.push_str("\n\nUnion fields:\n");
                                        detail.push_str(&field_strs.join("\n"));
                                    }
                                }
                            }
                            return Some(serde_json::json!({
                                "contents": {
                                    "kind": "markdown",
                                    "value": detail
                                }
                            }));
                        }
                        Item::Trait(t) if t.name == word => {
                            let methods: Vec<String> = t.methods.iter()
                                .map(|m| {
                                    let params: Vec<String> = m.params.iter()
                                        .map(|p| format!("{}: {}", p.name, Self::type_display(&p.ty)))
                                        .collect();
                                    let ret = m.ret.as_ref().map(|r| format!(" -> {}", Self::type_display(r))).unwrap_or_default();
                                    format!("  `fn {}({}){}`", m.name, params.join(", "), ret)
                                })
                                .collect();
                            let detail = if methods.is_empty() {
                                format!("**trait** `{}`", word)
                            } else {
                                format!("**trait** `{}`\n\nMethods:\n{}", word, methods.join("\n"))
                            };
                            return Some(serde_json::json!({
                                "contents": {
                                    "kind": "markdown",
                                    "value": detail
                                }
                            }));
                        }
                        Item::Impl(imp) if imp.type_name == word => {
                            let methods: Vec<String> = imp.methods.iter()
                                .map(|m| format!("  `fn {}(...)`", m.name))
                                .collect();
                            let detail = if methods.is_empty() {
                                format!("**impl** `{} for {}`", imp.trait_name, imp.type_name)
                            } else {
                                format!("**impl** `{} for {}`\n\nMethods:\n{}", imp.trait_name, imp.type_name, methods.join("\n"))
                            };
                            return Some(serde_json::json!({
                                "contents": {
                                    "kind": "markdown",
                                    "value": detail
                                }
                            }));
                        }
                        Item::Module(m) if m.name == word => {
                            let item_count = m.items.len();
                            return Some(serde_json::json!({
                                "contents": {
                                    "kind": "markdown",
                                    "value": format!("**module** `{}` ({} items)", word, item_count)
                                }
                            }));
                        }
                        Item::Actor(a) if a.name == word => {
                            let method_names: Vec<&str> = a.methods.iter().map(|m| m.name.as_str()).collect();
                            return Some(serde_json::json!({
                                "contents": {
                                    "kind": "markdown",
                                    "value": format!("**actor** `{}`\n\nMethods: {}", word, method_names.join(", "))
                                }
                            }));
                        }
                        _ => {}
                    }
                }
        }

        // Check builtins
        let builtins = vec![
            ("println", "fn println(args...)"),
            ("assert", "fn assert(condition: bool)"),
            ("assert_eq", "fn assert_eq(a, b)"),
            ("len", "fn len(collection) -> i64"),
            ("push", "fn push(list, item)"),
            ("pop", "fn pop(list) -> item"),
            ("range", "fn range(n) -> list"),
            ("sqrt", "fn sqrt(x: f64) -> f64"),
            ("abs", "fn abs(x) -> x"),
            ("min", "fn min(a, b) -> a"),
            ("max", "fn max(a, b) -> a"),
            ("to_string", "fn to_string(val) -> string"),
            ("print", "fn print(args...)"),
            ("pow", "fn pow(base, exp) -> result"),
            ("floor", "fn floor(x: f64) -> i64"),
            ("ceil", "fn ceil(x: f64) -> i64"),
            ("round", "fn round(x: f64) -> i64"),
            ("random", "fn random() -> f64"),
            ("pi", "fn pi() -> f64"),
            ("read_file", "fn read_file(path: string) -> string"),
            ("write_file", "fn write_file(path: string, content: string)"),
            ("file_exists", "fn file_exists(path: string) -> bool"),
            ("to_int", "fn to_int(val) -> i64"),
            ("to_float", "fn to_float(val) -> f64"),
            ("str_char_at", "fn str_char_at(s: string, i: i64) -> string"),
            ("str_substring", "fn str_substring(s: string, start: i64, len: i64) -> string"),
            ("str_parse_int", "fn str_parse_int(s: string) -> (bool, i64)"),
            ("str_parse_float", "fn str_parse_float(s: string) -> (bool, f64)"),
            ("keys", "fn keys(record) -> list"),
            ("values", "fn values(record) -> list"),
            ("has_key", "fn has_key(record, key) -> bool"),
            ("contains", "fn contains(list, elem) -> bool"),
            ("sum", "fn sum(list) -> i64"),
            ("reverse", "fn reverse(list) -> list"),
            ("flatten", "fn flatten(list) -> list"),
            ("str_split", "fn str_split(s: string, sep: string) -> list"),
            ("str_join", "fn str_join(list, sep: string) -> string"),
            ("str_replace", "fn str_replace(s: string, from: string, to: string) -> string"),
        ];

        for (name, sig) in builtins {
            if word == name {
                return Some(serde_json::json!({
                    "contents": {
                        "kind": "markdown",
                        "value": format!("**builtin** `{}`", sig)
                    }
                }));
            }
        }

        None
    }

    pub fn compute_completion(&self, text: &str) -> Vec<serde_json::Value> {
        let mut items = Vec::new();

        // Keywords
        let keywords = vec![
            "func", "type", "flow", "module", "if", "else", "while", "for",
            "return", "let", "mut", "shared", "local_shared", "weak",
            "match", "spawn", "await", "try", "comptime", "quote",
            "extern", "actor", "trait", "impl", "cap", "true", "false",
            "async", "newtype", "arena", "alloc", "requires", "ensures",
        ];

        for kw in keywords {
            items.push(serde_json::json!({
                "label": kw,
                "kind": 14, // Keyword
                "insertText": kw,
            }));
        }

        // Try to parse and extract function names
        if let Some(file) = self.parse_with_recovery(text) {
                for item in &file.items {
                    match item {
                        Item::Func(f) => {
                            items.push(serde_json::json!({
                                "label": f.name,
                                "kind": 3, // Function
                                "detail": format!("func {}(...)", f.name),
                                "insertText": format!("{}(${{1}})", f.name),
                                "insertTextFormat": 2, // Snippet
                            }));
                        }
                        Item::Type(t) => {
                            items.push(serde_json::json!({
                                "label": t.name,
                                "kind": 22, // TypeParameter
                                "detail": format!("type {}", t.name),
                            }));
                        }
                        Item::Module(m) => {
                            items.push(serde_json::json!({
                                "label": m.name,
                                "kind": 1, // Module
                                "detail": format!("module {}", m.name),
                            }));
                        }
                        Item::Trait(t) => {
                            let method_names: Vec<String> = t.methods.iter().map(|m| m.name.clone()).collect();
                            items.push(serde_json::json!({
                                "label": t.name,
                                "kind": 11, // Interface
                                "detail": format!("trait {} {{ {} }}", t.name, method_names.join(", ")),
                            }));
                        }
                        Item::Actor(a) => {
                            let method_names: Vec<String> = a.methods.iter().map(|m| m.name.clone()).collect();
                            items.push(serde_json::json!({
                                "label": a.name,
                                "kind": 23, // Struct (closest match)
                                "detail": format!("actor {} {{ {} }}", a.name, method_names.join(", ")),
                            }));
                            items.push(serde_json::json!({
                                "label": format!("{}.spawn", a.name),
                                "kind": 3, // Function
                                "detail": format!("actor {} constructor", a.name),
                                "insertText": format!("{}.spawn(${{1}})", a.name),
                                "insertTextFormat": 2,
                            }));
                        }
                        _ => {}
                    }
                }
        }

        // Builtins (updated with v5.0 additions)
        let builtins = vec![
            "println", "print", "assert", "assert_eq", "assert_ne", "len", "push",
            "pop", "range", "sqrt", "abs", "min", "max", "to_string",
            "map", "filter", "reduce", "sort", "reverse", "flatten",
            "zip", "enumerate", "sum", "contains", "input",
            "type_name", "type_fields", "type_variants", "type_info",
            "ast_dump", "ast_eval",
            // v5.0 additions
            "pow", "floor", "ceil", "round", "random", "pi",
            "read_file", "write_file", "file_exists",
            "to_int", "to_float",
            "str_char_at", "str_substring", "str_parse_int", "str_parse_float",
            "keys", "values", "has_key",
        ];

        for b in builtins {
            items.push(serde_json::json!({
                "label": b,
                "kind": 12, // Function (builtin)
                "detail": format!("builtin {}", b),
                "insertText": format!("{}(${{1}})", b),
                "insertTextFormat": 2,
            }));
        }

        items
    }

    /// Find all references to the symbol at the given position
    pub fn compute_references(&self, text: &str, line: usize, character: usize, uri: &str, include_decl: bool) -> Vec<serde_json::Value> {
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
                            def_line = text.lines().position(|l| l.contains(&format!("func {}", word)));
                            def_col = def_line.and_then(|l| lines.get(l).map(|line| line.find(&format!("func {}", word)).unwrap_or(0) + 5));
                            break;
                        }
                        Item::Type(t) if t.name == word => {
                            def_line = text.lines().position(|l| l.contains(&format!("type {}", word)));
                            def_col = def_line.and_then(|l| lines.get(l).map(|line| line.find(&format!("type {}", word)).unwrap_or(0) + 5));
                            break;
                        }
                        Item::Module(m) if m.name == word => {
                            def_line = text.lines().position(|l| l.contains(&format!("module {}", word)));
                            def_col = def_line.and_then(|l| lines.get(l).map(|line| line.find(&format!("module {}", word)).unwrap_or(0) + 7));
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
                let before = abs_pos > 0 && line_text.chars().nth(abs_pos - 1).map(|c| c.is_alphanumeric() || c == '_').unwrap_or(false);
                let after = line_text.chars().nth(abs_pos + word.len()).map(|c| c.is_alphanumeric() || c == '_').unwrap_or(false);

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
    pub fn compute_rename(&self, text: &str, line: usize, character: usize, uri: &str, new_name: &str) -> Option<serde_json::Value> {
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
                let before = abs_pos > 0 && line_text.chars().nth(abs_pos - 1).map(|c| c.is_alphanumeric() || c == '_').unwrap_or(false);
                let after = line_text.chars().nth(abs_pos + word.len()).map(|c| c.is_alphanumeric() || c == '_').unwrap_or(false);

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
    pub fn compute_signature_help(&self, text: &str, line: usize, character: usize) -> Option<serde_json::Value> {
        let lines: Vec<&str> = text.lines().collect();
        let current_line = lines.get(line)?;

        // Find the function call: look backward for '(' and the function name
        let before_cursor: String = current_line.chars().take(character).collect();
        let paren_pos = before_cursor.rfind('(')?;
        let before_paren = before_cursor[..paren_pos].trim_end();

        // Extract function name
        let func_name = before_paren.rsplit(|c: char| !c.is_alphanumeric() && c != '_')
            .next()
            .unwrap_or("");

        if func_name.is_empty() {
            return None;
        }

        // Count current argument index
        let args_in_call = before_cursor[paren_pos + 1..].chars()
            .filter(|&c| c == ',')
            .count();

        // Find function signature
        let mut signatures = Vec::new();

        if let Some(file) = self.parse_with_recovery(text) {
                for item in &file.items {
                    if let Item::Func(f) = item {
                        if f.name == func_name {
                            let params: Vec<String> = f.params.iter()
                                .map(|p| format!("{}: {:?}", p.name, p.ty))
                                .collect();
                            let ret = f.ret.as_ref().map(|t| format!(" -> {:?}", t)).unwrap_or_default();
                            signatures.push(serde_json::json!({
                                "label": format!("func {}({}){}", func_name, params.join(", "), ret),
                                "documentation": format!("Function {}", func_name),
                                "parameters": f.params.iter().map(|p| serde_json::json!({
                                    "label": format!("{}: {:?}", p.name, p.ty),
                                    "documentation": format!("Parameter {}", p.name)
                                })).collect::<Vec<_>>()
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

    /// Compute semantic tokens for the document
    pub fn compute_semantic_tokens(&self, text: &str) -> Vec<u32> {
        let mut tokens = Vec::new();

        if let Ok(lexer_tokens) = lexer::Lexer::new(text).tokenize() {
            let mut prev_line = 0u32;
            let mut prev_start = 0u32;

            for tok in &lexer_tokens {
                let line = (tok.line as u32).saturating_sub(1);
                let start = (tok.col as u32).saturating_sub(1);

                // Calculate token length from kind
                let len = match &tok.kind {
                    crate::lexer::TokenKind::Ident(s) => s.len() as u32,
                    crate::lexer::TokenKind::Int(s) => s.len() as u32,
                    crate::lexer::TokenKind::Float(s) => s.len() as u32,
                    crate::lexer::TokenKind::String(s) => s.len() as u32 + 2, // include quotes
                    crate::lexer::TokenKind::FString(s) => s.len() as u32 + 2,
                    _ => {
                        // For keywords/operators, calculate from token kind
                        match tok.kind {
                            crate::lexer::TokenKind::Module => 6,
                            crate::lexer::TokenKind::Type => 4,
                            crate::lexer::TokenKind::Func => 4,
                            crate::lexer::TokenKind::Fn => 2,
                            crate::lexer::TokenKind::Actor => 5,
                            crate::lexer::TokenKind::Let => 3,
                            crate::lexer::TokenKind::Mut => 3,
                            crate::lexer::TokenKind::Return => 6,
                            crate::lexer::TokenKind::If => 2,
                            crate::lexer::TokenKind::Else => 4,
                            crate::lexer::TokenKind::While => 5,
                            crate::lexer::TokenKind::For => 3,
                            crate::lexer::TokenKind::Match => 5,
                            crate::lexer::TokenKind::Spawn => 5,
                            crate::lexer::TokenKind::Await => 5,
                            crate::lexer::TokenKind::Extern => 6,
                            crate::lexer::TokenKind::Trait => 5,
                            crate::lexer::TokenKind::Impl => 4,
                            crate::lexer::TokenKind::Cap => 3,
                            crate::lexer::TokenKind::Async => 5,
                            crate::lexer::TokenKind::True => 4,
                            crate::lexer::TokenKind::False => 5,
                            crate::lexer::TokenKind::I32 => 3,
                            crate::lexer::TokenKind::I64 => 3,
                            crate::lexer::TokenKind::F64 => 3,
                            crate::lexer::TokenKind::Bool => 4,
                            crate::lexer::TokenKind::StringKw => 6,
                            _ => 1,
                        }
                    }
                };

                let (token_type, modifiers) = match &tok.kind {
                    crate::lexer::TokenKind::Func | crate::lexer::TokenKind::Type |
                    crate::lexer::TokenKind::Module | crate::lexer::TokenKind::Actor |
                    crate::lexer::TokenKind::Trait | crate::lexer::TokenKind::Impl |
                    crate::lexer::TokenKind::Newtype => (0, vec![0]), // keyword + declaration
                    crate::lexer::TokenKind::If | crate::lexer::TokenKind::Else |
                    crate::lexer::TokenKind::While | crate::lexer::TokenKind::For |
                    crate::lexer::TokenKind::Return | crate::lexer::TokenKind::Let |
                    crate::lexer::TokenKind::Mut | crate::lexer::TokenKind::Match |
                    crate::lexer::TokenKind::Spawn | crate::lexer::TokenKind::Await |
                    crate::lexer::TokenKind::Extern | crate::lexer::TokenKind::Cap |
                    crate::lexer::TokenKind::Async |
                    crate::lexer::TokenKind::True | crate::lexer::TokenKind::False |
                    crate::lexer::TokenKind::In | crate::lexer::TokenKind::Break |
                    crate::lexer::TokenKind::Continue | crate::lexer::TokenKind::Use |
                    crate::lexer::TokenKind::Pub | crate::lexer::TokenKind::Drop => (0, vec![]), // keyword
                    crate::lexer::TokenKind::Int(_) | crate::lexer::TokenKind::Float(_) => (4, vec![]), // number
                    crate::lexer::TokenKind::String(_) | crate::lexer::TokenKind::FString(_) => (5, vec![]), // string
                    crate::lexer::TokenKind::Ident(_) => (3, vec![]), // variable
                    crate::lexer::TokenKind::Plus | crate::lexer::TokenKind::Minus |
                    crate::lexer::TokenKind::Star | crate::lexer::TokenKind::Slash |
                    crate::lexer::TokenKind::Percent | crate::lexer::TokenKind::Eq |
                    crate::lexer::TokenKind::Ne | crate::lexer::TokenKind::Lt |
                    crate::lexer::TokenKind::Gt | crate::lexer::TokenKind::Le |
                    crate::lexer::TokenKind::Ge | crate::lexer::TokenKind::And |
                    crate::lexer::TokenKind::Or | crate::lexer::TokenKind::Not => (7, vec![]), // operator
                    _ => continue,
                };

                let delta_line = line.saturating_sub(prev_line);
                let delta_start = if delta_line == 0 {
                    start.saturating_sub(prev_start)
                } else {
                    start
                };

                tokens.push(delta_line);
                tokens.push(delta_start);
                tokens.push(len);
                tokens.push(token_type);
                tokens.push(modifiers.iter().fold(0u32, |acc, m| acc | (1 << m)));

                prev_line = line;
                prev_start = start;
            }
        }

        tokens
    }

    /// Compute document highlights for the symbol at the given position
    pub fn compute_document_highlight(&self, text: &str, line: usize, character: usize) -> Vec<serde_json::Value> {
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
                        def_line = text.lines().position(|l| l.contains(&format!("func {}", word)));
                        def_col = def_line.and_then(|l| lines.get(l).map(|line| line.find(&format!("func {}", word)).unwrap_or(0) + 5));
                        break;
                    }
                    Item::Type(t) if t.name == word => {
                        def_line = text.lines().position(|l| l.contains(&format!("type {}", word)));
                        def_col = def_line.and_then(|l| lines.get(l).map(|line| line.find(&format!("type {}", word)).unwrap_or(0) + 5));
                        break;
                    }
                    Item::Module(m) if m.name == word => {
                        def_line = text.lines().position(|l| l.contains(&format!("module {}", word)));
                        def_col = def_line.and_then(|l| lines.get(l).map(|line| line.find(&format!("module {}", word)).unwrap_or(0) + 7));
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
                let before = abs_pos > 0 && line_text.chars().nth(abs_pos - 1).map(|c| c.is_alphanumeric() || c == '_').unwrap_or(false);
                let after = line_text.chars().nth(abs_pos + word.len()).map(|c| c.is_alphanumeric() || c == '_').unwrap_or(false);

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

    /// Compute inlay hints for the document: type hints for let bindings
    /// and parameter name hints for function calls.
    pub fn compute_inlay_hints(&self, text: &str) -> Vec<serde_json::Value> {
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
        hints: &mut Vec<serde_json::Value>,
        func_params: &HashMap<String, Vec<String>>,
    ) {
        for stmt in stmts {
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
                                crate::ast::Pattern::Variable(n) => n.as_str(),
                                _ => "",
                            };
                            if let Some(let_line) = lines.iter().position(|l| {
                                l.trim().starts_with("let") && pat_name.len() > 0 && l.contains(pat_name)
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
        hints: &mut Vec<serde_json::Value>,
        func_params: &HashMap<String, Vec<String>>,
    ) {
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
                let call_line = text.lines().position(|l| {
                    l.contains(func_name) && l.contains('(')
                });
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
                let mut arg_start = paren_pos + 1;
                let mut arg_idx = 0;
                for (ch_idx, ch) in line_content.chars().enumerate() {
                    if ch_idx < paren_pos + 1 { continue; }
                    match ch {
                        '(' | '[' | '{' => depth += 1,
                        ')' | ']' | '}' => depth -= 1,
                        ',' if depth == 0 => {
                            if arg_idx < args.len() && arg_idx < param_names.len() {
                                let arg_str = line_content[arg_start..ch_idx].trim();
                                if !arg_str.is_empty() && !arg_str.chars().all(|c| c.is_alphanumeric() || c == '_') {
                                    hints.push(serde_json::json!({
                                        "position": {
                                            "line": cl,
                                            "character": arg_start as u64
                                        },
                                        "label": format!("{}:", param_names[arg_idx]),
                                        "kind": 2,  // Parameter
                                        "paddingRight": true
                                    }));
                                }
                            }
                            arg_start = ch_idx + 1;
                            arg_idx += 1;
                        }
                        _ => {}
                    }
                }
                // Last argument
                if arg_idx < args.len() && arg_idx < param_names.len() {
                    let end_pos = line_content.rfind(')').unwrap_or(line_content.len());
                    let arg_str = line_content[arg_start..end_pos].trim();
                    if !arg_str.is_empty() && !arg_str.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        hints.push(serde_json::json!({
                            "position": {
                                "line": cl,
                                "character": arg_start as u64
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

    /// Format a type for human-readable display
    fn type_display(ty: &Type) -> String {
        match ty {
            Type::Name(name, params) => {
                if params.is_empty() {
                    name.clone()
                } else {
                    let inner: Vec<String> = params.iter().map(Self::type_display).collect();
                    format!("{}[{}]", name, inner.join(", "))
                }
            }
            Type::Ref(lt, inner) => {
                let lt_str = lt.as_ref().map(|l| format!("'{} ", l)).unwrap_or_default();
                format!("&{} {}", lt_str, Self::type_display(inner))
            }
            Type::RefMut(lt, inner) => {
                let lt_str = lt.as_ref().map(|l| format!("'{} ", l)).unwrap_or_default();
                format!("&{} mut {}", lt_str, Self::type_display(inner))
            }
            Type::Tuple(elems) => {
                let inner: Vec<String> = elems.iter().map(Self::type_display).collect();
                format!("({})", inner.join(", "))
            }
            Type::Func(params, ret) => {
                let p: Vec<String> = params.iter().map(Self::type_display).collect();
                format!("fn({}) -> {}", p.join(", "), Self::type_display(ret))
            }
            Type::ExternFunc(params, ret) => {
                let p: Vec<String> = params.iter().map(Self::type_display).collect();
                format!("extern fn({}) -> {}", p.join(", "), Self::type_display(ret))
            }
            Type::RawPtr(inner) => format!("*{}", Self::type_display(inner)),
            Type::RawPtrMut(inner) => format!("*mut {}", Self::type_display(inner)),
            Type::CShared(inner) => format!("c_shared {}", Self::type_display(inner)),
            Type::CBorrow(inner) => format!("c_borrow {}", Self::type_display(inner)),
            Type::CBorrowMut(inner) => format!("c_borrow_mut {}", Self::type_display(inner)),
            Type::Option(inner) => format!("Option<{}>", Self::type_display(inner)),
            Type::Result(ok, err) => format!("Result<{}, {}>", Self::type_display(ok), Self::type_display(err)),
            Type::Shared(inner) => format!("shared {}", Self::type_display(inner)),
            Type::LocalShared(inner) => format!("local_shared {}", Self::type_display(inner)),
            Type::Weak(inner) => format!("weak {}", Self::type_display(inner)),
            Type::WeakLocal(inner) => format!("weak_local {}", Self::type_display(inner)),
            Type::Newtype(name, inner) => format!("{} (newtype over {})", name, Self::type_display(inner)),
            Type::Array(inner, n) => format!("[{}; {}]", Self::type_display(inner), n),
            Type::Slice(inner) => format!("[{}]", Self::type_display(inner)),
            Type::ImplTrait(ts) => format!("impl {}", ts.join(" + ")),
            Type::DynTrait(ts) => format!("dyn {}", ts.join(" + ")),
            Type::RawString => "RawString".to_string(),
            Type::Cap(name) => format!("cap {}", name),
            Type::CBuffer(inner) => format!("CBuffer<{}>", Self::type_display(inner)),
            Type::Nothing => "!".to_string(),
            Type::Allocator => "Allocator".to_string(),
            Type::Infer => "_".to_string(),
        }
    }

    /// Get the column of the word start at the given position
    pub fn word_start_col(&self, text: &str, line: usize, character: usize) -> usize {
        let lines: Vec<&str> = text.lines().collect();
        let current_line = match lines.get(line) {
            Some(l) => l,
            None => return character,
        };
        let before_cursor: String = current_line.chars().take(character).collect();
        before_cursor.rfind(|c: char| !c.is_alphanumeric() && c != '_')
            .map(|i| i + 1)
            .unwrap_or(0)
    }

    /// Get the number of characters from the cursor to the end of the word
    pub fn word_end_offset(&self, text: &str, line: usize, character: usize) -> usize {
        let lines: Vec<&str> = text.lines().collect();
        let current_line = match lines.get(line) {
            Some(l) => l,
            None => return 0,
        };
        let after_cursor: String = current_line.chars().skip(character).collect();
        after_cursor.find(|c: char| !c.is_alphanumeric() && c != '_')
            .map(|i| i)
            .unwrap_or_else(|| current_line.len().saturating_sub(character))
    }

    /// Helper: get the word at a given position
    pub fn get_word_at(&self, text: &str, line: usize, character: usize) -> String {
        let lines: Vec<&str> = text.lines().collect();
        let current_line = match lines.get(line) {
            Some(l) => l,
            None => return String::new(),
        };

        let before_cursor: String = current_line.chars().take(character).collect();
        let after_cursor: String = current_line.chars().skip(character).collect();

        let word_start = before_cursor.rfind(|c: char| !c.is_alphanumeric() && c != '_').map(|i| i + 1).unwrap_or(0);
        let word_end = after_cursor.find(|c: char| !c.is_alphanumeric() && c != '_').map(|i| character + i).unwrap_or(current_line.len());

        if word_start >= word_end {
            return String::new();
        }

        current_line[word_start..word_end].to_string()
    }
}
