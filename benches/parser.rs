use criterion::{black_box, criterion_group, criterion_main, Criterion};

use mimi::{lexer, parser};

fn parse_simple(c: &mut Criterion) {
    let src = "func main() -> i32 { 42 }";
    c.bench_function("parser/simple", |b| {
        b.iter(|| {
            let tokens = lexer::Lexer::new(black_box(src)).tokenize().unwrap();
            let _file = parser::Parser::new(tokens).parse_file().unwrap();
        })
    });
}

fn parse_complex(c: &mut Criterion) {
    let src = r#"
type Shape = Circle(f64) | Rect(f64, f64) | Line { x1: i32; y1: i32; x2: i32; y2: i32 }

func area(s: Shape) -> f64 {
    match s {
        Circle(r) => 3.14159 * r * r,
        Rect(w, h) => w * h,
        Line { .. } => 0.0,
    }
}

func main() -> f64 {
    let shapes = [Circle(5.0), Rect(3.0, 4.0), Line { x1: 0, y1: 0, x2: 1, y2: 1 }];
    let mut total = 0.0;
    for s in shapes { total = total + area(s); }
    total
}
"#;
    c.bench_function("parser/complex", |b| {
        b.iter(|| {
            let tokens = lexer::Lexer::new(black_box(src)).tokenize().unwrap();
            let _file = parser::Parser::new(tokens).parse_file().unwrap();
        })
    });
}

fn parse_large(c: &mut Criterion) {
    let src = (0..500).map(|i| format!("func f{}() -> i32 {{ {} }}\n", i, i)).collect::<String>();
    let src = format!("{}func main() -> i32 {{ 0 }}", src);
    c.bench_function("parser/500_functions", |b| {
        b.iter(|| {
            let tokens = lexer::Lexer::new(black_box(&src)).tokenize().unwrap();
            let _file = parser::Parser::new(tokens).parse_file().unwrap();
        })
    });
}

fn parse_deep_nesting(c: &mut Criterion) {
    let depth = 100;
    let mut src = "func main() -> i32 {\n".to_string();
    for _ in 0..depth {
        src.push_str("if true { ");
    }
    src.push_str("42");
    for _ in 0..depth {
        src.push_str(" } else { 0 }");
    }
    src.push_str("\n}");
    c.bench_function("parser/deep_nesting_100", |b| {
        b.iter(|| {
            let tokens = lexer::Lexer::new(black_box(&src)).tokenize().unwrap();
            let _file = parser::Parser::new(tokens).parse_file().unwrap();
        })
    });
}

criterion_group!(benches, parse_simple, parse_complex, parse_large, parse_deep_nesting);
criterion_main!(benches);
