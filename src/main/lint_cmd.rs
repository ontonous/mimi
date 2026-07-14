use std::path::PathBuf;

use mimi::diagnostic::Severity;
use mimi::{lexer, lint, parser};

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
        let (file, _parse_errors) = parser::Parser::new(tokens).parse_file_with_recovery();
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
