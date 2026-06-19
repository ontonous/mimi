use super::*;

#[test]
fn string_concat() {
    let src = r#"
func main() -> string {
    let s = "hello" + " " + "world";
    s
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("hello world".to_string()));
}

#[test]
fn string_concat_empty() {
    let src = r#"
func main() -> string {
    let s = "" + "abc" + "";
    s
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("abc".to_string()));
}

#[test]
fn fstring_basic() {
    let src = r#"
func main() -> string {
    let name = "World";
    f"Hello, {name}!"
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("Hello, World!".to_string()));
}

#[test]
fn fstring_multiple_interpolations() {
    let src = r#"
func main() -> string {
    let a = 1;
    let b = 2;
    f"{a} + {b} = {a + b}"
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("1 + 2 = 3".to_string()));
}

#[test]
fn fstring_no_interpolation() {
    let src = r#"
func main() -> string {
    f"just text"
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("just text".to_string()));
}

#[test]
fn fstring_expression_interpolation() {
    let src = r#"
func main() -> string {
    let x = 10;
    f"double is {x * 2}"
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("double is 20".to_string()));
}

#[test]
fn fstring_with_function_call() {
    let src = r#"
func greet(name: string) -> string {
    f"Hi, {name}!"
}

func main() -> string {
    greet("Alice")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("Hi, Alice!".to_string()));
}

#[test]
fn string_compare_equal() {
    let src = r#"
func main() -> bool {
    "hello" == "hello"
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}

#[test]
fn string_compare_not_equal() {
    let src = r#"
func main() -> bool {
    "hello" != "world"
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}

#[test]
fn string_concat_long_chain() {
    let src = r#"
func main() -> string {
    let s = "a" + "b" + "c" + "d" + "e";
    s
}
"#;
    assert_eq!(run_source(src), interp::Value::String("abcde".to_string()));
}

#[test]
fn fstring_integer_expression() {
    let src = r#"
func main() -> string {
    let a = 10;
    let b = 20;
    f"sum = {a + b}"
}
"#;
    assert_eq!(run_source(src), interp::Value::String("sum = 30".to_string()));
}

#[test]
fn fstring_boolean_interpolation() {
    let src = r#"
func main() -> string {
    let flag = true;
    f"flag is {flag}"
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("flag is true".to_string()));
}

#[test]
fn string_from_function_return() {
    let src = r#"
func greet() -> string {
    "hello world"
}

func main() -> string {
    greet()
}
"#;
    assert_eq!(run_source(src), interp::Value::String("hello world".to_string()));
}

#[test]
fn string_concat_with_variable() {
    let src = r#"
func main() -> string {
    let prefix = "pre";
    let suffix = "fix";
    prefix + suffix
}
"#;
    assert_eq!(run_source(src), interp::Value::String("prefix".to_string()));
}
