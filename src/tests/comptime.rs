use super::*;

#[test]
fn nothing_type_parsing() {
    let src = r#"
func diverge() -> nothing {
    assert(false)
}

func main() -> i32 {
    1
}
"#;
    let _file = parse(src);
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn quote_syntax_removed_at_parser() {
    let src = r#"func main() { let ast = quote! { 42 } }"#;
    let tokens = lexer::Lexer::new(src)
        .tokenize()
        .expect("lex quote! source");
    let err = parser::Parser::new(tokens)
        .parse_file()
        .expect_err("quote! syntax must be rejected after Phase E removal");
    assert!(
        err.to_string().contains("removed"),
        "unexpected quote! error: {err}"
    );
}

#[test]
fn quote_interpolation_removed_at_parser() {
    let src = r#"func main() -> i32 { 1 + $(2) }"#;
    let tokens = lexer::Lexer::new(src)
        .tokenize()
        .expect("lex quote interpolation source");
    let err = parser::Parser::new(tokens)
        .parse_file()
        .expect_err("quote interpolation must be rejected after Phase E removal");
    assert!(
        err.to_string().contains("removed"),
        "unexpected quote interpolation error: {err}"
    );
}

#[test]
fn quote_is_ordinary_identifier_now() {
    let src = r#"func main() -> i32 { let quote = 7; quote }"#;
    assert_eq!(run_source(src), interp::Value::Int(7));
}

#[test]
fn math_boolean_arithmetic_is_erased() {
    let src = r#"
func main() -> i32 {
    math: {
        1 + 2 == 3;
        3 * 4 == 12;
    }
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn math_with_variables() {
    let src = r#"
func main() -> i32 {
    let x = 5;
    math: {
        x + 1 == 6;
    }
    x * 2
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(10));
}

#[test]
fn math_boolean_expressions() {
    let src = r#"
func main() -> bool {
    math: {
        1 < 2;
        3 > 2;
        1 == 1;
    }
    true
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Bool(true));
}

#[test]
fn math_empty_block() {
    let src = r#"
func main() -> i32 {
    math: {
    }
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn math_with_division() {
    let src = r#"
func main() -> i32 {
    math: {
        10 / 2 == 5;
        100 / 10 == 10;
    }
    5
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn math_with_negative_numbers() {
    let src = r#"
func main() -> i32 {
    math: {
        -1 + 1 == 0;
        -5 * -3 == 15;
    }
    15
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(15));
}

// ===================== Comptime Function Tests =====================

#[test]
fn comptime_function_evaluation() {
    let src = r#"
comptime func get_magic_number() -> i32 {
    42
}

func main() -> i32 {
    get_magic_number()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn comptime_function_used_in_runtime() {
    let src = r#"
comptime func get_size() -> i32 {
    10
}

func main() -> i32 {
    let size = get_size()
    size * 2
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(20));
}

#[test]
fn comptime_function_with_computation() {
    let src = r#"
comptime func compute() -> i32 {
    let x = 5
    let y = 10
    x + y
}

func main() -> i32 {
    compute()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(15));
}

// ===================== P2-4: comptime + contracts =====================

#[test]
fn comptime_function_checked_at_runtime() {
    // comptime 函数调用通过 call_func()，所以 verify_contracts 会检查合约。
    // ensures: result > 0 但返回 0 → 运行时合约失败。
    let src = r#"
comptime func get_value() -> i32 {
    ensures: result > 0
    0
}

func main() -> i32 {
    get_value()
}
"#;
    // run_source uses default verify_contracts=true, so contract violation is caught
    let result = run_source_bytecode_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("ensures"),
        "error should mention ensures: {}",
        err
    );
}

#[test]
fn comptime_generated_closure_no_contracts() {
    // comptime 通过 quote! 生成的闭包不含合约（quote.rs:40 排除 Stmt::Ensures）。
    // In the bytecode VM, comptime functions are folded at compile time,
    // so their runtime contracts (ensures) are never checked at runtime.
    // The closure returned by make_adder() is inlined directly.
    let src = r#"
comptime func make_adder() -> func(i32) -> i32 {
    ensures: result > 0
    fn(x: i32) -> i32 { x + 1 }
}

func main() -> i32 {
    let f = make_adder()
    f(0)
}
"#;
    // Bytecode VM: comptime func is folded at compile time → ensures not checked at runtime.
    // The program succeeds: f(0) = 1.
    let result = run_source_bytecode_result(src);
    assert!(
        result.is_ok(),
        "comptime fold bypasses runtime contract: {:?}",
        result
    );
}

#[test]
fn comptime_contract_checked_at_call_site() {
    // comptime 函数的合约在调用时检查（通过 call_func）。
    // 如果 ensures 被满足，函数正常返回。
    let src = r#"
comptime func get_positive() -> i32 {
    ensures: result > 0
    42
}

func main() -> i32 {
    get_positive()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn comptime_requires_on_comptime_func() {
    let src = r#"
comptime func validate(n: i32) -> i32 {
    requires: n > 0
    n * 2
}

func main() -> i32 {
    validate(5)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(10));
}

#[test]
fn comptime_requires_fails_on_comptime_func() {
    let src = r#"
comptime func validate(n: i32) -> i32 {
    requires: n > 0
    n * 2
}

func main() -> i32 {
    validate(-1)
}
"#;
    let result = run_source_bytecode_result(src);
    assert!(result.is_err());
}

#[test]
fn math_block_and_comptime_interaction() {
    let src = r#"
comptime func get_val() -> i32 {
    50
}

func main() -> i32 {
    math: {
        get_val();
    }
    get_val() + 10
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(60));
}

#[test]
fn math_block_contract_cross_check() {
    let src = r#"
func safe_div(a: i32, b: i32) -> i32 {
    requires: b != 0
    ensures: result == a / b
    a / b
}

func main() -> i32 {
    math: {
        safe_div(10, 2);
    }
    42
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn comptime_zero_arg_not_double_executed() {
    // I-H9: zero-arg comptime must run once (cache), not again on call.
    let src = r#"
comptime func seed() -> i32 { 42 }
func main() -> i32 {
    seed() + seed()
}
"#;
    assert_eq!(run_source(src), crate::interp::Value::Int(84));
}
