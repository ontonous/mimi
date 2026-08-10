use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use crate::{is_production, is_sketch, resolve_path};
use mimi::diagnostic::format::{colors_enabled, format_diagnostic, strip_ansi};
use mimi::{lexer, loader};

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

fn run_once(
    path: &Path,
    verify_contracts: bool,
    verify_ffi: bool,
    allocator: &str,
    strict: bool,
    extra_args: &[String],
) -> Result<i32, String> {
    // §13-#67 (audit 2026-08-05, closed 2026-08-07): --allocator was a dead
    // flag on every backend. Fail loud instead of silently accepting a
    // selection no backend implements; the default "system" passes.
    if allocator != "system" {
        return Err(format!(
            "--allocator '{allocator}' is not implemented (arena/bump allocation is not wired into any backend); use 'system' or omit the flag"
        ));
    }
    // CL-H1: size-capped source load (shared with other CLI entry points).
    let source = mimi::path_safety::read_source_capped(path)?;
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
    let (file, parse_errors) = loader::parser_for_path(tokens, path)?.parse_file_with_recovery();
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
        loader.load_main_with_file(path, file)?;
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
            let src = mimi::path_safety::read_source_capped(path).ok();
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

    // Bytecode VM path (sole interpreter since v0.33).
    {
        use mimi::interp::bytecode::{BytecodeCompiler, BytecodeVM};
        let mut compiler = BytecodeCompiler::new();
        // G1: install type information from CheckedProgram for type-directed
        // method resolution and parameter type tracking.
        compiler.install_checked_program(&checked_program);
        let prog = compiler
            .compile_file(&merged_file)
            .map_err(|e| format!("bytecode compile error: {}", e))?;
        let mut vm = BytecodeVM::new(prog.clone()).with_cli_args(extra_args.to_vec());
        // §13-#67: --verify-contracts was silently ignored. Wire it: the CLI
        // flag is opt-in (default false), matching the documented semantics
        // ("Enable runtime contract verification"). The VM's internal default
        // (true) is overridden so the flag actually controls behavior.
        vm.verify_contracts = verify_contracts;
        // §13-#67: --verify-ffi (default true) was silently ignored — the VM
        // hardcodes ffi_runtime.verify_ffi = false until the bytecode engine
        // implements contract-expression eval. Fail loud when the user
        // expects FFI contract checking on a program that declares externs.
        if verify_ffi
            && merged_file
                .items
                .iter()
                .any(|item| matches!(item, mimi::ast::Item::ExternBlock(_)))
        {
            eprintln!(
                "warning: --verify-ffi is not yet supported by the bytecode VM; \
                 FFI contract verification is disabled (pass --skip-verify-ffi to silence)"
            );
        }
        match vm.run() {
            Ok(exit_code) => {
                if exit_code != 0 {
                    println!("-> {}", exit_code);
                }
                Ok(exit_code as i32)
            }
            Err(e) => {
                eprintln!("bytecode runtime error: {}", e);
                Err("runtime error".into())
            }
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
    // CL-H16 (deep audit): the watch loop had no termination condition and
    // could only be killed with SIGKILL. Install a SIGINT (Ctrl-C) handler
    // that flips a flag so the loop exits cleanly and the process can be
    // stopped normally.
    WATCH_RUNNING.store(true, Ordering::SeqCst);
    unsafe {
        // SAFETY: libc::signal 标准调用；watch_sigint_handler 为 extern "C" fn，SIGINT 仅置位原子标志。
        libc::signal(
            libc::SIGINT,
            watch_sigint_handler as *const () as libc::sighandler_t,
        );
    }
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
    while WATCH_RUNNING.load(Ordering::Relaxed) {
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
    Ok(())
}

fn get_mtime(path: &Path) -> Result<SystemTime, String> {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .map_err(|e| format!("failed to get file modification time: {}", e))
}

/// CL-H16: shared flag toggled by the SIGINT handler so `run_watch`'s loop
/// can terminate on Ctrl-C instead of running forever.
static WATCH_RUNNING: AtomicBool = AtomicBool::new(true);

extern "C" fn watch_sigint_handler(_sig: libc::c_int) {
    WATCH_RUNNING.store(false, Ordering::SeqCst);
}
