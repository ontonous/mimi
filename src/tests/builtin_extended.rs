use super::*;

#[test]
fn builtin_print() {
    let src = r#"
func main() -> i32 {
    print("hello");
    print(" ", "world");
    42
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn builtin_pow() {
    let src = r#"
func main() -> i32 {
    pow(2, 10)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(1024));
}

#[test]
fn builtin_floor() {
    let src = r#"
func main() -> f64 {
    floor(3.7)
}
"#;
    assert_eq!(run_source(src), interp::Value::Float(3.0));
}

#[test]
fn builtin_ceil() {
    let src = r#"
func main() -> f64 {
    ceil(3.2)
}
"#;
    assert_eq!(run_source(src), interp::Value::Float(4.0));
}

#[test]
fn builtin_round() {
    let src = r#"
func main() -> f64 {
    round(3.5)
}
"#;
    assert_eq!(run_source(src), interp::Value::Float(4.0));
}

#[test]
fn builtin_random_range() {
    let src = r#"
func main() -> bool {
    let r = random();
    r >= 0.0 && r < 1.0
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}

#[test]
fn builtin_pi() {
    let src = r#"
func main() -> f64 {
    pi()
}
"#;
    let result = run_source(src);
    if let interp::Value::Float(f) = result {
        assert!((f - std::f64::consts::PI).abs() < 1e-10);
    } else {
        panic!("expected float, got {:?}", result);
    }
}

#[test]
fn builtin_file_exists() {
    let src = r#"
func main() -> bool {
    file_exists("/nonexistent/path/that/should/not/exist.mimi")
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(false));
}

#[test]
fn builtin_to_int_from_string() {
    let src = r#"
func main() -> i32 {
    to_int("42")
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn builtin_to_float_from_string() {
    let src = r#"
func main() -> f64 {
    to_float("3.14")
}
"#;
    let result = run_source(src);
    if let interp::Value::Float(f) = result {
        assert!((f - 3.14).abs() < 1e-10);
    } else {
        panic!("expected float, got {:?}", result);
    }
}

#[test]
fn builtin_str_char_at() {
    let src = r#"
func main() -> string {
    str_char_at("hello", 1)
}
"#;
    assert_eq!(run_source(src), interp::Value::String("e".into()));
}

#[test]
fn builtin_str_substring() {
    let src = r#"
func main() -> string {
    str_substring("hello world", 0, 5)
}
"#;
    assert_eq!(run_source(src), interp::Value::String("hello".into()));
}

#[test]
fn builtin_str_parse_int_success() {
    let src = r#"
func main() -> bool {
    let (ok, _) = str_parse_int("123");
    ok
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}

#[test]
fn builtin_str_parse_int_failure() {
    let src = r#"
func main() -> bool {
    let (ok, _) = str_parse_int("abc");
    ok
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(false));
}

#[test]
fn builtin_keys() {
    let src = r#"
type Point {
    x: i32,
    y: i32
}

func main() -> bool {
    let p = Point { x: 1, y: 2 };
    let k = keys(p);
    len(k) == 2
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}

#[test]
fn builtin_values() {
    let src = r#"
type Point {
    x: i32,
    y: i32
}

func main() -> i32 {
    let p = Point { x: 10, y: 20 };
    let v = values(p);
    len(v)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(2));
}

#[test]
fn builtin_has_key() {
    let src = r#"
type Point {
    x: i32,
    y: i32
}

func main() -> bool {
    let p = Point { x: 1, y: 2 };
    has_key(p, "x")
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}
