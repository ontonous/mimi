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
    assert!(check_source(src).is_ok(), "simple immutable borrow should pass");
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
    assert!(check_source(src).is_ok(), "simple mutable borrow should pass");
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
    assert!(check_source(src).is_ok(), "sequential imm then mut should pass");
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
    assert!(check_source(src).is_ok(), "reborrow &mut T as &T should pass");
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
    assert!(check_source(src).is_ok(), "reborrow &mut T as &mut T should pass");
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
    assert!(check_source(src).is_ok(), "NLL: borrow released after last use");
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
    assert!(check_source(src).is_ok(), "NLL: &mut allowed after &x last use");
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
    assert!(check_source(src).is_err(), "& then &mut (both used) should be rejected");
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
    assert!(check_source(src).is_err(), "&mut then & (both used) should be rejected");
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
    assert!(check_source(src).is_ok(), "field-level disjoint borrow should pass");
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
    assert!(check_source(src).is_ok(), "function returning reference should pass");
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
    assert!(check_source(src).is_ok(), "fn returning & from &mut should pass");
}

#[test]
#[ignore = "v1.2: no borrow tracking through conditional paths"]
fn borrow_conditional_return() {
    let src = r#"
func max_ref<'a>(x: &'a i32, y: &'a i32) -> &'a i32 {
    if *x > *y { x } else { y }
}
func main() -> i32 {
    let a = 10;
    let b = 20;
    let r = max_ref(&a, &b);
    *r
}
"#;
    assert!(check_source(src).is_ok(), "conditional borrow return should pass");
}

#[test]
#[ignore = "v1.2: no closure capture borrow tracking"]
fn borrow_closure_capture_ref() {
    let src = r#"
func main() -> i32 {
    let a = 42;
    let r = &a;
    let f = || -> i32 { *r };
    f()
}
"#;
    assert!(check_source(src).is_ok(), "closure capturing ref should pass");
}

#[test]
#[ignore = "v1.2: no self-referential struct borrow tracking"]
fn borrow_self_referential() {
    let src = r#"
struct SelfRef<'a> {
    val: i32,
    ptr: &'a i32,
}
func main() -> i32 {
    let v = 42;
    let _s = SelfRef { val: 0, ptr: &v };
    0
}
"#;
    assert!(check_source(src).is_ok(), "self-referential borrow should pass");
}
