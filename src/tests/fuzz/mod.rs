//! # Mimi Fuzz Test Suite
//!
//! Fuzzing infrastructure:
//! - **Property-based** fuzzing via `proptest` (random program generation)
//! - **Target-specific** harnesses for parser, typechecker, interpreter, codegen
//! - **Corpus** of seed inputs in `corpus/` directory
//!
//! Run all fuzz targets:   `cargo test fuzz:: -- --nocapture`
//! Run with proptest:      `PROPTEST_CASES=1000 cargo test fuzz_`
//! Run a specific target:  `cargo test fuzz_parser_no_panic`

pub(crate) mod harness;
pub(crate) mod target_parser;
pub(crate) mod target_typechecker;
pub(crate) mod target_interpreter;
pub(crate) mod target_codegen;
pub(crate) mod corpus;

use crate::interp;
use crate::{lexer, parser};

// ==============================
// Exhaustive match checking
// ==============================

#[test]
fn test_exhaustive_wildcard_ok() {
    let src = r#"
func main() -> i32 {
    let x = 5;
    match x {
        1 => 10,
        2 => 20,
        _ => 0,
    }
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn test_exhaustive_bool_complete() {
    let src = r#"
func main() -> i32 {
    let b = true;
    match b { true => 1, false => 0 }
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn test_exhaustive_enum_data_complete() {
    let src = r#"
type Opt { Some(i32) None }
func main() -> i32 {
    let x = Some(42);
    match x {
        Some(v) => v,
        None => 0,
    }
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn test_exhaustive_enum_data_incomplete() {
    let src = r#"
type Opt { Some(i32) None }
func main() -> i32 {
    let x = Some(42);
    match x {
        Some(v) => v,
    }
}
"#;
    let errs = check_source(src).unwrap_err();
    let all_msg: String = errs.iter().map(|d| d.message.clone()).collect::<Vec<_>>().join(" ");
    assert!(
        all_msg.contains("exhaustive"),
        "expected 'exhaustive' error, got: {:?}",
        errs.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_exhaustive_plain_enum_complete() {
    let src = r#"
type Color { Red Green Blue }
func main() -> i32 {
    let c = Red;
    match c { Red => 1, Green => 2, Blue => 3 }
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn test_exhaustive_plain_enum_incomplete() {
    let src = r#"
type Color { Red Green Blue }
func main() -> i32 {
    let c = Red;
    match c { Red => 1, Green => 2 }
}
"#;
    let errs = check_source(src).unwrap_err();
    let all_msg: String = errs.iter().map(|d| d.message.clone()).collect::<Vec<_>>().join(" ");
    assert!(
        all_msg.contains("exhaustive"),
        "expected 'exhaustive' error, got: {:?}",
        errs.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_exhaustive_bool_incomplete() {
    let src = r#"
func main() -> i32 {
    let b = true;
    match b { true => 1 }
}
"#;
    let errs = check_source(src).unwrap_err();
    let all_msg: String = errs.iter().map(|d| d.message.clone()).collect::<Vec<_>>().join(" ");
    assert!(
        all_msg.contains("exhaustive"),
        "expected 'exhaustive' error, got: {:?}",
        errs.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

// ==============================
// Dual-path consistency tests (require cc)
// ==============================

#[test]
#[ignore = "requires cc linker toolchain"]
fn test_dual_path_arithmetic() {
    let stdout = compile_and_run(r#"
        func main() -> i32 { println(42 + 58); 0 }
    "#).expect("src/tests/fuzz/mod.rs:143 unwrap failed");
    assert_eq!(stdout.trim(), "100");
}

#[test]
#[ignore = "requires cc linker toolchain"]
fn test_dual_path_conditional() {
    let stdout = compile_and_run(r#"
        func main() -> i32 {
            let a = 27; let b = 22;
            println(if a > b { a - b } else { b - a }); 0
        }
    "#).expect("src/tests/fuzz/mod.rs:155 unwrap failed");
    assert_eq!(stdout.trim(), "5");
}

#[test]
#[ignore = "requires cc linker toolchain"]
fn test_dual_path_loop_accumulate() {
    let stdout = compile_and_run(r#"
        func main() -> i32 {
            let mut s = 0; let mut i = 0;
            while i <= 5 { s = s + i; i = i + 1 }
            println(s); 0
        }
    "#).expect("src/tests/fuzz/mod.rs:168 unwrap failed");
    assert_eq!(stdout.trim(), "15");
}

#[test]
#[ignore = "requires cc linker toolchain"]
fn test_dual_path_recursive() {
    let stdout = compile_and_run(r#"
        func fib(n: i32) -> i32 {
            if n <= 1 { n } else { fib(n-1) + fib(n-2) }
        }
        func main() -> i32 { println(fib(10)); 0 }
    "#).expect("src/tests/fuzz/mod.rs:180 unwrap failed");
    assert_eq!(stdout.trim(), "55");
}

#[test]
#[ignore = "requires cc linker toolchain"]
fn test_dual_path_match_literal() {
    let stdout = compile_and_run(r#"
        func main() -> i32 {
            let x = 2;
            println(match x { 0 => 100, 1 => 200, 2 => 300, _ => -1 }); 0
        }
    "#).expect("src/tests/fuzz/mod.rs:192 unwrap failed");
    assert_eq!(stdout.trim(), "300");
}

// ==============================
// Capability declaration tests
// ==============================

#[test]
fn test_cap_declaration_ok() {
    let src = r#"
cap FileReadCap;
func main() -> i32 { 42 }
"#;
    assert!(check_source(src).is_ok());
}

// ==============================
// FFI contract verification (no-crash)
// ==============================

#[test]
fn test_ffi_verify_no_crash() {
    let src = r#"
extern "C" {
    func process(x: i32) -> i32;
}
func main() -> i32 { process(5) }
"#;
    let tokens = lexer::Lexer::new(src).tokenize().expect("src/tests/fuzz/mod.rs:221 unwrap failed");
    let file = parser::Parser::new(tokens).parse_file().expect("src/tests/fuzz/mod.rs:222 unwrap failed");
    let mut interp = interp::Interpreter::new(&file);
    interp.verify_ffi = true;
    let _ = interp.run();
}

// Helper wrappers
use crate::tests::{check_source, compile_and_run};
