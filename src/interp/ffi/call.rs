// FFI fork lock poisoning panic is intentional.
#![cfg_attr(not(test), allow(clippy::unwrap_used))]
use super::super::*;
use crate::ffi::FfiRetContract;
use libffi::middle::{Cif, CodePtr};
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
    // P2-13 fix: OnceLock::get_or_init returns &'static T directly after first call.
    // The OnceLock internal check is a simple volatile read (not a heavy lock),
    // so the repeated calls have negligible overhead.
    FORK_LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

// ===================== #[no_panic] Signal / Panic Protection =====================

use std::collections::HashMap;

/// Wrapper for *mut SigJmpBuf that implements Send + Sync.
/// Raw pointers are !Send/!Sync by default, but jump buffers are
/// safely shareable across threads when used with siglongjmp.
struct JmpBufPtr(*mut SigJmpBuf);
// SAFETY: JmpBufPtr is only used as a key in a per-thread map. The pointer it
// wraps belongs to the thread that created it and is only dereferenced by that
// same thread (or by the signal handler reading the thread-local cell). Sharing
// the pointer value across threads does not grant cross-thread access to the
// jump buffer contents.
unsafe impl Send for JmpBufPtr {}
// SAFETY: Same as for Send: the wrapped pointer is thread-local in practice,
// so implementing Sync only shares the pointer value, not the buffer itself.
unsafe impl Sync for JmpBufPtr {}

/// FFI-7: Global map of jump buffers keyed by thread ID.
///
/// REPLACES thread_local! { static FFI_CRASH_JUMP_BUF: AtomicPtr<SigJmpBuf> }
/// which was NOT per-thread in a multi-threaded context.
///
/// Design: Two maps for crash-recovery FFI calls:
/// 1. FFI_CRASH_JUMP_BUFFERS  — HashMap<JmpBufPtr> for main thread (set/clear via Mutex)
/// 2. THREAD_JUMP_BUF         — thread_local Cell for signal handler read (async-signal-safe)
///
/// set_jump_buf writes to BOTH maps (main thread), get_jump_buf reads only the thread-local.
/// This ensures the signal handler path uses only async-signal-safe operations.
static FFI_CRASH_JUMP_BUFFERS: std::sync::OnceLock<std::sync::Mutex<HashMap<u64, JmpBufPtr>>> =
    std::sync::OnceLock::new();

// Thread-local jump buffer for signal handler access (async-signal-safe).
thread_local! {
    static THREAD_JUMP_BUF: std::cell::Cell<*mut SigJmpBuf> = const { std::cell::Cell::new(std::ptr::null_mut()) };
}

fn with_jump_buffers<R, F: FnOnce(&mut HashMap<u64, JmpBufPtr>) -> R>(f: F) -> R {
    let map = FFI_CRASH_JUMP_BUFFERS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut guard = map.lock().expect("FFI_CRASH_JUMP_BUFFERS lock poisoned");
    f(&mut guard)
}

/// Store the jump buffer for the current thread (main thread only).
fn set_jump_buf(buf: *mut SigJmpBuf) {
    // SAFETY: pthread_self returns the current thread ID; it is async-signal-safe
    // and the returned value is only used as a HashMap key for this thread.
    let tid = unsafe { libc::pthread_self() as u64 };
    with_jump_buffers(|map| {
        map.insert(tid, JmpBufPtr(buf));
    });
    // Also update thread-local so the signal handler can see it (async-signal-safe).
    THREAD_JUMP_BUF.with(|cell| cell.set(buf));
}

/// Clear the jump buffer for the current thread (main thread only).
fn clear_jump_buf() {
    // SAFETY: pthread_self returns the current thread ID; it is async-signal-safe
    // and the returned value is only used to remove this thread's entry.
    let tid = unsafe { libc::pthread_self() as u64 };
    with_jump_buffers(|map| {
        map.remove(&tid);
    });
    // Also clear thread-local.
    THREAD_JUMP_BUF.with(|cell| cell.set(std::ptr::null_mut()));
}

/// Get the jump buffer for the current thread (signal handler path).
/// Async-signal-safe: thread_local + Cell::get are signal-safe.
fn get_jump_buf() -> *mut SigJmpBuf {
    THREAD_JUMP_BUF.with(|cell| cell.get())
}

