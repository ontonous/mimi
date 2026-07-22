//! Mimi runtime concurrency primitives — Mutex / atomic (i32/i64/bool) /
//! Channel / Session, implemented in Rust via `std::sync` with a global
//! handle table (`CONCURRENCY_HANDLES`).
//!
//! Extracted verbatim from `runtime/mod.rs` (the `v0.28.20 Concurrency
//! primitives` section) during the 0.1.0 mechanical split (behavior
//! bit-exact). Self-contained: `ConcurrencyHandleTable` / `ConcurrencyAtomic`
//! / `ConcurrencyMutex` / `ConcurrencyChannel` / `HeldMutexGuard` and the
//! `CONCURRENCY_HANDLES` static all defined within. Pure `extern "C"` leaf
//! (no crate-level Rust-path callers, no top-level handle-registry deps).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

// =========================================================================
// v0.28.20 — Concurrency primitives (Mutex / atomic / Channel)
//
// All three primitives are implemented entirely in Rust using std::sync.
// They follow the existing handle-as-i64 convention used by Set/Map/Record
// so the interpreter (Value::Int handle) and codegen (i64 runtime fn) paths
// stay symmetric. Each primitive exposes:
//   * `_new` constructor returning an opaque i64 handle,
//   * methods that take the handle and return i64 (mimics Rust's ordering),
//   * `_drop` destructor that the codegen cleanup pass emits on scope exit.
//
// SAFETY invariants are identical to the actor mailbox above: handles are
// `Box`-allocated and recovered by handle id with a global mutex-protected
// table. All public functions are `#[no_mangle] pub extern "C"` and
// null-checked.
// =========================================================================

/// Global concurrency-primitive table. LazyLock because the table
/// contains a non-`const` HashMap. The `incompatible_msrv` allow mirrors
/// `src/ffi/runtime.rs`'s `MIMI_POOL` static — the project runtime
/// requires 1.80+ features regardless of the lib `rust-version` pin.
#[allow(clippy::incompatible_msrv)]
static CONCURRENCY_HANDLES: std::sync::LazyLock<std::sync::Mutex<ConcurrencyHandleTable>> =
    std::sync::LazyLock::new(|| {
        std::sync::Mutex::new(ConcurrencyHandleTable {
            next_id: 1,
            atomics: HashMap::new(),
            mutexes: HashMap::new(),
            channels: HashMap::new(),
        })
    });

/// Concurrency primitive handle table. Each variant key carries a boxed
/// primitive; `take_by_*` helpers retrieve + remove the handle for drop.
/// Once removed, any subsequent use returns a null/error sentinel.
struct ConcurrencyHandleTable {
    next_id: u64,
    atomics: HashMap<u64, ConcurrencyAtomic>,
    mutexes: HashMap<u64, ConcurrencyMutex>,
    channels: HashMap<u64, ConcurrencyChannel>,
}

enum ConcurrencyAtomic {
    I32(std::sync::atomic::AtomicI32),
    I64(std::sync::atomic::AtomicI64),
    Bool(std::sync::atomic::AtomicBool),
}

/// Per-primitive Mutex storage. The `Arc` gives the inner `Mutex` a stable
/// address and keeps it alive even if the handle is dropped while a guard
/// is still held (defensive against user error). Guards are stored in
/// `MIMI_MUTEX_GUARDS` and keep an `Arc` clone so the lifetime extension to
/// `'static` used in `HeldMutexGuard` is sound.
struct ConcurrencyMutex {
    inner: Arc<std::sync::Mutex<i64>>,
}

/// A held mutex guard. The `_arc` clone keeps the `Mutex` alive for the
/// guard's lifetime; the `guard` lifetime is extended to `'static` via
/// transmute because the Arc guarantees the Mutex is never deallocated
/// while the guard exists. The guard is stored in thread-local storage
/// (single-thread access) until `mimi_mutex_unlock` removes it.
///
/// R-C10: field order matters — Rust drops fields in declaration order, so
/// `guard` must be declared before `_arc` so unlock runs before Arc drop.
struct HeldMutexGuard {
    guard: std::sync::MutexGuard<'static, i64>,
    _arc: Arc<std::sync::Mutex<i64>>,
}

