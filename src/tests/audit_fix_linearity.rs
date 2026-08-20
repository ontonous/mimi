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

// ═══════════════════════════════════════════════════════════════════════════
// Wave-2 (full-audit-2026-08-05-0656 + wave1-review §1.3/§5.5), agent L.
// Every item carried a verified PoC under /tmp/opencode/w2/L/ against the
// Phase-0 binary before fixing; names below map to audit IDs.
// ═══════════════════════════════════════════════════════════════════════════

// ─── C-2 CRITICAL: branch expressions are XOR, not AND ─────────────────────
// If/Match consume EXACTLY ONE arm's value at runtime. Consuming a branch
// expression that carries SEVERAL distinct linear resources discharged every
// arm's obligation in the analysis while one arm's resource leaked at
// runtime. E0840 now rejects the shape at every consumption position.

#[test]
fn audit2_lin_c2_call_arg_xor_distinct_rejected() {
    let src = r#"
cap Token
func sink(t: cap Token) -> i32 { drop(t); 1 }
func f(a: cap Token, b: cap Token, flag: bool) -> i32 {
    sink(if flag { a } else { b })
}
func main() -> i32 { 0 }
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0840),
        "if-expression call argument with distinct caps must reject with E0840, got: {codes:?}"
    );
}

#[test]
fn audit2_lin_c2_match_xor_distinct_rejected() {
    let src = r#"
cap Token
func sink(t: cap Token) -> i32 { drop(t); 1 }
func f(a: cap Token, b: cap Token, v: i32) -> i32 {
    sink(match v {
        0 => a
        _ => b
    })
}
func main() -> i32 { 0 }
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0840),
        "match call argument with distinct caps must reject with E0840, got: {codes:?}"
    );
}

#[test]
fn audit2_lin_c2_return_xor_distinct_rejected() {
    let src = r#"
cap Token
func pick(a: cap Token, b: cap Token, flag: bool) -> cap Token {
    return if flag { a } else { b }
}
func main() -> i32 { 0 }
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0840),
        "return-if with distinct caps must reject with E0840, got: {codes:?}"
    );
}

#[test]
fn audit2_lin_c2_body_result_xor_distinct_rejected() {
    // The function body's result position is a consumption point too.
    let src = r#"
cap Token
func pick(a: cap Token, b: cap Token, flag: bool) -> cap Token {
    if flag { a } else { b }
}
func main() -> i32 { 0 }
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0840),
        "body-result if with distinct caps must reject with E0840, got: {codes:?}"
    );
}

#[test]
fn audit2_lin_c2_xor_same_resource_still_legal() {
    // One DISTINCT resource across the arms = one obligation, consumed once.
    let src = r#"
cap Token
func sink(t: cap Token) -> i32 { drop(t); 1 }
func f(t: cap Token, flag: bool) -> i32 {
    sink(if flag { t } else { t })
}
func g(t: cap Token, flag: bool) -> cap Token {
    if flag { t } else { t }
}
func main() -> i32 { 0 }
"#;
    assert!(
        check_source(src).is_ok(),
        "same-resource branch expression must check: {:?}",
        check_source(src)
    );
}

#[test]
fn audit2_lin_c2_tuple_and_still_legal() {
    // AND aggregates are not branch expressions: every element flows.
    let src = r#"
cap Token
func sink2(pair: (cap Token, cap Token)) -> i32 { drop(pair); 1 }
func f(a: cap Token, b: cap Token) -> i32 {
    sink2((a, b))
}
func main() -> i32 { 0 }
"#;
    assert!(
        check_source(src).is_ok(),
        "tuple-of-caps call argument must check: {:?}",
        check_source(src)
    );
}

// ─── G-2 MED: Bind position is now consistent with C-2 ─────────────────────
// Design call (documented at the Bind arm): XOR of DISTINCT resources into
// one binding is rejected at EVERY position (E0840). Exactly one resource
// flows at runtime; the surviving resource differs per path, so no
// straight-line continuation can consume both obligations verifiably. A
// branch duplicating ONE place stays legal (one obligation, one consumer).

#[test]
fn audit2_lin_g2_xor_bind_distinct_rejected_consistently() {
    let src = r#"
cap Token
func f(a: cap Token, b: cap Token, flag: bool) -> i32 {
    let z = if flag { a } else { b }
    drop(z)
    0
}
func main() -> i32 { 0 }
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0840),
        "bind of distinct-cap branch must reject with E0840 like call/return positions, got: {codes:?}"
    );
}

