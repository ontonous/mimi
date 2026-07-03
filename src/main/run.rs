use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::{is_production, is_sketch, resolve_path};
use mimi::diagnostic::format::{colors_enabled, format_diagnostic, strip_ansi};
use mimi::{interp, lexer, loader, parser};

#[allow(clippy::too_many_arguments)]
pub(crate) fn run(
    path: Option<&Path>,
    verify_contracts: bool,
    verify_ffi: bool,
    allocator: &str,
    strict: bool,
    watch: bool,
    profile: bool,
    extra_args: &[String],
) -> Result<i32, String> {
    let path = resolve_path(path)?;
    if profile {
        mimi::runtime::profiler::profiler_init();
    }
    let result = if watch {
        run_watch(
            &path,
            verify_contracts,
            verify_ffi,
            allocator,
            strict,
            extra_args,
        )?;
        0
    } else {
        run_once(
            &path,
            verify_contracts,
            verify_ffi,
            allocator,
            strict,
            extra_args,
        )?
    };
    if profile {
        mimi::runtime::profiler::profiler_report();
    }
    Ok(result)
}

/// Extract the integer exit code from an interpreter return value.
/// Unit returns are mapped to 0.
fn value_to_exit_code(value: &mimi::interp::Value) -> i32 {
    match value {
        mimi::interp::Value::Int(n) => *n as i32,
        mimi::interp::Value::Bool(b) => *b as i32,
        mimi::interp::Value::Unit => 0,
        _ => 0,
    }
}

fn run_once(
    path: &Path,
    verify_contracts: bool,
    verify_ffi: bool,
    allocator: &str,
    strict: bool,
    extra_args: &[String],
) -> Result<i32, String> {
    let source = fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    if is_sketch(path) {
        return Err("cannot run a .mms sketch file directly; promote to .mimi first".into());
    }
    if !is_production(path) {
        return Err(format!(
            "expected .mimi production file, got {}",
            path.display()
        ));
    }
    let tokens = lexer::Lexer::new(&source).tokenize()?;
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
        return Err(format!("{} parse error(s) found", parse_errors.len()));
    }

    // Load imports if any
    let mut merged_file = if !file.imports.is_empty() {
        let base_dir = path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();
        let mut loader = loader::ModuleLoader::new(base_dir);
        loader.load_main(path)?;
        loader.merge_all()?
    } else {
        file
    };

    // Auto-merge standard library prelude (identity, clamp, is_even, etc.)
    loader::merge_prelude_into(&mut merged_file);

    // Map inline rule statements to structured contracts
    mimi::contracts::map_rule_contracts(&mut merged_file);

    let check_result = if strict {
        mimi::core::check_strict(&merged_file)
    } else {
        mimi::core::check(&merged_file)
    };
    if let Err(diagnostics) = check_result {
        eprintln!(
            "{} has {} type error(s):",
            path.display(),
            diagnostics.len()
        );
        let use_color = colors_enabled();
        let src = fs::read_to_string(path).ok();
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
    let mut interp = interp::Interpreter::new(&merged_file);
    interp.verify_contracts = verify_contracts;
    interp.verify_ffi = verify_ffi;
    interp.default_allocator = match allocator {
        "arena" => interp::AllocatorKind::Arena,
        "bump" => interp::AllocatorKind::Bump,
        _ => interp::AllocatorKind::System,
    };
    interp.cli_args = extra_args.to_vec();
    match interp.run() {
        Ok(value) => {
            // P1-19: suppress the `-> ()` noise when the program's
            // return value is Unit (most main() functions). Show the
            // value only when it carries real information.
            if value != mimi::interp::Value::Unit {
                println!("-> {}", value);
            }
            Ok(value_to_exit_code(&value))
        }
        Err(interp_err) => {
            let use_color = colors_enabled();
            let src = fs::read_to_string(path).ok();
            let src_ref = src.as_deref();
            let diagnostic = interp_err.to_diagnostic();
            let formatted = format_diagnostic(&diagnostic, src_ref, &path.display().to_string());
            if use_color {
                eprintln!("{}", formatted);
            } else {
                eprintln!("{}", strip_ansi(&formatted));
            }
            Err("runtime error".into())
        }
    }
}

fn debounce_mtime(path: &Path, last: SystemTime) -> Option<SystemTime> {
    // Wait 150ms then re-check: debounces rapid save events
    std::thread::sleep(Duration::from_millis(150));
    get_mtime(path).ok().filter(|&m| m != last)
}

fn run_watch(
    path: &Path,
    verify_contracts: bool,
    verify_ffi: bool,
    allocator: &str,
    strict: bool,
    extra_args: &[String],
) -> Result<(), String> {
    println!("Watching {} for changes...", path.display());
    let mut last_modified = get_mtime(path)?;
    // Run once first
    if let Err(e) = run_once(
        path,
        verify_contracts,
        verify_ffi,
        allocator,
        strict,
        extra_args,
    ) {
        eprintln!("{}", e);
    }
    loop {
        std::thread::sleep(Duration::from_millis(500));
        match get_mtime(path) {
            Ok(mtime) if mtime != last_modified => {
                // Debounce: wait briefly and re-check for stable mtime
                let stable = debounce_mtime(path, last_modified).unwrap_or(mtime);
                last_modified = stable;
                println!("\n--- file changed, re-running ---");
                print!("\x1B[2J\x1B[H");
                if let Err(e) = run_once(
                    path,
                    verify_contracts,
                    verify_ffi,
                    allocator,
                    strict,
                    extra_args,
                ) {
                    eprintln!("{}", e);
                }
            }
            Err(e) => {
                eprintln!("watch error: {}", e);
            }
            _ => {}
        }
    }
}

fn get_mtime(path: &Path) -> Result<SystemTime, String> {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .map_err(|e| format!("failed to get file modification time: {}", e))
}
