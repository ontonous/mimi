//! Flow-based Mimi lexer — state machine outer loop over the scanning logic.
//!
//! States: Start → LineStart → Dispatch → (scanning helper) → LineStart → ... → Done
//! Each `Step` event processes one outer-loop iteration.
//! Transitions consume self and return new state (no output — tokens accumulate in acc).

use crate::lexer::errors::{
    dedent_mismatch, indent_not_multiple_of_four, invalid_escape, tabs_not_allowed,
    unexpected_character, unterminated_block_comment, unterminated_escape, unterminated_fstring,
    unterminated_fstring_escape, unterminated_interpolation, unterminated_string, LexerError,
};
use crate::lexer::keywords::keyword_or_ident;
use crate::lexer::token::{LexerMode, Token, TokenKind};
use std::str::Chars;

// ── Position advancement macros ──────────────────────────────────────

/// Advance one character, discard it, return new position.
///
/// LX-H6: at EOF `advance` is a no-op (peeked stays `None`). Callers must
/// check `peek()` before looping on `next!` — never use `next!` alone as a
/// loop bound.
macro_rules! next {
    ($pos:expr) => {{
        let (__p, _) = $pos.advance();
        __p
    }};
}

/// Advance one character, return (new_position, consumed_char).
macro_rules! consume {
    ($pos:expr) => {{
        let (__p, __c) = $pos.advance();
        (__p, __c)
    }};
}

// ── Shared position state ────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LexerPos<'a> {
    #[allow(dead_code)]
    pub src: &'a str,
    pub chars: Chars<'a>,
    pub line: usize,
    pub col: usize,
    pub peeked: Option<char>,
}

impl<'a> LexerPos<'a> {
    fn new(source: &'a str) -> Self {
        let mut chars = source.chars();
        let peeked = chars.next();
        LexerPos {
            src: source,
            chars,
            line: 1,
            col: 1,
            peeked,
        }
    }

    fn advance(mut self) -> (Self, Option<char>) {
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
        (self, c)
    }

    fn peek(&self) -> Option<char> {
        self.peeked
    }

    fn skip_whitespace_inline(self) -> Self {
        let mut pos = self;
        while let Some(' ') | Some('\t') | Some('\r') = pos.peek() {
            pos = next!(pos);
        }
        pos
    }

    fn skip_line_comment(self) -> Self {
        let mut pos = self;
        loop {
            match pos.peek() {
                Some('\n') | None => break,
                Some(_) => pos = next!(pos),
            }
        }
        pos
    }

    fn skip_block_comment(self) -> Result<Self, LexerError> {
        let pos = next!(self);
        let mut pos = next!(pos);
        // LX-M1: use usize; cap nesting to avoid pathological inputs.
        const MAX_BLOCK_COMMENT_DEPTH: usize = 10_000;
        let mut depth: usize = 1;
        while depth > 0 {
            match pos.peek() {
                None => return Err(unterminated_block_comment(pos.line, pos.col)),
                Some('*') => {
                    pos = next!(pos);
                    if pos.peek() == Some('/') {
                        pos = next!(pos);
                        depth -= 1;
                    }
                }
                Some('/') => {
                    pos = next!(pos);
                    if pos.peek() == Some('*') {
                        pos = next!(pos);
                        if depth >= MAX_BLOCK_COMMENT_DEPTH {
                            return Err(unterminated_block_comment(pos.line, pos.col));
                        }
                        depth += 1;
                    }
                }
                Some(_) => pos = next!(pos),
            }
        }
        Ok(pos)
    }

    fn scan_string(self) -> Result<(Self, String), LexerError> {
        let mut pos = next!(self);
        let mut s = String::new();
        loop {
            match pos.peek() {
                None => return Err(unterminated_string(pos.line, pos.col)),
                Some('"') => {
                    pos = next!(pos);
                    break;
                }
                Some('\\') => {
                    pos = next!(pos);
                    match pos.peek() {
                        Some('n') => {
                            s.push('\n');
                            pos = next!(pos);
                        }
                        Some('t') => {
                            s.push('\t');
                            pos = next!(pos);
                        }
                        Some('r') => {
                            s.push('\r');
                            pos = next!(pos);
                        }
                        Some('\\') => {
                            s.push('\\');
                            pos = next!(pos);
                        }
                        Some('"') => {
                            s.push('"');
                            pos = next!(pos);
                        }
                        Some('0') => {
                            s.push('\0');
                            pos = next!(pos);
                        }
                        Some('x') => {
                            let start_col = pos.col;
                            pos = next!(pos);
                            let mut hex = String::with_capacity(2);
                            for _ in 0..2 {
                                match pos.peek() {
                                    Some(c) if c.is_ascii_hexdigit() => {
                                        hex.push(c);
                                        pos = next!(pos);
                                    }
                                    _ => break,
                                }
                            }
                            if hex.len() != 2 {
                                return Err(invalid_escape("\\x", pos.line, start_col));
                            }
                            // SAFETY: hex.len() == 2 with only ASCII hexdigits
                            // (validated above), so from_str_radix is infallible.
                            let value = u8::from_str_radix(&hex, 16).map_err(|e| {
                                invalid_escape(&format!("\\x{}", e), pos.line, start_col)
                            })?;
                            s.push(value as char);
                        }
                        Some('u') => {
                            let start_col = pos.col;
                            pos = next!(pos);
                            let mut code = String::new();
                            match pos.peek() {
                                Some('{') => {
                                    pos = next!(pos);
                                    while let Some(c) = pos.peek() {
                                        if c.is_ascii_hexdigit() || c == '_' {
                                            code.push(c);
                                            pos = next!(pos);
                                        } else {
                                            break;
                                        }
                                    }
                                    if pos.peek() != Some('}') {
                                        return Err(invalid_escape("\\u{", pos.line, start_col));
                                    }
                                    if code.is_empty() {
                                        return Err(invalid_escape("\\u{}", pos.line, start_col));
                                    }
                                    pos = next!(pos);
                                }
                                _ => {
                                    for _ in 0..4 {
                                        match pos.peek() {
                                            Some(c) if c.is_ascii_hexdigit() => {
                                                code.push(c);
                                                pos = next!(pos);
                                            }
                                            _ => break,
                                        }
                                    }
                                    if code.len() != 4 {
                                        return Err(invalid_escape("\\u", pos.line, start_col));
                                    }
                                }
                            }
                            let cleaned: String = code.chars().filter(|c| *c != '_').collect();
                            // SAFETY: cleaned contains only ASCII hexdigits and
                            // its length is bounded by the caller, so the parse
                            // cannot fail.
                            let value = u32::from_str_radix(&cleaned, 16).map_err(|e| {
                                invalid_escape(&format!("\\u{}", e), pos.line, start_col)
                            })?;
                            match char::from_u32(value) {
                                Some(ch) => s.push(ch),
                                None => return Err(invalid_escape("\\u", pos.line, start_col)),
                            }
                        }
                        Some(c) => {
                            return Err(invalid_escape(&format!("\\{}", c), pos.line, pos.col))
                        }
                        None => return Err(unterminated_escape(pos.line, pos.col)),
                    }
                }
                Some(c) => {
                    s.push(c);
                    pos = next!(pos);
                }
            }
        }
        Ok((pos, s))
    }

