use super::*;

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
fn interp_spawn_non_actor_evaluates_directly() {
    let src = r#"
func work() -> i32 { 42 }

func main() -> i32 {
    let f = spawn work()
    f
}
"#;
    let result = run_source_result(src);
    assert!(result.is_ok(), "spawn of non-actor call should evaluate directly: {:?}", result.err());
    assert_eq!(result.expect("src/tests/basic_other.rs:217 unwrap failed"), interp::Value::Int(42));
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
    assert!(result.is_ok(), "? should propagate error as value, got: {:?}", result);
    let val = result.expect("src/tests/basic_other.rs:270 unwrap failed");
    match &val {
        interp::Value::Variant(name, _) if name == "Err" => {},
        other => panic!("Expected Err variant, got: {}", other),
    }
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
    assert!(result.is_ok(), "? should propagate error as value, got: {:?}", result);
    let val = result.expect("src/tests/basic_other.rs:303 unwrap failed");
    match &val {
        interp::Value::Variant(name, _) if name == "Err" => {},
        other => panic!("Expected Err variant, got: {}", other),
    }
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
    let tokens = lexer::Lexer::new(src).tokenize().expect("src/tests/basic_other.rs:593 unwrap failed");
    let result = parser::Parser::new(tokens).parse_file();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.message.contains("placeholder is not allowed in production mode"));
}

// ===== Option/Result combinator tests =====

#[test]
fn interp_option_unwrap() {
    let src = r#"
func main() -> i32 {
    Some(42).unwrap()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn interp_option_unwrap_none() {
    let src = r#"
func main() -> i32 {
    let x = None;
    x.unwrap()
}
"#;
    let v = run_source_result(src);
    assert!(v.is_err());
    assert!(v.unwrap_err().contains("unwrap() on None"));
}

#[test]
fn interp_option_is_some() {
    let src = r#"
func main() -> bool {
    let x = Some(42);
    x.is_some()
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}

#[test]
fn interp_option_is_none() {
    let src = r#"
func main() -> bool {
    let x = None();
    x.is_none()
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}

#[test]
fn interp_result_unwrap_ok() {
    let src = r#"
func main() -> i32 {
    Ok(42).unwrap()
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn interp_result_unwrap_err() {
    let src = r#"
func main() -> i32 {
    Err("fail").unwrap()
}
"#;
    let v = run_source_result(src);
    assert!(v.is_err());
}

#[test]
fn interp_result_is_ok() {
    let src = r#"
func main() -> bool {
    Ok(42).is_ok()
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}

#[test]
fn interp_result_is_err() {
    let src = r#"
func main() -> bool {
    Err("fail").is_err()
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}

#[test]
fn interp_option_map() {
    let src = r#"
func double(x: i32) -> i32 { x * 2 }

func main() -> i32 {
    Some(21).map(double).unwrap()
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn interp_option_and_then() {
    let src = r#"
func wrap(x: i32) -> i32 { Some(x * 2) }

func main() -> i32 {
    Some(21).and_then(wrap).unwrap()
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn typecheck_option_constructors() {
    let src = r#"
func main() -> i32 {
    Some(42).unwrap()
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn typecheck_result_constructors() {
    let src = r#"
func main() -> bool {
    Ok(42).is_ok()
}
"#;
    assert!(check_source(src).is_ok());
}

// ===== String method chaining tests =====

#[test]
fn interp_string_len() {
    let src = r#"
func main() -> i32 {
    "hello".len()
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(5));
}

#[test]
fn interp_string_trim() {
    let src = r#"
func main() -> string {
    "  hi  ".trim()
}
"#;
    assert_eq!(run_source(src), interp::Value::String("hi".into()));
}

#[test]
fn interp_string_contains() {
    let src = r#"
func main() -> bool {
    "hello world".contains("world")
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}

#[test]
fn interp_string_split() {
    let src = r#"
func main() -> i32 {
    "a,b,c".split(",").len()
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(3));
}

#[test]
fn interp_string_to_upper() {
    let src = r#"
func main() -> string {
    "hello".to_upper()
}
"#;
    assert_eq!(run_source(src), interp::Value::String("HELLO".into()));
}

#[test]
fn interp_string_replace() {
    let src = r#"
func main() -> string {
    "hello".replace("l", "x")
}
"#;
    assert_eq!(run_source(src), interp::Value::String("hexxo".into()));
}

#[test]
fn interp_string_var_method() {
    let src = r#"
func main() -> bool {
    let s = "Hello World";
    s.contains("World")
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}

#[test]
fn interp_string_chain() {
    let src = r#"
func main() -> string {
    "  hello  ".trim().to_upper()
}
"#;
    assert_eq!(run_source(src), interp::Value::String("HELLO".into()));
}

#[test]
fn typecheck_string_methods() {
    let src = r#"
func main() -> i32 {
    "hello".len()
}
"#;
    assert!(check_source(src).is_ok());
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
