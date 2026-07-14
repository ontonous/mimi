//! B2: PositionMap — correct UTF-16 ↔ byte offset conversion for LSP.
//!
//! LSP positions use **UTF-16 code unit** offsets (per the LSP specification).
//! Rust strings are UTF-8.  Mimi's internal spans use 1-indexed line/column
//! where column is a **byte** offset.
//!
//! This module provides bidirectional conversion that handles:
//! - Multi-byte UTF-8 characters (e.g. é = 2 bytes, 1 UTF-16 unit)
//! - Surrogate pairs (e.g. 😀 = 4 bytes, 2 UTF-16 units)
//! - CRLF line endings (counted as 1 character in LSP line numbering)

use std::str::Chars;

/// Pre-computed line offset table for a source document.
///
/// Constructed once per document text change.  All position conversions
/// go through this table, ensuring consistency.
pub struct PositionMap<'a> {
    /// Byte offset of the start of each line.
    line_starts: Vec<usize>,
    source: &'a str,
}

impl<'a> PositionMap<'a> {
    pub fn new(source: &'a str) -> Self {
        let mut line_starts = vec![0usize];
        for (offset, byte) in source.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(offset + 1);
            }
        }
        Self {
            line_starts,
            source,
        }
    }

    /// Number of lines in the document.
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Convert LSP position (0-indexed line, UTF-16 character) to byte offset.
    pub fn lsp_to_byte(&self, line: usize, character: usize) -> usize {
        let line_start = self
            .line_starts
            .get(line)
            .copied()
            .unwrap_or_else(|| self.source.len());

        // Get the line text
        let line_end = self
            .line_starts
            .get(line + 1)
            .copied()
            .unwrap_or_else(|| self.source.len());
        let line_text =
            &self.source[line_start.min(self.source.len())..line_end.min(self.source.len())];

        // Trim trailing newline
        let line_text = line_text.trim_end_matches('\n').trim_end_matches('\r');

        // Convert UTF-16 code unit offset to byte offset within the line,
        // then add the line's byte start offset.
        line_start + utf16_to_byte(line_text, character)
    }

    /// Convert byte offset to LSP position (0-indexed line, UTF-16 character).
    pub fn byte_to_lsp(&self, byte_offset: usize) -> (usize, usize) {
        // Find the line containing this byte offset
        let line = self
            .line_starts
            .partition_point(|&start| start <= byte_offset)
            .saturating_sub(1);

        let line_start = self.line_starts[line];
        let char_offset = byte_offset.saturating_sub(line_start);

        // Get the line text up to char_offset
        let line_end = self
            .line_starts
            .get(line + 1)
            .copied()
            .unwrap_or_else(|| self.source.len());
        let line_text =
            &self.source[line_start.min(self.source.len())..line_end.min(self.source.len())];
        let line_text = line_text.trim_end_matches('\n').trim_end_matches('\r');

        // Convert byte offset within line to UTF-16 code unit offset
        let byte_in_line = char_offset.min(line_text.len());
        let utf16_offset = byte_to_utf16(line_text, byte_in_line);

        (line, utf16_offset)
    }

    /// Convert Mimi span (1-indexed line, 1-indexed byte column) to LSP range.
    pub fn span_to_lsp(
        &self,
        start_line: usize,
        start_col: usize,
        end_line: usize,
        end_col: usize,
    ) -> serde_json::Value {
        // Mimi spans are 1-indexed; LSP is 0-indexed.
        // Mimi columns are byte offsets; LSP characters are UTF-16 units.
        let sl = start_line.saturating_sub(1);
        let el = end_line.saturating_sub(1);

        let sl_start = self.line_starts.get(sl).copied().unwrap_or(0);
        let el_start = self.line_starts.get(el).copied().unwrap_or(0);

        let sl_text = self.line_text(sl);
        let el_text = self.line_text(el);

        let sc = byte_to_utf16(&sl_text, start_col.saturating_sub(1).min(sl_text.len()));
        let ec = byte_to_utf16(&el_text, end_col.saturating_sub(1).min(el_text.len()));

        serde_json::json!({
            "start": { "line": sl, "character": sc },
            "end": { "line": el, "character": ec }
        })
    }

    /// Get the text of a specific line (without newline).
    fn line_text(&self, line: usize) -> String {
        let start = self.line_starts.get(line).copied().unwrap_or(0);
        let end = self
            .line_starts
            .get(line + 1)
            .copied()
            .unwrap_or_else(|| self.source.len());
        let text = &self.source[start.min(self.source.len())..end.min(self.source.len())];
        text.trim_end_matches('\n')
            .trim_end_matches('\r')
            .to_string()
    }
}

