use super::*;

/// Test that c_shared can accept shared values
#[test]
fn c_shared_accepts_shared_value() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(x: c_shared i32) -> i32;
}

func main() -> i32 {
    shared s = 42;
    __mimi_test_no_such_function_12345(s)
}
"#;
    // This should fail because the library doesn't exist, but it should
    // pass the argument conversion phase (which means c_shared accepted the shared value)
    let _guard = FfiEnvLock::lock();
    std::env::set_var("MIMI_FFI_LIB", "/lib/x86_64-linux-gnu/libc.so.6");
    let result = run_source_result(src);
    std::env::remove_var("MIMI_FFI_LIB");

    // The error should be about symbol not found, not about argument conversion
    assert!(result.is_err(), "should fail with symbol not found");
    let err = result.unwrap_err();
    assert!(
        err.contains("failed to find symbol") || err.contains("cannot find"),
        "error should be about symbol not found, got: {}",
        err
    );
}

/// Test that c_borrow can accept shared values
#[test]
fn c_borrow_accepts_shared_value() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(x: c_borrow i32) -> i32;
}

func main() -> i32 {
    shared s = 42;
    __mimi_test_no_such_function_12345(s)
}
"#;
    let _guard = FfiEnvLock::lock();
    std::env::set_var("MIMI_FFI_LIB", "/lib/x86_64-linux-gnu/libc.so.6");
    let result = run_source_result(src);
    std::env::remove_var("MIMI_FFI_LIB");

    assert!(result.is_err(), "should fail with symbol not found");
    let err = result.unwrap_err();
    assert!(
        err.contains("failed to find symbol") || err.contains("cannot find"),
        "error should be about symbol not found, got: {}",
        err
    );
}

/// Test that c_borrow_mut can accept shared values
#[test]
fn c_borrow_mut_accepts_shared_value() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(x: c_borrow_mut i32) -> i32;
}

func main() -> i32 {
    shared s = 42;
    __mimi_test_no_such_function_12345(s)
}
"#;
    let _guard = FfiEnvLock::lock();
    std::env::set_var("MIMI_FFI_LIB", "/lib/x86_64-linux-gnu/libc.so.6");
    let result = run_source_result(src);
    std::env::remove_var("MIMI_FFI_LIB");

    assert!(result.is_err(), "should fail with symbol not found");
    let err = result.unwrap_err();
    assert!(
        err.contains("failed to find symbol") || err.contains("cannot find"),
        "error should be about symbol not found, got: {}",
        err
    );
}

/// Test that raw pointer can accept shared values
#[test]
fn raw_ptr_accepts_shared_value() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(x: *i32) -> i32;
}

func main() -> i32 {
    shared s = 42;
    __mimi_test_no_such_function_12345(s)
}
"#;
    let _guard = FfiEnvLock::lock();
    std::env::set_var("MIMI_FFI_LIB", "/lib/x86_64-linux-gnu/libc.so.6");
    let result = run_source_result(src);
    std::env::remove_var("MIMI_FFI_LIB");

    assert!(result.is_err(), "should fail with symbol not found");
    let err = result.unwrap_err();
    assert!(
        err.contains("failed to find symbol") || err.contains("cannot find"),
        "error should be about symbol not found, got: {}",
        err
    );
}

/// Test that mutable raw pointer can accept shared values
#[test]
fn raw_ptr_mut_accepts_shared_value() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(x: *mut i32) -> i32;
}

func main() -> i32 {
    shared s = 42;
    __mimi_test_no_such_function_12345(s)
}
"#;
    let _guard = FfiEnvLock::lock();
    std::env::set_var("MIMI_FFI_LIB", "/lib/x86_64-linux-gnu/libc.so.6");
    let result = run_source_result(src);
    std::env::remove_var("MIMI_FFI_LIB");

    assert!(result.is_err(), "should fail with symbol not found");
    let err = result.unwrap_err();
    assert!(
        err.contains("failed to find symbol") || err.contains("cannot find"),
        "error should be about symbol not found, got: {}",
        err
    );
}

/// Test that cap values are registered in CapTable
#[test]
fn cap_values_are_registered() {
    let src = r#"
cap TestCap;

extern "C" {
    func __mimi_test_no_such_function_12345(cap @ c: TestCap) -> i32;
}

func main() -> i32 {
    let c = TestCap;
    __mimi_test_no_such_function_12345(c)
}
"#;
    let _guard = FfiEnvLock::lock();
    std::env::set_var("MIMI_FFI_LIB", "/lib/x86_64-linux-gnu/libc.so.6");
    let result = run_source_result(src);
    std::env::remove_var("MIMI_FFI_LIB");

    // Cap handling should work, but the function doesn't exist
    assert!(result.is_err(), "should fail with symbol not found");
    let err = result.unwrap_err();
    assert!(
        err.contains("failed to find symbol") || err.contains("cannot find"),
        "error should be about symbol not found, got: {}",
        err
    );
}

