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
#[command(name = "mimi", version = "0.1.0", about = "Mimi language driver")]
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
}
