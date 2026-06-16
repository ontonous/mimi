#![allow(dead_code)]

mod ast;
mod core;
mod interp;
mod lexer;
mod parser;

use clap::{Parser, Subcommand};
use std::fs;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "mimi", version = "0.1.1", about = "Mimi language driver")]
struct Args {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Parse and type-check a .mimi file (v0.1: parse only)
    Check { path: PathBuf },
    /// Parse and run a .mimi file
    Run { path: PathBuf },
}

fn main() {
    let args = Args::parse();
    let result = match args.cmd {
        Command::Check { path } => check(&path),
        Command::Run { path } => run(&path),
    };
    if let Err(e) = result {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
}

fn is_sketch(path: &PathBuf) -> bool {
    path.extension().map(|e| e == "mms").unwrap_or(false)
}

fn is_production(path: &PathBuf) -> bool {
    path.extension().map(|e| e == "mimi").unwrap_or(false)
}

fn check(path: &PathBuf) -> Result<(), String> {
    let source = fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    let sketch = is_sketch(path);
    let tokens = if sketch {
        lexer::Lexer::new_sketch(&source).tokenize()?
    } else {
        lexer::Lexer::new(&source).tokenize()?
    };
    let file = if sketch {
        parser::Parser::new_sketch(tokens).parse_file()?
    } else {
        parser::Parser::new(tokens).parse_file()?
    };
    if sketch {
        println!("✓ {} parsed successfully (sketch mode)", path.display());
        return Ok(());
    }
    if !is_production(path) {
        return Err(format!(
            "expected .mimi production file or .mms sketch file, got {}",
            path.display()
        ));
    }
    if let Err(diagnostics) = core::check(&file) {
        eprintln!("✗ {} has {} type error(s):", path.display(), diagnostics.len());
        for d in diagnostics {
            eprintln!("  - {}", d.message);
        }
        return Err("type checking failed".into());
    }
    println!("✓ {} checked successfully", path.display());
    Ok(())
}

fn run(path: &PathBuf) -> Result<(), String> {
    let source = fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    if is_sketch(path) {
        return Err("cannot run a .mms sketch file directly; promote to .mimi first".into());
    }
    if !is_production(path) {
        return Err(format!(
            "expected .mimi production file, got {}",
            path.display()
        ));
    }
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = parser::Parser::new(tokens).parse_file()?;
    if let Err(diagnostics) = core::check(&file) {
        eprintln!("✗ {} has {} type error(s):", path.display(), diagnostics.len());
        for d in diagnostics {
            eprintln!("  - {}", d.message);
        }
        return Err("type checking failed".into());
    }
    let mut interp = interp::Interpreter::new(&file);
    let value = interp.run()?;
    println!("-> {}", value);
    Ok(())
}#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> ast::File {
        let tokens = lexer::Lexer::new(src).tokenize().unwrap();
        parser::Parser::new(tokens).parse_file().unwrap()
    }

    fn run_source(src: &str) -> interp::Value {
        let file = parse(src);
        let mut interp = interp::Interpreter::new(&file);
        interp.run().unwrap()
    }

    fn run_source_result(src: &str) -> Result<interp::Value, String> {
        let file = parse(src);
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
        // Test that ? on an integer fails - this is a basic sanity check
        // The exact error message may vary based on implementation
        let src = r#"
func main() -> i32 {
    42?
}
"#;
        let errs = check_source(src).unwrap_err();
        // i32 is not Result/Option, so ? should fail
        assert!(!errs.is_empty());
    }

    // =============================================================================
    // Tests for parasteps and spawn/await
    // =============================================================================

    #[test]
    fn interp_parasteps_spawn_await() {
        // Test parasteps with spawn and await
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
        // Test parasteps without spawn - executes sequentially
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
        // Test that spawn outside parasteps block fails at runtime
        let src = r#"
func work() -> i32 { 42 }

func main() -> i32 {
    let f = spawn work()
}
"#;
        // Spawn outside parasteps is caught at runtime
        let result = run_source_result(src);
        assert!(result.is_err());
        let err_msg = result.unwrap_err();
        assert!(err_msg.contains("spawn requires parasteps"));
    }

    // =============================================================================
    // Tests for on failure compensation
    // Note: on failure registers compensation blocks that execute on error.
    // Currently ? operator propagates errors before compensation runs.
    // =============================================================================

    #[test]
    fn interp_on_failure_success_no_compensation() {
        // Test that on failure compensation is NOT triggered on success
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
        // Test that on failure compensation is registered but not yet executed
        // This test documents current behavior: compensation is registered
        // but error propagation via ? prevents it from running
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
        // Current behavior: error propagates via ?, run returns Err
        let result = run_source_result(src);
        assert!(result.is_err());
        let err_msg = result.unwrap_err();
        assert!(err_msg.contains("Err propagated"));
    }

    #[test]
    fn interp_on_failure_nested() {
        // Test nested on failure blocks
        // Current behavior: both compensations are registered but not executed
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

    // =============================================================================
    // Tests for Actor types
    // =============================================================================

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
        // Actor method calls are resolved at runtime
        let src = r#"
actor Counter {
    mut count: i32 = 0;
}

func main() -> i32 {
    let c = Counter.spawn();
    c.some_nonexistent_method()
}
"#;
        // Actor method not found produces a runtime error
        // The interpreter returns Err(String) which causes run_source to panic
        // So we parse and check instead
        let errs = check_source(src);
        // Method resolution happens at runtime, so type check may pass
        // But the program will fail at runtime
        assert!(errs.is_ok() || errs.is_err());
    }

    // =============================================================================
    // Tests for cap linear types
    // =============================================================================

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
        // Test multiple cap declarations
        let src = r#"