// ─── H-5 HIGH: use-after-move through the stale name ────────────────────────
// Move-with-target rewrote fact.owner keeping Available; later actions hit
// the same ResourceId by the OLD place name and consumed it. Owner
// validation (E0304) now precedes every transfer/consume.

#[test]
fn audit2_lin_h5_use_after_move_via_sink_rejected() {
    let src = r#"
cap Token
func sink(t: cap Token) -> i32 { drop(t); 1 }
func f(a: cap Token) -> i32 {
    let x = a
    sink(a)
    0
}
func main() -> i32 { 0 }
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0304),
        "sink(a) after `let x = a` must reject with E0304, got: {codes:?}"
    );
}

#[test]
fn audit2_lin_h5_use_after_move_via_drop_rejected() {
    let src = r#"
cap Token
func f(a: cap Token) -> i32 {
    let x = a
    drop(a)
    drop(x)
    0
}
func main() -> i32 { 0 }
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0304),
        "drop(a) after `let x = a` must reject with E0304, got: {codes:?}"
    );
}

#[test]
fn audit2_lin_h5_current_owner_still_legal() {
    let src = r#"
cap Token
func sink(t: cap Token) -> i32 { drop(t); 1 }
func f(a: cap Token) -> i32 {
    let x = a
    sink(x)
    0
}
func g(a: cap Token) -> i32 {
    let x = a
    let y = x
    drop(y)
    0
}
func main() -> i32 { 0 }
"#;
    assert!(
        check_source(src).is_ok(),
        "consumption through the current owner must check: {:?}",
        check_source(src)
    );
}

// ─── H-6 HIGH: anonymous temporary borrows terminate at statement end ──────
// `inc(&mut x)` created a loan with no named reference — liveness could
// never end it, so every later use of x hit a false E0415 and every loop
// iteration reported a live-across-backedge borrow. Synthesized BorrowEnd at
// the statement's terminating CFG point mirrors named-borrow NLL.

#[test]
fn audit2_lin_h6_anonymous_call_borrow_ends_legal() {
    let src = r#"
func inc(v: &mut i32) -> i32 { *v = *v + 1; 0 }
func f() -> i32 {
    let mut x = 1
    inc(&mut x)
    x
}
func main() -> i32 { f() }
"#;
    assert!(
        check_source(src).is_ok(),
        "read after an anonymous call-argument borrow must check: {:?}",
        check_source(src)
    );
}

#[test]
fn audit2_lin_h6_anonymous_borrow_in_loop_legal() {
    let src = r#"
func inc(v: &mut i32) -> i32 { *v = *v + 1; 0 }
func f() -> i32 {
    let mut x = 1
    let mut i = 0
    while i < 3 {
        inc(&mut x)
        i = i + 1
    }
    x
}
func main() -> i32 { f() }
"#;
    assert!(
        check_source(src).is_ok(),
        "anonymous borrow per loop iteration must check: {:?}",
        check_source(src)
    );
}

#[test]
fn audit2_lin_h6_overlapping_anonymous_borrows_still_rejected() {
    // Two mutable borrows alive inside ONE call still conflict — the fix
    // shortens loan lifetimes, it does not delete conflict detection.
    let src = r#"
func two(a: &mut i32, b: &mut i32) -> i32 { *a + *b }
func f() -> i32 {
    let mut x = 1
    two(&mut x, &mut x)
}
func main() -> i32 { 0 }
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0301),
        "two simultaneous mutable borrows of one place must reject with E0301, got: {codes:?}"
    );
}

#[test]
fn audit2_lin_h6_named_borrow_still_enforced() {
    // Named borrows keep their liveness-based NLL: reading the mutably
    // borrowed root while the reference is still live stays E0415.
    let src = r#"
func f() -> i32 {
    let mut value = 1
    let loan = &mut value
    let copied = value
    *loan = 2
    copied
}
func main() -> i32 { 0 }
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0415),
        "root read during a live named mutable borrow must reject with E0415, got: {codes:?}"
    );
}

// ─── RED LINE §1.3: split() + wildcard atom escape ─────────────────────────
// `let (_, w) = c.split()` compressed the wildcard away, count 1==1 passed
// vacuously and the read atom leaked with zero obligation.

