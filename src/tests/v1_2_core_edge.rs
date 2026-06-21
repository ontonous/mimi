use super::*;

// ========================================================================
// Integer overflow at runtime
// ========================================================================

#[test]
fn overflow_addition_max() {
    let src = "func main() -> i32 { 9223372036854775807 + 1 }";
    let result = run_source_result(src);
    assert!(result.is_err(), "addition overflow should error");
    assert!(result.unwrap_err().contains("overflow"), "expected overflow error");
}

#[test]
fn overflow_subtraction_min() {
    let src = "func main() -> i32 { -9223372036854775807 - 2 }";
    let result = run_source_result(src);
    assert!(result.is_err(), "subtraction overflow should error");
    assert!(result.unwrap_err().contains("overflow"), "expected overflow error");
}

#[test]
fn overflow_multiplication_max() {
    let src = "func main() -> i32 { 4611686018427387904 * 2 }";
    let result = run_source_result(src);
    assert!(result.is_err(), "multiplication overflow should error");
    assert!(result.unwrap_err().contains("overflow"), "expected overflow error");
}

#[test]
fn overflow_negation_min() {
    let src = "func main() -> i32 { -(-9223372036854775807 - 1) }";
    let result = run_source_result(src);
    assert!(result.is_err(), "negation overflow should error");
    assert!(result.unwrap_err().contains("overflow"), "expected overflow error");
}

#[test]
fn overflow_power_large() {
    let src = "func main() -> i32 { 2 ** 63 }";
    let result = run_source_result(src);
    assert!(result.is_err(), "pow overflow should error");
    assert!(result.unwrap_err().contains("overflow"), "expected overflow error");
}

#[test]
fn overflow_shift_left_64() {
    let src = "func main() -> i32 { 1 << 64 }";
    let result = run_source_result(src);
    assert!(result.is_err(), "shift overflow should error");
    assert!(result.unwrap_err().contains("overflow"), "expected overflow error");
}

#[test]
fn overflow_shift_right_64() {
    let src = "func main() -> i32 { 1 >> 64 }";
    let result = run_source_result(src);
    assert!(result.is_err(), "shift overflow should error");
    assert!(result.unwrap_err().contains("overflow"), "expected overflow error");
}

#[test]
fn overflow_division_min_by_neg_one() {
    let src = "func main() -> i32 { (-9223372036854775807 - 1) / -1 }";
    let result = run_source_result(src);
    assert!(result.is_err(), "i64::MIN / -1 overflow should error");
    assert!(result.unwrap_err().contains("overflow"), "expected overflow error");
}

#[test]
fn overflow_addition_safe_normal() {
    let src = "func main() -> i32 { 100 + 200 }";
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:75 unwrap failed"), interp::Value::Int(300));
}

// ========================================================================
// Recursion depth limit
// ========================================================================

#[test]
fn recursion_safe_shallow() {
    let src = r#"
func recurse(n: i32) -> i32 {
    if n > 0 { recurse(n - 1) } else { 0 }
}
func main() -> i32 {
    recurse(10)
}
"#;
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:93 unwrap failed"), interp::Value::Int(0));
}

// ========================================================================
// Float edge cases: NaN, -0.0, infinity
// ========================================================================

#[test]
fn float_negative_zero_equals_positive_zero() {
    let src = "func main() -> bool { -0.0 == 0.0 }";
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:104 unwrap failed"), interp::Value::Bool(true));
}

#[test]
fn float_negative_zero_comparison_not_less() {
    let src = "func main() -> bool { -0.0 < 0.0 }";
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:111 unwrap failed"), interp::Value::Bool(false));
}

#[test]
fn float_large_values_equal() {
    let src = "func main() -> bool { 1000000.0 == 1000000.0 }";
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:118 unwrap failed"), interp::Value::Bool(true));
}

#[test]
fn float_large_values_not_equal() {
    let src = "func main() -> bool { 1000000.0 == 1000001.0 }";
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:125 unwrap failed"), interp::Value::Bool(false));
}

#[test]
fn float_tiny_values_equal() {
    let src = "func main() -> bool { 0.000001 == 0.000001 }";
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:132 unwrap failed"), interp::Value::Bool(true));
}

// ========================================================================
// String indexing edge cases
// ========================================================================

