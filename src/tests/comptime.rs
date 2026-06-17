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
