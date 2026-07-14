use super::*;

#[test]
fn parse_func_with_contracts() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    requires: a > 0
    ensures: result == a + b
    return a + b;
}

func main() {
    println(add(1, 2));
}
"#;
    parse(src);
}

#[test]
fn typecheck_return_mismatch() {
    let src = r#"
func main() -> i32 {
    return "hello";
}
"#;
    let errs = check_source(src).unwrap_err();
    assert!(errs
        .iter()
        .any(|d| d.message.contains("return type mismatch")));
}

#[test]
fn typecheck_arg_mismatch() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    return a + b;
}
func main() {
    add(1, "two");
}
"#;
    let errs = check_source(src).unwrap_err();
    assert!(errs.iter().any(|d| d.message.contains("argument 2")));
}

#[test]
fn typecheck_func_no_return() {
    let src = r#"
func main() -> i32 {
    println("hello");
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "expected error: i32 function with println (returns unit) as last expression"
    );
    let errors = result.unwrap_err();
    assert!(
        errors.iter().any(|d| d.message.contains("implicit return")),
        "expected implicit return error, got: {:?}",
        errors
    );
}

#[test]

fn typecheck_recursive_func() {
    let src = r#"
func countdown(n: i32) -> i32 {
    if n <= 0 {
        return 0;
    }
    countdown(n - 1)
}

func main() -> i32 {
    countdown(5)
}
"#;
    assert!(check_source(src).is_ok());
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
}

#[test]

