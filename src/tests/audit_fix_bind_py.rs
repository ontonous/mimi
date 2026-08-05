//! Wave-1 audit-fix regression tests — bind_py.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).
//!
//! String ownership contract (src/ffi/contract.rs):
//! - `FfiRetContract::String`      = BORROWED from C → the wrapper must NOT free it.
//! - `FfiRetContract::StringOwned` = owned → free with mimi_string_free.
//! - `FfiRetContract::Json`        = owned → free with mimi_string_free.
//! - `FfiArgContract::StringTransfer` = ownership moves TO C → hand over a heap
//!   pointer and never free it after the call.

use std::collections::HashMap;

use crate::ast::{AstNodeMeta, AstOrigin, ExternFunc, ExternParam, Type};
use crate::ffi::contract::ERRNO_CHECK_FUNC_NAMES;
use crate::ffi::py_bind::PyBindGenerator;

fn fixture_meta() -> AstNodeMeta {
    AstNodeMeta::synthetic(AstOrigin::RuntimeSystem("test.audit_bind_py"))
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
fn audit_bind_py_borrowed_string_returns_are_not_freed() {
    let gen = PyBindGenerator::new(HashMap::new(), "audit_mod");
    let out = gen
        .generate(&string_contract_funcs())
        .expect("py bindgen string fixture");

    // Only StringOwned (owned_msg) and Json (json_msg) returns may call the
    // deallocator. The borrowed String return (borrowed_msg) must not — C
    // retains ownership (contract.rs FfiRetContract::String; interpreter
    // reference behavior warns instead of freeing, interp/ffi_runtime.rs).
    assert_eq!(
        out.matches("mimi_string_free(_r)").count(),
        2,
        "exactly the StringOwned and Json returns must be freed; \
         borrowed String returns must NOT be freed"
    );
    // All three string-like returns keep the nullptr-checked copy.
    assert_eq!(out.matches("if (!_r) return std::string();").count(), 3);
}

#[test]
fn audit_bind_py_string_transfer_arg_hands_over_heap_pointer() {
    let gen = PyBindGenerator::new(HashMap::new(), "audit_mod");
    let out = gen
        .generate(&string_contract_funcs())
        .expect("py bindgen string fixture");

    // Hand over a NUL-terminated heap copy (malloc => C's free(3) is the
    // correct deallocator, mirroring interp/ffi_runtime.rs StringTransfer).
    assert!(out.contains("std::malloc(msg.size() + 1)"));
    assert!(out.contains("std::memcpy(_b, msg.c_str(), msg.size() + 1)"));
    // Exactly one transfer buffer is allocated, and it is never freed by the
    // wrapper after the call — C owns it now (double-free/UAF otherwise).
    assert_eq!(out.matches("std::malloc(").count(), 1);
    assert!(!out.contains("std::free("));
    // The old buggy pattern passed the local buffer directly to C.
    assert!(!out.contains("take_msg(msg.c_str())"));
    // The handover is documented in the generated source.
    assert!(out.contains("ownership transferred to C (StringTransfer contract)"));
}

#[test]
fn audit_bind_py_errno_follows_contract_attribute_not_name_list() {
    let make = |returns_errno: bool, name: &str| {
        vec![ExternFunc {
            meta: fixture_meta(),
            name: name.to_string(),
            params: vec![ExternParam {
                meta: fixture_meta(),
                name: "flags".to_string(),
                ty: Type::Name("i32".to_string(), vec![]),
                cap_mode: None,
            }],
            ret: Some(Type::Name("i32".to_string(), vec![])),
            requires: None,
            ensures: None,
            variadic: false,
            no_panic: false,
            returns_errno,
        }]
    };

    // SD-3: explicit #[errno] attribute enables errno checking even for names
    // outside the legacy ERRNO_CHECK_FUNC_NAMES list.
    let gen = PyBindGenerator::new(HashMap::new(), "audit_mod");
    let out = gen
        .generate(&make(true, "my_custom_opener"))
        .expect("py bindgen errno fixture");
    assert!(out.contains("errno = 0;"));
    assert!(out.contains("PyErr_SetFromErrno(PyExc_OSError)"));

    // Same name without the attribute and not in the legacy list: no errno
    // checking.
    let out = gen
        .generate(&make(false, "my_custom_opener"))
        .expect("py bindgen errno fixture");
    assert!(!out.contains("errno = 0;"));

    // Deprecation transition: unannotated name that IS in the legacy list
    // still gets errno checking (via FfiContract::check_errno).
    let out = gen
        .generate(&make(false, "open"))
        .expect("py bindgen errno fixture");
    assert!(out.contains("errno = 0;"));
}

#[test]
fn audit_bind_py_fork_removed_from_errno_name_list() {
    // SD-4 deleted fork() isolation; the legacy errno name list must not
    // resurrect errno guessing for it. The rest of the list is kept for the
    // SD-3 deprecation transition.
    assert!(!ERRNO_CHECK_FUNC_NAMES.contains(&"fork"));
    assert!(ERRNO_CHECK_FUNC_NAMES.contains(&"open"));
}
