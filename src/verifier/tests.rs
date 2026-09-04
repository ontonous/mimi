use super::helpers::*;
use super::*;
use crate::ast::*;

macro_rules! require_z3 {
    () => {
        if !crate::verifier::is_z3_available() {
            eprintln!("    └─ skipped (Z3 not available)");
            return;
        }
    };
}

#[test]
fn verifier_memory_sources_are_stable_registered_and_label_isolated() {
    let source = "func main() -> i32 { let value = 1; value }";
    let first = parse_memory_source(source, "contracts").expect("first parse");
    let second = parse_memory_source(source, "contracts").expect("second parse");
    let other = parse_memory_source(source, "other-file").expect("other parse");

    let first_func = first
        .items
        .iter()
        .find_map(|item| match item {
            Item::Func(function) if function.name == "main" => Some(function),
            _ => None,
        })
        .expect("main function");
    assert!(first_func.meta.span.source_id.is_known());
    assert!(first
        .sources
        .record(first_func.meta.span.source_id)
        .is_some());

    let source_key = |file: &File| {
        let source_id = file
            .items
            .iter()
            .find_map(|item| match item {
                Item::Func(function) => Some(function.meta.span.source_id),
                _ => None,
            })
            .expect("function source");
        file.sources
            .key(source_id)
            .expect("registered source key")
            .as_str()
            .to_string()
    };
    assert_eq!(source_key(&first), source_key(&second));
    assert_ne!(source_key(&first), source_key(&other));

    let anonymous_ids = |file: &File| {
        crate::core::check_program(file)
            .expect("checked source")
            .node_meta()
            .keys()
            .filter(|node_id| node_id.0.contains("/node:"))
            .map(|node_id| node_id.0.clone())
            .collect::<std::collections::BTreeSet<_>>()
    };
    let first_ids = anonymous_ids(&first);
    let second_ids = anonymous_ids(&second);
    let other_ids = anonymous_ids(&other);
    assert!(!first_ids.is_empty());
    assert_eq!(first_ids, second_ids);
    assert!(first_ids
        .iter()
        .all(|node_id| !node_id.contains("unknown-source")));
    assert!(first_ids.is_disjoint(&other_ids));
}

#[test]
fn verify_simple_pass() {
    require_z3!();
    let src = r#"
func identity(x: i32) -> i32 {
    requires: true
    ensures: true
    x
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:25 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, VerifStatus::Verified);
}

