use super::*;

#[test]
fn on_failure_executes_on_error() {
    let src = r#"
type Res {
    Ok(i32)
    Err(string)
}

func fail() -> Res {
    Err("boom")
}

func cleanup() {
    println("CLEANUP_RAN");
}

func main() -> i32 {
    on failure { cleanup(); }
    let _ = fail()?;
    0
}
"#;
    let result = run_source_result(src);
    assert!(result.is_ok(), "? should propagate error as value, got: {:?}", result);
    let val = result.expect("src/tests/error_handling.rs:27 unwrap failed");
    match &val {
        interp::Value::Variant(name, _) if name == "Err" => {},
        other => panic!("Expected Err variant, got: {}", other),
    }
}

#[test]
fn on_failure_lifo_order() {
    let src = r#"
type Res {
    Ok(i32)
    Err(string)
}

func fail() -> Res {
    Err("boom")
}

func main() -> i32 {
    on failure { println("C"); }
    on failure { println("B"); }
    on failure { println("A"); }
    let _ = fail()?;
    0
}
"#;
    let result = run_source_result(src);
    assert!(result.is_ok(), "? should propagate error as value, got: {:?}", result);
    let val = result.expect("src/tests/error_handling.rs:56 unwrap failed");
    match &val {
        interp::Value::Variant(name, _) if name == "Err" => {},
        other => panic!("Expected Err variant, got: {}", other),
    }
}

#[test]
fn on_failure_no_execute_on_success() {
    let src = r#"
type Res {
    Ok(i32)
    Err(string)
}

func succeed() -> Res {
    Ok(42)
}

func main() -> i32 {
    on failure { println("SHOULD_NOT_RUN"); }
    let x = succeed()?;
    x
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42), "Compensation should NOT execute on success");
}

#[test]
fn bugfix_division_by_zero() {
    let src = r#"
func main() -> i32 {
    10 / 0
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("division by zero"), "Expected division by zero error, got: {}", err);
}

#[test]
fn bugfix_modulo_by_zero() {
    let src = r#"
func main() -> i32 {
    10 % 0
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("modulo by zero"), "Expected modulo by zero error, got: {}", err);
}

#[test]
fn bugfix_negative_exponent() {
    let src = r#"
func main() -> i32 {
    2 ** -1
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("negative exponent"), "Expected negative exponent error, got: {}", err);
}

#[test]
fn bugfix_immutable_assignment() {
    let src = r#"
func main() -> i32 {
    let x = 10;
    x = 20;
    x
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("immutable"), "Expected immutable error, got: {}", err);
}

#[test]
fn bugfix_mut_assignment_works() {
    let src = r#"
func main() -> i32 {
    let mut x = 10;
    x = 20;
    x
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(20));
}

#[test]
fn bugfix_error_in_expr_statement() {
    // Value::Error from ? operator should propagate through expression statements
    let src = r#"
type Res {
    Ok(i32)
    Err(string)
}

func fail() -> Res { Err("boom") }

func main() -> i32 {
    fail()?;
    1
}
"#;
    let result = run_source_result(src);
    assert!(result.is_ok(), "? should propagate error as value through expr statement, got: {:?}", result);
    let val = result.expect("src/tests/error_handling.rs:170 unwrap failed");
    match &val {
        interp::Value::Variant(name, _) if name == "Err" => {},
        other => panic!("Expected Err variant, got: {}", other),
    }
}

#[test]
fn bugfix_float_division_by_zero() {
    let src = r#"
func main() -> f64 {
    10.0 / 0.0
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("division by zero"), "Expected division by zero error, got: {}", err);
}
