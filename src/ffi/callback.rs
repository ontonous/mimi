//! FFI callback support: passing Mimi closures to C functions.
//!
//! This module provides the infrastructure for passing Mimi closures as C
//! function pointers, with proper userdata and lifecycle management.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::interp::Value;

/// A callback handle that wraps a Mimi closure for use as a C function pointer.
/// The handle is stored in a global table and referenced by an integer ID.
pub struct CallbackHandle {
    /// The Mimi closure to call
    pub closure: Value,
    /// Optional userdata pointer (passed as void* to C)
    pub userdata: Option<*mut std::ffi::c_void>,
    /// Reference count for lifecycle management
    pub ref_count: Arc<std::sync::atomic::AtomicI64>,
}

// Safety: CallbackHandle is only accessed through the global table
unsafe impl Send for CallbackHandle {}
unsafe impl Sync for CallbackHandle {}

/// Global table of callback handles
pub struct CallbackTable {
    next_id: AtomicI64,
    handles: Mutex<HashMap<i64, Arc<CallbackHandle>>>,
}

use std::sync::atomic::{AtomicI64, Ordering};

impl CallbackTable {
    /// Create a new callback table
    pub fn new() -> Self {
        Self {
            next_id: AtomicI64::new(1),
            handles: Mutex::new(HashMap::new()),
        }
    }

    /// Register a callback and return its ID
    pub fn register(&self, closure: Value, userdata: Option<*mut std::ffi::c_void>) -> i64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let handle = Arc::new(CallbackHandle {
            closure,
            userdata,
            ref_count: Arc::new(std::sync::atomic::AtomicI64::new(1)),
        });
        let mut handles = self.handles.lock().unwrap();
        handles.insert(id, handle);
        id
    }

    /// Get a callback handle by ID
    pub fn get(&self, id: i64) -> Option<Arc<CallbackHandle>> {
        let handles = self.handles.lock().unwrap();
        handles.get(&id).cloned()
    }

    /// Remove a callback handle
    pub fn remove(&self, id: i64) -> bool {
        let mut handles = self.handles.lock().unwrap();
        handles.remove(&id).is_some()
    }
}

impl Default for CallbackTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Global callback table instance
pub static CALLBACK_TABLE: LazyLock<CallbackTable> = LazyLock::new(CallbackTable::new);

use std::sync::LazyLock;

/// Create a C-compatible callback function pointer from a Mimi closure.
///
/// This function returns a function pointer that can be passed to C functions
/// expecting a callback. The closure is stored in the global callback table
/// and referenced by the returned pointer.
///
/// # Safety
/// The returned function pointer is only valid while the callback is registered.
/// The callback must be explicitly unregistered when no longer needed.
pub unsafe extern "C" fn callback_trampoline<F: Fn(*mut std::ffi::c_void, i64, i64) -> i64>(
    callback_id: i64,
    arg1: i64,
    arg2: i64,
    userdata: *mut std::ffi::c_void,
) -> i64 {
    if let Some(handle) = CALLBACK_TABLE.get(callback_id) {
        // Call the Mimi closure
        // Note: This is a simplified implementation. A full implementation would
        // need to properly invoke the Mimi closure with the correct arguments.
        // For now, we return 0 as a placeholder.
        0
    } else {
        -1 // Error: callback not found
    }
}

/// Create a callback wrapper for qsort-style callbacks
///
/// # Safety
/// The returned function pointer must only be used with the qsort function
/// and only while the callback is registered.
pub unsafe extern "C" fn qsort_trampoline(
    a: *const std::ffi::c_void,
    b: *const std::ffi::c_void,
    userdata: *mut std::ffi::c_void,
) -> i32 {
    // Extract the callback ID from userdata
    let callback_id = *(userdata as *const i64);

    if let Some(handle) = CALLBACK_TABLE.get(callback_id) {
        // Call the Mimi closure with the two pointers
        // Note: This is a simplified implementation. A full implementation would
        // need to properly invoke the Mimi closure with the correct arguments.
        // For now, we return 0 as a placeholder.
        0
    } else {
        0 // Default: equal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_callback_registration() {
        let closure = Value::Unit; // Placeholder
        let id = CALLBACK_TABLE.register(closure, None);
        assert!(id > 0);
        assert!(CALLBACK_TABLE.get(id).is_some());
        assert!(CALLBACK_TABLE.remove(id));
        assert!(CALLBACK_TABLE.get(id).is_none());
    }
}
