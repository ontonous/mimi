// ===========================================================================
// Capability runtime (self-contained, thread-local) — extracted from mod.rs
//
// Linear capability tokens: mimi_cap_register issues a fresh cap id bound to a
// name; mimi_cap_check / mimi_cap_consume verify / consume it exactly once.
// ===========================================================================

use super::cstr_to_string;
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

#[no_mangle]
pub extern "C" fn mimi_cap_register(name: *const std::ffi::c_char) -> i64 {
    let n = if name.is_null() {
        String::new()
    } else {
        // SAFETY: `cstr_to_string` handles null pointers safely.
        unsafe { cstr_to_string(name) }
    };
    CAP_TABLE.with(|table| {
        let mut state = table.lock().unwrap_or_else(|e| e.into_inner());
        let id = state.next_id;
        state.next_id += 1;
        state.entries.push(CapEntry {
            id,
            name: n,
            consumed: false,
        });
        id
    })
}

#[no_mangle]
pub extern "C" fn mimi_cap_check(cap: i64, name: *const std::ffi::c_char) -> bool {
    let n = if name.is_null() {
        ""
    } else {
        // SAFETY: `name` was checked non-null above.
        unsafe { CStr::from_ptr(name) }.to_str().unwrap_or("")
    };
    CAP_TABLE.with(|table| {
        let state = table.lock().unwrap_or_else(|e| e.into_inner());
        state
            .entries
            .iter()
            .any(|e| e.id == cap && !e.consumed && e.name == n)
    })
}

#[no_mangle]
pub extern "C" fn mimi_cap_consume(cap: i64, name: *const std::ffi::c_char) -> bool {
    let n = if name.is_null() {
        ""
    } else {
        // SAFETY: `name` was checked non-null above.
        unsafe { CStr::from_ptr(name) }.to_str().unwrap_or("")
    };
    CAP_TABLE.with(|table| {
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
    })
}
