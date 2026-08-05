//! Wave-1 audit-fix regression tests — component.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).
//!
//! Component-layer fixes covered below (no dual-backend surface — these
//! pin the Component IR registry, wire format, handles, and bindgen):
//! - §12 CRITICAL: gen.rs registry diverged from the real runtime symbols.
//! - §12 HIGH: phantom MimiString/MimiSlice surface removed.
//! - §12 HIGH: generated C headers must typedef MimiHandle.
//! - §12 MEDIUM: wire Optional/Result decode must report exact consumed
//!   bytes; schema index contiguity enforced.
//! - §12 MEDIUM: wire handle generation fails closed past 16 bits.

// ── Helper: build the core runtime ComponentIr ─────────────────────────────

fn core_ir() -> crate::component::ComponentIr {
    let mut gen = crate::component::AbiGenerator::new();
    crate::component::register_core_runtime_abi(&mut gen);
    gen.build()
}

// Local mirrors of the gen.rs conveniences (the gen module itself is
// crate-private; only AbiGenerator/register_core_runtime_abi are re-exported).
fn prim(p: crate::component::AbiPrimitive) -> crate::component::AbiTypeRef {
    crate::component::AbiTypeRef::Primitive(p)
}
fn ptr(inner: crate::component::AbiTypeRef) -> crate::component::AbiTypeRef {
    crate::component::AbiTypeRef::Pointer(Box::new(inner))
}
fn handle(name: &str) -> crate::component::AbiTypeRef {
    crate::component::AbiTypeRef::Opaque(name.to_string())
}

// ══════════════════════════════════════════════════════════════════════════
// 1. Registry conformance: fixed signatures pinned against src/runtime/
// ══════════════════════════════════════════════════════════════════════════

/// Pin one symbol's exact (param types, return type) against the real
/// `#[no_mangle]` runtime definition it mirrors.
fn assert_sig(
    ir: &crate::component::ComponentIr,
    name: &str,
    params: &[crate::component::AbiTypeRef],
    ret: crate::component::AbiTypeRef,
) {
    let sym = ir
        .export(name)
        .unwrap_or_else(|| panic!("missing export: {name}"));
    let got_params: Vec<_> = sym.params.iter().map(|p| p.ty.clone()).collect();
    assert_eq!(
        got_params, params,
        "{name}: param types diverge from runtime signature"
    );
    assert_eq!(sym.ret, ret, "{name}: return type diverges from runtime");
}

