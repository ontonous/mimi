use super::{Diagnostic, Severity};
use crate::span::{SourceRegistry, Span};
use std::path::Path;

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
    let default_filename = filename.to_string();
    format_diagnostic_with_note_filenames(diagnostic, source, filename, |_| {
        default_filename.clone()
    })
}

/// Format a diagnostic using the source registry carried by a merged file.
///
/// A CLI entry point usually starts with the entry file's source text, but a
/// checker diagnostic may belong to an imported module. Resolve the primary
/// source and every note from the registry so the rendered filename and
/// source snippet preserve cross-file provenance instead of relabeling an
/// imported span as the entry file.
pub fn format_diagnostic_with_registry(
    diagnostic: &Diagnostic,
    registry: &SourceRegistry,
    fallback_source: Option<&str>,
    fallback_filename: &str,
) -> String {
    let primary = source_location(
        registry,
        &diagnostic.span,
        fallback_source,
        fallback_filename,
    );
    format_diagnostic_with_note_filenames(
        diagnostic,
        primary.source.as_deref(),
        &primary.filename,
        |span| source_location(registry, span, None, fallback_filename).filename,
    )
}

struct SourceLocation {
    source: Option<String>,
    filename: String,
}

fn source_location(
    registry: &SourceRegistry,
    span: &Span,
    fallback_source: Option<&str>,
    fallback_filename: &str,
) -> SourceLocation {
    let Some(record) = registry.record(span.source_id) else {
        return SourceLocation {
            source: fallback_source.map(str::to_owned),
            filename: fallback_filename.to_string(),
        };
    };
    let filename = record
        .disk_path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| record.key.as_str().to_string());
    let source = record
        .disk_path
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok());
    let source = source.or_else(|| {
        let is_fallback_source = record
            .disk_path
            .as_deref()
            .is_some_and(|path| source_path_matches(path, fallback_filename))
            || (record.disk_path.is_none() && record.key.as_str() == fallback_filename);
        is_fallback_source
            .then(|| fallback_source.map(str::to_owned))
            .flatten()
    });
    SourceLocation { source, filename }
}

fn source_path_matches(path: &Path, fallback_filename: &str) -> bool {
    let fallback = Path::new(fallback_filename);
    path == fallback
        || match (path.canonicalize(), fallback.canonicalize()) {
            (Ok(path), Ok(fallback)) => path == fallback,
            _ => false,
        }
}

fn format_diagnostic_with_note_filenames(
    diagnostic: &Diagnostic,
    source: Option<&str>,
    filename: &str,
    note_filename: impl Fn(&Span) -> String,
) -> String {
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
        let note_filename = note_filename(&note.span);
        if note.span.start_line > 0 {
            out.push_str(&format!(
                " | note: {} @ {}:{}{}",
                note.message,
                note_filename,
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

#[cfg(test)]
mod tests {
    use super::{format_diagnostic_with_registry, strip_ansi};
    use crate::diagnostic::Diagnostic;
    use crate::span::{SourceKey, SourceRecord, SourceRegistry, SourceTextOrigin, Span};
    use std::fs;

    #[test]
    fn registry_formatter_routes_primary_and_note_sources() {
        let root = std::env::temp_dir().join(format!(
            "mimi_diagnostic_format_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("create formatter test directory");
        let left_path = root.join("left.mimi");
        let right_path = root.join("right.mimi");
        fs::write(&left_path, "extern \"C\" { func clash(x: i64) -> i64; }\n")
            .expect("write left source");
        fs::write(&right_path, "extern \"C\" { func clash(x: i64) -> i64; }\n")
            .expect("write right source");

        let mut registry = SourceRegistry::default();
        let left = registry
            .register(
                SourceRecord::new(
                    SourceKey::new("workspace:left.mimi").expect("left key"),
                    SourceTextOrigin::Disk,
                )
                .with_disk_path(left_path.clone()),
            )
            .expect("register left source");
        let right = registry
            .register(
                SourceRecord::new(
                    SourceKey::new("workspace:right.mimi").expect("right key"),
                    SourceTextOrigin::Disk,
                )
                .with_disk_path(right_path.clone()),
            )
            .expect("register right source");

        let diagnostic = Diagnostic::error_code(
            "E0402",
            "duplicate extern function 'clash'",
            Span::single(1, 22).with_source(right),
        )
        .with_note(
            "previous extern declaration is here",
            Span::single(1, 22).with_source(left),
        );
        let rendered = strip_ansi(&format_diagnostic_with_registry(
            &diagnostic,
            &registry,
            Some("entry fallback"),
            "main.mimi",
        ));

        assert!(rendered.contains(&format!("{}:1:22", right_path.display())));
        assert!(rendered.contains("src: extern \"C\" { func clash(x: i64) -> i64; }"));
        assert!(rendered.contains(&format!(
            "previous extern declaration is here @ {}:1:22",
            left_path.display()
        )));
        assert!(!rendered.contains("main.mimi"));

        fs::remove_dir_all(root).expect("remove formatter test directory");
    }

    #[test]
    fn registry_formatter_uses_fallback_for_unknown_source() {
        let diagnostic = Diagnostic::error("unknown source", Span::single(2, 4));
        let rendered = strip_ansi(&format_diagnostic_with_registry(
            &diagnostic,
            &SourceRegistry::default(),
            Some("first\nsecond source line"),
            "entry.mimi",
        ));

        assert!(rendered.contains("entry.mimi:2:4 unknown source"));
        assert!(rendered.contains("src: second source line"));
    }

    #[test]
    fn registry_formatter_uses_matching_fallback_when_disk_source_is_unreadable() {
        let root = std::env::temp_dir().join(format!(
            "mimi_diagnostic_format_fallback_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("create fallback formatter directory");
        let entry_path = root.join("entry.mimi");
        fs::write(&entry_path, "temporary source\n").expect("write temporary entry source");

        let mut registry = SourceRegistry::default();
        let entry = registry
            .register(
                SourceRecord::new(
                    SourceKey::new("workspace:entry.mimi").expect("entry key"),
                    SourceTextOrigin::Disk,
                )
                .with_disk_path(entry_path.clone()),
            )
            .expect("register entry source");
        fs::remove_file(&entry_path).expect("remove entry source before formatting");

        let diagnostic = Diagnostic::error(
            "entry source unavailable",
            Span::single(1, 1).with_source(entry),
        );
        let rendered = strip_ansi(&format_diagnostic_with_registry(
            &diagnostic,
            &registry,
            Some("fallback source line"),
            &entry_path.display().to_string(),
        ));

        assert!(rendered.contains("src: fallback source line"));
        fs::remove_dir_all(root).expect("remove fallback formatter directory");
    }
}
