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
static CAP_OWNERSHIP: std::sync::OnceLock<Mutex<HashMap<i64, std::thread::ThreadId>>> =
    std::sync::OnceLock::new();

fn cap_ownership() -> &'static Mutex<HashMap<i64, std::thread::ThreadId>> {
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

#[no_mangle]
pub extern "C" fn mimi_cap_register(name: *const std::ffi::c_char) -> i64 {
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
    cap_ownership()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id, std::thread::current().id());
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
        // Owner thread dropped it — release the ownership record too, so
        // the registry does not grow with dropped caps.
        cap_ownership()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&cap);
    } else {
        // H6: not in this thread's table. If it belongs to another thread,
        // this drop is a silent no-op that never frees the entry — say so.
        let owner = cap_ownership()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&cap)
            .copied();
        if let Some(owner) = owner {
            warn_cross_thread(cap, owner, "drop");
        }
    }
}

#[no_mangle]
pub extern "C" fn mimi_cap_check(cap: i64, name: *const std::ffi::c_char) -> bool {
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
        let owner = cap_ownership()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&cap)
            .copied();
        if let Some(owner) = owner {
            let here = std::thread::current().id();
            if owner != here {
                warn_cross_thread(cap, owner, "check");
            }
        }
    }
    found
}

#[no_mangle]
pub extern "C" fn mimi_cap_consume(cap: i64, name: *const std::ffi::c_char) -> bool {
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
        let owner = cap_ownership()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&cap)
            .copied();
        if let Some(owner) = owner {
            let here = std::thread::current().id();
            if owner != here {
                warn_cross_thread(cap, owner, "consume");
            }
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
        let cap = mimi_cap_register(cname("io"));
        // Owner thread: happy path.
        assert!(mimi_cap_check(cap, cname("io")));
        assert!(mimi_cap_consume(cap, cname("io")));
        assert!(!mimi_cap_check(cap, cname("io"))); // consumed

        // Still in the owner's table: consume marks it, drop must release.
        mimi_cap_drop(cap);
        assert!(
            cap_ownership()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&cap)
                .is_none(),
            "ownership record must be removed on owner-drop"
        );

        // Cross-thread: register on this thread, operate on another.
        let cap2 = mimi_cap_register(cname("net"));
        let handle = std::thread::spawn(move || {
            // Thread-local table on the spawned thread is EMPTY — the cap
            // belongs to the parent. All three call sites must diagnose.
            let check = mimi_cap_check(cap2, cname("net"));
            let consume = mimi_cap_consume(cap2, cname("net"));
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
        assert!(mimi_cap_check(cap2, cname("net")));
        assert!(mimi_cap_consume(cap2, cname("net")));
        mimi_cap_drop(cap2);
    }
}
