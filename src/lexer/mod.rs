pub mod errors;
pub(crate) mod flow;
pub(crate) mod keywords;
mod scan;
pub mod token;

#[cfg(test)]
pub(crate) use errors::unexpected_character;
pub use errors::LexerError;
pub use flow::flow_tokenize;
pub use keywords::is_keyword_kind;
pub use token::{LexerMode, Token, TokenKind};

pub struct Lexer<'a> {
    source: &'a str,
    #[cfg_attr(not(test), allow(dead_code))]
    chars: std::str::Chars<'a>,
    #[cfg_attr(not(test), allow(dead_code))]
    line: usize,
    #[cfg_attr(not(test), allow(dead_code))]
    col: usize,
    #[cfg_attr(not(test), allow(dead_code))]
    peeked: Option<char>,
    mode: LexerMode,
    #[cfg_attr(not(test), allow(dead_code))]
    at_line_start: bool,
    #[cfg_attr(not(test), allow(dead_code))]
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

    pub fn tokenize(self) -> Result<Vec<Token>, LexerError> {
        flow_tokenize(self.source, self.mode)
    }

    /// Legacy tokenize implementation, kept for test equivalence comparison.
    #[cfg(test)]
    pub fn legacy_tokenize(mut self) -> Result<Vec<Token>, LexerError> {
        let mut tokens = Vec::new();
        // Skip shebang line at the very beginning of the file (e.g. #!/usr/bin/env mimi)
        if self.peek() == Some('#') && self.chars.clone().next() == Some('!') {
            while self.peek().is_some_and(|c| c != '\n') {
                self.advance();
            }
        }
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
                    line,
                    col,
                });
                continue;
            }

            // Line continuation: backslash followed by newline
            if c == '\\' {
                self.advance();
                self.skip_whitespace_inline();
                if self.peek() == Some('\n') {
                    self.advance();
                    self.at_line_start = true;
                    continue;
                }
                return Err(unexpected_character('\\', line, col));
            }

            // Block comment: /* ... */
            if c == '/' && self.chars.clone().next() == Some('*') {
                self.skip_block_comment()?;
                continue;
            }

            if c == '/' && self.chars.clone().next() == Some('/') {
                self.skip_line_comment();
                continue;
            }

            // LX-H2: bare `#` (not `#[` attribute) is a line comment.
            if c == '#' && self.chars.clone().next() != Some('[') {
                self.skip_line_comment();
                continue;
            }

            self.at_line_start = false;
            let kind = self.scan_token(c, line, col)?;
            tokens.push(Token { kind, line, col });
        }
        self.flush_indent(&mut tokens);
        tokens.push(Token {
            kind: TokenKind::Eof,
            line: self.line,
            col: self.col,
        });
        Ok(tokens)
    }
}
