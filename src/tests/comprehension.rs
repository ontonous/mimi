use super::*;

#[test]
fn comprehension_basic() {
    let src = r#"
func main() -> i32 {
    let nums = [1, 2, 3, 4, 5];
    let doubled = [x * 2 for x in nums];
    len(doubled)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn comprehension_with_guard() {
    let src = r#"
func main() -> i32 {
    let nums = [1, 2, 3, 4, 5, 6];
    let evens = [x for x in nums if x % 2 == 0];
    len(evens)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn comprehension_transform() {
    let src = r#"
func main() -> string {
    let words = ["hello", "world"];
    let upper = [w + "!" for w in words];
    upper[0] + " " + upper[1]
}
"#;
    let v = run_source(src);
    assert_eq!(
        v,
        interp::Value::String(Arc::new("hello! world!".to_string()))
    );
}

#[test]
fn comprehension_empty_list() {
    let src = r#"
func main() -> i32 {
    let empty = [];
    let result = [x for x in empty];
    len(result)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn comprehension_nested_unsupported() {
    let src = r#"
func main() -> i32 {
    let lists = [[1, 2], [3, 4], [5]];
    let flat = [x for sub in lists for x in sub];
    len(flat)
}
"#;
    let result = run_source_result(src);
    // Nested comprehensions not yet supported, should error
    assert!(result.is_err());
}
