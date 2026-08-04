use super::{Diagnostic, Severity};
use crate::span::Span;

/// ANSI color codes for terminal output.
mod colors {
    pub const RESET: &str = "\x1b[0m";
    pub const RED: &str = "\x1b[31m";
    pub const YELLOW: &str = "\x1b[33m";
    pub const BLUE: &str = "\x1b[34m";
    pub const CYAN: &str = "\x1b[36m";
    pub const BOLD: &str = "\x1b[1m";
}

/// Maximum characters for an inline `src:` snippet — keeps the diagnostic
/// line bounded on pathological lines.
const MAX_SRC_SNIPPET_CHARS: usize = 200;

/// Format a diagnostic as a single dense line (machine-first, 0.34.34+).
///
/// Shape:
/// ```text
/// error[E0208] file.mimi:3:5-14 cannot assign to immutable variable 'x' | src: x = x + 1 | help: use 'let mut'
/// ```
/// One line per diagnostic, fields joined by `" | "`: severity+code, exact
/// location whose column range replaces the old caret underline, message,
/// source line (when available), notes, help. No gutter/arrow/caret
/// decoration — the coordinates carry all positional information with higher
/// density. Colors apply to the severity prefix only when the output is a
/// terminal (see [`colors_enabled`]).
pub fn format_diagnostic(diagnostic: &Diagnostic, source: Option<&str>, filename: &str) -> String {
    let severity_color = match diagnostic.severity {
        Severity::Error => colors::RED,
        Severity::Warning => colors::YELLOW,
        Severity::Note => colors::BLUE,
        Severity::Help => colors::CYAN,
    };

    let mut out = String::new();
    // Prefix: severity + optional code, e.g. `error[E0208]` or plain `error`.
    match diagnostic.code.as_deref() {
        Some(code) => out.push_str(&format!(
            "{}{}{}[{}]{} ",
            colors::BOLD,
            severity_color,
            diagnostic.severity,
            code,
            colors::RESET
        )),
        None => out.push_str(&format!(
            "{}{}{}{} ",
            colors::BOLD,
            severity_color,
            diagnostic.severity,
            colors::RESET
        )),
    }

    // Exact location with column range (the range subsumes the caret).
    if diagnostic.span.start_line > 0 {
        out.push_str(&format!(
            "{}:{}{} ",
            filename,
            diagnostic.span.start_line,
            span_columns(&diagnostic.span)
        ));
    }
    out.push_str(&diagnostic.message);

    // Source-line context: information, not decoration — one trimmed line.
    if diagnostic.span.start_line > 0 {
        if let Some(src) = source {
            if let Some(line_text) = src
                .lines()
                .nth(diagnostic.span.start_line.saturating_sub(1))
            {
                let trimmed = line_text.trim_end();
                if !trimmed.is_empty() {
                    let count = trimmed.chars().count();
                    let snippet: String = trimmed.chars().take(MAX_SRC_SNIPPET_CHARS).collect();
                    let suffix = if count > MAX_SRC_SNIPPET_CHARS {
                        " ..."
                    } else {
                        ""
                    };
                    out.push_str(&format!(" | src: {}{}", snippet, suffix));
                }
            }
        }
    }

    // Notes inline, each with its own coordinates when available.
    for note in &diagnostic.notes {
        if note.span.start_line > 0 {
            out.push_str(&format!(
                " | note: {} @ {}:{}{}",
                note.message,
                filename,
                note.span.start_line,
                span_columns(&note.span)
            ));
        } else {
            out.push_str(&format!(" | note: {}", note.message));
        }
    }

    // Help.
    if let Some(help) = &diagnostic.help {
        out.push_str(&format!(" | help: {}", help));
    }

    out.push('\n');
    out
}

/// Column part of a span location: `:5-14` for a single-line range,
/// `:5-9:2` style (start col to end line:end col) for multi-line spans,
/// bare `:5` when no end is known, empty when columns are absent.
fn span_columns(span: &Span) -> String {
    if span.start_col == 0 {
        return String::new();
    }
    if span.end_line > span.start_line {
        format!(":{}-{}:{}", span.start_col, span.end_line, span.end_col)
    } else if span.end_col > span.start_col {
        format!(":{}-{}", span.start_col, span.end_col)
    } else {
        format!(":{}", span.start_col)
    }
}

/// Format a simple legacy error message (without full span/source info).
pub fn format_simple_error(message: &str) -> String {
    format!("{}error{}: {}", colors::RED, colors::RESET, message)
}

/// Format a parse error with span information.
pub fn format_parse_error(message: &str, span: &Span, filename: &str) -> String {
    let diagnostic = Diagnostic::error(message, *span);
    format_diagnostic(&diagnostic, None, filename)
}

/// Check if the terminal supports ANSI colors.
pub fn colors_enabled() -> bool {
    use std::io::IsTerminal;
    // Check NO_COLOR environment variable (https://no-color.org/)
    if std::env::var("NO_COLOR").is_ok() {
        return false;
    }
    // Check if stderr is a terminal (diagnostics go to stderr, not stdout)
    std::io::stderr().is_terminal()
}

/// Strip ANSI escape codes from a string.
pub fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip until 'm' (end of escape sequence)
            while let Some(&next) = chars.clone().peekable().peek() {
                chars.next();
                if next == 'm' {
                    break;
                }
            }
        } else {
            result.push(c);
        }
    }
    result
}
