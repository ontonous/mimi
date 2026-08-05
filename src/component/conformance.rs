//! 0.31.38 SDK conformance tests.
//!
//! Stabilization checkpoint: no new SDK surface. These tests verify
//! the conformance of existing Component IR infrastructure under
//! adversarial and edge-case conditions.
//!
//! Categories:
//! 1. Rust SDK: cancel/complete race, late callback, lease expiry
//! 2. Wire fuzz: schema bomb, unknown tags, replay, bit-flip
//! 3. Full pipeline round-trip: build → validate → serialize → diff → bindgen

#[cfg(test)]
mod tests {
    use crate::component::gen::{register_core_runtime_abi, AbiGenerator};
    use crate::component::handle::{HandleKind, HandleRegistry, RuntimeId};
    use crate::component::serialize::MimiAbi;
    use crate::component::wire::{WireEnvelope, WireField, WireSchema, WireSchemaError, WireType};
    use crate::component::{diff_abi, generate_c_header, generate_rust_bindings, probe_layout};
    use std::sync::Arc;
    use std::thread;

    // ══════════════════════════════════════════════════════════════════════
    // 1. Rust SDK conformance: HandleRegistry + callback lifecycle
    // ══════════════════════════════════════════════════════════════════════

    /// Cancel/complete race: an operation is in-flight (leased) when
    /// cancellation is attempted. Destroy must fail until the lease is
    /// released (operation completes).
    #[test]
    fn sdk_cancel_complete_race() {
        let reg = HandleRegistry::new(RuntimeId::Native);
        let h = reg.acquire(HandleKind::Task).unwrap();

        // Operation starts: take an extra lease (simulates in-flight work)
        reg.lease(&h).unwrap();
        assert_eq!(reg.lease_count(&h).unwrap(), 2);

        // Cancellation attempted while operation is in-flight → must fail
        assert!(reg.destroy(&h).is_err());

        // Operation completes: release the extra lease
        assert_eq!(reg.release_lease(&h).unwrap(), 1);

        // Cancellation still fails (original lease from acquire)
        assert!(reg.destroy(&h).is_err());

        // Final release
        assert_eq!(reg.release_lease(&h).unwrap(), 0);

        // Now destroy succeeds
        assert!(reg.destroy(&h).is_ok());
        assert!(!reg.is_live(&h));
    }

    /// Late callback delivery: a callback arrives after its handle has
    /// been destroyed. The registry must reject it with StaleGeneration.
    #[test]
    fn sdk_late_callback_delivery() {
        let reg = HandleRegistry::new(RuntimeId::Interp);
        let h = reg.acquire(HandleKind::Subscription).unwrap();

        // Simulate callback subscription lifecycle
        reg.lease(&h).unwrap(); // callback holds a lease

        // Subscription cancelled: release + destroy
        reg.release_lease(&h).unwrap();
        reg.release_lease(&h).unwrap();
        reg.destroy(&h).unwrap();

        // Late callback arrives: tries to use the stale handle
        assert!(!reg.is_live(&h));
        assert!(reg.lease_count(&h).is_err()); // StaleGeneration
        assert!(reg.lease(&h).is_err());
        assert!(reg.release_lease(&h).is_err());
    }

    /// Lease expiry E2E: multiple leases are acquired and released in
    /// various orders. Destroy only succeeds when all are released.
    #[test]
    fn sdk_lease_expiry_e2e() {
        let reg = HandleRegistry::new(RuntimeId::Native);
        let h = reg.acquire(HandleKind::Foreign).unwrap();

        // Simulate 5 concurrent users
        for _ in 0..5 {
            reg.lease(&h).unwrap();
        }
        assert_eq!(reg.lease_count(&h).unwrap(), 6); // 1 from acquire + 5

        // Release in random order (all equivalent here)
        for expected_remaining in (0..6).rev() {
            assert_eq!(reg.release_lease(&h).unwrap(), expected_remaining);
            if expected_remaining > 0 {
                assert!(reg.destroy(&h).is_err());
            }
        }

        // All leases released → destroy succeeds
        assert!(reg.destroy(&h).is_ok());
    }

