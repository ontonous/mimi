// FFI fork lock poisoning panic is intentional.
#![cfg_attr(not(test), allow(clippy::unwrap_used))]
use super::super::*;
use crate::ffi::FfiRetContract;
use libffi::middle::{Cif, CodePtr};
use std::cell::RefCell;
use std::ffi::c_void;

// sigsetjmp / siglongjmp are not bound in the libc crate, so we declare
// them directly. The jmp_buf size matches glibc's __jmp_buf_tag on x86_64.
// On other platforms, the struct size may differ (handled via cfg).
#[cfg(target_arch = "x86_64")]
type SigJmpBuf = [i64; 40]; // 320 bytes covers glibc's sigjmp_buf on x86_64
#[cfg(not(target_arch = "x86_64"))]
type SigJmpBuf = [i64; 64]; // generous fallback for other archs

// glibc exposes sigsetjmp as __sigsetjmp (the macro in <setjmp.h> expands to it).
// siglongjmp is the actual symbol name. We use #[link_name] to remap.
extern "C" {
    #[link_name = "__sigsetjmp"]
    fn sigsetjmp_impl(env: *mut SigJmpBuf, savemask: i32) -> i32;
    fn siglongjmp(env: *mut SigJmpBuf, val: i32) -> !;
}

/// Global fork lock: acquired before fork() to prevent concurrent
/// FFI operations (thread pool, callbacks) during the fork window.
/// Held across fork(), released in parent and child via pthread_atfork.
static FORK_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();

fn ensure_fork_lock() -> &'static std::sync::Mutex<()> {
    FORK_LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

// ===================== #[no_panic] Signal / Panic Protection =====================

thread_local! {
    /// Thread-local jump buffer for signal-based crash recovery.
    /// Set before a #[no_panic] FFI call; the signal handler siglongjmps here.
    static FFI_CRASH_JUMP_BUF: RefCell<Option<*mut SigJmpBuf>> = const { RefCell::new(None) };
}

/// Signal handler for C-level crashes (SIGSEGV, SIGABRT, etc.).
/// First restores SIG_DFL (so a second crash actually kills us),
/// then longjmps back to the jump buffer installed before the FFI call.
extern "C" fn ffi_crash_signal_handler(sig: i32) {
    unsafe {
        let mut dfl: libc::sigaction = std::mem::zeroed();
        dfl.sa_sigaction = libc::SIG_DFL as usize;
        libc::sigaction(libc::SIGSEGV, &dfl, std::ptr::null_mut());
        libc::sigaction(libc::SIGABRT, &dfl, std::ptr::null_mut());
        libc::sigaction(libc::SIGBUS, &dfl, std::ptr::null_mut());
        libc::sigaction(libc::SIGILL, &dfl, std::ptr::null_mut());
        libc::sigaction(libc::SIGFPE, &dfl, std::ptr::null_mut());
    }
    FFI_CRASH_JUMP_BUF.with(|cell| {
        if let Some(buf) = *cell.borrow() {
            unsafe { siglongjmp(buf, sig as i32); }
        }
    });
}

/// Temporarily install crash-recovery signal handlers.
/// Returns the old handlers so they can be restored later.
fn install_crash_handlers() -> [libc::sigaction; 5] {
    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
    unsafe { libc::sigemptyset(&mut sa.sa_mask) };
    sa.sa_flags = libc::SA_NODEFER;
    sa.sa_sigaction = ffi_crash_signal_handler as extern "C" fn(i32) as usize;

    let mut old = [
        unsafe { std::mem::zeroed() },
        unsafe { std::mem::zeroed() },
        unsafe { std::mem::zeroed() },
        unsafe { std::mem::zeroed() },
        unsafe { std::mem::zeroed() },
    ];
    let sigs = [libc::SIGSEGV, libc::SIGABRT, libc::SIGBUS, libc::SIGILL, libc::SIGFPE];
    for (i, &s) in sigs.iter().enumerate() {
        unsafe { libc::sigaction(s, &sa, &mut old[i]); }
    }
    old
}

/// Restore previously saved signal handlers.
fn restore_crash_handlers(old: &[libc::sigaction; 5]) {
    let sigs = [libc::SIGSEGV, libc::SIGABRT, libc::SIGBUS, libc::SIGILL, libc::SIGFPE];
    for (i, &s) in sigs.iter().enumerate() {
        unsafe { libc::sigaction(s, &old[i], std::ptr::null_mut()); }
    }
}

// ===================== FFI Call Methods =====================

