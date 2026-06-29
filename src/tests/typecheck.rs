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
    let has_borrow_error = errors
        .iter()
        .any(|e| e.message.contains("already mutably borrowed"));
    assert!(
        has_borrow_error,
        "Expected mutable borrow error, got: {:?}",
        errors
    );
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
    let has_borrow_error = errors
        .iter()
        .any(|e| e.message.contains("already immutably borrowed"));
    assert!(
        has_borrow_error,
        "Expected immutable borrow error, got: {:?}",
        errors
    );
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
    assert!(
        result.is_ok(),
        "Multiple immutable borrows should be allowed"
    );
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
    assert!(
        result.is_ok(),
        "NLL should allow reborrow after last use: {:?}",
        result.err()
    );
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
    assert!(
        result.is_ok(),
        "NLL should allow &mut after &x last use: {:?}",
        result.err()
    );
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
    assert!(
        result.is_ok(),
        "Unused reference should release borrow: {:?}",
        result.err()
    );
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
    assert!(
        result.is_err(),
        "Should reject &mut while &x reference is still used later"
    );
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
    assert!(
        result.is_ok(),
        "Borrow used in last statement should be allowed: {:?}",
        result.err()
    );
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

#[test]
fn fmt_type_option_consistent_with_same_type() {
    // 4f7e760: fmt_type must produce same format as same_type uses internally
    // Option<T> should be "Option<T>", not "T?"
    let t = crate::ast::Type::Option(Box::new(crate::ast::Type::Name("i32".into(), vec![])));
    let formatted = crate::core::fmt_type(&t);
    assert_eq!(
        formatted, "Option<i32>",
        "Option type must format as `Option<T>`, got: {}",
        formatted
    );
}

#[test]
fn fmt_type_option_nested() {
    // Consistency check for nested Option<Option<T>>
    let inner = crate::ast::Type::Name("i32".into(), vec![]);
    let t = crate::ast::Type::Option(Box::new(crate::ast::Type::Option(Box::new(inner))));
    let formatted = crate::core::fmt_type(&t);
    assert_eq!(
        formatted, "Option<Option<i32>>",
        "Nested Option type must format as `Option<Option<T>>`, got: {}",
        formatted
    );
}

#[test]
fn fmt_type_result_contains_option() {
    // Verify Option<T> inside Result<_, _> also uses canonical format
    let opt_i32 = crate::ast::Type::Option(Box::new(crate::ast::Type::Name("i32".into(), vec![])));
    let t = crate::ast::Type::Result(
        Box::new(opt_i32),
        Box::new(crate::ast::Type::Name("string".into(), vec![])),
    );
    let formatted = crate::core::fmt_type(&t);
    assert_eq!(
        formatted, "Result<Option<i32>, string>",
        "Result<Option<i32>> must use canonical Option format, got: {}",
        formatted
    );
}

#[test]
fn typecheck_numeric_coercion_i32_to_i64_let() {
    // e895f82: is_numeric_coercion allows i32 literal → i64 declared type
    let src = r#"
        func main() -> i64 {
            let x: i64 = 42
            x
        }
    "#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "i32→i64 coercion in let should be accepted, got: {:?}",
        result.err()
    );
}

#[test]
fn typecheck_numeric_coercion_i32_to_i64_arg() {
    // e895f82: is_numeric_coercion allows i32 literal → i64 parameter
    // Use identity function to avoid mixed-type arithmetic (i64 * i32 is not auto-coerced)
    let src = r#"
        func identity(x: i64) -> i64 { x }
        func main() -> i64 { identity(21) }
    "#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "i32→i64 coercion in func arg should be accepted, got: {:?}",
        result.err()
    );
}

#[test]
fn typecheck_numeric_coercion_i32_to_f64() {
    // e895f82: is_numeric_coercion allows i32 literal → f64 declared type
    let src = r#"
        func main() -> f64 {
            let x: f64 = 3
            x
        }
    "#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "i32→f64 coercion should be accepted, got: {:?}",
        result.err()
    );
}

#[test]
fn typecheck_ensures_result_binding() {
    // e895f82: ensures can reference `result` via injected scope
    let src = r#"
        func double(x: i32) -> i32 {
            ensures: result == x * 2
            x * 2
        }
        func main() -> i32 { double(5) }
    "#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "ensures with `result` should type-check, got: {:?}",
        result.err()
    );
}

// ─── IDD binary numeric coercion tests ─────────────────────────
// Covers the gap tracked in fuzz::target_typesoundness:
// the interpreter executes i32 + i64 but the typechecker rejects it.

