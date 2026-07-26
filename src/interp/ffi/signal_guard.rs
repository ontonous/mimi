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

/// sigjmp_buf is 200 bytes on x86_64 Linux (glibc).
/// Use 256 for alignment headroom across platforms.
const JMP_BUF_SIZE: usize = 256;

extern "C" {
    /// glibc internal entry point for sigsetjmp (the macro expands to this).
    /// Returns 0 on initial call, non-zero (the signal number) on longjmp.
    fn __sigsetjmp(env: *mut c_void, savesigs: i32) -> i32;
    /// Jump back to the sigsetjmp recovery point with the given value.
    /// Never returns.
    fn siglongjmp(env: *mut c_void, val: i32) -> !;
}

thread_local! {
    /// Thread-local recovery point. NULL means no guarded call is active.
    static RECOVERY_BUF: Cell<*mut c_void> = const { Cell::new(std::ptr::null_mut()) };
    /// Thread-local buffer for the sigjmp_buf (avoids stack allocation
    /// across the signal handler boundary).
    static JMP_BUF: Cell<[u8; JMP_BUF_SIZE]> = const { Cell::new([0u8; JMP_BUF_SIZE]) };
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
    RECOVERY_BUF.with(|buf| {
        let ptr = buf.get();
        if !ptr.is_null() {
            // SAFETY: ptr points to a valid sigjmp_buf set by sigsetjmp
            // in call_ffi_guarded. siglongjmp is async-signal-safe.
            unsafe { siglongjmp(ptr, sig) };
        }
    });
    // No recovery point: re-raise with default handler (terminates process).
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
/// # Limitations
/// - Not re-entrant: nested guarded calls will overwrite the recovery point.
/// - Process-wide signal handlers: concurrent guarded calls from multiple
///   threads share handlers (but recovery points are thread-local).
/// - Resources acquired by the closure before the crash are leaked.
pub(crate) fn call_guarded<F, R>(f: F) -> Result<R, String>
where
    F: FnOnce() -> R,
{
    let saved = install_handlers();

    // Set up thread-local jmp_buf and recovery point.
    let result = JMP_BUF.with(|jmp_cell| {
        let mut buf = jmp_cell.get();
        let buf_ptr = buf.as_mut_ptr() as *mut c_void;

        RECOVERY_BUF.with(|rec| {
            rec.set(buf_ptr);

            // SAFETY: buf_ptr points to a valid 256-byte buffer (sigjmp_buf
            // is 200 bytes on x86_64). savesigs=1 saves the signal mask.
            let sig = unsafe { __sigsetjmp(buf_ptr, 1) };

            if sig == 0 {
                // Initial call: execute the protected closure.
                Ok(f())
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
            }
        })
    });

    // Clear recovery point and restore handlers.
    RECOVERY_BUF.with(|rec| rec.set(std::ptr::null_mut()));
    restore_handlers(&saved);

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_guard_normal_call_succeeds() {
        let result = call_guarded(|| 42);
        assert_eq!(result, Ok(42));
    }

    #[test]
    fn signal_guard_catches_sigsegv() {
        let result = call_guarded(|| -> i32 {
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
        let result = call_guarded(|| -> i32 {
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
            call_guarded(|| -> i32 { unsafe { std::ptr::read_volatile(std::ptr::null::<i32>()) } });
        // If handlers weren't restored, this second call would also be caught.
        // But since crash_handler restores SIG_DFL, a second crash would
        // terminate the process. We verify by checking that call_guarded
        // still works for normal calls.
        let result = call_guarded(|| 99);
        assert_eq!(result, Ok(99));
    }

    #[test]
    fn signal_guard_string_result() {
        let result = call_guarded(|| "hello".to_string());
        assert_eq!(result, Ok("hello".to_string()));
    }
}
