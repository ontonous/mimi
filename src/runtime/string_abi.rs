//! String boundary helpers shared by generated native code.
//!
//! Mimi Strings are valid UTF-8 byte sequences with an explicit byte length.
//! C strings have neither property, so every C-to-Mimi conversion must check
//! UTF-8 and extern calls receiving a Mimi String get an isolated,
//! NUL-terminated copy.

use std::cmp::Ordering;
use std::ffi::{c_char, CStr};

unsafe fn string_slice_from_ptr_len<'a>(ptr: *const c_char, len: i64) -> Option<&'a str> {
    let len = usize::try_from(len).ok()?;
    if len > isize::MAX as usize {
        return None;
    }
    if len == 0 {
        return Some("");
    }
    if ptr.is_null() {
        return None;
    }

    // SAFETY: the C ABI caller must provide `len` readable bytes. The length
    // bounds and null check above satisfy Rust's remaining slice preconditions.
    let bytes = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) };
    std::str::from_utf8(bytes).ok()
}

fn c_string_utf8_len(value: &CStr) -> Option<i64> {
    let text = value.to_str().ok()?;
    i64::try_from(text.len()).ok()
}

fn compare_utf8_bytes(left: &[u8], right: &[u8]) -> Option<Ordering> {
    Some(
        std::str::from_utf8(left)
            .ok()?
            .cmp(std::str::from_utf8(right).ok()?),
    )
}

unsafe fn abort_string_abi(message: &'static CStr) -> ! {
    // SAFETY: `message` is a static, valid NUL-terminated diagnostic.
    unsafe { super::mimi_runtime_abort(message.as_ptr()) }
}

/// Return the byte length of a valid UTF-8 C string.
///
/// # Safety
/// `ptr` must point to a readable, NUL-terminated C string. A null pointer or
/// invalid UTF-8 fails closed through the Mimi runtime abort path.
#[no_mangle]
pub unsafe extern "C" fn mimi_c_string_len_utf8(ptr: *const c_char) -> i64 {
    if ptr.is_null() {
        // SAFETY: static diagnostic only.
        unsafe { abort_string_abi(c"Mimi String ingress rejected a null C string") };
    }
    // SAFETY: the public ABI contract requires a valid NUL-terminated pointer.
    let cstr = unsafe { CStr::from_ptr(ptr) };
    match c_string_utf8_len(cstr) {
        Some(len) => len,
        None => {
            // SAFETY: static diagnostic only.
            unsafe { abort_string_abi(c"Mimi String ingress rejected invalid UTF-8") };
        }
    }
}

/// Copy a Mimi UTF-8 String into a mutable, NUL-terminated C buffer.
///
/// The returned allocation is owned by the caller and must be released with
/// `mimi_string_free`. The buffer is independent of the source, so a C
/// function accepting `char *` cannot mutate a Mimi String or any aliases.
/// Embedded NUL is rejected because a C string cannot preserve its suffix.
///
/// # Safety
/// For positive `len`, `ptr` must point to `len` readable bytes. The bytes
/// must be valid UTF-8. Invalid input and allocation failure abort.
#[no_mangle]
pub unsafe extern "C" fn mimi_string_to_cstr_copy(ptr: *const c_char, len: i64) -> *mut c_char {
    // SAFETY: this helper's ABI contract requires the source range to be live.
    let source = unsafe { string_slice_from_ptr_len(ptr, len) };
    let Some(source) = source else {
        // SAFETY: static diagnostic only.
        unsafe { abort_string_abi(c"Mimi String passed to C has invalid UTF-8 or length") };
    };
    if source.as_bytes().contains(&0) {
        // A C string cannot represent bytes after an embedded NUL. Reject it
        // instead of silently changing the value seen by the foreign callee.
        // SAFETY: static diagnostic only.
        unsafe { abort_string_abi(c"Mimi String passed to C contains embedded NUL") };
    }
    let Some(allocation_len) = source
        .len()
        .checked_add(1)
        .filter(|len| *len <= isize::MAX as usize)
    else {
        // SAFETY: static diagnostic only.
        unsafe { abort_string_abi(c"Mimi String passed to C is too large") };
    };
    let buffer = super::mimi_alloc(allocation_len).cast::<u8>();
    if buffer.is_null() {
        // SAFETY: static diagnostic only.
        unsafe { abort_string_abi(c"Mimi String to C allocation failed") };
    }
    if !source.is_empty() {
        // SAFETY: `buffer` has `source.len() + 1` writable bytes, and source
        // points to a separate live allocation supplied by the caller.
        unsafe { std::ptr::copy_nonoverlapping(source.as_ptr(), buffer, source.len()) };
    }
    // SAFETY: the extra byte is inside the allocation made above.
    unsafe { *buffer.add(source.len()) = 0 };
    buffer.cast()
}

