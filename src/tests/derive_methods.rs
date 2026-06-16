use super::*;

#[test]
fn derive_debug_to_string() {
    let src = r#"
#[derive(Debug)]
type Point {
    x: i32,
    y: i32
}

func main() -> string {
    let p = Point { x: 1, y: 2 };
    p.to_string()
}
"#;
    let result = run_source_result(src);
    assert!(result.is_ok(), "Debug derive should work: {:?}", result.err());
    let s = match result.unwrap() {
        interp::Value::String(s) => s,
        other => panic!("expected string, got {:?}", other),
    };
    assert!(s.contains("Point"), "should contain type name: {}", s);
    assert!(s.contains("x: 1"), "should contain field x: {}", s);
    assert!(s.contains("y: 2"), "should contain field y: {}", s);
}

#[test]
fn derive_clone_basic() {
    let src = r#"
#[derive(Clone)]
type Pair {
    a: i32,
    b: i32
}

func main() -> i32 {
    let p = Pair { a: 10, b: 20 };
    let q = p.clone();
    q.a + q.b
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(30));
}

#[test]
fn derive_eq_equal() {
    let src = r#"
#[derive(Eq)]
type Vec2 {
    x: i32,
    y: i32
}

func main() -> bool {
    let a = Vec2 { x: 1, y: 2 };
    let b = Vec2 { x: 1, y: 2 };
    a.eq(b)
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}

#[test]
fn derive_eq_not_equal() {
    let src = r#"
#[derive(Eq)]
type Vec2 {
    x: i32,
    y: i32
}

func main() -> bool {
    let a = Vec2 { x: 1, y: 2 };
    let b = Vec2 { x: 3, y: 4 };
    a.eq(b)
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(false));
}

#[test]
fn derive_multiple() {
    let src = r#"
#[derive(Debug, Clone, Eq)]
type Color {
    r: i32,
    g: i32,
    b: i32
}

func main() -> i32 {
    let c = Color { r: 255, g: 128, b: 0 };
    let d = c.clone();
    let s = c.to_string();
    d.r
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(255));
}
