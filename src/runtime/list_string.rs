//! Length-bearing `List<string>` elements (`{ptr, len}`).
//!
//! 0.38.26 (B-STR-001): string list slots no longer store a bare NUL-terminated
//! `char*`. Each element is a heap `MimiStr` `{ptr, len}` with the same
//! observation as a bare Mimi `string`. ABI v3 boxes own a copy of their
//! payload; v2 boxes had different ownership and are never freed as v3 boxes.
//! Old C-string payloads are rejected by the ABI handshake, never silently
//! truncated at the first `0x00`.

#[cfg(standalone)]
use super::libc;
use super::{
    alloc_c_string_from_bytes, mimi_alloc, mimi_free, str_from_ptr_len, ListElementKind, MimiList,
    MimiListAbiPrefix,
};

/// Current `List<string>` element ABI: v1 was `char*`, v2 used fat boxes with
/// legacy payload ownership, and v3 uses fat boxes that own their payload.
pub const LIST_STRING_ABI_VERSION: i32 = 3;

/// Written into `MimiList.string_abi` for every newly created string list.
pub const LIST_STRING_ABI_FAT: u8 = 3;

/// Legacy C-string element layout (no length). Readers must reject this.
pub const LIST_STRING_ABI_CSTR: u8 = 0;

/// Magic at the start of every current `MimiStr` box so a raw `char*` slot
/// cannot be mistaken for a fat element.
pub const MIMI_STR_MAGIC: u32 = 0x4D53_5452; // "MSTR"

/// Typed error: payload is the old NUL-terminated-without-length layout.
pub const MIMI_ERR_OLD_STRING_ABI: i32 = -100;

/// Heap box stored in each `List<string>` slot (the slot holds a pointer to this).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MimiStr {
    pub magic: u32,
    pub _pad: u32,
    pub ptr: *mut std::ffi::c_char,
    pub len: i64,
}

impl MimiStr {
    pub fn is_fat(self) -> bool {
        self.magic == MIMI_STR_MAGIC
    }
}

/// Allocate a fat string box owning a copy of `bytes`.
pub fn alloc_mimi_str(bytes: &[u8]) -> *mut MimiStr {
    let data = alloc_c_string_from_bytes(bytes);
    let box_ptr = mimi_alloc(std::mem::size_of::<MimiStr>()) as *mut MimiStr;
    if box_ptr.is_null() {
        if !data.is_null() {
            mimi_free(data as *mut std::ffi::c_void);
        }
        return std::ptr::null_mut();
    }
    unsafe {
        *box_ptr = MimiStr {
            magic: MIMI_STR_MAGIC,
            _pad: 0,
            ptr: data,
            len: bytes.len() as i64,
        };
    }
    box_ptr
}

/// Copy a raw byte range into a box-owned Mimi string. Returns null for an
/// invalid range or allocation failure. Strings are byte sequences: invalid
/// UTF-8 and embedded NUL bytes are preserved exactly.
fn box_mimi_str_copy(ptr: *const std::ffi::c_char, len: i64) -> *mut MimiStr {
    const MAX_STR_LEN: i64 = 64 * 1024 * 1024;
    if len < 0 || len > MAX_STR_LEN || (ptr.is_null() && len != 0) {
        return std::ptr::null_mut();
    }
    let n = len as usize;
    let bytes = if n == 0 {
        &[][..]
    } else {
        // SAFETY: the C ABI contract requires a non-null pointer readable for
        // `len` bytes; the length is bounded above.
        unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), n) }
    };
    let boxed = alloc_mimi_str(bytes);
    if boxed.is_null() || unsafe { (*boxed).ptr.is_null() } {
        if !boxed.is_null() {
            mimi_free(boxed.cast());
        }
        return std::ptr::null_mut();
    }
    boxed
}

/// Allocate a fat string box by copying `{ptr, len}`. The box owns its payload.
pub fn box_mimi_str(ptr: *const std::ffi::c_char, len: i64) -> *mut MimiStr {
    box_mimi_str_copy(ptr, len)
}

/// Read a list slot as a fat string. `Err(MIMI_ERR_OLD_STRING_ABI)` if the
/// slot is a bare C-string (or otherwise not a current `MimiStr`).
pub unsafe fn read_mimi_str(
    slot: *mut std::ffi::c_char,
) -> Result<(*mut std::ffi::c_char, i64), i32> {
    if slot.is_null() {
        return Ok((std::ptr::null_mut(), 0));
    }
    // Legacy C-string pointers (including .rodata literals) are often not
    // 8-aligned. Do not interpret them as MimiStr.
    if (slot as usize) % std::mem::align_of::<MimiStr>() != 0 {
        return Err(MIMI_ERR_OLD_STRING_ABI);
    }
    let s = slot as *const MimiStr;
    // A C-string slot points at character data; the first 4 bytes are almost
    // never MIMI_STR_MAGIC. Reject rather than strlen-truncate.
    if unsafe { (*s).magic } != MIMI_STR_MAGIC {
        return Err(MIMI_ERR_OLD_STRING_ABI);
    }
    Ok((unsafe { (*s).ptr }, unsafe { (*s).len }))
}

