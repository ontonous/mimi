use std::hash::{Hash, Hasher};

use crate::ast::{FuncDef, Item};
use crate::lsp::LspServer;
use crate::source_scan::{Region, SourceScanner};

/// Decode percent-encoded URI characters.
/// Handles %XX (byte escape) and %uXXXX (Unicode escape).
pub(crate) fn percent_decode(s: &str) -> String {
    let mut result = String::new();
    // CL-H7 (deep audit): consecutive `%XX` byte escapes form a UTF-8 byte
    // sequence and must be decoded together. Accumulate adjacent bytes and
    // flush them as a single UTF-8 string so multi-byte characters decode
    // correctly (e.g. `%C3%A9` → "é").
    let mut pending: Vec<u8> = Vec::new();
    let flush = |pending: &mut Vec<u8>, result: &mut String| {
        if !pending.is_empty() {
            result.push_str(&String::from_utf8_lossy(pending));
            pending.clear();
        }
    };
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            if let Some(&'u') = chars.peek() {
                // Unicode escape: %uXXXX
                flush(&mut pending, &mut result);
                chars.next(); // consume 'u'
                let hex: String = chars.by_ref().take(4).collect();
                if hex.len() == 4 {
                    if let Ok(code) = u32::from_str_radix(&hex, 16) {
                        if let Some(ch) = char::from_u32(code) {
                            result.push(ch);
                        } else {
                            // Invalid Unicode codepoint, keep original
                            result.push_str("%u");
                            result.push_str(&hex);
                        }
                    } else {
                        result.push_str("%u");
                        result.push_str(&hex);
                    }
                } else {
                    // Not enough hex chars, keep as-is
                    result.push_str("%u");
                    result.push_str(&hex);
                }
            } else {
                // Byte escape: %XX — collect the byte into the pending buffer.
                let hex: String = chars.by_ref().take(2).collect();
                if hex.len() == 2 {
                    if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                        pending.push(byte);
                    } else {
                        flush(&mut pending, &mut result);
                        result.push('%');
                        result.push_str(&hex);
                    }
                } else {
                    flush(&mut pending, &mut result);
                    result.push('%');
                    if !hex.is_empty() {
                        result.push_str(&hex);
                    }
                }
            }
        } else {
            flush(&mut pending, &mut result);
            result.push(c);
        }
    }
    flush(&mut pending, &mut result);
    result
}

/// Compute the verification-cache identity for a function.
///
/// Diagnostics store absolute source ranges, so text alone is insufficient:
/// inserting unchanged lines before a function preserves its body text while
/// moving every cached range. Include the parser-provided declaration anchor
/// to invalidate that stale location cache. The URI/SourceKey remains part of
/// the caller's cache key; session-local `SourceId` deliberately is not hashed.
/// func.meta.span lines are 1-indexed (from lexer).
///
/// 0.35.15 (DX backlog #3): the body window comes from the AST span
/// (start_line..=end_line) instead of brace-counting the source text.
pub(crate) fn hash_func_body(text: &str, func: &FuncDef) -> u64 {
    let start_idx = func.meta.span.start_line.saturating_sub(1); // 0-indexed
    let end_idx = func.meta.span.end_line.saturating_sub(1); // 0-indexed
    let lines: Vec<&str> = text.lines().collect();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // Keep a tag in the in-memory identity too, independently of the on-disk
    // cache schema, so future hash-layout changes are explicit.
    "mimi-lsp-verification-anchor-v1".hash(&mut hasher);
    let anchor = func.meta.span;
    anchor.start_line.hash(&mut hasher);
    anchor.start_col.hash(&mut hasher);
    anchor.end_line.hash(&mut hasher);
    anchor.end_col.hash(&mut hasher);
    // end_idx is 0-indexed, so we take (end_idx - start_idx + 1) lines
    let count = (end_idx.saturating_sub(start_idx) + 1).min(lines.len().saturating_sub(start_idx));
    for line in lines.iter().skip(start_idx).take(count) {
        line.hash(&mut hasher);
    }
    hasher.finish()
}

