//! Mimi runtime poll-based async runtime — `MimiFuture` + `MimiExecutor`
//! (future alloc/completion, spawn/await, executor spawn/run).
//!
//! Extracted verbatim from `runtime/mod.rs` (the `MimiFuture + MimiExecutor`
//! section) during the 0.1.0 mechanical split (behavior bit-exact).
//! Self-contained: `MimiFutureRepr` / `SendPtr` / `EXECUTOR_QUEUE` /
//! `SPAWN_HANDLES` all defined within. Pure `extern "C"` leaf (no crate-level
//! Rust-path callers). Part of the planned concurrency surface; uses `libc`
//! in standalone mode.

use std::sync::atomic::Ordering;

#[cfg(standalone)]
use super::libc;

// ─── MimiFuture + MimiExecutor (poll-based async runtime) ──────
//
// Future memory layout (managed by codegen):
//   offset 0: i32 (completed flag: 0=pending, 1=ready, -1=freed intent)
//   offset 4: i32 (refcount; starts at 1)
//   offset 8: <result> (8-byte aligned, up to 64 bytes)
//
// R-C5: free/poll must not UAF. Use atomic refcount so free only drops
// when the last concurrent accessor releases.

#[repr(C)]
struct MimiFutureRepr {
    completed: std::sync::atomic::AtomicI32,
    refs: std::sync::atomic::AtomicI32,
    data: [u8; 64],
}

/// Try to retain a live future. Returns false if already fully freed (refs==0).
/// SAFETY: `fut` must be null or a pointer from `mimi_future_alloc` that has
/// not yet been fully deallocated (refs may still be > 0 during free races).
unsafe fn future_try_retain(fut: *mut MimiFutureRepr) -> bool {
    use std::sync::atomic::Ordering;
    let rep = &*fut;
    let mut cur = rep.refs.load(Ordering::Acquire);
    loop {
        if cur <= 0 {
            return false;
        }
        match rep
            .refs
            .compare_exchange_weak(cur, cur + 1, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => return true,
            Err(c) => cur = c,
        }
    }
}

/// Release one ref; drop allocation when last ref is gone.
/// SAFETY: `fut` must have been successfully retained or be the owner ref.
unsafe fn future_release(fut: *mut MimiFutureRepr) {
    use std::sync::atomic::Ordering;
    let rep = &*fut;
    if rep.refs.fetch_sub(1, Ordering::Release) == 1 {
        // Ensure all prior accesses complete before deallocation.
        std::sync::atomic::fence(Ordering::Acquire);
        drop(Box::from_raw(fut));
    }
}

#[no_mangle]
pub extern "C" fn mimi_future_alloc(_result_size: u64) -> *mut std::ffi::c_void {
    use std::sync::atomic::AtomicI32;
    let b = Box::new(MimiFutureRepr {
        completed: AtomicI32::new(0),
        refs: AtomicI32::new(1),
        data: [0; 64],
    });
    Box::into_raw(b) as *mut std::ffi::c_void
}

#[no_mangle]
pub extern "C" fn mimi_future_free(fut: *mut std::ffi::c_void) {
    if fut.is_null() {
        return;
    }
    // SAFETY: non-null pointer from mimi_future_alloc (or already freed → retain fails).
    unsafe {
        let fut = fut as *mut MimiFutureRepr;
        // Mark freed-intent so set_completed CAS fails; then drop owner ref.
        (*fut).completed.store(-1, Ordering::Release);
        future_release(fut);
    }
}

#[no_mangle]
pub extern "C" fn mimi_future_set_completed(fut: *mut std::ffi::c_void) {
    if fut.is_null() {
        return;
    }
    use std::sync::atomic::Ordering;
    // R-C5: retain for the duration of the CAS so free cannot drop under us.
    unsafe {
        let fut = fut as *mut MimiFutureRepr;
        if !future_try_retain(fut) {
            return;
        }
        let rep = &*fut;
        let _ = rep
            .completed
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
        future_release(fut);
    }
}