    fn scan_fstring(self) -> Result<(Self, String), LexerError> {
        let pos = next!(self);
        let mut pos = next!(pos);
        let mut s = String::new();
        loop {
            match pos.peek() {
                None => return Err(unterminated_fstring(pos.line, pos.col)),
                Some('"') => {
                    pos = next!(pos);
                    break;
                }
                Some('\\') => {
                    // LX-C4: keep ALL escapes as raw `\` + char so the parser's
                    // parse_fstring_parts is the single unescape site (no mixed
                    // decoded-vs-raw representation).
                    pos = next!(pos);
                    match pos.peek() {
                        Some('n') => {
                            s.push_str("\\n");
                            pos = next!(pos);
                        }
                        Some('t') => {
                            s.push_str("\\t");
                            pos = next!(pos);
                        }
                        Some('r') => {
                            s.push_str("\\r");
                            pos = next!(pos);
                        }
                        Some('\\') => {
                            s.push_str("\\\\");
                            pos = next!(pos);
                        }
                        Some('"') => {
                            // Keep as \" so parser can accept escaped quotes in text.
                            s.push_str("\\\"");
                            pos = next!(pos);
                        }
                        Some('{') => {
                            s.push_str("\\{");
                            pos = next!(pos);
                        }
                        Some('}') => {
                            s.push_str("\\}");
                            pos = next!(pos);
                        }
                        Some('0') => {
                            s.push_str("\\0");
                            pos = next!(pos);
                        }
                        Some('x') => {
                            s.push_str("\\x");
                            let start_col = pos.col;
                            pos = next!(pos);
                            let mut got = 0;
                            for _ in 0..2 {
                                match pos.peek() {
                                    Some(c) if c.is_ascii_hexdigit() => {
                                        s.push(c);
                                        pos = next!(pos);
                                        got += 1;
                                    }
                                    _ => break,
                                }
                            }
                            if got != 2 {
                                return Err(invalid_escape("\\x", pos.line, start_col));
                            }
                        }
                        Some('u') => {
                            s.push_str("\\u");
                            let start_col = pos.col;
                            pos = next!(pos);
                            if pos.peek() == Some('{') {
                                s.push('{');
                                pos = next!(pos);
                                let hex_start = s.len();
                                while let Some(c) = pos.peek() {
                                    if c.is_ascii_hexdigit() || c == '_' {
                                        s.push(c);
                                        pos = next!(pos);
                                    } else {
                                        break;
                                    }
                                }
                                if pos.peek() != Some('}') {
                                    return Err(invalid_escape("\\u{", pos.line, start_col));
                                }
                                if s.len() == hex_start {
                                    return Err(invalid_escape("\\u{}", pos.line, start_col));
                                }
                                s.push('}');
                                pos = next!(pos);
                            } else {
                                let mut got = 0;
                                for _ in 0..4 {
                                    match pos.peek() {
                                        Some(c) if c.is_ascii_hexdigit() => {
                                            s.push(c);
                                            pos = next!(pos);
                                            got += 1;
                                        }
                                        _ => break,
                                    }
                                }
                                if got != 4 {
                                    return Err(invalid_escape("\\u", pos.line, start_col));
                                }
                            }
                        }
                        Some(c) => {
                            return Err(invalid_escape(&format!("\\{}", c), pos.line, pos.col))
                        }
                        None => return Err(unterminated_fstring_escape(pos.line, pos.col)),
                    }
                }
                Some('{') => {
                    s.push('{');
                    pos = next!(pos);
                    let mut depth = 1;
                    while let Some(c) = pos.peek() {
                        if c == '{' {
                            depth += 1;
                        } else if c == '}' {
                            depth -= 1;
                            if depth == 0 {
                                s.push('}');
                                pos = next!(pos);
                                break;
                            }
                        }
                        s.push(c);
                        pos = next!(pos);
                    }
                    if depth != 0 {
                        return Err(unterminated_interpolation(pos.line, pos.col));
                    }
                }
                Some(c) => {
                    s.push(c);
                    pos = next!(pos);
                }
            }
        }
        Ok((pos, s))
    }