thread_local! {
    static MIMI_MUTEX_GUARDS: std::cell::RefCell<HashMap<u64, HeldMutexGuard>> =
        std::cell::RefCell::new(HashMap::new());
}
static MIMI_MUTEX_GUARD_NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Bounded mpsc channel of i64 values. Constructed via `mimi_channel_new`.
/// `send` pushes; `recv`/`try_recv` pops; `drop` closes both endpoints.
/// The receiver is wrapped in `Arc<Mutex<Option<Receiver>>>` so that a
/// blocking `recv` can be performed without holding the global handle table
/// lock.
struct ConcurrencyChannel {
    tx: std::sync::mpsc::Sender<i64>,
    rx: Arc<Mutex<Option<std::sync::mpsc::Receiver<i64>>>>,
}

fn alloc_atomic(a: ConcurrencyAtomic) -> i64 {
    let mut table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let id = table.next_id;
    table.next_id += 1;
    table.atomics.insert(id, a);
    id as i64
}

fn alloc_mutex(m: ConcurrencyMutex) -> i64 {
    let mut table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let id = table.next_id;
    table.next_id += 1;
    table.mutexes.insert(id, m);
    id as i64
}

fn alloc_channel(c: ConcurrencyChannel) -> i64 {
    let mut table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let id = table.next_id;
    table.next_id += 1;
    table.channels.insert(id, c);
    id as i64
}

// ---------- AtomicI32 ----------

#[no_mangle]
pub extern "C" fn mimi_atomic_i32_new(value: i32) -> i64 {
    alloc_atomic(ConcurrencyAtomic::I32(std::sync::atomic::AtomicI32::new(
        value,
    )))
}

