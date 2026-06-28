use super::*;
#[test]
fn comptime_block_basic() {
    let src = r#"
func main() -> i32 {
    comptime {
        let x = 10;
        let y = 20;
        x + y
    }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(30));
}

#[test]
fn comptime_block_with_string() {
    let src = r#"
func main() -> string {
    comptime {
        "hello"
    }
}
"#;
    assert_eq!(run_source(src), interp::Value::String("hello".to_string()));
}

#[test]
fn comptime_block_nested() {
    let src = r#"
func main() -> i32 {
    comptime {
        let outer = comptime {
            5 * 6
        };
        outer + 1
    }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(31));
}

#[test]
fn type_of_int() {
    let src = r#"
func main() -> string {
    let x = 42;
    type_name(x)
}
"#;
    assert_eq!(run_source(src), interp::Value::String("i32".to_string()));
}

#[test]
fn type_of_bool() {
    let src = r#"
func main() -> string {
    let x = true;
    type_name(x)
}
"#;
    assert_eq!(run_source(src), interp::Value::String("bool".to_string()));
}

#[test]
fn type_of_string() {
    let src = r#"
func main() -> string {
    let x = "hello";
    type_name(x)
}
"#;
    assert_eq!(run_source(src), interp::Value::String("string".to_string()));
}

#[test]
fn type_of_list() {
    let src = r#"
func main() -> string {
    let x = [1, 2, 3];
    type_name(x)
}
"#;
    assert_eq!(run_source(src), interp::Value::String("list".to_string()));
}

#[test]
fn type_of_variant() {
    let src = r#"
type Color { Red | Green | Blue }

func main() -> string {
    let x = Red();
    type_name(x)
}
"#;
    assert_eq!(run_source(src), interp::Value::String("Red".to_string()));
}

#[test]
fn type_of_record() {
    let src = r#"
type Point {
    x: i32
    y: i32
}

func main() -> string {
    let p = Point { x: 1, y: 2 };
    type_name(p)
}
"#;
    assert_eq!(run_source(src), interp::Value::String("Point".to_string()));
}

#[test]
fn type_fields_record() {
    let src = r#"
type Point {
    x: i32
    y: i32
}

func main() -> i32 {
    let fields = type_fields("Point");
    len(fields)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(2));
}

#[test]
fn type_variants_enum() {
    let src = r#"
type Color { Red | Green | Blue }

func main() -> i32 {
    let variants = type_variants("Color");
    len(variants)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(3));
}

#[test]
fn type_info_for_record() {
    let src = r#"
type Point {
    x: i32
    y: i32
}

func main() -> i32 {
    let info = type_info(Point);
    1
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(1));
}

#[test]
fn comptime_func_basic() {
    let src = r#"
comptime func double(n: i32) -> i32 {
    n * 2
}

func main() -> i32 {
    double(5)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(10));
}

#[test]
fn comptime_func_with_type_of() {
    let src = r#"
func main() -> string {
    comptime {
        let x = 42;
        type_name(x)
    }
}
"#;
    assert_eq!(run_source(src), interp::Value::String("i32".to_string()));
}

#[test]
fn comptime_block_empty() {
    let src = r#"
func main() -> i32 {
    comptime {
    }
    42
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

// === T401: Comptime Code Generation Tests ===

#[test]
fn comptime_quote_basic() {
    let src = r#"
func main() -> i32 {
    let ast = comptime {
        quote! {
            42
        }
    };
    ast_eval(ast)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn comptime_quote_with_interpolation() {
    let src = r#"
func main() -> i32 {
    let n = 10;
    let ast = comptime {
        quote! {
            $(n + 5)
        }
    };
    ast_eval(ast)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(15));
}

#[test]
fn comptime_generate_expression() {
    let src = r#"
func main() -> i32 {
    let x = 3;
    let ast = comptime {
        quote! {
            $(x * 2)
        }
    };
    ast_eval(ast)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(6));
}

#[test]
fn comptime_ast_dump() {
    let src = r#"
func main() -> string {
    let ast = comptime {
        quote! {
            1 + 2
        }
    };
    ast_dump(ast)
}
"#;
    let result = run_source(src);
    assert!(matches!(result, interp::Value::String(_)));
}

#[test]
fn comptime_quote_with_let() {
    let src = r#"
func main() -> i32 {
    let ast = comptime {
        quote! {
            let x = 10;
            x + 5
        }
    };
    ast_eval(ast)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(15));
}

#[test]
fn comptime_runtime_mix() {
    let src = r#"
func double(n: i32) -> i32 {
    n * 2
}

func main() -> i32 {
    let val = 21;
    let result = double(val);
    comptime {
        result + 1
    }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(43));
}

// === T402: Compile-Time Function Execution Tests ===

#[test]
fn comptime_func_no_args() {
    let src = r#"
comptime func half() -> f64 {
    0.5
}

func main() -> f64 {
    half()
}
"#;
    assert_eq!(run_source(src), interp::Value::Float(0.5));
}

#[test]
fn comptime_func_constant_expression() {
    let src = r#"
comptime func max_value() -> i32 {
    2147483647
}

func main() -> i32 {
    max_value()
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(2147483647));
}

#[test]
fn comptime_func_with_args_not_precomputed() {
    let src = r#"
comptime func add(a: i32, b: i32) -> i32 {
    a + b
}

func main() -> i32 {
    add(3, 4)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(7));
}

#[test]
fn comptime_func_multiple() {
    let src = r#"
comptime func one() -> i32 {
    1
}

comptime func two() -> i32 {
    2
}

func main() -> i32 {
    one() + two()
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(3));
}

#[test]
fn comptime_func_string() {
    let src = r#"
comptime func greeting() -> string {
    "hello"
}

func main() -> string {
    greeting()
}
"#;
    assert_eq!(run_source(src), interp::Value::String("hello".to_string()));
}

// === T403: Derive Macro Tests ===

#[test]
fn derive_debug_parses() {
    let src = r#"
#[derive(Debug)]
type Point {
    x: i32
    y: i32
}

func main() -> i32 {
    42
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn derive_clone_parses() {
    let src = r#"
#[derive(Clone)]
type Point {
    x: i32
    y: i32
}

func main() -> i32 {
    42
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn derive_eq_parses() {
    let src = r#"
#[derive(Eq)]
type Point {
    x: i32
    y: i32
}

func main() -> i32 {
    42
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn derive_multiple() {
    let src = r#"
#[derive(Debug, Clone, Eq)]
type Point {
    x: i32
    y: i32
}

func main() -> i32 {
    42
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn derive_enum() {
    let src = r#"
#[derive(Debug)]
type Color { Red | Green | Blue }

func main() -> i32 {
    42
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn derive_on_actor() {
    let src = r#"
actor Counter {
    count: i32
}

func main() -> i32 {
    42
}
"#;
    assert!(check_source(src).is_ok());
}

// === T501: Standard Library Builtins Tests ===
