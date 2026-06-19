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
    pub const DIM: &str = "\x1b[2m";
}

/// Format a diagnostic for terminal output with colors and source context.
///
/// # Arguments
/// * `diagnostic` - The diagnostic to format
/// * `source` - The source code (optional, for showing code snippets)
/// * `filename` - The filename to show in the header
pub fn format_diagnostic(diagnostic: &Diagnostic, source: Option<&str>, filename: &str) -> String {
    let mut out = String::new();

    // Header: error[E0200]: message
    let severity_color = match diagnostic.severity {
        Severity::Error => colors::RED,
        Severity::Warning => colors::YELLOW,
        Severity::Note => colors::BLUE,
        Severity::Help => colors::CYAN,
    };

    out.push_str(&format!(
        "{}{}{}[{}{}]{} ",
        colors::BOLD,
        severity_color,
        diagnostic.severity,
        colors::BOLD,
        diagnostic.code.as_deref().unwrap_or(""),
        colors::RESET,
    ));
    out.push_str(&format!("{}{}{}\n", colors::BOLD, diagnostic.message, colors::RESET));

    // Location: --> filename:line:col
    if diagnostic.span.start_line > 0 {
        out.push_str(&format!(
            " {}{}-->{} {}:{}:{}\n",
            colors::DIM, colors::BOLD, colors::RESET,
            filename, diagnostic.span.start_line, diagnostic.span.start_col
        ));
    }

    // Source code snippet
    if let Some(src) = source {
        if diagnostic.span.start_line > 0 {
            let lines: Vec<&str> = src.lines().collect();
            let line_idx = diagnostic.span.start_line.saturating_sub(1);
            let gutter_width = format!("{}", diagnostic.span.end_line).len();

            out.push_str(&format!(
                " {}{}|{}\n",
                colors::DIM, colors::BOLD, colors::RESET
            ));

            // Show the error line
            if let Some(line_text) = lines.get(line_idx) {
                // Line number gutter
                out.push_str(&format!(
                    " {}{: >width$}{} | {}\n",
                    colors::DIM, diagnostic.span.start_line, colors::RESET,
                    line_text,
                    width = gutter_width
                ));

                // Underline the span
                let start_col = diagnostic.span.start_col.saturating_sub(1);
                let width = if diagnostic.span.end_line == diagnostic.span.start_line {
                    diagnostic.span.end_col.saturating_sub(diagnostic.span.start_col).max(1)
                } else {
                    line_text.len().saturating_sub(start_col)
                };

                let indicator_color = match diagnostic.severity {
                    Severity::Error => colors::RED,
                    Severity::Warning => colors::YELLOW,
                    _ => colors::CYAN,
                };

                out.push_str(&format!(
                    " {}{: >width$}{} | {}{}{}{}\n",
                    colors::DIM, "", colors::RESET,
                    " ".repeat(start_col),
                    indicator_color,
                    "^".repeat(width),
                    colors::RESET,
                ));
            }
        }
    }

    // Notes
    for note in &diagnostic.notes {
        if note.span.start_line > 0 {
            out.push_str(&format!(
                "  {}{}note{}: {}\n",
                colors::DIM, colors::BOLD, colors::RESET, note.message
            ));
            if let Some(src) = source {
                let lines: Vec<&str> = src.lines().collect();
                let line_idx = note.span.start_line.saturating_sub(1);
                let gutter_width = format!("{}", note.span.end_line).len();
                if let Some(line_text) = lines.get(line_idx) {
                    out.push_str(&format!(
                        " {}{}|{}\n",
                        colors::DIM, colors::BOLD, colors::RESET
                    ));
                    out.push_str(&format!(
                        " {}{: >width$}{} | {}\n",
                        colors::DIM, note.span.start_line, colors::RESET,
                        line_text,
                        width = gutter_width
                    ));
                    let start_col = note.span.start_col.saturating_sub(1);
                    let indicator_width = if note.span.end_line == note.span.start_line {
                        note.span.end_col.saturating_sub(note.span.start_col).max(1)
                    } else {
                        line_text.len().saturating_sub(start_col)
                    };
                    let indicator = "~".repeat(indicator_width.max(1));
                    out.push_str(&format!(
                        " {}{: >width$}{} | {}{}{} {}{}\n",
                        colors::DIM, "", colors::RESET,
                        " ".repeat(start_col),
                        colors::CYAN, indicator,
                        note.message, colors::RESET,
                        width = gutter_width,
                    ));
                }
            }
        } else {
            out.push_str(&format!(
                "  {}{}note{}: {}\n",
                colors::DIM, colors::BOLD, colors::RESET, note.message
            ));
        }
    }

    // Help
    if let Some(help) = &diagnostic.help {
        out.push_str(&format!(
            "  {}{}help{}: {}\n",
            colors::CYAN, colors::BOLD, colors::RESET, help
        ));
    }

    out
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
    // Check if stdout is a terminal (safe, no raw FFI)
    std::io::stdout().is_terminal()
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
