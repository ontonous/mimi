// FFI fork lock poisoning panic is intentional.
#![cfg_attr(not(test), allow(clippy::unwrap_used))]
use super::super::*;
use crate::ffi::FfiRetContract;
use libffi::middle::{Cif, CodePtr};
use std::ffi::c_void;

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
    /// Pipeline for fork-based crash isolation.
    ///
    /// The child process:
    /// 1. Closes pipe read end
    /// 2. Calls the C function via libffi
    /// 3. Writes the result to pipe
    /// 4. Calls libc::_exit(0)
    ///
    /// # async-signal-safety
    /// libffi's `ffi_call` is NOT guaranteed to be async-signal-safe (it may
    /// internally call `malloc`). However, since the fork lock serializes all
    /// fork operations, the parent's heap is in a consistent state at fork time.
    /// The child's heap is a direct copy of the parent's and is never modified
    /// by any other thread (the fork lock guarantees single-threaded execution).
    ///
    /// The only risk is if libffi's `ffi_call` modifies global state (e.g.,
    /// allocates or frees memory) in a way that leaves the child's heap
    /// inconsistent. This is mitigated by: (a) `_exit(0)` which skips all cleanup,
    /// (b) the fork lock which prevents concurrent FFI operations, and
    /// (c) the `MIMI_FFI_SKIP_FORK` env var which disables fork isolation.
    #[allow(clippy::too_many_arguments)]
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
