//! Mimi runtime poll-based async runtime — `MimiFuture` + `MimiExecutor`
//! (future alloc/completion, spawn/await, executor spawn/run).
//!
//! Extracted verbatim from `runtime/mod.rs` (the `MimiFuture + MimiExecutor`
//! section) during the 0.1.0 mechanical split (behavior bit-exact).
//! Self-contained: `MimiFutureHeader` / `SendPtr` / `EXECUTOR_QUEUE` /
//! `SPAWN_HANDLES` all defined within. Pure `extern "C"` leaf (no crate-level
//! Rust-path callers). Part of the planned concurrency surface; uses `libc`
//! in standalone mode.
//!
//! # Future memory layout — ABI contract (must match codegen, `src/codegen/func.rs`)
//!
//! Futures are allocated by `mimi_future_alloc(data_size)` via
//! `std::alloc::alloc` with alignment 8:
//!
//! ```text
//!   offset 0..4   : AtomicI32 `completed`     (0=pending, 1=ready, -1=freed intent)
//!   offset 4..8   : AtomicI32 `refs`          (refcount; starts at 1)
//!   offset 8..16  : u64 `data_capacity`       (exact size in bytes of the data
//!                                              region written at alloc time; >= 64)
//!   offset 16..   : data region — `data_capacity` bytes. Codegen stores the
//!                   async fn's arguments and result here (FUTURE_DATA_OFFSET).
//! ```
//!
//! Total allocation size = `FUTURE_HEADER_SIZE (16) + data_capacity`, where
//! `data_capacity = max(requested_data_size, 64)`. `data_capacity` stores the
//! ACTUAL allocated data size so `mimi_future_free` can reconstruct the exact
//! `Layout` (16 + data_capacity, align 8) and deallocate precisely.
//!
//! OLD ABI (removed by the 2026-08-05 full-audit fix, CRITICAL #11): a fixed
//! 72-byte `Box` (`completed`, `refs`, `data: [u8; 64]`) with the data region
//! at offset 8. `mimi_future_alloc` ignored its size argument while codegen
//! stored args/result past offset 8 by the computed size → heap overflow for
//! async fns with large args/results. Do not reintroduce a fixed-size future.
//!
//! R-C5: free/poll must not UAF. Use atomic refcount so free only drops
//! when the last concurrent accessor releases.

#[cfg(standalone)]
use super::libc;
use std::sync::atomic::Ordering;
use std::sync::{Condvar, Mutex};

/// Header prefix of every future allocation. The data region immediately
/// follows (see module docs). `#[repr(C)]` pins the field offsets:
/// completed @0, refs @4, data_capacity @8.
#[repr(C)]
struct MimiFutureHeader {
    completed: std::sync::atomic::AtomicI32,
    refs: std::sync::atomic::AtomicI32,
    data_capacity: u64,
}

const FUTURE_HEADER_SIZE: usize = 16;
const FUTURE_DATA_OFFSET: usize = 16;
const FUTURE_MIN_DATA_CAPACITY: usize = 64;
const FUTURE_ALIGN: usize = 8;

/// Reconstruct the allocation Layout for a future with the given stored
/// `data_capacity`. Returns `None` if the capacity is below the minimum
/// (corruption) or the total size overflows / exceeds `Layout` limits.
fn future_layout(data_capacity: u64) -> Option<std::alloc::Layout> {
    // Reject unrepresentable sizes on narrow (32-bit) targets explicitly.
    if data_capacity > usize::MAX as u64 {
        return None;
    }
    if (data_capacity as usize) < FUTURE_MIN_DATA_CAPACITY {
        return None;
    }
    let total = FUTURE_HEADER_SIZE.checked_add(data_capacity as usize)?;
    std::alloc::Layout::from_size_align(total, FUTURE_ALIGN).ok()
}

