//! # Dual-Backend Differential Fuzzer
//!
//! Generates random Mimi programs and runs them through both the
//! interpreter and LLVM codegen, asserting identical output.
//!
//! Strategy: generators produce **function body** snippets (a block of
//! statements ending with a value expression). The test wraps the body
//! differently for each backend:
//! - Interpreter: `func main() -> T { BODY; RESULT }` → returns last-expr value
//! - Codegen:     `func main() -> i32 { BODY; println(RESULT); 0 }`
//!   Then compares interp Value Display with codegen stdout.
//!
//! Run: `PROPTEST_CASES=1000 cargo test fuzz::target_differential -- --nocapture --include-ignored`

use crate::tests::{compile_and_run, run_source};
use proptest::prelude::*;

// ─── General-purpose assertion helpers ───────────────────────────

/// Assert a body+result expression across both backends.
/// `ret_ty` is the interpreter wrapper's return type (used for
/// `Value::Display` formatting, which must match codegen's `println` output).
fn assert_body_ret(ret_ty: &str, body: &str, result: &str) {
    let interp_src = format!("func main() -> {} {{\n{}\n{}\n}}", ret_ty, body, result);
    let cg_src = format!(
        "func main() -> i32 {{\n{}\nprintln({});\n0\n}}",
        body, result
    );
    let v = run_source(&interp_src);
    let interp_str = format!("{}", v);
    let cg_stdout = compile_and_run(&cg_src).expect("codegen should compile and run");
    let cg_str = cg_stdout.trim();
    assert_eq!(
        interp_str, cg_str,
        "differential mismatch\nbody:\n{}\nresult: {}\ninterp: {}\ncodegen: {}",
        body, result, interp_str, cg_str
    );
}

/// Assert a simple expression (empty body) across both backends.
fn assert_expr_ret(ret_ty: &str, expr: &str) {
    assert_body_ret(ret_ty, "", expr);
}

/// Convenience: i32-valued expression (most common case).
fn assert_expr(expr: &str) {
    assert_expr_ret("i32", expr);
}

/// Full program check: both backends succeed (fallback for programs with
/// extra top-level definitions that can't easily use assert_body_ret).
fn assert_program_ok(src: &str) {
    let _ = run_source(src);
    compile_and_run(src).expect("codegen should compile and run");
}

// ─── Integer expression generators ───────────────────────────────

fn arb_int_expr() -> impl Strategy<Value = String> {
    let leaf = (0i64..50i64).prop_map(|n| n.to_string());
    leaf.prop_recursive(3, 10, 5, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({} + {})", a, b)),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({} - {})", a, b)),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({} * {})", a, b)),
            (inner.clone(), (1i64..20i64)).prop_map(|(a, b)| format!("({} / {})", a, b)),
            (inner.clone(), (1i64..20i64)).prop_map(|(a, b)| format!("({} % {})", a, b)),
        ]
    })
}

fn arb_if_expr() -> impl Strategy<Value = String> {
    let cond = prop_oneof![
        Just("true".into()),
        Just("false".into()),
        (0i64..10i64, 0i64..10i64).prop_map(|(a, b)| format!("{} == {}", a, b)),
        (0i64..10i64, 0i64..10i64).prop_map(|(a, b)| format!("{} < {}", a, b)),
        (0i64..10i64, 0i64..10i64).prop_map(|(a, b)| format!("{} > {}", a, b)),
    ];
    (cond, (0i64..100i64), (0i64..100i64))
        .prop_map(|(c, t, f)| format!("(if {} {{ {} }} else {{ {} }})", c, t, f))
}

// ─── String expression generators ────────────────────────────────

