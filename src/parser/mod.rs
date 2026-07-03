// Parser uses .expect() on self.expect() returns as an intentional pattern.
#![cfg_attr(not(test), allow(clippy::unwrap_used))]
use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::lexer::{Token, TokenKind};
use crate::span::Span;

mod helpers;
mod parse_expr;
mod parse_stmt;
mod parse_type;
mod pattern;
mod top_level;

#[derive(Debug, Clone)]
pub struct ParseError {
    pub message: String,
    pub line: usize,
    pub col: usize,
    pub span: Option<Span>,
}

impl ParseError {
    fn new(message: impl Into<String>, line: usize, col: usize) -> Self {
        Self {
            message: message.into(),
            line,
            col,
            span: None,
        }
    }

    /// Convert to the new Diagnostic type.
    pub fn to_diagnostic(&self) -> Diagnostic {
        let span = self
            .span
            .unwrap_or_else(|| Span::single(self.line, self.col));
        Diagnostic::error(&self.message, span)
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at {}:{}", self.message, self.line, self.col)
    }
}

impl std::error::Error for ParseError {}

impl From<ParseError> for String {
    fn from(e: ParseError) -> Self {
        e.to_string()
    }
}

impl From<&ParseError> for Diagnostic {
    fn from(e: &ParseError) -> Self {
        e.to_diagnostic()
    }
}

impl From<ParseError> for Diagnostic {
    fn from(e: ParseError) -> Self {
        e.to_diagnostic()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseMode {
    Production,
    Sketch,
}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    mode: ParseMode,
    recovery_mode: bool,
    recursion_depth: std::cell::Cell<usize>,
    /// Statement-level errors collected during recovery parsing.
    /// These are returned alongside top-level errors by `parse_file_with_recovery`.
    errors: Vec<ParseError>,
    /// Background MimiSpec parse thread handle.
    ///
    /// Each `Parser` instance is allowed at most one concurrent background
    /// MimiSpec parse thread. If a parse exceeds the timeout, the handle is
    /// retained here so the thread can be joined later (on the next parse call
    /// or when the parser is dropped), preventing detached threads from
    /// accumulating.
    mimispec_thread: Option<std::thread::JoinHandle<mimispec::error::ParseResult>>,
}

impl Drop for Parser {
    fn drop(&mut self) {
        // Reclaim any background MimiSpec parse thread that outlived its
        // timeout. The thread cannot be cancelled mid-parse, so we join it
        // here to release its resources before the Parser is destroyed.
        if let Some(handle) = self.mimispec_thread.take() {
            let _ = handle.join();
        }
    }
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self::with_mode(tokens, ParseMode::Production)
    }

    pub fn new_sketch(tokens: Vec<Token>) -> Self {
        Self::with_mode(tokens, ParseMode::Sketch)
    }

    fn with_mode(tokens: Vec<Token>, mode: ParseMode) -> Self {
        Self {
            tokens,
            pos: 0,
            mode,
            recovery_mode: false,
            recursion_depth: std::cell::Cell::new(0),
            errors: Vec::new(),
            mimispec_thread: None,
        }
    }

    /// Create a parser in recovery mode: statement-level errors are caught and skipped.
    #[allow(dead_code)]
    pub fn new_with_recovery(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            pos: 0,
            mode: ParseMode::Production,
            recovery_mode: true,
            recursion_depth: std::cell::Cell::new(0),
            errors: Vec::new(),
            mimispec_thread: None,
        }
    }

    pub fn parse_file(mut self) -> Result<File, ParseError> {
        self.skip_newlines();
        let mut imports = Vec::new();
        while self.at(&TokenKind::Use) {
            imports.push(self.parse_import()?);
            self.skip_newlines();
        }
        let mut items = Vec::new();
        while !self.at(&TokenKind::Eof) {
            self.skip_newlines();
            if self.at(&TokenKind::Eof) {
                break;
            }
            items.push(self.parse_item()?);
        }
        Ok(File { imports, items })
    }

    /// Parse a file with error recovery, collecting multiple errors.
    /// Returns the parsed file (possibly partial) and all errors encountered.
    pub fn parse_file_with_recovery(mut self) -> (File, Vec<ParseError>) {
        self.recovery_mode = true;
        let mut errors = Vec::new();

        self.skip_newlines();
        let mut imports = Vec::new();
        while self.at(&TokenKind::Use) {
            match self.parse_import() {
                Ok(imp) => imports.push(imp),
                Err(e) => {
                    errors.push(e);
                    // Skip to next top-level sync point
                    self.recover_to_sync(&[
                        TokenKind::Func,
                        TokenKind::Type,
                        TokenKind::Module,
                        TokenKind::Actor,
                        TokenKind::Cap,
                        TokenKind::Trait,
                        TokenKind::Impl,
                        TokenKind::Extern,
                        TokenKind::Use,
                        TokenKind::RBrace,
                        TokenKind::Eof,
                    ]);
                    // If still at Use and recover didn't advance, force-advance
                    // to avoid infinite loop on malformed import.
                    if self.at(&TokenKind::Use) {
                        self.advance();
                    }
                }
            }
            self.skip_newlines();
        }

        let mut items = Vec::new();
        while !self.at(&TokenKind::Eof) {
            self.skip_newlines();
            if self.at(&TokenKind::Eof) {
                break;
            }
            let pos_before = self.pos;
            match self.parse_item() {
                Ok(item) => items.push(item),
                Err(e) => {
                    errors.push(e);
                    // Skip to next top-level sync point
                    self.recover_to_sync(&[
                        TokenKind::Func,
                        TokenKind::Type,
                        TokenKind::Module,
                        TokenKind::Actor,
                        TokenKind::Cap,
                        TokenKind::Trait,
                        TokenKind::Impl,
                        TokenKind::Extern,
                        TokenKind::Use,
                        TokenKind::RBrace,
                        TokenKind::Eof,
                    ]);
                    // Ensure progress: if recover_to_sync didn't advance
                    // (e.g. sync token was a structural token not consumed
                    // by parse_item), force-advance past it to avoid infinite loop.
                    if self.pos == pos_before {
                        self.advance();
                    }
                }
            }
        }

        errors.extend(std::mem::take(&mut self.errors));
        (File { imports, items }, errors)
    }

    pub(crate) fn parse_block(&mut self) -> Result<Block, ParseError> {
        self.check_depth()?;
        self.inc_depth();
        let result = if self.is_sketch() {
            self.parse_indent_block()
        } else {
            self.parse_brace_block()
        };
        self.dec_depth();
        result
    }

    /// Return the current token position (number of tokens consumed so far).
    pub fn pos(&self) -> usize {
        self.pos
    }
}
