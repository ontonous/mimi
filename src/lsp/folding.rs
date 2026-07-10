use serde_json::Value;

use crate::lsp::LspServer;

impl LspServer {
    /// Compute folding ranges based on brace matching and indentation.
    /// Skips braces inside strings and line comments.
    pub fn compute_folding_ranges(&self, text: &str) -> Vec<Value> {
        let mut ranges = Vec::new();
        let mut brace_stack: Vec<usize> = Vec::new();
        let mut in_string = false;

        for (line_idx, line) in text.lines().enumerate() {
            let chars: Vec<char> = line.chars().collect();
            let mut i = 0;
            while i < chars.len() {
                let ch = chars[i];
                if ch == '"' && (i == 0 || chars[i - 1] != '\\') {
                    in_string = !in_string;
                }
                if !in_string {
                    // Skip line comments
                    if ch == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
                        break;
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
                i += 1;
            }
        }

        ranges
    }
}
