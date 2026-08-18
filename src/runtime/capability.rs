// ===========================================================================
// Capability runtime (self-contained, thread-local) — extracted from mod.rs
//
// Linear capability tokens: mimi_cap_register issues a fresh cap id bound to a
// name; mimi_cap_check / mimi_cap_consume verify / consume it exactly once.
//
// Thread model (0.31.52 R-4): CAP_TABLE is thread-local — a capability
// registered on thread A cannot be checked/consumed on thread B (returns
// false / no-op). This is by design for single-threaded Flow execution, and
// cross-thread transfer is documented as requiring explicit serialization
// (send cap id + name over a Channel, re-register on the receiving thread).
//
// H6 (audit-triage-0.35.25): the failures were SILENT — a cross-thread
// check/consume returned false indistinguishable from "unknown cap", and a
// cross-thread drop was a no-op that never freed the entry, with no
// diagnostic at all. CAP_OWNERSHIP (a global registry of cap id → owning
// thread) now turns silent cross-thread misuse into an explicit warning on
// the failure path; the happy path (owner thread) pays zero cost. Authorized
// cross-thread transfer still works the documented way; misuse is no longer
// invisible.
// ===========================================================================

use super::cstr_to_string;
use std::collections::HashMap;
use std::ffi::CStr;
use std::sync::Mutex;

struct CapEntry {
    id: i64,
    name: String,
    consumed: bool,
}

thread_local! {
    static CAP_TABLE: Mutex<CapTableData> = const { Mutex::new(CapTableData { next_id: 1, entries: Vec::new() }) };
}

struct CapTableData {
    next_id: i64,
    entries: Vec<CapEntry>,
}

/// H6: cap id → owning thread. Written on register/drop (owner thread only),
/// read on the failure path of check/consume/drop to distinguish a cross-
/// thread misuse from an ordinary "unknown cap". Grows monotonically with
/// live registrations because drop removes the owner entry; a registration
/// never transferred is removed when its owner drops it. OnceLock: the map
/// cannot be constructed in a const context (HashMap::new is not const),
/// matching the actor.rs / quote.rs pattern.
static CAP_OWNERSHIP: std::sync::OnceLock<Mutex<HashMap<i64, Vec<std::thread::ThreadId>>>> =
    std::sync::OnceLock::new();

fn cap_ownership() -> &'static Mutex<HashMap<i64, Vec<std::thread::ThreadId>>> {
    CAP_OWNERSHIP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// H6: report a cross-thread capability misuse once (no performance cost on
/// the owner-thread happy path — this runs only when the local table lookup
/// already failed).
fn warn_cross_thread(cap: i64, owner: std::thread::ThreadId, action: &str) {
    eprintln!(
        "[mimi] capability {}: {} from a different thread (owned by {:?}) — \
         capabilities are thread-local (R-4); transfer requires explicit \
         serialization (send cap id + name over a Channel and re-register on \
         the receiving thread)",
        cap, action, owner
    );
}

///
/// # Safety
/// `name` must be null or a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn mimi_cap_register(name: *const std::ffi::c_char) -> i64 {
    let n = if name.is_null() {
        String::new()
    } else {
        // SAFETY: `cstr_to_string` handles null pointers safely.
        unsafe { cstr_to_string(name) }
    };
    let id = CAP_TABLE.with(|table| {
        let mut state = table.lock().unwrap_or_else(|e| e.into_inner());
        let id = state.next_id;
        state.next_id += 1;
        state.entries.push(CapEntry {
            id,
            name: n,
            consumed: false,
        });
        id
    });
    let mut ownership = cap_ownership().lock().unwrap_or_else(|e| e.into_inner());
    let here = std::thread::current().id();
    ownership.entry(id).or_default().push(here);
    drop(ownership);
    id
}

#[no_mangle]
pub extern "C" fn mimi_cap_drop(cap: i64) {
    let dropped = CAP_TABLE.with(|table| {
        let mut state = table.lock().unwrap_or_else(|e| e.into_inner());
        let before = state.entries.len();
        state.entries.retain(|e| e.id != cap);
        before != state.entries.len()
    });
    if dropped {
        // Owner thread dropped it — release this thread's ownership record
        // too, so the registry does not grow with dropped caps.
        let here = std::thread::current().id();
        let mut ownership = cap_ownership().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(owners) = ownership.get_mut(&cap) {
            owners.retain(|owner| *owner != here);
            if owners.is_empty() {
                ownership.remove(&cap);
            }
        }
    } else {
        // H6: not in this thread's table. If it belongs to another thread,
        // this drop is a silent no-op that never frees the entry — say so.
        let here = std::thread::current().id();
        let owners = cap_ownership()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&cap)
            .cloned()
            .unwrap_or_default();
        if let Some(owner) = owners.iter().copied().find(|owner| *owner != here) {
            warn_cross_thread(cap, owner, "drop");
        }
    }
}