/// Borrow the bytes of a fat-string payload returned by `mimi_list_read_string`.
pub unsafe fn slot_bytes(ptr: *const std::ffi::c_char, len: i64) -> &'static [u8] {
    if ptr.is_null() || len <= 0 {
        return b"";
    }
    std::slice::from_raw_parts(ptr as *const u8, len as usize)
}

/// Compare two current `List<string>` slots by their logical string bytes.
pub unsafe fn cmp_fat_slots(
    a: *mut std::ffi::c_char,
    b: *mut std::ffi::c_char,
) -> std::cmp::Ordering {
    match (read_mimi_str(a), read_mimi_str(b)) {
        (Ok((ap, al)), Ok((bp, bl))) => slot_bytes(ap, al).cmp(slot_bytes(bp, bl)),
        (Ok(_), Err(_)) => std::cmp::Ordering::Less,
        (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
        (Err(_), Err(_)) => {
            // Leftover non-fat slots (legacy literal C strings). Compare as
            // C strings so sort does not abort; current lists take the Ok path.
            let a_s = if a.is_null() {
                ""
            } else {
                unsafe { std::ffi::CStr::from_ptr(a) }
                    .to_str()
                    .unwrap_or("")
            };
            let b_s = if b.is_null() {
                ""
            } else {
                unsafe { std::ffi::CStr::from_ptr(b) }
                    .to_str()
                    .unwrap_or("")
            };
            a_s.cmp(b_s)
        }
    }
}

/// Free a fat string box and its owned bytes. No-op on null. Does **not**
/// treat a non-fat pointer as a C-string (that would be the silent-truncation
/// path this module exists to close).
pub unsafe fn free_mimi_str(slot: *mut std::ffi::c_char) {
    if slot.is_null() {
        return;
    }
    if (slot as usize) % std::mem::align_of::<MimiStr>() != 0 {
        return;
    }
    let s = slot as *mut MimiStr;
    if unsafe { (*s).magic } != MIMI_STR_MAGIC {
        return;
    }
    let ptr = unsafe { (*s).ptr };
    if !ptr.is_null() {
        mimi_free(ptr as *mut std::ffi::c_void);
    }
    mimi_free(slot as *mut std::ffi::c_void);
}

/// True when `list` is a full runtime `MimiList` carrying a string-ABI stamp
/// that is not the current fat layout.
pub unsafe fn list_has_legacy_string_abi(list: *const MimiList) -> bool {
    if list.is_null() {
        return false;
    }
    let lst = unsafe { &*list };
    matches!(
        lst.element_kind,
        ListElementKind::String | ListElementKind::Unknown
    ) && lst.string_abi != LIST_STRING_ABI_FAT
}

#[no_mangle]
pub extern "C" fn mimi_list_string_abi_version() -> i32 {
    LIST_STRING_ABI_VERSION
}

/// Copy `{ptr, len}` into an owning list-string box. Returns its handle as i64
/// (0 for an invalid range or allocation failure). On success release it with
/// `mimi_str_free_box`; the input buffer remains owned by the caller.
///
/// # Safety
/// For positive `len`, `ptr` must be readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_box(ptr: *mut std::ffi::c_char, len: i64) -> i64 {
    box_mimi_str(ptr.cast_const(), len) as i64
}

/// Copy `len` bytes and box them. Returns 0 for invalid input or allocation
/// failure. This alias is retained for existing generated code.
///
/// # Safety
/// For positive `len`, `ptr` must be readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_box_copy(ptr: *const std::ffi::c_char, len: i64) -> i64 {
    box_mimi_str_copy(ptr, len) as i64
}

