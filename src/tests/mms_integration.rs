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
    }).expect("src/tests/mms_integration.rs:17 unwrap failed");
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
    }).expect("src/tests/mms_integration.rs:34 unwrap failed");
    let mms_stmt = func.body.iter().find(|s| matches!(s, Stmt::MmsBlock { .. }));
    assert!(mms_stmt.is_some());
    if let Stmt::MmsBlock { content, .. } = mms_stmt.expect("src/tests/mms_integration.rs:37 unwrap failed") {
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
    }).expect("src/tests/mms_integration.rs:67 unwrap failed");
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
    }).expect("src/tests/mms_integration.rs:85 unwrap failed");
    let func = module.items.iter().find_map(|item| {
        if let Item::Func(f) = item { Some(f) } else { None }
    }).expect("src/tests/mms_integration.rs:88 unwrap failed");
    let mms_stmt = func.body.iter().find(|s| matches!(s, Stmt::MmsBlock { .. }));
    assert!(mms_stmt.is_some(), "should have MMS block in module");
}

#[test]
fn mms_block_ast_token_content() {
    let src = r#"
        func main() {
            mms {
                func add(a: i32, b: i32):
                    requires: a > 0
                    ensures: result > 0
            }
            42
        }
    "#;
    let file = parse(src);
    let func = file.items.iter().find_map(|item| {
        if let Item::Func(f) = item { Some(f) } else { None }
    }).expect("src/tests/mms_integration.rs:108 unwrap failed");
    let mms_stmt = func.body.iter().find_map(|s| {
        if let Stmt::MmsBlock { content, ast, .. } = s {
            Some((content.clone(), ast.clone()))
        } else {
            None
        }
    }).expect("src/tests/mms_integration.rs:115 unwrap failed");
    assert!(!mms_stmt.0.is_empty(), "content should not be empty");
    assert!(mms_stmt.0.contains("func add"), "content should contain original text");
    assert!(mms_stmt.0.contains("requires"), "content should contain requires");
    assert!(mms_stmt.1.is_some(), "valid MMS content should produce AST");
}

#[test]
fn mms_block_ast_graceful_degradation() {
    let src = r#"
        func main() {
            mms {
                this is not valid mimispec !@^&*
            }
            42
        }
    "#;
    let file = parse(src);
    let func = file.items.iter().find_map(|item| {
        if let Item::Func(f) = item { Some(f) } else { None }
    }).expect("src/tests/mms_integration.rs:135 unwrap failed");
    let mms_stmt = func.body.iter().find_map(|s| {
        if let Stmt::MmsBlock { content, ast, .. } = s {
            Some((content.clone(), ast.clone()))
        } else {
            None
        }
    }).expect("src/tests/mms_integration.rs:142 unwrap failed");
    assert!(!mms_stmt.0.is_empty(), "content should not be empty");
    assert!(mms_stmt.1.is_none(), "invalid MMS content should degrade to None");
}

#[test]
fn mms_block_ast_with_desc() {
    let src = r#"
        func main() {
            mms {
                desc "Process the order"
                rule "must validate inputs"
            }
            42
        }
    "#;
    let file = parse(src);
    let func = file.items.iter().find_map(|item| {
        if let Item::Func(f) = item { Some(f) } else { None }
    }).expect("src/tests/mms_integration.rs:161 unwrap failed");
    let mms_stmt = func.body.iter().find_map(|s| {
        if let Stmt::MmsBlock { content, ast, .. } = s {
            Some((content.clone(), ast.clone()))
        } else {
            None
        }
    }).expect("src/tests/mms_integration.rs:168 unwrap failed");
    assert!(!mms_stmt.0.is_empty());
    assert!(mms_stmt.1.is_none(), "token-represented content should not produce AST");
}

#[test]
fn mms_block_content_preserved() {
    let src = r#"
        func main() {
            mms {
                func Pay(amount):
                    desc "Process payment"
                    requires: amount > 0
            }
            42
        }
    "#;
    let file = parse(src);
    let func = file.items.iter().find_map(|item| {
        if let Item::Func(f) = item { Some(f) } else { None }
    }).expect("src/tests/mms_integration.rs:188 unwrap failed");
    let mms_stmt = func.body.iter().find_map(|s| {
        if let Stmt::MmsBlock { content, .. } = s {
            Some(content.clone())
        } else {
            None
        }
    }).expect("src/tests/mms_integration.rs:195 unwrap failed");
    assert!(mms_stmt.contains("Pay"), "content should contain function name");
    assert!(mms_stmt.contains("desc"), "content should contain desc keyword");
    assert!(mms_stmt.contains("requires"), "content should contain requires keyword");
}
