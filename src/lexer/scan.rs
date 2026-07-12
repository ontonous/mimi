#![cfg_attr(not(test), allow(dead_code))]

use crate::lexer::errors::{
    dedent_mismatch, indent_not_multiple_of_four, invalid_escape, tabs_not_allowed,
    unexpected_character, unexpected_dollar, unterminated_block_comment, unterminated_escape,
    unterminated_fstring, unterminated_fstring_escape, unterminated_interpolation,
    unterminated_string, LexerError,
};
use crate::lexer::keywords::keyword_or_ident;
use crate::lexer::token::{LexerMode, Token, TokenKind};

impl<'a> super::Lexer<'a> {
    pub(crate) fn advance(&mut self) -> Option<char> {
        let c = self.peeked;
        self.peeked = self.chars.next();
        if let Some(ch) = c {
            if ch == '\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
        }
        c
    }

    pub(crate) fn peek(&self) -> Option<char> {
        self.peeked
    }

    pub(crate) fn skip_line_comment(&mut self) {
        while let Some(c) = self.peek() {
            if c == '\n' {
                break;
            }
            self.advance();
        }
    }

    /// Skip a block comment `/* ... */`, supporting nesting.
    pub(crate) fn skip_block_comment(&mut self) -> Result<(), LexerError> {
        // consume '/*'
        self.advance();
        self.advance();
        let mut depth: i32 = 1;
        while depth > 0 {
            match self.peek() {
                None => return Err(unterminated_block_comment(self.line, self.col)),
                Some('*') => {
                    self.advance();
                    if self.peek() == Some('/') {
                        self.advance();
                        depth -= 1;
                    }
                }
                Some('/') => {
                    self.advance();
                    if self.peek() == Some('*') {
                        self.advance();
                        depth += 1;
                    }
                }
                Some(_) => {
                    self.advance();
                }
            }
        }
        Ok(())
    }

    pub(crate) fn skip_whitespace_inline(&mut self) {
        while let Some(c) = self.peek() {
            if c == ' ' || c == '\t' || c == '\r' {
                self.advance();
            } else {
                break;
            }
        }
    }

