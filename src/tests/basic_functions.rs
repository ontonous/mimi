use super::*;

#[test]
fn parse_func_with_contracts() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    requires: a > 0
    ensures: result == a + b
    return a + b;
}

func main() {
    println(add(1, 2));
}
"#;
    parse(src);
}

#[test]
fn typecheck_return_mismatch() {
    let src = r#"
func main() -> i32 {
    return "hello";
}
"#;
    let errs = check_source(src).unwrap_err();
    assert!(errs.iter().any(|d| d.message.contains("return type mismatch")));
}

#[test]
fn typecheck_arg_mismatch() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    return a + b;
}
func main() {
    add(1, "two");
}
"#;
    let errs = check_source(src).unwrap_err();
    assert!(errs.iter().any(|d| d.message.contains("argument 2")));
}

#[test]
fn typecheck_func_no_return() {
    let src = r#"
func main() -> i32 {
    println("hello");
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "expected error: i32 function with println (returns unit) as last expression");
    let errors = result.unwrap_err();
    assert!(errors.iter().any(|d| d.message.contains("implicit return")),
        "expected implicit return error, got: {:?}", errors);
}

#[test]
fn typecheck_recursive_func() {
    let src = r#"
func countdown(n: i32) -> i32 {
    if n <= 0 {
        return 0;
    }
    countdown(n - 1)
}

func main() -> i32 {
    countdown(5)
}
"#;
    assert!(check_source(src).is_ok());
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn typecheck_mutually_recursive_funcs() {
    let src = r#"
func is_even(n: i32) -> bool {
    if n == 0 {
        return true;
    }
    is_odd(n - 1)
}

func is_odd(n: i32) -> bool {
    if n == 0 {
        return false;
    }
    is_even(n - 1)
}

func main() -> i32 {
    if is_even(4) { 1 } else { 0 }
}
"#;
    assert!(check_source(src).is_ok());
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn interp_unit_return() {
    let src = r#"
func do_nothing() {
    println("nothing");
}

func main() -> i32 {
    do_nothing();
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn interp_requires_ensures_in_brace_block() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    requires: a > 0
    ensures: result == a + b
    return a + b;
}

func main() -> i32 {
    add(1, 2)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn typecheck_i32_coercion_to_i64_param() {
    let src = r#"
func double(x: i64) -> i64 { x * 2 }
func main() -> i64 {
    double(21)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn typecheck_i64_return_and_arith() {
    let src = r#"
func identity(x: i64) -> i64 { x }
func main() -> i64 {
    identity(41)
}
"#;
    assert!(check_source(src).is_ok());
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(41));
}

#[test]
fn interp_deeply_nested_if_else() {
    let src = r#"
func main() -> i32 {
    if true { if true { if true { if true { if true { 42 } else { 0 } } else { 0 } } else { 0 } } else { 0 } } else { 0 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn typecheck_f64_min_edge() {
    let src = r#"
func main() -> f64 {
    0.000001
}
"#;
    assert!(check_source(src).is_ok());
    let v = run_source(src);
    assert_eq!(v, interp::Value::Float(0.000001));
}

#[test]
fn interp_nested_function_calls() {
    let src = r#"
func double(x: i32) -> i32 { x * 2 }
func inc(x: i32) -> i32 { x + 1 }

func main() -> i32 {
    double(inc(5))
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(12));
}

#[test]
fn interp_format_basic() {
    let src = r#"
func main() -> string {
    format("hello {} world", "beautiful")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("hello beautiful world".to_string()));
}

#[test]
fn interp_format_multi() {
    let src = r#"
func main() -> string {
    format("{} + {} = {}", 1, 2, 3)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("1 + 2 = 3".to_string()));
}

#[test]
fn interp_format_no_placeholders() {
    let src = r#"
func main() -> string {
    format("hello world")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("hello world".to_string()));
}

#[test]
fn interp_format_mixed_types() {
    let src = r#"
func main() -> string {
    format("int={} float={} str={}", 42, 3.14, "test")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("int=42 float=3.14 str=test".to_string()));
}

#[test]
fn interp_default_param_value() {
    let src = r#"
func greet(name: string = "world") -> string {
    "hello " + name
}

func main() -> string {
    greet()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("hello world".to_string()));
}

#[test]
fn interp_default_param_value_override() {
    let src = r#"
func greet(name: string = "world") -> string {
    "hello " + name
}

func main() -> string {
    greet("mimi")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("hello mimi".to_string()));
}

#[test]
fn interp_default_param_multi() {
    let src = r#"
func add(a: i32 = 10, b: i32 = 20) -> i32 {
    a + b
}

func main() -> i32 {
    add(5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(25));
}

#[test]
fn interp_named_args_basic() {
    let src = r#"
func div(a: i32, b: i32) -> i32 {
    a / b
}

func main() -> i32 {
    div(b = 2, a = 10)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn interp_named_args_with_defaults() {
    let src = r#"
func create_point(x: i32 = 0, y: i32 = 0) -> string {
    "(" + to_string(x) + "," + to_string(y) + ")"
}

func main() -> string {
    create_point(y = 5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("(0,5)".to_string()));
}
