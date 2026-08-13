//! Wave-1 audit-fix regression tests — bind_jni.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).

use std::collections::HashMap;

use crate::ast::{
    AstNodeMeta, AstOrigin, ExternFunc, ExternParam, Field, Type, TypeAttribute, TypeDef,
    TypeDefKind,
};
use crate::ffi::jni_bind;

// ---------------------------------------------------------------------------
// Fixtures (harness style mirrors src/ffi/bindgen_tests.rs)
// ---------------------------------------------------------------------------

fn meta() -> AstNodeMeta {
    AstNodeMeta::synthetic(AstOrigin::RuntimeSystem("test.audit_bind_jni"))
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

fn point_type_defs() -> HashMap<String, TypeDef> {
    let mut map = HashMap::new();
    map.insert(
        "Point".to_string(),
        TypeDef {
            meta: meta(),
            name: "Point".to_string(),
            pub_: true,
            kind: TypeDefKind::Record(vec![
                Field {
                    meta: meta(),
                    name: "x".to_string(),
                    ty: i32_ty(),
                },
                Field {
                    meta: meta(),
                    name: "y".to_string(),
                    ty: i32_ty(),
                },
            ]),
            generics: vec![],
            derives: vec![],
            attributes: vec![TypeAttribute::ReprC],
        },
    );
    map
}

fn gen_c(funcs: &[ExternFunc]) -> String {
    jni_bind::JniBindGenerator::new(HashMap::new(), "auditmod")
        .generate_c(funcs)
        .unwrap()
}

fn gen_c_with_types(type_defs: HashMap<String, TypeDef>, funcs: &[ExternFunc]) -> String {
    jni_bind::JniBindGenerator::new(type_defs, "auditmod")
        .generate_c(funcs)
        .unwrap()
}

fn gen_java(funcs: &[ExternFunc]) -> String {
    jni_bind::JniBindGenerator::new(HashMap::new(), "auditmod")
        .generate_java(funcs)
        .unwrap()
}

// ---------------------------------------------------------------------------
// [HIGH] Fix 4a: FfiRetContract::String is borrowed — the JNI bridge must NOT
// free it (heap corruption). StringOwned/Json must keep freeing.
// ---------------------------------------------------------------------------

#[test]
fn audit_jni_borrowed_string_return_is_not_freed() {
    let c = gen_c(&[func(
        "peek_label",
        vec![],
        Some(Type::Name("string".to_string(), vec![])),
    )]);
    assert!(c.contains("JNIEXPORT jstring JNICALL Java_Auditmod_peek_label"));
    assert!(c.contains("mimi_ret = (*env)->NewStringUTF(env, raw_ret);"));
    assert!(
        c.contains("/* borrowed from C — do NOT free */"),
        "borrowed String return must not be freed:\n{}",
        c
    );
    assert!(
        !c.contains("mimi_string_free(raw_ret)"),
        "borrowed String return must not be freed:\n{}",
        c
    );
}

#[test]
fn audit_jni_json_return_still_freed() {
    // List return -> Json: owned string, freed after copying; the bridge type
    // is jstring (old code fell into the wildcard arm and returned 0 as jlong).
    let json = gen_c(&[func(
        "fetch_list",
        vec![],
        Some(Type::Name("List".to_string(), vec![])),
    )]);
    assert!(json.contains("JNIEXPORT jstring JNICALL Java_Auditmod_fetch_list"));
    assert!(
        json.contains("mimi_string_free(raw_ret);"),
        "Json return must be freed:\n{}",
        json
    );
    let java = gen_java(&[func(
        "fetch_list",
        vec![],
        Some(Type::Name("List".to_string(), vec![])),
    )]);
    assert!(java.contains("public static native String fetch_list();"));
}

#[test]
fn audit_jni_borrowed_string_arg_still_released() {
    // StringBorrow keeps the pre-existing (correct) get/release pairing.
    let c = gen_c(&[func(
        "greet",
        vec![param("name", Type::Name("string".to_string(), vec![]))],
        None,
    )]);
    assert!(c.contains("const char* name_str = (*env)->GetStringUTFChars(env, name, NULL);"));
    assert!(c.contains("if (name_str) (*env)->ReleaseStringUTFChars(env, name, name_str);"));
}

// ---------------------------------------------------------------------------
// [HIGH] Fix 2: struct-by-value returns emitted `ret.<field>` while the
// declared variable is `mimi_struct_ret` — uncompilable for ANY struct return.
// ---------------------------------------------------------------------------

#[test]
fn audit_jni_struct_return_uses_declared_variable() {
    let c = gen_c_with_types(
        point_type_defs(),
        &[func(
            "make_point",
            vec![],
            Some(Type::Name("Point".to_string(), vec![])),
        )],
    );
    assert!(c.contains("struct Point mimi_struct_ret = make_point();"));
    assert!(
        c.contains("(*env)->SetIntField(env, ret_obj, ret_x_fid, (int32_t)mimi_struct_ret.x);"),
        "struct field stores must read from mimi_struct_ret:\n{}",
        c
    );
    assert!(c.contains("(*env)->SetIntField(env, ret_obj, ret_y_fid, (int32_t)mimi_struct_ret.y);"));
    // The old, undeclared `ret` variable must be gone.
    assert!(!c.contains("_fid, ret.x)"));
    assert!(!c.contains("_fid, ret.y)"));
}

// ---------------------------------------------------------------------------
// [HIGH] Fix 3: RawPtr/RawPtrMut args fell through to `(intptr_t)NULL`,
// discarding the Java-supplied pointer. Marshal the jlong address.
// ---------------------------------------------------------------------------

#[test]
fn audit_jni_raw_ptr_arg_is_marshalled() {
    let c = gen_c(&[func(
        "write_cell",
        vec![
            param("p", Type::RawPtrMut(Box::new(i32_ty()))),
            param("v", i32_ty()),
        ],
        None,
    )]);
    assert!(
        c.contains("write_cell((void*)(intptr_t)p, v);"),
        "RawPtr arg must pass the Java-supplied address:\n{}",
        c
    );
    assert!(
        !c.contains("(intptr_t)NULL"),
        "RawPtr args must not be discarded as NULL:\n{}",
        c
    );
    let java = gen_java(&[func(
        "write_cell",
        vec![
            param("p", Type::RawPtrMut(Box::new(i32_ty()))),
            param("v", i32_ty()),
        ],
        None,
    )]);
    assert!(java.contains("public static native void write_cell(long p, int v);"));
}

// ---------------------------------------------------------------------------
// Fix 6 (general pass): pointer/handle returns used to fall into the wildcard
// arm and silently return 0. Marshal the actual value.
// ---------------------------------------------------------------------------

#[test]
fn audit_jni_pointer_return_is_marshalled() {
    let c = gen_c(&[func(
        "alloc_cell",
        vec![],
        Some(Type::RawPtrMut(Box::new(i32_ty()))),
    )]);
    assert!(
        c.contains("jlong mimi_ret = (jlong)(intptr_t)(alloc_cell());"),
        "pointer returns must marshal the address, not 0:\n{}",
        c
    );
    let java = gen_java(&[func(
        "alloc_cell",
        vec![],
        Some(Type::RawPtrMut(Box::new(i32_ty()))),
    )]);
    assert!(java.contains("public static native long alloc_cell();"));
}

// ---------------------------------------------------------------------------
// [HIGH] Fix 5: the callback trampoline mapped i32 -> int64_t while the extern
// expects int32_t (*)(int32_t, ...) — ABI width mismatch. Mirror declared
// scalar widths (py_bind.rs:493 / cpp_bind.rs:412).
// ---------------------------------------------------------------------------

#[test]
fn audit_jni_callback_trampoline_uses_declared_widths() {
    let c = gen_c(&[func(
        "apply_cb",
        vec![param(
            "f",
            Type::Func(vec![i32_ty(), i64_ty()], Box::new(i32_ty())),
        )],
        Some(i32_ty()),
    )]);
    assert!(
        c.contains("static int32_t mimi_cb_apply_cb_f_trampoline(int32_t arg0, int64_t arg1)"),
        "trampoline signature must mirror declared scalar widths:\n{}",
        c
    );
    // No blanket 64-bit widening of the i32 slots.
    assert!(!c.contains("int64_t mimi_cb_apply_cb_f_trampoline"));
    assert!(!c.contains("int64_t arg0"));
    // The JNI-side narrowing casts are still in place.
    assert!(c.contains("jint jarg0 = (jint)arg0;"));
    assert!(c.contains("jlong jarg1 = (jlong)arg1;"));
    assert!(c.contains("return (int32_t)jret;"));
}

// ---------------------------------------------------------------------------
// Fix 6 (general pass): unsupported arg/ret types must fail closed (throw a
// pending Java exception) instead of silently passing NULL / returning 0.
// ---------------------------------------------------------------------------

#[test]
fn audit_jni_unsupported_arg_fails_closed() {
    // "Widget" is not a record type -> FfiArgContract::Unsupported.
    let c = gen_c(&[func(
        "mystery",
        vec![param("w", Type::Name("Widget".to_string(), vec![]))],
        Some(i32_ty()),
    )]);
    assert!(
        c.contains("mimi FFI: unsupported argument type 'Widget' for parameter 'w'"),
        "unsupported arg types must raise, not pass NULL:\n{}",
        c
    );
    assert!(c.contains("(*env)->ThrowNew(env, mimi_exc,"));
    assert!(c.contains("return 0; /* pending exception is delivered */"));
    // The bogus call must not happen before the throw (the early return
    // guarantees it is unreachable).
    let throw_pos = c.find("ThrowNew").unwrap();
    let call_pos = c
        .find("mystery((intptr_t)NULL")
        .expect("deferred call emitted after the throw");
    assert!(throw_pos < call_pos, "throw must precede the call");
}

#[test]
fn audit_jni_unsupported_ret_fails_closed() {
    let c = gen_c(&[func(
        "produce",
        vec![],
        Some(Type::Name("Gadget".to_string(), vec![])),
    )]);
    assert!(
        c.contains("mimi FFI: unsupported return type 'Gadget'"),
        "unsupported return types must raise, not report success:\n{}",
        c
    );
    assert!(c.contains("jlong mimi_ret = 0;"));
}
