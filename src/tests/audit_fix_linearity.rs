//! Wave-1 audit-fix regression tests — linearity.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).
use super::*;


/// Collect the diagnostic codes emitted by `check_source` (fails on Ok).
fn rejection_codes(src: &str) -> Vec<String> {
    check_source(src)
        .expect_err("program must be rejected")
        .into_iter()
        .filter_map(|d| d.code)
        .collect()
}

// ─── Fix 1: wildcard/length-mismatch destructuring strands linear sources ───
// resource_lower.rs Bind arm: positional pairing (sources.get(index)) only
// works when every linear source has a linear binding. Otherwise the
// untouched source keeps its Available fact → use-after-move / silent leak.

#[test]
fn fix1_verified_wildcard_destructure_exploit_now_rejected() {
    // The exact verified exploit: `let (_, y) = (a, b)` emitted a Move only
    // for the FIRST source (a → y) while b stayed Available, so the
    // use-after-move `drop(b); drop(y)` checked OK. Fail-closed: E0304.
    let src = r#"
cap Token
func f(a: cap Token, b: cap Token) -> i32 {
    let (_, y) = (a, b)
    drop(b)
    drop(y)
    0
}
func main() -> i32 { 0 }
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0304),
        "wildcard destructure must reject with E0304, got: {codes:?}"
    );
}

#[test]
fn fix1_trailing_wildcard_variant_rejected() {
    let src = r#"
cap Token
func f(a: cap Token, b: cap Token) -> i32 {
    let (x, _) = (a, b)
    drop(x)
    0
}
func main() -> i32 { 0 }
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0304),
        "trailing-wildcard destructure must reject with E0304, got: {codes:?}"
    );
}

#[test]
fn fix1_nested_tuple_wildcard_mispairing_rejected() {
    // Nested patterns flatten positionally too: bindings [x, z] cannot be
    // paired with sources [a, b, c] — the wildcard strands b.
    let src = r#"
cap Token
func f(a: cap Token, b: cap Token, c: cap Token) -> i32 {
    let ((x, _), z) = ((a, b), c)
    drop(x)
    drop(z)
    0
}
func main() -> i32 { 0 }
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0304),
        "nested wildcard destructure must reject with E0304, got: {codes:?}"
    );
}

#[test]
fn fix1_split_shape_and_normal_drop_remain_legal() {
    // The sanctioned split() shape (Tuple([receiver]) with one source and
    // two bindings) and plain binding moves must keep checking.
    let src = r#"
cap FileReadCap;
cap FileWriteCap;
cap FullAccess = FileReadCap + FileWriteCap;
func main() -> i32 {
    let c = FullAccess
    let (r, w) = c.split()
    drop(r)
    drop(w)
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "split() destructure + drops must check: {:?}",
        check_source(src)
    );

    // An if-expression duplicating one place carries a single obligation;
    // the source count deduplicates by resource identity.
    let src = r#"
cap Token
func f(t: cap Token, flag: bool) -> i32 {
    let x = if flag { t } else { t }
    drop(x)
    0
}
func main() -> i32 { 0 }
"#;
    assert!(
        check_source(src).is_ok(),
        "shared-place if-expression move must check: {:?}",
        check_source(src)
    );
}

// ─── Fix 2: one-side-only join facts + MaybeConsumed return obligations ────
// dataflow.rs: a fact present on only one falling-through predecessor is a
// live obligation on that path. Available stays Available (return gate flags
// it), Consumed stays Consumed (discharged in-arm), and MaybeConsumed at a
// return terminator is a fail-closed E0256 obligation.

#[test]
fn fix2_conditional_construction_leak_now_rejected() {
    // Introduced on one branch, never consumed, falls through the join:
    // previously merged to MaybeConsumed and skipped by the return gate.
    let src = r#"
cap Token
func leak(flag: bool) -> i32 {
    if flag {
        let u = Token
    }
    0
}
func main() -> i32 { 0 }
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0256),
        "conditionally introduced cap must reject with E0256, got: {codes:?}"
    );
}

#[test]
fn fix2_conditional_construction_consumed_in_arm_still_legal() {
    // Consumed on the only path that owns the resource: no obligation.
    let src = r#"
cap Token
func ok(flag: bool) -> i32 {
    if flag {
        let u = Token
        drop(u)
    }
    0
}
func main() -> i32 { 0 }
"#;
    assert!(
        check_source(src).is_ok(),
        "in-arm introduce-then-drop must check: {:?}",
        check_source(src)
    );
}

#[test]
fn fix2_conditional_move_else_drop_rejected() {
    // The audit's regression shape: moved on one branch, dropped on the
    // other — incompatible join, fail-closed.
    let src = r#"
cap Token
func f(flag: bool, t: cap Token) -> i32 {
    if flag {
        let u = t
    } else {
        drop(t)
    }
    0
}
func main() -> i32 { 0 }
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0304),
        "conditional move vs drop must reject with E0304, got: {codes:?}"
    );
}

