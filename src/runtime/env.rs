// ===========================================================================
// Environment / CLI (extracted from runtime/mod.rs)
//
// Process environment + command-line argument access: mimi_args_init captures
// argv into a process-wide CLI_ARGS registry; mimi_args_count / mimi_args_list /
// mimi_args_get / mimi_getenv expose them to Mimi programs. Mirrors stdlib env.mimi.
// ===========================================================================

#[cfg(standalone)]
use super::libc;
use super::{alloc_c_string, cstr_to_string, ListElementKind, MimiList};
use std::sync::Mutex;

struct CliArgs {
    argc: i32,
    argv: Vec<usize>, // store raw pointers as usize (for Send safety)
}

// SAFETY: CliArgs holds raw pointers stored as usize; access is serialized via Mutex.
unsafe impl Send for CliArgs {}
// SAFETY: already documented above.
unsafe impl Sync for CliArgs {}

static CLI_ARGS: std::sync::OnceLock<Mutex<CliArgs>> = std::sync::OnceLock::new();

fn init_cli_args() {
    let _ = CLI_ARGS.get_or_init(|| {
        Mutex::new(CliArgs {
            argc: 0,
            argv: Vec::new(),
        })
    });
}

///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings/argv arrays
/// supplied by the C runtime or a matching mimi call.
#[no_mangle]
pub unsafe extern "C" fn mimi_args_init(argc: i32, argv: *mut *mut std::ffi::c_char) {
    init_cli_args();
    // M11: use get_or_init instead of get+expect to handle the case where
    // init_cli_args was already called but the OnceLock was not yet initialized
    // (e.g. when called before init_cli_args completes on another thread).
    let args_mutex = CLI_ARGS.get_or_init(|| {
        Mutex::new(CliArgs {
            argc: 0,
            argv: Vec::new(),
        })
    });
    let mut args = args_mutex.lock().unwrap_or_else(|e| e.into_inner());
    // H5 fix: free old C strings before clearing to prevent memory leak.
    for ptr in args.argv.drain(..) {
        if ptr != 0 {
            // SAFETY: ptr came from `alloc_c_string` (mimi_alloc), and the
            // null check above guards against double-free. `mimi_free` is the
            // matching deallocator (audit 2026-08-05, N-1: a raw libc::free
            // was the wrong allocator AND the wrong base under cfg(miri),
            // where mimi_alloc uses the Rust allocator + a size header; in
            // normal builds mimi_free IS libc::free).
            super::mimi_free(ptr as *mut std::ffi::c_void);
        }
    }
    // Audit fix (env.rs:47-72,121): `argc` must only ever reflect the number
    // of entries actually copied into `argv` below. When argv is null (or
    // argc non-positive) nothing is copied, so argc must be 0 — the old code
    // stored the raw argc unconditionally, leaving argc > argv.len() and an
    // out-of-bounds panic in mimi_args_list.
    if argv.is_null() || argc <= 0 {
        args.argc = 0;
        return;
    }
    // S9: Copy strings to owned memory instead of storing raw pointers.
    // Original argv may be freed after init returns.
    for i in 0..argc as isize {
        // SAFETY (M10): `argv` is a C main-style pointer array of length
        // `argc` (the caller's precondition; argv is non-null here). Loop
        // bound guarantees `0 <= i < argc`, so `argv.offset(i)` is in-bounds.
        // Each entry is a valid C string (or null handled by `cstr_to_string`).
        unsafe {
            let s = cstr_to_string(*argv.offset(i));
            let ptr = alloc_c_string(&s);
            args.argv.push(ptr as usize);
        }
    }
    // Guard argc against the actually-copied count (== argc here, but never
    // derived from an unchecked foreign value downstream).
    args.argc = args.argv.len() as i32;
}

