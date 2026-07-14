/// Simple AST-based formatter for Mimi source code.
///
/// Handles: indentation normalization (4 spaces), brace style, trailing commas,
/// blank line normalization. Does NOT reorder imports or restructure code.
///
/// A7: Uses `source_scan::SourceScanner` for correct string/comment tracking.
pub struct Formatter {
    indent_size: usize,
}

impl Formatter {
    pub fn new() -> Self {
        Self { indent_size: 4 }
    }

    /// Strip string literal contents from a line so brace counting ignores braces in strings.
    /// A7: delegates to `source_scan::SourceScanner::strip_string_contents`.
    fn strip_strings(line: &str) -> String {
        crate::source_scan::SourceScanner::strip_string_contents(line)
    }

    /// Normalize spacing around operators and punctuation.
    /// Handles: space before `{`, after `,`, around `:`, around `->`.
    ///
    /// A7: Uses `source_scan::SourceScanner` for correct string/comment tracking.
    /// String literals and comments are copied verbatim.
    fn normalize_spacing(line: &str) -> String {
        // Quick check: if no known punctuation needing normalization, skip
        if !line.contains(&['{', ',', ':', '-', '=', '+', '*', '<', '>', '|', '&'][..]) {
            return line.to_string();
        }
        // A7: Use scanner to get per-char regions, so we only normalize code chars.
        let scanner = crate::source_scan::SourceScanner::new(line);
        let scanned = scanner.scan();
        let chars: Vec<char> = scanned.iter().map(|(c, _)| *c).collect();
        let regions: Vec<crate::source_scan::Region> = scanned.iter().map(|(_, r)| *r).collect();
        let mut out = String::with_capacity(line.len() + 8);
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let region = regions[i];

            // Inside string/char/comment: copy verbatim and handle escapes.
            if region != crate::source_scan::Region::Code {
                out.push(c);
                if c == '\\' && i + 1 < chars.len() {
                    out.push(chars[i + 1]);
                    i += 1;
                }
                i += 1;
                continue;
            }

            match c {
                // A7: String/char delimiters are already handled by the region
                // check above (Region::Code for delimiter, StringContent/CharContent
                // for contents). No special handling needed here.
                '{' => {
                    // Ensure space before `{` (unless at start or preceded by space)
                    if i > 0 && chars[i - 1] != ' ' && chars[i - 1] != '(' {
                        out.push(' ');
                    }
                    out.push('{');
                    // Ensure space after `{` (unless at end or followed by space/})
                    if i + 1 < chars.len() && chars[i + 1] != ' ' && chars[i + 1] != '}' {
                        out.push(' ');
                    }
                }
                '}' => {
                    // Normalize `}` to have space before if needed
                    if i > 0 && chars[i - 1] == '{' {
                        // single-line block: already handled
                    }
                    out.push('}');
                }
                ',' => {
                    out.push(',');
                    // Ensure space after `,` (unless at end or already space)
                    if i + 1 < chars.len() && chars[i + 1] != ' ' {
                        out.push(' ');
                    }
                }
                ':' => {
                    // Avoid double colon ::
                    if i + 1 < chars.len() && chars[i + 1] == ':' {
                        out.push(':');
                        out.push(':');
                        i += 1;
                    } else {
                        out.push(':');
                        // Space after `:`  (e.g. `a: i32`, not `a:i32`)
                        if i + 1 < chars.len() && chars[i + 1] != ' ' && chars[i + 1] != ':' {
                            out.push(' ');
                        }
                    }
                }
                '=' => {
                    // Space before `=` (unless already space or after `<>!`)
                    if i == 0
                        || (chars[i - 1] != ' '
                            && !matches!(chars.get(i - 1), Some('<' | '>' | '!' | '=')))
                    {
                        out.push(' ');
                    }
                    out.push('=');
                    // Space after `=`
                    if i + 1 < chars.len() && chars[i + 1] != ' ' {
                        out.push(' ');
                    }
                }
                '-' => {
                    if i + 1 < chars.len() && chars[i + 1] == '>' {
                        out.push(' ');
                        out.push('-');
                        out.push('>');
                        i += 1;
                        if i + 1 < chars.len() && chars[i + 1] != ' ' {
                            out.push(' ');
                        }
                    } else {
                        out.push('-');
                    }
                }
                '/' => {
                    // DAT-C1 (deep audit): don't insert spaces inside // or /* or */
                    // comments — this corrupts the comment syntax.
                    if i + 1 < chars.len() && chars[i + 1] == '/' {
                        // Line comment: copy rest of line verbatim
                        out.push('/');
                        out.push('/');
                        i += 1;
                        while i + 1 < chars.len() {
                            i += 1;
                            out.push(chars[i]);
                        }
                    } else if i + 1 < chars.len() && chars[i + 1] == '*' {
                        // Block comment start: copy verbatim
                        out.push('/');
                        out.push('*');
                        i += 1;
                    } else {
                        // Division operator: normal spacing
                        if i > 0
                            && chars[i - 1] != ' '
                            && !matches!(chars.get(i - 1), Some('(' | '[' | '{'))
                        {
                            out.push(' ');
                        }
                        out.push('/');
                        if i + 1 < chars.len()
                            && chars[i + 1] != ' '
                            && !matches!(chars.get(i + 1), Some(')' | ']' | '}' | ',' | ';'))
                        {
                            out.push(' ');
                        }
                    }
                }
                '+' | '*' | '<' | '>' | '|' | '&' => {
                    // Space before operator (unless at start or preceded by space/punct)
                    if i > 0
                        && chars[i - 1] != ' '
                        && !matches!(chars.get(i - 1), Some('(' | '[' | '{'))
                    {
                        out.push(' ');
                    }
                    out.push(c);
                    // Space after operator
                    if i + 1 < chars.len()
                        && chars[i + 1] != ' '
                        && !matches!(chars.get(i + 1), Some(')' | ']' | '}' | ',' | ';'))
                    {
                        out.push(' ');
                    }
                }
                _ => out.push(c),
            }
            i += 1;
        }
        // Collapse multiple spaces
        let mut result = String::with_capacity(out.len());
        let mut prev_space = false;
        for c in out.chars() {
            if c == ' ' {
                if !prev_space {
                    result.push(c);
                }
                prev_space = true;
            } else {
                result.push(c);
                prev_space = false;
            }
        }
        result.trim().to_string()
    }
    pub fn format(&self, source: &str) -> String {
        let mut output = String::new();
        let mut indent_level: usize = 0;
        let mut prev_blank = false;

        for line in source.lines() {
            let trimmed = Self::normalize_spacing(line.trim());
            let trimmed: &str = &trimmed;

            // Skip empty lines but track them
            if trimmed.is_empty() {
                if !prev_blank {
                    output.push('\n');
                    prev_blank = true;
                }
                continue;
            }
            prev_blank = false;

            // Strip string literals before counting braces
            let stripped = Self::strip_strings(trimmed);

            // Decrease indent before closing braces
            if stripped.starts_with('}') || stripped.starts_with(')') || stripped.starts_with(']') {
                indent_level = indent_level.saturating_sub(1);
            }

            // Write indented line
            let indent_str = " ".repeat(indent_level * self.indent_size);
            output.push_str(&indent_str);
            output.push_str(trimmed);
            output.push('\n');

            // Increase indent after opening braces (on the stripped line)
            if stripped.ends_with('{') || stripped.ends_with('(') || stripped.ends_with('[') {
                indent_level += 1;
            }
            // Handle single-line blocks like `if x { y }` (on the stripped line)
            else if stripped.contains('{') && stripped.contains('}') {
                // No indent change for single-line blocks
            }
        }

        output
    }

    /// Format source in place, returning true if changes were made.
    pub fn format_in_place(&self, source: &mut String) -> bool {
        let formatted = self.format(source);
        if formatted != *source {
            *source = formatted;
            true
        } else {
            false
        }
    }
}

