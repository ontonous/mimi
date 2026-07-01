#![allow(dead_code)]
// Mutex/RwLock poisoning panics are intentional — inconsistent data after a
// poisoned lock is unrecoverable; aborting is the safe choice.
#![cfg_attr(not(test), allow(clippy::unwrap_used))]

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

use std::cell::RefCell;
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
        let mut entries = self
            .entries
            .lock()
            .expect("CAP_TABLE entries lock poisoned");
        entries.insert(
            id,
            CapEntry {
                name: name.to_string(),
                consumed: false,
            },
        );
        id
    }

    /// Check whether the cap with the given ID exists, matches the name, and
    /// has not been consumed.  Does NOT consume the cap.
    pub fn check(&self, id: i64, name: &str) -> bool {
        let entries = self
            .entries
            .lock()
            .expect("CAP_TABLE entries lock poisoned");
        match entries.get(&id) {
            Some(entry) => !entry.consumed && entry.name == name,
            None => false,
        }
    }

    /// Check and consume the cap.  Returns `true` if the cap existed, matched
    /// the name, and was not already consumed.  After this call the cap is
    /// marked as consumed and cannot be used again.
    pub fn consume(&self, id: i64, name: &str) -> bool {
        let mut entries = self
            .entries
            .lock()
            .expect("CAP_TABLE entries lock poisoned");
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
        let mut entries = self
            .entries
            .lock()
            .expect("CAP_TABLE entries lock poisoned");
        entries.remove(&id).is_some()
    }
}

impl Default for CapTable {
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    /// Per-thread capability table — tracks linear/affine cap values across FFI calls.
    ///
    /// Using a thread-local table isolates cap state per Mimi invocation/test
    /// thread. Caps registered on one thread are only visible to C code invoked
    /// on that same thread.
    static CAP_TABLE: CapTable = CapTable::new();
}

/// Execute a closure with the current thread's capability table.
pub fn with_cap_table<R, F: FnOnce(&CapTable) -> R>(f: F) -> R {
    CAP_TABLE.with(f)
}

// ---------------------------------------------------------------------------
// SharedHandleTable
// ---------------------------------------------------------------------------

/// A shared handle wraps an `Arc<RwLock<Value>>` and provides borrow/loan
/// semantics for crossing the FFI boundary.
pub struct SharedHandle {
    id: i64,
    inner: Arc<RwLock<Value>>,
    /// C-side strong reference count (for retain/release balance).
    strong: AtomicI64,
}

impl SharedHandle {
    /// Create a new handle from a shared value.
    pub fn new(id: i64, inner: Arc<RwLock<Value>>) -> Self {
        Self {
            id,
            inner,
            strong: AtomicI64::new(1),
        }
    }

    /// Execute a closure with a read-only reference to the inner value.
    /// Safe, scoped access — prefer this over raw pointer APIs.
    pub fn with_value<R>(&self, f: impl FnOnce(&Value) -> R) -> R {
        let guard = self
            .inner
            .read()
            .expect("SharedHandle inner read lock poisoned");
        f(&guard)
    }

    /// Execute a closure with a mutable reference to the inner value.
    /// Safe, scoped access — prefer this over raw pointer APIs.
    pub fn with_value_mut<R>(&self, f: impl FnOnce(&mut Value) -> R) -> R {
        let mut guard = self
            .inner
            .write()
            .expect("SharedHandle inner write lock poisoned");
        f(&mut guard)
    }

    /// Get a read guard for the inner value.
    pub fn borrow(&self) -> RwLockReadGuard<'_, Value> {
        self.inner
            .read()
            .expect("SharedHandle inner read lock poisoned")
    }

    /// Get a write guard for the inner value.
    pub fn borrow_mut(&self) -> RwLockWriteGuard<'_, Value> {
        self.inner
            .write()
            .expect("SharedHandle inner write lock poisoned")
    }

    /// Retain: increment the C-side strong reference count.
    pub fn retain(&self) {
        self.strong.fetch_add(1, Ordering::Relaxed);
    }

    /// Release: decrement the C-side strong reference count.  Returns `true`
    /// if the count reached zero (caller should free the handle).
    /// Uses Release ordering to ensure all writes to managed data are visible
    /// before the count reaches zero (matching mimi_rc_release pattern).
    pub fn release(&self) -> bool {
        self.strong.fetch_sub(1, Ordering::Release) == 1
    }

    /// Acquire fence after release returns true — ensures visibility of
    /// all writes made before the last release.
    pub fn release_with_fence(&self) -> bool {
        let reached_zero = self.strong.fetch_sub(1, Ordering::Release) == 1;
        if reached_zero {
            std::sync::atomic::fence(Ordering::Acquire);
        }
        reached_zero
    }

    /// Get the current C-side reference count.
    pub fn strong_count(&self) -> i64 {
        self.strong.load(Ordering::Relaxed)
    }

    /// Get the opaque handle ID.
    pub fn id(&self) -> i64 {
        self.id
    }
}

impl Drop for SharedHandle {
    fn drop(&mut self) {
        let _ = with_shared_table(|table| table.remove(self.id));
    }
}

