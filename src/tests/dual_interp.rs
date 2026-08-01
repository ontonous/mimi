// ============================================================
// Dual-Interpreter Equivalence Tests (0.33: bytecode correctness)
//
// Originally compared AST interpreter vs ResolvedInterpreter (0.31.45).
// Now validates bytecode VM correctness for plain functions.
// ============================================================

use super::*;

/// Run source via bytecode VM and return the result.
fn dual_interp(src: &str) -> interp::Value {
    run_source(src)
}

// ============================================================
// Basic arithmetic and control flow
// ============================================================

#[test]
fn dual_interp_basic_arithmetic() {
    let src = r#"
func main() -> i32 {
    let a = 10
    let b = 3
    return a + b * 2
}
"#;
    assert_eq!(dual_interp(src), interp::Value::Int(16));
}

#[test]
fn dual_interp_if_else() {
    let src = r#"
func main() -> i32 {
    let x = 5
    if x > 3 {
        return 1
    } else {
        return 0
    }
}
"#;
    assert_eq!(dual_interp(src), interp::Value::Int(1));
}

#[test]
fn dual_interp_while_loop() {
    let src = r#"
func main() -> i32 {
    let mut sum = 0
    let mut i = 1
    while i <= 10 {
        sum = sum + i
        i = i + 1
    }
    return sum
}
"#;
    assert_eq!(dual_interp(src), interp::Value::Int(55));
}

#[test]
fn dual_interp_for_loop() {
    let src = r#"
func main() -> i32 {
    let xs = [1, 2, 3, 4, 5]
    let mut sum = 0
    for x in xs {
        sum = sum + x
    }
    return sum
}
"#;
    assert_eq!(dual_interp(src), interp::Value::Int(15));
}

// ============================================================
// Functions and recursion
// ============================================================

#[test]
fn dual_interp_function_call() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    return a + b
}

func main() -> i32 {
    return add(3, 4)
}
"#;
    assert_eq!(dual_interp(src), interp::Value::Int(7));
}

#[test]
fn dual_interp_recursion() {
    let src = r#"
func factorial(n: i32) -> i32 {
    if n <= 1 {
        return 1
    }
    return n * factorial(n - 1)
}

func main() -> i32 {
    return factorial(5)
}
"#;
    assert_eq!(dual_interp(src), interp::Value::Int(120));
}

#[test]
fn dual_interp_mutual_recursion() {
    let src = r#"
func is_even(n: i32) -> bool {
    if n == 0 { return true }
    return is_odd(n - 1)
}

func is_odd(n: i32) -> bool {
    if n == 0 { return false }
    return is_even(n - 1)
}

func main() -> bool {
    return is_even(10)
}
"#;
    assert_eq!(dual_interp(src), interp::Value::Bool(true));
}

// ============================================================
// Data structures
// ============================================================

#[test]
fn dual_interp_tuple() {
    let src = r#"
func main() -> (i32, i32) {
    let t = (1, 2)
    return (t.0 + t.1, t.0 * t.1)
}
"#;
    assert_eq!(
        dual_interp(src),
        interp::Value::Tuple(vec![interp::Value::Int(3), interp::Value::Int(2)])
    );
}

#[test]
fn dual_interp_list() {
    let src = r#"
func main() -> i32 {
    let xs = [1, 2, 3, 4, 5]
    let mut sum = 0
    for x in xs {
        sum = sum + x
    }
    return sum
}
"#;
    assert_eq!(dual_interp(src), interp::Value::Int(15));
}

#[test]
fn dual_interp_list_operations() {
    let src = r#"
func main() -> i32 {
    let mut xs = [1, 2, 3]
    push(xs, 4)
    push(xs, 5)
    return len(xs)
}
"#;
    assert_eq!(dual_interp(src), interp::Value::Int(5));
}

#[test]
fn dual_interp_nested_list() {
    let src = r#"
func main() -> i32 {
    let matrix = [[1, 2], [3, 4]]
    return matrix[0][1] + matrix[1][0]
}
"#;
    assert_eq!(dual_interp(src), interp::Value::Int(5));
}

// ============================================================
// Pattern matching
// ============================================================

#[test]
fn dual_interp_match_literal() {
    let src = r#"
func classify(x: i32) -> i32 {
    match x {
        0 => 0,
        1 => 1,
        _ => -1,
    }
}

func main() -> i32 {
    return classify(1) + classify(5)
}
"#;
    assert_eq!(dual_interp(src), interp::Value::Int(0));
}

#[test]
fn dual_interp_match_tuple() {
    let src = r#"
func main() -> i32 {
    let t = (1, 2)
    match t {
        (0, _) => 0,
        (1, y) => y * 10,
        _ => -1,
    }
}
"#;
    assert_eq!(dual_interp(src), interp::Value::Int(20));
}

// ============================================================
// Records and variants
// ============================================================

#[test]
fn dual_interp_record() {
    let src = r#"
type Point { x: i32, y: i32 }

func main() -> i32 {
    let p = Point { x: 3, y: 4 }
    return p.x * p.x + p.y * p.y
}
"#;
    assert_eq!(dual_interp(src), interp::Value::Int(25));
}

