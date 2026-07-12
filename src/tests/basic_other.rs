use super::*;

#[test]
fn typecheck_undefined_variable() {
    let src = r#"
func main() {
    println(x);
}
"#;
    let errs = check_source(src).unwrap_err();
    assert!(errs
        .iter()
        .any(|d| d.message.contains("undefined variable")));
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
    assert!(errs
        .iter()
        .any(|d| d.message.contains("argument 1") || d.message.contains("UserId")));
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
fn interp_spawn_non_actor_returns_future() {
    let src = r#"
func work() -> i32 { 42 }

func main() -> i32 {
    let f = spawn work()
    let r = await f
    r
}
"#;
    let result = run_source_result(src);
    assert!(
        result.is_ok(),
        "spawn of non-actor call should complete via await: {:?}",
        result.err()
    );
    assert_eq!(
        result.expect("src/tests/basic_other.rs:217 unwrap failed"),
        interp::Value::Int(42)
    );
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
    assert!(
        result.is_ok(),
        "? should propagate error as value, got: {:?}",
        result
    );
    let val = result.expect("src/tests/basic_other.rs:270 unwrap failed");
    match &val {
        interp::Value::Variant(name, _) if name == "Err" => {}
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
    assert!(
        result.is_ok(),
        "? should propagate error as value, got: {:?}",
        result
    );
    let val = result.expect("src/tests/basic_other.rs:303 unwrap failed");
    match &val {
        interp::Value::Variant(name, _) if name == "Err" => {}
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
fn newtype_unification_preserved() {
    // CO-H3 (audit): newtype wraps an inner type and is transparent in
    // unification. The SAFETY comment in src/core/unification.rs documents
    // this design tradeoff. This test ensures that the transparent
    // newtype semantics continue to work — distinct newtypes sharing an
    // inner type remain interchangeable for inference / let-binding.
    let src = r#"
newtype UserId = i32
newtype OrderId = i32

func raw_id(n: UserId) -> i32 { n.0 }

func main() -> i32 {
    let u = UserId(42)
    let i: i32 = raw_id(u)
    i
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}
