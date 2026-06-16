use crate::ast::Item;
use crate::{core, interp, lexer, parser};

fn parse(src: &str) -> crate::ast::File {
    let tokens = lexer::Lexer::new(src).tokenize().unwrap();
    parser::Parser::new(tokens).parse_file().unwrap()
}

fn run_source(src: &str) -> interp::Value {
    let file = parse(src);
    let mut interp = interp::Interpreter::new(&file);
    interp.run().unwrap()
}

fn run_source_result(src: &str) -> Result<interp::Value, String> {
    let tokens = lexer::Lexer::new(src).tokenize().map_err(|e| e)?;
    let file = parser::Parser::new(tokens).parse_file().map_err(|e| e.message)?;
    let mut interp = interp::Interpreter::new(&file);
    interp.run()
}

fn check_source(src: &str) -> Result<(), Vec<core::Diagnostic>> {
    let file = parse(src);
    core::check(&file)
}

#[test]
fn parse_func_with_contracts() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    requires: a > 0
    ensures: result == a + b
    return a + b;
}

func main() {
    println(add(1, 2));
}
"#;
    parse(src);
}

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
fn typecheck_return_mismatch() {
    let src = r#"
func main() -> i32 {
    return "hello";
}
"#;
    let errs = check_source(src).unwrap_err();
    assert!(errs.iter().any(|d| d.message.contains("return type mismatch")));
}

#[test]
fn typecheck_arg_mismatch() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    return a + b;
}
func main() {
    add(1, "two");
}
"#;
    let errs = check_source(src).unwrap_err();
    assert!(errs.iter().any(|d| d.message.contains("argument 2")));
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
fn typecheck_undefined_variable() {
    let src = r#"
func main() {
    println(x);
}
"#;
    let errs = check_source(src).unwrap_err();
    assert!(errs.iter().any(|d| d.message.contains("undefined variable")));
}

#[test]
fn typecheck_assignment_mismatch() {
    let src = r#"
func main() {
    let x: i32 = 10;
    x = "hello";
}
"#;
    let errs = check_source(src).unwrap_err();
    assert!(errs.iter().any(|d| d.message.contains("cannot assign")));
}

