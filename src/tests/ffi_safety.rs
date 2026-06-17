use super::*;

/// Path to a real shared library used only to reach the argument-conversion
/// phase of call_extern. The called symbols do not exist, so no C function is
/// actually invoked.
fn ffi_lib_path() -> &'static str {
    "/lib/x86_64-linux-gnu/libc.so.6"
}

fn expect_ffi_safety_error(src: &str, expected_substring: &str) {
    let result = run_source_result(src);

    assert!(result.is_err(), "expected FFI safety error, got value: {:?}", result.ok());
    let err = result.unwrap_err();
    assert!(
        err.contains("FFI safety"),
        "expected error to contain 'FFI safety', got: {}",
        err
    );
    assert!(
        err.contains(expected_substring),
        "expected error to contain '{}', got: {}",
        expected_substring,
        err
    );
}

fn expect_symbol_not_found(src: &str) {
    let _guard = super::FfiEnvLock::lock();
    std::env::set_var("MIMI_FFI_LIB", ffi_lib_path());
    let result = run_source_result(src);
    std::env::remove_var("MIMI_FFI_LIB");

    assert!(result.is_err(), "expected symbol-not-found error, got value: {:?}", result.ok());
    let err = result.unwrap_err();
    assert!(
        err.contains("failed to find symbol") || err.contains("cannot find"),
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
    expect_ffi_safety_error(src, "shared");
}

#[test]
fn local_shared_not_allowed_in_ffi() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(x: local_shared i32) -> i32;
}

func main() -> i32 {
    local_shared s = 42;
    __mimi_test_no_such_function_12345(s)
}
"#;
    expect_ffi_safety_error(src, "shared");
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
    expect_ffi_safety_error(src, "borrowed reference");
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
    expect_ffi_safety_error(src, "borrowed reference");
}

#[test]
fn record_not_allowed_in_ffi() {
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
    expect_ffi_safety_error(src, "unsupported argument type");
}

#[test]
fn list_not_allowed_in_ffi() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(xs: List<i32>) -> i32;
}

func main() -> i32 {
    let xs = [1, 2, 3];
    __mimi_test_no_such_function_12345(xs)
}
"#;
    expect_ffi_safety_error(src, "unsupported argument type");
}

#[test]
fn cap_allowed_in_ffi_with_cap_table() {
    // After STAGE2, caps are registered in the CapTable and passed as i64 handles
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
    // Cap is now allowed - the function just doesn't exist, so we get symbol-not-found
    expect_symbol_not_found(src);
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
fn string_borrow_allowed_in_ffi() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(s: string) -> i32;
}

func main() -> i32 {
    __mimi_test_no_such_function_12345("hello")
}
"#;
    expect_symbol_not_found(src);
}

// ---------------------------------------------------------------------------
// Stage 1: FFI passport types (*T, *mut T, c_shared T, c_borrow T,
// c_borrow_mut T) are allowed in extern "C" signatures but rejected everywhere
// else.
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
    assert!(check_source(src).is_ok(), "raw pointer should be allowed in extern signature");
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
    assert!(check_source(src).is_ok(), "raw mutable pointer should be allowed in extern signature");
}

#[test]
fn c_shared_allowed_in_extern_signature() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(x: c_shared i32) -> i32;
}

func main() -> i32 {
    0
}
"#;
    assert!(check_source(src).is_ok(), "c_shared should be allowed in extern signature");
}

#[test]
fn c_borrow_allowed_in_extern_signature() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(x: c_borrow i32) -> i32;
}

func main() -> i32 {
    0
}
"#;
    assert!(check_source(src).is_ok(), "c_borrow should be allowed in extern signature");
}

#[test]
fn c_borrow_mut_allowed_in_extern_signature() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(x: c_borrow_mut i32) -> i32;
}

func main() -> i32 {
    0
}
"#;
    assert!(check_source(src).is_ok(), "c_borrow_mut should be allowed in extern signature");
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
fn c_borrow_rejected_in_function_return() {
    let src = r#"
func bad() -> c_borrow i32 {
    0
}

func main() -> i32 {
    0
}
"#;
    expect_type_error(src, "FFI passport type");
}

#[test]
fn c_shared_rejected_in_type_alias() {
    let src = r#"
type CSharedInt = c_shared i32;

func main() -> i32 {
    0
}
"#;
    expect_type_error(src, "FFI passport type");
}

#[test]
fn c_borrow_mut_rejected_in_record_field() {
    let src = r#"
type Wrapper {
    ptr: c_borrow_mut i32
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
fn c_shared_rejected_in_actor_field() {
    let src = r#"
actor BadActor {
    ptr: c_shared i32
}

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
fn passport_type_rejected_in_impl_signature() {
    let src = r#"
trait PtrTrait {
    func get(x: i32) -> i32;
}

type MyType {}

impl PtrTrait for MyType {
    func get(x: c_borrow i32) -> i32 {
        x
    }
}

func main() -> i32 {
    0
}
"#;
    expect_type_error(src, "FFI passport type");
}

#[test]
fn raw_string_rejected_in_function_signature() {
    let src = r#"
func bad(s: raw_string) -> i32 {
    0
}

func main() -> i32 {
    0
}
"#;
    expect_type_error(src, "FFI passport type");
}

#[test]
fn raw_string_rejected_in_function_return() {
    let src = r#"
func bad() -> raw_string {
    "hello"
}

func main() -> i32 {
    0
}
"#;
    expect_type_error(src, "FFI passport type");
}
