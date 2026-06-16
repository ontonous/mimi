use crate::ast::Commitment;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    // Literals
    Int(String),
    Float(String),
    String(String),
    True,
    False,
    Unit,

    // Identifiers / keywords
    Ident(String),

    // Keywords
    Module,
    Type,
    Func,
    Fn,
    Actor,
    Newtype,
    Let,
    Mut,
    Ref,
    Shared,
    LocalShared,
    Weak,
    Arena,
    Cap,
    Trait,
    Impl,
    Where,
    If,
    Else,
    For,
    In,
    While,
    Return,
    Match,
    Use,
    Pub,
    Drop,
    Await,
    Spawn,
    Steps,
    Parasteps,
    Quote,
    Comptime,
    Flow,
    Ui,
    Binds,
    On,
    Failure,
    Requires,
    Ensures,
    Math,
    Desc,
    Rule,
    Old,
    And,
    Or,
    Not,

    // Types
    I32,
    I64,
    F64,
    Bool,
    StringKw,
    Nothing,

    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Pow,
    Eq,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    EqEq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    AndAnd,
    OrOr,
    NotOp,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Tilde,
    DollarParen,

    // Punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Colon,
    Semi,
    Comma,
    Dot,
    ColonColon,
    Arrow,
    FatArrow,
    Question,
    Bang,
    Ellipsis,

    Newline,
    Indent,
    Dedent,
    Eof,
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            TokenKind::Int(v) => return write!(f, "integer `{}`", v),
            TokenKind::Float(v) => return write!(f, "float `{}`", v),
            TokenKind::String(v) => return write!(f, "string `{}`", v),
            TokenKind::Ident(v) => return write!(f, "identifier `{}`", v),
            TokenKind::True => "true",
            TokenKind::False => "false",
            TokenKind::Unit => "unit",
            TokenKind::Module => "module",
            TokenKind::Type => "type",
            TokenKind::Func => "func",
            TokenKind::Fn => "fn",
            TokenKind::Actor => "actor",
            TokenKind::Newtype => "newtype",
            TokenKind::Let => "let",
            TokenKind::Mut => "mut",
            TokenKind::Ref => "ref",
            TokenKind::Shared => "shared",
            TokenKind::LocalShared => "local_shared",
            TokenKind::Weak => "weak",
            TokenKind::Arena => "arena",
            TokenKind::Cap => "cap",
            TokenKind::Trait => "trait",
            TokenKind::Impl => "impl",
            TokenKind::Where => "where",
            TokenKind::If => "if",
            TokenKind::Else => "else",
            TokenKind::For => "for",
            TokenKind::In => "in",
            TokenKind::While => "while",
            TokenKind::Return => "return",
            TokenKind::Match => "match",
            TokenKind::Use => "use",
            TokenKind::Pub => "pub",
            TokenKind::Drop => "drop",
            TokenKind::Await => "await",
            TokenKind::Spawn => "spawn",
            TokenKind::Steps => "steps",
            TokenKind::Parasteps => "parasteps",
            TokenKind::Quote => "quote",
            TokenKind::Comptime => "comptime",
            TokenKind::Flow => "flow",
            TokenKind::Ui => "ui",
            TokenKind::Binds => "binds",
            TokenKind::On => "on",
            TokenKind::Failure => "failure",
            TokenKind::Requires => "requires",
            TokenKind::Ensures => "ensures",
            TokenKind::Math => "math",
            TokenKind::Desc => "desc",
            TokenKind::Rule => "rule",
            TokenKind::Old => "old",
            TokenKind::And => "and",
            TokenKind::Or => "or",
            TokenKind::Not => "not",
            TokenKind::I32 => "i32",
            TokenKind::I64 => "i64",
            TokenKind::F64 => "f64",
            TokenKind::Bool => "bool",
            TokenKind::StringKw => "string",
            TokenKind::Nothing => "nothing",
            TokenKind::Plus => "+",
            TokenKind::Minus => "-",
            TokenKind::Star => "*",
            TokenKind::Slash => "/",
            TokenKind::Percent => "%",
            TokenKind::Pow => "**",
            TokenKind::Eq => "=",
            TokenKind::PlusEq => "+=",
            TokenKind::MinusEq => "-=",
            TokenKind::StarEq => "*=",
            TokenKind::SlashEq => "/=",
            TokenKind::EqEq => "==",
            TokenKind::Ne => "!=",
            TokenKind::Lt => "<",
            TokenKind::Gt => ">",
            TokenKind::Le => "<=",
            TokenKind::Ge => ">=",
            TokenKind::AndAnd => "&&",
            TokenKind::OrOr => "||",
            TokenKind::NotOp => "!",
            TokenKind::BitAnd => "&",
            TokenKind::BitOr => "|",
            TokenKind::BitXor => "^",
            TokenKind::Shl => "<<",
            TokenKind::Shr => ">>",
            TokenKind::Tilde => "~",
            TokenKind::DollarParen => "$(",
            TokenKind::LParen => "(",
            TokenKind::RParen => ")",
            TokenKind::LBrace => "{",
            TokenKind::RBrace => "}",
            TokenKind::LBracket => "[",
            TokenKind::RBracket => "]",
            TokenKind::Colon => ":",
            TokenKind::Semi => ";",
            TokenKind::Comma => ",",
            TokenKind::Dot => ".",
            TokenKind::ColonColon => "::",
            TokenKind::Arrow => "->",
            TokenKind::FatArrow => "=>",
            TokenKind::Question => "?",
            TokenKind::Bang => "!",
            TokenKind::Ellipsis => "...",
            TokenKind::Newline => "newline",
            TokenKind::Indent => "INDENT",
            TokenKind::Dedent => "DEDENT",
            TokenKind::Eof => "EOF",
        };
        write!(f, "{}", s)
    }
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub commitment: Commitment,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LexerMode {
    Production,
    Sketch,
}

