//! SD-4: Signal guard for FFI crash recovery.
//!
//! Replaces fork()-based isolation (POSIX UB in multi-threaded contexts)
//! with in-process SIGSEGV/SIGABRT/SIGBUS/SIGILL/SIGFPE signal handlers
//! and `sigsetjmp`/`siglongjmp` recovery points.
//!
//! # Architecture
//!
//! ```text
//! call_ffi_guarded(cif, code_ptr, args, ret)
//!   ├── install signal handlers (sigaction)
//!   ├── sigsetjmp(recovery_point)
//!   │   ├── 0: call C function via libffi → Ok(result)
//!   │   └── sig: signal caught → Err("caught signal {sig}")
//!   └── restore signal handlers
//! ```
//!
//! # Safety
//!
//! - Signal handlers are process-wide (not thread-local). Concurrent FFI
//!   calls from multiple threads share the same handlers. This is acceptable
//!   because: (a) the handler only siglongjmps if a recovery point is set,
//!   (b) recovery points are thread-local, (c) the handler restores SIG_DFL
//!   before longjmping (preventing infinite signal loops).
//! - `siglongjmp` from a signal handler is async-signal-safe (POSIX).
//! - The C function called via libffi must not hold locks that would be
//!   leaked by the longjmp. This is the same limitation as the archived
//!   C runtime's `#[no_panic]` implementation.

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicI64, AtomicPtr, Ordering};
use std::sync::Mutex;

/// Global mutex serializing signal guard usage. Signal handlers are
/// process-wide (not thread-local), so concurrent `call_guarded()` calls
/// would overwrite each other's handlers. The mutex prevents this race.
/// Overhead is negligible: FFI calls are orders of magnitude slower.
static GUARD_LOCK: Mutex<()> = Mutex::new(());

/// sigjmp_buf is 200 bytes on x86_64 Linux (glibc).
/// Use a generous 1024-byte buffer for alignment/headroom across the
/// supported Linux/glibc targets. This is still a hardcoded fallback; a
/// future port should replace it with `size_of::<libc::sigjmp_buf>()` when
/// the libc crate exposes that type (batch5 P1-5).
const JMP_BUF_SIZE: usize = 1024;
// Keep the scratch buffer comfortably above the largest known sigjmp_buf
// size (200 bytes on x86_64 glibc) so accidental regressions fail at build.
const _: () = assert!(JMP_BUF_SIZE >= 512);

extern "C" {
    /// glibc internal entry point for sigsetjmp (the macro expands to this).
    /// Returns 0 on initial call, non-zero (the signal number) on longjmp.
    fn __sigsetjmp(env: *mut c_void, savesigs: i32) -> i32;
    /// Jump back to the sigsetjmp recovery point with the given value.
    /// Never returns.
    fn siglongjmp(env: *mut c_void, val: i32) -> !;
}

// Re-entrancy guard: true while inside call_guarded on this thread.
// Prevents deadlock when a C function calls back into Mimi → FFI.
thread_local! {
    static IN_GUARDED_CALL: Cell<bool> = const { Cell::new(false) };
}

/// Recovery buffer for sigjmp_buf. Only one guarded FFI call can be active
/// process-wide because GUARD_LOCK serializes entry; the buffer is therefore
/// global rather than thread-local. The signal handler reads the matching
/// RECOVERY_PTR/GUARDED_TID atomics instead of touching TLS, avoiding the
/// previous async-signal-unsafe TLS access.
static mut JMP_BUF: [u8; JMP_BUF_SIZE] = [0u8; JMP_BUF_SIZE];

/// Process-wide recovery pointer and owning thread id. These are written
/// only by the single thread that holds GUARD_LOCK and read by the signal
/// handler. A crashed thread only longjmps if it is the thread that armed
/// the guard; other threads (e.g. threads spawned by the C callee) re-raise
/// the signal with the default disposition.
static RECOVERY_PTR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static GUARDED_TID: AtomicI64 = AtomicI64::new(0);

#[cfg(target_os = "linux")]
unsafe fn current_tid() -> i64 {
    // SAFETY: syscall(SYS_gettid) is async-signal-safe and does not allocate.
    unsafe { libc::syscall(libc::SYS_gettid) as i64 }
}

#[cfg(not(target_os = "linux"))]
unsafe fn current_tid() -> i64 {
    // Non-Linux fallback: treat the whole process as one guarded "thread".
    // This is conservative; the signal guard is primarily a Linux/glibc
    // feature and this keeps the handler safe to compile elsewhere.
    0
}

