#![allow(dead_code)]

use crate::ast::Commitment;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    // Literals
    Int(String),
    Float(String),
    String(String),
    FString(String), // f"..." raw content for parser to split
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
    CShared,
    CBorrow,
    CBorrowMut,
    RawString,
    Arena,
    Alloc,
    Cap,
    Trait,
    Impl,
    Dyn,
    Where,
    Extern,
    If,
    Else,
    For,
    In,
    While,
    Return,
    Break,
    Continue,
    Match,
    Use,
    Pub,
    Drop,
    Await,
    Async,
    Unsafe,
    Spawn,
    Steps,
    Parasteps,
    Quote,
    Comptime,
    Failure,
    Requires,
    Ensures,
    Math,
    Desc,
    Rule,
    Old,
    Mms,
    With,
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
    BitAndEq,
    BitOrEq,
    BitXorEq,
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
    DotDot,
    ColonColon,
    Arrow,
    FatArrow,
    Question,
    Bang,
    Ellipsis,
    At,
    Hash,
    Tick,

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
            TokenKind::FString(v) => return write!(f, "f-string `{}`", v),
            TokenKind::Ident(v) => return write!(f, "identifier `{}`", v),
            TokenKind::True => "true",
            TokenKind::False => "false",
            TokenKind::Unit => "()",
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
            TokenKind::CShared => "c_shared",
            TokenKind::CBorrow => "c_borrow",
            TokenKind::CBorrowMut => "c_borrow_mut",
            TokenKind::RawString => "raw_string",
            TokenKind::Arena => "arena",
            TokenKind::Alloc => "alloc",
            TokenKind::Cap => "cap",
            TokenKind::Trait => "trait",
            TokenKind::Impl => "impl",
            TokenKind::Dyn => "dyn",
            TokenKind::Where => "where",
            TokenKind::Extern => "extern",
            TokenKind::If => "if",
            TokenKind::Else => "else",
            TokenKind::For => "for",
            TokenKind::In => "in",
            TokenKind::While => "while",
            TokenKind::Return => "return",
            TokenKind::Break => "break",
            TokenKind::Continue => "continue",
            TokenKind::Match => "match",
            TokenKind::Use => "use",
            TokenKind::Pub => "pub",
            TokenKind::Drop => "drop",
            TokenKind::Await => "await",
            TokenKind::Async => "async",
            TokenKind::Unsafe => "unsafe",
            TokenKind::Spawn => "spawn",
            TokenKind::Steps => "steps",
            TokenKind::Parasteps => "parasteps",
            TokenKind::Quote => "quote",
            TokenKind::Comptime => "comptime",

            TokenKind::Failure => "failure",
            TokenKind::Requires => "requires",
            TokenKind::Ensures => "ensures",
            TokenKind::Math => "math",
            TokenKind::Desc => "desc",
            TokenKind::Rule => "rule",
            TokenKind::Old => "old",
            TokenKind::Mms => "mms",
            TokenKind::With => "with",
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
            TokenKind::DotDot => "..",
            TokenKind::ColonColon => "::",
            TokenKind::Arrow => "->",
            TokenKind::FatArrow => "=>",
            TokenKind::Question => "?",
            TokenKind::Bang => "!",
            TokenKind::Ellipsis => "...",
            TokenKind::At => "@",
            TokenKind::Hash => "#",
            TokenKind::Newline => "newline",
            TokenKind::Indent => "INDENT",
            TokenKind::Dedent => "DEDENT",
            TokenKind::Tick => "'",
            TokenKind::Eof => "EOF",
            TokenKind::BitAndEq => "&=",
            TokenKind::BitOrEq => "|=",
            TokenKind::BitXorEq => "^=",
        };
        write!(f, "{}", s)
    }
}

