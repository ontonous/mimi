use super::*;

#[test]
fn cap_combined_declaration() {
    let src = r#"
cap FullAccess = FileReadCap + FileWriteCap;

func main() -> i32 {
    42
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn cap_split_returns_tuple() {
    let src = r#"
cap FileReadCap;
cap FileWriteCap;
cap FullAccess = FileReadCap + FileWriteCap;

func main() -> i32 {
    let c = FullAccess;
    let parts = c.split();
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn cap_split_runtime() {
    let src = r#"
cap FileReadCap;
cap FileWriteCap;
cap FullAccess = FileReadCap + FileWriteCap;

func main() -> i32 {
    let c = FullAccess;
    let (read, write) = c.split();
    drop(read);
    drop(write);
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn cap_split_single_error() {
    let src = r#"
cap FileReadCap;

func main() -> i32 {
    let c = FileReadCap;
    c.split();
    42
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("split() requires a combined capability"),
        "Expected split error, got: {}",
        err
    );
}

#[test]
fn cap_split_drop_one() {
    let src = r#"
cap FileReadCap;
cap FileWriteCap;
cap FullAccess = FileReadCap + FileWriteCap;

func main() -> i32 {
    let c = FullAccess;
    let (read, write) = c.split();
    drop(read);
    drop(write);
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn cap_split_nested_combination() {
    let src = r#"
cap FileReadCap;
cap FileWriteCap;
cap FullAccess = FileReadCap + FileWriteCap;

func main() -> i32 {
    let c = FullAccess;
    let (read, write) = c.split();
    drop(read);
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn cap_split_use_individual_parts() {
    let src = r#"
cap FileReadCap;
cap FileWriteCap;
cap FullAccess = FileReadCap + FileWriteCap;

func use_read(r: FileReadCap) -> i32 {
    1
}

func use_write(w: FileWriteCap) -> i32 {
    2
}

func main() -> i32 {
    let c = FullAccess;
    let (read, write) = c.split();
    let a = use_read(read);
    let b = use_write(write);
    a + b
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn cap_move_through_aggregate_projection_is_consumed() {
    let src = r#"
cap FileCap;

func consume(c: cap FileCap) -> i32 {
    drop(c);
    1
}

func main(c: cap FileCap) -> i32 {
    let alias = c;
    consume([alias][0])
}
"#;
    let result = check_source(src);
    assert!(result.is_ok(), "unexpected errors: {result:?}");
}

#[test]
fn cap_move_through_if_expression_is_consumed_once() {
    let src = r#"
cap FileCap;

func consume(c: cap FileCap) -> i32 {
    drop(c);
    1
}

func main(c: cap FileCap, choose_left: bool) -> i32 {
    let alias = c;
    consume(if choose_left { alias } else { alias })
}
"#;
    let result = check_source(src);
    assert!(result.is_ok(), "unexpected errors: {result:?}");
}
