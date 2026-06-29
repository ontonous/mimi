use serde_json::Value;

use crate::ast::{Item, Stmt, Type};
use crate::lsp::LspServer;

impl LspServer {
    /// Determine completion context from cursor position
    pub(crate) fn completion_context(text: &str, line: usize, character: usize) -> &'static str {
        let lines: Vec<&str> = text.lines().collect();
        let current_line = match lines.get(line) {
            Some(l) => l,
            None => return "top",
        };
        let before_cursor: String = current_line.chars().take(character).collect();
        let trimmed = before_cursor.trim();

        // After `.` — distinguish self. (actor/impl method completion)
        // from obj. (record field + method completion). Both go to the
        // same "dot" branch in compute_completion; the difference is
        // whether the receiver is `self`.
        if trimmed.ends_with('.') {
            if trimmed == "self." {
                return "self_dot";
            }
            return "dot";
        }
        // After `::` — qualified path completion
        if trimmed.ends_with("::") {
            return "path";
        }
        // After `:` (but not `::`) — type annotation
        if trimmed.ends_with(':') {
            return "type";
        }
        // After `use` — module name
        if trimmed.starts_with("use") || before_cursor.trim().starts_with("use ") {
            return "module";
        }
        // After `impl` — trait/type name
        if trimmed.starts_with("impl") || trimmed.starts_with("impl ") {
            return "impl";
        }
        // After `extern` — ABI string or block
        // "extern" (no space) triggers ABI completions; "extern " does not
        if !trimmed.starts_with("extern ") && trimmed.starts_with("extern") {
            return "extern";
        }
        // Start of expression or after opening brace/paren
        "top"
    }

    /// Extract the identifier immediately preceding the dot at the
    /// cursor position (for `obj.` or `self.` completions). Returns
    /// None if the context doesn't match.
    fn extract_obj_ident_for_dot(text: &str, line: usize, character: usize) -> Option<String> {
        let current_line = text.lines().nth(line)?;
        let before_cursor: String = current_line.chars().take(character).collect();
        // Trim trailing whitespace and a single `.`
        let trimmed = before_cursor.trim_end();
        if !trimmed.ends_with('.') {
            return None;
        }
        let before_dot = trimmed[..trimmed.len() - 1].trim_end();
        // The last identifier token (letters, digits, underscore, $)
        let last = before_dot
            .rsplit(|c: char| !c.is_alphanumeric() && c != '_' && c != '$')
            .next()?;
        if last.is_empty() {
            None
        } else {
            Some(last.to_string())
        }
    }

    pub fn compute_completion(&mut self, text: &str, line: usize, character: usize) -> Vec<Value> {
        // Load stdlib completions lazily on first completion request
        self.load_stdlib_completions();
        let mut items = Vec::new();
        let context = Self::completion_context(text, line, character);

        // v0.28.11: For "dot" and "self_dot" contexts, also offer record
        // fields when the object before the dot is a known typed local.
        let obj_ident = Self::extract_obj_ident_for_dot(text, line, character);
        let file = self.parse_with_recovery(text);

        match context {
            "dot" | "self_dot" => {
                let methods = vec![
                    ("to_string", "to_string() -> string"),
                    ("len", "len() -> i64"),
                    ("trim", "trim() -> string"),
                    ("to_upper", "to_upper() -> string"),
                    ("to_lower", "to_lower() -> string"),
                    ("repeat", "repeat(n: i64) -> string"),
                    ("replace", "replace(from: string, to: string) -> string"),
                    ("char_at", "char_at(i: i64) -> string"),
                    ("substring", "substring(start: i64, len: i64) -> string"),
                    ("split", "split(sep: string) -> list"),
                    ("contains", "contains(elem) -> bool"),
                    ("push", "push(item) -> list"),
                    ("pop", "pop() -> item"),
                    ("sort", "sort() -> list"),
                    ("reverse", "reverse() -> list"),
                    ("keys", "keys() -> list"),
                    ("values", "values() -> list"),
                    ("has_key", "has_key(key) -> bool"),
                ];
                for (name, sig) in methods {
                    items.push(serde_json::json!({
                        "label": name,
                        "kind": 2, // Method
                        "detail": sig,
                        "insertText": format!("{}(${{1}})", name),
                        "insertTextFormat": 2,
                    }));
                }
                if let Some(ref file) = file {
                    // v0.28.11: When the receiver before the dot is a
                    // typed local, surface the record's fields as
                    // CompletionItemKind::Field (5).
                    if let Some(obj_name) = &obj_ident {
                        if let Some(type_name) = Self::find_local_type_name(&file.items, obj_name) {
                            for item in &file.items {
                                if let Item::Type(td) = item {
                                    if td.name == type_name {
                                        if let crate::ast::TypeDefKind::Record(fields) = &td.kind {
                                            for f in fields {
                                                items.push(serde_json::json!({
                                                    "label": f.name,
                                                    "kind": 5, // Field
                                                    "detail": format!("field {}: {}", f.name, crate::core::fmt_type(&f.ty)),
                                                    "insertText": f.name.clone(),
                                                }));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    for item in &file.items {
                        match item {
                            Item::Trait(t) => {
                                for m in &t.methods {
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
                                        .map(|r| format!(" -> {}", crate::core::fmt_type(r)))
                                        .unwrap_or_default();
                                    items.push(serde_json::json!({
                                        "label": m.name,
                                        "kind": 2, // Method
                                        "detail": format!("fn {}({}){}", m.name, params.join(", "), ret),
                                        "insertText": format!("{}(${{1}})", m.name),
                                        "insertTextFormat": 2,
                                    }));
                                }
                            }
                            Item::Actor(a) => {
                                for m in &a.methods {
                                    items.push(serde_json::json!({
                                        "label": m.name,
                                        "kind": 2, // Method
                                        "detail": format!("actor method {}", m.name),
                                        "insertText": format!("{}(${{1}})", m.name),
                                        "insertTextFormat": 2,
                                    }));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                return items;
            }
            "path" => {
                // Qualified path completions: show items from the module before `::`
                let lines: Vec<&str> = text.lines().collect();
                let current_line = lines.get(line).unwrap_or(&"");
                let before_cursor: String = current_line.chars().take(character).collect();
                let trimmed = before_cursor.trim().trim_end_matches(':');
                // Extract the last identifier before `::` (the module name)
                let prefix = trimmed
                    .split_whitespace()
                    .last()
                    .unwrap_or("")
                    .trim_end_matches(':');
                // Also handle `use std::strings::` → the text before final `::` might have more segments
                // The prefix would be "std::strings" — only match the last segment for now
                let module_name = prefix.split("::").last().unwrap_or(prefix);
                // Check stdlib modules
                if !module_name.is_empty() {
                    if let Some(funcs) = self.stdlib_funcs.get(module_name) {
                        for (name, detail, insert) in funcs {
                            items.push(serde_json::json!({
                                "label": name,
                                "kind": 3, // Function
                                "detail": detail,
                                "insertText": insert,
                                "insertTextFormat": 2,
                            }));
                        }
                    }
                }
                return items;
            }
            "type" => {
                // Type completions after `:`
                let type_keywords = vec!["i32", "i64", "f64", "bool", "string", "unit"];
                for t in type_keywords {
                    items.push(serde_json::json!({
                        "label": t,
                        "kind": 22, // TypeParameter
                        "detail": format!("type {}", t),
                    }));
                }
                // Also add user-defined types from AST
                if let Some(file) = self.parse_with_recovery(text) {
                    for item in &file.items {
                        if let Item::Type(t) = item {
                            items.push(serde_json::json!({
                                "label": t.name.clone(),
                                "kind": 22, // TypeParameter
                                "detail": format!("type {}", t.name),
                            }));
                        }
                    }
                }
                return items;
            }
            "module" => {
                // Module name completions after `use`
                if let Some(file) = self.parse_with_recovery(text) {
                    for item in &file.items {
                        if let Item::Module(m) = item {
                            items.push(serde_json::json!({
                                "label": m.name.clone(),
                                "kind": 1, // Module
                                "detail": format!("module {}", m.name),
                            }));
                        }
                    }
                }
                // Add stdlib module names
                let mut std_modules: Vec<&str> =
                    self.stdlib_funcs.keys().map(|s| s.as_str()).collect();
                std_modules.sort();
                for name in std_modules {
                    items.push(serde_json::json!({
                        "label": name,
                        "kind": 1, // Module
                        "detail": format!("module {}", name),
                    }));
                }
                return items;
            }
            "impl" => {
                if let Some(file) = self.parse_with_recovery(text) {
                    for item in &file.items {
                        match item {
                            Item::Trait(t) => {
                                items.push(serde_json::json!({
                                    "label": t.name.clone(),
                                    "kind": 11, // Interface
                                    "detail": format!("trait {}", t.name),
                                }));
                            }
                            Item::Type(t) => {
                                items.push(serde_json::json!({
                                    "label": t.name.clone(),
                                    "kind": 22, // TypeParameter
                                    "detail": format!("type {}", t.name),
                                }));
                            }
                            _ => {}
                        }
                    }
                }
                return items;
            }
            "extern" => {
                items.push(serde_json::json!({
                    "label": "\"C\" { ... }",
                    "kind": 14, // Keyword
                    "detail": "extern \"C\" block with C function declarations",
                    "insertText": "\"C\" {\n  ${1}\n}",
                    "insertTextFormat": 2, // Snippet
                }));
                items.push(serde_json::json!({
                    "label": "\"stdcall\" { ... }",
                    "kind": 14,
                    "detail": "extern \"stdcall\" block (Windows)",
                    "insertText": "\"stdcall\" {\n  ${1}\n}",
                    "insertTextFormat": 2,
                }));
                let c_funcs = vec![
                    ("strlen", "strlen(s: string) -> i64", "get string length"),
                    (
                        "printf",
                        "printf(format: &i8, ...) -> i32",
                        "print formatted",
                    ),
                    ("malloc", "malloc(size: i64) -> *mut i8", "allocate memory"),
                    ("free", "free(ptr: *mut i8)", "free memory"),
                    (
                        "memcpy",
                        "memcpy(dest: *mut i8, src: *mut i8, n: i64) -> *mut i8",
                        "copy memory",
                    ),
                    (
                        "memset",
                        "memset(s: *mut i8, c: i32, n: i64) -> *mut i8",
                        "set memory",
                    ),
                    ("puts", "puts(s: &i8) -> i32", "print string with newline"),
                    ("exit", "exit(code: i32)", "exit program"),
                    ("atoi", "atoi(s: &i8) -> i32", "string to int"),
                    ("atof", "atof(s: &i8) -> f64", "string to float"),
                    ("rand", "rand() -> i32", "random number"),
                    ("srand", "srand(seed: i32)", "seed random"),
                    ("abs", "abs(x: i32) -> i32", "absolute value"),
                    ("clock", "clock() -> i64", "processor time"),
                    ("time", "time(t: *mut i64) -> i64", "current time"),
                ];
                for (name, sig, desc) in c_funcs {
                    items.push(serde_json::json!({
                        "label": name,
                        "kind": 3, // Function
                        "detail": format!("extern fn {};  // {}", sig, desc),
                        "insertText": format!("func {};", sig),
                        "insertTextFormat": 2, // Snippet
                        "documentation": desc,
                    }));
                }
                return items;
            }
            _ => {} // Fall through to default "top" context
        }

        // Default "top" context: show everything
        // Keywords
        let keywords = vec![
            "func",
            "type",
            "flow",
            "module",
            "if",
            "else",
            "while",
            "for",
            "return",
            "let",
            "mut",
            "shared",
            "local_shared",
            "weak",
            "match",
            "spawn",
            "await",
            "try",
            "comptime",
            "quote",
            "extern",
            "actor",
            "trait",
            "impl",
            "cap",
            "true",
            "false",
            "async",
            "newtype",
            "arena",
            "alloc",
            "requires",
            "ensures",
            "loop",
        ];

        for kw in keywords {
            items.push(serde_json::json!({
                "label": kw,
                "kind": 14, // Keyword
                "insertText": kw,
            }));
        }

        // User-defined functions, types, modules, traits, actors from AST
        if let Some(file) = self.parse_with_recovery(text) {
            for item in &file.items {
                match item {
                    Item::Func(f) => {
                        items.push(serde_json::json!({
                            "label": f.name.clone(),
                            "kind": 3, // Function
                            "detail": format!("func {}(...)", f.name),
                            "insertText": format!("{}(${{1}})", f.name),
                            "insertTextFormat": 2, // Snippet
                        }));
                    }
                    Item::Type(t) => {
                        items.push(serde_json::json!({
                            "label": t.name.clone(),
                            "kind": 22, // TypeParameter
                            "detail": format!("type {}", t.name),
                        }));
                    }
                    Item::Module(m) => {
                        items.push(serde_json::json!({
                            "label": m.name.clone(),
                            "kind": 1, // Module
                            "detail": format!("module {}", m.name),
                        }));
                    }
                    Item::Trait(t) => {
                        let method_names: Vec<String> =
                            t.methods.iter().map(|m| m.name.clone()).collect();
                        items.push(serde_json::json!({
                            "label": t.name.clone(),
                            "kind": 11, // Interface
                            "detail": format!("trait {} {{ {} }}", t.name, method_names.join(", ")),
                        }));
                    }
                    Item::Actor(a) => {
                        let method_names: Vec<String> =
                            a.methods.iter().map(|m| m.name.clone()).collect();
                        items.push(serde_json::json!({
                            "label": a.name.clone(),
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

        // Builtins
        let builtins = vec![
            "println",
            "print",
            "assert",
            "assert_eq",
            "assert_ne",
            "len",
            "push",
            "pop",
            "range",
            "sqrt",
            "abs",
            "min",
            "max",
            "to_string",
            "map",
            "filter",
            "reduce",
            "sort",
            "reverse",
            "flatten",
            "zip",
            "enumerate",
            "sum",
            "contains",
            "input",
            "type_name",
            "type_fields",
            "type_variants",
            "type_info",
            "ast_dump",
            "ast_eval",
            "pow",
            "floor",
            "ceil",
            "round",
            "random",
            "pi",
            "read_file",
            "write_file",
            "file_exists",
            "to_int",
            "to_float",
            "str_char_at",
            "str_substring",
            "str_parse_int",
            "str_parse_float",
            "keys",
            "values",
            "has_key",
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

        // Stdlib functions (loaded lazily)
        items.extend(self.stdlib_completions_raw.iter().cloned());

        items
    }

    /// v0.28.11: Find the declared type name of a local variable in
    /// any function body in `items`. Scans all `let name: T = ...`
    /// bindings. Returns the type name (or None) — does not perform
    /// full type inference, only matches explicit annotations.
    fn find_local_type_name(items: &[Item], target: &str) -> Option<String> {
        // Special case: `self` refers to the enclosing actor/impl type.
        if target == "self" {
            for item in items {
                match item {
                    Item::Actor(a) => return Some(a.name.clone()),
                    Item::Impl(imp) => return Some(imp.type_name.clone()),
                    _ => {}
                }
            }
        }
        for item in items {
            if let Item::Func(f) = item {
                for stmt in &f.body {
                    if let Stmt::Let {
                        pat: crate::ast::Pattern::Variable(name),
                        ty: Some(Type::Name(type_name, _)),
                        ..
                    } = stmt
                    {
                        if name == target {
                            return Some(type_name.clone());
                        }
                    }
                }
            }
        }
        None
    }
}