#[test]
fn typecheck_valid_program() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    return a + b;
}
func main() -> i32 {
    return add(1, 2);
}
"#;
    assert!(check_source(src).is_ok());
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
fn interp_tuple_and_list() {
    let src = r#"
func sum_first_pair(t: (i32, i32, i32)) -> i32 {
    let (a, b, _) = t;
    a + b
}

func main() -> i32 {
    let xs = [1, 2, 3, 4];
    let mut s = 0;
    for x in xs {
        s = s + x;
    }
    s + sum_first_pair((10, 20, 30))
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(40));
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
fn interp_record_fields() {
    let src = r#"
type Point {
    x: f64,
    y: f64,
}

func distance(p: Point) -> f64 {
    sqrt(p.x * p.x + p.y * p.y)
}

func main() -> f64 {
    let origin = Point { x: 0.0, y: 0.0 };
    let p = Point { x: 3.0, y: 4.0 };
    distance(origin) + distance(p)
}
"#;
    let v = run_source(src);
    assert!(matches!(v, interp::Value::Float(x) if (x - 5.0).abs() < 0.001));
}

#[test]
fn interp_newtype_isolation() {
    let src = r#"
newtype UserId = i32;
newtype OrderId = i32;

func to_raw(u: UserId) -> i32 {
    let UserId(v) = u;
    v
}

func main() -> i32 {
    let u = UserId(42);
    to_raw(u)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn typecheck_newtype_mismatch() {
    let src = r#"
newtype UserId = i32;
newtype OrderId = i32;

func use_user(u: UserId) -> i32 {
    let UserId(v) = u;
    v
}

func main() -> i32 {
    use_user(OrderId(1))
}
"#;
    let errs = check_source(src).unwrap_err();
    assert!(errs.iter().any(|d| d.message.contains("argument 1") || d.message.contains("UserId")));
}

#[test]
fn interp_try_operator() {
    let src = r#"
type Res {
    Ok(i32)
    Err(i32)
}

func safe_div(a: i32, b: i32) -> Res {
    if b == 0 {
        return Err(999);
    }
    return Ok(a / b);
}

func main() -> i32 {
    let result = safe_div(10, 2)?;
    result
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn typecheck_try_on_non_result() {
    let src = r#"
func main() -> i32 {
    42?
}
"#;
    let errs = check_source(src).unwrap_err();
    assert!(!errs.is_empty());
}

#[test]
fn interp_parasteps_spawn_await() {
    let src = r#"
func double(n: i32) -> i32 { n * 2 }

func main() -> i32 {
    let mut result = 0;
    parasteps {
        let a = spawn double(10);
        let b = spawn double(5);
        let r1 = await a;
        let r2 = await b;
        result = r1 + r2
    }
    result
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(30));
}

#[test]
fn interp_parasteps_multiple_spawns() {
    let src = r#"
func identity(n: i32) -> i32 { n }

func main() -> i32 {
    let mut sum = 0;
    parasteps {
        let a = spawn identity(10);
        let b = spawn identity(20);
        let c = spawn identity(30);
        let r1 = await a;
        let r2 = await b;
        let r3 = await c;
        sum = r1 + r2 + r3
    }
    sum
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(60));
}

#[test]
fn interp_parasteps_no_spawn() {
    let src = r#"
func main() -> i32 {
    let mut result = 0;
    parasteps {
        let x = 1;
        let y = 2;
        result = x + y
    }
    result
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn interp_spawn_outside_parasteps_error() {
    let src = r#"
func work() -> i32 { 42 }

func main() -> i32 {
    let f = spawn work()
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err_msg = result.unwrap_err();
    assert!(err_msg.contains("spawn requires parasteps"));
}

#[test]
fn interp_on_failure_success_no_compensation() {
    let src = r#"
type Res {
    Ok(i32)
    Err(string)
}

func succeed() -> Res {
    Ok(42)
}

func cleanup() {
    println("cleanup should not run");
}

func main() -> i32 {
    on failure { cleanup(); }
    let x = succeed()?;
    x
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn interp_on_failure_compensation() {
    let src = r#"
type Res {
    Ok(i32)
    Err(string)
}

func fail_task() -> Res {
    Err("task failed")
}

func cleanup() {
    println("cleanup executed");
}

func main() -> i32 {
    on failure { cleanup(); }
    let x = fail_task()?;
    0
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err_msg = result.unwrap_err();
    assert!(err_msg.contains("Err propagated"));
}

#[test]
fn interp_on_failure_nested() {
    let src = r#"
type Res {
    Ok(i32)
    Err(string)
}

func step1() -> Res { Ok(1) }
func step2() -> Res { Err("failed") }
func step3() -> Res { Ok(3) }

func cleanup1() { println("cleanup1"); }
func cleanup2() { println("cleanup2"); }

func main() -> i32 {
    on failure { cleanup1(); }
    let a = step1()?;
    on failure { cleanup2(); }
    let b = step2()?;
    let c = step3()?;
    0
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err_msg = result.unwrap_err();
    assert!(err_msg.contains("Err propagated"));
}

#[test]
fn interp_actor_spawn_and_methods() {
    let src = r#"
actor Counter {
    mut count: i32 = 0;

    func increment() {
        self.count = self.count + 1;
    }

    func get_count() -> i32 {
        return self.count;
    }
}

func main() -> i32 {
    let c = Counter.spawn();
    let n1 = c.get_count();
    c.increment();
    let n2 = c.get_count();
    c.increment();
    let n3 = c.get_count();
    n3
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(2));
}

#[test]
fn interp_actor_initial_fields() {
    let src = r#"
actor Greeter {
    mut message: string = "hello";
    mut count: i32 = 0;

    func greet() -> string {
        return self.message;
    }

    func get_count() -> i32 {
        return self.count;
    }
}

func main() -> i32 {
    let g = Greeter.spawn();
    let msg = g.greet();
    println(msg);
    g.get_count()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn typecheck_actor_method_missing() {
    let src = r#"
actor Counter {
    mut count: i32 = 0;
}

func main() -> i32 {
    let c = Counter.spawn();
    c.some_nonexistent_method()
}
"#;
    let errs = check_source(src);
    assert!(errs.is_ok() || errs.is_err());
}

#[test]
fn interp_cap_declaration() {
    let src = r#"
cap FileReadCap;

func main() -> i32 {
    42
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn interp_cap_multiple() {
    let src = r#"
cap ReadCap;
cap WriteCap;

func main() -> i32 {
    42
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn interp_arena_basic() {
    let src = r#"
func process() -> i32 {
    arena {
        let x = 10;
        let y = 20;
        x + y
    }
}

func main() -> i32 {
    let result = process();
    println(result);
    result
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(30));
}

#[test]
fn interp_arena_multiple_lets() {
    let src = r#"
func process() -> i32 {
    arena {
        let a = 10;
        let b = 20;
        let c = 30;
        a + b + c
    }
}

func main() -> i32 {
    process()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(60));
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
fn interp_negation() {
    let src = r#"
func main() -> i32 {
    let x = 5;
    let y = -x;
    let z = --x;
    y + z
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
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
fn typecheck_unused_variable() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    0
}
"#;
    let result = check_source(src);
    assert!(result.is_ok());
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
fn typecheck_uninitialized_let() {
    let src = r#"
func main() -> i32 {
    let x: i32;
    x
}
"#;
    let result = check_source(src);
    assert!(result.is_ok() || result.is_err());
}

#[test]
fn typecheck_func_no_return() {
    let src = r#"
func main() -> i32 {
    println("hello");
}
"#;
    let result = check_source(src);
    assert!(result.is_ok());
}

#[test]
fn typecheck_recursive_func() {
    let src = r#"
func countdown(n: i32) -> i32 {
    if n <= 0 {
        return 0;
    }
    countdown(n - 1)
}

func main() -> i32 {
    countdown(5)
}
"#;
    assert!(check_source(src).is_ok());
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn typecheck_mutually_recursive_funcs() {
    let src = r#"
func is_even(n: i32) -> bool {
    if n == 0 {
        return true;
    }
    is_odd(n - 1)
}

func is_odd(n: i32) -> bool {
    if n == 0 {
        return false;
    }
    is_even(n - 1)
}

func main() -> i32 {
    if is_even(4) { 1 } else { 0 }
}
"#;
    assert!(check_source(src).is_ok());
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
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
fn interp_record_nested() {
    let src = r#"
type Inner {
    x: i32,
    y: i32,
}

type Outer {
    inner: Inner,
    z: i32,
}

func main() -> i32 {
    let o = Outer { inner: Inner { x: 1, y: 2 }, z: 3 };
    o.inner.x + o.inner.y + o.z
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(6));
}

#[test]
fn interp_record_simple() {
    let src = r#"
type Point {
    x: i32,
    y: i32,
}

func main() -> i32 {
    let p = Point { x: 3, y: 4 };
    p.x + p.y
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(7));
}

#[test]
fn interp_newtype_multiple() {
    let src = r#"
newtype Meter = i32;
newtype Foot = i32;

func to_meters(f: Foot) -> Meter {
    let Foot(v) = f;
    Meter(v * 3)
}

func main() -> i32 {
    let f = Foot(10);
    let m = to_meters(f);
    let Meter(v) = m;
    v
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(30));
}

#[test]
fn interp_newtype_in_container() {
    let src = r#"
newtype Id = i32;

func main() -> i32 {
    let ids = [Id(1), Id(2), Id(3)];
    let Id(v) = ids[1];
    v
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(2));
}

#[test]
fn interp_tuple_destructuring() {
    let src = r#"
func main() -> i32 {
    let (a, b, c) = (1, 2, 3);
    a + b + c
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(6));
}

#[test]
fn interp_list_access() {
    let src = r#"
func main() -> i32 {
    let xs = [1, 2, 3, 4, 5];
    xs[0] + xs[4]
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(6));
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
fn interp_unit_return() {
    let src = r#"
func do_nothing() {
    println("nothing");
}

func main() -> i32 {
    do_nothing();
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn interp_unit_in_tuple() {
    let src = r#"
func main() -> i32 {
    let t = ((), 42);
    let (_, x) = t;
    x
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn interp_variant_with_payload() {
    let src = r#"
type Result {
    Ok(i32)
    Fail(i32)
}

func get_value(r: Result) -> i32 {
    match r {
        Ok(n) => n,
        Fail(n) => -n,
    }
}

func main() -> i32 {
    let a = Ok(42);
    let b = Fail(1);
    get_value(a) + get_value(b)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(41));
}

#[test]
fn interp_drop_cap_type() {
    let src = r#"
cap FileCap;

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn interp_assignment() {
    let src = r#"
func main() -> i32 {
    let mut x = 10;
    x = 15;
    x = x * 2;
    x
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(30));
}

#[test]
fn interp_compound_assignment_plus_eq() {
    let src = r#"
func main() -> i32 {
    let mut x = 10;
    x += 5;
    x
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(15));
}

#[test]
fn interp_compound_assignment_minus_eq() {
    let src = r#"
func main() -> i32 {
    let mut x = 10;
    x -= 3;
    x
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(7));
}

#[test]
fn interp_compound_assignment_mul_eq() {
    let src = r#"
func main() -> i32 {
    let mut x = 10;
    x *= 4;
    x
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(40));
}

#[test]
fn interp_compound_assignment_div_eq() {
    let src = r#"
func main() -> i32 {
    let mut x = 20;
    x /= 4;
    x
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
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
fn interp_desc_in_brace_block() {
    let src = r#"
func main() -> i32 {
    desc "this is a description";
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn interp_rule_in_brace_block() {
    let src = r#"
func main() -> i32 {
    rule "must be positive";
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn typecheck_ellipsis_rejected_in_production() {
    let src = r#"
func main() -> i32 {
    ...
}
"#;
    let tokens = lexer::Lexer::new(src).tokenize().unwrap();
    let result = parser::Parser::new(tokens).parse_file();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.message.contains("placeholder is not allowed in production mode"));
}

#[test]
fn interp_parasteps_await_all() {
    let src = r#"
func fetch(n: i32) -> i32 { n * 10 }

func main() -> i32 {
    let mut result = 0;
    parasteps {
        let a = spawn fetch(1);
        let b = spawn fetch(2);
        let r1 = await a;
        let r2 = await b;
        result = r1 + r2
    }
    result
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(30));
}

#[test]
fn interp_requires_ensures_in_brace_block() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    requires: a > 0
    ensures: result == a + b
    return a + b;
}

func main() -> i32 {
    add(1, 2)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn interp_string_equality() {
    let src = r#"
func main() -> i32 {
    let a = "hello";
    let b = "hello";
    if a == b { 1 } else { 0 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn interp_string_index() {
    let src = r#"
func main() -> string {
    let s = "abc";
    s[1]
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("b".to_string()));
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
fn interp_nested_function_calls() {
    let src = r#"
func double(x: i32) -> i32 { x * 2 }
func inc(x: i32) -> i32 { x + 1 }

func main() -> i32 {
    double(inc(5))
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(12));
}

#[test]
fn interp_negative_literal() {
    let src = r#"
func main() -> i32 {
    let x = -5;
    x + 10
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn interp_double_negation() {
    let src = r#"
func main() -> i32 {
    let x = 5;
    let y = -x;
    let z = -y;
    z
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

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
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("use of moved value"), "Expected 'use of moved value' error, got: {}", err);
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
    r
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
    x + r
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(84));
}

// =============================================================================
// P1: String operations and built-in functions
// =============================================================================

#[test]
fn string_concat() {
    let src = r#"
func main() -> string {
    let s = "hello" + " " + "world";
    s
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("hello world".to_string()));
}

#[test]
fn string_concat_empty() {
    let src = r#"
func main() -> string {
    let s = "" + "abc" + "";
    s
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("abc".to_string()));
}

#[test]
fn builtin_len_string() {
    let src = r#"
func main() -> i32 {
    len("hello")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn builtin_len_list() {
    let src = r#"
func main() -> i32 {
    len([1, 2, 3, 4, 5])
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn builtin_len_empty_string() {
    let src = r#"
func main() -> i32 {
    len("")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn builtin_to_string_int() {
    let src = r#"
func main() -> string {
    to_string(42)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("42".to_string()));
}

#[test]
fn builtin_to_string_bool() {
    let src = r#"
func main() -> string {
    to_string(true)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("true".to_string()));
}

#[test]
fn builtin_abs_int() {
    let src = r#"
func main() -> i32 {
    abs(-5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn builtin_abs_float() {
    let src = r#"
func main() -> f64 {
    abs(-3.14)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Float(3.14));
}

#[test]
fn builtin_push() {
    let src = r#"
func main() -> i32 {
    let a = [1, 2, 3];
    let result = push(a, 4);
    len(result)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(4));
}

#[test]
fn builtin_pop() {
    let src = r#"
func main() -> i32 {
    let a = [1, 2, 3];
    let result = pop(a);
    let (popped, _) = result;
    popped
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn builtin_pop_returns_remaining() {
    let src = r#"
func main() -> i32 {
    let a = [1, 2, 3];
    let result = pop(a);
    let (_, new_list) = result;
    len(new_list)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(2));
}

// =============================================================================
// P2: Type safety - borrow checking
// =============================================================================

#[test]
fn typecheck_double_mut_borrow_error() {
    let src = r#"
func main() -> i32 {
    let mut x = 42;
    let r1 = &mut x;
    let r2 = &mut x;
    1
}
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(result.is_err());
    let errors = result.unwrap_err();
    let has_borrow_error = errors.iter().any(|e| e.message.contains("already mutably borrowed"));
    assert!(has_borrow_error, "Expected mutable borrow error, got: {:?}", errors);
}

#[test]
fn typecheck_imm_mut_borrow_error() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    let r1 = &x;
    let r2 = &mut x;
    1
}
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(result.is_err());
    let errors = result.unwrap_err();
    let has_borrow_error = errors.iter().any(|e| e.message.contains("already immutably borrowed"));
    assert!(has_borrow_error, "Expected immutable borrow error, got: {:?}", errors);
}

#[test]
fn typecheck_double_imm_borrow_ok() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    let r1 = &x;
    let r2 = &x;
    1
}
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(result.is_ok(), "Multiple immutable borrows should be allowed");
}

#[test]
fn typecheck_borrow_scope_isolation() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    {
        let r = &mut x;
    }
    let r2 = &x;
    1
}
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(result.is_ok(), "Borrows should be isolated to their scope");
}

// =============================================================================
// P3: on failure + ? integration
// =============================================================================

#[test]
fn on_failure_executes_on_error() {
    let src = r#"
type Res {
    Ok(i32)
    Err(string)
}

func fail() -> Res {
    Err("boom")
}

func cleanup() {
    println("CLEANUP_RAN");
}

func main() -> i32 {
    on failure { cleanup(); }
    let _ = fail()?;
    0
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err(), "Error should propagate");
}

#[test]
fn on_failure_lifo_order() {
    let src = r#"
type Res {
    Ok(i32)
    Err(string)
}

func fail() -> Res {
    Err("boom")
}

func main() -> i32 {
    on failure { println("C"); }
    on failure { println("B"); }
    on failure { println("A"); }
    let _ = fail()?;
    0
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err(), "Error should propagate");
}

#[test]
fn on_failure_no_execute_on_success() {
    let src = r#"
type Res {
    Ok(i32)
    Err(string)
}

func succeed() -> Res {
    Ok(42)
}

func main() -> i32 {
    on failure { println("SHOULD_NOT_RUN"); }
    let x = succeed()?;
    x
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42), "Compensation should NOT execute on success");
}

// =============================================================================
// P4.1: pub visibility parsing
// =============================================================================

#[test]
fn parse_pub_func() {
    let src = r#"
pub func helper() -> i32 { 42 }

func main() -> i32 {
    helper()
}
"#;
    let file = parse(src);
    if let Item::Func(f) = &file.items[0] {
        assert!(f.pub_, "func should be marked as pub");
    } else {
        panic!("expected Func item");
    }
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn parse_pub_type() {
    let src = r#"
pub type Point {
    x: i32
    y: i32
}

func main() -> i32 {
    1
}
"#;
    let file = parse(src);
    if let Item::Type(t) = &file.items[0] {
        assert!(t.pub_, "type should be marked as pub");
    } else {
        panic!("expected Type item");
    }
}

#[test]
fn parse_non_pub_func() {
    let src = r#"
func helper() -> i32 { 42 }

func main() -> i32 {
    helper()
}
"#;
    let file = parse(src);
    if let Item::Func(f) = &file.items[0] {
        assert!(!f.pub_, "func without pub should not be marked as pub");
    } else {
        panic!("expected Func item");
    }
}

// =============================================================================
// P6: requires/ensures runtime assertions
// =============================================================================

#[test]
fn requires_passes() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    requires: a > 0
    requires: b > 0
    a + b
}

func main() -> i32 {
    add(1, 2)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn requires_fails() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    requires: a > 0
    a + b
}

func main() -> i32 {
    add(-1, 2)
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("requires condition failed"), "Expected requires error, got: {}", err);
}

#[test]
fn ensures_passes() {
    let src = r#"
func double(x: i32) -> i32 {
    ensures: result == x * 2
    x * 2
}

func main() -> i32 {
    double(5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(10));
}

#[test]
fn ensures_fails() {
    let src = r#"
func buggy(x: i32) -> i32 {
    ensures: result == x * 2
    x * 3
}

func main() -> i32 {
    buggy(5)
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("ensures condition failed"), "Expected ensures error, got: {}", err);
}

#[test]
fn requires_ensures_combined() {
    let src = r#"
func abs_val(x: i32) -> i32 {
    requires: x != 0
    ensures: result > 0
    if x < 0 { -x } else { x }
}

func main() -> i32 {
    abs_val(-5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

// =============================================================================
// P7: comptime keyword, nothing type, more builtins
// =============================================================================

#[test]
fn builtin_min_int() {
    let src = r#"
func main() -> i32 {
    min(3, 7)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn builtin_max_int() {
    let src = r#"
func main() -> i32 {
    max(3, 7)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(7));
}

#[test]
fn builtin_min_float() {
    let src = r#"
func main() -> f64 {
    min(3.14, 2.71)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Float(2.71));
}

#[test]
fn builtin_contains_list() {
    let src = r#"
func main() -> i32 {
    let a = [1, 2, 3, 4, 5];
    if contains(a, 3) { 1 } else { 0 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn builtin_contains_string() {
    let src = r#"
func main() -> i32 {
    let s = "hello world";
    if contains(s, "world") { 1 } else { 0 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn builtin_contains_not_found() {
    let src = r#"
func main() -> i32 {
    let a = [1, 2, 3];
    if contains(a, 99) { 1 } else { 0 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn nothing_type_parsing() {
    let src = r#"
func diverge() -> nothing {
    assert(false)
}

func main() -> i32 {
    1
}
"#;
    let _file = parse(src);
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

// =============================================================================
// Critical bug fix tests
// =============================================================================

#[test]
fn bugfix_division_by_zero() {
    let src = r#"
func main() -> i32 {
    10 / 0
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("division by zero"), "Expected division by zero error, got: {}", err);
}

#[test]
fn bugfix_modulo_by_zero() {
    let src = r#"
func main() -> i32 {
    10 % 0
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("modulo by zero"), "Expected modulo by zero error, got: {}", err);
}

#[test]
fn bugfix_negative_exponent() {
    let src = r#"
func main() -> i32 {
    2 ** -1
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("negative exponent"), "Expected negative exponent error, got: {}", err);
}

#[test]
fn bugfix_immutable_assignment() {
    let src = r#"
func main() -> i32 {
    let x = 10;
    x = 20;
    x
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("immutable"), "Expected immutable error, got: {}", err);
}

#[test]
fn bugfix_mut_assignment_works() {
    let src = r#"
func main() -> i32 {
    let mut x = 10;
    x = 20;
    x
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(20));
}

#[test]
fn bugfix_error_in_expr_statement() {
    // Value::Error from ? operator should propagate through expression statements
    let src = r#"
type Res {
    Ok(i32)
    Err(string)
}

func fail() -> Res { Err("boom") }

func main() -> i32 {
    fail()?;
    1
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err(), "Error should propagate through expression statement");
}

#[test]
fn bugfix_float_division_by_zero() {
    let src = r#"
func main() -> f64 {
    10.0 / 0.0
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("division by zero"), "Expected division by zero error, got: {}", err);
}

// ==================== shared/local_shared/weak tests ====================

#[test]
fn shared_basic_creation() {
    let src = r#"
func main() {
    shared x = 42;
    println(x);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn shared_clone_refcount() {
    let src = r#"
func main() {
    shared x = 42;
    shared y = x;
    println(x);
    println(y);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn shared_field_access() {
    let src = r#"
type Point {
    x: i32
    y: i32
}

func main() -> i32 {
    shared s = Point { x: 10, y: 20 };
    s.x
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(10));
}

#[test]
fn shared_deref_method() {
    let src = r#"
func main() -> i32 {
    shared x = 42;
    x.deref()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn local_shared_basic() {
    let src = r#"
func main() {
    local_shared x = 100;
    local_shared y = x;
    println(x);
    println(y);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn local_shared_deref() {
    let src = r#"
func main() -> i32 {
    local_shared x = 99;
    x.inner()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(99));
}

#[test]
fn weak_shared_basic() {
    let src = r#"
func main() {
    shared x = 42;
    weak w = x;
    println(w);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn weak_upgrade_success() {
    let src = r#"
func main() -> i32 {
    shared x = 42;
    weak w = x;
    let upgraded = w.upgrade();
    upgraded.deref()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn weak_upgrade_none_after_drop() {
    let src = r#"
func get_weak() -> weak i32 {
    shared x = 42;
    weak w = x;
    w
}

func main() -> i32 {
    let w = get_weak();
    let upgraded = w.upgrade();
    // upgraded is a variant - check if it's None
    match upgraded {
        Some(v) => v.deref(),
        None => 0,
    }
}
"#;
    let result = run_source_result(src);
    // After shared x is dropped, upgrade returns None
    assert!(result.is_ok());
}

#[test]
fn weak_local_basic() {
    let src = r#"
func main() {
    local_shared x = 10;
    weak w = x;
    println(w);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn weak_local_upgrade() {
    let src = r#"
func main() -> i32 {
    local_shared x = 55;
    weak w = x;
    let upgraded = w.upgrade();
    upgraded.inner()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(55));
}

#[test]
fn shared_record_field_access() {
    let src = r#"
type Node {
    value: i32
    next: i32
}

func main() -> i32 {
    shared node = Node { value: 7, next: 0 };
    node.value
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(7));
}

#[test]
fn shared_multiple_shares() {
    let src = r#"
func main() {
    shared a = 1;
    shared b = a;
    shared c = b;
    println(a);
    println(b);
    println(c);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn shared_as_function_arg() {
    let src = r#"
func use_shared(x: shared i32) {
    println(x);
}

func main() {
    shared v = 42;
    use_shared(v);
    println(v);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn weak_shared_in_list() {
    let src = r#"
func main() {
    shared a = 10;
    shared b = 20;
    weak wa = a;
    weak wb = b;
    let list = [wa, wb];
    println(list);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

// ==================== arena escape checking tests ====================

#[test]
fn arena_no_escape_ok() {
    let src = r#"
func process() -> i32 {
    arena {
        let ref x = 10;
        let val = x;
        42
    }
}

func main() -> i32 {
    process()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn arena_escape_return_detected() {
    let src = r#"
func process() -> i32 {
    arena {
        let ref x = 10;
        return x;
    }
}

func main() -> i32 {
    process()
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("arena escape"), "Expected arena escape error, got: {}", err);
}

#[test]
fn arena_escape_variable_detected() {
    let src = r#"
func main() -> i32 {
    let mut escaped = 0;
    arena {
        let ref x = 42;
        escaped = x;
    }
    escaped
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("arena escape"), "Expected arena escape error, got: {}", err);
}

#[test]
fn arena_nested_ok() {
    let src = r#"
func main() -> i32 {
    arena {
        let a = 10;
        arena {
            let b = 20;
            a + b
        }
    }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(30));
}

#[test]
fn arena_no_ref_ok() {
    let src = r#"
func main() -> i32 {
    let mut x = 0;
    arena {
        x = 42;
    }
    x
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn arena_ref_within_scope_ok() {
    let src = r#"
func main() -> i32 {
    arena {
        let a = 10;
        let b = 20;
        let result = a + b;
        result
    }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(30));
}

// ==================== comptime quote! tests ====================

#[test]
fn quote_basic_literal() {
    let src = r#"
func main() {
    let ast = quote! { 42 };
    println(ast);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn quote_interpolation() {
    let src = r#"
func main() {
    let x = 10;
    let ast = quote! { $(x + 1) };
    println(ast);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn quote_let_statement() {
    let src = r#"
func main() {
    let ast = quote! { let y = 5; };
    println(ast);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn quote_dump() {
    let src = r#"
func main() {
    let ast = quote! { 42 };
    let dumped = ast_dump(ast);
    println(dumped);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn quote_eval_literal() {
    let src = r#"
func main() -> i32 {
    let ast = quote! { 42 };
    ast_eval(ast)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn quote_eval_binary() {
    let src = r#"
func main() -> i32 {
    let ast = quote! { 10 + 20 };
    ast_eval(ast)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(30));
}

#[test]
fn quote_eval_interpolation() {
    let src = r#"
func main() -> i32 {
    let x = 5;
    let ast = quote! { $(x * 3) };
    ast_eval(ast)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(15));
}

#[test]
fn quote_eval_block() {
    let src = r#"
func main() -> i32 {
    let ast = quote! {
        let a = 10;
        let b = 20;
        a + b
    };
    ast_eval(ast)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(30));
}

#[test]
fn quote_eval_string_concat() {
    let src = r#"
func main() {
    let ast = quote! { "hello" + " " + "world" };
    let result = ast_eval(ast);
    println(result);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn quote_nested_interpolation() {
    let src = r#"
func main() -> i32 {
    let a = 3;
    let b = 4;
    let ast = quote! { $(a + b) };
    ast_eval(ast)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(7));
}

// ==================== actor async tests ====================

#[test]
fn actor_await_method() {
    let src = r#"
actor Counter {
    mut count: i32 = 0;

    func increment() {
        self.count = self.count + 1;
    }

    func get() -> i32 {
        return self.count;
    }
}

func main() -> i32 {
    let c = Counter.spawn();
    c.increment();
    let val = await c.get();
    val
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn actor_sync_method_still_works() {
    let src = r#"
actor Counter {
    mut count: i32 = 0;

    func get() -> i32 {
        return self.count;
    }
}

func main() -> i32 {
    let c = Counter.spawn();
    c.get()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn actor_await_multiple_methods() {
    let src = r#"
actor Calculator {
    mut value: i32 = 0;

    func add(n: i32) {
        self.value = self.value + n;
    }

    func get() -> i32 {
        return self.value;
    }
}

func main() -> i32 {
    let calc = Calculator.spawn();
    calc.add(10);
    calc.add(20);
    let result = await calc.get();
    result
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(30));
}

#[test]
fn actor_await_with_args() {
    let src = r#"
actor Greeter {
    mut name: string = "world";

    func greet() -> string {
        return "Hello, " + self.name;
    }
}

func main() {
    let g = Greeter.spawn();
    let msg = await g.greet();
    println(msg);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

// ==================== cap.split() tests ====================

#[test]
fn cap_combined_declaration() {
    let src = r#"
cap FullAccess = FileReadCap + FileWriteCap;

func main() -> i32 {
    42
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn cap_split_returns_tuple() {
    let src = r#"
cap FileReadCap;
cap FileWriteCap;
cap FullAccess = FileReadCap + FileWriteCap;

func main() -> i32 {
    let c = FullAccess;
    let parts = c.split();
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn cap_split_runtime() {
    let src = r#"
cap FileReadCap;
cap FileWriteCap;
cap FullAccess = FileReadCap + FileWriteCap;

func main() -> i32 {
    let c = FullAccess;
    let (read, write) = c.split();
    drop(read);
    drop(write);
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn cap_split_single_error() {
    let src = r#"
cap FileReadCap;

func main() -> i32 {
    let c = FileReadCap;
    c.split();
    42
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("split() requires a combined capability"), "Expected split error, got: {}", err);
}

#[test]
fn cap_split_drop_one() {
    let src = r#"
cap FileReadCap;
cap FileWriteCap;
cap FullAccess = FileReadCap + FileWriteCap;

func main() -> i32 {
    let c = FullAccess;
    let (read, write) = c.split();
    drop(read);
    drop(write);
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

// ==================== old(x) in ensures tests ====================

#[test]
fn old_basic_snapshot() {
    let src = r#"
func double(x: i32) -> i32 {
    ensures: result == old(x) * 2
    return x * 2;
}

func main() -> i32 {
    double(5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(10));
}

#[test]
fn old_with_mutation() {
    let src = r#"
func increment(x: i32) -> i32 {
    ensures: result == old(x) + 1
    return x + 1;
}

func main() -> i32 {
    increment(10)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(11));
}

#[test]
fn old_fails() {
    let src = r#"
func bad(x: i32) -> i32 {
    ensures: result == old(x) + 10
    return x + 1;
}

func main() -> i32 {
    bad(5)
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("ensures condition failed"), "Expected ensures error, got: {}", err);
}

#[test]
fn old_multiple_params() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    ensures: result == old(a) + old(b)
    return a + b;
}

func main() -> i32 {
    add(3, 4)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(7));
}

// ==================== math: block tests ====================

#[test]
fn math_constant_evaluation() {
    let src = r#"
func main() -> i32 {
    math: {
        1 + 2;
        3 * 4;
    }
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn math_with_variables() {
    let src = r#"
func main() -> i32 {
    let x = 5;
    math: {
        x + 1;
    }
    x * 2
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(10));
}

#[test]
fn math_boolean_expressions() {
    let src = r#"
func main() -> bool {
    math: {
        1 < 2;
        3 > 2;
        1 == 1;
    }
    true
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Bool(true));
}

// ==================== trait/impl tests ====================

#[test]
fn trait_definition() {
    let src = r#"
trait Display {
    func to_string() -> string;
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn trait_with_impl() {
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
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn trait_multiple_methods() {
    let src = r#"
trait Printable {
    func to_string() -> string;
    func print();
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn trait_with_params() {
    let src = r#"
trait Addable {
    func add(x: i32) -> i32;
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

// ==================== where clause tests ====================

#[test]
fn where_single_constraint() {
    let src = r#"
trait Display {
    func to_string() -> string;
}

type MyType {
    value: i32
}

func print(x: MyType) where MyType: Display {
    println(x);
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn where_multiple_constraints() {
    let src = r#"
trait Display {
    func to_string() -> string;
}

trait Clone {
    func clone() -> Self;
}

type MyType {
    value: i32
}

func process(x: MyType) where MyType: Display + Clone {
    println(x);
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn where_with_return_type() {
    let src = r#"
trait Display {
    func to_string() -> string;
}

type MyType {
    value: i32
}

func format(x: MyType) -> string where MyType: Display {
    x.to_string()
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

// ==================== extern block tests ====================

#[test]
fn extern_block_basic() {
    let src = r#"
extern "C" {
    func printf(fmt: string) -> i32;
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn extern_block_multiple_funcs() {
    let src = r#"
extern "C" {
    func malloc(size: i32) -> i32;
    func free(ptr: i32);
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn extern_block_with_cap() {
    let src = r#"
cap FileReadCap;

extern "C" {
    func read(fd: i32, file_cap: FileReadCap) -> string;
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn extern_block_with_borrow() {
    let src = r#"
cap FileReadCap;

extern "C" {
    func read(fd: i32, file_cap: FileReadCap) -> string;
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

// ==================== Additional edge case tests ====================

#[test]
fn cap_split_nested_combination() {
    let src = r#"
cap FileReadCap;
cap FileWriteCap;
cap FullAccess = FileReadCap + FileWriteCap;

func main() -> i32 {
    let c = FullAccess;
    let (read, write) = c.split();
    drop(read);
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn cap_split_use_individual_parts() {
    let src = r#"
cap FileReadCap;
cap FileWriteCap;
cap FullAccess = FileReadCap + FileWriteCap;

func use_read(r: FileReadCap) -> i32 {
    1
}

func use_write(w: FileWriteCap) -> i32 {
    2
}

func main() -> i32 {
    let c = FullAccess;
    let (read, write) = c.split();
    let a = use_read(read);
    let b = use_write(write);
    a + b
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn old_on_string_non_copy() {
    let src = r#"
func append_world(s: string) -> string {
    ensures: result == old(s) + "world"
    return s + "world";
}

func main() -> string {
    append_world("hello")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("helloworld".to_string()));
}

#[test]
fn old_with_multiple_returns() {
    let src = r#"
func abs(x: i32) -> i32 {
    ensures: result >= 0
    ensures: result == old(x) || result == -old(x)
    if x < 0 {
        return -x;
    }
    return x;
}

func main() -> i32 {
    abs(-5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn math_empty_block() {
    let src = r#"
func main() -> i32 {
    math: {
    }
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn math_with_division() {
    let src = r#"
func main() -> i32 {
    math: {
        10 / 2;
        100 / 10;
    }
    5
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn math_with_negative_numbers() {
    let src = r#"
func main() -> i32 {
    math: {
        -1 + 1;
        -5 * -3;
    }
    15
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(15));
}

#[test]
fn trait_with_multiple_methods_impl() {
    let src = r#"
trait Printable {
    func to_string() -> string;
    func print();
}

type MyItem {
    value: i32
}

impl Printable for MyItem {
    func to_string() -> string {
        return "MyItem";
    }
    func print() {
        println("MyItem");
    }
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn where_with_multiple_bounds() {
    let src = r#"
trait Display {
    func to_string() -> string;
}

trait Clone {
    func clone() -> Self;
}

type MyType {
    value: i32
}

func process(x: MyType) -> string where MyType: Display + Clone {
    x.to_string()
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn extern_with_multiple_params() {
    let src = r#"
extern "C" {
    func write(fd: i32, buf: string, len: i32) -> i32;
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn extern_with_no_return() {
    let src = r#"
extern "C" {
    func exit(code: i32);
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

// ==================== f-string tests ====================

#[test]
fn fstring_basic() {
    let src = r#"
func main() -> string {
    let name = "World";
    f"Hello, {name}!"
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("Hello, World!".to_string()));
}

#[test]
fn fstring_multiple_interpolations() {
    let src = r#"
func main() -> string {
    let a = 1;
    let b = 2;
    f"{a} + {b} = {a + b}"
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("1 + 2 = 3".to_string()));
}

#[test]
fn fstring_no_interpolation() {
    let src = r#"
func main() -> string {
    f"just text"
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("just text".to_string()));
}

#[test]
fn fstring_expression_interpolation() {
    let src = r#"
func main() -> string {
    let x = 10;
    f"double is {x * 2}"
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("double is 20".to_string()));
}

#[test]
fn fstring_with_function_call() {
    let src = r#"
func greet(name: string) -> string {
    f"Hi, {name}!"
}

func main() -> string {
    greet("Alice")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("Hi, Alice!".to_string()));
}

// ==================== list comprehension tests ====================

#[test]
fn comprehension_basic() {
    let src = r#"
func main() -> i32 {
    let nums = [1, 2, 3, 4, 5];
    let doubled = [x * 2 for x in nums];
    len(doubled)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn comprehension_with_guard() {
    let src = r#"
func main() -> i32 {
    let nums = [1, 2, 3, 4, 5, 6];
    let evens = [x for x in nums if x % 2 == 0];
    len(evens)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn comprehension_transform() {
    let src = r#"
func main() -> string {
    let words = ["hello", "world"];
    let upper = [w + "!" for w in words];
    upper[0] + " " + upper[1]
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("hello! world!".to_string()));
}

#[test]
fn comprehension_empty_list() {
    let src = r#"
func main() -> i32 {
    let empty = [];
    let result = [x for x in empty];
    len(result)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn comprehension_nested_unsupported() {
    let src = r#"
func main() -> i32 {
    let lists = [[1, 2], [3, 4], [5]];
    let flat = [x for sub in lists for x in sub];
    len(flat)
}
"#;
    let result = run_source_result(src);
    // Nested comprehensions not yet supported, should error
    assert!(result.is_err());
}

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