    fn scan_number(self) -> (Self, TokenKind) {
        let mut pos = self;
        let mut s = String::new();
        let mut is_float = false;
        if let Some('0') = pos.peek() {
            let mut tmp = pos.chars.clone();
            match tmp.next() {
                Some('x') | Some('X') => {
                    s.push('0');
                    pos = next!(pos);
                    s.push('x');
                    pos = next!(pos);
                    while let Some(c) = pos.peek() {
                        if c.is_ascii_hexdigit() {
                            s.push(c);
                            pos = next!(pos);
                        } else if c == '_' {
                            // LX-C3: separator only between digits.
                            let mut tmp = pos.chars.clone();
                            match tmp.next() {
                                Some(n) if n.is_ascii_hexdigit() => {
                                    s.push(c);
                                    pos = next!(pos);
                                }
                                _ => break,
                            }
                        } else {
                            break;
                        }
                    }
                    while s.ends_with('_') {
                        s.pop();
                    }
                    return (pos, TokenKind::Int(s));
                }
                Some('b') | Some('B') => {
                    s.push('0');
                    pos = next!(pos);
                    s.push('b');
                    pos = next!(pos);
                    while let Some(c) = pos.peek() {
                        if c == '0' || c == '1' {
                            s.push(c);
                            pos = next!(pos);
                        } else if c == '_' {
                            let mut tmp = pos.chars.clone();
                            match tmp.next() {
                                Some(n) if n == '0' || n == '1' => {
                                    s.push(c);
                                    pos = next!(pos);
                                }
                                _ => break,
                            }
                        } else {
                            break;
                        }
                    }
                    while s.ends_with('_') {
                        s.pop();
                    }
                    return (pos, TokenKind::Int(s));
                }
                Some('o') | Some('O') => {
                    s.push('0');
                    pos = next!(pos);
                    s.push('o');
                    pos = next!(pos);
                    while let Some(c) = pos.peek() {
                        if c.is_ascii_digit() && c != '8' && c != '9' {
                            s.push(c);
                            pos = next!(pos);
                        } else if c == '_' {
                            let mut tmp = pos.chars.clone();
                            match tmp.next() {
                                Some(n)
                                    if (n.is_ascii_digit() && n != '8' && n != '9') || n == '_' =>
                                {
                                    s.push(c);
                                    pos = next!(pos);
                                }
                                _ => break,
                            }
                        } else {
                            break;
                        }
                    }
                    while s.ends_with('_') {
                        s.pop();
                    }
                    return (pos, TokenKind::Int(s));
                }
                _ => {}
            }
        }
        while let Some(c) = pos.peek() {
            if c.is_ascii_digit() {
                s.push(c);
                pos = next!(pos);
            } else if c == '.' {
                if is_float {
                    break;
                }
                let mut tmp = pos.chars.clone();
                if tmp.next().map(|x| x.is_ascii_digit()).unwrap_or(false) {
                    is_float = true;
                    s.push(c);
                    pos = next!(pos);
                } else {
                    break;
                }
            } else if c == '_' {
                // LX-C3: digit separators must be between digits, not trailing.
                // Peek next char; only accept '_' when followed by a digit (or
                // another separator that will itself be validated later).
                let mut tmp = pos.chars.clone();
                match tmp.next() {
                    // F-H9: separators only between digits; consecutive '__' is invalid
                    // and is left for the next token (surface as error later).
                    Some(n) if n.is_ascii_digit() => {
                        s.push(c);
                        pos = next!(pos);
                    }
                    _ => break, // trailing/double '_' → leave for next token / error path
                }
            } else {
                break;
            }
        }
        // LX-C3: strip a trailing '_' that slipped through (e.g. "1_").
        while s.ends_with('_') {
            s.pop();
        }
        // LE-H4: Scientific notation: 1e5, 1.5e-3, 2E+10
        // HIGH fix: "1e" without following digits should not consume 'e'.
        // Peek ahead to verify there are valid digits (possibly after sign)
        // before consuming 'e'. If not, leave 'e' for the next token.
        if let Some(ch) = pos.peek() {
            if ch == 'e' || ch == 'E' {
                // pos.chars points to characters AFTER the peeked 'e'/'E'.
                let mut tmp = pos.chars.clone();
                let first_after_e = tmp.next();
                let first_digit = if first_after_e == Some('+') || first_after_e == Some('-') {
                    tmp.next()
                } else {
                    first_after_e
                };
                if first_digit.is_some_and(|d| d.is_ascii_digit()) {
                    s.push(ch);
                    pos = next!(pos);
                    if let Some(sign) = pos.peek() {
                        if sign == '+' || sign == '-' {
                            s.push(sign);
                            pos = next!(pos);
                        }
                    }
                    is_float = true;
                    while let Some(d) = pos.peek() {
                        if d.is_ascii_digit() || d == '_' {
                            s.push(d);
                            pos = next!(pos);
                        } else {
                            break;
                        }
                    }
                }
            }
        }
        let kind = if is_float {
            TokenKind::Float(s)
        } else {
            TokenKind::Int(s)
        };
        (pos, kind)
    }

    fn scan_ident(self, first: char) -> (Self, String) {
        let mut pos = self;
        let mut s = String::new();
        s.push(first);
        while let Some(c) = pos.peek() {
            if c.is_alphanumeric() || c == '_' {
                s.push(c);
                pos = next!(pos);
            } else {
                break;
            }
        }
        (pos, s)
    }
}

// ── Accumulator ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LexerAcc {
    pub tokens: Vec<Token>,
    pub indent_stack: Vec<usize>,
}

impl LexerAcc {
    fn new() -> Self {
        LexerAcc {
            tokens: vec![],
            indent_stack: vec![0],
        }
    }
}

