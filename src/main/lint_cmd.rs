use std::fs;
use std::path::PathBuf;

use crate::diagnostic::Severity;
use crate::{lexer, lint, parser};

pub(crate) fn lint_files(files: &[PathBuf]) -> Result<(), String> {
    let linter = lint::Linter::new();
    let mut has_warnings = false;

    if files.is_empty() {
        return Err("no files specified".into());
    }

    for path in files {
        let source = fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
        let tokens = lexer::Lexer::new(&source).tokenize()
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
            has_warnings = true;
        }
    }

    if has_warnings {
        std::process::exit(1);
    }
    println!("no issues found");
    Ok(())
}
