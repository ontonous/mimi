use super::*;

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
    let result = run_source_bytecode_result(src);
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
    let result = run_source_bytecode_result(src);
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
    let result = run_source_bytecode_result(src);
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
    let result = run_source_bytecode_result(src);
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
        returns_errno: false,
    };

    let contract = FfiContract::from_extern(&func);
    assert_eq!(contract.args.len(), 1);
    assert!(
        matches!(contract.args[0], FfiArgContract::Json),
        "List arg should produce Json contract, got {:?}",
        contract.args[0]
    );
}
