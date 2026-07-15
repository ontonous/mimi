//! # Tricky Feature-Interaction Tests
//!
//! Exercises 3+ feature combinations that real user code creates.

use super::*;

fn check(src: &str, expected: &str) {
    let _ = run_source(src);
    let out = compile_and_run(src).expect("codegen failed");
    assert_eq!(out.trim(), expected, "mismatch\nsrc: {}", src);
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

#[test]
fn tricky_string_match() {
    check(
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
        "42",
    );
}

// ─── 12. Nested List<List<i32>> indexing ─────────────────────────────

#[test]
fn tricky_nested_list_of_lists() {
    check(
        "func main() -> i32 {
             let matrix = [[1, 2], [3, 4], [5, 6]];
             let row = matrix[1];
             println(row[0] + row[1]);
             0
         }",
        "7",
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Group 1: Result + match + list operations (4 tests)
// ═══════════════════════════════════════════════════════════════════════

// 1a: Result-like enum -> match -> extract values into list.
// Combines: enum, match, list push, function with if/else.

#[test]
fn tricky_result_match_list() {
    check(
        "type MyResult { Ok(i32) Err(i32) }
         func div_safe(a: i32, b: i32) -> MyResult {
             if b == 0 { Err(-1) } else { Ok(a / b) }
         }
         func main() -> i32 {
             let mut vals = [];
             match div_safe(10, 2) {
                 Ok(v) => push(vals, v),
                 Err(e) => push(vals, e),
             }
             match div_safe(10, 0) {
                 Ok(v) => push(vals, v),
                 Err(e) => push(vals, e),
             }
             match div_safe(15, 3) {
                 Ok(v) => push(vals, v),
                 Err(e) => push(vals, e),
             }
             println(vals[0]);
             println(vals[1]);
             println(vals[2]);
             0
         }",
        "5\n-1\n5",
    );
}

// 1b: Option-like enum -> match Some/None -> push to list.
// Combines: enum, for loop, match, list push.

#[test]
fn tricky_option_match_push() {
    check(
        "type MyOption { Some(i32) None }
         func try_get(idx: i32) -> MyOption {
             if idx >= 0 && idx < 5 { Some(idx * 10) } else { None }
         }
         func main() -> i32 {
             let mut vals = [];
             let indexes = [0, 2, 5, 3];
             for i in indexes {
                 match try_get(i) {
                     Some(v) => push(vals, v),
                     None => push(vals, -1),
                 }
             }
             println(vals[0]);
             println(vals[1]);
             println(vals[2]);
             println(vals[3]);
             0
         }",
        "0\n20\n-1\n30",
    );
}

// 1c: Chain multiple Result-returning operations with nested match.
// Combines: enum, nested match, chained operations, list.

#[test]
fn tricky_result_chain() {
    check(
        "type MyResult { Ok(i32) Err(i32) }
         func step1(x: i32) -> MyResult {
             if x > 0 { Ok(x + 1) } else { Err(-1) }
         }
         func step2(x: i32) -> MyResult {
             if x < 100 { Ok(x * 2) } else { Err(-2) }
         }
         func main() -> i32 {
             let mut vals = [];
             match step1(5) {
                 Ok(v) => match step2(v) {
                     Ok(r) => push(vals, r),
                     Err(e) => push(vals, e),
                 },
                 Err(e) => push(vals, e),
             }
             match step1(-1) {
                 Ok(v) => match step2(v) {
                     Ok(r) => push(vals, r),
                     Err(e) => push(vals, e),
                 },
                 Err(e) => push(vals, e),
             }
             println(vals[0]);
             println(vals[1]);
             0
         }",
        "12\n-1",
    );
}

// 1d: Match Option with default values and list accumulation.
// Combines: enum, for loop, match with default, list arithmetic.

#[test]
fn tricky_option_default() {
    check(
        "type MyOption { Some(i32) None }
         func lookup(k: i32) -> MyOption {
             if k % 2 == 0 { Some(k * 10) } else { None }
         }
         func main() -> i32 {
             let keys = [0, 1, 2, 3, 4];
             let mut vals = [];
             for k in keys {
                 match lookup(k) {
                     Some(v) => push(vals, v),
                     None => push(vals, -1),
                 }
             }
             println(vals[0]);
             println(vals[1]);
             println(vals[2]);
             println(vals[3]);
             println(vals[4]);
             0
         }",
        "0\n-1\n20\n-1\n40",
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Group 2: Records + generics + closures (3 tests)
// ═══════════════════════════════════════════════════════════════════════

// 2a: Generic record type + closure processing.
// Combines: generic record, generic higher-order function, closure capture.

#[test]
#[ignore = "CODEGEN: generic record field access after monomorphization still i64 (v0.31 type engine)"]
#[test]
fn tricky_record_generic_closure() {
    check(
        "type Box<T> { value: T }
         func process<T>(b: Box<T>, f: func(T) -> T) -> T {
             f(b.value)
         }
         func main() -> i32 {
             let double = fn(x: i32) -> i32 { x * 2 };
             println(process(Box { value: 21 }, double));
             0
         }",
        "42",
    );
}

// 2b: Generic swap on single-type Pair record.
// Combines: generic record, generic function, field access.

#[ignore = "CODEGEN: generic record Pair<T> field access after monomorphization still i64 (v0.31 type engine)"]
#[test]
fn tricky_record_swap_generic() {
    check(
        "type Pair<T> { a: T, b: T }
         func swap<T>(p: Pair<T>) -> Pair<T> {
             Pair { a: p.b, b: p.a }
         }
         func main() -> i32 {
             let p = Pair { a: 10, b: 20 };
             let q = swap(p);
             println(q.a);
             println(q.b);
             0
         }",
        "20\n10",
    );
}

// 2c: Nested record construction and field access chain.
// Combines: nested record types, field access, constructor expressions.

#[test]
fn tricky_nested_record_access() {
    check(
        "type Inner { val: i32 }
         type Outer { inner: Inner, extra: i32 }
         func main() -> i32 {
             let o = Outer { inner: Inner { val: 42 }, extra: 7 };
             println(o.inner.val);
             println(o.extra);
             0
         }",
        "42\n7",
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Group 3: Lists + for/while + closures + match (4 tests)
// ═══════════════════════════════════════════════════════════════════════

// 3a: Build list -> iterate with closure -> accumulate.
// Combines: list literal, for loop, closure, list push.

#[test]
fn tricky_list_map_closure() {
    check(
        "func apply_all<T>(xs: List<T>, f: func(T) -> T) -> List<T> {
             let mut out = [];
             for x in xs { push(out, f(x)); }
             out
         }
         func main() -> i32 {
             let nums = [1, 2, 3, 4, 5];
             let double = fn(x: i32) -> i32 { x * 2 };
             let mapped = apply_all(nums, double);
             println(mapped[0]);
             println(mapped[4]);
             0
         }",
        "2\n10",
    );
}

// 3b: List of ints, match each, filter into new list.
// Combines: list literal, for loop, match on int, list push, if/else chains.

#[test]
fn tricky_list_filter_match() {
    check(
        "func classify(x: i32) -> i32 {
             match x % 3 {
                 0 => 0,
                 1 => 1,
                 _ => 2,
             }
         }
         func main() -> i32 {
             let nums = [1, 2, 3, 4, 5, 6, 7, 8, 9];
             let mut class1 = [];
             let mut class2 = [];
             for x in nums {
                 let c = classify(x);
                 if c == 1 { push(class1, x); }
                 else if c == 2 { push(class2, x); }
             }
             println(len(class1));
             println(len(class2));
             println(class1[0]);
             println(class2[0]);
             0
         }",
        "3\n3\n1\n2",
    );
}

// 3c: Nested for loops building and accessing 2D list structure.
// Combines: nested for loops, list push, nested list indexing.
// ISSUE: type checker cannot infer List<List<i32>> without annotation on `rows`.
// `let mut rows = []` gives List<unknown>, and push() can't retroactively set the
// element type at the type-checker level. Fix: add type annotation to `rows`.
// With annotation, codegen works (inttoptr for i64 heap pointer to inner list struct).

#[ignore = "TYPE_INFERENCE: push() cannot propagate element type to empty list without annotation"]
#[test]
fn tricky_nested_loop_list() {
    check(
        "func main() -> i32 {
             let bases = [0, 10, 20];
             let mut rows: List<List<i32>> = [];
             for b in bases {
                 let mut row = [];
                 push(row, b + 1);
                 push(row, b + 2);
                 push(row, b + 3);
                 push(rows, row);
             }
             println(rows[0][0] + rows[0][1] + rows[0][2]);
             println(rows[1][1]);
             println(rows[2][2]);
             0
         }",
        "6\n12\n23",
    );
}

// 3d: While loop + closure capturing and accumulating state.
// Combines: while loop, closure capturing local var, mutable list, list indexing.

#[test]
fn tricky_while_closure_mutate() {
    check(
        "func main() -> i32 {
             let factor = 3;
             let multiply = fn(x: i32) -> i32 { x * factor };
             let mut i = 1;
             let mut results = [];
             while i <= 4 {
                 push(results, multiply(i));
                 i = i + 1;
             }
             println(results[0]);
             println(results[1]);
             println(results[2]);
             println(results[3]);
             0
         }",
        "3\n6\n9\n12",
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Group 4: Strings + match + methods + Result (3 tests)
// ═══════════════════════════════════════════════════════════════════════

// 4a: String concatenation + match + list accumulation.
// Combines: string concat, match on string, for loop, list push.

#[test]
fn tricky_string_match_concat() {
    check(
        "func main() -> i32 {
             let names = [\"cat\", \"dog\", \"bird\"];
             let mut out = [];
             for n in names {
                 let full = \"my_\" + n;
                 let r = match full {
                     \"my_cat\" => 1,
                     \"my_dog\" => 2,
                     _ => 0,
                 };
                 push(out, r);
             }
             println(out[0]);
             println(out[1]);
             println(out[2]);
             0
         }",
        "1\n2\n0",
    );
}

// 4b: String prefixing with match on content + numeric extraction.
// Combines: string building, match on string, list of i32 results.

#[test]
fn tricky_string_prefix_match() {
    check(
        "func main() -> i32 {
             let items = [\"abc\", \"hello\", \"xy\"];
             let mut out = [];
             for s in items {
                 let r = match s {
                     \"hello\" => 10,
                     \"abc\" => 20,
                     _ => 0,
                 };
                 push(out, r);
             }
             println(out[0]);
             println(out[1]);
             println(out[2]);
             0
         }",
        "20\n10\n0",
    );
}

// 4c: String building with multiple concatenations + match.
// Combines: string concat with multiple parts, match, Result-like return.

#[test]
fn tricky_string_concat_match() {
    check(
        "func make_greeting(greet: string, name: string) -> string {
             greet + \", \" + name + \"!\"
         }
         func main() -> i32 {
             let msg = make_greeting(\"hello\", \"world\");
             let r = match msg {
                 \"hello, world!\" => 42,
                 _ => 0,
             };
             println(r);
             0
         }",
        "42",
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Group 5: Enums + generics + recursion (3 tests)
// ═══════════════════════════════════════════════════════════════════════

// 5a: Recursive enum (Expr: Num | Add) with match-based evaluation.
// Combines: recursive enum definition, match, recursion.

#[test]
fn tricky_recursive_enum_expr() {
    check(
        "type Expr { Num(i32) Add(Expr, Expr) }
         func eval(e: Expr) -> i32 {
             match e {
                 Num(n) => n,
                 Add(a, b) => eval(a) + eval(b),
             }
         }
         func main() -> i32 {
             let e = Add(Num(1), Add(Num(2), Num(3)));
             println(eval(e));
             0
         }",
        "6",
    );
}

// 5b: Nested enum matching (Result<Option<i32>>-like).
// Combines: two enum types, nested match, constructor nesting.

#[test]
fn tricky_enum_nested_match() {
    check(
        "type Status { Ok(i32) Err(i32) }
         type Container { Value(Status) Empty }
         func extract(c: Container) -> i32 {
             match c {
                 Value(s) => match s {
                     Ok(v) => v,
                     Err(e) => e,
                 },
                 Empty => -1,
             }
         }
         func main() -> i32 {
             println(extract(Value(Ok(42))));
             println(extract(Value(Err(99))));
             println(extract(Empty));
             0
         }",
        "42\n99\n-1",
    );
}

// 5c: Mutual recursion with recursive enum (even/odd on tree-like data).
// Combines: recursive enum, mutual recursion, match.

#[test]
fn tricky_mutual_recursion_enum() {
    check(
        "type Tree { Leaf(i32) Node(Tree, Tree) }
         func count_even(t: Tree) -> i32 {
             match t {
                 Leaf(n) => if n % 2 == 0 { 1 } else { 0 },
                 Node(l, r) => count_even(l) + count_even(r),
             }
         }
         func main() -> i32 {
             let t = Node(Leaf(1), Node(Leaf(2), Leaf(4)));
             println(count_even(t));
             0
         }",
        "2",
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Group 6: Newtype + trait + method calls (2 tests)
// ═══════════════════════════════════════════════════════════════════════

// 6a: Newtype + trait impl + method dispatch.
// Combines: newtype, trait definition, trait impl with self, method call.

#[test]
fn tricky_newtype_method() {
    check(
        "newtype UserId = i32
         trait Identifiable {
             func id() -> i32;
         }
         impl Identifiable for UserId {
             func id() -> i32 { self.0 }
         }
         func main() -> i32 {
             let u = UserId(42);
             println(u.id());
             0
         }",
        "42",
    );
}

// 6b: Record + trait + method call returning computed value.
// Combines: record type, trait definition, trait impl, method call.

#[test]
fn tricky_record_trait_method() {
    check(
        "type MyVal { val: i32 }
         trait Double {
             func twice() -> i32;
         }
         impl Double for MyVal {
             func twice() -> i32 { self.val * 2 }
         }
         func main() -> i32 {
             let v = MyVal { val: 21 };
             println(v.twice());
             0
         }",
        "42",
    );
}

// Enum record variants not supported in Mimi parser (syntax error).
// Removed — this is not a codegen gap but an unsupported syntax.
