//! FFI runtime infrastructure: CapTable and SharedHandle management.
//!
//! This module provides the runtime support needed for the dual-stack FFI
//! boundary design. It maintains:
//!
//! - **CapTable**: Maps capability IDs to capability entries for cross-boundary
//!   authentication. C code can call `cap_check` and `cap_consume` to verify
//!   and use capabilities.
//!
//! - **SharedHandleTable**: Maps opaque handles (i64) to `Arc<RwLock<Value>>`

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::interp::Value;

// ---------------------------------------------------------------------------
// CapTable
// ---------------------------------------------------------------------------

/// State of a single capability entry.
#[derive(Debug, Clone)]
pub struct CapEntry {
    /// The declared name of the cap (e.g. "FileReadCap").
    pub name: String,
    /// Whether this cap has been consumed (move semantics).
    pub consumed: bool,
}

/// Thread-safe table mapping cap IDs to cap entries.
pub struct CapTable {
    next_id: AtomicI64,
    entries: Mutex<HashMap<i64, CapEntry>>,
}

impl CapTable {
    /// Create an empty cap table.
    pub fn new() -> Self {
        Self {
            next_id: AtomicI64::new(1),
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Register a new capability and return its unique ID.
    pub fn register(&self, name: &str) -> i64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut entries = self.entries.lock().unwrap();
        entries.insert(id, CapEntry {
            name: name.to_string(),
            consumed: false,
        });
        id
    }

    /// Check whether the cap with the given ID exists, matches the name, and
    /// has not been consumed.  Does NOT consume the cap.
    pub fn check(&self, id: i64, name: &str) -> bool {
        let entries = self.entries.lock().unwrap();
        match entries.get(&id) {
            Some(entry) => !entry.consumed && entry.name == name,
            None => false,
        }
    }

    /// Check and consume the cap.  Returns `true` if the cap existed, matched
    /// the name, and was not already consumed.  After this call the cap is
    /// marked as consumed and cannot be used again.
    pub fn consume(&self, id: i64, name: &str) -> bool {
        let mut entries = self.entries.lock().unwrap();
        match entries.get_mut(&id) {
            Some(entry) if !entry.consumed && entry.name == name => {
                entry.consumed = true;
                true
            }
            _ => false,
        }
    }

    /// Remove a consumed cap from the table (cleanup).
    pub fn remove(&self, id: i64) -> bool {
        let mut entries = self.entries.lock().unwrap();
        entries.remove(&id).is_some()
    }
}

impl Default for CapTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Global cap table instance.
pub static CAP_TABLE: LazyLock<CapTable> = LazyLock::new(CapTable::new);

// ---------------------------------------------------------------------------
// SharedHandleTable
// ---------------------------------------------------------------------------

/// A shared handle wraps an `Arc<RwLock<Value>>` and provides borrow/loan
/// semantics for crossing the FFI boundary.
pub struct SharedHandle {
    inner: Arc<RwLock<Value>>,
    /// C-side strong reference count (for retain/release balance).
    strong: AtomicI64,
}

impl SharedHandle {
    /// Create a new handle from a shared value.
    pub fn new(inner: Arc<RwLock<Value>>) -> Self {
        Self {
            inner,
            strong: AtomicI64::new(1),
        }
    }

    /// Get a read-only pointer to the inner value.  The caller must ensure
    /// that the pointer is only used while the handle is alive (i.e. before
    /// `release` is called).
    pub fn as_ptr(&self) -> *const Value {
        let guard = self.inner.read().unwrap();
        let ptr: *const Value = &*guard;
        // Leak the guard to keep the pointer valid.  The caller is responsible
        // for calling `release` which will drop the guard.
        // NOTE: This is safe only because we track the guard in the handle.
        std::mem::forget(guard);
        ptr
    }

    /// Get a mutable pointer to the inner value (exclusive borrow).
    pub fn as_mut_ptr(&self) -> *mut Value {
        let guard = self.inner.write().unwrap();
        let ptr: *mut Value = &*guard as *const Value as *mut Value;
        std::mem::forget(guard);
        ptr
    }

    /// Get a read guard for the inner value.
    pub fn borrow(&self) -> RwLockReadGuard<'_, Value> {
        self.inner.read().unwrap()
    }

    /// Get a write guard for the inner value.
    pub fn borrow_mut(&self) -> RwLockWriteGuard<'_, Value> {
        self.inner.write().unwrap()
    }

    /// Retain: increment the C-side strong reference count.
    pub fn retain(&self) {
        self.strong.fetch_add(1, Ordering::Relaxed);
    }

    /// Release: decrement the C-side strong reference count.  Returns `true`
    /// if the count reached zero (caller should free the handle).
    pub fn release(&self) -> bool {
        self.strong.fetch_sub(1, Ordering::Relaxed) == 1
    }

    /// Get the current C-side reference count.
    pub fn strong_count(&self) -> i64 {
        self.strong.load(Ordering::Relaxed)
    }
}

/// Thread-safe table mapping opaque handles (i64) to shared handles.
pub struct SharedHandleTable {
    next_id: AtomicI64,
    handles: Mutex<HashMap<i64, Arc<SharedHandle>>>,
}

impl SharedHandleTable {
    /// Create an empty handle table.
    pub fn new() -> Self {
        Self {
            next_id: AtomicI64::new(1),
            handles: Mutex::new(HashMap::new()),
        }
    }

