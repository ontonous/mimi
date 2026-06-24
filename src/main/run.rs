use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use mimi::diagnostic::format::{colors_enabled, format_diagnostic, strip_ansi};
use mimi::{interp, lexer, loader, parser};
use crate::{is_production, is_sketch, resolve_path};

pub(crate) fn run(path: Option<&Path>, verify_contracts: bool, verify_ffi: bool, allocator: &str, strict: bool, watch: bool) -> Result<(), String> {
    let path = resolve_path(path)?;
    if watch {
        run_watch(&path, verify_contracts, verify_ffi, allocator, strict)
    } else {
        run_once(&path, verify_contracts, verify_ffi, allocator, strict)
    }
}

fn run_once(path: &Path, verify_contracts: bool, verify_ffi: bool, allocator: &str, strict: bool) -> Result<(), String> {
    let source = fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    if is_sketch(&path) {
        return Err("cannot run a .mms sketch file directly; promote to .mimi first".into());
    }
    if !is_production(&path) {
        return Err(format!(
            "expected .mimi production file, got {}",
            path.display()
        ));
    }
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = parser::Parser::new(tokens).parse_file()?;

    // Load imports if any
    let mut merged_file = if !file.imports.is_empty() {
        let base_dir = path.parent().unwrap_or_else(|| std::path::Path::new(".")).to_path_buf();
        let mut loader = loader::ModuleLoader::new(base_dir);
        loader.load_main(&path)?;
        loader.merge_all()?
    } else {
        file
    };

    // Map inline rule statements to structured contracts
    mimi::contracts::map_rule_contracts(&mut merged_file);

    let check_result = if strict { mimi::core::check_strict(&merged_file) } else { mimi::core::check(&merged_file) };
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
    let mut interp = interp::Interpreter::new(&merged_file);
    interp.verify_contracts = verify_contracts;
    interp.verify_ffi = verify_ffi;
    interp.default_allocator = match allocator {
        "arena" => interp::AllocatorKind::Arena,
        "bump" => interp::AllocatorKind::Bump,
        _ => interp::AllocatorKind::System,
    };
    match interp.run() {
        Ok(value) => {
            println!("-> {}", value);
            Ok(())
        }
        Err(interp_err) => {
            let use_color = colors_enabled();
            let src = fs::read_to_string(&path).ok();
            let src_ref = src.as_deref();
            let diagnostic = interp_err.to_diagnostic();
            let formatted = format_diagnostic(&diagnostic, src_ref, &path.display().to_string());
            if use_color {
                eprintln!("{}", formatted);
            } else {
                eprintln!("{}", strip_ansi(&formatted));
            }
            std::process::exit(1);
        }
    }
}

fn run_watch(path: &Path, verify_contracts: bool, verify_ffi: bool, allocator: &str, strict: bool) -> Result<(), String> {
    println!("Watching {} for changes...", path.display());
    let mut last_modified = get_mtime(path)?;
    // Run once first
    let _ = run_once(path, verify_contracts, verify_ffi, allocator, strict);
    loop {
        std::thread::sleep(Duration::from_millis(500));
        match get_mtime(path) {
            Ok(mtime) if mtime != last_modified => {
                last_modified = mtime;
                println!("\n--- file changed, re-running ---");
                print!("\x1B[2J\x1B[H");
                let _ = run_once(path, verify_contracts, verify_ffi, allocator, strict);
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