/// Find the function containing the cursor line, searching recursively through modules.
///
/// AU-LSP-4 (full audit 2026-08-05): `cursor_line` is **1-indexed** — the
/// caller (`compute_verification_diagnostics`) converts the 0-indexed LSP
/// cursor to 1-indexed once at the boundary.
///
/// 0.35.15 (DX backlog #3): containment comes from the AST span
/// (start_line..=end_line) instead of brace-counting the source text —
/// spans track nested blocks exactly, so `let s = "}"` and friends can no
/// longer truncate the region.
pub(crate) fn find_enclosing_func_in_items(items: &[Item], cursor_line: usize) -> Option<&FuncDef> {
    for item in items {
        match item {
            Item::Func(f) => {
                let end = f.meta.span.end_line.max(f.meta.span.start_line);
                if cursor_line >= f.meta.span.start_line && cursor_line <= end {
                    return Some(f);
                }
            }
            _ => {}
        }
    }
    None
}

/// Find all whole-word occurrences of `word` in `line`, returning byte offsets.
///
/// AU-LSP-2 (full audit 2026-08-05): `str::find` returns byte offsets, so the
/// scan advances by `word.len()` bytes (never by 1, which can land mid-char
/// and panic when slicing for the next `find`) and probes word boundaries
/// with byte-safe slices. The old code indexed chars by byte offset
/// (`chars().nth(byte_offset)`), which is wrong for multi-byte identifiers.
pub(crate) fn find_word_occurrences(line: &str, word: &str) -> Vec<usize> {
    let mut out = Vec::new();
    if word.is_empty() {
        return out;
    }
    let mut start = 0usize;
    while let Some(pos) = line.get(start..).and_then(|slice| slice.find(word)) {
        let abs = start + pos;
        // `abs` and `abs + word.len()` are guaranteed char boundaries: `find`
        // matched a valid &str at exactly this span.
        let before_ok = line[..abs]
            .chars()
            .next_back()
            .map_or(true, |c| !(c.is_alphanumeric() || c == '_'));
        let after_ok = line[abs + word.len()..]
            .chars()
            .next()
            .map_or(true, |c| !(c.is_alphanumeric() || c == '_'));
        if before_ok && after_ok {
            out.push(abs);
        }
        // Advance by the word's byte length. A boundary-respecting match can
        // never overlap the span just examined (overlap would put a word char
        // adjacent to a candidate, failing the boundary check), so advancing
        // full-length skips nothing valid.
        start = abs + word.len();
    }
    out
}

/// Convert a 0-indexed char column to a byte offset within `line`.
/// Columns beyond the line map to the line length (defensive: the LSP must
/// degrade gracefully on stale positions, not panic).
pub(crate) fn char_col_to_byte(line: &str, char_col: usize) -> usize {
    line.char_indices()
        .nth(char_col)
        .map(|(b, _)| b)
        .unwrap_or(line.len())
}

/// Per-line byte ranges that are NOT code (string contents, char contents,
/// line comments, block comments). Delimiter characters themselves count as
/// code, matching [`SourceScanner`] region semantics. Ranges on each line are
/// in scan order and non-overlapping.
///
/// AU-LSP-1 / AU-LSP-5 (full audit 2026-08-05): rename and brace counting
/// must skip non-code regions. The scan runs over the whole document so
/// regions that span lines (block comments, multi-line strings) are tracked
/// correctly.
pub(crate) fn non_code_byte_ranges(text: &str) -> Vec<Vec<(usize, usize)>> {
    let mut ranges: Vec<Vec<(usize, usize)>> = vec![Vec::new()];
    let mut line = 0usize;
    let mut byte = 0usize;
    let mut open: Option<(usize, usize)> = None;
    for (ch, region) in SourceScanner::new(text).scan() {
        // Line-bomb hardening: stop building per-line vectors after the hard
        // cap. Large documents still receive useful ranges for the prefix and
        // the server is protected from unbounded Vec allocation.
        if line > crate::lsp::MAX_LSP_DOCUMENT_LINES {
            break;
        }
        if ch == '\n' {
            if let Some(span) = open.take() {
                ranges[line].push(span);
            }
            line += 1;
            byte = 0;
            if line <= crate::lsp::MAX_LSP_DOCUMENT_LINES {
                ranges.push(Vec::new());
            }
            continue;
        }
        if region == Region::Code {
            if let Some(span) = open.take() {
                ranges[line].push(span);
            }
        } else {
            let end = byte + ch.len_utf8();
            // take() avoids borrowing `open` across the match (assigning to a
            // scrutinee-borrowed Option in an arm is E0506).
            let next = match open.take() {
                Some((span_start, _)) => (span_start, end),
                None => (byte, end),
            };
            open = Some(next);
        }
        byte += ch.len_utf8();
    }
    if line <= crate::lsp::MAX_LSP_DOCUMENT_LINES {
        if let Some(span) = open {
            ranges[line].push(span);
        }
    }
    ranges
}

