use super::*;

#[test]
fn cap_declaration() {
    let src = r#"
cap FileRead;

func main() -> i32 {
    42
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn cap_combined_declaration() {
    let src = r#"
cap A;
cap B = A;

func main() -> i32 {
    42
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn cap_combined_split() {
    let src = r#"
cap A;
cap B;
cap Combined = A + B;

func main() -> i32 {
    let (a, b) = Combined.split();
    42
}
"#;
    let result = run_source_result(src);
    assert!(result.is_ok(), "combined cap split should work");
}

#[test]
fn cap_drop() {
    let src = r#"
cap IO;

func main() -> i32 {
    drop(IO);
    42
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn cap_in_function_with_effect() {
    let src = r#"
cap FileRead;

func read(path: string) with FileRead {
    println(path);
}

func main() -> i32 {
    42
}
"#;
    let result = check_source(src);
    assert!(result.is_ok());
}
