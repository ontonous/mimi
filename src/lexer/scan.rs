use crate::lexer::errors::{
    dedent_mismatch, indent_not_multiple_of_four, tabs_not_allowed, unexpected_character,
    unexpected_dollar, unterminated_escape, unterminated_fstring, unterminated_fstring_escape,
    unterminated_interpolation, unterminated_string, LexerError,
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
                None => return Err(unterminated_string()),
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
                        Some(c) => {
                            s.push(c);
                            self.advance();
                        }
                        None => return Err(unterminated_escape()),
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
                None => return Err(unterminated_fstring()),
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
                            self.advance();
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(c) if c.is_ascii_hexdigit() => {
                                        s.push(c);
                                        self.advance();
                                    }
                                    _ => break,
                                }
                            }
                        }
                        Some('u') => {
                            s.push_str("\\u");
                            self.advance();
                            if self.peek() == Some('{') {
                                s.push('{');
                                self.advance();
                                while let Some(c) = self.peek() {
                                    if c.is_ascii_hexdigit() || c == '_' {
                                        s.push(c);
                                        self.advance();
                                    } else {
                                        break;
                                    }
                                }
                                if self.peek() == Some('}') {
                                    s.push('}');
                                    self.advance();
                                }
                            } else {
                                for _ in 0..4 {
                                    match self.peek() {
                                        Some(c) if c.is_ascii_hexdigit() => {
                                            s.push(c);
                                            self.advance();
                                        }
                                        _ => break,
                                    }
                                }
                            }
                        }
                        Some(c) => {
                            s.push(c);
                            self.advance();
                        }
                        None => return Err(unterminated_fstring_escape()),
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
                        return Err(unterminated_interpolation());
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
                    while let Some(c) = self.peek() {
                        if c.is_ascii_hexdigit() || c == '_' {
                            s.push(c);
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    return TokenKind::Int(s);
                }
                Some('b') | Some('B') => {
                    s.push('0');
                    self.advance();
                    s.push('b');
                    self.advance();
                    while let Some(c) = self.peek() {
                        if c == '0' || c == '1' || c == '_' {
                            s.push(c);
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    return TokenKind::Int(s);
                }
                Some('o') | Some('O') => {
                    s.push('0');
                    self.advance();
                    s.push('o');
                    self.advance();
                    while let Some(c) = self.peek() {
                        if c.is_ascii_digit() && c != '8' && c != '9' || c == '_' {
                            s.push(c);
                            self.advance();
                        } else {
                            break;
                        }
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
            let is_comment =
                self.peek() == Some('/') && self.chars.clone().next() == Some('/');
            let is_blank = self.peek() == Some('\n');
            if is_comment || is_blank {
                if is_comment {
                    self.skip_line_comment();
                }
                if self.peek() == Some('\n') {
                    self.advance();
                }
                // continue to next line
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
                    tokens.push(Token { kind: TokenKind::Indent, line: self.line, col: spaces });
                } else if spaces < current {
                    while *self.indent_stack.last().unwrap_or(&0) > spaces {
                        self.indent_stack.pop();
                        tokens.push(Token { kind: TokenKind::Dedent, line: self.line, col: spaces });
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
                tokens.push(Token { kind: TokenKind::Dedent, line: self.line, col: self.col });
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
                let first = self.advance().unwrap_or('\0');
                let name = self.scan_ident(first);
                Ok(keyword_or_ident(&name))
            }
            '+' => { self.advance(); if self.peek() == Some('=') { self.advance(); Ok(TokenKind::PlusEq) } else { Ok(TokenKind::Plus) } }
            '-' => { self.advance(); if self.peek() == Some('>') { self.advance(); Ok(TokenKind::Arrow) } else if self.peek() == Some('=') { self.advance(); Ok(TokenKind::MinusEq) } else { Ok(TokenKind::Minus) } }
            '*' => { self.advance(); if self.peek() == Some('*') { self.advance(); Ok(TokenKind::Pow) } else if self.peek() == Some('=') { self.advance(); Ok(TokenKind::StarEq) } else { Ok(TokenKind::Star) } }
            '/' => { self.advance(); if self.peek() == Some('=') { self.advance(); Ok(TokenKind::SlashEq) } else { Ok(TokenKind::Slash) } }
            '%' => { self.advance(); Ok(TokenKind::Percent) }
            '=' => { self.advance(); if self.peek() == Some('=') { self.advance(); Ok(TokenKind::EqEq) } else if self.peek() == Some('>') { self.advance(); Ok(TokenKind::FatArrow) } else { Ok(TokenKind::Eq) } }
            '!' => { self.advance(); if self.peek() == Some('=') { self.advance(); Ok(TokenKind::Ne) } else { Ok(TokenKind::Bang) } }
            '<' => { self.advance(); if self.peek() == Some('=') { self.advance(); Ok(TokenKind::Le) } else if self.peek() == Some('<') { self.advance(); Ok(TokenKind::Shl) } else { Ok(TokenKind::Lt) } }
            '>' => { self.advance(); if self.peek() == Some('=') { self.advance(); Ok(TokenKind::Ge) } else if self.peek() == Some('>') { self.advance(); Ok(TokenKind::Shr) } else { Ok(TokenKind::Gt) } }
            '&' => { self.advance(); if self.peek() == Some('&') { self.advance(); Ok(TokenKind::AndAnd) } else if self.peek() == Some('=') { self.advance(); Ok(TokenKind::BitAndEq) } else { Ok(TokenKind::BitAnd) } }
            '|' => { self.advance(); if self.peek() == Some('|') { self.advance(); Ok(TokenKind::OrOr) } else if self.peek() == Some('=') { self.advance(); Ok(TokenKind::BitOrEq) } else { Ok(TokenKind::BitOr) } }
            '^' => { self.advance(); if self.peek() == Some('=') { self.advance(); Ok(TokenKind::BitXorEq) } else { Ok(TokenKind::BitXor) } }
            '~' => { self.advance(); Ok(TokenKind::Tilde) }
            '$' => { self.advance(); if self.peek() == Some('(') { self.advance(); Ok(TokenKind::DollarParen) } else { Err(unexpected_dollar(line, col)) } }
            '(' => { self.advance(); Ok(TokenKind::LParen) }
            ')' => { self.advance(); Ok(TokenKind::RParen) }
            '{' => { self.advance(); Ok(TokenKind::LBrace) }
            '}' => { self.advance(); Ok(TokenKind::RBrace) }
            '[' => { self.advance(); Ok(TokenKind::LBracket) }
            ']' => { self.advance(); Ok(TokenKind::RBracket) }
            ':' => { self.advance(); if self.peek() == Some(':') { self.advance(); Ok(TokenKind::ColonColon) } else { Ok(TokenKind::Colon) } }
            ';' => { self.advance(); Ok(TokenKind::Semi) }
            ',' => { self.advance(); Ok(TokenKind::Comma) }
            '.' => { self.advance(); if self.peek() == Some('.') && self.chars.clone().next() == Some('.') { self.advance(); self.advance(); Ok(TokenKind::Ellipsis) } else if self.peek() == Some('.') { self.advance(); Ok(TokenKind::DotDot) } else { Ok(TokenKind::Dot) } }
            '?' => { self.advance(); Ok(TokenKind::Question) }
            '@' => { self.advance(); Ok(TokenKind::At) }
            '#' => { self.advance(); Ok(TokenKind::Hash) }
            '\'' => { self.advance(); Ok(TokenKind::Tick) }
            _ => Err(unexpected_character(c, line, col)),
        }
    }
}