    /// Concurrent cancel/complete: multiple threads race to release leases
    /// on the same handle. After all releases, destroy must succeed.
    /// Verifies no corruption under contention.
    #[test]
    fn sdk_concurrent_cancel_complete() {
        let reg = Arc::new(HandleRegistry::new(RuntimeId::Native));
        let h = reg.acquire(HandleKind::Task).unwrap();

        // Add leases so multiple threads can "complete"
        for _ in 0..8 {
            reg.lease(&h).unwrap();
        }
        // Total leases: 1 (acquire) + 8 = 9

        let mut threads = Vec::new();

        // 8 threads each release one lease
        for _ in 0..8 {
            let reg = Arc::clone(&reg);
            threads.push(thread::spawn(move || {
                reg.release_lease(&h).unwrap();
            }));
        }

        for t in threads {
            t.join().unwrap();
        }

        // After all 8 releases, 1 lease remains (from acquire)
        assert_eq!(reg.lease_count(&h).unwrap(), 1);
        // Destroy still fails
        assert!(reg.destroy(&h).is_err());

        // Release the final lease and destroy
        reg.release_lease(&h).unwrap();
        assert!(reg.destroy(&h).is_ok());
        assert!(!reg.is_live(&h));
    }

    /// Subscription lifecycle: AsyncSubscription pattern — acquire,
    /// multiple deliveries (leases), explicit cancellation, stale rejection.
    #[test]
    fn sdk_subscription_lifecycle() {
        let reg = HandleRegistry::new(RuntimeId::Interp);

        // Subscribe
        let sub = reg.acquire(HandleKind::Subscription).unwrap();
        assert!(reg.is_live(&sub));

        // Simulate 100 event deliveries (each takes + releases a lease)
        for _ in 0..100 {
            reg.lease(&sub).unwrap(); // delivery in progress
            reg.release_lease(&sub).unwrap(); // delivery complete
        }

        // Explicit cancellation
        reg.release_lease(&sub).unwrap(); // release the acquire lease
        assert_eq!(reg.lease_count(&sub).unwrap(), 0);
        reg.destroy(&sub).unwrap();

        // Post-cancellation delivery attempt → rejected
        assert!(!reg.is_live(&sub));
        assert!(reg.lease(&sub).is_err());
    }

    /// Cross-runtime rejection: a handle from Interp cannot be used
    /// against a Native registry (and vice versa).
    #[test]
    fn sdk_cross_runtime_rejection() {
        let interp_reg = HandleRegistry::new(RuntimeId::Interp);
        let native_reg = HandleRegistry::new(RuntimeId::Native);

        let h = interp_reg.acquire(HandleKind::List).unwrap();

        // Same slot index, same generation, same kind — but wrong runtime
        // The native registry has no slots, so this is UnknownSlot
        assert!(native_reg.lease_count(&h).is_err());
    }

    // ══════════════════════════════════════════════════════════════════════
    // 2. Wire fuzz: adversarial inputs
    // ══════════════════════════════════════════════════════════════════════

    /// Schema bomb: a WireSchema with an extreme number of fields.
    /// validate() must complete without hanging or panicking.
    #[test]
    fn wire_schema_bomb_many_fields() {
        let fields: Vec<WireField> = (0..10_000)
            .map(|i| WireField {
                name: format!("field_{}", i),
                ty: WireType::I64,
                index: i,
                optional: false,
            })
            .collect();
        let schema = WireSchema {
            name: "bomb".to_string(),
            version: 1,
            fields,
        };
        // Should complete quickly with no errors
        assert!(schema.validate().is_empty());
    }