/// Thread-safe table mapping opaque handles (i64) to shared handles.
/// Also provides cross-call deduplication: the same Arc pointer always maps
/// to the same handle ID, preventing handle table growth on repeated FFI
/// calls that pass the same shared value.
pub struct SharedHandleTable {
    next_id: AtomicI64,
    handles: Mutex<HashMap<i64, Arc<SharedHandle>>>,
    /// Cross-call dedup: Arc inner pointer → handle ID.
    /// Entries are cleaned up when the handle is removed from the table.
    dedup: Mutex<HashMap<*const (), i64>>,
}

impl SharedHandleTable {
    /// Create an empty handle table.
    pub fn new() -> Self {
        Self {
            next_id: AtomicI64::new(1),
            handles: Mutex::new(HashMap::new()),
            dedup: Mutex::new(HashMap::new()),
        }
    }

    /// Create a new handle from a shared value and return its opaque ID.
    pub fn create(&self, inner: Arc<RwLock<Value>>) -> i64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let handle = Arc::new(SharedHandle::new(id, inner));
        let mut handles = self
            .handles
            .lock()
            .expect("SHARED_TABLE handles lock poisoned");
        handles.insert(id, handle);
        id
    }

    /// Create or reuse a handle, deduplicating by Arc inner pointer.
    /// If the same Arc pointer has been registered before, returns the
    /// existing handle ID and increments the C-side reference count.
    ///
    /// # Deduplication semantics (FFI-DESIGN-4)
    /// Deduplication is by `Arc::as_ptr(arc) as *const ()` — i.e., the
    /// **identity** of the Arc allocation, not its content. Two `Arc`
    /// values that are equal in Mimi semantics but are distinct heap
    /// allocations will NOT be deduplicated. This is intentional: in FFI
    /// contexts, value equality ≠ pointer identity; deduplicating by content
    /// would conflate distinct handle lifetimes.
    pub fn create_dedup(&self, inner: Arc<RwLock<Value>>, ptr: *const ()) -> i64 {
        // Check dedup table first (lock dedup, then release before locking handles)
        {
            let dedup = self.dedup.lock().expect("SHARED_TABLE dedup lock poisoned");
            if let Some(&existing_id) = dedup.get(&ptr) {
                // Found existing handle — retain and return existing ID.
                // Don't hold dedup lock while touching handles to avoid deadlock.
                drop(dedup);
                if self.retain(existing_id) {
                    return existing_id;
                }
                // FFI-BUG-1: Handle was cleaned up but dedup entry is stale.
                // Remove it to prevent reuse of released handle ID and dedup pollution.
                if let Ok(mut dedup) = self.dedup.lock() {
                    dedup.remove(&ptr);
                }
                // Fall through to create new handle.
            }
        }

        // Create new handle
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let handle = Arc::new(SharedHandle::new(id, inner));
        {
            let mut handles = self
                .handles
                .lock()
                .expect("SHARED_TABLE handles lock poisoned");
            handles.insert(id, handle);
        }
        {
            let mut dedup = self.dedup.lock().expect("SHARED_TABLE dedup lock poisoned");
            dedup.insert(ptr, id);
        }
        id
    }

    /// Get a reference to the handle by ID.
    pub fn get(&self, id: i64) -> Option<Arc<SharedHandle>> {
        let handles = self
            .handles
            .lock()
            .expect("SHARED_TABLE handles lock poisoned");
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
            let handles = self
                .handles
                .lock()
                .expect("SHARED_TABLE handles lock poisoned");
            handles.get(&id).cloned()
        };
        if let Some(handle) = handle {
            if handle.release() {
                let removed = {
                    let mut handles = self
                        .handles
                        .lock()
                        .expect("SHARED_TABLE handles lock poisoned");
                    handles.remove(&id).is_some()
                };
                if removed {
                    // Clean up dedup entry (lock order: handles first, dedup second,
                    // consistent with remove()). The table is thread-local so no
                    // concurrent access to the same table is possible.
                    if let Ok(mut dedup) = self.dedup.lock() {
                        dedup.retain(|_, &mut vid| vid != id);
                    }
                }
                return true;
            }
        }
        false
    }

    /// Remove a handle from the table unconditionally (cleanup).
    /// Also removes the corresponding dedup entry.
    pub fn remove(&self, id: i64) -> bool {
        let removed = {
            let mut handles = self
                .handles
                .lock()
                .expect("SHARED_TABLE handles lock poisoned");
            handles.remove(&id).is_some()
        };
        if removed {
            // Clean up dedup entry for this handle ID.
            if let Ok(mut dedup) = self.dedup.lock() {
                dedup.retain(|_, &mut vid| vid != id);
            }
        }
        removed
    }

    /// Get the number of active handles (for diagnostics).
    pub fn len(&self) -> usize {
        let handles = self
            .handles
            .lock()
            .expect("SHARED_TABLE handles lock poisoned");
        handles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for SharedHandleTable {
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    /// Per-thread shared handle table — tracks reference-counted values across FFI calls.
    ///
    /// Using a thread-local table isolates shared-handle state per Mimi
    /// invocation/test thread. Handles created on one thread are only visible to
    /// C code invoked on that same thread.
    static SHARED_TABLE: SharedHandleTable = SharedHandleTable::new();
}

/// Execute a closure with the current thread's shared handle table.
pub fn with_shared_table<R, F: FnOnce(&SharedHandleTable) -> R>(f: F) -> R {
    SHARED_TABLE.with(f)
}

/// Convenience wrappers for the per-thread shared handle table.
pub fn shared_table_create(inner: Arc<RwLock<Value>>) -> i64 {
    SHARED_TABLE.with(|table| table.create(inner))
}

/// Create or reuse a shared handle with cross-call deduplication.
/// `ptr` should be `Arc::as_ptr(&arc) as *const ()`.
pub fn shared_table_create_dedup(inner: Arc<RwLock<Value>>, ptr: *const ()) -> i64 {
    SHARED_TABLE.with(|table| table.create_dedup(inner, ptr))
}

pub fn shared_table_get(id: i64) -> Option<Arc<SharedHandle>> {
    SHARED_TABLE.with(|table| table.get(id))
}

pub fn shared_table_retain(id: i64) -> bool {
    SHARED_TABLE.with(|table| table.retain(id))
}

pub fn shared_table_release(id: i64) -> bool {
    SHARED_TABLE.with(|table| table.release(id))
}

/// Convenience wrappers for the per-thread capability table.
pub fn cap_table_register(name: &str) -> i64 {
    CAP_TABLE.with(|table| table.register(name))
}

pub fn cap_table_check(id: i64, name: &str) -> bool {
    CAP_TABLE.with(|table| table.check(id, name))
}

pub fn cap_table_consume(id: i64, name: &str) -> bool {
    CAP_TABLE.with(|table| table.consume(id, name))
}

// ---------------------------------------------------------------------------
// C ABI functions (callable from generated code and interpreter)
// ---------------------------------------------------------------------------

/// Retain a shared handle.  Returns the handle ID (same as input).
#[no_mangle]
pub extern "C" fn mimi_shared_retain(handle: i64) -> i64 {
    SHARED_TABLE.with(|table| {
        table.retain(handle);
    });
    handle
}

/// Release a shared handle.
#[no_mangle]
pub extern "C" fn mimi_shared_release(handle: i64) {
    SHARED_TABLE.with(|table| {
        table.release(handle);
    });
}

/// Read the inner value of a shared handle and return a heap-allocated copy.
/// The caller takes ownership of the returned Value* and must free it with
/// `mimi_value_free` when done.
/// Returns null if the handle is invalid.
#[no_mangle]
pub extern "C" fn mimi_shared_get_ptr(handle: i64) -> *const Value {
    let h = match SHARED_TABLE.with(|table| table.get(handle)) {
        Some(h) => h,
        None => return std::ptr::null(),
    };
    // Return a heap copy so the pointer is valid regardless of handle lifetime.
    h.with_value(|v| {
        let boxed = Box::new(v.clone());
        Box::into_raw(boxed) as *const Value
    })
}

/// Free a Value that was obtained via `mimi_shared_get_ptr` or any other
/// `mimi_value_new_*` constructor.
/// This must be called for every non-null pointer returned by those functions.
#[no_mangle]
pub extern "C" fn mimi_value_free(ptr: *const Value) {
    if !ptr.is_null() {
        // SAFETY: ptr is a non-null pointer to a heap-allocated Value, previously obtained via Box::into_raw.
        unsafe {
            drop(Box::from_raw(ptr as *mut Value));
        }
    }
}

/// Create a new integer Value. The caller takes ownership of the returned pointer
/// and must free it with `mimi_value_free`.
#[no_mangle]
pub extern "C" fn mimi_value_new_int(n: i64) -> *mut Value {
    let boxed = Box::new(Value::Int(n));
    Box::into_raw(boxed)
}

/// Create a new boolean Value. The caller takes ownership of the returned pointer
/// and must free it with `mimi_value_free`.
#[no_mangle]
pub extern "C" fn mimi_value_new_bool(b: bool) -> *mut Value {
    let boxed = Box::new(Value::Bool(b));
    Box::into_raw(boxed)
}

/// Create a new floating-point Value. The caller takes ownership of the returned
/// pointer and must free it with `mimi_value_free`.
#[no_mangle]
pub extern "C" fn mimi_value_new_float(f: f64) -> *mut Value {
    let boxed = Box::new(Value::Float(f));
    Box::into_raw(boxed)
}

/// Read an integer from a Value pointer. Returns 0 if the pointer is null or
/// does not contain an integer.
///
/// # Safety
/// `ptr` must be null or a valid pointer to a heap-allocated `Value` obtained
/// via `mimi_value_new_*` or `mimi_shared_get_ptr`.
#[no_mangle]
pub unsafe extern "C" fn mimi_value_as_int(ptr: *const Value) -> i64 {
    if ptr.is_null() {
        return 0;
    }
    // SAFETY: ptr was null-checked above.
    unsafe {
        match &*ptr {
            Value::Int(n) => *n,
            Value::Bool(b) => *b as i64,
            Value::Float(f) => *f as i64,
            _ => 0,
        }
    }
}

/// Read a boolean from a Value pointer. Returns false if the pointer is null or
/// does not contain a boolean.
///
/// # Safety
/// `ptr` must be null or a valid pointer to a heap-allocated `Value` obtained
/// via `mimi_value_new_*` or `mimi_shared_get_ptr`.
#[no_mangle]
pub unsafe extern "C" fn mimi_value_as_bool(ptr: *const Value) -> bool {
    if ptr.is_null() {
        return false;
    }
    // SAFETY: ptr was null-checked above.
    unsafe {
        match &*ptr {
            Value::Bool(b) => *b,
            Value::Int(n) => *n != 0,
            Value::Float(f) => *f != 0.0,
            _ => false,
        }
    }
}

/// Read a floating-point number from a Value pointer. Returns 0.0 if the pointer
/// is null or does not contain a float.
///
/// # Safety
/// `ptr` must be null or a valid pointer to a heap-allocated `Value` obtained
/// via `mimi_value_new_*` or `mimi_shared_get_ptr`.
#[no_mangle]
pub unsafe extern "C" fn mimi_value_as_float(ptr: *const Value) -> f64 {
    if ptr.is_null() {
        return 0.0;
    }
    // SAFETY: ptr was null-checked above.
    unsafe {
        match &*ptr {
            Value::Float(f) => *f,
            Value::Int(n) => *n as f64,
            _ => 0.0,
        }
    }
}

/// Create a shared handle from a heap-allocated Value. The Value ownership is
/// transferred to the shared handle; the caller must NOT call `mimi_value_free`
/// on the same pointer after this call. Returns the opaque handle ID, or 0 on
/// error (in which case the Value is dropped).
///
/// # Safety
/// `value_ptr` must be null or a valid pointer to a heap-allocated `Value`
/// obtained via `mimi_value_new_*` or `mimi_string_from_raw`.
#[no_mangle]
pub unsafe extern "C" fn mimi_shared_create(value_ptr: *mut Value) -> i64 {
    if value_ptr.is_null() {
        return 0;
    }
    // SAFETY: value_ptr is non-null and points to a heap-allocated Value obtained
    // via Box::into_raw. We re-take ownership here.
    let value = unsafe { Box::from_raw(value_ptr) };
    let arc = Arc::new(RwLock::new(*value));
    shared_table_create(arc)
}

/// Free a raw string that was obtained via `string.into_raw()`.
///
/// # Safety
/// `c_str` must be a non-null pointer previously returned by `mimi_string_into_raw` or a valid
/// `CString::into_raw()` result. After this call, the pointer is invalidated.
#[no_mangle]
pub unsafe extern "C" fn mimi_string_free_raw(c_str: *mut std::ffi::c_char) {
    if !c_str.is_null() {
        // SAFETY: c_str is a non-null pointer to a CString previously created via CString::into_raw (null-checked above).
        unsafe {
            drop(std::ffi::CString::from_raw(c_str));
        }
    }
}

/// Get a C string pointer from a Mimi string value.
/// The caller must NOT free the returned pointer — use `mimi_string_as_c_str_free`
/// to release it when done. Each call allocates a new CString that the caller
/// must eventually free. Returns null if the pointer is invalid or not a string.
///
/// # Safety
/// `mimi_string` must be either null or a valid pointer to a heap-allocated `Value` previously
/// obtained via `Box::into_raw` or `mimi_value_*` functions.
#[no_mangle]
pub unsafe extern "C" fn mimi_string_as_c_str(
    mimi_string: *const Value,
) -> *const std::ffi::c_char {
    if mimi_string.is_null() {
        return std::ptr::null();
    }
    // SAFETY: mimi_string is a non-null pointer to a valid heap-allocated Value (null-checked above).
    unsafe {
        match &*mimi_string {
            Value::String(s) => {
                match std::ffi::CString::new(s.as_str()) {
                    Ok(c_str) => {
                        // Register for cleanup first, then take the pointer from the
                        // stored value. This avoids returning a pointer that is
                        // immediately invalidated by moving the CString into the
                        // thread-local vector (Stacked Borrows violation under Miri).
                        // SAFETY: the CString is heap-allocated and its address is
                        // stable while it remains in PENDING_C_STRINGS; callers must
                        // not hold the pointer across further calls to this function.
                        PENDING_C_STRINGS.with(|pending| {
                            let mut pending = pending.borrow_mut();
                            pending.push(c_str);
                            pending.last().unwrap().as_ptr()
                        })
                    }
                    Err(_) => {
                        // Interior null bytes: can't represent as C string.
                        // Return null to signal the error (caller can free null safely).
                        eprintln!("mimi_string_as_c_str: string contains interior null bytes, returning null");
                        std::ptr::null()
                    }
                }
            }
            _ => std::ptr::null(),
        }
    }
}

/// Free the C string pointer obtained from `mimi_string_as_c_str`.
/// Call this when the C string is no longer needed.
#[no_mangle]
pub extern "C" fn mimi_string_as_c_str_free(c_str: *const std::ffi::c_char) {
    if c_str.is_null() {
        return;
    }
    PENDING_C_STRINGS.with(|pending| {
        pending.borrow_mut().retain(|cs| cs.as_ptr() != c_str);
    });
}

thread_local! {
    /// Per-thread registry of CStrings allocated by `mimi_string_as_c_str`.
    /// Each call to `mimi_string_as_c_str` pushes a new `CString` here to
    /// keep the pointer alive.  Callers MUST call `mimi_string_as_c_str_free`
    /// to release the entry.  When the thread exits the thread-local Vec is
    /// dropped and any remaining CStrings are freed automatically.
    static PENDING_C_STRINGS: RefCell<Vec<std::ffi::CString>> = const { RefCell::new(Vec::new()) };
}

/// Return the byte length of a Mimi string value.
/// Returns -1 if the pointer is null or does not point to a string.
///
/// # Safety
/// `mimi_string` must be either null or a valid pointer to a heap-allocated
/// `Value` previously obtained via `Box::into_raw` or `mimi_value_*` functions.
#[no_mangle]
pub unsafe extern "C" fn mimi_string_len(mimi_string: *const Value) -> i64 {
    if mimi_string.is_null() {
        return -1;
    }
    // SAFETY: null-checked above.
    unsafe {
        match &*mimi_string {
            Value::String(s) => s.len() as i64,
            _ => -1,
        }
    }
}

/// Free all pending C strings allocated by `mimi_string_as_c_str` on the
/// current thread. This is a convenience for bulk cleanup; individual pointers
/// previously returned by `mimi_string_as_c_str` become invalid after this call.
#[no_mangle]
pub extern "C" fn mimi_string_as_c_str_free_all() {
    PENDING_C_STRINGS.with(|pending| {
        pending.borrow_mut().clear();
    });
}

/// Convert a Mimi string to a raw C string (transfer ownership to C).
/// The caller is responsible for calling `mimi_string_free_raw` on the result.
/// Returns null if the pointer is invalid or the string contains interior null bytes.
/// On success, the original Mimi string is cleared (ownership transferred).
/// On failure (null return), the original Mimi string is NOT modified.
///
/// # Safety
/// `mimi_string` must be either null or a valid, exclusive pointer to a heap-allocated `Value`
/// obtained via `Box::into_raw`. The caller transfers ownership of the string content.
#[no_mangle]
pub unsafe extern "C" fn mimi_string_into_raw(mimi_string: *mut Value) -> *mut std::ffi::c_char {
    if mimi_string.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `mimi_string` was null-checked above. The caller has transferred ownership of this
    // pointer to us, so a single mutable dereference to read the owned Value is valid.
    unsafe {
        match &mut *mimi_string {
            Value::String(s) => {
                match std::ffi::CString::new(s.as_str()) {
                    Ok(c_str) => {
                        let ptr = c_str.into_raw();
                        s.clear();
                        ptr
                    }
                    Err(_) => {
                        // Interior null bytes: don't modify the original string,
                        // return null to signal the error.
                        eprintln!("mimi_string_into_raw: string contains interior null bytes, returning null");
                        std::ptr::null_mut()
                    }
                }
            }
            _ => std::ptr::null_mut(),
        }
    }
}

/// Convert a raw C string back to a Mimi string.
/// The caller should NOT free the original C string after this call.
/// Returns a new Mimi Value (caller takes ownership).
/// Note: This function allocates a new Value on the heap.
///
/// # Safety
/// `c_str` must be a non-null pointer previously obtained via `CString::into_raw()`. Ownership
/// of the C string is transferred to this function; the caller must not use the pointer afterward.
#[no_mangle]
pub unsafe extern "C" fn mimi_string_from_raw(c_str: *mut std::ffi::c_char) -> *mut Value {
    if c_str.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `c_str` was null-checked above and must have been produced by `CString::into_raw`.
    // Reconstructing the CString here takes ownership back from the caller; the caller must not use
    // the pointer afterwards.
    unsafe {
        let c_str = std::ffi::CString::from_raw(c_str);
        let s = c_str.to_string_lossy().into_owned();
        let value = Box::new(Value::String(s));
        Box::into_raw(value)
    }
}

// ---------------------------------------------------------------------------
// Thread Pool (for codegen parasteps)
// ---------------------------------------------------------------------------

use std::sync::mpsc as std_mpsc;
use std::thread;

/// Global thread pool for generated code.
pub struct MimiThreadPool {
    workers: Vec<thread::JoinHandle<()>>,
    sender: Option<std_mpsc::Sender<RawTask>>,
    /// Tracks pending task count for `join_all`.
    pending: Arc<Mutex<i32>>,
    /// Signaled when `pending` reaches zero.
    completion: Arc<std::sync::Condvar>,
}

struct RawTask {
    func: extern "C" fn(*mut u8) -> *mut u8,
    arg: *mut u8,
    /// Shared pending counter — decremented after task completes.
    pending: Arc<Mutex<i32>>,
    completion: Arc<std::sync::Condvar>,
}
// SAFETY: RawTask only carries a function pointer and a raw pointer, both of which are Send-safe.
unsafe impl Send for RawTask {}

impl MimiThreadPool {
    pub fn new(size: usize) -> Self {
        let (sender, receiver) = std_mpsc::channel::<RawTask>();
        let receiver = std::sync::Arc::new(std::sync::Mutex::new(receiver));
        let pending = Arc::new(Mutex::new(0i32));
        let completion = Arc::new(std::sync::Condvar::new());
        let mut workers = Vec::with_capacity(size);

        for _ in 0..size {
            let receiver = std::sync::Arc::clone(&receiver);
            let worker = thread::spawn(move || loop {
                let task = receiver
                    .lock()
                    .expect("MIMI_POOL receiver lock poisoned")
                    .recv();
                match task {
                    Ok(task) => {
                        let _ = (task.func)(task.arg);
                        // Decrement pending count and notify waiters
                        let mut count = task
                            .pending
                            .lock()
                            .expect("MIMI_POOL pending counter lock poisoned");
                        *count -= 1;
                        if *count == 0 {
                            task.completion.notify_all();
                        }
                    }
                    Err(_) => break,
                }
            });
            workers.push(worker);
        }

        MimiThreadPool {
            workers,
            sender: Some(sender),
            pending,
            completion,
        }
    }

    pub fn submit_raw(&self, func: extern "C" fn(*mut u8) -> *mut u8, arg: *mut u8) {
        let mut count = self
            .pending
            .lock()
            .expect("MIMI_POOL pending counter lock poisoned");
        *count += 1;
        if let Some(ref sender) = self.sender {
            if let Err(e) = sender.send(RawTask {
                func,
                arg,
                pending: Arc::clone(&self.pending),
                completion: Arc::clone(&self.completion),
            }) {
                eprintln!("[mimi ffi] submit_raw: failed to send task: {}", e);
            }
        }
    }

    pub fn submit<F: FnOnce() + Send + 'static>(&self, job: F) {
        struct ClosureData {
            func: extern "C" fn(*mut u8) -> *mut u8,
            arg: *mut u8,
        }
        // FFI-8: Soundness — ClosureData is Send because:
        // - func: extern "C" fn pointer is Send (code pointers have no thread affinity)
        // - arg: *mut u8 is Send — it points to heap-allocated Box (system allocator),
        //   and ownership is transferred to the receiving thread via the task queue.
        //   The arg is only dereferenced AFTER the task is dequeued and the trampoline
        //   runs on the worker thread, which has exclusive ownership at that point.
        unsafe impl Send for ClosureData {}

        extern "C" fn closure_trampoline(data_ptr: *mut u8) -> *mut u8 {
            // SAFETY: data_ptr was created by Box::into_raw and is guaranteed to be a valid heap-allocated Box<dyn FnOnce() + Send>.
            let data = unsafe { Box::from_raw(data_ptr as *mut Box<dyn FnOnce() + Send>) };
            (*data)();
            // Return null — the task result is not used in the submit() path.
            std::ptr::null_mut()
        }

        let boxed: Box<dyn FnOnce() + Send> = Box::new(job);
        let data = Box::new(ClosureData {
            func: closure_trampoline,
            arg: Box::into_raw(boxed) as *mut u8,
        });
        let data = Box::into_raw(data);
        extern "C" fn data_trampoline(data_ptr: *mut u8) -> *mut u8 {
            // SAFETY: data_ptr was created by Box::into_raw and is guaranteed to be a valid heap-allocated ClosureData.
            let data = unsafe { Box::from_raw(data_ptr as *mut ClosureData) };
            // FFI-3: Call the function, store result, drop data, return result.
            let ret = (data.func)(data.arg);
            drop(data);
            ret
        }
        let mut count = self
            .pending
            .lock()
            .expect("MIMI_POOL pending counter lock poisoned");
        *count += 1;
        if let Some(ref sender) = self.sender {
            if let Err(e) = sender.send(RawTask {
                func: data_trampoline,
                arg: data as *mut u8,
                pending: Arc::clone(&self.pending),
                completion: Arc::clone(&self.completion),
            }) {
                eprintln!("[mimi ffi] submit: failed to send task: {}", e);
            }
        }
    }

    /// Wait until all submitted tasks have completed.
    pub fn join_all(&self) {
        let mut count = self
            .pending
            .lock()
            .expect("MIMI_POOL pending counter lock poisoned");
        while *count > 0 {
            count = self
                .completion
                .wait(count)
                .expect("MIMI_POOL completion condvar lock poisoned");
        }
    }
}

