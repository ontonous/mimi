//! 0.31.32 Component stability checkpoint.
//!
//! This module adds no new Component surface. It provides:
//!
//! 1. **ABI fuzz / layout probes** — verify that every `.mimiabi` struct with
//!    declared `size`/`align`/`offset` is internally consistent (offsets
//!    monotonic, within size, aligned; each field's offset + its size ≤ the
//!    struct size; per-field alignment respected) and that `.mimiabi`
//!    round-trips (serialize → deserialize → re-serialize is a fixpoint, and
//!    the BLAKE3 hash is stable). Audit 2026-08-05: the field-overflow and
//!    per-field alignment checks were missing; `probe_layout` now rejects
//!    those malformed layouts explicitly.
//!
//! 2. **Allocator provenance** — a small ledger that pairs cross-boundary
//!    `alloc`/`free` and detects mismatched frees (freed by the wrong side,
//!    double free, or freeing an unknown pointer). This is the checkpoint's
//!    model of "who allocated, who is allowed to free".
//!
//! The handle-lease race guarantees live in [`super::handle`]; this module
//! adds an additional multi-threaded stress that mixes acquire / lease /
//! release / destroy with generation reuse under contention.

use super::serialize::{MimiAbi, MimiAbiTypeRef};

/// A structural problem discovered by the layout probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutFault {
    /// A field offset lies outside the declared struct size.
    OffsetOutOfBounds {
        struct_name: String,
        field: String,
        offset: usize,
        size: usize,
    },
    /// A field extends past the end of the struct
    /// (`offset + field_size > size`). Audit 2026-08-05.
    FieldOverflowsStruct {
        struct_name: String,
        field: String,
        offset: usize,
        field_size: usize,
        size: usize,
    },
    /// A field offset is not a multiple of the field's alignment.
    /// Audit 2026-08-05.
    FieldMisaligned {
        struct_name: String,
        field: String,
        offset: usize,
        align: usize,
    },
    /// Field offsets are not strictly increasing (overlap / reorder).
    OffsetsNotMonotonic { struct_name: String, field: String },
    /// Struct size is not a multiple of its alignment.
    SizeNotAligned {
        struct_name: String,
        size: usize,
        align: usize,
    },
    /// Alignment is not a power of two.
    AlignNotPow2 { struct_name: String, align: usize },
}

impl std::fmt::Display for LayoutFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LayoutFault::OffsetOutOfBounds {
                struct_name,
                field,
                offset,
                size,
            } => write!(
                f,
                "{struct_name}.{field}: offset {offset} out of bounds (size {size})"
            ),
            LayoutFault::FieldOverflowsStruct {
                struct_name,
                field,
                offset,
                field_size,
                size,
            } => write!(
                f,
                "{struct_name}.{field}: field occupies bytes {offset}..{} past struct size {size}",
                offset + field_size
            ),
            LayoutFault::FieldMisaligned {
                struct_name,
                field,
                offset,
                align,
            } => write!(
                f,
                "{struct_name}.{field}: offset {offset} not aligned to {align}"
            ),
            LayoutFault::OffsetsNotMonotonic { struct_name, field } => {
                write!(f, "{struct_name}.{field}: offsets not strictly increasing")
            }
            LayoutFault::SizeNotAligned {
                struct_name,
                size,
                align,
            } => write!(
                f,
                "{struct_name}: size {size} not a multiple of align {align}"
            ),
            LayoutFault::AlignNotPow2 { struct_name, align } => {
                write!(f, "{struct_name}: align {align} is not a power of two")
            }
        }
    }
}

/// Byte size of a serialized type reference, when statically known.
///
/// Used by the layout probe to check `offset + field_size <= size`.
/// Pointers, opaque handles and named types are pointer-sized (8 bytes on
/// the supported 64-bit targets); fat pointers carry { data, len, capacity }
/// (24) or { data, len } (16); void fields are invalid but sized 0 so the
/// offset arithmetic still works.
fn type_size(ty: &MimiAbiTypeRef) -> Option<usize> {
    match ty {
        MimiAbiTypeRef::Primitive(name) => Some(match name.as_str() {
            "I8" | "U8" | "Bool" => 1,
            "I16" | "U16" => 2,
            "I32" | "U32" | "F32" => 4,
            "I64" | "U64" | "F64" | "IntPtr" | "UIntPtr" => 8,
            _ => return None, // unknown primitive — validated deserialization rejects it
        }),
        MimiAbiTypeRef::Pointer(_) | MimiAbiTypeRef::Opaque(_) | MimiAbiTypeRef::Named(_) => {
            Some(8)
        }
        MimiAbiTypeRef::Slice(_) => Some(16), // { data, len }
        MimiAbiTypeRef::FatPointer { has_capacity, .. } => {
            Some(if *has_capacity { 24 } else { 16 })
        }
        MimiAbiTypeRef::Void => Some(0),
    }
}

