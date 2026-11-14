use super::*;

// ===== Stage 4: Borrow checker boundary tests =====
//
// OWN-PERMISSION-001 / RESOURCE-LINEAR-001: these tests exercise the
// canonical LoanId + typed-CFG dataflow boundary.
//
// - Every borrow has an independent stable LoanId.
// - Backwards CFG use-def liveness ends loans after their last use.
// - Structured Place overlap separates provably disjoint projections.
// - Mutable overlap with a read, write, move, drop, or loan is rejected.

// ── Basic borrow patterns (should pass) ─────────────────────────

#[test]
fn borrow_imm_simple() {
    let src = r#"
extern "C" func read(x: &i32) -> i32 { *x }
func main() -> i32 {
    let a = 42;
    unsafe {
        let r = &a;
        read(r);
    }
    a
}
"#;
    assert!(
        check_source(src).is_ok(),
        "simple immutable borrow should pass"
    );
}

#[test]
fn borrow_mut_simple() {
    let src = r#"
extern "C" func write(x: &mut i32) { *x = *x + 1 }
func main() -> i32 {
    let mut a = 41;
    unsafe {
        let r = &mut a;
        write(r);
    }
    a
}
"#;
    assert!(
        check_source(src).is_ok(),
        "simple mutable borrow should pass"
    );
}

#[test]
fn borrow_imm_mut_sequential() {
    let src = r#"
extern "C" func read(x: &i32) -> i32 { *x }
extern "C" func write(x: &mut i32) { *x = *x + 1 }
func main() -> i32 {
    let mut a = 41;
    let mut v = 0;
    unsafe {
        let r1 = &a;
        v = read(r1);
        let r2 = &mut a;
        write(r2);
    }
    v + a
}
"#;
    assert!(
        check_source(src).is_ok(),
        "sequential imm then mut should pass"
    );
}

#[test]
fn borrow_reborrow_imm() {
    let src = r#"
extern "C" func read(x: &i32) -> i32 { *x }
func main() -> i32 {
    let a = 42;
    unsafe {
        let r1 = &a;
        let r2 = &*r1;
        read(r1);
        read(r2);
    }
    a
}
"#;
    assert!(check_source(src).is_ok(), "reborrow &T as &T should pass");
}

#[test]
fn borrow_mut_reborrow_imm() {
    let src = r#"
extern "C" func read(x: &i32) -> i32 { *x }
func main() -> i32 {
    let mut a = 42;
    unsafe {
        let r1 = &mut a;
        let r2 = &*r1;
        read(r2);
    }
    a
}
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "reborrow &mut T as &T should pass: {result:#?}"
    );
}

#[test]
fn borrow_mut_reborrow_mut() {
    let src = r#"
extern "C" func write(x: &mut i32) { *x = *x + 1 }
func main() -> i32 {
    let mut a = 42;
    unsafe {
        let r1 = &mut a;
        let r2 = &mut *r1;
        write(r2);
    }
    a
}
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "reborrow &mut T as &mut T should pass: {result:#?}"
    );
}

#[test]
fn borrow_projected_field_mutates_original_record_in_interpreter() {
    let src = r#"
type Inner { value: i32 }
type Outer { inner: Inner }
func main() -> i32 {
    let mut item = Outer { inner: Inner { value: 7 } }
    unsafe {
        let value = &mut item.inner.value
        *value = 12
    }
    item.inner.value
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(12));
}

#[test]
fn borrow_projected_tuple_mutates_original_tuple_in_interpreter() {
    let src = r#"
func main() -> i32 {
    let mut pair = (4, 8)
    unsafe {
        let value = &mut pair.1
        *value = 11
    }
    pair.1
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(11));
}

#[test]
fn borrow_nll_release_after_last_use() {
    let src = r#"
extern "C" func read(x: &i32) -> i32 { *x }
extern "C" func write(x: &mut i32) { *x = *x + 1 }
func main() -> i32 {
    let mut a = 42;
    let mut v = 0;
    unsafe {
        let r = &a;
        v = read(r);
        // r is no longer used — borrow released (NLL)
        let r2 = &mut a;
        write(r2);
    }
    v + a
}
"#;
    assert!(
        check_source(src).is_ok(),
        "NLL: borrow released after last use"
    );
}

#[test]
fn borrow_nll_mut_after_imm_last_use() {
    let src = r#"
extern "C" func read(x: &i32) -> i32 { *x }
func main() -> i32 {
    let mut a = 42;
    let mut v = 0;
    unsafe {
        let r = &a;
        v = read(r);
        // r no longer used — safe to &mut
        let r2 = &mut a;
        *r2 = v + 1;
    }
    a
}
"#;
    assert!(
        check_source(src).is_ok(),
        "NLL: &mut allowed after &x last use"
    );
}

#[test]
fn borrow_double_imm_ok() {
    let src = r#"
extern "C" func read(x: &i32) -> i32 { *x }
func main() -> i32 {
    let a = 42;
    let mut result = 0;
    unsafe {
        let r1 = &a;
        let r2 = &a;
        result = read(r1) + read(r2);
    }
    result
}
"#;
    assert!(check_source(src).is_ok(), "multiple &x allowed");
}