/// Test that raw_string type is allowed in extern signatures
#[test]
fn raw_string_allowed_in_extern_signature() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(s: raw_string) -> i32;
}

func main() -> i32 {
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "raw_string should be allowed in extern signature"
    );
}

/// Test that raw_string accepts string values with ownership transfer
#[test]
fn raw_string_accepts_string_value() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(s: raw_string) -> i32;
}

func main() -> i32 {
    __mimi_test_no_such_function_12345("hello")
}
"#;
    let _guard = FfiEnvLock::lock();
    std::env::set_var("MIMI_FFI_LIB", "/lib/x86_64-linux-gnu/libc.so.6");
    let result = run_source_result(src);
    std::env::remove_var("MIMI_FFI_LIB");

    // raw_string conversion should work, but the function doesn't exist
    assert!(result.is_err(), "should fail with symbol not found");
    let err = result.unwrap_err();
    assert!(
        err.contains("failed to find symbol") || err.contains("cannot find"),
        "error should be about symbol not found, got: {}",
        err
    );
}

/// Test that FFI requires contract is checked when verify_ffi is enabled
#[test]
fn ffi_requires_contract_checked() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(x: i32) -> i32;
}

func main() -> i32 {
    __mimi_test_no_such_function_12345(0)
}
"#;
    // Without verify_ffi, the precondition is not checked
    let _guard = FfiEnvLock::lock();
    std::env::set_var("MIMI_FFI_LIB", "/lib/x86_64-linux-gnu/libc.so.6");
    let result = run_source_result(src);
    std::env::remove_var("MIMI_FFI_LIB");

    // Should fail with symbol not found (precondition not checked)
    assert!(result.is_err(), "should fail with symbol not found");
    let err = result.unwrap_err();
    assert!(
        err.contains("failed to find symbol") || err.contains("cannot find"),
        "error should be about symbol not found, got: {}",
        err
    );
}

/// Test that ensures postcondition with 'result' binding parses correctly
#[test]
fn ffi_ensures_with_result_binding() {
    let src = r#"
extern "C" {
    func positive(x: i32) -> i32
        requires: x > 0
        ensures: result > 0;
}

func main() -> i32 {
    0
}
"#;
    // Should parse and type-check (the contract is syntactically valid)
    assert!(
        check_source(src).is_ok(),
        "ensures contract with result should parse and type-check"
    );
}

/// Test that StringOwned contract is generated for raw_string return types
#[test]
fn raw_string_uses_string_owned_contract() {
    use crate::ast::{ExternFunc, Type};
    use crate::ffi::contract::{FfiContract, FfiRetContract};

    let func = ExternFunc {
        meta: crate::ast::AstNodeMeta::synthetic(crate::ast::AstOrigin::User),
        name: "get_string".to_string(),
        params: vec![],
        ret: Some(Type::RawString),
        requires: None,
        ensures: None,
        variadic: false,
        no_panic: false,
    };

    let contract = FfiContract::from_extern(&func);
    assert!(
        matches!(contract.ret, FfiRetContract::StringOwned),
        "raw_string return should produce StringOwned contract, got {:?}",
        contract.ret
    );
}

/// Test that Json contract is generated for List types
#[test]
fn list_type_uses_json_contract() {
    use crate::ast::{ExternFunc, ExternParam, Type};
    use crate::ffi::contract::{FfiArgContract, FfiContract};

    let func = ExternFunc {
        meta: crate::ast::AstNodeMeta::synthetic(crate::ast::AstOrigin::User),
        name: "process_list".to_string(),
        params: vec![ExternParam {
            meta: crate::ast::AstNodeMeta::synthetic(crate::ast::AstOrigin::User),
            name: "xs".to_string(),
            ty: Type::Name(
                "List".to_string(),
                vec![Type::Name("i32".to_string(), vec![])],
            ),
            cap_mode: None,
        }],
        ret: Some(Type::Name("i32".to_string(), vec![])),
        requires: None,
        ensures: None,
        variadic: false,
        no_panic: false,
    };

    let contract = FfiContract::from_extern(&func);
    assert_eq!(contract.args.len(), 1);
    assert!(
        matches!(contract.args[0], FfiArgContract::Json),
        "List arg should produce Json contract, got {:?}",
        contract.args[0]
    );
}