/// Unpack a list-slot pointer into `{ptr, len}`.
///
/// Returns 0 on success. Returns `MIMI_ERR_OLD_STRING_ABI` if `boxed` is a
/// NUL-terminated-without-length payload (or otherwise not a fat box).
/// On error `out_len` is set to `-1` so a caller cannot mistake a C-string
/// prefix length for success.
///
/// # Safety
/// `boxed` must be zero or a live `MimiStr` box returned by `mimi_str_box` or
/// `mimi_str_box_copy`, and it must not be freed while this function runs.
/// Each non-null output pointer must be writable for one pointer or one `i64`,
/// respectively.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_unbox(
    boxed: i64,
    out_ptr: *mut *mut std::ffi::c_char,
    out_len: *mut i64,
) -> i32 {
    if boxed == 0 {
        if !out_ptr.is_null() {
            unsafe { *out_ptr = std::ptr::null_mut() };
        }
        if !out_len.is_null() {
            unsafe { *out_len = 0 };
        }
        return 0;
    }
    match unsafe { read_mimi_str(boxed as *mut std::ffi::c_char) } {
        Ok((ptr, len)) => {
            if !out_ptr.is_null() {
                unsafe { *out_ptr = ptr };
            }
            if !out_len.is_null() {
                unsafe { *out_len = len };
            }
            0
        }
        Err(e) => {
            if !out_ptr.is_null() {
                unsafe { *out_ptr = std::ptr::null_mut() };
            }
            if !out_len.is_null() {
                unsafe { *out_len = -1 };
            }
            e
        }
    }
}

/// Free a fat string box and its owned bytes. No-op on null. This is the
/// codegen-facing counterpart of `free_mimi_str`, used for `List<string>`
/// cleanup under the 0.1.8 Phase B fat ABI.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_free_box(boxed: i64) {
    unsafe { free_mimi_str(boxed as *mut std::ffi::c_char) };
}

/// Detach the payload of a fat List<String> box and release only the box.
/// This is used by `pop(List<string>)`: the returned Mimi string takes
/// ownership of the payload, while the removed list slot no longer owns it.
/// Returns the payload length, or `-1` if `boxed` is not a valid current box.
/// A zero handle is the empty string and returns length zero. A nonzero handle
/// also requires a non-null output pointer; that validation happens before the
/// box is detached so an invalid call preserves its current owner.
/// A nonzero box with a null output pointer returns `-1` without detaching.
///
/// # Safety
/// `boxed` must be zero or a live MimiStr box. For a nonzero `boxed`, `out_ptr`
/// must be non-null and writable for one pointer.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_box_take_payload(
    boxed: i64,
    out_ptr: *mut *mut std::ffi::c_char,
) -> i64 {
    if !out_ptr.is_null() {
        unsafe { *out_ptr = std::ptr::null_mut() };
    }
    if boxed == 0 {
        return 0;
    }
    if out_ptr.is_null() {
        return -1;
    }
    let box_ptr = boxed as *mut MimiStr;
    let Ok((payload, len)) = (unsafe { read_mimi_str(box_ptr.cast()) }) else {
        return -1;
    };
    if len < 0 || (payload.is_null() && len != 0) {
        return -1;
    }
    unsafe {
        *out_ptr = payload;
        (*box_ptr).magic = 0;
        (*box_ptr).ptr = std::ptr::null_mut();
    }
    mimi_free(box_ptr.cast());
    len
}

/// Deep-copy a native List<String> data array, including every ABI v3 box and
/// payload. Returns null for an empty list or on invalid input/allocation
/// failure. The caller owns the returned array and boxes.
///
/// # Safety
/// For positive `len`, `data` must point to `len` readable i64 box handles;
/// every nonzero handle must be a live current MimiStr box.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_list_data_clone(len: i64, data: *const i64) -> *mut i64 {
    if len < 0 || (len != 0 && data.is_null()) {
        return std::ptr::null_mut();
    }
    let Ok(count) = usize::try_from(len) else {
        return std::ptr::null_mut();
    };
    let Some(bytes) = count.checked_mul(std::mem::size_of::<i64>()) else {
        return std::ptr::null_mut();
    };
    if count == 0 {
        return std::ptr::null_mut();
    }
    if (data as usize) % std::mem::align_of::<i64>() != 0
        || !super::pages_mapped(data as usize, bytes)
    {
        return std::ptr::null_mut();
    }
    let cloned = mimi_alloc(bytes) as *mut i64;
    if cloned.is_null() {
        return std::ptr::null_mut();
    }
    unsafe { std::ptr::write_bytes(cloned, 0, count) };
    for index in 0..count {
        let handle = unsafe { *data.add(index) };
        if handle == 0 {
            continue;
        }
        let box_addr = handle as usize;
        if box_addr % std::mem::align_of::<MimiStr>() != 0
            || !super::pages_mapped(box_addr, std::mem::size_of::<MimiStr>())
        {
            unsafe { free_string_list_data(cloned, index) };
            return std::ptr::null_mut();
        }
        let Ok((payload, payload_len)) =
            (unsafe { read_mimi_str(handle as *mut std::ffi::c_char) })
        else {
            unsafe { free_string_list_data(cloned, index) };
            return std::ptr::null_mut();
        };
        const MAX_STR_LEN: i64 = 64 * 1024 * 1024;
        if payload_len < 0
            || payload_len > MAX_STR_LEN
            || (payload.is_null() && payload_len != 0)
            || !super::pages_mapped(payload as usize, payload_len as usize)
        {
            unsafe { free_string_list_data(cloned, index) };
            return std::ptr::null_mut();
        }
        let copy = box_mimi_str_copy(payload.cast_const(), payload_len);
        if copy.is_null() {
            unsafe { free_string_list_data(cloned, index) };
            return std::ptr::null_mut();
        }
        unsafe { *cloned.add(index) = copy as i64 };
    }
    cloned
}

