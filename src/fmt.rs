/// Simple AST-based formatter for Mimi source code.
///
/// Handles: indentation normalization (4 spaces), brace style, trailing commas,
/// blank line normalization. Does NOT reorder imports or restructure code.
pub struct Formatter {
    indent_size: usize,
}

impl Formatter {
    pub fn new() -> Self {
        Self { indent_size: 4 }
    }

    /// Strip string literal contents from a line so brace counting ignores braces in strings.
    fn strip_strings(line: &str) -> String {
        let mut result = String::with_capacity(line.len());
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '"' {
                result.push(c);
                // Skip until closing quote
                while let Some(&next) = chars.peek() {
                    result.push(next);
                    chars.next();
                    if next == '\\' {
                        // Escape sequence: consume the next char too
                        if let Some(escaped) = chars.next() {
                            result.push(escaped);
                        }
                    } else if next == '"' {
                        break;
                    }
                }
            } else if c == '\'' {
                result.push(c);
                // Skip single-quoted string (character literal)
                while let Some(&next) = chars.peek() {
                    result.push(next);
                    chars.next();
                    if next == '\\' {
                        if let Some(escaped) = chars.next() {
                            result.push(escaped);
                        }
                    } else if next == '\'' {
                        break;
                    }
                }
            } else {
                result.push(c);
            }
        }
        result
    }

    /// Normalize spacing around operators and punctuation.
    /// Handles: space before `{`, after `,`, around `:`, around `->`.
    fn normalize_spacing(line: &str) -> String {
        // Quick check: if no known punctuation needing normalization, skip
        if !line.contains(&['{', ',', ':', '-', '=', '+', '*', '<', '>', '|', '&'][..]) {
            return line.to_string();
        }
        let mut out = String::with_capacity(line.len() + 8);
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            match c {
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
                '+' | '*' | '/' | '<' | '>' | '|' | '&' => {
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
}