#[test]
fn dual_interp_variant() {
    let src = r#"
type Shape {
    Circle(i32),
    Rect(i32, i32),
}

func area(s: Shape) -> i32 {
    match s {
        Circle(r) => 3 * r * r,
        Rect(w, h) => w * h,
    }
}

func main() -> i32 {
    let c = Circle(5)
    let r = Rect(3, 4)
    return area(c) + area(r)
}
"#;
    assert_eq!(dual_interp(src), interp::Value::Int(87));
}

// ============================================================
// Option and Result
// ============================================================

#[test]
fn dual_interp_option() {
    let src = r#"
func safe_div(a: i32, b: i32) -> Option<i32> {
    if b == 0 {
        return None
    }
    return Some(a / b)
}

func main() -> i32 {
    match safe_div(10, 2) {
        Some(x) => x,
        None => -1,
    }
}
"#;
    assert_eq!(dual_interp(src), interp::Value::Int(5));
}

#[test]
fn dual_interp_result() {
    let src = r#"
func parse_int(s: string) -> Result<i32, string> {
    if s == "42" {
        return Ok(42)
    }
    return Err("not 42")
}

func main() -> i32 {
    match parse_int("42") {
        Ok(x) => x,
        Err(_) => -1,
    }
}
"#;
    assert_eq!(dual_interp(src), interp::Value::Int(42));
}

// ============================================================
// String operations
// ============================================================

#[test]
fn dual_interp_string_concat() {
    let src = r#"
func main() -> string {
    let a = "hello"
    let b = "world"
    return a + " " + b
}
"#;
    assert_eq!(
        dual_interp(src),
        interp::Value::String("hello world".to_string())
    );
}

#[test]
fn dual_interp_fstring() {
    let src = r#"
func main() -> string {
    let x = 42
    return f"value is {x}"
}
"#;
    assert_eq!(
        dual_interp(src),
        interp::Value::String("value is 42".to_string())
    );
}

// ============================================================
// Contracts (requires/ensures)
// ============================================================

#[test]
fn dual_interp_contract_simple() {
    let src = r#"
func abs(x: i32) -> i32 {
    requires: x >= -1000
    ensures: result >= 0
    if x < 0 {
        return -x
    }
    return x
}

func main() -> i32 {
    return abs(-5) + abs(3)
}
"#;
    assert_eq!(dual_interp(src), interp::Value::Int(8));
}

// ============================================================
// Comprehensions (simplified - ranges have type inference issues)
// ============================================================

#[test]
fn dual_interp_list_comprehension() {
    let src = r#"
func main() -> i32 {
    let xs = [1, 2, 3, 4, 5]
    let squares = [x * x for x in xs]
    let mut sum = 0
    for s in squares {
        sum = sum + s
    }
    return sum
}
"#;
    // 1 + 4 + 9 + 16 + 25 = 55
    assert_eq!(dual_interp(src), interp::Value::Int(55));
}

#[test]
fn dual_interp_filtered_comprehension() {
    let src = r#"
func main() -> i32 {
    let xs = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
    let evens = [x for x in xs if x % 2 == 0]
    return len(evens)
}
"#;
    assert_eq!(dual_interp(src), interp::Value::Int(5));
}

// ============================================================
// Nested functions and closures
// ============================================================

#[test]
fn dual_interp_closure() {
    // Closures ARE supported by ResolvedInterpreter!
    let src = r#"
func main() -> i32 {
    let f = fn(x: i32) -> i32 { x * 2 }
    return f(5)
}
"#;
    assert_eq!(dual_interp(src), interp::Value::Int(10));
}

#[test]
fn dual_interp_closure_capture() {
    let src = r#"
func main() -> i32 {
    let base = 10
    let add_base = fn(x: i32) -> i32 { x + base }
    return add_base(5)
}
"#;
    assert_eq!(dual_interp(src), interp::Value::Int(15));
}

// ============================================================
// FFI (should be unsupported)
// ============================================================

#[test]
fn dual_interp_ffi_unsupported() {
    // FFI extern calls fail at runtime in bytecode VM (no library loaded).
    let src = r#"
extern "C" {
    func missing_func(x: i32) -> i32;
}

func main() -> i32 {
    return missing_func(42)
}
"#;
    let result = run_source_bytecode_result(src);
    // Bytecode VM should fail (extern not available without FFI library).
    assert!(
        result.is_err(),
        "FFI extern call should fail without library, got: {:?}",
        result
    );
}

// ============================================================
// Edge cases
// ============================================================

#[test]
fn dual_interp_empty_main() {
    let src = r#"
func main() {
}
"#;
    assert_eq!(dual_interp(src), interp::Value::Unit);
}

#[test]
fn dual_interp_early_return() {
    let src = r#"
func main() -> i32 {
    let x = 5
    if x > 3 {
        return 1
    }
    return 0
}
"#;
    assert_eq!(dual_interp(src), interp::Value::Int(1));
}

#[test]
fn dual_interp_nested_if() {
    let src = r#"
func main() -> i32 {
    let x = 5
    let y = 10
    if x > 3 {
        if y > 5 {
            return 1
        } else {
            return 2
        }
    } else {
        return 3
    }
}
"#;
    assert_eq!(dual_interp(src), interp::Value::Int(1));
}

// Note: break/continue has type checker issues with resource analysis.
// Skipping dual_interp_break_continue test until checker is fixed.
