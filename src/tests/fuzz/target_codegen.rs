#![allow(unused_doc_comments)]

use super::harness::arb_mimi_program;
use crate::{core, lexer, parser};

/// Fuzz target: compile randomly generated Mimi programs to LLVM IR.
/// No runtime execution; we only verify codegen doesn't panic.
/// Codegen errors (unsupported features, type mismatches) are acceptable;
/// panics are not.
proptest::proptest! {
    #[test]
    fn fuzz_codegen_no_panic(src in arb_mimi_program()) {
        if let Ok(tokens) = lexer::Lexer::new(&src).tokenize() {
            if let Ok(file) = parser::Parser::new(tokens).parse_file() {
                if core::check(&file).is_ok() {
                    let context = inkwell::context::Context::create();
                    let mut codegen = crate::codegen::CodeGenerator::new(&context, "fuzz_test");
                    let _ = codegen.compile_file(&file);
                }
            }
        }
    }
}

/// Codegen edge-case tests (require `cc` for linking).
#[test]
#[ignore = "requires cc linker toolchain"]
fn test_codegen_empty_main() {
    let src = "func main() -> i32 { 0 }";
    let stdout = crate::tests::compile_and_run(src).expect("src/tests/fuzz/target_codegen.rs:30 unwrap failed");
    assert_eq!(stdout.trim(), "");
}

#[test]
#[ignore = "requires cc linker toolchain"]
fn test_codegen_large_return() {
    let src = r#"
        func main() -> i32 {
            let a = 1000000;
            let b = 2000000;
            println(a + b);
            0
        }
    "#;
    let stdout = crate::tests::compile_and_run(src).expect("src/tests/fuzz/target_codegen.rs:44 unwrap failed");
    assert_eq!(stdout.trim(), "3000000");
}

#[test]
#[ignore = "requires cc linker toolchain"]
fn test_codegen_string_manip() {
    let src = r#"
        func main() -> i32 {
            let s = "hello " + "world";
            println(len(s));
            0
        }
    "#;
    let stdout = crate::tests::compile_and_run(src).expect("src/tests/fuzz/target_codegen.rs:58 unwrap failed");
    assert_eq!(stdout.trim(), "11");
}

/// LLVM IR emission tests (no `cc` required).
#[test]
fn test_codegen_ir_emission() {
    let src = "func main() -> i32 { 42 }";
    let tokens = lexer::Lexer::new(src).tokenize().expect("src/tests/fuzz/target_codegen.rs:66 unwrap failed");
    let file = parser::Parser::new(tokens).parse_file().expect("src/tests/fuzz/target_codegen.rs:67 unwrap failed");
    if core::check(&file).is_err() {
        return;
    }
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "ir_test");
    assert!(codegen.compile_file(&file).is_ok());
    let ir_str = codegen.module.print_to_string().to_string();
    assert!(!ir_str.is_empty(), "LLVM IR should not be empty");
    assert!(ir_str.contains("main"), "LLVM IR should contain main function");
}
