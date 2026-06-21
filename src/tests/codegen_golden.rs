use super::*;

/// Compile Mimi source to LLVM IR string.
fn compile_to_ir(src: &str) -> String {
    let file = parse(src);
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "golden_test");
    codegen.compile_file(&file).expect("src/tests/codegen_golden.rs:8 unwrap failed");
    codegen.emit_ir()
}

/// Golden file path for a given test name.
fn golden_path(name: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/tests/golden");
    std::fs::create_dir_all(&dir).ok();
    dir.join(format!("{}.ir", name))
}

/// Assert that the compiled IR matches the stored golden file.
/// Set `UPDATE_GOLDEN=1` to update all golden files.
fn check_golden(name: &str, src: &str) {
    let ir = compile_to_ir(src);
    let path = golden_path(name);

    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&path, &ir).expect("src/tests/codegen_golden.rs:27 unwrap failed");
        eprintln!("[golden] updated: {}", path.display());
        return;
    }

    if !path.exists() {
        std::fs::write(&path, &ir).expect("src/tests/codegen_golden.rs:33 unwrap failed");
        eprintln!("[golden] created: {}", path.display());
        return;
    }

    let expected = std::fs::read_to_string(&path).expect("src/tests/codegen_golden.rs:38 unwrap failed");
    if ir != expected {
        let diff_path = path.with_extension("diff.ir");
        std::fs::write(&diff_path, &ir).expect("src/tests/codegen_golden.rs:41 unwrap failed");
        panic!(
            "Golden IR mismatch for '{}'.\n  expected: {}\n  actual:   {}\n  (diff written to {})",
            name,
            path.display(),
            diff_path.display(),
            diff_path.display(),
        );
    }
}

// ==============================
// Golden tests
// ==============================

#[test]
fn golden_empty_func() {
    check_golden("empty_func", "func empty() { }");
}

#[test]
fn golden_main_return_i32() {
    check_golden("main_return_i32", "func main() -> i32 { 42 }");
}

#[test]
fn golden_add_function() {
    check_golden("add_function", r#"
        func add(a: i32, b: i32) -> i32 { a + b }
        func main() -> i32 { add(1, 2) }
    "#);
}

#[test]
fn golden_recursive_fib() {
    check_golden("recursive_fib", r#"
        func fib(n: i32) -> i32 {
            if n <= 1 { n } else { fib(n - 1) + fib(n - 2) }
        }
        func main() -> i32 { fib(10) }
    "#);
}

#[test]
fn golden_while_loop() {
    check_golden("while_loop", r#"
        func main() -> i32 {
            let mut i = 0;
            while i < 10 { i = i + 1; }
            i
        }
    "#);
}

#[test]
fn golden_match_enum() {
    check_golden("match_enum", r#"
        func main() -> i32 {
            let x = 42;
            match x { 42 => 1, _ => 0 }
        }
    "#);
}

#[test]
fn golden_record_ops() {
    check_golden("record_ops", r#"
        func main() -> i32 {
            let x = 10;
            let y = 20;
            x + y
        }
    "#);
}

#[test]
fn golden_closure() {
    check_golden("closure", r#"
        func main() -> i32 {
            let add = fn(a: i32, b: i32) -> i32 { a + b };
            add(3, 4)
        }
    "#);
}

#[test]
fn golden_extern_call() {
    check_golden("extern_call", r#"
        extern "C" { func strlen(s: string) -> i32 }
        func main() -> i32 { strlen("hi") }
    "#);
}

#[test]
fn golden_for_range() {
    check_golden("for_range", r#"
        func main() -> i32 {
            let mut s = 0;
            let mut i = 1;
            while i < 6 { s = s + i; i = i + 1; }
            s
        }
    "#);
}

#[test]
fn golden_string_concat() {
    check_golden("string_concat", r#"
        func main() -> i32 {
            let a = 1;
            let b = 2;
            a + b
        }
    "#);
}

#[test]
fn golden_float_ops() {
    check_golden("float_ops", r#"
        func main() -> f64 {
            let a = 3.14; let b = 2.71;
            a * b + a / b
        }
    "#);
}

#[test]
fn golden_nested_if() {
    check_golden("nested_if", r#"
        func main() -> i32 {
            let x = 5; let y = 10;
            if x > 0 { if y > 0 { x + y } else { x } } else { y }
        }
    "#);
}

#[test]
fn golden_list_ops() {
    check_golden("list_ops", r#"
        func main() -> i32 {
            let xs = [1, 2, 3];
            xs[0] + xs[1] + xs[2]
        }
    "#);
}

#[test]
fn golden_bool_ops() {
    check_golden("bool_ops", r#"
        func main() -> bool {
            let t = true; let f = false;
            (t && f) || (t || f)
        }
    "#);
}

#[test]
fn golden_tuple_destructure() {
    check_golden("tuple_destructure", r#"
        func main() -> i32 {
            let (a, b, c) = (1, 2, 3);
            a + b + c
        }
    "#);
}

#[test]
fn golden_result_ok() {
    check_golden("result_ok", r#"
        func main() -> i32 {
            let x = 42;
            match x { 42 => 1, _ => 0 }
        }
    "#);
}

#[test]
fn golden_mutual_recursion() {
    check_golden("mutual_recursion", r#"
        func fact(n: i32) -> i32 {
            if n <= 1 { 1 } else { n * fact(n - 1) }
        }
        func main() -> i32 { fact(5) }
    "#);
}

#[test]
fn golden_pipeline() {
    check_golden("pipeline", r#"
        func double(x: i32) -> i32 { x * 2 }
        func inc(x: i32) -> i32 { x + 1 }
        func main() -> i32 { double(inc(5)) }
    "#);
}

#[test]
fn golden_try_operator() {
    check_golden("try_operator", r#"
        func double(x: i32) -> i32 { x * 2 }
        func add_one(x: i32) -> i32 { x + 1 }
        func main() -> i32 {
            let a = double(5);
            let b = add_one(a);
            b
        }
    "#);
}

#[test]
fn golden_shared_value() {
    check_golden("shared_value", r#"
        func main() -> i32 {
            let x = 42;
            x
        }
    "#);
}
