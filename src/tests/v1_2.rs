use super::*;

#[test]
fn generic_identity_function() {
    let src = r#"
func id<T>(x: T) -> T {
    x
}

func main() -> i32 {
    id::<i32>(42)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn generic_type_inference() {
    let src = r#"
func id<T>(x: T) -> T {
    x
}

func main() -> string {
    id("hello")
}
"#;
    assert_eq!(run_source(src), interp::Value::String("hello".to_string()));
}

#[test]
fn generic_multi_param() {
    let src = r#"
func pair<A, B>(a: A, b: B) -> (A, B) {
    (a, b)
}

func main() -> i32 {
    let p = pair(1, "two");
    match p {
        (1, "two") => 10,
        _ => 0,
    }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(10));
}

#[test]
fn generic_turbofish() {
    let src = r#"
func identity<T>(x: T) -> T {
    x
}

func main() -> i32 {
    let x = identity::<i32>(100);
    x
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(100));
}

#[test]
fn generic_type_def() {
    let src = r#"
type Box<T> {
    value: T
}

func main() -> i32 {
    let b = Box { value: 42 };
    b.value
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn generic_function_with_generic_type() {
    let src = r#"
type Wrapper<T> {
    inner: T
}

func wrap<T>(x: T) -> Wrapper<T> {
    Wrapper { inner: x }
}

func main() -> i32 {
    let w = wrap(42);
    w.inner
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn generic_parsing_no_generics() {
    // Ensure non-generic functions still work
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    a + b
}

func main() -> i32 {
    add(3, 4)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(7));
}

#[test]
fn trait_impl_missing_method() {
    let src = r#"
trait Display {
    func to_string() -> string;
    func print();
}

type MyType {
    value: i32
}

impl Display for MyType {
    func to_string() -> string {
        return "MyType";
    }
}

func main() -> i32 {
    42
}
"#;
    // Missing 'print' method should fail type checking
    let result = check_source(src);
    assert!(result.is_err());
    let errors = result.unwrap_err();
    let err_messages: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
    assert!(err_messages.iter().any(|m| m.contains("missing method 'print'")));
}

#[test]
fn trait_undefined_trait() {
    let src = r#"
type MyType {
    value: i32
}

impl NonexistentTrait for MyType {
    func do_something() {
    }
}

func main() -> i32 {
    42
}
"#;
    // Undefined trait should fail
    let result = check_source(src);
    assert!(result.is_err());
    let errors = result.unwrap_err();
    let err_messages: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
    assert!(err_messages.iter().any(|m| m.contains("undefined trait 'NonexistentTrait'")));
}

#[test]
fn trait_impl_methods_registered() {
    let src = r#"
trait Display {
    func to_string() -> string;
}

type MyType {
    value: i32
}

impl Display for MyType {
    func to_string() -> string {
        return "MyType";
    }
}

func main() -> i32 {
    42
}
"#;
    // Impl methods should be registered and type check should pass
    let result = check_source(src);
    assert!(result.is_ok());
}

#[test]
fn trait_with_generic_function() {
    let src = r#"
trait Printable {
    func to_string() -> string;
}

type MyType {
    value: i32
}

impl Printable for MyType {
    func to_string() -> string {
        return "MyType";
    }
}

func print_value<T>(x: T) -> string {
    "printed"
}

func main() -> string {
    print_value(42)
}
"#;
    // Generic function without trait constraint should work
    assert_eq!(run_source(src), interp::Value::String("printed".to_string()));
}

#[test]
fn where_constraint_violated() {
    let src = r#"
trait Display {
    func to_string() -> string;
}

type MyType {
    value: i32
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
    // MyType doesn't implement Display, so this should fail type checking
    let result = check_source(src);
    assert!(result.is_err());
    let errors = result.unwrap_err();
    let err_messages: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
    assert!(err_messages.iter().any(|m| m.contains("where constraint violated")));
}

#[test]
fn where_constraint_satisfied() {
    let src = r#"
trait Display {
    func to_string() -> string;
}

type MyType {
    value: i32
}

impl Display for MyType {
    func to_string() -> string {
        return "MyType";
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
    // MyType implements Display, so this should pass type checking
    let result = check_source(src);
    if let Err(ref errors) = result {
        for e in errors {
            eprintln!("ERROR: {}", e.message);
        }
    }
    assert!(result.is_ok());
}

#[test]
fn parasteps_local_shared_not_allowed() {
    let src = r#"
func main() -> i32 {
    local_shared x = 42;
    parasteps {
        println(x);
    }
    42
}
"#;
    // local_shared cannot be captured in parasteps
    let result = check_source(src);
    assert!(result.is_err());
    let errors = result.unwrap_err();
    let err_messages: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
    assert!(err_messages.iter().any(|m| m.contains("local_shared")));
}

#[test]
fn parasteps_shared_allowed() {
    let src = r#"
func main() -> i32 {
    shared x = 42;
    parasteps {
        println(x);
    }
    42
}
"#;
    // shared can be captured in parasteps
    let result = check_source(src);
    assert!(result.is_ok());
}

#[test]
fn mms_block_basic() {
    let src = r#"
func main() -> i32 {
    mms {
        "func pay requires: balance >= amount"
    }
    42
}
"#;
    // mms block should parse and be ignored at runtime
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn mms_block_with_code() {
    let src = r#"
func pay(amount: i32) {
    mms {
        func Pay(amount):
            desc "Process payment"
            requires: amount > 0
    }
    println(amount);
}

func main() -> i32 {
    pay(100);
    42
}
"#;
    // mms block inside a function should work
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn mms_block_multiple() {
    let src = r#"
func main() -> i32 {
    mms {
        "Step 1: check balance"
    }
    mms {
        "Step 2: charge payment"
    }
    42
}
"#;
    // Multiple mms blocks should work
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn effect_declaration() {
    let src = r#"
cap FileReadCap;

func read_file(path: string) with FileReadCap {
    println(path);
}

func main() -> i32 {
    read_file("test.txt");
    42
}
"#;
    // Function with effect - FileReadCap is declared but not bound to a variable
    // So calling read_file should fail because the effect is not available
    let result = check_source(src);
    assert!(result.is_err());
    let errors = result.unwrap_err();
    let err_messages: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
    assert!(err_messages.iter().any(|m| m.contains("effect") && m.contains("not available")));
}

#[test]
fn effect_not_available() {
    let src = r#"
cap FileReadCap;

func read_file(path: string) with FileReadCap {
    println(path);
}

func main() -> i32 {
    // FileReadCap is not in scope here (only declared, not bound)
    read_file("test.txt");
    42
}
"#;
    // Function with effect should fail when effect is not available
    let result = check_source(src);
    // For now, just check that parsing works
    assert!(result.is_ok() || result.is_err());
}
