//! Basic benchmark harness using `std::time::Instant`.
//!
//! These are NOT CI-enforced pass/fail tests; they print timing diagnostics.
//! Run with:  cargo test benchmarks -- --nocapture
//!
//! For proper regression detection, add criterion (cargo add criterion --dev)
//! and create `benches/*.rs` files with `harness = false`.

use std::time::Instant;
use crate::{core, lexer, parser};
use crate::interp::Interpreter;

/// Time a closure, print result, return the value.
fn bench<F, T>(name: &str, iterations: u32, mut f: F) -> T
where
    F: FnMut() -> T,
{
    let start = Instant::now();
    let mut result = None::<T>;
    for i in 0..iterations {
        result = Some(f());
        if i == 0 {
            let elapsed = start.elapsed();
            let first_ns = elapsed.as_nanos();
            if first_ns > 1_000_000 {
                eprintln!("  [{name}] first: {:.1}ms", first_ns as f64 / 1_000_000.0);
            } else {
                eprintln!("  [{name}] first: {:.1}µs", first_ns as f64 / 1_000.0);
            }
        }
    }
    let total = start.elapsed();
    let avg_ns = total.as_nanos() as f64 / iterations as f64;
    if avg_ns > 1_000_000.0 {
        eprintln!("  [{name}] avg ({iterations} iters): {:.1}ms", avg_ns / 1_000_000.0);
    } else {
        eprintln!("  [{name}] avg ({iterations} iters): {:.1}µs", avg_ns / 1_000.0);
    }
    result.expect("src/tests/benchmarks.rs:39 unwrap failed")
}

// ==============================
// Parser benchmarks
// ==============================

#[test]
fn bench_parser_simple() {
    let src = "func main() -> i32 { 42 }";
    bench("parse_simple", 1000, || {
        let tokens = lexer::Lexer::new(src).tokenize().expect("src/tests/benchmarks.rs:50 unwrap failed");
        parser::Parser::new(tokens).parse_file().expect("src/tests/benchmarks.rs:51 unwrap failed")
    });
}

#[test]
fn bench_parser_complex() {
    let src = r#"
type Opt {
        Some(i32)
        None
    }
func unwrap(x: Opt) -> i32 {
    match x {
        Some(n) => n,
        None => 0,
    }
}
func main() -> i32 { unwrap(Some(42)) }
"#;
    bench("parse_complex", 500, || {
        let tokens = lexer::Lexer::new(src).tokenize().expect("src/tests/benchmarks.rs:71 unwrap failed");
        parser::Parser::new(tokens).parse_file().expect("src/tests/benchmarks.rs:72 unwrap failed")
    });
}

#[test]
fn bench_parser_large_module() {
    let src = (0..100).map(|i| format!("func f_{i}() -> i32 {{ {i} }}\n")).collect::<String>();
    let src = format!("{src}func main() -> i32 {{ 0 }}");
    bench("parse_100_funcs", 100, || {
        let tokens = lexer::Lexer::new(&src).tokenize().expect("src/tests/benchmarks.rs:81 unwrap failed");
        parser::Parser::new(tokens).parse_file().expect("src/tests/benchmarks.rs:82 unwrap failed")
    });
}

#[test]
fn bench_parser_deep_nesting() {
    let depth = 50;
    let mut src = "func main() -> i32 {\n".to_string();
    for _ in 0..depth { src.push_str("if true { "); }
    src.push_str("42");
    for _ in 0..depth { src.push_str(" } else { 0 }"); }
    src.push_str("\n}");
    bench("parse_deep_nesting_50", 100, || {
        let tokens = lexer::Lexer::new(&src).tokenize().expect("src/tests/benchmarks.rs:95 unwrap failed");
        parser::Parser::new(tokens).parse_file().expect("src/tests/benchmarks.rs:96 unwrap failed")
    });
}

// ==============================
// Typechecker benchmarks
// ==============================

#[test]
fn bench_typecheck_simple() {
    let src = "func main() -> i32 { 42 }";
    let file = parser::Parser::new(lexer::Lexer::new(src).tokenize().expect("src/tests/benchmarks.rs:107 unwrap failed")).parse_file().expect("src/tests/benchmarks.rs:107 unwrap failed");
    let _ = bench("typecheck_simple", 500, || {
        core::check(&file)
    });
}

#[test]
fn bench_typecheck_complex() {
    let src = r#"
type Opt {
        Some(i32)
        None
    }
func unwrap(x: Opt) -> i32 {
    match x {
        Some(n) => n,
        None => 0,
    }
}
func main() -> i32 { unwrap(Some(42)) }
"#;
    let file = parser::Parser::new(lexer::Lexer::new(src).tokenize().expect("src/tests/benchmarks.rs:128 unwrap failed")).parse_file().expect("src/tests/benchmarks.rs:128 unwrap failed");
    let _ = bench("typecheck_complex", 500, || {
        core::check(&file)
    });
}

// ==============================
// Interpreter benchmarks
// ==============================

#[test]
fn bench_interp_simple() {
    let file = parser::Parser::new(lexer::Lexer::new("func main() -> i32 { 42 }").tokenize().expect("src/tests/benchmarks.rs:140 unwrap failed")).parse_file().expect("src/tests/benchmarks.rs:140 unwrap failed");
    let mut interp = Interpreter::new(&file);
    let _ = bench("interp_simple", 500, || interp.run());
}

#[test]
fn bench_interp_recursive() {
    let src = r#"
func fib(n: i32) -> i32 {
    if n <= 1 { n } else { fib(n - 1) + fib(n - 2) }
}
        func main() -> i32 { fib(5) }
"#;
    let file = parser::Parser::new(lexer::Lexer::new(src).tokenize().expect("src/tests/benchmarks.rs:153 unwrap failed")).parse_file().expect("src/tests/benchmarks.rs:153 unwrap failed");
    let mut interp = Interpreter::new(&file);
    let _ = bench("interp_fib_5", 50, || interp.run());
}
