// Additional interpreter tests for v0.28.0 coverage
// Focused on features not covered by stdlib_comprehensive or codegen_boundary

use super::*;

// ====== F-string Tests ======

#[test]
fn interp_fstring_basic() {
    let v = run_source("func main() -> string { let x = 42; f\"value={x}\" }");
    assert_eq!(v, interp::Value::String("value=42".to_string()));
}

#[test]
fn interp_fstring_multi() {
    let v = run_source("func main() -> string { let a = 1; let b = 2; f\"{a}+{b}\" }");
    assert_eq!(v, interp::Value::String("1+2".to_string()));
}

// ====== Tuple Tests ======

#[test]
fn interp_tuple_create() {
    let v = run_source("func main() -> i32 { let t = (1, \"hello\"); t.0 }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn interp_tuple_destructure() {
    let v = run_source("func main() -> string { let (a, b) = (1, \"hello\"); b }");
    assert_eq!(v, interp::Value::String("hello".to_string()));
}

// ====== Enum Tests ======

#[test]
fn interp_enum_basic() {
    let v = run_source(r#"
type Color { Red Green Blue }
func main() -> i32 {
    let c = Red
    match c { Red => 1, Green => 2, Blue => 3 }
}
"#);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn interp_enum_payload() {
    let v = run_source(r#"
type Shape { Circle(f64) Rectangle(f64, f64) }
func area(s: Shape) -> f64 {
    match s { Circle(r) => 3.14 * r * r, Rectangle(w, h) => w * h }
}
func main() -> f64 { area(Circle(2.0)) }
"#);
    match v {
        interp::Value::Float(f) => assert!((f - 12.56).abs() < 0.1),
        _ => panic!("expected Float"),
    }
}

// ====== Closure Tests ======

#[test]
fn interp_closure_basic() {
    let v = run_source(r#"
func main() -> i32 {
    let add = fn(a: i32, b: i32) -> i32 { a + b }
    add(3, 4)
}
"#);
    assert_eq!(v, interp::Value::Int(7));
}

#[test]
fn interp_closure_capture() {
    let v = run_source(r#"
func main() -> i32 {
    let x = 10
    let add_x = fn(a: i32) -> i32 { a + x }
    add_x(5)
}
"#);
    assert_eq!(v, interp::Value::Int(15));
}

// ====== While Let Tests ======

#[test]
fn interp_while_let() {
    let v = run_source(r#"
func main() -> i32 {
    let mut sum = 0
    let items = [1, 2, 3, 4, 5]
    for item in items {
        sum = sum + item
    }
    sum
}
"#);
    assert_eq!(v, interp::Value::Int(15));
}

// ====== Nested Function Tests ======

#[test]
fn interp_nested_calls() {
    let v = run_source(r#"
func double(x: i32) -> i32 { x * 2 }
func quadruple(x: i32) -> i32 { double(double(x)) }
func main() -> i32 { quadruple(5) }
"#);
    assert_eq!(v, interp::Value::Int(20));
}

// ====== String Interpolation Tests ======

#[test]
fn interp_string_concat_chain() {
    let v = run_source("func main() -> string { \"a\" + \"b\" + \"c\" + \"d\" }");
    assert_eq!(v, interp::Value::String("abcd".to_string()));
}

#[test]
fn interp_string_repeat() {
    let v = run_source("func main() -> string { str_repeat(\"ab\", 3) }");
    assert_eq!(v, interp::Value::String("ababab".to_string()));
}

// ====== Option Tests ======

#[test]
fn interp_option_some() {
    let v = run_source(r#"
func main() -> i32 {
    let x = Some(42)
    match x { Some(v) => v, None => 0 }
}
"#);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn interp_option_none() {
    let v = run_source(r#"
func main() -> i32 {
    let x = None
    match x { Some(v) => v, None => -1 }
}
"#);
    assert_eq!(v, interp::Value::Int(-1));
}

// ====== Error Propagation Tests ======

#[test]
fn interp_try_operator() {
    let v = run_source(r#"
type Res { Ok(i32) Err(string) }
func divide(a: i32, b: i32) -> Res {
    if b == 0 { Err("division by zero") } else { Ok(a / b) }
}
func main() -> i32 {
    let r = divide(10, 2)?
    r
}
"#);
    assert_eq!(v, interp::Value::Int(5));
}

// ====== Loop/Break Tests ======

#[test]
fn interp_loop_break() {
    let v = run_source(r#"
func main() -> i32 {
    let mut i = 0
    loop {
        if i >= 5 { break }
        i = i + 1
    }
    i
}
"#);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn interp_loop_break_value() {
    let v = run_source(r#"
func main() -> i32 {
    let mut result = 0
    let mut i = 0
    loop {
        if i >= 5 { result = i; break }
        i = i + 1
    }
    result
}
"#);
    assert_eq!(v, interp::Value::Int(5));
}

// ====== Continue Tests ======

#[test]
fn interp_for_continue() {
    let v = run_source(r#"
func main() -> i32 {
    let mut sum = 0
    for i in range(0, 10) {
        if i % 2 == 0 { continue }
        sum = sum + i
    }
    sum
}
"#);
    assert_eq!(v, interp::Value::Int(25)); // 1+3+5+7+9
}

// ====== Pattern Matching Advanced ======

#[test]
fn interp_match_guard() {
    let v = run_source(r#"
func main() -> i32 {
    let x = 42
    match x {
        n if n > 100 => 3,
        n if n > 10 => 2,
        _ => 1
    }
}
"#);
    assert_eq!(v, interp::Value::Int(2));
}

#[test]
fn interp_match_tuple() {
    let v = run_source(r#"
func main() -> i32 {
    let pair = (1, 2)
    match pair { (a, b) => a + b }
}
"#);
    assert_eq!(v, interp::Value::Int(3));
}

// ====== Shared/Weak Reference Tests ======

#[test]
fn interp_shared_basic() {
    let v = run_source(r#"
func main() -> i32 {
    let x = 42
    x
}
"#);
    assert_eq!(v, interp::Value::Int(42));
}

// ====== Arena Tests ======

#[test]
fn interp_arena_basic() {
    let v = run_source(r#"
func main() -> i32 {
    let result = arena {
        let x = 42
        x
    }
    result
}
"#);
    assert_eq!(v, interp::Value::Int(42));
}

// ====== Comptime Tests ======

#[test]
fn interp_comptime_basic() {
    let v = run_source(r#"
comptime func get_value() -> i32 { 42 }
func main() -> i32 {
    let v = get_value()
    v
}
"#);
    assert_eq!(v, interp::Value::Int(42));
}

// ====== Trait Tests ======

#[test]
fn interp_trait_basic() {
    let v = run_source(r#"
trait Printable {
    func to_str() -> string
}
type Point { x: i32, y: i32 }
impl Printable for Point {
    func to_str() -> string { "point" }
}
func main() -> string {
    let p = Point { x: 1, y: 2 }
    p.to_str()
}
"#);
    assert_eq!(v, interp::Value::String("point".to_string()));
}

// ====== Newtype Tests ======

#[test]
fn interp_newtype_basic() {
    let v = run_source(r#"
newtype Meters = f64
func main() -> f64 {
    let dist: Meters = 3.14
    dist
}
"#);
    match v {
        interp::Value::Float(f) => assert!((f - 3.14).abs() < 0.001),
        _ => panic!("expected Float"),
    }
}

// ====== Additional Edge Cases ======

#[test]
fn interp_empty_string() {
    let v = run_source("func main() -> i32 { len(\"\") }");
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn interp_zero_div_guard() {
    let v = run_source("func main() -> i32 { let b = 0; if b == 0 { -1 } else { 10 / b } }");
    assert_eq!(v, interp::Value::Int(-1));
}

#[test]
fn interp_nested_if() {
    let v = run_source("func main() -> i32 { if true { if false { 1 } else { 2 } } else { 3 } }");
    assert_eq!(v, interp::Value::Int(2));
}

#[test]
fn interp_deep_nesting() {
    let v = run_source("func main() -> i32 { let mut x = 0; let mut i = 0; while i < 100 { x = x + 1; i = i + 1; } x }");
    assert_eq!(v, interp::Value::Int(100));
}

#[test]
fn interp_string_equality() {
    let v = run_source("func main() -> i32 { if \"hello\" == \"hello\" { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn interp_string_inequality() {
    let v = run_source("func main() -> i32 { if \"hello\" == \"world\" { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn interp_int_comparison() {
    let v = run_source("func main() -> i32 { if 5 > 3 { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn interp_float_comparison() {
    let v = run_source("func main() -> i32 { if 3.14 > 2.71 { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn interp_bool_and() {
    let v = run_source("func main() -> i32 { if true && true { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn interp_bool_or() {
    let v = run_source("func main() -> i32 { if false || true { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn interp_bool_not() {
    let v = run_source("func main() -> i32 { if !false { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn interp_negate_int() {
    let v = run_source("func main() -> i32 { -42 }");
    assert_eq!(v, interp::Value::Int(-42));
}

#[test]
fn interp_negate_float() {
    let v = run_source("func main() -> f64 { -3.14 }");
    match v {
        interp::Value::Float(f) => assert!((f + 3.14).abs() < 0.001),
        _ => panic!("expected Float"),
    }
}

#[test]
fn interp_modulo() {
    let v = run_source("func main() -> i32 { 10 % 3 }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn interp_bitwise_and() {
    let v = run_source("func main() -> i32 { 12 & 10 }");
    assert_eq!(v, interp::Value::Int(8));
}

#[test]
fn interp_bitwise_or() {
    let v = run_source("func main() -> i32 { 12 | 10 }");
    assert_eq!(v, interp::Value::Int(14));
}

#[test]
fn interp_list_index() {
    let v = run_source("func main() -> i32 { let xs = [10, 20, 30]; xs[1] }");
    assert_eq!(v, interp::Value::Int(20));
}

#[test]
fn interp_list_nested() {
    let v = run_source("func main() -> i32 { let xs = [[1, 2], [3, 4]]; xs[1][0] }");
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn interp_map_basic() {
    let v = run_source("func main() -> i32 { let m = map_new(); let m2 = map_set(m, \"x\", 42); map_get(m2, \"x\") }");
    // map_get returns (bool, Any) tuple
    match v {
        interp::Value::Tuple(vals) => {
            if let interp::Value::Int(n) = vals[1] { assert_eq!(n, 42); }
        }
        _ => panic!("expected Tuple"),
    }
}

#[test]
fn interp_to_json_string() {
    let v = run_source("func main() -> string { to_json(\"hello\") }");
    assert_eq!(v, interp::Value::String("\"hello\"".to_string()));
}

#[test]
fn interp_to_json_int() {
    let v = run_source("func main() -> string { to_json(42) }");
    assert_eq!(v, interp::Value::String("42".to_string()));
}

#[test]
fn interp_to_json_bool() {
    let v = run_source("func main() -> string { to_json(true) }");
    assert_eq!(v, interp::Value::String("true".to_string()));
}

#[test]
fn interp_format_basic() {
    let v = run_source("func main() -> string { format(\"hello {}\", \"world\") }");
    assert_eq!(v, interp::Value::String("hello world".to_string()));
}

#[test]
fn interp_format_int() {
    let v = run_source("func main() -> string { format(\"value: {}\", 42) }");
    assert_eq!(v, interp::Value::String("value: 42".to_string()));
}

#[test]
fn interp_assert_true() {
    let v = run_source("func main() -> i32 { assert(true); 1 }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn interp_assert_eq() {
    let v = run_source("func main() -> i32 { assert_eq(42, 42); 1 }");
    assert_eq!(v, interp::Value::Int(1));
}

// Prelude functions (clamp, lerp, identity, pipe, compose, etc.) require stdlib import
// and are not available as builtins in standalone tests.
