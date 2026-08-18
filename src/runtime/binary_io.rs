//! Mimi runtime binary I/O + streaming line reading — partial/byte file reads,
//! byte writes, and line-by-line reading (each / JSON).
//!
//! Extracted verbatim from `runtime/mod.rs` (the `Binary I/O` section) during
//! the 0.1.0 mechanical split (behavior bit-exact). Pure `extern "C"` leaf: no
//! crate-level Rust-path callers. Filesystem-related; may merge into `fs.rs`
//! in a later refinement. Forward deps on the parent module's `alloc_c_string`
//! / `alloc_c_string_from_bytes` / `mimi_free` helpers (audit 2026-08-05,
//! N-1: every string freed through the matching mimi_alloc deallocator).

use std::ffi::CStr;

use super::{alloc_c_string, alloc_c_string_from_bytes};

/// Hard cap for whole-file reads. Prevents a malicious/oversized file from
/// exhausting memory through `std::fs::read` (batch4/06 P2).
const MAX_FILE_READ_BYTES: u64 = 256 * 1024 * 1024;
/// Hard cap on the number of lines processed by the line-streaming helpers.
const MAX_LINE_ITEMS: i64 = 1_000_000;
/// Hard cap on the JSON result size for `mimi_read_lines_json`.
const MAX_LINES_JSON_BYTES: usize = 256 * 1024 * 1024;

/// Reads up to max_bytes from a file. Returns an allocated C string.
/// Caller must free with mimi_string_free.
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings
/// (unless documented otherwise) and must live until the call returns.
#[no_mangle]
pub unsafe extern "C" fn mimi_read_file_partial(
    path: *const std::ffi::c_char,
    max_bytes: i64,
) -> *mut std::ffi::c_char {
    if path.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `path` was checked non-null above.
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return std::ptr::null_mut(),
    };
    if std::fs::metadata(path_str)
        .map(|meta| meta.len())
        .unwrap_or(0)
        > MAX_FILE_READ_BYTES
    {
        return std::ptr::null_mut();
    }
    match std::fs::read(path_str) {
        Ok(bytes) => {
            let limit = max_bytes.max(0) as usize;
            let slice = if limit > 0 && bytes.len() > limit {
                &bytes[..limit]
            } else {
                &bytes
            };
            // Use lossy conversion to handle arbitrary bytes
            let s = String::from_utf8_lossy(slice);
            alloc_c_string(&s)
        }
        Err(_) => std::ptr::null_mut(),
    }
}

/// Reads up to max_bytes from a file and returns the actual byte length
/// through `out_len`. Length-aware variant used by codegen so embedded NUL
/// bytes are not truncated.
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise); `out_len` must be null or point to a writable i64.
#[no_mangle]
pub unsafe extern "C" fn mimi_read_file_partial_ll(
    path: *const std::ffi::c_char,
    max_bytes: i64,
    out_len: *mut i64,
) -> *mut std::ffi::c_char {
    if !out_len.is_null() {
        // SAFETY: out_len was checked non-null above.
        unsafe { *out_len = 0 };
    }
    if path.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `path` was checked non-null above.
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return std::ptr::null_mut(),
    };
    if std::fs::metadata(path_str)
        .map(|meta| meta.len())
        .unwrap_or(0)
        > MAX_FILE_READ_BYTES
    {
        return std::ptr::null_mut();
    }
    match std::fs::read(path_str) {
        Ok(bytes) => {
            let limit = max_bytes.max(0) as usize;
            let slice = if limit > 0 && bytes.len() > limit {
                &bytes[..limit]
            } else {
                &bytes
            };
            if !out_len.is_null() {
                // SAFETY: out_len was checked non-null above.
                unsafe { *out_len = slice.len() as i64 };
            }
            alloc_c_string_from_bytes(slice)
        }
        Err(_) => std::ptr::null_mut(),
    }
}