/// Whether `byte` (a byte offset within its line) falls inside any non-code
/// range produced by [`non_code_byte_ranges`] for that line.
pub(crate) fn byte_in_non_code(ranges: &[(usize, usize)], byte: usize) -> bool {
    ranges
        .iter()
        .any(|(start, end)| byte >= *start && byte < *end)
}

impl LspServer {
    /// Get the column of the word start at the given position
    pub fn word_start_col(&self, text: &str, line: usize, character: usize) -> usize {
        word_range_at(text, line, character)
            .map(|(s, _)| s)
            .unwrap_or(character)
    }

    /// Get the number of characters from the cursor to the end of the word
    pub fn word_end_offset(&self, text: &str, line: usize, character: usize) -> usize {
        word_range_at(text, line, character)
            .map(|(_, e)| e.saturating_sub(character))
            .unwrap_or(0)
    }

    /// Helper: get the word at a given position
    pub fn get_word_at(&self, text: &str, line: usize, character: usize) -> String {
        word_range_at(text, line, character)
            .map(|(start, end)| {
                text.lines()
                    .nth(line)
                    .map(|l| l[start..end].to_string())
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    }

    /// Helper: get the (start, end) byte indices of the word at a given position
    pub fn get_word_range(
        &self,
        text: &str,
        line: usize,
        character: usize,
    ) -> Option<(usize, usize)> {
        word_range_at(text, line, character)
    }
}

/// Returns (start, end) byte indices for the word at the given position.
/// Returns None if the position is invalid.
///
/// B2: Uses PositionMap for correct UTF-16 → byte conversion.
pub fn word_range_at(text: &str, line: usize, character: usize) -> Option<(usize, usize)> {
    let lines: Vec<&str> = text.lines().collect();
    let current_line = lines.get(line)?;

    // B2: Convert UTF-16 character position to byte offset within the line.
    let byte_char = {
        let map = super::position_map::PositionMap::new(current_line);
        map.lsp_to_byte(0, character)
    };

    // Work in byte space from here
    let before_cursor = &current_line[..byte_char.min(current_line.len())];
    let after_cursor = &current_line[byte_char.min(current_line.len())..];

    // Find word boundaries in byte space
    let word_start_byte = before_cursor
        .rfind(|c: char| !c.is_alphanumeric() && c != '_' && c != '$')
        .map(|i| {
            // `rfind` returns the byte offset of the separator's first byte.
            // For multi-byte separators (e.g. Chinese comma, emoji) i+1 can
            // land inside a UTF-8 code point and cause a slicing panic.
            i + before_cursor[i..]
                .chars()
                .next()
                .map_or(1, |c| c.len_utf8())
        })
        .unwrap_or(0);

    let word_end_byte = after_cursor
        .find(|c: char| !c.is_alphanumeric() && c != '_' && c != '$')
        .map(|i| byte_char + i)
        .unwrap_or(current_line.len());

    if word_start_byte >= word_end_byte {
        return None;
    }
    // Return byte offsets relative to the line (as callers expect)
    Some((word_start_byte, word_end_byte))
}
