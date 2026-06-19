use super::*;

#[test]
fn interp_list_access() {
    let src = r#"
func main() -> i32 {
    let xs = [1, 2, 3, 4, 5];
    xs[0] + xs[4]
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(6));
}

#[test]
fn interp_list_len() {
    let src = r#"
func main() -> i32 {
    let xs = [1, 2, 3];
    len(xs)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(3));
}

#[test]
fn interp_list_of_lists() {
    let src = r#"
func main() -> i32 {
    let nested = [[1, 2], [3, 4], [5]];
    nested[0][0] + nested[1][1] + nested[2][0]
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(1 + 4 + 5));
}

#[test]
fn interp_list_index_variable() {
    let src = r#"
func main() -> i32 {
    let xs = [10, 20, 30];
    let idx = 1;
    xs[idx]
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(20));
}

#[test]
fn interp_list_of_strings() {
    let src = r#"
func main() -> string {
    let words = ["hello", " ", "world"];
    words[0] + words[1] + words[2]
}
"#;
    assert_eq!(run_source(src), interp::Value::String("hello world".to_string()));
}

#[test]
fn interp_list_iterate_for() {
    let src = r#"
func main() -> i32 {
    let xs = [10, 20, 30];
    let mut sum = 0;
    for x in xs {
        sum = sum + x;
    }
    sum
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(60));
}
