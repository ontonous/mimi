//! Heap free-uniqueness guard (0.39.x matrix sweep, LOOP-REBIND-HEAP-001).
//!
//! The native emitter registers heap ownership from multiple independent
//! sources — value construction (`build_list_struct`), call-result tracking
//! (`track_returned_heap_pointers`), and binding transfers. When a callee
//! returns its receiver verbatim (`impl ListExt { func take() { … self } }`),
//! the caller's argument materialization slot and the returned-value
//! registration end up pointing at the SAME allocation, and the function-exit
//! flush freed it twice.
//!
//! Rather than auditing every registration path forever, this module makes
//! release uniqueness an ENFORCED INVARIANT: within one scope-flush session,
//! each pointer is released at most once. Duplicate registrations degrade to
//! a leak (the safe direction), never to a double free.

use std::cell::RefCell;
use std::collections::HashSet;

thread_local! {
    /// Pointers already handed to `free` during the current flush session.
    static FREED: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
}

/// Start a fresh flush session: forget every pointer recorded so far.
///
/// Emitted at the top of each scope-flush sequence (function exit, loop-body
/// per-iteration drop, break/continue flush). Resetting per session is what
/// keeps the table bounded and lets the same address be safely re-freed in a
/// LATER session should a slot legitimately hold a fresh allocation at that
/// address.
///
/// # Safety
/// C ABI only; no pointer arguments.
#[no_mangle]
pub unsafe extern "C" fn mimi_heap_guard_reset() {
    FREED.with(|t| t.borrow_mut().clear());
}

/// Record `p` as about-to-be-freed and return it; return null if `p` was
/// already released in this session (caller must skip the free). Null input
/// passes through as null — `free(null)` is a no-op either way.
///
/// # Safety
/// `p` must be null or a pointer previously returned by the allocator.
#[no_mangle]
pub unsafe extern "C" fn mimi_heap_free_claim(p: *mut std::ffi::c_void) -> *mut std::ffi::c_void {
    if p.is_null() {
        return std::ptr::null_mut();
    }
    let addr = p as usize;
    FREED.with(|t| {
        let mut set = t.borrow_mut();
        if set.insert(addr) {
            p
        } else {
            std::ptr::null_mut()
        }
    })
}