///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings/argv arrays
/// supplied by the C runtime or a matching mimi call.
#[no_mangle]
pub unsafe extern "C" fn mimi_getenv(name: *const std::ffi::c_char) -> *mut std::ffi::c_char {
    // SAFETY: cstr_to_string safely handles null pointers.
    let n = unsafe { cstr_to_string(name) };
    // Audit fix (env.rs:76 vs fs.rs:641): take the same SETENV_LOCK that
    // mimi_set_env holds while writing. POSIX setenv may reallocate the
    // `environ` array; reading the environment concurrently with such a
    // write is a use-after-free on the old array. Every runtime accessor of
    // the process environment (this reader, mimi_set_env writers) must hold
    // the lock.
    // SAFETY: the mutex guard serializes with mimi_set_env's set_var;
    // poisoned mutexes are recovered via into_inner (no state to protect).
    let _lock = super::fs::SETENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match std::env::var(&n) {
        Ok(val) => alloc_c_string(&val),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn mimi_args_count() -> i64 {
    init_cli_args();
    // Prefer get_or_init so concurrent races never panic on missing OnceLock.
    let args_mutex = CLI_ARGS.get_or_init(|| {
        Mutex::new(CliArgs {
            argc: 0,
            argv: vec![],
        })
    });
    let args = args_mutex.lock().unwrap_or_else(|e| e.into_inner());
    // Audit fix: derive the count from the actually-stored vector (single
    // source of truth) — argv[0] is the program name and not user-facing.
    let total = args.argv.len();
    if total <= 1 {
        return 0;
    }
    (total - 1) as i64
}

#[no_mangle]
pub extern "C" fn mimi_args_list() -> *mut MimiList {
    init_cli_args();
    let args_mutex = CLI_ARGS.get_or_init(|| {
        Mutex::new(CliArgs {
            argc: 0,
            argv: vec![],
        })
    });
    let args = args_mutex.lock().unwrap_or_else(|e| e.into_inner());
    // Audit fix: derive the count from the actually-stored vector and skip
    // argv[0] (program name). The old code indexed `args.argv[i]` up to the
    // raw `argc`, which panicked out of bounds when argc != argv.len()
    // (e.g. after mimi_args_init(argc>0, argv=null)).
    let total = args.argv.len();
    let count = total.saturating_sub(1);
    // C8 fix: copy each arg string into an owned libc::malloc allocation
    // instead of returning pointers into CLI_ARGS storage. This eliminates
    // the dangling pointer risk when CLI_ARGS is re-initialized.
    //
    // H1-pattern fix (matches mod.rs `mimi_str_split`): allocate the element
    // array itself with libc::malloc and copy out of the Vec. mimi_list_free
    // frees the data buffer with libc::free — a Rust Vec buffer is a
    // different allocator (UB to free via libc). Audit 2026-08-05 (H-26):
    // the list is constructed with has_header=false, so list_cap/list_free
    // never read data[-8]; the flag replaced the old negative-value heuristic.
    let data_ptr = if count == 0 {
        std::ptr::null_mut()
    } else {
        let data_size = match count.checked_mul(std::mem::size_of::<*mut std::ffi::c_char>()) {
            Some(s) => s,
            None => {
                return Box::into_raw(Box::new(MimiList::new_with_kind(ListElementKind::String)))
            }
        };
        // SAFETY: data_size > 0 (count > 0); result is checked for null.
        let ptr = unsafe { libc::malloc(data_size) as *mut *mut std::ffi::c_char };
        if ptr.is_null() {
            // OOM: match the mimi_str_split failure convention (null list).
            return std::ptr::null_mut();
        }
        for i in 0..count {
            let src = args.argv[i + 1] as *const std::ffi::c_char;
            let s = if !src.is_null() {
                // SAFETY: ptr is a non-null C string owned by the args table.
                // cstr_to_string only reads up to the first NUL byte; the lifetime
                // of the resulting String is independent of the source buffer.
                unsafe { cstr_to_string(src) }
            } else {
                String::new()
            };
            // SAFETY: i < count and `ptr` is a fresh allocation of `count`
            // pointer slots; writing each slot exactly once.
            unsafe {
                *ptr.add(i) = alloc_c_string(&s);
            }
        }
        ptr
    };
    let len = count as i64;
    // 0.31.23: args are strings. The MimiList STRUCT is Box-allocated,
    // matching mimi_list_free which frees it via Box::from_raw (mod.rs).
    // No hidden capacity header: has_header=false (with_data default) →
    // list_cap returns 0 without reading data[-8] and free(data) is direct.
    Box::into_raw(Box::new(MimiList::with_data(
        data_ptr,
        len,
        true,
        ListElementKind::String,
    )))
}

#[no_mangle]
pub extern "C" fn mimi_args_get(i: i64) -> *mut std::ffi::c_char {
    init_cli_args();
    let args_mutex = CLI_ARGS.get_or_init(|| {
        Mutex::new(CliArgs {
            argc: 0,
            argv: vec![],
        })
    });
    let args = args_mutex.lock().unwrap_or_else(|e| e.into_inner());
    // Audit fix: bound against the actually-stored vector, not the raw argc.
    let total = args.argv.len() as i64;
    if i < 0 || total <= 1 || i >= total - 1 {
        return std::ptr::null_mut();
    }
    let idx = (i + 1) as usize; // +1 to skip program name
                                // C8 (deep audit): return an *owned* copy of the argument string rather than
                                // a raw pointer into CLI_ARGS storage. On a later `mimi_args_init` the stored
                                // strings are freed, which would otherwise leave the caller holding a dangling
                                // pointer (UAF). The returned buffer is independently allocated and must be
                                // freed by the caller with `mimi_string_free`.
    match args.argv.get(idx) {
        Some(&p) if p != 0 => {
            let s = unsafe { cstr_to_string(p as *const std::ffi::c_char) };
            alloc_c_string(&s)
        }
        _ => std::ptr::null_mut(),
    }
}

#[cfg(test)]
mod tests {
    //! Regression tests for the 2026-08-05 audit fixes:
    //! - mimi_args_init(argv=null) must store argc=0 (no argc/argv divergence
    //!   → the old mimi_args_list out-of-bounds index panic).
    //! - mimi_args_list must hand mimi_list_free a libc::malloc'd element
    //!   array (H1 pattern), not a Rust Vec buffer (allocator mismatch +
    //!   list_cap data[-8] OOB).

    use super::*;

    /// CLI_ARGS is a process global; cargo runs tests on parallel threads,
    /// so the tests that mutate it must serialize against each other.
    static CLI_ARGS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Reset the process-global CLI_ARGS to the default (empty) state so the
    /// tests leave no residue for other test modules.
    fn reset_cli_args() {
        unsafe {
            mimi_args_init(0, std::ptr::null_mut());
        }
    }

    #[test]
    fn args_init_null_argv_sets_argc_zero() {
        let _serial = CLI_ARGS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Old behavior stored argc=5 with an empty argv vector, and
        // mimi_args_list then indexed `argv[i]` out of bounds (panic).
        unsafe {
            mimi_args_init(5, std::ptr::null_mut());
        }
        assert_eq!(mimi_args_count(), 0);
        assert!(mimi_args_get(0).is_null());
        let list = mimi_args_list();
        assert!(!list.is_null());
        // SAFETY: `list` was just allocated by mimi_args_list.
        unsafe {
            assert_eq!((*list).len, 0);
        }
        // Freeing an empty list must not read data[-8] (list_cap guards null).
        unsafe {
            crate::runtime::mimi_list_free(list, true);
        }
        reset_cli_args();
    }

    #[test]
    fn args_list_elements_survive_mimi_list_free() {
        let _serial = CLI_ARGS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prog = std::ffi::CString::new("prog").unwrap();
        let a1 = std::ffi::CString::new("alpha").unwrap();
        let a2 = std::ffi::CString::new("beta").unwrap();
        let mut argv: [*mut std::ffi::c_char; 3] = [
            prog.as_ptr() as *mut _,
            a1.as_ptr() as *mut _,
            a2.as_ptr() as *mut _,
        ];
        unsafe {
            mimi_args_init(3, argv.as_mut_ptr());
        }

        assert_eq!(mimi_args_count(), 2);

        let s0 = mimi_args_get(0);
        assert!(!s0.is_null());
        // SAFETY: s0 is non-null (checked) and owned by alloc_c_string.
        assert_eq!(unsafe { cstr_to_string(s0) }, "alpha");
        // SAFETY: s0 was allocated by alloc_c_string (mimi_alloc); mimi_free
        // is the matching deallocator (N-1 pairing).
        crate::runtime::mimi_free(s0 as *mut std::ffi::c_void);

        let s1 = mimi_args_get(1);
        assert!(!s1.is_null());
        // SAFETY: s1 is non-null (checked) and owned by alloc_c_string.
        assert_eq!(unsafe { cstr_to_string(s1) }, "beta");
        // SAFETY: s1 was allocated by alloc_c_string (mimi_alloc); mimi_free
        // is the matching deallocator (N-1 pairing).
        crate::runtime::mimi_free(s1 as *mut std::ffi::c_void);

        let list = mimi_args_list();
        assert!(!list.is_null());
        // SAFETY: `list` was just allocated by mimi_args_list with len == 2;
        // data points to a malloc'd array of 2 elements.
        unsafe {
            assert_eq!((*list).len, 2);
            assert_eq!(cstr_to_string(*(*list).data), "alpha");
            assert_eq!(cstr_to_string(*(*list).data.add(1)), "beta");
            // H-26 flag contract: args_list is a header-less owning list —
            // list_cap/list_free must never read data[-8] for it.
            assert!(!(*list).has_header);
            assert!((*list).owns_data);
        }
        // The old Vec-buffer ABI failed exactly here: list_cap read data[-8]
        // OOB and mimi_list_free freed a Vec buffer via libc::free. With the
        // has_header flag the free goes straight to the malloc'd base.
        unsafe {
            crate::runtime::mimi_list_free(list, true);
        }
        reset_cli_args();
    }

    #[test]
    fn getenv_roundtrip_holds_setenv_lock() {
        // mimi_getenv must serialize with mimi_set_env on SETENV_LOCK;
        // a serial set → get cycle returns the written value.
        let key = std::ffi::CString::new("MIMI_AUDIT_ENV_SUB").unwrap();
        let val = std::ffi::CString::new("hello-audit").unwrap();
        assert_eq!(
            unsafe { crate::runtime::fs::mimi_set_env(key.as_ptr(), val.as_ptr()) },
            1
        );
        let p = unsafe { mimi_getenv(key.as_ptr()) };
        assert!(!p.is_null());
        // SAFETY: p is non-null (checked) and owned by alloc_c_string.
        assert_eq!(unsafe { cstr_to_string(p) }, "hello-audit");
        // SAFETY: p was allocated by alloc_c_string (mimi_alloc); mimi_free
        // is the matching deallocator (N-1 pairing).
        crate::runtime::mimi_free(p as *mut std::ffi::c_void);
        std::env::remove_var("MIMI_AUDIT_ENV_SUB");
    }
}
