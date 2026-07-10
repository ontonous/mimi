//! Flow-based Mimi lexer — state machine outer loop over the scanning logic.
//!
//! States: Start → LineStart → Dispatch → (scanning helper) → LineStart → ... → Done
//! Each `Step` event processes one outer-loop iteration (skip whitespace, scan one token, etc.).

use crate::lexer::errors::{
    dedent_mismatch, indent_not_multiple_of_four, tabs_not_allowed,
    unexpected_character, unexpected_dollar, unterminated_block_comment,
    unterminated_escape, unterminated_fstring, unterminated_fstring_escape,
    unterminated_interpolation, unterminated_string, invalid_escape, LexerError,
};
use crate::lexer::keywords::keyword_or_ident;
use crate::lexer::token::{LexerMode, Token, TokenKind};
use std::str::Chars;

// ── Shared position state ────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LexerPos<'a> {
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
        loop {
            match pos.peek() {
                Some(' ') | Some('\t') | Some('\r') => {
                    let (new_pos, _) = pos.advance();
                    pos = new_pos;
                }
                _ => break,
            }
        }
        pos
    }

    fn skip_line_comment(self) -> Self {
        let mut pos = self;
        loop {
            match pos.peek() {
                Some('\n') | None => break,
                Some(_) => {
                    let (new_pos, _) = pos.advance();
                    pos = new_pos;
                }
            }
        }
        pos
    }

    fn skip_block_comment(self) -> Result<Self, LexerError> {
        let (pos, _) = self.advance();
        let (mut pos, _) = pos.advance();
        let mut depth: i32 = 1;
        while depth > 0 {
            match pos.peek() {
                None => return Err(unterminated_block_comment(pos.line, pos.col)),
                Some('*') => {
                    let (new_pos, _) = pos.advance();
                    pos = new_pos;
                    if pos.peek() == Some('/') {
                        let (new_pos, _) = pos.advance();
                        pos = new_pos;
                        depth -= 1;
                    }
                }
                Some('/') => {
                    let (new_pos, _) = pos.advance();
                    pos = new_pos;
                    if pos.peek() == Some('*') {
                        let (new_pos, _) = pos.advance();
                        pos = new_pos;
                        depth += 1;
                    }
                }
                Some(_) => {
                    let (new_pos, _) = pos.advance();
                    pos = new_pos;
                }
            }
        }
        Ok(pos)
    }

    fn scan_string(self) -> Result<(Self, String), LexerError> {
        let (pos, _) = self.advance();
        let mut pos = pos;
        let mut s = String::new();
        loop {
            match pos.peek() {
                None => return Err(unterminated_string(pos.line, pos.col)),
                Some('"') => {
                    let (new_pos, _) = pos.advance();
                    pos = new_pos;
                    break;
                }
                Some('\\') => {
                    let (new_pos, _ch) = pos.advance();
                    pos = new_pos;
                    match pos.peek() {
                        Some('n') => { s.push('\n'); let (np, _) = pos.advance(); pos = np; }
                        Some('t') => { s.push('\t'); let (np, _) = pos.advance(); pos = np; }
                        Some('r') => { s.push('\r'); let (np, _) = pos.advance(); pos = np; }
                        Some('\\') => { s.push('\\'); let (np, _) = pos.advance(); pos = np; }
                        Some('"') => { s.push('"'); let (np, _) = pos.advance(); pos = np; }
                        Some('0') => { s.push('\0'); let (np, _) = pos.advance(); pos = np; }
                        Some('x') => {
                            let start_col = pos.col;
                            let (np, _) = pos.advance();
                            pos = np;
                            let mut hex = String::with_capacity(2);
                            for _ in 0..2 {
                                match pos.peek() {
                                    Some(c) if c.is_ascii_hexdigit() => {
                                        hex.push(c);
                                        let (np, _) = pos.advance();
                                        pos = np;
                                    }
                                    _ => break,
                                }
                            }
                            if hex.len() != 2 {
                                return Err(invalid_escape("\\x", pos.line, start_col));
                            }
                            let value = u8::from_str_radix(&hex, 16).unwrap();
                            s.push(value as char);
                        }
                        Some('u') => {
                            let start_col = pos.col;
                            let (np, _) = pos.advance();
                            pos = np;
                            let mut code = String::new();
                            match pos.peek() {
                                Some('{') => {
                                    let (np, _) = pos.advance();
                                    pos = np;
                                    while let Some(c) = pos.peek() {
                                        if c.is_ascii_hexdigit() || c == '_' {
                                            code.push(c);
                                            let (np, _) = pos.advance();
                                            pos = np;
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
                                    let (np, _) = pos.advance();
                                    pos = np;
                                }
                                _ => {
                                    for _ in 0..4 {
                                        match pos.peek() {
                                            Some(c) if c.is_ascii_hexdigit() => {
                                                code.push(c);
                                                let (np, _) = pos.advance();
                                                pos = np;
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
                            let value = u32::from_str_radix(&cleaned, 16).unwrap();
                            match char::from_u32(value) {
                                Some(ch) => s.push(ch),
                                None => return Err(invalid_escape("\\u", pos.line, start_col)),
                            }
                        }
                        Some(c) => return Err(invalid_escape(&format!("\\{}", c), pos.line, pos.col)),
                        None => return Err(unterminated_escape(pos.line, pos.col)),
                    }
                }
                Some(c) => {
                    s.push(c);
                    let (np, _) = pos.advance();
                    pos = np;
                }
            }
        }
        Ok((pos, s))
    }

    fn scan_fstring(self) -> Result<(Self, String), LexerError> {
        let (pos, _) = self.advance();
        let (mut pos, _) = pos.advance();
        let mut s = String::new();
        loop {
            match pos.peek() {
                None => return Err(unterminated_fstring(pos.line, pos.col)),
                Some('"') => {
                    let (np, _) = pos.advance();
                    pos = np;
                    break;
                }
                Some('\\') => {
                    let (np, _) = pos.advance();
                    pos = np;
                    match pos.peek() {
                        Some('n') => { s.push('\n'); let (np, _) = pos.advance(); pos = np; }
                        Some('t') => { s.push('\t'); let (np, _) = pos.advance(); pos = np; }
                        Some('r') => { s.push('\r'); let (np, _) = pos.advance(); pos = np; }
                        Some('\\') => { s.push_str("\\\\"); let (np, _) = pos.advance(); pos = np; }
                        Some('"') => { s.push('"'); let (np, _) = pos.advance(); pos = np; }
                        Some('{') => { s.push_str("\\{"); let (np, _) = pos.advance(); pos = np; }
                        Some('}') => { s.push_str("\\}"); let (np, _) = pos.advance(); pos = np; }
                        Some('0') => { s.push_str("\\0"); let (np, _) = pos.advance(); pos = np; }
                        Some('x') => {
                            s.push_str("\\x");
                            let start_col = pos.col;
                            let (np, _) = pos.advance();
                            pos = np;
                            let mut got = 0;
                            for _ in 0..2 {
                                match pos.peek() {
                                    Some(c) if c.is_ascii_hexdigit() => {
                                        s.push(c);
                                        let (np, _) = pos.advance();
                                        pos = np;
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
                            let (np, _) = pos.advance();
                            pos = np;
                            if pos.peek() == Some('{') {
                                s.push('{');
                                let (np, _) = pos.advance();
                                pos = np;
                                let hex_start = s.len();
                                while let Some(c) = pos.peek() {
                                    if c.is_ascii_hexdigit() || c == '_' {
                                        s.push(c);
                                        let (np, _) = pos.advance();
                                        pos = np;
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
                                let (np, _) = pos.advance();
                                pos = np;
                            } else {
                                let mut got = 0;
                                for _ in 0..4 {
                                    match pos.peek() {
                                        Some(c) if c.is_ascii_hexdigit() => {
                                            s.push(c);
                                            let (np, _) = pos.advance();
                                            pos = np;
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
                        Some(c) => return Err(invalid_escape(&format!("\\{}", c), pos.line, pos.col)),
                        None => return Err(unterminated_fstring_escape(pos.line, pos.col)),
                    }
                }
                Some('{') => {
                    s.push('{');
                    let (np, _) = pos.advance();
                    pos = np;
                    let mut depth = 1;
                    while let Some(c) = pos.peek() {
                        if c == '{' {
                            depth += 1;
                        } else if c == '}' {
                            depth -= 1;
                            if depth == 0 {
                                s.push('}');
                                let (np, _) = pos.advance();
                                pos = np;
                                break;
                            }
                        }
                        s.push(c);
                        let (np, _) = pos.advance();
                        pos = np;
                    }
                    if depth != 0 {
                        return Err(unterminated_interpolation(pos.line, pos.col));
                    }
                }
                Some(c) => {
                    s.push(c);
                    let (np, _) = pos.advance();
                    pos = np;
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
            let next = tmp.next();
            match next {
                Some('x') | Some('X') => {
                    s.push('0');
                    let (np, _) = pos.advance();
                    pos = np;
                    s.push('x');
                    let (np, _) = pos.advance();
                    pos = np;
                    while let Some(c) = pos.peek() {
                        if c.is_ascii_hexdigit() || c == '_' {
                            s.push(c);
                            let (np, _) = pos.advance();
                            pos = np;
                        } else {
                            break;
                        }
                    }
                    return (pos, TokenKind::Int(s));
                }
                Some('b') | Some('B') => {
                    s.push('0');
                    let (np, _) = pos.advance();
                    pos = np;
                    s.push('b');
                    let (np, _) = pos.advance();
                    pos = np;
                    while let Some(c) = pos.peek() {
                        if c == '0' || c == '1' || c == '_' {
                            s.push(c);
                            let (np, _) = pos.advance();
                            pos = np;
                        } else {
                            break;
                        }
                    }
                    return (pos, TokenKind::Int(s));
                }
                Some('o') | Some('O') => {
                    s.push('0');
                    let (np, _) = pos.advance();
                    pos = np;
                    s.push('o');
                    let (np, _) = pos.advance();
                    pos = np;
                    while let Some(c) = pos.peek() {
                        if (c.is_ascii_digit() && c != '8' && c != '9') || c == '_' {
                            s.push(c);
                            let (np, _) = pos.advance();
                            pos = np;
                        } else {
                            break;
                        }
                    }
                    return (pos, TokenKind::Int(s));
                }
                _ => {}
            }
        }
        while let Some(c) = pos.peek() {
            if c.is_ascii_digit() {
                s.push(c);
                let (np, _) = pos.advance();
                pos = np;
            } else if c == '.' {
                if is_float {
                    break;
                }
                let mut tmp = pos.chars.clone();
                if tmp.next().map(|x| x.is_ascii_digit()).unwrap_or(false) {
                    is_float = true;
                    s.push(c);
                    let (np, _) = pos.advance();
                    pos = np;
                } else {
                    break;
                }
            } else if c == '_' {
                s.push(c);
                let (np, _) = pos.advance();
                pos = np;
            } else {
                break;
            }
        }
        if let Some(ch) = pos.peek() {
            if ch == 'e' || ch == 'E' {
                s.push(ch);
                let (np, _) = pos.advance();
                pos = np;
                if let Some(sign) = pos.peek() {
                    if sign == '+' || sign == '-' {
                        s.push(sign);
                        let (np, _) = pos.advance();
                        pos = np;
                    }
                }
                if pos.peek().map_or(false, |d| d.is_ascii_digit()) {
                    is_float = true;
                    while let Some(d) = pos.peek() {
                        if d.is_ascii_digit() || d == '_' {
                            s.push(d);
                            let (np, _) = pos.advance();
                            pos = np;
                        } else {
                            break;
                        }
                    }
                }
            }
        }
        let kind = if is_float { TokenKind::Float(s) } else { TokenKind::Int(s) };
        (pos, kind)
    }

    fn scan_ident(self, first: char) -> (Self, String) {
        let mut pos = self;
        let mut s = String::new();
        s.push(first);
        while let Some(c) = pos.peek() {
            if c.is_alphanumeric() || c == '_' {
                s.push(c);
                let (np, _) = pos.advance();
                pos = np;
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
    pub errors: Vec<LexerError>,
}

impl LexerAcc {
    fn new() -> Self {
        LexerAcc {
            tokens: vec![],
            indent_stack: vec![0],
            errors: vec![],
        }
    }
}

// ── Events ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub enum LexerEvent {
    /// Advance to the next token-producing iteration
    Step,
    /// Force finalization (flush de-dents, emit EOF)
    Complete,
}

// ── Output ───────────────────────────────────────────────────────────

/// Optional token output from a transition.
pub type LexerOutput = Option<Token>;

// ── Flow macros ──────────────────────────────────────────────────────

macro_rules! state_continue {
    ($variant:ident { $($field:ident $(: $val:expr)?),* $(,)? }, $pos:expr, $mode:expr, $at_line_start:expr, $acc:expr) => {
        Ok((
            LexerState::$variant {
                pos: $pos,
                mode: $mode,
                at_line_start: $at_line_start,
                acc: $acc,
                $($field $(: $val)?),*
            },
            None,
        ))
    };
}

macro_rules! state_yield {
    ($variant:ident { $($field:ident $(: $val:expr)?),* $(,)? }, $pos:expr, $mode:expr, $at_line_start:expr, $acc:expr, $output:expr) => {
        Ok((
            LexerState::$variant {
                pos: $pos,
                mode: $mode,
                at_line_start: $at_line_start,
                acc: $acc,
                $($field $(: $val)?),*
            },
            Some($output),
        ))
    };
}

macro_rules! state_done {
    ($acc:expr) => {
        Ok((LexerState::Done(Ok($acc)), None))
    };
}

// ── Lexer Flow state machine ─────────────────────────────────────────

#[derive(Debug)]
pub enum LexerState<'a> {
    /// Initial — process shebang, then go to LineStart
    Start {
        pos: LexerPos<'a>,
        mode: LexerMode,
        at_line_start: bool,
        acc: LexerAcc,
    },
    /// At beginning of a line — handle indentation
    LineStart {
        pos: LexerPos<'a>,
        mode: LexerMode,
        at_line_start: bool,
        acc: LexerAcc,
    },
    /// Peeked a character — decide what to do
    Dispatch {
        pos: LexerPos<'a>,
        mode: LexerMode,
        at_line_start: bool,
        acc: LexerAcc,
        ch: char,
    },
    /// Done — holds the final result
    Done(Result<LexerAcc, LexerError>),
}

impl<'a> LexerState<'a> {
    /// Create the initial state from source text.
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
    /// Each successful transition returns `(new_state, Option<Token>)`.
    /// The `Complete` event flushes dedents and emits EOF.
    pub fn transition(
        self,
        event: LexerEvent,
    ) -> Result<(Self, LexerOutput), LexerError> {
        match (self, event) {
            // ── Start: handle shebang ─────────────────────────────
            (
                LexerState::Start {
                    pos, mode, at_line_start: _, mut acc,
                },
                LexerEvent::Step,
            ) => {
                let mut pos = pos;
                // Skip shebang at the very beginning
                if pos.peek() == Some('#') {
                    let mut tmp = pos.chars.clone();
                    if tmp.next() == Some('!') {
                        // Skip everything until newline
                        loop {
                            match pos.peek() {
                                Some('\n') | None => break,
                                Some(_) => {
                                    let (np, _) = pos.advance();
                                    pos = np;
                                }
                            }
                        }
                    }
                }
                state_continue!(LineStart { }, pos, mode, true, acc)
            }

            // ── LineStart: process indentation ─────────────────────
            (
                LexerState::LineStart {
                    mut pos, mode, at_line_start, mut acc,
                },
                LexerEvent::Step,
            ) => {
                if !at_line_start {
                    return state_continue!(Dispatch { ch: '\0' }, pos, mode, false, acc);
                }
                loop {
                    let mut spaces = 0usize;
                    while pos.peek() == Some(' ') {
                        let (np, _) = pos.advance();
                        pos = np;
                        spaces += 1;
                    }
                    if pos.peek() == Some('\t') {
                        return Err(tabs_not_allowed(pos.line, pos.col));
                    }
                    if pos.peek().is_none() {
                        let (pos, _) = pos.advance();
                        return state_continue!(Dispatch { ch: '\0' }, pos, mode, false, acc);
                    }
                    let mut is_comment_line = false;
                    if pos.peek() == Some('/') {
                        let mut tmp = pos.chars.clone();
                        if tmp.next() == Some('/') {
                            is_comment_line = true;
                            pos = pos.skip_line_comment();
                        } else if tmp.next() == Some('*') {
                            pos = pos.skip_block_comment()?;
                            pos = pos.skip_whitespace_inline();
                            if pos.peek() == Some('\n') || pos.peek().is_none() {
                                is_comment_line = true;
                            }
                        }
                    }
                    let is_blank = pos.peek() == Some('\n');
                    if is_comment_line || is_blank {
                        if pos.peek() == Some('\n') {
                            let (np, _) = pos.advance();
                            pos = np;
                        }
                        continue;
                    }
                    // real content
                    if mode == LexerMode::Sketch {
                        if !spaces.is_multiple_of(4) {
                            return Err(indent_not_multiple_of_four(pos.line, pos.col));
                        }
                        let current = *acc.indent_stack.last().unwrap_or(&0);
                        if spaces > current {
                            acc.indent_stack.push(spaces);
                            let token = Token {
                                kind: TokenKind::Indent,
                                line: pos.line,
                                col: spaces,
                            };
                            acc.tokens.push(token.clone());
                            return state_yield!(Dispatch { ch: '\0' }, pos, mode, false, acc, token);
                        } else if spaces < current {
                            while *acc.indent_stack.last().unwrap_or(&0) > spaces {
                                acc.indent_stack.pop();
                                let token = Token {
                                    kind: TokenKind::Dedent,
                                    line: pos.line,
                                    col: spaces,
                                };
                                acc.tokens.push(token.clone());
                                return state_yield!(Dispatch { ch: '\0' }, pos, mode, false, acc, token);
                            }
                            if *acc.indent_stack.last().unwrap_or(&0) != spaces {
                                return Err(dedent_mismatch(pos.line, pos.col));
                            }
                        }
                    }
                    return state_continue!(Dispatch { ch: '\0' }, pos, mode, false, acc);
                }
            }

            // ── Dispatch: peek character ──────────────────────────
            (
                LexerState::Dispatch {
                    pos, mode, at_line_start: _, mut acc, ch: _,
                },
                LexerEvent::Step,
            ) => {
                let mut pos = pos.skip_whitespace_inline();
                let line = pos.line;
                let col = pos.col;
                let c = match pos.peek() {
                    Some(c) => c,
                    None => {
                        // EOF
                        let (pos, _) = pos.advance();
                        return flush_and_done(pos, mode, acc);
                    }
                };

                // Newline
                if c == '\n' {
                    let (np, _) = pos.advance();
                    let token = Token {
                        kind: TokenKind::Newline,
                        line,
                        col,
                    };
                    acc.tokens.push(token.clone());
                    return state_yield!(LineStart { }, np, mode, true, acc, token);
                }

                // Line continuation: backslash
                if c == '\\' {
                    let (np, _) = pos.advance();
                    let np = np.skip_whitespace_inline();
                    if np.peek() == Some('\n') {
                        let (np, _) = np.advance();
                        return state_continue!(LineStart { }, np, mode, true, acc);
                    }
                    return Err(unexpected_character('\\', line, col));
                }

                // Block comment: /* ... */
                if c == '/' {
                    let mut tmp = pos.chars.clone();
                    if tmp.next() == Some('*') {
                        let pos = pos.skip_block_comment()?;
                        return state_continue!(LineStart { }, pos, mode, true, acc);
                    }
                }

                // Line comment: //
                if c == '/' {
                    let mut tmp = pos.chars.clone();
                    if tmp.next() == Some('/') {
                        let pos = pos.skip_line_comment();
                        return state_continue!(LineStart { }, pos, mode, true, acc);
                    }
                }

                // Scan a token (not at line start — false for at_line_start)
                let (pos, kind) = lex_scan_token(pos, c, line, col)?;
                let token = Token { kind, line, col };
                acc.tokens.push(token.clone());
                state_yield!(LineStart { }, pos, mode, false, acc, token)
            }

            // ── Complete: force finalization ───────────────────────
            (state, LexerEvent::Complete) => match state {
                LexerState::Start { acc, .. }
                | LexerState::LineStart { acc, .. }
                | LexerState::Dispatch { acc, .. }
                    => Ok((LexerState::Done(Ok(acc)), None)),
                LexerState::Done(result) => {
                    let result = result?;
                    Ok((LexerState::Done(Ok(result)), None))
                }
            },

            // ── Done + Step: already finished, stay Done ──────────
            (done @ LexerState::Done(_), LexerEvent::Step) => Ok((done, None)),
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
            let (pos, first_ch) = pos.advance();
            let (pos, name) = pos.scan_ident(first_ch.unwrap_or('\0'));
            Ok((pos, keyword_or_ident(&name)))
        }
        '+' => {
            let (pos, _) = pos.advance();
            if pos.peek() == Some('=') {
                let (pos, _) = pos.advance();
                Ok((pos, TokenKind::PlusEq))
            } else {
                Ok((pos, TokenKind::Plus))
            }
        }
        '-' => {
            let (pos, _) = pos.advance();
            if pos.peek() == Some('>') {
                let (pos, _) = pos.advance();
                Ok((pos, TokenKind::Arrow))
            } else if pos.peek() == Some('=') {
                let (pos, _) = pos.advance();
                Ok((pos, TokenKind::MinusEq))
            } else {
                Ok((pos, TokenKind::Minus))
            }
        }
        '*' => {
            let (pos, _) = pos.advance();
            if pos.peek() == Some('*') {
                let (pos, _) = pos.advance();
                Ok((pos, TokenKind::Pow))
            } else if pos.peek() == Some('=') {
                let (pos, _) = pos.advance();
                Ok((pos, TokenKind::StarEq))
            } else {
                Ok((pos, TokenKind::Star))
            }
        }
        '/' => {
            let (pos, _) = pos.advance();
            if pos.peek() == Some('=') {
                let (pos, _) = pos.advance();
                Ok((pos, TokenKind::SlashEq))
            } else {
                Ok((pos, TokenKind::Slash))
            }
        }
        '%' => {
            let (pos, _) = pos.advance();
            Ok((pos, TokenKind::Percent))
        }
        '=' => {
            let (pos, _) = pos.advance();
            if pos.peek() == Some('=') {
                let (pos, _) = pos.advance();
                Ok((pos, TokenKind::EqEq))
            } else if pos.peek() == Some('>') {
                let (pos, _) = pos.advance();
                Ok((pos, TokenKind::FatArrow))
            } else {
                Ok((pos, TokenKind::Eq))
            }
        }
        '!' => {
            let (pos, _) = pos.advance();
            if pos.peek() == Some('=') {
                let (pos, _) = pos.advance();
                Ok((pos, TokenKind::Ne))
            } else {
                Ok((pos, TokenKind::Bang))
            }
        }
        '<' => {
            let (pos, _) = pos.advance();
            if pos.peek() == Some('=') {
                let (pos, _) = pos.advance();
                Ok((pos, TokenKind::Le))
            } else if pos.peek() == Some('<') {
                let (pos, _) = pos.advance();
                Ok((pos, TokenKind::Shl))
            } else {
                Ok((pos, TokenKind::Lt))
            }
        }
        '>' => {
            let (pos, _) = pos.advance();
            if pos.peek() == Some('=') {
                let (pos, _) = pos.advance();
                Ok((pos, TokenKind::Ge))
            } else if pos.peek() == Some('>') {
                let (pos, _) = pos.advance();
                Ok((pos, TokenKind::Shr))
            } else {
                Ok((pos, TokenKind::Gt))
            }
        }
        '&' => {
            let (pos, _) = pos.advance();
            if pos.peek() == Some('&') {
                let (pos, _) = pos.advance();
                Ok((pos, TokenKind::AndAnd))
            } else if pos.peek() == Some('=') {
                let (pos, _) = pos.advance();
                Ok((pos, TokenKind::BitAndEq))
            } else {
                Ok((pos, TokenKind::BitAnd))
            }
        }
        '|' => {
            let (pos, _) = pos.advance();
            if pos.peek() == Some('|') {
                let (pos, _) = pos.advance();
                Ok((pos, TokenKind::OrOr))
            } else if pos.peek() == Some('=') {
                let (pos, _) = pos.advance();
                Ok((pos, TokenKind::BitOrEq))
            } else if pos.peek() == Some('>') {
                let (pos, _) = pos.advance();
                Ok((pos, TokenKind::PipeArrow))
            } else {
                Ok((pos, TokenKind::BitOr))
            }
        }
        '^' => {
            let (pos, _) = pos.advance();
            if pos.peek() == Some('=') {
                let (pos, _) = pos.advance();
                Ok((pos, TokenKind::BitXorEq))
            } else {
                Ok((pos, TokenKind::BitXor))
            }
        }
        '~' => {
            let (pos, _) = pos.advance();
            Ok((pos, TokenKind::Tilde))
        }
        '$' => {
            let (pos, _) = pos.advance();
            if pos.peek() == Some('(') {
                let (pos, _) = pos.advance();
                Ok((pos, TokenKind::DollarParen))
            } else {
                Err(unexpected_dollar(line, col))
            }
        }
        '(' => { let (pos, _) = pos.advance(); Ok((pos, TokenKind::LParen)) }
        ')' => { let (pos, _) = pos.advance(); Ok((pos, TokenKind::RParen)) }
        '{' => { let (pos, _) = pos.advance(); Ok((pos, TokenKind::LBrace)) }
        '}' => { let (pos, _) = pos.advance(); Ok((pos, TokenKind::RBrace)) }
        '[' => { let (pos, _) = pos.advance(); Ok((pos, TokenKind::LBracket)) }
        ']' => { let (pos, _) = pos.advance(); Ok((pos, TokenKind::RBracket)) }
        ':' => {
            let (pos, _) = pos.advance();
            if pos.peek() == Some(':') {
                let (pos, _) = pos.advance();
                Ok((pos, TokenKind::ColonColon))
            } else {
                Ok((pos, TokenKind::Colon))
            }
        }
        ';' => { let (pos, _) = pos.advance(); Ok((pos, TokenKind::Semi)) }
        ',' => { let (pos, _) = pos.advance(); Ok((pos, TokenKind::Comma)) }
        '.' => {
            let (pos, _) = pos.advance();
            if pos.peek() == Some('.') && pos.chars.clone().next() == Some('.') {
                let (pos, _) = pos.advance();
                let (pos, _) = pos.advance();
                Ok((pos, TokenKind::Ellipsis))
            } else if pos.peek() == Some('.') {
                let (pos, _) = pos.advance();
                Ok((pos, TokenKind::DotDot))
            } else {
                Ok((pos, TokenKind::Dot))
            }
        }
        '?' => { let (pos, _) = pos.advance(); Ok((pos, TokenKind::Question)) }
        '@' => { let (pos, _) = pos.advance(); Ok((pos, TokenKind::At)) }
        '#' => { let (pos, _) = pos.advance(); Ok((pos, TokenKind::Hash)) }
        '\'' => { let (pos, _) = pos.advance(); Ok((pos, TokenKind::Tick)) }
        _ => Err(unexpected_character(c, line, col)),
    }
}

// ── Finalization helpers ─────────────────────────────────────────────

fn flush_and_done(pos: LexerPos, mode: LexerMode, mut acc: LexerAcc) -> Result<(LexerState, Option<Token>), LexerError> {
    if mode == LexerMode::Sketch {
        while acc.indent_stack.len() > 1 {
            acc.indent_stack.pop();
            let token = Token {
                kind: TokenKind::Dedent,
                line: pos.line,
                col: pos.col,
            };
            acc.tokens.push(token);
        }
    }
    let token = Token {
        kind: TokenKind::Eof,
        line: pos.line,
        col: pos.col,
    };
    acc.tokens.push(token);
    state_done!(acc)
}

// ── Entry point ──────────────────────────────────────────────────────

/// Tokenize source text using the Flow-based lexer state machine.
pub fn flow_tokenize(source: &str, mode: LexerMode) -> Result<Vec<Token>, LexerError> {
    let mut state = LexerState::new(source, mode);
    // First transition: Start → LineStart
    let (new_state, _) = match state.transition(LexerEvent::Step) {
        Ok(result) => result,
        Err(e) => return Err(e),
    };
    state = new_state;

    loop {
        let new_state = match state.transition(LexerEvent::Step) {
            Ok((s, _)) => s,
            Err(e) => return Err(e),
        };
        match new_state {
            LexerState::Done(result) => {
                let acc = result?;
                return Ok(acc.tokens);
            }
            other => {
                state = other;
            }
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
            Lexer::new(source).tokenize()
        } else {
            Lexer::new_sketch(source).tokenize()
        }
    }

    fn compare_token_sets(flow: &[Token], legacy: &[Token], src: &str) {
        if flow.len() != legacy.len() {
            panic!(
                "token count mismatch for {:?}: flow={}, legacy={}\nflow: {:?}\nlegacy: {:?}",
                src, flow.len(), legacy.len(),
                flow.iter().map(|t| &t.kind).collect::<Vec<_>>(),
                legacy.iter().map(|t| &t.kind).collect::<Vec<_>>(),
            );
        }
        for (i, (f, l)) in flow.iter().zip(legacy.iter()).enumerate() {
            assert_eq!(f.kind, l.kind, "token {} kind mismatch for {:?}", i, src);
        }
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

    /// Test Flow lexer against a representative set of real-world files.
    #[test]
    fn test_flow_lexer_real_world_files() {
        let test_sources = [
            // Empty
            "",
            // Trivial programs
            " ",
            "\n",
            "func main() -> i32 { 0 }",
            // Newlines and indentation
            "func f() {\n    1\n}\n",
            // String with escapes
            r#""\n\t\r\\\"""#,
            // F-string with interpolation
            r#"f"hello {name}""#,
            // Blocks
            "func main() {\n    /* comment */\n    42\n}",
            // Operators
            "func main() { a + b - c * d / e % f }",
            // Comparison
            "func main() { a == b && c != d || e <= f }",
            // Bitwise
            "func main() { a & b | c ^ d ~ e }",
            // Shifts
            "func main() { a << b >> c }",
            // Compound assignment
            "func main() { x += 1; y -= 2; z *= 3; w /= 4; x |= y; x &= y; x ^= y; }",
            // Pipe arrow
            "func main() { x |> f |> g }",
            // Colon colon
            "func main() { let x = std::io::print_line; }",
            // Dollar paren
            "func main() { let x = $(command); }",
            // Ellipsis and dotdot
            "func main() { 1..10; 1...10; }",
        ];

        for src in &test_sources {
            let flow = flow_tokenize(src, LexerMode::Production).unwrap();
            let legacy = tokenize_legacy(src, LexerMode::Production).unwrap();
            compare_token_sets(&flow, &legacy, src);
        }
    }

    /// Test error cases match.
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
                (Err(_), Err(_)) => {} // both error — acceptable
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
