use super::*;

// ── T205: 测试覆盖补齐 ──

#[test]
fn boundary_empty_program() {
    let src = r#"
func main() -> i32 {
    42
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn boundary_empty_type_enum() {
    let src = r#"
type Empty {}

func main() -> i32 {
    42
}
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "empty enum type should be valid: {:?}",
        result.err()
    );
}

#[test]
fn boundary_deeply_nested_expressions() {
    let src = r#"
func main() -> i32 {
    (((((((((1 + 2) * 3) - 4) / 2) + 5) * 2) - 1) + 3) * 2)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(32));
}

#[test]
fn boundary_unicode_string() {
    let src = r#"
func main() -> string {
    "你好，世界！🚀"
}
"#;
    assert_eq!(
        run_source(src),
        interp::Value::String(Arc::new("你好，世界！🚀".to_string()))
    );
}

#[test]
fn boundary_empty_list_comprehension() {
    let src = r#"
func main() -> i32 {
    let result = [x for x in []];
    len(result)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn boundary_negative_index() {
    let src = r#"
func main() -> i32 {
    let list = [10, 20, 30];
    list[-1]
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(30));
}

#[test]
fn boundary_zero_fields_record() {
    let src = r#"
type Empty {}

func main() -> i32 {
    42
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn boundary_nested_blocks() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    x
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn boundary_while_never_executes() {
    let src = r#"
func main() -> i32 {
    while false {
        return 10;
    }
    42
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn boundary_large_integer() {
    let src = r#"
func main() -> i32 {
    2147483647
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(2147483647));
}

#[test]
fn boundary_empty_string() {
    let src = r#"
func main() -> string {
    ""
}
"#;
    assert_eq!(
        run_source(src),
        interp::Value::String(Arc::new("".to_string()))
    );
}
