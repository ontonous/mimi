//! Request-level allocation denial for isolated bytecode unit tests.
//!
//! This is independent of the runtime's synthetic allocation stage counter.
//! Only the armed thread rejects requests. One-shot probes reject once;
//! continuing probes stay armed through VM return and failed-frame cleanup.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Observation {
    pub requests: usize,
    pub rejected: usize,
}

#[derive(Clone, Copy, Default)]
struct State {
    active: bool,
    reject_next: bool,
    continuing: bool,
    observation: Observation,
}

thread_local! {
    static STATE: Cell<State> = const { Cell::new(State {
        active: false,
        reject_next: false,
        continuing: false,
        observation: Observation { requests: 0, rejected: 0 },
    }) };
    static SHARED_REQUEST: Cell<Option<(SharedStringSite, bool)>> = const { Cell::new(None) };
    static SHARED_OBSERVATION: Cell<Observation> = const {
        Cell::new(Observation { requests: 0, rejected: 0 })
    };
}

struct RequestAllocator;

fn reject_request() -> bool {
    STATE
        .try_with(|state| {
            let mut current = state.get();
            if !current.active {
                return false;
            }
            current.observation.requests += 1;
            let reject = current.continuing || current.reject_next;
            if reject {
                current.reject_next = false;
                current.observation.rejected += 1;
            }
            state.set(current);
            reject
        })
        .unwrap_or(false)
}

// SAFETY: every permitted allocation/deallocation delegates to System with its
// original layout. Returning null is the documented GlobalAlloc failure mode.
// The thread-local probe uses const Cells and never allocates recursively.
unsafe impl GlobalAlloc for RequestAllocator {
    // SAFETY: GlobalAlloc callers provide a valid nonzero layout; every
    // successful request is served by System with that same layout.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if reject_request() {
            std::ptr::null_mut()
        } else {
            // SAFETY: forward the caller's valid nonzero layout unchanged.
            unsafe { System.alloc(layout) }
        }
    }

    // SAFETY: GlobalAlloc callers provide a valid nonzero layout; permitted
    // requests retain System's zero-initialization guarantee.
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if reject_request() {
            std::ptr::null_mut()
        } else {
            // SAFETY: forward the caller's valid nonzero layout unchanged.
            unsafe { System.alloc_zeroed(layout) }
        }
    }

    // SAFETY: the caller supplies a live allocation from this allocator and
    // its original layout, plus a valid new size. All live storage is System's.
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if reject_request() {
            std::ptr::null_mut()
        } else {
            // SAFETY: preserve the live System pointer, original layout and
            // valid new size provided by the GlobalAlloc caller.
            unsafe { System.realloc(pointer, layout, new_size) }
        }
    }

    // SAFETY: the caller transfers a live allocation from this allocator with
    // its original layout. Every successful allocation delegates to System.
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: pass ownership of the live System allocation back with its
        // original layout; the denial probe never changes allocation origins.
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: RequestAllocator = RequestAllocator;

pub(super) fn with_one_shot_denial<T>(operation: impl FnOnce() -> T) -> (T, Observation) {
    struct Reset(State);
    impl Drop for Reset {
        fn drop(&mut self) {
            STATE.with(|state| state.set(self.0));
        }
    }
    let reset = Reset(STATE.with(|state| {
        let previous = state.get();
        assert!(!previous.active, "allocation probes must not nest");
        state.set(State {
            active: true,
            reject_next: true,
            continuing: false,
            observation: Observation::default(),
        });
        previous
    }));
    let result = operation();
    let observation = STATE.with(|state| state.get().observation);
    drop(reset);
    (result, observation)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SharedStringSite {
    Observer,
    Concat,
}

fn with_shared_denial<T>(
    site: SharedStringSite,
    continuing: bool,
    operation: impl FnOnce() -> T,
) -> (T, Observation) {
    struct Reset(State);
    impl Drop for Reset {
        fn drop(&mut self) {
            STATE.with(|state| state.set(self.0));
            SHARED_REQUEST.with(|request| request.set(None));
        }
    }
    let previous = STATE.with(Cell::get);
    assert!(!previous.active, "allocation probes must not nest");
    SHARED_REQUEST.with(|request| {
        assert!(
            request.replace(Some((site, continuing))).is_none(),
            "shared allocation probes must not nest"
        );
    });
    SHARED_OBSERVATION.with(|observation| observation.set(Observation::default()));
    let reset = Reset(previous);
    let result = operation();
    let observation = if continuing {
        STATE.with(|state| state.get().observation)
    } else {
        SHARED_OBSERVATION.with(Cell::get)
    };
    // Assertions/formatting happen only after the allocator is disarmed.
    drop(reset);
    (result, observation)
}

pub(super) fn with_observer_shared_denial<T>(operation: impl FnOnce() -> T) -> (T, Observation) {
    with_shared_denial(SharedStringSite::Observer, false, operation)
}

pub(super) fn with_observer_shared_continuing_denial<T>(
    operation: impl FnOnce() -> T,
) -> (T, Observation) {
    with_shared_denial(SharedStringSite::Observer, true, operation)
}

pub(super) fn with_concat_shared_continuing_denial<T>(
    operation: impl FnOnce() -> T,
) -> (T, Observation) {
    with_shared_denial(SharedStringSite::Concat, true, operation)
}

/// Arms the real allocator at the selected shared allocation request. Stage
/// failpoints run before this hook and remain independent of allocator denial.
pub(super) fn at_shared_string_request<T>(
    site: SharedStringSite,
    operation: impl FnOnce() -> T,
) -> T {
    let requested = SHARED_REQUEST.with(Cell::get);
    if requested.map(|(requested, _)| requested) != Some(site) {
        return operation();
    }
    let (_, continuing) = SHARED_REQUEST
        .with(|request| request.take())
        .expect("selected shared request");
    if continuing {
        STATE.with(|state| {
            state.set(State {
                active: true,
                reject_next: true,
                continuing: true,
                observation: Observation::default(),
            })
        });
        // The outer with_shared_denial guard resets this state only after VM
        // return; diagnostic propagation and cleanup remain under denial.
        operation()
    } else {
        let (result, observation) = with_one_shot_denial(operation);
        SHARED_OBSERVATION.with(|slot| slot.set(observation));
        result
    }
}
