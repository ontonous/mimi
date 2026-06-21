use super::super::*;
use crate::ffi::FfiRetContract;
use libffi::middle::{Cif, CodePtr};

/// Global fork lock: acquired before fork() to prevent concurrent
/// FFI operations (thread pool, callbacks) during the fork window.
/// Held across fork(), released in parent and child via pthread_atfork.
static FORK_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();

fn ensure_fork_lock() -> &'static std::sync::Mutex<()> {
    FORK_LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

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
            // CHILD: run the C call via raw trampoline, send result, _exit.
            // The child must NOT touch any Rust stdlib types (Arc, Mutex, etc.)
            // because they may be in an inconsistent state after fork.
            unsafe { libc::close(pipe_fds[0]); }
            // SAFETY: call_ffi_raw is safe to call after fork() because it
            // doesn't touch any Rust data structures beyond the raw CIF/args.
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

        // Set pipe read end to non-blocking so we never deadlock if the
        // child crashes without writing (e.g., panic unwind, SIGKILL before write).
        unsafe {
            let flags = libc::fcntl(pipe_fds[0], libc::F_GETFL, 0);
            if flags >= 0 {
                libc::fcntl(pipe_fds[0], libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }

        // F4: poll waitpid with WNOHANG + timeout so a hung C function does not
        // permanently block the Mimi process.
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
                break; // child exited
            }
            if ret == -1 {
                let err = std::io::Error::last_os_error();
                unsafe { libc::close(pipe_fds[0]); }
                return Err(format!("FFI safety: waitpid error: {}", err));
            }
            if std::time::Instant::now() >= deadline {
                // Timeout — kill the child forcefully
                unsafe { libc::kill(pid, libc::SIGKILL); }
                // Reap the zombie
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
            // Pipe was empty or error — child exited without writing result.
            // This is unexpected (child _exit should only run after write).
            Err("FFI safety: C function exited without producing a result".to_string())
        } else if result == i64::MIN {
            Err("FFI safety: C function returned an error".to_string())
        } else {
            Ok(result)
        }
    }
}