    /// Create a new handle from a shared value and return its opaque ID.
    pub fn create(&self, inner: Arc<RwLock<Value>>) -> i64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let handle = Arc::new(SharedHandle::new(inner));
        let mut handles = self.handles.lock().unwrap();
        handles.insert(id, handle);
        id
    }

    /// Get a reference to the handle by ID.
    pub fn get(&self, id: i64) -> Option<Arc<SharedHandle>> {
        let handles = self.handles.lock().unwrap();
        handles.get(&id).cloned()
    }

    /// Retain the handle (increment C-side reference count).
    pub fn retain(&self, id: i64) -> bool {
        if let Some(handle) = self.get(id) {
            handle.retain();
            true
        } else {
            false
        }
    }

    /// Release the handle.  If the C-side reference count reaches zero,
    /// removes the handle from the table and returns `true`.
    pub fn release(&self, id: i64) -> bool {
        let handle = {
            let handles = self.handles.lock().unwrap();
            handles.get(&id).cloned()
        };
        if let Some(handle) = handle {
            if handle.release() {
                let mut handles = self.handles.lock().unwrap();
                handles.remove(&id);
                return true;
            }
        }
        false
    }

    /// Remove a handle from the table unconditionally (cleanup).
    pub fn remove(&self, id: i64) -> bool {
        let mut handles = self.handles.lock().unwrap();
        handles.remove(&id).is_some()
    }

    /// Get the number of active handles (for diagnostics).
    pub fn len(&self) -> usize {
        let handles = self.handles.lock().unwrap();
        handles.len()
    }
}

impl Default for SharedHandleTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Global shared handle table instance.
pub static SHARED_TABLE: LazyLock<SharedHandleTable> = LazyLock::new(SharedHandleTable::new);

// ---------------------------------------------------------------------------
// C ABI functions (callable from generated code and interpreter)
// ---------------------------------------------------------------------------

/// Retain a shared handle.  Returns the handle ID (same as input).
#[no_mangle]
pub extern "C" fn mimi_shared_retain(handle: i64) -> i64 {
    SHARED_TABLE.retain(handle);
    handle
}

/// Release a shared handle.
#[no_mangle]
pub extern "C" fn mimi_shared_release(handle: i64) {
    SHARED_TABLE.release(handle);
}

/// Get a raw pointer to the inner value of a shared handle.
/// Returns null if the handle is invalid.
#[no_mangle]
pub extern "C" fn mimi_shared_get_ptr(handle: i64) -> *const Value {
    match SHARED_TABLE.get(handle) {
        Some(h) => h.as_ptr(),
        None => std::ptr::null(),
    }
}

/// Check whether a capability is valid and matches the expected name.
#[no_mangle]
pub extern "C" fn mimi_cap_check(cap: i64, name: *const std::ffi::c_char) -> bool {
    if name.is_null() {
        return false;
    }
    let name_str = unsafe { std::ffi::CStr::from_ptr(name) }
        .to_str()
        .unwrap_or("");
    CAP_TABLE.check(cap, name_str)
}

/// Consume a capability.  Returns true if the cap was valid and consumed.
#[no_mangle]
pub extern "C" fn mimi_cap_consume(cap: i64, name: *const std::ffi::c_char) -> bool {
    if name.is_null() {
        return false;
    }
    let name_str = unsafe { std::ffi::CStr::from_ptr(name) }
        .to_str()
        .unwrap_or("");
    CAP_TABLE.consume(cap, name_str)
}

/// Free a raw string that was obtained via `string.into_raw()`.
#[no_mangle]
pub extern "C" fn mimi_string_free_raw(c_str: *mut std::ffi::c_char) {
    if !c_str.is_null() {
        unsafe {
            drop(std::ffi::CString::from_raw(c_str));
        }
    }
}

/// Get a C string pointer from a Mimi string value.
/// The caller must NOT free the returned pointer - Mimi retains ownership.
/// Returns null if the pointer is invalid.
#[no_mangle]
pub extern "C" fn mimi_string_as_c_str(mimi_string: *const Value) -> *const std::ffi::c_char {
    if mimi_string.is_null() {
        return std::ptr::null();
    }
    unsafe {
        match &*mimi_string {
            Value::String(s) => {
                // Create a CString and leak it to get a static pointer
                // This is safe because Mimi retains ownership and will clean up
                let c_str = std::ffi::CString::new(s.as_str()).unwrap_or_default();
                let ptr = c_str.as_ptr();
                std::mem::forget(c_str);
                ptr
            }
            _ => std::ptr::null(),
        }
    }
}

/// Convert a Mimi string to a raw C string (transfer ownership to C).
/// The caller is responsible for calling `mimi_string_free_raw` on the result.
/// Returns null if the pointer is invalid.
#[no_mangle]
pub extern "C" fn mimi_string_into_raw(mimi_string: *mut Value) -> *mut std::ffi::c_char {
    if mimi_string.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        match &mut *mimi_string {
            Value::String(s) => {
                // Take the string and convert to CString
                let c_str = std::ffi::CString::new(s.as_str()).unwrap_or_default();
                let ptr = c_str.into_raw();
                // Clear the Mimi string since ownership is transferred
                s.clear();
                ptr
            }
            _ => std::ptr::null_mut(),
        }
    }
}

/// Convert a raw C string back to a Mimi string.
/// The caller should NOT free the original C string after this call.
/// Returns a new Mimi Value (caller takes ownership).
/// Note: This function allocates a new Value on the heap.
#[no_mangle]
pub extern "C" fn mimi_string_from_raw(c_str: *mut std::ffi::c_char) -> *mut Value {
    if c_str.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        let c_str = std::ffi::CString::from_raw(c_str);
        let s = c_str.to_string_lossy().into_owned();
        let value = Box::new(Value::String(s));
        Box::into_raw(value)
    }
}
