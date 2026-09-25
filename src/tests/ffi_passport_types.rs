use super::*;

fn assert_ffi_boundary(source: &str) {
    let error = run_source_bytecode_result(source)
        .expect_err("passport and capability ABI shapes are outside scalar FFI");
    assert!(
        error.contains("MIR-FFI-DECLARATION-001") || error.contains("[E0231]"),
        "expected explicit scalar FFI boundary, got: {error}"
    );
}

fn assert_pointer_argument_rejected_by_checker(source: &str) {
    let error = run_source_bytecode_result(source)
        .expect_err("shared integers do not implicitly become raw pointers");
    assert!(
        error.contains("[E0211]"),
        "expected the pointer argument type error, got: {error}"
    );
}

/// Raw pointer declarations parse, but shared integers do not coerce to them.
#[test]
fn raw_ptr_rejects_shared_value_argument() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(x: *i32) -> i32;
}

func main() -> i32 {
    shared s = 42;
    __mimi_test_no_such_function_12345(s)
}
"#;
    assert_pointer_argument_rejected_by_checker(src);
}

/// Mutable raw pointer calls have the same explicit argument type boundary.
#[test]
fn raw_ptr_mut_rejects_shared_value_argument() {
    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(x: *mut i32) -> i32;
}

func main() -> i32 {
    shared s = 42;
    __mimi_test_no_such_function_12345(s)
}
"#;
    assert_pointer_argument_rejected_by_checker(src);
}

/// Test that cap values are registered in CapTable
#[test]
fn capability_call_is_outside_scalar_ffi_profile() {
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
    assert_ffi_boundary(src);
}

/// A scalar declaration without contracts still uses receipt-bearing MIR.
#[test]
fn scalar_ffi_without_contract_reaches_canonical_symbol_lookup() {
    // MIMI_FFI_LIB is process-global. Other tests temporarily install shared
    // libraries to exercise the canonical host-binding path, so keep this
    // missing-symbol probe isolated and deterministic under the default test
    // harness parallelism.
    let _ffi_env_guard = FfiEnvGuard::lock();
    std::env::remove_var("MIMI_FFI_LIB");

    let src = r#"
extern "C" {
    func __mimi_test_no_such_function_12345(x: i32) -> i32;
}

func main() -> i32 {
    __mimi_test_no_such_function_12345(0)
}
"#;
    // The scalar declaration is checker-owned and executes only through its
    // canonical route. Its deliberately absent symbol proves this is not the
    // receiptless AST bytecode loader.
    let error = run_source_bytecode_result(src)
        .expect_err("the declared scalar symbol is intentionally absent");
    assert!(
        error.contains("failed to find canonical MIR FFI symbol"),
        "expected canonical scalar FFI lookup, got: {error}"
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
