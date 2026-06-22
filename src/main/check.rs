use std::fs;
use std::path::Path;

use crate::contracts;
use crate::diagnostic::format::{colors_enabled, format_diagnostic, strip_ansi};
use crate::{extract_all_contracts, is_production, is_sketch, lexer, parser, resolve_path};

pub(crate) fn check(path: Option<&Path>, extract_contracts: bool, strict: bool, verify_rules: bool) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = fs::read_to_string(&path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    let sketch = is_sketch(&path);
    let tokens = if sketch {
        lexer::Lexer::new_sketch(&source).tokenize()?
    } else {
        lexer::Lexer::new(&source).tokenize()?
    };
    let mut file = if sketch {
        parser::Parser::new_sketch(tokens).parse_file()?
    } else {
        let (file, parse_errors) = parser::Parser::new(tokens).parse_file_with_recovery();
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
            if parse_errors.iter().all(|e| e.span.as_ref().is_none_or(|s| s.start_line > 0)) {
                // All errors have valid positions, continue to type checking
            } else {
                return Err(format!("{} parse error(s)", parse_errors.len()));
            }
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

    // Extract contracts from mms blocks if requested
    if extract_contracts {
        let contracts = extract_all_contracts(&file);
        if contracts.is_empty() {
            println!("No contracts found in mms blocks.");
        } else {
            println!("Contracts extracted from mms blocks:");
            for (func_name, contract) in &contracts {
                println!("  {}:", func_name);
                for req in &contract.requires {
                    println!("    requires: {}", req);
                }
                for ens in &contract.ensures {
                    println!("    ensures: {}", ens);
                }
                for m in &contract.math {
                    println!("    math: {}", m);
                }
            }
        }
        // Bind contracts to functions
        contracts::bind_contracts(&mut file, contracts);
    }

    // Map inline rule statements to structured contracts (independent of mms extraction)
    contracts::map_rule_contracts(&mut file);

    let check_result = if strict {
        crate::core::check_strict(&file)
    } else {
        crate::core::check(&file)
    };
    if let Err(diagnostics) = check_result {
        eprintln!("{} has {} type error(s):", path.display(), diagnostics.len());
        let use_color = colors_enabled();
        let src = fs::read_to_string(&path).ok();
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
        let rule_errors = crate::core::verify_rules(&file);
        if !rule_errors.is_empty() {
            eprintln!("✗ {} has {} rule error(s):", path.display(), rule_errors.len());
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