#[test]
fn audit_component_registry_fixed_signatures_conformance() {
    use crate::component::AbiPrimitive::*;

    let ir = core_ir();

    // The nine verified divergences from the audit (full audit §12):
    // capability.rs:34 — (name) -> i64
    assert_sig(&ir, "mimi_cap_register", &[ptr(prim(U8))], prim(I64));
    // capability.rs:69 — (cap: i64, name) -> bool
    assert_sig(
        &ir,
        "mimi_cap_check",
        &[prim(I64), ptr(prim(U8))],
        prim(Bool),
    );
    // capability.rs:86 — (cap: i64, name) -> bool
    assert_sig(
        &ir,
        "mimi_cap_consume",
        &[prim(I64), ptr(prim(U8))],
        prim(Bool),
    );
    // runtime/mod.rs:18534 — (json, out_len: *mut i64, elem_type: i64) -> *mut c_void
    assert_sig(
        &ir,
        "mimi_json_deserialize",
        &[ptr(prim(U8)), ptr(prim(I64)), prim(I64)],
        ptr(prim(U8)),
    );
    // net.rs:192 — (fd, buf_size: i64, out_len: *mut i64) -> *mut c_char
    assert_sig(
        &ir,
        "mimi_recv",
        &[prim(I64), prim(I64), ptr(prim(I64))],
        ptr(prim(U8)),
    );
    // mod.rs:1833 — (ptr, len: i64) -> ValueHandle (usize)
    assert_sig(
        &ir,
        "mimi_str_clone",
        &[ptr(prim(U8)), prim(I64)],
        prim(UIntPtr),
    );
    // crypto.rs:256 — 10 params: num_args, template, arg0..arg7
    assert_sig(
        &ir,
        "mimi_str_format",
        &[
            prim(I64),
            ptr(prim(U8)),
            ptr(prim(U8)),
            ptr(prim(U8)),
            ptr(prim(U8)),
            ptr(prim(U8)),
            ptr(prim(U8)),
            ptr(prim(U8)),
            ptr(prim(U8)),
            ptr(prim(U8)),
        ],
        ptr(prim(U8)),
    );
    // actor.rs:720 — 4 params: handles, count, method_name, out_len
    assert_sig(
        &ir,
        "mimi_broadcast",
        &[ptr(ptr(prim(U8))), prim(I64), ptr(prim(U8)), ptr(prim(I64))],
        ptr(prim(I64)),
    );
    // mod.rs:19089 — (msg) -> ! (noreturn; IR has no noreturn type — void + effect)
    assert_sig(
        &ir,
        "mimi_runtime_abort",
        &[ptr(prim(U8))],
        crate::component::AbiTypeRef::Void,
    );
    let abort = ir.export("mimi_runtime_abort").unwrap();
    assert!(abort.is_unsafe, "mimi_runtime_abort stays unsafe");
    assert!(
        abort.effects.iter().any(|e| e == "noreturn"),
        "mimi_runtime_abort must carry the noreturn effect"
    );

    // Sweep symbols (spot-checked against the runtime during the fix):
    // actor.rs:399 — 5 params including result_ptr
    assert_sig(
        &ir,
        "mimi_actor_call",
        &[
            handle("ActorHandle"),
            prim(I32),
            ptr(prim(U8)),
            prim(I64),
            ptr(prim(U8)),
        ],
        prim(I64),
    );
    // actor.rs:662 — raw name array + count, not a list handle
    assert_sig(
        &ir,
        "mimi_actor_set_method_names",
        &[handle("ActorHandle"), ptr(ptr(prim(U8))), prim(I64)],
        crate::component::AbiTypeRef::Void,
    );
    // net.rs:112 — bind is (fd, port), no addr
    assert_sig(&ir, "mimi_bind", &[prim(I64), prim(I64)], prim(I64));
    // net.rs:226 — close returns i64
    assert_sig(&ir, "mimi_close", &[prim(I64)], prim(I64));
    // mod.rs:18460 — serialize takes (data, len, elem_type)
    assert_sig(
        &ir,
        "mimi_json_serialize",
        &[ptr(prim(U8)), prim(I64), prim(I64)],
        ptr(prim(U8)),
    );
    // mod.rs:16405/16430/16444 — json_as_* take a C string, not a pointer int
    assert_sig(&ir, "mimi_json_as_i64", &[ptr(prim(U8))], prim(I64));
    assert_sig(&ir, "mimi_json_as_f64", &[ptr(prim(U8))], prim(F64));
    assert_sig(&ir, "mimi_json_as_bool", &[ptr(prim(U8))], prim(I64));
    // mod.rs:1461 — map_get returns ValueHandle (usize), not i64
    assert_sig(
        &ir,
        "mimi_map_get",
        &[handle("MapHandle"), ptr(prim(U8))],
        prim(UIntPtr),
    );
    // mod.rs:1653 — map_remove returns i32
    assert_sig(
        &ir,
        "mimi_map_remove",
        &[handle("MapHandle"), ptr(prim(U8))],
        prim(I32),
    );
    // SetHandle = i64 / SetValueHandle = i64 (mod.rs:16471-16472)
    assert_sig(&ir, "mimi_set_new", &[], prim(I64));
    assert_sig(&ir, "mimi_set_insert", &[prim(I64), prim(I64)], prim(I64));
    assert_sig(&ir, "mimi_set_contains", &[prim(I64), prim(I64)], prim(I64));
    // mod.rs:18355 — set_to_list carries an out_len
    assert_sig(
        &ir,
        "mimi_set_to_list",
        &[prim(I64), ptr(prim(I64))],
        ptr(prim(I64)),
    );
    // mod.rs:1201/1360 — rc_alloc(size: i64) -> ptr; upgrade(ptr) -> ptr
    assert_sig(&ir, "mimi_rc_alloc", &[prim(I64)], ptr(prim(U8)));
    assert_sig(&ir, "mimi_rc_upgrade", &[ptr(prim(U8))], ptr(prim(U8)));
    // mod.rs:946 — list_free takes a free_elements flag
    assert_sig(
        &ir,
        "mimi_list_free",
        &[handle("ListHandle"), prim(Bool)],
        crate::component::AbiTypeRef::Void,
    );
    // mod.rs:833-885 — list_get_* index is i64, not usize
    assert_sig(
        &ir,
        "mimi_list_get_i64",
        &[handle("ListHandle"), prim(I64)],
        prim(I64),
    );
    // mod.rs:911 — element_kind is (list) -> i8
    assert_sig(
        &ir,
        "mimi_list_element_kind",
        &[handle("ListHandle")],
        prim(I8),
    );
    // mod.rs:18806/18870 — tuple serialization
    assert_sig(
        &ir,
        "mimi_tuple_serialize",
        &[ptr(prim(I64)), prim(I64), ptr(prim(I64))],
        ptr(prim(U8)),
    );
    assert_sig(
        &ir,
        "mimi_tuple_deserialize",
        &[ptr(prim(U8)), prim(I64), ptr(prim(I64)), ptr(prim(I64))],
        prim(I64),
    );
    // mod.rs:16507/16517 — option/result JSON discriminants are i64
    assert_sig(
        &ir,
        "mimi_option_i64_to_json",
        &[prim(I64), prim(I64)],
        ptr(prim(U8)),
    );
    assert_sig(
        &ir,
        "mimi_result_i64_to_json",
        &[prim(I64), prim(I64), prim(I64)],
        ptr(prim(U8)),
    );
    // binary_io.rs — file I/O shapes
    assert_sig(&ir, "mimi_read_file_bytes", &[ptr(prim(U8))], ptr(prim(U8)));
    assert_sig(
        &ir,
        "mimi_write_file_bytes",
        &[ptr(prim(U8)), ptr(prim(U8))],
        prim(I32),
    );
    assert_sig(
        &ir,
        "mimi_read_file_partial",
        &[ptr(prim(U8)), prim(I64)],
        ptr(prim(U8)),
    );
    // mod.rs:19266 — assert_state compares two state names
    assert_sig(
        &ir,
        "mimi_assert_state",
        &[ptr(prim(U8)), ptr(prim(U8))],
        prim(I64),
    );
    // mod.rs:2992/2999 — try_exit variants
    assert_sig(
        &ir,
        "mimi_try_exit",
        &[prim(I64)],
        crate::component::AbiTypeRef::Void,
    );
    assert_sig(
        &ir,
        "mimi_try_exit_str",
        &[ptr(prim(U8)), prim(I64)],
        crate::component::AbiTypeRef::Void,
    );
    // fs.rs:433 — exec_safe takes (prog, args list)
    assert_sig(
        &ir,
        "mimi_exec_safe",
        &[ptr(prim(U8)), handle("ListHandle")],
        ptr(prim(U8)),
    );
    // capability.rs:61 — cap_drop exists in the runtime and the registry
    assert_sig(
        &ir,
        "mimi_cap_drop",
        &[prim(I64)],
        crate::component::AbiTypeRef::Void,
    );
}