/// Deep-copy the data array of a native `List<List<string>>` value. The outer
/// array stores handles to heap-packed two-field `{ i64 len, i64* data }`
/// headers; each inner data array stores owning ABI v3 `MimiStr` box handles.
/// The clone therefore gets a fresh outer array, a fresh header and data array
/// for every inner list, and fresh string boxes and payloads for every element.
/// Returns null for an empty outer list or on invalid input/allocation failure.
///
/// # Safety
/// For positive `len`, `data` must point to `len` readable i64 handles. Every
/// nonzero handle must point to a readable native two-field list header; every
/// nonempty inner data pointer must point to readable i64 ABI v3 string-box
/// handles. The returned allocation tree is owned by the caller and must be
/// released with the native `List<List<string>>` destructor.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_list_list_data_clone(len: i64, data: *const i64) -> *mut i64 {
    if len < 0 || (len != 0 && data.is_null()) {
        return std::ptr::null_mut();
    }
    let Ok(count) = usize::try_from(len) else {
        return std::ptr::null_mut();
    };
    let Some(bytes) = count.checked_mul(std::mem::size_of::<i64>()) else {
        return std::ptr::null_mut();
    };
    if count == 0 {
        return std::ptr::null_mut();
    }
    if (data as usize) % std::mem::align_of::<i64>() != 0
        || !super::pages_mapped(data as usize, bytes)
    {
        return std::ptr::null_mut();
    }

    let cloned = mimi_alloc(bytes) as *mut i64;
    if cloned.is_null() {
        return std::ptr::null_mut();
    }
    unsafe { std::ptr::write_bytes(cloned, 0, count) };

    for index in 0..count {
        let inner_handle = unsafe { *data.add(index) };
        if inner_handle == 0 {
            // A zero handle is not a valid List header. Refuse to manufacture
            // an aggregate that the native nested-list destructor cannot read.
            unsafe { free_cloned_string_list_prefixes(cloned, index) };
            mimi_free(cloned.cast());
            return std::ptr::null_mut();
        }
        let inner_ptr = inner_handle as *const MimiListAbiPrefix;
        if (inner_ptr as usize) % std::mem::align_of::<MimiListAbiPrefix>() != 0
            || !super::pages_mapped(inner_ptr as usize, std::mem::size_of::<MimiListAbiPrefix>())
        {
            unsafe { free_cloned_string_list_prefixes(cloned, index) };
            mimi_free(cloned.cast());
            return std::ptr::null_mut();
        }
        let inner = unsafe { &*inner_ptr };
        if inner.len < 0 {
            unsafe { free_cloned_string_list_prefixes(cloned, index) };
            mimi_free(cloned.cast());
            return std::ptr::null_mut();
        }
        let Ok(inner_count) = usize::try_from(inner.len) else {
            unsafe { free_cloned_string_list_prefixes(cloned, index) };
            mimi_free(cloned.cast());
            return std::ptr::null_mut();
        };
        let Some(inner_bytes) = inner_count.checked_mul(std::mem::size_of::<i64>()) else {
            unsafe { free_cloned_string_list_prefixes(cloned, index) };
            mimi_free(cloned.cast());
            return std::ptr::null_mut();
        };
        if inner_count != 0
            && (inner.data.is_null()
                || (inner.data as usize) % std::mem::align_of::<i64>() != 0
                || !super::pages_mapped(inner.data as usize, inner_bytes))
        {
            unsafe { free_cloned_string_list_prefixes(cloned, index) };
            mimi_free(cloned.cast());
            return std::ptr::null_mut();
        }

        let inner_data = unsafe { mimi_str_list_data_clone(inner.len, inner.data.cast()) };
        if inner_count != 0 && inner_data.is_null() {
            unsafe { free_cloned_string_list_prefixes(cloned, index) };
            mimi_free(cloned.cast());
            return std::ptr::null_mut();
        }
        let inner_clone =
            mimi_alloc(std::mem::size_of::<MimiListAbiPrefix>()) as *mut MimiListAbiPrefix;
        if inner_clone.is_null() {
            unsafe {
                free_string_list_data(inner_data, inner_count);
                free_cloned_string_list_prefixes(cloned, index);
            }
            mimi_free(cloned.cast());
            return std::ptr::null_mut();
        }
        unsafe {
            *inner_clone = MimiListAbiPrefix {
                len: inner.len,
                data: inner_data.cast(),
            };
            *cloned.add(index) = inner_clone as i64;
        }
    }

    cloned
}

