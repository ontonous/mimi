//! FFI callback support: passing Mimi closures to C functions.
//!
//! This module provides the infrastructure for passing Mimi closures as C
//! function pointers, with proper userdata and lifecycle management.
//! The actual closure invocation is handled by the interpreter via
//! `register_with_invoker`, which stores a Rust closure alongside the
//! Mimi closure for efficient C callback dispatch.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

/// A registered callback with its C-compatible invoker.
pub struct CallbackHandle {
    /// Reference count for lifecycle management
    pub ref_count: Arc<AtomicI64>,
    /// C-compatible invoker function.
    /// Signature: fn(callback_id: i64, args: &[i64]) -> i64
    pub invoker: Option<Box<dyn Fn(i64, &[i64]) -> i64 + Send + Sync>>,
}

// Safety: userdata is only accessed from C code that respects the protocol.
unsafe impl Send for CallbackHandle {}
unsafe impl Sync for CallbackHandle {}

/// Global table of callback handles
pub struct CallbackTable {
    next_id: AtomicI64,
    handles: Mutex<HashMap<i64, Arc<CallbackHandle>>>,
}

impl CallbackTable {
    /// Create a new callback table
    pub fn new() -> Self {
        Self {
            next_id: AtomicI64::new(1),
            handles: Mutex::new(HashMap::new()),
        }
    }

    /// Register a callback and return its ID.
    /// The `invoker` is a closure that knows how to call the Mimi closure.
    pub fn register(
        &self,
        invoker: Option<Box<dyn Fn(i64, &[i64]) -> i64 + Send + Sync>>,
    ) -> i64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let handle = Arc::new(CallbackHandle {
            ref_count: Arc::new(AtomicI64::new(1)),
            invoker,
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



/// Standard trampoline: 2 args + userdata pattern.
/// C calls this with (callback_id, arg1, arg2, userdata).
pub unsafe extern "C" fn callback_trampoline(
    callback_id: i64,
    arg1: i64,
    arg2: i64,
    userdata: *mut std::ffi::c_void,
) -> i64 {
    if let Some(handle) = CALLBACK_TABLE.get(callback_id) {
        if let Some(ref invoker) = handle.invoker {
            return invoker(callback_id, &[arg1, arg2, userdata as i64]);
        }
        -1
    } else {
        -1
    }
}

/// qsort-style trampoline: compares two elements via userdata callback ID.
/// C calls this with (a_ptr, b_ptr, userdata_ptr_to_callback_id).
/// The two element pointers are passed as raw i64 values so the callback
/// can cast them back to typed pointers as needed.
pub unsafe extern "C" fn qsort_trampoline(
    a: *const std::ffi::c_void,
    b: *const std::ffi::c_void,
    userdata: *mut std::ffi::c_void,
) -> i32 {
    let a_val = a as i64;
    let b_val = b as i64;
    let callback_id = *(userdata as *const i64);
    if let Some(handle) = CALLBACK_TABLE.get(callback_id) {
        if let Some(ref invoker) = handle.invoker {
            return invoker(callback_id, &[a_val, b_val]) as i32;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_callback_registration() {
        let id = CALLBACK_TABLE.register(
            Some(Box::new(|_id: i64, args: &[i64]| -> i64 { args.iter().sum() })),
        );
        assert!(id > 0);
        assert!(CALLBACK_TABLE.get(id).is_some());
        assert!(CALLBACK_TABLE.remove(id));
        assert!(CALLBACK_TABLE.get(id).is_none());
    }

    #[test]
    fn test_callback_invocation() {
        let id = CALLBACK_TABLE.register(
            Some(Box::new(|_id: i64, args: &[i64]| -> i64 { args[0] + args[1] })),
        );
        // Safety: callback_trampoline is a safe-to-call extern "C" function; id is a valid registered callback ID and args are simple integers.
        let result = unsafe { callback_trampoline(id, 3, 4, std::ptr::null_mut()) };
        assert_eq!(result, 7);
        CALLBACK_TABLE.remove(id);
    }
}