#[derive(Debug, Clone)]
pub struct Lexer<'a> {
    source: &'a str,
    chars: std::str::Chars<'a>,
    line: usize,
    col: usize,
    peeked: Option<char>,
    mode: LexerMode,
    at_line_start: bool,
    indent_stack: Vec<usize>,
}

impl<'a> Lexer<'a> {
    pub fn new(source: &'a str) -> Self {
        Self::with_mode(source, LexerMode::Production)
    }

    pub fn new_sketch(source: &'a str) -> Self {
        Self::with_mode(source, LexerMode::Sketch)
    }

    fn with_mode(source: &'a str, mode: LexerMode) -> Self {
        let mut chars = source.chars();
        let peeked = chars.next();
        Self {
            source,
            chars,
            line: 1,
            col: 1,
            peeked,
            mode,
            at_line_start: true,
            indent_stack: vec![0],
        }
    }

    fn advance(&mut self) -> Option<char> {
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

    fn peek(&self) -> Option<char> {
        self.peeked
    }

    fn skip_line_comment(&mut self) {
        while let Some(c) = self.peek() {
            if c == '\n' {
                break;
            }
            self.advance();
        }
    }

    fn skip_whitespace_inline(&mut self) {
        while let Some(c) = self.peek() {
            if c == ' ' || c == '\t' || c == '\r' {
                self.advance();
            } else {
                break;
            }
        }
    }

    fn scan_string(&mut self) -> Result<String, String> {
        // consume opening quote
        self.advance();
        let mut s = String::new();
        loop {
            match self.peek() {
                None => return Err("unterminated string".into()),
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
                        None => return Err("unterminated escape".into()),
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

    fn scan_number(&mut self) -> TokenKind {
        let start_line = self.line;
        let start_col = self.col;
        let mut s = String::new();
        let mut is_float = false;
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
        let _ = (start_line, start_col);
        if is_float {
            TokenKind::Float(s)
        } else {
            TokenKind::Int(s)
        }
    }

    fn scan_ident(&mut self, first: char) -> (String, Commitment) {
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

    fn scan_commitment(&mut self) -> Commitment {
        let save_line = self.line;
        let save_col = self.col;
        let _ = (save_line, save_col);
        if self.peek() == Some('$') {
            self.advance();
            if self.peek() == Some('$') {
                self.advance();
                if self.peek() == Some('?') {
                    self.advance();
                    if self.peek() == Some('?') {
                        self.advance();
                        Commitment::StrongLockedQuestionQuestion
                    } else {
                        Commitment::StrongLockedQuestion
                    }
                } else if self.peek() == Some('?') {
                    self.advance();
                    Commitment::StrongLockedQuestionQuestion // $$? + ? impossible; treat as $$??
                } else {
                    Commitment::StrongLocked
                }
            } else if self.peek() == Some('?') {
                self.advance();
                if self.peek() == Some('?') {
                    self.advance();
                    Commitment::LockedQuestionQuestion
                } else {
                    Commitment::LockedQuestion
                }
            } else {
                Commitment::Locked
            }
        } else if self.peek() == Some('?') {
            self.advance();
            if self.peek() == Some('?') {
                self.advance();
                Commitment::QuestionQuestion
            } else {
                Commitment::Question
            }
        } else {
            Commitment::None
        }
    }

    fn keyword_or_ident(name: &str) -> TokenKind {
        match name {
            "module" => TokenKind::Module,
            "type" => TokenKind::Type,
            "func" => TokenKind::Func,
            "fn" => TokenKind::Fn,
            "actor" => TokenKind::Actor,
            "newtype" => TokenKind::Newtype,
            "let" => TokenKind::Let,
            "mut" => TokenKind::Mut,
            "ref" => TokenKind::Ref,
            "shared" => TokenKind::Shared,
            "local_shared" => TokenKind::LocalShared,
            "weak" => TokenKind::Weak,
            "arena" => TokenKind::Arena,
            "cap" => TokenKind::Cap,
            "trait" => TokenKind::Trait,
            "impl" => TokenKind::Impl,
            "where" => TokenKind::Where,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "while" => TokenKind::While,
            "return" => TokenKind::Return,
            "match" => TokenKind::Match,
            "use" => TokenKind::Use,
            "pub" => TokenKind::Pub,
            "drop" => TokenKind::Drop,
            "await" => TokenKind::Await,
            "spawn" => TokenKind::Spawn,
            "steps" => TokenKind::Steps,
            "parasteps" => TokenKind::Parasteps,
            "quote" => TokenKind::Quote,
            "comptime" => TokenKind::Comptime,
            "flow" => TokenKind::Flow,
            "ui" => TokenKind::Ui,
            "binds" => TokenKind::Binds,
            "on" => TokenKind::On,
            "failure" => TokenKind::Failure,
            "requires" => TokenKind::Requires,
            "ensures" => TokenKind::Ensures,
            "math" => TokenKind::Math,
            "desc" => TokenKind::Desc,
            "rule" => TokenKind::Rule,
            "old" => TokenKind::Old,
            "and" => TokenKind::And,
            "or" => TokenKind::Or,
            "not" => TokenKind::Not,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "unit" => TokenKind::Unit,
            "i32" => TokenKind::I32,
            "i64" => TokenKind::I64,
            "f64" => TokenKind::F64,
            "bool" => TokenKind::Bool,
            "string" => TokenKind::StringKw,
            "nothing" => TokenKind::Nothing,
            _ => TokenKind::Ident(name.into()),
        }
    }

    fn process_line_start(&mut self, tokens: &mut Vec<Token>) -> Result<(), String> {
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
                return Err(format!(
                    "tabs are not allowed for indentation at {}:{}",
                    self.line, self.col
                ));
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
                if spaces % 4 != 0 {
                    return Err(format!(
                        "indentation must be a multiple of 4 spaces at {}:{}",
                        self.line, self.col
                    ));
                }
                let current = *self.indent_stack.last().unwrap();
                if spaces > current {
                    self.indent_stack.push(spaces);
                    tokens.push(Token {
                        kind: TokenKind::Indent,
                        commitment: Commitment::None,
                        line: self.line,
                        col: spaces,
                    });
                } else if spaces < current {
                    while *self.indent_stack.last().unwrap() > spaces {
                        self.indent_stack.pop();
                        tokens.push(Token {
                            kind: TokenKind::Dedent,
                            commitment: Commitment::None,
                            line: self.line,
                            col: spaces,
                        });
                    }
                    if *self.indent_stack.last().unwrap() != spaces {
                        return Err(format!(
                            "dedent does not match any indentation level at {}:{}",
                            self.line, self.col
                        ));
                    }
                }
            }
            self.at_line_start = false;
            return Ok(());
        }
    }

    fn flush_indent(&mut self, tokens: &mut Vec<Token>) {
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

    pub fn tokenize(mut self) -> Result<Vec<Token>, String> {
        let mut tokens = Vec::new();
        loop {
            self.process_line_start(&mut tokens)?;
            self.skip_whitespace_inline();
            let line = self.line;
            let col = self.col;
            let c = match self.peek() {
                Some(c) => c,
                None => break,
            };

            if c == '\n' {
                self.advance();
                self.at_line_start = true;
                tokens.push(Token {
                    kind: TokenKind::Newline,
                    commitment: Commitment::None,
                    line,
                    col,
                });
                continue;
            }

            if c == '/' && self.chars.clone().next() == Some('/') {
                self.skip_line_comment();
                continue;
            }

            self.at_line_start = false;
            let (kind, commitment) = match c {
                '"' => {
                    let s = self.scan_string()?;
                    let commitment = self.scan_commitment();
                    (TokenKind::String(s), commitment)
                }
                '0'..='9' => (self.scan_number(), Commitment::None),
                'a'..='z' | 'A'..='Z' | '_' => {
                    let first = self.advance().unwrap();
                    let (name, commitment) = self.scan_ident(first);
                    (Self::keyword_or_ident(&name), commitment)
                }
                '+' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        (TokenKind::PlusEq, Commitment::None)
                    } else {
                        (TokenKind::Plus, Commitment::None)
                    }
                }
                '-' => {
                    self.advance();
                    if self.peek() == Some('>') {
                        self.advance();
                        (TokenKind::Arrow, Commitment::None)
                    } else if self.peek() == Some('=') {
                        self.advance();
                        (TokenKind::MinusEq, Commitment::None)
                    } else {
                        (TokenKind::Minus, Commitment::None)
                    }
                }
                '*' => {
                    self.advance();
                    if self.peek() == Some('*') {
                        self.advance();
                        (TokenKind::Pow, Commitment::None)
                    } else if self.peek() == Some('=') {
                        self.advance();
                        (TokenKind::StarEq, Commitment::None)
                    } else {
                        (TokenKind::Star, Commitment::None)
                    }
                }
                '/' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        (TokenKind::SlashEq, Commitment::None)
                    } else {
                        (TokenKind::Slash, Commitment::None)
                    }
                }
                '%' => {
                    self.advance();
                    (TokenKind::Percent, Commitment::None)
                }
                '=' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        (TokenKind::EqEq, Commitment::None)
                    } else if self.peek() == Some('>') {
                        self.advance();
                        (TokenKind::FatArrow, Commitment::None)
                    } else {
                        (TokenKind::Eq, Commitment::None)
                    }
                }
                '!' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        (TokenKind::Ne, Commitment::None)
                    } else {
                        (TokenKind::Bang, Commitment::None)
                    }
                }
                '<' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        (TokenKind::Le, Commitment::None)
                    } else if self.peek() == Some('<') {
                        self.advance();
                        (TokenKind::Shl, Commitment::None)
                    } else {
                        (TokenKind::Lt, Commitment::None)
                    }
                }
                '>' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        (TokenKind::Ge, Commitment::None)
                    } else if self.peek() == Some('>') {
                        self.advance();
                        (TokenKind::Shr, Commitment::None)
                    } else {
                        (TokenKind::Gt, Commitment::None)
                    }
                }
                '&' => {
                    self.advance();
                    if self.peek() == Some('&') {
                        self.advance();
                        (TokenKind::AndAnd, Commitment::None)
                    } else {
                        (TokenKind::BitAnd, Commitment::None)
                    }
                }
                '|' => {
                    self.advance();
                    if self.peek() == Some('|') {
                        self.advance();
                        (TokenKind::OrOr, Commitment::None)
                    } else {
                        (TokenKind::BitOr, Commitment::None)
                    }
                }
                '^' => {
                    self.advance();
                    (TokenKind::BitXor, Commitment::None)
                }
                '~' => {
                    self.advance();
                    (TokenKind::Tilde, Commitment::None)
                }
                '$' => {
                    self.advance();
                    if self.peek() == Some('(') {
                        self.advance();
                        (TokenKind::DollarParen, Commitment::None)
                    } else {
                        return Err(format!("unexpected '$' at {}:{}", line, col));
                    }
                }
                '(' => {
                    self.advance();
                    (TokenKind::LParen, Commitment::None)
                }
                ')' => {
                    self.advance();
                    (TokenKind::RParen, Commitment::None)
                }
                '{' => {
                    self.advance();
                    (TokenKind::LBrace, Commitment::None)
                }
                '}' => {
                    self.advance();
                    (TokenKind::RBrace, Commitment::None)
                }
                '[' => {
                    self.advance();
                    (TokenKind::LBracket, Commitment::None)
                }
                ']' => {
                    self.advance();
                    (TokenKind::RBracket, Commitment::None)
                }
                ':' => {
                    self.advance();
                    if self.peek() == Some(':') {
                        self.advance();
                        (TokenKind::ColonColon, Commitment::None)
                    } else {
                        (TokenKind::Colon, Commitment::None)
                    }
                }
                ';' => {
                    self.advance();
                    (TokenKind::Semi, Commitment::None)
                }
                ',' => {
                    self.advance();
                    (TokenKind::Comma, Commitment::None)
                }
                '.' => {
                    self.advance();
                    if self.peek() == Some('.') && self.chars.clone().next() == Some('.') {
                        self.advance();
                        self.advance();
                        (TokenKind::Ellipsis, Commitment::None)
                    } else {
                        (TokenKind::Dot, Commitment::None)
                    }
                }
                '?' => {
                    self.advance();
                    (TokenKind::Question, Commitment::None)
                }
                _ => return Err(format!("unexpected character '{}' at {}:{}", c, line, col)),
            };
            tokens.push(Token { kind, commitment, line, col });
        }
        self.flush_indent(&mut tokens);
        tokens.push(Token {
            kind: TokenKind::Eof,
            commitment: Commitment::None,
            line: self.line,
            col: self.col,
        });
        Ok(tokens)
    }
}
