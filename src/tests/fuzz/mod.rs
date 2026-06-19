//! # Mimi Fuzz Test Suite
//!
//! Rust-level fuzzing: 验证解释器与编译器的行为一致性，
//! 以及穷尽性匹配检查等完备性验证。

use crate::interp;
use crate::{core, lexer, parser};

// ==============================
// 穷尽性检查测试
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
// 双路径一致性测试 (需要 cc)
// ==============================

#[test]
#[ignore = "requires cc linker toolchain"]
fn test_dual_path_arithmetic() {
    let stdout = compile_and_run(r#"
        func main() -> i32 { println(42 + 58); 0 }
    "#).unwrap();
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
    "#).unwrap();
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
    "#).unwrap();
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
    "#).unwrap();
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
    "#).unwrap();
    assert_eq!(stdout.trim(), "300");
}

// ==============================
// 线性能力检查测试
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
// FFI 合约验证测试 (不崩溃即可)
// ==============================

#[test]
fn test_ffi_verify_no_crash() {
    let src = r#"
extern "C" {
    func process(x: i32) -> i32;
}
func main() -> i32 { process(5) }
"#;
    let tokens = lexer::Lexer::new(src).tokenize().unwrap();
    let file = parser::Parser::new(tokens).parse_file().unwrap();
    let mut interp = interp::Interpreter::new(&file);
    interp.verify_ffi = true;
    let _ = interp.run();
}

// Helper wrappers
use crate::tests::{check_source, compile_and_run};

// ==============================
// Fuzz: Edge case inputs
// ==============================

#[test]
fn test_fuzz_empty_string() {
    let val = super::run_source(r#"
        func main() -> i32 { len("") }
    "#);
    assert_eq!(val, interp::Value::Int(0));
}

#[test]
fn test_fuzz_large_integer() {
    let val = super::run_source(r#"
        func main() -> i64 { 9223372036854775807 }
    "#);
    assert_eq!(val, interp::Value::Int(9223372036854775807));
}

#[test]
fn test_fuzz_negative_zero() {
    let val = super::run_source(r#"
        func main() -> i32 { -0 }
    "#);
    assert_eq!(val, interp::Value::Int(0));
}

#[test]
fn test_fuzz_deeply_nested_if() {
    let val = super::run_source(r#"
        func main() -> i32 {
            let x = 5
            if x > 0 {
                if x > 2 {
                    if x > 4 {
                        100
                    } else {
                        50
                    }
                } else {
                    25
                }
            } else {
                0
            }
        }
    "#);
    assert_eq!(val, interp::Value::Int(100));
}

#[test]
fn test_fuzz_empty_list() {
    let val = super::run_source(r#"
        func main() -> i32 {
            let xs: List<i32> = []
            len(xs)
        }
    "#);
    assert_eq!(val, interp::Value::Int(0));
}

#[test]
fn test_fuzz_single_element_list() {
    let val = super::run_source(r#"
        func main() -> i32 {
            let xs = [42]
            xs[0]
        }
    "#);
    assert_eq!(val, interp::Value::Int(42));
}

#[test]
fn test_fuzz_nested_lists() {
    let val = super::run_source(r#"
        func main() -> i32 {
            let xs = [[1, 2], [3, 4]]
            xs[1][0]
        }
    "#);
    assert_eq!(val, interp::Value::Int(3));
}

#[test]
fn test_fuzz_string_concat_empty() {
    let val = super::run_source(r#"
        func main() -> string { "" + "" }
    "#);
    assert_eq!(val, interp::Value::String("".to_string()));
}

#[test]
fn test_fuzz_bool_conversion() {
    let val = super::run_source(r#"
        func main() -> i32 {
            let t = true
            let f = false
            let a = if t { 1 } else { 0 }
            let b = if f { 2 } else { 0 }
            a + b
        }
    "#);
    assert_eq!(val, interp::Value::Int(1));
}

#[test]
fn test_fuzz_while_zero_iterations() {
    let val = super::run_source(r#"
        func main() -> i32 {
            let mut sum = 0
            let mut i = 10
            while i < 5 { sum = sum + i; i = i + 1 }
            sum
        }
    "#);
    assert_eq!(val, interp::Value::Int(0));
}

#[test]
fn test_fuzz_recursive_factorial_zero() {
    let val = super::run_source(r#"
        func factorial(n: i32) -> i32 {
            if n <= 1 { 1 } else { n * factorial(n - 1) }
        }
        func main() -> i32 { factorial(0) }
    "#);
    assert_eq!(val, interp::Value::Int(1));
}

#[test]
fn test_fuzz_tuple_of_tuples() {
    let val = super::run_source(r#"
        func main() -> i32 {
            let t = ((1, 2), (3, 4))
            let inner = t.0
            inner.1
        }
    "#);
    assert_eq!(val, interp::Value::Int(2));
}
