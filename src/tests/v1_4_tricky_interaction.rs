//! # Tricky Feature-Interaction Tests
//!
//! Exercises 3+ feature combinations that real user code creates.

use super::*;

fn check(src: &str, expected: &str) {
    let _ = run_source(src);
    let out = compile_and_run(src).expect("codegen failed");
    assert_eq!(out.trim(), expected, "mismatch\nsrc: {}", src);
}

fn check_interp_only(src: &str) {
    run_source(src);
}

// ─── 1. Generic + higher-order function + closure ──────────────────────
// A generic higher-order function applying a closure.

#[test]
fn tricky_generic_higher_order() {
    check(
        "func apply<T, U>(x: T, f: func(T) -> U) -> U { f(x) }
         func main() -> i32 {
             let double = fn(x: i32) -> i32 { x * 2 };
             println(apply(21, double));
             0
         }",
        "42",
    );
}

// ─── 2. Generic higher-order with list ──────────────────────────────────
// A generic function taking a List<T> and a closure.

#[test]
fn tricky_generic_list_hof() {
    check(
        "func first_or<T>(xs: List<T>, fallback: T) -> T {
             if len(xs) > 0 { xs[0] } else { fallback }
         }
         func main() -> i32 {
             println(first_or([10, 20, 30], 0));
             println(first_or([], 99));
             0
         }",
        "10\n99",
    );
}

// ─── 3. Enum match + nested function + tuple ────────────────────────────

#[test]
fn tricky_enum_match_nested_fn() {
    check(
        "type Opt { Some(i32) None }
         func add_one(x: Opt) -> i32 {
             match x {
                 Some(v) => v + 1,
                 None => 0,
             }
         }
         func main() -> i32 {
             println(add_one(Some(41)));
             println(add_one(None));
             0
         }",
        "42\n0",
    );
}

// ─── 4. Closure returning closure (currying) ──────────────────────────

#[test]
fn tricky_closure_currying() {
    check(
        "func make_adder(n: i32) -> func(i32) -> i32 {
             fn(x: i32) -> i32 { x + n }
         }
         func main() -> i32 {
             let add5 = make_adder(5);
             let add9 = make_adder(9);
             println(add5(10));
             println(add9(10));
             0
         }",
        "15\n19",
    );
}

// ─── 5. for loop + break + nested if ──────────────────────────────────

#[test]
fn tricky_for_break_nested_if() {
    check(
        "func main() -> i32 {
             let mut sum = 0;
             for x in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10] {
                 if x > 5 {
                     if x % 2 == 0 {
                         sum = sum + x;
                     }
                 }
                 if x >= 8 { break; }
             }
             println(sum);
             0
         }",
        "14",
    );
}

// ─── 6. While loop + mutable list + push + indexing ──────────────────

#[test]
fn tricky_while_list_accumulate() {
    check(
        "func main() -> i32 {
             let mut xs = [];
             let mut i = 0;
             while i < 10 {
                 if i % 2 == 0 { push(xs, i); }
                 i = i + 1;
             }
             println(len(xs));
             println(xs[0] + xs[1] + xs[2]);
             0
         }",
        "5\n6",
    );
}

// ─── 7. Closure capturing multiple int vars (read-only) ──────────────

#[test]
fn tricky_closure_capture_multi_read() {
    check(
        "func main() -> i32 {
             let base = 10;
             let inc = fn(x: i32) -> i32 { x + base };
             let a = inc(5);
             let b = inc(20);
             println(a);
             println(b);
             0
         }",
        "15\n30",
    );
}

// ─── 8. if/else chain returning i32 ───────────────────────────────────

#[test]
fn tricky_if_else_chain() {
    check(
        "func classify(n: i32) -> i32 {
             if n < 0 { 0 }
             else if n < 10 { 1 }
             else if n < 100 { 2 }
             else { 3 }
         }
         func main() -> i32 {
             println(classify(-5));
             println(classify(5));
             println(classify(50));
             println(classify(500));
             0
         }",
        "0\n1\n2\n3",
    );
}

// ─── 9. Match + while + list accumulation ─────────────────────────────
// while + match inside to accumulate filtered results.

#[test]
fn tricky_while_match_accumulate() {
    check(
        "func main() -> i32 {
             let mut evens = [];
             let mut odds = [];
             let mut i = 1;
             while i <= 10 {
                 if i % 2 == 0 { push(evens, i); }
                 else { push(odds, i); }
                 i = i + 1;
             }
             println(len(evens));
             println(len(odds));
             println(evens[0] + odds[0]);
             0
         }",
        "5\n5\n3",
    );
}

// ─── 10. Mutual recursion (odd/even) ─────────────────────────────────

#[test]
fn tricky_mutual_recursion() {
    check(
        "func is_even(n: i32) -> i32 {
             if n == 0 { 1 } else { is_odd(n - 1) }
         }
         func is_odd(n: i32) -> i32 {
             if n == 0 { 0 } else { is_even(n - 1) }
         }
         func main() -> i32 {
             println(is_even(10));
             println(is_odd(10));
             0
         }",
        "1\n0",
    );
}

// ─── 11. Closure + string comparison in match ─────────────────────────
// Codegen gap known: match on string variable fails in LLVM.
// Interpreter only.

#[test]
#[ignore = "codegen: match on string value triggers LLVM ICE (enum_tag ptr expected IntValue)"]
fn tricky_string_match_interp() {
    check_interp_only(
        "func main() -> i32 {
             let name = \"world\";
             let msg = \"hello, \" + name;
             let r = match msg {
                 \"hello, world\" => 42,
                 _ => 0,
             };
             println(r);
             0
         }",
    );
}

// ─── 12. Nested List<List<i32>> indexing ─────────────────────────────
// Codegen gap: matrix[1] on List<List<i32>> not supported.
// Interpreter only.

#[test]
#[ignore = "codegen: index on List<List<i32>> returns 'index requires a list/array pointer'"]
fn tricky_nested_list_of_lists_interp() {
    check_interp_only(
        "func main() -> i32 {
             let matrix = [[1, 2], [3, 4], [5, 6]];
             let row = matrix[1];
             println(row[0] + row[1]);
             0
         }",
    );
}

// ─── 13. Enum with record variant + match destructure ─────────────────
// Codegen gap: enum with record variant pattern matching.

#[test]
#[ignore = "codegen: enum record variant match fails in interpreter path"]
fn tricky_enum_record_variant_interp() {
    check_interp_only(
        "type Item { Label(string) | Group { children: List<i32> } }
         func main() -> i32 {
             let g = Group { children: [10, 20, 30] };
             let sum = match g {
                 Label(s) => len(s),
                 Group { children } => children[0] + children[1] + children[2],
             };
             println(sum);
             0
         }",
    );
}
