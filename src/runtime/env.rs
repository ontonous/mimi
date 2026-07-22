// ===========================================================================
// Environment / CLI (extracted from runtime/mod.rs)
//
// Process environment + command-line argument access: mimi_args_init captures
// argv into a process-wide CLI_ARGS registry; mimi_args_count / mimi_args_list /
// mimi_args_get / mimi_getenv expose them to Mimi programs. Mirrors stdlib env.mimi.
// ===========================================================================

#[cfg(standalone)]
use super::libc;
use super::{alloc_c_string, cstr_to_string, MimiList};
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

#[no_mangle]
pub extern "C" fn mimi_args_init(argc: i32, argv: *mut *mut std::ffi::c_char) {
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
    args.argc = argc;
    // H5 fix: free old C strings before clearing to prevent memory leak.
    for ptr in args.argv.drain(..) {
        if ptr != 0 {
            // SAFETY: ptr came from `libc::malloc`-family allocator in `alloc_c_string`,
            // and the null check above guards against double-free. The same
            // allocator must be used to free.
            unsafe { libc::free(ptr as *mut std::ffi::c_void) };
        }
    }
    // S9: Copy strings to owned memory instead of storing raw pointers.
    // Original argv may be freed after init returns.
    if !argv.is_null() && argc > 0 {
        for i in 0..argc as isize {
            // SAFETY (M10): `argv` is a C main-style pointer array of length
            // `argc`. Loop bound guarantees `0 <= i < argc`, so `argv.offset(i)`
            // is in-bounds. Each entry is a valid C string (or null handled by
            // `cstr_to_string`).
            unsafe {
                let s = cstr_to_string(*argv.offset(i));
                let ptr = alloc_c_string(&s);
                args.argv.push(ptr as usize);
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn mimi_getenv(name: *const std::ffi::c_char) -> *mut std::ffi::c_char {
    // SAFETY: cstr_to_string safely handles null pointers.
    let n = unsafe { cstr_to_string(name) };
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
    if args.argc <= 1 {
        return 0;
    }
    (args.argc - 1) as i64
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
    let count = if args.argc <= 1 {
        0
    } else {
        (args.argc - 1) as usize
    };
    // C8 fix: copy each arg string into an owned libc::malloc allocation
    // instead of returning pointers into CLI_ARGS storage. This eliminates
    // the dangling pointer risk when CLI_ARGS is re-initialized.
    let mut items: Vec<*mut std::ffi::c_char> = Vec::with_capacity(count);
    for i in 1..args.argc as usize {
        let ptr = args.argv[i] as *const std::ffi::c_char;
        let s = if !ptr.is_null() {
            // SAFETY: ptr is a non-null C string owned by the args table.
            // cstr_to_string only reads up to the first NUL byte; the lifetime
            // of the resulting String is independent of the source buffer.
            unsafe { cstr_to_string(ptr) }
        } else {
            String::new()
        };
        items.push(alloc_c_string(&s));
    }
    let data_ptr = items.as_mut_ptr();
    let len = items.len() as i64;
    // M24: use ManuallyDrop to avoid leaking 24-byte Vec struct metadata.
    // The raw data buffer is owned by the MimiList returned below; when
    // mimi_list_free is called, it frees data via libc::free and the list
    // struct via Box::from_raw.
    let _drop_guard = std::mem::ManuallyDrop::new(items);
    Box::into_raw(Box::new(MimiList {
        len,
        data: data_ptr,
        owns_data: true,
    }))
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
    if i < 0 || i >= (args.argc - 1) as i64 {
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