/// Try to retain a live future. Returns false if already fully freed (refs==0).
/// SAFETY: `fut` must be null or a pointer from `mimi_future_alloc` that has
/// not yet been fully deallocated (refs may still be > 0 during free races).
unsafe fn future_try_retain(fut: *mut MimiFutureHeader) -> bool {
    // SAFETY: caller guarantees `fut` points to a live allocation.
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

/// Release one ref; deallocate when the last ref is gone.
/// SAFETY: `fut` must have been successfully retained or be the owner ref.
unsafe fn future_release(fut: *mut MimiFutureHeader) {
    // SAFETY: caller guarantees `fut` points to a live allocation.
    let rep = &*fut;
    if rep.refs.fetch_sub(1, Ordering::Release) == 1 {
        // Ensure all prior accesses complete before deallocation.
        std::sync::atomic::fence(Ordering::Acquire);
        // We hold the last reference, so reading `data_capacity` (written
        // once by mimi_future_alloc, immutable afterwards) cannot race.
        let data_capacity = rep.data_capacity;
        let layout = match future_layout(data_capacity) {
            Some(l) => l,
            // Corrupt header: fail loud instead of freeing with a wrong layout.
            None => super::mimi_runtime_abort(
                b"mimi_future_free: corrupt data_capacity in future header\0".as_ptr()
                    as *const std::ffi::c_char,
            ),
        };
        // SAFETY: `fut` was allocated by mimi_future_alloc with exactly this
        // Layout (16 + data_capacity, align 8) and all references are gone.
        std::alloc::dealloc(fut as *mut u8, layout);
    }
}

/// Allocate a future with a data region of at least `result_size` bytes
/// (minimum 64). See the module-level ABI contract for the layout.
/// Returns null if the size cannot be represented or allocation fails.
#[no_mangle]
pub extern "C" fn mimi_future_alloc(result_size: u64) -> *mut std::ffi::c_void {
    // Audit fix (CRITICAL #11): honor the requested size. The old
    // implementation ignored it and always allocated a fixed 72-byte box,
    // while codegen stored args/result by computed size → heap overflow.
    let requested = match usize::try_from(result_size) {
        Ok(v) => v,
        Err(_) => return std::ptr::null_mut(),
    };
    let data_capacity = requested.max(FUTURE_MIN_DATA_CAPACITY);
    // data_capacity >= 64 and FUTURE_HEADER_SIZE is small, so this cannot fail;
    // keep the fallible path for defensive symmetry with future_release.
    let layout = match future_layout(data_capacity as u64) {
        Some(l) => l,
        None => return std::ptr::null_mut(),
    };
    // SAFETY: layout size >= 80 and alignment 8 is a power of two;
    // std::alloc::alloc returns null on failure, which we propagate.
    let ptr = unsafe { std::alloc::alloc(layout) };
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `ptr` points to a fresh, uninitialized allocation of at least
    // 80 bytes (>= size_of::<MimiFutureHeader>() == 16), aligned for every
    // header field (AtomicI32 @0/@4 need align 4, u64 @8 needs align 8).
    // Fields are written with ptr::write so the uninitialized memory is never
    // read and nothing is dropped in place.
    unsafe {
        let hdr = ptr as *mut MimiFutureHeader;
        std::ptr::write(
            std::ptr::addr_of_mut!((*hdr).completed),
            std::sync::atomic::AtomicI32::new(0),
        );
        std::ptr::write(
            std::ptr::addr_of_mut!((*hdr).refs),
            std::sync::atomic::AtomicI32::new(1),
        );
        std::ptr::write(
            std::ptr::addr_of_mut!((*hdr).data_capacity),
            data_capacity as u64,
        );
    }
    ptr as *mut std::ffi::c_void
}

/// Global wait channel used by `mimi_await_future` so waiting threads block
/// instead of spin-aborting after a fixed iteration cap.
///
/// A single condvar is enough: completions are ordered by the same mutex,
/// waiters re-check the atomic flag after every wakeup, and spurious wakeups
/// are harmless. This also avoids per-future heap allocations in the runtime
/// (important for Valgrind-clean FFI binaries).
static AWAIT_LOCK: Mutex<()> = Mutex::new(());
static AWAIT_CONDVAR: Condvar = Condvar::new();

/// Mark a freed intent and release the owner ref. The allocation is
/// deallocated when the refcount reaches zero (see `future_release`).
///
/// Audit 2026-08-05 (N-3): the retain check now runs BEFORE the header
/// write. The old code stored the freed-intent first, so a double-free
/// WROTE to the already-freed header (UAF write) before the refcount could
/// reject it — every other future API has the retain precheck; free was the
/// exception. §10-#23 (closed 0.36.109 by design): standard Arc-class
/// boundary remains — a double-free of a FULLY deallocated pointer still
/// touches freed memory to read the refcount (no live registry exists for
/// bare pointers), but it is now a rejected read, never a write.
#[no_mangle]
pub extern "C" fn mimi_future_free(fut: *mut std::ffi::c_void) {
    if fut.is_null() {
        return;
    }
    // SAFETY: non-null pointer from mimi_future_alloc (precondition above);
    // the retain below rejects already-freed futures before any write.
    unsafe {
        let fut = fut as *mut MimiFutureHeader;
        if !future_try_retain(fut) {
            return; // already fully freed — reject before touching the header
        }
        // SAFETY: successfully retained → the allocation is live for the
        // store. Mark freed-intent so a concurrent set_completed CAS fails.
        (*fut).completed.store(-1, Ordering::Release);
        future_release(fut); // drop the ref taken above
        future_release(fut); // drop the owner ref that free() releases
    }
}

#[no_mangle]
pub extern "C" fn mimi_future_set_completed(fut: *mut std::ffi::c_void) {
    if fut.is_null() {
        return;
    }
    // R-C5: retain for the duration of the CAS so free cannot drop under us.
    // SAFETY: `fut` comes from mimi_future_alloc; the retain below rejects
    // already-freed futures before the header is touched for the CAS.
    unsafe {
        let fut = fut as *mut MimiFutureHeader;
        if !future_try_retain(fut) {
            return;
        }
        // SAFETY: successfully retained → allocation is live.
        let rep = &*fut;
        // Ordering with await: await holds AWAIT_LOCK while checking the
        // atomic flag, so a successful CAS under the same lock cannot miss
        // the condvar notify.
        let _guard = AWAIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if rep
            .completed
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            AWAIT_CONDVAR.notify_all();
        }
        future_release(fut);
    }
}

