use std::path::Path;

use crate::{is_sketch, resolve_path};
use mimi::ast::Item;
use mimi::diagnostic::format::{colors_enabled, format_diagnostic, strip_ansi};

use mimi::{lexer, loader};

pub(crate) fn test(
    path: Option<&Path>,
    _allocator: &str,
    filter: Option<&str>,
    verbose: bool,
    strict: bool,
) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = mimi::path_safety::read_source_capped(&path)?;
    if is_sketch(&path) {
        return Err("cannot test a .mms sketch file directly; promote to .mimi first".into());
    }
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = loader::parser_for_path(tokens, &path)?.parse_file()?;

    // Load imports if any
    let mut merged_file = if !file.imports.is_empty() {
        let base_dir = path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();
        let mut loader = loader::ModuleLoader::new(base_dir);
        loader.load_main_with_file(&path, file)?;
        loader.merge_all()?
    } else {
        file
    };

    // Auto-merge standard library prelude (identity, clamp, is_even, etc.)
    loader::merge_prelude_into(&mut merged_file);

    let checked_program = if strict {
        mimi::core::check_program_strict(&merged_file)
    } else {
        mimi::core::check_program(&merged_file)
    };
    let checked_program = match checked_program {
        Ok(program) => program,
        Err(diagnostics) => {
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
    };

    // Find test functions (functions starting with "test_"). 0.35.22 (#1):
    // only ZERO-PARAMETER functions are collected — a helper named
    // `test_color(c: Color)` is not a runnable test; calling it with zero
    // args used to fail with E0800 (arg-count mismatch) and mark the run
    // failed. Skipped helpers are reported once below.
    let mut skipped_helpers: Vec<String> = Vec::new();
    let test_funcs: Vec<String> = merged_file
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Func(f) if f.name.starts_with("test_") => {
                if f.params.is_empty() {
                    Some(f.name.clone())
                } else {
                    skipped_helpers.push(f.name.clone());
                    None
                }
            }
            _ => None,
        })
        .collect();

    // Apply filter if specified
    let test_funcs: Vec<String> = if let Some(pattern) = filter {
        test_funcs
            .into_iter()
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

    if !skipped_helpers.is_empty() {
        println!(
            "Skipped {} test_-prefixed helper(s) with parameters (not runnable as tests): {}",
            skipped_helpers.len(),
            skipped_helpers.join(", ")
        );
    }

    println!("Running {} test(s)...\n", test_funcs.len());

    let mut passed = 0;
    let mut failed = 0;
    let mut errors = Vec::new();
    let use_color = colors_enabled();

    // 0.33 Phase F: use bytecode VM (tree-walker removed).  A complete
    // migrated island is compiled from the one canonical MIR graph; the
    // compatibility compiler remains only for programs that have not yet
    // entered an island.  Once the selector recognizes a candidate, a
    // rejected preflight is a hard error and cannot fall through here.
    use mimi::interp::bytecode::{BytecodeCompiler, BytecodeVM};
    let (prog, canonical_function_names) =
        match crate::canonical_dispatch::select_default_route(&checked_program, &merged_file) {
            crate::canonical_dispatch::DefaultMirRoute::Canonical(canonical) => {
                let prog =
                    mimi::interp::bytecode::compile_mir_program(&canonical).map_err(|errors| {
                        let details = errors
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join("\n");
                        format!("canonical MIR bytecode is not eligible:\n{details}")
                    })?;
                (prog, true)
            }
            crate::canonical_dispatch::DefaultMirRoute::Legacy(_reason) => {
                crate::canonical_dispatch::report_legacy_route(_reason);
                let mut compiler = BytecodeCompiler::new();
                compiler.install_checked_program(&checked_program);
                let prog = compiler
                    .compile_file(&merged_file)
                    .map_err(|e| format!("bytecode compile error: {}", e))?;
                (prog, false)
            }
            crate::canonical_dispatch::DefaultMirRoute::Rejected(reason) => {
                return Err(format!("default Canonical MIR route rejected: {reason}"));
            }
        };

    for func_name in &test_funcs {
        let mut vm = BytecodeVM::new(prog.clone());
        let vm_function_name = if canonical_function_names {
            format!("function:{func_name}")
        } else {
            func_name.clone()
        };
        match vm.call_named(&vm_function_name, vec![]) {
            // TC-H1: bool-returning tests fail when the value is false;
            // non-bool Ok is still a pass (side-effect / unit tests).
            Ok(val) => {
                let fail_bool = matches!(&val, mimi::interp::Value::Bool(false));
                if fail_bool {
                    let msg = format!("{} returned false", func_name);
                    if use_color {
                        println!("  \x1b[31m✗\x1b[0m {}: {}", func_name, msg);
                    } else {
                        println!("  ✗ {}: {}", func_name, msg);
                    }
                    failed += 1;
                    errors.push((func_name.clone(), msg));
                } else {
                    if use_color {
                        println!("  \x1b[32m✓\x1b[0m {}", func_name);
                    } else {
                        println!("  ✓ {}", func_name);
                    }
                    passed += 1;
                }
            }
            Err(e) => {
                if verbose {
                    if use_color {
                        println!("  \x1b[31m✗\x1b[0m {}\n    Error: {}", func_name, e);
                    } else {
                        println!("  ✗ {}\n    Error: {}", func_name, e);
                    }
                } else if use_color {
                    println!("  \x1b[31m✗\x1b[0m {}: {}", func_name, e);
                } else {
                    println!("  ✗ {}: {}", func_name, e);
                }
                failed += 1;
                errors.push((func_name.clone(), e.to_string()));
            }
        }
    }

    if use_color {
        println!(
            "\n\x1b[1m{}\x1b[0m passed, \x1b[1m{}\x1b[0m failed",
            passed, failed
        );
    } else {
        println!("\n{} passed, {} failed", passed, failed);
    }
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
