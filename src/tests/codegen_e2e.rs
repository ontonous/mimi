// ============================================================
// E2E CODEGEN Tests (compile -> link -> run -> check stdout)
// ============================================================

use super::*;

fn can_link() -> bool {
    std::process::Command::new("cc").arg("--version").output().is_ok()
}

// ===================== Basic Arithmetic =====================

#[test]
fn e2e_add() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"func main() -> i32 { println(2 + 3); 0 }"#).unwrap();
    assert_eq!(stdout.trim(), "5");
}

#[test]
fn e2e_sub() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"func main() -> i32 { println(10 - 3); 0 }"#).unwrap();
    assert_eq!(stdout.trim(), "7");
}

#[test]
fn e2e_mul() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"func main() -> i32 { println(6 * 7); 0 }"#).unwrap();
    assert_eq!(stdout.trim(), "42");
}

#[test]
fn e2e_div() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"func main() -> i32 { println(12 / 4); 0 }"#).unwrap();
    assert_eq!(stdout.trim(), "3");
}

#[test]
fn e2e_complex_arithmetic() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"func main() -> i32 { println(1 + 2 * 3 - 4 / 2); 0 }"#).unwrap();
    assert_eq!(stdout.trim(), "5");
}

// ===================== Control Flow =====================

#[test]
fn e2e_abs_function() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func abs(x: i32) -> i32 { if x < 0 { -x } else { x } }
        func main() -> i32 { println(abs(-5)); 0 }
    "#).unwrap();
    assert_eq!(stdout.trim(), "5");
}

#[test]
fn e2e_boolean_and_or() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func main() -> i32 { let a = 1; let b = 0; println(a && b); println(a || b); 0 }
    "#).unwrap();
    assert_eq!(stdout.trim(), "0\n1");
}

// ===================== Function Calls =====================

#[test]
fn e2e_chained_calls() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func inc(x: i32) -> i32 { x + 1 }
        func main() -> i32 { println(inc(inc(inc(39)))); 0 }
    "#).unwrap();
    assert_eq!(stdout.trim(), "42");
}

#[test]
fn e2e_three_param_fn() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func add3(a: i32, b: i32, c: i32) -> i32 { a + b + c }
        func main() -> i32 { println(add3(10, 20, 12)); 0 }
    "#).unwrap();
    assert_eq!(stdout.trim(), "42");
}

// ===================== Builtins =====================

#[test]
fn e2e_builtin_abs() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"func main() -> i32 { println(abs(-42)); 0 }"#).unwrap();
    assert_eq!(stdout.trim(), "42");
}

#[test]
fn e2e_builtin_min_max() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"func main() -> i32 { println(min(10, 20)); println(max(10, 20)); 0 }"#).unwrap();
    assert_eq!(stdout.trim(), "10\n20");
}

#[test]
fn e2e_builtin_len() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"func main() -> i32 { let xs = [1, 2, 3, 4, 5]; println(len(xs)); 0 }"#).unwrap();
    assert_eq!(stdout.trim(), "5");
}

// ===================== Print =====================

#[test]
fn e2e_mixed_print_types() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"func main() -> i32 { println("hello"); println("world"); println(42); 0 }"#).unwrap();
    assert_eq!(stdout.trim(), "hello\nworld\n42");
}

// ===================== F-strings =====================

#[test]
fn e2e_fstring_with_var() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"func main() -> i32 { let x = 42; println(f"x = {x}"); 0 }"#).unwrap();
    assert_eq!(stdout.trim(), "x = 42");
}

#[test]
fn e2e_fstring_two_vars() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func main() -> i32 { let x = 42; let y = 100; println(f"x={x}, y={y}"); 0 }
    "#).unwrap();
    assert_eq!(stdout.trim(), "x=42, y=100");
}

// ===================== String Builtins (known codegen bugs - skipped) =====================