impl Drop for MimiThreadPool {
    fn drop(&mut self) {
        // FFI-BUG-4 fix: Drop the sender first to close the channel, signaling
        // workers to exit. Then let JoinHandles drop (detaching threads) instead
        // of joining to avoid blocking indefinitely if a worker is stuck
        // (e.g., in an uninterruptible syscall). Dropping detached handles is safe
        // because:
        // 1. The pool is a process-global that lives until process exit.
        // 2. Workers are daemons — they don't need to finish for other cleanup.
        // 3. OS will reclaim thread resources when the process exits.
        // 4. If a worker panicked, the panic is silently ignored (detached threads).
        drop(self.sender.take());
        for _ in self.workers.drain(..) {
            // Dropping JoinHandle detaches the thread — it will continue until
            // the channel is closed (sender dropped above), then exit.
        }
    }
}

#[allow(clippy::incompatible_msrv)]
static MIMI_POOL: LazyLock<MimiThreadPool> = LazyLock::new(|| {
    let size = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    MimiThreadPool::new(size)
});

/// Submit a function pointer to the thread pool.
/// `fn_ptr` is a C function pointer: `extern "C" fn(*mut u8) -> *mut u8`
/// `arg` is passed as the argument to fn_ptr.
///
/// # Safety
/// The caller must ensure that:
/// - `fn_ptr` is a valid function pointer with signature `extern "C" fn(*mut u8) -> *mut u8`
/// - `arg` is valid for the duration of the task
/// - The function pointed to by `fn_ptr` is safe to call from another thread
#[no_mangle]
pub unsafe extern "C" fn mimi_pool_submit(fn_ptr: extern "C" fn(*mut u8) -> *mut u8, arg: *mut u8) {
    MIMI_POOL.submit_raw(fn_ptr, arg);
}

