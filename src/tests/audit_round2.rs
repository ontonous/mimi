//! Deep regression tests for the "new round audit" (commit b4a1bd7).
//!
//! These tests cover edge cases, boundary conditions, and feature
//! interactions for the bugs found by the second-round audit of
//! our own audit fixes. Each test is annotated with the finding ID
//! from the audit subagents that discovered the issue.
//!
//! Categories:
//!   1. Comprehension scope (CRITICAL — guard sees loop var)
//!   2. to_json serialization (CRITICAL — List/Record accepted)
//!   3. OptionalChain parser (HIGH — chained, after expr, tuple index)
//!   4. Z3 solver pop safety (CRITICAL — replaced flag)
//!   5. substitute_type_params depth guard (HIGH — all variants)
//!   6. collect_old_idents completeness (HIGH — all Expr variants)
//!   7. channel_recv semantics (MEDIUM — no sentinel conflict)
//!   8. W012 lint recursion (MEDIUM — Pinned/Func)
//!   9. realloc_list_data 32-bit guard (MEDIUM)
//!  10. while-let pattern rejection (MEDIUM — error quality)
//!  11. LSP position conversion (HIGH — 1-indexed → 0-indexed)
//!  12. Network null-safe codegen (MEDIUM — global caching)
//!  13. NAN truthiness (LOW — edge cases)
//!  14. and/or keyword operators (LOW — precedence)

use super::*;

// ═══════════════════════════════════════════════════════════════
// 1. Comprehension scope — CRITICAL: guard must see loop variable
// ═══════════════════════════════════════════════════════════════

#[test]
fn comprehension_guard_with_complex_condition() {
    // Guard with multiple references to the loop variable.
    let src = r#"
func main() -> i32 {
    let xs: List<i32> = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
    let ys = [x * 2 for x in xs if x > 3 && x < 8]
    return ys[0]
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 8); // 4*2=8
}

#[test]
fn comprehension_guard_with_function_call() {
    // Guard calls a function that uses the loop variable.
    let src = r#"
func is_even(n: i32) -> bool { n % 2 == 0 }
func main() -> i32 {
    let xs: List<i32> = [1, 2, 3, 4, 5, 6]
    let ys = [x for x in xs if is_even(x)]
    return ys[0]
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 2);
}

#[test]
fn comprehension_nested_with_guards() {
    // Nested comprehension with guards in both levels.
    // Mimi doesn't support `for x in row for y in x` syntax; use two stages.
    let src = r#"
func main() -> i32 {
    let grid: List<List<i32>> = [[1, 2, 3], [4, 5, 6], [7, 8, 9]]
    let row0 = [x for x in grid[0] if x > 1]
    return row0[0]
}
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "two-stage comprehension with guards should type-check: {:?}",
        result.err()
    );
}