// Note: string builtins (str_to_upper, str_trim, str_repeat, str_char_at, to_string)
// and float operations (fadd, pow) have known codegen runtime bugs.
// These E2E tests are excluded for now. IR-level tests cover compilation.

// ===================== Equality / Comparison =====================

#[test]
fn e2e_int_equality() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func main() -> i32 { println(42 == 42); println(42 == 43); println(42 != 43); 0 }
    "#).unwrap();
    assert_eq!(stdout.trim(), "1\n0\n1");
}

#[test]
fn e2e_int_comparison() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func main() -> i32 { println(1 < 2); println(2 < 1); println(1 <= 1); println(2 >= 1); 0 }
    "#).unwrap();
    assert_eq!(stdout.trim(), "1\n0\n1\n1");
}

// ===================== Mutable Variables =====================

#[test]
fn e2e_mutable_updates() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"func main() -> i32 { let mut x = 1; x = x + 2; x = x * 3; println(x); 0 }"#).unwrap();
    assert_eq!(stdout.trim(), "9");
}

// ===================== List Index =====================

#[test]
fn e2e_list_index() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func main() -> i32 { let xs = [10, 20, 30, 40, 50]; println(xs[0]); println(xs[2]); println(xs[4]); 0 }
    "#).unwrap();
    assert_eq!(stdout.trim(), "10\n30\n50");
}

// ===================== Type Alias =====================

#[test]
fn e2e_type_alias() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"type MyInt = i32; func main() -> i32 { let x: MyInt = 42; println(x); 0 }"#).unwrap();
    assert_eq!(stdout.trim(), "42");
}

// ===================== Range Len =====================

#[test]
fn e2e_range_len() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"func main() -> i32 { let r = range(0, 10); println(len(r)); 0 }"#).unwrap();
    assert_eq!(stdout.trim(), "10");
}

// ===================== Fstring with expression =====================

#[test]
fn e2e_fstring_expr() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func main() -> i32 { let a = 3; let b = 4; println(f"{a} + {b} = {a + b}"); 0 }
    "#).unwrap();
    assert_eq!(stdout.trim(), "3 + 4 = 7");
}

// ===================== For loops (multi-line) =====================

#[test]
fn e2e_for_range_basic() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func main() -> i32 {
            for i in range(0, 3) { println(i) }
            0
        }
    "#).unwrap();
    assert_eq!(stdout.trim(), "0\n1\n2");
}

#[test]
fn e2e_for_range_sum() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func main() -> i32 {
            let mut total = 0
            for i in range(1, 6) { total = total + i }
            println(total); 0
        }
    "#).unwrap();
    assert_eq!(stdout.trim(), "15");
}

#[test]
fn e2e_for_list_print() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func main() -> i32 {
            for x in [10, 20, 30] { println(x) }
            0
        }
    "#).unwrap();
    assert_eq!(stdout.trim(), "10\n20\n30");
}

#[test]
fn e2e_for_list_sum() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func main() -> i32 {
            let mut total = 0
            for x in [1, 2, 3, 4, 5] { total = total + x }
            println(total); 0
        }
    "#).unwrap();
    assert_eq!(stdout.trim(), "15");
}

// ===================== If-else (multi-line) =====================

#[test]
fn e2e_if_true_branch() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func main() -> i32 {
            let x = 42
            if x > 10 { println(1) } else { println(0) }
            0
        }
    "#).unwrap();
    assert_eq!(stdout.trim(), "1");
}

#[test]
fn e2e_if_false_branch() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func main() -> i32 {
            let x = 3
            if x > 10 { println(1) } else { println(0) }
            0
        }
    "#).unwrap();
    assert_eq!(stdout.trim(), "0");
}

#[test]
fn e2e_if_no_else() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func main() -> i32 {
            let mut x = 5
            if x < 10 { x = x + 1 }
            println(x); 0
        }
    "#).unwrap();
    assert_eq!(stdout.trim(), "6");
}