// ── Events ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub enum LexerEvent {
    Step,
    #[allow(dead_code)]
    Complete,
}

// ── Flow macros ──────────────────────────────────────────────────────

macro_rules! state_continue {
    ($variant:ident { $($field:ident $(: $val:expr)?),* $(,)? }, $pos:expr, $mode:expr, $at_line_start:expr, $acc:expr) => {
        Ok(LexerState::$variant {
            pos: $pos,
            mode: $mode,
            at_line_start: $at_line_start,
            acc: $acc,
            $($field $(: $val)?),*
        })
    };
}

// ── Lexer Flow state machine ─────────────────────────────────────────

#[derive(Debug)]
pub enum LexerState<'a> {
    // LX-C7: `at_line_start` is live state for LineStart; Start/Dispatch carry
    // it for uniform `state_continue!` field propagation (not dead).
    Start {
        pos: LexerPos<'a>,
        mode: LexerMode,
        #[allow(dead_code)]
        at_line_start: bool,
        acc: LexerAcc,
    },
    LineStart {
        pos: LexerPos<'a>,
        mode: LexerMode,
        at_line_start: bool,
        acc: LexerAcc,
    },
    Dispatch {
        pos: LexerPos<'a>,
        mode: LexerMode,
        #[allow(dead_code)]
        at_line_start: bool,
        acc: LexerAcc,
    },
    Done(Result<LexerAcc, LexerError>),
}

impl<'a> LexerState<'a> {
    pub fn new(source: &'a str, mode: LexerMode) -> Self {
        LexerState::Start {
            pos: LexerPos::new(source),
            mode,
            at_line_start: true,
            acc: LexerAcc::new(),
        }
    }