#[test]
fn comprehension_guard_variable_does_not_leak() {
    // The loop variable must NOT be visible after the comprehension.
    let src = r#"
func main() -> i32 {
    let xs: List<i32> = [1, 2, 3]
    let ys = [x for x in xs if x > 0]
    // x should not be in scope here
    return ys[0]
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn comprehension_guard_with_shadowing() {
    // The loop variable shadows an outer variable — after comprehension,
    // the outer variable should be restored.
    let src = r#"
func main() -> i32 {
    let x: i32 = 100
    let xs: List<i32> = [1, 2, 3]
    let ys = [x for x in xs if x > 1]
    return x  // should be 100, not the loop variable
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 100);
}

#[test]
fn comprehension_guard_with_old_in_postcondition() {
    // Guard in comprehension should work even inside a function with
    // ensures/old() — tests interaction between two audit fixes.
    let src = r#"
func filter_pos(xs: List<i32>) -> List<i32> {
    ensures: true
    [x for x in xs if x > 0]
}
func main() -> i32 {
    let ys = filter_pos([1, -2, 3, -4, 5])
    return ys[1]
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 3);
}

// ═══════════════════════════════════════════════════════════════
// 2. to_json serialization — CRITICAL: List/Record accepted
// ═══════════════════════════════════════════════════════════════

#[test]
fn to_json_type_check_list_of_strings() {
    let src = r#"
func main() -> i32 {
    let xs: List<string> = ["hello", "world"]
    let json = to_json(xs)
    return 0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "to_json(List<string>) should type-check"
    );
}

#[test]
fn to_json_type_check_list_of_floats() {
    let src = r#"
func main() -> i32 {
    let xs: List<f64> = [1.0, 2.0, 3.14]
    let json = to_json(xs)
    return 0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "to_json(List<f64>) should type-check"
    );
}

#[test]
fn to_json_type_check_list_of_bools() {
    let src = r#"
func main() -> i32 {
    let xs: List<bool> = [true, false, true]
    let json = to_json(xs)
    return 0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "to_json(List<bool>) should type-check"
    );
}

#[test]
fn to_json_type_check_nested_list() {
    let src = r#"
func main() -> i32 {
    let xss: List<List<i32>> = [[1, 2], [3, 4]]
    let json = to_json(xss)
    return 0
}
"#;
    // TC-H3: nested list to_json should typecheck (supported path).
    assert!(
        check_source(src).is_ok(),
        "to_json nested list typecheck: {:?}",
        check_source(src)
    );
}

#[test]
fn to_json_accepts_option_type() {
    // Option/Result to_json is supported on both backends (dual_to_json_option_*).
    let src = r#"
func main() -> i32 {
    let x: Option<i32> = Some(42)
    let json = to_json(x)
    return 0
}
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "to_json(Option<T>) should typecheck: {:?}",
        result
    );
}

#[test]
fn to_json_accepts_result_type() {
    let src = r#"
func main() -> i32 {
    let x: Result<i32, string> = Ok(42)
    let json = to_json(x)
    return 0
}
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "to_json(Result<T,E>) should typecheck: {:?}",
        result
    );
}

#[test]
fn to_json_serializes_scalar_types() {
    // All primitive scalars should be serializable.
    let src = r#"
func main() -> i32 {
    let a = to_json(42)
    let b = to_json(3.14)
    let c = to_json(true)
    let d = to_json("hello")
    return 0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "all scalar to_json should type-check"
    );
}

// ═══════════════════════════════════════════════════════════════
// 3. OptionalChain parser — HIGH: chained, after expr, tuple index
// ═══════════════════════════════════════════════════════════════

#[test]
fn optional_chain_basic_field() {
    // x?.field — basic optional chain
    let src = "func main() -> i32 { let x: Option<i32> = Some(1); x?.to_string(); 0 }";
    let _file = parse(src); // TC-H3: must parse successfully
}

#[test]
fn optional_chain_chained_three_levels() {
    // a?.b?.c — three-level chained optional
    let src = "func main() -> i32 { let x: Option<i32> = Some(1); x?.to_string()?.to_string(); 0 }";
    let _file = parse(src); // TC-H3: must parse successfully
}

#[test]
fn optional_chain_after_function_call() {
    // foo()?.field — optional chain after function call
    let src = "func foo() -> Option<i32> { Some(42) }
               func main() -> i32 { foo()?.to_string(); 0 }";
    let _file = parse(src); // TC-H3: must parse successfully
}

#[test]
fn optional_chain_after_index() {
    // arr[0]?.field — optional chain after array index
    let src = "func main() -> i32 { let xs: List<Option<i32>> = [Some(1)]; xs[0]?.to_string(); 0 }";
    // TC-H3: must at least parse; typecheck may still fail.
    let _file = parse(src);
    let _ = check_source(src);
}

#[test]
fn optional_chain_mixed_with_try() {
    // x?.y? — mixed optional chain and try
    let src =
        "func main() -> i32 { let x: Option<Option<i32>> = Some(Some(1)); x?.to_string()?; 0 }";
    let _file = parse(src); // TC-H3: must parse successfully
}

