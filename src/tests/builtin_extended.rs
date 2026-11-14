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
fn builtin_pow_negative_base() {
    let src = r#"
func main() -> i64 {
    pow(-2, 3)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(-8));
}

#[test]
fn builtin_pow_negative_base_even_exp() {
    let src = r#"
func main() -> i64 {
    pow(-2, 4)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(16));
}

#[test]
fn builtin_pow_zero_exp() {
    let src = r#"
func main() -> i64 {
    pow(-5, 0)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(1));
}

#[test]
fn builtin_pow_negative_integer_exp_errors() {
    let src = r#"
func main() -> i32 {
    pow(2, -1)
}
"#;
    let err = run_source_result(src).expect_err("negative integer exponent must fail");
    assert!(
        err.contains("negative exponent"),
        "unexpected error: {}",
        err
    );
}

#[test]
fn builtin_floor() {
    let src = r#"
func observed_floor() -> f64 {
    floor(3.7)
}
func main() -> i32 {
    if observed_floor() == 3.0 { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn builtin_ceil() {
    let src = r#"
func observed_ceil() -> f64 {
    ceil(3.2)
}
func main() -> i32 {
    if observed_ceil() == 4.0 { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn builtin_round() {
    let src = r#"
func observed_round() -> f64 {
    round(3.5)
}
func main() -> i32 {
    if observed_round() == 4.0 { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
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
fn builtin_random_is_strictly_below_one() {
    let src = r#"
func main() -> bool {
    let mut i = 0
    while i < 10000 {
        let r = random()
        if r < 0.0 || r >= 1.0 { return false }
        i += 1
    }
    true
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}

#[test]
fn builtin_pi() {
    let src = r#"
func observed_pi() -> f64 {
    pi()
}
func main() -> i32 {
    let difference = observed_pi() - 3.141592653589793
    if difference > -0.0000000001 && difference < 0.0000000001 { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
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
func observed_to_float_from_string() -> f64 {
    to_float("2.5")
}
func main() -> i32 {
    let difference = observed_to_float_from_string() - 2.5
    if difference > -0.0000000001 && difference < 0.0000000001 { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn builtin_str_char_at() {
    let src = r#"
func observed_str_char_at() -> string {
    str_char_at("hello", 1)
}
func main() -> i32 {
    if observed_str_char_at() == "e" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn builtin_str_substring() {
    let src = r#"
func observed_str_substring() -> string {
    str_substring("hello world", 0, 5)
}
func main() -> i32 {
    if observed_str_substring() == "hello" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
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