    /// Transition the lexer state machine.
    ///
    /// Each `Step` event processes one outer-loop iteration:
    ///   Start → LineStart → Dispatch → (scan token or skip) → LineStart → ...
    /// Returns the new state with tokens accumulated in `acc.tokens`.
    pub fn transition(self, event: LexerEvent) -> Result<Self, LexerError> {
        match (self, event) {
            // ── Start: handle shebang ─────────────────────────────
            (LexerState::Start { pos, mode, acc, .. }, LexerEvent::Step) => {
                let mut pos = pos;
                if pos.peek() == Some('#') {
                    let mut tmp = pos.chars.clone();
                    if tmp.next() == Some('!') {
                        loop {
                            match pos.peek() {
                                Some('\n') | None => break,
                                Some(_) => pos = next!(pos),
                            }
                        }
                    }
                }
                state_continue!(LineStart {}, pos, mode, true, acc)
            }

            // ── LineStart: process indentation ─────────────────────
            (
                LexerState::LineStart {
                    pos,
                    mode,
                    at_line_start,
                    acc,
                },
                LexerEvent::Step,
            ) => {
                if !at_line_start {
                    return state_continue!(Dispatch {}, pos, mode, false, acc);
                }
                let mut pos = pos;
                let mut acc = acc;
                loop {
                    let mut spaces = 0usize;
                    // LX-C2: only spaces count as indent; skip CR so \r\n files work.
                    while pos.peek() == Some(' ') {
                        pos = next!(pos);
                        spaces += 1;
                    }
                    if pos.peek() == Some('\t') {
                        return Err(tabs_not_allowed(pos.line, pos.col));
                    }
                    // Consume a lone CR before newline (Windows / mixed endings).
                    if pos.peek() == Some('\r') {
                        pos = next!(pos);
                    }
                    if pos.peek().is_none() {
                        let acc = advance_to_done(pos, mode, acc);
                        return Ok(LexerState::Done(Ok(acc)));
                    }
                    let mut is_comment_line = false;
                    if pos.peek() == Some('/') {
                        if pos.chars.clone().next() == Some('*') {
                            // LX-C1/H4: multi-line block comments advance line/col via
                            // skip_block_comment; do NOT reuse pre-comment `spaces` for
                            // trailing code on the `*/` line.
                            pos = pos.skip_block_comment()?;
                            pos = pos.skip_whitespace_inline();
                            // After block comment, CR may precede newline.
                            if pos.peek() == Some('\r') {
                                pos = next!(pos);
                            }
                            if pos.peek() == Some('\n') || pos.peek().is_none() {
                                is_comment_line = true;
                            } else {
                                // Trailing tokens after `*/` on the same line: indent
                                // was for the comment, not this content. Dispatch mid-line.
                                return state_continue!(Dispatch {}, pos, mode, false, acc);
                            }
                        } else if pos.chars.clone().next() == Some('/') {
                            is_comment_line = true;
                            pos = pos.skip_line_comment();
                        }
                    }
                    // LX-H1: blank line is newline, CR, or EOF after indent spaces.
                    let is_blank = matches!(pos.peek(), Some('\n') | Some('\r') | None);
                    if is_comment_line || is_blank {
                        if pos.peek() == Some('\r') {
                            pos = next!(pos);
                        }
                        if pos.peek() == Some('\n') {
                            pos = next!(pos);
                        }
                        continue;
                    }
                    if mode == LexerMode::Sketch {
                        if spaces % 4 != 0 {
                            return Err(indent_not_multiple_of_four(pos.line, pos.col));
                        }
                        // LX-C6: never panic — stack is seeded with [0]; fall back to 0.
                        let current = *acc.indent_stack.last().unwrap_or(&0);
                        if spaces > current {
                            acc.indent_stack.push(spaces);
                            acc.tokens.push(Token {
                                kind: TokenKind::Indent,
                                line: pos.line,
                                col: spaces,
                                end_line: pos.line,
                                end_col: spaces,
                            });
                            return state_continue!(Dispatch {}, pos, mode, false, acc);
                        } else if spaces < current {
                            // CRITICAL #7 fix: accumulate ALL dedent tokens in this step.
                            while *acc.indent_stack.last().unwrap_or(&0) > spaces {
                                acc.indent_stack.pop();
                                // Never pop below the root indent level.
                                if acc.indent_stack.is_empty() {
                                    acc.indent_stack.push(0);
                                    break;
                                }
                                acc.tokens.push(Token {
                                    kind: TokenKind::Dedent,
                                    line: pos.line,
                                    col: spaces,
                                    end_line: pos.line,
                                    end_col: spaces,
                                });
                            }
                            if *acc.indent_stack.last().unwrap_or(&0) != spaces {
                                return Err(dedent_mismatch(pos.line, pos.col));
                            }
                        }
                    }
                    return state_continue!(Dispatch {}, pos, mode, false, acc);
                }
            }

            // ── Dispatch: peek character ──────────────────────────
            (
                LexerState::Dispatch {
                    pos, mode, mut acc, ..
                },
                LexerEvent::Step,
            ) => {
                let mut pos = pos.skip_whitespace_inline();
                let line = pos.line;
                let col = pos.col;
                let c = match pos.peek() {
                    Some(c) => c,
                    None => {
                        pos = next!(pos);
                        let mut acc = acc;
                        if mode == LexerMode::Sketch {
                            while acc.indent_stack.len() > 1 {
                                acc.indent_stack.pop();
                                acc.tokens.push(Token {
                                    kind: TokenKind::Dedent,
                                    line: pos.line,
                                    col: pos.col,
                                    end_line: pos.line,
                                    end_col: pos.col,
                                });
                            }
                        }
                        acc.tokens.push(Token {
                            kind: TokenKind::Eof,
                            line: pos.line,
                            col: pos.col,
                            end_line: pos.line,
                            end_col: pos.col,
                        });
                        return Ok(LexerState::Done(Ok(acc)));
                    }
                };

                if c == '\n' {
                    pos = next!(pos);
                    acc.tokens.push(Token {
                        kind: TokenKind::Newline,
                        line,
                        col,
                        end_line: pos.line,
                        end_col: pos.col,
                    });
                    return state_continue!(LineStart {}, pos, mode, true, acc);
                }

                // Line continuation: backslash + newline
                if c == '\\' {
                    pos = next!(pos);
                    let np = pos.skip_whitespace_inline();
                    if np.peek() == Some('\n') {
                        let np = next!(np);
                        return state_continue!(LineStart {}, np, mode, true, acc);
                    }
                    return Err(unexpected_character('\\', line, col));
                }

                // Block comment or line comment
                if c == '/' {
                    if pos.chars.clone().next() == Some('*') {
                        let pos = pos.skip_block_comment()?;
                        // Block comment is mid-line, continue dispatching on same line
                        // (LineStart indentation processing expects line-start position).
                        return state_continue!(Dispatch {}, pos, mode, false, acc);
                    }
                    if pos.chars.clone().next() == Some('/') {
                        let pos = pos.skip_line_comment();
                        return state_continue!(LineStart {}, pos, mode, true, acc);
                    }
                }

                // LX-H2: bare `#` (not `#[` attribute / shebang) is a line comment
                // so Python-style `# comment` does not emit a stray Hash token.
                if c == '#' {
                    let next = pos.chars.clone().next();
                    if next != Some('[') {
                        let pos = pos.skip_line_comment();
                        return state_continue!(LineStart {}, pos, mode, true, acc);
                    }
                }

                let (pos, kind) = lex_scan_token(pos, c, line, col)?;
                acc.tokens.push(Token {
                    kind,
                    line,
                    col,
                    end_line: pos.line,
                    end_col: pos.col,
                });
                state_continue!(LineStart {}, pos, mode, false, acc)
            }

            // ── Complete: force finalization ───────────────────────
            (state, LexerEvent::Complete) => match state {
                LexerState::Start { acc, .. }
                | LexerState::LineStart { acc, .. }
                | LexerState::Dispatch { acc, .. } => Ok(LexerState::Done(Ok(acc))),
                done @ LexerState::Done(_) => Ok(done),
            },

            // ── Done + Step: identity ─────────────────────────────
            (done @ LexerState::Done(_), LexerEvent::Step) => Ok(done),
        }
    }
}

// ── Token scanning dispatch (loose Flow) ─────────────────────────────

