//! Wave-1 audit-fix regression tests — bind_go.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).

use crate::ast::{
    AstNodeMeta, AstOrigin, ExternFunc, ExternParam, Field, Type, TypeAttribute, TypeDef,
    TypeDefKind,
};
use crate::ffi::go_bind::GoBindGenerator;
use std::collections::HashMap;

fn meta() -> AstNodeMeta {
    AstNodeMeta::synthetic(AstOrigin::RuntimeSystem("audit.bind_go"))
}

fn extern_fn(name: &str, params: Vec<(&str, Type)>, ret: Option<Type>) -> ExternFunc {
    ExternFunc {
        meta: meta(),
        name: name.to_string(),
        params: params
            .into_iter()
            .map(|(n, ty)| ExternParam {
                meta: meta(),
                name: n.to_string(),
                ty,
                cap_mode: None,
            })
            .collect(),
        ret,
        requires: None,
        ensures: None,
        variadic: false,
        no_panic: false,
        returns_errno: false,
    }
}

fn i32_ty() -> Type {
    Type::Name("i32".into(), vec![])
}
fn i64_ty() -> Type {
    Type::Name("i64".into(), vec![])
}
fn f64_ty() -> Type {
    Type::Name("f64".into(), vec![])
}
fn bool_ty() -> Type {
    Type::Name("bool".into(), vec![])
}
fn string_ty() -> Type {
    Type::Name("string".into(), vec![])
}