impl TokenKind {
    pub fn source_text(&self) -> &str {
        match self {
            TokenKind::Int(v) => v,
            TokenKind::Float(v) => v,
            TokenKind::String(v) => v,
            TokenKind::FString(v) => v,
            TokenKind::Ident(v) => v,
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
            TokenKind::CShared => "c_shared",
            TokenKind::CBorrow => "c_borrow",
            TokenKind::CBorrowMut => "c_borrow_mut",
            TokenKind::RawString => "raw_string",
            TokenKind::Arena => "arena",
            TokenKind::Alloc => "alloc",
            TokenKind::Cap => "cap",
            TokenKind::Trait => "trait",
            TokenKind::Impl => "impl",
            TokenKind::Dyn => "dyn",
            TokenKind::Where => "where",
            TokenKind::Extern => "extern",
            TokenKind::If => "if",
            TokenKind::Else => "else",
            TokenKind::For => "for",
            TokenKind::In => "in",
            TokenKind::While => "while",
            TokenKind::Return => "return",
            TokenKind::Break => "break",
            TokenKind::Continue => "continue",
            TokenKind::Match => "match",
            TokenKind::Use => "use",
            TokenKind::Pub => "pub",
            TokenKind::Drop => "drop",
            TokenKind::Await => "await",
            TokenKind::Async => "async",
            TokenKind::Unsafe => "unsafe",
            TokenKind::Spawn => "spawn",
            TokenKind::Steps => "steps",
            TokenKind::Parasteps => "parasteps",
            TokenKind::Quote => "quote",
            TokenKind::Comptime => "comptime",

            TokenKind::Failure => "failure",
            TokenKind::Requires => "requires",
            TokenKind::Ensures => "ensures",
            TokenKind::Math => "math",
            TokenKind::Desc => "desc",
            TokenKind::Rule => "rule",
            TokenKind::Old => "old",
            TokenKind::Mms => "mms",
            TokenKind::With => "with",
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
            TokenKind::DotDot => "..",
            TokenKind::ColonColon => "::",
            TokenKind::Arrow => "->",
            TokenKind::FatArrow => "=>",
            TokenKind::Question => "?",
            TokenKind::Bang => "!",
            TokenKind::Ellipsis => "...",
            TokenKind::At => "@",
            TokenKind::Hash => "#",
            TokenKind::Tick => "'",
            TokenKind::Newline => "\n",
            TokenKind::Indent => "",
            TokenKind::Dedent => "",
            TokenKind::BitAndEq => "&=",
            TokenKind::BitOrEq => "|=",
            TokenKind::BitXorEq => "^=",
            TokenKind::Eof => "",
        }
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

    /// Scan an f-string: f"text {expr} text"
    /// Returns the raw content string (with {expr} preserved for parser)
    fn scan_fstring(&mut self) -> Result<String, String> {
        // consume 'f' and opening quote
        self.advance(); // 'f'
        self.advance(); // '"'
        let mut s = String::new();
        loop {
            match self.peek() {
                None => return Err("unterminated f-string".into()),
                Some('"') => {
                    self.advance();
                    break;
                }
                Some('\\') => {
                    self.advance();
                    match self.peek() {
                        Some('n') => { s.push_str("\\n"); self.advance(); }
                        Some('t') => { s.push_str("\\t"); self.advance(); }
                        Some('r') => { s.push_str("\\r"); self.advance(); }
                        Some('\\') => { s.push_str("\\\\"); self.advance(); }
                        Some('"') => { s.push_str("\\\""); self.advance(); }
                        Some('{') => { s.push_str("\\{"); self.advance(); }
                        Some('}') => { s.push_str("\\}"); self.advance(); }
                        Some(c) => { s.push(c); self.advance(); }
                        None => return Err("unterminated escape in f-string".into()),
                    }
                }
                Some('{') => {
                    // interpolation start - track braces
                    s.push('{');
                    self.advance();
                    let mut depth = 1;
                    while let Some(c) = self.peek() {
                        if c == '{' { depth += 1; }
                        else if c == '}' { 
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
                        return Err("unterminated interpolation in f-string".into());
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
        let _start_line = self.line;
        let _start_col = self.col;
        let mut s = String::new();
        let mut is_float = false;
        // Check for 0x or 0X prefix (hex literal)
        if let Some('0') = self.peek() {
            let mut tmp = self.chars.clone();
            let next = tmp.next();
            if matches!(next, Some('x') | Some('X')) {
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
        let _ = (_start_line, _start_col);
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
            "c_shared" => TokenKind::CShared,
            "c_borrow" => TokenKind::CBorrow,
            "c_borrow_mut" => TokenKind::CBorrowMut,
            "raw_string" => TokenKind::RawString,
            "arena" => TokenKind::Arena,
            "alloc" => TokenKind::Alloc,
            "cap" => TokenKind::Cap,
            "trait" => TokenKind::Trait,
            "impl" => TokenKind::Impl,
            "dyn" => TokenKind::Dyn,
            "where" => TokenKind::Where,
            "extern" => TokenKind::Extern,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "while" => TokenKind::While,
            "return" => TokenKind::Return,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "match" => TokenKind::Match,
            "use" => TokenKind::Use,
            "pub" => TokenKind::Pub,
            "drop" => TokenKind::Drop,
            "await" => TokenKind::Await,
            "async" => TokenKind::Async,
            "unsafe" => TokenKind::Unsafe,
            "spawn" => TokenKind::Spawn,
            "steps" => TokenKind::Steps,
            "parasteps" => TokenKind::Parasteps,
            "quote" => TokenKind::Quote,
            "comptime" => TokenKind::Comptime,
            "failure" => TokenKind::Failure,
            "requires" => TokenKind::Requires,
            "ensures" => TokenKind::Ensures,
            "math" => TokenKind::Math,
            "desc" => TokenKind::Desc,
            "rule" => TokenKind::Rule,
            "old" => TokenKind::Old,
            "mms" => TokenKind::Mms,
            "with" => TokenKind::With,
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
                if !spaces.is_multiple_of(4) {
                    return Err(format!(
                        "indentation must be a multiple of 4 spaces at {}:{}",
                        self.line, self.col
                    ));
                }
                let current = *self.indent_stack.last().expect("indent stack non-empty");
                if spaces > current {
                    self.indent_stack.push(spaces);
                    tokens.push(Token {
                        kind: TokenKind::Indent,
                        commitment: Commitment::None,
                        line: self.line,
                        col: spaces,
                    });
                } else if spaces < current {
                    while *self.indent_stack.last().expect("indent stack non-empty") > spaces {
                        self.indent_stack.pop();
                        tokens.push(Token {
                            kind: TokenKind::Dedent,
                            commitment: Commitment::None,
                            line: self.line,
                            col: spaces,
                        });
                    }
                    if *self.indent_stack.last().expect("indent stack non-empty") != spaces {
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
                'f' if self.chars.clone().next() == Some('"') => {
                    let s = self.scan_fstring()?;
                    let commitment = self.scan_commitment();
                    (TokenKind::FString(s), commitment)
                }
                '"' => {
                    let s = self.scan_string()?;
                    let commitment = self.scan_commitment();
                    (TokenKind::String(s), commitment)
                }
                '0'..='9' => (self.scan_number(), Commitment::None),
                'a'..='z' | 'A'..='Z' | '_' => {
                    let first = self.advance().expect("peek confirmed non-EOF");
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
                    } else if self.peek() == Some('=') {
                        self.advance();
                        (TokenKind::BitAndEq, Commitment::None)
                    } else {
                        (TokenKind::BitAnd, Commitment::None)
                    }
                }
                '|' => {
                    self.advance();
                    if self.peek() == Some('|') {
                        self.advance();
                        (TokenKind::OrOr, Commitment::None)
                    } else if self.peek() == Some('=') {
                        self.advance();
                        (TokenKind::BitOrEq, Commitment::None)
                    } else {
                        (TokenKind::BitOr, Commitment::None)
                    }
                }
                '^' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        (TokenKind::BitXorEq, Commitment::None)
                    } else {
                        (TokenKind::BitXor, Commitment::None)
                    }
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
                    } else if self.peek() == Some('.') {
                        self.advance();
                        (TokenKind::DotDot, Commitment::None)
                    } else {
                        (TokenKind::Dot, Commitment::None)
                    }
                }
                '?' => {
                    self.advance();
                    (TokenKind::Question, Commitment::None)
                }
                '@' => {
                    self.advance();
                    (TokenKind::At, Commitment::None)
                }
                '#' => {
                    self.advance();
                    (TokenKind::Hash, Commitment::None)
                }
                '\'' => {
                    self.advance();
                    (TokenKind::Tick, Commitment::None)
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
