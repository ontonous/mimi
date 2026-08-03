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
    assert!(err_messages
        .iter()
        .any(|m| m.contains("missing method 'print'")));
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
    assert!(err_messages
        .iter()
        .any(|m| m.contains("undefined trait 'NonexistentTrait'")));
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
    assert_eq!(
        run_source(src),
        interp::Value::String("printed".to_string())
    );
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
    assert!(err_messages
        .iter()
        .any(|m| m.contains("where constraint violated")));
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

// ===== dyn Trait interpreter tests =====

#[test]
fn dyn_trait_coercion_basic() {
    let v = run_source(
        r#"
trait Drawable {
    func draw() -> i32;
}

type Circle {
    radius: i32
}

impl Drawable for Circle {
    func draw() -> i32 {
        self.radius * 2
    }
}

func main() -> i32 {
    let c = Circle { radius: 10 }
    let d: dyn Drawable = c
    d.draw()
}
"#,
    );
    assert_eq!(v, interp::Value::Int(20));
}

#[test]
fn dyn_trait_dispatch_return() {
    let v = run_source(
        r#"
trait Greeter {
    func greet() -> string;
}

type English {
    name: string
}

impl Greeter for English {
    func greet() -> string {
        "Hello, " + self.name
    }
}

type French {
    name: string
}

impl Greeter for French {
    func greet() -> string {
        "Bonjour, " + self.name
    }
}

func main() -> string {
    let e = English { name: "World" }
    let d: dyn Greeter = e
    d.greet()
}
"#,
    );
    assert_eq!(v, interp::Value::String("Hello, World".into()));
}

#[test]
fn dyn_trait_multi_impl() {
    let v = run_source(
        r#"
trait Calculator {
    func compute() -> i32;
}

type Adder {
    x: i32,
    y: i32
}

impl Calculator for Adder {
    func compute() -> i32 {
        self.x + self.y
    }
}

type Multiplier {
    a: i32,
    b: i32
}

impl Calculator for Multiplier {
    func compute() -> i32 {
        self.a * self.b
    }
}

func use_dyn(d: dyn Calculator) -> i32 {
    d.compute()
}

func main() -> i32 {
    let add = Adder { x: 3, y: 4 }
    let mul = Multiplier { a: 5, b: 6 }
    use_dyn(add) + use_dyn(mul)
}
"#,
    );
    assert_eq!(v, interp::Value::Int(3 + 4 + 5 * 6));
}

#[test]
fn dyn_trait_local_let_binding_multi_impl() {
    // M4 (audit-codegen 2026-08-03): a LOCAL `let d: dyn Trait = value`
    // binding with MULTIPLE impls must dispatch by the receiver's concrete
    // type at runtime. Before the fix the bytecode compiler tracked the
    // binding with infer_expr_type (the init's concrete/unknown type) instead
    // of the declared `dyn` annotation, so var_is_dyn stayed false and the
    // method call statically resolved to the FIRST impl — d2.show() on a Bar
    // printed "foo" (diverging from codegen's runtime vtable dispatch).
    let v = run_source(
        r#"
trait Show {
    func show() -> string;
}

type Foo { x: i32 }
type Bar { y: i32 }

impl Show for Foo {
    func show() -> string { "foo" }
}
impl Show for Bar {
    func show() -> string { "bar" }
}

func main() -> i32 {
    let d1: dyn Show = Foo { x: 1 }
    let d2: dyn Show = Bar { y: 2 }
    if d1.show() == "foo" && d2.show() == "bar" { 1 } else { 0 }
}
"#,
    );
    assert_eq!(
        v,
        interp::Value::Int(1),
        "dyn dispatch must follow the receiver's concrete type"
    );
}

#[test]
fn dyn_trait_local_let_binding_multi_impl_dual_backend() {
    // L1: the M4 fix must hold on both backends (codegen already dispatched
    // correctly via the runtime vtable; the bytecode VM is the side fixed).
    if !can_link() {
        return;
    }
    let src = r#"
trait Show {
    func show() -> string;
}

type Foo { x: i32 }
type Bar { y: i32 }

impl Show for Foo {
    func show() -> string { "foo" }
}
impl Show for Bar {
    func show() -> string { "bar" }
}

func main() -> i32 {
    let d1: dyn Show = Foo { x: 1 }
    let d2: dyn Show = Bar { y: 2 }
    println(d1.show())
    println(d2.show())
    0
}
"#;
    let (_, interp_out) = run_source_with_stdout(src);
    assert_eq!(interp_out.trim(), "foo\nbar", "bytecode dyn dispatch");
    let native = compile_and_run(src).expect("codegen must run dyn dispatch");
    assert_eq!(native.trim(), "foo\nbar", "codegen dyn dispatch");
}

#[test]
fn adt_trait_method_with_self() {
    let v = run_source(
        r#"
trait Move {
    func shift(dx: i32, dy: i32) -> Point
}

type Point {
    Pt(i32, i32)
}

impl Move for Point {
    func shift(dx: i32, dy: i32) -> Point {
        match self {
            Pt(x, y) => Pt(x + dx, y + dy)
        }
    }
}

func main() -> i32 {
    let p = Pt(1, 2)
    let q = p.shift(3, 4)
    match q {
        Pt(x, y) => x + y
    }
}
"#,
    );
    assert_eq!(v, interp::Value::Int(10));
}