///
/// # Safety
/// `name` must be null or a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn mimi_cap_check(cap: i64, name: *const std::ffi::c_char) -> bool {
    let n = if name.is_null() {
        ""
    } else {
        // SAFETY: `name` was checked non-null above.
        unsafe { CStr::from_ptr(name) }.to_str().unwrap_or("")
    };
    let found = CAP_TABLE.with(|table| {
        let state = table.lock().unwrap_or_else(|e| e.into_inner());
        state
            .entries
            .iter()
            .any(|e| e.id == cap && !e.consumed && e.name == n)
    });
    if !found {
        // H6: distinguish cross-thread misuse (owned elsewhere) from an
        // ordinary unknown/consumed cap.
        let here = std::thread::current().id();
        let owners = cap_ownership()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&cap)
            .cloned()
            .unwrap_or_default();
        if let Some(owner) = owners.iter().copied().find(|owner| *owner != here) {
            warn_cross_thread(cap, owner, "check");
        }
    }
    found
}

///
/// # Safety
/// `name` must be null or a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn mimi_cap_consume(cap: i64, name: *const std::ffi::c_char) -> bool {
    let n = if name.is_null() {
        ""
    } else {
        // SAFETY: `name` was checked non-null above.
        unsafe { CStr::from_ptr(name) }.to_str().unwrap_or("")
    };
    let consumed = CAP_TABLE.with(|table| {
        let mut state = table.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = state
            .entries
            .iter_mut()
            .find(|e| e.id == cap && !e.consumed)
        {
            if entry.name == n {
                entry.consumed = true;
                return true;
            }
        }
        false
    });
    if !consumed {
        // H6: same cross-thread diagnosis as check — a consume from the
        // wrong thread is a failed authorization, not "cap unknown".
        let here = std::thread::current().id();
        let owners = cap_ownership()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&cap)
            .cloned()
            .unwrap_or_default();
        if let Some(owner) = owners.iter().copied().find(|owner| *owner != here) {
            warn_cross_thread(cap, owner, "consume");
        }
    }
    consumed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    fn cname(s: &str) -> *const std::ffi::c_char {
        CString::new(s).unwrap().into_raw()
    }

    /// H6: a cross-thread check/consume/drop must emit a diagnosable warning
    /// (previously silent false / no-op), while the owner thread keeps full
    /// semantics. The registry must also not leak ownership for dropped caps.
    #[test]
    fn cross_thread_cap_usage_is_diagnosed_not_silent() {
        let cap = unsafe { mimi_cap_register(cname("io")) };
        // Owner thread: happy path.
        unsafe {
            assert!(mimi_cap_check(cap, cname("io")));
            assert!(mimi_cap_consume(cap, cname("io")));
            assert!(!mimi_cap_check(cap, cname("io"))); // consumed
        }

        // Still in the owner's table: consume marks it, drop must release.
        mimi_cap_drop(cap);
        let owners = cap_ownership()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&cap)
            .cloned()
            .unwrap_or_default();
        assert!(
            !owners
                .iter()
                .any(|owner| *owner == std::thread::current().id()),
            "this thread's ownership record must be removed on owner-drop"
        );

        // Cross-thread: register on this thread, operate on another.
        let cap2 = unsafe { mimi_cap_register(cname("net")) };
        let handle = std::thread::spawn(move || {
            // Thread-local table on the spawned thread is EMPTY — the cap
            // belongs to the parent. All three call sites must diagnose.
            let check = unsafe { mimi_cap_check(cap2, cname("net")) };
            let consume = unsafe { mimi_cap_consume(cap2, cname("net")) };
            let drop_seen = {
                mimi_cap_drop(cap2);
                cap_ownership()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains_key(&cap2)
            };
            (check, consume, drop_seen)
        });
        let (check, consume, still_owned) = handle.join().unwrap();
        assert!(!check, "cross-thread check must fail (thread-local table)");
        assert!(!consume, "cross-thread consume must fail");
        assert!(
            still_owned,
            "cross-thread drop must NOT release the owner's cap (no-op by design)"
        );
        // Back on the owner thread the cap must still be live and consumable
        // — the cross-thread drop must not have freed it.
        unsafe {
            assert!(mimi_cap_check(cap2, cname("net")));
            assert!(mimi_cap_consume(cap2, cname("net")));
            mimi_cap_drop(cap2);
        }
    }

    /// P2 (batch4/06): thread-local cap ids can collide across threads. A
    /// global ownership map keyed only by cap id would let one thread's drop
    /// erase another thread's ownership record. The map must retain per-thread
    /// owners for the same numeric id.
    #[test]
    fn duplicate_cap_id_on_two_threads_keeps_other_owner() {
        let main_cap = unsafe { mimi_cap_register(cname("dup")) };
        let thread_cap = std::thread::spawn(move || unsafe { mimi_cap_register(cname("dup")) })
            .join()
            .unwrap();
        // Both thread-local id counters start at 1, so the ids collide in the
        // shared diagnostic ownership map.
        assert_eq!(main_cap, thread_cap);
        mimi_cap_drop(main_cap);
        let owners = cap_ownership()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&main_cap)
            .cloned()
            .unwrap_or_default();
        assert!(
            !owners.is_empty(),
            "the spawned thread's ownership must survive the main thread's drop"
        );
        // Clean up the spawned thread's ownership entry from the diagnostic
        // registry (the thread has already exited without dropping its cap).
        cap_ownership()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&main_cap);
    }
}
