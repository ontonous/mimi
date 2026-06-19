use super::*;

#[test]
fn interp_tuple_destructuring() {
    let src = r#"
func main() -> i32 {
    let (a, b, c) = (1, 2, 3);
    a + b + c
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(6));
}

#[test]
fn interp_unit_in_tuple() {
    let src = r#"
func main() -> i32 {
    let t = ((), 42);
    let (_, x) = t;
    x
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn interp_tuple_in_list() {
    let src = r#"
func main() -> i32 {
    let points = [(1, 2), (3, 4), (5, 6)];
    let (x, y) = points[0];
    x + y
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(3));
}

#[test]
fn interp_tuple_three_elements() {
    let src = r#"
func main() -> i32 {
    let t = (1, 2, 3);
    let (a, b, c) = t;
    a + b + c
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(6));
}

#[test]
fn interp_tuple_with_underscore() {
    let src = r#"
func main() -> i32 {
    let t = (10, 20, 30);
    let (a, _, c) = t;
    a + c
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(40));
}

#[test]
fn interp_tuple_as_arg() {
    let src = r#"
func first(p: (i32, i32)) -> i32 {
    let (a, _) = p;
    a
}

func main() -> i32 {
    first((42, 99))
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}