#[no_mangle]
pub extern "C" fn mimi_future_is_completed(fut: *mut std::ffi::c_void) -> i32 {
    if fut.is_null() {
        return 1;
    }
    use std::sync::atomic::Ordering;
    // R-C5: retain before reading so concurrent free cannot UAF.
    unsafe {
        let fut = fut as *mut MimiFutureRepr;
        if !future_try_retain(fut) {
            return 1; // already freed — treat as completed/dead
        }
        let v = (*fut).completed.load(Ordering::Acquire);
        future_release(fut);
        if v < 0 {
            1
        } else {
            v
        }
    }
}

/// Spawned thread handles retained so they can be joined before process exit.
/// H15 fix: use OnceLock so the atexit handler can check whether SPAWN_HANDLES
/// is still initialized before accessing it. This prevents UB when atexit fires
/// after Rust's static destructors have already dropped the Mutex.
static SPAWN_HANDLES: std::sync::OnceLock<std::sync::Mutex<Vec<std::thread::JoinHandle<()>>>> =
    std::sync::OnceLock::new();
static SPAWN_ATEXIT_REGISTERED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn get_spawn_handles() -> &'static std::sync::Mutex<Vec<std::thread::JoinHandle<()>>> {
    SPAWN_HANDLES.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

extern "C" fn mimi_join_spawned_threads_atexit() {
    // H15 fix: check if SPAWN_HANDLES is still initialized before trying to
    // lock it. If Rust statics have already been dropped, OnceLock::get()
    // returns None and we skip joining (handles will be detached by OS).
    if let Some(handles_mutex) = SPAWN_HANDLES.get() {
        if let Ok(mut handles) = handles_mutex.lock() {
            for handle in handles.drain(..) {
                let _ = handle.join();
            }
        }
    }
}

/// Spawn a future on a real thread (used by codegen `spawn expr`).
/// The poll function is called on a new thread, which sets completed=1 when done.
/// Returns the future pointer (same as input).
/// The returned `JoinHandle` is retained in `SPAWN_HANDLES` and joined at
/// process exit so that the pthread stack is freed before Valgrind checks.
#[no_mangle]
pub extern "C" fn mimi_spawn_future(
    future: *mut std::ffi::c_void,
    // SAFETY: unsafe extern "C" function pointer used for C poll callbacks; see # Safety docs.
    poll_fn: unsafe extern "C" fn(*mut std::ffi::c_void),
) -> *mut std::ffi::c_void {
    if future.is_null() {
        return std::ptr::null_mut();
    }
    // R-C5: retain one ref for the worker thread; release when poll_fn returns.
    // SAFETY: non-null pointer from mimi_future_alloc.
    unsafe {
        let fut = future as *mut MimiFutureRepr;
        if !future_try_retain(fut) {
            return std::ptr::null_mut();
        }
    }
    let future_addr = future as usize;
    let handle = std::thread::spawn(move || {
        // SAFETY: retained above for this thread's lifetime.
        unsafe {
            let fut = future_addr as *mut MimiFutureRepr;
            poll_fn(fut as *mut std::ffi::c_void);
            future_release(fut);
        }
    });
    if let Ok(mut handles) = get_spawn_handles().lock() {
        handles.push(handle);
    }
    // Register an atexit handler once to join all spawned threads before exit.
    if SPAWN_ATEXIT_REGISTERED
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        )
        .is_ok()
    {
        // SAFETY: `mimi_join_spawned_threads_atexit` has C ABI and no parameters.
        unsafe { libc::atexit(mimi_join_spawned_threads_atexit) };
    }
    future
}

