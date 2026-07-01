//! # Dual-Backend Differential Fuzzer
//!
//! Generates random Mimi programs and runs them through both the
//! interpreter and LLVM codegen, asserting identical output.
//!
//! Strategy: generators produce **function body** snippets (a block of
//! statements ending with a value expression). The test wraps the body
//! differently for each backend:
//! - Interpreter: `func main() -> i32 { BODY }` → returns last-expr value
//! - Codegen:     `func main() -> i32 { BODY; println(RESULT); 0 }`
//!   where RESULT is the body's last expression.
//!   Then compares interp Value Display with codegen stdout.
//!
//! Run: `PROPTEST_CASES=1000 cargo test fuzz::target_differential -- --nocapture --include-ignored`

use crate::tests::{compile_and_run, run_source};
use proptest::prelude::*;

/// Wrap a body + result for the interpreter (returns the result as value).
fn interp_src(body: &str, result: &str) -> String {
    format!("func main() -> i32 {{\n{}\n{}\n}}", body, result)
}

/// Wrap body + result for the codegen (prints result, returns 0).
fn codegen_src(body: &str, result: &str) -> String {
    format!(
        "func main() -> i32 {{\n{}\nprintln({});\n0\n}}",
        body, result
    )
}

/// Run a body + result expression through both backends and assert equal.
fn assert_body(body: &str, result: &str) {
    let v = run_source(&interp_src(body, result));
    let interp_str = format!("{}", v);
    let cg_stdout =
        compile_and_run(&codegen_src(body, result)).expect("codegen should compile and run");
    let cg_str = cg_stdout.trim();
    assert_eq!(
        interp_str, cg_str,
        "differential mismatch\nbody:\n{}\nresult: {}\ninterp: {}\ncodegen: {}",
        body, result, interp_str, cg_str
    );
}

/// For simple expressions: body is empty, result is the expression itself.
fn assert_expr(expr: &str) {
    assert_body("", expr);
}

// ─── Expression generators ──────────────────────────────────────

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
        // Can't use assert_body for programs with extra top-level functions.
        // Use assert_program_ok instead.
        (body.clone(), n.to_string())
    })
}

/// Full program check: both backends succeed (fallback for tests that can't
/// easily compare outputs cross-backend due to top-level function definitions).
fn assert_program_ok(src: &str) {
    let _ = run_source(src);
    compile_and_run(src).expect("codegen should compile and run");
}

proptest! {
    #[test]
    #[ignore = "requires cc linker toolchain"]
    fn differential_int(expr in arb_int_expr()) {
        assert_expr(&expr);
    }

    #[test]
    #[ignore = "requires cc linker toolchain"]
    fn differential_if(expr in arb_if_expr()) {
        assert_expr(&expr);
    }

    #[test]
    #[ignore = "requires cc linker toolchain"]
    fn differential_let((body, result) in arb_let_body()) {
        assert_body(&body, &result);
    }

    #[test]
    #[ignore = "requires cc linker toolchain"]
    fn differential_closure((body, result) in arb_closure_body()) {
        assert_body(&body, &result);
    }

    #[test]
    #[ignore = "requires cc linker toolchain"]
    fn differential_while((body, result) in arb_while_body()) {
        assert_body(&body, &result);
    }

    /// For recursive tests, use assert_program_ok since the function definition
    /// is top-level (not inside main's body).
    #[test]
    #[ignore = "requires cc linker toolchain"]
    fn differential_recursive(src in arb_recursive_body().prop_map(|(s, _)| s)) {
        assert_program_ok(&src);
    }
}