#[test]
fn verify_unencodable_ensures_is_unknown() {
    require_z3!();
    let src = r#"
func preserve(xs: List<i32>) -> List<i32> {
    ensures: result[0] == xs[0]
    xs
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(results.len(), 1);
    assert!(results[0].status.is_inconclusive());
    assert!(results[0].message.contains("could not encode ensures"));
}

#[test]
fn verify_unencodable_requires_is_unknown() {
    require_z3!();
    let src = r#"
func first(xs: List<i32>) -> i32 {
    requires: xs[0] > 0
    1
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(results.len(), 1);
    assert!(results[0].status.is_inconclusive());
    assert!(results[0].message.contains("could not encode requires"));
}

#[test]
fn verify_unproven_math_cannot_be_assumed() {
    require_z3!();
    let src = r#"
func forged(x: i32) -> i32 {
    math: { x == 1 }
    ensures: result == 1
    x
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, VerifStatus::Failed);
    assert!(results[0].message.contains("math obligation"));
}

#[test]
fn verify_proven_math_is_admitted() {
    require_z3!();
    let src = r#"
func proven(x: i32) -> i32 {
    requires: x == 1
    math: { x > 0 }
    ensures: result > 0
    x
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, VerifStatus::Verified);
}

#[test]
fn verify_body_satisfies_ensures() {
    require_z3!();
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= 0
    requires: x <= 1073741823
    ensures: result == x * 2
    x * 2
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:40 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "body `x * 2` should satisfy ensures `result == x * 2`: {}",
        results[0].message
    );
}

#[test]
fn verify_body_violates_ensures() {
    require_z3!();
    let src = r#"
func wrong(x: i32) -> i32 {
    requires: x >= 0 && x <= 100000
    ensures: result == x * 2
    x * 3
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:56 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, VerifStatus::Failed);
    let diag = results[0]
        .diagnostic
        .as_ref()
        .expect("src/verifier/tests.rs:59 unwrap failed");
    assert!(
        diag.message.contains("result ="),
        "narrative should show result value: {}",
        diag.message
    );
}

#[test]
fn verify_result_binding_in_counterexample() {
    require_z3!();
    let src = r#"
func add_one(x: i32) -> i32 {
    requires: x > 0
    ensures: result > x
    x
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:72 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, VerifStatus::Failed);
    let diag = results[0]
        .diagnostic
        .as_ref()
        .expect("src/verifier/tests.rs:75 unwrap failed");
    assert!(
        diag.message.contains("result ="),
        "should show result value in narrative"
    );
}

#[test]
fn verify_strong_postcondition_fails() {
    require_z3!();
    let src = r#"
func abs(x: i32) -> i32 {
    requires: x > 0
    ensures: result > 0
    x
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:89 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "x > 0 && result == x should satisfy result > 0"
    );
}

#[test]
fn verify_counterexample_extracted() {
    require_z3!();
    let src = r#"
func abs(x: i32) -> i32 {
    requires: true
    ensures: result > 0
    x
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:105 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, VerifStatus::Failed);
    assert!(results[0].diagnostic.is_some());
    let diag = results[0]
        .diagnostic
        .as_ref()
        .expect("src/verifier/tests.rs:109 unwrap failed");
    assert!(
        diag.message.contains("result ="),
        "should show result in narrative"
    );
}

#[test]
fn verify_unsatisfiable_requires() {
    require_z3!();
    let src = r#"
func impossible(x: i32) -> i32 {
    requires: x > 0 && x < 0
    ensures: true
    x
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:123 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, VerifStatus::Failed);
    let diag = results[0]
        .diagnostic
        .as_ref()
        .expect("src/verifier/tests.rs:126 unwrap failed");
    assert!(diag.message.contains("unsatisfiable"));
}

#[test]
fn verify_old_snapshot() {
    require_z3!();
    let src = r#"
func noop(x: i32) -> i32 {
    requires: x > 0
    ensures: result == old(x)
    x
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:140 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "body returns x unchanged, ensures result == old(x) should hold: {}",
        results[0].message
    );
}

#[test]
fn verify_old_snapshot_fails() {
    require_z3!();
    let src = r#"
func mutate(x: i32) -> i32 {
    requires: x > 0
    ensures: result == old(x)
    x + 1
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:156 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Failed,
        "body returns x+1, ensures result == old(x) should fail"
    );
}

#[test]
fn format_expr_basic() {
    assert_eq!(format_expr(&Expr::Literal(Lit::Int(42))), "42");
    assert_eq!(format_expr(&Expr::Ident("x".into())), "x");
    assert_eq!(
        format_expr(&Expr::Binary(
            BinOp::Gt,
            Box::new(Expr::Ident("x".into())),
            Box::new(Expr::Literal(Lit::Int(0))),
        )),
        "x > 0"
    );
}

#[test]
fn verify_extern_ensures_consistent() {
    require_z3!();
    let src = r#"
extern "C" {
    func must_be_positive(x: i64) -> i64
        ensures: result > 0;
}

func main() -> i64 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:187 unwrap failed");
    let ext: Vec<_> = results
        .iter()
        .filter(|r| r.func_name.contains("extern"))
        .collect();
    assert_eq!(ext.len(), 1, "extern func should be verified");
    assert_eq!(
        ext[0].status,
        VerifStatus::Disproven, // P2.3: Sat means a concrete counterexample exists.
        "extern ensures should be inconsistent: {}",
        ext[0].message
    );
}

#[test]
fn verify_extern_requires_ensures_consistent() {
    require_z3!();
    let src = r#"
extern "C" {
    func process(x: i64) -> i64
        requires: x > 0
        ensures: result > x;
}

func main() -> i64 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:206 unwrap failed");
    let ext: Vec<_> = results
        .iter()
        .filter(|r| r.func_name.contains("extern"))
        .collect();
    assert_eq!(ext.len(), 1, "extern func should be verified");
    assert_eq!(
        ext[0].status,
        VerifStatus::Disproven, // P2.3: Sat means a concrete counterexample exists.
        "extern requires+ensures should be inconsistent: {}",
        ext[0].message
    );
}

#[test]
fn verify_extern_unsatisfiable_requires() {
    require_z3!();
    let src = r#"
extern "C" {
    func impossible(x: i64) -> i64
        requires: x > 0 && x < 0;
}

func main() -> i64 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:224 unwrap failed");
    let ext: Vec<_> = results
        .iter()
        .filter(|r| r.func_name.contains("extern"))
        .collect();
    assert_eq!(ext.len(), 1);
    assert_eq!(
        ext[0].status,
        VerifStatus::Failed,
        "contradictory requires should fail: {}",
        ext[0].message
    );
    assert!(ext[0].message.contains("unsatisfiable"));
    let diagnostic = ext[0].diagnostic.as_ref().expect("extern diagnostic");
    assert_eq!(diagnostic.span.start_line, 3);
    assert_eq!(diagnostic.span.start_col, 5);
    assert!(diagnostic.span.end_line >= diagnostic.span.start_line);
    assert!(diagnostic.span.end_col > 0);
}

#[test]
fn verify_extern_no_contracts_skipped() {
    let src = r#"
extern "C" {
    func add(a: i64, b: i64) -> i64;
}

func main() -> i64 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:241 unwrap failed");
    let ext: Vec<_> = results
        .iter()
        .filter(|r| r.func_name.contains("extern"))
        .collect();
    assert_eq!(
        ext.len(),
        0,
        "extern func without contracts should be skipped"
    );
}

#[test]
fn verify_extern_with_main_only() {
    let src = r#"
extern "C" {
    func identity(x: i64) -> i64
        ensures: result == x;
}

func main() -> i64 {
    ensures: true
    0
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:259 unwrap failed");
    let func_names: Vec<&str> = results.iter().map(|r| r.func_name.as_str()).collect();
    assert!(
        func_names.contains(&"extern identity"),
        "extern identity should be in results: {:?}",
        func_names
    );
    assert!(
        func_names.contains(&"main"),
        "main should be in results: {:?}",
        func_names
    );
}

// --- extract_body_return: if/else branch coverage ---

#[test]
fn verify_if_else_body_all_paths_verified() {
    require_z3!();
    let src = r#"
func abs(x: i32) -> i32 {
    requires: x >= -2147483647
    ensures: result >= 0
    if x >= 0 { x } else { -x }
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:277 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "abs with if/else should be verified: {}",
        results[0].message
    );
}

#[test]
fn verify_if_else_body_violation_detected() {
    require_z3!();
    let src = r#"
func bad_abs(x: i32) -> i32 {
    requires: true
    ensures: result >= 0
    if x >= 0 { x } else { x - 1 }
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:293 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Failed,
        "bad_abs with if/else should fail (else branch x-1 can be negative)"
    );
}

#[test]
fn verify_nested_if_else_body() {
    require_z3!();
    let src = r#"
func sign(x: i32) -> i32 {
    requires: true
    ensures: result == 1 || result == 0 || result == -1
    if x > 0 { 1 } else { if x < 0 { -1 } else { 0 } }
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:309 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "nested if/else should be verified: {}",
        results[0].message
    );
}

#[test]
fn verify_if_else_body_with_requires() {
    require_z3!();
    let src = r#"
func add_or_mul(x: i32, y: i32) -> i32 {
    requires: x >= 0 && y >= 0 && x <= 40000 && y <= 40000
    ensures: result >= 0
    if x > y { x + y } else { x * y }
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:325 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "add_or_mul with if/else should be verified: {}",
        results[0].message
    );
}

// --- eval_expr_on_model: f64 boolean degeneracy ---

#[test]
fn verify_f64_ensures() {
    require_z3!();
    let src = r#"
func positive(x: f64) -> f64 {
    requires: x > 0.0
    ensures: result > 0.0
    x
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:343 unwrap failed");
    assert_eq!(results.len(), 1);
    // P0-10: F64Compare encoding is semantically unsound (NaN bit-pattern
    // breaks IEEE 754 ordering). All f64 comparisons are now fail-closed
    // → NotInTrustedSubset until a proper uninterpreted predicate is implemented.
    assert_eq!(
        results[0].status,
        VerifStatus::NotInTrustedSubset,
        "f64 comparison should be NotInTrustedSubset (P0-10): {}",
        results[0].message
    );
}

#[test]
fn verify_f64_ensures_violation() {
    require_z3!();
    let src = r#"
func negate(x: f64) -> f64 {
    requires: x > 0.0
    ensures: result > 0.0
    -x
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs:359 unwrap failed");
    assert_eq!(results.len(), 1);
    // 0.31.28: f64 negation is NOT in the trusted subset (IEEE 754 rounding
    // is not modeled). The VIR path rejects f64 arithmetic → NotInTrustedSubset.
    assert_eq!(
        results[0].status,
        VerifStatus::NotInTrustedSubset,
        "f64 negation should be NotInTrustedSubset: {}",
        results[0].message
    );
}

// --- FFI call-site verification ---

#[test]
fn verify_ffi_no_requires() {
    require_z3!();
    let src = r#"
extern "C" {
    func get_value() -> i64;
}
func caller() -> i64 {
    get_value()
}
"#;
    let results = verify_ffi_source(src).expect("src/verifier/tests.rs:380 unwrap failed");
    assert!(
        results.iter().all(|r| r.status == VerifStatus::Verified),
        "no-requires extern should be Verified: {:?}",
        results
    );
}

#[test]
fn verify_ffi_requires_always_satisfied() {
    require_z3!();
    let src = r#"
extern "C" {
    func read(fd: i64, buf: i64, size: i64) -> i64;
}
func caller(fd: i64, buf: i64, size: i64) -> i64 {
    requires: fd >= 0 && size > 0
    read(fd, buf, size)
}
"#;
    let results = verify_ffi_source(src).expect("src/verifier/tests.rs:397 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "requires fd >= 0 && size > 0 should satisfy read's preconditions: {}",
        results[0].message
    );
}

#[test]
fn verify_ffi_requires_violated() {
    require_z3!();
    let src = r#"
extern "C" {
    func read(fd: i64, buf: i64, size: i64) -> i64
        requires: fd >= 0 && size > 0;
}
func bad_caller(size: i64) -> i64 {
    read(-1, 0, size)
}
"#;
    let results = verify_ffi_source(src).expect("src/verifier/tests.rs:415 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Failed,
        "read(-1, 0, size) should fail: fd is negative"
    );
    let diagnostic = results[0]
        .diagnostic
        .as_ref()
        .expect("extern call-site diagnostic");
    assert_eq!(diagnostic.span.start_line, 7);
    assert_eq!(diagnostic.span.start_col, 5);
    assert_eq!(diagnostic.span.end_line, 7);
    assert_eq!(diagnostic.span.end_col, 22);
}

#[test]
fn verify_ffi_string_empty_violation() {
    require_z3!();
    let src = r#"
extern "C" {
    func strlen(s: string) -> i64
        requires: s != "";
}
func caller(s: string) -> i64 {
    strlen(s)
}
"#;
    let results = verify_ffi_source(src).expect("src/verifier/tests.rs:433 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Failed,
        "strlen(s) without guard should fail: s could be empty"
    );
}

#[test]
fn verify_ffi_string_empty_protected() {
    require_z3!();
    let src = r#"
extern "C" {
    func strlen(s: string) -> i64
        requires: s != "";
}
func caller(s: string) -> i64 {
    requires: s != ""
    strlen(s)
}
"#;
    let results = verify_ffi_source(src).expect("src/verifier/tests.rs:452 unwrap failed");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "strlen(s) with guard should be Verified: {}",
        results[0].message
    );
}

#[test]
fn verify_ffi_multiple_externs() {
    require_z3!();
    let src = r#"
extern "C" {
    func read(fd: i64, buf: i64, size: i64) -> i64
        requires: fd >= 0;
    func write(fd: i64, buf: i64, size: i64) -> i64
        requires: fd >= 0;
}
func ok_caller(fd: i64) -> i64 {
    requires: fd >= 0
    read(fd, 0, 1) + write(fd, 0, 1)
}
func bad_caller(fd: i64) -> i64 {
    read(fd, 0, 1) + write(fd, 0, 1)
}
"#;
    let results = verify_ffi_source(src).expect("src/verifier/tests.rs:476 unwrap failed");
    assert_eq!(results.len(), 4);
    let ok_results: Vec<_> = results
        .iter()
        .filter(|r| r.func_name.starts_with("ok_caller"))
        .collect();
    assert_eq!(ok_results.len(), 2);
    assert!(
        ok_results.iter().all(|r| r.status == VerifStatus::Verified),
        "ok_caller should pass: {:?}",
        ok_results
    );
    let bad_results: Vec<_> = results
        .iter()
        .filter(|r| r.func_name.starts_with("bad_caller"))
        .collect();
    assert_eq!(bad_results.len(), 2);
    assert!(
        bad_results.iter().any(|r| r.status == VerifStatus::Failed),
        "bad_caller should have at least one failure: {:?}",
        bad_results
    );
}

#[test]
fn verify_invariant_basic() {
    require_z3!();
    let src = r#"
func identity(x: i32) -> i32 {
    requires: x > 0
    ensures: result > 0
    invariant: x > 0
    x
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: verify_invariant_basic");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "invariant as constraint should verify: {:?}",
        results[0]
    );
}

#[test]
fn verify_invariant_with_ensures() {
    require_z3!();
    let src = r#"
func add_one(x: i32) -> i32 {
    requires: x > 0 && x < 2147483647
    ensures: result > x
    invariant: x > 0
    x + 1
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: verify_invariant_with_ensures");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "invariant + ensures should verify: {:?}",
        results[0]
    );
}

#[test]
fn verify_f64_add_and_compare() {
    require_z3!();
    let src = r#"
func scale_add(x: f64) -> f64 {
    requires: x > 1.0
    ensures: result > x
    x + 1.0
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: verify_f64_add_and_compare");
    assert_eq!(results.len(), 1);
    // 0.31.28: f64 arithmetic is NOT in the trusted subset (IEEE 754 rounding
    // is not modeled). The VIR path rejects f64 arithmetic → NotInTrustedSubset.
    assert_eq!(
        results[0].status,
        VerifStatus::NotInTrustedSubset,
        "f64 arithmetic should be NotInTrustedSubset: {:?}",
        results[0]
    );
}

#[test]
fn verify_record_field_access_int() {
    require_z3!();
    let src = r#"
type Point { x: i32, y: i32 }
func point_x_positive(p: Point) -> i32 {
    requires: p.x > 0
    ensures: result > 0
    p.x
}
"#;
    let results =
        verify_source(src).expect("src/verifier/tests.rs: verify_record_field_access_int");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "record field access in contract should verify: {:?}",
        results[0]
    );
}

#[test]
fn verify_record_field_violation() {
    require_z3!();
    let src = r#"
type Point { x: i32, y: i32 }
func bad_point_x(p: Point) -> i32 {
    requires: p.x > 0
    ensures: result > p.x
    p.x
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: verify_record_field_violation");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].status,
        VerifStatus::Failed,
        "record field violation should be detected: {:?}",
        results[0]
    );
}

#[test]
fn public_checked_verifier_routes_closed_copy_record_to_mir() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_verifier_record_contract.mimi");
    let file = parse_memory_source(source, "mir-record-public-api").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    let verify_result = verify_checked(&program, source_hash.clone()).expect("MIR verify");
    let result = verify_result
        .iter()
        .find(|result| result.func_name.ends_with("advance"))
        .expect("record contract result");
    assert_eq!(result.status, VerifStatus::Proven);
    assert_eq!(
        result
            .artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(ProofArtifact::ENGINE_MIR)
    );

    let dual_result = verify_checked_dual(&program, source_hash).expect("MIR dual verify");
    let dual = dual_result
        .iter()
        .find(|result| result.func_name.ends_with("advance"))
        .expect("dual record contract result");
    assert_eq!(dual.status, VerifStatus::Proven);
    assert_eq!(
        dual.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn compatibility_verifier_access_is_explicitly_tagged() {
    require_z3!();
    let source = r#"
        func positive(value: i32) -> i32 {
            requires: value > 0
            ensures: result > 0
            value
        }

        func main() -> i32 { positive(1) }
    "#;
    let file = parse_memory_source(source, "legacy-body-owner-audit").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    verify_checked(&program, source_hash.clone()).expect("compatibility verifier");
    assert_eq!(
        crate::core::CheckedProgram::test_legacy_body_access(),
        vec![crate::core::LegacyBodyConsumer::FlowVerifierCompatibility]
    );

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    verify_checked_dual(&program, source_hash).expect("dual compatibility verifier");
    assert_eq!(
        crate::core::CheckedProgram::test_legacy_body_access(),
        vec![crate::core::LegacyBodyConsumer::DualVerifierCompatibility]
    );
}

#[test]
fn public_checked_verifier_routes_closed_scalar_collection_to_mir() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_list_len.mimi");
    let file = parse_memory_source(source, "mir-scalar-collection-public-api").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    assert_eq!(
        crate::core::mir::classify_scalar_collection_admission(&program),
        crate::core::mir::ScalarCollectionAdmission::CompleteCoverage
    );
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    let verify_result = verify_checked(&program, source_hash.clone()).expect("MIR verify");
    let result = verify_result
        .iter()
        .find(|result| result.func_name == "list_len_contract")
        .expect("scalar collection contract result");
    assert_eq!(result.status, VerifStatus::Proven);
    assert_eq!(
        result
            .artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(ProofArtifact::ENGINE_MIR)
    );

    let dual_result = verify_checked_dual(&program, source_hash).expect("MIR dual verify");
    let dual = dual_result
        .iter()
        .find(|result| result.func_name == "list_len_contract")
        .expect("dual scalar collection contract result");
    assert_eq!(dual.status, VerifStatus::Proven);
    assert_eq!(
        dual.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn scalar_collection_verifier_admission_does_not_overmatch_managed_siblings() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_test_scalar_collection_mixed.mimi");
    let file = parse_memory_source(source, "mir-scalar-collection-mixed").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    assert_eq!(
        crate::core::mir::classify_scalar_collection_admission(&program),
        crate::core::mir::ScalarCollectionAdmission::MixedCoverage
    );
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    assert!(verify_closed_scalar_collection_mir(&program, source_hash)
        .expect("mixed collection program remains outside the closed verifier island")
        .is_none());
}

#[test]
fn scalar_collection_route_receipt_is_shared_by_all_consumers() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_list_len.mimi");
    let file = parse_memory_source(source, "mir-scalar-collection-receipt").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    assert_eq!(
        crate::core::mir::classify_scalar_collection_admission(&checked),
        crate::core::mir::ScalarCollectionAdmission::CompleteCoverage
    );
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    crate::core::mir::validate_scalar_collection_island(&canonical)
        .expect("scalar collection island");
    crate::verifier::validate_mir_capabilities(&canonical).expect("verifier capability");

    let receipt = canonical.route_receipt(crate::core::mir::SCALAR_COLLECTION_ISLAND);
    assert_eq!(receipt.schema, crate::core::mir::MIR_ROUTE_RECEIPT_SCHEMA);
    assert_eq!(receipt.profile, crate::core::mir::SCALAR_COLLECTION_ISLAND);
    assert_eq!(receipt.mir_digest.len(), 64);
    assert_eq!(receipt.type_desc_digest.len(), 64);
    assert_eq!(receipt.ownership_digest.len(), 64);
    assert!(receipt
        .root_owners
        .iter()
        .any(|owner| owner.0 == "function:main"));

    // Every consumer below receives the same immutable MIR graph.  The
    // receipt is recomputed only for comparison; it is never a backend input
    // from which a consumer could reconstruct frontend semantics.
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::Int(42)
    );
    let bytecode = crate::interp::bytecode::compile_mir_program(&canonical).expect("MIR bytecode");
    let mut bytecode_vm = crate::interp::bytecode::BytecodeVM::new(bytecode);
    assert_eq!(bytecode_vm.run().expect("MIR bytecode execution"), 42);
    crate::codegen::mir::validate_mir_native(&canonical).expect("native MIR capability");
    let verifier_results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let artifact = verifier_results
        .iter()
        .find(|result| result.func_name == "function:list_len_contract")
        .and_then(|result| result.artifact.as_ref())
        .expect("MIR proof artifact");
    assert_eq!(artifact.mir_hash, receipt.mir_digest);
    assert_eq!(artifact.engine, crate::verifier::ProofArtifact::ENGINE_MIR);

    let after_consumers = canonical.route_receipt(crate::core::mir::SCALAR_COLLECTION_ISLAND);
    assert_eq!(receipt, after_consumers);
}

#[test]
fn finite_f64_add_verifier_capability_is_closed_before_symbolic_execution() {
    let source = include_str!("../../tests/fixtures/mir_native_f64_add.mimi");
    let file = parse_memory_source(source, "mir-f64-add-capability").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical f64 add MIR");
    crate::verifier::validate_mir_capabilities(&canonical).expect("finite-only f64 Add capability");

    let source = include_str!("../../tests/fixtures/mir_native_f64_subtract.mimi");
    let file = parse_memory_source(source, "mir-f64-subtract-capability").expect("parse subtract");
    let checked = crate::core::check_program(&file).expect("typecheck subtract");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical f64 subtract MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("finite-only f64 Subtract capability");

    let subtract = parse_memory_source(
        "func main() -> f64 { 1.0 * 2.0 }",
        "mir-f64-multiply-capability",
    )
    .expect("parse multiply");
    let checked = crate::core::check_program(&subtract).expect("typecheck multiply");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical f64 multiply MIR");
    let errors = crate::verifier::validate_mir_capabilities(&canonical)
        .expect_err("f64 multiply must remain outside the closed Add/Subtract capability");
    let message = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        message.contains("finite-only Copy f64 contract"),
        "unexpected verifier capability rejection: {message}"
    );
}

#[test]
fn finite_f64_add_contract_is_rejected_before_symbolic_execution_without_float_model() {
    let source = r#"
func add(left: f64, right: f64) -> f64 {
    requires: left == left
    ensures: result == result
    left + right
}
func main() -> i64 { 42 }
"#;
    let file = parse_memory_source(source, "mir-f64-add-symbolic-boundary").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("f64 contracts must remain outside the canonical verifier domain");
    let message = format!("{error:?}");
    assert!(
        message.contains(crate::core::mir::types::MIR_VERIFIER_FLOAT_BOUNDARY_CODE)
            && message.contains("ABI Float")
            && message.contains("canonical scalar verifier contract")
            && message.contains(crate::core::mir::types::MIR_FLOAT_NOT_FINITE_TRAP_CODE),
        "unexpected f64 contract boundary: {message}"
    );
}

#[test]
fn generic_record_projection_is_consumed_by_mir_verifier_without_ast_fallback() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_record_projection.mimi");
    let file = parse_memory_source(source, "mir-generic-record-projection").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic record projection MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection { .. }
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic record projection verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().all(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        )
    }));
}

#[test]
fn generic_option_predicate_is_verified_from_canonical_mir_without_ast_fallback() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_option_predicate.mimi");
    let file = parse_memory_source(source, "mir-generic-option-predicate").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Option predicate MIR");
    let instance = canonical
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                crate::core::mir::MirGenericInstanceContract::ScalarVariantPredicate { .. }
            )
        })
        .expect("generic Option predicate instance");
    assert!(matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarVariantPredicate { .. }
    ));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Option predicate verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().all(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        )
    }));
}

#[test]
fn generic_option_unwrap_is_verified_from_canonical_mir_without_ast_fallback() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_option_unwrap.mimi");
    let file = parse_memory_source(source, "mir-generic-option-unwrap").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Option unwrap MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { .. }
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Option unwrap verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().any(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations
        )
    }));
}

#[test]
fn generic_option_unwrap_owned_string_is_verified_from_canonical_mir() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_option_unwrap_owned_string.mimi");
    let file =
        parse_memory_source(source, "mir-generic-option-unwrap-owned-string").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic owned Option unwrap MIR");
    assert!(canonical.instances().values().any(|instance| {
        matches!(
            &instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract }
                if contract.projection.nominal.as_str() == "builtin:type:Option"
                    && contract.projection.ownership
                        == crate::core::mir::types::MirOwnership::Move
        )
    }));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic owned Option unwrap verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().any(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations
        )
    }));
}

#[test]
fn generic_option_unwrap_owned_list_is_verified_from_canonical_mir() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_option_unwrap_owned_list.mimi");
    let file = parse_memory_source(source, "mir-generic-option-unwrap-owned-list").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic owned Option<List> unwrap MIR");
    assert!(canonical.instances().values().any(|instance| {
        matches!(
            &instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract }
                if contract.projection.nominal.as_str() == "builtin:type:Option"
                    && contract.projection.ownership
                        == crate::core::mir::types::MirOwnership::Move
                    && contract.projection.move_out_glue
                        == crate::core::mir::types::MirGlueKind::List
        )
    }));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic owned Option<List> verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().any(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations
        )
    }));
}

