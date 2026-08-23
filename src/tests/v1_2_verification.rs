#![allow(dead_code)]
#[allow(unused_imports)]
use super::*;

// ── T202: --verify-contracts tests ──

#[test]
fn verify_contracts_requires_violation() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    requires: a > 0
    a + b
}

func main() -> i32 {
    add(-1, 2)
}
"#;
    // Without verify_contracts, requires is ignored
    let result = bytecode_run_with_contracts(src, false);
    assert!(
        result.is_ok(),
        "without verify_contracts, requires should be ignored"
    );

    // With verify_contracts, requires is enforced
    let result = bytecode_run_with_contracts(src, true);
    assert!(
        result.is_err(),
        "with verify_contracts, requires violation should error"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("requires condition failed"),
        "Expected requires error, got: {}",
        err
    );
}

#[test]
fn verify_contracts_ensures_violation() {
    let src = r#"
func double(x: i32) -> i32 {
    ensures: result == x * 2
    x * 3
}

func main() -> i32 {
    double(5)
}
"#;
    // Without verify_contracts, ensures is ignored
    let result = bytecode_run_with_contracts(src, false);
    assert!(
        result.is_ok(),
        "without verify_contracts, ensures should be ignored"
    );

    // With verify_contracts, ensures is enforced
    let result = bytecode_run_with_contracts(src, true);
    assert!(
        result.is_err(),
        "with verify_contracts, ensures violation should error"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("ensures condition failed"),
        "Expected ensures error, got: {}",
        err
    );
}

#[test]
fn verify_contracts_passes() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    requires: a > 0
    ensures: result == a + b
    a + b
}

func main() -> i32 {
    add(1, 2)
}
"#;
    // With verify_contracts, valid contracts should pass
    let result = bytecode_run_with_contracts(src, true);
    assert!(
        result.is_ok(),
        "valid contracts should pass with verify_contracts"
    );
    assert_eq!(result.unwrap(), crate::interp::Value::Int(3));
}

// ============================================================
// T601: Z3 形式化验证
// ============================================================

fn z3_available() -> bool {
    crate::verifier::is_z3_available()
}

fn verify_source(source: &str) -> Vec<crate::verifier::VerificationResult> {
    crate::verifier::verify_source(source)
        .expect("src/tests/v1_2_verification.rs:101 unwrap failed")
}

fn assert_verified(source: &str) {
    if !z3_available() {
        eprintln!("    └─ skipped (Z3 not available)");
        return;
    }
    let results = verify_source(source);
    for r in &results {
        assert_eq!(
            r.status,
            crate::verifier::VerifStatus::Verified,
            "{}: {}",
            r.func_name,
            r.message
        );
    }
}

fn assert_failed(source: &str) {
    if !z3_available() {
        eprintln!("    └─ skipped (Z3 not available)");
        return;
    }
    let results = verify_source(source);
    assert!(
        results
            .iter()
            .any(|r| r.status == crate::verifier::VerifStatus::Failed),
        "expected at least one Failed result, got: {:?}",
        results
            .iter()
            .map(|r| (&r.func_name, &r.status))
            .collect::<Vec<_>>()
    );
}

fn assert_unknown(source: &str) {
    let results = verify_source(source);
    assert!(
        results.iter().all(|r| r.status.is_inconclusive()),
        "expected all inconclusive results, got: {:?}",
        results
            .iter()
            .map(|r| (&r.func_name, &r.status))
            .collect::<Vec<_>>()
    );
}

#[test]
fn verify_no_contracts() {
    let src = r#"
func add(x: i32, y: i32) -> i32 {
    x + y
}
"#;
    assert_unknown(src);
}

#[test]
fn verify_simple_requires() {
    let src = r#"
func abs(x: i32) -> i32 {
    requires: x > 0
    if x > 0 {
        x
    } else {
        0 - x
    }
}
"#;
    let results = verify_source(src);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, crate::verifier::VerifStatus::Verified);
}

#[test]
fn verify_requires_with_literal() {
    let src = r#"
func double(x: i32) -> i32 {
    requires: x == 5
    x + x
}
"#;
    let results = verify_source(src);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, crate::verifier::VerifStatus::Verified);
}

#[test]
fn verify_ensures_simple() {
    let src = r#"
func positive(x: i32) -> i32 {
    requires: x > 0
    ensures: x > 0
    x
}
"#;
    let results = verify_source(src);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, crate::verifier::VerifStatus::Verified);
}

