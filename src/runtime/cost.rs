//! Runtime cost event accounting used by the `no_alloc` contract.
//!
//! The static cost graph lives in [`crate::cost`].  This module deliberately
//! records only events in an explicitly opened region so VM startup, module
//! loading and the host's own allocator traffic are not accidentally reported
//! as program costs.  Counters are process-wide atomics: allocations made by a
//! native worker thread remain inside the same region and cannot disappear in
//! a thread-local report.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static NO_ALLOC_DEPTH: AtomicU64 = AtomicU64::new(0);
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
static RELEASES: AtomicU64 = AtomicU64::new(0);
static RELEASED_BYTES: AtomicU64 = AtomicU64::new(0);
static DEEP_COPIES: AtomicU64 = AtomicU64::new(0);
static COW_COPIES: AtomicU64 = AtomicU64::new(0);
static RESIZES: AtomicU64 = AtomicU64::new(0);
static DEVICE_TRANSFERS: AtomicU64 = AtomicU64::new(0);
static UNKNOWN_EVENTS: AtomicU64 = AtomicU64::new(0);
static NO_ALLOC_VIOLATIONS: AtomicU64 = AtomicU64::new(0);

/// Stable event kinds for the C ABI and machine-readable reports.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeCostEvent {
    Allocate = 1,
    Release = 2,
    DeepCopy = 3,
    Cow = 4,
    Resize = 5,
    DeviceTransfer = 6,
    Unknown = 255,
}

/// Snapshot returned by the native counter API.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeCostSnapshot {
    pub allocations: u64,
    pub allocated_bytes: u64,
    pub releases: u64,
    pub released_bytes: u64,
    pub deep_copies: u64,
    pub cow_copies: u64,
    pub resizes: u64,
    pub device_transfers: u64,
    pub unknown_events: u64,
    pub no_alloc_violations: u64,
}

impl RuntimeCostSnapshot {
    /// True when no program-visible allocation event occurred in the region.
    pub fn is_no_alloc(&self) -> bool {
        self.allocations == 0
            && self.deep_copies == 0
            && self.cow_copies == 0
            && self.resizes == 0
            && self.unknown_events == 0
            && self.no_alloc_violations == 0
    }
}

/// Reset and open a process-wide measurement region.
pub fn begin_region() {
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    RELEASES.store(0, Ordering::Relaxed);
    RELEASED_BYTES.store(0, Ordering::Relaxed);
    DEEP_COPIES.store(0, Ordering::Relaxed);
    COW_COPIES.store(0, Ordering::Relaxed);
    RESIZES.store(0, Ordering::Relaxed);
    DEVICE_TRANSFERS.store(0, Ordering::Relaxed);
    UNKNOWN_EVENTS.store(0, Ordering::Relaxed);
    NO_ALLOC_VIOLATIONS.store(0, Ordering::Relaxed);
    NO_ALLOC_DEPTH.store(0, Ordering::Relaxed);
    ACTIVE.store(true, Ordering::Release);
}

/// Close the active region and return the final counters.
pub fn end_region() -> RuntimeCostSnapshot {
    ACTIVE.store(false, Ordering::Release);
    NO_ALLOC_DEPTH.store(0, Ordering::Relaxed);
    snapshot()
}

/// Begin a nested static `no_alloc` guard inside an active region.
pub fn enter_no_alloc() {
    if !ACTIVE.load(Ordering::Acquire) {
        // A guard without an explicit measurement region has no defined
        // boundary.  Treat that misuse as a violation instead of silently
        // claiming a zero-cost section.
        NO_ALLOC_VIOLATIONS.fetch_add(1, Ordering::Relaxed);
        return;
    }
    NO_ALLOC_DEPTH.fetch_add(1, Ordering::Relaxed);
}

/// Close one `no_alloc` guard.  Returns false if the region observed a
/// program-visible allocation while the guard was active.
pub fn leave_no_alloc() -> bool {
    let previous = NO_ALLOC_DEPTH.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |depth| {
        Some(depth.saturating_sub(1))
    });
    previous.unwrap_or(0) > 0 && NO_ALLOC_VIOLATIONS.load(Ordering::Relaxed) == 0
}

/// Read current counters without changing the active region.
pub fn snapshot() -> RuntimeCostSnapshot {
    RuntimeCostSnapshot {
        allocations: ALLOCATIONS.load(Ordering::Relaxed),
        allocated_bytes: ALLOCATED_BYTES.load(Ordering::Relaxed),
        releases: RELEASES.load(Ordering::Relaxed),
        released_bytes: RELEASED_BYTES.load(Ordering::Relaxed),
        deep_copies: DEEP_COPIES.load(Ordering::Relaxed),
        cow_copies: COW_COPIES.load(Ordering::Relaxed),
        resizes: RESIZES.load(Ordering::Relaxed),
        device_transfers: DEVICE_TRANSFERS.load(Ordering::Relaxed),
        unknown_events: UNKNOWN_EVENTS.load(Ordering::Relaxed),
        no_alloc_violations: NO_ALLOC_VIOLATIONS.load(Ordering::Relaxed),
    }
}

