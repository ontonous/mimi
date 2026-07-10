use super::*;

#[test]
fn interp_assignment() {
    let src = r#"
func main() -> i32 {
    let mut x = 10;
    x = 15;
    x = x * 2;
    x
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(30));
}

#[test]
fn interp_compound_assignment_plus_eq() {
    let src = r#"
func main() -> i32 {
    let mut x = 10;
    x += 5;
    x
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(15));
}

#[test]
fn interp_compound_assignment_minus_eq() {
    let src = r#"
func main() -> i32 {
    let mut x = 10;
    x -= 3;
    x
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(7));
}

#[test]
fn interp_compound_assignment_mul_eq() {
    let src = r#"
func main() -> i32 {
    let mut x = 10;
    x *= 4;
    x
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(40));
}

#[test]
fn interp_compound_assignment_div_eq() {
    let src = r#"
func main() -> i32 {
    let mut x = 20;
    x /= 4;
    x
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn interp_negation() {
    let src = r#"
func main() -> i32 {
    let x = 5;
    let y = -x;
    let z = --x;
    y + z
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn interp_negative_literal() {
    let src = r#"
func main() -> i32 {
    let x = -5;
    x + 10
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn interp_double_negation() {
    let src = r#"
func main() -> i32 {
    let x = 5;
    let y = -x;
    let z = -y;
    z
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn typecheck_uninitialized_let() {
    let src = r#"
func main() -> i32 {
    let x: i32;
    x
}
"#;
    let result = check_source(src);
    // uninitialized let with type annotation should fail type checking
    assert!(
        result.is_err(),
        "uninitialized typed let should fail: {:?}",
        result.ok()
    );
}

#[test]
fn typecheck_assignment_mismatch() {
    let src = r#"
func main() {
    let x: i32 = 10;
    x = "hello";
}
"#;
    let errs = check_source(src).unwrap_err();
    assert!(errs.iter().any(|d| d.message.contains("cannot assign")));
}

#[test]
fn typecheck_unused_variable() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    0
}
"#;
    let result = check_source(src);
    assert!(result.is_ok());
}

#[test]
fn parse_let_missing_initializer_errors() {
    // Regression: `let x =` with no expression should be a parse error.
    // Note: newlines after `=` are now skipped, so `let x =\n42` is valid.
    // Only truly empty let bindings should error.
    let src = r#"
func main() -> i32 {
    let x =
}
"#;
    let tokens = lexer::Lexer::new(src).tokenize().unwrap();
    let (_file, errors) = parser::Parser::new(tokens).parse_file_with_recovery();
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("expected expression after `=`")),
        "expected parse error for missing initializer, got: {:?}",
        errors
    );
}

#[test]
fn parse_let_multiline_initializer() {
    // `let x =\n42` should parse 42 as the initializer (newlines after = are skipped).
    let src = r#"
func main() -> i32 {
    let x =
    42
    x
}
"#;
    let tokens = lexer::Lexer::new(src).tokenize().unwrap();
    let (file, errors) = parser::Parser::new(tokens).parse_file_with_recovery();
    assert!(
        errors.is_empty(),
        "multiline let initializer should not error, got: {:?}",
        errors
    );
    // Verify the let binding exists with initializer
    use crate::ast::{Item, Stmt};
    let has_let = file.items.iter().any(|item| {
        if let Item::Func(func) = item {
            func.body
                .iter()
                .any(|stmt| matches!(stmt, Stmt::Let { init: Some(_), .. }))
        } else {
            false
        }
    });
    assert!(has_let, "expected let with initializer");
}
