// Codegen edge case and robustness tests
// Focus: scenarios where codegen might differ from interpreter, complex codegen paths

use super::*;

fn can_codegen() -> bool {
    std::process::Command::new("cc")
        .arg("--version")
        .output()
        .is_ok()
}

fn can_valgrind() -> bool {
    std::process::Command::new("valgrind")
        .arg("--version")
        .output()
        .is_ok()
}

// ====== Codegen: Deep Nesting ======

#[test]
fn cg_deeply_nested_if() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
func nested(x: i32) -> i32 {
    if x == 1 {
        if x == 1 {
            if x == 1 {
                if x == 1 {
                    if x == 1 { 42 } else { 0 }
                } else { 0 }
            } else { 0 }
        } else { 0 }
    } else { 0 }
}
func main() -> i32 { println(nested(1)); 0 }
"#,
    )
    .unwrap();
    assert_eq!(out.trim(), "42");
}

#[test]
fn cg_deeply_nested_arithmetic() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run("func main() -> i32 { println(1 + (2 + (3 + (4 + (5 + 6))))); 0 }")
        .unwrap();
    assert_eq!(out.trim(), "21");
}

// ====== Codegen: Complex Control Flow ======

#[test]
fn cg_while_with_break() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
func main() -> i32 {
    let mut i = 0
    while i < 100 {
        if i >= 5 { break }
        i = i + 1
    }
    println(i)
    0
}
"#,
    )
    .unwrap();
    assert_eq!(out.trim(), "5");
}

// cg_for_with_continue: known codegen limitation — continue in for loop may fail

#[test]
fn cg_nested_loops() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
func main() -> i32 {
    let mut count = 0
    let mut i = 0
    while i < 5 {
        let mut j = 0
        while j < 5 {
            count = count + 1
            j = j + 1
        }
        i = i + 1
    }
    println(count)
    0
}
"#,
    )
    .unwrap();
    assert_eq!(out.trim(), "25");
}

#[test]
fn cg_match_multiple_arms() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
func classify(n: i32) -> string {
    match n {
        1 => "one",
        2 => "two",
        3 => "three",
        _ => "other"
    }
}
func main() -> i32 {
    println(classify(2))
    println(classify(99))
    0
}
"#,
    )
    .unwrap();
    assert_eq!(out.trim(), "two\nother");
}

// ====== Codegen: Function Calls ======

#[test]
fn cg_recursive_fib() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
func fib(n: i32) -> i32 { if n <= 1 { n } else { fib(n - 1) + fib(n - 2) } }
func main() -> i32 { println(fib(10)); 0 }
"#,
    )
    .unwrap();
    assert_eq!(out.trim(), "55");
}

#[test]
fn cg_multiple_params() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
func add3(a: i32, b: i32, c: i32) -> i32 { a + b + c }
func main() -> i32 { println(add3(10, 20, 30)); 0 }
"#,
    )
    .unwrap();
    assert_eq!(out.trim(), "60");
}

#[test]
fn cg_closure_capture() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
func main() -> i32 {
    let x = 10
    let add_x = fn(a: i32) -> i32 { a + x }
    println(add_x(32))
    0
}
"#,
    )
    .unwrap();
    assert_eq!(out.trim(), "42");
}

// ====== Codegen: String Operations ======

#[test]
fn cg_string_concat() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run("func main() -> i32 { println(\"hello\" + \" \" + \"world\"); 0 }")
        .unwrap();
    assert_eq!(out.trim(), "hello world");
}

#[test]
fn cg_string_fstring() {
    if !can_codegen() {
        return;
    }
    let out =
        compile_and_run("func main() -> i32 { let x = 42; println(f\"value={x}\"); 0 }").unwrap();
    assert_eq!(out.trim(), "value=42");
}

#[test]
fn cg_string_len() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run("func main() -> i32 { println(len(\"hello\")); 0 }").unwrap();
    assert_eq!(out.trim(), "5");
}

// String returns exercise ownership transfer at function boundaries.
#[test]
fn cg_string_return_literal() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"func greet() -> string { "hello" }
        func main() -> i32 { println(greet()); 0 }"#,
    )
    .unwrap();
    assert_eq!(out.trim(), "hello");
}

#[test]
fn cg_string_return_concat() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"func greet() -> string { "hello" + " " + "world" }
        func main() -> i32 { println(greet()); 0 }"#,
    )
    .unwrap();
    assert_eq!(out.trim(), "hello world");
}

#[test]
fn cg_string_return_variable() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"func greet() -> string { let s = "hello"; s }
        func main() -> i32 { println(greet()); 0 }"#,
    )
    .unwrap();
    assert_eq!(out.trim(), "hello");
}

#[test]
fn cg_string_return_builtin() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"func greet() -> string { str_to_upper("hello") }
        func main() -> i32 { println(greet()); 0 }"#,
    )
    .unwrap();
    assert_eq!(out.trim(), "HELLO");
}

#[test]
fn cg_string_return_literal_valgrind() {
    if !can_codegen() {
        return;
    }
    if !can_valgrind() {
        return;
    }
    let out = compile_and_run_valgrind(
        r#"func greet() -> string { "hello" }
        func main() -> i32 { println(greet()); 0 }"#,
    )
    .expect("src/tests/codegen_boundary.rs:cg_string_return_literal_valgrind");
    assert_eq!(out.trim(), "hello");
}