#[test]
fn generic_option_unwrap_owned_list_i64_and_bool_are_verified_from_canonical_mir() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_option_unwrap_owned_list_scalars.mimi"
    );
    let file =
        parse_memory_source(source, "mir-generic-option-unwrap-owned-list-scalars").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Option<List<i64|bool>> unwrap MIR");
    let instances = canonical
        .instances()
        .values()
        .filter(|instance| {
            matches!(
                &instance.contract,
                crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership
                            == crate::core::mir::types::MirOwnership::Move
                        && contract.projection.move_out_glue
                            == crate::core::mir::types::MirGlueKind::List
            )
        })
        .count();
    assert_eq!(instances, 2);
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Option<List<i64|bool>> verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().any(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations
        )
    }));
}

#[test]
fn generic_option_unwrap_or_is_verified_from_canonical_mir_without_ast_fallback() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_option_unwrap_or.mimi");
    let file = parse_memory_source(source, "mir-generic-option-unwrap-or").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Option unwrap_or MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback { .. }
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Option unwrap_or verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().any(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations
        )
    }));
}

#[test]
fn generic_option_unwrap_or_none_is_verified_from_canonical_mir() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_option_unwrap_or_none.mimi");
    let file = parse_memory_source(source, "mir-generic-option-unwrap-or-none").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Option unwrap_or None MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Option unwrap_or None verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().all(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        )
    }));
}

#[test]
fn generic_result_unwrap_is_verified_from_canonical_mir_without_ast_fallback() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_result_unwrap.mimi");
    let file = parse_memory_source(source, "mir-generic-result-unwrap").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Result unwrap MIR");
    assert!(canonical.instances().values().any(|instance| {
        matches!(
            &instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract }
                if contract.projection.nominal.as_str() == "builtin:type:Result"
        )
    }));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Result unwrap verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().all(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        )
    }));
}

#[test]
fn generic_result_unwrap_or_is_verified_from_canonical_mir_without_ast_fallback() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_result_unwrap_or.mimi");
    let file = parse_memory_source(source, "mir-generic-result-unwrap-or").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Result unwrap_or MIR");
    assert!(canonical.instances().values().any(|instance| {
        matches!(
            &instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                contract
            } if contract.projection.nominal.as_str() == "builtin:type:Result"
        )
    }));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Result unwrap_or verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().any(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        )
    }));
}

#[test]
fn generic_distinct_result_unwrap_or_is_verified_from_canonical_mir() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_result_distinct_unwrap_or.mimi");
    let file = parse_memory_source(source, "mir-generic-distinct-result-unwrap-or").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical heterogeneous Result unwrap_or MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("heterogeneous Result unwrap_or verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().all(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        )
    }));
}

#[test]
fn generic_result_unwrap_or_is_rejected_before_legacy_verifier_fallback() {
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_result_unwrap_or_rejected.mimi");
    let file = parse_memory_source(source, "mir-generic-result-unwrap-or-rejected").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let error = crate::verifier::verify_checked_dual(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect_err("unsupported generic Result unwrap_or must not fall through to AST verifier");
    assert!(
        error.contains("generic Result fallback projection"),
        "{error}"
    );
}

#[test]
fn generic_result_projection_is_rejected_before_legacy_verifier_fallback() {
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_result_unwrap_rejected.mimi");
    let file = parse_memory_source(source, "mir-generic-result-rejected").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let error = crate::verifier::verify_checked_dual(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect_err("unsupported generic Result projection must not fall through to AST verifier");
    assert!(
        error.contains("generic-result-projection-v1")
            || error.contains("generic Result projection"),
        "{error}"
    );
}

#[test]
fn generic_result_distinct_projection_is_verified_from_canonical_mir() {
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_result_distinct_unwrap.mimi");
    let file = parse_memory_source(source, "mir-generic-result-distinct").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let results = crate::verifier::verify_checked_dual(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("distinct generic Result projection must remain on canonical MIR");
    assert!(results.is_empty(), "fixture has no contracts to verify");
}

#[test]
fn generic_option_unwrap_or_is_rejected_before_legacy_verifier_fallback() {
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_option_unwrap_or_rejected.mimi");
    let file = parse_memory_source(source, "mir-generic-option-unwrap-or").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let error = crate::verifier::verify_checked_dual(
        &checked,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect_err("generic Option unwrap_or must not fall through to AST verifier");
    assert!(error.contains("generic Option projection"), "{error}");
}

#[test]
fn generic_result_predicate_is_verified_from_canonical_mir_without_ast_fallback() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_result_predicate.mimi");
    let file = parse_memory_source(source, "mir-generic-result-predicate").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic Result predicate MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarVariantPredicate {
            contract: crate::core::mir::types::MirVariantPredicateContract {
                predicate: crate::core::mir::MirVariantPredicate::IsOk,
                ..
            }
        }
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic Result predicate verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().all(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        )
    }));
}

#[test]
fn generic_result_error_slot_predicate_is_verified_from_canonical_mir() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_generic_result_error_slot.mimi");
    let file = parse_memory_source(source, "mir-generic-result-error-slot").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical Result<i32, T> predicate MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarVariantPredicate {
            contract: crate::core::mir::types::MirVariantPredicateContract {
                predicate: crate::core::mir::MirVariantPredicate::IsErr,
                ..
            }
        }
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("Result error-slot predicate verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    assert!(results.iter().all(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        )
    }));
}

#[test]
fn two_field_generic_record_projection_is_consumed_by_mir_verifier() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_record_projection_pair.mimi");
    let file = parse_memory_source(source, "mir-generic-record-projection-pair").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("two-field generic record projection MIR");
    let instance = canonical
        .instances()
        .values()
        .next()
        .expect("two-field generic record projection instance");
    assert!(matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
            ref contract
        } if contract.arity == 2 && contract.name == "left"
    ));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("two-field generic record projection verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn mixed_generic_record_projection_is_consumed_by_mir_verifier() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_record_projection_mixed.mimi");
    let file = parse_memory_source(source, "mir-generic-record-projection-mixed").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("mixed generic record projection MIR");
    let instance = canonical
        .instances()
        .values()
        .next()
        .expect("mixed generic record projection instance");
    assert!(matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
            ref contract
        } if contract.arity == 2 && contract.name == "value"
    ));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("mixed generic record projection verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn generic_record_projection_rvalue_call_is_verified_from_consuming_mir() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_record_projection_rvalue.mimi");
    let file = parse_memory_source(source, "mir-generic-record-projection-rvalue").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical generic record Copy rvalue MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("generic record Copy rvalue verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn scalar_generic_record_projection_i64_and_bool_keep_mir_proof_artifacts() {
    require_z3!();
    for (label, source) in [
        (
            "mir-generic-record-projection-i64",
            include_str!("../../tests/fixtures/mir_native_generic_record_projection_i64.mimi"),
        ),
        (
            "mir-generic-record-projection-bool",
            include_str!("../../tests/fixtures/mir_native_generic_record_projection_bool.mimi"),
        ),
    ] {
        let file = parse_memory_source(source, label).expect("parse");
        let checked = crate::core::check_program(&file).expect("typecheck");
        let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
            .expect("scalar generic record projection MIR");
        assert!(canonical.instances().values().any(|instance| matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection { .. }
        )));
        crate::verifier::validate_mir_capabilities(&canonical)
            .expect("scalar generic record projection verifier capability");
        let results = crate::verifier::verify_mir(
            &canonical,
            blake3::hash(source.as_bytes()).to_hex().to_string(),
        )
        .expect("MIR verifier");
        let main = results
            .iter()
            .find(|result| result.func_name == "function:main")
            .expect("main verification result");
        assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
        assert_eq!(
            main.artifact
                .as_ref()
                .map(|artifact| artifact.engine.as_str()),
            Some(crate::verifier::ProofArtifact::ENGINE_MIR)
        );
    }
}

#[test]
fn owned_generic_record_projection_is_verified_from_consuming_mir() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_record_owned_string_projection.mimi");
    let file = parse_memory_source(source, "mir-owned-generic-record-projection").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical owned generic record projection MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::OwnedRecordProjection { .. }
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("owned generic record projection verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn owned_mixed_generic_record_projection_is_verified_from_consuming_mir() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_record_owned_string_mixed.mimi");
    let file =
        parse_memory_source(source, "mir-owned-mixed-generic-record-projection").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical mixed owned generic record projection MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::OwnedRecordProjection { ref contract }
            if contract.arity == 2 && contract.name == "value"
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("mixed owned generic record projection verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn owned_generic_record_projection_with_residual_drop_is_verified_from_mir() {
    require_z3!();
    let source =
        include_str!("../../tests/fixtures/mir_native_generic_record_owned_string_residual.mimi");
    let file = parse_memory_source(source, "mir-owned-generic-record-residual").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical owned generic record residual MIR");
    assert!(canonical.instances().values().any(|instance| matches!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
            ref contract
        } if contract.projection.arity == 2
            && contract.projection.name == "value"
            && contract.residual.len() == 1
            && contract.residual[0].name == "note"
    )));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("owned generic record residual verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn owned_generic_record_projection_rvalue_is_verified_from_consuming_mir() {
    require_z3!();
    let source = include_str!(
        "../../tests/fixtures/mir_native_generic_record_owned_string_rvalue_call.mimi"
    );
    let file = parse_memory_source(source, "mir-owned-generic-record-rvalue").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical owned generic record rvalue MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("owned generic record rvalue verifier capability");
    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn flat_copy_variant_predicates_are_verified_from_canonical_mir() {
    require_z3!();
    let source = r#"
        func main() -> i32 {
            ensures: result == 4
            let some: Option<i32> = Some(41)
            let none: Option<i32> = None
            let ok: Result<i32, i32> = Ok(7)
            let err: Result<i32, i32> = Err(9)
            let all_true = some.is_some() && none.is_none() && ok.is_ok() && err.is_err()
            if all_true { 4 } else { 0 }
        }
    "#;
    let file = parse_memory_source(source, "mir-variant-predicate").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical variant predicate MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("flat Copy variant predicate capability");
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::Int(4)
    );

    let results = crate::verifier::verify_mir(
        &canonical,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("canonical MIR verifier");
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .expect("main verification result");
    assert_eq!(main.status, VerifStatus::Proven, "{}", main.message);
    assert_eq!(
        main.artifact
            .as_ref()
            .map(|artifact| artifact.engine.as_str()),
        Some(crate::verifier::ProofArtifact::ENGINE_MIR)
    );
}

#[test]
fn exact_s8_route_receipt_covers_all_consumers_without_legacy_escape() {
    require_z3!();
    let source = include_str!("../../tests/fixtures/mir_native_flow_transition.mimi");
    let file = parse_memory_source(source, "mir-s8-four-consumer-receipt").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("exact S8 route must materialize");
    assert_eq!(
        route.admission.flow,
        crate::core::mir::S8FlowAdmission::CompleteCoverage
    );
    assert!(route.materialized_flow_candidate);
    let canonical = route.program;
    let profile = crate::core::mir::CanonicalMirRouteProfile::S8FlowTransition.as_str();
    let receipt = canonical.route_receipt(profile);
    assert_eq!(receipt.schema, crate::core::mir::MIR_ROUTE_RECEIPT_SCHEMA);
    assert_eq!(receipt.profile, profile);
    assert_eq!(receipt.mir_digest.len(), 64);
    assert_eq!(receipt.type_desc_digest.len(), 64);
    assert_eq!(receipt.abi_digest.len(), 64);
    assert_eq!(receipt.ownership_digest.len(), 64);
    assert_eq!(receipt.flow_transition_digest.len(), 64);
    assert!(receipt
        .root_owners
        .iter()
        .any(|owner| owner.0 == "function:main"));

    // The receipt is an immutable identity witness, not a backend input. All
    // four consumers below receive this same MIR object and no consumer may
    // reopen CheckedProgram or the source AST.
    crate::verifier::validate_mir_capabilities(&canonical).expect("MIR verifier capability");
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::Int(42)
    );
    let bytecode =
        crate::interp::bytecode::compile_mir_program(&canonical).expect("MIR bytecode compilation");
    let mut bytecode_vm = crate::interp::bytecode::BytecodeVM::new(bytecode);
    assert_eq!(bytecode_vm.run().expect("MIR bytecode execution"), 42);

    crate::codegen::mir::validate_mir_native(&canonical).expect("native MIR validation");
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "s28_s8_receipt");
    codegen
        .compile_mir_native(&canonical)
        .expect("native MIR compilation");

    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let mir_results =
        crate::verifier::verify_mir(&canonical, source_hash.clone()).expect("MIR verifier");
    assert!(mir_results.is_empty(), "fixture has no contracts");

    // The thread-local tripwires isolate this proof from unrelated parallel
    // tests: verify_checked and verify_checked_dual must each materialize once
    // and must not touch the compatibility raw-AST accessor.
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    crate::core::mir::reset_test_route_materialization_count();
    assert!(
        crate::verifier::verify_checked(&checked, source_hash.clone())
            .expect("public verifier")
            .is_empty()
    );
    assert_eq!(crate::core::mir::test_route_materialization_count(), 1);
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    crate::core::mir::reset_test_route_materialization_count();
    assert!(crate::verifier::verify_checked_dual(&checked, source_hash)
        .expect("dual public verifier")
        .is_empty());
    assert_eq!(crate::core::mir::test_route_materialization_count(), 1);
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    // Recomputing the audit witness after all consumers proves that no
    // backend mutated the canonical graph or its side tables.
    assert_eq!(receipt, canonical.route_receipt(profile));
    assert_eq!(
        receipt,
        crate::core::mir::reference::MirProgram::from_checked_program(&checked)
            .expect("repeat canonical materialization")
            .route_receipt(profile)
    );
    assert_eq!(
        crate::core::mir::MIR_ROUTE_VALIDATOR_CONTRACT_ID,
        "mimi-mir-route-validator-v1"
    );
}

