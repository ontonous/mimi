use super::*;

#[test]
fn interp_if_else() {
    let src = r#"
func main() -> i32 {
    let x = 5;
    if x > 3 {
        return 1;
    } else {
        return 0;
    }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(1));
}

#[test]
fn interp_while() {
    let src = r#"
func main() -> i32 {
    let mut i = 0;
    let mut sum = 0;
    while i < 5 {
        sum = sum + i;
        i = i + 1;
    }
    return sum;
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(10));
}

#[test]
fn interp_for_range() {
    let src = r#"
func main() -> i32 {
    let mut sum = 0;
    for i in range(0, 5) {
        sum = sum + i;
    }
    return sum;
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(10));
}

#[test]
fn interp_fib() {
    let src = r#"
func fib(n: i32) -> i32 {
    if n <= 1 {
        return n;
    } else {
        return fib(n - 1) + fib(n - 2);
    }
}

func main() -> i32 {
    return fib(10);
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(55));
}

#[test]
fn typecheck_if_condition_bool() {
    let src = r#"
func main() {
    if 42 {
        println("bad");
    }
}
"#;
    let errs = check_source(src).unwrap_err();
    assert!(errs.iter().any(|d| d.message.contains("if condition must be bool")));
}

#[test]
fn interp_match_enum() {
    let src = r#"
type Shape {
    Circle(f64)
    Rectangle(f64, f64)
}

func area(s: Shape) -> f64 {
    match s {
        Circle(r) => 3.14159 * r * r,
        Rectangle(w, h) => w * h,
    }
}

func main() -> f64 {
    area(Circle(2.0)) + area(Rectangle(3.0, 4.0))
}
"#;
    let v = run_source(src);
    assert!(matches!(v, interp::Value::Float(_)));
}

