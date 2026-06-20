use super::harness::arb_mimi_program;
use crate::{core, interp, lexer, parser};

/// Fuzz target: interpret randomly generated Mimi programs.
/// We verify the interpreter never panics. Runtime errors (division by zero,
/// out-of-bounds, etc.) are expected but must not cause panics.
proptest::proptest! {
    #[test]
    fn fuzz_interpreter_no_panic(src in arb_mimi_program()) {
        if let Ok(tokens) = lexer::Lexer::new(&src).tokenize() {
            if let Ok(file) = parser::Parser::new(tokens).parse_file() {
                if core::check(&file).is_ok() {
                    let mut interp = interp::Interpreter::new(&file);
                    interp.verify_contracts = false;
                    let _ = interp.run();
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
                let mut interp = interp::Interpreter::new(&file);
                let result = interp.run().unwrap();
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
    let mut interp = interp::Interpreter::new(&file);
    let _ = interp.run();
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
    let mut interp = interp::Interpreter::new(&file);
    let _ = interp.run();
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
    let mut interp = interp::Interpreter::new(&file);
    let result = interp.run().unwrap();
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
    let mut interp = interp::Interpreter::new(&file);
    let result = interp.run().unwrap();
    assert_eq!(result, interp::Value::Int(42));
}

fn parse_src(src: &str) -> crate::ast::File {
    let tokens = lexer::Lexer::new(src).tokenize().unwrap();
    parser::Parser::new(tokens).parse_file().unwrap()
}