/// Natural alignment of a field with the given size (power-of-two sizes
/// align to themselves; anything else conservatively aligns to 8).
fn field_align(field_size: usize) -> usize {
    match field_size {
        0 | 1 => 1,
        2 => 2,
        4 => 4,
        8 | 16 | 24 => 8,
        _ => 8,
    }
}

/// Probe every struct type in a `.mimiabi` for layout consistency.
///
/// Returns all discovered faults (empty = ABI internally consistent).
/// Malformed layouts — a field overflowing the struct tail, or a field
/// offset violating its natural alignment — are reported as explicit
/// faults rather than silently accepted (audit fix 2026-08-05).
pub fn probe_layout(abi: &MimiAbi) -> Vec<LayoutFault> {
    let mut faults = Vec::new();
    for ty in &abi.types {
        if let super::serialize::MimiAbiType::Struct {
            name,
            fields,
            size,
            align,
        } = ty
        {
            if let Some(align) = align {
                if *align == 0 || (align & (align - 1)) != 0 {
                    faults.push(LayoutFault::AlignNotPow2 {
                        struct_name: name.clone(),
                        align: *align,
                    });
                }
                if let Some(size) = size {
                    if *align != 0 && size % align != 0 {
                        faults.push(LayoutFault::SizeNotAligned {
                            struct_name: name.clone(),
                            size: *size,
                            align: *align,
                        });
                    }
                }
            }
            let mut prev: Option<usize> = None;
            for field in fields {
                let Some(offset) = field.offset else { continue };
                let field_size = type_size(&field.ty);
                if let Some(size) = size {
                    if offset >= *size {
                        faults.push(LayoutFault::OffsetOutOfBounds {
                            struct_name: name.clone(),
                            field: field.name.clone(),
                            offset,
                            size: *size,
                        });
                    } else if let Some(fsize) = field_size {
                        // Audit 2026-08-05: offset < size alone does not
                        // catch a field that starts inside the struct but
                        // extends past its tail. Use checked_add so an
                        // adversarial huge offset/size cannot overflow and
                        // bypass the check (batch4-09 P2-2).
                        if offset.checked_add(fsize).map_or(true, |end| end > *size) {
                            faults.push(LayoutFault::FieldOverflowsStruct {
                                struct_name: name.clone(),
                                field: field.name.clone(),
                                offset,
                                field_size: fsize,
                                size: *size,
                            });
                        }
                    }
                }
                // Audit 2026-08-05: per-field alignment check. A repr(C)
                // field of size N sits at an offset divisible by its
                // natural alignment.
                if let Some(fsize) = field_size {
                    let falign = field_align(fsize).min(align.unwrap_or(8));
                    if falign > 1 && offset % falign != 0 {
                        faults.push(LayoutFault::FieldMisaligned {
                            struct_name: name.clone(),
                            field: field.name.clone(),
                            offset,
                            align: falign,
                        });
                    }
                }
                if let Some(p) = prev {
                    if offset <= p {
                        faults.push(LayoutFault::OffsetsNotMonotonic {
                            struct_name: name.clone(),
                            field: field.name.clone(),
                        });
                    }
                }
                prev = Some(offset);
            }
        }
    }
    faults
}

/// Count the struct types in an ABI (probe coverage metric).
pub fn struct_type_count(abi: &MimiAbi) -> usize {
    abi.types
        .iter()
        .filter(|t| matches!(t, super::serialize::MimiAbiType::Struct { .. }))
        .count()
}

// ── Allocator provenance ledger ────────────────────────────────────────────

/// Which side of the boundary owns an allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocSide {
    /// Allocated by the Mimi runtime (mimi_alloc).
    Mimi,
    /// Allocated by a foreign library (C malloc / library-specific alloc).
    Foreign,
}

/// A mismatch detected by the allocator ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllocFault {
    /// Free of a pointer never recorded as allocated.
    UnknownPointer(u64),
    /// Double free.
    DoubleFree(u64),
    /// Double allocation (pointer already live). GAP-5 fix.
    DoubleAlloc(u64),
    /// Freed by the wrong side (Mimi freeing a Foreign allocation or vice versa).
    WrongSide {
        ptr: u64,
        allocated_by: AllocSide,
        freed_by: AllocSide,
    },
}

