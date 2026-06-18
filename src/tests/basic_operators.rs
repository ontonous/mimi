use super::*;

#[test]
fn interp_arithmetic() {
    let src = r#"
func main() -> i32 {
    let x = 10;
    let y = 3;
    return x * y + 1;
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(31));
}

#[test]
fn interp_bitwise_operators() {
    let src = r#"
func main() -> i32 {
    let a = 12;
    let b = 10;
    let band = a & b;
    let bor = a | b;
    let bxor = a ^ b;
    let shl = a << 2;
    let shr = a >> 1;
    band + bor + bxor + shl + shr
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(82));
}

#[test]
fn interp_power_operator() {
    let src = r#"
func main() -> i32 {
    let x = 2 ** 10;
    let y = 3 ** 4;
    x + y
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1105));
}

#[test]
fn interp_comparison_operators() {
    let src = r#"
func main() -> i32 {
    let mut sum = 0;
    if 10 == 10 { sum = sum + 1; }
    if 10 != 9 { sum = sum + 1; }
    if 5 < 10 { sum = sum + 1; }
    if 10 > 5 { sum = sum + 1; }
    if 5 <= 5 { sum = sum + 1; }
    if 5 >= 5 { sum = sum + 1; }
    sum
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(6));
}

#[test]
fn interp_builtin_sqrt() {
    let src = r#"
func main() -> f64 {
    sqrt(16.0) + sqrt(9.0)
}
"#;
    let v = run_source(src);
    assert!(matches!(v, interp::Value::Float(f) if (f - 7.0).abs() < 0.001));
}

#[test]
fn interp_builtin_range() {
    let src = r#"
func main() -> i32 {
    let mut sum = 0;
    for i in range(1, 5) {
        sum = sum + i;
    }
    sum
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(10));
}

#[test]
fn typecheck_invalid_binary_op() {
    let src = r#"
func main() -> i32 {
    let x = "hello" + 42;
    0
}
"#;
    let errs = check_source(src).unwrap_err();
    assert!(!errs.is_empty());
}

#[test]
fn typecheck_invalid_unary_op() {
    let src = r#"
func main() -> i32 {
    let x = !"hello";
    0
}
"#;
    let errs = check_source(src).unwrap_err();
    assert!(!errs.is_empty());
}

#[test]
fn interp_float_arithmetic() {
    let src = r#"
func main() -> f64 {
    let x = 3.14;
    let y = 2.0;
    x * y + 1.0
}
"#;
    let v = run_source(src);
    assert!(matches!(v, interp::Value::Float(f) if (f - 7.28).abs() < 0.001));
}

#[test]
fn interp_float_comparison() {
    let src = r#"
func main() -> i32 {
    let a = 3.14 == 3.14;
    let b = 3.14 != 3.15;
    if a && b { 1 } else { 0 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn interp_short_circuit_and() {
    let src = r#"
func main() -> i32 {
    let x = 0;
    if false && x > 0 { 1 } else { 0 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn interp_short_circuit_or() {
    let src = r#"
func main() -> i32 {
    let x = 0;
    if true || x > 0 { 1 } else { 0 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn interp_compound_bitwise_assign() {
    let src = r#"
func main() -> i32 {
    let mut x = 12;
    x |= 3;
    let mut y = 12;
    y &= 10;
    let mut z = 12;
    z ^= 5;
    x + y + z
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(32));
}
