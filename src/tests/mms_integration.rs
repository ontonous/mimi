use super::*;
use crate::ast::Item;

// 0.1.8 Phase E: the `mms{}` sketch container is removed. The parser must
// reject it in every statement context instead of silently consuming it as
// trivia. Standalone `desc:`/`rule:` trivia are unaffected.

fn assert_mms_rejected(src: &str) {
    let tokens = crate::lexer::Lexer::new(src)
        .tokenize()
        .expect("mms rejection test lex failed");
    let result = crate::parser::Parser::new(tokens).parse_file();
    let err = match result {
        Ok(_) => panic!("`mms{{}}` must be rejected, but parse succeeded"),
        Err(e) => e,
    };
    assert!(
        err.message.contains("mms") || err.message.contains("removed"),
        "expected mms-removal diagnostic, got: {}",
        err.message
    );
}

#[test]
fn mms_block_rejected_at_parser() {
    let src = r#"
        func add(a: i32, b: i32) -> i32 {
            mms {
                some content here
            }
            a + b
        }
    "#;
    assert_mms_rejected(src);
}

#[test]
fn mms_block_empty_body_rejected() {
    let src = r#"
        func main() {
            mms {
                hello world
            }
        }
    "#;
    assert_mms_rejected(src);
}

#[test]
fn mms_block_multiple_rejected() {
    let src = r#"
        func main() {
            mms { first block }
            mms { second block }
        }
    "#;
    assert_mms_rejected(src);
}

#[test]
fn mms_block_contract_shaped_content_rejected() {
    // Contract-shaped mms content was always inert; now the container itself
    // is rejected so no one mistakes it for a runnable contract surface.
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
    assert_mms_rejected(src);
}

#[test]
fn desc_rule_statements_consumed_as_trivia() {
    // Standalone desc/rule statements (outside mms{}) remain trivia —
    // the parser validates and discards them inside blocks.
    let src = r#"
        func main() -> i32 {
            desc "Process the order"
            rule "must validate inputs"
            42
        }
    "#;
    let file = parse(src);
    let body = file
        .items
        .iter()
        .find_map(|item| {
            if let Item::Func(f) = item {
                Some(f.body.clone())
            } else {
                None
            }
        })
        .expect("expected a function item");
    assert_eq!(body.len(), 1, "desc/rule must not enter the AST");
}