    pub(crate) fn scan_string(&mut self) -> Result<String, LexerError> {
        // consume opening quote
        self.advance();
        let mut s = String::new();
        loop {
            match self.peek() {
                None => return Err(unterminated_string(self.line, self.col)),
                Some('"') => {
                    self.advance();
                    break;
                }
                Some('\\') => {
                    self.advance();
                    match self.peek() {
                        Some('n') => {
                            s.push('\n');
                            self.advance();
                        }
                        Some('t') => {
                            s.push('\t');
                            self.advance();
                        }
                        Some('r') => {
                            s.push('\r');
                            self.advance();
                        }
                        Some('\\') => {
                            s.push('\\');
                            self.advance();
                        }
                        Some('"') => {
                            s.push('"');
                            self.advance();
                        }
                        Some('0') => {
                            s.push('\0');
                            self.advance();
                        }
                        Some('x') => {
                            // LE-C1: parse \xNN hex escape, produce byte value
                            let start_col = self.col;
                            self.advance();
                            let mut hex = String::with_capacity(2);
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(c) if c.is_ascii_hexdigit() => {
                                        hex.push(c);
                                        self.advance();
                                    }
                                    _ => break,
                                }
                            }
                            if hex.len() != 2 {
                                return Err(invalid_escape("\\x", self.line, start_col));
                            }
                            // SAFETY: hex.len() == 2 and all chars are ASCII hexdigits
                            // (checked above), so from_str_radix is infallible here.
                            let value = u8::from_str_radix(&hex, 16).map_err(|e| {
                                invalid_escape(&format!("\\x{}", e), self.line, start_col)
                            })?;
                            s.push(value as char);
                        }
                        Some('u') => {
                            let start_col = self.col;
                            self.advance();
                            let mut code = String::new();
                            match self.peek() {
                                Some('{') => {
                                    self.advance();
                                    while let Some(c) = self.peek() {
                                        if c.is_ascii_hexdigit() || c == '_' {
                                            code.push(c);
                                            self.advance();
                                        } else {
                                            break;
                                        }
                                    }
                                    if self.peek() != Some('}') {
                                        return Err(invalid_escape("\\u{", self.line, start_col));
                                    }
                                    if code.is_empty() {
                                        return Err(invalid_escape("\\u{}", self.line, start_col));
                                    }
                                    self.advance();
                                }
                                _ => {
                                    for _ in 0..4 {
                                        match self.peek() {
                                            Some(c) if c.is_ascii_hexdigit() => {
                                                code.push(c);
                                                self.advance();
                                            }
                                            _ => break,
                                        }
                                    }
                                    if code.len() != 4 {
                                        return Err(invalid_escape("\\u", self.line, start_col));
                                    }
                                }
                            }
                            let cleaned: String = code.chars().filter(|c| *c != '_').collect();
                            // SAFETY: cleaned contains only ASCII hex digits and
                            // has a length validated by the caller, so the parse
                            // is infallible.
                            let value = u32::from_str_radix(&cleaned, 16).map_err(|e| {
                                invalid_escape(&format!("\\u{}", e), self.line, start_col)
                            })?;
                            match char::from_u32(value) {
                                Some(ch) => s.push(ch),
                                None => return Err(invalid_escape("\\u", self.line, start_col)),
                            }
                        }
                        Some(c) => {
                            return Err(invalid_escape(&format!("\\{}", c), self.line, self.col));
                        }
                        None => return Err(unterminated_escape(self.line, self.col)),
                    }
                }
                Some(c) => {
                    s.push(c);
                    self.advance();
                }
            }
        }
        Ok(s)
    }

    /// Scan an f-string: f"text {expr} text"
    /// Returns the raw content string (with {expr} preserved for parser)
    pub(crate) fn scan_fstring(&mut self) -> Result<String, LexerError> {
        // consume 'f' and opening quote
        self.advance(); // 'f'
        self.advance(); // '"'
        let mut s = String::new();
        loop {
            match self.peek() {
                None => return Err(unterminated_fstring(self.line, self.col)),
                Some('"') => {
                    self.advance();
                    break;
                }
                Some('\\') => {
                    self.advance();
                    match self.peek() {
                        Some('n') => {
                            // Decode common escapes immediately so f-string literals
                            // match the semantics of ordinary string literals.
                            s.push('\n');
                            self.advance();
                        }
                        Some('t') => {
                            s.push('\t');
                            self.advance();
                        }
                        Some('r') => {
                            s.push('\r');
                            self.advance();
                        }
                        // Keep backslash escapes that the parser needs to see in
                        // order to distinguish escaped braces from interpolations.
                        Some('\\') => {
                            s.push_str("\\\\");
                            self.advance();
                        }
                        Some('"') => {
                            s.push('"');
                            self.advance();
                        }
                        Some('{') => {
                            s.push_str("\\{");
                            self.advance();
                        }
                        Some('}') => {
                            s.push_str("\\}");
                            self.advance();
                        }
                        Some('0') => {
                            s.push_str("\\0");
                            self.advance();
                        }
                        Some('x') => {
                            s.push_str("\\x");
                            let start_col = self.col;
                            self.advance();
                            let mut got = 0;
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(c) if c.is_ascii_hexdigit() => {
                                        s.push(c);
                                        self.advance();
                                        got += 1;
                                    }
                                    _ => break,
                                }
                            }
                            if got != 2 {
                                return Err(invalid_escape("\\x", self.line, start_col));
                            }
                        }
                        Some('u') => {
                            s.push_str("\\u");
                            let start_col = self.col;
                            self.advance();
                            if self.peek() == Some('{') {
                                s.push('{');
                                self.advance();
                                let hex_start = s.len();
                                while let Some(c) = self.peek() {
                                    if c.is_ascii_hexdigit() || c == '_' {
                                        s.push(c);
                                        self.advance();
                                    } else {
                                        break;
                                    }
                                }
                                if self.peek() != Some('}') {
                                    return Err(invalid_escape("\\u{", self.line, start_col));
                                }
                                if s.len() == hex_start {
                                    return Err(invalid_escape("\\u{}", self.line, start_col));
                                }
                                s.push('}');
                                self.advance();
                            } else {
                                let mut got = 0;
                                for _ in 0..4 {
                                    match self.peek() {
                                        Some(c) if c.is_ascii_hexdigit() => {
                                            s.push(c);
                                            self.advance();
                                            got += 1;
                                        }
                                        _ => break,
                                    }
                                }
                                if got != 4 {
                                    return Err(invalid_escape("\\u", self.line, start_col));
                                }
                            }
                        }
                        Some(c) => {
                            return Err(invalid_escape(&format!("\\{}", c), self.line, self.col));
                        }
                        None => return Err(unterminated_fstring_escape(self.line, self.col)),
                    }
                }
                Some('{') => {
                    // interpolation start - track braces
                    s.push('{');
                    self.advance();
                    let mut depth = 1;
                    while let Some(c) = self.peek() {
                        if c == '{' {
                            depth += 1;
                        } else if c == '}' {
                            depth -= 1;
                            if depth == 0 {
                                s.push('}');
                                self.advance();
                                break;
                            }
                        }
                        s.push(c);
                        self.advance();
                    }
                    if depth != 0 {
                        return Err(unterminated_interpolation(self.line, self.col));
                    }
                }
                Some(c) => {
                    s.push(c);
                    self.advance();
                }
            }
        }
        Ok(s)
    }

    pub(crate) fn scan_number(&mut self) -> TokenKind {
        let mut s = String::new();
        let mut is_float = false;
        // Check for 0x (hex), 0b (binary), 0o (octal) prefix
        if let Some('0') = self.peek() {
            let mut tmp = self.chars.clone();
            let next = tmp.next();
            match next {
                Some('x') | Some('X') => {
                    s.push('0');
                    self.advance();
                    s.push('x');
                    self.advance();
                    let digit_start = s.len();
                    while let Some(c) = self.peek() {
                        if c.is_ascii_hexdigit() || c == '_' {
                            s.push(c);
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    // HIGH fix: reject "0x" with no hex digits.
                    // Keep the prefix so the parser can produce a clear error.
                    if s.len() == digit_start {
                        // No digits after prefix — emit as error token.
                        return TokenKind::Int(s); // parser will report invalid hex
                    }
                    return TokenKind::Int(s);
                }
                Some('b') | Some('B') => {
                    s.push('0');
                    self.advance();
                    s.push('b');
                    self.advance();
                    let digit_start = s.len();
                    while let Some(c) = self.peek() {
                        if c == '0' || c == '1' || c == '_' {
                            s.push(c);
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    // HIGH fix: reject "0b" with no binary digits.
                    if s.len() == digit_start {
                        return TokenKind::Int(s); // parser will report invalid binary
                    }
                    return TokenKind::Int(s);
                }
                Some('o') | Some('O') => {
                    s.push('0');
                    self.advance();
                    s.push('o');
                    self.advance();
                    let digit_start = s.len();
                    while let Some(c) = self.peek() {
                        if c.is_ascii_digit() && c != '8' && c != '9' || c == '_' {
                            s.push(c);
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    // HIGH fix: reject "0o" with no octal digits.
                    if s.len() == digit_start {
                        return TokenKind::Int(s); // parser will report invalid octal
                    }
                    return TokenKind::Int(s);
                }
                _ => {}
            }
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                s.push(c);
                self.advance();
            } else if c == '.' {
                if is_float {
                    break;
                }
                // check next is digit
                let mut tmp = self.chars.clone();
                if tmp.next().map(|x| x.is_ascii_digit()).unwrap_or(false) {
                    is_float = true;
                    s.push(c);
                    self.advance();
                } else {
                    break;
                }
            } else if c == '_' {
                s.push(c);
                self.advance();
            } else {
                break;
            }
        }
        // LE-H4: Scientific notation: 1e5, 1.5e-3, 2E+10
        if let Some(ch) = self.peek() {
            if ch == 'e' || ch == 'E' {
                // HIGH fix: "1e" without following digits should not consume 'e'.
                // self.chars points to characters AFTER the peeked 'e'/'E'.
                let mut tmp = self.chars.clone();
                let first_after_e = tmp.next();
                let first_digit = if first_after_e == Some('+') || first_after_e == Some('-') {
                    tmp.next()
                } else {
                    first_after_e
                };
                if first_digit.map_or(false, |d| d.is_ascii_digit()) {
                    s.push(ch);
                    self.advance();
                    // Optional sign
                    if let Some(sign) = self.peek() {
                        if sign == '+' || sign == '-' {
                            s.push(sign);
                            self.advance();
                        }
                    }
                    is_float = true;
                    while let Some(d) = self.peek() {
                        if d.is_ascii_digit() || d == '_' {
                            s.push(d);
                            self.advance();
                        } else {
                            break;
                        }
                    }
                }
                // If no valid digit follows 'e', don't consume it — leave as
                // start of an identifier token.
            }
        }
        if is_float {
            TokenKind::Float(s)
        } else {
            TokenKind::Int(s)
        }
    }

    pub(crate) fn scan_ident(&mut self, first: char) -> String {
        let mut s = String::new();
        s.push(first);
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' {
                s.push(c);
                self.advance();
            } else {
                break;
            }
        }
        s
    }

    pub(crate) fn process_line_start(&mut self, tokens: &mut Vec<Token>) -> Result<(), LexerError> {
        if !self.at_line_start {
            return Ok(());
        }
        loop {
            let mut spaces = 0usize;
            while self.peek() == Some(' ') {
                self.advance();
                spaces += 1;
            }
            if self.peek() == Some('\t') {
                return Err(tabs_not_allowed(self.line, self.col));
            }
            if self.peek().is_none() {
                return Ok(());
            }
            let mut is_comment_line = false;
            if self.peek() == Some('/') {
                let next = self.chars.clone().next();
                if next == Some('/') {
                    is_comment_line = true;
                    self.skip_line_comment();
                } else if next == Some('*') {
                    self.skip_block_comment()?;
                    self.skip_whitespace_inline();
                    if self.peek() == Some('\n') || self.peek().is_none() {
                        is_comment_line = true;
                    }
                }
            }
            let is_blank = self.peek() == Some('\n');
            if is_comment_line || is_blank {
                if self.peek() == Some('\n') {
                    self.advance();
                }
                continue;
            }
            // real content
            if self.mode == LexerMode::Sketch {
                #[allow(clippy::incompatible_msrv)]
                if !spaces.is_multiple_of(4) {
                    return Err(indent_not_multiple_of_four(self.line, self.col));
                }
                let current = *self.indent_stack.last().unwrap_or(&0);
                if spaces > current {
                    self.indent_stack.push(spaces);
                    tokens.push(Token {
                        kind: TokenKind::Indent,
                        line: self.line,
                        col: spaces,
                    });
                } else if spaces < current {
                    while *self.indent_stack.last().unwrap_or(&0) > spaces {
                        self.indent_stack.pop();
                        tokens.push(Token {
                            kind: TokenKind::Dedent,
                            line: self.line,
                            col: spaces,
                        });
                    }
                    if *self.indent_stack.last().unwrap_or(&0) != spaces {
                        return Err(dedent_mismatch(self.line, self.col));
                    }
                }
            }
            self.at_line_start = false;
            return Ok(());
        }
    }

    pub(crate) fn flush_indent(&mut self, tokens: &mut Vec<Token>) {
        if self.mode == LexerMode::Sketch {
            while self.indent_stack.len() > 1 {
                self.indent_stack.pop();
                tokens.push(Token {
                    kind: TokenKind::Dedent,
                    line: self.line,
                    col: self.col,
                });
            }
        }
    }

    pub(crate) fn scan_token(
        &mut self,
        c: char,
        line: usize,
        col: usize,
    ) -> Result<TokenKind, LexerError> {
        match c {
            'f' if self.chars.clone().next() == Some('"') => {
                let s = self.scan_fstring()?;
                Ok(TokenKind::FString(s))
            }
            '"' => {
                let s = self.scan_string()?;
                Ok(TokenKind::String(s))
            }
            '0'..='9' => Ok(self.scan_number()),
            'a'..='z' | 'A'..='Z' | '_' => {
                // SAFETY: dispatch matched `peek() == Some(first_char)`, so the
                // stream cannot be empty here.
                let first = self.advance().expect("dispatch on peek guaranteed Some");
                let name = self.scan_ident(first);
                Ok(keyword_or_ident(&name))
            }
            '+' => {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    Ok(TokenKind::PlusEq)
                } else {
                    Ok(TokenKind::Plus)
                }
            }
            '-' => {
                self.advance();
                if self.peek() == Some('>') {
                    self.advance();
                    Ok(TokenKind::Arrow)
                } else if self.peek() == Some('=') {
                    self.advance();
                    Ok(TokenKind::MinusEq)
                } else {
                    Ok(TokenKind::Minus)
                }
            }
            '*' => {
                self.advance();
                if self.peek() == Some('*') {
                    self.advance();
                    Ok(TokenKind::Pow)
                } else if self.peek() == Some('=') {
                    self.advance();
                    Ok(TokenKind::StarEq)
                } else {
                    Ok(TokenKind::Star)
                }
            }
            '/' => {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    Ok(TokenKind::SlashEq)
                } else {
                    Ok(TokenKind::Slash)
                }
            }
            '%' => {
                self.advance();
                Ok(TokenKind::Percent)
            }
            '=' => {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    Ok(TokenKind::EqEq)
                } else if self.peek() == Some('>') {
                    self.advance();
                    Ok(TokenKind::FatArrow)
                } else {
                    Ok(TokenKind::Eq)
                }
            }
            '!' => {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    Ok(TokenKind::Ne)
                } else {
                    Ok(TokenKind::Bang)
                }
            }
            '<' => {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    Ok(TokenKind::Le)
                } else if self.peek() == Some('<') {
                    self.advance();
                    Ok(TokenKind::Shl)
                } else {
                    Ok(TokenKind::Lt)
                }
            }
            '>' => {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    Ok(TokenKind::Ge)
                } else if self.peek() == Some('>') {
                    self.advance();
                    Ok(TokenKind::Shr)
                } else {
                    Ok(TokenKind::Gt)
                }
            }
            '&' => {
                self.advance();
                if self.peek() == Some('&') {
                    self.advance();
                    Ok(TokenKind::AndAnd)
                } else if self.peek() == Some('=') {
                    self.advance();
                    Ok(TokenKind::BitAndEq)
                } else {
                    Ok(TokenKind::BitAnd)
                }
            }
            '|' => {
                self.advance();
                if self.peek() == Some('|') {
                    self.advance();
                    Ok(TokenKind::OrOr)
                } else if self.peek() == Some('=') {
                    self.advance();
                    Ok(TokenKind::BitOrEq)
                } else if self.peek() == Some('>') {
                    self.advance();
                    Ok(TokenKind::PipeArrow)
                } else {
                    Ok(TokenKind::BitOr)
                }
            }
            '^' => {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    Ok(TokenKind::BitXorEq)
                } else {
                    Ok(TokenKind::BitXor)
                }
            }
            '~' => {
                self.advance();
                Ok(TokenKind::Tilde)
            }
            '$' => {
                self.advance();
                if self.peek() == Some('(') {
                    self.advance();
                    Ok(TokenKind::DollarParen)
                } else {
                    Err(unexpected_dollar(line, col))
                }
            }
            '(' => {
                self.advance();
                Ok(TokenKind::LParen)
            }
            ')' => {
                self.advance();
                Ok(TokenKind::RParen)
            }
            '{' => {
                self.advance();
                Ok(TokenKind::LBrace)
            }
            '}' => {
                self.advance();
                Ok(TokenKind::RBrace)
            }
            '[' => {
                self.advance();
                Ok(TokenKind::LBracket)
            }
            ']' => {
                self.advance();
                Ok(TokenKind::RBracket)
            }
            ':' => {
                self.advance();
                if self.peek() == Some(':') {
                    self.advance();
                    Ok(TokenKind::ColonColon)
                } else {
                    Ok(TokenKind::Colon)
                }
            }
            ';' => {
                self.advance();
                Ok(TokenKind::Semi)
            }
            ',' => {
                self.advance();
                Ok(TokenKind::Comma)
            }
            '.' => {
                self.advance();
                if self.peek() == Some('.') && self.chars.clone().next() == Some('.') {
                    self.advance();
                    self.advance();
                    Ok(TokenKind::Ellipsis)
                } else if self.peek() == Some('.') {
                    self.advance();
                    Ok(TokenKind::DotDot)
                } else {
                    Ok(TokenKind::Dot)
                }
            }
            '?' => {
                self.advance();
                Ok(TokenKind::Question)
            }
            '@' => {
                self.advance();
                Ok(TokenKind::At)
            }
            '#' => {
                self.advance();
                Ok(TokenKind::Hash)
            }
            '\'' => {
                self.advance();
                Ok(TokenKind::Tick)
            }
            _ => Err(unexpected_character(c, line, col)),
        }
    }
}