    /// Schema bomb: duplicate indices in a large schema.
    #[test]
    fn wire_schema_bomb_duplicate_indices() {
        let fields: Vec<WireField> = (0..1000)
            .map(|i| WireField {
                name: format!("field_{}", i),
                ty: WireType::I32,
                index: (i % 10) as u32, // only 10 unique indices
                optional: false,
            })
            .collect();
        let schema = WireSchema {
            name: "dup".to_string(),
            version: 1,
            fields,
        };
        let errors = schema.validate();
        // 1000 fields, 10 unique indices → 990 duplicates
        assert_eq!(
            errors
                .iter()
                .filter(|e| matches!(e, WireSchemaError::DuplicateIndex(_)))
                .count(),
            990
        );
    }

    /// Envelope bit-flip: flipping any single byte in a valid envelope
    /// must produce either a valid (different) envelope or an error.
    /// No panics allowed.
    #[test]
    fn wire_envelope_bit_flip_no_panic() {
        let payload = b"conformance test payload".to_vec();
        let original = WireEnvelope::new(payload);
        let bytes = original.to_bytes();

        for pos in 0..bytes.len() {
            for bit in 0..8 {
                let mut corrupted = bytes.clone();
                corrupted[pos] ^= 1 << bit;
                // Must not panic — either Ok or Err
                let _ = WireEnvelope::from_bytes(&corrupted);
            }
        }
    }

    /// Envelope replay: decoding the same bytes twice produces
    /// identical results (wire is stateless, no replay detection).
    #[test]
    fn wire_envelope_replay_identical() {
        let payload = b"replay test".to_vec();
        let bytes = WireEnvelope::new(payload).to_bytes();

        let d1 = WireEnvelope::from_bytes(&bytes).unwrap();
        let d2 = WireEnvelope::from_bytes(&bytes).unwrap();

        assert_eq!(d1.magic, d2.magic);
        assert_eq!(d1.version, d2.version);
        assert_eq!(d1.payload, d2.payload);
    }

    /// Nested array bomb: 1000 levels of Array nesting.
    /// contains_handle must not stack overflow.
    #[test]
    fn wire_nested_array_bomb_no_stack_overflow() {
        let mut ty = WireType::Handle;
        for _ in 0..1000 {
            ty = WireType::Array(Box::new(ty));
        }
        // Depth limit 128 → returns false at extreme depth (bail-out)
        // The important thing: no stack overflow
        let _ = ty.contains_handle();
    }

    /// Deeply nested optional bomb.
    #[test]
    fn wire_nested_optional_bomb_no_stack_overflow() {
        let mut ty = WireType::I32;
        for _ in 0..1000 {
            ty = WireType::Optional(Box::new(ty));
        }
        let _ = ty.contains_handle();
        let _ = ty.fixed_size();
    }

    /// Wire type with all composite types nested.
    #[test]
    fn wire_composite_nesting_roundtrip() {
        // Array of Optional of Result of (String, Map<String, Bytes>)
        let ty = WireType::Array(Box::new(WireType::Optional(Box::new(WireType::Result(
            Box::new(WireType::String),
            Box::new(WireType::Map(
                Box::new(WireType::String),
                Box::new(WireType::Bytes),
            )),
        )))));

        assert!(ty.fixed_size().is_none()); // variable-length
        assert!(!ty.contains_handle());

        // Encode/decode a concrete value through the composite
        let mut buf = WireType::encode_array_header(1);
        buf.extend(WireType::encode_optional(Some(
            &WireType::encode_string("key").unwrap(),
        )));
        assert!(buf.len() > 4);
    }

    // ══════════════════════════════════════════════════════════════════════
    // 3. Full pipeline round-trip
    // ══════════════════════════════════════════════════════════════════════

