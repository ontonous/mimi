//! Mimi runtime binary I/O + streaming line reading — partial/byte file reads,
//! byte writes, and line-by-line reading (each / JSON).
//!
//! Extracted verbatim from `runtime/mod.rs` (the `Binary I/O` section) during
//! the 0.1.0 mechanical split (behavior bit-exact). Pure `extern "C"` leaf: no
//! crate-level Rust-path callers. Filesystem-related; may merge into `fs.rs`
//! in a later refinement. Forward deps on the parent module's `alloc_c_string`
//! / `alloc_c_string_from_bytes` helpers; uses `libc` in standalone mode.

use std::ffi::CStr;

#[cfg(standalone)]
use super::libc;
use super::{alloc_c_string, alloc_c_string_from_bytes};

/// Reads up to max_bytes from a file. Returns an allocated C string.
/// Caller must free with mimi_string_free.
#[no_mangle]
pub extern "C" fn mimi_read_file_partial(
    path: *const std::ffi::c_char,
    max_bytes: i64,
) -> *mut std::ffi::c_char {
    if path.is_null() {
        return alloc_c_string("");
    }
    // SAFETY: `path` was checked non-null above.
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return alloc_c_string(""),
    };
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
        Err(_) => alloc_c_string(""),
    }
}

/// Reads an entire file as raw bytes, returned as a C string.
/// Note: the returned C string is null-terminated, so any null byte in the
/// file content will be preserved in the allocation but consumer functions
/// that use `strlen`/`CStr::from_ptr` will see truncated content.
/// Caller must free with `mimi_string_free`.
#[no_mangle]
pub extern "C" fn mimi_read_file_bytes(path: *const std::ffi::c_char) -> *mut std::ffi::c_char {
    if path.is_null() {
        return alloc_c_string("");
    }
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return alloc_c_string(""),
    };
    match std::fs::read(path_str) {
        // M5/M22 fix: use raw bytes directly instead of from_utf8_lossy which
        // replaces non-UTF8 bytes with U+FFFD. alloc_c_string_from_bytes
        // preserves the exact byte content including null bytes (though the
        // first null will terminate if consumed as a C string).
        Ok(bytes) => alloc_c_string_from_bytes(&bytes),
        Err(_) => alloc_c_string(""),
    }
}

/// Writes raw byte data to a file. Returns 1 on success, 0 on failure.
#[no_mangle]
pub extern "C" fn mimi_write_file_bytes(
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
/// The `line_ptr` passed to `callback_fn` is freed by `libc::free` immediately
/// after the callback returns. The callback MUST copy the string if it needs
/// the data after returning (e.g., by calling `alloc_c_string` on it).
/// Holding onto the pointer after the callback returns is a use-after-free bug.
#[no_mangle]
pub extern "C" fn mimi_read_lines_each(
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
        match line_result {
            Ok(line) => {
                let c_line = alloc_c_string(&line);
                callback_fn(c_line);
                // Free the allocated string after callback
                // SAFETY: freeing the line buffer allocated by `alloc_c_string` after the callback.
                unsafe { libc::free(c_line as *mut std::ffi::c_void) };
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
#[no_mangle]
pub extern "C" fn mimi_read_lines_json(path: *const std::ffi::c_char) -> *mut std::ffi::c_char {
    use std::io::BufRead;
    if path.is_null() {
        return alloc_c_string("[]");
    }
    // SAFETY: `path` was checked non-null above.
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return alloc_c_string("[]"),
    };
    let file = match std::fs::File::open(path_str) {
        Ok(f) => f,
        Err(_) => return alloc_c_string("[]"),
    };
    let reader = std::io::BufReader::new(file);
    let mut result = String::from("[");
    let mut first = true;
    let mut lines = reader.lines();
    while let Some(Ok(line)) = lines.next() {
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
