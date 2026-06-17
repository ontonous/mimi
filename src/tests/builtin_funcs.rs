use super::*;

#[test]
fn builtin_len_string() {
    let src = r#"
func main() -> i32 {
    len("hello")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn builtin_len_list() {
    let src = r#"
func main() -> i32 {
    len([1, 2, 3, 4, 5])
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn builtin_len_empty_string() {
    let src = r#"
func main() -> i32 {
    len("")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn builtin_to_string_int() {
    let src = r#"
func main() -> string {
    to_string(42)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("42".to_string()));
}

#[test]
fn builtin_to_string_bool() {
    let src = r#"
func main() -> string {
    to_string(true)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("true".to_string()));
}

#[test]
fn builtin_abs_int() {
    let src = r#"
func main() -> i32 {
    abs(-5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn builtin_abs_float() {
    let src = r#"
func main() -> f64 {
    abs(-3.14)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Float(3.14));
}

#[test]
fn builtin_push() {
    let src = r#"
func main() -> i32 {
    let a = [1, 2, 3];
    let result = push(a, 4);
    len(result)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(4));
}

#[test]
fn builtin_pop() {
    let src = r#"
func main() -> i32 {
    let a = [1, 2, 3];
    let result = pop(a);
    let (popped, _) = result;
    popped
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn builtin_pop_returns_remaining() {
    let src = r#"
func main() -> i32 {
    let a = [1, 2, 3];
    let result = pop(a);
    let (_, new_list) = result;
    len(new_list)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(2));
}

#[test]
fn builtin_min_int() {
    let src = r#"
func main() -> i32 {
    min(3, 7)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn builtin_max_int() {
    let src = r#"
func main() -> i32 {
    max(3, 7)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(7));
}

#[test]
fn builtin_min_float() {
    let src = r#"
func main() -> f64 {
    min(3.14, 2.71)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Float(2.71));
}

#[test]
fn builtin_contains_list() {
    let src = r#"
func main() -> i32 {
    let a = [1, 2, 3, 4, 5];
    if contains(a, 3) { 1 } else { 0 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn builtin_contains_string() {
    let src = r#"
func main() -> i32 {
    let s = "hello world";
    if contains(s, "world") { 1 } else { 0 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn builtin_contains_not_found() {
    let src = r#"
func main() -> i32 {
    let a = [1, 2, 3];
    if contains(a, 99) { 1 } else { 0 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
}

// ===================== MimiSpec Runtime Functions Tests =====================

#[test]
fn builtin_lexer_basic() {
    let src = r#"
func main() -> i32 {
    let tokens = lexer("module Test:")
    len(tokens)
}
"#;
    let v = run_source(src);
    // Should return a list of tokens
    match v {
        interp::Value::Int(n) => assert!(n > 0, "lexer should return tokens"),
        _ => panic!("lexer should return a list"),
    }
}

#[test]
fn builtin_parse_basic() {
    let src = r#"
func main() -> i32 {
    let result = parse("module Test:")
    0
}
"#;
    let v = run_source(src);
    // Should return without error
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn builtin_parse_with_error() {
    let src = r#"
func main() -> i32 {
    let result = parse("module Test")
    0
}
"#;
    let v = run_source(src);
    // Should return without crashing
    assert_eq!(v, interp::Value::Int(0));
}
