#![allow(unused_doc_comments)]

use super::harness::arb_mimi_program;
use crate::{core, lexer, parser};

/// Fuzz target: type-check randomly generated Mimi programs.
/// We verify the type checker never panics. Type errors are expected;
/// panics from unwrap() or invariant violations are not.
proptest::proptest! {
    #[test]
    fn fuzz_typechecker_no_panic(src in arb_mimi_program()) {
        if let Ok(tokens) = lexer::Lexer::new(&src).tokenize() {
            if let Ok(file) = parser::Parser::new(tokens).parse_file() {
                let _ = core::check(&file);
            }
        }
    }
}

fn parse_src(src: &str) -> crate::ast::File {
    let tokens = lexer::Lexer::new(src).tokenize().expect("src/tests/fuzz/target_typechecker.rs:21 unwrap failed");
    parser::Parser::new(tokens).parse_file().expect("src/tests/fuzz/target_typechecker.rs:22 unwrap failed")
}

/// Edge-case typechecker regression tests.
#[test]
fn test_checker_empty_func() {
    let src = "func main() -> i32 { 0 }";
    let file = parse_src(src);
    assert!(core::check(&file).is_ok());
}

#[test]
fn test_checker_missing_return() {
    let src = r#"
        func main() -> i32 {
            let x = 5;
        }
    "#;
    let file = parse_src(src);
    let result = core::check(&file);
    assert!(result.is_err(), "expected type error for missing return");
}

#[test]
fn test_checker_type_mismatch() {
    let src = r#"
        func main() -> i32 {
            let x: string = 42;
        }
    "#;
    let file = parse_src(src);
    let result = core::check(&file);
    assert!(result.is_err(), "expected type error for type mismatch");
}

#[test]
fn test_checker_duplicate_func() {
    let src = r#"
        func foo() -> i32 { 1 }
        func foo() -> i32 { 2 }
        func main() -> i32 { 0 }
    "#;
    let file = parse_src(src);
    let result = core::check(&file);
    assert!(result.is_err(), "expected error for duplicate function");
}

#[test]
fn test_checker_recursive_type() {
    let src = r#"
        type A = B;
        type B = A;
        func main() -> i32 { 0 }
    "#;
    let file = parse_src(src);
    let result = core::check(&file);
    let _ = result;
}

#[test]
fn test_checker_import() {
    let src = r#"
        func helper() -> i32 { 42 }
        func main() -> i32 { helper() }
    "#;
    let file = parse_src(src);
    let _ = core::check(&file);
}
