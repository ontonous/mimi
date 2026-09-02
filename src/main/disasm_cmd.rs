//! `mimi disasm` — disassemble a .mimi file to bytecode.

use std::path::Path;
use std::process;

use mimi::diagnostic::format::{colors_enabled, format_diagnostic, strip_ansi};

pub fn disasm_file(path: &Path) -> ! {
    // Full audit 2026-08-05 §13: route through the 100 MiB capped read used
    // by every other command; a raw fs::read_to_string on a multi-GB file
    // would exhaust memory before parsing even starts.
    let source = match mimi::path_safety::read_source_capped(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {}", e);
            process::exit(1);
        }
    };

    let tokens = match mimi::lexer::Lexer::new(&source).tokenize() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: lexer: {}", e);
            process::exit(1);
        }
    };

    let file = match mimi::parser::Parser::new(tokens).parse_file() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: parse: {}", e);
            process::exit(1);
        }
    };

    // Keep disassembly on the same checked-program boundary as the other
    // production CLI entries.  The canonical route below consumes only the
    // resulting MIR; it never asks the bytecode consumer to rediscover types
    // or ownership from this source file.
    let mut merged_file = if !file.imports.is_empty() {
        let base_dir = path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();
        let mut loader = mimi::loader::ModuleLoader::new(base_dir);
        if let Err(error) = loader.load_main_with_file(path, file) {
            eprintln!("error: import loading failed: {}", error);
            process::exit(1);
        }
        match loader.merge_all() {
            Ok(merged) => merged,
            Err(error) => {
                eprintln!("error: import merge failed: {}", error);
                process::exit(1);
            }
        }
    } else {
        file
    };
    mimi::loader::merge_prelude_into(&mut merged_file);

    let checked_program = match mimi::core::check_program(&merged_file) {
        Ok(program) => program,
        Err(diagnostics) => {
            let use_color = colors_enabled();
            let filename = path.display().to_string();
            for diagnostic in &diagnostics {
                let formatted = format_diagnostic(diagnostic, Some(source.as_str()), &filename);
                if use_color {
                    eprint!("{}", formatted);
                } else {
                    eprint!("{}", strip_ansi(&formatted));
                }
            }
            eprintln!("error: type checking failed");
            process::exit(1);
        }
    };

    // S14: disasm is a consumer of the already-closed scalar-collection
    // island.  A recognized island that fails canonical preflight is a hard
    // error; it must not regain the old compiler as an implicit fallback.
    let program =
        match crate::canonical_dispatch::select_default_route(&checked_program, &merged_file) {
            crate::canonical_dispatch::DefaultMirRoute::Canonical(canonical) => {
                match mimi::interp::bytecode::compile_mir_program(&canonical) {
                    Ok(program) => program,
                    Err(errors) => {
                        eprintln!("error: canonical MIR bytecode is not eligible:");
                        for error in errors {
                            eprintln!("  {}", error);
                        }
                        process::exit(1);
                    }
                }
            }
            crate::canonical_dispatch::DefaultMirRoute::Rejected(reason) => {
                eprintln!("error: default Canonical MIR route rejected: {}", reason);
                process::exit(1);
            }
            crate::canonical_dispatch::DefaultMirRoute::Legacy(_reason) => {
                crate::canonical_dispatch::report_legacy_route(_reason);
                let mut compiler = mimi::interp::bytecode::BytecodeCompiler::new();
                compiler.install_checked_program(&checked_program);
                match compiler.compile_file(&merged_file) {
                    Ok(program) => program,
                    Err(e) => {
                        eprintln!("error: compilation failed: {}", e);
                        process::exit(1);
                    }
                }
            }
        };

    print!(
        "{}",
        mimi::interp::bytecode::disasm::disassemble_program(&program)
    );
    process::exit(0);
}
