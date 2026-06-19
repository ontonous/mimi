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
fn quote_basic_literal() {
    let src = r#"
func main() {
    let ast = quote! { 42 };
    println(ast);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn quote_interpolation() {
    let src = r#"
func main() {
    let x = 10;
    let ast = quote! { $(x + 1) };
    println(ast);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn quote_let_statement() {
    let src = r#"
func main() {
    let ast = quote! { let y = 5; };
    println(ast);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn quote_dump() {
    let src = r#"
func main() {
    let ast = quote! { 42 };
    let dumped = ast_dump(ast);
    println(dumped);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn quote_eval_literal() {
    let src = r#"
func main() -> i32 {
    let ast = quote! { 42 };
    ast_eval(ast)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn quote_eval_binary() {
    let src = r#"
func main() -> i32 {
    let ast = quote! { 10 + 20 };
    ast_eval(ast)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(30));
}

#[test]
fn quote_eval_interpolation() {
    let src = r#"
func main() -> i32 {
    let x = 5;
    let ast = quote! { $(x * 3) };
    ast_eval(ast)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(15));
}

#[test]
fn quote_eval_block() {
    let src = r#"
func main() -> i32 {
    let ast = quote! {
        let a = 10;
        let b = 20;
        a + b
    };
    ast_eval(ast)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(30));
}

#[test]
fn quote_eval_string_concat() {
    let src = r#"
func main() {
    let ast = quote! { "hello" + " " + "world" };
    let result = ast_eval(ast);
    println(result);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn quote_nested_interpolation() {
    let src = r#"
func main() -> i32 {
    let a = 3;
    let b = 4;
    let ast = quote! { $(a + b) };
    ast_eval(ast)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(7));
}

#[test]
fn math_constant_evaluation() {
    let src = r#"
func main() -> i32 {
    math: {
        1 + 2;
        3 * 4;
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
        x + 1;
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
        10 / 2;
        100 / 10;
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
        -1 + 1;
        -5 * -3;
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
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("ensures"), "error should mention ensures: {}", err);
}

#[test]
fn comptime_generated_closure_no_contracts() {
    // comptime 通过 quote! 生成的闭包不含合约（quote.rs:40 排除 Stmt::Ensures）。
    // eval_quoted_ast() 不经过 call_func()，所以合约检查被绕过。
    // 即使原始模板有 ensures，生成的闭包调用不触发合约检查。
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
    // make_adder() itself goes through call_func → catches ensures violation.
    // But f(0) calls the generated closure via eval_quoted_ast → no contract check.
    let result = run_source_result(src);
    // make_adder() has ensures: result > 0 but returns a closure (not an i32)
    // This will fail at contract check time
    assert!(result.is_err());
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
    let result = run_source_result(src);
    assert!(result.is_err());
}

#[test]
fn comptime_quote_with_contract_interaction() {
    let src = r#"
func main() -> i32 {
    let ast = quote! { 42 };
    ast_eval(ast)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn comptime_quote_eval_with_nested_interp() {
    let src = r#"
func main() -> i32 {
    let x = 5;
    let ast = quote! { $(x + 10) };
    ast_eval(ast)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(15));
}

#[test]
fn comptime_quote_eval_block_with_contract() {
    let src = r#"
func compute(x: i32) -> i32 {
    requires: x > 0
    x * 2
}

func main() -> i32 {
    let ast = quote! { compute(5) };
    ast_eval(ast)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(10));
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
