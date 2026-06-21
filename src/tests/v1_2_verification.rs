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
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let file = crate::parser::Parser::new(tokens).parse_file().unwrap();
    let mut interp = crate::interp::Interpreter::new(&file);
    interp.verify_contracts = false;
    let result = interp.run();
    assert!(result.is_ok(), "without verify_contracts, requires should be ignored");

    // With verify_contracts, requires is enforced
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let file = crate::parser::Parser::new(tokens).parse_file().unwrap();
    let mut interp = crate::interp::Interpreter::new(&file);
    interp.verify_contracts = true;
    let result = interp.run();
    assert!(result.is_err(), "with verify_contracts, requires violation should error");
    let err = result.unwrap_err();
    assert!(err.message.contains("requires condition failed"), "Expected requires error, got: {}", err.message);
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
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let file = crate::parser::Parser::new(tokens).parse_file().unwrap();
    let mut interp = crate::interp::Interpreter::new(&file);
    interp.verify_contracts = false;
    let result = interp.run();
    assert!(result.is_ok(), "without verify_contracts, ensures should be ignored");

    // With verify_contracts, ensures is enforced
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let file = crate::parser::Parser::new(tokens).parse_file().unwrap();
    let mut interp = crate::interp::Interpreter::new(&file);
    interp.verify_contracts = true;
    let result = interp.run();
    assert!(result.is_err(), "with verify_contracts, ensures violation should error");
    let err = result.unwrap_err();
    assert!(err.message.contains("ensures condition failed"), "Expected ensures error, got: {}", err.message);
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
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let file = crate::parser::Parser::new(tokens).parse_file().unwrap();
    let mut interp = crate::interp::Interpreter::new(&file);
    interp.verify_contracts = true;
    let result = interp.run();
    assert!(result.is_ok(), "valid contracts should pass with verify_contracts");
    assert_eq!(result.unwrap(), crate::interp::Value::Int(3));
}

// ============================================================
// T601: Z3 形式化验证
// ============================================================

fn z3_available() -> bool {
    crate::verifier::is_z3_available()
}

fn verify_source(source: &str) -> Vec<crate::verifier::VerificationResult> {
    crate::verifier::verify_source(source).unwrap()
}

fn assert_verified(source: &str) {
    if !z3_available() {
        eprintln!("    └─ skipped (Z3 not available)");
        return;
    }
    let results = verify_source(source);
    for r in &results {
        assert_eq!(r.status, crate::verifier::VerifStatus::Verified, "{}: {}", r.func_name, r.message);
    }
}

fn assert_failed(source: &str) {
    if !z3_available() {
        eprintln!("    └─ skipped (Z3 not available)");
        return;
    }
    let results = verify_source(source);
    assert!(results.iter().any(|r| r.status == crate::verifier::VerifStatus::Failed),
        "expected at least one Failed result, got: {:?}", results.iter().map(|r| (&r.func_name, &r.status)).collect::<Vec<_>>());
}

fn assert_unknown(source: &str) {
    let results = verify_source(source);
    assert!(results.iter().all(|r| r.status == crate::verifier::VerifStatus::Unknown),
        "expected all Unknown results, got: {:?}", results.iter().map(|r| (&r.func_name, &r.status)).collect::<Vec<_>>());
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
    mms { "requires: x > 0" }
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
    mms { "requires: x == 5" }
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
    mms { "requires: x > 0\nensures: x > 0" }
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
    mms { "requires: x == 1\nensures: x == 2" }
    x
}
"#;
    assert_failed(src);
}

#[test]
fn verify_requires_and_ensures() {
    let src = r#"
func identity(x: i32) -> i32 {
    mms { "requires: x >= 0\nensures: x >= 0" }
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
    mms { "requires: x == 3\nrequires: y == 4\nmath: { x * y == 12 }" }
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
    mms { "requires: x == 5\nrequires: y == 10\nensures: x <= 10" }
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
    mms { "requires: not(x == 0)\nensures: not(x == 0)" }
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
    mms { "requires: x > 0 and x < 100\nensures: x > 0" }
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
    mms { "requires: x == 1 or x == 2\nensures: x >= 1" }
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
    mms { "requires: x != 0\nensures: x != 0" }
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
    mms { "requires: x >= 0\nensures: x >= 0" }
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
    mms { "requires: x == 1\nensures: x == 1" }
    x
}

func f2(x: i32) -> i32 {
    mms { "requires: x == 2\nensures: x == 2" }
    x
}
"#;
    let results = verify_source(src);
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.status == crate::verifier::VerifStatus::Verified));
}

#[test]
fn verify_subtraction() {
    let src = r#"
func sub(x: i32, y: i32) -> i32 {
    mms { "requires: x == 10\nrequires: y == 3\nensures: x - y == 7" }
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
    mms { "requires: x == 12\nrequires: y == 4\nensures: x / y == 3" }
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
    mms { "requires: x == 10\nrequires: y == 3\nensures: x % y == 1" }
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
    mms { "requires: x == 5\nensures: x == 5" }
    0 - x
}
"#;
    let results = verify_source(src);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, crate::verifier::VerifStatus::Verified);
}

#[test]
fn verify_unsatisfiable_requires() {
    let src = r#"
func impossible(x: i32) -> i32 {
    mms { "requires: x > 0\nrequires: x < 0" }
    x
}
"#;
    assert_failed(src);
}

#[test]
fn verify_result_count() {
    let src = r#"
func f1(x: i32) -> i32 {
    mms { "requires: x == 1" }
    x
}

func f2(x: i32) -> i32 {
    mms { "requires: x == 2" }
    x
}

func f3(x: i32) -> i32 {
    mms { "requires: x == 3" }
    x
}
"#;
    let results = verify_source(src);
    assert_eq!(results.len(), 3);
}

#[test]
fn verify_module_nested() {
    let src = r#"
module Math {
    func identity(x: i32) -> i32 {
        mms { "requires: x >= 0\nensures: x >= 0" }
        x
    }
}
"#;
    let results = verify_source(src);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, crate::verifier::VerifStatus::Verified);
}

#[test]
fn verify_le_operator() {
    let src = r#"
func capped(x: i32) -> i32 {
    mms { "requires: x <= 100\nensures: x <= 100" }
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
    mms { "requires: x > 0\nensures: x > 0" }
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
    mms { "requires: x == 10\nensures: x == 20" }
    x
}
"#;
    assert_failed(src);
}
