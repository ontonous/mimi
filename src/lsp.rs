use crate::{core, lexer, parser};
use crate::ast::Item;
use std::collections::HashMap;
use std::io::{self, BufRead, Read, Write};

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

            if len == 0 {
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
                        "textDocumentSync": 1,
                        "completionProvider": {
                            "triggerCharacters": [".", ":"]
                        },
                        "diagnosticProvider": {
                            "interFileDependencies": false,
                            "workspaceDiagnostics": false
                        }
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
            "shutdown" => {
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": null
                }))
            }
            "exit" => std::process::exit(0),
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

        let file = match parser::Parser::new(tokens).parse_file() {
            Ok(f) => f,
            Err(e) => {
                diagnostics.push(serde_json::json!({
                    "range": {
                        "start": { "line": e.line.saturating_sub(1), "character": e.col.saturating_sub(1) },
                        "end": { "line": e.line.saturating_sub(1), "character": e.col }
                    },
                    "severity": 1,
                    "message": e.message
                }));
                return diagnostics;
            }
        };

        // Type check
        if let Err(errs) = core::check(&file) {
            for err in errs {
                diagnostics.push(serde_json::json!({
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 0, "character": 100 }
                    },
                    "severity": 1,
                    "message": err.message
                }));
            }
        }

        diagnostics
    }

    pub fn compute_completion(&self, text: &str) -> Vec<serde_json::Value> {
        let mut items = Vec::new();

        // Keywords
        let keywords = vec![
            "func", "type", "flow", "module", "if", "else", "while", "for",
            "return", "let", "mut", "shared", "local_shared", "weak",
            "match", "spawn", "await", "try", "comptime", "quote",
            "extern", "actor", "trait", "impl", "cap", "true", "false",
        ];

        for kw in keywords {
            items.push(serde_json::json!({
                "label": kw,
                "kind": 14, // Keyword
                "insertText": kw,
            }));
        }

        // Try to parse and extract function names
        if let Ok(tokens) = lexer::Lexer::new(text).tokenize() {
            if let Ok(file) = parser::Parser::new(tokens).parse_file() {
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
                        _ => {}
                    }
                }
            }
        }

        // Builtins
        let builtins = vec![
            "println", "assert", "assert_eq", "assert_ne", "len", "push",
            "pop", "range", "sqrt", "abs", "min", "max", "to_string",
            "map", "filter", "reduce", "sort", "reverse", "flatten",
            "zip", "enumerate", "sum", "contains", "input",
            "type_name", "type_fields", "type_variants", "type_info",
            "ast_dump", "ast_eval",
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
}