/// Compare two valid Mimi Strings using their complete byte lengths.
///
/// # Safety
/// Each pointer must provide the readable byte range described by its length.
/// Invalid lengths, null non-empty ranges, and invalid UTF-8 fail closed.
#[no_mangle]
pub unsafe extern "C" fn mimi_string_compare_utf8(
    left: *const c_char,
    left_len: i64,
    right: *const c_char,
    right_len: i64,
) -> i32 {
    // SAFETY: the native Mimi String ABI supplies each pointer with its exact
    // readable byte length; malformed ranges are rejected by the helper.
    let left = unsafe { string_slice_from_ptr_len(left, left_len) };
    // SAFETY: same ABI contract as the left-hand value.
    let right = unsafe { string_slice_from_ptr_len(right, right_len) };
    let (Some(left), Some(right)) = (left, right) else {
        // SAFETY: static diagnostic only.
        unsafe { abort_string_abi(c"Mimi String comparison received invalid UTF-8 or length") };
    };
    match left.cmp(right) {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn c_string_utf8_length_accepts_unicode_and_rejects_invalid_bytes() {
        let unicode = CString::new("雪").unwrap();
        assert_eq!(c_string_utf8_len(&unicode), Some(3));
        // SAFETY: CString guarantees a valid, NUL-terminated allocation.
        assert_eq!(unsafe { mimi_c_string_len_utf8(unicode.as_ptr()) }, 3);
        assert_eq!(c_string_utf8_len(c"\xff"), None);
        assert_eq!(c_string_utf8_len(c""), Some(0));
    }

    #[test]
    fn c_argument_copy_is_nul_terminated_and_does_not_alias_source() {
        let source = "雪tail";
        let bytes = source.as_bytes();
        // SAFETY: `bytes` is a live UTF-8 range for the duration of the call.
        let copy = unsafe { mimi_string_to_cstr_copy(bytes.as_ptr().cast(), bytes.len() as i64) };
        assert!(!copy.is_null());
        assert_ne!(copy.cast::<u8>() as *const u8, bytes.as_ptr());
        // SAFETY: the helper allocated `bytes.len() + 1` writable bytes.
        let copied = unsafe { std::slice::from_raw_parts(copy.cast::<u8>(), bytes.len() + 1) };
        assert_eq!(&copied[..bytes.len()], bytes);
        assert_eq!(copied[bytes.len()], 0);

        // SAFETY: `copy` points at the live independent copy.
        unsafe { *copy.cast::<u8>() = b'X' };
        assert_eq!(source, "雪tail");
        // SAFETY: `copy` was allocated through mimi_alloc by the helper.
        unsafe { super::super::mimi_string_free(copy) };
    }

    #[test]
    fn string_comparison_keeps_embedded_nul_suffixes_and_utf8_order() {
        let left = b"prefix\0left";
        let right = b"prefix\0right";
        // SAFETY: both slices remain live and each pointer is paired with its
        // exact readable byte length.
        assert_eq!(
            unsafe {
                mimi_string_compare_utf8(
                    left.as_ptr().cast(),
                    left.len() as i64,
                    right.as_ptr().cast(),
                    right.len() as i64,
                )
            },
            -1
        );
        assert_eq!(
            compare_utf8_bytes(b"prefix\0left", b"prefix\0right"),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare_utf8_bytes(b"prefix", b"prefix\0"),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare_utf8_bytes("雪".as_bytes(), "木".as_bytes()),
            Some(Ordering::Greater)
        );
        assert_eq!(compare_utf8_bytes(b"\xff", b"valid"), None);
    }
}
