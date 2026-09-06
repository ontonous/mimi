use criterion::{black_box, criterion_group, criterion_main, Criterion};

use mimi::{codegen, core, lexer, parser};

fn codegen_simple(c: &mut Criterion) {
    let src = "func main() -> i32 { 42 }".to_string();
    c.bench_function("codegen/simple", |b| {
        b.iter(|| {
            let tokens = lexer::Lexer::new(black_box(&src)).tokenize().unwrap();
            let file = parser::Parser::new(tokens).parse_file().unwrap();
            let program = core::check_program(&file).unwrap();
            let context = inkwell::context::Context::create();
            let mut gen = codegen::CodeGenerator::new(&context, "bench");
            gen.compile_checked(&program).unwrap();
            gen.emit_ir()
        })
    });
}

fn codegen_complex(c: &mut Criterion) {
    // Keep this benchmark inside the currently supported enum-constructor ABI;
    // multi-field enum constructors remain an explicit fail-closed boundary.
    let src = r#"
type Shape { Circle(f64), Triangle }
func area(s: Shape) -> f64 {
    match s {
        Circle(r) => 3.14159 * r * r,
        Triangle => 0.0,
    }
}
func main() -> f64 {
    area(Circle(5.0)) + area(Triangle)
}
"#
    .to_string();
    c.bench_function("codegen/complex", |b| {
        b.iter(|| {
            let tokens = lexer::Lexer::new(black_box(&src)).tokenize().unwrap();
            let file = parser::Parser::new(tokens).parse_file().unwrap();
            let program = core::check_program(&file).unwrap();
            let context = inkwell::context::Context::create();
            let mut gen = codegen::CodeGenerator::new(&context, "bench");
            gen.compile_checked(&program).unwrap();
            gen.emit_ir()
        })
    });
}

fn codegen_recursive(c: &mut Criterion) {
    let src = r#"
func fib(n: i32) -> i32 {
    if n <= 1 { n } else { fib(n - 1) + fib(n - 2) }
}
func main() -> i32 { fib(20) }
"#
    .to_string();
    c.bench_function("codegen/recursive_fib", |b| {
        b.iter(|| {
            let tokens = lexer::Lexer::new(black_box(&src)).tokenize().unwrap();
            let file = parser::Parser::new(tokens).parse_file().unwrap();
            let program = core::check_program(&file).unwrap();
            let context = inkwell::context::Context::create();
            let mut gen = codegen::CodeGenerator::new(&context, "bench");
            gen.compile_checked(&program).unwrap();
            gen.emit_ir()
        })
    });
}

fn codegen_contracts(c: &mut Criterion) {
    let src = r#"
func factorial(n: i32) -> i32 {
    requires: n >= 0
    ensures: result >= 1
    if n <= 1 { 1 } else { n * factorial(n - 1) }
}
func main() -> i32 { factorial(10) }
"#
    .to_string();
    c.bench_function("codegen/with_contracts", |b| {
        b.iter(|| {
            let tokens = lexer::Lexer::new(black_box(&src)).tokenize().unwrap();
            let file = parser::Parser::new(tokens).parse_file().unwrap();
            let program = core::check_program(&file).unwrap();
            let context = inkwell::context::Context::create();
            let mut gen = codegen::CodeGenerator::new(&context, "bench");
            gen.compile_checked(&program).unwrap();
            gen.emit_ir()
        })
    });
}

criterion_group!(
    benches,
    codegen_simple,
    codegen_complex,
    codegen_recursive,
    codegen_contracts,
);
criterion_main!(benches);