#[no_mangle]
pub extern "C" fn mimi_future_is_completed(fut: *mut std::ffi::c_void) -> i32 {
    if fut.is_null() {
        return 1;
    }
    // R-C5: retain before reading so concurrent free cannot UAF.
    // SAFETY: `fut` comes from mimi_future_alloc; the retain below rejects
    // already-freed futures before the header is read.
    unsafe {
        let fut = fut as *mut MimiFutureHeader;
        if !future_try_retain(fut) {
            return 1; // already freed — treat as completed/dead
        }
        // SAFETY: successfully retained → allocation is live.
        let v = (*fut).completed.load(Ordering::Acquire);
        future_release(fut);
        if v < 0 {
            1
        } else {
            v
        }
    }
}

/// Spawned task queue used by the lightweight worker pool.
///
/// `mimi_spawn_future` no longer creates a fresh OS thread per future; it
/// submits to a bounded pool of reusable workers.  This is the first slice of
/// the 0.1.7 Phase C lightweight Task scheduler work: tasks are still polled
/// on real threads, but thread count is bounded and worker threads are joined
/// before Valgrind checks.
struct SpawnJob {
    future_addr: usize,
    poll_fn: PollFn,
}

// SAFETY: SpawnJob only carries a function pointer and an address-sized raw
// pointer representation.  The future is separately reference-counted, so the
// pointer remains valid for whichever worker receives the job.
unsafe impl Send for SpawnJob {}

struct SpawnPool {
    sender: Option<std::sync::mpsc::Sender<SpawnJob>>,
    handles: Vec<std::thread::JoinHandle<()>>,
}