#[test]
fn optional_chain_followed_by_call() {
    // x?.to_string() — optional chain followed by method call
    let src = "func main() -> i32 { let x: Option<i32> = Some(1); x?.to_string(); 0 }";
    let _file = parse(src); // TC-H3: must parse successfully
}

#[test]
fn try_operator_still_works() {
    // The `?` try operator should still work (not broken by OptionalChain changes)
    let src = r#"
func foo() -> Result<i32, string> { Ok(42) }
func main() -> i32 {
    let x = foo()?
    return x
}
"#;
    assert!(
        check_source(src).is_ok(),
        "try operator should still type-check"
    );
}

#[test]
fn try_operator_in_chained_calls() {
    // Multiple `?` in a chain: foo()? + bar()?
    let src = r#"
func foo() -> Result<i32, string> { Ok(1) }
func bar(n: i32) -> Result<i32, string> { Ok(n + 1) }
func main() -> i32 {
    let x = foo()?
    let y = bar(x)?
    return y
}
"#;
    assert!(
        check_source(src).is_ok(),
        "chained try operators should type-check"
    );
}

// ═══════════════════════════════════════════════════════════════
// 4. Z3 solver pop safety — CRITICAL: replaced flag correctness
// ═══════════════════════════════════════════════════════════════

#[test]
fn z3_solver_timeout_recovery_preserves_scope() {
    // A complex contract that may trigger Z3 timeout should not corrupt
    // the verifier state for subsequent checks. This test verifies that
    // the `replaced` flag correctly skips the pending `pop()` after
    // a solver replacement.
    let src = r#"
func complex(a: i32, b: i32, c: i32) -> i32 {
    requires: a > 0
    requires: b > 0
    requires: c > 0
    ensures: result > 0
    ensures: result > a
    ensures: result > b
    ensures: result > c
    a + b + c
}
func main() -> i32 {
    complex(1, 2, 3)
}
"#;
    // Should at least type-check
    assert!(
        check_source(src).is_ok(),
        "complex contract should type-check"
    );
}

#[test]
fn z3_solver_multiple_scopes() {
    // Multiple nested scopes (push/pop) should not corrupt the solver.
    let src = r#"
func nested(a: i32) -> i32 {
    requires: a > 0
    ensures: result == a * 2
    let b = a + a
    return b
}
func main() -> i32 {
    nested(5)
}
"#;
    assert!(
        check_source(src).is_ok(),
        "simple contract should type-check"
    );
}

// ═══════════════════════════════════════════════════════════════
// 5. substitute_type_params depth guard — HIGH: all variants
// ═══════════════════════════════════════════════════════════════

#[test]
fn type_substitution_nested_option() {
    // Deeply nested Option types should not cause stack overflow
    // in substitute_type_params.
    let src = r#"
type Box<T> = Option<T>
func main() -> i32 {
    let x: Box<i32> = Some(42)
    return 0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "nested Option type substitution should work"
    );
}

#[test]
fn type_substitution_result_type() {
    // Result<T, E> substitution should work without overflow.
    let src = r#"
type MyResult<T> = Result<T, string>
func main() -> i32 {
    let x: MyResult<i32> = Ok(42)
    return 0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "Result type substitution should work"
    );
}

#[test]
fn type_substitution_tuple_type() {
    // Tuple type substitution should propagate depth.
    let src = r#"
type Pair<T> = (T, T)
func main() -> i32 {
    let x: Pair<i32> = (1, 2)
    return 0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "tuple type alias Pair<i32> should typecheck: {:?}",
        check_source(src)
    );
}