fn lex_scan_token(
    pos: LexerPos,
    c: char,
    line: usize,
    col: usize,
) -> Result<(LexerPos, TokenKind), LexerError> {
    match c {
        'f' if pos.chars.clone().next() == Some('"') => {
            let (pos, s) = pos.scan_fstring()?;
            Ok((pos, TokenKind::FString(s)))
        }
        '"' => {
            let (pos, s) = pos.scan_string()?;
            Ok((pos, TokenKind::String(s)))
        }
        '0'..='9' => Ok(pos.scan_number()),
        'a'..='z' | 'A'..='Z' | '_' => {
            let (pos, first_ch) = consume!(pos);
            // LX-C6: never panic — if consume returned None, fall through as unexpected.
            let Some(first_ch) = first_ch else {
                return Err(unexpected_character(c, line, col));
            };
            let (pos, name) = pos.scan_ident(first_ch);
            Ok((pos, keyword_or_ident(&name)))
        }
        '+' => {
            let pos = next!(pos);
            if pos.peek() == Some('=') {
                Ok((next!(pos), TokenKind::PlusEq))
            } else {
                Ok((pos, TokenKind::Plus))
            }
        }
        '-' => {
            let pos = next!(pos);
            if pos.peek() == Some('>') {
                Ok((next!(pos), TokenKind::Arrow))
            } else if pos.peek() == Some('=') {
                Ok((next!(pos), TokenKind::MinusEq))
            } else {
                Ok((pos, TokenKind::Minus))
            }
        }
        '*' => {
            let pos = next!(pos);
            if pos.peek() == Some('*') {
                Ok((next!(pos), TokenKind::Pow))
            } else if pos.peek() == Some('=') {
                Ok((next!(pos), TokenKind::StarEq))
            } else {
                Ok((pos, TokenKind::Star))
            }
        }
        '/' => {
            let pos = next!(pos);
            if pos.peek() == Some('=') {
                Ok((next!(pos), TokenKind::SlashEq))
            } else {
                Ok((pos, TokenKind::Slash))
            }
        }
        '%' => Ok((next!(pos), TokenKind::Percent)),
        '=' => {
            let pos = next!(pos);
            if pos.peek() == Some('=') {
                Ok((next!(pos), TokenKind::EqEq))
            } else if pos.peek() == Some('>') {
                Ok((next!(pos), TokenKind::FatArrow))
            } else {
                Ok((pos, TokenKind::Eq))
            }
        }
        '!' => {
            // LX-H9: keep Bang for `quote!` / macro-invoke syntax; parser also
            // accepts NotOp for unary not. NotOp remains a soft alias for users
            // who emit it explicitly — not dead, dual-matched in parse_expr.
            let pos = next!(pos);
            if pos.peek() == Some('=') {
                Ok((next!(pos), TokenKind::Ne))
            } else {
                Ok((pos, TokenKind::Bang))
            }
        }
        '<' => {
            let pos = next!(pos);
            if pos.peek() == Some('=') {
                Ok((next!(pos), TokenKind::Le))
            } else if pos.peek() == Some('<') {
                Ok((next!(pos), TokenKind::Shl))
            } else {
                Ok((pos, TokenKind::Lt))
            }
        }
        '>' => {
            let pos = next!(pos);
            if pos.peek() == Some('=') {
                Ok((next!(pos), TokenKind::Ge))
            } else if pos.peek() == Some('>') {
                Ok((next!(pos), TokenKind::Shr))
            } else {
                Ok((pos, TokenKind::Gt))
            }
        }
        '&' => {
            let pos = next!(pos);
            if pos.peek() == Some('&') {
                Ok((next!(pos), TokenKind::AndAnd))
            } else if pos.peek() == Some('=') {
                Ok((next!(pos), TokenKind::BitAndEq))
            } else {
                Ok((pos, TokenKind::BitAnd))
            }
        }
        '|' => {
            let pos = next!(pos);
            if pos.peek() == Some('|') {
                Ok((next!(pos), TokenKind::OrOr))
            } else if pos.peek() == Some('=') {
                Ok((next!(pos), TokenKind::BitOrEq))
            } else if pos.peek() == Some('>') {
                Ok((next!(pos), TokenKind::PipeArrow))
            } else {
                Ok((pos, TokenKind::BitOr))
            }
        }
        '^' => {
            let pos = next!(pos);
            if pos.peek() == Some('=') {
                Ok((next!(pos), TokenKind::BitXorEq))
            } else {
                Ok((pos, TokenKind::BitXor))
            }
        }
        '~' => Ok((next!(pos), TokenKind::Tilde)),
        '$' => {
            // LX-C5: bare `$` used to hard-error with no recovery. Emit a
            // distinctive Ident so the rest of the file can still tokenize;
            // the parser/checker will reject `$` as an unexpected identifier.
            let pos = next!(pos);
            if pos.peek() == Some('(') {
                Ok((next!(pos), TokenKind::DollarParen))
            } else {
                Ok((pos, TokenKind::Ident("$".to_string())))
            }
        }
        '(' => Ok((next!(pos), TokenKind::LParen)),
        ')' => Ok((next!(pos), TokenKind::RParen)),
        '{' => Ok((next!(pos), TokenKind::LBrace)),
        '}' => Ok((next!(pos), TokenKind::RBrace)),
        '[' => Ok((next!(pos), TokenKind::LBracket)),
        ']' => Ok((next!(pos), TokenKind::RBracket)),
        ':' => {
            let pos = next!(pos);
            if pos.peek() == Some(':') {
                Ok((next!(pos), TokenKind::ColonColon))
            } else {
                Ok((pos, TokenKind::Colon))
            }
        }
        ';' => Ok((next!(pos), TokenKind::Semi)),
        ',' => Ok((next!(pos), TokenKind::Comma)),
        '.' => {
            let pos = next!(pos);
            if pos.peek() == Some('.') && pos.chars.clone().next() == Some('.') {
                Ok((next!(next!(pos)), TokenKind::Ellipsis))
            } else if pos.peek() == Some('.') {
                Ok((next!(pos), TokenKind::DotDot))
            } else {
                Ok((pos, TokenKind::Dot))
            }
        }
        '?' => Ok((next!(pos), TokenKind::Question)),
        '@' => Ok((next!(pos), TokenKind::At)),
        '#' => Ok((next!(pos), TokenKind::Hash)),
        '\'' => Ok((next!(pos), TokenKind::Tick)),
        _ => Err(unexpected_character(c, line, col)),
    }
}

// ── Finalization helper (used by LineStart when EOF) ─────────────────