/// Record an allocation from the runtime allocator.  Calls outside an active
/// region are ignored, keeping startup and VM bookkeeping out of the report.
pub fn record_alloc(bytes: usize) {
    if !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    ALLOCATED_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
    if NO_ALLOC_DEPTH.load(Ordering::Relaxed) != 0 {
        NO_ALLOC_VIOLATIONS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record a release from the runtime allocator.
pub fn record_release(bytes: usize) {
    if !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    RELEASES.fetch_add(1, Ordering::Relaxed);
    RELEASED_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
}

/// Record a semantic cost event that may not pass through the allocator.
pub fn record_event(event: RuntimeCostEvent, units: u64) {
    if !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    match event {
        RuntimeCostEvent::Allocate => {
            ALLOCATIONS.fetch_add(units.max(1), Ordering::Relaxed);
            if NO_ALLOC_DEPTH.load(Ordering::Relaxed) != 0 {
                NO_ALLOC_VIOLATIONS.fetch_add(units.max(1), Ordering::Relaxed);
            }
        }
        RuntimeCostEvent::Release => {
            RELEASES.fetch_add(units.max(1), Ordering::Relaxed);
        }
        RuntimeCostEvent::DeepCopy => {
            DEEP_COPIES.fetch_add(units.max(1), Ordering::Relaxed);
            if NO_ALLOC_DEPTH.load(Ordering::Relaxed) != 0 {
                NO_ALLOC_VIOLATIONS.fetch_add(units.max(1), Ordering::Relaxed);
            }
        }
        RuntimeCostEvent::Cow => {
            COW_COPIES.fetch_add(units.max(1), Ordering::Relaxed);
            if NO_ALLOC_DEPTH.load(Ordering::Relaxed) != 0 {
                NO_ALLOC_VIOLATIONS.fetch_add(units.max(1), Ordering::Relaxed);
            }
        }
        RuntimeCostEvent::Resize => {
            RESIZES.fetch_add(units.max(1), Ordering::Relaxed);
            if NO_ALLOC_DEPTH.load(Ordering::Relaxed) != 0 {
                NO_ALLOC_VIOLATIONS.fetch_add(units.max(1), Ordering::Relaxed);
            }
        }
        RuntimeCostEvent::DeviceTransfer => {
            DEVICE_TRANSFERS.fetch_add(units.max(1), Ordering::Relaxed);
            if NO_ALLOC_DEPTH.load(Ordering::Relaxed) != 0 {
                NO_ALLOC_VIOLATIONS.fetch_add(units.max(1), Ordering::Relaxed);
            }
        }
        RuntimeCostEvent::Unknown => {
            UNKNOWN_EVENTS.fetch_add(units.max(1), Ordering::Relaxed);
            if NO_ALLOC_DEPTH.load(Ordering::Relaxed) != 0 {
                NO_ALLOC_VIOLATIONS.fetch_add(units.max(1), Ordering::Relaxed);
            }
        }
    };
}

#[no_mangle]
pub extern "C" fn mimi_cost_begin_region() {
    begin_region();
}

#[no_mangle]
pub extern "C" fn mimi_cost_end_region() -> RuntimeCostSnapshot {
    end_region()
}

#[no_mangle]
pub extern "C" fn mimi_cost_snapshot() -> RuntimeCostSnapshot {
    snapshot()
}

#[no_mangle]
pub extern "C" fn mimi_cost_enter_no_alloc() {
    enter_no_alloc();
}

/// Return 1 if the current no-alloc guard closed without a violation, 0 if it
/// observed an allocation/unknown event.
#[no_mangle]
pub extern "C" fn mimi_cost_leave_no_alloc() -> i32 {
    leave_no_alloc() as i32
}

#[no_mangle]
pub extern "C" fn mimi_cost_record_event(kind: u8, units: u64) {
    let event = match kind {
        1 => RuntimeCostEvent::Allocate,
        2 => RuntimeCostEvent::Release,
        3 => RuntimeCostEvent::DeepCopy,
        4 => RuntimeCostEvent::Cow,
        5 => RuntimeCostEvent::Resize,
        6 => RuntimeCostEvent::DeviceTransfer,
        _ => RuntimeCostEvent::Unknown,
    };
    record_event(event, units);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_alloc_guard_counts_allocations_and_unknown_events() {
        begin_region();
        enter_no_alloc();
        record_alloc(32);
        record_event(RuntimeCostEvent::Unknown, 1);
        assert!(!leave_no_alloc());
        let report = end_region();
        assert_eq!(report.allocations, 1);
        assert_eq!(report.allocated_bytes, 32);
        assert_eq!(report.unknown_events, 1);
        assert_eq!(report.no_alloc_violations, 2);
        assert!(!report.is_no_alloc());
    }

    #[test]
    fn inactive_region_is_quiet() {
        let before = snapshot();
        record_alloc(1024);
        record_event(RuntimeCostEvent::DeepCopy, 3);
        assert_eq!(snapshot(), before);
    }

    #[test]
    fn device_transfer_is_counted_as_no_alloc_violation() {
        begin_region();
        enter_no_alloc();
        record_event(RuntimeCostEvent::DeviceTransfer, 1);
        assert!(!leave_no_alloc());
        let report = end_region();
        assert_eq!(report.device_transfers, 1);
        assert_eq!(report.no_alloc_violations, 1);
        assert!(!report.is_no_alloc());
    }
}