/// Free an ABI v3 string-list data array produced by `mimi_str_list_data_clone`.
unsafe fn free_string_list_data(data: *mut i64, len: usize) {
    if data.is_null() {
        return;
    }
    for index in 0..len {
        let handle = unsafe { *data.add(index) };
        if handle != 0 {
            unsafe { free_mimi_str(handle as *mut std::ffi::c_char) };
        }
    }
    mimi_free(data.cast());
}

/// Free a partially built outer clone, including each completed inner string
/// list header, its data array, all string boxes, and their payloads.
unsafe fn free_cloned_string_list_prefixes(data: *mut i64, len: usize) {
    for index in 0..len {
        let handle = unsafe { *data.add(index) };
        if handle == 0 {
            continue;
        }
        let inner = handle as *mut MimiListAbiPrefix;
        let prefix = unsafe { &*inner };
        let inner_len = usize::try_from(prefix.len).unwrap_or(0);
        unsafe { free_string_list_data(prefix.data.cast(), inner_len) };
        mimi_free(inner.cast());
    }
}

/// Read string element `index` from a list as `{ptr, len}`.
///
/// Reads only the stable native `{len, data}` prefix. Old C-string slots and
/// unknown slot layouts are rejected by checking the per-element MimiStr
/// magic, without reading the larger runtime-only `MimiList` metadata.
///
/// # Safety
/// `list` must be null or point to a readable initialized `{i64 len, pointer
/// data}` prefix; for an in-range `index`, the data pointer must reference a
/// readable element slot containing zero or a live `MimiStr` box. A non-null
/// output pointer must be writable for one pointer or one `i64`, respectively.
/// Any returned payload pointer is borrowed from the list element and must not
/// be used after the owning box is freed.
#[no_mangle]
pub unsafe extern "C" fn mimi_list_read_string(
    list: *const MimiList,
    index: i64,
    out_ptr: *mut *mut std::ffi::c_char,
    out_len: *mut i64,
) -> i32 {
    if !out_ptr.is_null() {
        unsafe { *out_ptr = std::ptr::null_mut() };
    }
    if !out_len.is_null() {
        unsafe { *out_len = -1 };
    }
    if list.is_null() {
        return -1;
    }
    let prefix = unsafe { &*list.cast::<MimiListAbiPrefix>() };
    if prefix.data.is_null() || index < 0 || index >= prefix.len {
        return -2;
    }
    let slot = unsafe { *prefix.data.add(index as usize) };
    match unsafe { read_mimi_str(slot) } {
        Ok((ptr, len)) => {
            if !out_ptr.is_null() {
                unsafe { *out_ptr = ptr };
            }
            if !out_len.is_null() {
                unsafe { *out_len = len };
            }
            0
        }
        Err(e) => e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_copies_exact_bytes_and_owns_independent_storage() {
        let source = [0xff_u8, b'a', 0, b'b'];
        let boxed = unsafe {
            mimi_str_box(
                source.as_ptr() as *mut std::ffi::c_char,
                source.len() as i64,
            )
        } as *mut std::ffi::c_char;
        assert!(!boxed.is_null());
        let (payload, len) = unsafe { read_mimi_str(boxed).unwrap() };
        assert_eq!(len, source.len() as i64);
        assert_ne!(payload as *const u8, source.as_ptr());
        assert_eq!(unsafe { slot_bytes(payload, len) }, source);
        assert_eq!(source, [0xff, b'a', 0, b'b']);
        unsafe { free_mimi_str(boxed) };
        assert_eq!(source[0], 0xff);
    }

    #[test]
    fn box_rejects_invalid_lengths_and_null_nonempty_input() {
        assert_eq!(unsafe { mimi_str_box(std::ptr::null_mut(), 1) }, 0);
        assert_eq!(unsafe { mimi_str_box(std::ptr::null_mut(), -1) }, 0);
        assert_eq!(
            unsafe { mimi_str_box(std::ptr::null_mut(), 64 * 1024 * 1024 + 1) },
            0
        );
    }

    #[test]
    fn list_read_string_accepts_the_native_two_field_prefix() {
        let boxed = alloc_mimi_str(b"prefix");
        assert!(!boxed.is_null());
        let mut slot = boxed.cast::<std::ffi::c_char>();
        let prefix = MimiListAbiPrefix {
            len: 1,
            data: &mut slot,
        };
        let mut out_ptr = std::ptr::null_mut();
        let mut out_len = -1;
        // SAFETY: the native two-field prefix and its single box slot are live
        // for the duration of this read, and both outputs are writable.
        let status = unsafe {
            mimi_list_read_string(
                (&prefix as *const MimiListAbiPrefix).cast::<MimiList>(),
                0,
                &mut out_ptr,
                &mut out_len,
            )
        };
        assert_eq!(status, 0);
        assert_eq!(out_len, 6);
        assert_eq!(unsafe { slot_bytes(out_ptr, out_len) }, b"prefix");
        unsafe { free_mimi_str(slot) };
    }

    #[test]
    fn taking_string_box_payload_frees_only_the_box() {
        let boxed = alloc_mimi_str(b"popped");
        assert!(!boxed.is_null());
        let mut payload = std::ptr::null_mut();
        // SAFETY: `boxed` is a live MimiStr allocated above and `payload` is
        // writable for one pointer. The transferred payload is freed below.
        let len = unsafe { mimi_str_box_take_payload(boxed as i64, &mut payload) };
        assert_eq!(len, 6);
        assert!(!payload.is_null());
        assert_eq!(unsafe { slot_bytes(payload, len) }, b"popped");
        // SAFETY: taking detached the payload from the box; it remains a live
        // allocator-compatible string buffer until released here.
        mimi_free(payload.cast());
    }

    #[test]
    fn taking_string_box_payload_rejects_null_output_without_detaching() {
        let boxed = alloc_mimi_str(b"still-owned");
        assert!(!boxed.is_null());

        // SAFETY: the box remains live; a null output is explicitly rejected
        // before its payload or wrapper is modified.
        assert_eq!(
            unsafe { mimi_str_box_take_payload(boxed as i64, std::ptr::null_mut()) },
            -1
        );
        let (payload, len) = unsafe { read_mimi_str(boxed.cast()).expect("box must remain owned") };
        assert_eq!(unsafe { slot_bytes(payload, len) }, b"still-owned");

        // An empty handle has no payload to lose, so it remains a valid no-op
        // even when the output slot is omitted.
        assert_eq!(
            unsafe { mimi_str_box_take_payload(0, std::ptr::null_mut()) },
            0
        );
        // SAFETY: `boxed` is still the live owner because the invalid transfer
        // above returned before detaching either allocation.
        unsafe { free_mimi_str(boxed.cast()) };
    }

    #[test]
    fn list_string_data_clone_owns_independent_boxes_and_payloads() {
        let left = alloc_mimi_str(b"left");
        let right = alloc_mimi_str(b"right");
        assert!(!left.is_null() && !right.is_null());
        let source = [left as i64, right as i64];
        // SAFETY: source contains two live ABI v3 box handles and remains
        // readable while the helper copies it.
        let cloned = unsafe { mimi_str_list_data_clone(2, source.as_ptr()) };
        assert!(!cloned.is_null());
        let left_clone = unsafe { *cloned } as *mut std::ffi::c_char;
        let right_clone = unsafe { *cloned.add(1) } as *mut std::ffi::c_char;
        assert_ne!(left_clone, left.cast());
        assert_ne!(right_clone, right.cast());
        assert_eq!(
            unsafe { slot_bytes(read_mimi_str(left_clone).unwrap().0, 4) },
            b"left"
        );
        assert_eq!(
            unsafe { slot_bytes(read_mimi_str(right_clone).unwrap().0, 5) },
            b"right"
        );
        unsafe {
            free_mimi_str(left.cast());
            free_mimi_str(right.cast());
            free_mimi_str(left_clone.cast());
            free_mimi_str(right_clone.cast());
        }
        mimi_free(cloned.cast());
    }

    #[test]
    fn nested_string_list_clone_owns_every_level_and_preserves_empty_rows() {
        let left = alloc_mimi_str(b"left");
        let right = alloc_mimi_str(b"right");
        assert!(!left.is_null() && !right.is_null());
        let source_inner_data = mimi_alloc(2 * std::mem::size_of::<i64>()) as *mut i64;
        let source_empty_header =
            mimi_alloc(std::mem::size_of::<MimiListAbiPrefix>()) as *mut MimiListAbiPrefix;
        let source_full_header =
            mimi_alloc(std::mem::size_of::<MimiListAbiPrefix>()) as *mut MimiListAbiPrefix;
        let source_outer_data = mimi_alloc(2 * std::mem::size_of::<i64>()) as *mut i64;
        assert!(
            !source_inner_data.is_null()
                && !source_empty_header.is_null()
                && !source_full_header.is_null()
                && !source_outer_data.is_null()
        );
        unsafe {
            *source_inner_data = left as i64;
            *source_inner_data.add(1) = right as i64;
            *source_empty_header = MimiListAbiPrefix {
                len: 0,
                data: std::ptr::null_mut(),
            };
            *source_full_header = MimiListAbiPrefix {
                len: 2,
                data: source_inner_data.cast(),
            };
            *source_outer_data = source_empty_header as i64;
            *source_outer_data.add(1) = source_full_header as i64;
        }

        // SAFETY: the outer slots, both two-field inner headers, both string
        // boxes, and their payloads are live and initialized for the clone.
        let cloned_outer = unsafe { mimi_str_list_list_data_clone(2, source_outer_data) };
        assert!(!cloned_outer.is_null());
        let cloned_empty_header = unsafe { *cloned_outer } as *mut MimiListAbiPrefix;
        let cloned_full_header = unsafe { *cloned_outer.add(1) } as *mut MimiListAbiPrefix;
        assert_ne!(cloned_outer, source_outer_data);
        assert_ne!(cloned_empty_header, source_empty_header);
        assert_ne!(cloned_full_header, source_full_header);

        let cloned_empty = unsafe { &*cloned_empty_header };
        let cloned_full = unsafe { &*cloned_full_header };
        assert_eq!(cloned_empty.len, 0);
        assert!(cloned_empty.data.is_null());
        assert_eq!(cloned_full.len, 2);
        assert_ne!(cloned_full.data, source_inner_data.cast());
        let cloned_left = unsafe { *cloned_full.data } as *mut std::ffi::c_char;
        let cloned_right = unsafe { *cloned_full.data.add(1) } as *mut std::ffi::c_char;
        assert_ne!(cloned_left, left.cast());
        assert_ne!(cloned_right, right.cast());
        let (cloned_left_payload, cloned_left_len) = unsafe { read_mimi_str(cloned_left).unwrap() };
        let (cloned_right_payload, cloned_right_len) =
            unsafe { read_mimi_str(cloned_right).unwrap() };
        assert_ne!(cloned_left_payload, unsafe { (*left).ptr });
        assert_ne!(cloned_right_payload, unsafe { (*right).ptr });
        assert_eq!(
            unsafe { slot_bytes(cloned_left_payload, cloned_left_len) },
            b"left"
        );
        assert_eq!(
            unsafe { slot_bytes(cloned_right_payload, cloned_right_len) },
            b"right"
        );

        // Free the independently-owned source and clone trees with the same
        // allocator pairing used by the native nested-list scope cleanup.
        unsafe {
            free_cloned_string_list_prefixes(source_outer_data, 2);
            mimi_free(source_outer_data.cast());
            free_cloned_string_list_prefixes(cloned_outer, 2);
            mimi_free(cloned_outer.cast());
        }
    }

    #[test]
    fn string_list_cloners_reject_unmapped_and_malformed_inputs() {
        let invalid = 1usize as *const i64;
        // SAFETY: these deliberately malformed addresses must be rejected by
        // the mapped-page and alignment checks before any dereference.
        assert!(unsafe { mimi_str_list_data_clone(1, invalid) }.is_null());
        assert!(unsafe { mimi_str_list_list_data_clone(1, invalid) }.is_null());

        let bad_header_handle = [1_i64];
        // SAFETY: the outer slot is readable, while its invalid inner handle
        // is rejected before the helper attempts to read an inner header.
        assert!(unsafe { mimi_str_list_list_data_clone(1, bad_header_handle.as_ptr()) }.is_null());

        let bad_string_slot = mimi_alloc(std::mem::size_of::<i64>()) as *mut i64;
        let header = mimi_alloc(std::mem::size_of::<MimiListAbiPrefix>()) as *mut MimiListAbiPrefix;
        let outer = mimi_alloc(std::mem::size_of::<i64>()) as *mut i64;
        assert!(!bad_string_slot.is_null() && !header.is_null() && !outer.is_null());
        unsafe {
            *bad_string_slot = 1;
            *header = MimiListAbiPrefix {
                len: 1,
                data: bad_string_slot.cast(),
            };
            *outer = header as i64;
        }
        // SAFETY: all array/header storage is live; the string slot's handle
        // points to an unmapped address and must fail closed during cloning.
        assert!(unsafe { mimi_str_list_list_data_clone(1, outer) }.is_null());
        mimi_free(bad_string_slot.cast());
        mimi_free(header.cast());
        mimi_free(outer.cast());
    }

    #[test]
    fn list_free_does_not_apply_v3_ownership_to_v2_string_boxes() {
        let payload = alloc_c_string_from_bytes(b"legacy-owned-elsewhere");
        assert!(!payload.is_null());
        let boxed = mimi_alloc(std::mem::size_of::<MimiStr>()) as *mut MimiStr;
        assert!(!boxed.is_null());
        unsafe {
            *boxed = MimiStr {
                magic: MIMI_STR_MAGIC,
                _pad: 0,
                ptr: payload,
                len: 22,
            };
        }

        let data = unsafe {
            libc::malloc(std::mem::size_of::<*mut std::ffi::c_char>()) as *mut *mut std::ffi::c_char
        };
        assert!(!data.is_null());
        unsafe { *data = boxed.cast() };
        let mut list = MimiList::with_string_data(data, 1, true);
        list.string_abi = 2;

        // SAFETY: the full list, its libc-allocated one-slot data array, and
        // the old-layout fat box are live for the call. ABI v2 payload
        // ownership belongs elsewhere, so current cleanup must skip the box.
        unsafe { super::super::mimi_list_free(Box::into_raw(Box::new(list)), true) };
        assert_eq!(unsafe { (*boxed).magic }, MIMI_STR_MAGIC);
        assert_eq!(unsafe { (*boxed).ptr }, payload);

        // SAFETY: these allocations were intentionally retained by the v2
        // cleanup path and are released here by their matching allocator.
        mimi_free(payload.cast());
        mimi_free(boxed.cast());
    }
}

/// Length-aware `str_split`. Writes fat `{ptr, len}` elements and stamps
/// `string_abi = FAT`.
///
/// # Safety
/// `s` / `delim` must be valid for `s_len` / `delim_len` bytes (or null).
#[no_mangle]
pub unsafe extern "C" fn mimi_str_split_ll(
    s: *const std::ffi::c_char,
    s_len: i64,
    delim: *const std::ffi::c_char,
    delim_len: i64,
) -> *mut MimiList {
    let ss = str_from_ptr_len(s, s_len);
    let d = str_from_ptr_len(delim, delim_len);

    let parts: Vec<String> = if d.is_empty() {
        if ss.is_empty() {
            vec!["".to_string()]
        } else {
            ss.chars().map(|c| c.to_string()).collect()
        }
    } else {
        ss.split(&d).map(|p| p.to_string()).collect()
    };

    alloc_fat_string_list(&parts)
}

/// Build a runtime `MimiList` of fat string elements.
pub fn alloc_fat_string_list(parts: &[String]) -> *mut MimiList {
    let len = parts.len() as i64;
    let data_ptr = if len <= 0 {
        std::ptr::null_mut()
    } else {
        let data_size =
            match (len as usize).checked_mul(std::mem::size_of::<*mut std::ffi::c_char>()) {
                Some(s) => s,
                None => {
                    return Box::into_raw(Box::new(MimiList::new_string_list()));
                }
            };
        let ptr = unsafe { libc::malloc(data_size) as *mut *mut std::ffi::c_char };
        if ptr.is_null() {
            return std::ptr::null_mut();
        }
        for (i, p) in parts.iter().enumerate() {
            unsafe {
                *ptr.add(i) = alloc_mimi_str(p.as_bytes()) as *mut std::ffi::c_char;
            }
        }
        ptr
    };
    Box::into_raw(Box::new(MimiList::with_string_data(data_ptr, len, true)))
}

/// Join fat string elements. On old-ABI / non-fat slots returns null and
/// writes `*out_len = -1` (never a C-string prefix length).
pub unsafe fn join_fat_string_list(
    list: *const MimiList,
    sep_bytes: &[u8],
    out_len: *mut i64,
) -> *mut std::ffi::c_char {
    if !out_len.is_null() {
        unsafe { *out_len = 0 };
    }
    if list.is_null() {
        return alloc_c_string_from_bytes(b"");
    }
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
    if lst.data.is_null() || lst.len == 0 {
        return alloc_c_string_from_bytes(b"");
    }
    if lst.len < 0 || lst.len > 1_000_000 {
        return alloc_c_string_from_bytes(b"");
    }
    let separator = String::from_utf8_lossy(sep_bytes).into_owned();
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize);
    for i in 0..lst.len as usize {
        let slot = unsafe { *lst.data.add(i) };
        match unsafe { read_mimi_str(slot) } {
            Ok((ptr, len)) => parts.push(str_from_ptr_len(ptr, len)),
            Err(_) => {
                if !out_len.is_null() {
                    unsafe { *out_len = -1 };
                }
                return std::ptr::null_mut();
            }
        }
    }
    let result = parts.join(&separator);
    if !out_len.is_null() {
        unsafe { *out_len = result.len() as i64 };
    }
    alloc_c_string_from_bytes(result.as_bytes())
}