/// Convert UTF-16 code unit offset to byte offset within a string.
fn utf16_to_byte(s: &str, utf16_offset: usize) -> usize {
    let mut utf16_count = 0usize;
    let mut byte_offset = 0usize;
    for c in s.chars() {
        if utf16_count >= utf16_offset {
            break;
        }
        let units = c.len_utf16();
        utf16_count += units;
        byte_offset += c.len_utf8();
    }
    byte_offset
}

/// Convert byte offset to UTF-16 code unit offset within a string.
fn byte_to_utf16(s: &str, byte_offset: usize) -> usize {
    let mut utf16_count = 0usize;
    let mut byte_count = 0usize;
    for c in s.chars() {
        if byte_count >= byte_offset {
            break;
        }
        utf16_count += c.len_utf16();
        byte_count += c.len_utf8();
    }
    utf16_count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_text() {
        let map = PositionMap::new("let x = 42\nlet y = 10\n");
        assert_eq!(map.lsp_to_byte(0, 0), 0);
        assert_eq!(map.lsp_to_byte(0, 5), 5);
        assert_eq!(map.lsp_to_byte(1, 0), 11);
        assert_eq!(map.byte_to_lsp(0), (0, 0));
        assert_eq!(map.byte_to_lsp(5), (0, 5));
        assert_eq!(map.byte_to_lsp(11), (1, 0));
    }

    #[test]
    fn multibyte_text() {
        // é = 2 bytes (UTF-8), 1 UTF-16 unit
        // 😀 = 4 bytes (UTF-8), 2 UTF-16 units (surrogate pair)
        let map = PositionMap::new("café = 1\n😀 = 2\n");
        // Position after "café" (4 UTF-16 units, 5 bytes)
        assert_eq!(map.lsp_to_byte(0, 4), 5);
        // Position at start of line 1 (byte offset = "café = 1\n" = 10 bytes)
        assert_eq!(map.lsp_to_byte(1, 0), 10);
        // Position after 😀 (2 UTF-16 units, 4 bytes)
        assert_eq!(map.lsp_to_byte(1, 2), 10 + 4);
    }

    #[test]
    fn crlf_line_endings() {
        let map = PositionMap::new("let x = 1\r\nlet y = 2\r\n");
        assert_eq!(map.lsp_to_byte(1, 0), 11); // \r\n = 2 bytes, line 1 starts at byte 11
    }

    #[test]
    fn byte_to_lsp_multibyte() {
        let map = PositionMap::new("café = 1\n");
        // Byte offset 5 (after "café") → UTF-16 offset 4
        assert_eq!(map.byte_to_lsp(5), (0, 4));
    }

    #[test]
    fn span_to_lsp_ascii() {
        let map = PositionMap::new("let x = 42\n");
        let range = map.span_to_lsp(1, 1, 1, 7);
        assert_eq!(range["start"]["line"], 0);
        assert_eq!(range["start"]["character"], 0);
        assert_eq!(range["end"]["line"], 0);
        assert_eq!(range["end"]["character"], 6);
    }

    #[test]
    fn span_to_lsp_multibyte() {
        let map = PositionMap::new("café = 1\n");
        // Span covering "café" (1-indexed: line 1, col 1 to col 5)
        let range = map.span_to_lsp(1, 1, 1, 5);
        assert_eq!(range["start"]["character"], 0);
        assert_eq!(range["end"]["character"], 4); // 4 UTF-16 units
    }

    #[test]
    fn out_of_bounds_returns_end() {
        let map = PositionMap::new("short\n");
        // Line beyond end → clamp to source end
        let offset = map.lsp_to_byte(10, 0);
        assert_eq!(offset, 6); // end of source
    }

    #[test]
    fn empty_source() {
        let map = PositionMap::new("");
        assert_eq!(map.lsp_to_byte(0, 0), 0);
        assert_eq!(map.byte_to_lsp(0), (0, 0));
    }
}