static SPAWN_POOL: std::sync::OnceLock<std::sync::Mutex<Option<SpawnPool>>> =
    std::sync::OnceLock::new();
static SPAWN_POOL_ATEXIT_REGISTERED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Dedicated threads for nested spawns from inside a pool worker.  A pooled
/// worker may block while awaiting another future; queueing that child back
/// into the same bounded pool could deadlock when every worker is blocked.
static SPAWN_HANDLES: std::sync::OnceLock<std::sync::Mutex<Vec<std::thread::JoinHandle<()>>>> =
    std::sync::OnceLock::new();

thread_local! {
    static IN_WORKER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn get_spawn_handles() -> &'static std::sync::Mutex<Vec<std::thread::JoinHandle<()>>> {
    SPAWN_HANDLES.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

fn spawn_worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().max(2))
        .unwrap_or(4)
        .min(8)
}

fn worker_loop(rx: std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<SpawnJob>>>) {
    loop {
        let job = {
            let lock = rx.lock().unwrap_or_else(|e| e.into_inner());
            lock.recv()
        };
        let Ok(job) = job else {
            // All senders dropped: shutdown signal.
            return;
        };
        // SAFETY: retained by mimi_spawn_future before submitting; the worker
        // ref is released after poll_fn returns, so the pointer stays live.
        IN_WORKER.with(|flag| {
            let previous = flag.get();
            flag.set(true);
            unsafe {
                let fut = job.future_addr as *mut MimiFutureHeader;
                (job.poll_fn)(fut as *mut std::ffi::c_void);
                future_release(fut);
            }
            flag.set(previous);
        });
    }
}

fn ensure_spawn_pool() -> Option<std::sync::mpsc::Sender<SpawnJob>> {
    let pool_mutex = SPAWN_POOL.get_or_init(|| std::sync::Mutex::new(None));
    let mut guard = pool_mutex.lock().ok()?;
    if let Some(pool) = guard.as_ref() {
        return pool.sender.clone();
    }
    let (tx, rx) = std::sync::mpsc::channel::<SpawnJob>();
    let rx = std::sync::Arc::new(std::sync::Mutex::new(rx));
    let workers = spawn_worker_count();
    let mut handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let rx = rx.clone();
        handles.push(std::thread::spawn(move || worker_loop(rx)));
    }
    *guard = Some(SpawnPool {
        sender: Some(tx.clone()),
        handles,
    });
    Some(tx)
}

extern "C" fn mimi_join_spawn_pool_atexit() {
    // H15-style safety: if Rust statics were already dropped, OnceLock::get()
    // returns None and we skip joining (the OS will reap worker threads).
    if let Some(pool_mutex) = SPAWN_POOL.get() {
        if let Ok(mut guard) = pool_mutex.lock() {
            if let Some(pool) = guard.as_mut() {
                // Closing the channel makes idle workers exit after finishing
                // any in-flight task. Joining guarantees the pthread stacks are
                // released before Valgrind checks.
                pool.sender.take();
                for handle in pool.handles.drain(..) {
                    let _ = handle.join();
                }
            }
        }
    }
    // Join dedicated nested-spawn threads too.
    if let Some(handles_mutex) = SPAWN_HANDLES.get() {
        if let Ok(mut handles) = handles_mutex.lock() {
            for handle in handles.drain(..) {
                let _ = handle.join();
            }
        }
    }
}