#[no_mangle]
pub extern "C" fn mimi_atomic_i32_load(handle: i64) -> i32 {
    let table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match table.atomics.get(&(handle as u64)) {
        Some(ConcurrencyAtomic::I32(a)) => a.load(std::sync::atomic::Ordering::SeqCst),
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn mimi_atomic_i32_store(handle: i64, value: i32) {
    let table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(ConcurrencyAtomic::I32(a)) = table.atomics.get(&(handle as u64)) {
        a.store(value, std::sync::atomic::Ordering::SeqCst);
    }
}

#[no_mangle]
pub extern "C" fn mimi_atomic_i32_fetch_add(handle: i64, delta: i32) -> i32 {
    let table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match table.atomics.get(&(handle as u64)) {
        Some(ConcurrencyAtomic::I32(a)) => a.fetch_add(delta, std::sync::atomic::Ordering::SeqCst),
        _ => 0,
    }
}

/// Compare-and-swap: returns 1 on success, 0 on mismatch. Codegen also
/// reads back the value via `mimi_atomic_i32_load` after failure.
#[no_mangle]
pub extern "C" fn mimi_atomic_i32_compare_exchange(
    handle: i64,
    expected: i32,
    new_value: i32,
) -> i32 {
    let table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match table.atomics.get(&(handle as u64)) {
        Some(ConcurrencyAtomic::I32(a)) => match a.compare_exchange(
            expected,
            new_value,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        ) {
            Ok(_) => 1,
            Err(_) => 0,
        },
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn mimi_atomic_i32_drop(handle: i64) {
    let mut table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    table.atomics.remove(&(handle as u64));
}

// ---------- AtomicI64 ----------

#[no_mangle]
pub extern "C" fn mimi_atomic_i64_new(value: i64) -> i64 {
    alloc_atomic(ConcurrencyAtomic::I64(std::sync::atomic::AtomicI64::new(
        value,
    )))
}

#[no_mangle]
pub extern "C" fn mimi_atomic_i64_load(handle: i64) -> i64 {
    let table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match table.atomics.get(&(handle as u64)) {
        Some(ConcurrencyAtomic::I64(a)) => a.load(std::sync::atomic::Ordering::SeqCst),
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn mimi_atomic_i64_store(handle: i64, value: i64) {
    let table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(ConcurrencyAtomic::I64(a)) = table.atomics.get(&(handle as u64)) {
        a.store(value, std::sync::atomic::Ordering::SeqCst);
    }
}

#[no_mangle]
pub extern "C" fn mimi_atomic_i64_fetch_add(handle: i64, delta: i64) -> i64 {
    let table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match table.atomics.get(&(handle as u64)) {
        Some(ConcurrencyAtomic::I64(a)) => a.fetch_add(delta, std::sync::atomic::Ordering::SeqCst),
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn mimi_atomic_i64_drop(handle: i64) {
    let mut table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    table.atomics.remove(&(handle as u64));
}

// ---------- AtomicBool ----------

#[no_mangle]
pub extern "C" fn mimi_atomic_bool_new(value: i32) -> i64 {
    let b = value != 0;
    alloc_atomic(ConcurrencyAtomic::Bool(std::sync::atomic::AtomicBool::new(
        b,
    )))
}

#[no_mangle]
pub extern "C" fn mimi_atomic_bool_load(handle: i64) -> i32 {
    let table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match table.atomics.get(&(handle as u64)) {
        Some(ConcurrencyAtomic::Bool(a)) => a.load(std::sync::atomic::Ordering::SeqCst) as i32,
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn mimi_atomic_bool_store(handle: i64, value: i32) {
    let b = value != 0;
    let table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(ConcurrencyAtomic::Bool(a)) = table.atomics.get(&(handle as u64)) {
        a.store(b, std::sync::atomic::Ordering::SeqCst);
    }
}

#[no_mangle]
pub extern "C" fn mimi_atomic_bool_drop(handle: i64) {
    let mut table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    table.atomics.remove(&(handle as u64));
}

// ---------- Mutex<i64> ----------

#[no_mangle]
pub extern "C" fn mimi_mutex_new(value: i64) -> i64 {
    alloc_mutex(ConcurrencyMutex {
        inner: Arc::new(std::sync::Mutex::new(value)),
    })
}

/// Lock the mutex and return a separate guard-handle id. The guard handle
/// must be passed to `mimi_mutex_get`/`set` to read/write the held value, and
/// to `mimi_mutex_unlock` to release the lock. The lock is held continuously
/// between lock/get/set/unlock, providing real mutual exclusion across threads.
#[no_mangle]
pub extern "C" fn mimi_mutex_lock(handle: i64) -> i64 {
    let arc = {
        let table = CONCURRENCY_HANDLES
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match table.mutexes.get(&(handle as u64)) {
            Some(m) => Arc::clone(&m.inner),
            _ => return 0,
        }
    };
    // Drop the global table lock before blocking on the mutex.
    let guard = arc.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: Lifetime extension via transmute is sound because:
    //   1. `arc` (Arc clone) is stored alongside in HeldMutexGuard._arc,
    //      keeping the underlying Mutex alive for the guard's entire lifetime.
    //   2. The guard is stored in thread-local storage (MIMI_MUTEX_GUARDS),
    //      ensuring single-thread access — no cross-thread aliasing.
    //   3. `mimi_mutex_unlock` drops the guard before the Arc is dropped,
    //      guaranteeing the guard never outlives the Mutex.
    //   4. The Arc's strong count guarantees the Mutex memory is never freed
    //      while any guard exists.
    // This avoids the type-system limitation where MutexGuard's lifetime is
    // syntactically tied to the stack frame, not the Arc's heap lifetime.
    let guard: std::sync::MutexGuard<'static, i64> = unsafe { std::mem::transmute(guard) };
    let held = HeldMutexGuard { guard, _arc: arc };
    let id = MIMI_MUTEX_GUARD_NEXT_ID.fetch_add(1, Ordering::SeqCst);
    MIMI_MUTEX_GUARDS.with(|guards| {
        guards.borrow_mut().insert(id, held);
    });
    id as i64
}

#[no_mangle]
pub extern "C" fn mimi_mutex_get(guard_handle: i64) -> i64 {
    MIMI_MUTEX_GUARDS.with(|guards| {
        guards
            .borrow()
            .get(&(guard_handle as u64))
            .map(|held| *held.guard)
            .unwrap_or(0)
    })
}

#[no_mangle]
pub extern "C" fn mimi_mutex_set(guard_handle: i64, value: i64) {
    MIMI_MUTEX_GUARDS.with(|guards| {
        if let Some(held) = guards.borrow_mut().get_mut(&(guard_handle as u64)) {
            *held.guard = value;
        }
    });
}

#[no_mangle]
pub extern "C" fn mimi_mutex_unlock(guard_handle: i64) {
    MIMI_MUTEX_GUARDS.with(|guards| {
        // Removing the entry drops the `MutexGuard`, releasing the OS lock.
        guards.borrow_mut().remove(&(guard_handle as u64));
    });
}

#[no_mangle]
pub extern "C" fn mimi_mutex_drop(handle: i64) {
    let mut table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    table.mutexes.remove(&(handle as u64));
    // M3 note: existing HeldMutexGuard entries in TLS still hold Arc clones
    // of the Mutex, so the underlying data stays alive. However, after drop(),
    // no new lock() calls can acquire this mutex. Existing guards will still
    // work (get/set/unlock) until dropped via mimi_mutex_unlock — they reference
    // the old Mutex via their Arc clone.
}

// ---------- Channel<i64> (mpsc, unbounded) ----------

#[no_mangle]
pub extern "C" fn mimi_channel_new() -> i64 {
    let (tx, rx) = std::sync::mpsc::channel::<i64>();
    alloc_channel(ConcurrencyChannel {
        tx,
        rx: Arc::new(Mutex::new(Some(rx))),
    })
}

#[no_mangle]
pub extern "C" fn mimi_channel_send(handle: i64, value: i64) {
    let tx = {
        let table = CONCURRENCY_HANDLES
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        table.channels.get(&(handle as u64)).map(|ch| ch.tx.clone())
    };
    if let Some(tx) = tx {
        let _ = tx.send(value);
    }
}

#[no_mangle]
pub extern "C" fn mimi_channel_recv(handle: i64) -> i64 {
    // Look up the channel under the global lock, then clone the receiver Arc
    // and drop the global lock *before* blocking on recv(). This prevents a
    // receiver from stalling all other concurrency-handle operations.
    let rx_arc = {
        let table = CONCURRENCY_HANDLES
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match table.channels.get(&(handle as u64)) {
            Some(ch) => Arc::clone(&ch.rx),
            _ => return 0,
        }
    };
    // Take the receiver out under the mutex lock, then drop the lock before
    // blocking on recv(). This prevents a deadlock with mimi_channel_drop,
    // which needs the same mutex to set the receiver slot to None.
    let rx = rx_arc.lock().unwrap_or_else(|e| e.into_inner()).take();
    // MutexGuard is dropped here; the mutex is now free.
    match rx {
        Some(rx) => {
            // H12-fix: log channel disconnect instead of silently returning 0.
            // unwrap_or_default() returns 0 for i64 when the channel is closed,
            // which is indistinguishable from a legitimate 0 value.
            let result = rx.recv().unwrap_or_else(|e| {
                eprintln!("[mimi runtime] channel recv: channel disconnected: {}", e);
                0
            });
            // Re-acquire the mutex and put the receiver back only if the
            // channel still exists in the global table (i.e. mimi_channel_drop
            // has not been called while we were blocked).
            let still_alive = CONCURRENCY_HANDLES
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .channels
                .contains_key(&(handle as u64));
            if still_alive {
                *rx_arc.lock().unwrap_or_else(|e| e.into_inner()) = Some(rx);
            }
            // If the channel was dropped, `rx` is dropped here.
            result
        }
        None => 0,
    }
}

/// Non-blocking receive. Returns `value` on success, or `-1` if no value is
/// currently available (channel still open, queue empty).
#[no_mangle]
pub extern "C" fn mimi_channel_try_recv(handle: i64) -> i64 {
    let rx_arc = {
        let table = CONCURRENCY_HANDLES
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match table.channels.get(&(handle as u64)) {
            Some(ch) => Arc::clone(&ch.rx),
            _ => return -1,
        }
    };
    // Take the receiver out, try_recv (which is non-blocking), then put back.
    let rx = rx_arc.lock().unwrap_or_else(|e| e.into_inner()).take();
    match rx {
        Some(rx) => {
            let result = rx.try_recv().unwrap_or(-1);
            let still_alive = CONCURRENCY_HANDLES
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .channels
                .contains_key(&(handle as u64));
            if still_alive {
                *rx_arc.lock().unwrap_or_else(|e| e.into_inner()) = Some(rx);
            }
            // If the channel was dropped, `rx` is dropped here.
            result
        }
        None => -1,
    }
}

#[no_mangle]
pub extern "C" fn mimi_channel_drop(handle: i64) {
    // CRITICAL #15: TOCTOU race analysis:
    // 1. mimi_channel_recv takes the Receiver out of the Arc<Mutex<Option<_>>>
    //    and releases the mutex before calling recv().
    // 2. mimi_channel_drop removes the channel from the handle table (which
    //    drops the tx sender), then sets the receiver slot to None.
    // 3. The blocked recv() in step 1 unblocks when tx is dropped (step 2),
    //    returning Err (disconnected), which recv() handles via unwrap_or_else.
    // 4. After recv() returns, still_alive check prevents putting the receiver
    //    back into a dropped channel.
    //
    // This is safe because: the tx drop unblocks any pending recv, and the
    // receiver is either put back (if channel still alive) or dropped (if not).
    let rx_arc = {
        let mut table = CONCURRENCY_HANDLES
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match table.channels.remove(&(handle as u64)) {
            Some(ch) => ch.rx,
            _ => return,
        }
    };
    // Drop the receiver outside the global handle table lock so that any
    // pending `recv` unblocks promptly without needing the global lock.
    // The ConcurrencyChannel drop (from table.channels.remove above) also
    // drops tx, which unblocks any pending recv() on the taken-out receiver.
    *rx_arc.lock().unwrap_or_else(|e| e.into_inner()) = None;
}
#[no_mangle]
pub extern "C" fn mimi_session_pair() -> i64 {
    let pair1 = std::sync::mpsc::channel::<i64>();
    let pair2 = std::sync::mpsc::channel::<i64>();
    // Cross-wire: A sends to B's rx, B sends to A's rx
    let ha = alloc_channel(ConcurrencyChannel {
        tx: pair1.0,
        rx: std::sync::Arc::new(std::sync::Mutex::new(Some(pair2.1))),
    }) as u64;
    let hb = alloc_channel(ConcurrencyChannel {
        tx: pair2.0,
        rx: std::sync::Arc::new(std::sync::Mutex::new(Some(pair1.1))),
    }) as u64;
    ((hb << 32) | (ha & 0xFFFF_FFFFu64)) as i64
}
#[no_mangle]
pub extern "C" fn mimi_session_lo(pair: i64) -> i64 {
    (pair as u64 & 0xFFFF_FFFFu64) as i64
}
#[no_mangle]
pub extern "C" fn mimi_session_hi(pair: i64) -> i64 {
    ((pair as u64) >> 32) as i64
}