/// Reads an entire file as raw bytes, returned as a C string.
/// Note: the returned C string is null-terminated, so any null byte in the
/// file content will be preserved in the allocation but consumer functions
/// that use `strlen`/`CStr::from_ptr` will see truncated content.
/// Caller must free with `mimi_string_free`.
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings
/// (unless documented otherwise) and must live until the call returns.
#[no_mangle]
pub unsafe extern "C" fn mimi_read_file_bytes(
    path: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    if path.is_null() {
        return std::ptr::null_mut();
    }
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return std::ptr::null_mut(),
    };
    if std::fs::metadata(path_str)
        .map(|meta| meta.len())
        .unwrap_or(0)
        > MAX_FILE_READ_BYTES
    {
        return std::ptr::null_mut();
    }
    match std::fs::read(path_str) {
        // M5/M22 fix: use raw bytes directly instead of from_utf8_lossy which
        // replaces non-UTF8 bytes with U+FFFD. alloc_c_string_from_bytes
        // preserves the exact byte content including null bytes (though the
        // first null will terminate if consumed as a C string).
        Ok(bytes) => alloc_c_string_from_bytes(&bytes),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Reads an entire file as raw bytes and returns its byte length through
/// `out_len`. This is the length-aware variant used by codegen so embedded
/// NUL bytes are not truncated by strlen-based consumers.
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise); `out_len` must be null or point to a writable i64.
#[no_mangle]
pub unsafe extern "C" fn mimi_read_file_bytes_ll(
    path: *const std::ffi::c_char,
    out_len: *mut i64,
) -> *mut std::ffi::c_char {
    if path.is_null() {
        if !out_len.is_null() {
            // SAFETY: out_len was checked non-null above.
            unsafe { *out_len = 0 };
        }
        return std::ptr::null_mut();
    }
    // SAFETY: `path` was checked non-null above.
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            if !out_len.is_null() {
                // SAFETY: out_len was checked non-null above.
                unsafe { *out_len = 0 };
            }
            return std::ptr::null_mut();
        }
    };
    if std::fs::metadata(path_str)
        .map(|meta| meta.len())
        .unwrap_or(0)
        > MAX_FILE_READ_BYTES
    {
        if !out_len.is_null() {
            // SAFETY: out_len was checked non-null above.
            unsafe { *out_len = 0 };
        }
        return std::ptr::null_mut();
    }
    match std::fs::read(path_str) {
        Ok(bytes) => {
            if !out_len.is_null() {
                // SAFETY: out_len was checked non-null above.
                unsafe { *out_len = bytes.len() as i64 };
            }
            alloc_c_string_from_bytes(&bytes)
        }
        Err(_) => {
            if !out_len.is_null() {
                // SAFETY: out_len was checked non-null above.
                unsafe { *out_len = 0 };
            }
            std::ptr::null_mut()
        }
    }
}

/// Writes raw byte data to a file with an explicit byte length. Returns 1 on
/// success, 0 on failure. Length-aware variant used by codegen so embedded
/// NUL bytes are written intact.
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise) and must live until the call returns.
#[no_mangle]
pub unsafe extern "C" fn mimi_write_file_bytes_ll(
    path: *const std::ffi::c_char,
    data: *const std::ffi::c_char,
    len: i64,
) -> i32 {
    if path.is_null() || data.is_null() || len < 0 {
        return 0;
    }
    // SAFETY: `path` was checked non-null above.
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };
    // SAFETY: `data` is non-null and the caller guarantees it is valid for
    // `len` bytes; the length is checked non-negative above.
    let data_bytes = unsafe { std::slice::from_raw_parts(data as *const u8, len as usize) };
    match std::fs::write(path_str, data_bytes) {
        Ok(_) => 1,
        Err(_) => 0,
    }
}

