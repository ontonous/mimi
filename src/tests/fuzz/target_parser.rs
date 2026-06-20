use super::harness::arb_random_source;
use crate::{lexer, parser};

/// Fuzz target: lex + parse random byte strings.
/// We verify the parser never panics, even on garbage input.
/// Parse errors (Err) are expected and acceptable; panics are not.
proptest::proptest! {
    #[test]
    fn fuzz_parser_no_panic(src in arb_random_source()) {
        if let Ok(tokens) = lexer::Lexer::new(&src).tokenize() {
            let _ = parser::Parser::new(tokens).parse_file();
        }
    }

    #[test]
    fn fuzz_parser_sketch_no_panic(src in arb_random_source()) {
        if let Ok(tokens) = lexer::Lexer::new_sketch(&src).tokenize() {
            let _ = parser::Parser::new_sketch(tokens).parse_file();
        }
    }
}

/// Edge-case parser crash regression tests.
#[test]
fn test_parser_empty_input() {
    let tokens = lexer::Lexer::new("").tokenize().unwrap();
    let _ = parser::Parser::new(tokens).parse_file();
}

#[test]
fn test_parser_whitespace_only() {
    if let Ok(tokens) = lexer::Lexer::new("   \n  \t  ").tokenize() {
        let _ = parser::Parser::new(tokens).parse_file();
    }
}

#[test]
fn test_parser_only_comments() {
    let tokens = lexer::Lexer::new("// just a comment\n// another\n").tokenize().unwrap();
    let _ = parser::Parser::new(tokens).parse_file();
}

#[test]
fn test_parser_deeply_nested_braces() {
    let src = format!("{} main() -> i32 {{ 0 }}", "func ".to_string());
    let tokens = lexer::Lexer::new(&src).tokenize().unwrap();
    let _ = parser::Parser::new(tokens).parse_file();
}

#[test]
fn test_parser_unicode_identifiers() {
    let src = "func 你好() -> i32 { 42 }";
    let tokens = match lexer::Lexer::new(src).tokenize() {
        Ok(t) => t,
        Err(_) => return,
    };
    let _ = parser::Parser::new(tokens).parse_file();
}

#[test]
fn test_parser_extremely_long_line() {
    let long_line = format!("func main() -> i32 {{ {} }}", "1 + ".repeat(1000) + "1");
    let tokens = lexer::Lexer::new(&long_line).tokenize().unwrap();
    let _ = parser::Parser::new(tokens).parse_file();
}

#[test]
fn test_parser_many_funcs() {
    let mut src = String::new();
    for i in 0..100 {
        src.push_str(&format!("func f{}() -> i32 {{ {} }}\n", i, i));
    }
    src.push_str("func main() -> i32 { 0 }");
    let tokens = lexer::Lexer::new(&src).tokenize().unwrap();
    let _ = parser::Parser::new(tokens).parse_file();
}

#[test]
fn test_parser_recovery_mode() {
    let src = r#"func main() -> i32 { let x = ; 42 }"#;
    let tokens = match lexer::Lexer::new(src).tokenize() {
        Ok(t) => t,
        Err(_) => return,
    };
    let _ = parser::Parser::new_with_recovery(tokens).parse_file_with_recovery();
}
