//! Mimi runtime source lexer/parser — tokenizer + recursive-descent parser
//! producing a JSON AST, used by `mimi lint` and the `parse()` / `lexer()`
//! builtins.
//!
//! Extracted verbatim from `runtime/mod.rs` during the 0.1.0 mechanical split
//! (behavior bit-exact). Provides the `mimi_lexer_tokenize` /
//! `mimi_parse_source` `extern "C"` symbols. Self-contained except for the
//! parent module's `alloc_c_string` / `cstr_to_string` helpers.

use super::{alloc_c_string, cstr_to_string};

// ─── Mimi source parser for linting (mimi_parse_source / mimi_lexer_tokenize) ──
//
// These functions implement a minimal Mimi tokenizer + recursive-descent parser
// that produces a JSON string. The Mimi-level `parse()` and `lexer()` builtins
// call these at runtime. The JSON is consumed by Mimi code via `from_json()`.
//
// JSON schema for mimi_lexer_tokenize(source):
//   [{"kind":"IDENT","value":"func","line":1,"col":1}, ...]
//
// JSON schema for mimi_parse_source(source):
//   {
//     "functions": [{"name":"main","line":1,"col":6,"is_pub":false, ...}],
//     "types": [...], "imports": [...], "has_main": true
//   }

struct MimiToken {
    kind: String,
    value: String,
    line: usize,
    col: usize,
}

struct MimiLexer {
    chars: Vec<char>,
    pos: usize,
    line: usize,
    col: usize,
}

impl MimiLexer {
    fn new(source: &str) -> Self {
        Self {
            chars: source.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.chars.get(self.pos).copied();
        if let Some(ch) = c {
            self.pos += 1;
            if ch == '\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
        }
        c
    }

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.peek() {
            if ch == ' ' || ch == '\t' || ch == '\n' || ch == '\r' {
                self.advance();
            } else {
                break;
            }
        }
    }