/// Spawn a future on a real worker thread (used by codegen `spawn expr`).
/// The poll function is submitted to a bounded global worker pool, which calls
/// it and sets completed=1 when done. Returns the future pointer (same input).
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
        let fut = future as *mut MimiFutureHeader;
        if !future_try_retain(fut) {
            return std::ptr::null_mut();
        }
    }
    // Nested spawns from inside a pool worker use a dedicated thread.  This
    // keeps bounded workers safe when an async body itself awaits another
    // spawned future: the child is never queued behind a full pool.
    if IN_WORKER.with(|flag| flag.get()) {
        let future_addr = future as usize;
        let handle = std::thread::spawn(move || {
            // This dedicated thread is itself a worker context. Mark it so
            // further nested spawns also get dedicated threads; otherwise a
            // deep spawn chain can queue back into the bounded pool while all
            // pool workers are blocked awaiting children (deadlock).
            IN_WORKER.with(|flag| {
                let previous = flag.get();
                flag.set(true);
                // SAFETY: retained above for this thread's lifetime; the ref is
                // released after poll_fn returns, so the pointer stays live.
                unsafe {
                    let fut = future_addr as *mut MimiFutureHeader;
                    poll_fn(fut as *mut std::ffi::c_void);
                    future_release(fut);
                }
                flag.set(previous);
            });
        });
        if let Ok(mut handles) = get_spawn_handles().lock() {
            handles.push(handle);
        }
        if SPAWN_POOL_ATEXIT_REGISTERED
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
        {
            // SAFETY: `mimi_join_spawn_pool_atexit` has C ABI and no parameters.
            unsafe { libc::atexit(mimi_join_spawn_pool_atexit) };
        }
        return future;
    }
    let Some(sender) = ensure_spawn_pool() else {
        // Extremely defensive fallback: no pool available, don't leak the ref.
        unsafe {
            let fut = future as *mut MimiFutureHeader;
            future_release(fut);
        }
        return std::ptr::null_mut();
    };
    let job = SpawnJob {
        future_addr: future as usize,
        poll_fn,
    };
    if sender.send(job).is_err() {
        // Pool already shut down; release the worker ref and fail cleanly.
        unsafe {
            let fut = future as *mut MimiFutureHeader;
            future_release(fut);
        }
        return std::ptr::null_mut();
    }
    // Register an atexit handler once to join all workers before exit.
    if SPAWN_POOL_ATEXIT_REGISTERED
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        )
        .is_ok()
    {
        // SAFETY: `mimi_join_spawn_pool_atexit` has C ABI and no parameters.
        unsafe { libc::atexit(mimi_join_spawn_pool_atexit) };
    }
    future
}

/// Wait for a future to become completed. Used by codegen `await` for
/// thread-spawned futures.  Uses a per-future condvar so long-running tasks do
/// not trip an artificial spin cap and abort the process.
#[no_mangle]
pub extern "C" fn mimi_await_future(future: *mut std::ffi::c_void) {
    if future.is_null() {
        return;
    }
    // R-C5: retain for the wait so concurrent free cannot free under us.
    // SAFETY: non-null pointer from mimi_future_alloc.
    unsafe {
        let fut = future as *mut MimiFutureHeader;
        if !future_try_retain(fut) {
            return;
        }
        // Fast path: already completed before we block on the condvar.
        if (*fut).completed.load(Ordering::Acquire) != 0 {
            future_release(fut);
            return;
        }
        let mut guard = AWAIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: successfully retained → allocation is live while waiting.
        while (*fut).completed.load(Ordering::Acquire) == 0 {
            guard = AWAIT_CONDVAR.wait(guard).unwrap_or_else(|e| e.into_inner());
        }
        drop(guard);
        future_release(fut);
    }
}

type PollFn = unsafe extern "C" fn(*mut std::ffi::c_void);

