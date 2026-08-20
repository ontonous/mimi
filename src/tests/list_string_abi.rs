//! Phase B old List<string> ABI reject: a NUL-terminated-without-length
//! payload must not be accepted as a current List<string>.

use crate::runtime::{
    mimi_list_read_string, mimi_str_join, ListElementKind, MimiList, MIMI_ERR_OLD_STRING_ABI,
};

#[test]
fn list_string_old_cstr_abi_is_rejected() {
    // Old payload: element_kind=String, string_abi=0, data = char** with
    // an embedded NUL. A C-string reader would report length 5 ("hello").
    let payload = b"hello\0world\0";
    let mut slot: *mut std::ffi::c_char = payload.as_ptr() as *mut std::ffi::c_char;
    let old = MimiList {
        len: 1,
        data: &mut slot as *mut *mut std::ffi::c_char,
        owns_data: false,
        element_kind: ListElementKind::String,
        has_header: false,
        string_abi: 0,
    };

    let mut out_ptr: *mut std::ffi::c_char = std::ptr::null_mut();
    let mut out_len: i64 = 0;
    let rc = unsafe { mimi_list_read_string(&old, 0, &mut out_ptr, &mut out_len) };
    assert_eq!(
        rc, MIMI_ERR_OLD_STRING_ABI,
        "old C-string list element must be rejected, got rc={rc} out_len={out_len}"
    );
    assert_eq!(
        out_len, -1,
        "reject must not report the C-string prefix length 5"
    );
    assert!(out_ptr.is_null());

    let sep = b",\0".as_ptr() as *const std::ffi::c_char;
    let joined = unsafe { mimi_str_join(&old, sep) };
    assert!(
        joined.is_null(),
        "join of a legacy C-string list must fail, not return a truncated prefix"
    );
}