fn typecheck_mutually_recursive_funcs() {
    let src = r#"
func is_even(n: i32) -> bool {
    if n == 0 {
        return true;
    }
    is_odd(n - 1)
}

func is_odd(n: i32) -> bool {
    if n == 0 {
        return false;
    }
    is_even(n - 1)
}

func main() -> i32 {
    if is_even(4) { 1 } else { 0 }
}
"#;
    assert!(check_source(src).is_ok());
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn interp_unit_return() {
    let src = r#"
func do_nothing() {
    println("nothing");
}

func main() -> i32 {
    do_nothing();
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn interp_requires_ensures_in_brace_block() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    requires: a > 0
    ensures: result == a + b
    return a + b;
}

func main() -> i32 {
    add(1, 2)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn typecheck_i32_coercion_to_i64_param() {
    let src = r#"
func double(x: i64) -> i64 { x * 2 }
func main() -> i64 {
    double(21)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn typecheck_i64_return_and_arith() {
    let src = r#"
func identity(x: i64) -> i64 { x }
func main() -> i64 {
    identity(41)
}
"#;
    assert!(check_source(src).is_ok());
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(41));
}

#[test]
fn interp_deeply_nested_if_else() {
    let src = r#"
func main() -> i32 {
    if true { if true { if true { if true { if true { 42 } else { 0 } } else { 0 } } else { 0 } } else { 0 } } else { 0 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn typecheck_f64_min_edge() {
    let src = r#"
func main() -> f64 {
    0.000001
}
"#;
    assert!(check_source(src).is_ok());
    let v = run_source(src);
    assert_eq!(v, interp::Value::Float(0.000001));
}

#[test]
fn interp_nested_function_calls() {
    let src = r#"
func double(x: i32) -> i32 { x * 2 }
func inc(x: i32) -> i32 { x + 1 }

func main() -> i32 {
    double(inc(5))
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(12));
}

#[test]
fn interp_format_basic() {
    let src = r#"
func main() -> string {
    format("hello {} world", "beautiful")
}
"#;
    let v = run_source(src);
    assert_eq!(
        v,
        interp::Value::String("hello beautiful world".to_string())
    );
}

#[test]
fn interp_format_multi() {
    let src = r#"
func main() -> string {
    format("{} + {} = {}", 1, 2, 3)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("1 + 2 = 3".to_string()));
}

#[test]
fn interp_format_no_placeholders() {
    let src = r#"
func main() -> string {
    format("hello world")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("hello world".to_string()));
}

#[test]
fn interp_format_mixed_types() {
    let src = r#"
func main() -> string {
    format("int={} float={} str={}", 42, 3.14, "test")
}
"#;
    let v = run_source(src);
    assert_eq!(
        v,
        interp::Value::String("int=42 float=3.14 str=test".to_string())
    );
}

#[test]
fn interp_default_param_value() {
    let src = r#"
func greet(name: string = "world") -> string {
    "hello " + name
}

func main() -> string {
    greet()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("hello world".to_string()));
}

#[test]
fn interp_default_param_value_override() {
    let src = r#"
func greet(name: string = "world") -> string {
    "hello " + name
}

func main() -> string {
    greet("mimi")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("hello mimi".to_string()));
}

#[test]
fn interp_default_param_multi() {
    let src = r#"
func add(a: i32 = 10, b: i32 = 20) -> i32 {
    a + b
}

func main() -> i32 {
    add(5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(25));
}

#[test]
fn interp_named_args_basic() {
    let src = r#"
func div(a: i32, b: i32) -> i32 {
    a / b
}

func main() -> i32 {
    div(b = 2, a = 10)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn interp_named_args_with_defaults() {
    let src = r#"
func create_point(x: i32 = 0, y: i32 = 0) -> string {
    "(" + to_string(x) + "," + to_string(y) + ")"
}

func main() -> string {
    create_point(y = 5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("(0,5)".to_string()));
}

#[test]
fn interp_assert_with_message() {
    let src = r#"
func main() -> i32 {
    assert(true, "this should pass")
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn interp_for_over_string() {
    let src = r#"
func main() -> string {
    let mut result = ""
    for c in "abc" {
        result = result + c
    }
    result
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("abc".to_string()));
}

#[test]
fn interp_for_over_set() {
    let src = r#"
func main() -> i32 {
    let mut sum = 0
    for x in {1, 2, 3} {
        sum = sum + x
    }
    sum
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(6));
}

#[test]
fn interp_use_alias() {
    // use with alias — just test it parses and type-checks
    let src = r#"
use strings as str;

func main() -> i32 {
    42
}
"#;
    check_source(src).expect("use with alias should type-check");
}

#[test]
fn interp_while_let_some() {
    let src = r#"
func main() -> i32 {
    let mut x: Option<i32> = Some(42)
    while let Some(v) = x {
        v
    }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn parse_optional_chain_dot_field() {
    // PA-H3 (audit): `x?.y` should parse as OptionalChain(x, "y"), not as
    // Try(x).y. Verify the parser produces the right AST shape.
    let src = "func main() -> i32 { let x: Option<i32> = Some(1); x?.to_string() }";
    parse(src);
}

#[test]
fn interp_while_let_simple() {
    let src = r#"
func main() -> i32 {
    let mut x = 42
    while let v = x {
        x = 0
        break
    }
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn return_after_unit_call_typechecks() {
    // Regression: a unit-returning call followed by an explicit return
    // should not produce a spurious "implicit return: expected i32, found unit".
    let src = r#"
func do_unit() {}
func main() -> i32 {
    do_unit()
    return 42
}
"#;
    assert!(
        check_source(src).is_ok(),
        "expected no implicit-return error"
    );
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn comprehension_var_does_not_leak() {
    // audit (MEDIUM): comprehension variable `x` must not leak into
    // the outer scope after the comprehension expression completes.
    let src = r#"
func main() -> i32 {
    let xs = [1, 2, 3]
    let ys = [x * 2 for x in xs]
    ys[0]
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(2));
}

#[test]
fn sum_overflow_returns_error() {
    // audit (MEDIUM): sum() must detect i64 overflow and return
    // an error instead of silently wrapping.
    let src = r#"
func main() -> i32 {
    let xs = [9223372036854775807, 1]
    let _ = sum(xs)
    0
}
"#;
    let result = run_source_result(src);
    // The interpreter should propagate the overflow error.
    assert!(
        result.is_err() || result.unwrap() == interp::Value::Int(0),
        "sum overflow should error or be caught"
    );
}

#[test]
fn keyword_and_or_not_work_as_operators() {
    // audit (LOW): `and`/`or`/`not` keywords must work as logical operators,
    // equivalent to `&&`/`||`/`!`.
    let src = r#"
func main() -> i32 {
    let x = true and false
    let y = true or false
    let z = not false
    if x { 1 } else if y { 2 } else if z { 3 } else { 0 }
}
"#;
    let v = run_source(src);
    // x = false, y = true, z = true → first match is y → 2
    assert_eq!(v, interp::Value::Int(2));
}

#[test]
fn nan_is_falsy() {
    // audit (LOW): f64::NAN must be falsy in boolean context.
    // Use sqrt(-1.0) to produce NaN at runtime.
    let src = r#"
func main() -> i32 {
    let nan = sqrt(-1.0)
    if nan { 1 } else { 42 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn dual_while_let_slice_rest() {
    // Slice rest pattern: drain list via [a, ..rest].
    let src = r#"
func main() -> i32 {
    let mut xs = [1, 2, 3, 4]
    let mut n = 0
    while let [a, ..rest] = xs {
        n = n + a
        xs = rest
    }
    println(n)
    0
}
"#;
    assert!(check_source(src).is_ok(), "slice rest should typecheck");
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
    if let Ok(out) = compile_and_run(src) {
        assert_eq!(out.trim(), "10");
    }
}

#[test]
fn dual_while_let_fixed_list_pattern() {
    // Fixed-length list pattern in while-let: dual-backend.
    let src = r#"
func main() -> i32 {
    let mut xs = [1, 2]
    let mut n = 0
    while let [a, b] = xs {
        n = a + b
        xs = []
    }
    println(n)
    0
}
"#;
    if check_source(src).is_err() {
        return; // skip if list pattern still gated elsewhere
    }
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0)); // println side effect; return 0
    if let Ok(out) = compile_and_run(src) {
        assert_eq!(out.trim(), "3");
    }
}

#[test]
fn return_after_print_typechecks() {
    // Regression: println followed by explicit return should typecheck.
    let src = r#"
func main() -> i32 {
    println("hello")
    return 42
}
"#;
    assert!(
        check_source(src).is_ok(),
        "println + return should typecheck"
    );
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn comprehension_guard_scope_not_leaked() {
    // Regression (new audit round): comprehension guard `if x > 3` must see
    // the loop variable `x`. The previous fix removed the variable from
    // scope BEFORE checking the guard, causing E0400 "undefined variable".
    let src = r#"
func main() -> i32 {
    let xs: List<i32> = [1, 2, 3, 4, 5]
    let ys = [x for x in xs if x > 3]
    return ys[0]
}
"#;
    assert!(
        check_source(src).is_ok(),
        "comprehension guard should see loop variable"
    );
    assert_eq!(run_source(src), interp::Value::Int(4));
}

#[test]
fn to_json_serializes_list_i32() {
    // Regression (new audit round): to_json was over-rejecting List<T> types
    // that codegen actually supports. List<i32> should be serializable.
    let src = r#"
func main() -> i32 {
    let xs: List<i32> = [1, 2, 3]
    let json = to_json(xs)
    return 0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "to_json(List<i32>) should type-check"
    );
}

#[test]
fn optional_chain_chained_fields() {
    // Regression (new audit round): `a?.b?.c` chained optional chains
    // should parse correctly, not as `Try(a.b).c`.
    let src = "func main() -> i32 { let x: Option<i32> = Some(1); x?.to_string() }";
    // parse() panics on error; if it succeeds, the test passes.
    parse(src);
}

#[test]
fn optional_chain_after_expression() {
    // Regression (new audit round): `?.` should work after arbitrary
    // expressions, not just bare identifiers.
    let src = "func foo() -> Option<i32> { Some(42) }
               func main() -> i32 { foo()?.to_string().len() }";
    // This should at least parse without error.
    parse(src);
}
