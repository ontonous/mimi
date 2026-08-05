//! `mimi disasm` — disassemble a .mimi file to bytecode.

use std::path::Path;
use std::process;

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

    let mut compiler = mimi::interp::bytecode::BytecodeCompiler::new();
    let program = match compiler.compile_file(&file) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: compilation failed: {}", e);
            process::exit(1);
        }
    };

    print!(
        "{}",
        mimi::interp::bytecode::disasm::disassemble_program(&program)
    );
    process::exit(0);
}
