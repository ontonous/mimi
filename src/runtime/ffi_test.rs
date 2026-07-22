//! Mimi runtime FFI test helpers — `__mimi_*` test structs and
//! `__mimi_extern_test_*` extern functions used by the FFI contract test suite.
//!
//! Extracted verbatim from `runtime/mod.rs` (the `FFI test helpers` section)
//! during the 0.1.0 mechanical split (behavior bit-exact). Pure `extern "C"`
//! leaf: test fixtures linked from generated code, no crate-level Rust-path
//! callers.

use std::ffi::CStr;

use super::{alloc_c_string, cstr_to_string};

// ---------------------------------------------------------------------------
// FFI test helpers
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct __mimi_TestPoint {
    x: i32,
    y: i32,
}

#[repr(C)]
pub struct __mimi_MixedStruct {
    id: i32,
    value: f64,
    flag: i32,
}

#[repr(C)]
pub struct __mimi_InnerStruct {
    a: i32,
    b: i32,
}

#[repr(C)]
pub struct __mimi_OuterStruct {
    inner: __mimi_InnerStruct,
    c: i32,
}

#[repr(C)]
pub struct __mimi_TimespecStruct {
    sec: i64,
    nsec: i64,
}

#[no_mangle]
pub extern "C" fn __mimi_extern_test_positive(x: i32) -> i32 {
    x
}

#[no_mangle]
pub extern "C" fn __mimi_extern_test_callback(
    x: i32,
    cb: Option<unsafe extern "C" fn(i32) -> i32>,
) -> i32 {
    match cb {
        // SAFETY: callback pointer came from a valid `Option<unsafe extern C fn>` argument.
        Some(f) => unsafe { f(x) },
        None => x,
    }
}

#[no_mangle]
pub extern "C" fn __mimi_extern_test_float_identity(x: f64) -> f64 {
    x
}

#[no_mangle]
pub extern "C" fn __mimi_extern_test_struct_by_val(p: __mimi_TestPoint) -> i32 {
    p.x + p.y
}

#[no_mangle]
pub extern "C" fn __mimi_extern_test_mixed_struct(s: __mimi_MixedStruct) -> f64 {
    s.id as f64 + s.value + s.flag as f64
}

#[no_mangle]
pub extern "C" fn __mimi_extern_test_nested_struct(o: __mimi_OuterStruct) -> i32 {
    o.inner.a + o.inner.b + o.c
}

#[no_mangle]
pub extern "C" fn __mimi_extern_test_timespec_sum(t: __mimi_TimespecStruct) -> i64 {
    t.sec + t.nsec
}

#[no_mangle]
pub extern "C" fn __mimi_extern_test_strlen(s: *const std::ffi::c_char) -> i32 {
    if s.is_null() {
        return -1;
    }
    // SAFETY: `s` was checked non-null above.
    let str = unsafe { CStr::from_ptr(s) };
    str.to_bytes().len() as i32
}

#[no_mangle]
pub extern "C" fn __mimi_extern_test_nop() {}

#[no_mangle]
pub extern "C" fn __mimi_extern_test_parse_int(json: *const std::ffi::c_char) -> i32 {
    if json.is_null() {
        return -1;
    }
    // SAFETY: `json` was checked non-null above.
    let s = unsafe { cstr_to_string(json) };
    let s = s.trim();
    let neg = s.starts_with('-');
    let digits = s.trim_start_matches('-');
    let val: i32 = digits
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .unwrap_or_else(|e| {
            // C6-fix: log parse failure instead of silently returning 0
            eprintln!(
                "[mimi runtime] mimi_json_as_i32: parse error for '{}': {}",
                digits, e
            );
            0
        });
    if neg {
        -val
    } else {
        val
    }
}

#[no_mangle]
pub extern "C" fn __mimi_extern_test_greet(x: i32) -> *mut std::ffi::c_char {
    let msg = format!("Hello {}", x);
    alloc_c_string(&msg)
}

#[no_mangle]
pub extern "C" fn __mimi_extern_test_json_sum(json: *const std::ffi::c_char) -> i32 {
    if json.is_null() {
        return -1;
    }
    // SAFETY: `json` was checked non-null above.
    let s = unsafe { cstr_to_string(json) };
    let s = s.trim();
    if !s.starts_with('[') {
        return -1;
    }
    let inner = s.trim_start_matches('[').trim_end_matches(']');
    let mut sum = 0i32;
    for part in inner.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Ok(n) = part.parse::<i32>() {
            sum = sum.wrapping_add(n);
        }
    }
    sum
}

// M2 (pre-round6): deliberate UB test symbols must not ship in production
// binaries. Enabled under cargo test, feature `test_ub_symbols`, or the
// custom cfg `mimi_test_ub_symbols` (passed only by test FFI .so builds).
#[cfg(any(test, feature = "test_ub_symbols", mimi_test_ub_symbols))]
#[no_mangle]
pub extern "C" fn __mimi_extern_test_segfault() {
    // Deliberate null pointer dereference — used by FFI safety tests to verify
    // crash handling. Only Mimi test code calls this function.
    // SAFETY: deliberate null-pointer write for FFI crash testing only.
    unsafe {
        std::ptr::write_volatile(std::ptr::null_mut::<i32>(), 42);
    }
}