impl std::fmt::Display for AllocFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AllocFault::UnknownPointer(p) => write!(f, "free of unknown pointer 0x{p:X}"),
            AllocFault::DoubleFree(p) => write!(f, "double free of 0x{p:X}"),
            AllocFault::DoubleAlloc(p) => write!(f, "double allocation of 0x{p:X} (already live)"),
            AllocFault::WrongSide {
                ptr,
                allocated_by,
                freed_by,
            } => write!(
                f,
                "allocator mismatch: 0x{ptr:X} allocated by {allocated_by:?}, freed by {freed_by:?}"
            ),
        }
    }
}

/// Tracks cross-boundary allocations and validates their frees.
#[derive(Debug, Default)]
pub struct AllocLedger {
    live: std::collections::HashMap<u64, AllocSide>,
}

impl AllocLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an allocation by `side`.
    ///
    /// GAP-5 fix: Returns `Err` if the pointer is already live (double
    /// allocation). Previously this was silently overwritten by
    /// `HashMap::insert`, hiding use-after-free / double-alloc bugs.
    pub fn record_alloc(&mut self, ptr: u64, side: AllocSide) -> Result<(), AllocFault> {
        if self.live.contains_key(&ptr) {
            return Err(AllocFault::DoubleAlloc(ptr));
        }
        self.live.insert(ptr, side);
        Ok(())
    }

    /// Validate and record a free by `side`. Returns `Ok(())` if the free is
    /// well-paired, `Err(fault)` otherwise. On success the pointer is removed.
    pub fn record_free(&mut self, ptr: u64, side: AllocSide) -> Result<(), AllocFault> {
        match self.live.get(&ptr).copied() {
            None => Err(AllocFault::UnknownPointer(ptr)),
            Some(alloc_side) if alloc_side != side => Err(AllocFault::WrongSide {
                ptr,
                allocated_by: alloc_side,
                freed_by: side,
            }),
            Some(_) => {
                self.live.remove(&ptr);
                Ok(())
            }
        }
    }

    /// A second free of an already-freed pointer is an UnknownPointer at the
    /// ledger level; callers that need explicit double-free detection can use
    /// this helper which distinguishes the two.
    pub fn record_free_checked(
        &mut self,
        ptr: u64,
        side: AllocSide,
        ever_allocated: bool,
    ) -> Result<(), AllocFault> {
        if !self.live.contains_key(&ptr) && ever_allocated {
            return Err(AllocFault::DoubleFree(ptr));
        }
        self.record_free(ptr, side)
    }

    /// Number of still-live (unfreed) allocations. Non-zero at shutdown = leak.
    pub fn leak_count(&self) -> usize {
        self.live.len()
    }
}

#[cfg(test)]
mod tests {
    use super::super::gen::{register_core_runtime_abi, AbiGenerator};
    use super::super::handle::{HandleKind, HandleRegistry, RuntimeId};
    use super::*;
    use std::sync::Arc;
    use std::thread;

    // ── ABI layout probes ──

    #[test]
    fn core_abi_layout_is_consistent() {
        let mut gen = AbiGenerator::new();
        register_core_runtime_abi(&mut gen);
        let ir = gen.build();
        let abi = MimiAbi::from_component_ir(&ir);
        // Audit 2026-08-05: the phantom MimiString/MimiSlice struct layouts
        // were removed from the core registry — it now registers opaque
        // handle types only, so there are no struct layouts to probe.
        assert_eq!(
            struct_type_count(&abi),
            0,
            "core registry must not declare struct layouts"
        );
        let faults = probe_layout(&abi);
        assert!(faults.is_empty(), "layout faults: {faults:?}");
    }

