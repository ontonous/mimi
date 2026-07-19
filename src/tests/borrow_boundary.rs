use super::*;

// ===== Stage 4: Borrow checker boundary tests =====
//
// These tests document what the current borrow checker
// (core/borrow.rs) CAN and CANNOT verify.
//
// Current capability (v1.0):
// - BorrowState: Unborrowed | BorrowedImm{span} | BorrowedMut{span}
// - NLL: borrow ends at last use, not block end
// - Rejects simultaneous &mut + & of same variable
//
// v1.2 gaps (tracked in AGENTS.mimi.md):
// - No field-level borrow tracking: p.x borrows entire p
// - No closure capture borrow tracking
// - No lifetime annotations or inference
// - No borrow inference across function return boundaries

// ── Basic borrow patterns (should pass) ─────────────────────────

#[test]
fn borrow_imm_simple() {
    let src = r#"
func read(x: &i32) -> i32 { *x }
func main() -> i32 {
    let a = 42;
    let r = &a;
    read(r);
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
func write(x: &mut i32) { *x = *x + 1 }
func main() -> i32 {
    let mut a = 41;
    let r = &mut a;
    write(r);
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
func read(x: &i32) -> i32 { *x }
func write(x: &mut i32) { *x = *x + 1 }
func main() -> i32 {
    let mut a = 41;
    let r1 = &a;
    let v = read(r1);
    let r2 = &mut a;
    write(r2);
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
func read(x: &i32) -> i32 { *x }
func main() -> i32 {
    let a = 42;
    let r1 = &a;
    let r2 = &*r1;
    read(r1);
    read(r2);
    a
}
"#;
    assert!(check_source(src).is_ok(), "reborrow &T as &T should pass");
}

#[test]
fn borrow_mut_reborrow_imm() {
    let src = r#"
func read(x: &i32) -> i32 { *x }
func main() -> i32 {
    let mut a = 42;
    let r1 = &mut a;
    let r2 = &*r1;
    read(r2);
    a
}
"#;
    assert!(
        check_source(src).is_ok(),
        "reborrow &mut T as &T should pass"
    );
}

#[test]
fn borrow_mut_reborrow_mut() {
    let src = r#"
func write(x: &mut i32) { *x = *x + 1 }
func main() -> i32 {
    let mut a = 42;
    let r1 = &mut a;
    let r2 = &mut *r1;
    write(r2);
    a
}
"#;
    assert!(
        check_source(src).is_ok(),
        "reborrow &mut T as &mut T should pass"
    );
}

#[test]
fn borrow_projected_field_mutates_original_record_in_interpreter() {
    let src = r#"
type Inner { value: i32 }
type Outer { inner: Inner }
func main() -> i32 {
    let mut item = Outer { inner: Inner { value: 7 } }
    let value = &mut item.inner.value
    *value = 12
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
    let value = &mut pair.1
    *value = 11
    pair.1
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(11));
}

#[test]
fn borrow_nll_release_after_last_use() {
    let src = r#"
func read(x: &i32) -> i32 { *x }
func write(x: &mut i32) { *x = *x + 1 }
func main() -> i32 {
    let mut a = 42;
    let r = &a;
    let v = read(r);
    // r is no longer used — borrow released (NLL)
    let r2 = &mut a;
    write(r2);
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
func read(x: &i32) -> i32 { *x }
func main() -> i32 {
    let mut a = 42;
    let r = &a;
    let v = read(r);
    // r no longer used — safe to &mut
    let r2 = &mut a;
    *r2 = v + 1;
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
func read(x: &i32) -> i32 { *x }
func main() -> i32 {
    let a = 42;
    let r1 = &a;
    let r2 = &a;
    read(r1) + read(r2)
}
"#;
    assert!(check_source(src).is_ok(), "multiple &x allowed");
}

// ── Known rejection (should fail) ───────────────────────────────

#[test]
fn borrow_double_mut_rejected() {
    let src = r#"
func write(x: &mut i32) { *x = *x + 1 }
func main() -> i32 {
    let mut a = 42;
    let r1 = &mut a;
    let r2 = &mut a;
    write(r1);
    write(r2);
    a
}
"#;
    assert!(check_source(src).is_err(), "double &mut should be rejected");
}

#[test]
fn borrow_imm_then_mut_rejected() {
    let src = r#"
func read(x: &i32) -> i32 { *x }
func write(x: &mut i32) { *x = *x + 1 }
func main() -> i32 {
    let mut a = 42;
    let r1 = &a;
    let r2 = &mut a;
    write(r2);
    read(r1);
    a
}
"#;
    assert!(
        check_source(src).is_err(),
        "& then &mut (both used) should be rejected"
    );
}

#[test]
fn borrow_mut_then_imm_rejected() {
    let src = r#"
func read(x: &i32) -> i32 { *x }
func write(x: &mut i32) { *x = *x + 1 }
func main() -> i32 {
    let mut a = 42;
    let r1 = &mut a;
    let r2 = &a;
    write(r1);
    read(r2);
    a
}
"#;
    assert!(
        check_source(src).is_err(),
        "&mut then & (both used) should be rejected"
    );
}

// ── Known gaps (v1.2+) ──────────────────────────────────────────
// These tests document features that would pass once implemented.
// Currently the parser or checker rejects them.

#[test]
fn borrow_field_level_disjoint() {
    let src = r#"
type Pair { a: i32, b: i32 }
func main() -> i32 {
    let mut p = Pair { a: 1, b: 2 };
    let ra = &p.a;
    let rb = &mut p.b;
    println(*ra + *rb);
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
func first(x: &i32, _y: &i32) -> &i32 { x }
func main() -> i32 {
    let a = 10;
    let b = 20;
    let r = first(&a, &b);
    *r
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
func mut_to_imm(x: &mut i32) -> &i32 {
    *x = *x + 1;
    let r: &i32 = &*x;
    r
}
func main() -> i32 {
    let mut a = 41;
    let r = mut_to_imm(&mut a);
    *r
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
func choose(cond: bool, x: &i32, y: &i32) -> &i32 {
    if cond { x } else { y }
}
func main() -> i32 {
    let a = 10;
    let b = 20;
    let r = choose(true, &a, &b);
    *r
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
    let r = &a;
    let f = fn() -> i32 { *r };
    f()
}
"#;
    assert!(
        check_source(src).is_ok(),
        "closure capturing ref should pass"
    );
}

#[test]
fn borrow_self_referential() {
    let src = r#"
type Container { val: i32, ptr: &i32 }
func main() -> i32 {
    let v = 42;
    let _c = Container { val: 0, ptr: &v };
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "self-referential borrow should pass"
    );
}

/// E4: Match guard uses a borrow reference — NLL should not release it
/// before the guard evaluates.
#[test]
fn borrow_match_guard_uses_ref() {
    let src = r#"
func check(x: &i32) -> i32 {
    let val = *x;
    let result = match val {
        v if v > 10 => { *x }
        _ => { 0 }
    };
    result
}
func main() -> i32 {
    let a = 42;
    check(&a)
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
    let r = &p.x;
    let val = *r;
    let p2 = Point { x: 3, y: 4 };
    val + p2.x
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
    let r = &a;
    let v = *r;
    { v + 1 }
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
    let r = &a;
    let v = *r;
    { v + 1 }
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
    let r = &mut xs[1];
    *r = 99;
    xs[1]
}
"#;
    assert_eq!(run_source(src), crate::interp::Value::Int(99));
}

#[test]
fn borrow_index_imm_basic() {
    let src = r#"
func main() -> i32 {
    let xs = [1, 2, 3];
    let r = &xs[1];
    *r
}
"#;
    assert_eq!(run_source(src), crate::interp::Value::Int(2));
}

#[test]
fn borrow_index_mut_typecheck_rejects_imm() {
    let src = r#"
func main() -> i32 {
    let xs = [1, 2, 3];
    let r = &xs[1];
    *r = 99;
    xs[1]
}
"#;
    assert!(
        check_source(src).is_err(),
        "assign through &xs[i] should be rejected"
    );
}

#[test]
fn borrow_index_mut_conflict_rejected() {
    let src = r#"
func main() -> i32 {
    let mut xs = [1, 2, 3];
    let r1 = &mut xs[1];
    let r2 = &mut xs[1];
    *r1 = 10;
    *r2 = 20;
    xs[1]
}
"#;
    assert!(
        check_source(src).is_err(),
        "double &mut xs[i] should be rejected"
    );
}
