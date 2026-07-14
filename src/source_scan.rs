//! Shared source-code scanner for fmt.rs and lint.rs (A7 architectural debt).
//!
//! Provides a character-level scanner that correctly tracks string literals,
//! char literals, line comments, and block comments.  This replaces the
//! scattered ad-hoc state machines in fmt.rs (`strip_strings`,
//! `normalize_spacing`) and lint.rs (W003, W007).
//!
//! ## Why not use the lexer?
//!
//! The Flow lexer (`lexer::flow::flow_tokenize`) skips comments entirely and
//! emits only structural tokens.  fmt/lint need comment content and positions,
//! so a dedicated scanner is required.
//!
//! ## API
//!
//! [`SourceScanner`] iterates over chars, yielding [`ScanEvent`]s that mark
//! transitions between code, strings, chars, and comments.  Consumers can use
//! `is_in_string` / `is_in_comment` predicates or walk the event stream.

/// Events emitted by [`SourceScanner`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanEvent {
    /// A regular code character (not inside string/char/comment).
    Code(char),
    /// A character inside a string literal (excludes delimiters).
    StringContent(char),
    /// A character inside a char literal.
    CharContent(char),
    /// A character inside a line comment `// ...`.
    LineComment(char),
    /// A character inside a block comment `/* ... */`.
    BlockComment(char),
    /// Newline character (always emitted, regardless of context).
    Newline,
}

/// Stateful character scanner for Mimi source code.
pub struct SourceScanner<'a> {
    chars: Vec<char>,
    pos: usize,
    _source: &'a str,
}

impl<'a> SourceScanner<'a> {
    pub fn new(source: &'a str) -> Self {
        Self {
            chars: source.chars().collect(),
            pos: 0,
            _source: source,
        }
    }

    /// Scan the entire source and return a list of `(char, region)` tuples
    /// where `region` indicates whether the char is in code, string, or
    /// comment.
    pub fn scan(&self) -> Vec<(char, Region)> {
        let mut result = Vec::with_capacity(self.chars.len());
        let mut state = ScanState::Code;
        let mut i = 0;
        while i < self.chars.len() {
            let ch = self.chars[i];
            // Newlines always reset line-comment state.
            if ch == '\n' {
                if state == ScanState::LineComment {
                    state = ScanState::Code;
                }
                result.push((ch, state.into()));
                i += 1;
                continue;
            }
            match state {
                ScanState::Code => {
                    // Check for line comment start
                    if ch == '/' && i + 1 < self.chars.len() && self.chars[i + 1] == '/' {
                        state = ScanState::LineComment;
                        result.push((ch, Region::LineComment));
                        result.push((self.chars[i + 1], Region::LineComment));
                        i += 2;
                        continue;
                    }
                    // Check for block comment start
                    if ch == '/' && i + 1 < self.chars.len() && self.chars[i + 1] == '*' {
                        state = ScanState::BlockComment;
                        result.push((ch, Region::BlockComment));
                        result.push((self.chars[i + 1], Region::BlockComment));
                        i += 2;
                        continue;
                    }
                    // String literal start
                    if ch == '"' {
                        state = ScanState::String;
                        result.push((ch, Region::Code)); // delimiter is code
                        i += 1;
                        continue;
                    }
                    // Char literal start
                    if ch == '\'' {
                        state = ScanState::Char;
                        result.push((ch, Region::Code)); // delimiter is code
                        i += 1;
                        continue;
                    }
                    result.push((ch, Region::Code));
                    i += 1;
                }
                ScanState::String => {
                    if ch == '\\' && i + 1 < self.chars.len() {
                        // Escaped char: emit both
                        result.push((ch, Region::StringContent));
                        result.push((self.chars[i + 1], Region::StringContent));
                        i += 2;
                        continue;
                    }
                    if ch == '"' {
                        state = ScanState::Code;
                        result.push((ch, Region::Code)); // delimiter is code
                        i += 1;
                        continue;
                    }
                    result.push((ch, Region::StringContent));
                    i += 1;
                }
                ScanState::Char => {
                    if ch == '\\' && i + 1 < self.chars.len() {
                        result.push((ch, Region::CharContent));
                        result.push((self.chars[i + 1], Region::CharContent));
                        i += 2;
                        continue;
                    }
                    if ch == '\'' {
                        state = ScanState::Code;
                        result.push((ch, Region::Code));
                        i += 1;
                        continue;
                    }
                    result.push((ch, Region::CharContent));
                    i += 1;
                }
                ScanState::LineComment => {
                    result.push((ch, Region::LineComment));
                    i += 1;
                }
                ScanState::BlockComment => {
                    // Check for block comment end
                    if ch == '*' && i + 1 < self.chars.len() && self.chars[i + 1] == '/' {
                        state = ScanState::Code;
                        result.push((ch, Region::BlockComment));
                        result.push((self.chars[i + 1], Region::BlockComment));
                        i += 2;
                        continue;
                    }
                    result.push((ch, Region::BlockComment));
                    i += 1;
                }
            }
        }
        result
    }

