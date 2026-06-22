pub mod errors;
mod keywords;
mod scan;
pub mod token;

pub use errors::LexerError;
pub use token::{LexerMode, Token, TokenKind};

pub struct Lexer<'a> {
    #[allow(dead_code)]
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

    pub fn tokenize(mut self) -> Result<Vec<Token>, LexerError> {
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
                tokens.push(Token { kind: TokenKind::Newline, line, col });
                continue;
            }

            if c == '/' && self.chars.clone().next() == Some('/') {
                self.skip_line_comment();
                continue;
            }

            self.at_line_start = false;
            let kind = self.scan_token(c, line, col)?;
            tokens.push(Token { kind, line, col });
        }
        self.flush_indent(&mut tokens);
        tokens.push(Token { kind: TokenKind::Eof, line: self.line, col: self.col });
        Ok(tokens)
    }
}
