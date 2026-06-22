use std::fs;
use std::path::Path;

use crate::diagnostic::format::{colors_enabled, format_diagnostic, strip_ansi};
use crate::{interp, is_production, is_sketch, lexer, loader, parser, resolve_path};

pub(crate) fn run(path: Option<&Path>, verify_contracts: bool, verify_ffi: bool, allocator: &str, strict: bool) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = fs::read_to_string(&path)
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
    crate::contracts::map_rule_contracts(&mut merged_file);

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
