use super::*;
#[test]
fn string_concatenation() {
    let src = r#"
func main() -> string {
    "hello" + " " + "world"
}
"#;
    assert_eq!(
        run_source(src),
        interp::Value::String(Arc::new("hello world".to_string()))
    );
}

#[test]
fn float_arithmetic_chain() {
    let src = r#"
func main() -> f64 {
    (1.5 + 2.5) * 2.0
}
"#;
    assert_eq!(run_source(src), interp::Value::Float(8.0));
}

#[test]
fn boolean_logic() {
    let src = r#"
func main() -> bool {
    true && false || true
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}

#[test]
fn comparison_chain() {
    let src = r#"
func main() -> bool {
    1 < 2 && 2 < 3 && 3 < 4
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}

#[test]
fn bitwise_operations() {
    let src = r#"
func main() -> i32 {
    (1 | 2) & 3
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(3));
}

#[test]
fn shift_operations() {
    let src = r#"
func main() -> i32 {
    (1 << 3) | (8 >> 2)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(10));
}

#[test]
fn power_operator() {
    let src = r#"
func main() -> i32 {
    2 ** 10
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(1024));
}

#[test]
fn negate_expression() {
    let src = r#"
func main() -> i32 {
    -(5 + 3)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(-8));
}

#[test]
fn not_expression() {
    let src = r#"
func main() -> bool {
    !false
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}

#[test]
fn short_circuit_and_early() {
    let src = r#"
func main() -> bool {
    false && side_effect()
}

func side_effect() -> bool {
    assert(false);
    true
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(false));
}

#[test]
fn short_circuit_or_early() {
    let src = r#"
func main() -> bool {
    true || side_effect()
}

func side_effect() -> bool {
    assert(false);
    true
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}