/// Signal handler: restores SIG_DFL for all guarded signals, then
/// siglongjmps to the thread-local recovery point if one is set.
///
/// # Async-signal-safety
/// `signal()` and `siglongjmp()` are async-signal-safe (POSIX.1-2017).
/// Thread-local access via `RECOVERY_BUF` is technically UB in a signal
/// handler, but works in practice on Linux/glibc (thread-local storage
/// is a simple memory read, no locks involved).
extern "C" fn crash_handler(sig: libc::c_int) {
    // Restore default handlers FIRST to prevent infinite signal loops.
    // SAFETY: signal() with SIG_DFL is async-signal-safe.
    unsafe {
        libc::signal(libc::SIGSEGV, libc::SIG_DFL);
        libc::signal(libc::SIGABRT, libc::SIG_DFL);
        libc::signal(libc::SIGBUS, libc::SIG_DFL);
        libc::signal(libc::SIGILL, libc::SIG_DFL);
        libc::signal(libc::SIGFPE, libc::SIG_DFL);
    }
    let ptr = RECOVERY_PTR.load(Ordering::Relaxed);
    // SAFETY: current_tid() is an async-signal-safe syscall wrapper.
    let tid = unsafe { current_tid() };
    if !ptr.is_null() && GUARDED_TID.load(Ordering::Relaxed) == tid {
        // SAFETY: ptr points to a valid sigjmp_buf set by sigsetjmp
        // in call_ffi_guarded, and only the arming thread is allowed to
        // longjmp. siglongjmp is async-signal-safe.
        unsafe { siglongjmp(ptr, sig) };
    }
    // No matching recovery point (or crash came from another thread):
    // re-raise with default handler (terminates process).
    // SAFETY: raise() is async-signal-safe.
    unsafe {
        libc::raise(sig);
    }
}

/// Saved signal handlers for restoration after guarded call.
struct SavedHandlers {
    sigsegv: libc::sighandler_t,
    sigabrt: libc::sighandler_t,
    sigbus: libc::sighandler_t,
    sigill: libc::sighandler_t,
    sigfpe: libc::sighandler_t,
}

// libc::signal() requires fn-to-integer cast; this is the standard C API pattern.
#[allow(unknown_lints, function_casts_as_integer)]
fn install_handlers() -> SavedHandlers {
    // SAFETY: signal() returns the previous handler. crash_handler is a
    // valid extern "C" fn with the correct signature.
    // Clippy: fn-to-integer cast is required for libc::signal() API.
    unsafe {
        SavedHandlers {
            sigsegv: libc::signal(libc::SIGSEGV, crash_handler as libc::sighandler_t),
            sigabrt: libc::signal(libc::SIGABRT, crash_handler as libc::sighandler_t),
            sigbus: libc::signal(libc::SIGBUS, crash_handler as libc::sighandler_t),
            sigill: libc::signal(libc::SIGILL, crash_handler as libc::sighandler_t),
            sigfpe: libc::signal(libc::SIGFPE, crash_handler as libc::sighandler_t),
        }
    }
}

fn restore_handlers(saved: &SavedHandlers) {
    // SAFETY: restoring previously saved handlers.
    unsafe {
        libc::signal(libc::SIGSEGV, saved.sigsegv);
        libc::signal(libc::SIGABRT, saved.sigabrt);
        libc::signal(libc::SIGBUS, saved.sigbus);
        libc::signal(libc::SIGILL, saved.sigill);
        libc::signal(libc::SIGFPE, saved.sigfpe);
    }
}

