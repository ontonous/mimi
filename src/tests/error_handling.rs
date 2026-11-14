use super::*;

#[test]
fn on_failure_executes_on_error() {
    let src = r#"
func fail() -> Result<i32, string> {
    Err("boom")
}

func cleanup() {
    println("CLEANUP_RAN");
}

func execute() -> Result<i32, string> {
    on failure { cleanup(); }
    let _ = fail()?;
    Ok(0)
}

func main() -> i32 {
    match execute() {
        Ok(_) => 0,
        Err(error) => { println(error); 1 },
    }
}
"#;
    let (value, stdout) = run_source_with_stdout(src);
    assert_eq!(value, interp::Value::Int(1));
    assert!(
        stdout.contains("CLEANUP_RAN"),
        "cleanup did not run: {stdout}"
    );
    assert!(
        stdout.contains("boom"),
        "propagated error was lost: {stdout}"
    );
}

#[test]
fn on_failure_lifo_order() {
    let src = r#"
func fail() -> Result<i32, string> {
    Err("boom")
}

func execute() -> Result<i32, string> {
    on failure { println("C"); }
    on failure { println("B"); }
    on failure { println("A"); }
    let _ = fail()?;
    Ok(0)
}

func main() -> i32 {
    match execute() {
        Ok(_) => 0,
        Err(error) => { println(error); 1 },
    }
}
"#;
    let (value, stdout) = run_source_with_stdout(src);
    assert_eq!(value, interp::Value::Int(1));
    let positions = ["A", "B", "C", "boom"].map(|marker| {
        stdout
            .find(marker)
            .unwrap_or_else(|| panic!("missing {marker}: {stdout}"))
    });
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "failure handlers must run in LIFO order before the propagated error: {stdout}"
    );
}

#[test]
fn on_failure_no_execute_on_success() {
    let src = r#"
func succeed() -> Result<i32, string> {
    Ok(42)
}

func execute() -> Result<i32, string> {
    on failure { println("SHOULD_NOT_RUN"); }
    let x = succeed()?;
    Ok(x)
}

func main() -> i32 {
    match execute() {
        Ok(x) => x,
        Err(_) => -1,
    }
}
"#;
    let v = run_source(src);
    assert_eq!(
        v,
        interp::Value::Int(42),
        "Compensation should NOT execute on success"
    );
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
    assert!(
        err.contains("division by zero"),
        "Expected division by zero error, got: {}",
        err
    );
}

#[test]
fn bugfix_exit_basic() {
    let src = r#"
func main() -> i32 {
    exit(0);
    42
}
"#;
    let result = run_source_result(src);
    assert!(result.is_ok(), "exit should terminate: {:?}", result);
}

#[test]
fn bugfix_exit_nonzero() {
    let src = r#"
func main() -> i32 {
    exit(1);
    0
}
"#;
    let result = run_source_result(src);
    assert!(
        result.is_ok(),
        "exit with code 1 should terminate: {:?}",
        result
    );
}

#[test]
fn bugfix_assert_approx_eq_pass() {
    let src = r#"
func main() -> i32 {
    assert_approx_eq(3.14, 3.14);
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn bugfix_assert_approx_eq_fail() {
    let src = r#"
func main() -> i32 {
    assert_approx_eq(3.14, 3.15);
    0
}
"#;
    let result = run_source_result(src);
    assert!(
        result.is_err(),
        "assert_approx_eq should fail for unequal floats"
    );
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
    assert!(
        err.contains("division by zero") || err.contains("modulo"),
        "Expected division/modulo by zero error, got: {}",
        err
    );
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
    assert!(
        err.contains("negative exponent"),
        "Expected negative exponent error, got: {}",
        err
    );
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
    let result = run_source_bytecode_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("immutable"),
        "Expected immutable error, got: {}",
        err
    );
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
    // Result errors must propagate through expression statements to the
    // enclosing Result-returning helper.
    let src = r#"
func fail() -> Result<i32, string> { Err("boom") }

func execute() -> Result<i32, string> {
    fail()?;
    Ok(1)
}

func main() -> i32 {
    match execute() {
        Ok(_) => 0,
        Err(error) => { println(error); 1 },
    }
}
"#;
    let (value, stdout) = run_source_with_stdout(src);
    assert_eq!(value, interp::Value::Int(1));
    assert!(
        stdout.contains("boom"),
        "expression-statement `?` lost its propagated error: {stdout}"
    );
}

#[test]
fn bugfix_float_division_by_zero() {
    let src = r#"
func divide_float() -> f64 {
    10.0 / 0.0
}
func main() -> i32 {
    let _ = divide_float();
    0
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("division by zero"),
        "Expected division by zero error, got: {}",
        err
    );
}