// ─── Fix 3: `_`-prefix exemption restricted to droppable resources ─────────

#[test]
fn fix3_underscore_cap_binding_now_rejected() {
    // `let _t = cap` is a leak, not an intentional discard.
    let src = r#"
cap Token
func f(t: cap Token) -> i32 {
    let _t = t
    0
}
func main() -> i32 { 0 }
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0256),
        "underscore-prefixed cap binding must reject with E0256, got: {codes:?}"
    );
}

#[test]
fn fix3_underscore_flow_state_binding_still_legal() {
    // Flow states are droppable — `_`-prefix auto-drop remains legal.
    let src = r#"
flow Counter {
    state Zero
    state Done
    transition finish(Zero) -> Done {
        return Done { }
    }
}
func main() -> i32 {
    let s = Zero { }
    let _d = Counter::finish(s)
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "underscore-prefixed flow-state binding must check: {:?}",
        check_source(src)
    );
}

// ─── Fix 4: `?` lowers a real error-return edge ─────────────────────────────
// resolved_lower.rs: Try forks; the error edge reaches an implicit Return so
// validate_return_resources sees live linear facts (E0256) instead of
// accepting the leak on the error path.

#[test]
fn fix4_cap_live_across_try_error_path_now_rejected() {
    let src = r#"
cap Token
func may_fail(flag: bool) -> Result<i32, i32> {
    if flag { Ok(1) } else { Err(2) }
}
func f(t: cap Token, flag: bool) -> Result<i32, i32> {
    let x = may_fail(flag)?
    drop(t)
    Ok(x)
}
func main() -> i32 { 0 }
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0256),
        "cap live on the `?` error path must reject with E0256, got: {codes:?}"
    );
}

#[test]
fn fix4_cap_consumed_before_try_still_legal() {
    // drop(t) before the fallible operation discharges the obligation on
    // both edges (E0429's concern, satisfied here).
    let src = r#"
cap Token
func may_fail(flag: bool) -> Result<i32, i32> {
    if flag { Ok(1) } else { Err(2) }
}
func g(t: cap Token, flag: bool) -> Result<i32, i32> {
    drop(t)
    let x = may_fail(flag)?
    Ok(x)
}
func main() -> i32 { 0 }
"#;
    assert!(
        check_source(src).is_ok(),
        "consume-before-`?` must check: {:?}",
        check_source(src)
    );
}

// ─── Fix 5: E0432 on local-closure calls through unresolved binders ─────────
// infer/call/simple.rs Type::Func arm: `let f = generic_sink; f(cap)` used to
// unify T := cap and the non-linear GenericParameter discarded the value.

#[test]
fn fix5_local_generic_closure_linear_arg_rejected() {
    let src = r#"
cap Token
func sink<T>(v: T) -> i32 { 1 }
func main() -> i32 {
    let c = Token
    let f = sink
    f(c)
}
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0432),
        "cap through a let-bound generic function value must reject with E0432, got: {codes:?}"
    );
}

#[test]
fn fix5_direct_generic_call_still_rejected() {
    let src = r#"
cap Token
func sink<T>(v: T) -> i32 { 1 }
func main() -> i32 {
    let c = Token
    sink(c)
}
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0432),
        "direct cap into generic call must reject with E0432, got: {codes:?}"
    );
}

#[test]
fn fix5_local_concrete_closure_linear_arg_still_legal() {
    // Concrete (non-generic) signatures keep linear tracking: passing a cap
    // through a let-bound concrete function that drops it stays legal.
    let src = r#"
cap Token
func use_and_drop(t: cap Token) -> i32 {
    drop(t)
    1
}
func main() -> i32 {
    let c = Token
    let f = use_and_drop
    f(c)
}
"#;
    assert!(
        check_source(src).is_ok(),
        "cap through a let-bound concrete function must check: {:?}",
        check_source(src)
    );
}

// ─── Fix 6: pinned linear bindings become visible to dataflow ───────────────
// resource_lower.rs: pinned bindings of linear type used to get a catalog
// entry but no action — the pinned resource escaped consumption entirely.

#[test]
fn fix6_pinned_linear_binding_consumed_still_legal() {
    let src = r#"
cap Token
func f(t: cap Token) -> i32 {
    pinned(t) |p| {
        drop(p)
    }
    0
}
func main() -> i32 { 0 }
"#;
    assert!(
        check_source(src).is_ok(),
        "pinned cap binding consumed in the body must check: {:?}",
        check_source(src)
    );
}

#[test]
fn fix6_pinned_linear_binding_unconsumed_rejected() {
    let src = r#"
cap Token
func f(t: cap Token) -> i32 {
    pinned(t) |p| {
        println(1)
    }
    0
}
func main() -> i32 { 0 }
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0256),
        "unconsumed pinned cap binding must reject with E0256, got: {codes:?}"
    );
}
