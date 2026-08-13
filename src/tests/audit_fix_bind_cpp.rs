//! Wave-1 audit-fix regression tests — bind_cpp.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).

use std::collections::HashMap;

use crate::ast::{AstNodeMeta, AstOrigin, ExternFunc, ExternParam, Type};
use crate::ffi::cpp_bind;

// ---------------------------------------------------------------------------
// Fixtures (harness style mirrors src/ffi/bindgen_tests.rs)
// ---------------------------------------------------------------------------

fn meta() -> AstNodeMeta {
    AstNodeMeta::synthetic(AstOrigin::RuntimeSystem("test.audit_bind_cpp"))
}

fn param(name: &str, ty: Type) -> ExternParam {
    ExternParam {
        meta: meta(),
        name: name.to_string(),
        ty,
        cap_mode: None,
    }
}

fn func(name: &str, params: Vec<ExternParam>, ret: Option<Type>) -> ExternFunc {
    ExternFunc {
        meta: meta(),
        name: name.to_string(),
        params,
        ret,
        requires: None,
        ensures: None,
        variadic: false,
        no_panic: false,
        returns_errno: false,
    }
}

fn i32_ty() -> Type {
    Type::Name("i32".to_string(), vec![])
}

fn i64_ty() -> Type {
    Type::Name("i64".to_string(), vec![])
}

fn gen(funcs: &[ExternFunc]) -> String {
    cpp_bind::CppBindGenerator::new(HashMap::new(), "auditmod")
        .generate(funcs)
        .unwrap()
}

// ---------------------------------------------------------------------------
// [HIGH] Fix 1a: FfiRetContract::String is borrowed — the C++ wrapper must
// NOT free it (heap corruption). StringOwned must keep freeing.
// ---------------------------------------------------------------------------

#[test]
fn audit_cpp_borrowed_string_return_is_not_freed() {
    let out = gen(&[func(
        "peek_label",
        vec![],
        Some(Type::Name("string".to_string(), vec![])),
    )]);
    // Wrapper still returns MimiString (public API shape preserved) ...
    assert!(out.contains("inline MimiString peek_label()"));
    // ... but wraps the borrowed pointer with owned=false ...
    assert!(
        out.contains("MimiString mimi_ret(::peek_label(), false);"),
        "borrowed String return must construct MimiString with owned=false:\n{}",
        out
    );
    // ... and the destructor only frees owned wrappers.
    assert!(out.contains("~MimiString() { if (data_ && owned_) mimi_string_free(data_); }"));
    // The borrowed wrapper itself never frees its pointer.
    assert!(!out.contains("mimi_string_free(mimi_ret"));
}

#[test]
fn audit_cpp_json_return_is_freed_after_copy() {
    let out = gen(&[func(
        "fetch_json",
        vec![],
        Some(Type::Name("List".to_string(), vec![])),
    )]);
    assert!(out.contains("char* mimi_json_raw = ::fetch_json();"));
    assert!(out.contains("std::string mimi_ret(mimi_json_raw ? mimi_json_raw : \"\");"));
    assert!(
        out.contains("if (mimi_json_raw) mimi_string_free(mimi_json_raw);"),
        "Json return buffer must be freed after copying:\n{}",
        out
    );
}

// ---------------------------------------------------------------------------
// [HIGH] Fix 1a (cont.): Json returns are owned per contract — the old
// generic arm copied the char* into std::string and leaked it.
// ---------------------------------------------------------------------------

#[test]
fn audit_cpp_json_arg_still_borrowed_for_call() {
    // Json args are borrowed by C for the duration of the call only — the
    // std::string buffer stays alive, no malloc needed.
    let out = gen(&[func(
        "send_json",
        vec![param("payload", Type::Name("List".to_string(), vec![]))],
        None,
    )]);
    assert!(out.contains("auto payload_cstr = payload.c_str();"));
    assert!(out.contains("::send_json(payload_cstr);"));
    assert!(!out.contains("std::malloc(payload.size()"));
}

// ---------------------------------------------------------------------------
// Fix 6 (general pass): RawPtr args were cast to void* at the call site,
// which does not convert to the extern's typed pointer parameter in C++.
// Mirror the c_header.rs ABI (typed pointers).
// ---------------------------------------------------------------------------

#[test]
fn audit_cpp_raw_ptr_arg_uses_typed_pointer_cast() {
    let out = gen(&[func(
        "write_cell",
        vec![
            param("p", Type::RawPtrMut(Box::new(i32_ty()))),
            param("v", i32_ty()),
        ],
        None,
    )]);
    assert!(
        out.contains("static_cast<int32_t*>(p)"),
        "RawPtr arg must cast to the extern's typed pointer parameter:\n{}",
        out
    );
    assert!(!out.contains("static_cast<void*>(p)"));
}

// ---------------------------------------------------------------------------
// Fix 6 (general pass): unsupported arg/ret types must fail closed instead of
// silently passing/returning values.
// ---------------------------------------------------------------------------

#[test]
fn audit_cpp_unsupported_arg_fails_closed() {
    // "Widget" is not a record type -> FfiArgContract::Unsupported.
    let out = gen(&[func(
        "mystery",
        vec![param("w", Type::Name("Widget".to_string(), vec![]))],
        Some(i32_ty()),
    )]);
    assert!(
        out.contains(
            "throw std::runtime_error(\"mimi FFI: unsupported argument type 'Widget' for parameter 'w'\");"
        ),
        "unsupported arg types must raise, not silently pass through:\n{}",
        out
    );
}

#[test]
fn audit_cpp_unsupported_ret_fails_closed() {
    let out = gen(&[func(
        "produce",
        vec![],
        Some(Type::Name("Gadget".to_string(), vec![])),
    )]);
    assert!(
        out.contains("throw std::runtime_error(\"mimi FFI: unsupported return type 'Gadget'\");"),
        "unsupported return types must raise, not report success:\n{}",
        out
    );
}

// ---------------------------------------------------------------------------
// Regression pin: callback trampoline widths stay at declared scalar widths
// (cpp_bind was already correct; keep it aligned with jni_bind fix 5).
// ---------------------------------------------------------------------------

#[test]
fn audit_cpp_callback_trampoline_uses_declared_widths() {
    let out = gen(&[func(
        "apply_cb",
        vec![param(
            "f",
            Type::Func(vec![i32_ty(), i64_ty()], Box::new(i32_ty())),
        )],
        Some(i32_ty()),
    )]);
    assert!(out.contains(
        "extern \"C\" int32_t mimi_cb_apply_cb_f_trampoline(int32_t arg0, int64_t arg1)"
    ));
    assert!(out.contains("std::function<int32_t(int32_t, int64_t)> apply_cb_f_cb"));
}