/// Wait (spin) for a future to become completed. Used by codegen `await`
/// for thread-spawned futures (not managed by the single-threaded executor).
#[no_mangle]
pub extern "C" fn mimi_await_future(future: *mut std::ffi::c_void) {
    if future.is_null() {
        return;
    }
    use std::sync::atomic::Ordering;
    // R-C5: retain for the spin so concurrent free cannot free under us.
    // SAFETY: non-null pointer from mimi_future_alloc.
    unsafe {
        let fut = future as *mut MimiFutureRepr;
        if !future_try_retain(fut) {
            return;
        }
        let mut iterations: u64 = 0;
        const MAX_SPIN_ITERATIONS: u64 = 1_000_000;
        while (*fut).completed.load(Ordering::Acquire) == 0 {
            std::thread::yield_now();
            iterations += 1;
            if iterations >= MAX_SPIN_ITERATIONS {
                future_release(fut);
                std::process::abort();
            }
        }
        future_release(fut);
    }
}

type PollFn = unsafe extern "C" fn(*mut std::ffi::c_void);

/// Wrapper to make *mut c_void Send (needed for Mutex).
/// FFI-8: Soundness — a raw pointer is Send because:
/// - Sending a *mut T transfers exclusive ownership of the referent to the receiving thread
/// - The future pointer is only dereferenced inside `mimi_executor_run` while holding the queue mutex,
///   guaranteeing exclusive access (no data race)
/// - The pointer came from `mimi_rc_alloc` (system allocator, not thread-local), so it is safe to
///   access from any thread after the send
/// - `Sync` is safe because &SendPtr is never shared across threads (only &mut access via the mutex)
#[derive(Clone)]
struct SendPtr(*mut std::ffi::c_void);
// SAFETY: already documented above.
unsafe impl Send for SendPtr {}
// SAFETY: already documented above.
unsafe impl Sync for SendPtr {}

type ExecutorEntry = (PollFn, SendPtr);

static EXECUTOR_QUEUE: std::sync::Mutex<Vec<ExecutorEntry>> = std::sync::Mutex::new(Vec::new());

/// Submit a future + its poll function to the global executor.
/// The future is not polled immediately; call mimi_executor_run() to poll.
#[no_mangle]
pub extern "C" fn mimi_executor_spawn(
    future: *mut std::ffi::c_void,
    // SAFETY: unsafe extern "C" function pointer used for C poll callbacks; see # Safety docs.
    poll_fn: unsafe extern "C" fn(*mut std::ffi::c_void),
) {
    if future.is_null() {
        return;
    }
    let mut queue = EXECUTOR_QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    // Don't add duplicates
    if !queue.iter().any(|(_, f)| f.0 == future) {
        queue.push((poll_fn, SendPtr(future)));
    }
}

/// Poll all pending futures in the executor until all are completed.
/// Futures that become completed are removed from the queue.
#[no_mangle]
pub extern "C" fn mimi_executor_run() {
    loop {
        let entry = {
            let mut queue = EXECUTOR_QUEUE.lock().unwrap_or_else(|e| e.into_inner());
            if queue.is_empty() {
                return;
            }
            let mut found = None;
            for i in 0..queue.len() {
                let (_, future) = &queue[i];
                // SAFETY: future pointer came from the executor queue.
                // R-C5: retain while reading completed so free cannot UAF.
                let completed = unsafe {
                    let fut = future.0 as *mut MimiFutureRepr;
                    if !future_try_retain(fut) {
                        1 // freed — treat as done
                    } else {
                        let v = (*fut).completed.load(Ordering::Acquire);
                        future_release(fut);
                        if v < 0 {
                            1
                        } else {
                            v
                        }
                    }
                };
                if completed == 0 {
                    found = Some(i);
                    break;
                }
            }
            match found {
                Some(i) => {
                    let (poll_fn, future) = queue.swap_remove(i);
                    Some((poll_fn, future.0))
                }
                None => {
                    queue.clear();
                    return;
                }
            }
        };
        if let Some((poll_fn, future)) = entry {
            // SAFETY: retain for poll duration (R-C5).
            unsafe {
                let fut = future as *mut MimiFutureRepr;
                if future_try_retain(fut) {
                    poll_fn(future);
                    future_release(fut);
                }
            }
        }
    }
}
