use std::hash::{Hash, Hasher};

use crate::ast::{FuncDef, Item};
use crate::lsp::LspServer;

/// Decode percent-encoded URI characters.
/// Handles %XX (byte escape) and %uXXXX (Unicode escape).
pub(crate) fn percent_decode(s: &str) -> String {
    let mut result = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            if let Some(&'u') = chars.peek() {
                // Unicode escape: %uXXXX
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
                // Byte escape: %XX — decode to raw bytes, then convert to string.
                let hex: String = chars.by_ref().take(2).collect();
                if hex.len() == 2 {
                    if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                        // Use char::from_u32 to properly handle bytes >= 0x80.
                        result.push(char::from_u32(byte as u32).unwrap_or('\u{FFFD}'));
                    } else {
                        result.push('%');
                        result.push_str(&hex);
                    }
                } else {
                    result.push('%');
                    if !hex.is_empty() {
                        result.push_str(&hex);
                    }
                }
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Compute a hash of the function body source text for cache invalidation.
/// func.pos.0 is 1-indexed (from lexer), so we subtract 1 to convert to 0-indexed.
/// find_func_end_line returns 0-indexed line number.
pub(crate) fn hash_func_body(text: &str, func: &FuncDef) -> u64 {
    let start_idx = func.pos.0.saturating_sub(1); // Convert 1-indexed to 0-indexed
    let end_idx = find_func_end_line(text, func.pos.0); // Returns 0-indexed
    let lines: Vec<&str> = text.lines().collect();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // end_idx is 0-indexed, so we take (end_idx - start_idx + 1) lines
    let count = (end_idx.saturating_sub(start_idx) + 1).min(lines.len().saturating_sub(start_idx));
    for line in lines.iter().skip(start_idx).take(count) {
        line.hash(&mut hasher);
    }
    hasher.finish()
}

/// Find the closing brace line for a function starting at `start_line`.
/// `start_line` is 1-indexed (from lexer span), returns 0-indexed line number.
pub(crate) fn find_func_end_line(text: &str, start_line: usize) -> usize {
    let lines: Vec<&str> = text.lines().collect();
    let start_idx = start_line.saturating_sub(1); // Convert 1-indexed to 0-indexed
    if start_idx >= lines.len() {
        return start_line; // Return original 1-indexed value as fallback
    }
    let mut depth = 0;
    let mut started = false;
    for (i, line) in lines.iter().enumerate().skip(start_idx) {
        for ch in line.chars() {
            match ch {
                '{' => {
                    depth += 1;
                    started = true;
                }
                '}' if depth > 0 => {
                    depth -= 1;
                }
                _ => {}
            }
        }
        if started && depth == 0 {
            return i; // Returns 0-indexed
        }
    }
    lines.len().saturating_sub(1)
}

/// Find the function containing the cursor line, searching recursively through modules.
pub(crate) fn find_enclosing_func_in_items<'a>(
    items: &'a [Item],
    text: &str,
    cursor_line: usize,
) -> Option<&'a FuncDef> {
    for item in items {
        match item {
            Item::Func(f) => {
                let end = find_func_end_line(text, f.pos.0);
                if cursor_line >= f.pos.0 && cursor_line <= end {
                    return Some(f);
                }
            }
            Item::Module(m) => {
                if let Some(f) = find_enclosing_func_in_items(&m.items, text, cursor_line) {
                    return Some(f);
                }
            }
            _ => {}
        }
    }
    None
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
pub fn word_range_at(text: &str, line: usize, character: usize) -> Option<(usize, usize)> {
    let lines: Vec<&str> = text.lines().collect();
    let current_line = lines.get(line)?;

    let before_cursor: String = current_line.chars().take(character).collect();
    let after_cursor: String = current_line.chars().skip(character).collect();

    let word_start = before_cursor
        .rfind(|c: char| !c.is_alphanumeric() && c != '_' && c != '$')
        .map(|i| i + 1)
        .unwrap_or(0);
    let word_end = after_cursor
        .find(|c: char| !c.is_alphanumeric() && c != '_' && c != '$')
        .map(|i| character + i)
        .unwrap_or(current_line.len());

    if word_start >= word_end {
        return None;
    }
    Some((word_start, word_end))
}
