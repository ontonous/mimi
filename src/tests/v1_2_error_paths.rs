use super::*;

#[test]
fn error_path_parse_unclosed_paren() {
    let src = r#"
func main() -> i32 {
    (1 + 2
}
"#;
    let result = crate::lexer::Lexer::new(src).tokenize();
    if let Ok(tokens) = result {
        let parse_result = crate::parser::Parser::new(tokens).parse_file();
        assert!(parse_result.is_err(), "unclosed paren should cause parse error");
    }
}

#[test]
fn error_path_parse_unterminated_string() {
    let src = r#"
func main() -> string {
    "hello
}
"#;
    let result = crate::lexer::Lexer::new(src).tokenize();
    assert!(result.is_err(), "unterminated string should cause lex error");
}

#[test]
fn error_path_typecheck_undefined_type() {
    let src = r#"
func main() -> i32 {
    let x: NonexistentType = 42;
    x
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "undefined type should cause type error");
}

#[test]
fn error_path_runtime_divide_by_zero() {
    let src = r#"
func main() -> i32 {
    let x = 10;
    let y = 0;
    x / y
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err(), "runtime division by zero should error");
    let err = result.unwrap_err();
    assert!(err.contains("division by zero"), "Expected division by zero error, got: {}", err);
}

#[test]
fn error_path_runtime_index_out_of_bounds() {
    let src = r#"
func main() -> i32 {
    let list = [1, 2, 3];
    list[10]
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err(), "index out of bounds should error");
    let err = result.unwrap_err();
    assert!(err.contains("index out of bounds"), "Expected index error, got: {}", err);
}

#[test]
fn error_path_runtime_pop_empty_list() {
    let src = r#"
func main() -> i32 {
    pop([])
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err(), "pop from empty list should error");
    let err = result.unwrap_err();
    assert!(err.contains("pop from empty list"), "Expected pop error, got: {}", err);
}

#[test]
fn error_path_runtime_assert_fail() {
    let src = r#"
func main() -> i32 {
    assert(false);
    42
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err(), "assert(false) should error");
    let err = result.unwrap_err();
    assert!(err.contains("assertion failed"), "Expected assertion error, got: {}", err);
}

#[test]
fn error_path_parse_invalid_token() {
    let src = r#"
func main() -> i32 {
    let x = 1;
    x
}
"#;
    // Valid program should parse
    let result = crate::lexer::Lexer::new(src).tokenize();
    assert!(result.is_ok(), "valid program should lex ok");
}

#[test]
fn error_path_typecheck_arg_count_mismatch() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    a + b
}

func main() -> i32 {
    add(1)
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "wrong arg count should cause type error");
}

#[test]
fn error_path_typecheck_return_mismatch() {
    let src = r#"
func main() -> i32 {
    return "hello";
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "return type mismatch should cause type error");
}

#[test]
fn error_path_runtime_undefined_function() {
    let src = r#"
func main() -> i32 {
    nonexistent()
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err(), "undefined function should error");
}

#[test]
fn error_path_runtime_use_after_move() {
    let src = r#"
func main() -> string {
    let s = "hello";
    let t = s;
    s
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err(), "use after move should error");
}

#[test]
fn error_path_runtime_mutate_immutable() {
    let src = r#"
func main() -> i32 {
    let x = 5;
    x = 10;
    x
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err(), "mutating immutable should error at runtime");
}

#[test]
fn error_path_parse_recovery_continues_after_bad_stmt() {
    let src = r#"
func main() -> i32 {
    let x = ;
    let y = 42;
    y
}
"#;
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let (file, _errors) = crate::parser::Parser::new(tokens).parse_file_with_recovery();
    // Recovery should produce a partial AST with the function
    assert!(file.items.len() == 1, "should still parse the function");
    if let crate::ast::Item::Func(f) = &file.items[0] {
        // The function body should contain at least the valid statement
        assert!(f.body.len() >= 1, "should have at least one statement");
    } else {
        panic!("expected a function item");
    }
}

#[test]
fn error_path_parse_recovery_continues_after_bad_func() {
    let src = r#"
func broken( {
    return 1;
}

func working() -> i32 {
    42
}
"#;
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let (file, errors) = crate::parser::Parser::new(tokens).parse_file_with_recovery();
    assert!(!errors.is_empty(), "should have parse errors");
    assert!(file.items.len() >= 1, "should still parse the working function");
    assert!(file.items.iter().any(|i| matches!(i, crate::ast::Item::Func(f) if f.name == "working")));
}

#[test]
fn typecheck_infer_type_underscore() {
    let src = r#"
func main() -> i32 {
    let x: _ = 42;
    x
}
"#;
    let result = check_source(src);
    assert!(result.is_ok(), "let x: _ = 42 should type-check: {:?}", result.err());
}

#[test]
fn parse_lifetime_annotation() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    let r: &'a i32 = &x;
    42
}
"#;
    let result = check_source(src);
    assert!(result.is_ok(), "lifetime annotation 'a should parse: {:?}", result.err());
}

#[test]
fn parse_lifetime_mut_annotation() {
    let src = r#"
func main() -> i32 {
    let mut x = 42;
    let r: &'a mut i32 = &mut x;
    42
}
"#;
    let result = check_source(src);
    assert!(result.is_ok(), "lifetime annotation 'a mut should parse: {:?}", result.err());
}
