use super::*;

/// The scalar FFI route reaches symbol lookup only after checker-owned MIR
/// receipt validation. libc is used for deliberately missing test symbols.
fn ffi_lib_path() -> &'static str {
    "/lib/x86_64-linux-gnu/libc.so.6"
}

fn expect_scalar_ffi_boundary(src: &str) {
    let err = run_source_bytecode_result(src)
        .expect_err("unsupported FFI shape must not reach the legacy runtime");
    assert!(
        err.contains("MIR-FFI-DECLARATION-001") || err.contains("[E0231]"),
        "expected stable scalar FFI boundary diagnostic, got: {err}"
    );
}

fn expect_symbol_not_found(src: &str) {
    let _guard = super::FfiEnvGuard::set(std::path::Path::new(ffi_lib_path()));
    let result = run_source_bytecode_result(src);

    assert!(
        result.is_err(),
        "expected symbol-not-found error, got value: {:?}",
        result.ok()
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("failed to find symbol")
            || err.contains("failed to find canonical MIR FFI symbol")
            || err.contains("cannot find"),
        "expected symbol-not-found error, got: {}",
        err
    );
}

fn expect_type_error(src: &str, expected_substring: &str) {
    let result = check_source(src);
    assert!(result.is_err(), "expected type-check error, got Ok");
    let errors = result.unwrap_err();
    let messages: Vec<String> = errors.iter().map(|d| d.message.clone()).collect();
    let combined = messages.join("\n");
    assert!(
        combined.contains(expected_substring),
        "expected error to contain '{}', got:\n{}",
        expected_substring,
        combined
    );
}

#[test]
fn shared_not_allowed_in_ffi() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(x: shared i32) -> i32;
}

func main() -> i32 {
    shared s = 42;
    __mimi_test_no_such_function_12345(s)
}
"#;
    expect_scalar_ffi_boundary(src);
}

#[test]
fn local_shared_keyword_removed() {
    // 0.35.39: `local_shared` was culled. The program is now a parse error
    // (plain identifier, not a keyword).
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(x: local_shared i32) -> i32;
}

func main() -> i32 {
    local_shared s = 42;
    __mimi_test_no_such_function_12345(s)
}
"#;
    assert!(
        run_source_bytecode_result(src).is_err(),
        "local_shared is no longer a keyword and must be rejected"
    );
}

#[test]
fn immutable_borrow_not_allowed_in_ffi() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(x: &i32) -> i32;
}

func main() -> i32 {
    let x = 42;
    __mimi_test_no_such_function_12345(&x)
}
"#;
    expect_scalar_ffi_boundary(src);
}

#[test]
fn mutable_borrow_not_allowed_in_ffi() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(x: &mut i32) -> i32;
}

func main() -> i32 {
    let mut x = 42;
    __mimi_test_no_such_function_12345(&mut x)
}
"#;
    expect_scalar_ffi_boundary(src);
}

#[test]
fn record_ffi_rejected_outside_scalar_profile() {
    // Aggregate FFI remains outside the scalar Canonical MIR profile.
    let src = r#"
type Point {
    x: i32
    y: i32
}

extern "C" {
    func __mimi_test_no_such_function_12345(p: Point) -> i32;
}

func main() -> i32 {
    let p = Point { x: 1, y: 2 };
    __mimi_test_no_such_function_12345(p)
}
"#;
    expect_scalar_ffi_boundary(src);
}

#[test]
fn list_ffi_rejected_outside_scalar_profile() {
    // Managed aggregate FFI remains outside the scalar Canonical MIR profile.
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(xs: List<i32>) -> i32;
}

func main() -> i32 {
    let xs = [1, 2, 3];
    __mimi_test_no_such_function_12345(xs)
}
"#;
    expect_scalar_ffi_boundary(src);
}

#[test]
fn cap_ffi_rejected_outside_scalar_profile() {
    // Capability handles do not cross the scalar FFI boundary implicitly.
    let src = r#"
cap FileReadCap;

extern "C" {
    func __mimi_test_no_such_function_12345(c: FileReadCap) -> i32;
}

func main() -> i32 {
    let c = FileReadCap;
    __mimi_test_no_such_function_12345(c)
}
"#;
    expect_scalar_ffi_boundary(src);
}

#[test]
fn scalar_int_allowed_in_ffi() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(x: i32) -> i32;
}

func main() -> i32 {
    __mimi_test_no_such_function_12345(42)
}
"#;
    expect_symbol_not_found(src);
}

#[test]
fn scalar_float_allowed_in_ffi() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(x: f64) -> f64;
}

func main() -> f64 {
    __mimi_test_no_such_function_12345(3.14)
}
"#;
    expect_symbol_not_found(src);
}

#[test]
fn scalar_bool_allowed_in_ffi() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(x: bool) -> i32;
}

func main() -> i32 {
    __mimi_test_no_such_function_12345(true)
}
"#;
    expect_symbol_not_found(src);
}

#[test]
fn string_ffi_is_outside_scalar_profile() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(s: string) -> i32;
}

func main() -> i32 {
    __mimi_test_no_such_function_12345("hello")
}
"#;
    expect_scalar_ffi_boundary(src);
}

// ---------------------------------------------------------------------------
// Stage 1: FFI passport types (*T, *mut T) are allowed in extern "C"
// signatures but rejected everywhere else.
// ---------------------------------------------------------------------------

#[test]
fn raw_ptr_allowed_in_extern_signature() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(x: *i32) -> i32;
}

func main() -> i32 {
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "raw pointer should be allowed in extern signature"
    );
}

#[test]
fn raw_ptr_mut_allowed_in_extern_signature() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(x: *mut i32) -> i32;
}

func main() -> i32 {
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "raw mutable pointer should be allowed in extern signature"
    );
}

#[test]
fn raw_ptr_rejected_in_function_signature() {
    let src = r#"
func bad(x: *i32) -> i32 {
    0
}

func main() -> i32 {
    0
}
"#;
    expect_type_error(src, "FFI passport type");
}

#[test]
fn raw_ptr_rejected_in_enum_variant() {
    let src = r#"
type OptPtr { Some(*i32) | None }

func main() -> i32 {
    0
}
"#;
    expect_type_error(src, "FFI passport type");
}

#[test]
fn passport_type_rejected_in_trait_signature() {
    let src = r#"
trait PtrTrait {
    func get(x: *i32) -> i32;
}

func main() -> i32 {
    0
}
"#;
    expect_type_error(src, "FFI passport type");
}

#[test]
fn unsafe_extern_allows_shared_type() {
    let src = r#"
unsafe extern "C" {
    func process(data: shared i32) -> i32
}
func main() -> i32 {
    0
}
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "unsafe extern should accept shared type, got: {:?}",
        result.err()
    );
}

#[test]
fn regular_extern_rejects_shared_type() {
    let src = r#"
extern "C" {
    func process(data: shared i32) -> i32
}
func main() -> i32 {
    0
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "regular extern should reject shared type");
}

/// 追加 C: `?` on extern "C" calls is rejected — FFI failures are Faults, not Rejected.
#[test]
fn ffi_try_operator_rejected_on_extern_call() {
    let src = r#"
extern "C" {
    func risky_ffi() -> i32
}
func main() -> i32 {
    let x = risky_ffi()?
    x
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "? on extern call should be rejected");
    let errors = result.unwrap_err();
    let messages: Vec<String> = errors.iter().map(|d| d.message.clone()).collect();
    let combined = messages.join("\n");
    assert!(
        combined.contains("E0428") || combined.contains("FFI failures are Faults"),
        "expected E0428 error about FFI Faults, got:\n{}",
        combined
    );
}