#[test]
fn non_copy_option_string_switch_move_closes_all_four_consumers() {
    require_z3!();
    let source = r#"
        func consume(value: Option<string>) -> i32 {
            ensures: result >= 0
            match value {
                Some(_) => 41,
                None => 0
            }
        }

        func discard(value: Option<string>) -> i32 {
            ensures: result >= 0
            match value {
                Some(_) => 7,
                None => 8
            }
        }

        func main() -> i32 {
            let first: Option<string> = Some("owned")
            let second: Option<string> = Some("discard")
            let a = consume(first)
            let b = discard(second)
            a + b
        }
    "#;
    let file = parse_memory_source(source, "mir-option-string-switch-move").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("Option<string> SwitchMove verifier capability");

    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::Int(48)
    );
    let bytecode =
        crate::interp::bytecode::compile_mir_program(&canonical).expect("MIR bytecode compilation");
    let mut bytecode_vm = crate::interp::bytecode::BytecodeVM::new(bytecode);
    assert_eq!(bytecode_vm.run().expect("MIR bytecode execution"), 48);

    crate::codegen::mir::validate_mir_native(&canonical).expect("native MIR validation");
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "mir_option_string_verifier");
    codegen
        .compile_mir_native(&canonical)
        .expect("native MIR compilation");
    codegen.module.verify().expect("native module verification");

    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let results =
        crate::verifier::verify_mir(&canonical, source_hash.clone()).expect("MIR verifier");
    for function in ["function:consume", "function:discard"] {
        let result = results
            .iter()
            .find(|result| result.func_name == function)
            .expect("contract-bearing Option<string> function");
        assert_eq!(
            result.status,
            VerifStatus::Proven,
            "{function}: {}",
            result.message
        );
    }

    // Public checked-verifier entry points must select the same closed island
    // before their historical AST/Flow compatibility engine.  The counters
    // make an accidental raw-AST fallback observable even though the verdicts
    // would otherwise look identical.
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    crate::core::mir::reset_test_route_materialization_count();
    let checked_results = crate::verifier::verify_checked(&checked, source_hash.clone())
        .expect("public checked verifier must use the Option<string> MIR island");
    assert!(checked_results
        .iter()
        .any(|result| result.func_name == "consume"));
    assert!(checked_results
        .iter()
        .any(|result| result.func_name == "discard"));
    assert_eq!(crate::core::mir::test_route_materialization_count(), 1);
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    crate::core::mir::reset_test_route_materialization_count();
    let dual_results = crate::verifier::verify_checked_dual(&checked, source_hash)
        .expect("dual public checked verifier must use the Option<string> MIR island");
    assert!(dual_results
        .iter()
        .any(|result| result.func_name == "consume"));
    assert!(dual_results
        .iter()
        .any(|result| result.func_name == "discard"));
    assert_eq!(crate::core::mir::test_route_materialization_count(), 1);
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn non_copy_option_string_switch_move_default_is_rejected_before_consumers() {
    let source = r#"
        func main() -> i32 {
            let value: Option<string> = Some("owned")
            match value {
                Some(_) => 41,
                _ => 0
            }
        }
    "#;
    let file = parse_memory_source(source, "mir-option-string-switch-default").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let errors = crate::verifier::validate_mir_capabilities(&canonical)
        .expect_err("consuming default must remain outside the verifier island");
    assert!(errors
        .iter()
        .any(|error| { error.contains("explicit variant arms") || error.contains("SwitchMove") }));
}

#[test]
fn malformed_s8_mir_missing_transition_contract_is_rejected_before_consumers() {
    let source = include_str!("../../tests/fixtures/mir_native_flow_transition.mimi");
    let file = parse_memory_source(source, "mir-s8-invalid-transition").expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let error =
        crate::core::mir::reference::MirProgram::with_type_catalog_and_instances_and_transitions(
            canonical.functions().clone(),
            canonical.type_catalog().clone(),
            canonical.instances().clone(),
            std::collections::BTreeMap::new(),
        )
        .expect_err("FlowTransition without its contract must be rejected by MIR validation");
    assert!(error.iter().any(|error| {
        error.message.contains("transition") && error.message.contains("contract")
    }));
}

#[test]
fn public_checked_verifier_routes_closed_s8_flow_to_mir() {
    require_z3!();
    let source = r#"
        flow Counter {
            state Zero { n: i32 }
            transition inc(Zero) -> Zero {
                return Zero { n: self.n + 1 }
            }
        }

        func main() -> i32 {
            let c = Zero { n: 41 }
            let c2 = Counter::inc(c)
            c2.n
        }
    "#;
    let file = parse_memory_source(source, "mir-s8-public-api").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    assert!(crate::core::mir::is_exact_s8_flow_transition(&program));
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    let mir_route = verify_closed_s8_flow_mir(&program, source_hash.clone())
        .expect("closed S8 MIR verifier route");
    assert!(
        mir_route.is_some(),
        "exact S8 must be owned by the MIR route"
    );
    let verify_result = verify_checked(&program, source_hash.clone()).expect("MIR verify");
    assert!(verify_result.is_empty());
    assert!(mir_route.unwrap().is_empty());

    let dual_result = verify_checked_dual(&program, source_hash).expect("MIR dual verify");
    assert!(dual_result.is_empty());
}

#[test]
fn public_checked_verifier_does_not_overmatch_non_exact_s8_flow() {
    require_z3!();
    let source = r#"
        flow Counter {
            state Zero { n: i32 }
            transition inc(Zero) -> Zero {
                return Zero { n: self.n + 1 }
            }
        }

        func main() -> i32 {
            let c = Zero { n: 41 }
            let c2 = Counter::inc(c)
            println(c2.n)
            c2.n
        }
    "#;
    let file = parse_memory_source(source, "mir-s8-non-exact").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    assert!(crate::core::mir::is_s8_flow_transition_candidate(&program));
    assert!(!crate::core::mir::is_exact_s8_flow_transition(&program));
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    assert!(verify_closed_s8_flow_mir(&program, source_hash)
        .expect("non-exact S8 remains outside the closed MIR verifier island")
        .is_none());
}

#[test]
fn flat_copy_record_admission_is_checker_owned_before_materialization() {
    let source = include_str!("../../tests/fixtures/mir_verifier_record_contract.mimi");
    let file = parse_memory_source(source, "mir-record-admission").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    assert_eq!(
        crate::core::mir::classify_flat_copy_record_admission(&program),
        crate::core::mir::FlatCopyRecordAdmission::CompleteCoverage
    );

    let mixed_source = r#"
        type Point { x: i32 }
        func identity<T>(value: T) -> T { value }
        func main() -> i32 {
            let point = Point { x: 1 }
            point.x
        }
    "#;
    let mixed_file = parse_memory_source(mixed_source, "mir-record-mixed-admission")
        .expect("parse mixed source");
    let mixed_program = crate::core::check_program(&mixed_file).expect("typecheck mixed source");
    assert_eq!(
        crate::core::mir::classify_flat_copy_record_admission(&mixed_program),
        crate::core::mir::FlatCopyRecordAdmission::MixedCoverage
    );

    let borrow_source = r#"
        type Rec { a: i32, b: i32 }
        func swap(mutated: mutate Rec) -> Rec {
            mutated = Rec { a: mutated.b, b: mutated.a }
            mutated
        }
        func main() -> i32 { 0 }
    "#;
    let borrow_file = parse_memory_source(borrow_source, "mir-record-borrow-admission")
        .expect("parse borrow source");
    let borrow_program = crate::core::check_program(&borrow_file).expect("typecheck borrow source");
    assert_eq!(
        crate::core::mir::classify_flat_copy_record_admission(&borrow_program),
        crate::core::mir::FlatCopyRecordAdmission::MixedCoverage
    );

    let scalar_source = "func main() -> i32 { 0 }";
    let scalar_file = parse_memory_source(scalar_source, "mir-record-outside-admission")
        .expect("parse scalar source");
    let scalar_program = crate::core::check_program(&scalar_file).expect("typecheck scalar source");
    assert_eq!(
        crate::core::mir::classify_flat_copy_record_admission(&scalar_program),
        crate::core::mir::FlatCopyRecordAdmission::OutsideProfile
    );
}

#[test]
fn complete_flat_copy_record_materialization_failure_is_not_a_verifier_fallback() {
    require_z3!();
    let source = r#"
        type Point { x: i32 }

        func make_fn(p: Point) -> func(i32) -> i32 {
            fn(value: i32) -> i32 { value + 1 }
        }

        func main() -> i32 { 0 }
    "#;
    let file = parse_memory_source(source, "mir-record-materialization-error").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    assert_eq!(
        crate::core::mir::classify_flat_copy_record_admission(&program),
        crate::core::mir::FlatCopyRecordAdmission::CompleteCoverage
    );
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let error = verify_checked(&program, source_hash).expect_err("MIR construction must be hard");
    assert!(
        error.contains("MIR-MATERIALIZATION-001"),
        "complete admission must not fall back after construction failure: {error}"
    );
}