// ── Known rejection (should fail) ───────────────────────────────

#[test]
fn borrow_double_mut_rejected() {
    let src = r#"
extern "C" func write(x: &mut i32) { *x = *x + 1 }
func main() -> i32 {
    let mut a = 42;
    unsafe {
        let r1 = &mut a;
        let r2 = &mut a;
        write(r1);
        write(r2);
    }
    a
}
"#;
    let errors = check_source(src).expect_err("double `&mut` should be rejected");
    assert!(
        errors.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0301)
        }),
        "overlapping mutable loans should report E0301: {errors:?}"
    );
}

#[test]
fn borrow_imm_then_mut_rejected() {
    let src = r#"
extern "C" func read(x: &i32) -> i32 { *x }
extern "C" func write(x: &mut i32) { *x = *x + 1 }
func main() -> i32 {
    let mut a = 42;
    unsafe {
        let r1 = &a;
        let r2 = &mut a;
        write(r2);
        read(r1);
    }
    a
}
"#;
    let errors = check_source(src).expect_err("an overlapping shared/mutable borrow must fail");
    assert!(
        errors.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0300)
        }),
        "shared then mutable overlap should report E0300: {errors:?}"
    );
}

#[test]
fn borrow_mut_then_imm_rejected() {
    let src = r#"
extern "C" func read(x: &i32) -> i32 { *x }
extern "C" func write(x: &mut i32) { *x = *x + 1 }
func main() -> i32 {
    let mut a = 42;
    unsafe {
        let r1 = &mut a;
        let r2 = &a;
        write(r1);
        read(r2);
    }
    a
}
"#;
    let errors = check_source(src).expect_err("an overlapping mutable/shared borrow must fail");
    assert!(
        errors.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0302)
        }),
        "mutable then shared overlap should report E0302: {errors:?}"
    );
}

// ── Structured-place and control-flow coverage ─────────────────

#[test]
fn borrow_field_level_disjoint() {
    let src = r#"
type Pair { a: i32, b: i32 }
func main() -> i32 {
    let mut p = Pair { a: 1, b: 2 };
    unsafe {
        let ra = &p.a;
        let rb = &mut p.b;
        println(*ra + *rb);
    }
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "field-level disjoint borrow should pass"
    );
}

#[test]
fn borrow_fn_return_ref() {
    let src = r#"
extern "C" func first(x: &i32, _y: &i32) -> &i32 { x }
func main() -> i32 {
    let a = 10;
    let b = 20;
    let mut result = 0;
    unsafe {
        let r = first(&a, &b);
        result = *r;
    }
    result
}
"#;
    assert!(
        check_source(src).is_ok(),
        "function returning reference should pass"
    );
}

#[test]
fn borrow_fn_mut_to_imm_return() {
    let src = r#"
extern "C" func mut_to_imm(x: &mut i32) -> &i32 {
    *x = *x + 1;
    let r: &i32 = &*x;
    r
}
func main() -> i32 {
    let mut a = 41;
    let mut result = 0;
    unsafe {
        let r = mut_to_imm(&mut a);
        result = *r;
    }
    result
}
"#;
    assert!(
        check_source(src).is_ok(),
        "fn returning & from &mut should pass"
    );
}

#[test]
fn borrow_conditional_return() {
    // Mimi does not support explicit lifetime params; the test validates that
    // conditional branches returning references type-check correctly.
    let src = r#"
extern "C" func choose(cond: bool, x: &i32, y: &i32) -> &i32 {
    if cond { x } else { y }
}
func main() -> i32 {
    let a = 10;
    let b = 20;
    let mut result = 0;
    unsafe {
        let r = choose(true, &a, &b);
        result = *r;
    }
    result
}
"#;
    assert!(
        check_source(src).is_ok(),
        "conditional borrow return should pass"
    );
}

#[test]
fn borrow_closure_capture_ref() {
    let src = r#"
func main() -> i32 {
    let a = 42;
    let mut result = 0;
    unsafe {
        let r = &a;
        let f = fn() -> i32 {
            let mut captured = 0;
            unsafe { captured = *r; }
            captured
        };
        result = f();
    }
    result
}
"#;
    assert!(
        check_source(src).is_ok(),
        "closure capturing ref should pass"
    );
}

#[test]
fn borrow_self_referential_type_requires_explicit_low_level_context() {
    let src = r#"
type Container { val: i32, ptr: &i32 }
func main() -> i32 {
    let v = 42;
    let _c = Container { val: 0, ptr: &v };
    0
}
"#;
    let errors = check_source(src).expect_err("record reference fields are outside safe source");
    assert!(
        errors.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0449)
        }),
        "the record reference field must report E0449: {errors:?}"
    );
}