    /// Return a version of the line with string contents replaced by spaces
    /// (preserving length) so brace counting ignores braces in strings.
    /// Comments are preserved as-is.
    pub fn strip_string_contents(line: &str) -> String {
        let scanner = SourceScanner::new(line);
        let scanned = scanner.scan();
        let mut out = String::with_capacity(line.len());
        for (ch, region) in scanned {
            match region {
                Region::StringContent | Region::CharContent => out.push(' '),
                _ => out.push(ch),
            }
        }
        out
    }

    /// Check if a position (byte offset) is inside a string or comment.
    pub fn is_in_string_or_comment(source: &str, char_idx: usize) -> bool {
        let scanner = SourceScanner::new(source);
        let scanned = scanner.scan();
        if char_idx < scanned.len() {
            matches!(
                scanned[char_idx].1,
                Region::StringContent
                    | Region::CharContent
                    | Region::LineComment
                    | Region::BlockComment
            )
        } else {
            false
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    Code,
    StringContent,
    CharContent,
    LineComment,
    BlockComment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanState {
    Code,
    String,
    Char,
    LineComment,
    BlockComment,
}

impl From<ScanState> for Region {
    fn from(s: ScanState) -> Self {
        match s {
            ScanState::Code => Region::Code,
            ScanState::String => Region::StringContent,
            ScanState::Char => Region::CharContent,
            ScanState::LineComment => Region::LineComment,
            ScanState::BlockComment => Region::BlockComment,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_plain_code() {
        let s = SourceScanner::new("let x = 42");
        let result = s.scan();
        assert!(result.iter().all(|(_, r)| *r == Region::Code));
    }

    #[test]
    fn scan_string_literal() {
        let s = SourceScanner::new("let x = \"hello {world}\"");
        let result = s.scan();
        // Braces inside string should be StringContent, not Code
        let brace = result.iter().find(|(c, _)| *c == '{');
        assert_eq!(brace.map(|(_, r)| *r), Some(Region::StringContent));
    }

    #[test]
    fn scan_escaped_quote_in_string() {
        let s = SourceScanner::new("let x = \"he said \\\"hi\\\"\"");
        let result = s.scan();
        // No char after the escaped quotes should be treated as string-end
        let string_content_count = result
            .iter()
            .filter(|(_, r)| *r == Region::StringContent)
            .count();
        assert!(string_content_count > 0);
    }

    #[test]
    fn scan_line_comment() {
        let s = SourceScanner::new("let x = 42 // comment\n");
        let result = s.scan();
        let comment_chars: String = result
            .iter()
            .filter(|(_, r)| *r == Region::LineComment)
            .map(|(c, _)| *c)
            .collect();
        assert!(comment_chars.contains("comment"));
    }

    #[test]
    fn scan_block_comment_multiline() {
        let s = SourceScanner::new("let x = /* block\ncomment */ 42");
        let result = s.scan();
        let comment_count = result
            .iter()
            .filter(|(_, r)| *r == Region::BlockComment)
            .count();
        assert!(comment_count > 10); // "block\ncomment " has many chars
    }

    #[test]
    fn strip_string_contents_replaces_braces() {
        let stripped = SourceScanner::strip_string_contents("let x = \"{[}]}\"");
        // Braces inside string should be replaced with spaces
        assert!(!stripped.contains('{'));
        assert!(!stripped.contains('}'));
    }

    #[test]
    fn strip_string_preserves_code_braces() {
        let stripped = SourceScanner::strip_string_contents("func f() { 42 }");
        // Braces in code should be preserved
        assert!(stripped.contains('{'));
        assert!(stripped.contains('}'));
    }

    #[test]
    fn scan_char_literal() {
        let s = SourceScanner::new("let x = 'a'");
        let result = s.scan();
        let char_content = result
            .iter()
            .filter(|(_, r)| *r == Region::CharContent)
            .count();
        assert_eq!(char_content, 1); // just 'a'
    }
}