#[test]
fn typecheck_binary_numeric_coercion_i32_i64_add() {
    let src = r#"
        func main() -> i64 {
            let x: i32 = 1;
            let y: i64 = 2;
            x + y
        }
    "#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "i32 + i64 should type-check with widening, got: {:?}",
        result.err()
    );
}

#[test]
fn typecheck_binary_numeric_coercion_i32_i64_all_ops() {
    let src = r#"
        func main() -> i64 {
            let x: i32 = 10;
            let y: i64 = 3;
            let a = x + y;
            let b = x - y;
            let c = x * y;
            let d = x / y;
            a + b + c + d
        }
    "#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "mixed i32/i64 arithmetic should type-check, got: {:?}",
        result.err()
    );
}

#[test]
fn typecheck_binary_numeric_coercion_i32_f64() {
    let src = r#"
        func main() -> f64 {
            let x: i32 = 1;
            let y: f64 = 2.5;
            x + y
        }
    "#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "i32 + f64 should type-check with widening, got: {:?}",
        result.err()
    );
}

#[test]
fn typecheck_binary_numeric_coercion_i64_f64() {
    let src = r#"
        func main() -> f64 {
            let x: i64 = 1;
            let y: f64 = 2.5;
            x * y
        }
    "#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "i64 * f64 should type-check with widening, got: {:?}",
        result.err()
    );
}

#[test]
fn typecheck_comparison_numeric_coercion_i32_i64() {
    let src = r#"
        func main() -> bool {
            let x: i32 = 1;
            let y: i64 = 2;
            x < y
        }
    "#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "i32 < i64 comparison should type-check, got: {:?}",
        result.err()
    );
}

#[test]
fn typecheck_binary_numeric_coercion_does_not_allow_string() {
    // Sanity check: widening must not accept string + number.
    let src = r#"
        func main() -> i32 {
            "hello" + 1
        }
    "#;
    let result = check_source(src);
    assert!(result.is_err(), "string + number must remain a type error");
}

#[test]
fn typecheck_contract_with_shared_param_is_error() {
    let src = r#"
func bad_shared(x: shared i32) -> i32 {
    requires: x > 0
    x
}
func main() -> i32 { 0 }
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "expected error for contract on shared param function"
    );
    let errors = result.unwrap_err();
    let has_shared_contract_error = errors.iter().any(|e| e.message.contains("shared"));
    assert!(
        has_shared_contract_error,
        "expected shared contract error, got: {:?}",
        errors
    );
}

#[test]
fn typecheck_contract_with_local_shared_param_is_error() {
    let src = r#"
func bad_local(x: local_shared i32) -> i32 {
    requires: x > 0
    x
}
func main() -> i32 { 0 }
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "expected error for contract on local_shared param function"
    );
}

#[test]
fn typecheck_shared_param_no_contract_ok() {
    let src = r#"
func ok_shared(x: shared i32) -> i32 {
    x
}
func main() -> i32 { 0 }
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "expected no error for shared param without contract, got: {:?}",
        result
    );
}

#[test]
fn typecheck_contract_without_shared_param_ok() {
    let src = r#"
func ok_normal(x: i32) -> i32 {
    requires: x > 0
    x
}
func main() -> i32 { ok_normal(1) }
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "expected no error for contract without shared param, got: {:?}",
        result
    );
}

#[test]
fn warn_shared_write_write_parasteps() {
    // Two steps writing to the same shared var → W005
    let src = r#"
func main() -> i32 {
    shared x = 0
    parasteps {
        *x = 1
        *x = 2
    }
    0
}
"#;
    let warnings = check_source_warnings(src);
    let has_w005 = warnings
        .iter()
        .any(|w| w.code.as_deref() == Some(crate::diagnostic::codes::W005));
    assert!(
        has_w005,
        "expected W005 warning for shared var written by multiple steps, got: {:?}",
        warnings
    );
}

#[test]
fn warn_shared_push_same_list_parasteps() {
    // push on same shared list in multiple steps → W005
    let src = r#"
func main() -> i32 {
    shared xs = [1, 2, 3]
    parasteps {
        push(xs, 4)
        push(xs, 5)
    }
    0
}
"#;
    let warnings = check_source_warnings(src);
    let has_w005 = warnings
        .iter()
        .any(|w| w.code.as_deref() == Some(crate::diagnostic::codes::W005));
    assert!(
        has_w005,
        "expected W005 warning for push on shared list from multiple steps, got: {:?}",
        warnings
    );
}

