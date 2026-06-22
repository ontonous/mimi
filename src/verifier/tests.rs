use super::*;
use super::helpers::*;
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
    fn verify_body_satisfies_ensures() {
        require_z3!();
        let src = r#"
func double(x: i32) -> i32 {
    requires: x >= 0
    ensures: result == x * 2
    x * 2
}
"#;
        let results = verify_source(src).expect("src/verifier/tests.rs:40 unwrap failed");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Verified,
            "body `x * 2` should satisfy ensures `result == x * 2`: {}", results[0].message);
    }

    #[test]
    fn verify_body_violates_ensures() {
        require_z3!();
        let src = r#"
func wrong(x: i32) -> i32 {
    requires: x >= 0
    ensures: result == x * 2
    x * 3
}
"#;
        let results = verify_source(src).expect("src/verifier/tests.rs:56 unwrap failed");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Failed);
        let diag = results[0].diagnostic.as_ref().expect("src/verifier/tests.rs:59 unwrap failed");
        assert!(diag.message.contains("result ="), "narrative should show result value: {}", diag.message);
    }

    #[test]
    fn verify_result_binding_in_counterexample() {
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
        let diag = results[0].diagnostic.as_ref().expect("src/verifier/tests.rs:75 unwrap failed");
        assert!(diag.message.contains("result ="), "should show result value in narrative");
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
        assert_eq!(results[0].status, VerifStatus::Verified,
            "x > 0 && result == x should satisfy result > 0");
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
        let diag = results[0].diagnostic.as_ref().expect("src/verifier/tests.rs:109 unwrap failed");
        assert!(diag.message.contains("result ="), "should show result in narrative");
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
        let diag = results[0].diagnostic.as_ref().expect("src/verifier/tests.rs:126 unwrap failed");
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
        assert_eq!(results[0].status, VerifStatus::Verified,
            "body returns x unchanged, ensures result == old(x) should hold: {}", results[0].message);
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
        assert_eq!(results[0].status, VerifStatus::Failed,
            "body returns x+1, ensures result == old(x) should fail");
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
        let ext: Vec<_> = results.iter().filter(|r| r.func_name.contains("extern")).collect();
        assert_eq!(ext.len(), 1, "extern func should be verified");
        assert_eq!(ext[0].status, VerifStatus::Verified,
            "extern ensures should be consistent: {}", ext[0].message);
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
        let ext: Vec<_> = results.iter().filter(|r| r.func_name.contains("extern")).collect();
        assert_eq!(ext.len(), 1, "extern func should be verified");
        assert_eq!(ext[0].status, VerifStatus::Verified,
            "extern requires+ensures should be consistent: {}", ext[0].message);
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
        let ext: Vec<_> = results.iter().filter(|r| r.func_name.contains("extern")).collect();
        assert_eq!(ext.len(), 1);
        assert_eq!(ext[0].status, VerifStatus::Failed,
            "contradictory requires should fail: {}", ext[0].message);
        assert!(ext[0].message.contains("unsatisfiable"));
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
        let ext: Vec<_> = results.iter().filter(|r| r.func_name.contains("extern")).collect();
        assert_eq!(ext.len(), 0, "extern func without contracts should be skipped");
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
        assert!(func_names.contains(&"extern identity"), "extern identity should be in results: {:?}", func_names);
        assert!(func_names.contains(&"main"), "main should be in results: {:?}", func_names);
    }

    // --- extract_body_return: if/else branch coverage ---

    #[test]
    fn verify_if_else_body_all_paths_verified() {
        require_z3!();
        let src = r#"
func abs(x: i32) -> i32 {
    requires: true
    ensures: result >= 0
    if x >= 0 { x } else { -x }
}
"#;
        let results = verify_source(src).expect("src/verifier/tests.rs:277 unwrap failed");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Verified,
            "abs with if/else should be verified: {}", results[0].message);
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
        assert_eq!(results[0].status, VerifStatus::Failed,
            "bad_abs with if/else should fail (else branch x-1 can be negative)");
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
        assert_eq!(results[0].status, VerifStatus::Verified,
            "nested if/else should be verified: {}", results[0].message);
    }

    #[test]
    fn verify_if_else_body_with_requires() {
        require_z3!();
        let src = r#"
func add_or_mul(x: i32, y: i32) -> i32 {
    requires: x >= 0 && y >= 0
    ensures: result >= 0
    if x > y { x + y } else { x * y }
}
"#;
        let results = verify_source(src).expect("src/verifier/tests.rs:325 unwrap failed");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Verified,
            "add_or_mul with if/else should be verified: {}", results[0].message);
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
        assert_eq!(results[0].status, VerifStatus::Verified,
            "f64 ensures should be verified: {}", results[0].message);
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
        assert_eq!(results[0].status, VerifStatus::Failed,
            "negate should fail: result = -x violates ensures result > 0.0");
        let diag = results[0].diagnostic.as_ref().expect("src/verifier/tests.rs:363 unwrap failed");
        assert!(diag.message.contains("result"), "should include result in narrative");
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
        assert!(results.iter().all(|r| r.status == VerifStatus::Verified),
            "no-requires extern should be Verified: {:?}", results);
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
        assert_eq!(results[0].status, VerifStatus::Verified,
            "requires fd >= 0 && size > 0 should satisfy read's preconditions: {}", results[0].message);
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
        assert_eq!(results[0].status, VerifStatus::Failed,
            "read(-1, 0, size) should fail: fd is negative");
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
        assert_eq!(results[0].status, VerifStatus::Failed,
            "strlen(s) without guard should fail: s could be empty");
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
        assert_eq!(results[0].status, VerifStatus::Verified,
            "strlen(s) with guard should be Verified: {}", results[0].message);
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
        let ok_results: Vec<_> = results.iter().filter(|r| r.func_name.starts_with("ok_caller")).collect();
        assert_eq!(ok_results.len(), 2);
        assert!(ok_results.iter().all(|r| r.status == VerifStatus::Verified),
            "ok_caller should pass: {:?}", ok_results);
        let bad_results: Vec<_> = results.iter().filter(|r| r.func_name.starts_with("bad_caller")).collect();
        assert_eq!(bad_results.len(), 2);
        assert!(bad_results.iter().any(|r| r.status == VerifStatus::Failed),
            "bad_caller should have at least one failure: {:?}", bad_results);
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
        assert_eq!(results[0].status, VerifStatus::Verified,
            "invariant as constraint should verify: {:?}", results[0]);
    }

    #[test]
    fn verify_invariant_with_ensures() {
        require_z3!();
        let src = r#"
func add_one(x: i32) -> i32 {
    requires: x > 0
    ensures: result > x
    invariant: x > 0
    x + 1
}
"#;
        let results = verify_source(src).expect("src/verifier/tests.rs: verify_invariant_with_ensures");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Verified,
            "invariant + ensures should verify: {:?}", results[0]);
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
        assert_eq!(results[0].status, VerifStatus::Verified,
            "f64 add and compare should verify: {:?}", results[0]);
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
        let results = verify_source(src).expect("src/verifier/tests.rs: verify_record_field_access_int");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Verified,
            "record field access in contract should verify: {:?}", results[0]);
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
        assert_eq!(results[0].status, VerifStatus::Failed,
            "record field violation should be detected: {:?}", results[0]);
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
        let results = verify_source(src).expect("src/verifier/tests.rs: verify_shared_param_field_scalar_contract");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifStatus::Verified,
            "shared scalar param contract should verify: {:?}", results[0]);
    }

    #[test]
    fn verify_multi_func_no_calls() {
        require_z3!();
        // Multiple functions with contracts, no function calls in bodies.
        let src = r#"
func add(x: i32) -> i32 {
    requires: x > 0
    ensures: result > x
    x + 1
}
func double(y: i32) -> i32 {
    requires: y > 5
    ensures: result > 5
    y * 2
}
"#;
        let results = verify_source(src).expect("src/verifier/tests.rs: verify_multi_func_no_calls");
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.status == VerifStatus::Verified),
            "all functions should verify: {:?}", results);
    }