/// Wrapper to make *mut c_void Send (needed for Mutex).
/// FFI-8: Soundness — a raw pointer is Send because:
/// - Sending a *mut T transfers exclusive ownership of the referent to the receiving thread
/// - The future pointer is only dereferenced inside `mimi_executor_run` while holding the queue mutex,
///   guaranteeing exclusive access (no data race)
/// - The pointer came from `mimi_future_alloc` (system allocator, not thread-local), so it is safe to
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
                // SAFETY: future pointer came from the executor queue
                // (mimi_executor_spawn) and was allocated by mimi_future_alloc.
                // R-C5: retain while reading completed so free cannot UAF.
                let completed = unsafe {
                    let fut = future.0 as *mut MimiFutureHeader;
                    if !future_try_retain(fut) {
                        1 // freed — treat as done
                    } else {
                        // SAFETY: successfully retained → allocation is live.
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
            // SAFETY: future came from the executor queue; retain for the
            // poll duration (R-C5) so concurrent free cannot drop under us.
            unsafe {
                let fut = future as *mut MimiFutureHeader;
                if future_try_retain(fut) {
                    poll_fn(future);
                    future_release(fut);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Regression tests for the 2026-08-05 audit fix (CRITICAL #11):
    //! mimi_future_alloc must honor the requested data size instead of always
    //! allocating the old fixed 72-byte box.

    use super::*;

    #[test]
    fn future_alloc_honors_requested_size_and_frees_exactly() {
        let f = mimi_future_alloc(1024);
        assert!(!f.is_null());
        // SAFETY: `f` was just allocated by mimi_future_alloc with
        // data_capacity 1024 (>= header + 1024 bytes, align 8); header reads
        // and data writes below stay within that allocation.
        unsafe {
            let hdr = f as *mut MimiFutureHeader;
            assert_eq!((*hdr).completed.load(Ordering::SeqCst), 0);
            assert_eq!((*hdr).refs.load(Ordering::SeqCst), 1);
            assert_eq!((*hdr).data_capacity, 1024);
            // The data region (offset 16) must be writable for the FULL
            // requested size — the old ABI overflowed the 64-byte inline
            // buffer here.
            let data = (f as *mut u8).add(FUTURE_DATA_OFFSET);
            std::ptr::write_volatile(data, 0xAB);
            std::ptr::write_volatile(data.add(1023), 0xCD);
            assert_eq!(std::ptr::read_volatile(data), 0xAB);
            assert_eq!(std::ptr::read_volatile(data.add(1023)), 0xCD);
        }
        mimi_future_set_completed(f);
        assert_eq!(mimi_future_is_completed(f), 1);
        mimi_future_free(f);
    }

    #[test]
    fn future_alloc_applies_min_capacity() {
        let f = mimi_future_alloc(0);
        assert!(!f.is_null());
        // SAFETY: `f` was just allocated by mimi_future_alloc.
        unsafe {
            let hdr = f as *mut MimiFutureHeader;
            assert_eq!((*hdr).data_capacity, FUTURE_MIN_DATA_CAPACITY as u64);
        }
        mimi_future_free(f);
    }

    #[test]
    fn future_alloc_rejects_unrepresentable_size() {
        // usize::try_from fails on 64-bit only for values > usize::MAX;
        // on any platform, u64::MAX must either fail cleanly or allocate —
        // never panic across the FFI boundary.
        let f = mimi_future_alloc(u64::MAX);
        if !f.is_null() {
            mimi_future_free(f);
        }
    }

    #[test]
    fn future_await_blocks_until_completion_without_spin_abort() {
        use std::time::Duration;

        let f = mimi_future_alloc(64);
        assert!(!f.is_null());
        let other = f as usize;
        let completer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            mimi_future_set_completed(other as *mut std::ffi::c_void);
        });
        // This is the call that used to abort after 1M yield iterations if a
        // spawned task took slightly longer than the fixed spin budget.
        mimi_await_future(f);
        assert_eq!(mimi_future_is_completed(f), 1);
        mimi_future_free(f);
        completer.join().expect("completer thread panicked");
    }
    #[test]
    fn bounded_pool_nested_spawn_does_not_deadlock() {
        unsafe extern "C" fn child_poll(fut: *mut std::ffi::c_void) {
            mimi_future_set_completed(fut);
        }
        unsafe extern "C" fn nested_poll(fut: *mut std::ffi::c_void) {
            let child = mimi_future_alloc(0);
            assert!(!child.is_null());
            mimi_spawn_future(child, child_poll);
            mimi_await_future(child);
            mimi_future_free(child);
            mimi_future_set_completed(fut);
        }
        let parent = mimi_future_alloc(0);
        assert!(!parent.is_null());
        mimi_spawn_future(parent, nested_poll);
        mimi_await_future(parent);
        mimi_future_free(parent);
    }
}