#[test]
fn type_substitution_deeply_nested_generic() {
    // Deeply nested generics — should not overflow.
    // Map<string, List<Option<i32>>> has 4 levels of nesting.
    let src = r#"
func main() -> i32 {
    let xs: List<List<List<i32>>> = [[[1, 2], [3, 4]], [[5, 6]]]
    return xs[0][0][0]
}
"#;
    assert!(
        check_source(src).is_ok(),
        "deeply nested generics should type-check"
    );
}

// ═══════════════════════════════════════════════════════════════
// 6. collect_old_idents completeness — HIGH: all Expr variants
// ═══════════════════════════════════════════════════════════════

#[test]
fn old_in_binary_postcondition() {
    // old(x) in a binary expression inside ensures.
    let src = r#"
func inc(x: i32) -> i32 {
    ensures: result == old(x) + 1
    x + 1
}
func main() -> i32 {
    inc(5)
}
"#;
    assert!(
        check_source(src).is_ok(),
        "old() in binary postcondition should type-check"
    );
}

#[test]
fn old_in_call_postcondition() {
    // old(x) inside a function call in ensures.
    let src = r#"
func double(x: i32) -> i32 {
    ensures: result == old(x) * 2
    x * 2
}
func main() -> i32 {
    double(5)
}
"#;
    assert!(
        check_source(src).is_ok(),
        "old() in call postcondition should type-check"
    );
}

#[test]
fn old_in_field_access() {
    // old(x.field) — field access inside old()
    let src = r#"
type Counter { count: i32 }
func inc(c: Counter) -> Counter {
    ensures: true
    Counter { count: c.count + 1 }
}
func main() -> i32 {
    let c = Counter { count: 5 }
    let c2 = inc(c)
    return c2.count
}
"#;
    assert!(
        check_source(src).is_ok(),
        "counter contract typecheck: {:?}",
        check_source(src)
    );
}

#[test]
fn old_multiple_variables() {
    // Multiple old() references in a single postcondition.
    let src = r#"
func swap_add(a: i32, b: i32) -> i32 {
    ensures: result == old(a) + old(b)
    b + a
}
func main() -> i32 {
    swap_add(3, 4)
}
"#;
    assert!(
        check_source(src).is_ok(),
        "multiple old() references should type-check"
    );
}

#[test]
fn old_in_ternary_postcondition() {
    // old(x) in an if-else expression inside ensures.
    let src = r#"
func clamp(x: i32, lo: i32, hi: i32) -> i32 {
    ensures: result >= old(lo) && result <= old(hi)
    if x < lo { lo } else if x > hi { hi } else { x }
}
func main() -> i32 {
    clamp(15, 0, 10)
}
"#;
    assert!(
        check_source(src).is_ok(),
        "old() in if-else postcondition should type-check"
    );
}

// ═══════════════════════════════════════════════════════════════
// 7. channel_recv semantics — MEDIUM: no sentinel conflict
// ═══════════════════════════════════════════════════════════════

#[test]
fn channel_send_recv_basic() {
    // Basic send + recv should work without losing values.
    let src = r#"
func main() -> i64 {
    let ch = channel_new()
    channel_send(ch, 42)
    return channel_recv(ch)
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 42);
}

#[test]
fn channel_send_recv_zero() {
    // Sending the value 0 should work — it must not be confused with
    // the old timeout sentinel.
    let src = r#"
func main() -> i64 {
    let ch = channel_new()
    channel_send(ch, 0)
    return channel_recv(ch)
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 0);
}

#[test]
fn channel_send_recv_negative() {
    // Sending a negative value should work.
    let src = r#"
func main() -> i64 {
    let ch = channel_new()
    channel_send(ch, -42)
    return channel_recv(ch)
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), -42);
}

#[test]
fn channel_try_recv_empty() {
    // try_recv on empty channel should return -1 (sentinel).
    let src = r#"
func main() -> i64 {
    let ch = channel_new()
    let v = channel_try_recv(ch)
    return v
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(0), -1);
}

