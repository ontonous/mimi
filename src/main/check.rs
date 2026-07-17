use std::path::Path;

use crate::{is_production, is_sketch, resolve_path};
use mimi::diagnostic::format::{colors_enabled, format_diagnostic, strip_ansi};
use mimi::{lexer, parser};

pub(crate) fn check(path: Option<&Path>, strict: bool, verify_rules: bool) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = mimi::path_safety::read_source_capped(&path)?;
    let sketch = is_sketch(&path);
    let tokens = if sketch {
        lexer::Lexer::new_sketch(&source).tokenize()?
    } else {
        lexer::Lexer::new(&source).tokenize()?
    };
    let file = if sketch {
        parser::Parser::new_sketch(tokens).parse_file()?
    } else {
        let (file, parse_errors) = parser::Parser::new(tokens).parse_file_with_recovery();
        if !parse_errors.is_empty() {
            // Round6: never report "checked successfully" after parse errors.
            // Recovery may yield a partial AST; surface parse errors and fail.
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
            return Err(format!("{} parse error(s)", parse_errors.len()));
        }
        file
    };
    if sketch {
        println!("✓ {} parsed successfully (sketch mode)", path.display());
        return Ok(());
    }
    if !is_production(&path) {
        return Err(format!(
            "expected .mimi production file or .mms sketch file, got {}",
            path.display()
        ));
    }

    // Load imports if any (so `use std::json` and friends resolve)
    let mut file = if !file.imports.is_empty() {
        let base_dir = path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();
        let mut loader = mimi::loader::ModuleLoader::new(base_dir);
        loader
            .load_main(&path)
            .map_err(|e| format!("failed to load imports: {}", e))?;
        loader
            .merge_all()
            .map_err(|e| format!("failed to merge imports: {}", e))?
    } else {
        file
    };

    // Auto-merge standard library prelude (identity, clamp, is_even, etc.)
    mimi::loader::merge_prelude_into(&mut file);

    let check_result = if strict {
        mimi::core::check_program_strict(&file).map(|_| ())
    } else {
        mimi::core::check_program(&file).map(|_| ())
    };
    if let Err(diagnostics) = check_result {
        eprintln!(
            "{} has {} type error(s):",
            path.display(),
            diagnostics.len()
        );
        let use_color = colors_enabled();
        let src = mimi::path_safety::read_source_capped(&path).ok();
        let src_ref = src.as_deref();
        for d in &diagnostics {
            let formatted = format_diagnostic(d, src_ref, &path.display().to_string());
            if use_color {
                eprint!("{}", formatted);
            } else {
                eprint!("{}", strip_ansi(&formatted));
            }
        }
        return Err("type checking failed".into());
    }

    // Verify MMS rule attachment consistency
    if verify_rules {
        let rule_errors = mimi::core::verify_rules(&file);
        if !rule_errors.is_empty() {
            eprintln!(
                "✗ {} has {} rule error(s):",
                path.display(),
                rule_errors.len()
            );
            for e in &rule_errors {
                eprintln!("  - {}", e);
            }
            return Err("rule verification failed".into());
        }
        println!("✓ {} rules verified", path.display());
    }

    println!("✓ {} checked successfully", path.display());
    Ok(())
}
