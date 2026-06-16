use super::*;
use crate::ast::{Item, Stmt};

#[test]
fn mms_block_exists() {
    let src = r#"
        func add(a: i32, b: i32) -> i32 {
            mms {
                some content here
            }
            a + b
        }
    "#;
    let file = parse(src);
    let func = file.items.iter().find_map(|item| {
        if let Item::Func(f) = item { Some(f) } else { None }
    }).unwrap();
    let mms_stmt = func.body.iter().find(|s| matches!(s, Stmt::MmsBlock { .. }));
    assert!(mms_stmt.is_some(), "should have MMS block");
}

#[test]
fn mms_block_has_content() {
    let src = r#"
        func main() {
            mms {
                hello world
            }
        }
    "#;
    let file = parse(src);
    let func = file.items.iter().find_map(|item| {
        if let Item::Func(f) = item { Some(f) } else { None }
    }).unwrap();
    let mms_stmt = func.body.iter().find(|s| matches!(s, Stmt::MmsBlock { .. }));
    assert!(mms_stmt.is_some());
    if let Stmt::MmsBlock { content, .. } = mms_stmt.unwrap() {
        assert!(!content.is_empty(), "content should not be empty");
    }
}

#[test]
fn mms_block_runtime_accessible() {
    let src = r#"
        func main() -> i32 {
            mms {
                some content
            }
            42
        }
    "#;
    let result = run_source(src);
    assert_eq!(result, interp::Value::Int(42));
}

#[test]
fn mms_block_multiple() {
    let src = r#"
        func main() {
            mms { first block }
            mms { second block }
        }
    "#;
    let file = parse(src);
    let func = file.items.iter().find_map(|item| {
        if let Item::Func(f) = item { Some(f) } else { None }
    }).unwrap();
    let mms_count = func.body.iter().filter(|s| matches!(s, Stmt::MmsBlock { .. })).count();
    assert!(mms_count >= 2, "should have at least 2 MMS blocks");
}

#[test]
fn mms_block_in_module() {
    let src = r#"
        module Math {
            func add(a: i32, b: i32) -> i32 {
                mms { some content }
                a + b
            }
        }
    "#;
    let file = parse(src);
    let module = file.items.iter().find_map(|item| {
        if let Item::Module(m) = item { Some(m) } else { None }
    }).unwrap();
    let func = module.items.iter().find_map(|item| {
        if let Item::Func(f) = item { Some(f) } else { None }
    }).unwrap();
    let mms_stmt = func.body.iter().find(|s| matches!(s, Stmt::MmsBlock { .. }));
    assert!(mms_stmt.is_some(), "should have MMS block in module");
}
