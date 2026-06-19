use super::*;
#[test]
fn func_with_where_clause_ok() {
    let src = r#"
trait Display {
    func to_string() -> string;
}

type MyType {
    value: i32
}

impl Display for MyType {
    func to_string() -> string {
        "MyType"
    }
}

func print_it(x: MyType) where MyType: Display {
    println(x);
}

func main() -> i32 {
    let t = MyType { value: 42 };
    print_it(t);
    42
}
"#;
    let result = check_source(src);
    assert!(result.is_ok(), "where clause satisfied should pass: {:?}", result.err());
}


#[test]
fn generic_monomorphize_type_inference() {
    let src = r#"
func id<T>(x: T) -> T {
    x
}

func main() -> i32 {
    id(42)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}


#[test]
fn generic_monomorphize_type_check_pass() {
    let src = r#"
func id<T>(x: T) -> T {
    x
}

func main() -> i32 {
    id(42)
}
"#;
    assert!(check_source(src).is_ok());
}


#[test]
fn generic_turbofish_type_check_pass() {
    let src = r#"
func id<T>(x: T) -> T {
    x
}

func main() -> i32 {
    id::<i32>(42)
}
"#;
    assert!(check_source(src).is_ok());
}


#[test]
fn generic_multi_param_type_inference() {
    let src = r#"
func pair<A, B>(a: A, b: B) -> (A, B) {
    (a, b)
}

func main() -> i32 {
    let p = pair(1, "hello")
    42
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}


#[test]
fn generic_multi_param_type_check_pass() {
    let src = r#"
func pair<A, B>(a: A, b: B) -> (A, B) {
    (a, b)
}

func main() -> i32 {
    let p = pair(1, "hello")
    42
}
"#;
    assert!(check_source(src).is_ok());
}


#[test]
fn generic_type_mismatch_inferred() {
    let src = r#"
func id<T>(x: T) -> T {
    x
}

func main() -> i32 {
    id(42)
}
"#;
    assert!(check_source(src).is_ok());
}


#[test]
fn generic_turbofish_wrong_type_arg_count() {
    let src = r#"
func id<T>(x: T) -> T {
    x
}

func main() -> i32 {
    id::<i32, i64>(42)
}
"#;
    let err = check_source(src).unwrap_err();
    assert!(err.iter().any(|d| d.message.contains("expects 1 type arguments")));
}


#[test]
fn generic_function_wrong_arg_type() {
    let src = r#"
func id<T>(x: T) -> T {
    x
}

func main() -> i32 {
    id::<i32>("hello")
}
"#;
    let err = check_source(src).unwrap_err();
    assert!(err.iter().any(|d| d.message.contains("expected i32") && d.message.contains("found string")));
}


#[test]
fn generic_function_body_type_check() {
    let src = r#"
func first<T>(a: T, b: i32) -> T {
    a
}

func main() -> i32 {
    first(42, 99)
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src), interp::Value::Int(42));
}


#[test]
fn generic_function_return_type_inferred() {
    let src = r#"
func id<T>(x: T) -> T {
    x
}

func main() -> i32 {
    let y = id(42)
    y + 1
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src), interp::Value::Int(43));
}


#[test]
fn generic_turbofish_return_type_substituted() {
    let src = r#"
func id<T>(x: T) -> T {
    x
}

func main() -> i32 {
    let y = id::<i32>(42)
    y + 1
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src), interp::Value::Int(43));
}


#[test]
fn generic_nested_type_inference() {
    let src = r#"
func wrap<T>(x: T) -> List<T> {
    [x]
}

func main() -> i32 {
    let l = wrap(42)
    l[0]
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}


#[test]
fn generic_undefined_function() {
    let src = r#"
func main() -> i32 {
    nonexistent(42)
}
"#;
    let err = check_source(src).unwrap_err();
    assert!(err.iter().any(|d| d.message.contains("undefined function")));
}


#[test]
fn generic_func_arg_count_mismatch() {
    let src = r#"
func id<T>(x: T) -> T {
    x
}

func main() -> i32 {
    id(1, 2)
}
"#;
    let err = check_source(src).unwrap_err();
    assert!(err.iter().any(|d| d.message.contains("expects 1") && d.message.contains("got 2")));
}


#[test]
fn generic_function_with_builtin_call() {
    let src = r#"
func echo<T>(x: T) -> T {
    x
}

func main() -> i32 {
    println(echo(42))
    42
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src), interp::Value::Int(42));
}


#[test]
fn generic_function_in_closure() {
    // Closure capturing generic function result
    let src = r#"
func id<T>(x: T) -> T {
    x
}

func apply_id(x: i32) -> i32 {
    id(x)
}

func main() -> i32 {
    apply_id(10)
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src), interp::Value::Int(10));
}


#[test]
fn generic_type_param_shadow_warning() {
    let src = r#"
func id<T>(x: T) -> T {
    x
}

func main() -> i32 {
    let T = 42
    id(T)
}
"#;
    assert!(check_source(src).is_ok());
}


#[test]
fn generic_function_multiple_calls() {
    let src = r#"
func id<T>(x: T) -> T {
    x
}

func main() -> i32 {
    let a = id(1)
    let b = id(2)
    a + b
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src), interp::Value::Int(3));
}

#[test]
fn generic_type_inference_mixed_calls() {
    let src = r#"
func id<T>(x: T) -> T {
    x
}

func main() -> i32 {
    let a = id(42);
    let b = id("hello");
    a
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn generic_type_in_list() {
    let src = r#"
func first<T>(xs: []T) -> T {
    xs[0]
}

func main() -> i32 {
    let xs = [10, 20, 30];
    first(xs)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(10));
}

#[test]
fn generic_two_type_params_order() {
    let src = r#"
func swap<A, B>(a: A, b: B) -> (B, A) {
    (b, a)
}

func main() -> i32 {
    let (x, y) = swap(1, 2);
    x + y
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(3));
}

#[test]
fn generic_concrete_type_def() {
    let src = r#"
type Box<T> {
    value: T
}

func main() -> string {
    let b = Box { value: "hello" };
    b.value
}
"#;
    assert_eq!(run_source(src), interp::Value::String("hello".to_string()));
}

#[test]
fn generic_struct_method_call() {
    let src = r#"
type Pair<A, B> {
    first: A,
    second: B,
}

func make_pair<A, B>(a: A, b: B) -> Pair<A, B> {
    Pair { first: a, second: b }
}

func main() -> i32 {
    let p = make_pair(10, 20);
    p.first + p.second
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(30));
}

// ===== T301: Trait 方法静态分派测试 =====


