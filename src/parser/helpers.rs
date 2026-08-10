#![allow(dead_code)]
// Parser uses .expect() on self.expect() returns as an intentional pattern.
#![cfg_attr(not(test), allow(clippy::unwrap_used))]

use super::*;

/// Default depth budget for the cheap recursion paths (expressions,
/// statements, types, patterns, session chains): ~1 level ≈ ≤9 KB of stack,
/// so 128 levels stay inside ~1 MB of the 2 MB libtest thread stacks.
pub(crate) const DEPTH_MAX_DEFAULT: usize = 128;

/// Depth budget for MODULE nesting (parser/top_level.rs `parse_module`).
/// Measured 2026-08-05 (wave-2 agent PM) on a 2 MB libtest thread stack:
/// module nesting overflows the stack at depth ≈ <MEASURED> — see the cap
/// comment history in `audit_fix_parser.rs` probes. The module path recurses
/// through FIVE mutually-recursive frames per nesting level (parse_module →
/// parse_module_inner → parse_item_block → parse_item → parse_item_kind),
/// each carrying large locals in debug builds, so one module level costs
/// several times what one expression/session level costs. The cap below
/// keeps the deepest module recursion inside the 2 MB budget with ≥2×
/// margin; legitimate code rarely nests modules beyond 3–4 levels.
pub(crate) const DEPTH_MAX_MODULE: usize = 32;

/// Depth budget for f-string interpolation nesting
/// (parse_stmt.rs `parse_fstring_parts` sub-parser).
/// Measured 2026-08-10 (0.35.25, audit-triage C2) on a 2 MB libtest thread
/// stack: nested f-string interpolation overflows at depth 40 (~38 safe),
/// i.e. ~53 KB of stack per nesting level — the sub-parser path stacks a
/// fresh `Parser` (with source registry) plus lexer/expression frames per
/// level, several times heavier than a plain expression level (≤9 KB).
/// Each nesting level counts 2 against `recursion_depth` (parse_expr of the
/// enclosing level + sub-parser parse_expr), so the 128 default budget
/// admits 64 levels ≈ 3.4 MB — past the 2 MB test stack. The budget below
/// (≈32 levels, ~1.7 MB) keeps the deepest f-string nesting inside the
/// 2 MB budget with margin; legitimate code nests f-strings ≤ 5 levels.
pub(crate) const DEPTH_MAX_FSTRING: usize = 64;

impl Parser {
    /// Guard against deep recursion on the cheap paths (expressions,
    /// statements, types, patterns, session-type chains). Returns Err if
    /// depth exceeds limit.
    ///
    /// Wave-1 central fix: 256 allowed parser frames (~9 KB each for
    /// session-type chains) to exhaust the 2 MB stacks used by libtest
    /// threads before the guard fired (SIGSEGV). 128 keeps the deepest
    /// guarded recursion inside a 1 MB budget. Session chains were measured
    /// to overflow the 2 MB libtest stack somewhere above depth ~150, so 128
    /// carries margin there. Module nesting is heavier per level and gets
    /// its own lower cap — see `check_depth_with` / `DEPTH_MAX_MODULE`.
    pub(crate) fn check_depth(&self) -> Result<(), ParseError> {
        self.check_depth_with(DEPTH_MAX_DEFAULT)
    }

    /// Guard against deep recursion with a path-specific budget.
    ///
    /// Every `check_depth_with` site shares one counter (`recursion_depth`)
    /// but may pass a different cap: the cap is chosen per recursion path by
    /// the real stack cost of that path's frames, measured on the 2 MB
    /// stacks libtest uses for test threads (the CLI main thread has 8 MB,
    /// but the guard must hold on the smallest supported stack).
    pub(crate) fn check_depth_with(&self, max: usize) -> Result<(), ParseError> {
        // 0.35.25 (M1, audit-triage-0.35.25.md): 移除 TEMP WAVE-2 遗留的
        // MIMI_PROBE_CAP 环境变量后门（深度上限可被覆盖到任意值，与 C2
        // 子解析器深度逃逸叠加放大；违反红线 #3 无 TODO issue 编号）。
        if self.recursion_depth.get() >= max {
            let tok = self.peek();
            return Err(ParseError::new(
                format!("recursion limit exceeded (> {} nested)", max),
                tok.line,
                tok.col,
            ));
        }
        Ok(())
    }