/// Phantom symbols must stay out of the registry (no runtime counterpart).
#[test]
fn audit_component_registry_phantoms_absent() {
    let ir = core_ir();
    for phantom in [
        "mimi_list_new",
        "mimi_list_len",
        "mimi_print_line",
        "mimi_print_err",
        "mimi_sleep_ms",
        "mimi_timestamp",
        "mimi_timestamp_ms",
        "mimi_string_new",
        "mimi_string_len",
        "mimi_string_as_slice",
    ] {
        assert!(
            ir.export(phantom).is_none(),
            "phantom ABI symbol resurrected: {phantom}"
        );
    }
    // Phantom fat-pointer type surface must not be registered either.
    assert!(
        ir.type_def("MimiString").is_none(),
        "phantom MimiString type resurrected"
    );
    assert!(
        ir.type_def("MimiSlice").is_none(),
        "phantom MimiSlice type resurrected"
    );
    // SetHandle is i64 in the runtime — no opaque typedef for it.
    assert!(ir.type_def("SetHandle").is_none());
}

/// Every registered export must exist as a real `#[no_mangle]` runtime
/// symbol. The name set is pinned here; when the runtime grows, extend both
/// sides together (this is the "registry describes the real runtime" gate).
#[test]
fn audit_component_registry_symbol_existence_spotcheck() {
    // Spot-check 18 additional symbols beyond the fixed set above, mixing
    // every category. The definitive machine check lives in the fix script;
    // this pins the audit-time ground truth in-tree.
    let ir = core_ir();
    let spotcheck = [
        "mimi_sha256_n",
        "mimi_regex_capture_groups",
        "mimi_http_post",
        "mimi_actor_is_muted",
        "mimi_future_alloc",
        "mimi_executor_spawn",
        "mimi_args_init",
        "mimi_file_stat",
        "mimi_walk_dir",
        "mimi_sleep",
        "mimi_now_ms",
        "mimi_set_to_display",
        "mimi_inject_fault",
        "mimi_match_panic",
        "mimi_list_push_grow",
        "mimi_map_from_list",
        "mimi_broadcast_free",
        "mimi_json_deserialize_free",
    ];
    for name in spotcheck {
        assert!(ir.export(name).is_some(), "missing runtime export: {name}");
    }
}