fn advance_to_done(pos: LexerPos, mode: LexerMode, mut acc: LexerAcc) -> LexerAcc {
    if mode == LexerMode::Sketch {
        while acc.indent_stack.len() > 1 {
            acc.indent_stack.pop();
            acc.tokens.push(Token {
                kind: TokenKind::Dedent,
                line: pos.line,
                col: pos.col,
                end_line: pos.line,
                end_col: pos.col,
            });
        }
    }
    acc.tokens.push(Token {
        kind: TokenKind::Eof,
        line: pos.line,
        col: pos.col,
        end_line: pos.line,
        end_col: pos.col,
    });
    acc
}

// ── Entry point ──────────────────────────────────────────────────────

/// Tokenize source text using the Flow-based lexer state machine.
pub fn flow_tokenize(source: &str, mode: LexerMode) -> Result<Vec<Token>, LexerError> {
    let mut state = LexerState::new(source, mode);
    loop {
        state = state.transition(LexerEvent::Step)?;
        match state {
            LexerState::Done(Ok(acc)) => return Ok(acc.tokens),
            LexerState::Done(Err(e)) => return Err(e),
            _ => {}
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    fn tokenize_legacy(source: &str, mode: LexerMode) -> Result<Vec<Token>, LexerError> {
        if mode == LexerMode::Production {
            Lexer::new(source).legacy_tokenize()
        } else {
            Lexer::new_sketch(source).legacy_tokenize()
        }
    }

    fn compare_token_sets(flow: &[Token], legacy: &[Token], src: &str) {
        if flow.len() != legacy.len() {
            panic!(
                "token count mismatch for {:?}: flow={}, legacy={}\nflow: {:?}\nlegacy: {:?}",
                src,
                flow.len(),
                legacy.len(),
                flow.iter().map(|t| &t.kind).collect::<Vec<_>>(),
                legacy.iter().map(|t| &t.kind).collect::<Vec<_>>(),
            );
        }
        for (i, (f, l)) in flow.iter().zip(legacy.iter()).enumerate() {
            assert_eq!(f.kind, l.kind, "token {} kind mismatch for {:?}", i, src);
            assert_eq!(
                (f.line, f.col, f.end_line, f.end_col),
                (l.line, l.col, l.end_line, l.end_col),
                "token {} span mismatch for {:?}",
                i,
                src
            );
        }
    }

    #[test]
    fn token_end_positions_follow_source_text_not_decoded_values() {
        let tokens = flow_tokenize("\"a\\n\\\"b\" -> value", LexerMode::Production).unwrap();
        assert_eq!(
            (
                tokens[0].line,
                tokens[0].col,
                tokens[0].end_line,
                tokens[0].end_col
            ),
            (1, 1, 1, 9)
        );
        assert_eq!(
            (
                tokens[1].line,
                tokens[1].col,
                tokens[1].end_line,
                tokens[1].end_col
            ),
            (1, 10, 1, 12)
        );
        assert_eq!(
            (
                tokens[2].line,
                tokens[2].col,
                tokens[2].end_line,
                tokens[2].end_col
            ),
            (1, 13, 1, 18)
        );
    }

    #[test]
    fn test_flow_lexer_empty() {
        let src = "";
        let flow = flow_tokenize(src, LexerMode::Production).unwrap();
        let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
        compare_token_sets(&flow, &legacy, src);
    }

    #[test]
    fn test_flow_lexer_simple() {
        let src = "func main() -> i32 { 42 }";
        let flow = flow_tokenize(src, LexerMode::Production).unwrap();
        let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
        compare_token_sets(&flow, &legacy, src);
    }

    #[test]
    fn test_flow_lexer_multiline() {
        let src = "func main() -> i32 {\n    42\n}";
        let flow = flow_tokenize(src, LexerMode::Production).unwrap();
        let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
        compare_token_sets(&flow, &legacy, src);
    }

    #[test]
    fn test_flow_lexer_string() {
        let src = r#"func main() { let s = "hello\nworld"; }"#;
        let flow = flow_tokenize(src, LexerMode::Production).unwrap();
        let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
        compare_token_sets(&flow, &legacy, src);
    }

    #[test]
    fn test_flow_lexer_fstring() {
        let src = r#"func main() { let s = f"hello {name}"; }"#;
        let flow = flow_tokenize(src, LexerMode::Production).unwrap();
        let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
        compare_token_sets(&flow, &legacy, src);
    }

    #[test]
    fn test_flow_lexer_numbers() {
        let src = "func main() { let x = 42; let y = 3.14; let z = 0xFF; }";
        let flow = flow_tokenize(src, LexerMode::Production).unwrap();
        let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
        compare_token_sets(&flow, &legacy, src);
    }

    #[test]
    fn test_flow_lexer_operators() {
        let src = "func main() { let x = a + b - c * d / e; }";
        let flow = flow_tokenize(src, LexerMode::Production).unwrap();
        let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
        compare_token_sets(&flow, &legacy, src);
    }

    #[test]
    fn test_flow_lexer_compound_ops() {
        let src = "func main() { x += 1; y -= 2; z *= 3; w /= 4; }";
        let flow = flow_tokenize(src, LexerMode::Production).unwrap();
        let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
        compare_token_sets(&flow, &legacy, src);
    }

    #[test]
    fn test_flow_lexer_comparison() {
        let src = "func main() { let a = x == y; let b = x != y; let c = x <= y; }";
        let flow = flow_tokenize(src, LexerMode::Production).unwrap();
        let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
        compare_token_sets(&flow, &legacy, src);
    }

    #[test]
    fn test_flow_lexer_comments() {
        let src = "func main() {\n    // line comment\n    /* block */ 42\n}";
        let flow = flow_tokenize(src, LexerMode::Production).unwrap();
        let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
        compare_token_sets(&flow, &legacy, src);
    }

    #[test]
    fn test_flow_lexer_line_continuation() {
        let src = "func main() {\\\n    42 }";
        let flow = flow_tokenize(src, LexerMode::Production).unwrap();
        let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
        compare_token_sets(&flow, &legacy, src);
    }

    #[test]
    fn test_flow_lexer_sketch_mode() {
        let src = "func main():\n    pass\n";
        let flow = flow_tokenize(src, LexerMode::Sketch).unwrap();
        let legacy = tokenize_legacy(src, LexerMode::Sketch).unwrap();
        compare_token_sets(&flow, &legacy, src);
    }

    #[test]
    fn test_flow_lexer_nested_block_comment() {
        let src = "func main() { /* outer /* inner */ ok */ 42 }";
        let flow = flow_tokenize(src, LexerMode::Production).unwrap();
        let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
        compare_token_sets(&flow, &legacy, src);
    }

    #[test]
    fn test_flow_lexer_shebang() {
        let src = "#!/usr/bin/env mimi\nfunc main() { 42 }";
        let flow = flow_tokenize(src, LexerMode::Production).unwrap();
        let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
        compare_token_sets(&flow, &legacy, src);
    }

    #[test]
    fn test_flow_lexer_ellipsis_dotdot() {
        let src = "func main() { let r = 1..10; let e = 1...10; }";
        let flow = flow_tokenize(src, LexerMode::Production).unwrap();
        let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
        compare_token_sets(&flow, &legacy, src);
    }

    #[test]
    fn test_flow_lexer_arrow_fat_arrow() {
        let src = "func main() -> i32 { let f = x => x + 1; }";
        let flow = flow_tokenize(src, LexerMode::Production).unwrap();
        let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
        compare_token_sets(&flow, &legacy, src);
    }

    #[test]
    fn test_flow_lexer_binary_hex() {
        let src = "func main() { let b = 0b1010; let h = 0xFF; }";
        let flow = flow_tokenize(src, LexerMode::Production).unwrap();
        let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
        compare_token_sets(&flow, &legacy, src);
    }

    #[test]
    fn test_flow_lexer_scientific() {
        let src = "func main() { let e = 1.5e-3; }";
        let flow = flow_tokenize(src, LexerMode::Production).unwrap();
        let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
        compare_token_sets(&flow, &legacy, src);
    }

    #[test]
    fn test_flow_lexer_bitwise_ops() {
        let src = "func main() { let a = x & y; let o = x | y; let xr = x ^ y; }";
        let flow = flow_tokenize(src, LexerMode::Production).unwrap();
        let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
        compare_token_sets(&flow, &legacy, src);
    }

    #[test]
    fn test_flow_lexer_shift_ops() {
        let src = "func main() { let l = x << y; let r = x >> y; }";
        let flow = flow_tokenize(src, LexerMode::Production).unwrap();
        let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
        compare_token_sets(&flow, &legacy, src);
    }

    #[test]
    fn test_flow_lexer_pipe() {
        let src = "func main() { let r = x |> f; }";
        let flow = flow_tokenize(src, LexerMode::Production).unwrap();
        let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
        compare_token_sets(&flow, &legacy, src);
    }

    #[test]
    fn test_flow_lexer_string_unicode_escape() {
        let src = r#"func main() { let s = "\u{1F600}"; }"#;
        let flow = flow_tokenize(src, LexerMode::Production).unwrap();
        let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
        compare_token_sets(&flow, &legacy, src);
    }

    #[test]
    fn test_flow_lexer_string_hex_escape() {
        let src = r#"func main() { let s = "\x41"; }"#;
        let flow = flow_tokenize(src, LexerMode::Production).unwrap();
        let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
        compare_token_sets(&flow, &legacy, src);
    }

    #[test]
    fn test_flow_lexer_keywords() {
        let src = "func main() { if true { let x = false; } }";
        let flow = flow_tokenize(src, LexerMode::Production).unwrap();
        let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
        compare_token_sets(&flow, &legacy, src);
    }

    #[test]
    fn test_flow_lexer_real_world_files() {
        let test_sources = [
            "",
            " ",
            "\n",
            "func main() -> i32 { 0 }",
            "func f() {\n    1\n}\n",
            r#""\n\t\r\\\"""#,
            r#"f"hello {name}""#,
            "func main() {\n    /* comment */\n    42\n}",
            "func main() { a + b - c * d / e % f }",
            "func main() { a == b && c != d || e <= f }",
            "func main() { a & b | c ^ d ~ e }",
            "func main() { a << b >> c }",
            "func main() { x += 1; y -= 2; z *= 3; w /= 4; x |= y; x &= y; x ^= y; }",
            "func main() { x |> f |> g }",
            "func main() { let x = std::io::print_line; }",
            "func main() { let x = $(command); }",
            "func main() { 1..10; 1...10; }",
        ];

        for src in &test_sources {
            let flow = flow_tokenize(src, LexerMode::Production).unwrap();
            let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
            compare_token_sets(&flow, &legacy, src);
        }
    }

    #[test]
    fn test_flow_lexer_errors() {
        let error_cases = [
            r#""unterminated"#,
            r#"f"unterminated f-string"#,
            r#"f"unterminated {interpolation"#,
            "func main() {\n\tlet x = 1;\n}",
            "$",
        ];

        for src in &error_cases {
            let flow = flow_tokenize(src, LexerMode::Production);
            let legacy = tokenize_legacy(src, LexerMode::Production);
            match (&flow, &legacy) {
                (Err(_), Err(_)) => {}
                (Ok(f), Ok(l)) => compare_token_sets(f, l, src),
                _ => panic!(
                    "error mismatch for {:?}: flow={:?}, legacy={:?}",
                    src,
                    flow.as_ref().map(|t| t.len()),
                    legacy.as_ref().map(|t| t.len()),
                ),
            }
        }
    }
}