#[test]
fn audit2_lin_split_wildcard_first_rejected() {
    let src = r#"
cap FileReadCap;
cap FileWriteCap;
cap FullAccess = FileReadCap + FileWriteCap;
func main() -> i32 {
    let c = FullAccess
    let (_, w) = c.split()
    drop(w)
    0
}
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0304),
        "wildcard-discarded split atom must reject with E0304, got: {codes:?}"
    );
}

#[test]
fn audit2_lin_split_wildcard_second_rejected() {
    let src = r#"
cap FileReadCap;
cap FileWriteCap;
cap FullAccess = FileReadCap + FileWriteCap;
func main() -> i32 {
    let c = FullAccess
    let (r, _) = c.split()
    drop(r)
    0
}
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0304),
        "wildcard-discarded split atom must reject with E0304, got: {codes:?}"
    );
}

#[test]
fn audit2_lin_split_all_bound_still_legal() {
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
        "fully bound split must check: {:?}",
        check_source(src)
    );
}

// ─── Obligation-establishment surface (wave1-review §5.5) ───────────────────
// Statement-style discard of a linear call result established NO obligation.

#[test]
fn audit2_lin_discarded_linear_call_result_rejected() {
    let src = r#"
cap Token
func make() -> cap Token { Token }
func f() -> i32 {
    make()
    0
}
func main() -> i32 { 0 }
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0256),
        "discarded linear call result must reject with E0256, got: {codes:?}"
    );
}

#[test]
fn audit2_lin_call_result_bound_and_consumed_still_legal() {
    let src = r#"
cap Token
func make() -> cap Token { Token }
func f() -> i32 {
    let t = make()
    drop(t)
    0
}
func main() -> i32 { 0 }
"#;
    assert!(
        check_source(src).is_ok(),
        "bound-then-consumed call result must check: {:?}",
        check_source(src)
    );
}

// ─── G-1 MED: assignment re-keys target identity + moves every source ──────

#[test]
fn audit2_lin_g1_reassign_after_drop_legal() {
    let src = r#"
cap Token
func f(a: cap Token, b: cap Token) -> i32 {
    let mut x = a
    drop(x)
    x = b
    drop(x)
    0
}
func main() -> i32 { 0 }
"#;
    assert!(
        check_source(src).is_ok(),
        "drop → reassign → drop must check: {:?}",
        check_source(src)
    );
}

#[test]
fn audit2_lin_g1_aggregate_bind_then_drop_legal() {
    // `let x = (a, b)` is a legal aggregate merge; drop(x) discharges both.
    let src = r#"
cap Token
func f(a: cap Token, b: cap Token) -> i32 {
    let x = (a, b)
    drop(x)
    0
}
func main() -> i32 { 0 }
"#;
    assert!(
        check_source(src).is_ok(),
        "aggregate bind + drop must check: {:?}",
        check_source(src)
    );
}

#[test]
fn audit2_lin_g1_aggregate_bind_unconsumed_rejected() {
    let src = r#"
cap Token
func f(a: cap Token, b: cap Token) -> i32 {
    let x = (a, b)
    0
}
func main() -> i32 { 0 }
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0256),
        "unconsumed aggregate binding must reject with E0256, got: {codes:?}"
    );
}

#[test]
fn audit2_lin_g1_overwrite_live_linear_rejected() {
    // Assigning over a still-live linear place leaks the old value.
    let src = r#"
cap Token
func f(a: cap Token, b: cap Token) -> i32 {
    let mut x = a
    x = b
    drop(x)
    0
}
func main() -> i32 { 0 }
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0256),
        "overwriting a live linear place must reject with E0256, got: {codes:?}"
    );
}

// ─── G-4 LOW: for/while-let iterable evaluated once (hoisted) ──────────────

#[test]
fn audit2_lin_g4_for_iterable_single_evaluation_legal() {
    // The iterable's borrow ends with the (once-evaluated) iterable, so the
    // body may write through the borrowed root's name.
    let src = r#"
func g(r: &i32) -> i32 { *r }
func f() -> i32 {
    let mut y = 1
    for x in [g(&y)] {
        y = 2
        println(x)
    }
    y
}
func main() -> i32 { f() }
"#;
    assert!(
        check_source(src).is_ok(),
        "write after a hoisted for-iterable borrow must check: {:?}",
        check_source(src)
    );
}

// ─── G-5 LOW: diverging paths are audited for linear obligations ───────────

#[test]
fn audit2_lin_g5_divergent_path_leak_rejected() {
    let src = r#"
cap Token
func f(t: cap Token, flag: bool) -> i32 {
    if flag { drop(t) } else { loop { } }
    0
}
func main() -> i32 { 0 }
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0256),
        "linear resource held into an infinite loop must reject with E0256, got: {codes:?}"
    );
}

