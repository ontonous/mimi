use serde_json::Value;

use crate::lsp::LspServer;

impl LspServer {
    /// Compute folding ranges based on brace matching and indentation.
    /// Skips braces inside strings, char literals, and comments.
    ///
    /// X-10 (full audit 2026-08-05 §3.10): A7 — region tracking is delegated
    /// to the shared [`crate::source_scan::SourceScanner`]. The old ad-hoc
    /// `in_string` flag had three defects: (a) escape detection via
    /// `chars[i - 1] != '\\'` misread `"a\\"` (escaped backslash, so the
    /// closing quote DOES close) and desynced the toggle; (b) the flag
    /// persisted across lines, so one desync corrupted folding for the rest
    /// of the file; (c) block comments were not tracked at all, so `/* { */`
    /// pushed phantom braces. The scanner consumes escape pairs structurally,
    /// resets implicitly at its state-machine level, and covers both comment
    /// kinds — only Code-region braces are counted.
    pub fn compute_folding_ranges(&self, text: &str) -> Vec<Value> {
        let mut ranges = Vec::new();
        let mut brace_stack: Vec<usize> = Vec::new();
        let mut line_idx = 0usize;

        for (ch, region) in crate::source_scan::SourceScanner::new(text).scan() {
            if ch == '\n' {
                line_idx += 1;
                continue;
            }
            if region != crate::source_scan::Region::Code {
                continue;
            }
            match ch {
                '{' | '(' | '[' => {
                    brace_stack.push(line_idx);
                }
                '}' | ')' | ']' => {
                    if let Some(start_line) = brace_stack.pop() {
                        if start_line < line_idx {
                            ranges.push(serde_json::json!({
                                "startLine": start_line,
                                "endLine": line_idx
                            }));
                        }
                    }
                }
                _ => {}
            }
        }

        ranges
    }
}
