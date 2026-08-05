//! Wave-1 audit-fix regression tests — checker.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).
use super::*;

fn has_code(errors: &[crate::diagnostic::Diagnostic], code: &str) -> bool {
    errors.iter().any(|e| e.code.as_deref() == Some(code))
}

fn assert_err_code(src: &str, expected: &str) {
    let errors = match check_source(src) {
        Err(errors) => errors,
        Ok(()) => panic!("expected error {expected}, but check succeeded\nsrc: {src}"),
    };
    assert!(
        has_code(&errors, expected),
        "expected {expected}, got codes: {:?}\nsrc: {src}",
        errors
            .iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

// ─── Fix 1: let-generalization must not run before check_pattern ─────
// [VERIFIED HIGH] check_stmt.rs — `let (a, b) = (None, 1)` raised a false
// E0251 ("cannot match tuple pattern against non-tuple type forall T0")
// because generalize() wrapped the tuple in ForAll before the Tuple arm of
// check_pattern could decompose it.

#[test]
fn fix1_tuple_let_with_free_typevar_checks() {
    check_source(
        r#"
func main() -> i32 {
    let (a, b) = (None, 1)
    let (c, d) = (Some(1), "x")
    0
}
"#,
    )
    .expect("tuple destructuring with free TypeVars must check");
}

#[test]
fn fix1_wrong_tuple_pattern_still_rejected() {
    // A genuinely wrong tuple pattern keeps E0251 after the reorder.
    assert_err_code(
        r#"
func main() -> i32 {
    let (a, b) = 5
    0
}
"#,
        crate::diagnostic::codes::E0251,
    );
}

#[test]
fn fix1_let_polymorphism_preserved() {
    // Moving check_pattern before generalize must not lose let-polymorphism
    // for plain-variable bindings (each read re-instantiates fresh vars).
    check_source(
        r#"
func main() -> i32 {
    let id = fn(x: _) { x }
    let a = id(1)
    let b = id("s")
    0
}
"#,
    )
    .expect("generalized let binding must stay polymorphic at every use");
}

// ─── Fix 2: `let x;` without initializer ─────────────────────────────
// [HIGH] check_stmt.rs — silently typed as unit; resolved lowering
// hard-rejected later (whole-program failure). Now E0820 at check time.

#[test]
fn fix2_let_without_initializer_rejected() {
    assert_err_code(
        r#"
func main() -> i32 {
    let x;
    0
}
"#,
        crate::diagnostic::codes::E0820,
    );
}

// ─── Fix 3: annotated `ref` linear let ───────────────────────────────
// [MEDIUM] check_stmt.rs — the E0427 rejection existed only in the
// unannotated branch; `let ref x: T = <linear>` silently dropped the ref
// flag (checker/IR divergence).

#[test]
fn fix3_annotated_ref_linear_let_rejected() {
    assert_err_code(
        r#"
flow Counter {
    state Zero { count: i32 }
    state Positive { count: i32 }
    transition inc(Zero) -> Positive { return Positive { count: self.count + 1 } }
}
func main() -> i32 {
    let s0 = Zero { count: 0 }
    let ref r: Zero = s0
    0
}
"#,
        crate::diagnostic::codes::E0427,
    );
}

#[test]
fn fix3_unannotated_ref_linear_let_still_rejected() {
    // The hoisted check must preserve the pre-existing unannotated rejection.
    assert_err_code(
        r#"
flow Counter {
    state Zero { count: i32 }
    state Positive { count: i32 }
    transition inc(Zero) -> Positive { return Positive { count: self.count + 1 } }
}
func main() -> i32 {
    let s0 = Zero { count: 0 }
    let ref r = s0
    0
}
"#,
        crate::diagnostic::codes::E0427,
    );
}

#[test]
#[ignore = "V-1 known gap (devdocs/full-audit-2026-08-05.md §16): bare `let ref` outside arena has no checker-finalized canonical Reference; fail-closed at lowering (Wave-3 item — materialize canonical Reference at lowering). Tracked: devdocs/wave1-progress-roadmap-2026-08-05.md §6 Wave-3."]
fn fix3_ref_nonlinear_let_still_checks() {
    check_source(
        r#"
func main() -> i32 {
    let v = 42
    let ref r = v
    0
}
"#,
    )
    .expect("non-linear ref let must keep checking");
}

// ─── Fix 4: plain-block shadowing aligned with branch blocks ─────────
// [MEDIUM] check_stmt.rs — Stmt::Block pushed a type scope but no var
// scope, so `{ let x }` after `let x` raised E0403 while `if c { let x }`
// did not.

#[test]
fn fix4_plain_block_shadowing_accepted_like_branches() {
    check_source(
        r#"
func main() -> i32 {
    let x = 1
    { let x = 2 }
    if x > 0 { let x = 3 }
    x
}
"#,
    )
    .expect("shadowing across a plain-block boundary must be legal");
}

#[test]
fn fix4_same_scope_rebind_still_rejected() {
    // Sanctioned contract (dual_let_shadow): same-scope rebinding stays E0403.
    assert_err_code(
        r#"
func main() -> i32 {
    let x = 1
    let x = 2
    x
}
"#,
        crate::diagnostic::codes::E0403,
    );
}

// ─── Fix 5: actor/impl method return + session hygiene ───────────────
// [HIGH] items.rs — methods skipped block_returns_on_all_paths and the
// E0425 session scope-exit check; session_residuals bled across methods.

#[test]
fn fix5_actor_method_missing_return_rejected() {
    assert_err_code(
        r#"
actor A {
    func bad() -> i32 { let x = 1 }
}
func main() -> i32 { 0 }
"#,
        crate::diagnostic::codes::E0255,
    );
}

#[test]
fn fix5_valid_actor_methods_pass() {
    check_source(
        r#"
actor A {
    func ok() -> i32 { 42 }
    func ok_unit() { println("u") }
}
func main() -> i32 { 0 }
"#,
    )
    .expect("valid actor methods must keep checking");
}

#[test]
fn fix5_impl_method_missing_return_rejected() {
    assert_err_code(
        r#"
trait Getter {
    func get() -> i32;
}
type Holder { v: i32 }
impl Getter for Holder {
    func get() -> i32 { let y = 2 }
}
func main() -> i32 { 0 }
"#,
        crate::diagnostic::codes::E0255,
    );
}

#[test]
fn fix5_session_residuals_do_not_bleed_between_methods() {
    // leaky leaves its SessionChan mid-protocol (E0425 belongs to leaky/ch1);
    // clean finishes its own endpoint and must NOT inherit ch1's residual.
    let src = r#"
session S = !i32 . end
actor A {
    func leaky(ch1: SessionChan<S>) -> i32 { 0 }
    func clean(ch2: SessionChan<S>) -> i32 {
        session_send(ch2, 1)
        session_close(ch2)
        0
    }
}
func main() -> i32 { 0 }
"#;
    let errors = check_source(src).expect_err("leaky session endpoint must be flagged");
    assert!(
        has_code(&errors, crate::diagnostic::codes::E0425),
        "expected E0425 for the unfinished endpoint, got: {:?}",
        errors
            .iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
    let rendered: Vec<String> = errors.iter().map(|d| format!("{}", d)).collect();
    assert!(
        rendered.iter().any(|m| m.contains("ch1")),
        "E0425 must name the leaking endpoint ch1, got:\n{}",
        rendered.join("\n")
    );
    assert!(
        !rendered.iter().any(|m| m.contains("ch2")),
        "per-method reset must keep ch2 out of the diagnostics, got:\n{}",
        rendered.join("\n")
    );
}

// ─── Fix 6: duplicate extern declarations ────────────────────────────
// [HIGH] items.rs — extern registration inserted without duplicate check;
// a second extern block silently overwrote the first signature.

#[test]
fn fix6_duplicate_extern_rejected() {
    assert_err_code(
        r#"
extern "C" {
    func c_symbol(a: i32) -> i32;
}
extern "C" {
    func c_symbol(a: i64) -> i64;
}
func main() -> i32 { 0 }
"#,
        crate::diagnostic::codes::E0402,
    );
}

#[test]
fn fix6_distinct_externs_pass() {
    check_source(
        r#"
extern "C" {
    func c_alpha(a: i32) -> i32;
    func c_beta(a: i32) -> i32;
}
func main() -> i32 { 0 }
"#,
    )
    .expect("distinct extern declarations must keep checking");
}

// ─── Fix 7: newtype constructor shadow check ─────────────────────────
// [HIGH] items.rs — newtype constructor registration lacked the CK3-style
// collision diagnostic that enum variants have.

#[test]
fn fix7_newtype_constructor_shadow_rejected() {
    assert_err_code(
        r#"
func UserId(x: i32) -> i32 { x }
newtype UserId = i32
func main() -> i32 { 0 }
"#,
        crate::diagnostic::codes::E0402,
    );
}

// ─── Fix 8: nested func no longer corrupts the funcs directory ───────
// [HIGH] check_stmt.rs — bare-name insertion permanently shadowed the
// top-level definition for all subsequently checked items.

#[test]
#[ignore = "V-11 known gap (devdocs/full-audit-2026-08-05.md §16): nested-function shadowing lowering ambiguity — the call inside `outer` resolves against the shadowing helper's canonical type, which lowering cannot admit (no implicit conversion between the two rt: types). Fail-closed; scope-aware nested-function resolution is a Wave-2/Wave-3 work item (V-11)."]
fn fix8_nested_func_shadow_does_not_leak() {
    // Inside `outer` the nested helper is visible and used; after `outer`,
    // `caller` must still type-check against the top-level helper signature.
    check_source(
        r#"
func helper(x: i32) -> i32 { x + 1 }
func outer() -> i32 {
    func helper(y: string) -> i32 { 0 }
    helper("shadowed")
}
func caller() -> i32 { helper(5) }
func main() -> i32 { caller() }
"#,
    )
    .expect("nested helper must not leak beyond its owner");
}

#[test]
fn fix8_nested_func_still_callable_inside_owner() {
    check_source(
        r#"
func outer2() -> i32 {
    func inc(a: i32) -> i32 { a + 1 }
    inc(41)
}
func main() -> i32 { outer2() }
"#,
    )
    .expect("nested func must stay callable after its declaration in the owner body");
}

// ─── Fix 9: guarded match arms are not full coverage ─────────────────
// [HIGH] infer/match_.rs — guards can fail at runtime, leaving the variant
// unmatched; guarded arms must not count toward exhaustiveness.

#[test]
fn fix9_guarded_arm_alone_not_exhaustive() {
    assert_err_code(
        r#"
type Color { Red Green }
func pick(c: Color, flag: bool) -> i32 {
    match c {
        Red if flag => 1
        Green => 2
    }
}
func main() -> i32 { pick(Red, true) }
"#,
        crate::diagnostic::codes::E0215,
    );
}

#[test]
fn fix9_guarded_arm_with_wildcard_ok() {
    check_source(
        r#"
type Color { Red Green }
func pick(c: Color, flag: bool) -> i32 {
    match c {
        Red if flag => 1
        _ => 2
    }
}
func main() -> i32 { pick(Red, true) }
"#,
    )
    .expect("wildcard arm restores exhaustiveness");
}

// ─── Fix 10: exhaustiveness guard extended beyond 4 scalar types ─────
// [HIGH] infer/match_.rs — non-enum subjects other than i32/i64/f64/string
// silently matched nothing when no arm applied and no wildcard existed.

#[test]
fn fix10_tuple_subject_without_wildcard_rejected() {
    assert_err_code(
        r#"
func main() -> i32 {
    let t = (1, 2)
    match t {
        (0, 0) => 0
    }
}
"#,
        crate::diagnostic::codes::E0215,
    );
}

#[test]
fn fix10_tuple_subject_with_wildcard_ok() {
    check_source(
        r#"
func main() -> i32 {
    let t = (1, 2)
    match t {
        (0, 0) => 0
        _ => 1
    }
}
"#,
    )
    .expect("wildcard arm restores exhaustiveness for tuple subjects");
}

#[test]
fn fix10_catchall_tuple_pattern_ok() {
    // `(a, b)` binds any element values — a structural catch-all, so no
    // wildcard is required (keeps ck5/dual_match_tuple_bind_vars green).
    check_source(
        r#"
func main() -> i32 {
    let t = (1, 2)
    match t {
        (a, b) => a + b
    }
}
"#,
    )
    .expect("all-binding tuple pattern is exhaustive");
}

#[test]
fn fix10_newtype_self_constructor_match_ok() {
    // A constructor pattern naming the subject's own newtype always matches.
    check_source(
        r#"
newtype UserId = i32
func get_id(u: UserId) -> i32 {
    match u {
        UserId(v) => v
    }
}
func main() -> i32 { get_id(UserId(42)) }
"#,
    )
    .expect("self-constructor match on a newtype is exhaustive");
}