#[cfg(any(test, feature = "test_ub_symbols", mimi_test_ub_symbols))]
#[no_mangle]
pub extern "C" fn __mimi_extern_test_abort() {
    std::process::abort();
}

#[no_mangle]
pub extern "C" fn __mimi_extern_test_make_point(x: i32, y: i32) -> __mimi_TestPoint {
    __mimi_TestPoint { x, y }
}

#[no_mangle]
pub extern "C" fn __mimi_extern_test_make_mixed(
    id: i32,
    value: f64,
    flag: i32,
) -> __mimi_MixedStruct {
    __mimi_MixedStruct { id, value, flag }
}

// Interpreter FFI wrappers (plain names, without __mimi_extern_test_ prefix)
// These MUST have the exact `extern "C"` ABI so the FFI test .so bindings work.

#[no_mangle]
pub extern "C" fn test_float_identity(x: f64) -> f64 {
    __mimi_extern_test_float_identity(x)
}

#[no_mangle]
pub extern "C" fn test_strlen(s: *const std::ffi::c_char) -> i32 {
    __mimi_extern_test_strlen(s)
}

#[no_mangle]
pub extern "C" fn test_nop() {
    __mimi_extern_test_nop()
}

#[no_mangle]
pub extern "C" fn test_parse_int(json: *const std::ffi::c_char) -> i32 {
    __mimi_extern_test_parse_int(json)
}

#[no_mangle]
pub extern "C" fn test_json_sum(json: *const std::ffi::c_char) -> i32 {
    __mimi_extern_test_json_sum(json)
}

#[cfg(any(test, feature = "test_ub_symbols", mimi_test_ub_symbols))]
#[no_mangle]
pub extern "C" fn test_segfault() {
    __mimi_extern_test_segfault()
}

#[cfg(any(test, feature = "test_ub_symbols", mimi_test_ub_symbols))]
#[no_mangle]
pub extern "C" fn test_abort() {
    __mimi_extern_test_abort()
}

#[no_mangle]
pub extern "C" fn test_struct_by_val(p: __mimi_TestPoint) -> i32 {
    __mimi_extern_test_struct_by_val(p)
}

#[no_mangle]
pub extern "C" fn test_make_point(x: i32, y: i32) -> __mimi_TestPoint {
    __mimi_extern_test_make_point(x, y)
}

#[no_mangle]
pub extern "C" fn test_make_mixed(id: i32, value: f64, flag: i32) -> __mimi_MixedStruct {
    __mimi_extern_test_make_mixed(id, value, flag)
}

#[no_mangle]
pub extern "C" fn test_mixed_struct(s: __mimi_MixedStruct) -> f64 {
    __mimi_extern_test_mixed_struct(s)
}

#[no_mangle]
pub extern "C" fn test_nested_struct(o: __mimi_OuterStruct) -> i32 {
    __mimi_extern_test_nested_struct(o)
}

#[no_mangle]
pub extern "C" fn test_timespec_sum(t: __mimi_TimespecStruct) -> i64 {
    __mimi_extern_test_timespec_sum(t)
}

#[no_mangle]
pub extern "C" fn test_greet(x: i32) -> *mut std::ffi::c_char {
    __mimi_extern_test_greet(x)
}

#[no_mangle]
pub extern "C" fn test_callback(x: i32, cb: Option<unsafe extern "C" fn(i32) -> i32>) -> i32 {
    __mimi_extern_test_callback(x, cb)
}

// Cross-thread callback helper (v0.28.18).
//
// `test_callback` invokes the callback on the SAME thread as the caller.
// To validate that the interpreter correctly evaluates callbacks invoked
// from a DIFFERENT thread (where TLS interpreter context is absent), we
// provide `test_threaded_callback` which spawns a worker thread to invoke
// the callback, then joins and returns the result.
//
// This exercises v0.28.18 cross-thread callback infrastructure:
//   - src/interp/ffi/callback.rs:SendFilePtr + CALLBACK_FILE store
//   - ensure_callback_file() registration at FFI call setup
//   - evaluate_cross_thread_callback() temporary Interpreter path
//
// The cross-thread path is interpreter-only because the codegen callback
// trampoline does not yet implement cross-thread closure evaluation.

#[no_mangle]
pub extern "C" fn test_threaded_callback(
    x: i32,
    cb: Option<unsafe extern "C" fn(i32) -> i32>,
) -> i32 {
    let handle = std::thread::spawn(move || match cb {
        // SAFETY: `f` is a user-supplied `unsafe extern "C" fn(i32) -> i32` whose
        // soundness is the caller's responsibility; the closure captures `x` by
        // value and `f` is invoked with that single argument.
        Some(f) => unsafe { f(x) },
        // IP-C4: 0 error sentinel (i64::MIN is a legal return).
        None => 0,
    });
    handle.join().unwrap_or(0)
}