#[test]
fn warn_no_shared_write_write_parasteps() {
    // Two steps writing to different shared vars → no W005
    let src = r#"
func main() -> i32 {
    shared x = 0
    shared y = 0
    parasteps {
        *x = 1
        *y = 2
    }
    0
}
"#;
    let warnings = check_source_warnings(src);
    let has_w005 = warnings
        .iter()
        .any(|w| w.code.as_deref() == Some(crate::diagnostic::codes::W005));
    assert!(
        !has_w005,
        "expected no W005 for different shared vars, got: {:?}",
        warnings
    );
}

#[test]
fn warn_no_shared_write_single_step_parasteps() {
    // Single step writes to shared var → no W005
    let src = r#"
func main() -> i32 {
    shared x = 0
    parasteps {
        *x = 1
    }
    0
}
"#;
    let warnings = check_source_warnings(src);
    let has_w005 = warnings
        .iter()
        .any(|w| w.code.as_deref() == Some(crate::diagnostic::codes::W005));
    assert!(
        !has_w005,
        "expected no W005 for single step, got: {:?}",
        warnings
    );
}

#[test]
fn typecheck_parasteps_requires_local_shared_rejected() {
    // requires inside parasteps referencing local_shared → E0305
    let src = r#"
func main() -> i32 {
    local_shared x = 42
    parasteps {
        requires: *x > 0
        0
    }
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "expected E0305 for local_shared in parasteps requires"
    );
}

#[test]
fn typecheck_parasteps_ensures_local_shared_rejected() {
    // ensures inside parasteps referencing local_shared → E0305
    let src = r#"
func main() -> i32 {
    local_shared x = 42
    parasteps {
        ensures: *x > 0
        0
    }
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "expected E0305 for local_shared in parasteps ensures"
    );
}

#[test]
fn typecheck_arena_escape_ref_to_outer_rejected() {
    // Assigning arena-scoped ref to outer-scope ref → error
    let src = r#"
func main() -> i32 {
    let mut x: &i32 = &0;
    arena {
        let ref y = 42;
        x = y;
    }
    *x
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "expected error for arena escape via ref-to-ref assign"
    );
    let errors = result.unwrap_err();
    let has_escape_error = errors.iter().any(|e| e.message.contains("arena"));
    assert!(
        has_escape_error,
        "expected arena escape error, got: {:?}",
        errors
    );
}

#[test]
fn typecheck_arena_no_escape_value_copy_ok() {
    // Copying value out of arena (via deref) is fine → no error
    let src = r#"
func main() -> i32 {
    arena {
        let ref x = 42;
        *x
    }
}
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "expected no error for copying value out of arena, got: {:?}",
        result
    );
}

#[test]
fn typecheck_arena_ref_stays_in_scope_ok() {
    // Using arena ref within arena scope is fine → no error
    let src = r#"
func main() -> i32 {
    arena {
        let ref x = 42;
        let ref y = 10;
        println(*x + *y);
    }
    0
}
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "expected no error for ref use within arena, got: {:?}",
        result
    );
}

#[allow(dead_code)]
fn warn_no_shared_no_parasteps_write() {
    // No shared vars in parasteps → no W005
    let src = r#"
func main() -> i32 {
    let x = 0
    parasteps {
        x = 1
    }
    0
}
"#;
    let warnings = check_source_warnings(src);
    let has_w005 = warnings
        .iter()
        .any(|w| w.code.as_deref() == Some(crate::diagnostic::codes::W005));
    assert!(
        !has_w005,
        "expected no W005 for non-shared vars, got: {:?}",
        warnings
    );
}

// ─── Regex builtin type check tests (L2) ──────────────────────

#[test]
fn typecheck_regex_match_wrong_args() {
    check_source(
        r#"
func main() -> bool {
    regex_match("hello")
}
"#,
    )
    .expect_err("regex_match with 1 arg should fail typecheck");
}

#[test]
fn typecheck_regex_find_return_string() {
    let src = r#"
func main() -> string {
    regex_find("hello", "[a-z]+")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("hello".to_string()));
}

#[test]
fn typecheck_regex_replace_wrong_args() {
    check_source(
        r#"
func main() -> string {
    regex_replace("hello", "pattern")
}
"#,
    )
    .expect_err("regex_replace with 2 args should fail typecheck");
}

// ─── Generic bounds type check tests (L2) ─────────────────────

