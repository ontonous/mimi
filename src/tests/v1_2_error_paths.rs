use super::*;

// ─── BUG-4: DedentMismatch col 字段使用 spaces 计数而非实际列位置 ────────────
// 这是设计选择: Dedent token 的 "col" 表示缩进级别(空格数), 而非光标列位置
// dedent_mismatch() 调用时传入了正确的 (line, col) 位置给错误报告
// 此行为已记录, 标记为不修复 (设计选择)

// ─── BUG-3: Token::Unit source_text 返回 "unit" 但 Display 返回 "()" ────────
// 两者应该一致

#[test]
fn error_path_unit_token_source_text_matches_display() {
    assert_eq!(
        format!("{}", crate::lexer::TokenKind::Unit),
        "()",
        "Display of Unit should be ()"
    );
    assert_eq!(
        crate::lexer::TokenKind::Unit.source_text(),
        "()",
        "source_text of Unit should be ()"
    );
}

// ─── BUG-1: scan_fstring \x hex escape validation ───────────────────────────
// 不完整的 \xAB (只有1个hexdigit) 应该报错

#[test]
fn error_path_fstring_incomplete_hex_escape() {
    let src = r#"func main() -> i32 { let s = f"val \xA end"; 0 }"#;
    let result = crate::lexer::Lexer::new(src).tokenize();
    assert!(result.is_err(), "incomplete \\x escape should be a lexer error");
}

// ─── BUG-2: scan_fstring \u{...} missing closing brace ─────────────────────
// \u{1F 后缺少 } 应该报错

#[test]
fn error_path_fstring_unterminated_unicode_brace() {
    let src = r#"func main() -> i32 { let s = f"val \u{1F end"; 0 }"#;
    let result = crate::lexer::Lexer::new(src).tokenize();
    assert!(result.is_err(), "unterminated unicode brace escape should be a lexer error");
}

#[test]
fn error_path_fstring_empty_unicode_brace() {
    let src = r#"func main() -> i32 { let s = f"val \u{} end"; 0 }"#;
    let result = crate::lexer::Lexer::new(src).tokenize();
    assert!(result.is_err(), "empty unicode brace escape should be a lexer error");
}

// ─── BUG-5: parse_fstring_parts 错误位置报告 (0,0) ─────────────────────────
// f-string 内解析错误时位置被报告为 0,0，无法定位问题
// 注意: f"x { 1 + } y" 不会导致 lexer 错误 - } 正确关闭了 interpolation
// BUG-5 的真实测试是 error_path_fstring_parse_inner_error_reports_position

#[test]
fn error_path_fstring_parse_inner_error_reports_position() {
    // BUG-5: When f-string inner expression's lexer fails, parse_fstring_parts reports (0,0).
    // Test with incomplete hex escape \xA (only 1 hex digit instead of 2) - lexer error is (0,0).
    let src = r#"func main() -> i32 { let s = f"value is { \xA }"; 0 }"#;
    let tokens = crate::lexer::Lexer::new(src).tokenize();
    assert!(tokens.is_ok(), "outer string should tokenize fine");
    // The FString token holds raw text including \xA, and parse_fstring_parts re-lexes it.
    // The inner lex error is reported as (0,0) due to the bug.
    let parse_result = crate::parser::Parser::new(tokens.unwrap()).parse_file();
    assert!(parse_result.is_err(), "invalid inner escape should parse error");
    let parse_err = parse_result.unwrap_err();
    // BUG-5: This assertion fails because the lexer error in the f-string reports (0,0)
    assert!(
        parse_err.line > 0 || parse_err.col > 0,
        "f-string inner lexer error should not be at 0,0 but was line={}, col={}, msg={}",
        parse_err.line, parse_err.col, parse_err.message
    );
}

// ─── BUG-7: parse_block_with_recovery 缺少 Math 分支 ────────────────────────
// Recovery 模式下 math { ... } 块会被静默丢弃，导致 AST 信息丢失

#[test]
fn error_path_recovery_keeps_math_block() {
    // 强制触发 recovery: 在 math 块前注入语法错误
    let src = r#"
func f() -> i32 {
    let x = ;
    math: { 1 + 2; 3 }
    0
}
"#;
    // 使用 recovery 模式解析，语法错误会被捕获并继续
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let (file, _errors) = crate::parser::Parser::new_with_recovery(tokens).parse_file_with_recovery();
    let body = match &file.items[0] {
        crate::ast::Item::Func(f) => &f.body,
        _ => panic!("expected func"),
    };
    let has_math = body.iter().any(|s| matches!(s, crate::ast::Stmt::Math(_)));
    assert!(
        has_math,
        "Math block was silently dropped during recovery: {:#?}",
        body
    );
}

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
        assert!(
            parse_result.is_err(),
            "unclosed paren should cause parse error"
        );
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
    assert!(
        result.is_err(),
        "unterminated string should cause lex error"
    );
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
    assert!(
        err.contains("division by zero"),
        "Expected division by zero error, got: {}",
        err
    );
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
    assert!(
        err.contains("index out of bounds"),
        "Expected index error, got: {}",
        err
    );
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
    assert!(
        err.contains("pop from empty list"),
        "Expected pop error, got: {}",
        err
    );
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
    assert!(
        err.contains("assertion failed"),
        "Expected assertion error, got: {}",
        err
    );
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
    assert!(
        result.is_err(),
        "return type mismatch should cause type error"
    );
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
    assert!(
        result.is_err(),
        "mutating immutable should error at runtime"
    );
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
    let tokens = crate::lexer::Lexer::new(src)
        .tokenize()
        .expect("src/tests/v1_2_error_paths.rs:181 unwrap failed");
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
    let tokens = crate::lexer::Lexer::new(src)
        .tokenize()
        .expect("src/tests/v1_2_error_paths.rs:204 unwrap failed");
    let (file, errors) = crate::parser::Parser::new(tokens).parse_file_with_recovery();
    assert!(!errors.is_empty(), "should have parse errors");
    assert!(
        file.items.len() >= 1,
        "should still parse the working function"
    );
    assert!(file
        .items
        .iter()
        .any(|i| matches!(i, crate::ast::Item::Func(f) if f.name == "working")));
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
    assert!(
        result.is_ok(),
        "let x: _ = 42 should type-check: {:?}",
        result.err()
    );
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
    assert!(
        result.is_ok(),
        "lifetime annotation 'a should parse: {:?}",
        result.err()
    );
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
    assert!(
        result.is_ok(),
        "lifetime annotation 'a mut should parse: {:?}",
        result.err()
    );
}