impl Default for Formatter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_simple_function() {
        let fmt = Formatter::new();
        let input = "func main() -> i32 {
println(42)
0
}";
        let expected = "func main() -> i32 {
    println(42)
    0
}
";
        assert_eq!(fmt.format(input), expected);
    }

    #[test]
    fn format_nested_braces() {
        let fmt = Formatter::new();
        let input = "func f() -> i32 {
if true {
println(1)
} else {
println(2)
}
0
}";
        let expected = "func f() -> i32 {
    if true {
        println(1)
    } else {
        println(2)
    }
    0
}
";
        assert_eq!(fmt.format(input), expected);
    }

    #[test]
    fn format_no_change_needed() {
        let fmt = Formatter::new();
        let input = "func main() -> i32 {
    42
}
";
        assert!(!fmt.format_in_place(&mut input.to_string()));
    }

    // A7 regression tests

    #[test]
    fn format_preserves_line_comments() {
        // A7/DAT-C1: `//` comments must not be corrupted to `/ /`
        let fmt = Formatter::new();
        let input = "func main() -> i32 {
    // this is a comment
    42
}
";
        let result = fmt.format(input);
        assert!(result.contains("// this is a comment"));
        assert!(!result.contains("/ /"));
    }

    #[test]
    fn format_preserves_block_comments() {
        // A7: `/* */` block comments must not be corrupted
        let fmt = Formatter::new();
        let input = "func main() -> i32 {
    /* block comment */
    42
}
";
        let result = fmt.format(input);
        assert!(result.contains("/* block comment */"));
    }

    #[test]
    fn format_string_braces_not_counted() {
        // A7: braces inside string literals should not affect indentation
        let fmt = Formatter::new();
        let input = "func f() -> i32 {
    let s = \"{not a block}\"
    42
}
";
        let result = fmt.format(input);
        // The line after the string should still be at indent level 1 (4 spaces)
        assert!(result.contains("    42\n"));
    }

    #[test]
    fn format_escaped_quote_in_string() {
        // A7: escaped quotes inside strings should not terminate the string early
        let fmt = Formatter::new();
        let input = "func f() -> i32 {
    let s = \"he said \\\"hi\\\"\"
    42
}
";
        let result = fmt.format(input);
        // The escaped quotes should be preserved, and the string should not
        // be split across lines
        assert!(result.contains("\\\"hi\\\""));
    }

    #[test]
    fn format_comment_with_braces() {
        // A7: braces in comments should not affect indentation
        let fmt = Formatter::new();
        let input = "func f() -> i32 {
    // comment with { brace
    42
}
";
        let result = fmt.format(input);
        assert!(result.contains("    42\n"));
        assert!(result.contains("// comment with { brace"));
    }
}