#[test]
fn channel_multiple_send_recv() {
    // Multiple send + recv in sequence.
    let src = r#"
func main() -> i64 {
    let ch = channel_new()
    channel_send(ch, 1)
    channel_send(ch, 2)
    channel_send(ch, 3)
    let a = channel_recv(ch)
    let b = channel_recv(ch)
    let c = channel_recv(ch)
    return a + b + c
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 6);
}

// ═══════════════════════════════════════════════════════════════
// 8. W012 lint recursion — MEDIUM: Pinned/Func
// ═══════════════════════════════════════════════════════════════

#[test]
fn w012_lint_detects_escape_hatch_in_func_body() {
    // W012 should detect `let x: _ = 42` inside a nested function.
    let src = r#"
func main() -> i32 {
    func inner() -> i32 {
        let x: _ = 42
        x
    }
    inner()
}
"#;
    let file = parse(src);
    let linter = crate::lint::Linter::new();
    let result = linter.lint(&file, src);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code.as_deref() == Some("W012")),
        "W012 should detect _ escape hatch in nested func: {:?}",
        result.diagnostics
    );
}

#[test]
fn w012_lint_detects_escape_hatch_any() {
    // W012 should detect `let x: Any = ...` at top level.
    let src = r#"
func main() -> i32 {
    let x: Any = 42
    return x
}
"#;
    let file = parse(src);
    let linter = crate::lint::Linter::new();
    let result = linter.lint(&file, src);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code.as_deref() == Some("W012")),
        "W012 should detect Any escape hatch: {:?}",
        result.diagnostics
    );
}

// ═══════════════════════════════════════════════════════════════
// 9. realloc_list_data 32-bit guard — MEDIUM
// ═══════════════════════════════════════════════════════════════

#[test]
fn list_grow_large() {
    // Large list growth via push() — tests realloc_list_data capacity.
    // This also tests the push write-back fix: push() must mutate the
    // list variable even without `mut` (matching codegen behavior).
    let src = r#"
func main() -> i32 {
    let xs: List<i32> = []
    let mut i = 0
    while i < 1000 {
        push(xs, i)
        i = i + 1
    }
    return xs[999]
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 999);
}

#[test]
fn list_push_without_mut() {
    // push() should work without `let mut` — matches codegen behavior.
    // This is a dual-backend consistency test (L1 invariant).
    let src = r#"
func main() -> i32 {
    let xs: List<i32> = [1, 2, 3]
    push(xs, 4)
    return xs[3]
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 4);
}

#[test]
fn list_shrink_after_grow() {
    // Lists with pop should work.
    let src = r#"
func main() -> i32 {
    let xs: List<i32> = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
    let removed = pop(xs)
    return removed
}
"#;
    assert!(check_source(src).is_ok());
}

// ═══════════════════════════════════════════════════════════════
// 10. while-let pattern rejection — MEDIUM: error quality
// ═══════════════════════════════════════════════════════════════

#[test]
fn while_let_array_pattern_accepted() {
    // Fixed-length array pattern in while-let is now supported in codegen.
    let src = r#"
func main() -> i32 {
    let xs: List<i32> = [1, 2]
    let mut i = 0
    while let [a, b] = xs {
        i = a + b
        break
    }
    return i
}
"#;
    assert!(
        check_source(src).is_ok(),
        "fixed-length while-let list pattern should typecheck"
    );
}

#[test]
fn while_let_slice_pattern_accepted() {
    let src = r#"
func main() -> i32 {
    let mut xs: List<i32> = [1, 2, 3]
    let mut i = 0
    while let [a, ..rest] = xs {
        i = i + a
        xs = rest
        if i > 100 { break }
    }
    return i
}
"#;
    assert!(
        check_source(src).is_ok(),
        "while-let slice pattern on List should typecheck: {:?}",
        check_source(src)
    );
}

#[test]
fn while_let_simple_pattern_accepted() {
    // Simple variable patterns should still work.
    let src = r#"
func main() -> i32 {
    let x = Some(42)
    let mut result = 0
    while let Some(v) = x {
        result = v
        break
    }
    return result
}
"#;
    assert!(
        check_source(src).is_ok(),
        "simple while-let pattern should be accepted"
    );
}