impl<'a> Interpreter<'a> {
    /// Call a C function via libffi (raw, standalone — no self access).
    /// Safe to call after fork() since it doesn't touch Rust data structures
    /// beyond the raw pointers passed in.
    unsafe fn call_ffi_raw(
        cif: &Cif,
        code_ptr: CodePtr,
        ffi_args: &[libffi::middle::Arg],
        ret_contract: &FfiRetContract,
    ) -> i64 {
        match ret_contract {
            FfiRetContract::Unit => {
                cif.call::<()>(code_ptr, ffi_args);
                0i64
            }
            FfiRetContract::Float => {
                let val: f64 = cif.call(code_ptr, ffi_args);
                val.to_bits() as i64
            }
            _ => cif.call::<i64>(code_ptr, ffi_args),
        }
    }

    /// Call a C function that returns a struct by value, writing into a
    /// caller-provided buffer. Uses the low-level `raw::ffi_call` API to
    /// supply a custom return-value buffer of the struct's size.
    /// Call a C function that returns a struct by value, writing into a
    /// caller-provided buffer. Uses the low-level `raw::ffi_call` API to
    /// supply a custom return-value buffer of the struct's size.
    pub(in crate::interp) unsafe fn call_ffi_raw_struct(
        cif: &Cif,
        code_ptr: CodePtr,
        ffi_args: &[libffi::middle::Arg],
        rvalue: *mut c_void,
    ) {
        // SAFETY: rvalue must be a valid, writable buffer of sufficient
        // size for the struct return type. cif.as_raw_ptr() provides a
        // valid CIF descriptor for libffi.
        let fn_ptr = unsafe { *code_ptr.as_safe_fun() };
        unsafe {
            libffi::raw::ffi_call(
                cif.as_raw_ptr(),
                Some(fn_ptr),
                rvalue,
                ffi_args.as_ptr() as *mut *mut c_void,
            );
        }
    }

    /// Call a C function without crash protection via libffi.
    pub(in crate::interp) fn call_ffi_direct(
        &self,
        cif: &Cif,
        code_ptr: CodePtr,
        ffi_args: &[libffi::middle::Arg],
        ret_contract: &FfiRetContract,
    ) -> Result<i64, String> {
        unsafe {
            Ok(Self::call_ffi_raw(cif, code_ptr, ffi_args, ret_contract))
        }
    }