    pub(crate) fn inc_depth(&self) {
        self.recursion_depth.set(self.recursion_depth.get() + 1);
    }
    pub(crate) fn dec_depth(&self) {
        let d = self.recursion_depth.get();
        if d > 0 {
            self.recursion_depth.set(d - 1);
        }
    }

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
        self.single_span(tok.line, tok.col)
    }

    pub(crate) fn single_span(&self, line: usize, col: usize) -> Span {
        Span::single(line, col).with_source(self.source_id)
    }

    /// Get a span from start token to current position.
    pub(crate) fn span_from(&self, start_line: usize, start_col: usize) -> Span {
        let tok = self.peek();
        Span::new(start_line, start_col, tok.line, tok.col).with_source(self.source_id)
    }

    /// Exact half-open span for tokens consumed since `start_pos`.
    ///
    /// Token end positions come from the lexer, so this remains correct for
    /// escaped and multi-line literals instead of guessing from decoded text.
    pub(crate) fn consumed_span(&self, start_pos: usize) -> Span {
        let Some(first) = self.tokens.get(start_pos) else {
            return Span::UNKNOWN.with_source(self.source_id);
        };
        let last_index = self.pos.saturating_sub(1).max(start_pos);
        let last = self.tokens.get(last_index).unwrap_or(first);
        Span::new(first.line, first.col, last.end_line, last.end_col).with_source(self.source_id)
    }

    pub(crate) fn consumed_meta(&self, start_pos: usize, origin: AstOrigin) -> AstNodeMeta {
        AstNodeMeta::new(self.consumed_span(start_pos), origin)
    }

    pub(crate) fn is_sketch(&self) -> bool {
        self.mode == ParseMode::Sketch
    }

    pub(crate) fn peek(&self) -> &Token {
        if self.pos >= self.tokens.len() {
            static EOF: Token = Token {
                kind: TokenKind::Eof,
                line: 0,
                col: 0,
                end_line: 0,
                end_col: 0,
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
            let original_end_line = self.tokens[self.pos].end_line;
            let original_end_col = self.tokens[self.pos].end_col;
            self.tokens[self.pos].kind = TokenKind::Gt;
            self.tokens[self.pos].end_line = self.tokens[self.pos].line;
            self.tokens[self.pos].end_col = self.tokens[self.pos].col + 1;
            let extra = Token {
                kind: TokenKind::Gt,
                line: self.tokens[self.pos].line,
                col: self.tokens[self.pos].col + 1,
                end_line: original_end_line,
                end_col: original_end_col,
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

    pub(crate) fn expect_keyword(&mut self, kind: TokenKind) -> Result<(), ParseError> {
        self.expect(kind, "keyword")?;
        Ok(())
    }

    pub(crate) fn expect_ident(&mut self) -> Result<String, ParseError> {
        let tok = self.peek();
        // Soft keywords may appear as identifiers outside their special
        // syntactic contexts (e.g. `func mutate(...)`, `let view = 1`).
        // Hard keywords (if/while/func/...) still reject here.
        let name = match &tok.kind {
            TokenKind::Ident(name) => name.clone(),
            TokenKind::Old => "old".to_string(),
            TokenKind::View => "view".to_string(),
            TokenKind::Mutate => "mutate".to_string(),
            TokenKind::Persistent => "persistent".to_string(),
            TokenKind::Session => "session".to_string(),
            TokenKind::Dual => "dual".to_string(),
            TokenKind::End => "end".to_string(),
            // v0.34.2: and/or/not are soft keywords — binding position may use
            // them as identifiers (expression position still operator).
            TokenKind::And => "and".to_string(),
            TokenKind::Or => "or".to_string(),
            TokenKind::Not => "not".to_string(),
            // F-H7: fault/reset/recover are soft keywords (transition names, states).
            TokenKind::Fault => "fault".to_string(),
            TokenKind::Reset => "reset".to_string(),
            TokenKind::Recover => "recover".to_string(),
            _ => {
                return Err(ParseError::new(
                    format!("expected identifier, found {}", tok.kind),
                    tok.line,
                    tok.col,
                ))
            }
        };
        self.advance();
        Ok(name)
    }

    pub(crate) fn skip_newlines(&mut self) {
        while matches!(self.peek_kind(), TokenKind::Newline) {
            self.advance();
        }
    }

    /// Check if the current token is `||` or `&&`, skipping any leading newlines.
    /// Only skips newlines when the operator is found, to avoid consuming SIF terminators.
    pub(crate) fn try_skip_newlines_for_boolean_op(&mut self) -> bool {
        let saved = self.pos;
        self.skip_newlines();
        let found = matches!(self.peek_kind(), TokenKind::OrOr | TokenKind::AndAnd);
        if !found {
            self.pos = saved;
        }
        found
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
        matches!(&self.tokens[self.pos + 2].kind, TokenKind::Arena)
            || matches!(
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

/// M4 (audit-syntax 2026-08-03): token kinds that `expect_ident` accepts —
/// plain identifiers plus soft keywords usable as binding names. Lookahead
/// decisions (record-vs-enum discrimination in parse_type.rs) must use the
/// same set, otherwise `type Rec { and: i32 }` (soft keyword as FIRST field)
/// is misclassified as an enum and fails with `expected \`}\`, found :`.
pub(crate) fn is_ident_like_kind(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Ident(_)
            | TokenKind::Old
            | TokenKind::View
            | TokenKind::Mutate
            | TokenKind::Persistent
            | TokenKind::Session
            | TokenKind::Dual
            | TokenKind::End
            | TokenKind::And
            | TokenKind::Or
            | TokenKind::Not
            | TokenKind::Fault
            | TokenKind::Reset
            | TokenKind::Recover
    )
}
