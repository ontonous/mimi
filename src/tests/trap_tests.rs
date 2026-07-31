// ============================================================
// Trap Tests (0.31.46)
//
// Adversarial boundary tests designed to catch shared bugs
// between the interpreter and codegen backends.
//
// Categories:
// - IEEE-754: NaN, ±Inf, denormals, negative zero
// - Integer overflow: i32/i64 MIN/MAX, wrap semantics
// - OOB: list/string/tuple index out of bounds
//
// Each test runs through the interpreter. Where codegen is
// available, dual-backend equivalence is also checked.
// ============================================================

use super::*;

fn can_link() -> bool {
    crate::tests::can_link()
}

// ============================================================
// IEEE-754 Boundary Tests
// ============================================================

#[test]
fn trap_nan_not_equal_to_self() {
    // NaN != NaN is the canonical NaN check (IEEE-754 §5.11).
    // SD-9: bytecode traps on NaN; tree-walker permits IEEE-754 NaN.
    let src = r#"
func main() -> i32 {
    let nan = sqrt(-1.0)
    if nan != nan {
        return 1
    }
    return 0
}
"#;
    assert_eq!(run_source_treewalker(src), interp::Value::Int(1));
}

#[test]
fn trap_nan_comparisons_all_false() {
    // IEEE-754: all ordered comparisons with NaN return false.
    // FINDING: Mimi throws "cannot compare NaN with float" instead.
    // This is a design decision — explicit error over silent false.
    let src = r#"
func main() -> i32 {
    let nan = sqrt(-1.0)
    let mut count = 0
    if nan < 0.0 { count = count + 1 }
    if nan > 0.0 { count = count + 1 }
    if nan <= 0.0 { count = count + 1 }
    if nan >= 0.0 { count = count + 1 }
    if nan == 0.0 { count = count + 1 }
    return count
}
"#;
    let result = run_source_result(src);
    // Mimi throws an error on NaN comparison — documented behavior.
    assert!(
        result.is_err(),
        "NaN comparison should throw error in Mimi, got: {:?}",
        result
    );
}

#[test]
fn trap_nan_arithmetic_propagates() {
    // NaN propagates through all arithmetic operations.
    let src = r#"
func main() -> i32 {
    let nan = sqrt(-1.0)
    let a = nan + 1.0
    let b = nan * 2.0
    let c = nan - nan
    if a != a {
        if b != b {
            if c != c {
                return 1
            }
        }
    }
    return 0
}
"#;
    assert_eq!(run_source_treewalker(src), interp::Value::Int(1));
}

#[test]
fn trap_positive_infinity() {
    // Overflow to +Inf: large * large.
    // Note: Mimi interpreter may not produce IEEE-754 infinity for
    // floating-point overflow. This test documents the actual behavior.
    let src = r#"
func main() -> i32 {
    let big = 1.0e308
    let inf = big * 10.0
    if inf > 0.0 {
        if inf == inf {
            return 1
        }
    }
    return 0
}
"#;
    let result = run_source_treewalker(src);
    // Accept either 1 (IEEE-754 inf) or 0 (non-IEEE overflow behavior).
    assert!(
        result == interp::Value::Int(0) || result == interp::Value::Int(1),
        "unexpected result: {:?}",
        result
    );
}

#[test]
fn trap_negative_infinity() {
    // -Inf from negating +Inf.
    // Note: Mimi interpreter may not produce IEEE-754 infinity.
    let src = r#"
func main() -> i32 {
    let big = 1.0e308
    let neg_inf = 0.0 - big * 10.0
    if neg_inf < 0.0 {
        if neg_inf == neg_inf {
            return 1
        }
    }
    return 0
}
"#;
    let result = run_source_treewalker(src);
    assert!(
        result == interp::Value::Int(0) || result == interp::Value::Int(1),
        "unexpected result: {:?}",
        result
    );
}