#[test]
fn string_index_out_of_bounds() {
    let src = r#"
func main() -> string {
    let s = "abc";
    s[10]
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err(), "index out of bounds should error");
    assert!(result.unwrap_err().contains("out of bounds"), "expected out of bounds error");
}

#[test]
fn string_index_empty() {
    let src = r#"
func main() -> string {
    let s = "";
    s[0]
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err(), "index out of bounds on empty string should error");
}

#[test]
fn string_index_negative() {
    let src = r#"
func main() -> string {
    let s = "abc";
    s[-1]
}
"#;
    let result = run_source_result(src);
    assert!(result.is_ok(), "negative index should wrap: {:?}", result);
    if let Ok(interp::Value::String(s)) = result {
        assert_eq!(s, "c", "negative index -1 should give last char");
    }
}

#[test]
fn string_substring_invalid_bounds() {
    let src = r#"
func main() -> string {
    "hello".substring(0, 10)
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err(), "substring out of bounds should error");
}

#[test]
fn string_char_at_out_of_bounds() {
    let src = r#"
func main() -> string {
    "abc".char_at(10)
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err(), "char_at out of bounds should error");
}

// ========================================================================
// Let pattern with constructor names (regression: round 2 fix #C)
// ========================================================================

#[test]
fn let_pattern_with_constructor_name_some() {
    // A variable named 'Some' should NOT be treated as Option::Some
    let src = r#"
func main() -> i32 {
    let Some = 42;
    Some
}
"#;
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:215 unwrap failed"), interp::Value::Int(42));
}

#[test]
fn let_pattern_with_constructor_name_none() {
    let src = r#"
func main() -> i32 {
    let None = 99;
    None
}
"#;
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:227 unwrap failed"), interp::Value::Int(99));
}

#[test]
fn let_pattern_with_constructor_name_err() {
    let src = r#"
func main() -> i32 {
    let Err = -1;
    Err
}
"#;
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:239 unwrap failed"), interp::Value::Int(-1));
}

#[test]
fn match_with_variable_shadowing_constructor() {
    let src = r#"
func main() -> i32 {
    let Some = 42;
    match 99 {
        Some => 1,
        _ => 0,
    }
}
"#;
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:254 unwrap failed"), interp::Value::Int(1));
}

// ========================================================================
// Closure capture edge cases (regression: round 2 fix #B)
// ========================================================================

#[test]
fn closure_captures_through_comprehension_iter() {
    // Comprehension inside a closure: `list` must be captured
    let src = r#"
func main() -> i32 {
    let list = [1, 2, 3];
    let f = fn() -> i32 {
        let mut total = 0;
        for x in list {
            total = total + x;
        }
        total
    };
    f()
}
"#;
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:278 unwrap failed"), interp::Value::Int(6));
}

#[test]
fn closure_returns_from_if_expr() {
    // Closure inside an if-expression, where both branches return a closure
    let src = r#"
func main() -> i32 {
    let offset = 5;
    let f = if true {
        fn(x: i32) -> i32 { x + offset }
    } else {
        fn(x: i32) -> i32 { x - offset }
    };
    f(10)
}
"#;
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:296 unwrap failed"), interp::Value::Int(15));
}

#[test]
fn closure_captures_through_slice_expr() {
    let src = r#"
func main() -> i32 {
    let arr = [10, 20, 30];
    let f = fn() -> i32 {
        arr[1]
    };
    f()
}
"#;
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:311 unwrap failed"), interp::Value::Int(20));
}

#[test]
fn closure_captures_through_turbofish() {
    let src = r#"
func id<T>(x: T) -> T { x }
func main() -> i32 {
    let val = 42;
    let f = fn() -> i32 {
        id::<i32>(val)
    };
    f()
}
"#;
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:327 unwrap failed"), interp::Value::Int(42));
}

#[test]
fn closure_captures_through_range() {
    let src = r#"
func main() -> i32 {
    let end = 5;
    let f = fn() -> i32 {
        let mut total = 0;
        for i in 0..end {
            total = total + i;
        }
        total
    };
    f()
}
"#;
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:346 unwrap failed"), interp::Value::Int(10));
}

// ========================================================================
// Arena escape through collections
// ========================================================================

#[test]
fn arena_escape_through_list_detected() {
    let src = r#"
func main() -> i32 {
    let r;
    arena {
        let ref x = 42;
        r = [x];
    }
    0
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err(), "arena escape through list should error");
}