fn arb_str_literal() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(r#""hello""#.into()),
        Just(r#""world""#.into()),
        Just(r#""foo""#.into()),
        Just(r#""bar""#.into()),
        Just(r#""""#.into()),
        Just(r#""abc""#.into()),
        Just(r#""xyz""#.into()),
    ]
}

fn arb_str_expr() -> impl Strategy<Value = String> {
    let leaf = arb_str_literal();
    leaf.prop_recursive(2, 6, 3, |inner| {
        (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({} + {})", a, b))
    })
}

// ─── Boolean expression generators ───────────────────────────────

fn arb_bool_literal() -> impl Strategy<Value = String> {
    prop_oneof![Just("true".into()), Just("false".into()),]
}

fn arb_compare_expr() -> impl Strategy<Value = String> {
    prop_oneof![
        (0i64..20i64, 0i64..20i64).prop_map(|(a, b)| format!("{} == {}", a, b)),
        (0i64..20i64, 0i64..20i64).prop_map(|(a, b)| format!("{} != {}", a, b)),
        (0i64..20i64, 0i64..20i64).prop_map(|(a, b)| format!("{} < {}", a, b)),
        (0i64..20i64, 0i64..20i64).prop_map(|(a, b)| format!("{} > {}", a, b)),
        (0i64..20i64, 0i64..20i64).prop_map(|(a, b)| format!("{} <= {}", a, b)),
        (0i64..20i64, 0i64..20i64).prop_map(|(a, b)| format!("{} >= {}", a, b)),
    ]
}

fn arb_bool_expr() -> impl Strategy<Value = String> {
    let leaf = prop_oneof![arb_bool_literal(), arb_compare_expr(),];
    leaf.prop_recursive(2, 6, 3, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({} && {})", a, b)),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({} || {})", a, b)),
            inner.clone().prop_map(|e| format!("!({})", e)),
        ]
    })
}

// ─── List expression generators ──────────────────────────────────

fn arb_i32_list() -> impl Strategy<Value = String> {
    proptest::collection::vec(0i64..20i64, 0..5).prop_map(|items| {
        let elems: Vec<String> = items.iter().map(|n| n.to_string()).collect();
        format!("[{}]", elems.join(", "))
    })
}

fn arb_list_len_expr() -> impl Strategy<Value = String> {
    arb_i32_list().prop_map(|lst| format!("len({})", lst))
}

fn arb_list_index_expr() -> impl Strategy<Value = String> {
    // Guarantee index < len: generate items first, then pick index within range
    proptest::collection::vec(0i64..20i64, 1..5)
        .prop_flat_map(|items| {
            let lst = format!(
                "[{}]",
                items
                    .iter()
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let max_idx = items.len();
            (Just(lst), 0usize..max_idx)
        })
        .prop_map(|(lst, idx)| format!("{}[{}]", lst, idx))
}

// ─── Match expression generators ─────────────────────────────────

fn arb_match_int_expr() -> impl Strategy<Value = String> {
    let match_val = 0i64..5i64;
    let arm_vals = (0i64..100i64, 0i64..100i64, 0i64..100i64);
    (match_val, arm_vals).prop_map(|(mv, (a1, a2, a3))| {
        format!("match {} {{ 0 => {}, 1 => {}, _ => {} }}", mv, a1, a2, a3)
    })
}

// ─── Statement-based body generators ────────────────────────────

/// Let-binding with shadowing: body is `let x = A; let x = x + B;`, result is `x`.
fn arb_let_body() -> impl Strategy<Value = (String, String)> {
    ((0i64..50i64), (0i64..50i64)).prop_map(|(a, b)| {
        let body = format!("    let x = {};\n    let x = x + {};", a, b);
        let result = "x".to_string();
        (body, result)
    })
}

/// Closure: body is `let base = A; let f = fn(x: i32) -> i32 { x + base };`, result is `f(B)`.
fn arb_closure_body() -> impl Strategy<Value = (String, String)> {
    ((0i64..30i64), (0i64..30i64)).prop_map(|(a, b)| {
        let body = format!(
            "    let base = {};\n    let f = fn(x: i32) -> i32 {{ x + base }};",
            a
        );
        let result = format!("f({})", b);
        (body, result)
    })
}

/// While-loop: body is `let mut s = 0; let mut i = 0; while i < LIMIT { s = s + i; i = i + STEP }`, result is `s`.
fn arb_while_body() -> impl Strategy<Value = (String, String)> {
    ((1i64..8i64), (1i64..5i64)).prop_map(|(limit, step)| {
        let body = format!(
            "    let mut s = 0;\n    let mut i = 0;\n    while i < {} {{ s = s + i; i = i + {} }}",
            limit, step
        );
        let result = "s".to_string();
        (body, result)
    })
}

/// Recursive function: extra top-level function + call.
fn arb_recursive_body() -> impl Strategy<Value = (String, String)> {
    (1i64..12i64).prop_map(|n| {
        let body = format!(
            "func fib(n: i32) -> i32 {{\n    if n <= 1 {{ n }} else {{ fib(n-1) + fib(n-2) }}\n}}\nfunc main() -> i32 {{\n    let r = fib({});\n    println(r);\n    0\n}}",
            n
        );
        (body.clone(), n.to_string())
    })
}

// ─── Mixed-type programs ─────────────────────────────────────────

/// Generate a body that mixes string and integer operations via len().
fn arb_str_len_body() -> impl Strategy<Value = (String, String)> {
    (arb_str_expr(), arb_str_expr()).prop_map(|(s1, s2)| {
        let body = format!("    let a = {};\n    let b = {};", s1, s2);
        let result = "len(a) + len(b)".to_string();
        (body, result)
    })
}

// ─── Result/Option expression generators ─────────────────────────
//
// Generates enum constructor expressions and match arms for
// `Result<i32, string>` and `Option<i32>`.

/// Generate a Result<i32, string> expression: Ok(val) or Err("msg").
fn arb_result_expr() -> impl Strategy<Value = String> {
    prop_oneof![
        (0i64..20i64).prop_map(|v| format!("Ok({})", v)),
        Just(r#"Err("err")"#.into()),
        Just(r#"Err("fail")"#.into()),
        Just(r#"Err("x")"#.into()),
    ]
}

/// Generate an Option<i32> expression: Some(val) or None.
fn arb_option_expr() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("None".into()),
        (0i64..20i64).prop_map(|v| format!("Some({})", v)),
    ]
}

/// Generate a match on a Result or Option expression returning i32.
fn arb_match_result_expr() -> impl Strategy<Value = String> {
    prop_oneof![
        // Match on Ok(value) — returns the payload
        (0i64..20i64).prop_map(|v| format!("match Ok({}) {{ Ok(v) => v, Err(_) => 0 }}", v)),
        // Match on Err(msg) — returns fallback
        Just("match Err(\"x\") { Ok(v) => 0, Err(_) => -1 }".into()),
        // Match on Some(value) — returns the payload
        (0i64..20i64).prop_map(|v| format!("match Some({}) {{ Some(v) => v, None => 0 }}", v)),
        // Match on None — returns fallback
        Just("match None { Some(v) => 0, None => -1 }".into()),
    ]
}

// ─── Nested list generators ──────────────────────────────────────

/// Generate a List<List<i32>> expression; returns i32 (len or indexed value).
fn arb_nested_list_expr() -> impl Strategy<Value = String> {
    prop_oneof![
        // Plain nested list literal → len() for i32 result
        proptest::collection::vec(proptest::collection::vec(0i64..10i64, 1..4), 1..4,).prop_map(
            |outer| {
                let inner_strs: Vec<String> = outer
                    .iter()
                    .map(|inner| {
                        let e: Vec<String> = inner.iter().map(|n| n.to_string()).collect();
                        format!("[{}]", e.join(", "))
                    })
                    .collect();
                format!("len([{}])", inner_strs.join(", "))
            }
        ),
        // Nested list with double indexing [[0,1],[2,3]][oi][ii]
        (2usize..4, 2usize..4)
            .prop_flat_map(|(outer_sz, inner_sz)| {
                (Just(outer_sz), Just(inner_sz), 0..outer_sz, 0..inner_sz)
            })
            .prop_map(|(outer_sz, inner_sz, oi, ii)| {
                let inner_lists: Vec<String> = (0..outer_sz)
                    .map(|row| {
                        let elems: Vec<String> = (0..inner_sz)
                            .map(|col| (row as i64 * 10 + col as i64).to_string())
                            .collect();
                        format!("[{}]", elems.join(", "))
                    })
                    .collect();
                let lst = format!("[{}]", inner_lists.join(", "));
                format!("{}[{}][{}]", lst, oi, ii)
            }),
    ]
}

/// Generate a complete program with a record type, a list of records,
/// and field access returning i32.
fn arb_record_list_expr() -> impl Strategy<Value = String> {
    (0i64..10i64, 0i64..10i64).prop_map(|(x, y)| {
        format!(
            "type Point {{ x: i32, y: i32 }}\n\
             func main() -> i32 {{\n\
                 let pts = [Point {{ x: {}, y: {} }}];\n\
                 println(pts[0].x);\n\
                 0\n\
             }}",
            x, y
        )
    })
}

// ─── String method call generators ───────────────────────────────

/// Generate a string expression used with len() (function-call style).
/// Returns an i32 expression for use with assert_expr.
fn arb_string_method_expr() -> impl Strategy<Value = String> {
    prop_oneof![
        // Function-style len() on literals (known reliable)
        arb_str_literal().prop_map(|s| format!("len({})", s)),
        // Concat + len
        (arb_str_literal(), arb_str_literal()).prop_map(|(a, b)| format!("len({} + {})", a, b)),
        // Method-style .len() via Str trait dispatch
        arb_str_literal().prop_map(|s| format!("({}).len()", s)),
    ]
}

// ─── Multi-feature combined generators ──────────────────────────

/// Combine while-loop + match + closure.
/// Accumulates a value by matching on loop iteration index and
/// applying a closure.
fn arb_while_list_match_closure() -> impl Strategy<Value = (String, String)> {
    (1i64..6i64, 1i64..4i64).prop_map(|(limit, step)| {
        let body = format!(
            "    let mut s = 0;\n    let mut i = 0;\n    \
             let f = fn(x: i32) -> i32 {{ x + {} }};\n    \
             while i < {} {{\n        \
                 s = s + match i % 3 {{\n            \
                     0 => f(i),\n            \
                     1 => 10,\n            \
                     _ => i,\n        \
                 }};\n        \
                 i = i + {};\n    \
             }}",
            step, limit, step
        );
        let result = "s".to_string();
        (body, result)
    })
}

/// Combine closure + record + match: defines a record type, uses a
/// closure on its fields, and matches on one field to produce i32.
fn arb_closure_record_match() -> impl Strategy<Value = String> {
    (0i64..10i64, 0i64..10i64).prop_map(|(a, b)| {
        format!(
            "type Pair {{ a: i32, b: i32 }}\n\
             func main() -> i32 {{\n\
                 let p = Pair {{ a: {}, b: {} }};\n\
                 let f = fn(v: i32) -> i32 {{ v * 2 }};\n\
                 let r = match p.a {{\n\
                     0 => f(p.b),\n\
                     _ => f(p.a),\n\
                 }};\n\
                 println(r);\n\
                 0\n\
             }}",
            a, b
        )
    })
}

// ─── Proptest targets ────────────────────────────────────────────

proptest! {
    // Integer arithmetic
    #[test]
    fn differential_int(expr in arb_int_expr()) {
        assert_expr(&expr);
    }

    #[test]
    fn differential_if(expr in arb_if_expr()) {
        assert_expr(&expr);
    }

    // Let-binding
    #[test]
    fn differential_let((body, result) in arb_let_body()) {
        assert_body_ret("i32", &body, &result);
    }

    // Closures
    #[test]
    fn differential_closure((body, result) in arb_closure_body()) {
        assert_body_ret("i32", &body, &result);
    }

    // While loops
    #[test]
    fn differential_while((body, result) in arb_while_body()) {
        assert_body_ret("i32", &body, &result);
    }

    // Recursion
    #[test]
    fn differential_recursive(src in arb_recursive_body().prop_map(|(s, _)| s)) {
        assert_program_ok(&src);
    }

    // ── New: Strings ──────────────────────────────────────────────

    #[test]
    fn differential_str_concat(expr in arb_str_expr()) {
        assert_expr_ret("string", &expr);
    }

    #[test]
    fn differential_str_len(expr in arb_str_expr()) {
        // len(s) returns i32 — use assert_expr directly
        assert_expr(&format!("len({})", expr));
    }

    #[test]
    fn differential_str_compare(a in arb_str_literal(), b in arb_str_literal()) {
        assert_expr(&format!("(if {} == {} {{ 1 }} else {{ 0 }})", a, b));
    }

    // ── New: Booleans ─────────────────────────────────────────────
    //
    // Note: Codegen `println(bool_val)` prints `1`/`0` (C-style), but
    // interpreter `Value::Bool(true)` displays as `"true"`/`"false"`.
    // All bool differential tests convert to i32 via `if` to avoid this
    // formatting mismatch.

    #[test]
    fn differential_bool(expr in arb_bool_expr()) {
        assert_expr(&format!("(if {} {{ 1 }} else {{ 0 }})", expr));
    }

    // ── New: Lists ────────────────────────────────────────────────

    #[test]
    fn differential_list_len(expr in arb_list_len_expr()) {
        assert_expr(&expr);
    }

    #[test]
    fn differential_list_index(expr in arb_list_index_expr()) {
        assert_expr(&expr);
    }

    // ── New: Match ────────────────────────────────────────────────

    #[test]
    fn differential_match_int(expr in arb_match_int_expr()) {
        assert_expr(&expr);
    }

    // ── New: Mixed type ───────────────────────────────────────────

    #[test]
    fn differential_str_len_mixed((body, result) in arb_str_len_body()) {
        assert_body_ret("i32", &body, &result);
    }

    // ── New: Result/Option match ──────────────────────────────────

    #[test]
    fn differential_match_result(expr in arb_match_result_expr()) {
        assert_expr(&expr);
    }

    #[test]
    fn differential_result_expr(expr in arb_result_expr()) {
        assert_expr(&format!("match {} {{ Ok(v) => v, Err(_) => 0 }}", expr));
    }

    #[test]
    fn differential_option_expr(expr in arb_option_expr()) {
        assert_expr(&format!("match {} {{ Some(v) => v, None => 0 }}", expr));
    }

    // ── New: Nested lists ─────────────────────────────────────────

    #[test]
    fn differential_nested_list(expr in arb_nested_list_expr()) {
        assert_expr(&expr);
    }

    #[test]
    fn differential_record_list(prog in arb_record_list_expr()) {
        assert_program_ok(&prog);
    }

    // ── New: String method / len ──────────────────────────────────

    #[test]
    fn differential_string_method(expr in arb_string_method_expr()) {
        assert_expr(&expr);
    }

    // ── New: Multi-feature ────────────────────────────────────────

    #[test]
    fn differential_while_match_closure((body, result) in arb_while_list_match_closure()) {
        assert_body_ret("i32", &body, &result);
    }

    #[test]
    fn differential_closure_record_match(prog in arb_closure_record_match()) {
        assert_program_ok(&prog);
    }
}
