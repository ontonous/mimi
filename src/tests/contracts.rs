use super::*;

#[test]
fn requires_passes() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    requires: a > 0
    requires: b > 0
    a + b
}

func main() -> i32 {
    add(1, 2)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn requires_fails() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    requires: a > 0
    a + b
}

func main() -> i32 {
    add(-1, 2)
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("requires condition failed"),
        "Expected requires error, got: {}",
        err
    );
}

#[test]
fn ensures_passes() {
    let src = r#"
func double(x: i32) -> i32 {
    ensures: result == x * 2
    x * 2
}

func main() -> i32 {
    double(5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(10));
}

#[test]
fn ensures_fails() {
    let src = r#"
func buggy(x: i32) -> i32 {
    ensures: result == x * 2
    x * 3
}

func main() -> i32 {
    buggy(5)
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("ensures condition failed"),
        "Expected ensures error, got: {}",
        err
    );
}

#[test]
fn requires_ensures_combined() {
    let src = r#"
func abs_val(x: i32) -> i32 {
    requires: x != 0
    ensures: result > 0
    if x < 0 { -x } else { x }
}

func main() -> i32 {
    abs_val(-5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn old_basic_snapshot() {
    let src = r#"
func double(x: i32) -> i32 {
    ensures: result == old(x) * 2
    return x * 2;
}

func main() -> i32 {
    double(5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(10));
}

#[test]
fn old_with_mutation() {
    let src = r#"
func increment(x: i32) -> i32 {
    ensures: result == old(x) + 1
    return x + 1;
}

func main() -> i32 {
    increment(10)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(11));
}

#[test]
fn old_fails() {
    let src = r#"
func bad(x: i32) -> i32 {
    ensures: result == old(x) + 10
    return x + 1;
}

func main() -> i32 {
    bad(5)
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("ensures condition failed"),
        "Expected ensures error, got: {}",
        err
    );
}

#[test]
fn old_multiple_params() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    ensures: result == old(a) + old(b)
    return a + b;
}

func main() -> i32 {
    add(3, 4)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(7));
}

#[test]
fn old_on_string_non_copy() {
    let src = r#"
func append_world(s: string) -> string {
    ensures: result == old(s) + "world"
    return s + "world";
}

func main() -> string {
    append_world("hello")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("helloworld".to_string()));
}

#[test]
fn ensures_result_binding_in_type_check() {
    let src = r#"
func inc(x: i32) -> i32 {
    ensures: result == x + 1
    x + 1
}

func main() -> i32 {
    inc(41)
}
"#;
    assert!(
        check_source(src).is_ok(),
        "ensures with `result` should type-check"
    );
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn ensures_result_with_old_binding() {
    let src = r#"
func add_to(x: i32, y: i32) -> i32 {
    ensures: result == old(x) + y
    x + y
}

func main() -> i32 {
    add_to(40, 2)
}
"#;
    assert!(
        check_source(src).is_ok(),
        "ensures with `result` and `old()` should type-check"
    );
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn old_with_multiple_returns() {
    let src = r#"
func abs(x: i32) -> i32 {
    ensures: result >= 0
    ensures: result == old(x) || result == -old(x)
    if x < 0 {
        return -x;
    }
    return x;
}

func main() -> i32 {
    abs(-5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

fn can_link() -> bool {
    crate::tests::can_link()
}

#[test]
fn ensures_string_concat_with_old_runtime_check() {
    let src = r#"
func greet(name: string) -> string {
    ensures: result == "Hello, " + old(name)
    return "Hello, " + name;
}

func main() -> i32 {
    let g = greet("World");
    println(g);
    0
}
"#;
    let result = run_source_result(src);
    assert!(
        result.is_ok(),
        "string ensures via --verify-contracts should pass: {:?}",
        result
    );
}

#[test]
fn ensures_string_requires_violation_caught() {
    let src = r#"
func greet(name: string) -> string {
    requires: len(name) > 0
    return "Hello, " + name;
}

func main() -> i32 {
    let g = greet("");
    println(g);
    0
}
"#;
    let v = run_source_result(src);
    assert!(v.is_err(), "string requires violation should be caught");
}

#[test]
fn ensures_string_requires_ok() {
    let src = r#"
func greet(name: string) -> string {
    requires: len(name) > 0
    return "Hello, " + name;
}

func main() -> i32 {
    let g = greet("World");
    println(g);
    0
}
"#;
    let v = run_source_result(src);
    assert!(
        v.is_ok(),
        "string requires with valid input should pass: {:?}",
        v
    );
}

#[test]
fn ensures_string_concat_with_old_snapshot() {
    let src = r#"
func append_world(s: string) -> string {
    ensures: result == old(s) + "world"
    return s + "world";
}

func main() -> string {
    append_world("hello")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("helloworld".to_string()));
}

#[test]
fn codegen_contract_string_requires_nonempty() {
    if !can_link() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let stdout = compile_and_verify_contracts(
        r#"
func greet(name: string) -> i32 {
    requires: name != ""
    println(name);
    0
}

func main() -> i32 {
    greet("World")
}
"#,
    )
    .expect("string requires should pass under codegen");
    assert_eq!(stdout.trim(), "World");
}

#[test]
fn codegen_contract_string_requires_nonempty_fails() {
    if !can_link() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let result = compile_and_verify_contracts(
        r#"
func greet(name: string) -> i32 {
    requires: name != ""
    println(name);
    0
}

func main() -> i32 {
    greet("")
}
"#,
    );
    assert!(result.is_err(), "empty string should fail requires");
}

#[test]
fn dual_contract_string_requires_nonempty() {
    if !can_link() {
        return;
    }
    dual_assert_contract_ok(
        r#"
func greet(name: string) -> i32 {
    requires: name != ""
    println(name);
    0
}

func main() -> i32 {
    greet("World")
}
"#,
    );
}

#[test]
fn dual_contract_string_nonempty_ensures() {
    if !can_link() {
        return;
    }
    dual_assert_contract_ok(
        r#"
func greet(name: string) -> i32 {
    requires: name != ""
    ensures: result == 0
    println(name);
    0
}

func main() -> i32 {
    greet("World")
}
"#,
    );
}