    /// Complete pipeline: build → validate → serialize → deserialize →
    /// validate → reverse → diff → C header → Rust bindings → fixpoint.
    #[test]
    fn pipeline_full_roundtrip_conformance() {
        // Step 1: Build
        let mut gen = AbiGenerator::new();
        register_core_runtime_abi(&mut gen);
        let ir = gen.build();
        assert!(ir.exports.len() >= 140);

        // Step 2: Validate
        let errors = ir.validate();
        assert!(errors.is_empty(), "validation errors: {:?}", errors);

        // Step 3: Serialize
        let abi = MimiAbi::from_component_ir(&ir);
        let json = abi.to_json().expect("serialize");

        // Step 4: Deserialize (validated)
        let abi2 = MimiAbi::from_json_validated(&json).expect("validated deserialize");

        // Step 5: Reverse
        let ir2 = abi2.to_component_ir();
        assert_eq!(ir.exports.len(), ir2.exports.len());
        assert_eq!(ir.identity, ir2.identity);

        // Step 6: Layout probe
        let faults = probe_layout(&abi);
        assert!(faults.is_empty(), "layout faults: {:?}", faults);

        // Step 7: Diff (identical → no changes)
        let diff = diff_abi(&abi, &abi2);
        assert!(!diff.has_breaking_changes());
        assert_eq!(diff.summary(), "no changes");

        // Step 8: C header
        let c_header = generate_c_header(&ir);
        assert!(c_header.contains("#ifndef MIMI_RUNTIME_ABI_H"));
        assert!(c_header.contains("extern \"C\" {"));
        // Balanced braces
        assert_eq!(c_header.matches('{').count(), c_header.matches('}').count());

        // Step 9: Rust bindings
        let rust_bind = generate_rust_bindings(&ir);
        // Audit 2026-08-05: the phantom MimiString/MimiSlice structs were
        // removed from the core registry, so it carries opaque handles only —
        // no #[repr(C)] structs. Pin the opaque aliases instead.
        assert!(rust_bind.contains("pub type ListHandle = usize;"));
        assert!(rust_bind.contains("pub type MapHandle = usize;"));
        assert!(rust_bind.contains("extern \"C\" {"));
        assert_eq!(
            rust_bind.matches('{').count(),
            rust_bind.matches('}').count()
        );
        // Audit 2026-08-05: corrected signature survives into Rust bindings.
        assert!(rust_bind.contains("pub fn mimi_cap_register(name: *mut u8) -> i64;"));
        assert!(rust_bind.contains("pub fn mimi_cap_check(cap: i64, name: *mut u8) -> bool;"));

        // Step 10: Fixpoint (serialize → deserialize → re-serialize)
        let json2 = abi2.to_json().expect("re-serialize");
        assert_eq!(json, json2, "roundtrip is not a fixpoint");

        // Step 11: Hash stability
        let h1 = abi.hash().expect("hash");
        let h2 = abi2.hash().expect("hash");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // BLAKE3 hex
    }

    /// Pipeline with imports: verify import round-trip through serialization.
    #[test]
    fn pipeline_import_roundtrip() {
        let mut gen = AbiGenerator::new();
        gen.import("abs", |f| {
            f.param(
                "x",
                crate::component::gen::prim(crate::component::types::AbiPrimitive::I32),
            )
            .returns(crate::component::gen::prim(
                crate::component::types::AbiPrimitive::I32,
            ))
        });
        gen.import("strlen", |f| {
            f.param(
                "s",
                crate::component::gen::ptr(crate::component::gen::prim(
                    crate::component::types::AbiPrimitive::U8,
                )),
            )
            .returns(crate::component::gen::prim(
                crate::component::types::AbiPrimitive::UIntPtr,
            ))
        });
        let ir = gen.build();
        assert_eq!(ir.imports.len(), 2);

        let abi = MimiAbi::from_component_ir(&ir);
        let json = abi.to_json().expect("serialize");
        let abi2 = MimiAbi::from_json_validated(&json).expect("validated");
        let ir2 = abi2.to_component_ir();

        assert_eq!(ir2.imports.len(), 2);
        assert_eq!(ir2.import("abs").unwrap().params.len(), 1);
        assert_eq!(ir2.import("strlen").unwrap().params.len(), 1);
    }