/// #[repr(C)] Flags { on: bool, n: i32 }
fn flags_type_defs() -> HashMap<String, TypeDef> {
    let mut map = HashMap::new();
    map.insert(
        "Flags".to_string(),
        TypeDef {
            meta: meta(),
            name: "Flags".to_string(),
            pub_: true,
            kind: TypeDefKind::Record(vec![
                Field {
                    meta: meta(),
                    name: "on".to_string(),
                    ty: bool_ty(),
                },
                Field {
                    meta: meta(),
                    name: "n".to_string(),
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

/// Fix 4 (go_bind.rs:483-487): FfiRetContract::String returns are borrowed —
/// no `defer C.mimi_string_free`, NULL-checked before C.GoString.
/// StringOwned returns are freed after copying.
#[test]
fn go_string_return_borrowed_not_freed_and_null_checked() {
    let gen = GoBindGenerator::new(HashMap::new(), "audit");
    let out = gen
        .generate(&[extern_fn("borrow_str", vec![], Some(string_ty()))])
        .unwrap();

    assert!(out.contains("\tif result == nil {"));
    assert!(out.contains("\t\treturn \"\""));
    assert!(out.contains(
        "\t// String returns are borrowed from C (FfiRetContract::String): do NOT free."
    ));
    assert!(out.contains("\treturn C.GoString(result)"));
    // Borrowed return: never freed.
    assert!(!out.contains("C.mimi_string_free(result)"));
}

/// Fix 5 (go_bind.rs:607): repr(C) bool fields were declared `int` (4 bytes)
/// while Mimi's codegen emits LLVM i8 (1 byte) — layout corruption. They now
/// cross as uint8_t/uint8 (1 byte on both sides).
#[test]
fn go_represent_c_bool_field_is_one_byte() {
    let gen = GoBindGenerator::new(flags_type_defs(), "audit");
    let out = gen
        .generate(&[extern_fn(
            "check_flags",
            vec![("fl", Type::Name("Flags".into(), vec![]))],
            Some(bool_ty()),
        )])
        .unwrap();

    // C declaration in the cgo preamble: 1-byte uint8_t, not 4-byte int.
    assert!(out.contains("typedef struct Flags {"));
    assert!(out.contains("    uint8_t on;"));
    assert!(out.contains("    int n;"));
    assert!(!out.contains("int on;"));
    assert!(out.contains("#include <stdint.h>"));
    // Go mirror struct: 1-byte uint8.
    assert!(out.contains("type Flags struct {"));
    assert!(out.contains("    On uint8"));
    // Field conversion is width-correct in both directions.
    assert!(out.contains("fl_c.on = C.uint8_t(fl.On)"));
    assert!(out.contains("fl_c.n = C.int(fl.N)"));
    // bool scalar return is converted explicitly (cgo distinct types).
    assert!(out.contains("return bool(C.check_flags(fl_c))"));
}

/// Fix 6 (go_bind.rs:159-164): package-level callback slots were accessed
/// without synchronization — concurrent callers overwrote each other and cgo
/// trampolines read the slot from C threads unsynchronized. Every access is
/// now guarded by a per-slot mutex; the mutex is never held across the C
/// call (deferred LIFO Lock/Unlock), so a synchronous C→Go upcall cannot
/// deadlock.
#[test]
fn go_callback_slots_guarded_by_mutex() {
    let cb_ty = Type::Func(vec![i32_ty(), i32_ty()], Box::new(i32_ty()));
    let gen = GoBindGenerator::new(HashMap::new(), "audit");
    let funcs = vec![extern_fn(
        "apply_callback",
        vec![("f", cb_ty), ("x", i32_ty())],
        Some(i32_ty()),
    )];
    let out = gen.generate(&funcs).unwrap();

    // sync imported only when callbacks exist.
    assert!(out.contains("import \"sync\""));
    // Slot declaration kept; per-slot mutex added.
    assert!(out.contains("var apply_callback_f_cb_slot Apply_callback_f_cb"));
    assert!(out.contains("var apply_callback_f_cb_slot_mu sync.Mutex"));

    // Trampoline snapshots the slot under the mutex.
    assert!(out.contains(
        "\tapply_callback_f_cb_slot_mu.Lock()\n\tcb := apply_callback_f_cb_slot\n\tapply_callback_f_cb_slot_mu.Unlock()"
    ));
    assert!(out.contains("return C.int(cb(int32(arg0), int32(arg1)))"));

    // Caller: set under mutex, deferred clear under mutex via LIFO ordering.
    assert!(out.contains(
        "\tapply_callback_f_cb_slot_mu.Lock()\n\tapply_callback_f_cb_slot = (Apply_callback_f_cb)(f)\n\tapply_callback_f_cb_slot_mu.Unlock()"
    ));
    assert!(out.contains("\tdefer apply_callback_f_cb_slot_mu.Unlock()"));
    assert!(out.contains("\tdefer func() { apply_callback_f_cb_slot = nil }()"));
    assert!(out.contains("\tdefer apply_callback_f_cb_slot_mu.Lock()"));

    // Module without callbacks must not import sync (unused import = Go build error).
    let out_no_cb = gen
        .generate(&[extern_fn("add", vec![("a", i32_ty())], Some(i32_ty()))])
        .unwrap();
    assert!(!out_no_cb.contains("import \"sync\""));
}

/// Sweep: cgo's C numeric types are distinct Go types — the emitted body needs
/// explicit conversions or the generated file does not compile.
#[test]
fn go_scalar_returns_explicitly_converted() {
    let gen = GoBindGenerator::new(HashMap::new(), "audit");
    let out = gen
        .generate(&[
            extern_fn(
                "add",
                vec![("a", i32_ty()), ("b", i32_ty())],
                Some(i32_ty()),
            ),
            extern_fn("counter", vec![], Some(i64_ty())),
            extern_fn("ratio", vec![], Some(f64_ty())),
            extern_fn("ok", vec![], Some(bool_ty())),
            extern_fn("handle_get", vec![], Some(i64_ty())),
        ])
        .unwrap();

    assert!(out.contains("\treturn int32(C.add(C.int(a), C.int(b)))"));
    assert!(out.contains("\treturn int64(C.counter())"));
    assert!(out.contains("\treturn float64(C.ratio())"));
    assert!(out.contains("\treturn bool(C.ok())"));
    assert!(out.contains("extern long long handle_get();"));
    assert!(out.contains("\treturn int64(C.handle_get())"));
}

/// Sweep: raw pointer args pass unsafe.Pointer straight to C void*; pointer
/// returns surface as unsafe.Pointer with an explicit conversion.
#[test]
fn go_raw_pointer_args_and_returns() {
    let gen = GoBindGenerator::new(HashMap::new(), "audit");
    let funcs = vec![
        extern_fn(
            "read_ptr",
            vec![("p", Type::RawPtr(Box::new(i32_ty())))],
            Some(i32_ty()),
        ),
        extern_fn(
            "alloc_buf",
            vec![],
            Some(Type::RawPtrMut(Box::new(i32_ty()))),
        ),
    ];
    let out = gen.generate(&funcs).unwrap();

    assert!(out.contains("func Read_ptr(p unsafe.Pointer) int32 {"));
    assert!(out.contains("extern int read_ptr(void* p);"));
    assert!(out.contains("\treturn int32(C.read_ptr(p))"));
    assert!(out.contains("extern void* alloc_buf();"));
    assert!(out.contains("func Alloc_buf() unsafe.Pointer {"));
    assert!(out.contains("\treturn unsafe.Pointer(C.alloc_buf())"));
}
