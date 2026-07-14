use super::*;
use crate::ast::Item;

#[test]
fn parse_pub_func() {
    let src = r#"
pub func helper() -> i32 { 42 }

func main() -> i32 {
    helper()
}
"#;
    let file = parse(src);
    if let Some(Item::Func(f)) = file
        .items
        .iter()
        .find(|item| matches!(item, Item::Func(function) if function.name == "helper"))
    {
        assert!(f.pub_, "func should be marked as pub");
    } else {
        panic!("expected Func item");
    }
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn parse_pub_type() {
    let src = r#"
pub type Point {
    x: i32
    y: i32
}

func main() -> i32 {
    1
}
"#;
    let file = parse(src);
    if let Some(Item::Type(t)) = file
        .items
        .iter()
        .find(|item| matches!(item, Item::Type(type_def) if type_def.name == "Point"))
    {
        assert!(t.pub_, "type should be marked as pub");
    } else {
        panic!("expected Type item");
    }
}

#[test]
fn parse_non_pub_func() {
    let src = r#"
func helper() -> i32 { 42 }

func main() -> i32 {
    helper()
}
"#;
    let file = parse(src);
    if let Some(Item::Func(f)) = file
        .items
        .iter()
        .find(|item| matches!(item, Item::Func(function) if function.name == "helper"))
    {
        assert!(!f.pub_, "func without pub should not be marked as pub");
    } else {
        panic!("expected Func item");
    }
}
