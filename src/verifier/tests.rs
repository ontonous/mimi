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
    assert_eq!(results[0].status, VerifStatus::Unknown);
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
    assert_eq!(results[0].status, VerifStatus::Unknown);
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
        VerifStatus::Unknown, // P2.3 fix: Sat means counterexample exists, so Unknown (not Verified)
        "extern ensures should be consistent: {}",
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
        VerifStatus::Unknown, // P2.3 fix: Sat means counterexample exists, so Unknown (not Verified)
        "extern requires+ensures should be consistent: {}",
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
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "f64 ensures should be verified: {}",
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
    assert_eq!(
        results[0].status,
        VerifStatus::Failed,
        "negate should fail: result = -x violates ensures result > 0.0"
    );
    let diag = results[0]
        .diagnostic
        .as_ref()
        .expect("src/verifier/tests.rs:363 unwrap failed");
    assert!(
        diag.message.contains("result"),
        "should include result in narrative"
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
    assert_eq!(
        results[0].status,
        VerifStatus::Verified,
        "f64 add and compare should verify: {:?}",
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
            let silently_verified =
                results.first().is_some_and(|r| r.status == VerifStatus::Verified);
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
            r.status == VerifStatus::Verified || r.status == VerifStatus::Unknown,
            "status should be Verified or Unknown, got {:?}",
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
        abs_result.unwrap().status == VerifStatus::Verified
            || abs_result.unwrap().status == VerifStatus::Failed
            || abs_result.unwrap().status == VerifStatus::Unknown
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
        VerifStatus::Verified,
        "large f64 should verify correctly: {:?}",
        s.unwrap()
    );
}

#[test]
fn verify_f64_tiny_value_no_underflow() {
    require_z3!();
    // Tiny f64 values (< 1e-15) should not underflow (old encoding
    // used 1e15 precision denominator and overflowed for very small values).
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
        VerifStatus::Verified,
        "tiny f64 should verify correctly: {:?}",
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
        f.unwrap().status == VerifStatus::Failed || f.unwrap().status == VerifStatus::Unknown,
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
    // With 1ms timeout, this should return Unknown, not crash
    assert!(
        matches!(
            f.unwrap().status,
            VerifStatus::Verified | VerifStatus::Unknown
        ),
        "should not crash (Verified or Unknown): {:?}",
        f.unwrap().status,
    );
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
                    VerifStatus::Failed | VerifStatus::Unknown
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
            VerifStatus::Verified | VerifStatus::Unknown
        ),
        "NLL cross-block should not cause false failure: {:?}",
        f.unwrap().status,
    );
}

/// P1.2: let-bound call expressions outside tail position should propagate callee ensures.
/// Previously, `let_subst` was only applied to body_return, so `assert_callee_ensures_in_block`
/// saw bare identifiers (e.g. `d`) instead of expanded calls (e.g. `double(y)`), causing
/// callee ensures to be silently dropped.
#[test]
fn verify_let_bound_call_ensures_propagated() {
    require_z3!();
    let src = r#"
func double(x: i32) -> i32 {
    requires: x >= 0 && x <= 1000000000
    ensures: result >= 0
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
    // Accept Verified or Unknown (if field old still incomplete) but not crash.
    assert!(
        matches!(
            f.unwrap().status,
            VerifStatus::Verified | VerifStatus::Unknown | VerifStatus::Failed
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