    #[test]
    fn probe_catches_field_overflowing_struct_tail() {
        // Audit fix 2026-08-05: offset < size is not enough — a field that
        // starts inside the struct but extends past the tail must be
        // rejected (offset + field_size > size).
        let json = r#"{
            "format_version": 1,
            "identity": { "name": "t", "version": "0", "abi_version": 1 },
            "exports": [], "imports": [],
            "types": [{
                "kind": "Struct", "name": "TailOverflow",
                "fields": [
                    { "name": "a", "ty": {"kind":"Primitive","value":"U64"}, "offset": 0 },
                    { "name": "b", "ty": {"kind":"Primitive","value":"U64"}, "offset": 12 }
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
            "expected FieldOverflowsStruct, got: {faults:?}"
        );
        // b starts at 12 (< 16) so it must NOT be reported as out-of-bounds
        // by the old coarse check; the precise fault is the overflow one.
        assert!(!faults
            .iter()
            .any(|f| matches!(f, LayoutFault::OffsetOutOfBounds { field, .. } if field == "b")));
    }

    #[test]
    fn probe_catches_misaligned_field() {
        // Audit fix 2026-08-05: per-field alignment is now checked.
        let json = r#"{
            "format_version": 1,
            "identity": { "name": "t", "version": "0", "abi_version": 1 },
            "exports": [], "imports": [],
            "types": [{
                "kind": "Struct", "name": "MisalignedField",
                "fields": [
                    { "name": "a", "ty": {"kind":"Primitive","value":"U8"}, "offset": 0 },
                    { "name": "b", "ty": {"kind":"Primitive","value":"U64"}, "offset": 1 }
                ],
                "size": 16, "align": 8
            }]
        }"#;
        let abi = MimiAbi::from_json(json).expect("parse");
        let faults = probe_layout(&abi);
        assert!(
            faults
                .iter()
                .any(|f| matches!(f, LayoutFault::FieldMisaligned { field, align, .. } if field == "b" && *align == 8)),
            "expected FieldMisaligned(b, align 8), got: {faults:?}"
        );
    }

    #[test]
    fn probe_accepts_valid_layout_with_tail_padding() {
        // A valid layout whose last field ends before the struct tail
        // (trailing padding) must stay fault-free.
        let json = r#"{
            "format_version": 1,
            "identity": { "name": "t", "version": "0", "abi_version": 1 },
            "exports": [], "imports": [],
            "types": [{
                "kind": "Struct", "name": "Padded",
                "fields": [
                    { "name": "a", "ty": {"kind":"Primitive","value":"U8"}, "offset": 0 },
                    { "name": "b", "ty": {"kind":"Primitive","value":"U32"}, "offset": 4 }
                ],
                "size": 16, "align": 8
            }]
        }"#;
        let abi = MimiAbi::from_json(json).expect("parse");
        let faults = probe_layout(&abi);
        assert!(faults.is_empty(), "unexpected faults: {faults:?}");
    }