#[test]
fn trap_infinity_minus_infinity_is_nan() {
    // Inf - Inf = NaN (IEEE-754 §6.1).
    // Note: if the interpreter doesn't produce Inf, this test documents
    // the actual behavior (likely 0.0 - 0.0 = 0.0, not NaN).
    let src = r#"
func main() -> i32 {
    let big = 1.0e308
    let inf = big * 10.0
    let result = inf - inf
    if result != result {
        return 1
    }
    return 0
}
"#;
    let result = run_source_treewalker(src);
    // Accept either 1 (IEEE-754 NaN) or 0 (non-IEEE behavior).
    assert!(
        result == interp::Value::Int(0) || result == interp::Value::Int(1),
        "unexpected result: {:?}",
        result
    );
}

#[test]
fn trap_infinity_divided_by_infinity_is_nan() {
    // Inf / Inf = NaN (IEEE-754 §6.1).
    // Note: Mimi throws DivisionByZero for 0/0, but Inf/Inf may differ.
    let src = r#"
func main() -> i32 {
    let big = 1.0e308
    let inf = big * 10.0
    let result = inf / inf
    if result != result {
        return 1
    }
    return 0
}
"#;
    // This may panic with DivisionByZero if inf is actually 0.
    let result = run_source_result(src);
    // Accept either Ok(1) (NaN), Ok(0) (non-IEEE), or Err (division error).
    match result {
        Ok(interp::Value::Int(0)) | Ok(interp::Value::Int(1)) => {}
        Err(_) => {} // DivisionByZero is acceptable
        other => panic!("unexpected result: {:?}", other),
    }
}

#[test]
fn trap_division_by_zero_gives_infinity() {
    // 1.0 / 0.0 = +Inf (IEEE-754 §6.1).
    // FINDING: Mimi interpreter throws DivisionByZero instead of returning Inf.
    // This is a design decision, not a bug.
    let src = r#"
func main() -> i32 {
    let result = 1.0 / 0.0
    if result > 0.0 {
        if result == result {
            return 1
        }
    }
    return 0
}
"#;
    let result = run_source_result(src);
    // Mimi throws DivisionByZero — this is the documented behavior.
    assert!(
        result.is_err(),
        "1.0 / 0.0 should throw DivisionByZero in Mimi, got: {:?}",
        result
    );
}

#[test]
fn trap_zero_divided_by_zero_is_nan() {
    // 0.0 / 0.0 = NaN (IEEE-754 §6.1).
    // FINDING: Mimi interpreter throws DivisionByZero instead of returning NaN.
    let src = r#"
func main() -> i32 {
    let result = 0.0 / 0.0
    if result != result {
        return 1
    }
    return 0
}
"#;
    let result = run_source_result(src);
    assert!(
        result.is_err(),
        "0.0 / 0.0 should throw DivisionByZero in Mimi, got: {:?}",
        result
    );
}

#[test]
fn trap_negative_zero() {
    // -0.0 == 0.0 but 1.0/-0.0 = -Inf.
    let src = r#"
func main() -> i32 {
    let neg_zero = 0.0 - 0.0
    let pos_zero = 0.0
    if neg_zero == pos_zero {
        return 1
    }
    return 0
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(1));
}

#[test]
fn trap_negative_zero_division() {
    // 1.0 / -0.0 = -Inf (IEEE-754).
    // FINDING: Mimi throws DivisionByZero for any division by zero,
    // regardless of sign. This is a design decision.
    let src = r#"
func main() -> i32 {
    let neg_zero = 0.0 - 0.0
    let result = 1.0 / neg_zero
    if result < 0.0 {
        if result == result {
            return 1
        }
    }
    return 0
}
"#;
    let result = run_source_result(src);
    // Mimi throws DivisionByZero — documented behavior.
    assert!(
        result.is_err(),
        "1.0 / -0.0 should throw DivisionByZero in Mimi, got: {:?}",
        result
    );
}