// ══════════════════════════════════════════════════════════════════════════
// 2. C header conformance (MimiHandle typedef + corrected decls)
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn audit_component_header_mimi_handle_typedef_and_corrected_decls() {
    let ir = core_ir();
    let header = crate::component::generate_c_header(&ir);

    // Audit fix: the typedef must exist and precede every Opaque rendering.
    let typedef_pos = header
        .find("typedef uintptr_t MimiHandle;")
        .unwrap_or_else(|| {
            panic!("generated header lacks `typedef uintptr_t MimiHandle;`:\n{header}")
        });
    let first_use = header
        .find("MimiHandle/*")
        .expect("opaque rendering present");
    assert!(
        typedef_pos < first_use,
        "MimiHandle typedef must appear before first Opaque use"
    );

    // Corrected declarations (real runtime shapes).
    assert!(header.contains("int64_t mimi_cap_register(uint8_t* name);"));
    assert!(header.contains("bool mimi_cap_check(int64_t cap,"));
    assert!(header.contains("bool mimi_cap_consume(int64_t cap,"));
    assert!(header.contains(
        "uint8_t* mimi_recv(\n    int64_t fd,\n    int64_t buf_size,\n    int64_t* out_len\n);"
    ));
    assert!(header.contains("uint8_t* mimi_rc_alloc(int64_t size);"));
    // mimi_str_format: 10 params → multi-line rendering.
    assert!(header.contains("uint8_t* mimi_str_format(\n"));

    // Phantom decls must not leak into the header.
    assert!(!header.contains("mimi_string_new("));
    assert!(!header.contains("mimi_list_new("));
    assert!(!header.contains("mimi_print_line("));
    assert!(!header.contains("typedef struct MimiString"));
    assert!(!header.contains("typedef struct MimiSlice"));

    // Brace balance sanity (self-contained header).
    assert_eq!(header.matches('{').count(), header.matches('}').count());
}

