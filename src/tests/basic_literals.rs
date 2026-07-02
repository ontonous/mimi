use super::*;

#[test]
fn interp_string_equality() {
    let src = r#"
func main() -> i32 {
    let a = "hello";
    let b = "hello";
    if a == b { 1 } else { 0 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn interp_string_index() {
    let src = r#"
func main() -> string {
    let s = "abc";
    s[1]
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("b".to_string()));
}

#[test]
fn lexer_skips_shebang() {
    let src = "#!/usr/bin/env mimi\nfunc main() -> i32 { 42 }\n";
    let tokens = lexer::Lexer::new(src).tokenize().unwrap();
    assert!(
        tokens
            .iter()
            .all(|t| !matches!(t.kind, lexer::TokenKind::Hash | lexer::TokenKind::Bang)),
        "shebang line should not produce tokens"
    );
    assert!(tokens
        .iter()
        .any(|t| matches!(t.kind, lexer::TokenKind::Func)));
}
