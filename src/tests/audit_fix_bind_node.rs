//! Wave-1 audit-fix regression tests — bind_node.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).

use crate::ast::{
    AstNodeMeta, AstOrigin, ExternFunc, ExternParam, Field, Type, TypeAttribute, TypeDef,
    TypeDefKind,
};
use crate::ffi::node_bind::NodeBindGenerator;
use std::collections::HashMap;

fn meta() -> AstNodeMeta {
    AstNodeMeta::synthetic(AstOrigin::RuntimeSystem("audit.bind_node"))
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
fn string_ty() -> Type {
    Type::Name("string".into(), vec![])
}
fn unit_ty() -> Type {
    Type::Name("unit".into(), vec![])
}
fn raw_string_ty() -> Type {
    Type::RawString
}
fn raw_ptr_i32() -> Type {
    Type::RawPtr(Box::new(i32_ty()))
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
                    ty: Type::Name("bool".into(), vec![]),
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

/// Fix 1 (node_bind.rs:435-440): argc must be the ACTUAL count written by
/// napi_get_cb_info; missing JS args must throw a TypeError instead of reading
/// uninitialised napi_value slots; every napi_status on the marshalling path
/// must be checked.
#[test]
fn node_argc_actual_count_and_missing_args_throw() {
    let gen = NodeBindGenerator::new(HashMap::new(), "audit");
    let out = gen
        .generate(&[extern_fn(
            "add2",
            vec![("a", i32_ty()), ("b", i32_ty())],
            Some(i32_ty()),
        )])
        .unwrap();

    assert!(out.contains("size_t argc = 2;"));
    assert!(out.contains("if (napi_get_cb_info(env, info, &argc, args, NULL, NULL) != napi_ok) {"));
    // Unchecked legacy form must be gone.
    assert!(!out.contains("napi_get_cb_info(env, info, &argc, args, NULL, NULL);"));
    assert!(out.contains("if (argc < 2)"));
    assert!(out.contains(r#""mimi: add2 expects 2 argument(s)""#));
    assert!(out.contains("napi_throw_type_error"));
}

#[test]
fn node_every_napi_status_checked_via_macro() {
    let gen = NodeBindGenerator::new(HashMap::new(), "audit");
    let out = gen
        .generate(&[extern_fn(
            "add2",
            vec![("a", i32_ty()), ("b", i64_ty())],
            Some(i32_ty()),
        )])
        .unwrap();

    // The sweep macro is emitted in the preamble.
    assert!(out.contains("#define MIMI_NAPI_CHECK(env, expr)"));
    assert!(out.contains("napi_status mimi_st = (expr);"));
    // Argument extraction goes through the checked macro.
    assert!(out.contains("MIMI_NAPI_CHECK(env, napi_get_value_int32(env, args[0], &a_val));"));
    assert!(out.contains(
        "MIMI_NAPI_CHECK(env, napi_get_value_bigint_int64(env, args[1], &b_val, NULL));"
    ));
    // Result construction is checked too.
    assert!(out.contains("MIMI_NAPI_CHECK(env, napi_create_int32(env, ret, &result));"));
}

/// Fix 2 (node_bind.rs:589-602): FfiRetContract::String returns are borrowed
/// and must NOT be freed; StringOwned returns keep the free.
#[test]
fn node_string_return_borrowed_not_freed() {
    let gen = NodeBindGenerator::new(HashMap::new(), "audit");
    let out = gen
        .generate(&[
            extern_fn("borrow_str", vec![], Some(string_ty())),
            extern_fn("owned_str", vec![], Some(raw_string_ty())),
        ])
        .unwrap();

    assert!(out.contains("/* Contract FfiRetContract::String: borrowed from C — do NOT free. */"));
    assert!(out.contains("/* Contract FfiRetContract::StringOwned: owned — free after copying. */"));
    // Exactly one free — the owned return. The borrowed return must not free.
    assert_eq!(out.matches("mimi_string_free(ret);").count(), 1);
    // NULL returns become undefined instead of crashing napi_create_string_utf8.
    assert!(out.contains("if (ret == NULL)"));
}

/// Fix 2 (node_bind.rs:492-507+621-627): StringTransfer args transfer buffer
/// ownership to C; the post-call free was a double-free/UAF. Borrow/Json args
/// still free their Mimi-owned temporaries.
#[test]
fn node_string_transfer_arg_not_freed_post_call() {
    let gen = NodeBindGenerator::new(HashMap::new(), "audit");
    let out = gen
        .generate(&[
            extern_fn("take", vec![("s", raw_string_ty())], Some(unit_ty())),
            extern_fn("greet", vec![("name", string_ty())], Some(unit_ty())),
        ])
        .unwrap();

    assert!(out.contains("/* s_buf: StringTransfer — ownership moved to C; do NOT free. */"));
    assert!(!out.contains("free(s_buf)"));
    // Control: borrowed string args still free the temporary buffer.
    assert!(out.contains("free(name_buf);"));
}

/// Fix 3 (node_bind.rs:539): RawPtr/RawPtrMut args must marshal the actual JS
/// pointer address (number or BigInt), not discard it as NULL.
#[test]
fn node_raw_ptr_args_marshaled_as_address() {
    let gen = NodeBindGenerator::new(HashMap::new(), "audit");
    let out = gen
        .generate(&[extern_fn(
            "read_ptr",
            vec![("p", raw_ptr_i32())],
            Some(i32_ty()),
        )])
        .unwrap();

    assert!(out.contains("napi_valuetype p_vt;"));
    assert!(out.contains("if (p_vt == napi_bigint)"));
    assert!(out.contains("void* p_ptr = (void*)(intptr_t)p_addr;"));
    // The C call receives the marshaled pointer, not NULL.
    assert!(out.contains("int32_t ret = read_ptr(p_ptr);"));
    assert!(!out.contains("(intptr_t)NULL /* p */"));
    // d.ts: pointers accept number or BigInt addresses.
    let dts = gen
        .generate_dts(&[extern_fn(
            "read_ptr",
            vec![("p", raw_ptr_i32())],
            Some(i32_ty()),
        )])
        .unwrap();
    // Contract (verified against `mimi bindgen` output for
    // `func read_ptr(p: *i32) -> i32`): the RawPtr ARGUMENT marshals as an
    // address — Component IR maps `*T`/`*mut T` to `AbiTypeRef::Pointer`
    // (src/component/gen.rs `mimi_type_to_abi_depth`, rendered as C `T*` in
    // src/component/types.rs), and the d.ts address type is `number | bigint`
    // (node_bind.rs `mimi_type_to_ts` RawPtr/RawPtrMut arm; generated C
    // accepts both and casts `(void*)(intptr_t)addr`). The RETURN here is
    // plain `i32`, which maps to TS `number` (node_bind.rs `ret_type_to_ts`
    // Int(I32) arm — same mapping pinned for `point_sum` in
    // ffi/bindgen_tests.rs). `bigint` returns are reserved for i64 / RawPtr /
    // handle returns and are pinned separately in
    // `node_raw_ptr_returns_and_extern_prototypes` below.
    assert!(dts.contains("export function read_ptr(p: number | bigint): number;"));
}

/// Fix 3 companion + sweep: pointer returns previously emitted JS `undefined`,
/// silently discarding the value; they now surface as BigInt addresses. The
/// emitted file also declares real prototypes so pointer returns are not
/// truncated by implicit `int` declarations.
#[test]
fn node_raw_ptr_returns_and_extern_prototypes() {
    let gen = NodeBindGenerator::new(HashMap::new(), "audit");
    let out = gen
        .generate(&[
            extern_fn("alloc_buf", vec![], Some(raw_ptr_i32())),
            extern_fn(
                "add2",
                vec![("a", i32_ty()), ("b", i32_ty())],
                Some(i32_ty()),
            ),
        ])
        .unwrap();

    assert!(out.contains("void* ret = alloc_buf();"));
    assert!(out.contains(
        "MIMI_NAPI_CHECK(env, napi_create_bigint_int64(env, (int64_t)(intptr_t)ret, &result));"
    ));
    assert!(out.contains("extern void* alloc_buf(void);"));
    assert!(out.contains("extern int32_t add2(int32_t a, int32_t b);"));
    // d.ts: pointer returns are bigint.
    let dts = gen
        .generate_dts(&[extern_fn("alloc_buf", vec![], Some(raw_ptr_i32()))])
        .unwrap();
    assert!(dts.contains("export function alloc_buf(): bigint;"));
}

/// Sweep: callback trampolines check every N-API status and build a proper
/// argv array (the old emitter passed `&argv0, &argv1, ...` as separate
/// napi_call_function arguments — invalid C against the real N-API headers —
/// and emitted `fn, 0, , &result` for zero-arg callbacks).
#[test]
fn node_callback_trampolines_checked_and_argv_array() {
    let cb_ty = |params: Vec<Type>, ret: Type| Type::Func(params, Box::new(ret));
    let gen = NodeBindGenerator::new(HashMap::new(), "audit");
    let out = gen
        .generate(&[
            extern_fn(
                "apply_cb",
                vec![
                    ("f", cb_ty(vec![i32_ty(), i64_ty()], i32_ty())),
                    ("x", i32_ty()),
                ],
                Some(i32_ty()),
            ),
            extern_fn(
                "fire",
                vec![("cb", cb_ty(vec![], unit_ty()))],
                Some(unit_ty()),
            ),
        ])
        .unwrap();

    // Reference resolution is checked.
    assert!(out.contains(
        "if (napi_get_reference_value(mimi_cb_apply_cb_f_slot.env, mimi_cb_apply_cb_f_slot.ref, &fn) != napi_ok) {"
    ));
    // argv is an array; i32 uses int32 creation (width-correct), i64 BigInt.
    assert!(out.contains("napi_value argv[2];"));
    assert!(out.contains(
        "if (napi_create_int32(mimi_cb_apply_cb_f_slot.env, arg0, &argv[0]) != napi_ok) {"
    ));
    assert!(out.contains(
        "if (napi_create_bigint_int64(mimi_cb_apply_cb_f_slot.env, arg1, &argv[1]) != napi_ok) {"
    ));
    assert!(out.contains(
        "if (napi_call_function(mimi_cb_apply_cb_f_slot.env, NULL, fn, 2, argv, &result) != napi_ok) {"
    ));
    // Return extraction checked + width-correct for i32.
    assert!(out.contains("int32_t ret = 0;"));
    assert!(out.contains(
        "if (napi_get_value_int32(mimi_cb_apply_cb_f_slot.env, result, &ret) != napi_ok) {"
    ));
    // Zero-arg callbacks pass NULL argv, not an empty argument list.
    assert!(out.contains(
        "if (napi_call_function(mimi_cb_fire_cb_slot.env, NULL, fn, 0, NULL, &result) != napi_ok) {"
    ));
    // The legacy double-comma syntax bug must be gone.
    assert!(!out.contains(", ,"));
    // Callback args must be functions; non-functions throw a TypeError.
    assert!(out.contains(r#""mimi: callback argument f must be a function""#));
}

/// Fix 5 companion (node side) + prototypes: repr(C) struct fields keep the
/// C99 `bool` (1 byte) and the generated file includes stdbool.h; callback
/// parameters are declared with the name inside the function-pointer
/// declarator.
#[test]
fn node_struct_bool_field_and_callback_prototype() {
    let cb_ty = Type::Func(vec![i32_ty()], Box::new(i32_ty()));
    let gen = NodeBindGenerator::new(flags_type_defs(), "audit");
    let out = gen
        .generate(&[
            extern_fn(
                "check_flags",
                vec![("fl", Type::Name("Flags".into(), vec![]))],
                Some(Type::Name("bool".into(), vec![])),
            ),
            extern_fn(
                "apply_cb",
                vec![("f", cb_ty), ("x", i32_ty())],
                Some(i32_ty()),
            ),
        ])
        .unwrap();

    assert!(out.contains("#include <stdbool.h>"));
    assert!(out.contains("typedef struct Flags {"));
    assert!(out.contains("    bool on;"));
    assert!(out.contains("    int32_t n;"));
    assert!(out.contains("extern bool check_flags(struct Flags fl);"));
    assert!(out.contains("extern int32_t apply_cb(int32_t (*f)(int32_t), int32_t x);"));
    // Struct field extraction is checked through the macro.
    assert!(out.contains(
        "MIMI_NAPI_CHECK(env, napi_get_named_property(env, args[0], \"on\", &fl_on_val));"
    ));
    assert!(
        out.contains("MIMI_NAPI_CHECK(env, napi_get_value_bool(env, fl_on_val, &fl_struct.on));")
    );
}

/// #[repr(C)] Bad { name: string, n: i32 } — `string` has no N-API field
/// extraction (napi_extract_for_field supports only i32/i64/f64/bool).
fn bad_field_type_defs() -> HashMap<String, TypeDef> {
    let mut map = HashMap::new();
    map.insert(
        "Bad".to_string(),
        TypeDef {
            meta: meta(),
            name: "Bad".to_string(),
            pub_: true,
            kind: TypeDefKind::Record(vec![
                Field {
                    meta: meta(),
                    name: "name".to_string(),
                    ty: string_ty(),
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

/// X-12 (full-audit-2026-08-05-0656 §3.10), CONFIRMED reachable chain: the
/// checker accepts #[repr(C)] records with unsupported field types because
/// `is_valid_extern_type` (core/checker/types.rs) does not recurse into
/// record fields — `mimi check` passes `type Bad { name: string, n: i32 }`
/// in an extern signature (PoC: /tmp/opencode/w2/FFI/poc_x12_*.mimi). The old
/// emitter declared `struct Bad b_struct;` uninitialised, skipped `name` with
/// a comment, and passed the struct by value to C → indeterminate stack bytes
/// cross the ABI (UB / information leak). The wrapper must refuse the call.
#[test]
fn audit2_ffi_node_struct_arg_unsupported_field_fails_closed() {
    let gen = NodeBindGenerator::new(bad_field_type_defs(), "audit");
    let out = gen
        .generate(&[extern_fn(
            "take_bad",
            vec![("b", Type::Name("Bad".into(), vec![]))],
            Some(i32_ty()),
        )])
        .unwrap();

    // Fail-closed throw naming the offending field.
    assert!(out.contains(
        "napi_throw_type_error(env, NULL, \"mimi: unsupported field type in struct argument 'b' (field 'name')\");"
    ));
    // The struct is declared zero-initialised so the (now unreachable) call
    // site emitted afterwards still references a complete, defined variable.
    assert!(out.contains("struct Bad b_struct = {0};"));
    // The C call must be unreachable: the throw precedes it.
    let throw_pos = out
        .find("mimi: unsupported field type in struct argument")
        .expect("fail-closed throw emitted");
    let call_pos = out
        .find("take_bad(b_struct)")
        .expect("call still emitted as dead code after the early return");
    assert!(throw_pos < call_pos, "throw must precede the C call");
    // No field extraction is emitted before the throw (nothing half-marshalled).
    assert!(!out.contains("napi_get_named_property(env, args[0], \"name\""));
    assert!(!out.contains("napi_get_named_property(env, args[0], \"n\""));
}

/// X-12, the audit's literal phrasing ("type_defs 查不到该类型"): UNREACHABLE
/// by construction, and this test pins the fail-closed shape that guards it.
/// `build_contract` derives the record sets from the SAME `type_defs` map
/// (node_bind.rs), so a name absent from `type_defs` can never become
/// `StructByValue` — it lands in `FfiArgContract::Unsupported` and the
/// wrapper throws before any struct is declared. (Upstream, `mimi bindgen`
/// already rejects unresolved extern types via the checker, E0231.)
#[test]
fn audit2_ffi_node_unknown_arg_type_fails_closed_without_struct() {
    let gen = NodeBindGenerator::new(HashMap::new(), "audit");
    let out = gen
        .generate(&[extern_fn(
            "mystery",
            vec![("w", Type::Name("Ghost".into(), vec![]))],
            Some(i32_ty()),
        )])
        .unwrap();

    assert!(out.contains(
        "napi_throw_type_error(env, NULL, \"mimi: unsupported FFI argument type for parameter w\");"
    ));
    // No uninitialised struct is ever declared for the unknown type.
    assert!(!out.contains("struct Ghost"));
    assert!(!out.contains("w_struct"));
}

/// X-12 defense-in-depth: a fully marshalable record is declared
/// zero-initialised (`= {0}`) so no byte is indeterminate even if a future
/// extraction path skips a field; extraction of every field is preserved.
#[test]
fn audit2_ffi_node_supported_struct_arg_zero_initialized() {
    let gen = NodeBindGenerator::new(flags_type_defs(), "audit");
    let out = gen
        .generate(&[extern_fn(
            "check_flags",
            vec![("fl", Type::Name("Flags".into(), vec![]))],
            Some(i32_ty()),
        )])
        .unwrap();

    assert!(out.contains("struct Flags fl_struct = {0};"));
    assert!(out.contains(
        "MIMI_NAPI_CHECK(env, napi_get_named_property(env, args[0], \"on\", &fl_on_val));"
    ));
    assert!(out.contains(
        "MIMI_NAPI_CHECK(env, napi_get_named_property(env, args[0], \"n\", &fl_n_val));"
    ));
    // The call still receives the marshaled struct.
    assert!(out.contains("check_flags(fl_struct)"));
}

/// Wave-2 item 3 (wave1-review-2026-08-05 §6.6): every bindgen test was a
/// static text assertion — none compiled the generated code. Minimal gate:
/// the generated C header must pass `cc -fsyntax-only`. Follows the repo's
/// can_link() skip convention (src/tests/mod.rs): skips loudly-but-gracefully
/// when no C compiler is installed. The fixture surface mirrors
/// /tmp/opencode/w2/FFI/poc_ccgate.mimi, verified through the real
/// `mimi bindgen` pipeline.
#[test]
fn audit2_ffi_c_header_passes_cc_syntax_only() {
    if !crate::tests::can_link() {
        eprintln!(
            "SKIP audit2_ffi_c_header_passes_cc_syntax_only: no `cc` available (can_link() = false)"
        );
        return;
    }

    let cb_ty = Type::Func(vec![i32_ty(), i64_ty()], Box::new(i32_ty()));
    let header = crate::ffi::c_header::generate_c_header(
        &[
            extern_fn(
                "add2",
                vec![("a", i32_ty()), ("b", i32_ty())],
                Some(i32_ty()),
            ),
            extern_fn("borrow_str", vec![], Some(string_ty())),
            extern_fn("take", vec![("s", raw_string_ty())], Some(unit_ty())),
            extern_fn("read_ptr", vec![("p", raw_ptr_i32())], Some(i32_ty())),
            extern_fn("alloc_buf", vec![], Some(raw_ptr_i32())),
            extern_fn(
                "check_flags",
                vec![("fl", Type::Name("Flags".into(), vec![]))],
                Some(i32_ty()),
            ),
            extern_fn(
                "apply_cb",
                vec![("f", cb_ty), ("x", i32_ty())],
                Some(i32_ty()),
            ),
        ],
        flags_type_defs(),
    )
    .expect("C header generation");

    let dir = std::env::temp_dir().join(format!("mimi_audit2_ffi_hdr_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir for header gate");
    let header_path = dir.join("mimi_ffi.h");
    std::fs::write(&header_path, &header).expect("write generated header");
    let output = std::process::Command::new("cc")
        .arg("-fsyntax-only")
        .arg(&header_path)
        .output()
        .expect("spawn `cc` (can_link() was true)");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        output.status.success(),
        "generated C header failed `cc -fsyntax-only`:\n--- stderr ---\n{}\n--- header ---\n{}",
        String::from_utf8_lossy(&output.stderr),
        header
    );
}
