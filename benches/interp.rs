use criterion::{black_box, criterion_group, criterion_main, Criterion};

use mimi::{core, interp, lexer, parser};

fn interp_simple(c: &mut Criterion) {
    let src = "func main() -> i32 { 42 }".to_string();
    c.bench_function("interp/simple", |b| {
        b.iter(|| {
            let tokens = lexer::Lexer::new(black_box(&src)).tokenize().unwrap();
            let file = parser::Parser::new(tokens).parse_file().unwrap();
            core::check(&file).unwrap();
            let mut vm = interp::Interpreter::new(&file);
            vm.run().unwrap();
        })
    });
}

fn interp_fib(c: &mut Criterion) {
    let src = r#"
func fib(n: i32) -> i32 {
    if n <= 1 { n } else { fib(n - 1) + fib(n - 2) }
}
func main() -> i32 { fib(30) }
"#.to_string();
    c.bench_function("interp/fib_30", |b| {
        b.iter(|| {
            let tokens = lexer::Lexer::new(black_box(&src)).tokenize().unwrap();
            let file = parser::Parser::new(tokens).parse_file().unwrap();
            core::check(&file).unwrap();
            let mut vm = interp::Interpreter::new(&file);
            vm.run().unwrap();
        })
    });
}

fn interp_prime(c: &mut Criterion) {
    let src = r#"
func is_prime(n: i32) -> bool {
    if n < 2 { return false; }
    let mut i = 2;
    while i * i <= n {
        if n % i == 0 { return false; }
        i = i + 1;
    }
    true
}
func main() -> bool { is_prime(9973) }
"#.to_string();
    c.bench_function("interp/prime_check", |b| {
        b.iter(|| {
            let tokens = lexer::Lexer::new(black_box(&src)).tokenize().unwrap();
            let file = parser::Parser::new(tokens).parse_file().unwrap();
            core::check(&file).unwrap();
            let mut vm = interp::Interpreter::new(&file);
            vm.run().unwrap();
        })
    });
}

fn interp_list_sum(c: &mut Criterion) {
    let src = r#"
func sum(items: List<i32>) -> i32 {
    let mut total = 0;
    for x in items { total = total + x; }
    total
}
func main() -> i32 { sum([1,2,3,4,5,6,7,8,9,10]) }
"#.to_string();
    c.bench_function("interp/list_sum_10", |b| {
        b.iter(|| {
            let tokens = lexer::Lexer::new(black_box(&src)).tokenize().unwrap();
            let file = parser::Parser::new(tokens).parse_file().unwrap();
            core::check(&file).unwrap();
            let mut vm = interp::Interpreter::new(&file);
            vm.run().unwrap();
        })
    });
}

fn interp_match_enum(c: &mut Criterion) {
    let src = r#"
type Shape = Circle(f64) | Rect(f64, f64)
func area(s: Shape) -> f64 {
    match s {
        Circle(r) => 3.14159 * r * r,
        Rect(w, h) => w * h,
    }
}
func main() -> f64 {
    area(Circle(5.0)) + area(Rect(3.0, 4.0))
}
"#.to_string();
    c.bench_function("interp/match_enum", |b| {
        b.iter(|| {
            let tokens = lexer::Lexer::new(black_box(&src)).tokenize().unwrap();
            let file = parser::Parser::new(tokens).parse_file().unwrap();
            core::check(&file).unwrap();
            let mut vm = interp::Interpreter::new(&file);
            vm.run().unwrap();
        })
    });
}

fn interp_contract_check(c: &mut Criterion) {
    let src = r#"
func factorial(n: i32) -> i32 {
    requires: n >= 0
    ensures: result >= 1
    if n <= 1 { 1 } else { n * factorial(n - 1) }
}
func main() -> i32 { factorial(10) }
"#.to_string();
    c.bench_function("interp/contract_check", |b| {
        b.iter(|| {
            let tokens = lexer::Lexer::new(black_box(&src)).tokenize().unwrap();
            let file = parser::Parser::new(tokens).parse_file().unwrap();
            core::check(&file).unwrap();
            let mut vm = interp::Interpreter::new(&file);
            vm.verify_contracts = true;
            vm.run().unwrap();
        })
    });
}

criterion_group!(benches,
    interp_simple,
    interp_fib,
    interp_prime,
    interp_list_sum,
    interp_match_enum,
    interp_contract_check,
);
criterion_main!(benches);