#[test]
fn verify_ensures_fails() {
    let src = r#"
func bad(x: i32) -> i32 {
    requires: x == 1
    ensures: x == 2
    x
}
"#;
    assert_failed(src);
}

#[test]
fn verify_requires_and_ensures() {
    let src = r#"
func identity(x: i32) -> i32 {
    requires: x >= 0
    ensures: x >= 0
    x
}
"#;
    let results = verify_source(src);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, crate::verifier::VerifStatus::Verified);
}

#[test]
fn verify_math_constraint() {
    let src = r#"
func mul(x: i32, y: i32) -> i32 {
    requires: x == 3
    requires: y == 4
    math: { x * y == 12 }
    x * y
}
"#;
    let results = verify_source(src);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, crate::verifier::VerifStatus::Verified);
}

#[test]
fn verify_comparison_ops() {
    let src = r#"
func min(x: i32, y: i32) -> i32 {
    requires: x == 5
    requires: y == 10
    ensures: x <= 10
    if x < y { x } else { y }
}
"#;
    let results = verify_source(src);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, crate::verifier::VerifStatus::Verified);
}

#[test]
fn verify_not_operator() {
    let src = r#"
func is_positive(x: i32) -> i32 {
    requires: not(x == 0)
    ensures: not(x == 0)
    x
}
"#;
    let results = verify_source(src);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, crate::verifier::VerifStatus::Verified);
}

#[test]
fn verify_and_operator() {
    let src = r#"
func bounded(x: i32) -> i32 {
    requires: x > 0 and x < 100
    ensures: x > 0
    x
}
"#;
    let results = verify_source(src);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, crate::verifier::VerifStatus::Verified);
}

#[test]
fn verify_or_operator() {
    let src = r#"
func either(x: i32) -> i32 {
    requires: x == 1 or x == 2
    ensures: x >= 1
    x
}
"#;
    let results = verify_source(src);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, crate::verifier::VerifStatus::Verified);
}

#[test]
fn verify_ne_operator() {
    let src = r#"
func nonzero(x: i32) -> i32 {
    requires: x != 0
    ensures: x != 0
    x
}
"#;
    let results = verify_source(src);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, crate::verifier::VerifStatus::Verified);
}

#[test]
fn verify_ge_operator() {
    let src = r#"
func non_negative(x: i32) -> i32 {
    requires: x >= 0
    ensures: x >= 0
    x
}
"#;
    let results = verify_source(src);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, crate::verifier::VerifStatus::Verified);
}

#[test]
fn verify_multiple_functions() {
    let src = r#"
func f1(x: i32) -> i32 {
    requires: x == 1
    ensures: x == 1
    x
}

func f2(x: i32) -> i32 {
    requires: x == 2
    ensures: x == 2
    x
}
"#;
    let results = verify_source(src);
    assert_eq!(results.len(), 2);
    assert!(results
        .iter()
        .all(|r| r.status == crate::verifier::VerifStatus::Verified));
}

#[test]
fn verify_subtraction() {
    let src = r#"
func sub(x: i32, y: i32) -> i32 {
    requires: x == 10
    requires: y == 3
    ensures: x - y == 7
    x - y
}
"#;
    let results = verify_source(src);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, crate::verifier::VerifStatus::Verified);
}

#[test]
fn verify_division() {
    let src = r#"
func div(x: i32, y: i32) -> i32 {
    requires: x == 12
    requires: y == 4
    ensures: x / y == 3
    x / y
}
"#;
    let results = verify_source(src);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, crate::verifier::VerifStatus::Verified);
}

#[test]
fn verify_modulo() {
    let src = r#"
func rem(x: i32, y: i32) -> i32 {
    requires: x == 10
    requires: y == 3
    ensures: x % y == 1
    x % y
}
"#;
    let results = verify_source(src);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, crate::verifier::VerifStatus::Verified);
}

#[test]
fn verify_negation() {
    let src = r#"
func negate(x: i32) -> i32 {
    requires: x == 5
    ensures: x == 5
    0 - x
}
"#;
    let results = verify_source(src);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, crate::verifier::VerifStatus::Verified);
}

