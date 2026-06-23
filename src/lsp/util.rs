use std::hash::{Hash, Hasher};

use crate::ast::{FuncDef, Item};
use crate::lsp::LspServer;

/// Decode percent-encoded URI characters (minimal implementation)
pub(crate) fn percent_decode(s: &str) -> String {
    let mut result = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                result.push(byte as char);
            } else {
                result.push('%');
                result.push_str(&hex);
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Compute a hash of the function body source text for cache invalidation.
pub(crate) fn hash_func_body(text: &str, func: &FuncDef) -> u64 {
    let end_line = find_func_end_line(text, func.pos.0);
    let lines: Vec<&str> = text.lines().collect();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for line in lines.iter().take(end_line.min(lines.len().saturating_sub(1)) + 1).skip(func.pos.0) {
        line.hash(&mut hasher);
    }
    hasher.finish()
}

/// Find the closing brace line for a function starting at `start_line`.
pub(crate) fn find_func_end_line(text: &str, start_line: usize) -> usize {
    let lines: Vec<&str> = text.lines().collect();
    if start_line >= lines.len() {
        return start_line;
    }
    let mut depth = 0;
    let mut started = false;
    for (i, line) in lines.iter().enumerate().skip(start_line) {
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
            return i;
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
        let lines: Vec<&str> = text.lines().collect();
        let current_line = match lines.get(line) {
            Some(l) => l,
            None => return character,
        };
        let before_cursor: String = current_line.chars().take(character).collect();
        before_cursor
            .rfind(|c: char| !c.is_alphanumeric() && c != '_')
            .map(|i| i + 1)
            .unwrap_or(0)
    }

    /// Get the number of characters from the cursor to the end of the word
    pub fn word_end_offset(&self, text: &str, line: usize, character: usize) -> usize {
        let lines: Vec<&str> = text.lines().collect();
        let current_line = match lines.get(line) {
            Some(l) => l,
            None => return 0,
        };
        let after_cursor: String = current_line.chars().skip(character).collect();
        after_cursor
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or_else(|| current_line.len().saturating_sub(character))
    }

    /// Helper: get the word at a given position
    pub fn get_word_at(&self, text: &str, line: usize, character: usize) -> String {
        let lines: Vec<&str> = text.lines().collect();
        let current_line = match lines.get(line) {
            Some(l) => l,
            None => return String::new(),
        };

        let before_cursor: String = current_line.chars().take(character).collect();
        let after_cursor: String = current_line.chars().skip(character).collect();

        let word_start = before_cursor
            .rfind(|c: char| !c.is_alphanumeric() && c != '_')
            .map(|i| i + 1)
            .unwrap_or(0);
        let word_end = after_cursor
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .map(|i| character + i)
            .unwrap_or(current_line.len());

        if word_start >= word_end {
            return String::new();
        }

        current_line[word_start..word_end].to_string()
    }
}
