#![allow(unused_doc_comments)]

use super::harness::arb_mimi_program;
use crate::interp::bytecode::{BytecodeCompiler, BytecodeVM};
use crate::{core, interp, lexer, parser};

/// Fuzz target: interpret randomly generated Mimi programs via bytecode VM.
/// We verify the interpreter never panics. Runtime errors (division by zero,
/// out-of-bounds, etc.) are expected but must not cause panics.
proptest::proptest! {
    #[test]
    fn fuzz_interpreter_no_panic(src in arb_mimi_program()) {
        if let Ok(tokens) = lexer::Lexer::new(&src).tokenize() {
            if let Ok(file) = parser::Parser::new(tokens).parse_file() {
                if core::check(&file).is_ok() {
                    let mut compiler = BytecodeCompiler::new();
                    if let Ok(prog) = compiler.compile_file(&file) {
                        let mut vm = BytecodeVM::new(&prog);
                        vm.verify_contracts = false;
                        let _ = vm.run_value();
                    }
                }
            }
        }
    }
}

/// Edge-case interpreter tests.
#[test]
fn test_interp_simple_loop() {
    let src = r#"
        func main() -> i32 {
            let mut i = 0;
            while i < 5 { i = i + 1 }
            i
        }
    "#;
    if let Ok(tokens) = lexer::Lexer::new(src).tokenize() {
        if let Ok(file) = parser::Parser::new(tokens).parse_file() {
            if core::check(&file).is_ok() {
                let mut compiler = BytecodeCompiler::new();
                let prog = compiler.compile_file(&file).expect("compile");
                let mut vm = BytecodeVM::new(&prog);
                let result = vm.run_value().expect("run");
                assert_eq!(result, interp::Value::Int(5));
            }
        }
    }
}

#[test]
fn test_interp_zero_division() {
    let src = r#"
        func main() -> i32 {
            let x = 1 / 0;
            0
        }
    "#;
    let file = parse_src(src);
    let mut compiler = BytecodeCompiler::new();
    if let Ok(prog) = compiler.compile_file(&file) {
        let mut vm = BytecodeVM::new(&prog);
        let _ = vm.run_value();
    }
}

#[test]
fn test_interp_out_of_bounds() {
    let src = r#"
        func main() -> i32 {
            let xs = [1, 2, 3];
            xs[100]
        }
    "#;
    let file = parse_src(src);
    let mut compiler = BytecodeCompiler::new();
    if let Ok(prog) = compiler.compile_file(&file) {
        let mut vm = BytecodeVM::new(&prog);
        let _ = vm.run_value();
    }
}

#[test]
fn test_interp_while_loop() {
    let src = r#"
        func main() -> i32 {
            let mut i = 0;
            while i < 100 { i = i + 1 }
            i
        }
    "#;
    let file = parse_src(src);
    if core::check(&file).is_err() {
        return;
    }
    let mut compiler = BytecodeCompiler::new();
    let prog = compiler.compile_file(&file).expect("compile");
    let mut vm = BytecodeVM::new(&prog);
    let result = vm.run_value().expect("run");
    assert_eq!(result, interp::Value::Int(100));
}

#[test]
fn test_interp_complex_match_edge_cases() {
    let src = r#"
        type Opt { Some(i32) None }
        func unwrap_or_zero(x: Opt) -> i32 {
            match x { Some(v) => v, None => 0 }
        }
        func main() -> i32 { unwrap_or_zero(Some(42)) + unwrap_or_zero(None) }
    "#;
    let file = parse_src(src);
    if core::check(&file).is_err() {
        return;
    }
    let mut compiler = BytecodeCompiler::new();
    let prog = compiler.compile_file(&file).expect("compile");
    let mut vm = BytecodeVM::new(&prog);
    let result = vm.run_value().expect("run");
    assert_eq!(result, interp::Value::Int(42));
}

fn parse_src(src: &str) -> crate::ast::File {
    let tokens = lexer::Lexer::new(src)
        .tokenize()
        .expect("src/tests/fuzz/target_interpreter.rs:108 unwrap failed");
    parser::Parser::new(tokens)
        .parse_file()
        .expect("src/tests/fuzz/target_interpreter.rs:109 unwrap failed")
}