/// Writes raw byte data to a file. Returns 1 on success, 0 on failure.
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings
/// (unless documented otherwise) and must live until the call returns.
#[no_mangle]
pub unsafe extern "C" fn mimi_write_file_bytes(
    path: *const std::ffi::c_char,
    data: *const std::ffi::c_char,
) -> i32 {
    if path.is_null() || data.is_null() {
        return 0;
    }
    // SAFETY: `path` was checked non-null above.
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };
    // SAFETY: `data` was checked non-null above.
    let data_bytes = unsafe { CStr::from_ptr(data) }.to_bytes();
    match std::fs::write(path_str, data_bytes) {
        Ok(_) => 1,
        Err(_) => 0,
    }
}

/// Reads file line-by-line, calling callback(line) for each line.
/// callback_fn is a function pointer: fn(line_ptr: *const c_char) -> ()
///
/// # String lifecycle (M7)
/// The `line_ptr` passed to `callback_fn` is freed via `mimi_free` (the
/// `alloc_c_string` / `mimi_alloc` matching deallocator — audit 2026-08-05,
/// N-1) immediately after the callback returns. The callback MUST copy the
/// string if it needs the data after returning (e.g., by calling
/// `alloc_c_string` on it). Holding onto the pointer after the callback
/// returns is a use-after-free bug.
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings
/// (unless documented otherwise) and must live until the call returns.
#[no_mangle]
pub unsafe extern "C" fn mimi_read_lines_each(
    path: *const std::ffi::c_char,
    callback_fn: extern "C" fn(*const std::ffi::c_char),
) -> i64 {
    use std::io::BufRead;
    if path.is_null() {
        return -1;
    }
    // SAFETY: `path` was checked non-null above.
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let file = match std::fs::File::open(path_str) {
        Ok(f) => f,
        Err(_) => return -1,
    };
    let reader = std::io::BufReader::new(file);
    let mut count: i64 = 0;
    for line_result in reader.lines() {
        if count >= MAX_LINE_ITEMS {
            break;
        }
        match line_result {
            Ok(line) => {
                let c_line = alloc_c_string(&line);
                callback_fn(c_line);
                // Free the allocated string after callback through the
                // matching deallocator (N-1: alloc_c_string → mimi_alloc;
                // a raw libc::free was wrong-allocator/wrong-base under miri).
                // SAFETY: `c_line` was allocated by `alloc_c_string` just
                // above and the callback has already returned.
                super::mimi_free(c_line as *mut std::ffi::c_void);
                count += 1;
            }
            Err(_) => break,
        }
    }
    count
}

/// Reads file line-by-line and collects lines into a JSON array string.
/// More memory-efficient than read_file + split for very large files since
/// it uses BufReader, but still returns all lines as a single JSON string.
/// Caller must free with mimi_string_free.
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings
/// (unless documented otherwise) and must live until the call returns.
#[no_mangle]
pub unsafe extern "C" fn mimi_read_lines_json(
    path: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    use std::io::BufRead;
    if path.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `path` was checked non-null above.
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return std::ptr::null_mut(),
    };
    let file = match std::fs::File::open(path_str) {
        Ok(f) => f,
        Err(_) => return std::ptr::null_mut(),
    };
    let reader = std::io::BufReader::new(file);
    let mut result = String::from("[");
    let mut first = true;
    let mut count: i64 = 0;
    for line in reader.lines() {
        if count >= MAX_LINE_ITEMS || result.len() > MAX_LINES_JSON_BYTES {
            return std::ptr::null_mut();
        }
        let line = match line {
            Ok(line) => line,
            Err(_) => return std::ptr::null_mut(),
        };
        count += 1;
        if !first {
            result.push(',');
        }
        first = false;
        // Escape the line for JSON
        result.push('"');
        for ch in line.chars() {
            match ch {
                '"' => result.push_str("\\\""),
                '\\' => result.push_str("\\\\"),
                '\n' => result.push_str("\\n"),
                '\r' => result.push_str("\\r"),
                '\t' => result.push_str("\\t"),
                c if c < '\x20' => {
                    result.push_str(&format!("\\u{:04x}", c as u32));
                }
                c => result.push(c),
            }
        }
        result.push('"');
    }
    result.push(']');
    alloc_c_string(&result)
}