#[test]
fn audit2_lin_g5_divergent_path_consumed_still_legal() {
    let src = r#"
cap Token
func f(t: cap Token, flag: bool) -> i32 {
    drop(t)
    if flag { loop { } }
    0
}
func main() -> i32 { 0 }
"#;
    assert!(
        check_source(src).is_ok(),
        "consumed-before-divergence must check: {:?}",
        check_source(src)
    );
}

// ─── Audit §2-#16: from_json::<cap> turbofish must not fabricate a linear
// value ──────────────────────────────────────────────────────────────
// VERIFIED 2026-08-05: `from_json::<Token>("...")` checked OK — a JSON
// string minted a capability out of thin air, bypassing exactly-once.
// Fix: infer_turbofish rejects linear target types (E0432, H2 deep
// predicate). Bare capability names parse as `Type::Name` in turbofish /
// type-argument position, so `Checker::is_linear_surface_type` now also
// consults `declared_caps`; the unification table seeds cap names for the
// C-1 bare-container arm.
#[test]
fn audit2_lin_from_json_turbofish_cap_rejected() {
    let src = r#"
cap Token
func main() -> i32 {
    let t = from_json::<Token>("{\"x\": 1}")
    drop(t)
    0
}
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0432),
        "from_json::<cap> must reject with E0432, got: {codes:?}"
    );
}

#[test]
fn audit2_lin_from_json_turbofish_cap_container_rejected() {
    // H2: a container carrying a linear element is equally forbidden.
    let src = r#"
cap Token
func main() -> i32 {
    let l = from_json::<List<Token>>("[{\"x\": 1}]")
    drop(l)
    0
}
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0432),
        "from_json::<List<cap>> must reject with E0432, got: {codes:?}"
    );
}

#[test]
fn audit2_lin_from_json_nonlinear_still_ok() {
    // Sanity: concrete non-linear targets keep working.
    let src = r#"
func main() -> i32 {
    let v = from_json::<i32>("42")
    let m = from_json::<Map<string, i32>>("{\"a\": 1}")
    drop(v)
    drop(m)
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "non-linear from_json targets must still check: {:?}",
        check_source(src)
    );
}

// ─── Audit §2-#12: generic parameter names shadowing builtin types ───
// VERIFIED 2026-08-05: `func f<i32>(x: i32)` — the generic param `i32`
// hijacks every same-named type in the signature at instantiation; the
// declared `-> i32` actually returns string (call-site E0209) or slips to
// resolved TOOL-RESOLUTION-001. Fix: E0436 up front, function + type
// generics.
#[test]
fn audit2_gen_builtin_shadow_func_rejected() {
    let src = r#"
func f<i32>(x: i32) -> i32 {
    return x
}
func main() -> i32 {
    let s = "hello"
    let y = f(s)
    0
}
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0436),
        "builtin-shadowing func generic must reject with E0436, got: {codes:?}"
    );
}

#[test]
fn audit2_gen_builtin_shadow_type_rejected() {
    let src = r#"
type Box<string> = string
func main() -> i32 {
    let b: Box<i32> = 1
    0
}
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0436),
        "builtin-shadowing type generic must reject with E0436, got: {codes:?}"
    );
}

#[test]
fn audit2_gen_normal_generics_still_ok() {
    let src = r#"
func id<T>(x: T) -> T {
    return x
}
func main() -> i32 {
    let a = id(42)
    let b = id("hi")
    drop(b)
    a
}
"#;
    assert!(
        check_source(src).is_ok(),
        "normal generics must still check: {:?}",
        check_source(src)
    );
}

/// AUD-4 (2026-08-20 critical audit): a linear capability passed into OR out of
/// an actor mailbox must be rejected — the mailbox byte-copies the handle,
/// which would duplicate a linear resource (exactly-once violation). Regression
/// asserts E0432 on both the parameter and the return type.
#[test]
fn audit_linear_cap_cannot_enter_actor_mailbox_e0432() {
    let src = r#"
cap File
actor A {
    func m(c: cap File) -> cap File {
        c
    }
}
func main() -> i32 { 0 }
"#;
    let codes = rejection_codes(src);
    assert!(
        codes.iter().any(|c| c == crate::diagnostic::codes::E0432),
        "linear cap in actor mailbox must reject with E0432, got: {codes:?}"
    );
}
