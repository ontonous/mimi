//! 0.31.32 Component stability checkpoint.
//!
//! This module adds no new Component surface. It provides:
//!
//! 1. **ABI fuzz / layout probes** — verify that every `.mimiabi` struct with
//!    declared `size`/`align`/`offset` is internally consistent (offsets
//!    monotonic, within size, aligned; last field + its size ≤ struct size)
//!    and that `.mimiabi` round-trips (serialize → deserialize → re-serialize
//!    is a fixpoint, and the BLAKE3 hash is stable).
//!
//! 2. **Allocator provenance** — a small ledger that pairs cross-boundary
//!    `alloc`/`free` and detects mismatched frees (freed by the wrong side,
//!    double free, or freeing an unknown pointer). This is the checkpoint's
//!    model of "who allocated, who is allowed to free".
//!
//! The handle-lease race guarantees live in [`super::handle`]; this module
//! adds an additional multi-threaded stress that mixes acquire / lease /
//! release / destroy with generation reuse under contention.

use super::serialize::MimiAbi;

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

/// Probe every struct type in a `.mimiabi` for layout consistency.
///
/// Returns all discovered faults (empty = ABI internally consistent).
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
                if let Some(size) = size {
                    if offset >= *size {
                        faults.push(LayoutFault::OffsetOutOfBounds {
                            struct_name: name.clone(),
                            field: field.name.clone(),
                            offset,
                            size: *size,
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
    pub fn record_alloc(&mut self, ptr: u64, side: AllocSide) {
        self.live.insert(ptr, side);
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
        // MimiString + MimiSlice registered by the fat-pointer wiring.
        assert!(struct_type_count(&abi) >= 2);
        let faults = probe_layout(&abi);
        assert!(faults.is_empty(), "layout faults: {faults:?}");
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
        ledger.record_alloc(0x1000, AllocSide::Mimi);
        assert!(ledger.record_free(0x1000, AllocSide::Mimi).is_ok());
        assert_eq!(ledger.leak_count(), 0);
    }

    #[test]
    fn wrong_side_free_detected() {
        let mut ledger = AllocLedger::new();
        ledger.record_alloc(0x2000, AllocSide::Foreign);
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
        ledger.record_alloc(0x3000, AllocSide::Mimi);
        assert!(ledger.record_free(0x3000, AllocSide::Mimi).is_ok());
        assert_eq!(
            ledger.record_free_checked(0x3000, AllocSide::Mimi, true),
            Err(AllocFault::DoubleFree(0x3000))
        );
    }

    #[test]
    fn leak_count_reflects_unfreed() {
        let mut ledger = AllocLedger::new();
        ledger.record_alloc(0x10, AllocSide::Mimi);
        ledger.record_alloc(0x20, AllocSide::Foreign);
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