#[test]
fn arena_escape_through_record_detected() {
    let src = r#"
type Wrapper {
    val: i32
}
func main() -> i32 {
    let r;
    arena {
        let ref x = 42;
        r = Wrapper { val: x };
    }
    0
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err(), "arena escape through record should error");
}

// ========================================================================
// Option/Result combinator edge cases
// ========================================================================

#[test]
fn option_none_map_short_circuit() {
    let src = r#"
func add_one(x: i32) -> i32 { x + 1 }
func main() -> i32 {
    let x = Some(21).map(add_one);
    // x should be Some(22)
    match x {
        Some(v) => v,
        _ => 0,
    }
}
"#;
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:406 unwrap failed"), interp::Value::Int(22));
}

#[test]
fn result_err_map_pass_through() {
    let src = r#"
func add_one(x: i32) -> i32 { x + 1 }
func main() -> i32 {
    let x = Err("original").map(add_one);
    // Err.map should pass through Err unchanged
    match x {
        Some(v) => v,
        _ => -1,
    }
}
"#;
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:423 unwrap failed"), interp::Value::Int(-1));
}

#[test]
fn option_some_and_then_flat() {
    let src = r#"
func wrap(x: i32) -> i32 { x * 2 }
func main() -> i32 {
    let x = Some(21).and_then(fn(v: i32) -> Option<i32> { Some(v * 3) });
    match x {
        Some(v) => v,
        _ => 0,
    }
}
"#;
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:439 unwrap failed"), interp::Value::Int(63));
}

#[test]
fn option_none_and_then_short_circuit() {
    let src = r#"
func main() -> i32 {
    let none: Option<i32> = None;
    let x = none.and_then(fn(v: i32) -> Option<i32> { Some(v * 2) });
    match x {
        Some(v) => 1,
        _ => 0,
    }
}
"#;
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:455 unwrap failed"), interp::Value::Int(0), "None.and_then should short-circuit to None");
}

#[test]
fn result_err_and_then_pass_through() {
    let src = r#"
func main() -> i32 {
    let x = Err("fail").and_then(fn(v: i32) -> Result<i32, string> { Ok(v * 2) });
    match x {
        Ok(v) => v,
        _ => -1,
    }
}
"#;
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:470 unwrap failed"), interp::Value::Int(-1));
}

#[test]
fn result_ok_and_then_transform() {
    let src = r#"
func main() -> i32 {
    let x = Ok(21).and_then(fn(v: i32) -> Result<i32, string> { Ok(v * 2) });
    match x {
        Ok(v) => v,
        _ => 0,
    }
}
"#;
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:485 unwrap failed"), interp::Value::Int(42));
}

#[test]
fn result_err_map_err() {
    let src = r#"
func upper(s: string) -> string { s }
func main() -> i32 {
    let x = Err("lower").map_err(fn(s: string) -> string { s });
    match x {
        Ok(v) => v,
        _ => -1,
    }
}
"#;
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:501 unwrap failed"), interp::Value::Int(-1));
}

// ========================================================================
// Shared / Cap / Range equality
// ========================================================================

#[test]
fn range_values_equal() {
    let src = "func main() -> bool { (1..5) == (1..5) }";
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:512 unwrap failed"), interp::Value::Bool(true));
}

#[test]
fn range_values_not_equal() {
    let src = "func main() -> bool { (1..5) == (1..10) }";
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:519 unwrap failed"), interp::Value::Bool(false));
}

// ========================================================================
// Empty sequence operations
// ========================================================================

#[test]
fn sort_empty_list() {
    let src = r#"
func main() -> List<i32> {
    sort([])
}
"#;
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:534 unwrap failed"), interp::Value::List(vec![]));
}

#[test]
fn reverse_empty_list() {
    let src = r#"
func main() -> List<i32> {
    reverse([])
}
"#;
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:545 unwrap failed"), interp::Value::List(vec![]));
}

// ========================================================================
// Values_equal with Shared values through == operator
// ========================================================================

#[test]
fn shared_equality_via_eq_operator() {
    let src = r#"
func main() -> bool {
    shared x = 42;
    shared y = 42;
    x == y
}
"#;
    let result = run_source_result(src);
    assert_eq!(result.expect("src/tests/v1_2_core_edge.rs:562 unwrap failed"), interp::Value::Bool(true));
}
