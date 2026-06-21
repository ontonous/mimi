use crate::ast::Commitment;
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

    pub(crate) fn scan_ident(&mut self, first: char) -> (String, Commitment) {
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
        let commitment = self.scan_commitment();
        (s, commitment)
    }

    pub(crate) fn scan_commitment(&mut self) -> Commitment {
        match self.peek() {
            Some('$') => {
                self.advance();
                match self.peek() {
                    Some('$') => {
                        self.advance();
                        match self.peek() {
                            Some('?') => {
                                self.advance();
                                if self.peek() == Some('?') {
                                    self.advance();
                                    Commitment::StrongLockedQuestionQuestion
                                } else {
                                    Commitment::StrongLockedQuestion
                                }
                            }
                            _ => Commitment::StrongLocked,
                        }
                    }
                    Some('?') => {
                        self.advance();
                        if self.peek() == Some('?') {
                            self.advance();
                            Commitment::LockedQuestionQuestion
                        } else {
                            Commitment::LockedQuestion
                        }
                    }
                    _ => Commitment::Locked,
                }
            }
            Some('?') => {
                self.advance();
                if self.peek() == Some('?') {
                    self.advance();
                    Commitment::QuestionQuestion
                } else {
                    Commitment::Question
                }
            }
            _ => Commitment::None,
        }
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
                if !spaces.is_multiple_of(4) {
                    return Err(indent_not_multiple_of_four(self.line, self.col));
                }
                let current = *self.indent_stack.last().unwrap_or(&0);
                if spaces > current {
                    self.indent_stack.push(spaces);
                    tokens.push(Token {
                        kind: TokenKind::Indent,
                        commitment: Commitment::None,
                        line: self.line,
                        col: spaces,
                    });
                } else if spaces < current {
                    while *self.indent_stack.last().unwrap_or(&0) > spaces {
                        self.indent_stack.pop();
                        tokens.push(Token {
                            kind: TokenKind::Dedent,
                            commitment: Commitment::None,
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
                    commitment: Commitment::None,
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
    ) -> Result<(TokenKind, Commitment), LexerError> {
        match c {
            'f' if self.chars.clone().next() == Some('"') => {
                let s = self.scan_fstring()?;
                let commitment = self.scan_commitment();
                Ok((TokenKind::FString(s), commitment))
            }
            '"' => {
                let s = self.scan_string()?;
                let commitment = self.scan_commitment();
                Ok((TokenKind::String(s), commitment))
            }
            '0'..='9' => Ok((self.scan_number(), Commitment::None)),
            'a'..='z' | 'A'..='Z' | '_' => {
                let first = self.advance().unwrap_or('\0');
                let (name, commitment) = self.scan_ident(first);
                Ok((keyword_or_ident(&name), commitment))
            }
            '+' => {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    Ok((TokenKind::PlusEq, Commitment::None))
                } else {
                    Ok((TokenKind::Plus, Commitment::None))
                }
            }
            '-' => {
                self.advance();
                if self.peek() == Some('>') {
                    self.advance();
                    Ok((TokenKind::Arrow, Commitment::None))
                } else if self.peek() == Some('=') {
                    self.advance();
                    Ok((TokenKind::MinusEq, Commitment::None))
                } else {
                    Ok((TokenKind::Minus, Commitment::None))
                }
            }
            '*' => {
                self.advance();
                if self.peek() == Some('*') {
                    self.advance();
                    Ok((TokenKind::Pow, Commitment::None))
                } else if self.peek() == Some('=') {
                    self.advance();
                    Ok((TokenKind::StarEq, Commitment::None))
                } else {
                    Ok((TokenKind::Star, Commitment::None))
                }
            }
            '/' => {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    Ok((TokenKind::SlashEq, Commitment::None))
                } else {
                    Ok((TokenKind::Slash, Commitment::None))
                }
            }
            '%' => {
                self.advance();
                Ok((TokenKind::Percent, Commitment::None))
            }
            '=' => {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    Ok((TokenKind::EqEq, Commitment::None))
                } else if self.peek() == Some('>') {
                    self.advance();
                    Ok((TokenKind::FatArrow, Commitment::None))
                } else {
                    Ok((TokenKind::Eq, Commitment::None))
                }
            }
            '!' => {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    Ok((TokenKind::Ne, Commitment::None))
                } else {
                    Ok((TokenKind::Bang, Commitment::None))
                }
            }
            '<' => {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    Ok((TokenKind::Le, Commitment::None))
                } else if self.peek() == Some('<') {
                    self.advance();
                    Ok((TokenKind::Shl, Commitment::None))
                } else {
                    Ok((TokenKind::Lt, Commitment::None))
                }
            }
            '>' => {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    Ok((TokenKind::Ge, Commitment::None))
                } else if self.peek() == Some('>') {
                    self.advance();
                    Ok((TokenKind::Shr, Commitment::None))
                } else {
                    Ok((TokenKind::Gt, Commitment::None))
                }
            }
            '&' => {
                self.advance();
                if self.peek() == Some('&') {
                    self.advance();
                    Ok((TokenKind::AndAnd, Commitment::None))
                } else if self.peek() == Some('=') {
                    self.advance();
                    Ok((TokenKind::BitAndEq, Commitment::None))
                } else {
                    Ok((TokenKind::BitAnd, Commitment::None))
                }
            }
            '|' => {
                self.advance();
                if self.peek() == Some('|') {
                    self.advance();
                    Ok((TokenKind::OrOr, Commitment::None))
                } else if self.peek() == Some('=') {
                    self.advance();
                    Ok((TokenKind::BitOrEq, Commitment::None))
                } else {
                    Ok((TokenKind::BitOr, Commitment::None))
                }
            }
            '^' => {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    Ok((TokenKind::BitXorEq, Commitment::None))
                } else {
                    Ok((TokenKind::BitXor, Commitment::None))
                }
            }
            '~' => {
                self.advance();
                Ok((TokenKind::Tilde, Commitment::None))
            }
            '$' => {
                self.advance();
                if self.peek() == Some('(') {
                    self.advance();
                    Ok((TokenKind::DollarParen, Commitment::None))
                } else {
                    Err(unexpected_dollar(line, col))
                }
            }
            '(' => {
                self.advance();
                Ok((TokenKind::LParen, Commitment::None))
            }
            ')' => {
                self.advance();
                Ok((TokenKind::RParen, Commitment::None))
            }
            '{' => {
                self.advance();
                Ok((TokenKind::LBrace, Commitment::None))
            }
            '}' => {
                self.advance();
                Ok((TokenKind::RBrace, Commitment::None))
            }
            '[' => {
                self.advance();
                Ok((TokenKind::LBracket, Commitment::None))
            }
            ']' => {
                self.advance();
                Ok((TokenKind::RBracket, Commitment::None))
            }
            ':' => {
                self.advance();
                if self.peek() == Some(':') {
                    self.advance();
                    Ok((TokenKind::ColonColon, Commitment::None))
                } else {
                    Ok((TokenKind::Colon, Commitment::None))
                }
            }
            ';' => {
                self.advance();
                Ok((TokenKind::Semi, Commitment::None))
            }
            ',' => {
                self.advance();
                Ok((TokenKind::Comma, Commitment::None))
            }
            '.' => {
                self.advance();
                if self.peek() == Some('.') && self.chars.clone().next() == Some('.') {
                    self.advance();
                    self.advance();
                    Ok((TokenKind::Ellipsis, Commitment::None))
                } else if self.peek() == Some('.') {
                    self.advance();
                    Ok((TokenKind::DotDot, Commitment::None))
                } else {
                    Ok((TokenKind::Dot, Commitment::None))
                }
            }
            '?' => {
                self.advance();
                Ok((TokenKind::Question, Commitment::None))
            }
            '@' => {
                self.advance();
                Ok((TokenKind::At, Commitment::None))
            }
            '#' => {
                self.advance();
                Ok((TokenKind::Hash, Commitment::None))
            }
            '\'' => {
                self.advance();
                Ok((TokenKind::Tick, Commitment::None))
            }
            _ => Err(unexpected_character(c, line, col)),
        }
    }
}