#[test]
fn typecheck_generic_bounds_clone_ok() {
    check_source(
        r#"
func clone_it<T: Clone>(x: T) -> T { x }
func main() -> i32 {
    let r = clone_it(42);
    r
}
"#,
    )
    .expect("i32 should satisfy Clone bound");
}

#[test]
fn typecheck_generic_bounds_default_ok() {
    check_source(
        r#"
func default_it<T: Default>(x: T) -> T { x }
func main() -> i32 {
    default_it(42)
}
"#,
    )
    .expect("i32 should satisfy Default bound");
}

#[test]
fn typecheck_generic_bounds_copy_ok() {
    check_source(
        r#"
func copy_it<T: Copy>(x: T) -> T { x }
func main() -> i32 {
    copy_it(42)
}
"#,
    )
    .expect("i32 should satisfy Copy bound");
}

#[test]
fn typecheck_generic_bounds_copy_rejected_for_string() {
    check_source(
        r#"
func copy_it<T: Copy>(x: T) -> T { x }
func main() -> string {
    copy_it("hello")
}
"#,
    )
    .expect_err("string should NOT satisfy Copy bound");
}

#[test]
fn typecheck_generic_bounds_eq_ok() {
    check_source(
        r#"
func eq_it<T: Eq>(x: T) -> T { x }
func main() -> i32 {
    eq_it(42)
}
"#,
    )
    .expect("i32 should satisfy Eq bound");
}

#[test]
fn typecheck_generic_bounds_turbofish_ok() {
    check_source(
        r#"
func clone_it<T: Clone>(x: T) -> T { x }
func main() -> i32 {
    clone_it::<i32>(42)
}
"#,
    )
    .expect("turbofish with i32 should satisfy Clone bound");
}

#[test]
fn typecheck_generic_bounds_multiple_ok() {
    check_source(
        r#"
func process<T: Clone + Default>(x: T) -> T { x }
func main() -> i32 {
    process(42)
}
"#,
    )
    .expect("i32 should satisfy Clone+Default bounds");
}

#[test]
fn typecheck_generic_bounds_multi_param_ok() {
    check_source(
        r#"
func pair<T: Copy, U: Default>(a: T, b: U) -> T { a }
func main() -> i32 {
    pair(1, 2)
}
"#,
    )
    .expect("i32 should satisfy Copy, i32 should satisfy Default");
}

// ─── v0.25 CK bug fix tests ──────────────────────────────────────────

#[test]
fn ck9_loop_returns_on_all_paths() {
    // CK9: loop with return in body should satisfy return-on-all-paths
    check_source(
        r#"
func main() -> i32 {
    loop {
        return 42
    }
}
"#,
    )
    .expect("loop with return should satisfy all-paths-return");
}

#[test]
fn ck9_while_returns_on_all_paths() {
    check_source(
        r#"
func main() -> i32 {
    let mut i = 0;
    while i < 10 {
        i = i + 1;
        return i
    }
    0
}
"#,
    )
    .expect("while with return should satisfy all-paths-return");
}

#[test]
fn ck9_for_returns_on_all_paths() {
    check_source(
        r#"
func main() -> i32 {
    for x in [1, 2, 3] {
        return x
    }
    0
}
"#,
    )
    .expect("for with return should satisfy all-paths-return");
}

#[test]
fn ck6_list_pattern_element_type_check() {
    // CK6: [1, "hi"] should fail for List<i32>
    let src = r#"
func check(xs: List<i32>) -> i32 {
    match xs {
        [1, "hi"] => 1
        _ => 0
    }
}
func main() -> i32 { check([1, 2]) }
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(
        result.is_err(),
        "List<i32> pattern with string element should fail"
    );
}

#[test]
fn ck5_tuple_pattern_dual_representation() {
    // CK5: tuple pattern should work with both Type::Tuple and Type::Name("Tuple")
    check_source(
        r#"
func main() -> i32 {
    let t = (1, 2);
    match t {
        (a, b) => a + b
    }
}
"#,
    )
    .expect("tuple pattern should work");
}

#[test]
fn ck3_constructor_shadow_warning() {
    // CK3: variant constructor shadowing a function should produce an error
    let src = r#"
type MyBool { True False }
func True() -> i32 { 1 }
func main() -> i32 { 1 }
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(
        result.is_err(),
        "Constructor shadowing function should produce error"
    );
}

#[test]
fn ck8_user_defined_none_not_shadowed() {
    // CK8: user-defined None variant should not be shadowed by built-in
    check_source(
        r#"
type MyOption { Some(i32) None }
func unwrap(x: MyOption) -> i32 {
    match x {
        Some(v) => v
        None => -1
    }
}
func main() -> i32 { unwrap(Some(99)) }
"#,
    )
    .expect("user-defined None should work");
}