    /// Call a C function with full #[no_panic] protection:
    ///   1. Install crash-recovery signal handlers (SIGSEGV/SIGABRT/SIGBUS/…)
    ///   2. sigsetjmp recovery point for C-level crashes
    ///   3. catch_unwind for Rust panics in callbacks
    ///
    /// On success: Ok(result)
    /// On C crash (signal): Err("FFI safety: C function crashed with SIG*")
    /// On Rust panic: Err("FFI safety: Rust panic in extern function: …")
    pub(in crate::interp) fn call_ffi_no_panic(
        &self,
        cif: &Cif,
        code_ptr: CodePtr,
        ffi_args: &[libffi::middle::Arg],
        ret_contract: &FfiRetContract,
    ) -> Result<i64, String> {
        // 1. Install signal handlers and save old ones
        let old_handlers = install_crash_handlers();

        // 2. Allocate jump buffer on the heap (survives siglongjmp)
        let jump_buf = Box::new(unsafe { std::mem::zeroed::<SigJmpBuf>() });
        let buf_ptr = Box::into_raw(jump_buf) as *mut SigJmpBuf;

        // 3. Register jump buffer in TLS so the signal handler can find it
        FFI_CRASH_JUMP_BUF.with(|cell| {
            *cell.borrow_mut() = Some(buf_ptr);
        });

        // 4. sigsetjmp — recovery point for C crashes
        //    First call returns 0; siglongjmp returns with sig >= 1
        let sig = unsafe { sigsetjmp_impl(buf_ptr, 1) };
        if sig != 0 {
            // C crash: signal handler already set handlers to SIG_DFL.
            // Free the heap-allocated jump buffer.
            unsafe { let _ = Box::from_raw(buf_ptr); }
            let sig_name = match sig {
                6 => "SIGABRT", 11 => "SIGSEGV", 7 => "SIGBUS",
                4 => "SIGILL", 8 => "SIGFPE", n => {
                    return Err(format!("FFI safety: C function crashed with signal {}", n));
                }
            };
            return Err(format!("FFI safety: C function crashed with {} (signal {})", sig_name, sig));
        }

        // 5. Call the actual C function, wrapped in catch_unwind for Rust panics
        let result = std::panic::catch_unwind(|| {
            unsafe { Self::call_ffi_raw(cif, code_ptr, ffi_args, ret_contract) }
        });

        // 6. Normal path: restore signal handlers and free jump buffer
        FFI_CRASH_JUMP_BUF.with(|cell| {
            *cell.borrow_mut() = None;
        });
        restore_crash_handlers(&old_handlers);
        unsafe { let _ = Box::from_raw(buf_ptr); }

        match result {
            Ok(val) => Ok(val),
            Err(panic_info) => {
                let msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_info.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown cause".to_string()
                };
                Err(format!("FFI safety: Rust panic in extern function: {}", msg))
            }
        }
    }

    /// Call a C function with crash isolation via fork().
    /// If the child process crashes (SIGSEGV, SIGBUS, etc.), returns an Err.
    ///
    /// ⚠ SAFETY: fork() is only safe in single-threaded contexts.
    /// The fork lock serializes fork() against concurrent FFI operations,
    /// but async-signal-safety of libffi calls in the child is not guaranteed.
    pub(in crate::interp) fn call_ffi_with_fork_isolation(
        &self,
        cif: &Cif,
        code_ptr: CodePtr,
        ffi_args: &[libffi::middle::Arg],
        ret_contract: &FfiRetContract,
    ) -> Result<i64, String> {
        // Acquire fork lock to serialize fork() with other FFI operations.
        // The lock is held across fork and released in parent/child handlers.
        let _guard = ensure_fork_lock().lock()
            .expect("FFI fork lock poisoned");

        let mut pipe_fds: [std::ffi::c_int; 2] = [0; 2];
        let pipe_ret = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
        if pipe_ret != 0 {
            return Err("FFI safety: failed to create pipe for crash isolation".to_string());
        }

        let pid = unsafe { libc::fork() };
        if pid == 0 {
            unsafe { libc::close(pipe_fds[0]); }
            let result_code = unsafe { Self::call_ffi_raw(cif, code_ptr, ffi_args, ret_contract) };
            unsafe {
                libc::write(pipe_fds[1], &result_code as *const i64 as *const libc::c_void,
                    std::mem::size_of::<i64>());
                libc::close(pipe_fds[1]);
                libc::_exit(0);
            }
        }

        // PARENT
        unsafe { libc::close(pipe_fds[1]); }

        unsafe {
            let flags = libc::fcntl(pipe_fds[0], libc::F_GETFL, 0);
            if flags >= 0 {
                libc::fcntl(pipe_fds[0], libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }

        let ffi_timeout_ms = std::env::var("MIMI_FFI_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(30_000);
        let deadline = std::time::Instant::now()
            .checked_add(std::time::Duration::from_millis(ffi_timeout_ms))
            .unwrap_or_else(|| std::time::Instant::now() + std::time::Duration::from_secs(30));

        let mut status: i32 = 0;
        loop {
            let ret = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            if ret == pid {
                break;
            }
            if ret == -1 {
                let err = std::io::Error::last_os_error();
                unsafe { libc::close(pipe_fds[0]); }
                return Err(format!("FFI safety: waitpid error: {}", err));
            }
            if std::time::Instant::now() >= deadline {
                unsafe { libc::kill(pid, libc::SIGKILL); }
                unsafe { libc::waitpid(pid, &mut status, 0); }
                unsafe { libc::close(pipe_fds[0]); }
                return Err(format!(
                    "FFI safety: C function timed out after {}ms",
                    ffi_timeout_ms,
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        if libc::WIFSIGNALED(status) {
            let sig = libc::WTERMSIG(status);
            let sig_name = match sig {
                6 => "SIGABRT", 11 => "SIGSEGV", 7 => "SIGBUS",
                4 => "SIGILL", 8 => "SIGFPE", _ => "unknown signal",
            };
            unsafe { libc::close(pipe_fds[0]); }
            return Err(format!("FFI safety: C function crashed with {} (signal {})", sig_name, sig));
        }

        let mut result: i64 = 0;
        let nread = unsafe {
            let n = libc::read(pipe_fds[0], &mut result as *mut i64 as *mut libc::c_void,
                std::mem::size_of::<i64>());
            libc::close(pipe_fds[0]);
            n
        };

        if nread <= 0 {
            Err("FFI safety: C function exited without producing a result".to_string())
        } else if result == i64::MIN {
            Err("FFI safety: C function returned an error".to_string())
        } else {
            Ok(result)
        }
    }
}