/// Signal handler for C-level crashes (SIGSEGV, SIGABRT, etc.).
/// FFI-7: Async-signal-safe operations ONLY:
/// - libc::signal (POSIX-mandated signal-safe)
/// - thread_local Cell::get (async-signal-safe)
/// - siglongjmp (async-signal-safe per POSIX)
///   NO mutex, NO pthread_getspecific, NO RefCell.
extern "C" fn ffi_crash_signal_handler(
    sig: libc::c_int,
    _siginfo: *mut libc::siginfo_t,
    _ucontext: *mut c_void,
) {
    // Restore SIG_DFL using signal() (async-signal-safe per POSIX).
    // A second crash will then actually kill the process.
    // SAFETY: libc::signal is async-signal-safe per POSIX. We reinstall the
    // default handler so a repeated crash terminates the process.
    unsafe {
        libc::signal(libc::SIGSEGV, libc::SIG_DFL);
        libc::signal(libc::SIGABRT, libc::SIG_DFL);
        libc::signal(libc::SIGBUS, libc::SIG_DFL);
        libc::signal(libc::SIGILL, libc::SIG_DFL);
        libc::signal(libc::SIGFPE, libc::SIG_DFL);
    }
    // FFI-7: Read jump buffer via atomic load (async-signal-safe).
    // Uses pthread_self (not TLS) to find the buffer.
    let buf = get_jump_buf();
    if !buf.is_null() {
        // SAFETY: buf is the active jump buffer for this thread, registered by
        // set_jump_buf and valid until clear_jump_buf or siglongjmp. siglongjmp
        // is async-signal-safe and returns to the matching sigsetjmp call.
        unsafe {
            siglongjmp(buf, sig);
        }
    }
}

/// Signal handler for C-level crashes (SIGSEGV, SIGABRT, etc.).
/// Returns the old handlers so they can be restored later.
fn install_crash_handlers() -> [libc::sigaction; 5] {
    // SAFETY: libc::sigaction is a C struct; zeroing it yields a valid initial
    // value (all flags/masks/handlers set to zero).
    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
    // FFI-6: Explicitly initialize sa_mask to empty.
    // zeroed() already sets it to empty, but being explicit ensures clarity.
    // SAFETY: sa_mask was just zero-initialized; sigemptyset is idempotent and
    // operates on a valid sigset_t pointer.
    unsafe { libc::sigemptyset(&mut sa.sa_mask) };
    // FFI-5: Use SA_SIGINFO so we can use sa_sigaction (3-arg handler).
    // The 1-arg sa_handler field is not available on all libc versions.
    sa.sa_flags = libc::SA_NODEFER | libc::SA_SIGINFO;
    // FFI-5: sa_sigaction is a usize field — cast the function pointer.
    // ffi_crash_signal_handler now has the matching 3-arg signature.
    sa.sa_sigaction = ffi_crash_signal_handler as *const () as usize;

    // SAFETY: A zeroed sigaction is a valid initial value for each element;
    // sigaction will overwrite them with the previous handlers.
    let mut old = [
        unsafe { std::mem::zeroed() },
        unsafe { std::mem::zeroed() },
        unsafe { std::mem::zeroed() },
        unsafe { std::mem::zeroed() },
        unsafe { std::mem::zeroed() },
    ];
    let sigs = [
        libc::SIGSEGV,
        libc::SIGABRT,
        libc::SIGBUS,
        libc::SIGILL,
        libc::SIGFPE,
    ];
    for (i, &s) in sigs.iter().enumerate() {
        // SAFETY: sa is fully initialized; old[i] is a valid out-pointer.
        unsafe {
            libc::sigaction(s, &sa, &mut old[i]);
        }
    }
    old
}

/// Restore previously saved signal handlers.
fn restore_crash_handlers(old: &[libc::sigaction; 5]) {
    let sigs = [
        libc::SIGSEGV,
        libc::SIGABRT,
        libc::SIGBUS,
        libc::SIGILL,
        libc::SIGFPE,
    ];
    for (i, &s) in sigs.iter().enumerate() {
        // SAFETY: old[i] was previously returned by a successful sigaction call.
        unsafe {
            libc::sigaction(s, &old[i], std::ptr::null_mut());
        }
    }
}

// ===================== FFI Call Methods =====================