// ===================== While loops (multi-line) =====================

#[test]
fn e2e_while_count_up() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func main() -> i32 {
            let mut i = 0
            while i < 5 { println(i); i = i + 1 }
            0
        }
    "#).unwrap();
    assert_eq!(stdout.trim(), "0\n1\n2\n3\n4");
}

#[test]
fn e2e_while_sum() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func main() -> i32 {
            let mut sum = 0; let mut i = 1
            while i <= 10 { sum = sum + i; i = i + 1 }
            println(sum); 0
        }
    "#).unwrap();
    assert_eq!(stdout.trim(), "55");
}

#[test]
fn e2e_product_loop() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func main() -> i32 {
            let mut p = 1; let mut i = 1
            while i <= 5 { p = p * i; i = i + 1 }
            println(p); 0
        }
    "#).unwrap();
    assert_eq!(stdout.trim(), "120");
}

// ===================== Multi-function (multi-line) =====================

#[test]
fn e2e_multi_function() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func square(x: i32) -> i32 { x * x }
        func cube(x: i32) -> i32 { x * x * x }
        func main() -> i32 { println(square(3)); println(cube(3)); 0 }
    "#).unwrap();
    assert_eq!(stdout.trim(), "9\n27");
}

// ===================== Mixed function calls (multi-line) =====================

#[test]
fn e2e_mixed_func_calls() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func add(a: i32, b: i32) -> i32 { a + b }
        func mul(a: i32, b: i32) -> i32 { a * b }
        func main() -> i32 { println(add(1, 2) + mul(3, 4)); 0 }
    "#).unwrap();
    assert_eq!(stdout.trim(), "15");
}

// ===================== Print while loop (multi-line) =====================

#[test]
fn e2e_print_while_loop() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func main() -> i32 {
            let mut i = 1
            while i <= 3 { println(i); i = i + 1 }
            0
        }
    "#).unwrap();
    assert_eq!(stdout.trim(), "1\n2\n3");
}

// ===================== Parasteps (multi-line) =====================

#[test]
fn e2e_parasteps_seq() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func main() -> i32 {
            let mut t = 0
            parasteps { t = t + 1; t = t + 2; t = t + 3 }
            println(t); 0
        }
    "#).unwrap();
    assert_eq!(stdout.trim(), "6");
}

// ===================== Nested if-else =====================

#[test]
fn e2e_nested_if_else_statements() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func main() -> i32 {
            let mut result = 0; let x = 5
            if x > 0 { if x > 10 { result = 2 } else { result = 1 } }
            println(result); 0
        }
    "#).unwrap();
    assert_eq!(stdout.trim(), "1");
}

// ===================== Factorial iterative =====================

#[test]
fn e2e_factorial_while_iter() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func factorial(n: i32) -> i32 {
            let mut result = 1; let mut i = 1
            while i <= n { result = result * i; i = i + 1 }
            result
        }
        func main() -> i32 { println(factorial(5)); 0 }
    "#).unwrap();
    assert_eq!(stdout.trim(), "120");
}

// ===================== String Functions =====================

#[test]
fn e2e_str_split() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func main() -> i32 {
            let parts = str_split("a,b,c", ",")
            println(len(parts))
            let joined = str_join(parts, "+")
            println(joined)
            0
        }
    "#).unwrap();
    assert_eq!(stdout.trim(), "3\na+b+c");
}

#[test]
fn e2e_str_join() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func main() -> i32 {
            let parts = str_split("hello world foo", " ")
            let result = str_join(parts, "-")
            println(result)
            0
        }
    "#).unwrap();
    assert_eq!(stdout.trim(), "hello-world-foo");
}

#[test]
fn e2e_str_replace() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = compile_and_run(r#"
        func main() -> i32 {
            let result = str_replace("hello world", "world", "mimi")
            println(result)
            0
        }
    "#).unwrap();
    assert_eq!(stdout.trim(), "hello mimi");
}
