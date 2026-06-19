use super::*;

#[test]
fn typecheck_double_mut_borrow_error() {
    // NLL: double &mut is only an error if the first is used later
    let src = r#"
func main() -> i32 {
    let mut x = 42;
    let r1 = &mut x;
    let r2 = &mut x;
    println(*r1);
    1
}
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(result.is_err());
    let errors = result.unwrap_err();
    let has_borrow_error = errors.iter().any(|e| e.message.contains("already mutably borrowed"));
    assert!(has_borrow_error, "Expected mutable borrow error, got: {:?}", errors);
}

#[test]
fn typecheck_imm_mut_borrow_error() {
    // NLL: &x then &mut x is only an error if &x reference is used later
    let src = r#"
func main() -> i32 {
    let x = 42;
    let r1 = &x;
    let r2 = &mut x;
    println(*r1);
    1
}
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(result.is_err());
    let errors = result.unwrap_err();
    let has_borrow_error = errors.iter().any(|e| e.message.contains("already immutably borrowed"));
    assert!(has_borrow_error, "Expected immutable borrow error, got: {:?}", errors);
}

#[test]
fn typecheck_double_imm_borrow_ok() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    let r1 = &x;
    let r2 = &x;
    1
}
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(result.is_ok(), "Multiple immutable borrows should be allowed");
}

#[test]
fn typecheck_borrow_scope_isolation() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    {
        let r = &mut x;
    }
    let r2 = &x;
    1
}
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(result.is_ok(), "Borrows should be isolated to their scope");
}

#[test]
fn nll_borrow_released_after_last_use() {
    // NLL: borrow should end at last use, not at block end
    let src = r#"
func main() -> i32 {
    let mut x = 42;
    let r = &x;
    let _ = *r;
    // r is no longer used, so x should be borrowable again
    let r2 = &mut x;
    *r2 = 100;
    1
}
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(result.is_ok(), "NLL should allow reborrow after last use: {:?}", result.err());
}

#[test]
fn nll_mut_borrow_after_last_use_of_imm() {
    // NLL: &mut x should be allowed after &x's last use
    let src = r#"
func main() -> i32 {
    let mut x = 10;
    let r1 = &x;
    let val = *r1;
    // r1 is no longer used
    let r2 = &mut x;
    *r2 = val + 1;
    1
}
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(result.is_ok(), "NLL should allow &mut after &x last use: {:?}", result.err());
}

#[test]
fn nll_still_rejects_concurrent_borrows() {
    // NLL: borrows that ARE used in the same statement should conflict
    let src = r#"
func main() -> i32 {
    let mut x = 42;
    let r1 = &x;
    let r2 = &mut x;
    let _ = *r1;
    1
}
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(result.is_err(), "Overlapping borrows should be rejected");
}

#[test]
fn nll_unused_ref_releases_borrow() {
    // NLL: unused reference should release the borrow
    let src = r#"
func main() -> i32 {
    let mut x = 42;
    let _r1 = &x;
    let r2 = &mut x;
    *r2 = 100;
    1
}
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(result.is_ok(), "Unused reference should release borrow: {:?}", result.err());
}

#[test]
fn nll_rejects_mut_during_active_borrow() {
    // NLL: &mut x while &x reference is still live (used in later statement)
    let src = r#"
func main() -> i32 {
    let mut x = 42;
    let r1 = &x;
    let r2 = &mut x;
    println(*r1);
    1
}
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(result.is_err(), "Should reject &mut while &x reference is still used later");
}

#[test]
fn nll_borrow_used_in_last_statement() {
    // Borrow used in the last statement should be fine
    let src = r#"
func main() -> i32 {
    let x = 42;
    let r = &x;
    *r
}
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(result.is_ok(), "Borrow used in last statement should be allowed: {:?}", result.err());
}

#[test]
fn typecheck_arg_type_mismatch() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    a + b
}

func main() -> i32 {
    add(1, "hello")
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "Should reject string arg for i32 param");
}

#[test]
fn typecheck_return_type_mismatch() {
    let src = r#"
func main() -> i32 {
    "hello"
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "Should reject string return when i32 expected");
}

#[test]
fn typecheck_unary_not_on_non_bool() {
    let src = r#"
func main() -> bool {
    !42
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "Should reject ! on non-bool");
}

#[test]
fn typecheck_binary_op_type_mismatch() {
    let src = r#"
func main() -> i32 {
    1 + "hello"
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "Should reject i32 + string");
}

#[test]
fn typecheck_if_condition_non_bool() {
    let src = r#"
func main() -> i32 {
    if 42 {
        1
    } else {
        0
    }
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "Should reject non-bool if condition");
}

#[test]
fn typecheck_assignment_type_mismatch() {
    let src = r#"
func main() {
    let x: string = 42;
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "Should reject int assigned to string var");
}

#[test]
fn typecheck_missing_return() {
    let src = r#"
func main() -> i32 {
    let x = 1;
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "Should reject function missing return");
}