#[test]
fn ck7_actor_methods_isolated() {
    // CK7: different actors can have same method names
    check_source(
        r#"
actor Foo {
    func handle() -> i32 { 1 }
}
actor Bar {
    func handle() -> i32 { 2 }
}
func main() -> i32 { 1 }
"#,
    )
    .expect("different actors with same method name should not conflict");
}

// ─── v0.25 D4: Newtype .0 unwrap ─────────────────────────────────────

#[test]
fn d4_newtype_dot0_typecheck() {
    check_source(
        r#"
newtype UserId = i32
func get_id(u: UserId) -> i32 { u.0 }
func main() -> i32 { get_id(UserId(42)) }
"#,
    )
    .expect("newtype .0 should typecheck");
}

// ─── v0.25.1 D3: Exhaustiveness check improvements ──────────────────

#[test]
fn d3_int_match_with_catchall_ok() {
    check_source(
        r#"
func classify(x: i32) -> i32 {
    match x {
        0 => 1
        1 => 2
        _ => 3
    }
}
func main() -> i32 { classify(5) }
"#,
    )
    .expect("int match with catch-all should pass");
}

#[test]
fn d3_int_match_without_catchall_warns() {
    let src = r#"
func classify(x: i32) -> i32 {
    match x {
        0 => 1
        1 => 2
    }
}
func main() -> i32 { classify(5) }
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(result.is_err(), "int match without catch-all should warn");
}

#[test]
fn d3_string_match_with_catchall_ok() {
    check_source(
        r#"
func classify(s: string) -> i32 {
    match s {
        "hello" => 1
        _ => 0
    }
}
func main() -> i32 { classify("world") }
"#,
    )
    .expect("string match with catch-all should pass");
}

// ─── v0.25.2: Missing CK tests ─────────────────────────────────────

#[test]
fn ck1_constructor_scoped_to_subject() {
    // CK1: constructor pattern resolved against subject type
    // When subject type has the variant, it uses the scoped lookup
    check_source(
        r#"
type Color { Red Green Blue }
type TrafficLight { Stop Go Caution }
func pick(c: Color) -> i32 {
    match c {
        Red => 1
        _ => 0
    }
}
func main() -> i32 { pick(Red) }
"#,
    )
    .expect("constructor scoped to subject type should pass");
}

#[test]
fn ck2_generic_enum_self_ty_includes_args() {
    // CK2: generic enum self_ty should include type parameter args
    // Full generic constructor substitution requires C2 (unification engine)
    check_source(
        r#"
type Wrapper<T> { Wrap(T) }
func main() -> i32 { 1 }
"#,
    )
    .expect("generic enum definition should pass typecheck");
}

#[test]
fn ck4_alias_cycle_transitive() {
    // CK4: alias cycle through nested types should be detected
    let src = r#"
type A = B
type B = A
func main() -> i32 { 1 }
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(result.is_err(), "transitive alias cycle should be detected");
}

// ─── v0.26: Unification + Bidirectional + ForAll tests ─────────────

#[test]
fn v026_unify_let_binding_i32() {
    // C2: basic unification through let binding
    check_source(
        r#"
func main() -> i32 {
    let x: i32 = 42;
    x
}
"#,
    )
    .expect("i32 let binding should unify");
}

#[test]
fn v026_unify_let_binding_generic_func() {
    // C2: unification through generic function call
    check_source(
        r#"
func identity<T>(x: T) -> T { x }
func main() -> i32 {
    identity(42)
}
"#,
    )
    .expect("generic function call should unify");
}

#[test]
fn v026_bidirectional_none_in_option_context() {
    // C3: None in Option<i32> context
    check_source(
        r#"
func main() -> i32 {
    let x: Option<i32> = None;
    0
}
"#,
    )
    .expect("None in Option context should be accepted");
}

#[test]
fn v026_bidirectional_return_type() {
    // C3: return type propagation
    check_source(
        r#"
func get_value() -> Option<i32> {
    None
}
func main() -> i32 { 0 }
"#,
    )
    .expect("None as Option return should work");
}

#[test]
fn v026_unify_nested_option() {
    // C2: nested Option unification
    check_source(
        r#"
func main() -> i32 {
    let x: Option<Option<i32>> = Some(Some(42));
    0
}
"#,
    )
    .expect("nested Option should unify");
}

