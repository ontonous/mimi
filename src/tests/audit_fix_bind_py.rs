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

// ---------------------------------------------------------------------------
// batch5 P1-31: pybind callback trampolines must catch C++/Python exceptions
// escaping the user callback instead of terminating through the C ABI.
// ---------------------------------------------------------------------------

#[test]
fn audit_bind_py_callback_trampoline_catches_exceptions() {
    let func = ExternFunc {
        meta: fixture_meta(),
        name: "apply_cb".to_string(),
        params: vec![ExternParam {
            meta: fixture_meta(),
            name: "f".to_string(),
            ty: Type::Func(
                vec![
                    Type::Name("i32".to_string(), vec![]),
                    Type::Name("i64".to_string(), vec![]),
                ],
                Box::new(Type::Name("i32".to_string(), vec![])),
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
    let gen = PyBindGenerator::new(HashMap::new(), "audit_mod");
    let out = gen.generate(&[func]).expect("py bindgen callback fixture");
    assert!(
        out.contains("try {"),
        "must guard callback invocation:\n{out}"
    );
    assert!(out.contains("return g_apply_cb_f_cb(arg0, arg1);"));
    assert!(
        out.contains("} catch (...) {"),
        "must catch all exceptions crossing extern \"C\":\n{out}"
    );
}