#[test]
fn cg_string_return_variable_valgrind() {
    if !can_codegen() {
        return;
    }
    if !can_valgrind() {
        return;
    }
    let out = compile_and_run_valgrind(
        r#"func greet() -> string { let s = "hi"; s }
        func main() -> i32 { println(greet()); 0 }"#,
    )
    .expect("src/tests/codegen_boundary.rs:cg_string_return_variable_valgrind");
    assert_eq!(out.trim(), "hi");
}

#[test]
fn cg_string_return_nested_valgrind() {
    if !can_codegen() {
        return;
    }
    if !can_valgrind() {
        return;
    }
    let out = compile_and_run_valgrind(
        r#"func inner() -> string { "world" }
        func outer() -> string { "hello " + inner() }
        func main() -> i32 { println(outer()); 0 }"#,
    )
    .expect("src/tests/codegen_boundary.rs:cg_string_return_nested_valgrind");
    assert_eq!(out.trim(), "hello world");
}

#[test]
fn cg_string_return_concat_valgrind() {
    if !can_codegen() {
        return;
    }
    if !can_valgrind() {
        return;
    }
    let out = compile_and_run_valgrind(
        r#"func greet() -> string { "hello" + " " + "world" }
        func main() -> i32 { println(greet()); 0 }"#,
    )
    .expect("src/tests/codegen_boundary.rs:cg_string_return_concat_valgrind");
    assert_eq!(out.trim(), "hello world");
}

#[test]
fn cg_string_direct_concat_valgrind() {
    if !can_codegen() {
        return;
    }
    if !can_valgrind() {
        return;
    }
    let out = compile_and_run_valgrind(r#"func main() -> i32 { println("hello" + " world"); 0 }"#)
        .expect("src/tests/codegen_boundary.rs:cg_string_direct_concat_valgrind");
    assert_eq!(out.trim(), "hello world");
}

// ====== Codegen: Crypto ======

#[test]
fn cg_sha256_known_vector() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run("func main() -> i32 { println(sha256(\"abc\")); 0 }").unwrap();
    assert_eq!(
        out.trim(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn cg_base64_roundtrip() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
func main() -> i32 {
    let e = base64_encode("Hello")
    println(e)
    0
}
"#,
    )
    .unwrap();
    assert_eq!(out.trim(), "SGVsbG8=");
}

// ====== Codegen: Directory Operations ======

#[test]
fn cg_listdir_and_count() {
    if !can_codegen() {
        return;
    }
    let out =
        compile_and_run("func main() -> i32 { let e = listdir(\"examples\"); println(len(e)); 0 }")
            .unwrap();
    let n: i32 = out.trim().parse().unwrap_or(0);
    assert!(n > 0, "expected entries, got {}", n);
}

#[test]
fn cg_is_dir_current() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        "func main() -> i32 { if is_dir(\".\") { println(\"yes\") } else { println(\"no\") } 0 }",
    )
    .unwrap();
    assert_eq!(out.trim(), "yes");
}

#[test]
fn cg_path_join_chain() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        "func main() -> i32 { println(path_join(path_join(\"a\", \"b\"), \"c\")); 0 }",
    )
    .unwrap();
    assert_eq!(out.trim(), "a/b/c");
}

#[test]
fn cg_for_listdir() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
func main() -> i32 {
    let mut count = 0
    for e in listdir("examples") {
        count = count + 1
    }
    println(count)
    0
}
"#,
    )
    .unwrap();
    let n: i32 = out.trim().parse().unwrap_or(0);
    assert!(n > 0, "expected entries, got {}", n);
}

// ====== Codegen: Record Operations ======

#[test]
fn cg_record_create_and_access() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
type Point { x: i32, y: i32 }
func main() -> i32 {
    let p = Point { x: 3, y: 4 }
    println(p.x)
    println(p.y)
    0
}
"#,
    )
    .unwrap();
    assert_eq!(out.trim(), "3\n4");
}

// ====== Codegen: Enum Pattern Matching ======

#[test]
fn cg_enum_match() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
type Color { Red Green Blue }
func name(c: Color) -> string {
    match c { Red => "red", Green => "green", Blue => "blue" }
}
func main() -> i32 {
    println(name(Red))
    println(name(Blue))
    0
}
"#,
    )
    .unwrap();
    assert_eq!(out.trim(), "red\nblue");
}

// ====== Codegen: Error Handling ======

#[test]
fn cg_file_exists_check() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run("func main() -> i32 { if file_exists(\"examples/hello.mimi\") { println(\"yes\") } else { println(\"no\") } 0 }").unwrap();
    assert_eq!(out.trim(), "yes");
}

// ====== Codegen: Conversion ======

#[test]
fn cg_to_string_int() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run("func main() -> i32 { println(to_string(42)); 0 }").unwrap();
    assert_eq!(out.trim(), "42");
}

#[test]
fn cg_to_json_int() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run("func main() -> i32 { println(to_json(42)); 0 }").unwrap();
    assert_eq!(out.trim(), "42");
}

// ====== Codegen: Large Computation ======

#[test]
fn cg_sum_1_to_100() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
func main() -> i32 {
    let mut sum = 0
    for i in range(1, 101) {
        sum = sum + i
    }
    println(sum)
    0
}
"#,
    )
    .unwrap();
    assert_eq!(out.trim(), "5050");
}

#[test]
fn cg_factorial() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
func fact(n: i32) -> i32 { if n <= 1 { 1 } else { n * fact(n - 1) } }
func main() -> i32 { println(fact(10)); 0 }
"#,
    )
    .unwrap();
    assert_eq!(out.trim(), "3628800");
}
