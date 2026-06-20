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
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.insert(id, CapEntry {
            name: name.to_string(),
            consumed: false,
        });
        id
    }

    /// Check whether the cap with the given ID exists, matches the name, and
    /// has not been consumed.  Does NOT consume the cap.
    pub fn check(&self, id: i64, name: &str) -> bool {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        match entries.get(&id) {
            Some(entry) => !entry.consumed && entry.name == name,
            None => false,
        }
    }

    /// Check and consume the cap.  Returns `true` if the cap existed, matched
    /// the name, and was not already consumed.  After this call the cap is
    /// marked as consumed and cannot be used again.
    pub fn consume(&self, id: i64, name: &str) -> bool {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
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
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
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
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        f(&*guard)
    }

    /// Execute a closure with a mutable reference to the inner value.
    /// Safe, scoped access — prefer this over raw pointer APIs.
    pub fn with_value_mut<R>(&self, f: impl FnOnce(&mut Value) -> R) -> R {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        f(&mut *guard)
    }

    /// Get a read guard for the inner value.
    pub fn borrow(&self) -> RwLockReadGuard<'_, Value> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Get a write guard for the inner value.
    pub fn borrow_mut(&self) -> RwLockWriteGuard<'_, Value> {
        self.inner.write().unwrap_or_else(|e| e.into_inner())
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

    /// Get the opaque handle ID.
    pub fn id(&self) -> i64 {
        self.id
    }
}

impl Drop for SharedHandle {
    fn drop(&mut self) {
        let _ = SHARED_TABLE.remove(self.id);
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
        let handle = Arc::new(SharedHandle::new(id, inner));
        let mut handles = self.handles.lock().unwrap_or_else(|e| e.into_inner());
        handles.insert(id, handle);
        id
    }

