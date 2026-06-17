use super::*;
#[test]
fn interp_closure_basic() {
    let src = r#"
func main() -> i32 {
    let add = fn(x: i32, y: i32) -> i32 { x + y };
    add(3, 4)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(7));
}

#[test]
fn interp_closure_single_param() {
    let src = r#"
func main() -> i32 {
    let double = fn(x: i32) -> i32 { x * 2 };
    double(5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(10));
}

#[test]
fn interp_closure_no_params() {
    let src = r#"
func main() -> i32 {
    let get_five = fn() -> i32 { 5 };
    get_five()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn interp_closure_capture() {
    let src = r#"
func main() -> i32 {
    let offset = 10;
    let add_offset = fn(x: i32) -> i32 { x + offset };
    add_offset(5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(15));
}

#[test]
fn interp_closure_as_argument() {
    let src = r#"
func apply(f: i32, x: i32) -> i32 {
    f(x)
}

func main() -> i32 {
    let double = fn(x: i32) -> i32 { x * 2 };
    apply(double, 5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(10));
}

#[test]
fn interp_closure_in_list() {
    let src = r#"
func main() -> i32 {
    let fns = [
        fn(x: i32) -> i32 { x + 1 },
        fn(x: i32) -> i32 { x * 2 },
        fn(x: i32) -> i32 { x - 1 }
    ];
    fns[0](10) + fns[1](10) + fns[2](10)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(40));
}

#[test]
fn interp_closure_in_tuple() {
    let src = r#"
func main() -> i32 {
    let inc = fn(x: i32) -> i32 { x + 1 };
    let dec = fn(x: i32) -> i32 { x - 1 };
    inc(10) + dec(10)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(20));
}

#[test]
fn interp_closure_return_closure() {
    let src = r#"
func make_adder(n: i32) -> i32 {
    fn(x: i32) -> i32 { x + n }
}

func main() -> i32 {
    let add10 = make_adder(10);
    let add20 = make_adder(20);
    add10(5) + add20(5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(40));
}

#[test]
fn interp_first_class_function() {
    let src = r#"
func double(x: i32) -> i32 { x * 2 }
func inc(x: i32) -> i32 { x + 1 }

func main() -> i32 {
    let f1 = double;
    let f2 = inc;
    f1(3) + f2(5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(12));
}

#[test]
fn interp_closure_with_if() {
    let src = r#"
func main() -> i32 {
    let abs = fn(x: i32) -> i32 {
        if x < 0 { -x } else { x }
    };
    abs(-5) + abs(3)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(8));
}

#[test]
fn interp_closure_with_while() {
    let src = r#"
func main() -> i32 {
    let count = fn(n: i32) -> i32 {
        let mut sum = 0;
        let mut i = 0;
        while i < n {
            sum += i;
            i += 1;
        }
        sum
    };
    count(5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(10));
}

#[test]
fn interp_closure_multiple_captures() {
    let src = r#"
func main() -> i32 {
    let a = 10;
    let b = 20;
    let c = 30;
    let sum = fn(x: i32) -> i32 { x + a + b + c };
    sum(1)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(61));
}

#[test]
fn interp_closure_nested_calls() {
    let src = r#"
func main() -> i32 {
    let add = fn(a: i32, b: i32) -> i32 { a + b };
    let mul = fn(a: i32, b: i32) -> i32 { a * b };
    add(mul(2, 3), mul(4, 5))
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(26));
}

#[test]
fn move_semantics_int_copy() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    let y = x;
    x + y
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(84));
}

#[test]
fn move_semantics_string_move() {
    let src = r#"
func main() -> i32 {
    let s = "hello";
    let t = s;
    1
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn move_semantics_string_use_after_move() {
    let src = r#"
func main() -> i32 {
    let s = "hello";
    let t = s;
    s
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("use of moved value"), "Expected 'use of moved value' error, got: {}", err);
}

#[test]
fn move_semantics_list_move() {
    let src = r#"
func main() -> i32 {
    let a = [1, 2, 3];
    let b = a;
    1
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn move_semantics_list_use_after_move() {
    let src = r#"
func main() -> i32 {
    let a = [1, 2, 3];
    let b = a;
    a
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("use of moved value"), "Expected 'use of moved value' error, got: {}", err);
}

#[test]
fn move_semantics_tuple_copy() {
    let src = r#"
func main() -> i32 {
    let t = (1, 2, 3);
    let u = t;
    let (a, _, _) = t;
    let (b, _, _) = u;
    a + b
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(2));
}

#[test]
fn move_semantics_bool_copy() {
    let src = r#"
func main() -> i32 {
    let b = true;
    let c = b;
    if b { 1 } else { 0 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn move_semantics_float_copy() {
    let src = r#"
func main() -> f64 {
    let x = 3.14;
    let y = x;
    x + y
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Float(6.28));
}

#[test]
fn move_semantics_assignment_move() {
    let src = r#"
func main() -> i32 {
    let s = "hello";
    let mut t = "world";
    t = s;
    1
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn move_semantics_assignment_use_after_move() {
    let src = r#"
func main() -> i32 {
    let s = "hello";
    let mut t = "world";
    t = s;
    s
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("use of moved value"), "Expected 'use of moved value' error, got: {}", err);
}

#[test]
fn move_semantics_function_arg_move() {
    let src = r#"
func consume(s: string) -> i32 { 1 }

func main() -> i32 {
    let s = "hello";
    consume(s)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn move_semantics_closure_capture() {
    let src = r#"
func main() -> i32 {
    let x = 10;
    let f = fn() -> i32 { x };
    f()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(10));
}

#[test]
fn move_semantics_variant_move() {
    let src = r#"
type Opt {
    Some(i32)
    None
}

func main() -> i32 {
    let o = Some(42);
    let p = o;
    1
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn move_semantics_variant_use_after_move() {
    // With auto-Copy: Some(42) has all Copy args, so it IS Copy.
    // Using it after move should succeed.
    let src = r#"
type Opt {
    Some(i32)
    None
}

func main() -> i32 {
    let o = Some(42);
    let p = o;
    match o {
        Some(v) => v,
        None => 0,
    }
}
"#;
    let result = run_source_result(src);
    assert!(result.is_ok(), "Some(42) should be Copy (all args are Copy): {:?}", result);
}

#[test]
fn borrow_immutable() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    let r = &x;
    r
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn borrow_mutable() {
    let src = r#"
func main() -> i32 {
    let mut x = 42;
    let r = &mut x;
    *r
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn borrow_does_not_move_copy() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    let r = &x;
    x + *r
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(84));
}


