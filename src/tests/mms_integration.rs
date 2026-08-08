use super::*;
use crate::ast::Item;

// 0.35.13 (DX backlog #10 trivia-ization): `mms {}` blocks are consumed by
// the parser as trivia — validated for brace structure but never entering
// the AST (Stmt 33→30). These tests lock the new contract:
//   1. mms{}-bearing sources still parse without errors;
//   2. the mms block contributes ZERO statements to the enclosing body;
//   3. runtime semantics of the surrounding code are unaffected.
// Pre-0.35.13 this file asserted `Stmt::MmsBlock` presence and verbatim
// content preservation; that surface no longer exists.

fn first_func_body(src: &str) -> crate::ast::Block {
    let file = parse(src);
    file.items
        .iter()
        .find_map(|item| {
            if let Item::Func(f) = item {
                Some(f.body.clone())
            } else {
                None
            }
        })
        .expect("expected a function item")
}

#[test]
fn mms_block_consumed_as_trivia() {
    let src = r#"
        func add(a: i32, b: i32) -> i32 {
            mms {
                some content here
            }
            a + b
        }
    "#;
    let body = first_func_body(src);
    assert_eq!(body.len(), 1, "mms{{}} must not enter the AST");
}

#[test]
fn mms_block_empty_body_after_trivia() {
    let src = r#"
        func main() {
            mms {
                hello world
            }
        }
    "#;
    let body = first_func_body(src);
    assert!(body.is_empty(), "only mms{{}} ⇒ empty function body");
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
fn mms_block_multiple_all_consumed() {
    let src = r#"
        func main() {
            mms { first block }
            mms { second block }
        }
    "#;
    let body = first_func_body(src);
    assert!(body.is_empty(), "both mms{{}} blocks must be trivia");
}

#[test]
fn mms_block_in_module_consumed() {
    let src = r#"
        module Math {
            func add(a: i32, b: i32) -> i32 {
                mms { some content }
                a + b
            }
        }
    "#;
    let file = parse(src);
    let module = file
        .items
        .iter()
        .find_map(|item| {
            if let Item::Module(m) = item {
                Some(m)
            } else {
                None
            }
        })
        .expect("expected a module item");
    let func = module
        .items
        .iter()
        .find_map(|item| {
            if let Item::Func(f) = item {
                Some(f)
            } else {
                None
            }
        })
        .expect("expected a function in the module");
    assert_eq!(func.body.len(), 1, "mms{{}} must be trivia in modules too");
}

#[test]
fn mms_block_contract_shaped_content_consumed() {
    // Contract-shaped mms content (requires:/ensures:) was always inert in
    // .mimi (AGENTS.md §10 super-comment ruling) — after trivia-ization it
    // simply never reaches any tool path.
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
    let body = first_func_body(src);
    assert_eq!(body.len(), 1, "contract-shaped mms{{}} is trivia too");
}

#[test]
fn mms_block_with_desc_rule_consumed() {
    let src = r#"
        func main() {
            mms {
                desc "Process the order"
                rule "must validate inputs"
            }
            42
        }
    "#;
    let body = first_func_body(src);
    assert_eq!(body.len(), 1, "desc/rule inside mms{{}} are trivia");
}

#[test]
fn desc_rule_statements_consumed_as_trivia() {
    // Standalone desc/rule statements (outside mms{}) are trivia too —
    // the parser validates and discards them inside blocks. Grammar is
    // `desc "text"` / `desc { ... }` (no colon).
    let src = r#"
        func main() -> i32 {
            desc "Process the order"
            rule "must validate inputs"
            42
        }
    "#;
    let body = first_func_body(src);
    assert_eq!(body.len(), 1, "desc/rule must not enter the AST");
}