/// Block until all submitted pool tasks complete.
/// Uses a pending task counter and Condvar to properly synchronize.
#[no_mangle]
pub extern "C" fn mimi_pool_join_all() {
    MIMI_POOL.join_all();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{mimi_cap_check, mimi_cap_consume, mimi_cap_register};

    /// Regression test for FFI-BUG-1: create_dedup must remove stale dedup
    /// entries when retain() returns false (handle was cleaned up from under
    /// us). Otherwise the next create_dedup with the same Arc pointer returns
    /// the released handle ID, causing the C caller to use a dangling handle.
    #[test]
    fn test_create_dedup_removes_stale_entry_on_retain_failure() {
        // Given: a fresh handle table and an Arc allocation
        let table = SharedHandleTable::new();
        let value = Arc::new(RwLock::new(Value::Int(42)));
        let ptr = Arc::as_ptr(&value) as *const ();

        // When: we create a dedup handle
        let id1 = table.create_dedup(Arc::clone(&value), ptr);
        assert_eq!(table.get(id1).map(|h| h.strong_count()), Some(1));

        // And: we release it to drop refcount to zero (simulating C-side cleanup)
        table.retain(id1); // bump to 2
        table.release(id1); // back to 1
        table.release(id1); // to 0 → SharedHandle is dropped here

        // Then: the next create_dedup for the same pointer must NOT reuse
        // the stale id1 — it must create a new handle (id2).
        // Before the fix, id1 would be returned (dedup entry was not cleaned),
        // causing C code to use a dangling handle.
        let id2 = table.create_dedup(Arc::clone(&value), ptr);
        assert_ne!(
            id1, id2,
            "create_dedup returned the same id after handle was released; \
             FFI-BUG-1: stale dedup entry was not removed"
        );
        assert_eq!(table.get(id2).map(|h| h.strong_count()), Some(1));
        // id1 should no longer be valid
        assert!(
            table.get(id1).is_none(),
            "released handle id1 should not be retrievable"
        );
    }

    /// Regression test for FFI-BUG-1: verify dedup entry is removed so that
    /// distinct Arc allocations (same content, different addresses) are NOT
    /// conflated — this is FFI-DESIGN-4 (identity-based dedup, not content-based).
    #[test]
    fn test_create_dedup_is_by_identity_not_content() {
        let table = SharedHandleTable::new();

        // Two equal Mimi values in distinct Arc allocations
        let arc1 = Arc::new(RwLock::new(Value::Int(42)));
        let arc2 = Arc::new(RwLock::new(Value::Int(42)));
        let ptr1 = Arc::as_ptr(&arc1) as *const ();
        let ptr2 = Arc::as_ptr(&arc2) as *const ();

        let id1 = table.create_dedup(Arc::clone(&arc1), ptr1);
        let id2 = table.create_dedup(Arc::clone(&arc2), ptr2);

        // Distinct Arc allocations → distinct handle IDs
        assert_ne!(
            id1, id2,
            "two equal values in distinct Arc allocations should get distinct IDs; \
             dedup is by Arc identity (pointer), not Mimi content equality"
        );
    }

    /// FFI-BUG-4: MimiThreadPool::drop must not block on detached/dropped JoinHandles.
    /// Creating and immediately dropping a pool (no tasks submitted) is a minimal
    /// smoke test that the drop path does not hang. The real regression test is
    /// that the Drop impl no longer calls join() — verified by code inspection.
    #[test]
    fn test_thread_pool_drop_does_not_hang() {
        use std::time::{Duration, Instant};
        let pool = MimiThreadPool::new(1);
        let start = Instant::now();
        drop(pool);
        // Should return almost immediately, not block on join()
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "drop(pool) took >1s — FFI-BUG-4: Drop impl may still be calling join()"
        );
    }

    // ─── Capability C API tests ─────────────────────────────────────────────

    #[test]
    fn cap_c_api_lifecycle() {
        let name = std::ffi::CString::new("read").unwrap();
        let id = mimi_cap_register(name.as_ptr());
        assert!(id > 0);

        // Check succeeds before consume, fails for wrong name.
        assert!(mimi_cap_check(id, name.as_ptr()));
        let wrong = std::ffi::CString::new("write").unwrap();
        assert!(!mimi_cap_check(id, wrong.as_ptr()));

        // Consume succeeds once and only with the correct name.
        assert!(mimi_cap_consume(id, name.as_ptr()));
        assert!(!mimi_cap_consume(id, name.as_ptr()));
        assert!(!mimi_cap_check(id, name.as_ptr()));
    }

    #[test]
    fn cap_c_api_invalid_id() {
        let name = std::ffi::CString::new("read").unwrap();
        assert!(!mimi_cap_check(9999, name.as_ptr()));
        assert!(!mimi_cap_consume(9999, name.as_ptr()));
    }

    // ─── Shared handle C API tests ──────────────────────────────────────────

    #[test]
    fn shared_c_api_retain_release_get_ptr() {
        let value = Arc::new(RwLock::new(Value::Int(42)));
        let id = with_shared_table(|table| table.create(Arc::clone(&value)));
        assert!(id > 0);

        // Retain bumps the C-side reference count.
        assert_eq!(mimi_shared_retain(id), id);

        // Get a heap copy of the inner value.
        let ptr = mimi_shared_get_ptr(id);
        assert!(!ptr.is_null());
        unsafe {
            assert!(matches!(&*ptr, Value::Int(42)));
        }
        mimi_value_free(ptr);

        // Release twice to balance the initial + retain references.
        mimi_shared_release(id);
        mimi_shared_release(id);

        // Handle is no longer valid.
        assert!(mimi_shared_get_ptr(id).is_null());
    }

    #[test]
    fn shared_c_api_invalid_handle() {
        assert!(mimi_shared_get_ptr(9999).is_null());
        // Release on invalid handle should be a no-op and not panic.
        mimi_shared_release(9999);
    }

    // ─── String C API tests ─────────────────────────────────────────────────

    #[test]
    fn string_c_api_len_and_borrow() {
        let value = Box::new(Value::String("hello".to_string()));
        let raw = Box::into_raw(value);

        assert_eq!(unsafe { mimi_string_len(raw) }, 5);

        let c_str = unsafe { mimi_string_as_c_str(raw) };
        assert!(!c_str.is_null());
        unsafe {
            assert_eq!(std::ffi::CStr::from_ptr(c_str).to_str().unwrap(), "hello");
        }

        mimi_string_as_c_str_free(c_str);
        mimi_value_free(raw);
    }

    #[test]
    fn string_c_api_free_all_clears_pending() {
        let value = Box::new(Value::String("bulk".to_string()));
        let raw = Box::into_raw(value);

        let c_str = unsafe { mimi_string_as_c_str(raw) };
        assert!(!c_str.is_null());

        // Free all pending strings; the individual pointer becomes invalid.
        mimi_string_as_c_str_free_all();

        mimi_value_free(raw);
    }

    #[test]
    fn string_c_api_null_inputs() {
        assert_eq!(unsafe { mimi_string_len(std::ptr::null()) }, -1);
        assert!(unsafe { mimi_string_as_c_str(std::ptr::null()) }.is_null());
        // Free on null / unknown pointer should be a no-op.
        mimi_string_as_c_str_free(std::ptr::null());
    }

    // ─── Value C API tests ──────────────────────────────────────────────────

    #[test]
    fn value_c_api_constructors_and_accessors() {
        let int_val = mimi_value_new_int(42);
        assert_eq!(unsafe { mimi_value_as_int(int_val) }, 42);
        assert!(unsafe { mimi_value_as_bool(int_val) }); // non-zero int is truthy
        mimi_value_free(int_val);

        let bool_val = mimi_value_new_bool(true);
        assert!(unsafe { mimi_value_as_bool(bool_val) });
        assert_eq!(unsafe { mimi_value_as_int(bool_val) }, 1);
        mimi_value_free(bool_val);

        let float_val = mimi_value_new_float(2.5);
        assert!((unsafe { mimi_value_as_float(float_val) } - 2.5).abs() < 0.001);
        assert_eq!(unsafe { mimi_value_as_int(float_val) }, 2);
        mimi_value_free(float_val);

        // Null inputs are safe.
        assert_eq!(unsafe { mimi_value_as_int(std::ptr::null()) }, 0);
        assert!(!unsafe { mimi_value_as_bool(std::ptr::null()) });
        assert_eq!(unsafe { mimi_value_as_float(std::ptr::null()) }, 0.0);
        mimi_value_free(std::ptr::null());
    }

    #[test]
    fn shared_c_api_create_from_value() {
        let value = mimi_value_new_int(123);
        let id = unsafe { mimi_shared_create(value) };
        assert!(id > 0);

        let ptr = mimi_shared_get_ptr(id);
        assert!(!ptr.is_null());
        assert_eq!(unsafe { mimi_value_as_int(ptr) }, 123);
        mimi_value_free(ptr);

        mimi_shared_release(id);
        assert!(mimi_shared_get_ptr(id).is_null());
    }

    #[test]
    fn shared_c_api_create_null_is_safe() {
        assert_eq!(unsafe { mimi_shared_create(std::ptr::null_mut()) }, 0);
    }
}
