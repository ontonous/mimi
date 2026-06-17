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
                        "hoverProvider": true,
                        "definitionProvider": true,
                        "referencesProvider": true,
                        "renameProvider": true,
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
                diagnostics.push(serde_json::json!({
                    "range": {
                        "start": { "line": err.span.start_line.saturating_sub(1), "character": err.span.start_col.saturating_sub(1) },
                        "end": { "line": err.span.end_line.saturating_sub(1), "character": err.span.end_col.saturating_sub(1) }
                    },
                    "severity": 1,
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
                                .map(|p| format!("{}: {:?}", p.name, p.ty))
                                .collect();
                            let ret = f.ret.as_ref().map(|t| format!(" -> {:?}", t)).unwrap_or_default();
                            return Some(serde_json::json!({
                                "contents": {
                                    "kind": "markdown",
                                    "value": format!("**func** `{}({}){}`", word, params.join(", "), ret)
                                }
                            }));
                        }
                        Item::Type(t) if t.name == word => {
                            return Some(serde_json::json!({
                                "contents": {
                                    "kind": "markdown",
                                    "value": format!("**type** `{}`", word)
                                }
                            }));
                        }
                        Item::Module(m) if m.name == word => {
                            return Some(serde_json::json!({
                                "contents": {
                                    "kind": "markdown",
                                    "value": format!("**module** `{}`", word)
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