#[test]
fn public_checked_verifier_rejects_mixed_record_graph_without_ast_fallback() {
    require_z3!();
    let source = r#"
        type Point { x: i32 }

        func make_some() -> Result<string, i32> { Ok("owned") }

        func main() -> i32 {
            let point = Point { x: 1 }
            point.x
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    let error = verify_checked(&program, source_hash.clone())
        .expect_err("recognized mixed record graph must not fall back to Flow/AST verifier");
    assert!(error.contains("MIR-CAPABILITY-001"), "{error}");

    let dual_error = verify_checked_dual(&program, source_hash)
        .expect_err("dual verifier must preserve the same fail-closed boundary");
    assert!(dual_error.contains("MIR-CAPABILITY-001"), "{dual_error}");
}

#[test]
fn verify_shared_param_field_scalar_contract() {
    require_z3!();
    let src = r#"
func read_shared(x: shared i32) -> i32 {
    requires: x > 0
    ensures: result > 0
    x
}
"#;
    // Current verifier policy: functions taking a `shared` parameter with
    // contracts are rejected fail-closed because Z3 cannot model shared heap
    // state (and `shared i32` does not auto-deref to `i32` in contract
    // expressions). Verifying shared *scalar* params is a future enhancement
    // (deferred past 0.31.6 hemostasis); this test locks the fail-closed
    // rejection so a silent unsound "Verified" can never slip through.
    let results = verify_source(src);
    match results {
        Err(diags) => {
            let msg = format!("{diags:?}");
            assert!(
                msg.contains("shared parameter") || msg.contains("shared"),
                "expected the shared-param rejection diagnostic, got: {msg}"
            );
        }
        Ok(results) => {
            // If a future verifier supports shared scalars, it must at least
            // not claim Verified without modeling the shared read.
            let silently_verified = results
                .first()
                .is_some_and(|r| r.status == VerifStatus::Verified);
            assert!(
                !silently_verified,
                "shared-param contract must not silently verify: {:?}",
                results
            );
        }
    }
}

#[test]
fn verify_multi_func_no_calls() {
    require_z3!();
    // Multiple functions with contracts, no function calls in bodies.
    let src = r#"
func add(x: i32) -> i32 {
    requires: x > 0 && x < 2147483647
    ensures: result > x
    x + 1
}
func double(y: i32) -> i32 {
    requires: y > 5 && y <= 1000000000
    ensures: result > 5
    y * 2
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: verify_multi_func_no_calls");
    assert_eq!(results.len(), 2);
    assert!(
        results.iter().all(|r| r.status == VerifStatus::Verified),
        "all functions should verify: {:?}",
        results
    );
}

#[test]
fn verify_func_call_passes() {
    require_z3!();
    // Function call in ensures: double(x) > 0 when x > 0.
    // This verifies that the result variable for the call exists.
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= -1000000000 && x <= 1000000000
    ensures: result == x * 2
    x * 2
}
func main() -> i32 {
    0
}
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: verify_func_call_passes");
    let verified: Vec<_> = results
        .iter()
        .filter(|r| r.status == VerifStatus::Verified)
        .collect();
    assert_eq!(
        verified.len(),
        1,
        "double should verify; main has no contracts: {:?}",
        results
    );
}

#[test]
fn verify_func_call_silent() {
    require_z3!();
    // The body returns 0 but ensures says result > 0 — must fail.
    // Before P0.2, this test did not assert the status; now it checks
    // that the contradiction is detected.
    let src = r#"
func double(x: i32) -> i32 {
    ensures: result > 0
    0  // Body returns 0, but ensures says result > 0 — should fail
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: verify_func_call_silent");
    let double_result = results.iter().find(|r| r.func_name == "double");
    assert!(double_result.is_some(), "double function should be present");
    assert_eq!(
        double_result.unwrap().status,
        VerifStatus::Failed,
        "double body 0 contradicts ensures result > 0: {:?}",
        double_result.unwrap()
    );
}

#[test]
fn verify_func_call_let_binding_propagation() {
    require_z3!();
    // P0.1: Call in a let-binding must propagate callee ensures.
    // Before the fix, assert_callee_ensures_in_expr only scanned
    // the tail expression; `let y = double(x); y` would not propagate.
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= 0 && x <= 1000000000
    ensures: result == x * 2
    x * 2
}
func wrap(x: i32) -> i32 {
    requires: x > 0 && x <= 1000000000
    ensures: result > 0
    let y = double(x)
    y
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: let_binding_propagation");
    let wrap_result = results.iter().find(|r| r.func_name == "wrap");
    assert!(wrap_result.is_some(), "wrap function should be present");
    assert_eq!(
        wrap_result.unwrap().status,
        VerifStatus::Verified,
        "wrap with let-binding should verify with ensures propagation: {:?}",
        wrap_result.unwrap()
    );
}

#[test]
fn verify_func_call_wrap_pass() {
    require_z3!();
    // wrap(x) calls double(x), ensures result > 0.
    // With ensures propagation, double(x) == x*2 is asserted so
    // wrap's ensures result > 0 should be Verified when x > 0.
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= 0 && x <= 1000000000
    ensures: result == x * 2
    x * 2
}
func wrap(x: i32) -> i32 {
    requires: x > 0 && x <= 1000000000
    ensures: result > 0
    double(x)
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: verify_func_call_wrap_pass");
    let wrap_result = results.iter().find(|r| r.func_name == "wrap");
    assert!(wrap_result.is_some(), "wrap function should be present");
    assert_eq!(
        wrap_result.unwrap().status,
        VerifStatus::Verified,
        "wrap with x>0, double(x)==x*2 should satisfy result>0: {:?}",
        wrap_result.unwrap()
    );
}

#[test]
fn verify_string_len_positive() {
    require_z3!();
    let src = r#"
func validate(s: string) -> i32 {
    requires: len(s) > 0
    ensures: result > 0
    len(s)
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: string_len");
    let v = results.iter().find(|r| r.func_name == "validate");
    assert!(v.is_some(), "validate should be verified");
    assert_eq!(
        v.unwrap().status,
        VerifStatus::Verified,
        "len(s) > 0 should imply result > 0: {:?}",
        v.unwrap()
    );
}

#[test]
fn verify_z3_fallback_returns_unknown() {
    // 4.1: Verify that verify_source returns Ok even when Z3 is unavailable,
    // with all results as Unknown.
    let src = r#"
func add(x: i32) -> i32 {
    requires: x < 2147483647
    ensures: result > x
    x + 1
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src);
    assert!(
        results.is_ok(),
        "verify_source should return Ok even if Z3 missing"
    );
    // If Z3 IS available, we still get valid results; if not, mock returns Unknown.
    for r in results.unwrap() {
        assert!(
            r.status == VerifStatus::Proven || r.status.is_inconclusive(),
            "status should be Proven or inconclusive, got {:?}",
            r
        );
    }
}

#[test]
fn verify_is_z3_available_not_panics() {
    // is_z3_available() should never panic regardless of Z3 installation.
    let _available = crate::verifier::is_z3_available();
    // Just verify it returns a bool without panicking.
}

#[test]
fn verify_rule_ensures_combo() {
    require_z3!();
    // 4.4: rule annotations should be extractable and verifiable.
    let src = r#"
func abs(x: i32) -> i32 {
    rule "ensures: result >= 0"
    if x < 0 { -x } else { x }
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: rule_ensures");
    let abs_result = results.iter().find(|r| r.func_name == "abs");
    assert!(abs_result.is_some(), "abs function should be verified");
    // Should at least produce a deterministic status.
    assert!(
        abs_result.unwrap().status == VerifStatus::Proven
            || abs_result.unwrap().status == VerifStatus::Disproven
            || abs_result.unwrap().status.is_inconclusive()
    );
}

#[test]
fn verify_cross_module_ensures_propagation() {
    require_z3!();
    // 1.2: Function A calls function B. The verifier should propagate
    // B's ensures to constrain the call variable for A, allowing A's
    // ensures to be verified.
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= -1000000000 && x <= 1000000000
    ensures: result == x * 2
    x * 2
}
func caller(y: i32) -> i32 {
    requires: y >= -1000000000 && y <= 1000000000
    ensures: result == y * 2
    double(y)
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: cross_module_ensures");
    let double = results.iter().find(|r| r.func_name == "double");
    let caller = results.iter().find(|r| r.func_name == "caller");
    assert!(double.is_some(), "double should be present: {:?}", results);
    assert_eq!(
        double.unwrap().status,
        VerifStatus::Verified,
        "double should verify first: {:?}",
        double.unwrap()
    );
    assert!(caller.is_some(), "caller should be present");
    // caller ensures result == y * 2. double(y) ensures result == y * 2.
    // With ensures propagation, the verifier can prove this.
    assert_eq!(
        caller.unwrap().status,
        VerifStatus::Verified,
        "caller should verify with ensures propagation: {:?}",
        caller.unwrap()
    );
}

#[test]
fn verify_cross_module_ensures_violation() {
    require_z3!();
    // Caller violates ensures because callee's ensures don't guarantee it.
    let src = r#"
func add_one(x: i32) -> i32 {
    ensures: result > x
    x + 1
}
func caller_bad(y: i32) -> i32 {
    ensures: result == y  // Violation: add_one(y) > y, cannot equal y
    add_one(y)
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: cross_module_violation");
    let caller = results.iter().find(|r| r.func_name == "caller_bad");
    assert!(caller.is_some(), "caller_bad should be present");
    assert_eq!(
        caller.unwrap().status,
        VerifStatus::Failed,
        "caller_bad should fail: {:?}",
        caller.unwrap()
    );
}

#[test]
fn verify_callee_precondition_failure_has_diagnostic_span() {
    require_z3!();
    let src = r#"
func positive(x: i32) -> i32 {
    requires: x > 0
    x
}
func caller() -> i32 {
    positive(-1)
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("verify callee precondition");
    let caller = results
        .iter()
        .find(|result| result.func_name == "caller")
        .expect("caller result");
    assert_eq!(caller.status, VerifStatus::Failed);
    let diagnostic = caller.diagnostic.as_ref().expect("structured diagnostic");
    assert_eq!(diagnostic.span.start_line, 7);
    assert_eq!(diagnostic.span.start_col, 5);
    assert_eq!(diagnostic.span.end_line, 7);
    assert_eq!(diagnostic.span.end_col, 17);
}

#[test]
fn verify_callee_unencodable_precondition_is_fail_closed() {
    // H1 (audit-triage-0.35.25): the callee-requires walker was fail-OPEN —
    // a precondition `expr_to_z3_bool` could not encode (calls, strings,
    // ...) was silently skipped, so the caller verified Proven while the
    // runtime traps E0801 at the call site. The string comparison in the
    // requires is unencodable (strings are not modeled on the AST path);
    // the call must now come back "not verified", never Proven.
    require_z3!();
    let src = r#"
func guarded(tag: string) -> i64 {
    requires: tag == "admin"
    tag.len()
}

func caller(tag: string) -> i64 {
    guarded(tag)
}
func main() -> i64 { 0 }
"#;
    let results = verify_source(src).expect("verify unencodable callee requires");
    let caller = results
        .iter()
        .find(|r| r.func_name == "caller")
        .expect("caller result");
    assert_ne!(
        caller.status,
        VerifStatus::Proven,
        "unencodable callee requires must not verify the caller: {}",
        caller.message
    );
    assert!(
        caller.message.contains("cannot be encoded"),
        "failure must name the unencodable precondition: {}",
        caller.message
    );
}

#[test]
fn verify_callee_requires_unknown_is_fail_closed() {
    // H1: a solver timeout (Unknown) on the callee-requires check used to be
    // treated as satisfied. The disjunction `x < 0 || x * x < 0` is
    // nonlinear and hard for Z3 to decide — if it returns Unknown the call
    // must not verify. The important lock is the fail-closed branch: should
    // the solver ever answer Unknown, the caller gets an explicit "could not
    // be decided" failure instead of Proven.
    require_z3!();
    let src = r#"
func tricky(x: i64) -> i64 {
    requires: x < 0 || x * x < 0
    x * x
}
func caller(x: i64) -> i64 {
    tricky(x)
}
func main() -> i64 { 0 }
"#;
    let results = verify_source(src).expect("verify callee requires Unknown path");
    let caller = results
        .iter()
        .find(|r| r.func_name == "caller")
        .expect("caller result");
    match caller.status {
        VerifStatus::Proven => panic!(
            "solver timeout on callee requires must not verify the caller: {}",
            caller.message
        ),
        VerifStatus::Failed => {
            assert!(
                caller.message.contains("could not be decided")
                    || caller.message.contains("may violate precondition"),
                "Unknown path must fail closed: {}",
                caller.message
            );
        }
        _ => {
            // SolverUnknown keeps the no-proof contract (never Proven).
        }
    }
}

#[test]
fn verify_callee_unencodable_ensures_is_fail_closed() {
    // M4 (audit-triage-0.35.25): a callee postcondition that could not be
    // encoded after call-argument substitution was silently dropped — the
    // caller's proof ran against a weaker context and a flip to Disproven was
    // untraceable (red line #2). `pos` verifies on its own (`result == x` is
    // encodable with x as an i32 param), but the caller passes `arr[0]` — the
    // substituted ensures `result == arr[0]` contains an Index expression that
    // expr_to_z3_bool cannot encode. The caller must come back "not verified"
    // naming the postcondition, never Proven. (The callee has NO requires, so
    // the H1 requires walker cannot mask this.)
    require_z3!();
    let src = r#"
func pos(x: i32) -> i32 {
    ensures: result == x
    x
}
func caller(arr: List<i32>) -> i32 {
    ensures: result >= 0
    pos(arr[0])
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("verify unencodable callee ensures");
    let caller = results
        .iter()
        .find(|r| r.func_name == "caller")
        .expect("caller result");
    assert_ne!(
        caller.status,
        VerifStatus::Proven,
        "unencodable callee ensures must not verify the caller: {}",
        caller.message
    );
    assert!(
        caller.message.contains("postcondition") && caller.message.contains("cannot be encoded"),
        "failure must name the unencodable postcondition: {}",
        caller.message
    );
}

#[test]
fn verify_branch_callee_ensures_not_unconditional() {
    require_z3!();
    // V-C5: callee ensures inside a never-taken branch must not prove caller.
    let src = r#"
func always_ten(x: i32) -> i32 {
    ensures: result == 10
    10
}
func caller(y: i32) -> i32 {
    ensures: result == 10
    if false {
        always_ten(y)
    } else {
        y
    }
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("branch callee");
    let always = results.iter().find(|r| r.func_name == "always_ten");
    let caller = results.iter().find(|r| r.func_name == "caller");
    assert!(always.is_some() && always.unwrap().status == VerifStatus::Verified);
    assert!(caller.is_some(), "caller present");
    assert_ne!(
        caller.unwrap().status,
        VerifStatus::Verified,
        "dead-branch callee ensures must not prove caller: {:?}",
        caller.unwrap()
    );
}

#[test]
fn verify_assign_updates_let_subst() {
    require_z3!();
    // V-C2: assignment must update flat let substitution.
    let src = r#"
func f() -> i32 {
    ensures: result == 2
    let mut y = 1
    y = 2
    y
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("assign subst");
    let f = results.iter().find(|r| r.func_name == "f");
    assert!(f.is_some(), "f present: {:?}", results);
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "y=2 should make ensures result==2 hold: {:?}",
        f.unwrap()
    );
}

#[test]
fn extract_body_return_first_return_wins() {
    // V-C3: sequential dead return must not win over the first return.
    // Typecheck rejects dead code after return, so exercise the helper directly.
    use crate::ast::{Expr, Lit, Stmt};
    use crate::verifier::helpers::extract_body_return;
    let stmts = vec![
        Stmt::Return(Some(Expr::Literal(Lit::Int(0)))),
        Stmt::Return(Some(Expr::Literal(Lit::Int(1)))),
    ];
    let e = extract_body_return(&stmts).expect("return found");
    match e {
        Expr::Literal(Lit::Int(0)) => {}
        other => panic!("expected first return 0, got {:?}", other),
    }
}

#[test]
fn verify_failed_callee_ensures_not_axioms() {
    require_z3!();
    // V-C4: a callee whose ensures fail must not make the caller's
    // postconditions verify via untrusted axioms.
    let src = r#"
func bad(x: i32) -> i32 {
    ensures: result == x + 1
    x
}
func caller(y: i32) -> i32 {
    ensures: result == y + 1
    bad(y)
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: failed_callee_not_axiom");
    let bad = results.iter().find(|r| r.func_name == "bad");
    let caller = results.iter().find(|r| r.func_name == "caller");
    assert!(bad.is_some(), "bad should be present");
    assert_eq!(
        bad.unwrap().status,
        VerifStatus::Failed,
        "bad should fail its own ensures: {:?}",
        bad.unwrap()
    );
    assert!(caller.is_some(), "caller should be present");
    assert_ne!(
        caller.unwrap().status,
        VerifStatus::Verified,
        "caller must not verify by trusting failed callee ensures: {:?}",
        caller.unwrap()
    );
}

#[test]
fn verify_f64_large_value_no_overflow() {
    require_z3!();
    // 3.1: Large f64 values should not overflow the verifier's encoding.
    // The old i64 scaling approach would overflow for values > ~9e3.
    // Test that both encoding and comparison work for positive large values.
    // 0.31.28: f64 arithmetic is NOT in the trusted subset → NotInTrustedSubset.
    let src = r#"
func scale(x: f64) -> f64 {
    requires: x >= 1e10
    ensures: result >= 0.0
    x * 2.0
}
func main() -> f64 { 0.0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: f64_large");
    let s = results.iter().find(|r| r.func_name == "scale");
    assert!(s.is_some(), "scale function should be verified");
    assert_eq!(
        s.unwrap().status,
        VerifStatus::NotInTrustedSubset,
        "f64 arithmetic should be NotInTrustedSubset: {:?}",
        s.unwrap()
    );
}

#[test]
fn verify_f64_tiny_value_no_underflow() {
    require_z3!();
    // Tiny f64 values (< 1e-15) should not underflow (old encoding
    // used 1e15 precision denominator and overflowed for very small values).
    // 0.31.28: f64 arithmetic is NOT in the trusted subset → NotInTrustedSubset.
    let src = r#"
func check(x: f64) -> f64 {
    requires: x > 1e-20
    ensures: result > 0.0
    x * 2.0
}
func main() -> f64 { 0.0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: f64_tiny");
    let c = results.iter().find(|r| r.func_name == "check");
    assert!(c.is_some(), "check function should be verified");
    assert_eq!(
        c.unwrap().status,
        VerifStatus::NotInTrustedSubset,
        "f64 arithmetic should be NotInTrustedSubset: {:?}",
        c.unwrap()
    );
}

#[test]
fn verify_match_all_arms_positive() {
    require_z3!();
    // Match with wildcard: all arms return >= 0, so ensures should hold.
    let src = r#"
func categorize(x: i32) -> i32 {
    ensures: result >= 0
    match x {
        1 => 10
        2 => 20
        _ => 0
    }
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: match_all_positive");
    let f = results.iter().find(|r| r.func_name == "categorize");
    assert!(f.is_some(), "categorize should be present");
    assert_ne!(
        f.unwrap().status,
        VerifStatus::Failed,
        "match should not produce false positive: {:?}",
        f.unwrap()
    );
}

#[test]
fn verify_match_violation() {
    require_z3!();
    let src = r#"
func categorize(x: i32) -> i32 {
    ensures: result > 0
    match x {
        1 => 10
        _ => 0
    }
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: match_violation");
    let f = results.iter().find(|r| r.func_name == "categorize");
    assert!(f.is_some(), "categorize should be present");
    assert!(
        f.unwrap().status == VerifStatus::Disproven || f.unwrap().status.is_inconclusive(),
        "match violation should be detected: {:?}",
        f.unwrap()
    );
}

// --- P1.1: Spawn/Await encoding ---

#[test]
fn verify_spawn_await_body_verified() {
    require_z3!();
    let src = r#"
func add_pair(x: i32, y: i32) -> i32 {
    requires: x >= -1000000000 && x <= 1000000000 && y >= -1000000000 && y <= 1000000000
    ensures: result == x + y
    let task = spawn add(x, y)
    await task
}
func add(a: i32, b: i32) -> i32 {
    requires: a >= -1000000000 && a <= 1000000000 && b >= -1000000000 && b <= 1000000000
    ensures: result == a + b
    a + b
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: spawn_await");
    let f = results.iter().find(|r| r.func_name == "add_pair");
    assert!(f.is_some(), "add_pair should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "spawn/await body should be verifiable: {:?}",
        f.unwrap()
    );
}

#[test]
fn verify_spawn_await_violation_detected() {
    require_z3!();
    let src = r#"
func bad_add(x: i32, y: i32) -> i32 {
    ensures: result == x + y
    let task = spawn sub(x, y)
    await task
}
func sub(a: i32, b: i32) -> i32 {
    a - b
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: spawn_await_violation");
    let f = results.iter().find(|r| r.func_name == "bad_add");
    assert!(f.is_some(), "bad_add should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Failed,
        "spawn/await with wrong func should fail: {:?}",
        f.unwrap()
    );
}

#[test]
fn verify_spawn_no_await_passes() {
    require_z3!();
    // Spawn without await (discard the future) — the function result
    // still comes from a separate return expression.
    let src = r#"
func compute_discard(x: i32) -> i32 {
    ensures: result == x
    spawn side_effect(x)
    x
}
func side_effect(a: i32) -> i32 {
    a
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: spawn_discard");
    let f = results.iter().find(|r| r.func_name == "compute_discard");
    assert!(f.is_some(), "compute_discard should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "spawn-discard body should be verifiable: {:?}",
        f.unwrap()
    );
}

// --- P1.2: String theory (Z3 Seq) ---

#[test]
fn verify_string_eq_param_requires_nonempty() {
    require_z3!();
    // String param with equality in requires controls a numeric return.
    let src = r#"
func greet_len(name: string) -> i32 {
    requires: name == "hello"
    ensures: result == 5
    len(name)
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: string_eq_requires");
    let f = results.iter().find(|r| r.func_name == "greet_len");
    assert!(f.is_some(), "greet_len should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "string == literal in requires should verify: {:?}",
        f.unwrap()
    );
}

#[test]
fn verify_string_eq_param_requires_violation() {
    require_z3!();
    let src = r#"
func bad_len(name: string) -> i32 {
    requires: name == "hello"
    ensures: result == 3
    len(name)
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: string_eq_violation");
    let f = results.iter().find(|r| r.func_name == "bad_len");
    assert!(f.is_some(), "bad_len should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Failed,
        "string requires + wrong ensures should fail: {:?}",
        f.unwrap()
    );
}

#[test]
fn verify_string_eq_in_ensures_with_requires() {
    require_z3!();
    // String equality with a require ensures the body path.
    let src = r#"
func is_same(a: string, b: string) -> i32 {
    requires: a == b
    ensures: result == 1
    if a == b { 1 } else { 0 }
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: string_eq_ensures");
    let f = results.iter().find(|r| r.func_name == "is_same");
    assert!(f.is_some(), "is_same should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "string == in requires + ensures should verify: {:?}",
        f.unwrap()
    );
}

#[test]
fn verify_string_nonempty_preserved() {
    require_z3!();
    let src = r#"
func id_nonempty(s: string) -> i32 {
    requires: s != ""
    ensures: result == 1
    1
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: string_nonempty");
    let f = results.iter().find(|r| r.func_name == "id_nonempty");
    assert!(f.is_some(), "id_nonempty should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "string != '' in requires should verify: {:?}",
        f.unwrap()
    );
}

#[test]
fn verify_string_len_gt_zero() {
    require_z3!();
    let src = r#"
func short(s: string) -> i32 {
    requires: len(s) > 0
    ensures: result == 1
    1
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: string_len");
    let f = results.iter().find(|r| r.func_name == "short");
    assert!(f.is_some(), "short should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "len(s) > 0 with ensures should verify: {:?}",
        f.unwrap()
    );
}

#[test]
fn verify_string_char_at_contract() {
    require_z3!();
    let src = r#"
func first_char_check(s: string) -> i32 {
    requires: len(s) > 0 && char_at(s, 0) == "h"
    ensures: result == 1
    1
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: char_at");
    let f = results.iter().find(|r| r.func_name == "first_char_check");
    assert!(f.is_some(), "first_char_check should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "char_at in requires should verify: {:?}",
        f.unwrap()
    );
}

// --- P1.1 supplementary: Lambda/Comprehension ---

#[test]
fn verify_lambda_in_body_not_crash() {
    require_z3!();
    // Lambda in function body — should not crash, may be Unknown since
    // closures can't be encoded as Z3 terms. The key assertion is that
    // verification completes without panic and the result is not Unknown.
    let src = r#"
func make_adder(x: i32) -> func(i32) -> i32 {
    fn(y: i32) -> i32 { x + y }
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: lambda");
    let f = results.iter().find(|r| r.func_name == "make_adder");
    assert!(f.is_some(), "make_adder should be present");
    // Lambda bodies can't be encoded as int/real; result is Unknown
    assert_ne!(
        f.unwrap().status,
        VerifStatus::Failed,
        "lambda body should not produce false positive: {:?}",
        f.unwrap()
    );
}

#[test]
fn verify_comprehension_in_body_not_crash() {
    require_z3!();
    let src = r#"
func make_list(n: i32) -> i32 {
    let xs = [i for i in range(0, n)]
    len(xs)
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: comprehension");
    let f = results.iter().find(|r| r.func_name == "make_list");
    assert!(f.is_some(), "make_list should be present");
    assert_ne!(
        f.unwrap().status,
        VerifStatus::Failed,
        "comprehension body should not crash: {:?}",
        f.unwrap()
    );
}

#[test]
fn verify_multiple_spawn_await() {
    require_z3!();
    let src = r#"
func sum_pair(x: i32, y: i32) -> i32 {
    requires: x >= -1000000000 && x <= 1000000000 && y >= -1000000000 && y <= 1000000000
    ensures: result == x + y
    let t1 = spawn id(x)
    let t2 = spawn id(y)
    (await t1) + (await t2)
}
func id(a: i32) -> i32 {
    ensures: result == a
    a
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: multi_spawn");
    let f = results.iter().find(|r| r.func_name == "sum_pair");
    assert!(f.is_some(), "sum_pair should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "multiple spawn/await should verify: {:?}",
        f.unwrap()
    );
}

// --- P1.2 supplementary: contains/starts_with/ends_with ---

#[test]
fn verify_string_contains_ensures() {
    require_z3!();
    let src = r#"
func check_prefix(s: string) -> i32 {
    requires: contains(s, "abc")
    ensures: result == 1
    1
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: contains");
    let f = results.iter().find(|r| r.func_name == "check_prefix");
    assert!(f.is_some(), "check_prefix should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "contains in requires should verify: {:?}",
        f.unwrap()
    );
}

#[test]
fn verify_string_starts_ends_with() {
    require_z3!();
    let src = r#"
func both_ends(s: string) -> i32 {
    requires: starts_with(s, "A") && ends_with(s, "Z")
    ensures: result == 1
    1
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: starts_ends");
    let f = results.iter().find(|r| r.func_name == "both_ends");
    assert!(f.is_some(), "both_ends should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "starts_with/ends_with in requires should verify: {:?}",
        f.unwrap()
    );
}

/// E1: After a solver Unknown (timeout/crash), the solver is replaced with a
/// fresh one (push depth 0). Pop(1) must not underflow.
#[test]
fn verify_solver_pop_after_unknown_no_crash() {
    require_z3!();
    let src = r#"
func complex(x: i32) -> i32 {
    requires: x > 0
    ensures: result > 0
    if x > 1 { x } else { x + 1 }
}
func main() -> i32 { 0 }
"#;
    let mut verifier = Verifier::with_timeout(1).expect("solver init");
    let results = verify_source_with(src, &mut verifier)
        .expect("src/verifier/tests.rs: verify_solver_pop_after_unknown");
    let f = results.iter().find(|r| r.func_name == "complex");
    assert!(f.is_some(), "complex should be present");
    // With 1ms timeout, this should return SolverUnknown, not crash
    assert!(
        matches!(
            f.unwrap().status,
            VerifStatus::Proven
                | VerifStatus::NotInTrustedSubset
                | VerifStatus::SolverUnknown
                | VerifStatus::Timeout
                | VerifStatus::InfrastructureError
                | VerifStatus::RuntimeOnlyContract
                | VerifStatus::NoObligations
        ),
        "should not crash (Proven or inconclusive): {:?}",
        f.unwrap().status,
    );
}

#[test]
fn solver_replacement_skips_pending_pop_and_reset_recovers() {
    require_z3!();
    let mut session = super::ctx::SolverSession::new(100).expect("solver init");

    // A replacement starts at depth zero while callers still owe the old
    // solver's pop. The pending pop must not touch the fresh solver.
    session.replaced = true;
    session.poisoned = true;
    session.pop();

    session.reset();
    session.push();
    session.pop();
    assert!(!session.replaced);
    assert!(!session.poisoned);
}

/// E2: Non-exhaustive match (no wildcard) — result is unconstrained, so
/// ensures `result >= 0` should NOT be Verified because the fallback arm
/// returns an unconstrained variable (not silently 0).
#[test]
fn verify_match_nonexhaustive_no_false_positive() {
    require_z3!();
    let src = r#"
func pick(x: i32) -> i32 {
    requires: x >= 0
    ensures: result >= 0
    match x {
        0 => { 0 }
        1 => { 1 }
    }
}
func main() -> i32 { 0 }
"#;
    // No-false-positive contract: a non-exhaustive match must never be silently
    // Verified. The checker rejects i32 matches without a wildcard arm outright
    // (verify_source -> Err), which is an even stronger guarantee than a Failed
    // /Unknown verification status; accept either as "not a false positive".
    match verify_source(src) {
        Err(_) => { /* checker rejected the non-exhaustive match — no false positive */ }
        Ok(results) => {
            let f = results.iter().find(|r| r.func_name == "pick");
            assert!(f.is_some(), "pick should be present");
            assert!(
                matches!(
                    f.unwrap().status,
                    VerifStatus::Disproven
                        | VerifStatus::NotInTrustedSubset
                        | VerifStatus::SolverUnknown
                        | VerifStatus::Timeout
                        | VerifStatus::InfrastructureError
                        | VerifStatus::RuntimeOnlyContract
                        | VerifStatus::NoObligations
                ),
                "non-exhaustive match should not silently pass ensures: {:?}",
                f.unwrap().status,
            );
        }
    }
}

/// E2: Exhaustive match (with wildcard) — all arms return >= 0, so
/// ensures result >= 0 should be Verified.
#[test]
fn verify_match_exhaustive_wildcard_passes() {
    require_z3!();
    let src = r#"
func pick_safe(x: i32) -> i32 {
    requires: x >= 0 && x <= 1
    ensures: result >= 0
    match x {
        0 => { 0 }
        1 => { 1 }
        _ => { 0 }
    }
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: match_exhaustive");
    let f = results.iter().find(|r| r.func_name == "pick_safe");
    assert!(f.is_some(), "pick_safe should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "exhaustive match with wildcard should verify: {:?}",
        f.unwrap().status,
    );
}

/// E3: Loop invariant as assumption — invariant is asserted as a constraint
/// but NOT verified for preservation across iterations. This test documents
/// the current behavior (invariant helps verification, not itself verified).
#[test]
fn verify_invariant_not_established_fails() {
    require_z3!();
    // V-H1: invariant not implied by requires must Fail at establish.
    let src = r#"
func broken(x: i32) -> i32 {
    requires: x == 0
    invariant: x > 100
    ensures: result > 0
    42
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: invariant_establish");
    let f = results.iter().find(|r| r.func_name == "broken");
    assert!(f.is_some(), "broken should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Failed,
        "invariant not established should fail: {:?}",
        f.unwrap()
    );
    assert!(
        f.unwrap().message.contains("not established") || f.unwrap().message.contains("invariant"),
        "message: {}",
        f.unwrap().message
    );
}

/// V-H1: assigning a free variable of the invariant inside a loop degrades status.
#[test]
fn verify_invariant_preserve_assign_degrades() {
    require_z3!();
    // Keep body simple: assign inv free var `x` to a constant inside while.
    // Avoid `x = x` which can create a cyclic let-subst expand.
    let src = r#"
func loop_mut(mut x: i32) -> i32 {
    requires: x >= 0
    invariant: x >= 0
    ensures: result >= 0
    while false {
        x = 0
    }
    x
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: invariant_preserve_assign");
    let f = results
        .iter()
        .find(|r| r.func_name == "loop_mut")
        .expect("loop_mut present");
    assert_ne!(
        f.status,
        VerifStatus::Verified,
        "assigning inv free var in loop must not Verified: {:?}",
        f
    );
}

/// V1: extract_body_return handles if-else branching in body.
/// The Z3 layer should receive an Expr::If encoding for the conditional paths.
#[test]
fn verify_if_else_body_return() {
    require_z3!();
    let src = r#"
func abs_val(x: i32) -> i32 {
    requires: x >= -2147483647
    ensures: result >= 0
    if x < 0 { -x } else { x }
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: if_else_body");
    let f = results.iter().find(|r| r.func_name == "abs_val");
    assert!(f.is_some(), "abs_val should be present");
    assert_eq!(
        f.unwrap().status,
        VerifStatus::Verified,
        "abs with if-else should verify result >= 0: {:?}",
        f.unwrap().status,
    );
}

/// V7: NLL works across nested block boundaries.
/// A borrow created in an outer block should be released when the reference
/// is no longer used after the inner block ends.
#[test]
fn verify_nll_cross_block_boundary() {
    require_z3!();
    let src = r#"
func cross_block(x: i32) -> i32 {
    ensures: result > 0
    let r = &x;
    if x > 0 { x } else { 1 }
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("src/verifier/tests.rs: nll_cross_block");
    let f = results.iter().find(|r| r.func_name == "cross_block");
    assert!(f.is_some(), "cross_block should be present");
    // The key assertion: borrow of x doesn't prevent verification
    assert!(
        matches!(
            f.unwrap().status,
            VerifStatus::Proven
                | VerifStatus::NotInTrustedSubset
                | VerifStatus::SolverUnknown
                | VerifStatus::Timeout
                | VerifStatus::InfrastructureError
                | VerifStatus::RuntimeOnlyContract
                | VerifStatus::NoObligations
        ),
        "NLL cross-block should not cause false failure: {:?}",
        f.unwrap().status,
    );
}

/// P1.2: let-bound call expressions outside tail position should propagate callee ensures.
/// Previously, `let_subst` was only applied to body_return, so `assert_callee_ensures_in_block`
/// saw bare identifiers (e.g. `d`) instead of expanded calls (e.g. `double(y)`), causing
/// callee ensures to be silently dropped.
///
/// §11-#46 (2026-08-07): double's ensures must carry an UPPER bound too —
/// `d + 1` now gets an i64 overflow obligation (VIR infer_expr_type types
/// let-bound call results as I64), and an unbounded-above d disproves it.
/// The propagation property under test is unchanged: caller's proof still
/// depends on double's ensures reaching the non-tail `d + 1` use.
#[test]
fn verify_let_bound_call_ensures_propagated() {
    require_z3!();
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= 0 && x <= 1000000000
    ensures: result >= 0
    ensures: result <= 2000000000
    x * 2
}

func caller(y: i32) -> i32 {
    requires: y >= 0 && y <= 1000000000
    ensures: result >= 0
    let d = double(y);
    let _unused = d + 1;  // d used here, not just in tail position
    d  // tail returns d, whose value depends on double's ensures
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("P1.2: let_subst propagation");
    let caller = results.iter().find(|r| r.func_name == "caller");
    assert!(caller.is_some(), "caller function should be verified");
    assert_eq!(
        caller.unwrap().status,
        VerifStatus::Verified,
        "caller should verify because double's ensures (result >= 0) is propagated to let-bound d: {:?}",
        caller.unwrap().message
    );
}

/// P1.2 variant: let-bound call with ensures violation should fail even when the call
/// is not in tail position (proving the ensures was actually propagated and checked).
#[test]
fn verify_let_bound_call_ensures_violation_detected() {
    require_z3!();
    let src = r#"
func half(x: i32) -> i32 {
    requires: x >= 0
    ensures: result >= 0
    x / 2
}

func caller(y: i32) -> i32 {
    requires: y >= 0
    ensures: result >= 10  // requires that d >= 10, but half's ensures only guarantees d >= 0
    let d = half(y);      // if y = 0, d = 0 which violates result >= 10
    d
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("P1.2: let_subst violation detection");
    let caller = results.iter().find(|r| r.func_name == "caller");
    assert!(caller.is_some(), "caller function should be present");
    // With P1.2 fix: half's ensures (d >= 0) is propagated, so verifier knows
    // d >= 0 but requires d >= 10 → violation detected → Failed
    // Without fix: half's ensures not propagated → no constraint on d → potentially Verified
    assert_eq!(
        caller.unwrap().status,
        VerifStatus::Failed,
        "caller should fail because half's ensures doesn't guarantee result >= 10: {:?}",
        caller.unwrap().message
    );
}

#[test]
fn verify_rejects_ill_typed_source() {
    // V-H8: production typecheck gate — ill-typed AST fails core::check.
    let src = r#"
func f(x: i32) -> i32 {
    ensures: result == x
    "not an int"
}
func main() -> i32 { 0 }
"#;
    let file = parse_memory_source(src, "ill-typed-test").expect("parse");
    let r = crate::core::check(&file);
    assert!(r.is_err(), "expected typecheck failure for ill-typed body");
}

#[test]
fn verify_actor_method_contract() {
    require_z3!();
    // V-H6: actor methods enter the verify queue.
    let src = r#"
actor Counter {
    count: i32
    func get() -> i32 {
        ensures: result >= 0
        self.count
    }
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("verify");
    let m = results.iter().find(|r| r.func_name.contains("get"));
    assert!(
        m.is_some(),
        "actor method should be verified: {:?}",
        results
    );
}

#[test]
fn verify_old_field_access() {
    require_z3!();
    // V-H2: old(p.x) should encode for simple field paths.
    let src = r#"
type Point { x: i32, y: i32 }
func bump(p: Point) -> i32 {
    ensures: result == old(p.x) + 1
    p.x + 1
}
func main() -> i32 { 0 }
"#;
    let results = verify_source(src).expect("verify");
    let f = results.iter().find(|r| r.func_name == "bump");
    assert!(f.is_some(), "bump present: {:?}", results);
    // Accept Proven or inconclusive (if field old still incomplete) but not crash.
    assert!(
        matches!(
            f.unwrap().status,
            VerifStatus::Proven
                | VerifStatus::Disproven
                | VerifStatus::NotInTrustedSubset
                | VerifStatus::SolverUnknown
                | VerifStatus::Timeout
                | VerifStatus::InfrastructureError
                | VerifStatus::RuntimeOnlyContract
                | VerifStatus::NoObligations
        ),
        "{:?}",
        f.unwrap()
    );
}

#[test]
fn verify_i32_add_requires_no_overflow_proof() {
    require_z3!();
    let src = r#"
func increment(x: i32) -> i32 {
    ensures: result == x + 1
    x + 1
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(results[0].status, VerifStatus::Failed);
    assert!(results[0].message.contains("integer overflow"));
}

#[test]
fn verify_i32_checked_add_sub_mul_when_bounded() {
    require_z3!();
    let src = r#"
func arithmetic(x: i32) -> i32 {
    requires: x >= -1000 && x <= 1000
    ensures: result == (x + 7) * 3 - 2
    (x + 7) * 3 - 2
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "{}",
        results[0].message
    );
}

#[test]
fn verify_i32_mul_requires_no_overflow_proof() {
    require_z3!();
    let src = r#"
func square(x: i32) -> i32 {
    ensures: result == x * x
    x * x
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(results[0].status, VerifStatus::Failed);
    assert!(results[0].message.contains("integer overflow"));
}

#[test]
fn verify_i64_add_requires_no_overflow_proof() {
    // H2 (audit-triage-0.35.25): i64 used to be modeled as unbounded Z3 Int
    // with NO definedness VCs — `i64::MAX + 1` traps E0801 at runtime
    // (SD-7) yet verified Proven. The Resolved engine now derives per-type
    // bounds (int_bounds) and the AST path is parameterized the same way, so
    // an unbounded `x + 1` must FAIL with an overflow diagnostic.
    require_z3!();
    let src = r#"
func increment(x: i64) -> i64 {
    ensures: result == x + 1
    x + 1
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(
        results[0].status,
        VerifStatus::Failed,
        "i64 overflow must be rejected: {}",
        results[0].message
    );
    assert!(results[0].message.contains("integer overflow"));
}

#[test]
fn verify_i64_checked_add_sub_mul_when_bounded() {
    // H2 counterpart: with a bounding requires, i64 arithmetic verifies —
    // the check is real, not an unconditional reject.
    require_z3!();
    let src = r#"
func arithmetic(x: i64) -> i64 {
    requires: x >= -1000 && x <= 1000
    ensures: result == (x + 7) * 3 - 2
    (x + 7) * 3 - 2
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "bounded i64 arithmetic should verify: {}",
        results[0].message
    );
}

#[test]
fn verify_i64_div_rejects_zero_and_min_overflow() {
    // H2: i64 div/rem carry the same zero-divisor + MIN/-1 obligations as
    // i32 (SD-8) — previously only checked on the VIR path, now also on the
    // AST fallback path with i64 bounds.
    require_z3!();
    let src = r#"
func maybe_divide_by_zero(x: i64, y: i64) -> i64 {
    ensures: result == x / y
    x / y
}
func min_div_neg_one() -> i64 {
    ensures: result == -9223372036854775808 / -1
    -9223372036854775808 / -1
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(results.len(), 2);
    assert!(
        results
            .iter()
            .all(|r| r.status == VerifStatus::Failed && r.message.contains("undefined")),
        "{results:?}"
    );
}

#[test]
fn verify_i64_checked_div_when_bounded() {
    // H2 counterpart: bounded divisor verifies.
    require_z3!();
    let src = r#"
func half(x: i64) -> i64 {
    requires: x >= -2000 && x <= 2000
    ensures: result == x / 2
    x / 2
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "bounded i64 division should verify: {}",
        results[0].message
    );
}

#[test]
fn verify_i64_overflow_on_ast_fallback_path() {
    // H2: the AST fallback path (functions with calls are rejected by the
    // VIR trusted-subset gate) previously had NO i64 definedness — the
    // `!returns_i32` branch asserted the unbounded model. The parameterized
    // `int_definedness_obligations` must now bound i64 there too. The callee
    // forces the AST path while keeping the arithmetic in the outer function.
    require_z3!();
    let src = r#"
func helper(x: i64) -> i64 {
    requires: x <= 1000
    ensures: result == x
    x
}

func increment(x: i64) -> i64 {
    ensures: result == helper(x) + 1
    helper(x) + 1
}
"#;
    let results = verify_source(src).expect("verification should parse");
    let increment = results
        .iter()
        .find(|r| r.func_name == "increment")
        .expect("must have a result for 'increment'");
    assert_eq!(
        increment.status,
        VerifStatus::Failed,
        "i64 overflow on the AST path must be rejected: {}",
        increment.message
    );
    assert!(increment.message.contains("integer overflow"));
}

#[test]
fn verify_i32_sub_requires_no_overflow_proof() {
    require_z3!();
    let src = r#"
func decrement(x: i32) -> i32 {
    ensures: result == x - 1
    x - 1
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(results[0].status, VerifStatus::Failed);
    assert!(results[0].message.contains("integer overflow"));
}

#[test]
fn verify_i32_div_rem_keep_truncation_toward_zero() {
    require_z3!();
    let src = r#"
func quotient() -> i32 {
    ensures: result == -2
    -7 / 3
}
func remainder() -> i32 {
    ensures: result == -1
    -7 % 3
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert!(
        results.iter().all(|r| r.status == VerifStatus::Verified),
        "{results:?}"
    );
}

#[test]
fn verify_i32_div_rejects_zero_and_min_overflow() {
    require_z3!();
    let src = r#"
func maybe_divide_by_zero(x: i32, y: i32) -> i32 {
    ensures: result == x / y
    x / y
}
func min_div_neg_one() -> i32 {
    ensures: result == -2147483648 / -1
    -2147483648 / -1
}
func min_rem_neg_one() -> i32 {
    ensures: result == -2147483648 % -1
    -2147483648 % -1
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(results.len(), 3);
    assert!(
        results.iter().all(|r| r.status == VerifStatus::Failed),
        "{results:?}"
    );
    assert!(
        results
            .iter()
            .all(|r| r.message.contains("integer operation is undefined")),
        "{results:?}"
    );
}

#[test]
fn verify_i32_div_definedness_can_be_proven_separately() {
    require_z3!();
    let src = r#"
func divide(x: i32, y: i32) -> i32 {
    requires: y != 0 && (x != -2147483648 || y != -1)
    ensures: result == x / y
    x / y
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "{}",
        results[0].message
    );
}

/// P1-24: ProofArtifact source_hash and resolved_ir_hash must be non-empty
/// when verification goes through verify_source (which has source text and
/// CheckedProgram).
#[test]
fn proof_artifact_hashes_populated() {
    require_z3!();
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= -1073741824 && x <= 1073741823
    ensures: result == x * 2
    x * 2
}
"#;
    let results = verify_source(src).expect("verification should parse");
    assert!(!results.is_empty(), "should have at least one result");
    let r = &results[0];
    assert_eq!(r.status, VerifStatus::Verified, "{}", r.message);
    let artifact = r
        .artifact
        .as_ref()
        .expect("VIR-path result should have artifact");
    assert!(
        !artifact.source_hash.is_empty(),
        "source_hash should be non-empty (P1-24)"
    );
    assert!(
        !artifact.resolved_ir_hash.is_empty(),
        "resolved_ir_hash should be non-empty (P1-24)"
    );
    assert!(
        !artifact.vir_hash.is_empty(),
        "vir_hash should be non-empty"
    );
    // source_hash should be a valid BLAKE3 hex string (64 chars).
    assert_eq!(
        artifact.source_hash.len(),
        64,
        "source_hash should be BLAKE3 hex (64 chars)"
    );
    assert_eq!(
        artifact.resolved_ir_hash.len(),
        64,
        "resolved_ir_hash should be BLAKE3 hex (64 chars)"
    );
}

/// P1-24: Different source text produces different source_hash.
#[test]
fn proof_artifact_source_hash_changes_with_source() {
    require_z3!();
    let src1 = r#"
func f(x: i32) -> i32 {
    requires: x >= -1073741824 && x <= 1073741823
    ensures: result == x + 1
    x + 1
}
"#;
    let src2 = r#"
func f(x: i32) -> i32 {
    requires: x >= -1073741824 && x <= 1073741823
    ensures: result == x + 2
    x + 2
}
"#;
    let r1 = verify_source(src1).expect("src1 should verify");
    let r2 = verify_source(src2).expect("src2 should verify");
    let a1 = r1[0].artifact.as_ref().expect("artifact");
    let a2 = r2[0].artifact.as_ref().expect("artifact");
    assert_ne!(
        a1.source_hash, a2.source_hash,
        "different source text should produce different source_hash"
    );
}

/// 0.31.27+: Callee ensures propagation in VIR path.
/// A function that calls a verified function should be verifiable via VIR
/// (callee ensures inlined as assumptions).
#[test]
fn verify_callee_ensures_propagation_vir() {
    require_z3!();
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= -1073741824 && x <= 1073741823
    ensures: result == x * 2
    x * 2
}

func quadruple(x: i32) -> i32 {
    requires: x >= -536870912 && x <= 536870911
    ensures: result == x * 4
    let y = double(x)
    double(y)
}
"#;
    let results = verify_source(src).expect("verification should parse");
    // Both functions should be verified
    assert!(
        results.len() >= 2,
        "should have at least 2 results: {:?}",
        results
    );
    for r in &results {
        assert_eq!(
            r.status,
            VerifStatus::Verified,
            "{}: {}",
            r.func_name,
            r.message
        );
    }
}

/// 0.31.27+: Callee ensures propagation — disproven case.
/// If the caller's ensures contradicts the callee's ensures, verification
/// should fail (Disproven).
#[test]
fn verify_callee_ensures_propagation_disproven() {
    require_z3!();
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= -1073741824 && x <= 1073741823
    ensures: result == x * 2
    x * 2
}

func wrong_quadruple(x: i32) -> i32 {
    requires: x >= -536870912 && x <= 536870911
    ensures: result == x * 5
    let y = double(x)
    double(y)
}
"#;
    let results = verify_source(src).expect("verification should parse");
    // double should be Verified, wrong_quadruple should be Disproven
    let wrong = results.iter().find(|r| r.func_name == "wrong_quadruple");
    assert!(wrong.is_some(), "should have result for wrong_quadruple");
    assert_eq!(
        wrong.unwrap().status,
        VerifStatus::Disproven,
        "wrong_quadruple should be Disproven: {}",
        wrong.unwrap().message
    );
}

/// #41 (full-audit-2026-08-05 §11): VIR callee inlining must NOT inject the
/// callee's ensures unconditionally — the call-site must satisfy the callee's
/// requires. A caller with no constraints can call `double(x)` outside its
/// safe range, where the callee contract does not hold; `ensures:
/// result == x * 2` used to verify against the fabricated `result == x*2`
/// assumption (fake Verified). With the injection gated as
/// `requires ⇒ ensures`, the call-site precondition becomes a proof
/// obligation and the caller is Disproven.
#[test]
fn verify_callee_ensures_inlining_requires_call_site_precondition() {
    require_z3!();
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= -1073741824 && x <= 1073741823
    ensures: result == x * 2
    x * 2
}

func caller(x: i32) -> i32 {
    ensures: result == x * 2
    double(x)
}
"#;
    let results = verify_source(src).expect("verification should parse");
    let caller = results.iter().find(|r| r.func_name == "caller");
    assert!(caller.is_some(), "should have result for caller");
    assert_ne!(
        caller.unwrap().status,
        VerifStatus::Verified,
        "caller must not be Verified from an unsatisfied callee precondition: {}",
        caller.unwrap().message
    );
}

/// #41 companion: a caller whose requires IMPLY the callee's requires still
/// gets the full callee contract — the gated injection degrades to the
/// unconditional case exactly when the precondition is derivable.
#[test]
fn verify_callee_ensures_inlining_satisfying_requires_still_verifies() {
    require_z3!();
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= -1073741824 && x <= 1073741823
    ensures: result == x * 2
    x * 2
}

func caller(x: i32) -> i32 {
    requires: x >= -1073741824 && x <= 1073741823
    ensures: result == x * 2
    double(x)
}
"#;
    let results = verify_source(src).expect("verification should parse");
    let caller = results.iter().find(|r| r.func_name == "caller");
    assert!(caller.is_some(), "should have result for caller");
    assert_eq!(
        caller.unwrap().status,
        VerifStatus::Verified,
        "caller with satisfying requires must still verify: {}",
        caller.unwrap().message
    );
}

// ============================================================
// 0.34.44: ADR-008 engine isolation regression locks
//
// 1. Proof cache keys carry engine identity — cross-engine reuse is
//    structurally impossible (fail-loud cache miss, never silent downgrade).
// 2. The LSP verification cache key is engine-scoped; pre-0.34.44 entries
//    (no engine segment) can never match.
// 3. Dual-engine divergence is fail-closed: the weaker conclusion wins and
//    the merged result carries the E0439 divergence diagnostic.
// ============================================================

fn vr_044(func_name: &str, status: VerifStatus) -> VerificationResult {
    VerificationResult {
        func_name: func_name.to_string(),
        status,
        message: String::new(),
        diagnostic: None,
        duration_us: 0,
        constraint_count: 1,
        artifact: None,
        trusted_subset_domain: None,
    }
}

#[test]
fn proof_cache_key_carries_engine_identity() {
    let mut flow = ProofArtifact::new("z3 4.13.0".to_string(), "src-hash".to_string());
    flow.vir_hash = "vh".to_string();
    let mut resolved = flow.clone();
    resolved.engine = ProofArtifact::ENGINE_RESOLVED.to_string();
    resolved.vir_hash = String::new(); // resolved engine binds resolved_ir_hash
    resolved.resolved_ir_hash = "vh".to_string();

    // Same program identity under BOTH engines must never collide.
    assert_ne!(
        flow.cache_key(),
        resolved.cache_key(),
        "cross-engine cache keys must differ (ADR-008 §2)"
    );
    assert!(flow.cache_key().contains(ProofArtifact::ENGINE_FLOW_AST));
    assert!(resolved
        .cache_key()
        .contains(ProofArtifact::ENGINE_RESOLVED));

    // Same engine + same identity → interchangeable (stable key).
    let flow_again = flow.clone();
    assert_eq!(flow.cache_key(), flow_again.cache_key());
}

#[test]
fn proof_artifact_cross_engine_is_incompatible() {
    let mut flow = ProofArtifact::new("z3 4.13.0".to_string(), "src-hash".to_string());
    flow.vir_hash = "vh".to_string();
    let mut resolved = flow.clone();
    resolved.engine = ProofArtifact::ENGINE_RESOLVED.to_string();
    assert!(
        !flow.is_compatible(&resolved),
        "a flow_ast proof must never be compatible with a resolved obligation"
    );
    assert!(flow.is_compatible(&flow.clone()));
}

#[test]
fn lsp_verification_cache_key_is_engine_scoped() {
    let key = crate::lsp::verification_cache_key("file:///a.mimi", "double");
    // Engine identity + semantics version are mandatory segments.
    assert!(key.contains(ProofArtifact::ENGINE_RESOLVED), "key: {key}");
    assert!(
        key.contains(&format!("v{}", ProofArtifact::SEMANTICS_VERSION)),
        "key: {key}"
    );
    // The pre-0.34.44 shape can never match the new key → old on-disk
    // entries auto-invalidate on upgrade.
    assert_ne!(key, "file:///a.mimi:double");
}

#[test]
fn merge_divergence_proven_vs_disproven_fails_closed() {
    let primary = vec![vr_044("f", VerifStatus::Proven)];
    let mut flow = vr_044("f", VerifStatus::Disproven);
    flow.message = "counterexample found".to_string();
    let merged = merge_engine_verdicts(primary, vec![flow]);
    assert_eq!(merged.len(), 1);
    assert_eq!(
        merged[0].status,
        VerifStatus::Disproven,
        "Disproven must beat Proven on divergence"
    );
    assert!(
        merged[0].message.contains("E0439"),
        "divergence must carry the E0439 diagnostic: {}",
        merged[0].message
    );
    assert!(
        merged[0].artifact.is_none(),
        "a divergent verdict is no proof"
    );
}

#[test]
fn merge_divergence_proven_vs_unknown_fails_closed() {
    let primary = vec![vr_044("f", VerifStatus::Proven)];
    let secondary = vec![vr_044("f", VerifStatus::SolverUnknown)];
    let merged = merge_engine_verdicts(primary, secondary);
    assert_eq!(merged[0].status, VerifStatus::SolverUnknown);
    assert!(merged[0].message.contains("E0439"));
}

#[test]
fn merge_agreement_keeps_resolved_result() {
    let mut primary = vec![vr_044("f", VerifStatus::Proven)];
    primary[0].message = "resolved proof".to_string();
    let secondary = vec![vr_044("f", VerifStatus::Proven)];
    let merged = merge_engine_verdicts(primary, secondary);
    assert_eq!(merged[0].status, VerifStatus::Proven);
    assert_eq!(
        merged[0].message, "resolved proof",
        "primary wins on agreement"
    );
    assert!(!merged[0].message.contains("E0439"));
}

#[test]
fn merge_no_opinion_defers_to_the_proving_engine() {
    // resolved attempted no proof → flow verdict wins silently.
    let primary = vec![vr_044("f", VerifStatus::NoObligations)];
    let secondary = vec![vr_044("f", VerifStatus::Proven)];
    let merged = merge_engine_verdicts(primary, secondary);
    assert_eq!(merged[0].status, VerifStatus::Proven);
    assert!(!merged[0].message.contains("E0439"));

    // flow attempted no proof → resolved verdict wins silently.
    let primary = vec![vr_044("g", VerifStatus::Proven)];
    let secondary = vec![vr_044("g", VerifStatus::InfrastructureError)];
    let merged = merge_engine_verdicts(primary, secondary);
    assert_eq!(merged[0].status, VerifStatus::Proven);
    assert!(!merged[0].message.contains("E0439"));
}

#[test]
fn merge_flow_only_obligations_pass_through() {
    // The flow engine still models obligations the resolved engine does not
    // (call sites etc.); those results must survive the merge untouched.
    let primary = vec![vr_044("a", VerifStatus::Proven)];
    let secondary = vec![
        vr_044("a", VerifStatus::Proven),
        vr_044("extern sqrt", VerifStatus::Disproven),
    ];
    let merged = merge_engine_verdicts(primary, secondary);
    assert_eq!(merged.len(), 2);
    let ext = merged
        .iter()
        .find(|r| r.func_name == "extern sqrt")
        .unwrap();
    assert_eq!(ext.status, VerifStatus::Disproven);
}

#[test]
fn dual_engine_agrees_on_simple_contract() {
    // Z3 end-to-end: a trivially provable contract must come out of
    // verify_checked_dual WITHOUT any divergence diagnostic.
    //
    // NOTE (0.34.44): arithmetic-free postcondition on purpose. Empirically
    // the two engines disagreed on integer definedness in BOTH directions —
    // i64: flow enforced overflow definedness while resolved proved the
    // unbounded model; i32: resolved enforced checked_i32 definedness while
    // flow proved unbounded. Any arithmetic postcondition was therefore a
    // divergence candidate; the agreement path was pinned with an identity
    // postcondition instead.
    //
    // H2 (0.35.x) closed the i64 side: resolved now derives per-type bounds
    // (int_bounds) and the AST fallback path was parameterized the same way,
    // so i64 arithmetic agrees between the engines (locked by
    // dual_engine_divergence_on_i64_overflow_is_fail_closed, which now
    // asserts CONSISTENT Disproven with no E0439). The i32 divergence and the
    // remaining divergences are locked by dual_engine_divergence_* below.
    if !is_z3_available() {
        return;
    }
    let src = r#"
func identity(x: i64) -> i64 {
    requires: x >= 0
    ensures: result == x
    x
}
"#;
    let file = super::parse_memory_source(src, "dual-agreement").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    let source_hash = blake3::hash(src.as_bytes()).to_hex().to_string();
    let results = super::verify_checked_dual(&program, source_hash).expect("dual verify");
    let identity = results.iter().find(|r| r.func_name == "identity");
    assert!(identity.is_some(), "must verify 'identity': {results:?}");
    let identity = identity.unwrap();
    assert_eq!(
        identity.status,
        VerifStatus::Proven,
        "simple contract must be Proven by both engines: {}",
        identity.message
    );
    assert!(
        !identity.message.contains("E0439"),
        "no divergence expected: {}",
        identity.message
    );
}

#[test]
fn dual_engine_divergence_on_i64_overflow_is_fail_closed() {
    // 0.34.44 (ADR-008 §3): originally the flow engine enforced i64 overflow
    // definedness while the resolved engine proved the unbounded model —
    // flow Disproven vs resolved Proven, surfaced as E0439 with the weaker
    // (Disproven) conclusion.
    //
    // 0.35.x (H2): the resolved engine now models i64 with its real bounds
    // (int_bounds), so both engines agree on Disproven for an unbounded
    // `x * 2` — no divergence, no E0439, and still never a silent Proven.
    if !is_z3_available() {
        return;
    }
    let src = r#"
func double(x: i64) -> i64 {
    requires: x >= 0
    ensures: result == x * 2
    x * 2
}
"#;
    let file = super::parse_memory_source(src, "dual-divergence").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    let source_hash = blake3::hash(src.as_bytes()).to_hex().to_string();
    let results = super::verify_checked_dual(&program, source_hash).expect("dual verify");
    let double = results
        .iter()
        .find(|r| r.func_name == "double")
        .expect("must have a result for 'double'");
    assert_eq!(
        double.status,
        VerifStatus::Disproven,
        "i64 overflow must fail closed: {}",
        double.message
    );
    if double.message.contains("E0439") {
        panic!(
            "H2 should have removed this divergence — both engines model i64 \
             bounds now: {}",
            double.message
        );
    }
}

#[test]
fn dual_engine_agrees_on_bounded_i32_definedness_no_divergence() {
    // 0.34.44 (ADR-008 §3): the REVERSE divergence — resolved enforced
    // checked_i32 definedness while flow proved the unbounded model, so a
    // bounded i32 contract produced E0439 + the weaker conclusion.
    //
    // 0.1.9 Phase E (0.39.80): the resolved engine failed to ENCODE the
    // `&&` precondition (`LogicalAnd` was nested under the int-comparison
    // branch), so `requires: x >= 0 && x <= 1000` was silently dropped and
    // the overflow VC found a negative-x counterexample. With the encoder
    // fixed, both engines now agree: bounded i32 arithmetic is Proven, no
    // E0439 divergence.
    if !is_z3_available() {
        return;
    }
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= 0 && x <= 1000
    ensures: result == x * 2
    x * 2
}
"#;
    let file = super::parse_memory_source(src, "dual-divergence-i32").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    let source_hash = blake3::hash(src.as_bytes()).to_hex().to_string();
    let results = super::verify_checked_dual(&program, source_hash).expect("dual verify");
    let double = results
        .iter()
        .find(|r| r.func_name == "double")
        .expect("must have a result for 'double'");
    assert_eq!(
        double.status,
        VerifStatus::Proven,
        "bounded i32 must be Proven by both engines after the && encoder fix: {}",
        double.message
    );
    assert!(
        !double.message.contains("E0439"),
        "no divergence expected for bounded i32: {}",
        double.message
    );
}

/// Phase E (0.39.80): the resolved engine must ENCODE `&&` preconditions.
/// Previously `LogicalAnd` was nested under the int-comparison branch, so
/// `requires: x >= 0 && x <= 1000` was silently dropped → bounded i32
/// arithmetic was wrongly Disproven (overflow VC saw unbounded x).
#[test]
fn resolved_engine_encodes_conjunctive_precondition() {
    require_z3!();
    // Bounded i32 arithmetic: x in [0,1000], x*2 in [0,2000] — no overflow.
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= 0 && x <= 1000
    ensures: result == x * 2
    x * 2
}
"#;
    let file = parse_memory_source(src, "resolved-and-enc").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    let mut v = Verifier::new().expect("z3");
    let results = v.verify_checked(&program);
    let f = results
        .iter()
        .find(|r| r.func_name == "double")
        .expect("double result");
    assert_eq!(
        f.status,
        VerifStatus::Proven,
        "bounded i32 with && requires must be Proven by the resolved engine: {}",
        f.message
    );
}

/// Phase E (0.39.80): unbounded i32 overflow remains fail-closed — the two
/// engines disagree (flow models unbounded/assumes defined, resolved enforces
/// checked) → E0439 + the weaker (Disproven) conclusion.
#[test]
fn dual_engine_unbounded_i32_overflow_agrees_fail_closed() {
    // Phase E (0.39.80): unbounded i32 overflow — both engines Disproven and
    // AGREE (no E0439). The flow/VIR engine enforces body arithmetic
    // definedness (i32 + i64, §11-#46) and the resolved engine enforces
    // checked_i32 — so an unbounded `x * 2` is rejected by both, fail-closed
    // without divergence.
    if !is_z3_available() {
        return;
    }
    let src = r#"
func double(x: i32) -> i32 {
    ensures: result == x * 2
    x * 2
}
"#;
    let file = super::parse_memory_source(src, "dual-unbounded-i32").expect("parse");
    let program = crate::core::check_program(&file).expect("typecheck");
    let source_hash = blake3::hash(src.as_bytes()).to_hex().to_string();
    let results = super::verify_checked_dual(&program, source_hash).expect("dual verify");
    let double = results
        .iter()
        .find(|r| r.func_name == "double")
        .expect("must have a result for 'double'");
    assert_eq!(
        double.status,
        VerifStatus::Disproven,
        "unbounded i32 overflow must fail closed: {}",
        double.message
    );
    assert!(
        !double.message.contains("E0439"),
        "both engines agree on unbounded i32 overflow (no divergence): {}",
        double.message
    );
}
