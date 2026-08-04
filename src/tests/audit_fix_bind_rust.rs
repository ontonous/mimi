//! Wave-1 audit-fix regression tests — bind_rust.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).
//!
//! String ownership contract (src/ffi/contract.rs):
//! - `FfiRetContract::String`      = BORROWED from C → the wrapper must NOT free it.
//! - `FfiRetContract::StringOwned` = owned → free with mimi_string_free.
//! - `FfiRetContract::Json`        = owned → free with mimi_string_free.
//! - `FfiArgContract::StringTransfer` = ownership moves TO C → hand over a heap
//!   pointer (CString::into_raw) and never free it after the call.

use std::collections::HashMap;

use crate::ast::{AstNodeMeta, AstOrigin, ExternFunc, ExternParam, Type};
use crate::ffi::rust_bind::RustBindGenerator;


fn fixture_meta() -> AstNodeMeta {
    AstNodeMeta::synthetic(AstOrigin::RuntimeSystem("test.audit_bind_rust"))
}

fn string_contract_funcs() -> Vec<ExternFunc> {
    vec![
        // Borrowed string return (FfiRetContract::String): must NOT be freed.
        ExternFunc {
            meta: fixture_meta(),
            name: "borrowed_msg".to_string(),
            params: vec![],
            ret: Some(Type::Name("string".to_string(), vec![])),
            requires: None,
            ensures: None,
            variadic: false,
            no_panic: false,
            returns_errno: false,
        },
        // Owned raw_string return (FfiRetContract::StringOwned): must be freed.
        ExternFunc {
            meta: fixture_meta(),
            name: "owned_msg".to_string(),
            params: vec![],
            ret: Some(Type::RawString),
            requires: None,
            ensures: None,
            variadic: false,
            no_panic: false,
            returns_errno: false,
        },
        // JSON return (FfiRetContract::Json): runtime-owned, must be freed.
        ExternFunc {
            meta: fixture_meta(),
            name: "json_msg".to_string(),
            params: vec![],
            ret: Some(Type::Name("List".to_string(), vec![])),
            requires: None,
            ensures: None,
            variadic: false,
            no_panic: false,
            returns_errno: false,
        },
        // StringTransfer argument (FfiArgContract::StringTransfer): heap
        // pointer handed to C; the wrapper must NOT free it post-call.
        ExternFunc {
            meta: fixture_meta(),
            name: "take_msg".to_string(),
            params: vec![ExternParam {
                meta: fixture_meta(),
                name: "msg".to_string(),
                ty: Type::RawString,
                cap_mode: None,
            }],
            ret: None,
            requires: None,
            ensures: None,
            variadic: false,
            no_panic: false,
            returns_errno: false,
        },
    ]
}

#[test]
fn audit_bind_rust_borrowed_string_returns_are_not_freed() {
    let gen = RustBindGenerator::new(HashMap::new(), "audit_mod");
    let out = gen
        .generate(&string_contract_funcs())
        .expect("rust bindgen string fixture");

    // Only StringOwned (owned_msg) and Json (json_msg) returns may call the
    // deallocator. The borrowed String return (borrowed_msg) must not — C
    // retains ownership (contract.rs FfiRetContract::String; interpreter
    // reference behavior warns instead of freeing, interp/ffi_runtime.rs).
    assert_eq!(
        out.matches("super::ffi_raw::mimi_string_free(raw)").count(),
        2,
        "exactly the StringOwned and Json returns must be freed; \
         borrowed String returns must NOT be freed"
    );
    // All three string-like returns copy through CStr...
    assert_eq!(out.matches("std::ffi::CStr::from_ptr(raw)").count(), 3);
    // ...with the nullptr check preserved on every path.
    assert_eq!(
        out.matches("if raw.is_null() { return String::new(); }")
            .count(),
        3
    );
}

#[test]
fn audit_bind_rust_string_transfer_arg_hands_over_heap_pointer() {
    let gen = RustBindGenerator::new(HashMap::new(), "audit_mod");
    let out = gen
        .generate(&string_contract_funcs())
        .expect("rust bindgen string fixture");

    // Hand over the heap pointer via CString::into_raw; the wrapper never
    // frees it after the call (C owns it and frees via mimi_string_free_raw).
    assert!(out.contains("msg_cstr.into_raw()"));
    assert!(!out.contains("drop(msg_cstr)"));
    // The old buggy pattern passed a local CString's borrowed pointer across
    // the ownership boundary and dropped it after the call (double-free/UAF).
    assert!(!out.contains("msg_cstr.as_ptr()"));
    // Interior NUL bytes are filtered before crossing the boundary (parity
    // with the interpreter reference behavior, interp/ffi_runtime.rs).
    assert!(out.contains("msg.bytes().filter(|&b| b != 0)"));
    // The raw extern declaration receives an owned mutable pointer, matching
    // the C header (`char*`), not a const borrow.
    assert!(out.contains("pub fn take_msg(msg: *mut c_char)"));
}