/// E4: Match guard uses a borrow reference — NLL should not release it
/// before the guard evaluates.
#[test]
fn borrow_match_guard_uses_ref() {
    let src = r#"
extern "C" func check(x: &i32) -> i32 {
    let val = *x;
    let result = match val {
        v if v > 10 => { *x }
        _ => { 0 }
    };
    result
}
func main() -> i32 {
    let a = 42;
    let mut result = 0;
    unsafe {
        result = check(&a);
    }
    result
}
"#;
    assert!(
        check_source(src).is_ok(),
        "match guard using borrow ref should pass"
    );
}

/// E5: Field-level borrow released at NLL last use.
#[test]
fn borrow_field_level_nll_release() {
    let src = r#"
type Point { x: i32, y: i32 }
func main() -> i32 {
    let p = Point { x: 1, y: 2 };
    let mut result = 0;
    unsafe {
        let r = &p.x;
        let val = *r;
        let p2 = Point { x: 3, y: 4 };
        result = val + p2.x;
    }
    result
}
"#;
    assert!(
        check_source(src).is_ok(),
        "field borrow should release at last use"
    );
}

/// V7: NLL borrow released across nested block boundaries.
#[test]
fn borrow_nll_cross_block() {
    let src = r#"
func main() -> i32 {
    let a = 10;
    let mut result = 0;
    unsafe {
        let r = &a;
        let v = *r;
        { result = v + 1; }
    }
    result
}
"#;
    assert!(
        check_source(src).is_ok(),
        "NLL cross-block borrow should pass"
    );
}

/// V7: NLL borrow released after last use even with multiple blocks.
#[test]
fn borrow_nll_multi_block() {
    let src = r#"
func main() -> i32 {
    let a = 10;
    let mut result = 0;
    unsafe {
        let r = &a;
        let v = *r;
        { result = v + 1; }
    }
    result
}
"#;
    assert!(
        check_source(src).is_ok(),
        "NLL multi-block borrow should pass"
    );
}

// ── Borrowed index semantics (v0.28.24) ─────────────────────────

#[test]
fn borrow_index_mut_basic() {
    let src = r#"
func main() -> i32 {
    let mut xs = [1, 2, 3];
    let mut result = 0;
    unsafe {
        let r = &mut xs[1];
        *r = 99;
        result = xs[1];
    }
    result
}
"#;
    assert_eq!(run_source(src), crate::interp::Value::Int(99));
}

#[test]
fn borrow_index_imm_basic() {
    let src = r#"
func main() -> i32 {
    let xs = [1, 2, 3];
    let mut result = 0;
    unsafe {
        let r = &xs[1];
        result = *r;
    }
    result
}
"#;
    assert_eq!(run_source(src), crate::interp::Value::Int(2));
}

#[test]
fn borrow_index_mut_typecheck_rejects_imm() {
    let src = r#"
func main() -> i32 {
    let xs = [1, 2, 3];
    unsafe {
        let r = &xs[1];
        *r = 99;
    }
    xs[1]
}
"#;
    let errors = check_source(src).expect_err("an immutable reference cannot be assigned through");
    assert!(
        errors.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0233)
        }),
        "assignment through `&xs[i]` should report E0233: {errors:?}"
    );
}

#[test]
fn borrow_index_mut_conflict_rejected() {
    let src = r#"
func main() -> i32 {
    let mut xs = [1, 2, 3];
    unsafe {
        let r1 = &mut xs[1];
        let r2 = &mut xs[1];
        *r1 = 10;
        *r2 = 20;
    }
    xs[1]
}
"#;
    let errors = check_source(src).expect_err("overlapping mutable element loans must fail");
    assert!(
        errors.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0301)
        }),
        "double `&mut xs[i]` should report E0301: {errors:?}"
    );
}

// ── Worklist convergence regression (deep-eval 2026-08-09) ────────
//
// `while { … if c { continue } … }` used to deadlock the resource
// dataflow worklist: the loop header carries a Continue-edge predecessor
// from inside the body, `predecessors_ready` demanded its out-state before
// processing the header, dropped the header when the state was absent, and
// the queued-gate never re-pushed it. The loop-exit block then never got
// an out-state and every such function failed with a spurious "resource
// analysis did not reach return block" (demos/02_control_flow shipped
// broken through the whole 0.1.4 window).

#[test]
fn resource_analysis_converges_with_continue_in_while() {
    let src = r#"
func f() {
    let mut val = 0
    while val < 10 {
        val += 1
        if val == 3 { continue }
        println("x")
    }
}
func main() -> i32 { f(); 0 }
"#;
    assert!(
        check_source(src).is_ok(),
        "if-continue inside while must not stall resource analysis"
    );
}

#[test]
fn resource_analysis_converges_with_continue_and_break() {
    let src = r#"
func count_with_break() {
    let mut val = 0
    while val < 10 {
        val += 1
        if val == 3 { continue }
        if val == 8 { break }
        println("  val =", val)
    }
}
func main() -> i32 { count_with_break(); 0 }
"#;
    assert!(
        check_source(src).is_ok(),
        "continue + break combo inside while must type-check"
    );
}
