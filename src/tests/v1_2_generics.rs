use super::*;

#[test]
fn generic_identity_function() {
    let src = r#"
func id<T>(x: T) -> T {
    x
}

func main() -> i32 {
    id::<i32>(42)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn generic_type_inference() {
    let src = r#"
func id<T>(x: T) -> T {
    x
}

func main() -> string {
    id("hello")
}
"#;
    assert_eq!(
        run_source(src),
        interp::Value::String(Arc::new("hello".to_string()))
    );
}

#[test]
fn generic_multi_param() {
    let src = r#"
func pair<A, B>(a: A, b: B) -> (A, B) {
    (a, b)
}

func main() -> i32 {
    let p = pair(1, "two");
    match p {
        (1, "two") => 10,
        _ => 0,
    }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(10));
}

#[test]
fn generic_turbofish() {
    let src = r#"
func identity<T>(x: T) -> T {
    x
}

func main() -> i32 {
    let x = identity::<i32>(100);
    x
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(100));
}

#[test]
fn generic_type_def() {
    let src = r#"
type Box<T> {
    value: T
}

func main() -> i32 {
    let b = Box { value: 42 };
    b.value
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn generic_function_with_generic_type() {
    let src = r#"
type Wrapper<T> {
    inner: T
}

func wrap<T>(x: T) -> Wrapper<T> {
    Wrapper { inner: x }
}

func main() -> i32 {
    let w = wrap(42);
    w.inner
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn generic_parsing_no_generics() {
    // Ensure non-generic functions still work
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    a + b
}

func main() -> i32 {
    add(3, 4)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(7));
}