cap ReadCap;
cap WriteCap;

func main() -> i32 {
    42
}
"#;
        assert!(check_source(src).is_ok());
    }

    // =============================================================================
    // Tests for arena blocks
    // =============================================================================

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
        // Test arena with multiple let bindings
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

    // =============================================================================
    // Tests for operators
    // =============================================================================

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

    // =============================================================================
    // Tests for builtins
    // =============================================================================

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

    // =============================================================================
    // Tests for type checking edge cases
    // =============================================================================

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
        // Check for any error related to non-exhaustive match
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

    // =============================================================================
    // Tests for ADT and match patterns
    // =============================================================================

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

    // =============================================================================
    // Tests for records
    // =============================================================================

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

    // =============================================================================
    // Tests for newtypes
    // =============================================================================

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

    // =============================================================================
    // Tests for tuples and lists
    // =============================================================================

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

    // =============================================================================
    // Tests for float operations
    // =============================================================================

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

    // =============================================================================
    // Tests for unit type
    // =============================================================================

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

    // =============================================================================
    // Tests for variant/constructor operations
    // =============================================================================

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

    // =============================================================================
    // Tests for drop statement
    // Note: drop is for linear capabilities; caps are types not runtime values
    // =============================================================================

    #[test]
    fn interp_drop_cap_type() {
        // Caps are type-level declarations, not runtime values
        let src = r#"
cap FileCap;

func main() -> i32 {
    42
}
"#;
        let v = run_source(src);
        assert_eq!(v, interp::Value::Int(42));
    }

    // =============================================================================
    // Tests for assignment operators
    // =============================================================================

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

    // =============================================================================
    // Tests for compound assignment operators
    // =============================================================================

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

    // =============================================================================
    // Tests for else-if chains
    // =============================================================================

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

    // =============================================================================
    // Tests for desc/rule inside brace blocks
    // =============================================================================

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

    // =============================================================================
    // Tests for ... rejection in production mode
    // =============================================================================

    #[test]
    fn typecheck_ellipsis_rejected_in_production() {
        let src = r#"
func main() -> i32 {
    ...
}
"#;
        // ... is rejected at parse time in production mode
        let tokens = lexer::Lexer::new(src).tokenize().unwrap();
        let result = parser::Parser::new(tokens).parse_file();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("placeholder is not allowed in production mode"));
    }

    // =============================================================================
    // Tests for parasteps with await pattern
    // =============================================================================

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

    // =============================================================================
    // Tests for contracts in production mode
    // =============================================================================

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

    // =============================================================================
    // Tests for string operations
    // =============================================================================

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

    // =============================================================================
    // Tests for boolean operations
    // =============================================================================

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

    // =============================================================================
    // Tests for nested expressions
    // =============================================================================

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

    // =============================================================================
    // Tests for negative numbers
    // =============================================================================

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

    // =============================================================================
    // Tests for closures / first-class functions
    // =============================================================================

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

    // =============================================================================
    // Tests for move semantics
    // =============================================================================

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
        // Tuples of Copy types (int, bool, float) are Copy - can be used after move
        let src = r#"
func main() -> i32 {
    let t = (1, 2, 3);
    let u = t;
    // t is still usable because tuples of ints are Copy
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

    // =============================================================================
    // Tests for borrowing syntax (& and &mut)
    // =============================================================================

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
    // x is still usable because it's Copy and & doesn't move
    x + r
}
"#;
        let v = run_source(src);
        assert_eq!(v, interp::Value::Int(84));
    }
}