/// Call a closure with SIGSEGV/SIGABRT/SIGBUS/SIGILL/SIGFPE crash protection.
///
/// If the closure triggers a fatal signal, returns `Err` with the signal name.
/// If the closure completes normally, returns `Ok(result)`.
///
/// # Safety
///
/// This function is `unsafe` because `siglongjmp` from a signal handler
/// crosses ordinary Rust stack frames, and the signal handler reads a
/// thread-local recovery pointer. That recovery mechanism is inherently
/// not guaranteed by Rust's memory model; it is provided only as the
/// low-level FFI crash guard. Callers must ensure:
///
/// - the closure only contains FFI calls and does not rely on Rust-scoped
///   destructors running when a catchable signal is delivered;
/// - resources acquired by the closure that outlive a caught crash are
///   treated as leaked;
/// - only one guarded FFI call is active per thread at a time (the same
///   thread re-entrant path is supported);
/// - the host platform supports this POSIX signal/longjmp recovery approach
///   (primary target: Linux/glibc).
///
/// # Limitations
/// - Process-wide signal handlers: concurrent guarded calls from multiple
///   threads are serialized by GUARD_LOCK.
/// - Resources acquired by the closure before the crash are leaked.
///
/// # Re-entrancy
/// If the closure calls back into `call_guarded` (e.g. C callback → Mimi → FFI),
/// the inner call runs WITHOUT signal protection (returns the closure result
/// directly). This avoids deadlock on GUARD_LOCK and is acceptable because
/// the outer call already has a recovery point active. A panic in the inner
/// (re-entrant) closure unwinds into the OUTER call's `catch_unwind`, which
/// performs the state cleanup described below.
///
/// # Panic safety (2026-08-05 audit fix)
/// The closure invocation is wrapped in `std::panic::catch_unwind`. If the
/// closure panics (a Rust unwind, not a C signal), unwinding straight up
/// would skip the cleanup below and leave the process permanently poisoned:
/// `crash_handler` still installed, `RECOVERY_BUF` pointing at a dead stack
/// frame, and `IN_GUARDED_CALL` stuck true — the next SIGSEGV would then
/// `siglongjmp` into dead stack. On unwind we therefore: null out
/// `RECOVERY_BUF` (so the handler's fast path cannot reuse it; the next
/// guarded call must re-establish it), clear `IN_GUARDED_CALL`, restore the
/// saved signal handlers, and then resume the unwind via
/// `std::panic::resume_unwind` (the caller observes the original panic).
pub(crate) unsafe fn call_guarded<F, R>(f: F) -> Result<R, String>
where
    F: FnOnce() -> R,
{
    // Re-entrancy check: if we're already inside a guarded call on this
    // thread, skip the lock and signal setup. The outer call's recovery
    // point is still active, so crashes in the inner call are caught.
    let is_reentrant = IN_GUARDED_CALL.with(|flag| flag.get());
    if is_reentrant {
        return Ok(f());
    }

    // Serialize signal guard usage: handlers are process-wide.
    let _lock = GUARD_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // Mark this thread as inside a guarded call.
    IN_GUARDED_CALL.with(|flag| flag.set(true));

    let saved = install_handlers();

    // Set up the global jmp_buf and recovery point. GUARD_LOCK guarantees
    // only one thread is inside a guarded call at a time; the buffer is
    // global, and the owning thread id is recorded for the signal handler.
    // SAFETY: JMP_BUF is only accessed under GUARD_LOCK.
    let buf_ptr = std::ptr::addr_of_mut!(JMP_BUF).cast::<c_void>();
    // SAFETY: current_tid() is an async-signal-safe syscall wrapper.
    let tid = unsafe { current_tid() };
    RECOVERY_PTR.store(buf_ptr, Ordering::Relaxed);
    GUARDED_TID.store(tid, Ordering::Relaxed);

    // SAFETY: buf_ptr points to a valid 1024-byte buffer (sigjmp_buf
    // is 200 bytes on x86_64). savesigs=1 saves the signal mask.
    let sig = unsafe { __sigsetjmp(buf_ptr, 1) };

    let result = if sig == 0 {
        // Initial call: execute the protected closure.
        //
        // Audit fix (signal_guard.rs:149-208): intercept Rust
        // panics. A bare unwind would skip the cleanup after
        // JMP_BUF.with(..) and leave crash_handler installed with
        // RECOVERY_BUF pointing at this dying frame and
        // IN_GUARDED_CALL stuck true — the next SIGSEGV would
        // siglongjmp into dead stack. AssertUnwindSafe: the closure
        // borrows nothing whose invariants we must re-check here;
        // any state it touched is leaked per the documented
        // "resources acquired before the crash are leaked"
        // limitation, same as the signal (longjmp) path.
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
            Ok(v) => Ok(v),
            Err(payload) => {
                // Neutralize the guard state BEFORE resuming the
                // unwind so the signal handler can never longjmp
                // into this frame once it unwinds.
                RECOVERY_PTR.store(std::ptr::null_mut(), Ordering::Relaxed);
                GUARDED_TID.store(0, Ordering::Relaxed);
                IN_GUARDED_CALL.with(|flag| flag.set(false));
                restore_handlers(&saved);
                std::panic::resume_unwind(payload)
            }
        }
    } else {
        // Signal caught: siglongjmp returned here with the signal number.
        let sig_name = match sig {
            6 => "SIGABRT",
            7 => "SIGBUS",
            8 => "SIGFPE",
            11 => "SIGSEGV",
            4 => "SIGILL",
            _ => "UNKNOWN",
        };
        Err(format!(
            "FFI safety: C function crashed with {} (signal {})",
            sig_name, sig
        ))
    };

    // Clear recovery point, re-entrancy flag, and restore handlers.
    RECOVERY_PTR.store(std::ptr::null_mut(), Ordering::Relaxed);
    GUARDED_TID.store(0, Ordering::Relaxed);
    IN_GUARDED_CALL.with(|flag| flag.set(false));
    restore_handlers(&saved);

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Safe test-only wrapper around the intentionally unsafe crash guard.
    fn guarded<F, R>(f: F) -> Result<R, String>
    where
        F: FnOnce() -> R,
    {
        // SAFETY: test-only call sites only run the signal-guard scenarios
        // described in each test, satisfying the FFI crash-recovery contract.
        unsafe { call_guarded(f) }
    }

    #[test]
    fn signal_guard_normal_call_succeeds() {
        let result = guarded(|| 42);
        assert_eq!(result, Ok(42));
    }

    #[test]
    fn signal_guard_catches_sigsegv() {
        let result = guarded(|| -> i32 {
            // SAFETY: intentionally dereference null to trigger SIGSEGV.
            unsafe { std::ptr::read_volatile(std::ptr::null::<i32>()) }
        });
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("SIGSEGV"),
            "error should mention SIGSEGV: {}",
            err
        );
    }

    #[test]
    fn signal_guard_catches_sigabrt() {
        let result = guarded(|| -> i32 {
            // SAFETY: intentionally abort to trigger SIGABRT.
            unsafe { libc::abort() }
        });
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("SIGABRT"),
            "error should mention SIGABRT: {}",
            err
        );
    }

    #[test]
    fn signal_guard_restores_handlers_after_crash() {
        // After a crash, the default handlers should be restored.
        let _ =
            // SAFETY: this deliberately reads from a null pointer to produce a
            // SIGSEGV that tests the signal guard recovery mechanism.
            guarded(|| -> i32 { unsafe { std::ptr::read_volatile(std::ptr::null::<i32>()) } });
        // If handlers weren't restored, this second call would also be caught.
        // But since crash_handler restores SIG_DFL, a second crash would
        // terminate the process. We verify by checking that guarded
        // still works for normal calls.
        let result = guarded(|| 99);
        assert_eq!(result, Ok(99));
    }

    #[test]
    fn signal_guard_string_result() {
        let result = guarded(|| "hello".to_string());
        assert_eq!(result, Ok("hello".to_string()));
    }

    #[test]
    fn signal_guard_reentrant_call_does_not_deadlock() {
        // Simulates C callback → Mimi → FFI re-entrancy.
        // The inner guarded should NOT deadlock on GUARD_LOCK.
        let result = guarded(|| {
            // Inner call: re-entrant, should skip lock and run directly.
            let inner = guarded(|| 42);
            inner.unwrap() + 1
        });
        assert_eq!(result, Ok(43));
    }

    #[test]
    fn signal_guard_reentrant_crash_caught_by_outer() {
        // If the inner (re-entrant) call crashes, the outer recovery
        // point catches it (inner has no separate recovery point).
        let result = guarded(|| -> i32 {
            let inner = guarded(|| -> i32 {
                // SAFETY: deliberately crashes to test signal guard reentrancy
                unsafe { std::ptr::read_volatile(std::ptr::null::<i32>()) }
            });
            // inner crash is caught by outer's recovery point,
            // so we never reach here.
            inner.unwrap()
        });
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("SIGSEGV"));
    }

    #[test]
    fn signal_guard_panic_leaves_clean_state() {
        // 2026-08-05 audit fix: a panic inside guarded must not poison
        // the process-wide signal state. Before the fix the unwind skipped
        // cleanup, leaving IN_GUARDED_CALL stuck true and RECOVERY_BUF
        // dangling at a dead frame — the next SIGSEGV would siglongjmp into
        // dead stack.
        let r = std::panic::catch_unwind(|| {
            let _ = guarded(|| -> i32 { panic!("deliberate audit test panic") });
        });
        assert!(r.is_err(), "the panic must propagate to the caller");

        // Guard state must be clean on this thread after the unwind.
        IN_GUARDED_CALL.with(|flag| assert!(!flag.get()));
        assert!(RECOVERY_PTR.load(Ordering::Relaxed).is_null());
        assert_eq!(GUARDED_TID.load(Ordering::Relaxed), 0);

        // And a subsequent guarded call must still catch real crashes.
        // (With IN_GUARDED_CALL stuck true this would run unprotected and
        // kill the test process instead of returning Err.)
        let result = guarded(|| -> i32 {
            // SAFETY: deliberate null dereference to verify the signal
            // guard still recovers after a panicked guarded call.
            unsafe { std::ptr::read_volatile(std::ptr::null::<i32>()) }
        });
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("SIGSEGV"));
    }

    #[test]
    fn signal_guard_reentrant_panic_cleaned_by_outer() {
        // A panic in a re-entrant (inner) closure unwinds into the outer
        // call's catch_unwind, which performs the cleanup.
        let r = std::panic::catch_unwind(|| {
            let _ = guarded(|| -> i32 {
                let _inner = guarded(|| -> i32 { panic!("inner panic") });
                0
            });
        });
        assert!(r.is_err());
        IN_GUARDED_CALL.with(|flag| assert!(!flag.get()));
        assert!(RECOVERY_PTR.load(Ordering::Relaxed).is_null());
        assert_eq!(GUARDED_TID.load(Ordering::Relaxed), 0);
        // Sanity: guard still usable afterwards.
        assert_eq!(guarded(|| 7), Ok(7));
    }
}