#[test]
fn typecheck_match_exhaustive() {
    let src = r#"
type Opt { Some(i32) None }
func main() -> i32 {
    let x = Some(42);
    match x {
        Some(v) => v,
        None => 0,
    }
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn typecheck_match_non_exhaustive() {
    let src = r#"
type Color {
    Red
    Green
    Blue
}

func main() -> i32 {
    let c = Red;
    match c {
        Red => 1,
        Green => 2,
    }
}
"#;
    let errs = check_source(src).unwrap_err();
    assert!(!errs.is_empty());
}

#[test]
fn interp_match_with_guard() {
    let src = r#"
type Opt {
    Some(i32)
    None
}

func main() -> i32 {
    let x = Some(5);
    match x {
        Some(n) if n > 3 => 1,
        Some(n) if n <= 3 => 2,
        None => 0,
    }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn interp_match_nested_variants() {
    let src = r#"
type Tree {
    Leaf(i32)
    Node(Tree, Tree)
}

func sum(t: Tree) -> i32 {
    match t {
        Leaf(n) => n,
        Node(l, r) => sum(l) + sum(r),
    }
}

func main() -> i32 {
    let t = Node(Leaf(1), Node(Leaf(2), Leaf(3)));
    sum(t)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(6));
}

#[test]
fn interp_match_tuple_pattern() {
    let src = r#"
type Pair {
    Pair(i32, i32)
}

func main() -> i32 {
    let p = Pair(10, 20);
    match p {
        Pair(a, b) => a + b,
    }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(30));
}

#[test]
fn interp_else_if_chain() {
    let src = r#"
func classify(n: i32) -> i32 {
    if n < 0 {
        -1
    } else if n == 0 {
        0
    } else {
        1
    }
}

func main() -> i32 {
    classify(-5) + classify(0) + classify(10)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn interp_else_if_multiple() {
    let src = r#"
func grade(score: i32) -> i32 {
    if score >= 90 {
        4
    } else if score >= 80 {
        3
    } else if score >= 70 {
        2
    } else if score >= 60 {
        1
    } else {
        0
    }
}

func main() -> i32 {
    grade(95) + grade(85) + grade(75) + grade(65) + grade(50)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(10));
}

#[test]
fn interp_nested_match() {
    let src = r#"
type Opt {
    Some(i32)
    None
}

func unwrap_or(o: Opt, default: i32) -> i32 {
    match o {
        Some(v) => v,
        None => default,
    }
}

func main() -> i32 {
    unwrap_or(Some(42), 0) + unwrap_or(None, 10)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(52));
}

#[test]
fn break_exits_while_loop() {
    let src = r#"
func main() -> i32 {
    let mut i = 0;
    while i < 10 {
        if i == 3 {
            break;
        }
        i = i + 1;
    }
    return i;
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(3));
}

#[test]
fn break_with_value() {
    let src = r#"
func main() -> i32 {
    let mut i = 0;
    while i < 10 {
        if i == 5 {
            break i * 2;
        }
        i = i + 1;
    }
    return 0;
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(10));
}

#[test]
fn continue_skips_iteration() {
    let src = r#"
func main() -> i32 {
    let mut sum = 0;
    let mut i = 0;
    while i < 5 {
        i = i + 1;
        if i == 3 {
            continue;
        }
        sum = sum + i;
    }
    return sum;
}
"#;
    // sum = 1 + 2 + 4 + 5 = 12 (skips 3)
    assert_eq!(run_source(src), interp::Value::Int(12));
}

#[test]
fn break_in_nested_loop() {
    let src = r#"
func main() -> i32 {
    let mut result = 0;
    let mut i = 0;
    while i < 3 {
        let mut j = 0;
        while j < 3 {
            if j == 1 {
                break;
            }
            result = result + 1;
            j = j + 1;
        }
        i = i + 1;
    }
    return result;
}
"#;
    // inner loop: j=0 executes (result+=1, j=1), then j=1 breaks
    // so result += 1 per outer iteration, 3 outer iterations
    assert_eq!(run_source(src), interp::Value::Int(3));
}

#[test]
fn continue_in_for_loop() {
    let src = r#"
func main() -> i32 {
    let mut sum = 0;
    for x in [1, 2, 3, 4, 5] {
        if x == 3 {
            continue;
        }
        sum = sum + x;
    }
    return sum;
}
"#;
    // sum = 1 + 2 + 4 + 5 = 12
    assert_eq!(run_source(src), interp::Value::Int(12));
}

#[test]
fn break_in_for_loop() {
    let src = r#"
func main() -> i32 {
    let mut result = 0;
    for x in [10, 20, 30, 40, 50] {
        if x == 30 {
            break;
        }
        result = result + x;
    }
    return result;
}
"#;
    // result = 10 + 20 = 30
    assert_eq!(run_source(src), interp::Value::Int(30));
}

#[test]
fn break_outside_loop_error() {
    let src = r#"
func main() -> i32 {
    break;
    return 0;
}
"#;
    let result = check_source(src);
    assert!(result.is_err());
    let errors = result.unwrap_err();
    assert!(errors.iter().any(|e| e.message.contains("break outside of loop")));
}

#[test]
fn continue_outside_loop_error() {
    let src = r#"
func main() -> i32 {
    continue;
    return 0;
}
"#;
    let result = check_source(src);
    assert!(result.is_err());
    let errors = result.unwrap_err();
    assert!(errors.iter().any(|e| e.message.contains("continue outside of loop")));
}

#[test]
fn break_in_if_inside_loop() {
    let src = r#"
func main() -> i32 {
    let mut i = 0;
    while i < 10 {
        if i == 5 {
            break;
        }
        i = i + 1;
    }
    return i;
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(5));
}

#[test]
fn continue_while_condition_reevaluated() {
    let src = r#"
func main() -> i32 {
    let mut count = 0;
    let mut i = 0;
    while i < 5 {
        i = i + 1;
        if i == 2 {
            continue;
        }
        count = count + 1;
    }
    return count;
}
"#;
    // i: 1(count=1), 2(skip), 3(count=2), 4(count=3), 5(count=4)
    assert_eq!(run_source(src), interp::Value::Int(4));
}

#[test]
fn array_literal_creation() {
    let src = r#"
func main() -> i32 {
    let arr: [i32; 3] = [1, 2, 3];
    return arr[0] + arr[1] + arr[2];
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(6));
}

#[test]
fn array_index_access() {
    let src = r#"
func main() -> i32 {
    let arr: [i32; 5] = [10, 20, 30, 40, 50];
    return arr[2];
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(30));
}

#[test]
fn array_index_out_of_bounds() {
    let src = r#"
func main() -> i32 {
    let arr: [i32; 3] = [1, 2, 3];
    return arr[5];
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
}

#[test]
fn array_type_annotation() {
    let src = r#"
func main() -> i32 {
    let arr: [i32; 4] = [100, 200, 300, 400];
    return arr[3];
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(400));
}

#[test]
fn array_size_mismatch_error() {
    let src = r#"
func main() -> i32 {
    let arr: [i32; 2] = [1, 2, 3];
    return arr[0];
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
}

#[test]
fn array_empty() {
    let src = r#"
func main() -> i32 {
    let arr: [i32; 0] = [];
    return 42;
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn array_single_element() {
    let src = r#"
func main() -> i32 {
    let arr: [i32; 1] = [99];
    return arr[0];
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(99));
}

#[test]
fn array_with_expressions() {
    let src = r#"
func main() -> i32 {
    let x = 10;
    let arr: [i32; 3] = [x, x + 1, x * 2];
    return arr[0] + arr[1] + arr[2];
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(41));
}

#[test]
fn array_negative_index() {
    let src = r#"
func main() -> i32 {
    let arr: [i32; 3] = [10, 20, 30];
    return arr[-1];
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(30));
}

#[test]
fn array_equality() {
    let src = r#"
func main() -> bool {
    let a: [i32; 3] = [1, 2, 3];
    let b: [i32; 3] = [1, 2, 3];
    return a == b;
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}

#[test]
fn array_inequality() {
    let src = r#"
func main() -> bool {
    let a: [i32; 3] = [1, 2, 3];
    let b: [i32; 3] = [1, 2, 4];
    return a == b;
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(false));
}

#[test]
fn array_display() {
    let src = r#"
func main() -> string {
    let arr: [i32; 3] = [1, 2, 3];
    return to_string(arr);
}
"#;
    assert_eq!(run_source(src), interp::Value::String("[1, 2, 3]".to_string()));
}

#[test]
fn slice_of_list() {
    let src = r#"
func main() -> i32 {
    let arr = [10, 20, 30, 40, 50];
    let s = arr[1..4];
    return s[0] + s[1] + s[2];
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(90));
}

#[test]
fn slice_range_syntax() {
    let src = r#"
func main() -> i32 {
    let arr = [1, 2, 3, 4, 5];
    let s = arr[0..3];
    return len(s);
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(3));
}

#[test]
fn slice_open_end() {
    let src = r#"
func main() -> i32 {
    let arr = [10, 20, 30, 40, 50];
    let s = arr[2..];
    return len(s);
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(3));
}

#[test]
fn slice_open_start() {
    let src = r#"
func main() -> i32 {
    let arr = [10, 20, 30, 40, 50];
    let s = arr[..3];
    return len(s);
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(3));
}

#[test]
fn slice_empty() {
    let src = r#"
func main() -> i32 {
    let arr = [1, 2, 3];
    let s = arr[1..1];
    return len(s);
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn slice_full() {
    let src = r#"
func main() -> i32 {
    let arr = [1, 2, 3];
    let s = arr[..];
    return len(s);
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(3));
}

#[test]
fn slice_out_of_bounds() {
    let src = r#"
func main() -> i32 {
    let arr = [1, 2, 3];
    let s = arr[1..10];
    return 0;
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
}

#[test]
fn slice_start_greater_than_end() {
    let src = r#"
func main() -> i32 {
    let arr = [1, 2, 3];
    let s = arr[2..1];
    return 0;
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
}

#[test]
fn slice_negative_index() {
    let src = r#"
func main() -> i32 {
    let arr = [10, 20, 30, 40, 50];
    let s = arr[-3..];
    return s[0] + s[1] + s[2];
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(120));
}

#[test]
fn slice_equality() {
    let src = r#"
func main() -> bool {
    let arr = [1, 2, 3, 4, 5];
    let a = arr[0..3];
    let b = arr[0..3];
    return a == b;
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}

#[test]
fn slice_display() {
    let src = r#"
func main() -> string {
    let arr = [10, 20, 30];
    let s = arr[1..3];
    return to_string(s);
}
"#;
    assert_eq!(run_source(src), interp::Value::String("[20, 30]".to_string()));
}

#[test]
fn slice_of_string() {
    let src = r#"
func main() -> string {
    let s = "hello world";
    return s[0..5];
}
"#;
    assert_eq!(run_source(src), interp::Value::String("hello".to_string()));
}

#[test]
fn array_pattern_match() {
    let src = r#"
func main() -> i32 {
    let arr: [i32; 3] = [1, 2, 3];
    match arr {
        [1, 2, 3] => 10,
        _ => 0,
    }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(10));
}

#[test]
fn array_pattern_with_wildcard() {
    let src = r#"
func main() -> i32 {
    let arr: [i32; 3] = [1, 2, 3];
    match arr {
        [1, _, _] => 10,
        _ => 0,
    }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(10));
}

#[test]
fn array_pattern_wrong_length() {
    let src = r#"
func main() -> i32 {
    let arr: [i32; 3] = [1, 2, 3];
    match arr {
        [1, 2] => 10,
        _ => 0,
    }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn slice_pattern_prefix() {
    let src = r#"
func main() -> i32 {
    let arr = [1, 2, 3, 4, 5];
    let s = arr[1..4];
    match s {
        [2, 3, _] => 10,
        _ => 0,
    }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(10));
}

#[test]
fn slice_pattern_with_rest() {
    let src = r#"
func main() -> i32 {
    let arr = [10, 20, 30, 40, 50];
    let s = arr[0..3];
    match s {
        [10, ..rest] => len(rest),
        _ => 0,
    }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(2));
}

#[test]
fn array_pattern_with_variable() {
    let src = r#"
func main() -> i32 {
    let arr: [i32; 3] = [10, 20, 30];
    match arr {
        [a, b, c] => a + b + c,
        _ => 0,
    }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(60));
}

#[test]
fn range_for_loop() {
    let src = r#"
func main() -> i32 {
    let mut sum = 0;
    for i in 0..5 {
        sum = sum + i;
    }
    return sum;
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(10));
}

#[test]
fn range_display() {
    let src = r#"
func main() -> i32 {
    let r = 1..10;
    return 0;
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn async_func_basic() {
    let src = r#"
async func add_one(x: i32) -> i32 {
    return x + 1;
}

func main() -> i32 {
    let f = add_one(5);
    return await f;
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(6));
}

#[test]
fn impl_trait_return_type() {
    let src = r#"
type Point {
    x: i32
    y: i32
}

func make_point() -> impl Display {
    return Point { x: 1, y: 2 };
}

func main() -> i32 {
    let p = make_point();
    return 42;
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn impl_trait_multiple_bounds() {
    let src = r#"
func make_value() -> impl Printable + Cloneable {
    return 42;
}

func main() -> i32 {
    let v = make_value();
    return 99;
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(99));
}

#[test]
fn unsafe_block_basic() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    unsafe {
        let y = x + 1;
        y
    }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(43));
}

#[test]
fn unsafe_block_with_mutation() {
    let src = r#"
func main() -> i32 {
    let mut x = 10;
    unsafe {
        x = x * 2;
    }
    x
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(20));
}
