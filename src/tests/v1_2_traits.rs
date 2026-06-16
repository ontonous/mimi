use super::*;

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
    assert!(result.is_ok(), "trait impl should pass: {:?}", result.err());
}