    #[test]
    fn probe_catches_offset_out_of_bounds() {
        let json = r#"{
            "format_version": 1,
            "identity": { "name": "t", "version": "0", "abi_version": 1 },
            "exports": [], "imports": [],
            "types": [{
                "kind": "Struct", "name": "Bad",
                "fields": [
                    { "name": "a", "ty": {"kind":"Primitive","value":"U8"}, "offset": 0 },
                    { "name": "b", "ty": {"kind":"Primitive","value":"U8"}, "offset": 99 }
                ],
                "size": 8, "align": 8
            }]
        }"#;
        let abi = MimiAbi::from_json(json).expect("parse");
        let faults = probe_layout(&abi);
        assert!(faults
            .iter()
            .any(|f| matches!(f, LayoutFault::OffsetOutOfBounds { .. })));
    }

    #[test]
    fn probe_catches_non_monotonic_offsets() {
        let json = r#"{
            "format_version": 1,
            "identity": { "name": "t", "version": "0", "abi_version": 1 },
            "exports": [], "imports": [],
            "types": [{
                "kind": "Struct", "name": "Overlap",
                "fields": [
                    { "name": "a", "ty": {"kind":"Primitive","value":"U64"}, "offset": 8 },
                    { "name": "b", "ty": {"kind":"Primitive","value":"U64"}, "offset": 0 }
                ],
                "size": 16, "align": 8
            }]
        }"#;
        let abi = MimiAbi::from_json(json).expect("parse");
        let faults = probe_layout(&abi);
        assert!(faults
            .iter()
            .any(|f| matches!(f, LayoutFault::OffsetsNotMonotonic { .. })));
    }

    #[test]
    fn probe_catches_bad_alignment() {
        let json = r#"{
            "format_version": 1,
            "identity": { "name": "t", "version": "0", "abi_version": 1 },
            "exports": [], "imports": [],
            "types": [{
                "kind": "Struct", "name": "Misaligned",
                "fields": [],
                "size": 12, "align": 8
            }]
        }"#;
        let abi = MimiAbi::from_json(json).expect("parse");
        let faults = probe_layout(&abi);
        assert!(faults
            .iter()
            .any(|f| matches!(f, LayoutFault::SizeNotAligned { .. })));
    }

    #[test]
    fn mimiabi_roundtrip_is_fixpoint() {
        let mut gen = AbiGenerator::new();
        register_core_runtime_abi(&mut gen);
        let ir = gen.build();
        let abi = MimiAbi::from_component_ir(&ir);

        let json1 = abi.to_json().expect("serialize");
        let abi2 = MimiAbi::from_json(&json1).expect("deserialize");
        let json2 = abi2.to_json().expect("re-serialize");
        assert_eq!(json1, json2, "round-trip is not a fixpoint");
        assert_eq!(abi.hash().unwrap(), abi2.hash().unwrap());
    }

    // ── Allocator provenance ──

    #[test]
    fn well_paired_alloc_free() {
        let mut ledger = AllocLedger::new();
        ledger.record_alloc(0x1000, AllocSide::Mimi).unwrap();
        assert!(ledger.record_free(0x1000, AllocSide::Mimi).is_ok());
        assert_eq!(ledger.leak_count(), 0);
    }

    #[test]
    fn wrong_side_free_detected() {
        let mut ledger = AllocLedger::new();
        ledger.record_alloc(0x2000, AllocSide::Foreign).unwrap();
        assert_eq!(
            ledger.record_free(0x2000, AllocSide::Mimi),
            Err(AllocFault::WrongSide {
                ptr: 0x2000,
                allocated_by: AllocSide::Foreign,
                freed_by: AllocSide::Mimi,
            })
        );
    }

    #[test]
    fn unknown_free_detected() {
        let mut ledger = AllocLedger::new();
        assert_eq!(
            ledger.record_free(0xDEAD, AllocSide::Mimi),
            Err(AllocFault::UnknownPointer(0xDEAD))
        );
    }

    #[test]
    fn double_free_detected() {
        let mut ledger = AllocLedger::new();
        ledger.record_alloc(0x3000, AllocSide::Mimi).unwrap();
        assert!(ledger.record_free(0x3000, AllocSide::Mimi).is_ok());
        assert_eq!(
            ledger.record_free_checked(0x3000, AllocSide::Mimi, true),
            Err(AllocFault::DoubleFree(0x3000))
        );
    }

    /// GAP-5 regression: double allocation must be detected.
    #[test]
    fn double_alloc_detected() {
        let mut ledger = AllocLedger::new();
        ledger.record_alloc(0x4000, AllocSide::Mimi).unwrap();
        assert_eq!(
            ledger.record_alloc(0x4000, AllocSide::Foreign),
            Err(AllocFault::DoubleAlloc(0x4000))
        );
    }

    #[test]
    fn leak_count_reflects_unfreed() {
        let mut ledger = AllocLedger::new();
        ledger.record_alloc(0x10, AllocSide::Mimi).unwrap();
        ledger.record_alloc(0x20, AllocSide::Foreign).unwrap();
        ledger.record_free(0x10, AllocSide::Mimi).unwrap();
        assert_eq!(ledger.leak_count(), 1);
    }

    // ── Handle lease race stress (checkpoint) ──

    #[test]
    fn handle_lease_race_generation_no_reuse() {
        // High-contention mix of acquire/lease/release/destroy across kinds.
        // Any generation-reuse bug would surface as a stale handle succeeding
        // or a live handle being rejected. The registry's own resolve() gates
        // all of these; here we assert the aggregate invariant: no slot ends
        // up double-occupied and all leases balance to zero.
        let reg = Arc::new(HandleRegistry::new(RuntimeId::Native));
        let kinds = [
            HandleKind::List,
            HandleKind::Map,
            HandleKind::Set,
            HandleKind::Foreign,
        ];
        let mut threads = Vec::new();
        for t in 0..8 {
            let reg = Arc::clone(&reg);
            let kind = kinds[t % kinds.len()];
            threads.push(thread::spawn(move || {
                for _ in 0..1000 {
                    let h = reg.acquire(kind).unwrap();
                    // extra lease then over-release must balance
                    reg.lease(&h).unwrap();
                    assert_eq!(reg.release_lease(&h).unwrap(), 1);
                    assert_eq!(reg.release_lease(&h).unwrap(), 0);
                    reg.destroy(&h).unwrap();
                    // after destroy the handle must be stale
                    assert!(!reg.is_live(&h));
                }
            }));
        }
        for th in threads {
            th.join().unwrap();
        }
        assert_eq!(reg.live_count(), 0);
    }
}