    /// Pipeline diff detection: verify that adding/removing exports
    /// produces correct diff results.
    #[test]
    fn pipeline_diff_breaking_detection() {
        let mut gen = AbiGenerator::new();
        register_core_runtime_abi(&mut gen);
        let ir_old = gen.build();
        let abi_old = MimiAbi::from_component_ir(&ir_old);

        // New version: remove one export, add one
        let mut gen2 = AbiGenerator::new();
        register_core_runtime_abi(&mut gen2);
        gen2.export("mimi_new_feature", |f| {
            f.param(
                "x",
                crate::component::gen::prim(crate::component::types::AbiPrimitive::I64),
            )
            .returns(crate::component::gen::prim(
                crate::component::types::AbiPrimitive::Bool,
            ))
        });
        let ir_new = gen2.build();
        let mut abi_new = MimiAbi::from_component_ir(&ir_new);
        abi_new.exports.retain(|s| s.name != "mimi_rc_alloc");

        let diff = diff_abi(&abi_old, &abi_new);
        assert!(diff.has_breaking_changes());
        assert!(diff.breaking_count() >= 1); // removed mimi_rc_alloc
        assert!(diff.non_breaking_count() >= 1); // added mimi_new_feature
    }

    /// Audit 2026-08-05 (full audit §12): the core registry was corrected
    /// against the real `#[no_mangle]` runtime symbols. Pin the fixed
    /// signatures at the pipeline level (build → serialize → deserialize →
    /// reverse) so a regression through ANY stage is caught.
    #[test]
    fn pipeline_registry_real_runtime_signatures() {
        use crate::component::types::{AbiPrimitive, AbiTypeRef};

        let mut gen = AbiGenerator::new();
        register_core_runtime_abi(&mut gen);
        let ir = gen.build();

        // Round-trip through .mimiabi so the pin covers serialization too.
        let abi = MimiAbi::from_component_ir(&ir);
        let json = abi.to_json().expect("serialize");
        let ir2 = MimiAbi::from_json_validated(&json)
            .expect("validated")
            .to_component_ir();

        let ptr_u8 = || AbiTypeRef::Pointer(Box::new(AbiTypeRef::Primitive(AbiPrimitive::U8)));
        let ptr_i64 = || AbiTypeRef::Pointer(Box::new(AbiTypeRef::Primitive(AbiPrimitive::I64)));
        let prim_i64 = AbiTypeRef::Primitive(AbiPrimitive::I64);
        let prim_bool = AbiTypeRef::Primitive(AbiPrimitive::Bool);
        let prim_uintptr = AbiTypeRef::Primitive(AbiPrimitive::UIntPtr);
        let void = AbiTypeRef::Void;

        // name → (param types, return type) pinned against runtime/*.rs
        let pinned: &[(&str, &[AbiTypeRef], AbiTypeRef)] = &[
            // capability.rs:34
            ("mimi_cap_register", &[ptr_u8()], prim_i64.clone()),
            // capability.rs:69
            (
                "mimi_cap_check",
                &[prim_i64.clone(), ptr_u8()],
                prim_bool.clone(),
            ),
            // capability.rs:86
            (
                "mimi_cap_consume",
                &[prim_i64.clone(), ptr_u8()],
                prim_bool.clone(),
            ),
            // capability.rs:61
            (
                "mimi_cap_drop",
                std::slice::from_ref(&prim_i64),
                void.clone(),
            ),
            // mod.rs:18534
            (
                "mimi_json_deserialize",
                &[ptr_u8(), ptr_i64(), prim_i64.clone()],
                ptr_u8(),
            ),
            // net.rs:192
            (
                "mimi_recv",
                &[prim_i64.clone(), prim_i64.clone(), ptr_i64()],
                ptr_u8(),
            ),
            // mod.rs:1833
            (
                "mimi_str_clone",
                &[ptr_u8(), prim_i64.clone()],
                prim_uintptr.clone(),
            ),
            // crypto.rs:256 — 10 params
            (
                "mimi_str_format",
                &[
                    prim_i64.clone(),
                    ptr_u8(),
                    ptr_u8(),
                    ptr_u8(),
                    ptr_u8(),
                    ptr_u8(),
                    ptr_u8(),
                    ptr_u8(),
                    ptr_u8(),
                    ptr_u8(),
                ],
                ptr_u8(),
            ),
            // actor.rs:720 — 4 params
            (
                "mimi_broadcast",
                &[
                    AbiTypeRef::Pointer(Box::new(ptr_u8())),
                    prim_i64.clone(),
                    ptr_u8(),
                    ptr_i64(),
                ],
                AbiTypeRef::Pointer(Box::new(AbiTypeRef::Primitive(AbiPrimitive::I64))),
            ),
            // mod.rs:19089 — noreturn surfaced as void + effect
            ("mimi_runtime_abort", &[ptr_u8()], void.clone()),
        ];

        for (name, params, ret) in pinned {
            for candidate in [&ir, &ir2] {
                let sym = candidate
                    .export(name)
                    .unwrap_or_else(|| panic!("{name}: missing after round-trip"));
                let got: Vec<AbiTypeRef> = sym.params.iter().map(|p| p.ty.clone()).collect();
                assert_eq!(&got[..], *params, "{name}: param types regressed");
                assert_eq!(&sym.ret, ret, "{name}: return type regressed");
            }
        }

        // noreturn marker on abort/try_exit/match_panic.
        for name in ["mimi_runtime_abort", "mimi_try_exit", "mimi_match_panic"] {
            let sym = ir.export(name).unwrap_or_else(|| panic!("missing {name}"));
            assert!(
                sym.effects.iter().any(|e| e == "noreturn"),
                "{name}: noreturn effect missing"
            );
        }

        // Phantoms must stay absent after the round-trip too.
        for name in [
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
                ir2.export(name).is_none(),
                "{name}: phantom resurrected through serialization"
            );
        }
    }

    /// Callback category round-trip through serialization.
    #[test]
    fn pipeline_callback_category_roundtrip() {
        use crate::component::symbol::AbiCallbackCategory;

        let categories = [
            AbiCallbackCategory::SyncSameThread,
            AbiCallbackCategory::SyncCrossThread,
            AbiCallbackCategory::AsyncOneShot,
            AbiCallbackCategory::AsyncMultiShot,
            AbiCallbackCategory::AsyncSubscription,
        ];

        for (i, cat) in categories.iter().enumerate() {
            let mut gen = AbiGenerator::new();
            gen.export(&format!("mimi_cb_{}", i), |f| {
                f.param(
                    "data",
                    crate::component::gen::prim(crate::component::types::AbiPrimitive::I64),
                )
                .callback(*cat)
            });
            let ir = gen.build();

            let abi = MimiAbi::from_component_ir(&ir);
            let json = abi.to_json().expect("serialize");
            let abi2 = MimiAbi::from_json_validated(&json).expect("validated");
            let ir2 = abi2.to_component_ir();

            let sym = ir2.export(&format!("mimi_cb_{}", i)).unwrap();
            assert_eq!(sym.callback_category, Some(*cat));
        }
    }

    /// Handle registry stress: high-contention mixed operations
    /// across all handle kinds.
    #[test]
    fn sdk_handle_registry_stress_all_kinds() {
        let reg = Arc::new(HandleRegistry::new(RuntimeId::Native));
        let kinds = [
            HandleKind::List,
            HandleKind::Map,
            HandleKind::Set,
            HandleKind::Buffer,
            HandleKind::Task,
            HandleKind::Subscription,
            HandleKind::Foreign,
        ];

        let mut threads = Vec::new();
        for t in 0..16 {
            let reg = Arc::clone(&reg);
            let kind = kinds[t % kinds.len()];
            threads.push(thread::spawn(move || {
                for _ in 0..500 {
                    let h = reg.acquire(kind).unwrap();
                    assert_eq!(h.kind(), kind);

                    // Random-ish lease pattern
                    reg.lease(&h).unwrap();
                    reg.lease(&h).unwrap();
                    assert_eq!(reg.release_lease(&h).unwrap(), 2);
                    assert_eq!(reg.release_lease(&h).unwrap(), 1);
                    assert_eq!(reg.release_lease(&h).unwrap(), 0);
                    reg.destroy(&h).unwrap();
                }
            }));
        }

        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(reg.live_count(), 0);
    }
}