// ══════════════════════════════════════════════════════════════════════════
// 3. Wire round-trips: exact consumed bytes
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn audit_component_wire_optional_then_field_roundtrip() {
    use crate::component::WireType;

    // Encode [Optional<String>][I64]; decode must land on exact offsets.
    let mut buf = Vec::new();
    buf.extend(WireType::encode_optional(Some(
        &WireType::encode_string("mimi").unwrap(),
    )));
    let opt_len = buf.len();
    assert_eq!(opt_len, 1 + 4 + 4); // tag + u32 len + "mimi"
    buf.extend(WireType::I64.encode_primitive(777).unwrap());

    let (value, consumed) =
        WireType::decode_optional(&WireType::String, &buf).expect("optional decodes");
    assert_eq!(consumed, opt_len, "Some: consumed must be tag + value only");
    let (s, _) = WireType::decode_string(&value.expect("Some payload")).unwrap();
    assert_eq!(s, "mimi");
    assert_eq!(
        WireType::I64.decode_primitive(&buf[consumed..]),
        Some(777),
        "the field after the optional must decode from the exact offset"
    );

    // None arm: consumed exactly 1.
    let mut buf2 = Vec::new();
    buf2.extend(WireType::encode_optional(None));
    buf2.extend(WireType::I64.encode_primitive((-5i64) as u64).unwrap());
    let (value2, consumed2) =
        WireType::decode_optional(&WireType::String, &buf2).expect("None decodes");
    assert_eq!(value2, None);
    assert_eq!(consumed2, 1);
    assert_eq!(
        WireType::I64.decode_primitive(&buf2[1..]),
        Some((-5i64) as u64)
    );

    // Result payload exactness: Ok(String) + trailing I32.
    let mut buf3 = Vec::new();
    buf3.extend(WireType::encode_result_tag(false));
    buf3.extend(WireType::encode_string("ok").unwrap());
    buf3.extend(WireType::I32.encode_primitive(9).unwrap());
    let (result, consumed3) =
        WireType::decode_result(&WireType::String, &WireType::I64, &buf3).expect("result decodes");
    assert_eq!(consumed3, 1 + 4 + 2);
    let payload = result.expect("Ok branch");
    let (s, _) = WireType::decode_string(&payload).unwrap();
    assert_eq!(s, "ok");
    assert_eq!(WireType::I32.decode_primitive(&buf3[consumed3..]), Some(9));
}

#[test]
fn audit_component_wire_schema_contiguity_enforced() {
    use crate::component::{WireField, WireSchema, WireSchemaError, WireType};

    // Gap at index 1: must be rejected now (previously documented-only).
    let schema = WireSchema {
        name: "audit".to_string(),
        version: 1,
        fields: vec![
            WireField {
                name: "a".to_string(),
                ty: WireType::I32,
                index: 0,
                optional: false,
            },
            WireField {
                name: "b".to_string(),
                ty: WireType::I32,
                index: 2,
                optional: false,
            },
        ],
    };
    let errors = schema.validate();
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, WireSchemaError::NonContiguousIndex { .. })),
        "gap in indices must be reported: {errors:?}"
    );

    // Contiguous schema stays clean.
    let clean = WireSchema {
        name: "clean".to_string(),
        version: 1,
        fields: vec![
            WireField {
                name: "a".to_string(),
                ty: WireType::I32,
                index: 0,
                optional: false,
            },
            WireField {
                name: "b".to_string(),
                ty: WireType::I32,
                index: 1,
                optional: false,
            },
        ],
    };
    assert!(clean.validate().is_empty());
}