    fn tokenize(&mut self) -> Vec<MimiToken> {
        let mut tokens = Vec::new();
        loop {
            self.skip_whitespace();
            let line = self.line;
            let col = self.col;
            match self.peek() {
                None => break,
                Some('/') if self.pos + 1 < self.chars.len() && self.chars[self.pos + 1] == '/' => {
                    while let Some(ch) = self.peek() {
                        if ch == '\n' {
                            break;
                        }
                        self.advance();
                    }
                }
                Some('/') if self.pos + 1 < self.chars.len() && self.chars[self.pos + 1] == '*' => {
                    self.advance();
                    self.advance();
                    while let Some(ch) = self.peek() {
                        if ch == '*'
                            && self.pos + 1 < self.chars.len()
                            && self.chars[self.pos + 1] == '/'
                        {
                            self.advance();
                            self.advance();
                            break;
                        }
                        self.advance();
                    }
                }
                Some('"') => {
                    self.advance();
                    let mut s = String::new();
                    while let Some(ch) = self.peek() {
                        if ch == '"' {
                            self.advance();
                            break;
                        }
                        if ch == '\\' {
                            self.advance();
                            if let Some(esc) = self.peek() {
                                match esc {
                                    'n' => s.push('\n'),
                                    't' => s.push('\t'),
                                    'r' => s.push('\r'),
                                    '"' => s.push('"'),
                                    '\\' => s.push('\\'),
                                    c => s.push(c),
                                }
                                self.advance();
                            }
                        } else {
                            s.push(ch);
                            self.advance();
                        }
                    }
                    tokens.push(MimiToken {
                        kind: "STRING".into(),
                        value: s,
                        line,
                        col,
                    });
                }
                Some(c)
                    if c.is_ascii_digit()
                        || (c == '-'
                            && self.pos + 1 < self.chars.len()
                            && self.chars[self.pos + 1].is_ascii_digit()) =>
                {
                    let mut s = String::new();
                    if c == '-' {
                        s.push('-');
                        self.advance();
                    }
                    let mut is_float = false;
                    while let Some(ch) = self.peek() {
                        if ch.is_ascii_digit() {
                            s.push(ch);
                            self.advance();
                        } else if ch == '.' {
                            is_float = true;
                            s.push(ch);
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    tokens.push(MimiToken {
                        kind: if is_float {
                            "FLOAT".into()
                        } else {
                            "INT".into()
                        },
                        value: s,
                        line,
                        col,
                    });
                }
                Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                    let mut s = String::new();
                    while let Some(ch) = self.peek() {
                        if ch.is_ascii_alphanumeric() || ch == '_' {
                            s.push(ch);
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    let kind = match s.as_str() {
                        "func" | "pub" | "let" | "mut" | "if" | "else" | "while" | "for"
                        | "return" | "break" | "continue" | "true" | "false" | "module" | "use"
                        | "const" | "type" | "extern" | "match" | "in" | "as" | "struct"
                        | "enum" | "union" | "newtype" | "where" | "trait" | "impl" | "cap"
                        | "shared" | "local_shared" | "weak" | "loop" | "parasteps" | "alloc"
                        | "arena" | "unsafe" | "drop" | "on_failure" | "comptime" | "async"
                        | "requires" | "ensures" | "desc" | "rule" | "mms" | "invariant"
                        | "math" | "Record" | "Any" | "Option" | "Result" | "List" | "Set"
                        | "Map" | "Future" | "String" | "bool" | "i32" | "i64" | "f32" | "f64" => {
                            "KEYWORD"
                        }
                        _ => "IDENT",
                    };
                    tokens.push(MimiToken {
                        kind: kind.into(),
                        value: s,
                        line,
                        col,
                    });
                }
                Some(c) => {
                    let mut val = String::new();
                    val.push(c);
                    self.advance();
                    if matches!(
                        c,
                        '=' | '!' | '<' | '>' | '&' | '|' | '+' | '-' | '*' | '/' | '.' | ':'
                    ) {
                        if let Some(next) = self.peek() {
                            if (matches!(c, '=' | '!' | '<' | '>') && next == '=')
                                || (c == '&' && next == '&')
                                || (c == '|' && next == '|')
                                || (c == '+' && next == '=')
                                || (c == '-' && (next == '=' || next == '>'))
                                || (c == ':' && next == ':')
                                || (c == '.' && next == '.')
                            {
                                val.push(next);
                                self.advance();
                            }
                        }
                    }
                    tokens.push(MimiToken {
                        kind: if matches!(
                            c,
                            '{' | '}'
                                | '('
                                | ')'
                                | '['
                                | ']'
                                | ';'
                                | ','
                                | ':'
                                | '|'
                                | '&'
                                | '#'
                                | '@'
                                | '~'
                                | '?'
                        ) {
                            "PUNCT".into()
                        } else {
                            "OP".into()
                        },
                        value: val,
                        line,
                        col,
                    });
                }
            }
        }
        tokens.push(MimiToken {
            kind: "EOF".into(),
            value: String::new(),
            line: self.line,
            col: self.col,
        });
        tokens
    }
}

fn mimi_tokens_to_json(tokens: &[MimiToken]) -> String {
    let mut json = String::from("[");
    for (i, tok) in tokens.iter().enumerate() {
        if i > 0 {
            json.push(',');
        }
        let v_escaped = tok.value.replace('\\', "\\\\").replace('"', "\\\"");
        json.push_str(&format!(
            r#"{{"kind":"{}","value":"{}","line":{},"col":{}}}"#,
            tok.kind, v_escaped, tok.line, tok.col
        ));
    }
    json.push(']');
    json
}

#[no_mangle]
pub extern "C" fn mimi_lexer_tokenize(source: *const std::ffi::c_char) -> *mut std::ffi::c_char {
    if source.is_null() {
        return alloc_c_string("[]");
    }
    // SAFETY: `source` was checked non-null above.
    let src = unsafe { cstr_to_string(source) };
    let mut lexer = MimiLexer::new(&src);
    let tokens = lexer.tokenize();
    let json = mimi_tokens_to_json(&tokens[..tokens.len().saturating_sub(1)]);
    alloc_c_string(&json)
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // M1 fix: U+2028 LINE SEPARATOR and U+2029 PARAGRAPH SEPARATOR are
            // valid JSON only when escaped. Unescaped, they break JSON parsers
            // that follow ECMAScript 2018+ line-terminator rules.
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if c < '\x20' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[no_mangle]
pub extern "C" fn mimi_parse_source(source: *const std::ffi::c_char) -> *mut std::ffi::c_char {
    if source.is_null() {
        return alloc_c_string(r#"{"functions":[],"types":[],"imports":[],"has_main":false}"#);
    }
    // SAFETY: `source` was checked non-null above.
    let src = unsafe { cstr_to_string(source) };
    let mut lexer = MimiLexer::new(&src);
    let tokens = lexer.tokenize();
    let json = mimi_build_ast_json(&tokens);
    alloc_c_string(&json)
}

fn mimi_build_ast_json(tokens: &[MimiToken]) -> String {
    let mut out = String::from(r#"{"functions":["#);
    let mut first_func = true;
    let mut types_json = Vec::new();
    let mut modules_json = Vec::new();
    let mut imports_json = Vec::new();
    let mut has_main = false;
    let mut idx = 0;

    while idx < tokens.len() {
        let tok = &tokens[idx];
        if tok.kind == "EOF" {
            break;
        }
        if tok.kind != "KEYWORD" && tok.kind != "IDENT" {
            idx += 1;
            continue;
        }

        match tok.value.as_str() {
            "pub" => {
                if idx + 1 < tokens.len() && tokens[idx + 1].value == "func" {
                    let (func_json, consumed, is_main) = parse_func_decl(tokens, idx, true);
                    if !func_json.is_empty() {
                        if !first_func {
                            out.push(',');
                        }
                        out.push_str(&func_json);
                        first_func = false;
                        if is_main {
                            has_main = true;
                        }
                    }
                    idx += consumed;
                } else {
                    idx += 1;
                }
            }
            "func" => {
                let (func_json, consumed, is_main) = parse_func_decl(tokens, idx, false);
                if !func_json.is_empty() {
                    if !first_func {
                        out.push(',');
                    }
                    out.push_str(&func_json);
                    first_func = false;
                    if is_main {
                        has_main = true;
                    }
                }
                idx += consumed;
            }
            "type" | "struct" | "enum" | "union" | "newtype" => {
                let line = tok.line;
                let col = tok.col;
                let kind = tok.value.clone();
                idx += 1;
                let mut name = String::from("_");
                if idx < tokens.len()
                    && (tokens[idx].kind == "IDENT" || tokens[idx].kind == "KEYWORD")
                {
                    name = tokens[idx].value.clone();
                    idx += 1;
                }
                if idx < tokens.len() && tokens[idx].value == "<" {
                    let mut depth = 1;
                    idx += 1;
                    while idx < tokens.len() && depth > 0 {
                        if tokens[idx].value == "<" {
                            depth += 1;
                        } else if tokens[idx].value == ">" {
                            depth -= 1;
                        }
                        idx += 1;
                    }
                }
                if idx < tokens.len() && tokens[idx].value == "{" {
                    let mut depth = 1;
                    idx += 1;
                    while idx < tokens.len() && depth > 0 {
                        if tokens[idx].value == "{" {
                            depth += 1;
                        } else if tokens[idx].value == "}" {
                            depth -= 1;
                        }
                        idx += 1;
                    }
                } else {
                    while idx < tokens.len()
                        && tokens[idx].value != ";"
                        && tokens[idx].kind != "EOF"
                    {
                        idx += 1;
                    }
                    if idx < tokens.len() && tokens[idx].value == ";" {
                        idx += 1;
                    }
                }
                types_json.push(format!(
                    r#"{{"name":"{}","line":{},"col":{},"kind":"{}"}}"#,
                    json_escape(&name),
                    line,
                    col,
                    json_escape(&kind)
                ));
            }
            "module" => {
                idx += 1;
                if idx < tokens.len()
                    && (tokens[idx].kind == "IDENT" || tokens[idx].kind == "KEYWORD")
                {
                    let mname = tokens[idx].value.clone();
                    let mline = tokens[idx].line;
                    let mcol = tokens[idx].col;
                    idx += 1;
                    if idx < tokens.len() && tokens[idx].value == ";" {
                        idx += 1;
                    } else if idx < tokens.len() && tokens[idx].value == "{" {
                        let mut depth = 1;
                        idx += 1;
                        while idx < tokens.len() && depth > 0 {
                            if tokens[idx].value == "{" {
                                depth += 1;
                            } else if tokens[idx].value == "}" {
                                depth -= 1;
                            }
                            idx += 1;
                        }
                    }
                    modules_json.push(format!(
                        r#"{{"name":"{}","line":{},"col":{}}}"#,
                        json_escape(&mname),
                        mline,
                        mcol
                    ));
                }
            }
            "use" | "import" => {
                idx += 1;
                let mut path_parts: Vec<String> = Vec::new();
                let line = tok.line;
                let col = tok.col;
                while idx < tokens.len()
                    && (tokens[idx].kind == "IDENT" || tokens[idx].kind == "KEYWORD")
                {
                    path_parts.push(tokens[idx].value.clone());
                    idx += 1;
                    if idx < tokens.len() && tokens[idx].value == "::" {
                        idx += 1;
                    } else {
                        break;
                    }
                }
                let mut alias: Option<String> = None;
                if idx + 1 < tokens.len() && tokens[idx].value == "as" && idx + 1 < tokens.len() {
                    idx += 1;
                    if tokens[idx].kind == "IDENT" || tokens[idx].kind == "KEYWORD" {
                        alias = Some(tokens[idx].value.clone());
                        idx += 1;
                    }
                }
                while idx < tokens.len() && tokens[idx].value != ";" && tokens[idx].kind != "EOF" {
                    idx += 1;
                }
                if idx < tokens.len() && tokens[idx].value == ";" {
                    idx += 1;
                }
                imports_json.push(format!(
                    r#"{{"path":[{}],"alias":{},"line":{},"col":{}}}"#,
                    path_parts
                        .iter()
                        .map(|p| format!("\"{}\"", json_escape(p)))
                        .collect::<Vec<_>>()
                        .join(","),
                    alias
                        .as_ref()
                        .map_or("null".to_string(), |a| format!("\"{}\"", json_escape(a))),
                    line,
                    col
                ));
            }
            "const" => {
                idx += 1;
                while idx < tokens.len() && tokens[idx].value != ";" && tokens[idx].kind != "EOF" {
                    idx += 1;
                }
                if idx < tokens.len() && tokens[idx].value == ";" {
                    idx += 1;
                }
            }
            "extern" => {
                idx += 1;
                let mut depth = 0;
                while idx < tokens.len() {
                    if tokens[idx].value == "{" {
                        depth += 1;
                    } else if tokens[idx].value == "}" {
                        if depth == 0 {
                            idx += 1;
                            break;
                        }
                        depth -= 1;
                    }
                    if depth == 0 && tokens[idx].value == ";" {
                        idx += 1;
                        break;
                    }
                    idx += 1;
                }
            }
            "trait" | "impl" => {
                idx += 1;
                let mut depth = 0;
                while idx < tokens.len() {
                    if tokens[idx].value == "{" {
                        depth += 1;
                    } else if tokens[idx].value == "}" {
                        if depth == 0 {
                            idx += 1;
                            break;
                        }
                        depth -= 1;
                    }
                    if depth == 0 && tokens[idx].value == ";" {
                        idx += 1;
                        break;
                    }
                    idx += 1;
                }
            }
            _ => {
                idx += 1;
            }
        }
    }

    out.push(']');
    if !types_json.is_empty() {
        out.push_str(&format!(r#","types":[{}]"#, types_json.join(",")));
    } else {
        out.push_str(",\"types\":[]");
    }
    if !modules_json.is_empty() {
        out.push_str(&format!(r#","modules":[{}]"#, modules_json.join(",")));
    }
    if !imports_json.is_empty() {
        out.push_str(&format!(r#","imports":[{}]"#, imports_json.join(",")));
    } else {
        out.push_str(",\"imports\":[]");
    }
    out.push_str(&format!(
        r#","has_main":{}}}"#,
        if has_main { "true" } else { "false" }
    ));
    out
}

fn parse_func_decl(tokens: &[MimiToken], start: usize, is_pub: bool) -> (String, usize, bool) {
    let mut idx = start;
    let line = tokens[idx].line;
    let col = tokens[idx].col;

    if tokens[idx].value == "pub" {
        idx += 1;
    }

    let mut is_comptime = false;
    let mut is_async = false;
    if idx < tokens.len() && tokens[idx].kind == "KEYWORD" {
        if tokens[idx].value == "comptime" {
            is_comptime = true;
            idx += 1;
        } else if tokens[idx].value == "async" {
            is_async = true;
            idx += 1;
        }
    }

    if idx >= tokens.len() || tokens[idx].value != "func" {
        return (String::new(), 1, false);
    }
    idx += 1;

    let mut name = String::from("_");
    if idx < tokens.len() && (tokens[idx].kind == "IDENT" || tokens[idx].kind == "KEYWORD") {
        name = tokens[idx].value.clone();
        idx += 1;
    }
    let is_main = name == "main";

    if idx < tokens.len() && tokens[idx].value == "<" {
        let mut depth = 1;
        idx += 1;
        while idx < tokens.len() && depth > 0 {
            if tokens[idx].value == "<" {
                depth += 1;
            } else if tokens[idx].value == ">" {
                depth -= 1;
            }
            idx += 1;
        }
    }

    let mut params_json = Vec::new();
    let mut has_body = false;
    let mut body_end_line = line;

    if idx < tokens.len() && tokens[idx].value == "(" {
        idx += 1;
        while idx < tokens.len() && tokens[idx].value != ")" {
            if tokens[idx].value == "," {
                idx += 1;
                continue;
            }
            let pline = tokens[idx].line;
            let pcol = tokens[idx].col;
            let mut pname = String::from("_");
            let mut is_mut_param = false;
            if idx < tokens.len() && tokens[idx].value == "mut" {
                is_mut_param = true;
                idx += 1;
            }
            if idx < tokens.len() && (tokens[idx].kind == "IDENT" || tokens[idx].kind == "KEYWORD")
            {
                pname = tokens[idx].value.clone();
                idx += 1;
            }
            if idx < tokens.len() && tokens[idx].value == ":" {
                idx += 1;
                let mut ptype = String::new();
                while idx < tokens.len()
                    && !matches!(tokens[idx].value.as_str(), "," | ")" | "=")
                    && tokens[idx].kind != "EOF"
                {
                    ptype.push_str(&tokens[idx].value);
                    idx += 1;
                }
                params_json.push(format!(
                    r#"{{"name":"{}","type":"{}","mut":{},"line":{},"col":{}}}"#,
                    json_escape(&pname),
                    json_escape(ptype.trim()),
                    is_mut_param,
                    pline,
                    pcol
                ));
            } else {
                params_json.push(format!(
                    r#"{{"name":"{}","type":"_","mut":{},"line":{},"col":{}}}"#,
                    json_escape(&pname),
                    is_mut_param,
                    pline,
                    pcol
                ));
            }
            if idx < tokens.len() && tokens[idx].value == "=" {
                idx += 1;
                let mut depth = 0;
                while idx < tokens.len() {
                    if matches!(tokens[idx].value.as_str(), "(" | "{" | "[") {
                        depth += 1;
                    } else if matches!(tokens[idx].value.as_str(), ")" | "}" | "]") {
                        if depth == 0 {
                            break;
                        }
                        depth -= 1;
                    }
                    if depth == 0 && matches!(tokens[idx].value.as_str(), "," | ")") {
                        break;
                    }
                    idx += 1;
                }
            }
        }
        if idx < tokens.len() && tokens[idx].value == ")" {
            idx += 1;
        }
    }

    let mut ret_type = String::new();
    if idx < tokens.len() && tokens[idx].value == "->" {
        idx += 1;
        while idx < tokens.len()
            && !matches!(tokens[idx].value.as_str(), "{" | "where")
            && tokens[idx].kind != "EOF"
        {
            ret_type.push_str(&tokens[idx].value);
            idx += 1;
        }
    }

    if idx < tokens.len() && tokens[idx].value == "where" {
        while idx < tokens.len() && tokens[idx].value != "{" && tokens[idx].kind != "EOF" {
            idx += 1;
        }
    }

    let mut stmts_json = Vec::new();
    if idx < tokens.len() && tokens[idx].value == "{" {
        let body_start = idx;
        let mut depth = 1;
        idx += 1;
        while idx < tokens.len() && depth > 0 {
            if tokens[idx].value == "{" {
                depth += 1;
            } else if tokens[idx].value == "}" {
                depth -= 1;
            }
            if depth > 0 {
                idx += 1;
            }
        }
        if idx < tokens.len() {
            body_end_line = tokens[idx].line;
            idx += 1;
        }
        has_body = true;

        let mut bi = body_start + 1;
        let mut body_depth = 1;
        while bi < idx - 1 && body_depth > 0 {
            if tokens[bi].value == "{" {
                body_depth += 1;
                bi += 1;
                continue;
            }
            if tokens[bi].value == "}" {
                body_depth -= 1;
                bi += 1;
                continue;
            }
            if body_depth != 1 {
                bi += 1;
                continue;
            }

            match tokens[bi].kind.as_str() {
                "KEYWORD" => {
                    let stmt_line = tokens[bi].line;
                    let stmt_col = tokens[bi].col;
                    match tokens[bi].value.as_str() {
                        "let" => {
                            bi += 1;
                            let mut is_mut = false;
                            let mut sname = String::new();
                            if bi < tokens.len() && tokens[bi].value == "mut" {
                                is_mut = true;
                                bi += 1;
                            }
                            if bi < tokens.len() && tokens[bi].kind == "IDENT" {
                                sname = tokens[bi].value.clone();
                                bi += 1;
                            }
                            if bi < tokens.len() && tokens[bi].value == ":" {
                                bi += 1;
                                while bi < tokens.len()
                                    && !matches!(tokens[bi].value.as_str(), "=" | ";" | "{" | "}")
                                    && tokens[bi].kind != "EOF"
                                {
                                    bi += 1;
                                }
                            }
                            if bi < tokens.len() && tokens[bi].value == "=" {
                                bi += 1;
                                let mut ed = 0;
                                while bi < tokens.len() {
                                    if matches!(tokens[bi].value.as_str(), "{" | "(" | "[") {
                                        ed += 1;
                                    } else if matches!(tokens[bi].value.as_str(), "}" | ")" | "]") {
                                        if ed == 0 {
                                            break;
                                        }
                                        ed -= 1;
                                    }
                                    if ed == 0 && tokens[bi].value == ";" {
                                        break;
                                    }
                                    bi += 1;
                                }
                            }
                            if bi < tokens.len() && tokens[bi].value == ";" {
                                bi += 1;
                            }
                            stmts_json.push(format!(
                                r#"{{"kind":"let","name":"{}","mut":{},"line":{},"col":{}}}"#,
                                json_escape(&sname),
                                is_mut,
                                stmt_line,
                                stmt_col
                            ));
                        }
                        "return" => {
                            bi += 1;
                            while bi < tokens.len()
                                && !matches!(tokens[bi].value.as_str(), ";" | "}")
                                && tokens[bi].kind != "EOF"
                            {
                                if tokens[bi].value == "{" {
                                    let mut d = 1;
                                    bi += 1;
                                    while bi < tokens.len() && d > 0 {
                                        if tokens[bi].value == "{" {
                                            d += 1;
                                        } else if tokens[bi].value == "}" {
                                            d -= 1;
                                        }
                                        bi += 1;
                                    }
                                } else {
                                    bi += 1;
                                }
                            }
                            if bi < tokens.len() && tokens[bi].value == ";" {
                                bi += 1;
                            }
                            stmts_json.push(format!(
                                r#"{{"kind":"return","line":{},"col":{}}}"#,
                                stmt_line, stmt_col
                            ));
                        }
                        "if" | "while" | "for" | "loop" => {
                            let sk = tokens[bi].value.clone();
                            bi += 1;
                            let mut ed = 0;
                            while bi < tokens.len() {
                                if tokens[bi].value == "{" {
                                    bi += 1;
                                    let mut d = 1;
                                    while bi < tokens.len() && d > 0 {
                                        if tokens[bi].value == "{" {
                                            d += 1;
                                        } else if tokens[bi].value == "}" {
                                            d -= 1;
                                        }
                                        if d > 0 {
                                            bi += 1;
                                        }
                                    }
                                    break;
                                }
                                if tokens[bi].value == "(" {
                                    ed += 1;
                                } else if tokens[bi].value == ")" {
                                    if ed == 0 {
                                        bi += 1;
                                        break;
                                    }
                                    ed -= 1;
                                }
                                bi += 1;
                            }
                            if sk == "if" {
                                let bi2 = bi + 1;
                                if bi2 < tokens.len() && tokens[bi2].value == "else" {
                                    bi = bi2 + 1;
                                    if bi < tokens.len() && tokens[bi].value == "if" {
                                        bi += 1;
                                        while bi < tokens.len() && tokens[bi].value != "{" {
                                            bi += 1;
                                        }
                                        if bi < tokens.len() && tokens[bi].value == "{" {
                                            bi += 1;
                                            let mut d = 1;
                                            while bi < tokens.len() && d > 0 {
                                                if tokens[bi].value == "{" {
                                                    d += 1;
                                                } else if tokens[bi].value == "}" {
                                                    d -= 1;
                                                }
                                                if d > 0 {
                                                    bi += 1;
                                                }
                                            }
                                        }
                                    } else if bi < tokens.len() && tokens[bi].value == "{" {
                                        bi += 1;
                                        let mut d = 1;
                                        while bi < tokens.len() && d > 0 {
                                            if tokens[bi].value == "{" {
                                                d += 1;
                                            } else if tokens[bi].value == "}" {
                                                d -= 1;
                                            }
                                            if d > 0 {
                                                bi += 1;
                                            }
                                        }
                                    }
                                }
                            }
                            stmts_json.push(format!(
                                r#"{{"kind":"{}","line":{},"col":{}}}"#,
                                sk, stmt_line, stmt_col
                            ));
                        }
                        "break" | "continue" => {
                            let sk = tokens[bi].value.clone();
                            bi += 1;
                            if bi < tokens.len() && tokens[bi].value == ";" {
                                bi += 1;
                            }
                            stmts_json.push(format!(
                                r#"{{"kind":"{}","line":{},"col":{}}}"#,
                                sk, stmt_line, stmt_col
                            ));
                        }
                        "requires" | "ensures" | "desc" | "rule" => {
                            let sk = tokens[bi].value.clone();
                            bi += 1;
                            while bi < tokens.len()
                                && !matches!(tokens[bi].value.as_str(), ";" | "{" | "}")
                                && tokens[bi].kind != "EOF"
                            {
                                bi += 1;
                            }
                            if bi < tokens.len() && tokens[bi].value == ";" {
                                bi += 1;
                            }
                            stmts_json.push(format!(
                                r#"{{"kind":"{}","line":{},"col":{}}}"#,
                                sk, stmt_line, stmt_col
                            ));
                        }
                        "mms" => {
                            bi += 1;
                            if bi < tokens.len() && tokens[bi].value == "{" {
                                bi += 1;
                                let mut d = 1;
                                while bi < tokens.len() && d > 0 {
                                    if tokens[bi].value == "{" {
                                        d += 1;
                                    } else if tokens[bi].value == "}" {
                                        d -= 1;
                                    }
                                    bi += 1;
                                }
                            }
                            stmts_json.push(format!(
                                r#"{{"kind":"mms","line":{},"col":{}}}"#,
                                stmt_line, stmt_col
                            ));
                        }
                        _ => {
                            bi += 1;
                        }
                    }
                }
                _ => {
                    bi += 1;
                }
            }
        }
    }

    let name_esc = json_escape(&name);
    let ret_esc = json_escape(ret_type.trim());
    let params_s = params_json.join(",");
    let mut json = format!(
        r#"{{"name":"{}","line":{},"col":{},"is_pub":{},"is_comptime":{},"is_async":{},"params":[{}],"return_type":"{}","has_body":{},"body_end_line":{}"#,
        name_esc,
        line,
        col,
        if is_pub { "true" } else { "false" },
        if is_comptime { "true" } else { "false" },
        if is_async { "true" } else { "false" },
        params_s,
        ret_esc,
        if has_body { "true" } else { "false" },
        body_end_line
    );

    if !stmts_json.is_empty() {
        json.push_str(&format!(r#","stmts":[{}]"#, stmts_json.join(",")));
    } else {
        json.push_str(r#","stmts":[]"#);
    }

    json.push('}');
    (json, idx - start, is_main)
}