    /// Get a reference to the handle by ID.
    pub fn get(&self, id: i64) -> Option<Arc<SharedHandle>> {
        let handles = self.handles.lock().unwrap_or_else(|e| e.into_inner());
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
            let handles = self.handles.lock().unwrap_or_else(|e| e.into_inner());
            handles.get(&id).cloned()
        };
        if let Some(handle) = handle {
            if handle.release() {
                let mut handles = self.handles.lock().unwrap_or_else(|e| e.into_inner());
                handles.remove(&id);
                return true;
            }
        }
        false
    }

    /// Remove a handle from the table unconditionally (cleanup).
    pub fn remove(&self, id: i64) -> bool {
        let mut handles = self.handles.lock().unwrap_or_else(|e| e.into_inner());
        handles.remove(&id).is_some()
    }

    /// Get the number of active handles (for diagnostics).
    pub fn len(&self) -> usize {
        let handles = self.handles.lock().unwrap_or_else(|e| e.into_inner());
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

/// Read the inner value of a shared handle and return a heap-allocated copy.
/// The caller takes ownership of the returned Value* and must free it with
/// `mimi_value_free` when done.
/// Returns null if the handle is invalid.
#[no_mangle]
pub extern "C" fn mimi_shared_get_ptr(handle: i64) -> *const Value {
    let h = match SHARED_TABLE.get(handle) {
        Some(h) => h,
        None => return std::ptr::null(),
    };
    // Return a heap copy so the pointer is valid regardless of handle lifetime.
    h.with_value(|v| {
        let boxed = Box::new(v.clone());
        Box::into_raw(boxed) as *const Value
    })
}

/// Free a Value that was obtained via `mimi_shared_get_ptr`.
/// This must be called for every non-null pointer returned by `mimi_shared_get_ptr`.
#[no_mangle]
pub extern "C" fn mimi_value_free(ptr: *mut Value) {
    if !ptr.is_null() {
        // SAFETY: ptr is a non-null pointer to a heap-allocated Value, previously obtained via Box::into_raw.
        unsafe { drop(Box::from_raw(ptr)); }
    }
}

/// Check whether a capability is valid and matches the expected name.
#[no_mangle]
pub extern "C" fn mimi_cap_check(cap: i64, name: *const std::ffi::c_char) -> bool {
    if name.is_null() {
        return false;
    }
    // SAFETY: name is a non-null pointer to a null-terminated C string (null-checked above).
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
    // SAFETY: name is a non-null pointer to a null-terminated C string (null-checked above).
    let name_str = unsafe { std::ffi::CStr::from_ptr(name) }
        .to_str()
        .unwrap_or("");
    CAP_TABLE.consume(cap, name_str)
}

/// Free a raw string that was obtained via `string.into_raw()`.
#[no_mangle]
pub extern "C" fn mimi_string_free_raw(c_str: *mut std::ffi::c_char) {
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
#[no_mangle]
pub extern "C" fn mimi_string_as_c_str(mimi_string: *const Value) -> *const std::ffi::c_char {
    if mimi_string.is_null() {
        return std::ptr::null();
    }
    // SAFETY: mimi_string is a non-null pointer to a valid heap-allocated Value (null-checked above).
    unsafe {
        match &*mimi_string {
            Value::String(s) => {
                let c_str = std::ffi::CString::new(s.as_str()).unwrap_or_default();
                let ptr = c_str.as_ptr();
                // Register for cleanup — caller must call mimi_string_as_c_str_free
                PENDING_C_STRINGS.lock().unwrap_or_else(|e| e.into_inner()).push(c_str);
                ptr
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
    let mut pending = PENDING_C_STRINGS.lock().unwrap_or_else(|e| e.into_inner());
    pending.retain(|cs| cs.as_ptr() != c_str);
}

/// Global registry of CStrings allocated by `mimi_string_as_c_str`.
/// Each call to `mimi_string_as_c_str` pushes a new `CString` here to
/// keep the pointer alive.  Callers MUST call `mimi_string_as_c_str_free`
/// to release the entry.  Entries that are never freed will leak until
/// process exit (OS reclaims the memory).
static PENDING_C_STRINGS: std::sync::LazyLock<Mutex<Vec<std::ffi::CString>>> =
    std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

/// Convert a Mimi string to a raw C string (transfer ownership to C).
/// The caller is responsible for calling `mimi_string_free_raw` on the result.
/// Returns null if the pointer is invalid or the string contains interior null bytes.
/// On success, the original Mimi string is cleared (ownership transferred).
/// On failure (null return), the original Mimi string is NOT modified.
#[no_mangle]
pub extern "C" fn mimi_string_into_raw(mimi_string: *mut Value) -> *mut std::ffi::c_char {
    if mimi_string.is_null() {
        return std::ptr::null_mut();
    }
    // Safety: mimi_string is a non-null pointer to a valid heap-allocated Value (null-checked above); the mutable dereference is safe because this function takes ownership from the caller.
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
#[no_mangle]
pub extern "C" fn mimi_string_from_raw(c_str: *mut std::ffi::c_char) -> *mut Value {
    if c_str.is_null() {
        return std::ptr::null_mut();
    }
    // Safety: c_str is a non-null pointer to a CString previously created via CString::into_raw (null-checked above); Box::into_raw transfers ownership to the C caller.
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
    sender: std_mpsc::Sender<RawTask>,
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
                let task = receiver.lock().unwrap_or_else(|e| e.into_inner()).recv();
                match task {
                    Ok(task) => {
                        let _ = (task.func)(task.arg);
                        // Decrement pending count and notify waiters
                        let mut count = task.pending.lock().unwrap_or_else(|e| e.into_inner());
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

        MimiThreadPool { workers, sender, pending, completion }
    }

    pub fn submit_raw(&self, func: extern "C" fn(*mut u8) -> *mut u8, arg: *mut u8) {
        let mut count = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        *count += 1;
        let _ = self.sender.send(RawTask {
            func,
            arg,
            pending: Arc::clone(&self.pending),
            completion: Arc::clone(&self.completion),
        });
    }

    pub fn submit<F: FnOnce() + Send + 'static>(&self, job: F) {
        struct ClosureData {
            func: extern "C" fn(*mut u8),
            arg: *mut u8,
        }
        // SAFETY: ClosureData holds only function pointers and raw pointers, both Send-safe.
        unsafe impl Send for ClosureData {}

        extern "C" fn closure_trampoline(data_ptr: *mut u8) {
            // SAFETY: data_ptr was created by Box::into_raw and is guaranteed to be a valid heap-allocated Box<dyn FnOnce() + Send>.
            let data = unsafe { Box::from_raw(data_ptr as *mut Box<dyn FnOnce() + Send>) };
            (*data)();
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
            (data.func)(data.arg);
            std::ptr::null_mut()
        }
        let mut count = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        *count += 1;
        let _ = self.sender.send(RawTask {
            func: data_trampoline,
            arg: data as *mut u8,
            pending: Arc::clone(&self.pending),
            completion: Arc::clone(&self.completion),
        });
    }

    /// Wait until all submitted tasks have completed.
    pub fn join_all(&self) {
        let mut count = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        while *count > 0 {
            count = self.completion.wait(count).unwrap_or_else(|e| e.into_inner());
        }
    }
}

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
pub unsafe extern "C" fn mimi_pool_submit(
    fn_ptr: extern "C" fn(*mut u8) -> *mut u8,
    arg: *mut u8,
) {
    MIMI_POOL.submit_raw(fn_ptr, arg);
}

/// Block until all submitted pool tasks complete.
/// Uses a pending task counter and Condvar to properly synchronize.
#[no_mangle]
pub extern "C" fn mimi_pool_join_all() {
    MIMI_POOL.join_all();
}
