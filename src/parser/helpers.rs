#![allow(dead_code)]
// Parser uses .expect() on self.expect() returns as an intentional pattern.
#![cfg_attr(not(test), allow(clippy::unwrap_used))]

use super::*;

/// Default depth budget for the cheap recursion paths (expressions,
/// statements, types, patterns, session chains): ~1 level ≈ ≤9 KB of stack,
/// so 128 levels stay inside ~1 MB of the 2 MB libtest thread stacks.
pub(crate) const DEPTH_MAX_DEFAULT: usize = 128;

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

/// Return true if `cleaned` (digits with underscores already removed) is the
/// textual magnitude of `i64::MIN` in one of the supported integer bases
/// (decimal, hexadecimal, binary, octal).
///
/// The positive decimal `9223372036854775808` cannot be parsed as `i64`, so
/// unary-minus and negative-pattern folding recognize all base spellings of
/// `2^63` and directly produce `Lit::Int(i64::MIN)`.
pub(crate) fn is_i64_min_magnitude(cleaned: &str) -> bool {
    if cleaned == "9223372036854775808" {
        return true;
    }
    let (digits, min_digits) = if cleaned.starts_with("0x") || cleaned.starts_with("0X") {
        (&cleaned[2..], "8000000000000000")
    } else if cleaned.starts_with("0b") || cleaned.starts_with("0B") {
        (
            &cleaned[2..],
            "1000000000000000000000000000000000000000000000000000000000000000",
        )
    } else if cleaned.starts_with("0o") || cleaned.starts_with("0O") {
        (&cleaned[2..], "1000000000000000000000")
    } else {
        return false;
    };
    digits == min_digits
}

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
    /// carries margin there.
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
        let Some(first) = self.tokens().get(start_pos) else {
            return Span::UNKNOWN.with_source(self.source_id);
        };
        let last_index = self.pos.saturating_sub(1).max(start_pos);
        let last = self.tokens().get(last_index).unwrap_or(first);
        Span::new(first.line, first.col, last.end_line, last.end_col).with_source(self.source_id)
    }

    pub(crate) fn consumed_meta(&self, start_pos: usize, origin: AstOrigin) -> AstNodeMeta {
        AstNodeMeta::new(self.consumed_span(start_pos), origin)
    }

    pub(crate) fn is_sketch(&self) -> bool {
        self.mode == ParseMode::Sketch
    }

    pub(crate) fn peek(&self) -> &Token {
        if self.pos >= self.tokens().len() {
            static EOF: Token = Token {
                kind: TokenKind::Eof,
                line: 0,
                col: 0,
                end_line: 0,
                end_col: 0,
            };
            &EOF
        } else {
            &self.tokens()[self.pos]
        }
    }

    pub(crate) fn peek_kind(&self) -> &TokenKind {
        &self.peek().kind
    }

    pub(crate) fn advance(&mut self) -> &Token {
        // Mirror peek()'s EOF fallback: if the parser is already past the
        // final token, return that synthetic EOF instead of panicking on a
        // direct index (batch1 P2-1). Position is updated before borrowing
        // the result to keep borrowck happy.
        let at_eof = matches!(
            self.tokens().get(self.pos).map(|t| &t.kind),
            Some(TokenKind::Eof)
        );
        if !at_eof {
            self.pos = self.pos.saturating_add(1);
        }
        let idx = self.pos.min(self.tokens().len().saturating_sub(1));
        &self.tokens()[idx]
    }

    /// Contextual-keyword promotion: rewrite the CURRENT token's kind
    /// (`parasteps`→Parasteps, `fault`→Fault). Materializes the owned copy.
    pub(crate) fn promote_current_kind(&mut self, kind: TokenKind) {
        self.materialize();
        let idx = self.pos.min(self.tokens().len() - 1);
        self.owned_tokens.as_mut().unwrap()[idx].kind = kind;
    }

    pub(crate) fn at(&self, kind: &TokenKind) -> bool {
        *self.peek_kind() == *kind
    }

    /// True when the current token is an identifier with the given name.
    /// 0.35.39: used for trivia keywords (desc/rule/mms) that were demoted
    /// from TokenKind to plain identifiers.
    pub(crate) fn at_ident_name(&self, name: &str) -> bool {
        matches!(self.peek_kind(), TokenKind::Ident(n) if n == name)
    }

    /// Consume the current token, requiring it to be an identifier with the
    /// given name. 0.35.39: mirrors `expect` for demoted trivia keywords.
    pub(crate) fn expect_ident_name(&mut self, name: &str) -> Result<(), ParseError> {
        if self.at_ident_name(name) {
            self.advance();
            Ok(())
        } else {
            let tok = self.peek();
            Err(ParseError::new(
                format!("expected `{name}`, found {}", tok.kind),
                tok.line,
                tok.col,
            ))
        }
    }

    pub(crate) fn expect(&mut self, kind: TokenKind, expected: &str) -> Result<(), ParseError> {
        if self.at(&kind) {
            self.advance();
            Ok(())
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
    /// `>>` is split into two `>` reads so nested generics like
    /// `List<List<T>>` work. 0.39.136: the split is served through the
    /// one-token overlay instead of inserting into the shared token vector —
    /// this is what lets every sub-parser share one immutable allocation.
    pub(crate) fn expect_gt(&mut self, expected: &str) -> Result<Token, ParseError> {
        if self.at(&TokenKind::Gt) {
            let first = self.peek().clone();
            self.advance();
            Ok(first)
        } else if self.at(&TokenKind::Shr) {
            self.materialize();
            let toks = self.owned_tokens.as_mut().unwrap();
            let p = self.pos.min(toks.len() - 1);
            let original_end_line = toks[p].end_line;
            let original_end_col = toks[p].end_col;
            toks[p].kind = TokenKind::Gt;
            toks[p].end_line = toks[p].line;
            toks[p].end_col = toks[p].col + 1;
            let extra = Token {
                kind: TokenKind::Gt,
                line: toks[p].line,
                col: toks[p].col + 1,
                end_line: original_end_line,
                end_col: original_end_col,
            };
            toks.insert(p + 1, extra);
            Ok(self.advance().clone())
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