#[test]
fn verify_unsatisfiable_requires() {
    // §11-#49 (closed 0.36.81): the old case wrote contracts in a `mms{}`
    // block, which the compiler no longer reads (§10 — Mimi never extracts
    // contracts from mms{}); verification saw no requires and returned
    // InfrastructureError. Rewritten with top-level `requires:` —
    // contradictory preconditions must make the contract Failed, not Verified.
    let src = r#"
func impossible(x: i32) -> i32 {
    requires: x > 0
    requires: x < 0
    x
}
"#;
    assert_failed(src);
}

#[test]
fn verify_result_count() {
    let src = r#"
func f1(x: i32) -> i32 {
    x
}

func f2(x: i32) -> i32 {
    x
}

func f3(x: i32) -> i32 {
    x
}
"#;
    let results = verify_source(src);
    assert_eq!(results.len(), 3);
}

#[test]
fn verify_le_operator() {
    let src = r#"
func capped(x: i32) -> i32 {
    requires: x <= 100
    ensures: x <= 100
    x
}
"#;
    let results = verify_source(src);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, crate::verifier::VerifStatus::Verified);
}

#[test]
fn verify_gt_operator() {
    let src = r#"
func positive(x: i32) -> i32 {
    requires: x > 0
    ensures: x > 0
    x
}
"#;
    let results = verify_source(src);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, crate::verifier::VerifStatus::Verified);
}

#[test]
fn verify_ensures_fails_counterexample() {
    let src = r#"
func wrong(x: i32) -> i32 {
    requires: x == 10
    ensures: x == 20
    x
}
"#;
    assert_failed(src);
}

#[test]
fn verify_audit37_field_access_no_longer_aliases_same_named_param() {
    // #37 (full-audit-2026-08-05 §11): field accesses were flattened with
    // underscore joins (`p.x` → Z3 name "p_x"), aliasing a parameter literally
    // named `p_x`. Cross-object proof: `ensures: p.a == p_a` became the
    // tautology `p_a == p_a` and verified vacuously. Field names now use a
    // `.`; tuple indices `[i]` — both outside the identifier charset, so the
    // generated Z3 name can never collide with a user parameter.
    let src = r#"
type Pair {
    a: i32,
    b: i32,
}

func f(p: Pair, p_a: i32) -> i32 {
    requires: p.a == 5
    ensures: p.a == p_a
    p.a
}
"#;
    assert_failed(src);
}

#[test]
fn verify_audit37_call_key_underscore_join_no_cross_call_alias() {
    // §11-#37 (full-audit-2026-08-05 §11, residual): `call_var_key` joined
    // parts with `_`, so `g(a_b, c)` and `g(a, b_c)` produced the identical
    // Z3 key `call_g_a_b_c` — two distinct call results aliased into one
    // variable. The proven callee ensures of the first call then became an
    // axiom for the second, proving `result == 5` from the unrelated fact
    // `a_b == 5` (cross-call fake Proven). Parts now join with `#`, outside
    // the identifier charset.
    let src = r#"
func g(x: i32, y: i32) -> i32 {
    ensures: result == x
    x
}

func caller(a_b: i32, a: i32, b_c: i32, c: i32) -> i32 {
    requires: a_b == 5
    ensures: result == 5
    let r1 = g(a_b, c)
    let r2 = g(a, b_c)
    r2
}
"#;
    assert_failed(src);
}

#[test]
fn verify_audit37_string_len_derived_const_no_param_alias() {
    // §11-#37 (residual): the string length constant of parameter `s` was
    // named `s_len`, aliasing a user parameter literally named `s_len`.
    // `requires: len(s) == 5` then constrained the unrelated i32 param and
    // proved `ensures: s_len == 5` vacuously. Derived constants now use a
    // dot separator (`s.len`), outside the identifier charset.
    let src = r#"
func f(s: string, s_len: i32) -> i32 {
    requires: len(s) == 5
    ensures: s_len == 5
    s_len
}
"#;
    assert_failed(src);
}

#[test]
fn verify_audit40_tail_if_let_without_value_is_not_fake_proven() {
    // #40 (full-audit-2026-08-05 §11): a tail `if let` whose branches yield
    // no extractable value used to fall through to `result = 0` in func.rs,
    // so `ensures: result == 0` proved against a fabricated constraint even
    // though the program really returns the tail expression `7` — a fake
    // Proven. The reverse scan must keep looking past the value-less if:
    // the body returns 7, so the postcondition must NOT hold.
    let src = r#"
func f(opt: Option<i32>) -> i32 {
    ensures: result == 0
    if let Some(x) = opt {
        let y = x
    }
    7
}
"#;
    assert_failed(src);
}