impl<'a> Interpreter<'a> {
    /// Call a C function via libffi (raw, standalone — no self access).
    /// Safe to call after fork() since it doesn't touch Rust data structures
    /// beyond the raw pointers passed in.
    ///
    /// SAFETY: `cif` and `code_ptr` must describe a valid C function and ABI.
    /// `ffi_args` must be valid libffi arguments whose lifetimes exceed the call.
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
        // SAFETY: code_ptr was constructed from a valid non-null function pointer
        // address, so as_safe_fun returns Some. We deref to obtain the concrete
        // extern "C" fn pointer required by raw::ffi_call.
        let fn_ptr = unsafe { *code_ptr.as_safe_fun() };
        // SAFETY: ffi_call is called with a valid CIF, function pointer, return
        // buffer, and argument array; all lifetimes exceed this call.
        unsafe {
            libffi::raw::ffi_call(
                cif.as_raw_ptr(),
                Some(fn_ptr),
                rvalue,
                ffi_args.as_ptr() as *mut *mut c_void,
            );
        }
    }

    /// Call a C function returning struct-by-value with full #[no_panic] protection.
    /// Same signal/crash protection as `call_ffi_no_panic`, but writes the
    /// result into a caller-provided buffer via `call_ffi_raw_struct`.
    pub(in crate::interp) fn call_ffi_no_panic_struct(
        &self,
        cif: &Cif,
        code_ptr: CodePtr,
        ffi_args: &[libffi::middle::Arg],
        rvalue: *mut c_void,
    ) -> Result<(), String> {
        let old_handlers = install_crash_handlers();
        // SAFETY: SigJmpBuf is a plain byte array; zeroing it yields a valid
        // initial state for sigsetjmp.
        let jump_buf = Box::new(unsafe { std::mem::zeroed::<SigJmpBuf>() });
        let buf_ptr = Box::into_raw(jump_buf) as *mut SigJmpBuf;
        set_jump_buf(buf_ptr);
        // SAFETY: buf_ptr points to a live, exclusively-owned Box allocation.
        // savemask=1 preserves the current signal mask; the matching siglongjmp
        // restores it on the crash path.
        let sig = unsafe { sigsetjmp_impl(buf_ptr, 1) };
        if sig != 0 {
            restore_crash_handlers(&old_handlers);
            // SAFETY: buf_ptr was obtained from Box::into_raw above and has not
            // been freed yet on the crash path.
            unsafe {
                let _ = Box::from_raw(buf_ptr);
            }
            let sig_name = match sig {
                6 => "SIGABRT",
                11 => "SIGSEGV",
                7 => "SIGBUS",
                4 => "SIGILL",
                8 => "SIGFPE",
                n => {
                    return Err(format!("FFI safety: C function crashed with signal {}", n));
                }
            };
            return Err(format!(
                "FFI safety: C function crashed with {} (signal {})",
                sig_name, sig
            ));
        }
        // SAFETY: call_ffi_raw_struct is an unsafe fn whose contract is satisfied
        // by the valid CIF, code pointer, and caller-provided rvalue buffer.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            Self::call_ffi_raw_struct(cif, code_ptr, ffi_args, rvalue)
        }));
        clear_jump_buf();
        restore_crash_handlers(&old_handlers);
        // SAFETY: buf_ptr was obtained from Box::into_raw and is no longer needed.
        unsafe {
            let _ = Box::from_raw(buf_ptr);
        }
        match result {
            Ok(()) => Ok(()),
            Err(panic_info) => {
                let msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_info.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown cause".to_string()
                };
                Err(format!(
                    "FFI safety: Rust panic in extern function: {}",
                    msg
                ))
            }
        }
    }

    /// Call a C function returning struct-by-value with crash isolation via fork().
    /// The ret_buf is allocated in the parent before fork; the child writes the
    /// struct result and sends it back through a pipe. On crash the parent
    /// detects the signal and returns Err.
    ///
    /// # Async-signal-safety (Item 8)
    /// Same limitation as `call_ffi_with_fork_isolation`: libffi/child heap
    /// operations are not async-signal-safe. The buffer is pre-allocated in
    /// the parent to avoid malloc in the child.
    pub(in crate::interp) fn call_ffi_with_fork_isolation_struct(
        &self,
        cif: &Cif,
        code_ptr: CodePtr,
        ffi_args: &[libffi::middle::Arg],
        ret_buf: &mut [u8],
    ) -> Result<(), String> {
        let _guard = ensure_fork_lock().lock().expect("FFI fork lock poisoned");
        let mut pipe_fds: [std::ffi::c_int; 2] = [0; 2];
        // SAFETY: pipe_fds is a valid two-element out-array.
        let pipe_ret = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
        if pipe_ret != 0 {
            return Err("FFI safety: failed to create pipe for crash isolation".to_string());
        }
        // SAFETY: fork() is serialized by FORK_LOCK. The child path uses only
        // async-signal-safe functions and _exit(0) (see function-level docs).
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            // SAFETY: pipe_fds[0] is a valid pipe fd; closing it in the child is
            // required before writing to pipe_fds[1].
            unsafe {
                libc::close(pipe_fds[0]);
            }
            let rvalue = ret_buf.as_mut_ptr() as *mut c_void;
            // SAFETY: call_ffi_raw_struct contract is satisfied by ret_buf.
            unsafe {
                Self::call_ffi_raw_struct(cif, code_ptr, ffi_args, rvalue);
            }
            // SAFETY: pipe_fds[1] is a valid write end; ret_buf points to the
            // initialized struct result. _exit(0) avoids double-flushing stdio.
            unsafe {
                libc::write(
                    pipe_fds[1],
                    ret_buf.as_ptr() as *const libc::c_void,
                    ret_buf.len(),
                );
                libc::close(pipe_fds[1]);
                libc::_exit(0);
            }
        }
        // SAFETY: pipe_fds[1] is the write end in the parent and is no longer needed.
        unsafe {
            libc::close(pipe_fds[1]);
        }
        // SAFETY: pipe_fds[0] is a valid read end; fcntl only mutates its flags.
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
            // SAFETY: status is a valid out-pointer; waitpid with WNOHANG is non-blocking.
            let ret = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            if ret == pid {
                break;
            }
            if ret == -1 {
                let err = std::io::Error::last_os_error();
                // SAFETY: pipe_fds[0] is a valid read-end fd in the parent.
                unsafe {
                    libc::close(pipe_fds[0]);
                }
                return Err(format!("FFI safety: waitpid error: {}", err));
            }
            if std::time::Instant::now() >= deadline {
                // SAFETY: pid is a valid child process created by fork above.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
                // SAFETY: status is a valid out-pointer; waitpid with flags=0 blocks
                // until the killed child is reaped.
                unsafe {
                    libc::waitpid(pid, &mut status, 0);
                }
                // SAFETY: pipe_fds[0] is a valid read-end fd in the parent.
                unsafe {
                    libc::close(pipe_fds[0]);
                }
                return Err(format!(
                    "FFI safety: C function timed out after {}ms",
                    ffi_timeout_ms
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if libc::WIFSIGNALED(status) {
            let sig = libc::WTERMSIG(status);
            let sig_name = match sig {
                6 => "SIGABRT",
                11 => "SIGSEGV",
                7 => "SIGBUS",
                4 => "SIGILL",
                8 => "SIGFPE",
                _ => "unknown signal",
            };
            // SAFETY: pipe_fds[0] is a valid read-end fd in the parent.
            unsafe {
                libc::close(pipe_fds[0]);
            }
            return Err(format!(
                "FFI safety: C function crashed with {} (signal {})",
                sig_name, sig
            ));
        }
        let buf_len = ret_buf.len();
        let mut total_read = 0usize;
        while total_read < buf_len {
            // SAFETY: ret_buf remains live and total_read < buf_len, so the pointer
            // arithmetic stays within the mutable slice.
            let nread = unsafe {
                libc::read(
                    pipe_fds[0],
                    ret_buf.as_mut_ptr().add(total_read) as *mut libc::c_void,
                    buf_len - total_read,
                )
            };
            if nread > 0 {
                total_read += nread as usize;
            } else if nread == 0 {
                break;
            } else {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::WouldBlock {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }
                // SAFETY: pipe_fds[0] is a valid read-end fd in the parent.
                unsafe {
                    libc::close(pipe_fds[0]);
                }
                return Err(format!("FFI safety: failed to read struct return: {}", err));
            }
        }
        // SAFETY: pipe_fds[0] is a valid read-end fd in the parent.
        unsafe {
            libc::close(pipe_fds[0]);
        }
        if total_read != buf_len {
            return Err(
                "FFI safety: C function exited without producing a struct result".to_string(),
            );
        }
        Ok(())
    }

    /// Call a C function without crash protection via libffi.
    pub(in crate::interp) fn call_ffi_direct(
        &self,
        cif: &Cif,
        code_ptr: CodePtr,
        ffi_args: &[libffi::middle::Arg],
        ret_contract: &FfiRetContract,
    ) -> Result<i64, String> {
        // SAFETY: call_ffi_raw is an unsafe fn; its contract is satisfied by the
        // valid CIF, code pointer, and argument slice passed by call_extern.
        unsafe { Ok(Self::call_ffi_raw(cif, code_ptr, ffi_args, ret_contract)) }
    }

    /// Call a C function with full #[no_panic] protection:
    ///   1. Install crash-recovery signal handlers (SIGSEGV/SIGABRT/SIGBUS/…)
    ///   2. sigsetjmp recovery point for C-level crashes
    ///   3. catch_unwind for Rust panics in callbacks
    ///
    /// # S14: siglongjmp and destructors
    /// siglongjmp skips all Rust destructors on the crash path. Any
    /// Box/Vec/MutexGuard allocated between sigsetjmp and the crash will
    /// be leaked. Mitigations: jump buffer is heap-allocated BEFORE
    /// sigsetjmp, signal handlers are restored in crash path, minimal
    /// resources are held between sigsetjmp and the C call.
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
        // SAFETY: SigJmpBuf is a plain byte array; zeroing it yields a valid
        // initial state for sigsetjmp.
        let jump_buf = Box::new(unsafe { std::mem::zeroed::<SigJmpBuf>() });
        let buf_ptr = Box::into_raw(jump_buf) as *mut SigJmpBuf;

        // 3. Register jump buffer in the global map so the signal handler can find it
        //    FFI-7: Uses set_jump_buf (atomic store) which is signal-safe.
        set_jump_buf(buf_ptr);

        // 4. sigsetjmp — recovery point for C crashes
        //    First call returns 0; siglongjmp returns with sig >= 1
        // SAFETY: buf_ptr points to a live, exclusively-owned Box allocation.
        // savemask=1 preserves/restores the signal mask around the longjmp.
        let sig = unsafe { sigsetjmp_impl(buf_ptr, 1) };
        if sig != 0 {
            // C crash: after siglongjmp, restore saved handlers and free jump buf.
            restore_crash_handlers(&old_handlers);
            // SAFETY: buf_ptr was obtained from Box::into_raw and has not been freed.
            unsafe {
                let _ = Box::from_raw(buf_ptr);
            }
            let sig_name = match sig {
                6 => "SIGABRT",
                11 => "SIGSEGV",
                7 => "SIGBUS",
                4 => "SIGILL",
                8 => "SIGFPE",
                n => {
                    return Err(format!("FFI safety: C function crashed with signal {}", n));
                }
            };
            return Err(format!(
                "FFI safety: C function crashed with {} (signal {})",
                sig_name, sig
            ));
        }

        // 5. Call the actual C function, wrapped in catch_unwind for Rust panics
        // SAFETY: call_ffi_raw contract is satisfied by the valid CIF, code pointer,
        // and argument slice.
        let result = std::panic::catch_unwind(|| unsafe {
            Self::call_ffi_raw(cif, code_ptr, ffi_args, ret_contract)
        });

        // 6. Normal path: restore signal handlers and free jump buffer
        clear_jump_buf();
        restore_crash_handlers(&old_handlers);
        // SAFETY: buf_ptr was obtained from Box::into_raw and is no longer needed.
        unsafe {
            let _ = Box::from_raw(buf_ptr);
        }

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
                Err(format!(
                    "FFI safety: Rust panic in extern function: {}",
                    msg
                ))
            }
        }
    }

    /// Call a C function with crash isolation via fork().
    /// If the child process crashes (SIGSEGV, SIGBUS, etc.), returns an Err.
    ///
    /// ⚠ SAFETY: fork() is only safe in single-threaded contexts.
    /// The fork lock serializes fork() against concurrent FFI operations,
    /// but async-signal-safety of libffi calls in the child is not guaranteed.
    ///
    /// # Async-signal-safety (Item 8)
    /// After fork(), the child process inherits the parent's memory and
    /// (potentially) locked mutexes. Only async-signal-safe functions may
    /// be called in the child. libffi's internal malloc is NOT async-signal-safe.
    /// However, in practice the child only calls `call_ffi_raw` which is a
    /// thin wrapper around `ffi_call` — the actual library being called (not
    /// libffi itself) is the one that may perform allocations. This is a
    /// documented limitation: users who set `MIMI_FFI_PREFORK` can disable
    /// fork isolation entirely and use `#[no_panic]` signal-based protection
    /// instead.
    ///
    /// # S13: fork mutex deadlock
    /// fork() inherits all mutexes from the parent. If another thread holds
    /// a lock at fork time, the child deadlocks on any lock acquisition.
    /// The `FORK_LOCK` serializes fork() calls, but cannot prevent inheritance
    /// of locks held by other threads. This is an inherent fork() limitation.
    /// Workaround: use `MIMI_FFI_PREFORK=1` to disable fork isolation.
    pub(in crate::interp) fn call_ffi_with_fork_isolation(
        &self,
        cif: &Cif,
        code_ptr: CodePtr,
        ffi_args: &[libffi::middle::Arg],
        ret_contract: &FfiRetContract,
    ) -> Result<i64, String> {
        // Acquire fork lock to serialize fork() with other FFI operations.
        // The lock is held across fork and released in parent/child handlers.
        let _guard = ensure_fork_lock().lock().expect("FFI fork lock poisoned");

        let mut pipe_fds: [std::ffi::c_int; 2] = [0; 2];
        // SAFETY: pipe_fds is a valid two-element out-array.
        let pipe_ret = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
        if pipe_ret != 0 {
            return Err("FFI safety: failed to create pipe for crash isolation".to_string());
        }

        // SAFETY: fork() is serialized by FORK_LOCK. The child uses only
        // async-signal-safe functions before _exit(0) (see function-level docs).
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            // SAFETY: pipe_fds[0] is the read end in the child and is closed before writing.
            unsafe {
                libc::close(pipe_fds[0]);
            }
            // SAFETY: call_ffi_raw contract is satisfied by the valid CIF and args.
            let result_code = unsafe { Self::call_ffi_raw(cif, code_ptr, ffi_args, ret_contract) };
            // SAFETY: pipe_fds[1] is a valid write end; result_code is initialized.
            // _exit(0) avoids double-flushing inherited stdio buffers.
            unsafe {
                libc::write(
                    pipe_fds[1],
                    &result_code as *const i64 as *const libc::c_void,
                    std::mem::size_of::<i64>(),
                );
                libc::close(pipe_fds[1]);
                libc::_exit(0);
            }
        }

        // PARENT
        // SAFETY: pipe_fds[1] is the write end in the parent and is no longer needed.
        unsafe {
            libc::close(pipe_fds[1]);
        }

        // SAFETY: pipe_fds[0] is a valid read end; fcntl only mutates its flags.
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
            // SAFETY: status is a valid out-pointer; waitpid with WNOHANG is non-blocking.
            let ret = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            if ret == pid {
                break;
            }
            if ret == -1 {
                let err = std::io::Error::last_os_error();
                // SAFETY: pipe_fds[0] is a valid read-end fd in the parent.
                unsafe {
                    libc::close(pipe_fds[0]);
                }
                return Err(format!("FFI safety: waitpid error: {}", err));
            }
            if std::time::Instant::now() >= deadline {
                // SAFETY: pid is a valid child process created by fork above.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
                // SAFETY: status is a valid out-pointer; blocking waitpid reaps the child.
                unsafe {
                    libc::waitpid(pid, &mut status, 0);
                }
                // SAFETY: pipe_fds[0] is a valid read-end fd in the parent.
                unsafe {
                    libc::close(pipe_fds[0]);
                }
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
                6 => "SIGABRT",
                11 => "SIGSEGV",
                7 => "SIGBUS",
                4 => "SIGILL",
                8 => "SIGFPE",
                _ => "unknown signal",
            };
            // SAFETY: pipe_fds[0] is a valid read-end fd in the parent.
            unsafe {
                libc::close(pipe_fds[0]);
            }
            return Err(format!(
                "FFI safety: C function crashed with {} (signal {})",
                sig_name, sig
            ));
        }

        let mut result: i64 = 0;
        // SAFETY: result is a live local variable; read fills exactly 8 bytes and
        // then closes the valid read-end fd.
        let nread = unsafe {
            let n = libc::read(
                pipe_fds[0],
                &mut result as *mut i64 as *mut libc::c_void,
                std::mem::size_of::<i64>(),
            );
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