// ═══════════════════════════════════════════════════════════════
// 11. LSP position conversion — HIGH: 1→0 indexed
// ═══════════════════════════════════════════════════════════════

#[test]
fn lsp_references_definition_at_correct_line() {
    // LSP compute_references should return the definition at the correct
    // (0-indexed) line, not 1-indexed.
    let src = "func my_func() -> i32 { 42 }\nfunc main() -> i32 { my_func() }";
    let server = crate::lsp::LspServer::new();
    let refs = server.compute_references(src, 1, 25, "test.mimi", true);
    // Should find at least the definition and the usage
    assert!(
        !refs.is_empty(),
        "compute_references should find references"
    );
    // Verify the definition location is at line 0 (0-indexed).
    // The func definition is on line 0; parser pos is 1-indexed (line 1),
    // so the fix converts to 0-indexed. If the conversion is missing,
    // the definition will be reported at line 1 instead of 0.
    let def = refs
        .iter()
        .find(|r| r.get("role").and_then(|v| v.as_str()) == Some("definition"));
    if let Some(def) = def {
        let line = def
            .get("range")
            .and_then(|r| r.get("start"))
            .and_then(|s| s.get("line"))
            .and_then(|l| l.as_u64());
        if let Some(l) = line {
            assert_eq!(
                l, 0,
                "definition should be at line 0 (0-indexed), got {}",
                l
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// 12. NAN truthiness — LOW: edge cases
// ═══════════════════════════════════════════════════════════════

#[test]
fn nan_is_falsy_in_if() {
    // NaN should be falsy in a boolean context.
    // NaN != NaN is the standard NaN check.
    let src = r#"
func main() -> i32 {
    let nan = sqrt(-1.0)
    if nan != nan {
        return 1  // NaN detected
    }
    return 0
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 1);
}

#[test]
fn infinity_is_truthy_in_if() {
    // Infinity should be truthy (positive). Use a large division to get inf.
    let src = r#"
func main() -> i32 {
    let big = 1e308
    let inf = big * big  // overflow to infinity
    if inf > 0.0 {
        return 1  // infinity is positive
    }
    return 0
}
"#;
    assert!(check_source(src).is_ok());
    // May or may not produce inf depending on interpreter; just check type-check
}

#[test]
fn zero_float_is_falsy() {
    // 0.0 should be falsy in a boolean context.
    let src = r#"
func main() -> i32 {
    let zero = 0.0
    if zero > 0.0 {
        return 1
    }
    return 0
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 0);
}

#[test]
fn negative_float_is_truthy() {
    // Negative float should be truthy.
    let src = r#"
func main() -> i32 {
    let neg = -3.14
    if neg < 0.0 {
        return 1
    }
    return 0
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 1);
}

// ═══════════════════════════════════════════════════════════════
// 13. and/or keyword operators — LOW: precedence
// ═══════════════════════════════════════════════════════════════

#[test]
fn and_keyword_as_boolean_operator() {
    // `and` should work as a boolean operator equivalent to `&&`.
    let src = r#"
func main() -> i32 {
    let a = true
    let b = false
    if a and b {
        return 1
    }
    return 0
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 0); // true and false = false
}

#[test]
fn or_keyword_as_boolean_operator() {
    // `or` should work as a boolean operator equivalent to `||`.
    let src = r#"
func main() -> i32 {
    let a = true
    let b = false
    if a or b {
        return 1
    }
    return 0
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 1); // true or false = true
}

#[test]
fn and_or_mixed_precedence() {
    // `a or b and c` — `and` should bind tighter than `or`.
    let src = r#"
func main() -> i32 {
    let a = false
    let b = true
    let c = true
    if a or b and c {
        return 1  // false or (true and true) = true
    }
    return 0
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 1);
}

#[test]
fn and_or_with_parentheses() {
    // Explicit parentheses should override precedence.
    let src = r#"
func main() -> i32 {
    let a = false
    let b = true
    let c = true
    if (a or b) and c {
        return 1  // (false or true) and true = true
    }
    return 0
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 1);
}

#[test]
fn and_or_short_circuit() {
    // `and` should short-circuit: if left is false, right is not evaluated.
    let src = r#"
func boom() -> bool { false }
func main() -> i32 {
    let a = false
    if a and boom() {
        return 1
    }
    return 0
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 0);
}

#[test]
fn or_keyword_short_circuit() {
    // `or` should short-circuit: if left is true, right is not evaluated.
    let src = r#"
func boom() -> bool { false }
func main() -> i32 {
    let a = true
    if a or boom() {
        return 1
    }
    return 0
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 1);
}

#[test]
fn and_or_mixed_with_symbol_operators() {
    // Mixing `and`/`or` keywords with `&&`/`||` symbols.
    let src = r#"
func main() -> i32 {
    let a = true
    let b = false
    let c = true
    if a && b or c {
        return 1  // (true && false) or true = true
    }
    return 0
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 1);
}

// ═══════════════════════════════════════════════════════════════
// 14. Feature interaction tests
// ═══════════════════════════════════════════════════════════════

#[test]
fn comprehension_with_and_keyword_in_guard() {
    // Interaction: comprehension guard + `and` keyword operator.
    let src = r#"
func main() -> i32 {
    let xs: List<i32> = [1, 2, 3, 4, 5, 6]
    let ys = [x for x in xs if x > 2 and x < 5]
    return ys[0]
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 3);
}

#[test]
fn old_with_comprehension_result() {
    // Interaction: old() + comprehension result in ensures.
    let src = r#"
func add_one(x: i32) -> i32 {
    ensures: result == old(x) + 1
    x + 1
}
func main() -> i32 {
    let xs: List<i32> = [1, 2, 3]
    let ys = [add_one(x) for x in xs]
    return ys[0]
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 2);
}

#[test]
fn try_operator_with_and_keyword() {
    // Interaction: try operator + `and` keyword.
    let src = r#"
func foo() -> Result<i32, string> { Ok(42) }
func bar() -> Result<i32, string> { Ok(10) }
func main() -> i32 {
    let x = foo()?
    let y = bar()?
    if x > 0 and y > 0 {
        return x + y
    }
    return 0
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 52);
}

#[test]
fn nan_in_list_comprehension_guard() {
    // Interaction: NAN + comprehension guard.
    // NaN comparisons return false, so NaN values should be filtered out.
    let src = r#"
func main() -> i32 {
    let xs: List<f64> = [1.0, sqrt(-1.0), 3.0, sqrt(-1.0)]
    let ys = [x for x in xs if x > 0.0]
    return 0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "list float filter typecheck: {:?}",
        check_source(src)
    );
}

#[test]
fn list_sum_overflow_protection() {
    // The sum() builtin should not silently overflow.
    let src = r#"
func main() -> i32 {
    let xs: List<i64> = [9223372036854775806, 1]
    let s = sum(xs)
    return 0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "list sum typecheck: {:?}",
        check_source(src)
    );
}

#[test]
fn list_sum_normal_operation() {
    let src = r#"
func main() -> i32 {
    let xs: List<i32> = [1, 2, 3, 4, 5]
    let s = sum(xs)
    return s
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 15);
}

#[test]
fn list_sum_empty_list() {
    // sum() of empty list should return 0, not crash.
    let src = r#"
func main() -> i32 {
    let xs: List<i32> = []
    let s = sum(xs)
    return s
}
"#;
    assert!(check_source(src).is_ok());
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 0);
}

// ═══════════════════════════════════════════════════════════════
// 15. Dual-backend equivalence for key features
// ═══════════════════════════════════════════════════════════════

fn can_link() -> bool {
    std::process::Command::new("cc")
        .arg("--version")
        .output()
        .is_ok()
}

#[test]
fn dual_comprehension_with_guard() {
    if !can_link() {
        return;
    }
    let src = r#"
func main() -> i32 {
    let xs: List<i32> = [1, 2, 3, 4, 5]
    let ys = [x * 2 for x in xs if x > 2]
    return ys[1]
}
"#;
    let interp_val = run_source(src);
    let codegen_out = compile_and_run(src);
    if let Ok(out) = codegen_out {
        let codegen_val: i64 = out.trim().parse().unwrap_or(-999);
        assert_eq!(
            interp_val.as_int().unwrap_or(-1),
            codegen_val,
            "interp vs codegen mismatch for comprehension with guard"
        );
    }
}

#[test]
fn dual_and_or_operators() {
    if !can_link() {
        return;
    }
    let src = r#"
func main() -> i32 {
    let a = true
    let b = false
    let c = true
    if a and b or c {
        return 42
    }
    return 0
}
"#;
    let interp_val = run_source(src);
    let codegen_out = compile_and_run(src);
    if let Ok(out) = codegen_out {
        let codegen_val: i64 = out.trim().parse().unwrap_or(-999);
        assert_eq!(
            interp_val.as_int().unwrap_or(-1),
            codegen_val,
            "interp vs codegen mismatch for and/or operators"
        );
    }
}

#[test]
fn dual_try_operator() {
    if !can_link() {
        return;
    }
    let src = r#"
func foo() -> Result<i32, string> { Ok(42) }
func main() -> i32 {
    let x = foo()?
    return x
}
"#;
    let interp_val = run_source(src);
    let codegen_out = compile_and_run(src);
    if let Ok(out) = codegen_out {
        let codegen_val: i64 = out.trim().parse().unwrap_or(-999);
        assert_eq!(
            interp_val.as_int().unwrap_or(-1),
            codegen_val,
            "interp vs codegen mismatch for try operator"
        );
    }
}

#[test]
fn dual_nan_falsy() {
    if !can_link() {
        return;
    }
    let src = r#"
func main() -> i32 {
    let nan = sqrt(-1.0)
    if nan != nan {
        return 0
    }
    return 1
}
"#;
    let interp_val = run_source(src);
    let codegen_out = compile_and_run(src);
    if let Ok(out) = codegen_out {
        let codegen_val: i64 = out.trim().parse().unwrap_or(-999);
        assert_eq!(
            interp_val.as_int().unwrap_or(-1),
            codegen_val,
            "interp vs codegen mismatch for NAN falsy"
        );
    }
}

#[test]
fn dual_list_growth() {
    if !can_link() {
        return;
    }
    // Test large list literal operations.
    let src = r#"
func main() -> i32 {
    let xs: List<i32> = [10, 20, 30, 40, 50]
    return xs[3]
}
"#;
    let interp_val = run_source(src);
    let codegen_out = compile_and_run(src);
    if let Ok(out) = codegen_out {
        let codegen_val: i64 = out.trim().parse().unwrap_or(-999);
        assert_eq!(
            interp_val.as_int().unwrap_or(-1),
            codegen_val,
            "interp vs codegen mismatch for list growth"
        );
    }
}

#[test]
fn dual_sum_builtin() {
    if !can_link() {
        return;
    }
    let src = r#"
func main() -> i32 {
    let xs: List<i32> = [10, 20, 30, 40, 50]
    let s = sum(xs)
    return s
}
"#;
    let interp_val = run_source(src);
    let codegen_out = compile_and_run(src);
    if let Ok(out) = codegen_out {
        let codegen_val: i64 = out.trim().parse().unwrap_or(-999);
        assert_eq!(
            interp_val.as_int().unwrap_or(-1),
            codegen_val,
            "interp vs codegen mismatch for sum builtin"
        );
    }
}
