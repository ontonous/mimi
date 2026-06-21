#![allow(dead_code)]

use super::*;

impl Parser {
    /// Guard against deep recursion. Returns Err if depth exceeds limit.
    pub(crate) fn check_depth(&self) -> Result<(), ParseError> {
        const MAX: usize = 256;
        if self.recursion_depth.get() >= MAX {
            let tok = self.peek();
            return Err(ParseError::new(
                format!("recursion limit exceeded (> {} nested)", MAX), tok.line, tok.col,
            ));
        }
        Ok(())
    }

    pub(crate) fn inc_depth(&self) { self.recursion_depth.set(self.recursion_depth.get() + 1); }
    pub(crate) fn dec_depth(&self) { let d = self.recursion_depth.get(); if d > 0 { self.recursion_depth.set(d - 1); } }

    /// Skip tokens until we reach a synchronization point.
    /// Returns true if we found a sync point, false if we reached EOF.
    /// Does NOT consume the sync token — the caller must consume it.
    /// NOTE: The caller MUST ensure progress after this returns; callers
    /// that find themselves in a loop on the same token should advance.
    pub(crate) fn recover_to_sync(&mut self, sync_tokens: &[TokenKind]) -> bool {
        let max_skip = 100;
        let mut skipped = 0;
        while !self.at(&TokenKind::Eof) && skipped < max_skip {
            for sync in sync_tokens {
                if self.at(sync) {
                    return true; // DON'T consume — caller will parse the sync token
                }
            }
            self.advance();
            skipped += 1;
        }
        !self.at(&TokenKind::Eof)
    }

    /// Get the current token's span.
    pub(crate) fn current_span(&self) -> Span {
        let tok = self.peek();
        Span::single(tok.line, tok.col)
    }

    /// Get a span from start token to current position.
    pub(crate) fn span_from(&self, start_line: usize, start_col: usize) -> Span {
        let tok = self.peek();
        Span::new(start_line, start_col, tok.line, tok.col)
    }

    pub(crate) fn is_sketch(&self) -> bool {
        self.mode == ParseMode::Sketch
    }

    pub(crate) fn peek(&self) -> &Token {
        if self.pos >= self.tokens.len() {
            use crate::ast::Commitment;
            static EOF: Token = Token {
                kind: TokenKind::Eof,
                commitment: Commitment::None,
                line: 0,
                col: 0,
            };
            &EOF
        } else {
            &self.tokens[self.pos]
        }
    }

    pub(crate) fn peek_kind(&self) -> &TokenKind {
        &self.peek().kind
    }

    pub(crate) fn advance(&mut self) -> &Token {
        let tok = &self.tokens[self.pos];
        if !matches!(tok.kind, TokenKind::Eof) {
            self.pos += 1;
        }
        &self.tokens[self.pos.saturating_sub(1)]
    }

    pub(crate) fn at(&self, kind: &TokenKind) -> bool {
        *self.peek_kind() == *kind
    }

    pub(crate) fn expect(&mut self, kind: TokenKind, expected: &str) -> Result<&Token, ParseError> {
        if self.at(&kind) {
            Ok(self.advance())
        } else {
            let tok = self.peek();
            Err(ParseError::new(
                format!("expected {}, found {}", expected, tok.kind),
                tok.line,
                tok.col,
            ))
        }
    }

    /// Expect `>` or `>>` when closing generic angle brackets.
    /// `>>` is split into two `>` tokens so nested generics like `List<List<T>>` work.
    pub(crate) fn expect_gt(&mut self, expected: &str) -> Result<&Token, ParseError> {
        if self.at(&TokenKind::Gt) {
            Ok(self.advance())
        } else if self.at(&TokenKind::Shr) {
            self.tokens[self.pos].kind = TokenKind::Gt;
            let extra = Token {
                kind: TokenKind::Gt,
                commitment: self.tokens[self.pos].commitment,
                line: self.tokens[self.pos].line,
                col: self.tokens[self.pos].col,
            };
            self.tokens.insert(self.pos + 1, extra);
            Ok(self.advance())
        } else {
            let tok = self.peek();
            Err(ParseError::new(
                format!("expected {}, found {}", expected, tok.kind),
                tok.line,
                tok.col,
            ))
        }
    }

    pub(crate) fn expect_keyword(&mut self, kind: TokenKind) -> Result<Commitment, ParseError> {
        let tok = self.expect(kind, "keyword")?;
        Ok(tok.commitment)
    }

    pub(crate) fn expect_ident(&mut self) -> Result<String, ParseError> {
        let tok = self.peek();
        match &tok.kind {
            TokenKind::Ident(name) => {
                let name = name.clone();
                self.advance();
                Ok(name)
            }
            _ => Err(ParseError::new(
                format!("expected identifier, found {}", tok.kind),
                tok.line,
                tok.col,
            )),
        }
    }

    /// Expect a keyword token that doubles as a type name (i32, i64, f64, bool, string, unit)
    pub(crate) fn expect_keyword_as_type_name(&mut self) -> Result<String, ParseError> {
        let tok = self.peek();
        let name = match &tok.kind {
            TokenKind::I32 => "i32",
            TokenKind::I64 => "i64",
            TokenKind::F64 => "f64",
            TokenKind::Bool => "bool",
            TokenKind::StringKw => "string",
            TokenKind::Unit => "unit",
            _ => return Err(ParseError::new(
                format!("expected type name, found {}", tok.kind),
                tok.line,
                tok.col,
            )),
        };
        let name = name.to_string();
        self.advance();
        Ok(name)
    }

    pub(crate) fn skip_newlines(&mut self) {
        while matches!(self.peek_kind(), TokenKind::Newline) {
            self.advance();
        }
    }

    /// Check if current position is `alloc(Arena) {` or `alloc(System) {` or `alloc(Bump) {`
    pub(crate) fn is_alloc_block(&self) -> bool {
        if !self.at(&TokenKind::Alloc) {
            return false;
        }
        // Peek ahead: alloc must be followed by LParen
        if self.pos + 1 >= self.tokens.len() {
            return false;
        }
        if self.tokens[self.pos + 1].kind != TokenKind::LParen {
            return false;
        }
        // Check the token after LParen: must be Arena/System/Bump identifier
        if self.pos + 2 >= self.tokens.len() {
            return false;
        }
        matches!(
            &self.tokens[self.pos + 2].kind,
            TokenKind::Arena
        ) || matches!(
            &self.tokens[self.pos + 2].kind,
            TokenKind::Ident(name) if name == "System" || name == "Bump" || name == "Arena"
        )
    }

    pub(crate) fn match_semi(&mut self) {
        // SIF (Semicolon Inference): both explicit `;` and newline act as statement terminators
        if matches!(self.peek_kind(), TokenKind::Semi | TokenKind::Newline) {
            self.advance();
        }
    }
}
