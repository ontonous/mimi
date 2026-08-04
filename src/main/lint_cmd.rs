use std::path::PathBuf;

use mimi::diagnostic::format::{colors_enabled, format_diagnostic, strip_ansi};
use mimi::diagnostic::Severity;
use mimi::{lexer, lint, loader};

pub(crate) fn lint_files(files: &[PathBuf], fail_on_warnings: bool) -> Result<(), String> {
    let linter = lint::Linter::new();
    let mut has_errors = false;
    let mut has_warnings = false;

    if files.is_empty() {
        return Err("no files specified".into());
    }

    for path in files {
        let source = mimi::path_safety::read_source_capped(path)?;
        let tokens = lexer::Lexer::new(&source)
            .tokenize()
            .map_err(|e| format!("lexer error in {}: {}", path.display(), e))?;
        let (file, parse_errors) =
            loader::parser_for_path(tokens, path)?.parse_file_with_recovery();
        // Full audit 2026-08-05 §13: parse errors used to be dropped here,
        // so lint reported "no issues found" and exited 0 on unparseable
        // input. Fail closed like `mimi check` (src/main/check.rs): surface
        // every parse error and exit non-zero. The recovered AST is partial;
        // linting it as healthy would be a false clean bill.
        if !parse_errors.is_empty() {
            let use_color = colors_enabled();
            let src_ref = Some(source.as_str());
            let filename = &path.display().to_string();
            for e in &parse_errors {
                let formatted = format_diagnostic(&e.to_diagnostic(), src_ref, filename);
                if use_color {
                    eprint!("{}", formatted);
                } else {
                    eprint!("{}", strip_ansi(&formatted));
                }
            }
            has_errors = true;
            continue;
        }
        let result = linter.lint(&file, &source);

        for diag in &result.diagnostics {
            let severity = match diag.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
                Severity::Note => "note",
                Severity::Help => "help",
            };
            eprintln!("{}: [{}] {}", path.display(), severity, diag.message);
            if diag.severity == Severity::Error {
                has_errors = true;
            } else if diag.severity == Severity::Warning {
                has_warnings = true;
            }
        }
    }

    if has_errors || (fail_on_warnings && has_warnings) {
        std::process::exit(1);
    }
    if has_warnings {
        println!("no errors found (warnings present; use --fail-on-warnings to exit non-zero)");
    } else {
        println!("no issues found");
    }
    Ok(())
}
