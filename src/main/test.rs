use std::fs;
use std::path::Path;

use crate::ast::Item;
use crate::diagnostic::format::{colors_enabled, format_diagnostic, strip_ansi};
use crate::{interp, is_sketch, lexer, loader, parser, resolve_path};

pub(crate) fn test(path: Option<&Path>, allocator: &str, filter: Option<&str>, verbose: bool, strict: bool) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = fs::read_to_string(&path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    if is_sketch(&path) {
        return Err("cannot test a .mms sketch file directly; promote to .mimi first".into());
    }
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = parser::Parser::new(tokens).parse_file()?;

    // Load imports if any
    let merged_file = if !file.imports.is_empty() {
        let base_dir = path.parent().unwrap_or_else(|| std::path::Path::new(".")).to_path_buf();
        let mut loader = loader::ModuleLoader::new(base_dir);
        loader.load_main(&path)?;
        loader.merge_all()?
    } else {
        file
    };

    let check_result = if strict { crate::core::check_strict(&merged_file) } else { crate::core::check(&merged_file) };
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

    // Find test functions (functions starting with "test_")
    let test_funcs: Vec<String> = merged_file.items.iter().filter_map(|item| {
        match item {
            Item::Func(f) if f.name.starts_with("test_") && (!strict || f.commitment.is_locked()) => Some(f.name.clone()),
            _ => None,
        }
    }).collect();

    // Apply filter if specified
    let test_funcs: Vec<String> = if let Some(pattern) = filter {
        test_funcs.into_iter()
            .filter(|name| name.contains(pattern))
            .collect()
    } else {
        test_funcs
    };

    if test_funcs.is_empty() {
    if let Some(pattern) = filter {
        println!("No test functions found matching '{}'.", pattern);
    } else {
        println!("No test functions found.");
    }
        return Ok(());
    }

    println!("Running {} test(s)...\n", test_funcs.len());

    let mut passed = 0;
    let mut failed = 0;
    let mut errors = Vec::new();

    for func_name in &test_funcs {
        let mut interp = interp::Interpreter::new(&merged_file);
        interp.default_allocator = match allocator {
            "arena" => interp::AllocatorKind::Arena,
            "bump" => interp::AllocatorKind::Bump,
            _ => interp::AllocatorKind::System,
        };
        match interp.call_named(func_name, vec![]) {
            Ok(_) => {
                println!("  \x1b[32m✓\x1b[0m {}", func_name);
                passed += 1;
            }
            Err(e) => {
                if verbose {
                    println!("  \x1b[31m✗\x1b[0m {}\n    Error: {}", func_name, e);
                } else {
                    println!("  \x1b[31m✗\x1b[0m {}: {}", func_name, e);
                }
                failed += 1;
                errors.push((func_name.clone(), e));
            }
        }
    }

    println!("\n\x1b[1m{}\x1b[0m passed, \x1b[1m{}\x1b[0m failed", passed, failed);
    if failed > 0 {
        if verbose {
            println!("\nFailed tests:");
            for (name, err) in &errors {
                println!("  {}: {}", name, err);
            }
        }
        Err(format!("{} test(s) failed", failed))
    } else {
        Ok(())
    }
}