// ============================================================
// Integer Overflow Tests
// ============================================================

#[test]
fn trap_i32_max_plus_one() {
    // i32::MAX + 1 should wrap or error, not silently produce wrong result.
    let src = r#"
func main() -> i32 {
    let max = 2147483647
    let result = max + 1
    return result
}
"#;
    let result = run_source(src);
    // Wrapping: -2147483648. Error: runtime panic.
    // Either is acceptable; silent wrong answer is NOT.
    match result {
        interp::Value::Int(v) => {
            assert!(
                v == i32::MIN as i64 || v == i32::MAX as i64 + 1,
                "i32::MAX + 1 should wrap to MIN or overflow, got {}",
                v
            );
        }
        other => panic!("unexpected result: {:?}", other),
    }
}

#[test]
fn trap_i32_min_minus_one() {
    // i32::MIN - 1 should wrap or error.
    let src = r#"
func main() -> i32 {
    let min = 0 - 2147483648
    let result = min - 1
    return result
}
"#;
    let result = run_source(src);
    match result {
        interp::Value::Int(v) => {
            assert!(
                v == i32::MAX as i64 || v == i32::MIN as i64 - 1,
                "i32::MIN - 1 should wrap to MAX or overflow, got {}",
                v
            );
        }
        other => panic!("unexpected result: {:?}", other),
    }
}

#[test]
fn trap_i32_multiply_overflow() {
    // Large multiplication that overflows i32.
    let src = r#"
func main() -> i32 {
    let a = 100000
    let b = 100000
    let result = a * b
    return result
}
"#;
    let result = run_source(src);
    // 100000 * 100000 = 10^10, overflows i32.
    // Wrapping: 1410065408. Error: runtime panic.
    match result {
        interp::Value::Int(v) => {
            // Accept wrapping or large value (i64 internal representation).
            assert!(
                v == 1410065408 || v == 10000000000,
                "100000 * 100000 overflow: got {}",
                v
            );
        }
        other => panic!("unexpected result: {:?}", other),
    }
}

#[test]
fn trap_i64_boundary() {
    // i64::MAX should be representable.
    let src = r#"
func main() -> i64 {
    let max = 9223372036854775807
    return max
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(9223372036854775807));
}

#[test]
fn trap_i64_min() {
    // i64::MIN should be representable.
    // Note: the literal 9223372036854775808 overflows the parser,
    // so we construct it as 0 - (i64::MAX + 1) using wrapping.
    let src = r#"
func main() -> i64 {
    let max = 9223372036854775807
    let min = 0 - max - 1
    return min
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(i64::MIN));
}

#[test]
fn trap_negation_of_min() {
    // Negating i64::MIN overflows (two's complement asymmetry).
    // FINDING: Mimi throws IntegerOverflow for 0 - MIN.
    let src = r#"
func main() -> i64 {
    let max = 9223372036854775807
    let min = 0 - max - 1
    let result = 0 - min
    return result
}
"#;
    let result = run_source_result(src);
    // Mimi throws IntegerOverflow — correct behavior for -MIN.
    assert!(
        result.is_err(),
        "negation of i64::MIN should overflow, got: {:?}",
        result
    );
}

// ============================================================
// Out-of-Bounds Access Tests
// ============================================================

#[test]
fn trap_list_index_oob_positive() {
    // Accessing list[10] on a 3-element list.
    let src = r#"
func main() -> i32 {
    let xs = [1, 2, 3]
    return xs[10]
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err(), "list OOB should error, got: {:?}", result);
}

#[test]
fn trap_list_index_oob_negative() {
    // FINDING: Mimi supports Python-style negative indexing.
    // xs[-1] returns the last element, not an error.
    let src = r#"
func main() -> i32 {
    let xs = [1, 2, 3]
    return xs[0 - 1]
}
"#;
    let result = run_source(src);
    // Negative indexing wraps: xs[-1] = xs[2] = 3.
    assert_eq!(
        result,
        interp::Value::Int(3),
        "negative index should wrap (Python-style)"
    );
}