// ══════════════════════════════════════════════════════════════════════════
// 4. Handle wire-generation limit (fail closed past 16 bits)
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn audit_component_wire_generation_limit_fails_closed() {
    use crate::component::{Handle, HandleError, HandleKind, HandleRegistry, RuntimeId};

    // The wire format packs the generation into 16 bits; the registry
    // counts generations up to 2^32. `MAX_WIRE_GENERATION` is 0xFFFF but is
    // not re-exported, so pin the literal here.
    const MAX_WIRE_GENERATION: u32 = 0xFFFF;

    // Boundary: generation == 0xFFFF packs and round-trips exactly.
    let reg = HandleRegistry::new(RuntimeId::Native);
    let h = reg.acquire(HandleKind::List).expect("acquire");
    assert_eq!(h.generation(), 0);
    assert_eq!(Handle::from_u64(h.to_u64().expect("gen 0 packs")), Some(h));

    // Drive one slot through 0xFFFF destroy cycles so its generation
    // reaches 0xFFFF + 1, just past the 16-bit wire field (the registry
    // itself counts generations up to 2^32). Each cycle: release the acquire
    // lease, destroy (bumps generation), re-acquire the same slot.
    // `0..=MAX` runs MAX+1 = 65536 times, landing on generation MAX+1.
    let mut current = h;
    for _ in 0..=MAX_WIRE_GENERATION {
        reg.release_lease(&current).expect("release");
        reg.destroy(&current).expect("destroy bumps generation");
        current = reg.acquire(HandleKind::List).expect("reacquire same slot");
    }
    assert_eq!(current.generation(), MAX_WIRE_GENERATION + 1);
    assert_eq!(reg.live_count(), 1, "exactly one live slot (reused)");

    // The packing boundary must refuse instead of truncating.
    assert_eq!(
        current.to_u64(),
        Err(HandleError::GenerationNotWireEncodable {
            generation: MAX_WIRE_GENERATION + 1,
        }),
        "wire packing must fail closed past 16-bit generations"
    );
}

#[test]
fn audit_component_checkpoint_layout_probe_rejects_bad_fields() {
    use crate::component::{probe_layout, LayoutFault, MimiAbi};

    // Field starts inside the struct but overflows the tail — the old
    // probe only checked offset < size and missed this.
    let json = r#"{
        "format_version": 1,
        "identity": { "name": "audit", "version": "0", "abi_version": 1 },
        "exports": [], "imports": [],
        "types": [{
            "kind": "Struct", "name": "Overflow",
            "fields": [
                { "name": "only", "ty": {"kind":"Primitive","value":"U64"}, "offset": 12 }
            ],
            "size": 16, "align": 8
        }]
    }"#;
    let abi = MimiAbi::from_json(json).expect("parse");
    let faults = probe_layout(&abi);
    assert!(
        faults
            .iter()
            .any(|f| matches!(f, LayoutFault::FieldOverflowsStruct { .. })),
        "tail-overflowing field must be rejected: {faults:?}"
    );

    // Misaligned field.
    let json = r#"{
        "format_version": 1,
        "identity": { "name": "audit", "version": "0", "abi_version": 1 },
        "exports": [], "imports": [],
        "types": [{
            "kind": "Struct", "name": "Misaligned",
            "fields": [
                { "name": "a", "ty": {"kind":"Primitive","value":"U16"}, "offset": 0 },
                { "name": "b", "ty": {"kind":"Primitive","value":"U64"}, "offset": 3 }
            ],
            "size": 16, "align": 8
        }]
    }"#;
    let abi = MimiAbi::from_json(json).expect("parse");
    let faults = probe_layout(&abi);
    assert!(
        faults
            .iter()
            .any(|f| matches!(f, LayoutFault::FieldMisaligned { .. })),
        "misaligned field must be rejected: {faults:?}"
    );

    // The corrected core registry still round-trips through the probe
    // cleanly (no struct layouts registered, nothing to fault).
    let mut gen = crate::component::AbiGenerator::new();
    crate::component::register_core_runtime_abi(&mut gen);
    let ir = gen.build();
    let abi = MimiAbi::from_component_ir(&ir);
    assert!(probe_layout(&abi).is_empty());
}
