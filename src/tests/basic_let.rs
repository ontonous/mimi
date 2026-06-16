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
    assert!(result.is_err(), "uninitialized typed let should fail: {:?}", result.ok());
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
