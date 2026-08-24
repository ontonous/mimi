/// Simple AST-based formatter for Mimi source code.
///
/// Handles: indentation normalization (4 spaces), brace style, trailing commas,
/// blank line normalization. Does NOT reorder imports or restructure code.
///
/// A7: Uses `source_scan::SourceScanner` for correct string/comment tracking.

/// F-H1: whether a block comment remains open after scanning `line`.
fn block_comment_carries(line: &str, already_open: bool) -> bool {
    let mut open = already_open;
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if !open {
            if chars[i] == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
                open = true;
                i += 2;
                continue;
            }
            // Skip line comments
            if chars[i] == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
                break;
            }
            // Skip strings roughly
            if chars[i] == '"' {
                i += 1;
                while i < chars.len() {
                    if chars[i] == '\\' {
                        i += 2;
                        continue;
                    }
                    if chars[i] == '"' {
                        break;
                    }
                    i += 1;
                }
            }
        } else if chars[i] == '*' && i + 1 < chars.len() && chars[i + 1] == '/' {
            open = false;
            i += 2;
            continue;
        }
        i += 1;
    }
    open
}

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

    /// FMT-OP1 (full audit 2026-08-05 §13.1): multi-character operators that
    /// the lexer scans as single tokens (`src/lexer/scan.rs` `scan_token`).
    /// Spacing normalization must keep these glued: splitting `==` into `= =`
    /// changes the token stream (EqEq → Eq, Eq) and makes the formatted
    /// output unparseable. The lexer is greedy, so preserving the original
    /// adjacency guarantees identical re-tokenization. All entries are 2 chars.
    ///
    /// Note: `<<`/`>>` are deliberately NOT listed — a line-level rewriter
    /// cannot tell shift operators from generic boundaries (`List<List<T>>`,
    /// `Map<K, V>>`), and inserting spaces there corrupts types. They fall
    /// through to the verbatim `<`/`>` arms below, which preserve source
    /// adjacency byte-for-byte (the input parsed once, so it re-lexes
    /// identically).
    fn multi_char_operator(chars: &[char], i: usize) -> Option<&'static str> {
        let next = *chars.get(i + 1)?;
        Some(match (chars[i], next) {
            ('=', '=') => "==", // EqEq
            ('=', '>') => "=>", // FatArrow
            ('!', '=') => "!=", // Ne
            ('<', '=') => "<=", // Le
            ('>', '=') => ">=", // Ge
            ('+', '=') => "+=", // PlusEq
            ('-', '>') => "->", // Arrow
            ('-', '=') => "-=", // MinusEq
            ('*', '*') => "**", // Pow
            ('*', '=') => "*=", // StarEq
            ('/', '=') => "/=", // SlashEq
            ('&', '&') => "&&", // AndAnd
            ('&', '=') => "&=", // BitAndEq
            ('|', '|') => "||", // OrOr
            ('|', '=') => "|=", // BitOrEq
            ('|', '>') => "|>", // PipeArrow
            ('^', '=') => "^=", // BitXorEq
            _ => return None,
        })
    }

    /// Normalize spacing around operators and punctuation.
    /// Handles: space before `{`, after `,`, around `:`, around operators.
    ///
    /// FMT-OP1: lexer multi-char operators (== => != <= >= += -= *= /= && ||
    /// |> -> ** << >> &= |= ^=) are emitted glued with spacing around them.
    ///
    /// A7: Uses `source_scan::SourceScanner` for correct string/comment tracking.
    /// String literals and comments are copied verbatim.
    ///
    /// H-29 (full audit 2026-08-05 §2.9): every emitted char carries a
    /// `collapsible` flag (true only for Code-region chars). The final
    /// space-collapse pass runs over that tagged stream, so consecutive spaces
    /// are collapsed in CODE only. The old code collapsed over the flattened
    /// output, silently rewriting string-literal and comment bodies
    /// (`let s = "a    b"` → `"a b"`), which `mimi fmt` then wrote back to
    /// disk — silent corruption of user source.
    fn normalize_spacing(line: &str) -> String {
        // Quick check: if no known punctuation needing normalization, skip
        if !line.contains(
            &[
                '{', '}', '(', ')', '[', ']', ',', ':', '-', '=', '+', '*', '<', '>', '|', '&',
            ][..],
        ) {
            return line.to_string();
        }
        // A7: Use scanner to get per-char regions, so we only normalize code chars.
        let scanner = crate::source_scan::SourceScanner::new(line);
        let scanned = scanner.scan();
        let chars: Vec<char> = scanned.iter().map(|(c, _)| *c).collect();
        let regions: Vec<crate::source_scan::Region> = scanned.iter().map(|(_, r)| *r).collect();
        // (char, collapsible) — see H-29 note above.
        let mut out: Vec<(char, bool)> = Vec::with_capacity(line.len() + 8);
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let region = regions[i];

            // Inside string/char/comment: copy verbatim and handle escapes.
            // Verbatim chars are NOT collapsible — their spaces are data.
            if region != crate::source_scan::Region::Code {
                out.push((c, false));
                if c == '\\' && i + 1 < chars.len() {
                    out.push((chars[i + 1], false));
                    i += 1;
                }
                i += 1;
                continue;
            }

            // FMT-OP1: keep lexer multi-char operators glued. Must run before
            // the single-char arms below, which would otherwise insert a space
            // inside the operator (`==` → `= =`, `&&` → `& &`, ...).
            // Spacing around the operator mirrors the single-char arm rules.
            if regions.get(i + 1) == Some(&crate::source_scan::Region::Code) {
                if let Some(op) = Self::multi_char_operator(&chars, i) {
                    // Space before (unless at start / already spaced / opening bracket)
                    if i > 0
                        && chars[i - 1] != ' '
                        && !matches!(chars.get(i - 1), Some('(' | '[' | '{'))
                    {
                        out.push((' ', true));
                    }
                    out.extend(op.chars().map(|ch| (ch, true)));
                    // Space after (unless at end / already spaced / closing punct)
                    if let Some(&after) = chars.get(i + 2) {
                        if after != ' ' && !matches!(after, ')' | ']' | '}' | ',' | ';') {
                            out.push((' ', true));
                        }
                    }
                    i += 2;
                    continue;
                }
            }

            match c {
                // A7: String/char delimiters are already handled by the region
                // check above (Region::Code for delimiter, StringContent/CharContent
                // for contents). No special handling needed here.
                '{' => {
                    // Ensure space before `{` (unless at start or preceded by space)
                    if i > 0 && chars[i - 1] != ' ' && chars[i - 1] != '(' {
                        out.push((' ', true));
                    }
                    out.push(('{', true));
                    // Ensure space after `{` (unless at end or followed by space/})
                    if i + 1 < chars.len() && chars[i + 1] != ' ' && chars[i + 1] != '}' {
                        out.push((' ', true));
                    }
                }
                '}' => {
                    // Ensure space before `}` (unless at start or preceded by
                    // space or `{` — `{}` empty block stays tight).
                    if i > 0 && chars[i - 1] != ' ' && chars[i - 1] != '{' {
                        out.push((' ', true));
                    }
                    out.push(('}', true));
                }
                ',' => {
                    // Canonical form: no space BEFORE a comma (`a, b`, not
                    // `a , b`). Pop a preceding collapsible space if present.
                    while matches!(out.last(), Some((' ', true))) {
                        out.pop();
                    }
                    out.push((',', true));
                    // Ensure space after `,` (unless at end or already space)
                    if i + 1 < chars.len() && chars[i + 1] != ' ' {
                        out.push((' ', true));
                    }
                }
                ':' => {
                    // Avoid double colon ::
                    if i + 1 < chars.len() && chars[i + 1] == ':' {
                        out.push((':', true));
                        out.push((':', true));
                        i += 1;
                    } else {
                        out.push((':', true));
                        // Space after `:`  (e.g. `a: i32`, not `a:i32`)
                        if i + 1 < chars.len() && chars[i + 1] != ' ' && chars[i + 1] != ':' {
                            out.push((' ', true));
                        }
                    }
                }
                '=' => {
                    // Space before `=` (unless already space or after `<>!`)
                    if i == 0
                        || (chars[i - 1] != ' '
                            && !matches!(chars.get(i - 1), Some('<' | '>' | '!' | '=')))
                    {
                        out.push((' ', true));
                    }
                    out.push(('=', true));
                    // Space after `=`
                    if i + 1 < chars.len() && chars[i + 1] != ' ' {
                        out.push((' ', true));
                    }
                }
                '(' | '[' => {
                    // 0.39.136 fmt quality: canonical form is tight after an
                    // opening bracket — `f(x)`, `(1, 2)`, `[1, 2]`. Previously
                    // user spacing like `f( x )` survived formatting.
                    out.push((c, true));
                    if i + 1 < chars.len() && chars[i + 1] == ' ' {
                        i += 1; // swallow the space right after ( or [
                    }
                }
                ')' | ']' => {
                    // Canonical form is tight before a closing bracket.
                    while matches!(out.last(), Some((' ', true))) {
                        out.pop();
                    }
                    out.push((c, true));
                }
                '-' => {
                    // FMT-OP1: `->` and `-=` are consumed by the multi-char
                    // pre-match above; a bare `-` is copied verbatim
                    // (existing behavior: subtraction is not re-spaced).
                    out.push(('-', true));
                }
                '/' => {
                    // DAT-C1 (deep audit): don't insert spaces inside // or /* or */
                    // comments — this corrupts the comment syntax.
                    // (Defensive: SourceScanner normally classifies `//` / `/*`
                    // into comment regions before this arm can see them, so the
                    // verbatim region branch above handles the content.)
                    if i + 1 < chars.len() && chars[i + 1] == '/' {
                        // Line comment: copy rest of line verbatim
                        out.push(('/', false));
                        out.push(('/', false));
                        i += 1;
                        while i + 1 < chars.len() {
                            i += 1;
                            out.push((chars[i], false));
                        }
                    } else if i + 1 < chars.len() && chars[i + 1] == '*' {
                        // Block comment start: copy verbatim
                        out.push(('/', false));
                        out.push(('*', false));
                        i += 1;
                    } else {
                        // Division operator: normal spacing
                        if i > 0
                            && chars[i - 1] != ' '
                            && !matches!(chars.get(i - 1), Some('(' | '[' | '{'))
                        {
                            out.push((' ', true));
                        }
                        out.push(('/', true));
                        if i + 1 < chars.len()
                            && chars[i + 1] != ' '
                            && !matches!(chars.get(i + 1), Some(')' | ']' | '}' | ',' | ';'))
                        {
                            out.push((' ', true));
                        }
                    }
                }
                '+' => {
                    // Space before operator (unless at start or preceded by space/punct)
                    if i > 0
                        && chars[i - 1] != ' '
                        && !matches!(chars.get(i - 1), Some('(' | '[' | '{'))
                    {
                        out.push((' ', true));
                    }
                    out.push((c, true));
                    // Space after operator
                    if i + 1 < chars.len()
                        && chars[i + 1] != ' '
                        && !matches!(chars.get(i + 1), Some(')' | ']' | '}' | ',' | ';'))
                    {
                        out.push((' ', true));
                    }
                }
                // 0.39.136 fmt correctness: adjacency-sensitive symbols are
                // copied VERBATIM — a line-level character rewriter cannot
                // disambiguate their dual roles, and inserting spaces corrupts
                // the token stream (`List<string>` → `List < string >`,
                // `|ptr|` closure pipes → `| ptr |`, `&mut` borrow → `& mut`,
                // deref `*p` → `* p`). Preserving source adjacency is always
                // safe: the input parsed once, so its exact byte sequence
                // re-lexes identically. (Tight comparisons stay tight —
                // cosmetic only, still parseable.)
                '<' | '>' | '*' | '&' | '|' => out.push((c, true)),
                _ => out.push((c, true)),
            }
            i += 1;
        }
        // H-29: collapse consecutive spaces in CODE regions only. Non-code
        // spaces (string/char/comment bodies) are preserved verbatim and also
        // break a collapsing run. Inserted whitespace is always code-region.
        let mut result = String::with_capacity(out.len());
        let mut prev_code_space = false;
        for (c, collapsible) in out {
            if c == ' ' {
                if collapsible {
                    if prev_code_space {
                        continue;
                    }
                    prev_code_space = true;
                } else {
                    prev_code_space = false;
                }
            } else {
                prev_code_space = false;
            }
            result.push(c);
        }
        result.trim().to_string()
    }
    pub fn format(&self, source: &str) -> String {
        let mut output = String::new();
        let mut indent_level: usize = 0;
        let mut prev_blank = false;
        // F-H1: track open block comments across lines so `*/` is not
        // re-spaced as multiply/divide on continuation lines.
        let mut in_block_comment = false;

        for line in source.lines() {
            let raw_trim = line.trim();
            let trimmed = if in_block_comment {
                raw_trim.to_string()
            } else {
                Self::normalize_spacing(raw_trim)
            };
            let trimmed: &str = &trimmed;

            in_block_comment = block_comment_carries(trimmed, in_block_comment);

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
    fn format_preserves_multiline_block_comments() {
        // F-H1: cross-line /* ... */ must not be corrupted.
        let fmt = Formatter::new();
        let input = "func main() -> i32 {
    /* line1
       line2 */
    42
}
";
        let result = fmt.format(input);
        assert!(
            result.contains("/*")
                && result.contains("line1")
                && result.contains("line2")
                && result.contains("*/"),
            "multiline block comment corrupted: {}",
            result
        );
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