#[test]
fn v026_newtype_transparent() {
    // Bug 5: newtype should be transparent — implicit wrap/unwrap with inner type
    let src = r#"
newtype UserId = i32
func main() -> i32 {
    let id: UserId = 42;
    let x: i32 = id;
    x
}
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(
        result.is_ok(),
        "newtype should transparently unify with inner type: {:?}",
        result
    );
}

#[test]
fn newtype_transparent_fn_return_wrap() {
    // Implicit wrap: func returns MyId, body returns bare i32 literal
    let src = r#"
newtype MyId = i32
func make() -> MyId { 42 }
func main() -> i32 { 0 }
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(
        result.is_ok(),
        "newtype implicit wrap on return: {:?}",
        result
    );
}

#[test]
fn newtype_transparent_fn_return_unwrap() {
    // Implicit unwrap: func returns i32, body returns MyId value
    let src = r#"
newtype MyId = i32
func make_id() -> MyId { 42 }
func unwrap() -> i32 { make_id() }
func main() -> i32 { 0 }
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(
        result.is_ok(),
        "newtype implicit unwrap on return: {:?}",
        result
    );
}

#[test]
fn newtype_transparent_fn_arg_wrap() {
    // Implicit wrap: func expects MyId, called with i32
    let src = r#"
newtype MyId = i32
func apply(x: MyId) -> i32 { x.0 }
func main() -> i32 { apply(42) }
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(
        result.is_ok(),
        "newtype implicit wrap on fn arg: {:?}",
        result
    );
}

#[test]
fn newtype_transparent_fn_arg_unwrap() {
    // Implicit unwrap: func expects i32, called with MyId
    let src = r#"
newtype MyId = i32
func make() -> MyId { 42 }
func apply(x: i32) -> i32 { x }
func main() -> i32 { apply(make()) }
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(
        result.is_ok(),
        "newtype implicit unwrap on fn arg: {:?}",
        result
    );
}

#[test]
fn newtype_transparent_assign_wrap() {
    // Implicit wrap: assign i32 to MyId variable
    let src = r#"
newtype MyId = i32
func main() -> i32 {
    let mut x: MyId = 0;
    x = 42;
    0
}
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(
        result.is_ok(),
        "newtype implicit wrap on assign: {:?}",
        result
    );
}

#[test]
fn newtype_transparent_assign_unwrap() {
    // Implicit unwrap: assign MyId to i32 variable
    let src = r#"
newtype MyId = i32
func main() -> i32 {
    let id: MyId = 42;
    let mut x: i32 = 0;
    x = id;
    0
}
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(
        result.is_ok(),
        "newtype implicit unwrap on assign: {:?}",
        result
    );
}

#[test]
fn newtype_transparent_if_branches() {
    // Implicit wrap in if/else branches
    let src = r#"
newtype MyId = i32
func pick(c: bool) -> MyId {
    if c { 42 } else { 43 }
}
func main() -> i32 { 0 }
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(
        result.is_ok(),
        "newtype implicit wrap in if/else: {:?}",
        result
    );
}

#[test]
fn newtype_transparent_cross_rejected() {
    // Cross-newtype should still be rejected
    let src = r#"
newtype UserId = i32
newtype OrderId = i32
func apply(u: UserId) -> i32 { u.0 }
func main() -> i32 { apply(OrderId(1)) }
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(
        result.is_err(),
        "cross-newtype should be rejected: {:?}",
        result
    );
}

#[test]
fn newtype_transparent_assign_cross_rejected() {
    // Cross-newtype assignment should be rejected
    let src = r#"
newtype UserId = i32
newtype OrderId = i32
func main() -> i32 {
    let mut u: UserId = 0;
    u = OrderId(1);
    0
}
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(
        result.is_err(),
        "cross-newtype assignment should be rejected: {:?}",
        result
    );
}

// ─── v0.25.5: Bug 6 + Bug 7 tests ──────────────────────────────────

#[test]
fn v0255_bug7_substitute_tuple_inner() {
    // Bug 7: substitute_type_vars must recurse into Tuple elements
    // Tuple<T, U> was already handled but verify it still works after
    // the full variant coverage fix (Array/Slice/Shared/Ref/etc.)
    check_source(
        r#"
func swap<T, U>(p: (T, U)) -> (U, T) { (p.1, p.0) }
func main() -> i32 {
    let p = swap((1, 2));
    p.0
}
"#,
    )
    .expect("Tuple<T, U> elements should be substituted during instantiation");
}
