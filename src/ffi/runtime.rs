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

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::cell::RefCell;
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
        let mut entries = self.entries.lock()
            .expect("CAP_TABLE entries lock poisoned");
        entries.insert(id, CapEntry {
            name: name.to_string(),
            consumed: false,
        });
        id
    }

    /// Check whether the cap with the given ID exists, matches the name, and
    /// has not been consumed.  Does NOT consume the cap.
    pub fn check(&self, id: i64, name: &str) -> bool {
        let entries = self.entries.lock()
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
        let mut entries = self.entries.lock()
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
        let mut entries = self.entries.lock()
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
        let guard = self.inner.read()
            .expect("SharedHandle inner read lock poisoned");
        f(&*guard)
    }

    /// Execute a closure with a mutable reference to the inner value.
    /// Safe, scoped access — prefer this over raw pointer APIs.
    pub fn with_value_mut<R>(&self, f: impl FnOnce(&mut Value) -> R) -> R {
        let mut guard = self.inner.write()
            .expect("SharedHandle inner write lock poisoned");
        f(&mut *guard)
    }

    /// Get a read guard for the inner value.
    pub fn borrow(&self) -> RwLockReadGuard<'_, Value> {
        self.inner.read()
            .expect("SharedHandle inner read lock poisoned")
    }

    /// Get a write guard for the inner value.
    pub fn borrow_mut(&self) -> RwLockWriteGuard<'_, Value> {
        self.inner.write()
            .expect("SharedHandle inner write lock poisoned")
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
        let mut handles = self.handles.lock()
            .expect("SHARED_TABLE handles lock poisoned");
        handles.insert(id, handle);
        id
    }

    /// Create or reuse a handle, deduplicating by Arc inner pointer.
    /// If the same Arc pointer has been registered before, returns the
    /// existing handle ID and increments the C-side reference count.
    pub fn create_dedup(&self, inner: Arc<RwLock<Value>>, ptr: *const ()) -> i64 {
        // Check dedup table first (lock dedup, then release before locking handles)
        {
            let dedup = self.dedup.lock()
                .expect("SHARED_TABLE dedup lock poisoned");
            if let Some(&existing_id) = dedup.get(&ptr) {
                // Found existing handle — retain and return existing ID.
                // Don't hold dedup lock while touching handles to avoid deadlock.
                drop(dedup);
                if self.retain(existing_id) {
                    return existing_id;
                }
                // Handle was cleaned up from under us; fall through to create new.
            }
        }

        // Create new handle
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let handle = Arc::new(SharedHandle::new(id, inner));
        {
            let mut handles = self.handles.lock()
                .expect("SHARED_TABLE handles lock poisoned");
            handles.insert(id, handle);
        }
        {
            let mut dedup = self.dedup.lock()
                .expect("SHARED_TABLE dedup lock poisoned");
            dedup.insert(ptr, id);
        }
        id
    }

    /// Get a reference to the handle by ID.
    pub fn get(&self, id: i64) -> Option<Arc<SharedHandle>> {
        let handles = self.handles.lock()
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
            let handles = self.handles.lock()
                .expect("SHARED_TABLE handles lock poisoned");
            handles.get(&id).cloned()
        };
        if let Some(handle) = handle {
            if handle.release() {
                let removed = {
                    let mut handles = self.handles.lock()
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
            let mut handles = self.handles.lock()
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
        let handles = self.handles.lock()
            .expect("SHARED_TABLE handles lock poisoned");
        handles.len()
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
    SHARED_TABLE.with(|table| { table.retain(handle); });
    handle
}

/// Release a shared handle.
#[no_mangle]
pub extern "C" fn mimi_shared_release(handle: i64) {
    SHARED_TABLE.with(|table| { table.release(handle); });
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

/// Free a Value that was obtained via `mimi_shared_get_ptr`.
/// This must be called for every non-null pointer returned by `mimi_shared_get_ptr`.
#[no_mangle]
pub extern "C" fn mimi_value_free(ptr: *const Value) {
    if !ptr.is_null() {
        // SAFETY: ptr is a non-null pointer to a heap-allocated Value, previously obtained via Box::into_raw.
        unsafe { drop(Box::from_raw(ptr as *mut Value)); }
    }
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
                match std::ffi::CString::new(s.as_str()) {
                    Ok(c_str) => {
                        let ptr = c_str.as_ptr();
                        // Register for cleanup — caller must call mimi_string_as_c_str_free
                        PENDING_C_STRINGS.with(|pending| pending.borrow_mut().push(c_str));
                        ptr
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
    /// to release the entry.  Entries that are never freed will leak until
    /// thread exit.
    static PENDING_C_STRINGS: RefCell<Vec<std::ffi::CString>> = RefCell::new(Vec::new());
}

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
#[no_mangle]
pub extern "C" fn mimi_string_from_raw(c_str: *mut std::ffi::c_char) -> *mut Value {
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
                let task = receiver.lock()
                    .expect("MIMI_POOL receiver lock poisoned")
                    .recv();
                match task {
                    Ok(task) => {
                        let _ = (task.func)(task.arg);
                        // Decrement pending count and notify waiters
                        let mut count = task.pending.lock()
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

        MimiThreadPool { workers, sender: Some(sender), pending, completion }
    }

    pub fn submit_raw(&self, func: extern "C" fn(*mut u8) -> *mut u8, arg: *mut u8) {
        let mut count = self.pending.lock()
            .expect("MIMI_POOL pending counter lock poisoned");
        *count += 1;
        if let Some(ref sender) = self.sender {
            let _ = sender.send(RawTask {
                func,
                arg,
                pending: Arc::clone(&self.pending),
                completion: Arc::clone(&self.completion),
            });
        }
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
        let mut count = self.pending.lock()
            .expect("MIMI_POOL pending counter lock poisoned");
        *count += 1;
        if let Some(ref sender) = self.sender {
            let _ = sender.send(RawTask {
                func: data_trampoline,
                arg: data as *mut u8,
                pending: Arc::clone(&self.pending),
                completion: Arc::clone(&self.completion),
            });
        }
    }

    /// Wait until all submitted tasks have completed.
    pub fn join_all(&self) {
        let mut count = self.pending.lock()
            .expect("MIMI_POOL pending counter lock poisoned");
        while *count > 0 {
            count = self.completion.wait(count)
                .expect("MIMI_POOL completion condvar lock poisoned");
        }
    }
}

impl Drop for MimiThreadPool {
    fn drop(&mut self) {
        // Drop the sender first to close the channel, signaling workers to exit.
        drop(self.sender.take());
        // Join worker threads.
        for handle in self.workers.drain(..) {
            let _ = handle.join();
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