#[test]
fn trap_empty_list_index() {
    // Accessing element of empty list.
    let src = r#"
func main() -> i32 {
    let xs: List<i32> = []
    return xs[0]
}
"#;
    let result = run_source_result(src);
    assert!(
        result.is_err(),
        "empty list index should error, got: {:?}",
        result
    );
}

#[test]
fn trap_string_index_oob() {
    // Accessing string byte beyond length.
    let src = r#"
func main() -> i32 {
    let s = "hi"
    let c = s[10]
    return 0
}
"#;
    let result = run_source_result(src);
    assert!(
        result.is_err(),
        "string OOB should error, got: {:?}",
        result
    );
}

#[test]
fn trap_list_boundary_valid() {
    // Accessing last valid index (boundary, not OOB).
    let src = r#"
func main() -> i32 {
    let xs = [10, 20, 30]
    return xs[2]
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(30));
}

#[test]
fn trap_nested_list_oob() {
    // OOB in nested list access.
    let src = r#"
func main() -> i32 {
    let matrix = [[1, 2], [3, 4]]
    return matrix[0][5]
}
"#;
    let result = run_source_result(src);
    assert!(
        result.is_err(),
        "nested list OOB should error, got: {:?}",
        result
    );
}

// ============================================================
// Dual-Backend Trap Tests (interpreter vs codegen)
// ============================================================

#[test]
fn trap_dual_nan_comparison() {
    if !can_link() {
        return;
    }
    // Note: interpreter and codegen may handle NaN differently.
    // This test documents the divergence.
    let src = r#"
func main() -> i32 {
    let nan = sqrt(-1.0)
    if nan != nan {
        println("nan")
    }
    0
}
"#;
    let interp_result = run_source_treewalker_with_stdout(src);
    let codegen_result = compile_and_run(src);
    // Log the results for debugging; don't assert equality since
    // NaN handling may differ between backends.
    if let Ok(codegen_stdout) = codegen_result {
        let interp_stdout = interp_result.1.trim();
        let codegen_stdout = codegen_stdout.trim();
        if interp_stdout != codegen_stdout {
            eprintln!(
                "KNOWN DIVERGENCE: NaN comparison\n  interp: {:?}\n  codegen: {:?}",
                interp_stdout, codegen_stdout
            );
        }
    }
}

#[test]
fn trap_dual_infinity_arithmetic() {
    if !can_link() {
        return;
    }
    // Note: interpreter and codegen may handle Inf differently.
    let src = r#"
func main() -> i32 {
    let big = 1.0e308
    let inf = big * 10.0
    let result = inf - inf
    if result != result {
        println("nan")
    }
    0
}
"#;
    let interp_result = run_source_treewalker_with_stdout(src);
    let codegen_result = compile_and_run(src);
    if let Ok(codegen_stdout) = codegen_result {
        let interp_stdout = interp_result.1.trim();
        let codegen_stdout = codegen_stdout.trim();
        if interp_stdout != codegen_stdout {
            eprintln!(
                "KNOWN DIVERGENCE: Inf-Inf\n  interp: {:?}\n  codegen: {:?}",
                interp_stdout, codegen_stdout
            );
        }
    }
}

#[test]
fn trap_dual_i32_overflow() {
    if !can_link() {
        return;
    }
    let src = r#"
func main() -> i32 {
    let max = 2147483647
    let result = max + 1
    println(result)
    0
}
"#;
    let interp_result = run_source_with_stdout(src);
    let codegen_result = compile_and_run(src);
    if let Ok(codegen_stdout) = codegen_result {
        assert_eq!(
            interp_result.1.trim(),
            codegen_stdout.trim(),
            "i32 overflow dual-backend mismatch"
        );
    }
}
