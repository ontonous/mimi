//! Length-bearing `List<string>` elements (`{ptr, len}`).
//!
//! 0.38.26 (B-STR-001): string list slots no longer store a bare NUL-terminated
//! `char*`. Each element is a heap `MimiStr` `{ptr, len}` with the same
//! observation as a bare Mimi `string`. Old C-string payloads are rejected
//! (handshake / ABI version), never silently truncated at the first `0x00`.

#[cfg(standalone)]
use super::libc;
use super::{
    alloc_c_string_from_bytes, mimi_alloc, mimi_free, str_from_ptr_len, ListElementKind, MimiList,
    MimiListAbiPrefix,
};

/// Current `List<string>` element ABI. Version 1 was NUL-terminated `char*`.
pub const LIST_STRING_ABI_VERSION: i32 = 2;

/// Written into `MimiList.string_abi` for every newly created string list.
pub const LIST_STRING_ABI_FAT: u8 = 2;

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

/// Allocate a fat string box that *borrows* an existing `{ptr, len}` (no copy).
/// Used when codegen already owns the bytes and only needs a list slot.
pub fn box_mimi_str(ptr: *mut std::ffi::c_char, len: i64) -> *mut MimiStr {
    let box_ptr = mimi_alloc(std::mem::size_of::<MimiStr>()) as *mut MimiStr;
    if box_ptr.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        *box_ptr = MimiStr {
            magic: MIMI_STR_MAGIC,
            _pad: 0,
            ptr,
            len: len.max(0),
        };
    }
    box_ptr
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

/// Box `{ptr, len}` for storage in a list slot. Returns the box pointer as i64
/// (0 on OOM). Does not copy bytes.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_box(ptr: *mut std::ffi::c_char, len: i64) -> i64 {
    box_mimi_str(ptr, len) as i64
}

/// Copy `len` bytes and box them. Returns the box pointer as i64 (0 on OOM).
#[no_mangle]
pub unsafe extern "C" fn mimi_str_box_copy(ptr: *const std::ffi::c_char, len: i64) -> i64 {
    let s = str_from_ptr_len(ptr, len);
    alloc_mimi_str(s.as_bytes()) as i64
}

/// Unpack a list-slot pointer into `{ptr, len}`.
///
/// Returns 0 on success. Returns `MIMI_ERR_OLD_STRING_ABI` if `boxed` is a
/// NUL-terminated-without-length payload (or otherwise not a fat box).
/// On error `out_len` is set to `-1` so a caller cannot mistake a C-string
/// prefix length for success.
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

/// Read string element `index` from a list as `{ptr, len}`.
///
/// Full `MimiList` values with `string_abi != FAT` are rejected (old C-string
/// layout). Native `{len, data}` prefixes are read as fat slots; a non-fat
/// slot still returns `MIMI_ERR_OLD_STRING_ABI`.
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
    // Prefer the full header when present: a stamped legacy list must not
    // be silently reinterpreted as fat.
    let kind = unsafe { (*list).element_kind };
    let abi = unsafe { (*list).string_abi };
    if matches!(kind, ListElementKind::String | ListElementKind::Unknown)
        && abi == LIST_STRING_ABI_CSTR
    {
        return MIMI_ERR_OLD_STRING_ABI;
    }
    if matches!(kind, ListElementKind::String | ListElementKind::Unknown)
        && abi != LIST_STRING_ABI_FAT
        && abi != LIST_STRING_ABI_CSTR
    {
        // Unknown future stamp: fail closed.
        return MIMI_ERR_OLD_STRING_ABI;
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
